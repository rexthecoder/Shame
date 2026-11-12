//! A kernel session over Jupyter Server's websocket
//! (`/api/kernels/{id}/channels`), using the JSON message format.
//!
//! One background thread owns the socket. Commands go in over a channel;
//! kernel status, outputs (already in nbformat shape) and replies come out
//! as [`KernelEvent`]s, each tagged with the request id the caller chose
//! (the app uses the cell id).

use std::collections::HashMap;
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use tungstenite::client::IntoClientRequest as _;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::server::{ServerInfo, random_token};

#[derive(Debug, Clone, PartialEq)]
pub enum KernelEvent {
    /// `starting`, `busy`, `idle`, `restarting`, `dead`.
    Status(String),
    /// The kernel accepted request `req` as execution number `count`.
    Started { req: String, count: Option<i64> },
    /// An nbformat output object for request `req`.
    Output { req: String, output: Value },
    /// `clear_output`: drop `req`'s outputs (`wait`: when the next arrives).
    Clear { req: String, wait: bool },
    /// Request `req` finished: `ok`, `error` or `aborted`.
    Done { req: String, status: String, count: Option<i64> },
    /// The socket closed or failed.
    Disconnected(String),
}

enum Command {
    Execute { req: String, code: String },
    Close,
}

/// A live connection to one kernel. Dropping it closes the socket (the
/// kernel keeps running on the server until shut down).
pub struct KernelSession {
    pub kernel_id: String,
    commands: mpsc::Sender<Command>,
}

impl KernelSession {
    /// Connect to kernel `kernel_id` on `server`. Events arrive on the
    /// returned receiver until the session is dropped or the socket fails.
    pub fn connect(server: &ServerInfo, kernel_id: &str) -> Result<(Self, async_channel::Receiver<KernelEvent>), String> {
        let session_id = random_token();
        let url = server.channels_url(kernel_id, &session_id);
        let mut request = url.as_str().into_client_request().map_err(|e| format!("bad kernel URL: {e}"))?;
        if let Some(auth) = server.auth() {
            if let Ok(v) = auth.parse() {
                request.headers_mut().insert("Authorization", v);
            }
        }
        let (mut socket, _resp) = tungstenite::connect(request).map_err(|e| format!("couldn't open the kernel channel: {e}"))?;
        // Short reads, so the thread can also look at its command queue.
        set_read_timeout(&mut socket, Some(Duration::from_millis(40)));

        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = async_channel::unbounded();
        std::thread::spawn(move || run(socket, session_id, cmd_rx, ev_tx));
        Ok((Self { kernel_id: kernel_id.to_string(), commands: cmd_tx }, ev_rx))
    }

    /// Run `code`; its events carry `req`.
    pub fn execute(&self, req: impl Into<String>, code: impl Into<String>) -> Result<(), String> {
        self.commands
            .send(Command::Execute { req: req.into(), code: code.into() })
            .map_err(|_| "the kernel connection is closed".into())
    }
}

impl Drop for KernelSession {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Close);
    }
}

fn set_read_timeout(socket: &mut WebSocket<MaybeTlsStream<TcpStream>>, timeout: Option<Duration>) {
    let tcp: Option<&TcpStream> = match socket.get_mut() {
        MaybeTlsStream::Plain(s) => Some(s),
        MaybeTlsStream::Rustls(s) => Some(&s.sock),
        _ => None,
    };
    if let Some(tcp) = tcp {
        let _ = tcp.set_read_timeout(timeout);
    }
}

/// An ISO-8601 UTC timestamp for message headers.
fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// An `execute_request` on the shell channel.
pub(crate) fn execute_request(msg_id: &str, session: &str, code: &str) -> Value {
    json!({
        "header": {
            "msg_id": msg_id,
            "msg_type": "execute_request",
            "username": "shame",
            "session": session,
            "date": now_iso(),
            "version": "5.3"
        },
        "parent_header": {},
        "metadata": {},
        "content": {
            "code": code,
            "silent": false,
            "store_history": true,
            "user_expressions": {},
            "allow_stdin": false,
            "stop_on_error": true
        },
        "channel": "shell",
        "buffers": []
    })
}

