//! Dart debugging over DAP: session lifecycle, stepping, debug console lines.

use super::*;

impl DartIde {
    pub(crate) fn push_debug_line(&mut self, raw: &str) {
        let spans = editor::dock::parse_ansi_line(raw);
        let plain: String = spans.iter().map(|s| s.text.as_str()).collect();
        let file_link = editor::dock::extract_file_link(&plain, &self.root);
        self.debug_lines
            .push(editor::dock::TerminalLine::Output { spans, file_link });
        const MAX_DEBUG_LINES: usize = 5_000;
        if self.debug_lines.len() > MAX_DEBUG_LINES {
            let drain = self.debug_lines.len() - MAX_DEBUG_LINES;
            self.debug_lines.drain(0..drain);
        }
        self.debug_scroll
            .scroll_to_item(self.debug_lines.len().saturating_sub(1));
    }

    /// Launch a `core_dap::DapClient` (`dart debug_adapter`) session for the active Dart file
    /// (or `bin/main.dart` / `lib/main.dart`), registering all gutter breakpoints.
    pub(crate) fn start_dap_debug(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_dap_debug(cx);
        let program = self
            .active_editor()
            .and_then(|ed| ed.read(cx).buffer().path().map(Path::to_path_buf))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("dart"))
            .or_else(|| {
                let bin = self.root.join("bin/main.dart");
                let lib = self.root.join("lib/main.dart");
                if bin.is_file() {
                    Some(bin)
                } else if lib.is_file() {
                    Some(lib)
                } else {
                    None
                }
            });
        let Some(program_path) = program else {
            self.terminal_open = true;
            self.dock_tab = DockTab::DebugConsole;
            self.push_flutter_log(
                "\x1b[33mOpen a `.dart` file to start a DAP debug session.\x1b[0m",
            );
            cx.notify();
            return;
        };

        self.terminal_open = true;
        self.dock_tab = DockTab::DebugConsole;

        let dart_bin = if self.root.join(".fvm/flutter_sdk/bin/dart").is_file() {
            self.root
                .join(".fvm/flutter_sdk/bin/dart")
                .to_string_lossy()
                .into_owned()
        } else {
            "dart".to_string()
        };

        let client = match core_dap::DapClient::spawn(&dart_bin, &["debug_adapter"]) {
            Ok(c) => c,
            Err(err) => {
                self.push_flutter_log(&format!(
                    "\x1b[31mFailed to spawn `dart debug_adapter`: {err}\x1b[0m"
                ));
                cx.notify();
                return;
            }
        };

        let events = client.events.clone();
        let _ = client.request(
            "initialize",
            serde_json::json!({
                "adapterID": "dart",
                "linesStartAt1": true,
                "columnsStartAt1": true,
            }),
        );
        let program_str = program_path.to_string_lossy().to_string();
        let cwd_str = self.root.to_string_lossy().to_string();
        let _ = client.launch(&program_str, &cwd_str);
        for (bp_path, lines) in &self.dap_breakpoints {
            if !lines.is_empty() {
                let _ = client.set_breakpoints(&bp_path.to_string_lossy(), lines);
            }
        }
        let _ = client.configuration_done();

        self.push_flutter_log(&format!(
            "\x1b[36m\u{25b6} Started DAP debugger on {}\x1b[0m",
            program_path
                .strip_prefix(&self.root)
                .unwrap_or(&program_path)
                .display()
        ));
        let client_arc = std::sync::Arc::new(std::sync::Mutex::new(client));
        self.dap_client = Some(client_arc);
        cx.notify();

