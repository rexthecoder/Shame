use std::io::{self, BufReader};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use std::thread;

use async_channel::{Receiver, unbounded};
use serde_json::Value;

use crate::transport::{read_message, write_message};

/// Something the debug adapter sent us, unprompted or in reply to a
/// request.
#[derive(Debug)]
pub enum DapMessage {
    /// A reply to a request we sent, matched by `request_seq` (the
    /// `seq` we sent it with). `Err` carries the adapter's `message`
    /// field plus its `body` (DAP puts machine-readable error detail
    /// in `body.error`, not just a string).
    Response { request_seq: i64, command: String, result: Result<Value, Value> },
    /// An unprompted event, e.g. `stopped`, `output`, `terminated`.
    Event { event: String, body: Value },
    /// A request *from* the adapter (e.g. `runInTerminal`,
    /// `startDebugging`), which expects a reply — call
    /// [`DapClient::respond`] with the same `seq`.
    Request { seq: i64, command: String, arguments: Value },
}

/// A line breakpoint, optionally conditional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breakpoint {
    pub line: u32,
    pub condition: Option<String>,
}

/// The `breakpoints` array of a `setBreakpoints` request.
pub fn breakpoints_json(breakpoints: &[Breakpoint]) -> Vec<Value> {
    breakpoints
        .iter()
        .map(|bp| match &bp.condition {
            Some(condition) if !condition.trim().is_empty() => {
                serde_json::json!({"line": bp.line, "condition": condition})
            }
            _ => serde_json::json!({"line": bp.line}),
        })
        .collect()
}

/// A client for a debug adapter speaking the Debug Adapter Protocol
/// (`Content-Length`-framed JSON, same base framing as LSP) over its
/// stdin/stdout.
///
/// Like `core-lsp::LspClient`, this knows nothing about Dart or any
/// particular adapter's commands — callers pass `command`/`arguments`
/// as plain JSON and get `DapMessage`s back, so this crate stays
/// reusable for any DAP-speaking adapter.
pub struct DapClient {
    child: Child,
    stdin: Mutex<ChildStdin>,
    /// DAP seq numbers are per-sender (each side counts its own
    /// messages from 1), unlike LSP's single shared id space.
    next_seq: AtomicI64,
    /// Adapter-sent messages, in arrival order.
    pub events: Receiver<DapMessage>,
}

