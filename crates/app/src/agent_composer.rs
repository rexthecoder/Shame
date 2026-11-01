//! The agent composer: a mode switch above the input, context chips (active
//! file, problems, attached files and selections), a multi-line input, and
//! searchable tool / model / file pickers that open inline above it.

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    h_flex,
    input::{InlineToken, Input, Textarea},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::agent_ctx::{
    AGENT_MODES, Attachment, ContextKind, ModeId, SlashCommand, active_mention, add_attachment,
    classify_path, history_step,
};
use crate::rails::agent::{AgentHarness, ModelOption, filter_models};
use crate::{DartIde, tokens};

/// How many files the attach picker lists at once.
const MAX_ATTACH_ROWS: usize = 12;

/// Workspace-relative paths matching `query`: filename matches first, then
/// path matches. With an empty query, the open tabs (already relative) lead.
pub(crate) fn attach_matches(
    all: &[String],
    open_tabs: &[String],
    query: &str,
    limit: usize,
) -> Vec<String> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        let mut out: Vec<String> = open_tabs.to_vec();
        for path in all {
            if out.len() >= limit {
                break;
            }
            if !out.contains(path) {
                out.push(path.clone());
            }
        }
        out.truncate(limit);
        return out;
    }
    let mut name_hits = Vec::new();
    let mut path_hits = Vec::new();
    for path in all {
        let lower = path.to_lowercase();
        let name = lower.rsplit('/').next().unwrap_or(&lower);
        if name.contains(&q) {
            name_hits.push(path.clone());
        } else if lower.contains(&q) {
            path_hits.push(path.clone());
        }
    }
    name_hits.extend(path_hits);
    name_hits.truncate(limit);
    name_hits
}

fn pasted_image_extension(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Webp => "webp",
        ImageFormat::Gif => "gif",
        ImageFormat::Svg => "svg",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tiff",
        ImageFormat::Ico => "ico",
        ImageFormat::Pnm => "pnm",
    }
}

/// Where pasted images live: `~/.shame/attachments`.
fn attachments_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".shame").join("attachments"))
}

/// Write pasted image bytes to a new file and return its path.
fn save_pasted_image(bytes: &[u8], extension: &str) -> Option<PathBuf> {
    let dir = attachments_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("pasted-{stamp}.{extension}"));
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

/// The small second line on an attachment tile: where the file lives, or which
/// lines were selected.
pub(crate) fn attachment_sub(attachment: &Attachment) -> String {
    match attachment {
        Attachment::File { rel } => {
            let dir = Path::new(rel)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            if dir.is_empty() {
                String::new()
            } else if Path::new(rel).is_absolute() {
                // Outside the project: the last two folders are enough to place it.
                let parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
                format!("\u{2026}/{}", parts[parts.len().saturating_sub(2)..].join("/"))
            } else {
                dir
            }
        }
        Attachment::Selection { start_line, end_line, .. } if start_line == end_line => {
            format!("line {start_line}")
        }
        Attachment::Selection { start_line, end_line, .. } => {
            format!("lines {start_line}\u{2013}{end_line}")
        }
        Attachment::Image { .. } | Attachment::Folder { .. } | Attachment::Context(_) => String::new(),
        Attachment::Symbol { rel, line, .. } => format!("{rel}:{line}"),
    }
}

/// A small text glyph for live-context mentions (no file icon applies).
fn context_glyph(name: &str, color: Hsla) -> Div {
    let glyph = match name {
        "problems" => "\u{26a0}",
        "diff" => "\u{b1}",
        "terminal" => ">_",
        _ => "\u{2022}",
    };
    div()
        .flex_shrink_0()
        .size(px(16.))
        .rounded(px(4.))
        .bg(color.opacity(0.15))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(9.5))
        .font_semibold()
        .text_color(color)
        .child(glyph)
}

/// One row of the mention picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Candidate {
    Context(ContextKind),
    File(String),
    Folder(String),
    Symbol { name: String, rel: String, line: usize },
}

impl Candidate {
    pub(crate) fn into_attachment(self) -> Attachment {
        match self {
            Candidate::Context(kind) => Attachment::Context(kind),
            Candidate::File(rel) => Attachment::File { rel },
            Candidate::Folder(rel) => Attachment::Folder { rel },
            Candidate::Symbol { name, rel, line } => Attachment::Symbol { name, rel, line },
        }
    }
}

/// Folders (workspace-relative, no trailing slash) whose own name contains
/// `query`, shallowest first.
fn matching_folders(all: &[String], query: &str, limit: usize) -> Vec<String> {
    let q = query.to_lowercase();
    let mut dirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for path in all {
        let mut parts: Vec<&str> = path.split('/').collect();
        parts.pop(); // the file name
        for end in 1..=parts.len() {
            dirs.insert(parts[..end].join("/"));
        }
    }
    let mut hits: Vec<String> = dirs
        .into_iter()
        .filter(|d| d.rsplit('/').next().unwrap_or(d).to_lowercase().contains(&q))
        .collect();
    hits.sort_by_key(|d| (d.matches('/').count(), d.clone()));
    hits.truncate(limit);
    hits
}

