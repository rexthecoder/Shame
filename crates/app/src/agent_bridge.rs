//! The live bridge between the agent CLIs and the running editor.
//!
//! The rail used to be one-way: the editor described itself in a prompt and the
//! agent could only look at the filesystem. This module lets an agent ask the
//! *open* editor what is on screen instead of guessing — the active file, the
//! exact selection, the cursor, every diagnostic in the open tabs, the terminal
//! tail, the preview URL and the simulator. The same channel can open a file at
//! a line for the user.
//!
//! The editor listens on a per-process Unix socket. Every agent process gets
//! that path in `SHAME_IDE_SOCKET` and runs the editor binary with
//! `--ide-bridge <tool>` (also exported as `SHAME_IDE_BIN`) to talk to it.

#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::Duration;

use gpui_kit::{App, Context, Window};
use serde_json::{Value, json};

use crate::DartIde;

/// Where the running editor's bridge socket lives, for agent processes.
pub(crate) const SOCKET_ENV: &str = "SHAME_IDE_SOCKET";
/// The editor binary, so the agent can invoke `--ide-bridge`.
pub(crate) const BIN_ENV: &str = "SHAME_IDE_BIN";
/// The workspace root, for the tools that read files or run `git`.
pub(crate) const ROOT_ENV: &str = "SHAME_IDE_ROOT";

/// A tool call waiting for the UI thread to answer it.
pub(crate) struct BridgeRequest {
    pub(crate) tool: String,
    pub(crate) args: Value,
    pub(crate) reply: std::sync::mpsc::SyncSender<BridgeReply>,
}

pub(crate) struct BridgeReply {
    pub(crate) ok: bool,
    pub(crate) body: Value,
}

fn answer(reply: std::sync::mpsc::SyncSender<BridgeReply>, ok: bool, body: Value) {
    let _ = reply.send(BridgeReply { ok, body });
}

/// The editor side of the bridge: the socket path and the request queue.
pub(crate) struct AgentBridge {
    pub(crate) socket: PathBuf,
}

impl AgentBridge {
    /// Bind a private socket and return the receiver the UI loop drains.
    #[cfg(unix)]
    pub(crate) fn start() -> Option<(Self, async_channel::Receiver<BridgeRequest>)> {
        let socket = std::env::temp_dir().join(format!("shame-ide-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket).ok()?;
        let _ = std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600));
        let (tx, rx) = async_channel::unbounded();
        std::thread::spawn(move || serve(listener, tx));
        Some((Self { socket }, rx))
    }

    #[cfg(not(unix))]
    pub(crate) fn start() -> Option<(Self, async_channel::Receiver<BridgeRequest>)> {
        None
    }
}

impl Drop for AgentBridge {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

#[cfg(unix)]
fn serve(listener: UnixListener, requests: async_channel::Sender<BridgeRequest>) {
    for stream in listener.incoming().flatten() {
        let requests = requests.clone();
        std::thread::spawn(move || handle_connection(stream, requests));
    }
}

#[cfg(unix)]
fn handle_connection(mut stream: UnixStream, requests: async_channel::Sender<BridgeRequest>) {
    let mut line = String::new();
    {
        let mut reader = match stream.try_clone() {
            Ok(s) => BufReader::new(s),
            Err(_) => return,
        };
        if reader.read_line(&mut line).is_err() {
            return;
        }
    }
    let parsed: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null);
    let tool = parsed.get("tool").and_then(Value::as_str).unwrap_or("").to_string();
    let args = parsed.get("args").cloned().unwrap_or(Value::Null);
    if tool.is_empty() {
        let _ = write_reply(&mut stream, false, json!("missing tool"));
        return;
    }

    let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
    if requests
        .send_blocking(BridgeRequest { tool, args, reply: reply_tx })
        .is_err()
    {
        let _ = write_reply(&mut stream, false, json!("editor is not running"));
        return;
    }
    match reply_rx.recv_timeout(Duration::from_secs(15)) {
        Ok(reply) => {
            let _ = write_reply(&mut stream, reply.ok, reply.body);
        }
        Err(_) => {
            let _ = write_reply(&mut stream, false, json!("the editor did not answer in time"));
        }
    }
}

