//! The embedded iOS Simulator: state, attach/detach, the live frame stream and touch/Home input.
//!
//! The device-side work (frames, HID) is done by `shame-sim` (`tools/simhelper`), driven through
//! `core_flutter`. The panel UI lives in `rails/simulator.rs`.

use gpui_kit::*;

use crate::{DartIde, RightRailTab};

/// Everything the editor tracks about the embedded simulator.
pub(crate) struct SimulatorState {
    /// Attached means the panel is showing and frames are streaming. Only the titlebar
    /// Simulator button (or an explicit choice inside the panel) changes this.
    pub(crate) attached: bool,
    pub(crate) live_sync: bool,
    /// Newest frame, already decoded for GPUI.
    pub(crate) frame: Option<std::sync::Arc<RenderImage>>,
    /// A touch is in progress (mouse is down on the phone).
    pub(crate) touching: bool,
    /// The live framebuffer stream is running (otherwise `simctl` screenshots are the fallback).
    pub(crate) streaming: bool,
    /// What the panel is waiting on ("Booting…", "Connecting…") until the first frame.
    pub(crate) status: Option<String>,
    pub(crate) stream_retry_at: Option<std::time::Instant>,
    pub(crate) last_hash: u64,
    pub(crate) capturing: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Default for SimulatorState {
    fn default() -> Self {
        Self {
            attached: false,
            live_sync: true,
            frame: None,
            touching: false,
            streaming: false,
            status: None,
            stream_retry_at: None,
            last_hash: 0,
            capturing: Default::default(),
        }
    }
}

impl DartIde {
    /// Open and attach the embedded Simulator panel inside the IDE (headless;
    /// `Simulator.app` is not opened or moved).
    pub(crate) fn attach_simulator(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.agent_open = true;
        self.right_rail_tab = RightRailTab::Simulator;
        self.sim.attached = true;

        // Pick a booted simulator or the first available iPhone simulator
        let sim_udid = self
            .selected_device
            .as_ref()
            .filter(|d| d.is_ios())
            .map(|d| d.id.clone())
            .or_else(|| {
                self.ios_simulators
                    .iter()
                    .find(|s| s.is_booted())
                    .or_else(|| self.ios_simulators.first())
                    .map(|s| {
                        self.selected_device = Some(core_flutter::Device {
                            id: s.udid.clone(),
                            name: s.name.clone(),
                            target_platform: "ios".to_string(),
                            emulator: true,
                            is_supported: true,
                        });
                        s.udid.clone()
                    })
            });

        if let Some(udid) = sim_udid {
            let sim = self.ios_simulators.iter().find(|s| s.udid == udid);
            let name = sim.map_or("simulator".to_string(), |s| s.name.clone());
            let booted = sim.is_some_and(|s| s.is_booted());
            self.sim.status = Some(if booted {
                format!("Connecting to {name}…")
            } else {
                format!("Booting {name}…")
            });
            // `simctl boot` can take seconds on a shut-down device; never block the UI on it.
            let (tx, rx) = async_channel::bounded(1);
            std::thread::spawn(move || {
                let _ = tx.send_blocking(core_flutter::boot_ios_simulator(&udid));
            });
            cx.spawn(async move |weak, cx| {
                let Ok(res) = rx.recv().await else { return };
                let _ = weak.update(cx, |this, cx| {
                    if let Err(err) = res {
                        this.push_flutter_log(&format!("[simulator] {err}"));
                        this.sim.status = Some(format!("Couldn't boot the simulator: {err}"));
                    }
                    this.sim.stream_retry_at = None;
                    this.start_simulator_stream(cx);
                    this.refresh_simulator_screenshot(cx);
                });
            })
            .detach();
        }
        self.start_simulator_stream(cx);
        self.refresh_simulator_screenshot(cx);
        cx.notify();
    }