impl DapClient {
    /// Spawn `command` with `args` (e.g. `dart`, `["debug_adapter"]`),
    /// piping its stdin/stdout, and start a background thread reading
    /// framed messages off stdout into `events`.
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
                let Some(message) = parse_dap_message(value) else {
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
            next_seq: AtomicI64::new(1),
            events: rx,
        })
    }

    /// Send a request and return its `seq`; match the reply by reading
    /// `events` for a `DapMessage::Response` with that same
    /// `request_seq`.
    pub fn request(&self, command: &str, arguments: Value) -> io::Result<i64> {
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        self.send_raw(&serde_json::json!({
            "seq": seq,
            "type": "request",
            "command": command,
            "arguments": arguments,
        }))?;
        Ok(seq)
    }

    /// `launch` request: start the program and begin executing it
    /// (`noDebug: false` so breakpoints set beforehand actually stop
    /// it).
    pub fn launch(&self, program: &str, cwd: &str) -> io::Result<i64> {
        self.request("launch", serde_json::json!({"program": program, "cwd": cwd, "noDebug": false}))
    }

    /// Set the full set of line breakpoints for one source file —
    /// per the DAP spec this *replaces* whatever breakpoints were
    /// previously set for that file, it isn't additive.
    pub fn set_breakpoints(&self, path: &str, lines: &[u32]) -> io::Result<i64> {
        let plain: Vec<Breakpoint> = lines.iter().map(|&line| Breakpoint { line, condition: None }).collect();
        self.set_breakpoints_ex(path, &plain)
    }

    /// Like [`set_breakpoints`](Self::set_breakpoints), but each breakpoint
    /// may carry a `condition` expression the adapter evaluates at the
    /// breakpoint (it only stops when the expression is true).
    pub fn set_breakpoints_ex(&self, path: &str, breakpoints: &[Breakpoint]) -> io::Result<i64> {
        self.request(
            "setBreakpoints",
            serde_json::json!({"source": {"path": path}, "breakpoints": breakpoints_json(breakpoints)}),
        )
    }

    /// Tell the adapter the client is done setting up (breakpoints,
    /// exception filters) and it should start running.
    pub fn configuration_done(&self) -> io::Result<i64> {
        self.request("configurationDone", serde_json::json!({}))
    }

    pub fn threads(&self) -> io::Result<i64> {
        self.request("threads", serde_json::json!({}))
    }

    pub fn stack_trace(&self, thread_id: i64) -> io::Result<i64> {
        self.request("stackTrace", serde_json::json!({"threadId": thread_id}))
    }

    pub fn scopes(&self, frame_id: i64) -> io::Result<i64> {
        self.request("scopes", serde_json::json!({"frameId": frame_id}))
    }

    pub fn variables(&self, variables_reference: i64) -> io::Result<i64> {
        self.request("variables", serde_json::json!({"variablesReference": variables_reference}))
    }

    pub fn continue_thread(&self, thread_id: i64) -> io::Result<i64> {
        self.request("continue", serde_json::json!({"threadId": thread_id}))
    }

    /// Step over the current line.
    pub fn next(&self, thread_id: i64) -> io::Result<i64> {
        self.request("next", serde_json::json!({"threadId": thread_id}))
    }

    /// Step into a function call on the current line.
    pub fn step_in(&self, thread_id: i64) -> io::Result<i64> {
        self.request("stepIn", serde_json::json!({"threadId": thread_id}))
    }

    /// Run until the current function returns.
    pub fn step_out(&self, thread_id: i64) -> io::Result<i64> {
        self.request("stepOut", serde_json::json!({"threadId": thread_id}))
    }

    /// Evaluate an expression in the context of a stack frame (watch
    /// expressions, a debug console, or a hover value — `context`
    /// distinguishes which per the DAP spec: `"watch"`, `"repl"`,
    /// `"hover"`, or `"clipboard"`). Only valid while stopped at that
    /// frame — real captured error: evaluating while running responds
    /// `success: false` with `message: "Evaluation is only supported
    /// when the debugger is paused..."`, same error shape
    /// [`DapMessage::Response`] already handles for any other command.
    pub fn evaluate(&self, expression: &str, frame_id: i64, context: &str) -> io::Result<i64> {
        self.request(
            "evaluate",
            serde_json::json!({"expression": expression, "frameId": frame_id, "context": context}),
        )
    }

    pub fn disconnect(&self, terminate_debuggee: bool) -> io::Result<i64> {
        self.request("disconnect", serde_json::json!({"terminateDebuggee": terminate_debuggee}))
    }

    /// Reply to an adapter-to-client [`DapMessage::Request`] with a
    /// success response, echoing back its `seq` as `request_seq`.
    pub fn respond(&self, request_seq: i64, command: &str, body: Value) -> io::Result<()> {
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        self.send_raw(&serde_json::json!({
            "seq": seq,
            "type": "response",
            "request_seq": request_seq,
            "command": command,
            "success": true,
            "body": body,
        }))
    }

    fn send_raw(&self, value: &Value) -> io::Result<()> {
        let bytes = serde_json::to_vec(value)?;
        let mut stdin = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
        write_message(&mut *stdin, &bytes)
    }

    /// Stop the adapter process. Prefer sending a `disconnect` or
    /// `terminate` request first when the session allows it; this is
    /// the fallback.
    pub fn kill(&mut self) -> io::Result<()> {
        self.child.kill()
    }
}