/// Turn one kernel message into events. `requests` maps our msg ids to the
/// caller's request ids.
pub(crate) fn handle_message(msg: &Value, requests: &mut HashMap<String, String>) -> Vec<KernelEvent> {
    let msg_type = msg.pointer("/header/msg_type").and_then(Value::as_str).unwrap_or("");
    let content = msg.get("content").cloned().unwrap_or(Value::Null);
    if msg_type == "status" {
        // Status replies to our own requests are noise; kernel-wide ones
        // (starting, restarting, dead) and busy/idle both matter for the UI.
        let state = content.get("execution_state").and_then(Value::as_str).unwrap_or("").to_string();
        return vec![KernelEvent::Status(state)];
    }
    let parent = msg.pointer("/parent_header/msg_id").and_then(Value::as_str).unwrap_or("");
    let Some(req) = requests.get(parent).cloned() else { return Vec::new() };
    let output = |mut v: Value, otype: &str| {
        if let Some(o) = v.as_object_mut() {
            o.insert("output_type".into(), json!(otype));
            o.remove("transient");
        }
        v
    };
    match msg_type {
        "execute_input" => vec![KernelEvent::Started { req, count: content.get("execution_count").and_then(Value::as_i64) }],
        "stream" => vec![KernelEvent::Output { req, output: output(content, "stream") }],
        "execute_result" => vec![KernelEvent::Output { req, output: output(content, "execute_result") }],
        "display_data" | "update_display_data" => vec![KernelEvent::Output { req, output: output(content, "display_data") }],
        "error" => vec![KernelEvent::Output { req, output: output(content, "error") }],
        "clear_output" => vec![KernelEvent::Clear { req, wait: content.get("wait").and_then(Value::as_bool).unwrap_or(false) }],
        "execute_reply" => {
            requests.remove(parent);
            vec![KernelEvent::Done {
                req,
                status: content.get("status").and_then(Value::as_str).unwrap_or("ok").to_string(),
                count: content.get("execution_count").and_then(Value::as_i64),
            }]
        }
        _ => Vec::new(),
    }
}

