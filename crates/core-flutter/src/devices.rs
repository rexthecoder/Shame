use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use serde_json::Value;

/// A device or emulator `flutter devices` can see, enough to populate a
/// device picker.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    #[serde(rename = "targetPlatform")]
    pub target_platform: String,
    pub emulator: bool,
    #[serde(rename = "isSupported")]
    pub is_supported: bool,
}

/// An iOS Simulator discovered via `xcrun simctl list devices available -j`
/// (including simulators that are currently `"Shutdown"` so the user can
/// boot and attach them directly from the IDE).
#[derive(Debug, Clone, PartialEq)]
pub struct IosSimulator {
    pub udid: String,
    pub name: String,
    pub state: String,
    pub runtime: String,
}

impl IosSimulator {
    pub fn is_booted(&self) -> bool {
        self.state.eq_ignore_ascii_case("Booted")
    }
}

/// Run `flutter devices --machine` and parse its output. Unlike the
/// streaming daemon protocol (`FlutterDaemon`), this is a one-shot
/// command: it prints a single (pretty-printed, multi-line) JSON array
/// and exits, so we just wait for it and parse the whole thing.
pub fn list_devices() -> io::Result<Vec<Device>> {
    let output = Command::new("flutter").args(["devices", "--machine"]).output()?;
    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("`flutter devices --machine` exited with {}", output.status),
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Query `xcrun simctl list devices available -j` on macOS for available
/// iPhone/iPad simulators (both Booted and Shutdown).
pub fn list_ios_simulators() -> io::Result<Vec<IosSimulator>> {
    let output = Command::new("xcrun")
        .args(["simctl", "list", "devices", "available", "-j"])
        .output()?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let root: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut sims = Vec::new();
    if let Some(devices_by_runtime) = root.get("devices").and_then(Value::as_object) {
        for (runtime_key, list) in devices_by_runtime {
            if !runtime_key.contains("iOS") {
                continue;
            }
            let runtime_label = runtime_key
                .rsplit('.')
                .next()
                .unwrap_or(runtime_key)
                .replace('-', ".");
            let Some(arr) = list.as_array() else {
                continue;
            };
            for item in arr {
                let is_available = item
                    .get("isAvailable")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                if !is_available {
                    continue;
                }
                let Some(udid) = item.get("udid").and_then(Value::as_str) else {
                    continue;
                };
                let Some(name) = item.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let state = item
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("Shutdown")
                    .to_string();
                sims.push(IosSimulator {
                    udid: udid.to_string(),
                    name: name.to_string(),
                    state,
                    runtime: runtime_label.clone(),
                });
            }
        }
    }
    // Sort booted simulators first, then iPhones before iPads, then by name.
    sims.sort_by(|a, b| {
        b.is_booted()
            .cmp(&a.is_booted())
            .then_with(|| b.name.starts_with("iPhone").cmp(&a.name.starts_with("iPhone")))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(sims)
}

/// Boot an iOS Simulator by UDID (if not already booted), headlessly.
///
/// The embedded mirror reads frames via `simctl`, so `Simulator.app` is not opened.
/// `simctl boot` exits non-zero when the device is already booted, which is fine.
pub fn boot_ios_simulator(udid: &str) -> io::Result<()> {
    let out = Command::new("xcrun")
        .args(["simctl", "boot", udid])
        .output()?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if out.status.success() || stderr.contains("current state: Booted") {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!("simctl boot failed: {}", stderr.trim()),
        ))
    }
}

/// Capture a live PNG screenshot of the booted (or specified UDID) iOS Simulator
/// into `out_path` using `xcrun simctl io <target> screenshot`.
pub fn capture_simulator_screenshot(udid: Option<&str>, out_path: &Path) -> io::Result<()> {
    let target = udid
        .filter(|id| !id.is_empty() && *id != "macos" && *id != "chrome")
        .unwrap_or("booted");
    // JPEG encodes several times faster than PNG, which is what makes live mirroring smooth.
    let status = Command::new("xcrun")
        .args([
            "simctl",
            "io",
            target,
            "screenshot",
            "--type=jpeg",
            &out_path.to_string_lossy(),
        ])
        .status()?;
    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "simctl screenshot failed (no booted simulator)",
        ));
    }
    Ok(())
}