        cx.spawn_in(window, async move |weak, cx| {
            while let Ok(msg) = events.recv().await {
                let alive = weak
                    .update_in(cx, |this, window, cx| {
                        this.handle_dap_message(msg, window, cx);
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
            let _ = weak.update(cx, |this, cx| {
                this.stop_dap_debug(cx);
            });
        })
        .detach();
    }

    pub(crate) fn handle_dap_message(
        &mut self,
        msg: core_dap::DapMessage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match msg {
            core_dap::DapMessage::Event { event, body } => match event.as_str() {
                "stopped" => {
                    let thread_id = body.get("threadId").and_then(|v| v.as_i64()).unwrap_or(1);
                    let reason = body
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("breakpoint");
                    self.dap_paused_thread = Some(thread_id);
                    self.dap_stack_summary = Some(format!("Paused ({reason})"));
                    if let Some(dap) = &self.dap_client {
                        if let Ok(guard) = dap.lock() {
                            let _ = guard.stack_trace(thread_id);
                        }
                    }
                    cx.notify();
                }
                "continued" => {
                    self.dap_paused_thread = None;
                    self.dap_stack_summary = None;
                    self.dap_variables.clear();
                    self.clear_all_debug_lines(cx);
                    cx.notify();
                }
                "output" => {
                    if let Some(out) = body.get("output").and_then(|v| v.as_str()) {
                        for line in out.lines() {
                            if !line.trim().is_empty() {
                                self.push_flutter_log(line);
                            }
                        }
                        cx.notify();
                    }
                }
                "terminated" | "exited" => {
                    self.stop_dap_debug(cx);
                }
                _ => {}
            },
            core_dap::DapMessage::Response {
                command,
                result: Ok(body),
                ..
            } => match command.as_str() {
                "stackTrace" => {
                    if let Some(frame) = body
                        .get("stackFrames")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.first())
                    {
                        let frame_id = frame.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                        let fn_name = frame.get("name").and_then(|v| v.as_str()).unwrap_or("main");
                        let line_1 = frame.get("line").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
                        let src_path = frame
                            .get("source")
                            .and_then(|s| s.get("path"))
                            .and_then(|v| v.as_str())
                            .map(PathBuf::from);
                        self.dap_stack_summary = Some(format!("Paused in {fn_name}:{line_1}"));
                        if let Some(path) = src_path {
                            self.open_path(path, window, cx);
                            if let Some(ed) = self.active_editor().cloned() {
                                ed.update(cx, |editor, cx| {
                                    editor.set_active_debug_line(Some(line_1), cx);
                                });
                            }
                        }
                        if let Some(dap) = &self.dap_client {
                            if let Ok(guard) = dap.lock() {
                                let _ = guard.scopes(frame_id);
                            }
                        }
                        cx.notify();
                    }
                }
                "scopes" => {
                    if let Some(var_ref) = body
                        .get("scopes")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.first())
                        .and_then(|s| s.get("variablesReference"))
                        .and_then(|v| v.as_i64())
                        .filter(|&r| r > 0)
                    {
                        if let Some(dap) = &self.dap_client {
                            if let Ok(guard) = dap.lock() {
                                let _ = guard.variables(var_ref);
                            }
                        }
                    }
                }
                "variables" => {
                    if let Some(arr) = body.get("variables").and_then(|v| v.as_array()) {
                        self.dap_variables = arr
                            .iter()
                            .take(8)
                            .filter_map(|v| {
                                Some((
                                    v.get("name")?.as_str()?.to_string(),
                                    v.get("value")?.as_str()?.to_string(),
                                ))
                            })
                            .collect();
                        cx.notify();
                    }
                }
                _ => {}
            },
            core_dap::DapMessage::Request { seq, command, .. } => {
                if let Some(dap) = &self.dap_client {
                    if let Ok(guard) = dap.lock() {
                        let _ = guard.respond(seq, &command, serde_json::json!({}));
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn clear_all_debug_lines(&mut self, cx: &mut Context<Self>) {
        let editors: Vec<_> = self.tabs.iter().map(|t| t.editor.clone()).collect();
        for ed in editors {
            ed.update(cx, |editor, cx| editor.set_active_debug_line(None, cx));
        }
    }

    pub(crate) fn stop_dap_debug(&mut self, cx: &mut Context<Self>) {
        if let Some(dap) = self.dap_client.take() {
            if let Ok(mut guard) = dap.lock() {
                let _ = guard.disconnect(true);
                let _ = guard.kill();
            }
            self.dap_paused_thread = None;
            self.dap_stack_summary = None;
            self.dap_variables.clear();
            self.clear_all_debug_lines(cx);
            self.push_flutter_log("\x1b[90m\u{25a0} DAP debug session ended.\x1b[0m");
            cx.notify();
        }
    }

    pub(crate) fn dap_continue(&mut self, cx: &mut Context<Self>) {
        let (Some(dap), Some(tid)) = (&self.dap_client, self.dap_paused_thread) else {
            return;
        };
        if let Ok(guard) = dap.lock() {
            let _ = guard.continue_thread(tid);
        }
        self.dap_paused_thread = None;
        self.dap_variables.clear();
        self.clear_all_debug_lines(cx);
        cx.notify();
    }

    pub(crate) fn dap_step_over(&mut self, cx: &mut Context<Self>) {
        let (Some(dap), Some(tid)) = (&self.dap_client, self.dap_paused_thread) else {
            return;
        };
        if let Ok(guard) = dap.lock() {
            let _ = guard.next(tid);
        }
        cx.notify();
    }

    pub(crate) fn dap_step_in(&mut self, cx: &mut Context<Self>) {
        let (Some(dap), Some(tid)) = (&self.dap_client, self.dap_paused_thread) else {
            return;
        };
        if let Ok(guard) = dap.lock() {
            let _ = guard.step_in(tid);
        }
        cx.notify();
    }

    pub(crate) fn dap_step_out(&mut self, cx: &mut Context<Self>) {
        let (Some(dap), Some(tid)) = (&self.dap_client, self.dap_paused_thread) else {
            return;
        };
        if let Ok(guard) = dap.lock() {
            let _ = guard.step_out(tid);
        }
        cx.notify();
    }
}
