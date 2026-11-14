//! Working with turns of the agent conversation after they happen: editing a
//! message and sending it again, retrying a reply, rewinding the workspace to
//! before a message, and undoing (or redoing) what one turn changed. Also the
//! transcript pieces those need: the per-turn changes card, the error card,
//! the reasoning row, nested helper-agent calls, the task list pinned above
//! the composer and the context meter.

use std::path::Path;

use core_workspace::{ChangeKind, ChangedFile};
use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::agent_checkpoint::{self, Checkpoint, FileChange, TurnFile};
use crate::rails::agent::{
    AgentHarness, AgentItem, AgentToolCall, AgentToolKind, ErrorKind, ToolStatus, TurnInput,
};
use crate::{DartIde, tokens};

/// The model's context window when the CLI doesn't report one.
fn default_context_window(harness: AgentHarness, model_id: &str) -> Option<u64> {
    let id = model_id.to_ascii_lowercase();
    match harness {
        _ if id.contains("[1m]") => Some(1_000_000),
        AgentHarness::ClaudeCode => Some(200_000),
        AgentHarness::Antigravity => Some(1_000_000),
        AgentHarness::Codex | AgentHarness::OpenCode => None,
    }
}

/// `src/app/main.rs` → (`main.rs`, `src/app`).
fn split_path(path: &str) -> (String, String) {
    let p = Path::new(path);
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or(path).to_string();
    let dir = p.parent().and_then(|d| d.to_str()).unwrap_or("").to_string();
    (name, dir)
}

/// A small text button for card rows.
fn chip(id: impl Into<ElementId>, label: &str, color: Hsla, hover_bg: Hsla) -> Stateful<Div> {
    div()
        .id(id.into())
        .flex_shrink_0()
        .px(px(6.))
        .py(px(2.))
        .rounded(px(tokens::RADIUS_SM))
        .cursor_pointer()
        .text_size(px(tokens::UI_XS))
        .text_color(color)
        .hover(move |s| s.bg(hover_bg))
        .child(label.to_string())
}

impl DartIde {
    // ----- finding things in the transcript -------------------------------

    /// The turn record (tool group) that follows the user message at `user_ix`.
    pub(crate) fn agent_turn_group(&self, user_ix: usize) -> Option<usize> {
        self.agent
            .items
            .iter()
            .enumerate()
            .skip(user_ix + 1)
            .take_while(|(_, item)| !matches!(item, AgentItem::Message { is_user: true, .. }))
            .find(|(_, item)| matches!(item, AgentItem::ToolGroup { .. }))
            .map(|(ix, _)| ix)
    }

    /// The workspace as it was just before the message at `user_ix` was sent.
    pub(crate) fn agent_turn_checkpoint(&self, user_ix: usize) -> Option<Checkpoint> {
        match self.agent.items.get(self.agent_turn_group(user_ix)?)? {
            AgentItem::ToolGroup { checkpoint, .. } => checkpoint.clone(),
            _ => None,
        }
    }

    /// What the message at `user_ix` asked. Messages restored from disk lost
    /// their attachments; their text (minus the "Attached" note) is used.
    fn agent_turn_input(&self, user_ix: usize) -> Option<TurnInput> {
        if let Some(input) = self.agent.turn_inputs.get(&user_ix) {
            return Some(input.clone());
        }
        match self.agent.items.get(user_ix)? {
            AgentItem::Message { is_user: true, text } => Some(TurnInput {
                prompt: text.split("\n\n_Attached:_").next().unwrap_or(text).trim().to_string(),
                attachments: Vec::new(),
            }),
            _ => None,
        }
    }

    fn last_user_message(&self) -> Option<usize> {
        self.agent
            .items
            .iter()
            .rposition(|item| matches!(item, AgentItem::Message { is_user: true, .. }))
    }

