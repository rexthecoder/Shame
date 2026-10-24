use std::io::{self, BufReader};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use std::thread;

use async_channel::{Receiver, unbounded};
use serde_json::Value;

use crate::transport::{read_message, write_message};

/// Something the server sent us, unprompted or in reply to a request.
#[derive(Debug)]
pub enum ServerMessage {
    /// A reply to a request we sent, matched by id.
    Response { id: i64, result: Result<Value, Value> },
    /// A notification (no reply expected), e.g. `textDocument/publishDiagnostics`.
    Notification { method: String, params: Value },
    /// A request *from* the server (e.g. `workspace/applyEdit`), which
    /// expects a reply — call [`LspClient::respond`] with the same `id`.
    Request { id: i64, method: String, params: Value },
}

/// A JSON-RPC client for a language server speaking the LSP "Base
/// Protocol" (`Content-Length`-framed JSON) over its stdin/stdout.
///
/// This is the whole client: spawn the process, send requests and
/// notifications, and read `ServerMessage`s off `events` as they arrive.
/// It knows nothing about Dart or LSP's actual methods/types beyond the
/// JSON-RPC envelope — callers build `lsp_types` values and pass them in
/// as `serde_json::Value`, keeping this crate reusable for any LSP server.
pub struct LspClient {
    child: Child,
    stdin: Mutex<ChildStdin>,
    next_id: AtomicI64,
    /// Server-sent messages, in arrival order. Reading this never blocks
    /// the server's own reader thread; it just queues up.
    pub events: Receiver<ServerMessage>,
}

impl LspClient {
    /// Spawn `command` with `args`, piping its stdin/stdout, and start a
    /// background thread reading framed messages off stdout into `events`.
    pub fn spawn(command: &str, args: &[&str]) -> io::Result<Self> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdin = child.stdin.take().expect("spawned with Stdio::piped()");
        let stdout = child.stdout.take().expect("spawned with Stdio::piped()");

