//! The live web preview's engine: a headless Chrome driven over the Chrome
//! DevTools Protocol (CDP).
//!
//! Chrome is started with `--remote-debugging-pipe`, which speaks CDP over
//! two plain pipes (fd 3 in, fd 4 out) with NUL-separated JSON messages, so
//! no WebSocket stack is needed. The page is streamed with
//! `Page.startScreencast`: each JPEG frame is acknowledged at once and handed
//! to a decoder thread that only ever keeps the newest one, so a slow decode
//! drops frames instead of falling behind. Input goes the other way as
//! `Input.dispatch*Event` calls.
//!
//! Nothing here touches the UI beyond producing `RenderImage`s; the panel in
//! `rails/web_preview.rs` owns layout and turns its events into calls here.
//! When the IDE exits, the pipe closes and Chrome exits with it.
//!
//! The visible preview now uses the system WebView (`rails/web_preview.rs`);
//! this headless engine is kept for driving pages without showing them
//! (agent tools, console and network capture), so parts are unused for now.
#![allow(dead_code)]

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::FromRawFd;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use base64::Engine as _;
use gpui_kit::RenderImage;
use serde_json::{Value, json};

/// What the browser tells the panel.
pub(crate) enum BrowserEvent {
    /// A new frame, and the page size (CSS pixels) it shows.
    Frame { image: RenderImage, page_width: f32, page_height: f32 },
    /// The top frame now shows `url`.
    Navigated(String),
    Title(String),
    Loading(bool),
    Error(String),
    /// Chrome went away (crashed, or was closed).
    Exited,
}

/// Mouse button state for an input event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MouseKind {
    Pressed,
    Released,
    Moved,
}

/// A running Chrome with one page attached.
pub(crate) struct Browser {
    child: Mutex<Child>,
    writer: Mutex<File>,
    next_id: AtomicU64,
    /// CDP session of the page tab, once attached.
    session: Mutex<Option<String>>,
    pending: Mutex<HashMap<u64, mpsc::Sender<Result<Value, String>>>>,
    /// Profile directory to delete on exit (private mode only).
    temp_profile: Option<PathBuf>,
    /// A screenshot is being taken / the page changed since it started.
    capturing: std::sync::atomic::AtomicBool,
    dirty: std::sync::atomic::AtomicBool,
}

/// The first Chromium-family browser installed, for the headless engine.
pub(crate) fn find_chrome() -> Option<&'static str> {
    const CANDIDATES: &[&str] = &[
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Vivaldi.app/Contents/MacOS/Vivaldi",
    ];
    CANDIDATES.iter().copied().find(|p| Path::new(p).is_file())
}

/// Where the preview keeps its Chrome profile: a fresh temporary one per
/// session (private), or a lasting one so sign-ins stick.
pub(crate) fn profile_dir(keep_logins: bool) -> (PathBuf, bool) {
    if keep_logins {
        let home = std::env::var("HOME").unwrap_or_default();
        (
            PathBuf::from(home).join("Library/Application Support/shame/preview-profile"),
            false,
        )
    } else {
        (
            std::env::temp_dir().join(format!("shame-preview-{}", std::process::id())),
            true,
        )
    }
}

