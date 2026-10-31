//! Dart debugging over DAP: session lifecycle, stepping, debug console lines.

use super::*;

/// How many locals the debug dock lists (the adapter may return many more).
const MAX_SHOWN_VARIABLES: usize = 30;

/// One variable shown in the debug dock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DapVar {
    pub name: String,
    pub value: String,
    /// DAP `variablesReference`; `> 0` means it has children to expand.
    pub var_ref: i64,
    pub depth: usize,
}

/// Maximum nesting shown, which also bounds self-referential structures.
const MAX_VARIABLE_DEPTH: usize = 6;

/// Top-level variables followed (recursively) by the children of every
/// expanded variable, with `depth` set for indentation.
pub(crate) fn flatten_variables(
    top: &[DapVar],
    children: &HashMap<i64, Vec<DapVar>>,
    expanded: &std::collections::HashSet<i64>,
) -> Vec<DapVar> {
    fn walk(
        vars: &[DapVar],
        depth: usize,
        children: &HashMap<i64, Vec<DapVar>>,
        expanded: &std::collections::HashSet<i64>,
        out: &mut Vec<DapVar>,
    ) {
        for var in vars {
            out.push(DapVar { depth, ..var.clone() });
            if depth < MAX_VARIABLE_DEPTH && var.var_ref > 0 && expanded.contains(&var.var_ref) {
                if let Some(kids) = children.get(&var.var_ref) {
                    walk(kids, depth + 1, children, expanded, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(top, 0, children, expanded, &mut out);
    out
}

fn parse_variables(body: &serde_json::Value) -> Vec<DapVar> {
    body.get("variables")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .take(MAX_SHOWN_VARIABLES)
                .filter_map(|v| {
                    Some(DapVar {
                        name: v.get("name")?.as_str()?.to_string(),
                        value: v.get("value")?.as_str()?.to_string(),
                        var_ref: v.get("variablesReference").and_then(|r| r.as_i64()).unwrap_or(0),
                        depth: 0,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What the debug input prompt is collecting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DebugPrompt {
    Watch,
    /// A condition for the breakpoint at `(file, 1-indexed line)`.
    BreakpointCondition(PathBuf, u32),
}

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
                let _ = client.set_breakpoints_ex(
                    &bp_path.to_string_lossy(),
                    &self.breakpoints_with_conditions(bp_path, lines),
                );
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
                    self.clear_dap_variables();
                    self.dap_frame_id = None;
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
                request_seq,
                command,
                result: Ok(body),
            } => match command.as_str() {
                "evaluate" => {
                    if let Some(expr) = self.dap_eval_pending.remove(&request_seq) {
                        let value = body
                            .get("result")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        self.dap_watch_values.insert(expr, value);
                        cx.notify();
                    }
                }
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
                        self.dap_frame_id = Some(frame_id);
                        self.refresh_watches();
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
                    let vars = parse_variables(&body);
                    match self.dap_var_pending.remove(&request_seq) {
                        Some(parent_ref) => {
                            self.dap_children.insert(parent_ref, vars);
                        }
                        None => self.dap_variables = vars,
                    }
                    cx.notify();
                }
                _ => {}
            },
            core_dap::DapMessage::Response { request_seq, result: Err(err), .. } => {
                if let Some(expr) = self.dap_eval_pending.remove(&request_seq) {
                    let message = err
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("error")
                        .to_string();
                    self.dap_watch_values.insert(expr, format!("<{message}>"));
                    cx.notify();
                }
            }
            core_dap::DapMessage::Request { seq, command, .. } => {
                if let Some(dap) = &self.dap_client {
                    if let Ok(guard) = dap.lock() {
                        let _ = guard.respond(seq, &command, serde_json::json!({}));
                    }
                }
            }
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
            self.clear_dap_variables();
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
        self.clear_dap_variables();
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

    /// Breakpoint list for `path` with any conditions attached.
    pub(crate) fn breakpoints_with_conditions(&self, path: &Path, lines: &[u32]) -> Vec<core_dap::Breakpoint> {
        let conditions = self.dap_conditions.get(path);
        lines
            .iter()
            .map(|&line| core_dap::Breakpoint {
                line,
                condition: conditions.and_then(|c| c.get(&line)).cloned(),
            })
            .collect()
    }

    /// Push `path`'s breakpoints (and conditions) to the running adapter.
    pub(crate) fn send_breakpoints(&self, path: &Path) {
        let Some(dap) = &self.dap_client else { return };
        let lines = self.dap_breakpoints.get(path).cloned().unwrap_or_default();
        let breakpoints = self.breakpoints_with_conditions(path, &lines);
        if let Ok(guard) = dap.lock() {
            let _ = guard.set_breakpoints_ex(&path.to_string_lossy(), &breakpoints);
        }
    }

    /// Re-evaluate every watch expression in the paused frame.
    pub(crate) fn refresh_watches(&mut self) {
        let (Some(frame), Some(dap)) = (self.dap_frame_id, self.dap_client.clone()) else {
            return;
        };
        self.dap_eval_pending.clear();
        let Ok(guard) = dap.lock() else { return };
        for expr in &self.dap_watches {
            if let Ok(seq) = guard.evaluate(expr, frame, "watch") {
                self.dap_eval_pending.insert(seq, expr.clone());
            }
        }
    }

    pub(crate) fn open_debug_prompt(&mut self, kind: DebugPrompt, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = match &kind {
            DebugPrompt::Watch => "Expression to watch (e.g. items.length)",
            DebugPrompt::BreakpointCondition(..) => "Stop only when… (e.g. i > 2); empty clears",
        };
        self.debug_prompt = Some(kind);
        self.debug_prompt_input.update(cx, |state, cx| {
            state.set_placeholder(placeholder, window, cx);
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    /// Prompt for a condition on the breakpoint at the cursor line, creating
    /// the breakpoint first if the line has none.
    pub(crate) fn open_breakpoint_condition_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor().cloned() else { return };
        let (path, line_1) = {
            let ed = editor.read(cx);
            let Some(path) = ed.buffer().path().map(|p| p.to_path_buf()) else { return };
            (path, ed.cursor_position().0)
        };
        if !editor.read(cx).breakpoints().contains(&line_1) {
            editor.update(cx, |ed, cx| ed.toggle_breakpoint(line_1, cx));
        }
        self.open_debug_prompt(DebugPrompt::BreakpointCondition(path, line_1), window, cx);
    }

    pub(crate) fn submit_debug_prompt(&mut self, cx: &mut Context<Self>) {
        let Some(kind) = self.debug_prompt.take() else { return };
        let text = self.debug_prompt_input.read(cx).value().trim().to_string();
        match kind {
            DebugPrompt::Watch => {
                if !text.is_empty() && !self.dap_watches.contains(&text) {
                    self.dap_watches.push(text);
                    self.refresh_watches();
                }
            }
            DebugPrompt::BreakpointCondition(path, line) => {
                let conditions = self.dap_conditions.entry(path.clone()).or_default();
                if text.is_empty() {
                    conditions.remove(&line);
                } else {
                    conditions.insert(line, text);
                }
                self.send_breakpoints(&path);
            }
        }
        cx.notify();
    }

    pub(crate) fn render_debug_prompt(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> Option<Div> {
        let kind = self.debug_prompt.as_ref()?;
        let title = match kind {
            DebugPrompt::Watch => "Add watch expression",
            DebugPrompt::BreakpointCondition(..) => "Breakpoint condition",
        };
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(84.))
                .bg(rgba(0x00000066))
                .child(
                    v_flex()
                        .w(px(420.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .child(
                            div()
                                .px_3()
                                .py_2()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(title),
                        )
                        .child(
                            div()
                                .px_3()
                                .pb_3()
                                .on_key_down({
                                    let view = view.clone();
                                    move |event, _window, cx| {
                                        let key = event.keystroke.key.as_str();
                                        if key == "escape" {
                                            view.update(cx, |this, cx| {
                                                this.debug_prompt = None;
                                                cx.notify();
                                            });
                                        } else if key == "enter" {
                                            view.update(cx, |this, cx| this.submit_debug_prompt(cx));
                                        }
                                    }
                                })
                                .child(
                                    h_flex()
                                        .h(px(28.))
                                        .px_2p5()
                                        .items_center()
                                        .rounded(px(tokens::RADIUS_SM))
                                        .bg(cx.theme().background)
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .child(Input::new(&self.debug_prompt_input).appearance(false).small()),
                                ),
                        ),
                ),
        )
    }

    pub(crate) fn clear_dap_variables(&mut self) {
        self.dap_variables.clear();
        self.dap_children.clear();
        self.dap_expanded.clear();
        self.dap_var_pending.clear();
    }

    /// Expand or collapse a variable with children, fetching them on first expand.
    pub(crate) fn toggle_dap_variable(&mut self, var_ref: i64, cx: &mut Context<Self>) {
        if self.dap_expanded.remove(&var_ref) {
            cx.notify();
            return;
        }
        self.dap_expanded.insert(var_ref);
        if !self.dap_children.contains_key(&var_ref) {
            if let Some(dap) = &self.dap_client {
                if let Ok(guard) = dap.lock() {
                    if let Ok(seq) = guard.variables(var_ref) {
                        self.dap_var_pending.insert(seq, var_ref);
                    }
                }
            }
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{DapVar, flatten_variables};
    use std::collections::{HashMap, HashSet};

    fn var(name: &str, var_ref: i64) -> DapVar {
        DapVar { name: name.into(), value: "v".into(), var_ref, depth: 0 }
    }

    #[test]
    fn flattens_only_expanded_children_with_depth() {
        let top = vec![var("a", 1), var("b", 0), var("c", 2)];
        let children = HashMap::from([(1, vec![var("a.x", 3)]), (3, vec![var("a.x.y", 0)])]);
        let collapsed = flatten_variables(&top, &children, &HashSet::new());
        assert_eq!(collapsed.len(), 3);

        let expanded = HashSet::from([1, 3]);
        let flat = flatten_variables(&top, &children, &expanded);
        let shape: Vec<(&str, usize)> = flat.iter().map(|v| (v.name.as_str(), v.depth)).collect();
        assert_eq!(shape, vec![("a", 0), ("a.x", 1), ("a.x.y", 2), ("b", 0), ("c", 0)]);
    }

    #[test]
    fn self_referential_children_stop_at_max_depth() {
        let top = vec![var("loop", 1)];
        let children = HashMap::from([(1, vec![var("loop", 1)])]);
        let flat = flatten_variables(&top, &children, &HashSet::from([1]));
        assert!(flat.len() <= 8, "recursion must be bounded, got {}", flat.len());
    }
}