/// Everything `@query` can offer, best first: live IDE context, files, folders,
/// then symbols. With no query it is the context kinds plus the open files.
pub(crate) fn mention_candidates(
    all_files: &[String],
    open_tabs: &[String],
    symbols: &[(String, String, usize)],
    query: &str,
    limit: usize,
) -> Vec<Candidate> {
    let q = query.trim().to_lowercase();
    let mut out: Vec<Candidate> = ContextKind::ALL
        .into_iter()
        .filter(|k| q.is_empty() || k.name().contains(&q))
        .map(Candidate::Context)
        .collect();
    let file_budget = if q.is_empty() { limit } else { 6 };
    out.extend(
        attach_matches(all_files, open_tabs, query, file_budget)
            .into_iter()
            .map(Candidate::File),
    );
    if !q.is_empty() {
        out.extend(matching_folders(all_files, &q, 3).into_iter().map(Candidate::Folder));
        if q.chars().count() >= 2 {
            let mut hits: Vec<&(String, String, usize)> = symbols
                .iter()
                .filter(|(name, _, _)| name.to_lowercase().contains(&q))
                .collect();
            hits.sort_by_key(|(name, _, _)| !name.to_lowercase().starts_with(&q));
            out.extend(hits.into_iter().take(4).map(|(name, rel, line)| Candidate::Symbol {
                name: name.clone(),
                rel: rel.clone(),
                line: *line,
            }));
        }
    }
    out.truncate(limit);
    out
}

impl DartIde {
    /// Start a fresh conversation (the `+` button and `/clear`).
    pub(crate) fn agent_new_thread(&mut self, cx: &mut Context<Self>) {
        self.stop_agent_run(cx);
        self.agent.tasks.clear();
        self.agent.items.clear();
        self.agent.ai_touched_paths.clear();
        self.agent.attachments.clear();
        self.agent.session_id = None;
        self.agent.question_inputs.clear();
        cx.notify();
    }