    /// Detach the embedded simulator: stop the helper and its stream, clear the frame and close
    /// the panel. Attaching is always an explicit action (the titlebar Simulator button).
    pub(crate) fn detach_simulator(&mut self, cx: &mut Context<Self>) {
        core_flutter::simulator_detach();
        self.sim.attached = false;
        self.sim.touching = false;
        self.sim.status = None;
        self.agent_open = false;
        if let Some(old) = self.sim.frame.take() {
            cx.drop_image(old, None);
        }
        cx.notify();
    }

    /// Start mirroring the simulator from `shame-sim`'s live framebuffer stream. Retried (at most
    /// every few seconds) from the live loop, e.g. when the simulator was still booting; until it
    /// is up the panel falls back to `simctl` screenshots.
    pub(crate) fn start_simulator_stream(&mut self, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        if self.sim.streaming || self.sim.stream_retry_at.is_some_and(|t| now < t) {
            return;
        }
        self.sim.stream_retry_at = Some(now + std::time::Duration::from_secs(1));
        let udid = self.active_simulator_udid();
        let Ok(stream) = core_flutter::simulator_frame_stream(udid.as_deref()) else {
            return;
        };
        self.sim.streaming = true;
        cx.spawn(async move |weak, cx| {
            while stream.ready.recv().await.is_ok() {
                let frame = stream
                    .latest
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                // The helper sends BGRA, which is what GPUI wants, so the bytes go in as-is.
                let Some(frame) = frame.and_then(|f| {
                    image::RgbaImage::from_raw(f.width, f.height, f.bgra)
                        .map(|img| RenderImage::new(vec![image::Frame::new(img)]))
                }) else {
                    continue;
                };
                let alive = weak
                    .update(cx, |this, cx| {
                        if let Some(old) = this.sim.frame.replace(std::sync::Arc::new(frame)) {
                            cx.drop_image(old, None);
                        }
                        this.sim.status = None;
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    return;
                }
            }
            let _ = weak.update(cx, |this, _| this.sim.streaming = false);
        })
        .detach();
    }

    /// Capture a fresh frame from the running iOS Simulator (`xcrun simctl io screenshot`),
    /// single-flight guarded. The JPEG is decoded off the UI thread straight into a GPUI
    /// `RenderImage` — no per-frame files and no path-keyed image cache, which is what used to
    /// show stale or black frames. Identical frames (byte hash) are skipped so static screens
    /// cause no texture uploads, and the replaced frame's atlas texture is dropped.
    pub(crate) fn refresh_simulator_screenshot(&mut self, cx: &mut Context<Self>) {
        // The live stream already keeps the frame current; screenshots are only the fallback.
        if self.sim.streaming {
            return;
        }
        if self
            .sim
            .capturing
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_err()
        {
            return;
        }

        let udid = self.active_simulator_udid();
        let last_hash = self.sim.last_hash;
        let has_frame = self.sim.frame.is_some();
        let capturing_flag = self.sim.capturing.clone();

        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let stage_path = std::env::temp_dir().join("dart_ide_sim_stage.jpg");
            let result = core_flutter::capture_simulator_screenshot(udid.as_deref(), &stage_path)
                .ok()
                .and_then(|_| std::fs::read(&stage_path).ok())
                .and_then(|bytes| {
                    let mut hash: u64 = 0xcbf29ce484222325;
                    for &b in bytes.iter().step_by(4) {
                        hash ^= b as u64;
                        hash = hash.wrapping_mul(0x100000001b3);
                    }
                    hash ^= bytes.len() as u64;
                    if has_frame && hash == last_hash {
                        return None;
                    }
                    let mut rgba =
                        image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)
                            .ok()?
                            .into_rgba8();
                    // GPUI renders BGRA.
                    for px in rgba.pixels_mut() {
                        px.0.swap(0, 2);
                    }
                    Some((RenderImage::new(vec![image::Frame::new(rgba)]), hash))
                });
            capturing_flag.store(false, std::sync::atomic::Ordering::SeqCst);
            let _ = tx.send_blocking(result);
        });