        let (tx, rx) = unbounded();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let body = match read_message(&mut reader) {
                    Ok(body) => body,
                    Err(_) => break,
                };
                let Ok(value) = serde_json::from_slice::<Value>(&body) else {
                    continue;
                };
                let Some(message) = parse_server_message(value) else {
                    continue;
                };
                if tx.send_blocking(message).is_err() {
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

    /// Send a notification (fire-and-forget; the server never replies).
    pub fn notify(&self, method: &str, params: Value) -> io::Result<()> {
        self.send_raw(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    /// Send a request and return its id; match the reply by reading
    /// `events` for a `ServerMessage::Response` with that same id.
    pub fn request(&self, method: &str, params: Value) -> io::Result<i64> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.send_raw(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;
        Ok(id)
    }

    /// Reply to a server-to-client [`ServerMessage::Request`] with the
    /// same `id`.
    pub fn respond(&self, id: i64, result: Value) -> io::Result<()> {
        self.send_raw(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }))
    }

    fn send_raw(&self, value: &Value) -> io::Result<()> {
        let bytes = serde_json::to_vec(value)?;
        let mut stdin = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
        write_message(&mut *stdin, &bytes)
    }

    /// Stop the server process. Prefer sending `shutdown`/`exit` requests
    /// first when the protocol allows it; this is the fallback.
    pub fn kill(&mut self) -> io::Result<()> {
        self.child.kill()
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

fn parse_server_message(value: Value) -> Option<ServerMessage> {
    let obj = value.as_object()?;

    if let Some(id) = obj.get("id").and_then(Value::as_i64) {
        if let Some(result) = obj.get("result") {
            return Some(ServerMessage::Response {
                id,
                result: Ok(result.clone()),
            });
        }
        if let Some(error) = obj.get("error") {
            return Some(ServerMessage::Response {
                id,
                result: Err(error.clone()),
            });
        }
        // `id` present but no `result`/`error`: this isn't a reply to one
        // of our requests, it's the server making its own request to us
        // (e.g. `workspace/applyEdit`) — it still needs `method`.
        let method = obj.get("method")?.as_str()?.to_string();
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        return Some(ServerMessage::Request { id, method, params });
    }

    let method = obj.get("method")?.as_str()?.to_string();
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    Some(ServerMessage::Notification { method, params })
}

/// Block on `events` until a `Response` with `id` arrives, discarding (not
/// dropping — callers that also want notifications should drain `events`
/// themselves instead) any notifications seen along the way. Meant for
/// simple request/response calls and tests; a real UI integration should
/// poll `events` from a background task instead of blocking.
pub fn wait_for_response(events: &Receiver<ServerMessage>, id: i64) -> Option<Result<Value, Value>> {
    loop {
        match events.recv_blocking() {
            Ok(ServerMessage::Response { id: got_id, result }) if got_id == id => {
                return Some(result);
            }
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_response() {
        let value = serde_json::json!({"jsonrpc": "2.0", "id": 7, "result": {"ok": true}});
        match parse_server_message(value) {
            Some(ServerMessage::Response { id, result: Ok(result) }) => {
                assert_eq!(id, 7);
                assert_eq!(result, serde_json::json!({"ok": true}));
            }
            other => panic!("expected a successful response, got {other:?}"),
        }
    }

    #[test]
    fn parses_an_error_response() {
        let value = serde_json::json!({"jsonrpc": "2.0", "id": 3, "error": {"code": -32600, "message": "nope"}});
        match parse_server_message(value) {
            Some(ServerMessage::Response { id, result: Err(err) }) => {
                assert_eq!(id, 3);
                assert_eq!(err["message"], "nope");
            }
            other => panic!("expected an error response, got {other:?}"),
        }
    }

    #[test]
    fn parses_a_notification() {
        let value = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {"uri": "file:///a.dart", "diagnostics": []},
        });
        match parse_server_message(value) {
            Some(ServerMessage::Notification { method, params }) => {
                assert_eq!(method, "textDocument/publishDiagnostics");
                assert_eq!(params["uri"], "file:///a.dart");
            }
            other => panic!("expected a notification, got {other:?}"),
        }
    }

    /// Spawns the real `dart language-server` and does a minimal
    /// `initialize` handshake. Skips (rather than fails) if `dart` isn't
    /// on `PATH`, so this still passes in environments without the Dart
    /// SDK installed — but on a machine that has it, this is a genuine
    /// end-to-end check of the transport and client against a real LSP
    /// server, not just a parser test against synthetic JSON.
    #[test]
    fn initialize_handshake_against_the_real_dart_language_server() {
        if Command::new("dart").arg("--version").output().is_err() {
            eprintln!("skipping: `dart` not found on PATH");
            return;
        }

        let client = LspClient::spawn("dart", &["language-server"]).expect("failed to spawn dart language-server");

        let id = client
            .request(
                "initialize",
                serde_json::json!({
                    "processId": null,
                    "rootUri": null,
                    "capabilities": {},
                }),
            )
            .expect("failed to send initialize request");

        let response = wait_for_response(&client.events, id).expect("server closed without responding");
        let result = response.expect("initialize returned an error");
        assert!(
            result["capabilities"]["hoverProvider"].as_bool().unwrap_or(false),
            "expected the real analysis server to advertise hoverProvider, got: {result}"
        );
    }

    #[test]
    fn parses_a_server_to_client_request() {
        // Servers can send requests *to* the client too, e.g.
        // workspace/applyEdit — these have an id (expecting a reply, via
        // respond()) but no result/error, unlike our own responses.
        let value = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "workspace/applyEdit", "params": {"edit": {}}});
        match parse_server_message(value) {
            Some(ServerMessage::Request { id, method, params }) => {
                assert_eq!(id, 1);
                assert_eq!(method, "workspace/applyEdit");
                assert!(params.get("edit").is_some());
            }
            other => panic!("expected a server-to-client request, got {other:?}"),
        }
    }

    #[test]
    fn a_malformed_id_bearing_message_with_no_method_either_is_ignored() {
        let value = serde_json::json!({"jsonrpc": "2.0", "id": 1});
        assert!(parse_server_message(value).is_none());
    }
}