/// Locate `shame-sim`, the headless touch/button helper built from `tools/simhelper`.
///
/// Lookup order: `SHAME_SIM_HELPER` at runtime, a `shame-sim` next to the editor binary
/// (for packaged builds), then the copy `build.rs` compiled for this checkout.
pub fn find_sim_helper() -> Option<PathBuf> {
    static HELPER: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    HELPER
        .get_or_init(|| {
            let beside_exe = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|d| d.join("shame-sim")));
            [
                std::env::var_os("SHAME_SIM_HELPER").map(PathBuf::from),
                beside_exe,
                option_env!("SHAME_SIM_HELPER").map(PathBuf::from),
            ]
            .into_iter()
            .flatten()
            .find(|p| p.is_file())
        })
        .clone()
}

/// Whether taps can be injected into a headless simulator.
pub fn simulator_input_available() -> bool {
    find_sim_helper().is_some()
}

/// A phase of a single-finger touch on the embedded simulator screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchPhase {
    Down,
    Move,
    Up,
}

enum SimCommand {
    Touch(TouchPhase, f32, f32),
    Home,
    /// Does nothing on the device; only ensures the helper is running.
    Noop,
}

/// One decoded simulator frame: half-resolution BGRA (what GPUI renders), top-left origin.
pub struct SimFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// Live frames from the helper. `latest` always holds the newest frame (older ones are
/// overwritten, so a slow consumer never builds a backlog); `ready` is signalled when it
/// changes and closes when the helper exits.
#[derive(Clone)]
pub struct SimFrameStream {
    pub latest: std::sync::Arc<std::sync::Mutex<Option<SimFrame>>>,
    pub ready: async_channel::Receiver<()>,
}

struct SimSession {
    udid: String,
    tx: std::sync::mpsc::Sender<SimCommand>,
    frames: SimFrameStream,
}

static SIM_SESSION: std::sync::Mutex<Option<SimSession>> = std::sync::Mutex::new(None);

/// Start a long-running `shame-sim serve` for `udid`. A writer thread owns the child's stdin,
/// coalescing bursts of `Move`s so a fast drag never queues stale positions; it exits (killing
/// the helper) when the write side breaks, and the next call starts a fresh one.
fn start_sim_session(udid: &str) -> io::Result<SimSession> {
    use std::io::Write;
    use std::process::Stdio;
    use std::sync::mpsc;

    let helper = find_sim_helper().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "shame-sim helper not built (Xcode is required to build it)",
        )
    })?;
    let mut child = Command::new(helper)
        .args(["serve", udid, "--stream"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let latest = std::sync::Arc::new(std::sync::Mutex::new(None));
    let (ready_tx, ready_rx) = async_channel::bounded::<()>(1);
    {
        let latest = latest.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut header = [0u8; 16];
            while stdout.read_exact(&mut header).is_ok() {
                let word = |i: usize| u32::from_le_bytes(header[i..i + 4].try_into().unwrap());
                if word(0) != 0x4D52_4653 || word(12) as usize != word(4) as usize * word(8) as usize * 4 {
                    break; // out of sync: give up, the session restarts on the next call
                }
                let mut bgra = vec![0u8; word(12) as usize];
                if stdout.read_exact(&mut bgra).is_err() {
                    break;
                }
                let frame = SimFrame { width: word(4), height: word(8), bgra };
                *latest.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame);
                let _ = ready_tx.try_send(());
            }
            // Dropping `ready_tx` closes the stream.
        });
    }
    let (tx, rx) = mpsc::channel::<SimCommand>();
    std::thread::spawn(move || {
        let line = |cmd: &SimCommand| match cmd {
            SimCommand::Touch(phase, x, y) => {
                let verb = match phase {
                    TouchPhase::Down => "down",
                    TouchPhase::Move => "move",
                    TouchPhase::Up => "up",
                };
                format!("{verb} {x:.4} {y:.4}\n")
            }
            SimCommand::Home => "home\n".to_string(),
            SimCommand::Noop => "\n".to_string(),
        };
        'outer: while let Ok(mut cmd) = rx.recv() {
            loop {
                if let SimCommand::Touch(TouchPhase::Move, ..) = cmd {
                    match rx.try_recv() {
                        Ok(next @ SimCommand::Touch(TouchPhase::Move, ..)) => {
                            cmd = next;
                            continue;
                        }
                        Ok(next) => {
                            // A non-move follows: send this move, then the follower.
                            if stdin.write_all(line(&cmd).as_bytes()).is_err() {
                                break 'outer;
                            }
                            cmd = next;
                            continue;
                        }
                        Err(_) => {}
                    }
                }
                break;
            }
            if stdin
                .write_all(line(&cmd).as_bytes())
                .and_then(|_| stdin.flush())
                .is_err()
            {
                break;
            }
        }
        drop(stdin);
        let _ = child.wait();
    });
    Ok(SimSession {
        udid: udid.to_string(),
        tx,
        frames: SimFrameStream { latest, ready: ready_rx },
    })
}