    pub(crate) fn run_slash_command(
        &mut self,
        command: SlashCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            SlashCommand::NewThread => self.agent_new_thread(cx),
            SlashCommand::SetMode(mode) => self.agent.mode_ix = mode.index(),
            SlashCommand::PickModel => self.open_model_picker(window, cx),
            SlashCommand::AddContext => self.open_attach_picker(window, cx),
        }
        cx.notify();
    }

    fn rel_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string()
    }

    /// What the file picker is filtering by: the `@query` being typed in the
    /// message, or the picker's own search box when opened with the + button.
    fn attach_query(&self, cx: &App) -> String {
        match &self.agent.mention_range {
            Some(range) => {
                let text = self.agent.input.read(cx).value();
                text.get(range.clone())
                    .map(|q| q.trim_start_matches('@').to_string())
                    .unwrap_or_default()
            }
            None => self.agent.attach_search.read(cx).value().to_string(),
        }
    }

    fn attach_candidates(&self, cx: &App) -> Vec<Candidate> {
        let query = self.attach_query(cx);
        let all: Vec<String> = self.all_files.iter().map(|p| self.rel_path(p)).collect();
        let open: Vec<String> = self
            .tabs
            .iter()
            .filter_map(|t| t.path.as_deref())
            .map(|p| self.rel_path(p))
            .collect();
        let symbols: Vec<(String, String, usize)> = self
            .workspace_symbols
            .iter()
            .map(|sym| (sym.name.clone(), self.rel_path(&sym.path), sym.line as usize + 1))
            .collect();
        mention_candidates(&all, &open, &symbols, &query, MAX_ATTACH_ROWS)
    }

    pub(crate) fn open_attach_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.agent.mention_range = None;
        self.agent.attach_open = true;
        self.ensure_symbol_index(cx);
        self.agent.tool_picker_open = false;
        self.agent.model_picker_open = false;
        self.agent.attach_selected = 0;
        self.agent.attach_search.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn close_agent_pickers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.agent.mention_range = None;
        self.agent.attach_open = false;
        self.agent.tool_picker_open = false;
        self.agent.model_picker_open = false;
        self.agent.input.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    /// Attach files or images that came from outside the picker (dropped,
    /// pasted or chosen in the system dialog).
    pub(crate) fn attach_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            if let Some(attachment) = classify_path(&path, &self.root) {
                add_attachment(&mut self.agent.attachments, attachment);
            }
        }
        self.agent_open = true;
        cx.notify();
    }

    /// The macOS file dialog, for files that aren't in the project.
    pub(crate) fn choose_files_from_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_agent_pickers(window, cx);
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |weak, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else { return };
            let _ = weak.update(cx, |this, cx| this.attach_paths(paths, cx));
        })
        .detach();
    }

    /// ⌘V with files or an image on the clipboard (a screenshot, a copied
    /// Finder file): attach it instead of pasting text. Returns whether it did.
    fn try_paste_attachment(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else { return false };
        let entries = item.entries();
        let paths: Vec<PathBuf> = entries
            .iter()
            .filter_map(|e| match e {
                ClipboardEntry::ExternalPaths(p) => Some(p.paths().to_vec()),
                _ => None,
            })
            .flatten()
            .collect();
        if !paths.is_empty() {
            self.attach_paths(paths, cx);
            return true;
        }
        // Copied text wins over an image preview of it (e.g. from a spreadsheet).
        if entries.iter().any(|e| matches!(e, ClipboardEntry::String(_))) {
            return false;
        }
        let Some(image) = entries.iter().find_map(|e| match e {
            ClipboardEntry::Image(image) => Some(image),
            _ => None,
        }) else {
            return false;
        };
        match save_pasted_image(&image.bytes, pasted_image_extension(image.format)) {
            Some(path) => {
                self.attach_paths(vec![path], cx);
                true
            }
            None => false,
        }
    }

    /// Track an `@query` being typed: open the live picker while there is one,
    /// close it when there isn't.
    pub(crate) fn update_mention_state(&mut self, cx: &mut Context<Self>) {
        let (text, cursor, tokens) = {
            let input = self.agent.input.read(cx);
            (
                input.value().to_string(),
                input.selected_range().end,
                input.tokens().iter().map(|t| t.range()).collect::<Vec<_>>(),
            )
        };
        match active_mention(&text, cursor, &tokens) {
            Some(range) => {
                let query = text[range.clone()].trim_start_matches('@').to_string();
                if query != self.agent.last_mention_query || self.agent.mention_range.is_none() {
                    self.agent.attach_selected = 0;
                }
                self.agent.last_mention_query = query;
                self.agent.mention_range = Some(range);
                self.ensure_symbol_index(cx);
                self.agent.attach_open = true;
                self.agent.tool_picker_open = false;
                self.agent.model_picker_open = false;
            }
            None => {
                if self.agent.mention_range.take().is_some() {
                    self.agent.attach_open = false;
                }
            }
        }
        cx.notify();
    }

    /// Put `attachment` into the message as an inline pill, replacing the
    /// `@query` being typed (or the cursor position when opened from the
    /// + button), followed by a space so typing carries on.
    pub(crate) fn insert_mention(
        &mut self,
        attachment: Attachment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The id carries the kind (`mention-3:folder`) so the pill can pick its icon.
        let kind = match &attachment {
            Attachment::File { .. } | Attachment::Image { .. } => "file",
            Attachment::Selection { .. } => "selection",
            Attachment::Folder { .. } => "folder",
            Attachment::Symbol { .. } => "symbol",
            Attachment::Context(kind) => kind.name(),
        };
        let id = format!("mention-{}:{kind}", self.agent.next_mention);
        self.agent.next_mention += 1;
        let label = attachment.label();
        let token = InlineToken::new(id.clone(), format!("@{label}")).with_label(label);
        self.agent.mentions.insert(id, attachment);
        let range = self.agent.mention_range.take();
        self.agent.attach_open = false;
        self.agent.input.update(cx, |state, cx| {
            let _ = match range {
                Some(range) => state.replace_range_with_token(range, token, window, cx),
                None => state.replace_with_token(token, window, cx),
            };
            state.insert(" ", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    /// Click on a pill: open that file (at the selected line, for a selection).
    pub(crate) fn open_mention(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(attachment) = self.agent.mentions.get(id).cloned() else { return };
        let (rel, line) = match attachment {
            Attachment::File { rel } => (rel, None),
            Attachment::Selection { rel, start_line, .. } => (rel, Some(start_line as u32 - 1)),
            Attachment::Symbol { rel, line, .. } => (rel, Some(line as u32 - 1)),
            Attachment::Image { .. } | Attachment::Folder { .. } | Attachment::Context(_) => return,
        };
        self.open_path(self.root.join(rel), window, cx);
        if let (Some(line), Some(editor)) = (line, self.active_editor().cloned()) {
            editor.update(cx, |editor, cx| editor.set_cursor_to_position(line, 0, cx));
        }
    }

    /// ⌘L: attach the editor's selection (or, with none, the whole file) to the
    /// chat and show the agent panel.
    pub(crate) fn attach_editor_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor().cloned() else { return };
        let (path, selection) = {
            let ed = editor.read(cx);
            (ed.buffer().path().map(Path::to_path_buf), ed.selection_for_context())
        };
        let Some(path) = path else { return };
        let rel = self.rel_path(&path);
        let attachment = match selection {
            Some((start_line, end_line, text)) => {
                Attachment::Selection { rel, start_line, end_line, text }
            }
            None => Attachment::File { rel },
        };
        self.agent_open = true;
        self.insert_mention(attachment, window, cx);
    }

    pub(crate) fn open_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.agent.model_picker_open = true;
        self.agent.tool_picker_open = false;
        self.agent.attach_open = false;
        self.agent.model_selected = self.agent.model_ix;
        self.agent.model_search.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    /// Matching model indices, plus the typed text when it should be offered as
    /// a custom model id (non-empty and not already an exact id).
    fn model_candidates(&self, cx: &App) -> (Vec<usize>, Option<String>) {
        let query = self.agent.model_search.read(cx).value().trim().to_string();
        let matches = filter_models(&self.agent.models, &query);
        let custom = (!query.is_empty()
            && !self.agent.models.iter().any(|m| m.id.eq_ignore_ascii_case(&query)))
        .then_some(query);
        (matches, custom)
    }

    fn choose_model(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix < self.agent.models.len() {
            self.agent.model_ix = ix;
        }
        self.close_agent_pickers(window, cx);
    }

    fn choose_custom_model(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.agent.models.push(ModelOption { id: id.clone(), label: id });
        self.agent.model_ix = self.agent.models.len() - 1;
        self.close_agent_pickers(window, cx);
    }

    /// Enter in an open picker: take the highlighted row.
    fn accept_picker_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.agent.attach_open {
            let candidates = self.attach_candidates(cx);
            if let Some(candidate) = candidates.get(self.agent.attach_selected).cloned() {
                self.insert_mention(candidate.into_attachment(), window, cx);
            }
        } else if self.agent.model_picker_open {
            let (matches, custom) = self.model_candidates(cx);
            let sel = self.agent.model_selected;
            if let Some(&ix) = matches.get(sel) {
                self.choose_model(ix, window, cx);
            } else if let Some(id) = custom {
                self.choose_custom_model(id, window, cx);
            }
        }
    }

    fn picker_row_count(&self, cx: &App) -> usize {
        if self.agent.attach_open {
            self.attach_candidates(cx).len()
        } else if self.agent.model_picker_open {
            let (matches, custom) = self.model_candidates(cx);
            matches.len() + usize::from(custom.is_some())
        } else {
            0
        }
    }

    /// Keyboard for the composer, run in the capture phase so Enter sends
    /// (Shift+Enter keeps its newline) before the textarea can swallow it.
    fn agent_composer_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let shift = event.keystroke.modifiers.shift;
        let any_picker = self.agent.attach_open
            || self.agent.model_picker_open
            || self.agent.tool_picker_open;

        if any_picker {
            let handled = match key {
                "escape" => {
                    self.close_agent_pickers(window, cx);
                    true
                }
                "down" | "up" => {
                    let count = self.picker_row_count(cx);
                    let sel = if self.agent.attach_open {
                        &mut self.agent.attach_selected
                    } else {
                        &mut self.agent.model_selected
                    };
                    *sel = if key == "down" {
                        (*sel + 1).min(count.saturating_sub(1))
                    } else {
                        sel.saturating_sub(1)
                    };
                    cx.notify();
                    true
                }
                "enter" if !self.agent.tool_picker_open => {
                    self.accept_picker_selection(window, cx);
                    true
                }
                "tab" if self.agent.mention_range.is_some() => {
                    self.accept_picker_selection(window, cx);
                    true
                }
                _ => false,
            };
            if handled {
                cx.stop_propagation();
            }
            return;
        }

        let primary_mod = event.keystroke.modifiers.platform || event.keystroke.modifiers.control;
        if key == "v" && primary_mod && self.try_paste_attachment(cx) {
            cx.stop_propagation();
            return;
        }

        match key {
            // Esc stops the agent mid-run (the indicator says so).
            "escape" if self.agent.running_pid.is_some() => {
                self.stop_agent_run(cx);
                cx.stop_propagation();
            }
            "enter" if !shift => {
                self.agent_send_message(window, cx);
                cx.stop_propagation();
            }
            "up" | "down" => {
                let empty = self.agent.input.read(cx).value().is_empty();
                if !(empty || self.agent.history_ix.is_some()) {
                    return;
                }
                let next = history_step(self.agent.history.len(), self.agent.history_ix, key == "up");
                self.agent.history_ix = next;
                let text = next.map(|ix| self.agent.history[ix].clone()).unwrap_or_default();
                self.agent
                    .input
                    .update(cx, |state, cx| state.set_value(text, window, cx));
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    pub(crate) fn render_agent_composer(&self, cx: &mut Context<Self>) -> Div {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let red = cx.theme().red;
        let border = cx.theme().border;
        let list_hover = cx.theme().list_hover;
        let harness = self.agent.harness;
        let (_, model_label) = self.agent.active_model();
        let model_label = model_label.to_string();
        let is_running = self.agent.running_pid.is_some();

        // --- context chips ---
        let active_name = self
            .active_editor()
            .and_then(|ed| ed.read(cx).buffer().path().map(Path::to_path_buf))
            .and_then(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string));
        let problem_count: usize = self
            .tabs
            .iter()
            .map(|t| t.editor.read(cx).diagnostics().len())
            .sum();

        // What the agent will see automatically: quiet, borderless toggles in the
        // header. Click to leave one out of the next message.
        let ghost = |id: ElementId| {
            h_flex()
                .id(id)
                .h(px(22.))
                .px(px(6.))
                .gap(px(4.))
                .items_center()
                .flex_shrink_0()
                .rounded(px(tokens::RADIUS_SM))
                .cursor_pointer()
                .text_size(px(tokens::UI_XS))
                .text_color(muted)
                .hover(move |s| s.bg(list_hover).text_color(fg))
        };
        let mut header_chips: Vec<AnyElement> = Vec::new();
        // The file the agent is told is open. Click to stop sharing it; click again to share.
        if let Some(name) = active_name {
            let on = self.agent.include_active_file;
            header_chips.push(
                ghost("agent-ctx-active".into())
                    .when(!on, |c| c.opacity(0.5))
                    .child(crate::rails::files::render_file_icon(&name))
                    .child(
                        div()
                            .max_w(px(110.))
                            .truncate()
                            .when(!on, |d| d.line_through())
                            .child(name),
                    )
                    .when(!on, |c| c.child("off"))
                    .on_click(cx.listener(|this, _e, _w, cx| {
                        this.agent.include_active_file = !this.agent.include_active_file;
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        if problem_count > 0 {
            let on = self.agent.include_problems;
            header_chips.push(
                ghost("agent-ctx-problems".into())
                    .when(!on, |c| c.opacity(0.45))
                    .child(div().text_color(cx.theme().warning).child("\u{26a0}"))
                    .child(div().when(!on, |d| d.line_through()).child(problem_count.to_string()))
                    .on_click(cx.listener(|this, _e, _w, cx| {
                        this.agent.include_problems = !this.agent.include_problems;
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }

        // What you attached from your Mac (dropped, pasted or chosen files and images),
        // at the top of the composer as plain inline items:
        // an icon and a name, no boxes. The remove button shows on hover. Hidden
        // when nothing is attached.
        let mut tiles: Vec<AnyElement> = Vec::new();
        for (ix, attachment) in self.agent.attachments.iter().enumerate() {
            let label = attachment.label();
            let group = format!("agent-attached-{ix}");
            let sub = match attachment {
                Attachment::Selection { .. } => attachment_sub(attachment),
                _ => String::new(),
            };
            tiles.push(
                h_flex()
                    .id(("agent-attached", ix))
                    .group(group.clone())
                    .max_w(px(190.))
                    .flex_shrink_0()
                    .h(px(26.))
                    .pl(px(5.))
                    .pr(px(7.))
                    .gap(px(6.))
                    .items_center()
                    .rounded(px(tokens::RADIUS_SM))
                    .bg(tokens::accent_dim())
                    .text_size(px(tokens::UI_SM))
                    .text_color(fg)
                    .child(match attachment {
                        Attachment::Image { path } => img(std::path::PathBuf::from(path))
                            .size(px(18.))
                            .rounded(px(4.))
                            .flex_shrink_0()
                            .object_fit(ObjectFit::Cover)
                            .into_any_element(),
                        _ => crate::rails::files::render_file_icon_sized(&label, 14.).into_any_element(),
                    })
                    .child(div().min_w(px(0.)).truncate().child(label))
                    .when(!sub.is_empty(), |row| {
                        row.child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(tokens::UI_XS))
                                .text_color(muted)
                                .child(sub),
                        )
                    })
                    .child(
                        div()
                            .id(("agent-attached-remove", ix))
                            .size(px(14.))
                            .flex_shrink_0()
                            .rounded(px(tokens::RADIUS_PILL))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_color(muted)
                            .invisible()
                            .group_hover(group, |s| s.visible())
                            .hover(|s| s.bg(rgba(0xffffff1f)).text_color(red))
                            .child(Icon::new(IconName::X).with_size(px(9.)))
                            .on_click(cx.listener(move |this, _e, _w, cx| {
                                if ix < this.agent.attachments.len() {
                                    this.agent.attachments.remove(ix);
                                }
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            );
        }
        // --- mode switch ---
        let mode_switch = h_flex()
            .flex_shrink_0()
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(tokens::RADIUS_SM + 2.))
            .bg(cx.theme().background)
            .border_1()
            .border_color(border)
            .children(AGENT_MODES.iter().enumerate().map(|(ix, m)| {
                let selected = ix == self.agent.mode_ix % AGENT_MODES.len();
                let danger = m.id == ModeId::Auto;
                h_flex()
                    .id(("agent-mode", ix))
                    .h(px(22.))
                    .px(px(10.))
                    .items_center()
                    .rounded(px(tokens::RADIUS_SM))
                    .cursor_pointer()
                    .text_size(px(tokens::UI_SM))
                    .when(selected, |s| {
                        s.bg(tokens::accent_dim())
                            .font_medium()
                            .text_color(if danger { cx.theme().warning } else { primary })
                    })
                    .when(!selected, |s| {
                        s.text_color(muted).hover(move |h| h.text_color(fg).bg(list_hover))
                    })
                    .child(m.label)
                    .on_click(cx.listener(move |this, _e, _w, cx| {
                        this.agent.mode_ix = ix;
                        cx.notify();
                    }))
            }));

        let trigger = |id: &'static str, open: bool| {
            h_flex()
                .id(id)
                .h(px(22.))
                .px(px(6.))
                .items_center()
                .gap(px(4.))
                .rounded(px(tokens::RADIUS_SM))
                .when(open, |s| s.bg(list_hover))
                .cursor_pointer()
                .hover(move |s| s.bg(list_hover))
                .text_size(px(tokens::UI_SM))
        };

        let footer = h_flex()
            .w_full()
            .px(px(tokens::SPACE_3))
            .pb(px(tokens::SPACE_3))
            .items_center()
            .justify_between()
            .gap(px(tokens::SPACE_2))
            .child(
                h_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .items_center()
                    .gap(px(2.))
                    // Attach files (also: type `@`, or ⌘L in the editor).
                    .child(
                        div()
                            .id("agent-attach-btn")
                            .size(px(24.))
                            .flex_shrink_0()
                            .rounded(px(tokens::RADIUS_PILL))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_color(muted)
                            .when(self.agent.attach_open, |s| s.bg(list_hover).text_color(primary))
                            .hover(move |s| s.bg(list_hover).text_color(fg))
                            .child(Icon::new(IconName::Plus).with_size(px(14.)))
                            .on_click(cx.listener(|this, _e, window, cx| {
                                if this.agent.attach_open {
                                    this.close_agent_pickers(window, cx);
                                } else {
                                    this.open_attach_picker(window, cx);
                                }
                            })),
                    )
                    .child(
                        trigger("agent-tool-select", self.agent.tool_picker_open)
                            .flex_shrink_0()
                            .child(
                                div()
                                    .text_color(if self.agent.tool_picker_open { primary } else { muted })
                                    .child(harness.short_label()),
                            )
                            .child(Icon::new(IconName::ChevronDown).with_size(px(9.5)).text_color(muted))
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.agent.tool_picker_open = !this.agent.tool_picker_open;
                                this.agent.model_picker_open = false;
                                this.agent.attach_open = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        trigger("agent-model-select", self.agent.model_picker_open)
                            .min_w(px(0.))
                            .child(
                                div()
                                    .max_w(px(170.))
                                    .truncate()
                                    .font_medium()
                                    .text_color(if self.agent.model_picker_open { primary } else { fg })
                                    .child(model_label),
                            )
                            .child(Icon::new(IconName::ChevronDown).with_size(px(9.5)).text_color(muted))
                            .on_click(cx.listener(|this, _e, window, cx| {
                                if this.agent.model_picker_open {
                                    this.close_agent_pickers(window, cx);
                                } else {
                                    this.open_model_picker(window, cx);
                                }
                            })),
                    ),
            )
            .child(if is_running {
                div()
                    .id("agent-stop-btn")
                    .size(px(26.))
                    .flex_shrink_0()
                    .rounded(px(tokens::RADIUS_PILL))
                    .bg(red.opacity(0.2))
                    .border_1()
                    .border_color(red)
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .child(Icon::new(IconName::X).with_size(px(12.)).text_color(red))
                    .on_click(cx.listener(|this, _e, _w, cx| this.stop_agent_run(cx)))
            } else {
                div()
                    .id("agent-send-btn")
                    .size(px(26.))
                    .flex_shrink_0()
                    .rounded(px(tokens::RADIUS_PILL))
                    .bg(primary)
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.9))
                    .child(
                        Icon::new(IconName::ArrowUp)
                            .with_size(px(12.5))
                            .text_color(cx.theme().primary_foreground),
                    )
                    .on_click(cx.listener(|this, _e, window, cx| this.agent_send_message(window, cx)))
            });

        v_flex()
            // Fill the panel but never push it wider: `w_auto` sized the
            // composer to its widest row, which spilled past narrow panels.
            .min_w(px(0.))
            .flex_shrink_0()
            .mx(px(tokens::SPACE_4))
            .mb(px(tokens::SPACE_4))
            .gap(px(tokens::SPACE_2))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.agent_composer_key(event, window, cx);
            }))
            .when_some(self.render_agent_picker(cx), |col, panel| col.child(panel))
            .when(!self.agent.queue.is_empty(), |col| {
                let count = self.agent.queue.len();
                let first = crate::agent_ctx::notification_body(&self.agent.queue[0].prompt, 48);
                col.child(
                    h_flex()
                        .w_full()
                        .px(px(tokens::SPACE_2))
                        .gap(px(tokens::SPACE_2))
                        .items_center()
                        .justify_between()
                        .text_size(px(tokens::UI_XS))
                        .text_color(muted)
                        .child(
                            div()
                                .min_w(px(0.))
                                .truncate()
                                .child(format!(
                                    "Queued{} \u{b7} {first}",
                                    if count > 1 { format!(" ({count})") } else { String::new() }
                                )),
                        )
                        .child(
                            div()
                                .id("agent-queue-clear")
                                .flex_shrink_0()
                                .cursor_pointer()
                                .hover(move |s| s.text_color(fg))
                                .child("Clear")
                                .on_click(cx.listener(|this, _e, _w, cx| {
                                    this.agent.queue.clear();
                                    cx.notify();
                                })),
                        ),
                )
            })
            .child(
                v_flex()
                    .w_full()
                    .min_w(px(0.))
                    .rounded(px(tokens::RADIUS_CARD))
                    .bg(cx.theme().input)
                    .border_1()
                    .border_color(border)
                    // Drop files or images from Finder anywhere on the composer.
                    .drag_over::<ExternalPaths>(move |style, _, _, _| {
                        style.border_color(primary).bg(tokens::accent_dim())
                    })
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                        this.attach_paths(paths.paths().to_vec(), cx);
                    }))
                    // Attached files, at the top: one quiet line, wrapping if needed.
                    .when(!tiles.is_empty(), |card| {
                        card.child(
                            h_flex()
                                .w_full()
                                .px(px(tokens::SPACE_3))
                                .pt(px(tokens::SPACE_3))
                                .pb(px(2.))
                                .flex_wrap()
                                .gap(px(6.))
                                .children(tiles),
                        )
                    })
                    // Header: the mode on the left, what the agent sees on the right.
                    .child(
                        h_flex()
                            .w_full()
                            .px(px(tokens::SPACE_3))
                            .pt(px(tokens::SPACE_2))
                            .items_center()
                            .justify_between()
                            .flex_wrap()
                            .gap(px(tokens::SPACE_2))
                            .child(mode_switch)
                            .child(
                                h_flex()
                                    .min_w(px(0.))
                                    .overflow_hidden()
                                    .gap(px(2.))
                                    .children(header_chips),
                            ),
                    )
                    .child(
                        div().w_full().px(px(tokens::SPACE_3)).pt(px(2.)).child(
                            Textarea::new(&self.agent.input)
                                .small()
                                .appearance(false)
                                .bordered(false)
                                // `@file` mentions render as pills inside the sentence.
                                .token(move |ctx, _window, cx| {
                                    let label = ctx.token().label().to_string();
                                    let name = label.split(':').next().unwrap_or(&label).to_string();
                                    let line = ctx.line_height().to_f64() as f32;
                                    // The pill is shorter than the line and centered in it, so
                                    // wrapped lines keep their spacing and nothing sits low.
                                    div()
                                        .h(px(line))
                                        .flex()
                                        .items_center()
                                        .child(
                                            h_flex()
                                                .h(px((line - 4.).max(16.)))
                                                .mx(px(1.))
                                                .pl(px(4.))
                                                .pr(px(6.))
                                                .gap(px(4.))
                                                .items_center()
                                                .rounded(px(tokens::RADIUS_SM))
                                                .bg(tokens::accent_dim())
                                                .when(ctx.is_selected(), |p| p.bg(cx.theme().selection))
                                                .text_size(px(tokens::UI_SM))
                                                .line_height(px((line - 4.).max(16.)))
                                                .text_color(cx.theme().foreground)
                                                .child(match ctx.token().id().rsplit(':').next().unwrap_or("") {
                                                    kind @ ("problems" | "diff" | "terminal") => {
                                                        context_glyph(kind, primary).into_any_element()
                                                    }
                                                    "folder" => Icon::new(IconName::Folder)
                                                        .with_size(px(12.))
                                                        .text_color(muted)
                                                        .into_any_element(),
                                                    "symbol" => crate::rails::files::render_file_icon_sized("symbol.dart", 12.)
                                                        .into_any_element(),
                                                    _ => crate::rails::files::render_file_icon_sized(&name, 12.)
                                                        .into_any_element(),
                                                })
                                                .child(label),
                                        )
                                })
                                .on_token_click({
                                    let view = cx.entity();
                                    move |event, window, cx| {
                                        let id = event.token().id().to_string();
                                        view.update(cx, |this, cx| this.open_mention(&id, window, cx));
                                    }
                                }),
                        ),
                    )
                    .child(footer),
            )
    }

    /// The tool, model or file picker, shown as a panel above the composer.
    fn render_agent_picker(&self, cx: &mut Context<Self>) -> Option<Div> {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let green = cx.theme().green;
        let border = cx.theme().border;
        let list_hover = cx.theme().list_hover;
        let mono = cx.theme().mono_font_family.clone();

        let panel = || {
            v_flex()
                .w_full()
                .max_h(px(280.))
                .p(px(tokens::SPACE_2))
                .gap(px(tokens::SPACE_1))
                .rounded(px(tokens::RADIUS_CARD))
                .bg(cx.theme().popover)
                .border_1()
                .border_color(border)
                .shadow_lg()
        };
        let search_box = |input: &Entity<gpui_kit::component::input::InputState>| {
            h_flex()
                .w_full()
                .h(px(28.))
                .px(px(tokens::SPACE_3))
                .items_center()
                .rounded(px(tokens::RADIUS_SM))
                .bg(cx.theme().background)
                .border_1()
                .border_color(border)
                .child(Input::new(input).appearance(false).small())
        };
        let row = |id: ElementId, selected: bool| {
            h_flex()
                .id(id)
                .w_full()
                .min_h(px(28.))
                .px(px(tokens::SPACE_3))
                .items_center()
                .justify_between()
                .gap(px(tokens::SPACE_2))
                .rounded(px(tokens::RADIUS_SM))
                .cursor_pointer()
                .when(selected, |s| s.bg(tokens::accent_dim()))
                .when(!selected, |s| s.hover(move |h| h.bg(list_hover)))
        };

        if self.agent.tool_picker_open {
            let current = self.agent.harness;
            return Some(
                panel()
                    .child(
                        div()
                            .px(px(tokens::SPACE_2))
                            .py(px(2.))
                            .text_size(px(tokens::UI_XS))
                            .font_medium()
                            .text_color(muted)
                            .child("CODING TOOL"),
                    )
                    .children(AgentHarness::ALL.iter().copied().enumerate().map(|(ix, tool)| {
                        let installed = tool.resolve_binary().is_some();
                        row(("agent-tool-opt", ix).into(), tool == current)
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(tokens::SPACE_2))
                                    .child(div().size(px(6.)).rounded(px(3.)).bg(if installed { green } else { muted }))
                                    .child(
                                        div()
                                            .text_size(px(tokens::UI_SM))
                                            .font_medium()
                                            .text_color(fg)
                                            .child(tool.label()),
                                    ),
                            )
                            .child(
                                div()
                                    .font_family(mono.clone())
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(if installed { green } else { muted })
                                    .child(if installed { "ready" } else { "not found" }),
                            )
                            .on_click(cx.listener(move |this, _e, window, cx| {
                                this.agent.set_harness(tool);
                                this.close_agent_pickers(window, cx);
                            }))
                    })),
            );
        }

        if self.agent.model_picker_open {
            let (matches, custom) = self.model_candidates(cx);
            let selected = self.agent.model_selected;
            let current = self.agent.model_ix;
            let match_count = matches.len();
            return Some(
                panel()
                    .child(search_box(&self.agent.model_search))
                    .child(
                        v_flex()
                            .id("agent-model-list")
                            .w_full()
                            .min_h(px(0.))
                            .overflow_y_scroll()
                            .gap(px(1.))
                            .children(matches.into_iter().enumerate().map(|(pos, ix)| {
                                let model = &self.agent.models[ix];
                                row(("agent-model-opt", ix).into(), pos == selected)
                                    .child(
                                        h_flex()
                                            .min_w(px(0.))
                                            .items_center()
                                            .gap(px(tokens::SPACE_2))
                                            .child(div().w(px(12.)).when(ix == current, |c| {
                                                c.child(Icon::new(IconName::Check).xsmall().text_color(primary))
                                            }))
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_size(px(tokens::UI_SM))
                                                    .text_color(fg)
                                                    .child(model.label.clone()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .max_w(px(150.))
                                            .truncate()
                                            .font_family(mono.clone())
                                            .text_size(px(tokens::UI_XS))
                                            .text_color(muted)
                                            .child(model.id.clone()),
                                    )
                                    .on_click(cx.listener(move |this, _e, window, cx| {
                                        this.choose_model(ix, window, cx);
                                    }))
                            }))
                            .when_some(custom, |list, id| {
                                let label = format!("Use custom model \u{201c}{id}\u{201d}");
                                list.child(
                                    row("agent-model-custom".into(), selected == match_count)
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(px(tokens::UI_SM))
                                                .text_color(fg)
                                                .child(label),
                                        )
                                        .on_click(cx.listener(move |this, _e, window, cx| {
                                            this.choose_custom_model(id.clone(), window, cx);
                                        })),
                                )
                            }),
                    ),
            );
        }

        if self.agent.attach_open {
            let candidates = self.attach_candidates(cx);
            let selected = self.agent.attach_selected;
            let empty = candidates.is_empty();
            let typing_mention = self.agent.mention_range.is_some();
            return Some(
                panel()
                    .when(!typing_mention, |p| p.child(search_box(&self.agent.attach_search)))
                    .when(!typing_mention, |p| {
                        p.child(
                        row("agent-attach-browse".into(), false)
                            .child(
                                div()
                                    .text_size(px(tokens::UI_SM))
                                    .text_color(primary)
                                    .child("Choose from your Mac\u{2026}"),
                            )
                            .child(
                                div()
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(muted)
                                    .child("files or images"),
                            )
                            .on_click(cx.listener(|this, _e, window, cx| {
                                this.choose_files_from_disk(window, cx);
                            })),
                        )
                    })
                    .child(
                        v_flex()
                            .id("agent-attach-list")
                            .w_full()
                            .min_h(px(0.))
                            .overflow_y_scroll()
                            .gap(px(1.))
                            .when(empty, |l| {
                                l.child(
                                    div()
                                        .px(px(tokens::SPACE_3))
                                        .py(px(6.))
                                        .text_size(px(tokens::UI_SM))
                                        .text_color(muted)
                                        .child("No matching files"),
                                )
                            })
                            .children(candidates.into_iter().enumerate().map(|(ix, candidate)| {
                                // (icon, label, muted right-hand text)
                                let (icon, label, sub): (AnyElement, String, String) = match &candidate {
                                    Candidate::Context(kind) => (
                                        context_glyph(kind.name(), primary).into_any_element(),
                                        kind.name().to_string(),
                                        kind.description().to_string(),
                                    ),
                                    Candidate::File(rel) => {
                                        let name = PathBuf::from(rel)
                                            .file_name()
                                            .and_then(|n| n.to_str())
                                            .unwrap_or(rel)
                                            .to_string();
                                        let dir = rel.strip_suffix(&name).unwrap_or("").trim_end_matches('/').to_string();
                                        (
                                            crate::rails::files::render_file_icon(&name).into_any_element(),
                                            name,
                                            dir,
                                        )
                                    }
                                    Candidate::Folder(rel) => (
                                        Icon::new(IconName::Folder).with_size(px(14.)).text_color(muted).into_any_element(),
                                        format!("{}/", rel.rsplit('/').next().unwrap_or(rel)),
                                        rel.clone(),
                                    ),
                                    Candidate::Symbol { name, rel, line } => (
                                        crate::rails::files::render_file_icon("symbol.dart").into_any_element(),
                                        name.clone(),
                                        format!("{rel}:{line}"),
                                    ),
                                };
                                row(("agent-attach-opt", ix).into(), ix == selected)
                                    .child(
                                        h_flex()
                                            .min_w(px(0.))
                                            .items_center()
                                            .gap(px(tokens::SPACE_2))
                                            .child(icon)
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_size(px(tokens::UI_SM))
                                                    .text_color(fg)
                                                    .child(label),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .max_w(px(180.))
                                            .truncate()
                                            .text_size(px(tokens::UI_XS))
                                            .text_color(muted)
                                            .child(sub),
                                    )
                                    .on_click(cx.listener(move |this, _e, window, cx| {
                                        this.insert_mention(candidate.clone().into_attachment(), window, cx);
                                    }))
                            })),
                    ),
            );
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{Candidate, attach_matches, attachment_sub, mention_candidates};
    use crate::agent_ctx::{Attachment, ContextKind};

    #[core::prelude::v1::test]
    fn tile_subtitles_place_the_file_or_selection() {
        let file = |rel: &str| Attachment::File { rel: rel.into() };
        assert_eq!(attachment_sub(&file("main.dart")), "");
        assert_eq!(attachment_sub(&file("lib/src/a.dart")), "lib/src");
        assert_eq!(attachment_sub(&file("/Users/me/Documents/notes/todo.txt")), "\u{2026}/Documents/notes");
        let sel = |a, b| Attachment::Selection { rel: "x.dart".into(), start_line: a, end_line: b, text: String::new() };
        assert_eq!(attachment_sub(&sel(7, 7)), "line 7");
        assert_eq!(attachment_sub(&sel(7, 12)), "lines 7\u{2013}12");
        assert_eq!(attachment_sub(&Attachment::Image { path: "/a/b.png".into() }), "");
    }

    fn files() -> Vec<String> {
        ["lib/main.dart", "lib/src/user_model.dart", "test/main_test.dart", "README.md"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    fn symbols() -> Vec<(String, String, usize)> {
        vec![
            ("UserModel".into(), "lib/src/user_model.dart".into(), 3),
            ("MainApp".into(), "lib/main.dart".into(), 7),
        ]
    }

    #[core::prelude::v1::test]
    fn empty_query_offers_live_context_then_open_files() {
        let tabs = vec!["README.md".to_string()];
        let c = mention_candidates(&files(), &tabs, &symbols(), "", 6);
        assert_eq!(&c[..3], &[
            Candidate::Context(ContextKind::Problems),
            Candidate::Context(ContextKind::GitDiff),
            Candidate::Context(ContextKind::Terminal),
        ]);
        assert_eq!(c[3], Candidate::File("README.md".into()), "open tabs lead the files");
        assert_eq!(c.len(), 6);
    }

    #[core::prelude::v1::test]
    fn typing_narrows_across_context_files_folders_and_symbols() {
        let c = mention_candidates(&files(), &[], &symbols(), "prob", 12);
        assert_eq!(c, vec![Candidate::Context(ContextKind::Problems)]);

        let c = mention_candidates(&files(), &[], &symbols(), "src", 12);
        assert!(c.contains(&Candidate::File("lib/src/user_model.dart".into())));
        assert!(c.contains(&Candidate::Folder("lib/src".into())), "{c:?}");

        let c = mention_candidates(&files(), &[], &symbols(), "user", 12);
        assert!(c.contains(&Candidate::Symbol { name: "UserModel".into(), rel: "lib/src/user_model.dart".into(), line: 3 }));
        // Files rank before symbols.
        let first_symbol = c.iter().position(|x| matches!(x, Candidate::Symbol { .. })).unwrap();
        let first_file = c.iter().position(|x| matches!(x, Candidate::File(_))).unwrap();
        assert!(first_file < first_symbol);

        // One character is too vague for symbols.
        assert!(!mention_candidates(&files(), &[], &symbols(), "u", 12)
            .iter()
            .any(|x| matches!(x, Candidate::Symbol { .. })));
    }

    #[core::prelude::v1::test]
    fn empty_query_lists_open_tabs_first_without_duplicates() {
        let tabs = vec!["README.md".to_string()];
        let hits = attach_matches(&files(), &tabs, "", 3);
        assert_eq!(hits, vec!["README.md", "lib/main.dart", "lib/src/user_model.dart"]);
    }

    #[core::prelude::v1::test]
    fn filename_matches_rank_before_path_matches() {
        let hits = attach_matches(&files(), &[], "main", 10);
        assert_eq!(hits, vec!["lib/main.dart", "test/main_test.dart"]);
        let by_dir = attach_matches(&files(), &[], "src", 10);
        assert_eq!(by_dir, vec!["lib/src/user_model.dart"]);
        assert!(attach_matches(&files(), &[], "zzz", 10).is_empty());
    }
}
