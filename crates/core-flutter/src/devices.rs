use std::io;
use std::path::Path;
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

/// Boot an iOS Simulator by UDID (if not already booted) and open `Simulator.app`.
pub fn boot_ios_simulator(udid: &str) -> io::Result<()> {
    let _ = Command::new("xcrun").args(["simctl", "boot", udid]).status();
    let _ = Command::new("open")
        .args(["-g", "-a", "Simulator", "--args", "-CurrentDeviceUDID", udid])
        .status();
    Ok(())
}

/// Magnetically dock/attach the native macOS `Simulator.app` window flush
/// against the right edge of our IDE window (or inside its right rail bounds)
/// and raise it cleanly alongside the editor.
pub fn attach_simulator_window(ide_x: i32, ide_y: i32, ide_w: i32, ide_h: i32) -> io::Result<()> {
    // Ensure Simulator.app is open (`-g`: don't steal focus from the IDE)
    let _ = Command::new("open").args(["-g", "-a", "Simulator"]).status();
    let target_x = (ide_x + ide_w - 400).max(40);
    let target_y = ide_y.max(28);
    let target_h = ide_h.max(680);
    let script = format!(
        r#"tell application "System Events"
            if exists process "Simulator" then
                tell process "Simulator"
                    if (count of windows) > 0 then
                        set position of window 1 to {{{target_x}, {target_y}}}
                        try
                            set size of window 1 to {{390, {target_h}}}
                        end try
                    end if
                end tell
            end if
        end tell"#
    );
    let _ = Command::new("osascript").args(["-e", &script]).status();
    Ok(())
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

/// Run a JXA (JavaScript for Automation) snippet via `osascript -l JavaScript`.
/// Errors if the script throws (e.g. no Simulator window).
fn run_jxa(script: &str) -> io::Result<()> {
    let status = Command::new("osascript")
        .args(["-l", "JavaScript", "-e", script])
        .stderr(std::process::Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::NotFound, "no Simulator window"))
    }
}

/// Run a JXA snippet, and if it fails because `Simulator.app` has no window yet,
/// launch Simulator in the background and retry once.
fn run_jxa_with_simulator(script: &str) -> io::Result<()> {
    if run_jxa(script).is_ok() {
        return Ok(());
    }
    let _ = Command::new("open").args(["-g", "-a", "Simulator"]).status();
    for _ in 0..8 {
        std::thread::sleep(std::time::Duration::from_millis(400));
        if run_jxa(script).is_ok() {
            return Ok(());
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "no Simulator window"))
}

/// JXA prelude: finds the `Simulator` device window (`win`, with `win.kCGWindowBounds`)
/// and its owner pid (`pid`) via CoreGraphics, without needing Accessibility APIs and
/// without activating the app. Lists *all* windows (not only on-screen ones, which
/// omits windows on other Spaces) and ignores the menu-bar-sized helper windows.
const JXA_FIND_SIMULATOR: &str = r#"
ObjC.import('CoreGraphics');
const infos = ObjC.deepUnwrap(ObjC.castRefToObject($.CGWindowListCopyWindowInfo(0, 0)));
const cands = infos.filter(w => w.kCGWindowOwnerName === 'Simulator'
    && w.kCGWindowLayer === 0 && w.kCGWindowBounds.Height > 200 && w.kCGWindowBounds.Width > 150);
const win = cands.find(w => w.kCGWindowIsOnscreen) || cands[0];
if (!win) { throw new Error('no simulator window'); }
const pid = win.kCGWindowOwnerPID;
const b = win.kCGWindowBounds;
"#;