impl Browser {
    /// Start Chrome, open one page sized `width`×`height` (CSS pixels) at
    /// `scale`, and begin streaming it. Events arrive on the returned channel.
    pub(crate) fn launch(
        chrome: &str,
        keep_logins: bool,
        width: u32,
        height: u32,
        scale: f32,
    ) -> std::io::Result<(
        Arc<Self>,
        async_channel::Sender<BrowserEvent>,
        async_channel::Receiver<BrowserEvent>,
    )> {
        let (profile, temporary) = profile_dir(keep_logins);
        let _ = std::fs::create_dir_all(&profile);

        // Two pipes: we write CDP commands into `to_chrome`, Chrome writes
        // replies and events into `from_chrome`.
        let mut to_chrome = [0i32; 2];
        let mut from_chrome = [0i32; 2];
        // SAFETY: plain pipe(2) calls on stack arrays; failures are checked.
        unsafe {
            if libc::pipe(to_chrome.as_mut_ptr()) != 0 || libc::pipe(from_chrome.as_mut_ptr()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Our ends must not leak into Chrome, or it would never see EOF.
            libc::fcntl(to_chrome[1], libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(from_chrome[0], libc::F_SETFD, libc::FD_CLOEXEC);
        }
        let (child_in, child_out) = (to_chrome[0], from_chrome[1]);

        let mut command = Command::new(chrome);
        command
            .args([
                "--headless=new",
                "--remote-debugging-pipe",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-gpu",
                "--mute-audio",
                "--disable-features=Translate,MediaRouter",
                &format!("--user-data-dir={}", profile.display()),
                "about:blank",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: only async-signal-safe dup2 calls between fork and exec.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(child_in, 3) < 0 || libc::dup2(child_out, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let spawned = command.spawn();
        // SAFETY: Chrome holds its own copies now; ours are no longer needed.
        unsafe {
            libc::close(child_in);
            libc::close(child_out);
        }
        let child = match spawned {
            Ok(child) => child,
            Err(err) => {
                // SAFETY: closing the fds we still own.
                unsafe {
                    libc::close(to_chrome[1]);
                    libc::close(from_chrome[0]);
                }
                return Err(err);
            }
        };
        // SAFETY: each fd is ours alone from here on.
        let writer = unsafe { File::from_raw_fd(to_chrome[1]) };
        let reader = unsafe { File::from_raw_fd(from_chrome[0]) };

        let browser = Arc::new(Self {
            child: Mutex::new(child),
            writer: Mutex::new(writer),
            next_id: AtomicU64::new(1),
            session: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            temp_profile: temporary.then_some(profile),
            capturing: std::sync::atomic::AtomicBool::new(false),
            dirty: std::sync::atomic::AtomicBool::new(false),
        });
        let (events, rx) = async_channel::unbounded();
        let frames = FrameSlot::new();
        spawn_decoder(frames.clone(), events.clone());
        spawn_reader(browser.clone(), reader, frames, events.clone());

        let setup = browser.clone();
        let setup_events = events.clone();
        std::thread::spawn(move || {
            if let Err(err) = setup.attach_page(width, height, scale) {
                let _ = setup_events.send_blocking(BrowserEvent::Error(err));
            }
        });
        Ok((browser, events, rx))
    }

    /// Create the page tab, attach to it, and start the screencast.
    fn attach_page(&self, width: u32, height: u32, scale: f32) -> Result<(), String> {
        let target = self.call_wait("Target.createTarget", json!({ "url": "about:blank" }), None)?;
        let target_id = target["targetId"].as_str().ok_or("Chrome opened no page")?.to_string();
        let attached = self.call_wait(
            "Target.attachToTarget",
            json!({ "targetId": target_id, "flatten": true }),
            None,
        )?;
        let session = attached["sessionId"].as_str().ok_or("Could not attach to the page")?;
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(session.to_string());
        let s = Some(session);
        self.call_wait("Page.enable", json!({}), s)?;
        self.set_viewport(width, height, scale, false);
        Ok(())
    }

    fn write(&self, message: &Value) -> bool {
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let mut bytes = serde_json::to_vec(message).unwrap_or_default();
        bytes.push(0);
        writer.write_all(&bytes).and_then(|_| writer.flush()).is_ok()
    }

    fn message(&self, method: &str, params: Value, session: Option<&str>) -> (u64, Value) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        (id, message)
    }

    /// A command and its reply, waiting at most a few seconds (setup only).
    fn call_wait(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value, String> {
        let (id, message) = self.message(method, params, session);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
        if !self.write(&message) {
            return Err("Chrome is not running".into());
        }
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| format!("Chrome did not answer {method}"))?
    }

    /// A page command whose reply is handled on a background thread.
    fn call_page(&self, method: &str, params: Value) -> Option<mpsc::Receiver<Result<Value, String>>> {
        let session = self.session.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        let (id, message) = self.message(method, params, Some(&session));
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
        self.write(&message).then_some(rx)
    }

    /// A fire-and-forget command to the page.
    fn send(&self, method: &str, params: Value) {
        let session = self.session.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(session) = session {
            let (_, message) = self.message(method, params, Some(&session));
            self.write(&message);
        }
    }

    pub(crate) fn is_attached(&self) -> bool {
        self.session.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// Go to `url`; a failure (no server on that port, bad host) comes back
    /// as an `Error` event.
    pub(crate) fn navigate(&self, url: &str, events: async_channel::Sender<BrowserEvent>) {
        let Some(reply) = self.call_page("Page.navigate", json!({ "url": url })) else {
            return;
        };
        let url = url.to_string();
        std::thread::spawn(move || {
            if let Ok(Ok(result)) = reply.recv_timeout(Duration::from_secs(30))
                && let Some(error) = result["errorText"].as_str()
            {
                let message = match error {
                    "net::ERR_CONNECTION_REFUSED" => format!("Nothing is running at {url}"),
                    "net::ERR_NAME_NOT_RESOLVED" => format!("Couldn't find {url}"),
                    "net::ERR_FILE_NOT_FOUND" => format!("No such file: {url}"),
                    other => format!("Couldn't load {url} ({other})"),
                };
                let _ = events.send_blocking(BrowserEvent::Error(message));
            }
        });
    }

    /// Take a full-resolution screenshot of the page into `frames`. One at
    /// a time: a change during a capture triggers one more afterwards.
    fn capture(self: &Arc<Self>, frames: FrameSlot, page: (f32, f32)) {
        self.dirty.store(true, Ordering::SeqCst);
        if self.capturing.swap(true, Ordering::SeqCst) {
            return;
        }
        let browser = Arc::downgrade(self);
        std::thread::spawn(move || {
            loop {
                let Some(this) = browser.upgrade() else { return };
                this.dirty.store(false, Ordering::SeqCst);
                let reply = this.call_page(
                    "Page.captureScreenshot",
                    json!({ "format": "jpeg", "quality": 92, "optimizeForSpeed": true }),
                );
                if let Some(Ok(Ok(shot))) = reply.map(|r| r.recv_timeout(Duration::from_secs(5)))
                    && let Some(data) = shot["data"].as_str()
                {
                    frames.put(data.to_string(), page.0, page.1);
                }
                if !this.dirty.load(Ordering::SeqCst) {
                    this.capturing.store(false, Ordering::SeqCst);
                    // A change may have landed between the check and the store.
                    if !this.dirty.load(Ordering::SeqCst) || this.capturing.swap(true, Ordering::SeqCst) {
                        return;
                    }
                }
            }
        });
    }

    pub(crate) fn reload(&self) {
        self.send("Page.reload", json!({}));
    }

    pub(crate) fn go_back(&self) {
        self.send("Runtime.evaluate", json!({ "expression": "history.back()" }));
    }

    pub(crate) fn go_forward(&self) {
        self.send("Runtime.evaluate", json!({ "expression": "history.forward()" }));
    }

    /// Size the page to `width`×`height` CSS pixels, rendered at `scale` so
    /// frames are sharp on a Retina panel.
    pub(crate) fn set_viewport(&self, width: u32, height: u32, scale: f32, mobile: bool) {
        let scale = scale.max(1.0);
        self.send(
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": width.max(1),
                "height": height.max(1),
                "deviceScaleFactor": scale,
                "mobile": mobile,
            }),
        );
        self.send("Emulation.setTouchEmulationEnabled", json!({ "enabled": mobile }));
        // The screencast only says *when* the page changed (its frames are
        // CSS-sized, blurry on Retina); `capture` takes the sharp picture.
        self.send(
            "Page.startScreencast",
            json!({ "format": "jpeg", "quality": 30, "everyNthFrame": 1 }),
        );
    }

    pub(crate) fn mouse(&self, kind: MouseKind, x: f64, y: f64, clicks: usize, held: bool, modifiers: i64) {
        let (kind, button) = match kind {
            MouseKind::Pressed => ("mousePressed", "left"),
            MouseKind::Released => ("mouseReleased", "left"),
            MouseKind::Moved => ("mouseMoved", if held { "left" } else { "none" }),
        };
        self.send(
            "Input.dispatchMouseEvent",
            json!({
                "type": kind,
                "x": x,
                "y": y,
                "button": button,
                "buttons": if held || kind == "mousePressed" { 1 } else { 0 },
                "clickCount": clicks.max(1),
                "modifiers": modifiers,
            }),
        );
    }

    pub(crate) fn wheel(&self, x: f64, y: f64, dx: f64, dy: f64) {
        self.send(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": dx, "deltaY": dy }),
        );
    }

    pub(crate) fn key(&self, action: &KeyAction) {
        match action {
            KeyAction::Text(text) => {
                self.send(
                    "Input.dispatchKeyEvent",
                    json!({ "type": "keyDown", "key": text, "text": text, "unmodifiedText": text }),
                );
                self.send("Input.dispatchKeyEvent", json!({ "type": "keyUp", "key": text }));
            }
            KeyAction::Key { key, code, text, modifiers, commands } => {
                let mut down = json!({
                    "type": if text.is_some() { "keyDown" } else { "rawKeyDown" },
                    "key": key,
                    "windowsVirtualKeyCode": code,
                    "modifiers": modifiers,
                    "commands": commands,
                });
                if let Some(text) = text {
                    down["text"] = json!(text);
                }
                self.send("Input.dispatchKeyEvent", down);
                self.send(
                    "Input.dispatchKeyEvent",
                    json!({ "type": "keyUp", "key": key, "windowsVirtualKeyCode": code, "modifiers": modifiers }),
                );
            }
            KeyAction::Paste | KeyAction::Copy | KeyAction::Cut => {}
        }
    }

    pub(crate) fn insert_text(&self, text: &str) {
        self.send("Input.insertText", json!({ "text": text }));
    }

    /// The page's selected text, for ⌘C. Blocks briefly; call off the UI
    /// thread.
    pub(crate) fn selected_text(&self) -> Option<String> {
        let reply = self.call_page(
            "Runtime.evaluate",
            json!({ "expression": "window.getSelection().toString()", "returnByValue": true }),
        )?;
        let value = reply.recv_timeout(Duration::from_secs(2)).ok()?.ok()?;
        value["result"]["value"].as_str().map(str::to_string).filter(|s| !s.is_empty())
    }

    pub(crate) fn delete_selection(&self) {
        self.send("Input.dispatchKeyEvent", json!({ "type": "rawKeyDown", "key": "Backspace", "windowsVirtualKeyCode": 8, "commands": ["delete"] }));
        self.send("Input.dispatchKeyEvent", json!({ "type": "keyUp", "key": "Backspace", "windowsVirtualKeyCode": 8 }));
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
        if let Some(profile) = &self.temp_profile {
            let _ = std::fs::remove_dir_all(profile);
        }
    }
}

/// A base64 JPEG frame and the page size it shows.
type PendingFrame = (String, f32, f32);

/// The newest undecoded frame, shared between the reader and the decoder.
#[derive(Clone)]
struct FrameSlot(Arc<(Mutex<Option<PendingFrame>>, Condvar)>);

impl FrameSlot {
    fn new() -> Self {
        Self(Arc::new((Mutex::new(None), Condvar::new())))
    }

    /// Replace whatever was waiting: an older frame is never worth decoding.
    fn put(&self, data: String, width: f32, height: f32) {
        let (slot, ready) = &*self.0;
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some((data, width, height));
        ready.notify_one();
    }

    /// The next frame, or `None` after a second with none (so the decoder
    /// can notice the panel has gone).
    fn take(&self) -> Option<PendingFrame> {
        let (slot, ready) = &*self.0;
        let guard = slot.lock().unwrap_or_else(|e| e.into_inner());
        let (mut guard, _) = ready
            .wait_timeout_while(guard, Duration::from_secs(1), |frame| frame.is_none())
            .unwrap_or_else(|e| e.into_inner());
        guard.take()
    }
}

fn spawn_decoder(frames: FrameSlot, events: async_channel::Sender<BrowserEvent>) {
    std::thread::spawn(move || {
        while !events.is_closed() {
            let Some((data, page_width, page_height)) = frames.take() else { continue };
            let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else {
                continue;
            };
            let Ok(decoded) = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)
            else {
                continue;
            };
            let mut rgba = decoded.into_rgba8();
            // GPUI renders BGRA.
            for px in rgba.pixels_mut() {
                px.0.swap(0, 2);
            }
            let image = RenderImage::new(vec![image::Frame::new(rgba)]);
            if events
                .send_blocking(BrowserEvent::Frame { image, page_width, page_height })
                .is_err()
            {
                return;
            }
        }
    });
}

fn spawn_reader(
    browser: Arc<Browser>,
    reader: File,
    frames: FrameSlot,
    events: async_channel::Sender<BrowserEvent>,
) {
    // Hold the browser weakly: the reader must not keep Chrome alive after
    // the panel lets go of it.
    let browser = Arc::downgrade(&browser);
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        let mut main_frame: Option<String> = None;
        loop {
            buf.clear();
            match reader.read_until(0, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if buf.last() == Some(&0) {
                buf.pop();
            }
            let Ok(message) = serde_json::from_slice::<Value>(&buf) else { continue };
            let Some(browser) = browser.upgrade() else { break };

            if let Some(id) = message["id"].as_u64() {
                let reply = match message.get("error") {
                    Some(error) => Err(error["message"].as_str().unwrap_or("CDP error").to_string()),
                    None => Ok(message["result"].clone()),
                };
                if let Some(tx) = browser.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id) {
                    let _ = tx.send(reply);
                }
                continue;
            }
            let params = &message["params"];
            let event = match message["method"].as_str().unwrap_or_default() {
                "Page.screencastFrame" => {
                    // Ack first: Chrome sends the next frame only after this.
                    browser.send(
                        "Page.screencastFrameAck",
                        json!({ "sessionId": params["sessionId"] }),
                    );
                    let meta = &params["metadata"];
                    let page = (
                        meta["deviceWidth"].as_f64().unwrap_or(0.0) as f32,
                        meta["deviceHeight"].as_f64().unwrap_or(0.0) as f32,
                    );
                    browser.capture(frames.clone(), page);
                    None
                }
                "Page.frameNavigated" if params["frame"]["parentId"].is_null() => {
                    main_frame = params["frame"]["id"].as_str().map(str::to_string);
                    params["frame"]["url"].as_str().map(|u| BrowserEvent::Navigated(u.to_string()))
                }
                "Page.frameStartedLoading" if params["frameId"].as_str() == main_frame.as_deref() => {
                    Some(BrowserEvent::Loading(true))
                }
                "Page.frameStoppedLoading" if params["frameId"].as_str() == main_frame.as_deref() => {
                    // Tab info reports the URL as the title for some pages,
                    // so ask the page itself once it has loaded.
                    if let Some(reply) = browser.call_page(
                        "Runtime.evaluate",
                        json!({ "expression": "document.title", "returnByValue": true }),
                    ) {
                        let events = events.clone();
                        std::thread::spawn(move || {
                            if let Ok(Ok(value)) = reply.recv_timeout(Duration::from_secs(5))
                                && let Some(title) = value["result"]["value"].as_str()
                            {
                                let _ = events.send_blocking(BrowserEvent::Title(title.to_string()));
                            }
                        });
                    }
                    Some(BrowserEvent::Loading(false))
                }
                _ => None,
            };
            if let Some(event) = event
                && events.send_blocking(event).is_err()
            {
                break;
            }
        }
        let _ = events.send_blocking(BrowserEvent::Exited);
    });
}