#[cfg(unix)]
fn write_reply(stream: &mut UnixStream, ok: bool, body: Value) -> std::io::Result<()> {
    let line = serde_json::to_string(&json!({ "ok": ok, "body": body }))
        .unwrap_or_else(|_| "{\"ok\":false,\"body\":\"encode failed\"}".to_string());
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

/// True when this process was launched by an agent as the bridge client.
pub(crate) fn is_cli_invocation() -> bool {
    matches!(
        std::env::args().nth(1).as_deref(),
        Some("--ide-bridge" | "--version" | "-v" | "--help" | "-h")
    )
}

/// Run `shame --ide-bridge <tool> [args]` (or `--version` / `--help`) and print the answer.
/// Returns the process exit code, or `None` when this is a normal GUI launch.
#[cfg(unix)]
pub(crate) fn run_cli_if_requested() -> Option<i32> {
    if !is_cli_invocation() {
        return None;
    }
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-v") => {
            println!("shame {}", env!("CARGO_PKG_VERSION"));
            return Some(0);
        }
        Some("--help" | "-h") => {
            println!(
                "Shame {}\n\nUsage:\n  shame [path]                        Open workspace folder\n  shame --ide-bridge <tool> [args]    Query or drive the live editor\n  shame --version                     Print version\n  shame --help                        Print this help\n\nBridge tools:\n  context | diagnostics [path] | symbols [path] | outline [path]\n  read <path> [start] [end] | diff [--staged] | terminal [lines]\n  preview | open <path> [line] | format | hot-reload | hot-restart",
                env!("CARGO_PKG_VERSION")
            );
            return Some(0);
        }
        Some("--ide-bridge") => {}
        _ => return None,
    }
    let argv: Vec<String> = std::env::args().skip(2).collect();
    let Some(tool) = argv.first().filter(|t| !t.is_empty()).cloned() else {
        eprintln!(
            "usage: shame --ide-bridge <context|diagnostics|symbols|outline|read|diff|terminal|preview|open|format|hot-reload|hot-restart> [args]"
        );
        return Some(2);
    };
    let Some(socket) = std::env::var_os(SOCKET_ENV) else {
        eprintln!("{SOCKET_ENV} is not set; run this through Shame's agent rail");
        return Some(2);
    };
    let args = json!({ "argv": argv.iter().skip(1).collect::<Vec<_>>() });
    let request = json!({ "tool": tool, "args": args });
    match send_request(Path::new(&socket), &request) {
        Ok((true, body)) => {
            print_body(&body);
            Some(0)
        }
        Ok((false, body)) => {
            eprintln!("{}", body.as_str().unwrap_or("the bridge returned an error"));
            Some(1)
        }
        Err(err) => {
            eprintln!("{err}");
            Some(1)
        }
    }
}

#[cfg(not(unix))]
pub(crate) fn run_cli_if_requested() -> Option<i32> {
    None
}

#[cfg(unix)]
fn send_request(socket: &Path, request: &Value) -> std::io::Result<(bool, Value)> {
    let mut stream = UnixStream::connect(socket)?;
    let line = serde_json::to_string(request).unwrap_or_default();
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    reader.read_line(&mut reply)?;
    let parsed: Value = serde_json::from_str(reply.trim())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok((
        parsed.get("ok").and_then(Value::as_bool).unwrap_or(false),
        parsed.get("body").cloned().unwrap_or(Value::Null),
    ))
}

/// Text tools print their payload raw (easy to read in a terminal); the rest
/// print JSON.
fn print_body(body: &Value) {
    if let Some(text) = body.get("text").and_then(Value::as_str) {
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(body).unwrap_or_else(|_| body.to_string())
        );
    }
}