    /// Files of the turn whose checkpoints are `before..after`, for stepping
    /// through them in the diff view.
    pub(crate) fn agent_turn_changed_files(&self, range: &str) -> Vec<ChangedFile> {
        self.agent
            .items
            .iter()
            .find_map(|item| match item {
                AgentItem::Changes { before, after, files, .. }
                    if format!("{}..{after}", before.tree) == range =>
                {
                    Some(files.iter().map(turn_changed_file).collect())
                }
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Where the agent's pending change to `path` started: the checkpoint
    /// before the first turn (not undone) that changed it.
    pub(crate) fn agent_base_checkpoint(&self, path: &str) -> Option<Checkpoint> {
        self.agent.items.iter().find_map(|item| match item {
            AgentItem::Changes { before, files, .. }
                if files.iter().any(|f| f.path == path && !f.reverted) =>
            {
                Some(before.clone())
            }
            _ => None,
        })
    }

    fn agent_busy(&mut self, cx: &mut Context<Self>) -> bool {
        if self.agent.running_pid.is_some() {
            self.show_toast("Stop the agent first.", true, cx);
            return true;
        }
        false
    }

    /// Show what is on disk now, after files were put back.
    fn after_agent_file_restore(&mut self, cx: &mut Context<Self>) {
        self.check_disk_changes(cx);
        self.refresh_git(cx);
        self.sync_workspace_files_if_changed(cx);
    }

    // ----- editing, retrying, rewinding --------------------------------------

    /// Drop the conversation from `ix` on. The CLI's session still remembers
    /// those turns, so the next message starts a fresh one and brings what is
    /// left of the conversation as text.
    fn agent_truncate(&mut self, ix: usize) {
        self.agent.items.truncate(ix);
        self.agent.turn_stats.retain(|k, _| *k < ix);
        self.agent.turn_inputs.retain(|k, _| *k < ix);
        self.agent.question_inputs.retain(|(k, _), _| *k < ix);
        self.agent.restored_items = self.agent.restored_items.min(ix);
        self.agent.tasks.clear();
        self.agent.session_id = None;
        self.agent.usage = Default::default();
        self.agent.replay_transcript = !self.agent.items.is_empty();
    }

    /// Put the message at `user_ix` back in the composer to change and send
    /// again; everything from it on leaves the conversation. With
    /// `restore_files` the workspace also goes back to how it was before it.
    pub(crate) fn agent_edit_message(
        &mut self,
        user_ix: usize,
        restore_files: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.agent_busy(cx) {
            return;
        }
        let Some(input) = self.agent_turn_input(user_ix) else { return };
        if restore_files {
            let Some(checkpoint) = self.agent_turn_checkpoint(user_ix) else {
                self.show_toast("No checkpoint was taken for that message.", true, cx);
                return;
            };
            match agent_checkpoint::rewind_to(&checkpoint) {
                Ok(0) => self.show_toast("Files already match that point.", false, cx),
                Ok(n) => self.show_toast(
                    format!("Restored {n} file{} to before that message.", if n == 1 { "" } else { "s" }),
                    false,
                    cx,
                ),
                Err(e) => {
                    self.show_toast(e, true, cx);
                    return;
                }
            }
            self.agent.ai_touched_paths.clear();
            self.after_agent_file_restore(cx);
        }
        self.agent_truncate(user_ix);
        self.agent.attachments = input.attachments;
        self.agent.history_open = false;
        self.agent.input.update(cx, |state, cx| {
            state.set_value(input.prompt, window, cx);
            state.focus(window, cx);
        });
        self.save_agent_conversation();
        cx.notify();
    }

    /// Run the message at `user_ix` again, dropping everything after it.
    /// `keep_session` resumes the CLI's session (after a failure, which left
    /// no answer in it); otherwise it starts fresh so the old answer is gone.
    pub(crate) fn agent_retry(&mut self, user_ix: usize, keep_session: bool, cx: &mut Context<Self>) {
        if self.agent_busy(cx) {
            return;
        }
        let Some(input) = self.agent_turn_input(user_ix) else { return };
        let session = self.agent.session_id.clone();
        self.agent_truncate(user_ix);
        if keep_session && session.is_some() {
            self.agent.session_id = session;
            self.agent.replay_transcript = false;
        }
        self.agent.attachments = input.attachments;
        self.agent_run_prompt(input.prompt, cx);
    }

    /// `/retry`: the last message again.
    pub(crate) fn agent_retry_last(&mut self, cx: &mut Context<Self>) {
        let Some(user_ix) = self.last_user_message() else {
            self.show_toast("Nothing to retry yet.", false, cx);
            return;
        };
        let failed = matches!(self.agent.items.last(), Some(AgentItem::Error { .. }));
        self.agent_retry(user_ix, failed, cx);
    }

    /// `/compact`: free up context. Claude Code summarizes its own session;
    /// for the others a new session starts with the recent conversation.
    pub(crate) fn agent_compact(&mut self, cx: &mut Context<Self>) {
        if self.agent_busy(cx) {
            return;
        }
        if self.agent.items.is_empty() {
            self.show_toast("Nothing to compact yet.", false, cx);
            return;
        }
        if self.agent.harness == AgentHarness::ClaudeCode && self.agent.session_id.is_some() {
            self.agent.raw_next = true;
            self.agent_run_prompt("/compact".to_string(), cx);
            return;
        }
        self.agent.session_id = None;
        self.agent.replay_transcript = true;
        self.agent.usage = Default::default();
        self.agent.items.push(AgentItem::Message {
            is_user: false,
            text: "_Started a fresh session. Your next message brings the recent conversation along as text._"
                .to_string(),
        });
        self.save_agent_conversation();
        cx.notify();
    }

    // ----- undoing changes ---------------------------------------------------

    /// Put back what the turn card at `item_ix` changed: one file, or all.
    pub(crate) fn agent_undo_changes(&mut self, item_ix: usize, file_ix: Option<usize>, cx: &mut Context<Self>) {
        self.agent_set_reverted(item_ix, file_ix, true, cx);
    }

    /// Re-apply what an undo put back.
    pub(crate) fn agent_redo_changes(&mut self, item_ix: usize, file_ix: Option<usize>, cx: &mut Context<Self>) {
        self.agent_set_reverted(item_ix, file_ix, false, cx);
    }

    fn agent_set_reverted(&mut self, item_ix: usize, file_ix: Option<usize>, revert: bool, cx: &mut Context<Self>) {
        let Some(AgentItem::Changes { before, after, files, .. }) = self.agent.items.get(item_ix) else {
            return;
        };
        let targets: Vec<usize> = (0..files.len())
            .filter(|ix| file_ix.is_none_or(|only| only == *ix))
            .filter(|ix| files[*ix].reverted != revert)
            .collect();
        if targets.is_empty() {
            return;
        }
        let paths: Vec<String> = targets.iter().map(|ix| files[*ix].path.clone()).collect();
        let source = if revert {
            before.clone()
        } else {
            Checkpoint { repo: before.repo.clone(), tree: after.clone() }
        };
        match agent_checkpoint::restore(&source, &paths) {
            Ok(_) => {
                if let Some(AgentItem::Changes { files, .. }) = self.agent.items.get_mut(item_ix) {
                    for ix in &targets {
                        files[*ix].reverted = revert;
                    }
                }
                if revert {
                    self.agent.ai_touched_paths.retain(|p| !paths.contains(p));
                } else {
                    for p in &paths {
                        if !self.agent.ai_touched_paths.contains(p) {
                            self.agent.ai_touched_paths.push(p.clone());
                        }
                    }
                }
                let n = paths.len();
                let what = if n == 1 {
                    split_path(&paths[0]).0
                } else {
                    format!("{n} files")
                };
                self.show_toast(
                    if revert { format!("Undid the changes to {what}") } else { format!("Re-applied {what}") },
                    false,
                    cx,
                );
            }
            Err(e) => self.show_toast(e, true, cx),
        }
        self.after_agent_file_restore(cx);
        self.save_agent_conversation();
        cx.notify();
    }

    /// `/undo`: the most recent turn that still has changes in effect.
    pub(crate) fn agent_undo_last(&mut self, cx: &mut Context<Self>) {
        let last = self.agent.items.iter().rposition(|item| {
            matches!(item, AgentItem::Changes { files, .. } if files.iter().any(|f| !f.reverted))
        });
        match last {
            Some(ix) => self.agent_undo_changes(ix, None, cx),
            None => self.show_toast("No agent changes to undo.", false, cx),
        }
    }

    /// Throw away the agent's pending changes to `file`: back to before the
    /// agent touched it, keeping the user's own earlier edits. Without a
    /// checkpoint (not a git repo, or an older thread) it falls back to the
    /// git discard, which asks for a second click.
    pub(crate) fn agent_reject_file(&mut self, file: ChangedFile, window: &Window, cx: &mut Context<Self>) {
        if let Some(base) = self.agent_base_checkpoint(&file.path) {
            match agent_checkpoint::restore(&base, std::slice::from_ref(&file.path)) {
                Ok(_) => {
                    for item in &mut self.agent.items {
                        if let AgentItem::Changes { files, .. } = item {
                            for f in files.iter_mut().filter(|f| f.path == file.path) {
                                f.reverted = true;
                            }
                        }
                    }
                    self.agent.ai_touched_paths.retain(|p| p != &file.path);
                    self.after_agent_file_restore(cx);
                    self.save_agent_conversation();
                }
                Err(e) => self.show_toast(e, true, cx),
            }
            cx.notify();
            return;
        }
        let armed = self.git.confirm_discard.as_deref() == Some(file.path.as_str());
        let path = file.path.clone();
        self.git_discard_clicked(file, window, cx);
        if armed {
            self.agent.ai_touched_paths.retain(|p| p != &path);
        }
        cx.notify();
    }

    /// Open one file of a turn in the diff view: before vs after that turn.
    pub(crate) fn agent_open_turn_diff(&mut self, item_ix: usize, file_ix: usize, cx: &mut Context<Self>) {
        let Some(AgentItem::Changes { before, after, files, .. }) = self.agent.items.get(item_ix) else {
            return;
        };
        let Some(file) = files.get(file_ix) else { return };
        let range = format!("{}..{after}", before.tree);
        let changed = turn_changed_file(file);
        self.open_git_commit_diff(range, changed, cx);
    }

    /// A code block's Insert: put the code at the cursor of the open file.
    pub(crate) fn agent_insert_code(&mut self, code: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor().cloned() else {
            self.show_toast("Open a file to insert the code into.", true, cx);
            return;
        };
        let name = editor
            .read(cx)
            .buffer()
            .path()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "the editor".to_string());
        editor.update(cx, |ed, cx| ed.insert_text(&code, cx));
        let focus = editor.read(cx).focus_handle(cx);
        focus.focus(window, cx);
        self.show_toast(format!("Inserted into {name}"), false, cx);
    }

    /// Run a shell code block from an agent reply in the dock terminal.
    pub(crate) fn agent_run_code_in_terminal(&mut self, code: String, cx: &mut Context<Self>) {
        let trimmed = code.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        self.terminal_open = true;
        self.dock_tab = crate::DockTab::Terminal;
        self.execute_terminal_command(trimmed, cx);
        self.show_toast("Running command in Terminal", false, cx);
    }

    /// Open a markdown link from an agent reply: workspace files open at the
    /// target line in the editor; web URLs open in the system browser.
    pub(crate) fn agent_open_markdown_link(&mut self, raw_url: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((path_str, line_1)) = crate::markdown::parse_file_link_target(&raw_url) {
            let p = Path::new(&path_str);
            let full = if p.is_absolute() { p.to_path_buf() } else { self.root.join(p) };
            if full.exists() {
                self.open_path(full, window, cx);
                if let Some(line) = line_1
                    && let Some(editor) = self.active_editor().cloned()
                {
                    let line_0 = line.saturating_sub(1) as u32;
                    editor.update(cx, |ed, cx| ed.set_cursor_to_position(line_0, 0, cx));
                }
                return;
            }
        }
        if raw_url.starts_with("http://") || raw_url.starts_with("https://") {
            let _ = std::process::Command::new("open").arg(&raw_url).spawn();
        }
    }

    // ----- rendering ---------------------------------------------------------

    /// The reasoning row at the top of a turn: collapsed to one line, opens to
    /// the full text.
    pub(crate) fn render_turn_thinking(
        &self,
        item_ix: usize,
        thinking: &str,
        open: bool,
        live: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let border = cx.theme().border;
        let words = thinking.split_whitespace().count();
        let label = if live { "Thinking\u{2026}".to_string() } else { format!("Thought \u{b7} {words} words") };
        v_flex()
            .w_full()
            .gap(px(tokens::SPACE_1))
            .child(
                h_flex()
                    .id(("agent-thinking", item_ix))
                    .items_center()
                    .gap(px(tokens::SPACE_2))
                    .cursor_pointer()
                    .text_size(px(tokens::UI_SM))
                    .text_color(muted)
                    .hover(move |s| s.text_color(fg))
                    .child(Icon::new(IconName::Brain).with_size(px(11.5)).text_color(muted))
                    .child(label)
                    .child(
                        Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                            .with_size(px(11.))
                            .text_color(muted),
                    )
                    .on_click(cx.listener(move |this, _e, _w, cx| {
                        if let Some(AgentItem::ToolGroup { thinking_open, .. }) = this.agent.items.get_mut(item_ix) {
                            *thinking_open = !*thinking_open;
                            cx.notify();
                        }
                    })),
            )
            .when(open, |el| {
                el.child(
                    v_flex()
                        .w_full()
                        .ml(px(5.))
                        .pl(px(tokens::SPACE_3))
                        .border_l_1()
                        .border_color(border)
                        .gap(px(4.))
                        .text_size(px(tokens::UI_SM))
                        .line_height(px(17.))
                        .text_color(muted)
                        .children(
                            thinking
                                .split("\n\n")
                                .filter(|p| !p.trim().is_empty())
                                .map(|p| div().w_full().child(p.trim().replace('\n', " "))),
                        ),
                )
            })
            .into_any_element()
    }

    /// A turn's tool calls; a helper agent's own calls fold under the call
    /// that started it.
    pub(crate) fn render_tool_rows(&self, item_ix: usize, calls: &[AgentToolCall], cx: &mut Context<Self>) -> Vec<AnyElement> {
        let ids: std::collections::HashSet<&str> = calls.iter().filter_map(|c| c.id.as_deref()).collect();
        let mut rows = Vec::new();
        for (call_ix, call) in calls.iter().enumerate() {
            // Children are drawn under their parent.
            if call.parent_id.as_deref().is_some_and(|p| ids.contains(p)) {
                continue;
            }
            let children: Vec<(usize, &AgentToolCall)> = match call.id.as_deref() {
                Some(id) => calls
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.parent_id.as_deref() == Some(id))
                    .collect(),
                None => Vec::new(),
            };
            let open = call.id.as_ref().is_some_and(|id| self.agent.open_subagents.contains(id));
            rows.push(self.render_tool_row(item_ix, call_ix, call, Some((children.len(), open)), false, cx));
            if open {
                for (child_ix, child) in children {
                    rows.push(self.render_tool_row(item_ix, child_ix, child, None, true, cx));
                }
            }
        }
        rows
    }

    fn render_tool_row(
        &self,
        item_ix: usize,
        call_ix: usize,
        call: &AgentToolCall,
        sub: Option<(usize, bool)>,
        nested: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let info = cx.theme().info;
        let green = cx.theme().green;
        let warning = cx.theme().warning;
        let border = cx.theme().border;
        let card_bg = cx.theme().muted;
        let list_hover = cx.theme().list_hover;
        let red = cx.theme().red;
        let mono_font = cx.theme().mono_font_family.clone();

        let target = call.target_path.clone();
        let is_term = call.kind == AgentToolKind::Terminal;
        let (sub_count, sub_open) = sub.unwrap_or((0, false));
        let toggle_id = call.id.clone().filter(|_| sub_count > 0);

        let (badge_text, badge_color, subject) = if is_term {
            ("RUN", primary, call.label.as_str())
        } else if sub_count > 0 || call.label.starts_with("Start a helper") || call.label.starts_with("Task") {
            let rest = call
                .label
                .strip_prefix("Task ")
                .or_else(|| call.label.strip_prefix("Start a helper: "))
                .unwrap_or(call.label.as_str());
            ("AGENT", primary, rest)
        } else if let Some((verb, rest)) = call.label.split_once(' ') {
            let (b, col) = match verb {
                "Read" => ("READ", info),
                "Edit" => ("EDIT", warning),
                "Write" => ("WRITE", green),
                "List" => ("LIST", info),
                "Grep" | "Glob" | "Search" => ("SEARCH", primary),
                _ => (verb, muted),
            };
            (b, col, rest)
        } else {
            ("TOOL", muted, call.label.as_str())
        };

        let parent_dir = target.as_deref().and_then(|rel| {
            let p = Path::new(rel);
            let stripped = p.strip_prefix(&self.root).unwrap_or(p);
            stripped
                .parent()
                .and_then(|d| d.to_str())
                .filter(|s| !s.is_empty() && *s != ".")
                .map(str::to_string)
        });

        let detail_str = call.detail.clone();
        let is_multiline_detail = detail_str.as_ref().is_some_and(|d| d.contains('\n'));
        let inline_detail = detail_str.as_ref().filter(|d| !d.contains('\n')).cloned();
        let detail_open = is_multiline_detail && self.agent.open_tool_details.contains(&(item_ix, call_ix));

        v_flex()
            .w_full()
            .when(nested, |el| el. pl(px(tokens::SPACE_5)))
            .gap(px(3.))
            .child(
                h_flex()
                    .id(("agent-tool-card", item_ix * 512 + call_ix))
                    .w_full()
                    .min_h(px(if nested { 26. } else { 32. }))
                    .px(px(tokens::SPACE_3))
                    .py(px(if nested { 3. } else { tokens::SPACE_2 }))
                    .items_center()
                    .gap(px(tokens::SPACE_2))
                    .rounded(px(tokens::RADIUS_CARD))
                    .bg(if nested { card_bg.opacity(0.5) } else { card_bg })
                    .border_1()
                    .border_color(if call.status == ToolStatus::Failed { red.opacity(0.5) } else { border })
                    .when(target.is_some() || toggle_id.is_some() || is_multiline_detail, |s| {
                        s.cursor_pointer().hover(move |h| h.bg(list_hover))
                    })
                    .child(if is_term {
                        div()
                            .font_family(mono_font.clone())
                            .text_size(px(tokens::UI_XS))
                            .text_color(primary)
                            .child(">_")
                            .into_any_element()
                    } else if badge_text == "AGENT" {
                        Icon::new(IconName::Bot).with_size(px(13.)).text_color(primary).into_any_element()
                    } else if badge_text == "SEARCH" {
                        Icon::new(IconName::Search).with_size(px(12.5)).text_color(primary).into_any_element()
                    } else if badge_text == "LIST" {
                        Icon::new(IconName::Folder).with_size(px(13.)).text_color(info).into_any_element()
                    } else if target.is_some() {
                        crate::rails::files::render_file_icon_sized(subject, 13.).into_any_element()
                    } else {
                        Icon::new(IconName::FileCode).with_size(px(13.)).text_color(info).into_any_element()
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .px(px(5.))
                            .py(px(1.))
                            .rounded(px(3.))
                            .bg(badge_color.opacity(0.14))
                            .font_family(mono_font.clone())
                            .text_size(px(9.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(badge_color)
                            .child(badge_text.to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(if nested { tokens::UI_XS } else { tokens::UI_SM }))
                            .text_color(if call.status == ToolStatus::Failed { red } else { fg })
                            .when(is_term, |el| el.font_family(mono_font.clone()))
                            .child(subject.to_string()),
                    )
                    .when_some(parent_dir, |el, dir| {
                        el.child(
                            div()
                                .max_w(px(130.))
                                .truncate()
                                .text_size(px(tokens::UI_XS))
                                .text_color(muted.opacity(0.8))
                                .child(dir),
                        )
                    })
                    .when_some(inline_detail, |el, info_lbl| {
                        el.child(
                            div()
                                .flex_shrink_0()
                                .px(px(5.))
                                .py(px(1.))
                                .rounded(px(3.))
                                .bg(border.opacity(0.45))
                                .font_family(mono_font.clone())
                                .text_size(px(10.))
                                .text_color(muted)
                                .child(info_lbl),
                        )
                    })
                    .when(is_multiline_detail, |el| {
                        el.child(
                            h_flex()
                                .id(("agent-tool-detail-btn", item_ix * 512 + call_ix))
                                .flex_shrink_0()
                                .items_center()
                                .gap(px(2.))
                                .px(px(5.))
                                .py(px(1.))
                                .rounded(px(3.))
                                .bg(border.opacity(0.45))
                                .text_size(px(10.))
                                .text_color(muted)
                                .hover(move |s| s.text_color(fg))
                                .child(if badge_text == "EDIT" { "diff" } else { "details" })
                                .child(
                                    Icon::new(if detail_open { IconName::ChevronDown } else { IconName::ChevronRight })
                                        .with_size(px(9.5))
                                        .text_color(muted),
                                )
                                .on_click(cx.listener(move |this, _e, _w, cx| {
                                    let key = (item_ix, call_ix);
                                    if !this.agent.open_tool_details.remove(&key) {
                                        this.agent.open_tool_details.insert(key);
                                    }
                                    cx.notify();
                                })),
                        )
                    })
                    .when(sub_count > 0, |el| {
                        el.child(
                            h_flex()
                                .flex_shrink_0()
                                .items_center()
                                .gap(px(2.))
                                .text_size(px(tokens::UI_XS))
                                .text_color(muted)
                                .child(format!("{sub_count} step{}", if sub_count == 1 { "" } else { "s" }))
                                .child(
                                    Icon::new(if sub_open { IconName::ChevronDown } else { IconName::ChevronRight })
                                        .with_size(px(10.))
                                        .text_color(muted),
                                ),
                        )
                    })
                    .child(match call.status {
                        ToolStatus::Running => div()
                            .w(px(7.))
                            .h(px(7.))
                            .rounded_full()
                            .bg(primary)
                            .into_any_element(),
                        ToolStatus::Done => Icon::new(IconName::Check)
                            .with_size(px(11.))
                            .text_color(green.opacity(0.85))
                            .into_any_element(),
                        ToolStatus::Failed => Icon::new(IconName::CircleX)
                            .with_size(px(12.))
                            .text_color(red)
                            .into_any_element(),
                    })
                    .when_some(toggle_id, |el, id| {
                        el.on_click(cx.listener(move |this, _e, _w, cx| {
                            if !this.agent.open_subagents.remove(&id) {
                                this.agent.open_subagents.insert(id.clone());
                            }
                            cx.notify();
                        }))
                    })
                    .when(sub_count == 0, |el| {
                        if let Some(rel_path) = target {
                            el.on_click(cx.listener(move |this, _e, window, cx| {
                                let p = Path::new(&rel_path);
                                let full = if p.is_absolute() { p.to_path_buf() } else { this.root.join(p) };
                                if full.exists() {
                                    this.open_path(full, window, cx);
                                }
                            }))
                        } else if is_multiline_detail {
                            el.on_click(cx.listener(move |this, _e, _w, cx| {
                                let key = (item_ix, call_ix);
                                if !this.agent.open_tool_details.remove(&key) {
                                    this.agent.open_tool_details.insert(key);
                                }
                                cx.notify();
                            }))
                        } else {
                            el
                        }
                    }),
            )
            .when_some(detail_str.filter(|_| detail_open), |el, detail| {
                el.child(
                    v_flex()
                        .w_full()
                        .px(px(tokens::SPACE_3))
                        .py(px(tokens::SPACE_2))
                        .rounded(px(tokens::RADIUS_SM))
                        .bg(card_bg.opacity(0.65))
                        .border_1()
                        .border_color(border)
                        .font_family(mono_font)
                        .text_size(px(11.))
                        .line_height(px(15.5))
                        .gap(px(1.))
                        .children(detail.lines().map(|line| {
                            let color = if line.starts_with("+ ") {
                                green
                            } else if line.starts_with("- ") {
                                red
                            } else {
                                muted
                            };
                            div().w_full().truncate().text_color(color).child(line.to_string())
                        })),
                )
            })
            .into_any_element()
    }

    /// "Changed 3 files": what one turn did, each file openable as a diff,
    /// undoable on its own or all together.
    pub(crate) fn render_changes_card(
        &self,
        item_ix: usize,
        files: &[TurnFile],
        collapsed: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let border = cx.theme().border;
        let card_bg = cx.theme().muted;
        let list_hover = cx.theme().list_hover;
        let green = cx.theme().green;
        let red = cx.theme().red;
        let warning = cx.theme().warning;
        let running = self.agent.running_pid.is_some();
        let live: Vec<&TurnFile> = files.iter().filter(|f| !f.reverted).collect();
        let all_reverted = live.is_empty();
        let (added, removed) = live.iter().fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
        let title = if all_reverted {
            format!("Undid changes to {} file{}", files.len(), if files.len() == 1 { "" } else { "s" })
        } else {
            format!("Changed {} file{}", live.len(), if live.len() == 1 { "" } else { "s" })
        };
        let header = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap(px(tokens::SPACE_2))
            .child(
                h_flex()
                    .id(("agent-changes-toggle", item_ix))
                    .flex_1()
                    .min_w(px(0.))
                    .items_center()
                    .gap(px(tokens::SPACE_2))
                    .cursor_pointer()
                    .child(
                        Icon::new(if all_reverted { IconName::Undo2 } else { IconName::FileDiff })
                            .with_size(px(13.))
                            .text_color(if all_reverted { muted } else { primary }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(tokens::UI_SM))
                            .font_medium()
                            .text_color(if all_reverted { muted } else { fg })
                            .child(title),
                    )
                    .when(!all_reverted && added + removed > 0, |el| {
                        el.child(crate::diff_view::stat_chip(added, removed, cx))
                    })
                    .child(
                        Icon::new(if collapsed { IconName::ChevronRight } else { IconName::ChevronDown })
                            .with_size(px(11.))
                            .text_color(muted),
                    )
                    .on_click(cx.listener(move |this, _e, _w, cx| {
                        if let Some(AgentItem::Changes { collapsed, .. }) = this.agent.items.get_mut(item_ix) {
                            *collapsed = !*collapsed;
                            cx.notify();
                        }
                    })),
            )
            .when(!running, |el| {
                el.child(if all_reverted {
                    chip(("agent-changes-redo", item_ix), "Redo", primary, list_hover)
                        .on_click(cx.listener(move |this, _e, _w, cx| this.agent_redo_changes(item_ix, None, cx)))
                } else {
                    chip(("agent-changes-undo", item_ix), "Undo", muted, list_hover)
                        .hover(move |s| s.text_color(red))
                        .on_click(cx.listener(move |this, _e, _w, cx| this.agent_undo_changes(item_ix, None, cx)))
                })
            });

        v_flex()
            .w_full()
            .p(px(tokens::SPACE_3))
            .gap(px(tokens::SPACE_1))
            .rounded(px(tokens::RADIUS_CARD))
            .bg(card_bg)
            .border_1()
            .border_color(border)
            .child(header)
            .when(!collapsed, |el| {
                el.children(files.iter().enumerate().map(|(file_ix, file)| {
                    let (name, dir) = split_path(&file.path);
                    let letter_color = match file.change {
                        FileChange::Added => green,
                        FileChange::Deleted => red,
                        FileChange::Modified => warning,
                    };
                    let reverted = file.reverted;
                    h_flex()
                        .id(("agent-change-row", item_ix * 512 + file_ix))
                        .w_full()
                        .h(px(26.))
                        .px(px(tokens::SPACE_2))
                        .items_center()
                        .gap(px(tokens::SPACE_2))
                        .rounded(px(tokens::RADIUS_SM))
                        .cursor_pointer()
                        .hover(move |s| s.bg(list_hover))
                        .child(
                            div()
                                .w(px(10.))
                                .flex_shrink_0()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_size(px(tokens::UI_XS))
                                .text_color(if reverted { muted } else { letter_color })
                                .child(file.change.letter()),
                        )
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w(px(0.))
                                .items_baseline()
                                .gap(px(tokens::SPACE_2))
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(12.))
                                        .text_color(if reverted { muted } else { fg })
                                        .when(reverted, |s| s.line_through())
                                        .child(name),
                                )
                                .child(
                                    div()
                                        .min_w(px(0.))
                                        .truncate()
                                        .text_size(px(tokens::UI_XS))
                                        .text_color(muted)
                                        .child(dir),
                                ),
                        )
                        .when(!reverted && file.added + file.removed > 0, |el| {
                            el.child(crate::diff_view::stat_chip(file.added, file.removed, cx))
                        })
                        .when(!running, |el| {
                            el.child(if reverted {
                                chip(("agent-change-redo", item_ix * 512 + file_ix), "Redo", primary, list_hover)
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                    .on_click(cx.listener(move |this, _e, _w, cx| {
                                        this.agent_redo_changes(item_ix, Some(file_ix), cx)
                                    }))
                            } else {
                                chip(("agent-change-undo", item_ix * 512 + file_ix), "Undo", muted, list_hover)
                                    .hover(move |s| s.text_color(red))
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                    .on_click(cx.listener(move |this, _e, _w, cx| {
                                        this.agent_undo_changes(item_ix, Some(file_ix), cx)
                                    }))
                            })
                        })
                        .on_click(cx.listener(move |this, _e, _w, cx| {
                            this.agent_open_turn_diff(item_ix, file_ix, cx);
                        }))
                }))
            })
            .into_any_element()
    }

    /// A failed turn: what went wrong, what to do, and Retry.
    pub(crate) fn render_error_card(
        &self,
        item_ix: usize,
        kind: ErrorKind,
        message: &str,
        prompt_ix: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let red = cx.theme().red;
        let primary = cx.theme().primary;
        let list_hover = cx.theme().list_hover;
        let running = self.agent.running_pid.is_some();
        let is_last = item_ix + 1 == self.agent.items.len();
        let (shown, cut) = crate::agent_ctx::clip_text(message.trim(), 700);
        let detail = format!("{shown}{}", if cut { "\u{2026}" } else { "" });
        let to_copy = message.to_string();
        v_flex()
            .w_full()
            .px(px(tokens::SPACE_4))
            .py(px(tokens::SPACE_3))
            .gap(px(tokens::SPACE_2))
            .rounded(px(tokens::RADIUS_CARD))
            .bg(red.opacity(0.07))
            .border_1()
            .border_color(red.opacity(0.35))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(tokens::SPACE_2))
                    .child(Icon::new(IconName::TriangleAlert).with_size(px(13.)).text_color(red))
                    .child(div().text_size(px(tokens::UI_SM)).font_medium().text_color(fg).child(kind.title())),
            )
            .child(
                div()
                    .w_full()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(px(tokens::UI_XS))
                    .line_height(px(16.))
                    .text_color(muted)
                    .child(detail),
            )
            .child(
                div()
                    .text_size(px(tokens::UI_XS))
                    .text_color(fg.opacity(0.85))
                    .child(kind.hint(self.agent.harness)),
            )
            .child(
                h_flex()
                    .gap(px(tokens::SPACE_2))
                    .when(!running && is_last && prompt_ix.is_some(), |el| {
                        let user_ix = prompt_ix.unwrap_or_default();
                        el.child(
                            chip(("agent-error-retry", item_ix), "Retry", primary, list_hover)
                                .border_1()
                                .border_color(primary.opacity(0.5))
                                .on_click(cx.listener(move |this, _e, _w, cx| this.agent_retry(user_ix, true, cx))),
                        )
                    })
                    .when(!running, |el| {
                        el.child(
                            chip(("agent-error-model", item_ix), "Switch model", muted, list_hover)
                                .on_click(cx.listener(|this, _e, window, cx| this.open_model_picker(window, cx))),
                        )
                    })
                    .child(chip(("agent-error-copy", item_ix), "Copy error", muted, list_hover).on_click(
                        move |_e, _w, cx| cx.write_to_clipboard(ClipboardItem::new_string(to_copy.clone())),
                    )),
            )
            .into_any_element()
    }

    /// Edit / Rewind under a sent message (shown on hover).
    pub(crate) fn render_user_message_actions(&self, item_ix: usize, group: SharedString, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let list_hover = cx.theme().list_hover;
        let fg = cx.theme().foreground;
        let can_rewind = self.agent_turn_checkpoint(item_ix).is_some();
        h_flex()
            .w_full()
            .justify_end()
            .gap(px(tokens::SPACE_1))
            .invisible()
            .group_hover(group, |s| s.visible())
            .child(
                chip(("agent-msg-edit", item_ix), "Edit", muted, list_hover)
                    .hover(move |s| s.text_color(fg))
                    .tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(
                            "Change this message and send it again (files stay as they are)",
                        )
                        .build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _e, window, cx| {
                        this.agent_edit_message(item_ix, false, window, cx);
                    })),
            )
            .when(can_rewind, |el| {
                el.child(
                    chip(("agent-msg-rewind", item_ix), "Rewind", muted, list_hover)
                        .hover(move |s| s.text_color(fg))
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(
                                "Restore the files to before this message, and edit it",
                            )
                            .build(window, cx)
                        })
                        .on_click(cx.listener(move |this, _e, window, cx| {
                            this.agent_edit_message(item_ix, true, window, cx);
                        })),
                )
            })
            .into_any_element()
    }

    /// The agent's task list, pinned above the composer while it has one.
    pub(crate) fn render_agent_tasks(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.agent.tasks.is_empty() {
            return None;
        }
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let border = cx.theme().border;
        let card_bg = cx.theme().muted;
        let running = self.agent.running_pid.is_some();
        let total = self.agent.tasks.len();
        let done = self.agent.tasks.iter().filter(|t| t.done).count();
        let collapsed = self.agent.tasks_collapsed;
        let current = self.agent.tasks.iter().find_map(|t| t.active.clone());
        let progress = done as f32 / total.max(1) as f32;
        Some(
            v_flex()
                .min_w(px(0.))
                .flex_shrink_0()
                .mx(px(tokens::SPACE_4))
                .mb(px(tokens::SPACE_2))
                .p(px(tokens::SPACE_3))
                .gap(px(tokens::SPACE_1))
                .rounded(px(tokens::RADIUS_CARD))
                .bg(card_bg)
                .border_1()
                .border_color(border)
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap(px(tokens::SPACE_2))
                        .child(
                            h_flex()
                                .id("agent-tasks-toggle")
                                .flex_1()
                                .min_w(px(0.))
                                .items_center()
                                .gap(px(tokens::SPACE_2))
                                .cursor_pointer()
                                .child(Icon::new(IconName::ListTodo).with_size(px(12.)).text_color(primary))
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(tokens::UI_SM))
                                        .font_medium()
                                        .text_color(fg)
                                        .child(format!("Tasks {done}/{total}")),
                                )
                                .when_some(current.filter(|_| collapsed), |el, now| {
                                    el.child(
                                        div()
                                            .min_w(px(0.))
                                            .truncate()
                                            .text_size(px(tokens::UI_XS))
                                            .text_color(muted)
                                            .child(now),
                                    )
                                })
                                .child(
                                    Icon::new(if collapsed { IconName::ChevronRight } else { IconName::ChevronDown })
                                        .with_size(px(11.))
                                        .text_color(muted),
                                )
                                .on_click(cx.listener(|this, _e, _w, cx| {
                                    this.agent.tasks_collapsed = !this.agent.tasks_collapsed;
                                    cx.notify();
                                })),
                        )
                        .when(!running, |el| {
                            el.child(
                                div()
                                    .id("agent-tasks-clear")
                                    .flex_shrink_0()
                                    .cursor_pointer()
                                    .text_color(muted)
                                    .hover(move |s| s.text_color(fg))
                                    .child(Icon::new(IconName::X).with_size(px(11.)))
                                    .on_click(cx.listener(|this, _e, _w, cx| {
                                        this.agent.tasks.clear();
                                        this.save_agent_conversation();
                                        cx.notify();
                                    })),
                            )
                        }),
                )
                .child(
                    div()
                        .w_full()
                        .h(px(2.))
                        .rounded(px(1.))
                        .bg(border)
                        .child(div().h_full().rounded(px(1.)).bg(primary).w(relative(progress))),
                )
                .when(!collapsed, |el| {
                    el.child(
                        v_flex()
                            .id("agent-tasks-list")
                            .w_full()
                            .max_h(px(156.))
                            .overflow_y_scroll()
                            .pt(px(2.))
                            .children(self.agent.tasks.iter().map(|task| {
                                let active = task.active.is_some() && !task.done;
                                h_flex()
                                    .w_full()
                                    .min_h(px(22.))
                                    .items_center()
                                    .gap(px(tokens::SPACE_2))
                                    .child(
                                        div()
                                            .size(px(13.))
                                            .flex_shrink_0()
                                            .rounded(px(tokens::RADIUS_PILL))
                                            .border_1()
                                            .border_color(if task.done || active { primary } else { muted.opacity(0.6) })
                                            .when(task.done, |c| c.bg(primary.opacity(0.15)))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .when(task.done, |c| {
                                                c.child(Icon::new(IconName::Check).with_size(px(8.5)).text_color(primary))
                                            })
                                            .when(active, |c| c.child(div().size(px(5.)).rounded(px(3.)).bg(primary))),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .truncate()
                                            .text_size(px(12.))
                                            .text_color(if task.done { muted } else { fg })
                                            .when(task.done, |s| s.line_through())
                                            .when(active, |s| s.font_medium())
                                            .child(match (&task.active, task.done) {
                                                (Some(now), false) => now.clone(),
                                                _ => task.label.clone(),
                                            }),
                                    )
                                    .into_any_element()
                            })),
                    )
                })
                .into_any_element(),
        )
    }

    /// How full the model's context is: `38%` (or `76k` when the window is
    /// unknown), amber past 60%, red past 85%.
    pub(crate) fn render_context_meter(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let used = self.agent.usage.context.filter(|c| *c > 0)?;
        let (model_id, _) = self.agent.active_model();
        let window = self
            .agent
            .usage
            .window
            .or_else(|| default_context_window(self.agent.harness, model_id));
        let muted = cx.theme().muted_foreground;
        let (label, color, tip) = match window {
            Some(window) => {
                let pct = ((used as f64 / window as f64) * 100.0).round().min(100.0) as u32;
                let color = if pct >= 85 {
                    cx.theme().red
                } else if pct >= 60 {
                    cx.theme().warning
                } else {
                    muted
                };
                (
                    format!("{pct}%"),
                    color,
                    format!(
                        "Context: {} of {} tokens used. Type /compact to summarize and free some up.",
                        crate::agent_ctx::format_tokens(used),
                        crate::agent_ctx::format_tokens(window)
                    ),
                )
            }
            None => (
                crate::agent_ctx::format_tokens(used),
                muted,
                format!(
                    "Context: {} tokens used. Type /compact to start fresh with a summary.",
                    crate::agent_ctx::format_tokens(used)
                ),
            ),
        };
        let fill = window.map(|w| (used as f32 / w as f32).min(1.0)).unwrap_or(0.0);
        Some(
            h_flex()
                .id("agent-context-meter")
                .flex_shrink_0()
                .items_center()
                .gap(px(4.))
                .px(px(4.))
                .text_size(px(tokens::UI_XS))
                .text_color(color)
                .child(
                    div()
                        .w(px(18.))
                        .h(px(4.))
                        .rounded(px(2.))
                        .bg(muted.opacity(0.25))
                        .child(div().h_full().rounded(px(2.)).bg(color).w(relative(fill))),
                )
                .child(label)
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                })
                .into_any_element(),
        )
    }
}

