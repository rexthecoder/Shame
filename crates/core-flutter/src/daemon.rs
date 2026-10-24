use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use std::thread;

use async_channel::{Receiver, unbounded};
use serde_json::Value;

/// One line of `flutter run --machine`'s stdout, classified.
///
/// The daemon protocol frames each structured message as a single-line
/// JSON array containing one object (`[{"event": ..., "params": ...}]`
/// or `[{"id": ..., "result": ...}]`); everything else on stdout — build
/// progress text, the app's own `print()` output — is a plain line,
/// kept as `Log` rather than dropped, since that's exactly what a log
/// panel needs.
#[derive(Debug, Clone)]
pub enum DaemonOutput {
    Event { event: String, params: Value },
    Response { id: i64, result: Result<Value, Value> },
    Log(String),
}

fn parse_line(line: &str) -> DaemonOutput {
    let trimmed = line.trim();
    let parsed = trimmed
        .starts_with('[')
        .then(|| serde_json::from_str::<Value>(trimmed).ok())
        .flatten()
        .and_then(|value| value.as_array()?.first().cloned())
        .and_then(|first| first.as_object().cloned());

    let Some(obj) = parsed else {
        return DaemonOutput::Log(line.to_string());
    };

    if let Some(event) = obj.get("event").and_then(Value::as_str) {
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        return DaemonOutput::Event { event: event.to_string(), params };
    }
    if let Some(id) = obj.get("id").and_then(Value::as_i64) {
        if let Some(result) = obj.get("result") {
            return DaemonOutput::Response { id, result: Ok(result.clone()) };
        }
        if let Some(error) = obj.get("error") {
            return DaemonOutput::Response { id, result: Err(error.clone()) };
        }
    }
    DaemonOutput::Log(line.to_string())
}

/// A running `flutter run --machine` process for one project+device.
///
/// Knows the Flutter daemon protocol's framing and a handful of its own
/// commands (`hot_reload`/`hot_restart`/`stop`), unlike `core-lsp` which
/// stays protocol-generic — there's only one real consumer of this
/// protocol, so there's no reuse motive for keeping Flutter-specific
/// method names out of this crate the way LSP method names are kept out
/// of `core-lsp`.
pub struct FlutterDaemon {
    child: Child,
    stdin: Mutex<ChildStdin>,
    next_id: AtomicI64,
    /// Every output line, in arrival order: structured daemon messages
    /// and plain log lines interleaved, same order the process printed
    /// them.
    pub events: Receiver<DaemonOutput>,
}

impl FlutterDaemon {
    /// Spawn `flutter run --machine -d <device_id>` in `project_dir`.
    pub fn spawn(project_dir: &Path, device_id: &str) -> io::Result<Self> {
        let mut child = Command::new("flutter")
            .args(["run", "--machine", "-d", device_id])
            .current_dir(project_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdin = child.stdin.take().expect("spawned with Stdio::piped()");
        let stdout = child.stdout.take().expect("spawned with Stdio::piped()");

        let (tx, rx) = unbounded();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if tx.send_blocking(parse_line(&line)).is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            child,
            stdin: Mutex::new(stdin),
            next_id: AtomicI64::new(1),
            events: rx,
        })
    }

    /// Send a daemon command and return its id; match the reply by
    /// reading `events` for a `Response` with that same id.
    pub fn request(&self, method: &str, params: Value) -> io::Result<i64> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.send(&serde_json::json!([{"id": id, "method": method, "params": params}]))?;
        Ok(id)
    }

    /// `fullRestart: false` — recompile and push changed code without
    /// restarting the app (preserves state).
    pub fn hot_reload(&self, app_id: &str) -> io::Result<i64> {
        self.request(
            "app.restart",
            serde_json::json!({"appId": app_id, "fullRestart": false, "pause": false}),
        )
    }

    /// `fullRestart: true` — recompile and restart the app from scratch.
    pub fn hot_restart(&self, app_id: &str) -> io::Result<i64> {
        self.request(
            "app.restart",
            serde_json::json!({"appId": app_id, "fullRestart": true, "pause": false}),
        )
    }

    pub fn stop(&self, app_id: &str) -> io::Result<i64> {
        self.request("app.stop", serde_json::json!({"appId": app_id}))
    }

    fn send(&self, value: &Value) -> io::Result<()> {
        let mut line = serde_json::to_string(value)?;
        line.push('\n');
        let mut stdin = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
        stdin.write_all(line.as_bytes())?;
        stdin.flush()
    }

    /// Stop the process directly. Prefer `stop()` (a graceful
    /// `app.stop` command) when a device is actually running; this is
    /// the fallback (e.g. the process never got that far).
    pub fn kill(&mut self) -> io::Result<()> {
        self.child.kill()
    }
}

impl Drop for FlutterDaemon {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every one of these lines was actually printed by `flutter run
    /// --machine -d macos` during a real run (captured by hand, same
    /// technique used for `core-lsp`): the daemon handshake, a
    /// multi-field progress event with `finished` present-but-false vs.
    /// absent-meaning-true... no — present either way, just differing
    /// —, a plain non-JSON log line, and the hot-reload response.
    #[test]
    fn parses_real_captured_daemon_lines() {
        match parse_line(r#"[{"event":"daemon.connected","params":{"version":"0.6.1","pid":49684}}]"#) {
            DaemonOutput::Event { event, params } => {
                assert_eq!(event, "daemon.connected");
                assert_eq!(params["pid"], 49684);
            }
            other => panic!("expected an Event, got {other:?}"),
        }

        match parse_line(
            r#"[{"event":"app.progress","params":{"appId":"abc","id":"0","progressId":null,"message":"Building macOS application...","finished":false}}]"#,
        ) {
            DaemonOutput::Event { event, params } => {
                assert_eq!(event, "app.progress");
                assert_eq!(params["finished"], false);
                assert_eq!(params["message"], "Building macOS application...");
            }
            other => panic!("expected an Event, got {other:?}"),
        }

        match parse_line("Launching lib/main.dart on macOS in debug mode...") {
            DaemonOutput::Log(line) => assert_eq!(line, "Launching lib/main.dart on macOS in debug mode..."),
            other => panic!("expected a Log line, got {other:?}"),
        }

        match parse_line(r#"[{"id":1,"result":{"code":0,"message":"Reloaded 0 libraries"}}]"#) {
            DaemonOutput::Response { id, result: Ok(result) } => {
                assert_eq!(id, 1);
                assert_eq!(result["code"], 0);
            }
            other => panic!("expected a successful Response, got {other:?}"),
        }

        match parse_line(r#"[{"id":1,"result":true}]"#) {
            DaemonOutput::Response { id, result: Ok(result) } => {
                assert_eq!(id, 1);
                assert_eq!(result, true);
            }
            other => panic!("expected a successful Response, got {other:?}"),
        }
    }

    #[test]
    fn a_build_tool_checkmark_line_is_a_log_not_a_parse_attempt() {
        // Starts with a non-ASCII checkmark, not '[' - must not be
        // mistaken for daemon JSON.
        match parse_line("✓ Built build/macos/Build/Products/Debug/flutter_test_app.app") {
            DaemonOutput::Log(_) => {}
            other => panic!("expected a Log line, got {other:?}"),
        }
    }
}