        cx.spawn(async move |weak, cx| {
            let Ok(frame_opt) = rx.recv().await else {
                return;
            };
            let _ = weak.update(cx, |this, cx| {
                if let Some((frame, hash)) = frame_opt {
                    this.sim.last_hash = hash;
                    if let Some(old) = this.sim.frame.replace(std::sync::Arc::new(frame)) {
                        cx.drop_image(old, None);
                    }
                    this.sim.status = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Forward one phase of a touch at normalized `(norm_x, norm_y)` inside the embedded iPhone
    /// screen to the simulator (via the long-running `shame-sim` helper). `Down` → `Move`… → `Up`
    /// is a drag; `Down` → `Up` is a tap. Frames are refreshed while the finger is down and for
    /// a short while after lifting, to catch the resulting animation.
    pub(crate) fn simulator_touch(
        &mut self,
        phase: core_flutter::TouchPhase,
        norm_x: f32,
        norm_y: f32,
        cx: &mut Context<Self>,
    ) {
        use core_flutter::TouchPhase;
        match phase {
            TouchPhase::Down => self.sim.touching = true,
            TouchPhase::Up if !self.sim.touching => return,
            TouchPhase::Up => self.sim.touching = false,
            TouchPhase::Move if !self.sim.touching => return,
            TouchPhase::Move => {}
        }
        let udid = self.active_simulator_udid();
        if let Err(err) = core_flutter::simulator_touch(udid.as_deref(), phase, norm_x, norm_y) {
            self.sim.touching = false;
            self.push_flutter_log(&format!("[simulator] touch failed: {err}"));
            return;
        }
        self.refresh_simulator_screenshot(cx);
        if phase == TouchPhase::Up {
            let (tx, rx) = async_channel::bounded::<()>(6);
            std::thread::spawn(move || {
                for delay_ms in [60u64, 120, 200, 300, 500] {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    let _ = tx.send_blocking(());
                }
            });
            cx.spawn(async move |weak, cx| {
                while rx.recv().await.is_ok() {
                    let _ = weak.update(cx, |this, cx| this.refresh_simulator_screenshot(cx));
                }
            })
            .detach();
        }
    }

    /// UDID of the simulator currently mirrored in the rail (selected iOS device, else the first booted one).
    pub(crate) fn active_simulator_udid(&self) -> Option<String> {
        self.selected_device
            .as_ref()
            .filter(|d| d.is_ios())
            .map(|d| d.id.clone())
            .or_else(|| {
                self.ios_simulators
                    .iter()
                    .find(|s| s.is_booted())
                    .map(|s| s.udid.clone())
            })
    }

    /// Press the iOS Home button and capture the resulting frames.
    pub(crate) fn press_simulator_home(&mut self, cx: &mut Context<Self>) {
        let udid = self.active_simulator_udid();
        std::thread::spawn(move || {
            let _ = core_flutter::simulator_press_home(udid.as_deref());
        });
        let (tx, rx) = async_channel::bounded::<()>(4);
        std::thread::spawn(move || {
            for delay_ms in [150u64, 250, 450] {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                let _ = tx.send_blocking(());
            }
        });
        cx.spawn(async move |weak, cx| {
            while rx.recv().await.is_ok() {
                let _ = weak.update(cx, |this, cx| {
                    this.refresh_simulator_screenshot(cx);
                });
            }
        })
        .detach();
    }

    /// Periodically refresh the Attached Simulator frame when the Simulator rail
    /// is open and `simulator_live_sync` is enabled.
    pub(crate) fn start_simulator_live_loop(&mut self, cx: &mut Context<Self>) {
        let (tx, rx) = async_channel::bounded::<()>(1);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(80));
                if tx.send_blocking(()).is_err() {
                    break;
                }
            }
        });
        cx.spawn(async move |weak, cx| {
            while rx.recv().await.is_ok() {
                let keep_going = weak
                    .update(cx, |this, cx| {
                        if this.agent_open
                            && this.right_rail_tab == RightRailTab::Simulator
                            && this.sim.live_sync
                            && this.sim.attached
                        {
                            this.start_simulator_stream(cx);
                            this.refresh_simulator_screenshot(cx);
                        }
                    })
                    .is_ok();
                if !keep_going {
                    break;
                }
            }
        })
        .detach();
    }
}