fn turn_changed_file(file: &TurnFile) -> ChangedFile {
    ChangedFile {
        path: file.path.clone(),
        orig_path: None,
        kind: match file.change {
            FileChange::Added => ChangeKind::Added,
            FileChange::Deleted => ChangeKind::Deleted,
            FileChange::Modified => ChangeKind::Modified,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn splits_paths_into_name_and_folder() {
        assert_eq!(split_path("src/app/main.rs"), ("main.rs".to_string(), "src/app".to_string()));
        assert_eq!(split_path("README.md"), ("README.md".to_string(), String::new()));
    }

    #[core::prelude::v1::test]
    fn knows_claude_and_gemini_context_windows() {
        assert_eq!(default_context_window(AgentHarness::ClaudeCode, "sonnet"), Some(200_000));
        assert_eq!(default_context_window(AgentHarness::ClaudeCode, "claude-sonnet-4-5[1m]"), Some(1_000_000));
        assert_eq!(default_context_window(AgentHarness::Codex, "gpt-5"), None);
    }

    #[core::prelude::v1::test]
    fn classifies_errors_for_the_card() {
        assert_eq!(ErrorKind::classify("API Error: 429 rate_limit_error"), ErrorKind::RateLimit);
        assert_eq!(ErrorKind::classify("Invalid API key · Please run /login"), ErrorKind::Auth);
        assert_eq!(ErrorKind::classify("Overloaded"), ErrorKind::Overloaded);
        assert_eq!(ErrorKind::classify("connect ECONNREFUSED"), ErrorKind::Network);
        assert_eq!(ErrorKind::classify("something odd"), ErrorKind::Other);
    }
}