impl DartIde {
    /// Start the socket and the loop that answers bridge calls from the UI
    /// thread (so they see the same state the window is showing).
    pub(crate) fn start_agent_bridge(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((bridge, requests)) = AgentBridge::start() else {
            return;
        };
        self.agent_bridge = Some(bridge);
        cx.spawn_in(window, async move |weak, cx| {
            while let Ok(request) = requests.recv().await {
                if weak
                    .update_in(cx, |this, window, cx| {
                        this.answer_bridge_request(request, window, cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// Environment every agent process needs to find and call the bridge.
    pub(crate) fn agent_bridge_env(&self, root: &Path) -> Vec<(String, String)> {
        let mut env = vec![(ROOT_ENV.to_string(), root.display().to_string())];
        if let Some(bridge) = &self.agent_bridge {
            env.push((SOCKET_ENV.to_string(), bridge.socket.display().to_string()));
        }
        if let Ok(exe) = std::env::current_exe() {
            env.push((BIN_ENV.to_string(), exe.display().to_string()));
        }
        env
    }

    /// The prompt block that tells every harness the bridge exists.
    pub(crate) fn agent_bridge_prompt(&self) -> String {
        if self.agent_bridge.is_none() {
            return String::new();
        }
        "[Shame IDE Bridge]\n\
         The editor is live. Ask it before guessing what the user is looking at, with one of:\n  \
         \"$SHAME_IDE_BIN\" --ide-bridge context\n  \
         \"$SHAME_IDE_BIN\" --ide-bridge diagnostics [path]\n  \
         \"$SHAME_IDE_BIN\" --ide-bridge symbols [path]\n  \
         \"$SHAME_IDE_BIN\" --ide-bridge outline [path]\n  \
         \"$SHAME_IDE_BIN\" --ide-bridge read <path> [start] [end]\n  \
         \"$SHAME_IDE_BIN\" --ide-bridge diff\n  \
         \"$SHAME_IDE_BIN\" --ide-bridge terminal [lines]\n  \
         \"$SHAME_IDE_BIN\" --ide-bridge preview\n  \
         \"$SHAME_IDE_BIN\" --ide-bridge open <path> [line]\n  \
         \"$SHAME_IDE_BIN\" --ide-bridge format [path]\n  \
         \"$SHAME_IDE_BIN\" --ide-bridge hot-reload\n  \
         \"$SHAME_IDE_BIN\" --ide-bridge hot-restart\n\
         `context` returns JSON with the active file, the exact selection text, cursor, open tabs, \
         diagnostics, Flutter session status, preview URL and simulator; `symbols` and `outline` inspect \
         file declarations; `hot-reload` / `hot-restart` refresh a running Flutter app; the read tools \
         print raw text. Use `open` when you point the user at a file or line. Keep using your normal \
         tools to edit files."
            .to_string()
    }

    fn answer_bridge_request(
        &mut self,
        request: BridgeRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let BridgeRequest { tool, args, reply } = request;
        let argv = args
            .get("argv")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        match tool.as_str() {
            "context" => answer(reply, true, self.bridge_context(cx)),
            "diagnostics" => answer(
                reply,
                true,
                self.bridge_diagnostics(argv.first().map(String::as_str), cx),
            ),
            "symbols" => match self.bridge_symbols(argv.first().map(String::as_str), cx) {
                Ok(value) => answer(reply, true, value),
                Err(err) => answer(reply, false, json!(err)),
            },
            "outline" => match self.bridge_outline(argv.first().map(String::as_str), cx) {
                Ok(text) => answer(reply, true, json!({ "text": text })),
                Err(err) => answer(reply, false, json!(err)),
            },
            "terminal" => {
                let lines = argv
                    .first()
                    .and_then(|n| n.parse::<usize>().ok())
                    .unwrap_or(60)
                    .clamp(1, 500);
                let text = self
                    .terminals
                    .get(self.active_terminal)
                    .map(|t| crate::agent_ctx::terminal_text(&t.lines, lines))
                    .unwrap_or_default();
                answer(reply, true, json!({ "text": text }))
            }
            "preview" => {
                let url = self.web_preview_url.read(cx).value().to_string();
                answer(reply, true, json!({ "url": url }))
            }
            "open" => match self.bridge_open(&argv, window, cx) {
                Ok(value) => answer(reply, true, value),
                Err(err) => answer(reply, false, json!(err)),
            },
            "format" => match self.bridge_format(argv.first().map(String::as_str), cx) {
                Ok(value) => answer(reply, true, value),
                Err(err) => answer(reply, false, json!(err)),
            },
            "hot-reload" => match self.bridge_flutter_reload(false, cx) {
                Ok(value) => answer(reply, true, value),
                Err(err) => answer(reply, false, json!(err)),
            },
            "hot-restart" => match self.bridge_flutter_reload(true, cx) {
                Ok(value) => answer(reply, true, value),
                Err(err) => answer(reply, false, json!(err)),
            },
            "read" => {
                let root = self.root.clone();
                std::thread::spawn(move || {
                    let (ok, body) = match bridge_read(&root, &argv) {
                        Ok(text) => (true, json!({ "text": text })),
                        Err(err) => (false, json!(err)),
                    };
                    answer(reply, ok, body);
                });
            }
            "diff" => {
                let root = self.root.clone();
                let staged = argv.iter().any(|a| a == "--staged");
                std::thread::spawn(move || {
                    let (ok, body) = match bridge_diff(&root, staged) {
                        Ok(text) => (true, json!({ "text": text })),
                        Err(err) => (false, json!(err)),
                    };
                    answer(reply, ok, body);
                });
            }
            other => answer(reply, false, json!(format!("unknown bridge tool: {other}"))),
        }
    }

    fn bridge_open(
        &mut self,
        argv: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Value, String> {
        let raw = argv.first().ok_or("open needs a path")?;
        let path = workspace_path(&self.root, raw)?;
        if !path.is_file() {
            return Err(format!("{raw} is not a file"));
        }
        let line = argv.get(1).and_then(|n| n.parse::<usize>().ok()).unwrap_or(1);
        let rel = relative_path(&self.root, &path);
        self.open_path(path, window, cx);
        if line > 1
            && let Some(editor) = self.active_editor().cloned()
        {
            editor.update(cx, |ed, cx| {
                ed.set_cursor_to_position(line.saturating_sub(1) as u32, 0, cx);
            });
        }
        Ok(json!({ "opened": rel, "line": line }))
    }

    fn bridge_context(&self, cx: &App) -> Value {
        let active = self.tabs.get(self.active_tab);
        let active_path = active.and_then(|t| t.path.as_ref());
        let mut cursor = Value::Null;
        let mut selection = Value::Null;
        if let Some(editor) = active {
            let ed = editor.editor.read(cx);
            let (line, column) = ed.cursor_position();
            cursor = json!({ "line": line, "column": column });
            if let Some((start, end, text)) = ed.selection_for_context() {
                let (text, truncated) = clip_text(&text, 12_000);
                selection = json!({
                    "path": active_path.map(|p| relative_path(&self.root, p)),
                    "start_line": start,
                    "end_line": end,
                    "text": text,
                    "truncated": truncated,
                });
            }
        }
        let tabs: Vec<Value> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(ix, tab)| {
                let path = tab.path.as_ref()?;
                Some(json!({
                    "path": relative_path(&self.root, path),
                    "active": ix == self.active_tab,
                    "dirty": tab.editor.read(cx).buffer().is_dirty(),
                    "diagnostics": tab.editor.read(cx).diagnostics().len(),
                }))
            })
            .collect();
        let (errors, warnings) = self
            .tabs
            .iter()
            .flat_map(|t| t.editor.read(cx).diagnostics())
            .fold((0usize, 0usize), |(e, w), d| match d.severity {
                Some(core_lsp::lsp_types::DiagnosticSeverity::ERROR) => (e + 1, w),
                Some(core_lsp::lsp_types::DiagnosticSeverity::WARNING) => (e, w + 1),
                _ => (e, w),
            });
        let simulator = self
            .selected_device
            .as_ref()
            .filter(|d| d.is_ios())
            .map(|d| {
                let booted = self
                    .ios_simulators
                    .iter()
                    .find(|s| s.udid == d.id)
                    .is_some_and(|s| s.is_booted());
                json!({
                    "name": d.name,
                    "udid": d.id,
                    "booted": booted,
                    "attached": self.sim.attached,
                })
            });
        json!({
            "workspace": self.root.display().to_string(),
            "git_branch": self.git_branch,
            "active_file": active_path.map(|p| relative_path(&self.root, p)),
            "cursor": cursor,
            "selection": selection,
            "open_tabs": tabs,
            "diagnostics": { "errors": errors, "warnings": warnings },
            "flutter": {
                "running": self.flutter_app_id.is_some(),
                "app_id": self.flutter_app_id,
                "status": self.flutter_status,
            },
            "preview_url": self.web_preview_url.read(cx).value(),
            "simulator": simulator,
            "harnesses": {
                "claude": AgentHarnessProbe::found(crate::rails::agent::AgentHarness::ClaudeCode),
                "codex": AgentHarnessProbe::found(crate::rails::agent::AgentHarness::Codex),
                "opencode": AgentHarnessProbe::found(crate::rails::agent::AgentHarness::OpenCode),
                "antigravity": AgentHarnessProbe::found(crate::rails::agent::AgentHarness::Antigravity),
            },
        })
    }

    fn bridge_symbols(&self, raw_path: Option<&str>, cx: &App) -> Result<Value, String> {
        let (rel, symbols) = self.resolve_file_symbols(raw_path, cx)?;
        let items: Vec<Value> = symbols
            .into_iter()
            .map(|(name, kind, line0)| json!({ "name": name, "kind": kind, "line": line0 + 1 }))
            .collect();
        Ok(json!({ "path": rel, "symbols": items }))
    }

    fn bridge_outline(&self, raw_path: Option<&str>, cx: &App) -> Result<String, String> {
        let (rel, symbols) = self.resolve_file_symbols(raw_path, cx)?;
        Ok(format_outline(&rel, &symbols))
    }

    fn resolve_file_symbols(
        &self,
        raw_path: Option<&str>,
        cx: &App,
    ) -> Result<(String, Vec<(String, &'static str, u32)>), String> {
        if let Some(raw) = raw_path {
            let path = workspace_path(&self.root, raw)?;
            let rel = relative_path(&self.root, &path);
            if let Some(tab) = self.tabs.iter().find(|t| t.path.as_deref() == Some(path.as_path())) {
                return Ok((rel, tab.editor.read(cx).document_symbols()));
            }
            return bridge_symbols_from_disk(&self.root, raw);
        }
        let tab = self
            .tabs
            .get(self.active_tab)
            .ok_or_else(|| "no active file; pass a path to symbols/outline".to_string())?;
        let rel = tab
            .path
            .as_deref()
            .map(|p| relative_path(&self.root, p))
            .unwrap_or_else(|| "untitled".to_string());
        Ok((rel, tab.editor.read(cx).document_symbols()))
    }

    fn bridge_format(&mut self, raw_path: Option<&str>, cx: &mut Context<Self>) -> Result<Value, String> {
        let target_tab = if let Some(raw) = raw_path {
            let path = workspace_path(&self.root, raw)?;
            self.tabs
                .iter()
                .find(|t| t.path.as_deref() == Some(path.as_path()))
                .ok_or_else(|| format!("{raw} is not open in an editor tab"))?
        } else {
            self.tabs
                .get(self.active_tab)
                .ok_or_else(|| "no active editor tab to format".to_string())?
        };
        let rel = target_tab
            .path
            .as_deref()
            .map(|p| relative_path(&self.root, p))
            .unwrap_or_else(|| "untitled".to_string());
        let ed = target_tab.editor.clone();
        let requested = ed.update(cx, |editor, _cx| editor.request_format());
        Ok(json!({ "path": rel, "format_requested": requested }))
    }

    fn bridge_flutter_reload(
        &mut self,
        full_restart: bool,
        cx: &mut Context<Self>,
    ) -> Result<Value, String> {
        let Some(app_id) = self.flutter_app_id.clone() else {
            return Err("no running Flutter session".to_string());
        };
        if full_restart {
            self.trigger_hot_restart(cx);
            Ok(json!({ "action": "hot-restart", "app_id": app_id }))
        } else {
            self.trigger_hot_reload(cx);
            Ok(json!({ "action": "hot-reload", "app_id": app_id }))
        }
    }

    fn bridge_diagnostics(&self, only: Option<&str>, cx: &App) -> Value {
        let mut rows = Vec::new();
        for tab in &self.tabs {
            let Some(path) = tab.path.as_ref() else { continue };
            let rel = relative_path(&self.root, path);
            if only.is_some_and(|want| !rel.ends_with(want) && rel != want) {
                continue;
            }
            for d in tab.editor.read(cx).diagnostics() {
                let severity = match d.severity {
                    Some(core_lsp::lsp_types::DiagnosticSeverity::ERROR) => "error",
                    Some(core_lsp::lsp_types::DiagnosticSeverity::WARNING) => "warning",
                    Some(core_lsp::lsp_types::DiagnosticSeverity::INFORMATION) => "info",
                    Some(core_lsp::lsp_types::DiagnosticSeverity::HINT) => "hint",
                    _ => "unknown",
                };
                let message = d.message.lines().next().unwrap_or(&d.message);
                let (message, _) = clip_text(message, 400);
                rows.push(json!({
                    "path": rel,
                    "line": d.range.start.line + 1,
                    "column": d.range.start.character + 1,
                    "severity": severity,
                    "message": message,
                    "source": d.source,
                }));
                if rows.len() >= 300 {
                    return json!({ "diagnostics": rows, "truncated": true });
                }
            }
        }
        json!({ "diagnostics": rows, "truncated": false })
    }
}

struct AgentHarnessProbe;

impl AgentHarnessProbe {
    fn found(harness: crate::rails::agent::AgentHarness) -> bool {
        harness.resolve_binary().is_some()
    }
}

/// Resolve a user-supplied path against the workspace and refuse anything that
/// escapes it (including through a symlink).
pub(crate) fn workspace_path(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("empty path".to_string());
    }
    let candidate = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        root.join(raw)
    };
    let canonical = candidate
        .canonicalize()
        .map_err(|e| format!("{raw}: {e}"))?;
    let root = root
        .canonicalize()
        .map_err(|e| format!("workspace root: {e}"))?;
    if !canonical.starts_with(&root) {
        return Err(format!("{raw} is outside the workspace"));
    }
    Ok(canonical)
}

pub(crate) fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

/// Read `start..=end` (1-indexed) of a file, defaulting to the whole file with
/// a 2000-line ceiling so a stray call can't dump a generated file.
pub(crate) fn bridge_read(root: &Path, argv: &[String]) -> Result<String, String> {
    let raw = argv.first().ok_or("read needs a path")?;
    let path = workspace_path(root, raw)?;
    if !path.is_file() {
        return Err(format!("{raw} is not a file"));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{raw}: {e}"))?;
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let start = argv.get(1).and_then(|n| n.parse::<usize>().ok()).unwrap_or(1).max(1);
    let end = argv
        .get(2)
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(total)
        .min(total);
    if start > total {
        return Err(format!("{raw} has {total} lines; start {start} is past the end"));
    }
    let end = end.max(start);
    let truncated = end - start + 1 > 2_000;
    let end = if truncated { start + 1_999 } else { end };
    let mut out = lines[start - 1..end].join("\n");
    out.push('\n');
    if truncated {
        out.push_str(&format!(
            "[shame: stopped at line {end} of {total}; ask for read {raw} {} {} for the rest]\n",
            end + 1,
            total
        ));
    }
    Ok(out)
}

/// `git diff HEAD` (or `--staged`), capped so a giant refactor does not become
/// the whole prompt.
pub(crate) fn bridge_diff(root: &Path, staged: bool) -> Result<String, String> {
    let mut cmd = std::process::Command::new("git");
    if staged {
        cmd.args(["diff", "--cached", "--no-color"]);
    } else {
        cmd.args(["diff", "HEAD", "--no-color"]);
    }
    let output = cmd
        .current_dir(root)
        .output()
        .map_err(|e| format!("git diff: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git diff failed: {}", err.trim()));
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let (mut text, truncated) = clip_text(&text, 80_000);
    if text.trim().is_empty() {
        text = "No uncommitted changes.".to_string();
    } else if truncated {
        text.push_str("\n[shame: diff truncated at 80k characters]\n");
    }
    Ok(text)
}

/// Extract document symbols from a workspace file on disk.
pub(crate) fn bridge_symbols_from_disk(
    root: &Path,
    raw: &str,
) -> Result<(String, Vec<(String, &'static str, u32)>), String> {
    let path = workspace_path(root, raw)?;
    if !path.is_file() {
        return Err(format!("{raw} is not a file"));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{raw}: {e}"))?;
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let rel = relative_path(&canon_root, &path);
    Ok((rel, editor_view::extract_document_symbols(&text)))
}

/// Format document symbols into a compact 1-based outline string.
pub(crate) fn format_outline(rel: &str, symbols: &[(String, &'static str, u32)]) -> String {
    if symbols.is_empty() {
        return format!("{rel} (no symbols found)\n");
    }
    let mut out = format!("# {rel} ({} symbols)\n", symbols.len());
    for (name, kind, line0) in symbols {
        out.push_str(&format!("{:>5}: [{kind}] {name}\n", line0 + 1));
    }
    out
}

fn clip_text(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_string(), false);
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shame-bridge-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn workspace_paths_stay_inside_the_root() {
        let root = temp_root("paths");
        std::fs::write(root.join("hello.txt"), "hi\n").unwrap();
        let inside = workspace_path(&root, "hello.txt").unwrap();
        assert!(inside.ends_with("hello.txt"));
        assert!(workspace_path(&root, "../outside.txt").is_err());
        // An absolute path to the file is fine; one outside is not.
        assert!(workspace_path(&root, inside.to_str().unwrap()).is_ok());
        assert!(workspace_path(&root, "/etc/hosts").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_clips_and_reports_the_rest() {
        let root = temp_root("read");
        let body: String = (1..=2_500).map(|n| format!("line {n}\n")).collect();
        std::fs::write(root.join("big.txt"), body).unwrap();
        let all = bridge_read(&root, &["big.txt".into()]).unwrap();
        assert!(all.contains("line 1"));
        assert!(all.contains("line 2000"));
        assert!(!all.contains("line 2001"));
        assert!(all.contains("[shame: stopped at line 2000 of 2500"));
        let range = bridge_read(&root, &["big.txt".into(), "2001".into(), "2002".into()]).unwrap();
        assert_eq!(range, "line 2001\nline 2002\n");
        assert!(bridge_read(&root, &["missing.txt".into()]).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clipping_is_utf8_safe() {
        let (text, truncated) = clip_text("héllo", 3);
        assert!(truncated);
        // Never cuts inside the two-byte é, so at most one byte shorter.
        assert!(text.len() <= 3);
        assert!(text.starts_with('h'));
    }

    #[test]
    fn relative_paths_are_prefixed_only_inside_the_workspace() {
        let root = Path::new("/work/project");
        assert_eq!(relative_path(root, Path::new("/work/project/src/a.rs")), "src/a.rs");
        assert_eq!(relative_path(root, Path::new("/elsewhere/a.rs")), "/elsewhere/a.rs");
    }

    #[test]
    fn symbols_and_outline_extract_from_workspace_file() {
        let root = temp_root("symbols");
        std::fs::write(
            root.join("sample.rs"),
            "pub struct Counter {\n    value: i32,\n}\n\npub fn increment(c: &mut Counter) {\n    c.value += 1;\n}\n",
        )
        .unwrap();
        let (rel, symbols) = bridge_symbols_from_disk(&root, "sample.rs").unwrap();
        assert_eq!(rel, "sample.rs");
        assert_eq!(
            symbols,
            vec![
                ("Counter".to_string(), "struct", 0),
                ("increment".to_string(), "fn", 4),
            ]
        );
        let outline = format_outline(&rel, &symbols);
        assert!(outline.contains("# sample.rs (2 symbols)"));
        assert!(outline.contains("    1: [struct] Counter"));
        assert!(outline.contains("    5: [fn] increment"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn the_socket_round_trips_a_tool_request() {
        let (bridge, requests) = AgentBridge::start().expect("bind the bridge socket");
        let socket = bridge.socket.clone();
        let responder = std::thread::spawn(move || {
            let request = requests.recv_blocking().expect("one request");
            assert_eq!(request.tool, "context");
            let _ = request.reply.send(BridgeReply {
                ok: true,
                body: json!({ "active_file": "lib/main.dart" }),
            });
        });
        let (ok, body) = send_request(&socket, &json!({ "tool": "context", "args": {} }))
            .expect("the server answers");
        assert!(ok);
        assert_eq!(body["active_file"], "lib/main.dart");
        responder.join().unwrap();
    }
}