/// A key press as the page should receive it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeyAction {
    /// A printable character.
    Text(String),
    /// A named key, with its Windows virtual key code (CDP wants it), the
    /// text it types if any, the CDP modifier bits, and editing commands for
    /// ⌘-shortcuts (which macOS handles through the menu, not the page).
    Key { key: String, code: i64, text: Option<String>, modifiers: i64, commands: Vec<String> },
    Copy,
    Cut,
    Paste,
}

/// CDP modifier bits: Alt=1, Ctrl=2, Meta(⌘)=4, Shift=8.
pub(crate) fn modifier_bits(alt: bool, ctrl: bool, cmd: bool, shift: bool) -> i64 {
    (alt as i64) | ((ctrl as i64) << 1) | ((cmd as i64) << 2) | ((shift as i64) << 3)
}

/// Map a GPUI key press to what the page should receive, or `None` for keys
/// the IDE keeps (⌘P and the like).
pub(crate) fn map_key(
    key: &str,
    key_char: Option<&str>,
    shift: bool,
    ctrl: bool,
    alt: bool,
    cmd: bool,
) -> Option<KeyAction> {
    let modifiers = modifier_bits(alt, ctrl, cmd, shift);
    let named = |name: &str, code: i64, text: Option<&str>, commands: &[&str]| KeyAction::Key {
        key: name.to_string(),
        code,
        text: text.map(str::to_string),
        modifiers,
        commands: commands.iter().map(|c| c.to_string()).collect(),
    };
    if cmd && !ctrl {
        return match (key, shift) {
            ("c", false) => Some(KeyAction::Copy),
            ("x", false) => Some(KeyAction::Cut),
            ("v", false) => Some(KeyAction::Paste),
            ("a", false) => Some(named("a", 65, None, &["selectAll"])),
            ("z", false) => Some(named("z", 90, None, &["undo"])),
            ("z", true) => Some(named("z", 90, None, &["redo"])),
            ("left", _) => Some(named("ArrowLeft", 37, None, &[if shift { "moveToBeginningOfLineAndModifySelection" } else { "moveToBeginningOfLine" }])),
            ("right", _) => Some(named("ArrowRight", 39, None, &[if shift { "moveToEndOfLineAndModifySelection" } else { "moveToEndOfLine" }])),
            ("backspace", _) => Some(named("Backspace", 8, None, &["deleteToBeginningOfLine"])),
            _ => None,
        };
    }
    let special = match key {
        "enter" => Some(named("Enter", 13, Some("\r"), &[])),
        "backspace" => Some(named("Backspace", 8, None, &[])),
        "delete" => Some(named("Delete", 46, None, &[])),
        "tab" => Some(named("Tab", 9, None, &[])),
        "escape" => Some(named("Escape", 27, None, &[])),
        "left" => Some(named("ArrowLeft", 37, None, &[])),
        "up" => Some(named("ArrowUp", 38, None, &[])),
        "right" => Some(named("ArrowRight", 39, None, &[])),
        "down" => Some(named("ArrowDown", 40, None, &[])),
        "home" => Some(named("Home", 36, None, &[])),
        "end" => Some(named("End", 35, None, &[])),
        "pageup" => Some(named("PageUp", 33, None, &[])),
        "pagedown" => Some(named("PageDown", 34, None, &[])),
        "space" if ctrl || alt => Some(named(" ", 32, None, &[])),
        _ => None,
    };
    if special.is_some() {
        return special;
    }
    if ctrl {
        // Ctrl+letter (emacs-style movement in text fields) as a raw key.
        let letter = key.chars().next().filter(|c| c.is_ascii_alphabetic() && key.len() == 1)?;
        return Some(named(key, letter.to_ascii_uppercase() as i64, None, &[]));
    }
    key_char
        .filter(|t| !t.is_empty() && !t.chars().any(char::is_control))
        .map(|t| KeyAction::Text(t.to_string()))
}