fn run(
    mut socket: WebSocket<MaybeTlsStream<TcpStream>>,
    session: String,
    commands: mpsc::Receiver<Command>,
    events: async_channel::Sender<KernelEvent>,
) {
    let mut requests: HashMap<String, String> = HashMap::new();
    let mut counter = 0u64;
    loop {
        // Commands first, so a run starts without waiting on a read.
        loop {
            match commands.try_recv() {
                Ok(Command::Execute { req, code }) => {
                    counter += 1;
                    let msg_id = format!("{session}-{counter}");
                    requests.insert(msg_id.clone(), req);
                    let text = execute_request(&msg_id, &session, &code).to_string();
                    if let Err(e) = socket.send(Message::Text(text.into())) {
                        let _ = events.send_blocking(KernelEvent::Disconnected(e.to_string()));
                        return;
                    }
                }
                Ok(Command::Close) | Err(mpsc::TryRecvError::Disconnected) => {
                    let _ = socket.close(None);
                    let _ = socket.flush();
                    return;
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                if let Ok(msg) = serde_json::from_str::<Value>(&text) {
                    for ev in handle_message(&msg, &mut requests) {
                        if events.send_blocking(ev).is_err() {
                            return;
                        }
                    }
                }
            }
            Ok(Message::Close(_)) => {
                let _ = events.send_blocking(KernelEvent::Disconnected("the server closed the kernel channel".into()));
                return;
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => {
                let _ = events.send_blocking(KernelEvent::Disconnected(e.to_string()));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(msg_type: &str, parent: &str, content: Value) -> Value {
        json!({"header": {"msg_type": msg_type}, "parent_header": {"msg_id": parent}, "content": content})
    }

    #[test]
    fn kernel_messages_become_events() {
        let mut reqs = HashMap::from([("m1".to_string(), "cell-a".to_string())]);
        assert_eq!(
            handle_message(&msg("status", "m1", json!({"execution_state": "busy"})), &mut reqs),
            vec![KernelEvent::Status("busy".into())]
        );
        assert_eq!(
            handle_message(&msg("execute_input", "m1", json!({"execution_count": 7, "code": "1+1"})), &mut reqs),
            vec![KernelEvent::Started { req: "cell-a".into(), count: Some(7) }]
        );
        let out = handle_message(&msg("stream", "m1", json!({"name": "stdout", "text": "hi\n"})), &mut reqs);
        assert_eq!(
            out,
            vec![KernelEvent::Output {
                req: "cell-a".into(),
                output: json!({"name": "stdout", "text": "hi\n", "output_type": "stream"})
            }]
        );
        let disp = handle_message(
            &msg("display_data", "m1", json!({"data": {"text/plain": "x"}, "metadata": {}, "transient": {"display_id": "d"}})),
            &mut reqs,
        );
        assert!(matches!(&disp[0], KernelEvent::Output { output, .. } if output.get("transient").is_none()));
        // Messages for requests that aren't ours are ignored.
        assert!(handle_message(&msg("stream", "other", json!({"name": "stdout", "text": "x"})), &mut reqs).is_empty());
        assert_eq!(
            handle_message(&msg("execute_reply", "m1", json!({"status": "ok", "execution_count": 7})), &mut reqs),
            vec![KernelEvent::Done { req: "cell-a".into(), status: "ok".into(), count: Some(7) }]
        );
        assert!(reqs.is_empty());
    }

    #[test]
    fn requests_are_well_formed() {
        let r = execute_request("id1", "sess", "print(1)");
        assert_eq!(r["channel"], "shell");
        assert_eq!(r["header"]["msg_type"], "execute_request");
        assert_eq!(r["content"]["code"], "print(1)");
        let date = r["header"]["date"].as_str().unwrap();
        assert_eq!(date.len(), 20);
        assert!(date.ends_with('Z') && date.as_bytes()[10] == b'T');
    }

    /// Start a real `jupyter server`, run code in a kernel, and read the
    /// outputs back. Needs Jupyter installed: run with
    /// `JUPYTER=/path/to/jupyter cargo test -p core-jupyter -- --ignored`.
    #[test]
    #[ignore]
    fn runs_code_against_a_real_jupyter_server() {
        let jupyter = std::env::var("JUPYTER").unwrap_or_else(|_| "jupyter".into());
        let root = std::env::temp_dir();
        let server = crate::LocalServer::start(std::path::Path::new(&jupyter), &root).expect("server starts");
        let specs = server.info.kernel_specs().unwrap();
        assert!(!specs.is_empty());
        let kernel = server.info.start_kernel(&specs[0].name, None).unwrap();
        let (session, events) = KernelSession::connect(&server.info, &kernel).unwrap();
        session.execute("c1", "print('hello')\n6*7").unwrap();
        session.execute("c2", "1/0").unwrap();
        let mut seen = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            match events.recv_blocking() {
                Ok(ev) => {
                    let done_c2 = matches!(&ev, KernelEvent::Done { req, .. } if req == "c2");
                    seen.push(ev);
                    if done_c2 {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let text = |req: &str| -> String {
            seen.iter()
                .filter_map(|e| match e {
                    KernelEvent::Output { req: r, output } if r == req => Some(output.to_string()),
                    _ => None,
                })
                .collect()
        };
        assert!(text("c1").contains("hello"), "{seen:?}");
        assert!(text("c1").contains("42"), "{seen:?}");
        assert!(text("c2").contains("ZeroDivisionError"), "{seen:?}");
        assert!(seen.contains(&KernelEvent::Done { req: "c2".into(), status: "error".into(), count: Some(2) }));
        drop(session);
        server.info.shutdown_kernel(&kernel).unwrap();
    }
}