fn send_sim_command(udid: Option<&str>, make: impl Fn() -> SimCommand) -> io::Result<()> {
    let udid = udid
        .filter(|u| !u.is_empty() && *u != "macos" && *u != "chrome")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no simulator selected"))?;
    let mut guard = SIM_SESSION.lock().unwrap_or_else(|e| e.into_inner());
    for _ in 0..2 {
        if guard.as_ref().is_none_or(|s| s.udid != udid) {
            *guard = Some(start_sim_session(udid)?);
        }
        if guard.as_ref().is_some_and(|s| s.tx.send(make()).is_ok()) {
            return Ok(());
        }
        *guard = None; // helper died: restart once
    }
    Err(io::Error::new(io::ErrorKind::BrokenPipe, "shame-sim exited"))
}

/// The live frame stream for `udid`, starting the helper if needed. When the stream's `ready`
/// channel closes the helper has exited (e.g. the simulator shut down).
pub fn simulator_frame_stream(udid: Option<&str>) -> io::Result<SimFrameStream> {
    send_sim_command(udid, || SimCommand::Noop)?;
    SIM_SESSION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|s| s.frames.clone())
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "shame-sim exited"))
}

/// Stop the helper (if running). It exits when its stdin closes, ending the frame stream.
pub fn simulator_detach() {
    *SIM_SESSION.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Start the input helper for `udid` ahead of the first touch, so its warm-up isn't felt.
pub fn simulator_prewarm(udid: Option<&str>) -> io::Result<()> {
    send_sim_command(udid, || SimCommand::Noop)
}

/// Send one phase of a touch at a normalized `(x, y)` (`0.0..=1.0`, top-left origin).
/// `Down` then `Up` is a tap; `Down`, `Move`…, `Up` is a drag.
pub fn simulator_touch(udid: Option<&str>, phase: TouchPhase, x: f32, y: f32) -> io::Result<()> {
    let (x, y) = (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
    send_sim_command(udid, || SimCommand::Touch(phase, x, y))
}

/// Press the iOS Home button on the simulator.
pub fn simulator_press_home(udid: Option<&str>) -> io::Result<()> {
    send_sim_command(udid, || SimCommand::Home)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not synthetic JSON — the actual (trimmed) output of `flutter
    /// devices --machine` on this machine: a simulator, macOS itself,
    /// and Chrome.
    #[test]
    fn deserializes_real_flutter_devices_output() {
        let json = r#"[
          {
            "name": "iPhone 16",
            "id": "4B6D332E-101F-4CBB-B288-4FEACF0A94BE",
            "isSupported": true,
            "targetPlatform": "ios",
            "emulator": true,
            "sdk": "com.apple.CoreSimulator.SimRuntime.iOS-18-5",
            "capabilities": {"hotReload": true, "hotRestart": true, "screenshot": true, "flutterExit": true, "hardwareRendering": false, "startPaused": true}
          },
          {
            "name": "macOS",
            "id": "macos",
            "isSupported": true,
            "targetPlatform": "darwin",
            "emulator": false,
            "sdk": "macOS 27.0.1 26A434 darwin-arm64",
            "capabilities": {"hotReload": true, "hotRestart": true, "screenshot": false, "flutterExit": true, "hardwareRendering": false, "startPaused": true}
          }
        ]"#;
        let devices: Vec<Device> = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].name, "iPhone 16");
        assert!(devices[0].emulator);
        assert_eq!(devices[1].id, "macos");
        assert!(!devices[1].emulator);
    }

    /// Spawns the real `flutter devices --machine` and checks it finds
    /// at least one device — on this machine that's always `macos`
    /// itself (desktop target), so this is a genuine check against the
    /// real CLI, not just the parser test above. Skips (not fails) if
    /// `flutter` isn't on `PATH`.
    #[test]
    fn lists_real_devices_on_this_machine() {
        if Command::new("flutter").arg("--version").output().is_err() {
            eprintln!("skipping: `flutter` not found on PATH");
            return;
        }
        let devices = list_devices().expect("flutter devices --machine should succeed");
        assert!(!devices.is_empty(), "expected at least one device (e.g. macos itself)");
    }
}