/// Where a point in the panel lands on the page: `view` is where the frame
/// is drawn (origin and size, panel pixels), `page` the page size in CSS
/// pixels. `None` outside the frame.
pub(crate) fn to_page(
    point: (f32, f32),
    view_origin: (f32, f32),
    view_size: (f32, f32),
    page: (f32, f32),
) -> Option<(f64, f64)> {
    let (rx, ry) = (point.0 - view_origin.0, point.1 - view_origin.1);
    if view_size.0 <= 1.0 || view_size.1 <= 1.0 || rx < 0.0 || ry < 0.0 || rx > view_size.0 || ry > view_size.1 {
        return None;
    }
    Some(((rx / view_size.0 * page.0) as f64, (ry / view_size.1 * page.1) as f64))
}

/// The largest rectangle with the page's aspect that fits `area`, centred:
/// `(origin, size)` relative to the area.
pub(crate) fn fit_rect(area: (f32, f32), page: (f32, f32)) -> ((f32, f32), (f32, f32)) {
    if page.0 <= 0.0 || page.1 <= 0.0 {
        return ((0.0, 0.0), area);
    }
    let scale = (area.0 / page.0).min(area.1 / page.1);
    let size = (page.0 * scale, page.1 * scale);
    (((area.0 - size.0) / 2.0, (area.1 - size.1) / 2.0), size)
}