impl Drop for DapClient {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

fn parse_dap_message(value: Value) -> Option<DapMessage> {
    let obj = value.as_object()?;
    let ty = obj.get("type")?.as_str()?;

    match ty {
        "response" => {
            let request_seq = obj.get("request_seq")?.as_i64()?;
            let command = obj.get("command")?.as_str()?.to_string();
            let body = obj.get("body").cloned().unwrap_or(Value::Null);
            let success = obj.get("success")?.as_bool()?;
            let result = if success {
                Ok(body)
            } else {
                let message = obj.get("message").cloned().unwrap_or(Value::Null);
                Err(serde_json::json!({"message": message, "body": body}))
            };
            Some(DapMessage::Response { request_seq, command, result })
        }
        "event" => {
            let event = obj.get("event")?.as_str()?.to_string();
            let body = obj.get("body").cloned().unwrap_or(Value::Null);
            Some(DapMessage::Event { event, body })
        }
        "request" => {
            let seq = obj.get("seq")?.as_i64()?;
            let command = obj.get("command")?.as_str()?.to_string();
            let arguments = obj.get("arguments").cloned().unwrap_or(Value::Null);
            Some(DapMessage::Request { seq, command, arguments })
        }
        _ => None,
    }
}

/// Block on `events` until a `Response` with `request_seq` arrives,
/// discarding any events/requests seen along the way. Meant for simple
/// request/response calls and tests; a real UI integration should poll
/// `events` from a background task instead of blocking.
pub fn wait_for_response(events: &Receiver<DapMessage>, request_seq: i64) -> Option<Result<Value, Value>> {
    loop {
        match events.recv_blocking() {
            Ok(DapMessage::Response { request_seq: got, result, .. }) if got == request_seq => {
                return Some(result);
            }
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
}

/// Block on `events` until an `Event` named `name` arrives, discarding
/// anything else seen along the way (same caveats as
/// [`wait_for_response`]).
pub fn wait_for_event(events: &Receiver<DapMessage>, name: &str) -> Option<Value> {
    loop {
        match events.recv_blocking() {
            Ok(DapMessage::Event { event, body }) if event == name => return Some(body),
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured for real from `dart debug_adapter` (piped through a
    /// FIFO by hand): a successful `initialize` response.
    #[test]
    fn deserializes_a_real_server_initialize_response() {
        let value = serde_json::json!({
            "seq": 1,
            "type": "response",
            "body": {
                "supportsConfigurationDoneRequest": true,
                "supportsEvaluateForHovers": true,
                "exceptionBreakpointFilters": [
                    {"filter": "All", "label": "All Exceptions", "default": false},
                    {"filter": "Unhandled", "label": "Uncaught Exceptions", "default": true}
                ]
            },
            "command": "initialize",
            "request_seq": 1,
            "success": true
        });
        match parse_dap_message(value) {
            Some(DapMessage::Response { request_seq, command, result: Ok(body) }) => {
                assert_eq!(request_seq, 1);
                assert_eq!(command, "initialize");
                assert_eq!(body["supportsConfigurationDoneRequest"], true);
                assert_eq!(body["exceptionBreakpointFilters"][1]["filter"], "Unhandled");
            }
            other => panic!("expected a successful response, got {other:?}"),
        }
    }

    /// Captured for real: the `initialized` event the adapter sends
    /// right after a successful `initialize` response.
    #[test]
    fn deserializes_a_real_initialized_event() {
        let value = serde_json::json!({"seq": 2, "type": "event", "body": {}, "event": "initialized"});
        match parse_dap_message(value) {
            Some(DapMessage::Event { event, body }) => {
                assert_eq!(event, "initialized");
                assert_eq!(body, serde_json::json!({}));
            }
            other => panic!("expected an event, got {other:?}"),
        }
    }

    /// Captured for real: the error response `dart debug_adapter` sends
    /// back for an unrecognized command (`success: false`, plus a
    /// `message` alongside the usual `body`).
    #[test]
    fn deserializes_a_real_error_response() {
        let value = serde_json::json!({
            "seq": 3,
            "type": "response",
            "body": {"error": {"id": 1, "format": "{message}", "showUser": false, "variables": {"message": "Unknown command notARealCommand"}}},
            "command": "notARealCommand",
            "message": "Unknown command notARealCommand",
            "request_seq": 2,
            "success": false
        });
        match parse_dap_message(value) {
            Some(DapMessage::Response { request_seq, command, result: Err(err) }) => {
                assert_eq!(request_seq, 2);
                assert_eq!(command, "notARealCommand");
                assert_eq!(err["message"], "Unknown command notARealCommand");
                assert_eq!(err["body"]["error"]["variables"]["message"], "Unknown command notARealCommand");
            }
            other => panic!("expected a failed response, got {other:?}"),
        }
    }

    /// Captured for real from `next`/`stepIn`/`stepOut` against a real
    /// paused session: an empty-body success response, then a
    /// `continued` event, then a `stopped` event with `reason: "step"`
    /// — the same three-message shape for all three step commands.
    #[test]
    fn deserializes_a_real_step_response_and_event_sequence() {
        match parse_dap_message(serde_json::json!({
            "seq": 14, "type": "response", "command": "next", "request_seq": 5, "success": true
        })) {
            Some(DapMessage::Response { request_seq, command, result: Ok(_) }) => {
                assert_eq!(request_seq, 5);
                assert_eq!(command, "next");
            }
            other => panic!("expected a successful step response, got {other:?}"),
        }

        match parse_dap_message(serde_json::json!({
            "seq": 16, "type": "event", "body": {"allThreadsStopped": false, "reason": "step", "threadId": 1}, "event": "stopped"
        })) {
            Some(DapMessage::Event { event, body }) => {
                assert_eq!(event, "stopped");
                assert_eq!(body["reason"], "step");
            }
            other => panic!("expected a stopped(step) event, got {other:?}"),
        }
    }

    /// Captured for real: `evaluate` sent while the program was running
    /// (not paused at the given frame) — a genuine DAP error response,
    /// same shape as `deserializes_a_real_error_response` but from a
    /// real command we actually support, not a made-up one.
    #[test]
    fn deserializes_a_real_evaluate_error_response() {
        let value = serde_json::json!({
            "seq": 17,
            "type": "response",
            "body": {"error": {"format": "{message}", "id": 1, "showUser": false, "variables": {
                "message": "Evaluation is only supported when the debugger is paused unless you have a Dart file active in the editor"
            }}},
            "command": "evaluate",
            "message": "Evaluation is only supported when the debugger is paused unless you have a Dart file active in the editor",
            "request_seq": 6,
            "success": false
        });
        match parse_dap_message(value) {
            Some(DapMessage::Response { request_seq, command, result: Err(err) }) => {
                assert_eq!(request_seq, 6);
                assert_eq!(command, "evaluate");
                assert!(err["message"].as_str().unwrap().contains("only supported when the debugger is paused"));
            }
            other => panic!("expected a failed evaluate response, got {other:?}"),
        }
    }

    #[test]
    fn parses_an_adapter_to_client_request() {
        // e.g. runInTerminal: the adapter asks the client to run a
        // process for it, expecting a response via respond().
        let value = serde_json::json!({
            "seq": 10,
            "type": "request",
            "command": "runInTerminal",
            "arguments": {"cwd": "/tmp", "args": ["dart", "run"]},
        });
        match parse_dap_message(value) {
            Some(DapMessage::Request { seq, command, arguments }) => {
                assert_eq!(seq, 10);
                assert_eq!(command, "runInTerminal");
                assert_eq!(arguments["cwd"], "/tmp");
            }
            other => panic!("expected an adapter-to-client request, got {other:?}"),
        }
    }

    #[test]
    fn unknown_message_type_is_ignored() {
        let value = serde_json::json!({"seq": 1, "type": "something_else"});
        assert!(parse_dap_message(value).is_none());
    }

    /// Spawns the real `dart debug_adapter` and does a minimal
    /// `initialize` handshake. Skips (rather than fails) if `dart`
    /// isn't on `PATH` — on a machine that has it, this is a genuine
    /// end-to-end check against a real adapter, not just a parser test.
    #[test]
    fn initialize_handshake_against_the_real_dart_debug_adapter() {
        if Command::new("dart").arg("--version").output().is_err() {
            eprintln!("skipping: `dart` not found on PATH");
            return;
        }

        let client = DapClient::spawn("dart", &["debug_adapter"]).expect("failed to spawn dart debug_adapter");

        let seq = client
            .request(
                "initialize",
                serde_json::json!({
                    "clientID": "core-dap-test",
                    "adapterID": "dart",
                    "linesStartAt1": true,
                    "columnsStartAt1": true,
                    "pathFormat": "path",
                }),
            )
            .expect("failed to send initialize request");

        let response = wait_for_response(&client.events, seq).expect("adapter closed without responding");
        let body = response.expect("initialize returned an error");
        assert!(
            body["supportsConfigurationDoneRequest"].as_bool().unwrap_or(false),
            "expected the real dart debug adapter to advertise supportsConfigurationDoneRequest, got: {body}"
        );
    }

    /// End-to-end against the real `dart debug_adapter`: launch a
    /// genuine throwaway Dart script, set a real breakpoint, run until
    /// it's actually hit, then inspect the real call stack — not just
    /// a parser test against captured JSON. Skips (rather than fails)
    /// if `dart` isn't on `PATH`.
    #[test]
    fn hits_a_real_breakpoint_in_a_real_dart_script() {
        if Command::new("dart").arg("--version").output().is_err() {
            eprintln!("skipping: `dart` not found on PATH");
            return;
        }

        let dir = std::env::temp_dir().join(format!("core-dap-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("failed to create scratch dir");
        let program = dir.join("main.dart");
        std::fs::write(
            &program,
            "int add(int a, int b) {\n  final result = a + b;\n  return result;\n}\n\nvoid main() {\n  final sum = add(1, 2);\n  print('sum=$sum');\n}\n",
        )
        .expect("failed to write scratch program");

        let client = DapClient::spawn("dart", &["debug_adapter"]).expect("failed to spawn dart debug_adapter");

        let init_id = client
            .request(
                "initialize",
                serde_json::json!({
                    "clientID": "core-dap-test",
                    "adapterID": "dart",
                    "linesStartAt1": true,
                    "columnsStartAt1": true,
                    "pathFormat": "path",
                }),
            )
            .expect("failed to send initialize request");
        wait_for_response(&client.events, init_id)
            .expect("adapter closed without responding")
            .expect("initialize returned an error");

        let program_path = program.to_str().expect("scratch path should be valid utf-8");
        let dir_path = dir.to_str().expect("scratch dir should be valid utf-8");
        client.launch(program_path, dir_path).expect("failed to send launch");
        // Breakpoint on `final result = a + b;`, inside `add`.
        client.set_breakpoints(program_path, &[2]).expect("failed to send setBreakpoints");
        client.configuration_done().expect("failed to send configurationDone");

        let stopped = loop {
            match client.events.recv_blocking() {
                Ok(DapMessage::Event { event, body }) if event == "stopped" && body["reason"] == "breakpoint" => {
                    break body;
                }
                Ok(_) => continue,
                Err(_) => panic!("adapter closed before hitting the breakpoint"),
            }
        };
        let thread_id = stopped["threadId"].as_i64().expect("stopped event should carry a threadId");

        let st_id = client.stack_trace(thread_id).expect("failed to send stackTrace");
        let frames = wait_for_response(&client.events, st_id)
            .expect("adapter closed without responding")
            .expect("stackTrace returned an error");
        assert_eq!(
            frames["stackFrames"][0]["name"], "add",
            "expected the top stack frame to be the function we set the breakpoint in, got: {frames}"
        );

        client.disconnect(true).ok();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn breakpoints_json_includes_conditions_only_when_present() {
        let json = breakpoints_json(&[
            Breakpoint { line: 3, condition: None },
            Breakpoint { line: 9, condition: Some("i > 2".into()) },
            Breakpoint { line: 12, condition: Some("  ".into()) },
        ]);
        assert_eq!(json[0], serde_json::json!({"line": 3}));
        assert_eq!(json[1], serde_json::json!({"line": 9, "condition": "i > 2"}));
        assert_eq!(json[2], serde_json::json!({"line": 12}));
    }
}