/// Locate Facebook's `idb` CLI (`brew install idb-companion && pip3 install fb-idb`).
/// `idb` injects real HID touches into a booted simulator **headlessly** — no
/// `Simulator.app` window needed — which is what makes the embedded view interactive.
pub fn find_idb() -> Option<String> {
    static IDB: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    IDB.get_or_init(|| {
        let home = std::env::var("HOME").unwrap_or_default();
        [
            "/opt/homebrew/bin/idb".to_string(),
            "/usr/local/bin/idb".to_string(),
            format!("{home}/.local/bin/idb"),
            format!("{home}/Library/Python/3.12/bin/idb"),
            format!("{home}/Library/Python/3.11/bin/idb"),
            format!("{home}/Library/Python/3.10/bin/idb"),
            format!("{home}/Library/Python/3.9/bin/idb"),
        ]
        .into_iter()
        .find(|p| Path::new(p).is_file())
        .or_else(|| {
            Command::new("which")
                .arg("idb")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
    })
    .clone()
}

/// Whether taps can be injected into a headless simulator (i.e. `idb` is installed).
pub fn simulator_input_available() -> bool {
    find_idb().is_some()
}

/// Screen size in points of a simulator, via `idb describe` (cached per udid).
fn simulator_screen_points(idb: &str, udid: &str) -> Option<(f32, f32)> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, (f32, f32)>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(v) = cache.lock().ok().and_then(|c| c.get(udid).copied()) {
        return Some(v);
    }
    let out = Command::new(idb)
        .args(["describe", "--udid", udid, "--json"])
        .output()
        .ok()?;
    let json: Value = serde_json::from_slice(&out.stdout).ok()?;
    let dims = json.get("screen_dimensions")?;
    let w = dims.get("width_points").and_then(Value::as_f64)? as f32;
    let h = dims.get("height_points").and_then(Value::as_f64)? as f32;
    if let Ok(mut c) = cache.lock() {
        c.insert(udid.to_string(), (w, h));
    }
    Some((w, h))
}

/// Forward a normalized `(norm_x, norm_y)` tap (`0.0..=1.0`) from the embedded
/// iPhone screen to the simulator.
///
/// Uses `idb` HID injection when available (works headless, never raises
/// `Simulator.app`). Otherwise falls back to posting a click to the Simulator
/// window, which only works while that window exists.
pub fn tap_simulator_window(udid: Option<&str>, norm_x: f32, norm_y: f32) -> io::Result<()> {
    let nx = norm_x.clamp(0.0, 1.0);
    let ny = norm_y.clamp(0.0, 1.0);
    if let (Some(idb), Some(udid)) = (find_idb(), udid.filter(|u| !u.is_empty())) {
        if let Some((w, h)) = simulator_screen_points(&idb, udid) {
            let status = Command::new(&idb)
                .args([
                    "ui",
                    "tap",
                    "--udid",
                    udid,
                    &format!("{}", (nx * w).round() as i32),
                    &format!("{}", (ny * h).round() as i32),
                ])
                .status()?;
            if status.success() {
                return Ok(());
            }
        }
    }
    tap_simulator_window_via_events(nx, ny)
}

/// Fallback tap: post mouse events straight to a visible `Simulator.app` window's pid.
fn tap_simulator_window_via_events(norm_x: f32, norm_y: f32) -> io::Result<()> {
    let nx = norm_x.clamp(0.02, 0.98);
    let ny = norm_y.clamp(0.02, 0.98);
    let script = format!(
        r#"{JXA_FIND_SIMULATOR}
const p = {{ x: b.X + 18 + (b.Width - 36) * {nx:.4}, y: b.Y + 44 + (b.Height - 62) * {ny:.4} }};
function post(type) {{
    const e = $.CGEventCreateMouseEvent(null, type, p, 0);
    $.CGEventSetIntegerValueField(e, 1, 1);
    $.CGEventPostToPid(pid, e);
}}
post(5);
delay(0.02);
post(1);
delay(0.05);
post(2);
"#
    );
    run_jxa_with_simulator(&script)
}

/// Press the iOS Home button on the simulator (`idb` when available, else `Cmd+Shift+H`).
pub fn simulator_press_home(udid: Option<&str>) -> io::Result<()> {
    if let (Some(idb), Some(udid)) = (find_idb(), udid.filter(|u| !u.is_empty())) {
        if Command::new(&idb)
            .args(["ui", "button", "--udid", udid, "HOME"])
            .status()
            .is_ok_and(|s| s.success())
        {
            return Ok(());
        }
    }
    let script = format!(
        r#"{JXA_FIND_SIMULATOR}
function key(down) {{
    const e = $.CGEventCreateKeyboardEvent(null, 4, down);
    $.CGEventSetFlags(e, 0x100000 | 0x20000);
    $.CGEventPostToPid(pid, e);
}}
key(true);
key(false);
"#
    );
    run_jxa_with_simulator(&script)
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