/// Turn what was typed in the URL bar into a URL: `:3000` and `localhost`
/// get `http://`, a path in the project becomes `file://`, anything else
/// with a dot gets `https://`.
pub(crate) fn normalize_url(raw: &str, root: &Path) -> String {
    let raw = raw.trim();
    if raw.contains("://") || raw.starts_with("about:") {
        raw.to_string()
    } else if raw.starts_with(':') {
        format!("http://localhost{raw}")
    } else if raw.starts_with("localhost") || raw.starts_with("127.0.0.1") {
        format!("http://{raw}")
    } else if root.join(raw).is_file() {
        format!("file://{}", root.join(raw).display())
    } else if Path::new(raw).is_absolute() && Path::new(raw).is_file() {
        format!("file://{raw}")
    } else {
        format!("https://{raw}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn keys_map_to_what_the_page_expects() {
        assert_eq!(map_key("a", Some("A"), true, false, false, false), Some(KeyAction::Text("A".into())));
        assert_eq!(map_key("v", None, false, false, false, true), Some(KeyAction::Paste));
        assert!(matches!(
            map_key("enter", Some("\n"), false, false, false, false),
            Some(KeyAction::Key { ref key, code: 13, .. }) if key == "Enter"
        ));
        assert!(matches!(
            map_key("a", None, false, false, false, true),
            Some(KeyAction::Key { ref commands, modifiers: 4, .. }) if commands == &["selectAll"]
        ));
        // IDE shortcuts stay with the IDE.
        assert_eq!(map_key("p", None, false, false, false, true), None);
        assert_eq!(modifier_bits(true, true, true, true), 15);
    }

    #[core::prelude::v1::test]
    fn panel_points_map_onto_the_page() {
        assert_eq!(to_page((60.0, 30.0), (10.0, 10.0), (100.0, 50.0), (400.0, 200.0)), Some((200.0, 80.0)));
        assert_eq!(to_page((5.0, 30.0), (10.0, 10.0), (100.0, 50.0), (400.0, 200.0)), None);
        // A phone page in a wide panel: centred, full height.
        assert_eq!(fit_rect((800.0, 844.0), (390.0, 844.0)), ((205.0, 0.0), (390.0, 844.0)));
    }

    #[core::prelude::v1::test]
    fn typed_addresses_become_urls() {
        let root = Path::new("/nonexistent-root");
        assert_eq!(normalize_url(":5173", root), "http://localhost:5173");
        assert_eq!(normalize_url("localhost:8080/x", root), "http://localhost:8080/x");
        assert_eq!(normalize_url("example.com", root), "https://example.com");
        assert_eq!(normalize_url("file:///a.html", root), "file:///a.html");
    }

    /// Drives a real Chrome: launch, load a page, get a decoded frame and its
    /// title. Needs Chrome installed; run with `--ignored`.
    #[core::prelude::v1::test]
    #[ignore]
    fn a_real_chrome_streams_a_page() {
        let chrome = find_chrome().expect("Chrome installed");
        let (browser, sender, events) = Browser::launch(chrome, false, 400, 300, 2.0).expect("launch");
        for _ in 0..100 {
            if browser.is_attached() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        eprintln!("attached: {}", browser.is_attached());
        assert!(browser.is_attached(), "attached to a page");
        browser.navigate("data:text/html,<title>Hi</title><h1>hello</h1>", sender);
        let (mut frame, mut title) = (None, None);
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while std::time::Instant::now() < deadline && (frame.is_none() || title.is_none()) {
            match events.try_recv() {
                Ok(BrowserEvent::Frame { page_width, page_height, image }) => {
                    let size = image.size(0);
                    eprintln!("frame pixels: {}x{}", size.width.0, size.height.0);
                    frame = Some((page_width, page_height))
                }
                Ok(BrowserEvent::Title(t)) => {
                    eprintln!("title: {t}");
                    if t == "Hi" {
                        title = Some(t);
                    }
                }
                Ok(BrowserEvent::Navigated(u)) => eprintln!("navigated: {u}"),
                Ok(BrowserEvent::Loading(l)) => eprintln!("loading: {l}"),
                Ok(BrowserEvent::Error(e)) => eprintln!("error: {e}"),
                Ok(BrowserEvent::Exited) => break,
                Err(async_channel::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => break,
            }
        }
        // Later frames, after the viewport settles.
        let settle = std::time::Instant::now() + Duration::from_secs(2);
        browser.navigate("data:text/html,<h1 style='font-size:80px'>later</h1>", async_channel::unbounded().0);
        while std::time::Instant::now() < settle {
            if let Ok(BrowserEvent::Frame { image, .. }) = events.try_recv() {
                let size = image.size(0);
                eprintln!("later frame pixels: {}x{}", size.width.0, size.height.0);
            } else {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        assert_eq!(frame, Some((400.0, 300.0)), "a frame at the page size");
        assert_eq!(title.as_deref(), Some("Hi"));
    }

}
