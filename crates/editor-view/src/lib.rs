//! Own GPUI editor element: cursor, selection, scroll, gutter, text drawing (M1).
//!
//! Built only on raw `gpui-kit` primitives (`gpui_kit::*`), never on
//! `gpui_kit::component`'s pre-built `Editor`/`Textarea` widgets — those
//! are read as a reference (see `NOTES.md` at the repo root), not a
//! dependency. `core-buffer` and `core-syntax` are the other two
//! dependencies, kept behind this crate's own API so the UI layer can
//! change without touching either of them.
//!
//! That boundary is also why rename has no input box *here*: a text
//! field is exactly the kind of pre-built widget this crate avoids, and
//! writing our own from scratch isn't worth it for one feature. Rename
//! starts in `EditorView` (F2/cmd+ctrl+R, computing the cursor position)
//! but the actual "type a new name" prompt is `app`'s job — see
//! `EditorEvent::RequestRename`.
//!
//! Known M1 scope cuts (not yet implemented, tracked for M2+):
//! - Only the primary selection gets a visual highlight; a second
//!   multi-cursor selection (if one were added) wouldn't show one.
//! - No double/triple-click word/line selection.
//! - Every line is rendered on every frame; no virtualization for huge
//!   files.

mod lsp_integration;

use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};

use core_buffer::{Buffer, Selection};
use core_lsp::lsp_types;
use core_syntax::{Highlight, HighlightKind, InputEdit, SyntaxTree, point_at};
use core_workspace::LineChange;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use lsp_integration::{
    CompletionEntry, CompletionKindBadge, LspState,
};

/// Second pass on this editor's look. The first pass piled on chrome —
/// a bordered gutter column with its own background tint, a left-edge
/// accent bar on the active line, an underline under diagnostic text —
/// and the result fought itself instead of reading as one surface. This
/// pass goes the other way: fewer elements, each one doing one clear job,
/// which is the actual throughline in the `cutoff-studio` reference this
/// crate is meant to be learning from ("a flat, quiet ... layout") rather
/// than any specific color or border it draws.
const GUTTER_WIDTH: f32 = 68.;
const LINE_FONT_SIZE: f32 = 12.5;
/// Numbers run one step smaller than the code they label, the way every
/// reference editor's gutter does.
const GUTTER_FONT_SIZE: f32 = 11.5;
/// Enough breathing room to read as an editor without the file feeling
/// sparse (the first pass tried 26px, which did).
const LINE_HEIGHT: f32 = 22.;
/// `tokens::LINE_ACTIVE` in the app crate — duplicated here because this
/// crate deliberately has no dependency on `app`.
const ACTIVE_LINE_BG: u32 = 0x15161a;
/// SF Mono over Menlo: still the system monospace macOS ships, but with
/// tighter, more even letterforms — the difference a long file's worth
/// of code actually shows, where a typeface pass alone wouldn't.
/// Fallback monospace family if theme is not initialized.
#[allow(dead_code)]
const FONT_FAMILY: &str = "SF Mono";

/// Emitted when this editor wants something handled above it that it has
/// no business doing itself: opening a different file (`editor-view` has
/// no idea tabs exist), collecting a new name for a rename, or showing a
/// picker for more than one code action (no text input or list widget
/// here — see the crate's module docs on why). `app` subscribes to all
/// three per tab.
#[derive(Debug, Clone)]
pub enum EditorEvent {
    OpenAt { path: PathBuf, line: u32, column: u32 },
    RequestRename { line: u32, column: u32 },
    ShowCodeActions { actions: Vec<lsp_types::CodeActionOrCommand> },
    Saved { path: PathBuf },
}

/// Maximum file size (256 KiB) for tree-sitter parsing and syntax queries.
/// Files above this threshold or non-code lockfiles open in fast plain-text mode.
const MAX_SYNTAX_BYTES: usize = 256 * 1024;

fn should_parse_syntax(buffer: &Buffer) -> bool {
    if buffer.len_bytes() > MAX_SYNTAX_BYTES {
        return false;
    }
    let Some(ext) = buffer.path().and_then(|p| p.extension()).and_then(|e| e.to_str()) else {
        return true;
    };
    !matches!(
        ext.to_ascii_lowercase().as_str(),
        "lock" | "txt" | "log" | "csv" | "tsv" | "svg" | "map"
    )
}

pub struct EditorView {
    buffer: Buffer,
    focus_handle: FocusHandle,
    scroll_handle: UniformListScrollHandle,
    syntax_tree: Option<SyntaxTree>,
    /// The text `syntax_tree` was last parsed/reparsed from, so `render`
    /// can diff against the buffer's current text to build a single
    /// [`InputEdit`] rather than reparsing from scratch every frame.
    synced_text: String,
    synced_revision: u64,
    indent_width: usize,
    line_changes: std::collections::HashMap<u32, LineChange>,
    /// `dart language-server` state for this buffer, if it's a `.dart`
    /// file. See `lsp_integration.rs`.
    lsp: Option<LspState>,
    /// `(line, column)` the mouse is currently hovering, for debouncing
    /// `textDocument/hover` requests. See `lsp_integration.rs`.
    hover_point: Option<(usize, usize)>,
    hover_sequence: u64,
    hover_result: Option<(usize, String)>,
    /// Active in-file search query (`⌘F`) and all case-insensitive byte ranges
    /// matching it across the buffer.
    search_query: String,
    search_matches: Vec<Range<usize>>,
    active_search_match: usize,
    /// Active autocomplete candidates and selected row index.
    completions: Vec<CompletionEntry>,
    completion_selected: usize,
    /// VS Code Flutter-style closing labels (`// WidgetName` on multi-line closing `),` / `);`).
    closing_labels: std::collections::HashMap<usize, String>,
}

impl EditorView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::from_buffer(Buffer::new(), cx)
    }

    pub fn from_buffer(buffer: Buffer, cx: &mut Context<Self>) -> Self {
        let parse_syntax = should_parse_syntax(&buffer);
        let synced_text = if parse_syntax {
            buffer.text()
        } else {
            String::new()
        };
        let syntax_tree = parse_syntax.then(|| SyntaxTree::parse(&synced_text));
        let closing_labels = if parse_syntax {
            Self::compute_closing_widget_labels(&synced_text)
        } else {
            std::collections::HashMap::new()
        };
        let synced_revision = buffer.revision();
        let indent_width = buffer.detect_indent_width();
        let line_changes = buffer
            .path()
            .map(core_workspace::line_changes)
            .unwrap_or_default();
        let mut this = Self {
            buffer,
            focus_handle: cx.focus_handle(),
            scroll_handle: UniformListScrollHandle::new(),
            syntax_tree,
            synced_text,
            synced_revision,
            indent_width,
            line_changes,
            lsp: None,
            hover_point: None,
            hover_sequence: 0,
            hover_result: None,
            search_query: String::new(),
            search_matches: Vec::new(),
            active_search_match: 0,
            completions: Vec::new(),
            completion_selected: 0,
            closing_labels,
        };
        this.maybe_start_lsp(cx);
        this
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// 1-indexed (line, column) of the primary cursor, for a status bar —
    /// e.g. "Ln 4, Col 1" the way every other editor shows it, even though
    /// everywhere else in this crate works in 0-indexed byte offsets.
    pub fn cursor_position(&self) -> (u32, u32) {
        let head = self.buffer.selections().primary().head;
        let line = self.buffer.byte_to_line(head);
        let column = head - self.buffer.line_to_byte(line);
        (line as u32 + 1, column as u32 + 1)
    }

    /// Replace the buffer with the contents of `path`, discarding undo
    /// history and resetting the cursor to the start of the file.
    pub fn open_file(&mut self, path: impl AsRef<Path>, cx: &mut Context<Self>) -> io::Result<()> {
        self.buffer = Buffer::open(path)?;
        let parse_syntax = should_parse_syntax(&self.buffer);
        if parse_syntax {
            let text = self.buffer.text();
            self.syntax_tree = Some(SyntaxTree::parse(&text));
            self.synced_text = text;
        } else {
            self.syntax_tree = None;
            self.synced_text.clear();
        }
        self.synced_revision = self.buffer.revision();
        self.indent_width = self.buffer.detect_indent_width();
        self.line_changes = self
            .buffer
            .path()
            .map(core_workspace::line_changes)
            .unwrap_or_default();
        self.lsp = None;
        self.maybe_start_lsp(cx);
        cx.notify();
        Ok(())
    }

    pub fn save(&mut self, cx: &mut Context<Self>) {
        if self.buffer.save().is_ok() {
            if let Some(path) = self.buffer.path().map(Path::to_path_buf) {
                cx.emit(EditorEvent::Saved { path });
            }
        }
        self.line_changes = self
            .buffer
            .path()
            .map(core_workspace::line_changes)
            .unwrap_or_default();
    }

    fn set_cursor(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.buffer
            .selections_mut()
            .set_primary(Selection::cursor(offset));
        cx.notify();
    }

    fn ensure_cursor_visible(&mut self) {
        let head = self.buffer.selections().primary().head;
        let line = self.buffer.byte_to_line(head);
        self.scroll_handle
            .scroll_to_item(line, ScrollStrategy::Nearest);
    }

    /// Move the cursor to a 0-indexed (line, column) — e.g. where a
    /// go-to-definition in a newly-opened file landed. `column` is
    /// treated as a byte offset within the line, the same approximation
    /// used everywhere else in this crate.
    pub fn set_cursor_to_position(&mut self, line: u32, column: u32, cx: &mut Context<Self>) {
        let offset = self.buffer.line_to_byte(line as usize) + column as usize;
        self.set_cursor(offset, cx);
        self.scroll_handle
            .scroll_to_item(line as usize, ScrollStrategy::Center);
    }

    /// Update the active in-file search query (`⌘F`), recomputing all
    /// case-insensitive matches and jumping to the nearest match at or after
    /// the current cursor.
    pub fn set_search_query(&mut self, query: &str, cx: &mut Context<Self>) {
        if self.search_query == query {
            return;
        }
        self.search_query = query.to_string();
        self.recompute_search_matches();
        if !self.search_matches.is_empty() {
            let head = self.buffer.selections().primary().start();
            self.active_search_match = self
                .search_matches
                .iter()
                .position(|r| r.start >= head)
                .unwrap_or(0);
            let target = self.search_matches[self.active_search_match].clone();
            self.buffer
                .selections_mut()
                .set_primary(Selection::cursor(target.start));
            let line = self.buffer.byte_to_line(target.start);
            self.scroll_handle
                .scroll_to_item(line, ScrollStrategy::Center);
        }
        cx.notify();
    }

    pub fn clear_search(&mut self, cx: &mut Context<Self>) {
        if !self.search_query.is_empty() || !self.search_matches.is_empty() {
            self.search_query.clear();
            self.search_matches.clear();
            self.active_search_match = 0;
            cx.notify();
        }
    }

    /// `(current_1_indexed_match, total_matches)` for the find bar badge.
    pub fn search_status(&self) -> (usize, usize) {
        if self.search_matches.is_empty() {
            (0, 0)
        } else {
            (
                (self.active_search_match + 1).min(self.search_matches.len()),
                self.search_matches.len(),
            )
        }
    }

    pub fn find_next(&mut self, cx: &mut Context<Self>) {
        if self.search_matches.is_empty() {
            return;
        }
        self.active_search_match = (self.active_search_match + 1) % self.search_matches.len();
        let target = self.search_matches[self.active_search_match].clone();
        self.buffer
            .selections_mut()
            .set_primary(Selection::cursor(target.start));
        let line = self.buffer.byte_to_line(target.start);
        self.scroll_handle
            .scroll_to_item(line, ScrollStrategy::Center);
        cx.notify();
    }

    pub fn find_prev(&mut self, cx: &mut Context<Self>) {
        if self.search_matches.is_empty() {
            return;
        }
        self.active_search_match = if self.active_search_match == 0 {
            self.search_matches.len() - 1
        } else {
            self.active_search_match - 1
        };
        let target = self.search_matches[self.active_search_match].clone();
        self.buffer
            .selections_mut()
            .set_primary(Selection::cursor(target.start));
        let line = self.buffer.byte_to_line(target.start);
        self.scroll_handle
            .scroll_to_item(line, ScrollStrategy::Center);
        cx.notify();
    }

    fn recompute_search_matches(&mut self) {
        self.search_matches.clear();
        if self.search_query.is_empty() {
            self.active_search_match = 0;
            return;
        }
        let text = self.buffer.text();
        let needle = self.search_query.to_lowercase();
        let needle_len = needle.len();
        if needle_len == 0 {
            return;
        }
        let hay = text.to_lowercase();
        let mut from = 0;
        while let Some(pos) = hay[from..].find(&needle) {
            let start = from + pos;
            let end = start + needle_len;
            if text.is_char_boundary(start) && text.is_char_boundary(end) {
                self.search_matches.push(start..end);
            }
            from = start + needle_len.max(1);
            if from >= hay.len() {
                break;
            }
        }
        if self.active_search_match >= self.search_matches.len() {
            self.active_search_match = 0;
        }
    }

    /// Select the word at `column` on `line_idx` (double-click behavior).
    fn select_word_on_line(&mut self, line_idx: usize, column: usize, cx: &mut Context<Self>) {
        let line_start = self.buffer.line_to_byte(line_idx);
        let line_text = self.buffer.line(line_idx);
        let display = line_text.trim_end_matches(['\n', '\r']);
        if let Some(range) = Self::word_range_at(display, column.min(display.len())) {
            self.buffer.selections_mut().set_primary(Selection::new(
                line_start + range.start,
                line_start + range.end,
            ));
            cx.notify();
        } else {
            self.set_cursor(line_start + column.min(display.len()), cx);
        }
    }

    /// Select the entire `line_idx` including its newline (triple-click behavior).
    fn select_line(&mut self, line_idx: usize, cx: &mut Context<Self>) {
        let start = self.buffer.line_to_byte(line_idx);
        let end = if line_idx + 1 < self.buffer.line_count() {
            self.buffer.line_to_byte(line_idx + 1)
        } else {
            self.buffer.len_bytes()
        };
        self.buffer
            .selections_mut()
            .set_primary(Selection::new(start, end));
        cx.notify();
    }

    /// Move the head to `offset`, keeping the current anchor fixed —
    /// i.e. extend (or shrink) the selection rather than collapsing it.
    fn extend_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let anchor = self.buffer.selections().primary().anchor;
        self.buffer
            .selections_mut()
            .set_primary(Selection::new(anchor, offset));
        cx.notify();
    }

    fn move_head_or_cursor(&mut self, offset: usize, extend: bool, cx: &mut Context<Self>) {
        if extend {
            self.extend_to(offset, cx);
        } else {
            self.set_cursor(offset, cx);
        }
        self.ensure_cursor_visible();
    }

    fn insert_at_cursor(&mut self, text: &str, cx: &mut Context<Self>) {
        let primary = self.buffer.selections().primary();
        let range = primary.range();
        let insert_at = range.start;
        self.buffer.edit(range, text);
        self.set_cursor(insert_at + text.len(), cx);
        self.ensure_cursor_visible();
    }

    fn backspace(&mut self, cx: &mut Context<Self>) {
        let primary = self.buffer.selections().primary();
        if !primary.is_empty() {
            let start = primary.start();
            self.buffer.remove(primary.range());
            self.set_cursor(start, cx);
            return;
        }
        let head = primary.head;
        let start = self.buffer.prev_char_boundary(head);
        if start == head {
            return;
        }
        // Smart pair deletion: if the cursor sits right between an empty
        // pair `()`, `{}`, `[]`, `""`, or `''`, delete both characters at once.
        let end = self.buffer.next_char_boundary(head);
        if end > head {
            let pair = self.buffer.slice(start..end);
            if matches!(pair.as_str(), "()" | "{}" | "[]" | "\"\"" | "''" | "``") {
                self.buffer.remove(start..end);
                self.set_cursor(start, cx);
                return;
            }
        }
        self.buffer.remove(start..head);
        self.set_cursor(start, cx);
        if !self.completions.is_empty() {
            self.trigger_completions(false, cx);
        }
    }

    /// Smart newline on `Enter`: preserves the current line's leading
    /// indentation, adds one indent level after `{` / `(` / `[`, and splits an
    /// empty `{|}` block into three properly indented lines.
    fn insert_newline(&mut self, cx: &mut Context<Self>) {
        let primary = self.buffer.selections().primary();
        let range = primary.range();
        let line_idx = self.buffer.byte_to_line(range.start);
        let line_start = self.buffer.line_to_byte(line_idx);
        let line_text = self.buffer.line(line_idx);
        let indent_len = line_text
            .as_bytes()
            .iter()
            .take_while(|&&b| b == b' ')
            .count();
        let base_indent = " ".repeat(indent_len);
        let step_indent = " ".repeat(self.indent_width);

        let before_cursor = self.buffer.slice(line_start..range.start);
        let trimmed_before = before_cursor.trim_end();
        let opens_block = trimmed_before.ends_with('{')
            || trimmed_before.ends_with('(')
            || trimmed_before.ends_with('[');

        let next_end = self.buffer.next_char_boundary(range.end);
        let next_char = (next_end > range.end)
            .then(|| self.buffer.slice(range.end..next_end))
            .unwrap_or_default();
        let closes_block = matches!(next_char.as_str(), "}" | ")" | "]");

        if opens_block && closes_block {
            let inserted = format!("\n{base_indent}{step_indent}\n{base_indent}");
            let cursor_target = range.start + 1 + base_indent.len() + step_indent.len();
            self.buffer.edit(range, &inserted);
            self.set_cursor(cursor_target, cx);
            self.ensure_cursor_visible();
        } else if opens_block {
            let inserted = format!("\n{base_indent}{step_indent}");
            self.insert_at_cursor(&inserted, cx);
        } else {
            let inserted = format!("\n{base_indent}");
            self.insert_at_cursor(&inserted, cx);
        }
    }

    /// Smart character input: handles auto-closing brackets/quotes, wrapping
    /// an active selection in brackets/quotes, and skipping over an existing
    /// closing bracket/quote.
    fn handle_char_input(&mut self, text: &str, cx: &mut Context<Self>) {
        let primary = self.buffer.selections().primary();
        let close_for = match text {
            "(" => Some(")"),
            "{" => Some("}"),
            "[" => Some("]"),
            "\"" => Some("\""),
            "'" => Some("'"),
            "`" => Some("`"),
            _ => None,
        };

        // 1. Wrap a non-empty selection in the pair
        if !primary.is_empty() {
            if let Some(close) = close_for {
                let range = primary.range();
                let selected = self.buffer.slice(range.clone());
                let wrapped = format!("{text}{selected}{close}");
                self.buffer.edit(range.clone(), &wrapped);
                self.buffer.selections_mut().set_primary(Selection::new(
                    range.start + text.len(),
                    range.start + text.len() + selected.len(),
                ));
                cx.notify();
                return;
            }
        }

        // 2. Skip over an existing closing delimiter when typing `)`, `}`, `]`, `"`, `'`
        if primary.is_empty() && matches!(text, ")" | "}" | "]" | "\"" | "'" | "`") {
            let head = primary.head;
            let next_end = self.buffer.next_char_boundary(head);
            if next_end > head && self.buffer.slice(head..next_end) == text {
                self.set_cursor(next_end, cx);
                return;
            }
        }

        // 3. Auto-close opening delimiter if the character after the cursor is
        //    whitespace, a closing delimiter, or end-of-line.
        if primary.is_empty() {
            if let Some(close) = close_for {
                let head = primary.head;
                let next_end = self.buffer.next_char_boundary(head);
                let next_ch = (next_end > head)
                    .then(|| self.buffer.slice(head..next_end))
                    .unwrap_or_default();
                let should_autoclose = next_ch.is_empty()
                    || next_ch.starts_with([' ', '\t', '\n', '\r', ')', '}', ']', ',', ';']);
                if should_autoclose {
                    let pair = format!("{text}{close}");
                    self.buffer.edit(head..head, &pair);
                    self.set_cursor(head + text.len(), cx);
                    self.ensure_cursor_visible();
                    if text == "(" {
                        self.trigger_completions(true, cx);
                    }
                    return;
                }
            }
        }

        self.insert_at_cursor(text, cx);

        let is_trigger = text == "."
            || text == ":"
            || text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if is_trigger {
            self.trigger_completions(text == ":", cx);
        } else {
            self.dismiss_completions(cx);
        }
    }

    pub(crate) fn dismiss_completions(&mut self, cx: &mut Context<Self>) {
        if !self.completions.is_empty() {
            self.completions.clear();
            self.completion_selected = 0;
            cx.notify();
        }
    }

    /// Return `(prefix_byte_range_in_buffer, prefix_string, is_after_dot)` at
    /// the current primary cursor.
    fn current_completion_prefix(&self) -> (Range<usize>, String, bool) {
        let head = self.buffer.selections().primary().head;
        let line_idx = self.buffer.byte_to_line(head);
        let line_start = self.buffer.line_to_byte(line_idx);
        let before = self.buffer.slice(line_start..head);
        let bytes = before.as_bytes();
        let mut word_start = bytes.len();
        while word_start > 0 && is_word_byte(bytes[word_start - 1]) {
            word_start -= 1;
        }
        let prefix = before[word_start..].to_string();
        let after_dot = word_start > 0 && bytes[word_start - 1] == b'.';
        ((line_start + word_start)..head, prefix, after_dot)
    }

    pub(crate) fn trigger_completions(&mut self, manual: bool, cx: &mut Context<Self>) {
        let (_range, prefix, after_dot) = self.current_completion_prefix();
        if !manual && prefix.is_empty() && !after_dot {
            self.dismiss_completions(cx);
            return;
        }

        // Sync buffer text to LSP immediately so `textDocument/completion`
        // sees the newly typed character before the next frame's `render()`.
        if should_parse_syntax(&self.buffer) {
            let text = self.buffer.text();
            self.sync_lsp_text(&text);
            let head = self.buffer.selections().primary().head;
            let line = self.buffer.byte_to_line(head);
            let col = head - self.buffer.line_to_byte(line);
            self.request_lsp_completion(line, col);
        }

        self.completions = self.compute_local_completions(&prefix, after_dot, manual);
        self.completion_selected = 0;
        cx.notify();
    }

    pub(crate) fn merge_lsp_completion_entries(
        &mut self,
        lsp_entries: Vec<CompletionEntry>,
        cx: &mut Context<Self>,
    ) {
        let (_range, prefix, after_dot) = self.current_completion_prefix();
        let prefix_lower = prefix.to_lowercase();
        let mut entries: Vec<CompletionEntry> = lsp_entries
            .into_iter()
            .filter(|e| {
                e.label != prefix
                    && (prefix_lower.is_empty()
                        || e.label.to_lowercase().contains(&prefix_lower))
            })
            .collect();

        entries.sort_by_key(|e| {
            let lower = e.label.to_lowercase();
            let exact = e.label.starts_with(&prefix);
            let starts = lower.starts_with(&prefix_lower);
            (!exact, !starts, e.label.len(), e.label.clone())
        });

        let mut seen = std::collections::HashSet::new();
        let mut merged = Vec::new();
        for entry in entries
            .into_iter()
            .chain(self.compute_local_completions(&prefix, after_dot, true))
        {
            if seen.insert(entry.label.clone()) {
                merged.push(entry);
                if merged.len() >= 28 {
                    break;
                }
            }
        }
        self.completions = merged;
        self.completion_selected = 0;
        cx.notify();
    }

    fn compute_local_completions(
        &self,
        prefix: &str,
        after_dot: bool,
        manual: bool,
    ) -> Vec<CompletionEntry> {
        if prefix.is_empty() && !after_dot && !manual {
            return Vec::new();
        }
        let prefix_lower = prefix.to_lowercase();
        let mut seen = std::collections::HashSet::new();
        let mut results: Vec<CompletionEntry> = Vec::new();
        let is_dart = self
            .buffer
            .path()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            == Some("dart");

        // 1. VS Code Flutter Snippets (`stless`, `stful`, `build`)
        if is_dart && !after_dot && !prefix.is_empty() {
            let snippets: &[(&str, &str, &str)] = &[
                (
                    "stless",
                    "Flutter StatelessWidget",
                    "class MyWidget extends StatelessWidget {\n  const MyWidget({super.key});\n\n  @override\n  Widget build(BuildContext context) {\n    return const Placeholder();\n  }\n}",
                ),
                (
                    "stful",
                    "Flutter StatefulWidget",
                    "class MyWidget extends StatefulWidget {\n  const MyWidget({super.key});\n\n  @override\n  State<MyWidget> createState() => _MyWidgetState();\n}\n\nclass _MyWidgetState extends State<MyWidget> {\n  @override\n  Widget build(BuildContext context) {\n    return const Placeholder();\n  }\n}",
                ),
                (
                    "build",
                    "@override Widget build(BuildContext context)",
                    "@override\n  Widget build(BuildContext context) {\n    return const SizedBox.shrink();\n  }",
                ),
            ];
            for &(trigger, desc, expansion) in snippets {
                if trigger.starts_with(&prefix_lower) && seen.insert(trigger.to_string()) {
                    results.push(CompletionEntry {
                        label: trigger.to_string(),
                        insert_text: expansion.to_string(),
                        detail: Some(desc.to_string()),
                        kind: CompletionKindBadge::Keyword,
                        additional_edits: Vec::new(),
                    });
                }
            }
        }

        // 2. Flutter Widget Constructors & Named Parameters (instant zero-latency suggestions)
        if is_dart && !after_dot {
            const FLUTTER_NAMED_ARGS: &[(&str, &str, &str)] = &[
                ("child:", "child: ", "Widget?"),
                ("children:", "children: [\n  \n],", "List<Widget>"),
                ("padding:", "padding: const EdgeInsets.all(8.0),", "EdgeInsetsGeometry"),
                ("margin:", "margin: const EdgeInsets.all(8.0),", "EdgeInsetsGeometry"),
                ("mainAxisAlignment:", "mainAxisAlignment: MainAxisAlignment.center,", "MainAxisAlignment"),
                ("crossAxisAlignment:", "crossAxisAlignment: CrossAxisAlignment.start,", "CrossAxisAlignment"),
                ("mainAxisSize:", "mainAxisSize: MainAxisSize.min,", "MainAxisSize"),
                ("onPressed:", "onPressed: () {},", "VoidCallback?"),
                ("onTap:", "onTap: () {},", "GestureTapCallback?"),
                ("color:", "color: ", "Color?"),
                ("backgroundColor:", "backgroundColor: ", "Color?"),
                ("decoration:", "decoration: const BoxDecoration(),", "Decoration?"),
                ("style:", "style: const TextStyle(),", "TextStyle?"),
                ("width:", "width: ", "double?"),
                ("height:", "height: ", "double?"),
                ("alignment:", "alignment: Alignment.center,", "AlignmentGeometry"),
                ("builder:", "builder: (context) => ", "WidgetBuilder"),
                ("itemCount:", "itemCount: ", "int?"),
                ("itemBuilder:", "itemBuilder: (context, index) => ", "IndexedWidgetBuilder"),
                ("appBar:", "appBar: AppBar(),", "PreferredSizeWidget?"),
                ("body:", "body: ", "Widget?"),
                ("floatingActionButton:", "floatingActionButton: ", "Widget?"),
            ];
            for &(label, insert, ty) in FLUTTER_NAMED_ARGS {
                if (prefix_lower.is_empty() || label.to_lowercase().starts_with(&prefix_lower))
                    && seen.insert(label.to_string())
                {
                    results.push(CompletionEntry {
                        label: label.to_string(),
                        insert_text: insert.to_string(),
                        detail: Some(ty.to_string()),
                        kind: CompletionKindBadge::Property,
                        additional_edits: Vec::new(),
                    });
                }
            }

            const FLUTTER_WIDGETS: &[(&str, &str, &str)] = &[
                ("Scaffold", "Scaffold(\n  body: \n)", "package:flutter/material.dart"),
                ("Container", "Container(\n  child: \n)", "package:flutter/widgets.dart"),
                ("Column", "Column(\n  children: [\n    \n  ],\n)", "package:flutter/widgets.dart"),
                ("Row", "Row(\n  children: [\n    \n  ],\n)", "package:flutter/widgets.dart"),
                ("Padding", "Padding(\n  padding: const EdgeInsets.all(8.0),\n  child: \n)", "package:flutter/widgets.dart"),
                ("Center", "Center(\n  child: \n)", "package:flutter/widgets.dart"),
                ("SizedBox", "SizedBox(\n  width: ,\n  height: ,\n)", "package:flutter/widgets.dart"),
                ("SizedBox.shrink", "const SizedBox.shrink()", "package:flutter/widgets.dart"),
                ("Expanded", "Expanded(\n  child: \n)", "package:flutter/widgets.dart"),
                ("Flexible", "Flexible(\n  child: \n)", "package:flutter/widgets.dart"),
                ("Text", "Text('')", "package:flutter/widgets.dart"),
                ("Icon", "Icon(Icons.)", "package:flutter/widgets.dart"),
                ("ElevatedButton", "ElevatedButton(\n  onPressed: () {},\n  child: const Text(''),\n)", "package:flutter/material.dart"),
                ("TextButton", "TextButton(\n  onPressed: () {},\n  child: const Text(''),\n)", "package:flutter/material.dart"),
                ("IconButton", "IconButton(\n  onPressed: () {},\n  icon: const Icon(Icons.add),\n)", "package:flutter/material.dart"),
                ("ListView.builder", "ListView.builder(\n  itemCount: 0,\n  itemBuilder: (context, index) {\n    return const SizedBox();\n  },\n)", "package:flutter/widgets.dart"),
                ("Stack", "Stack(\n  children: [\n    \n  ],\n)", "package:flutter/widgets.dart"),
                ("Positioned", "Positioned(\n  child: \n)", "package:flutter/widgets.dart"),
                ("SafeArea", "SafeArea(\n  child: \n)", "package:flutter/widgets.dart"),
                ("GestureDetector", "GestureDetector(\n  onTap: () {},\n  child: \n)", "package:flutter/widgets.dart"),
                ("InkWell", "InkWell(\n  onTap: () {},\n  child: \n)", "package:flutter/material.dart"),
                ("Card", "Card(\n  child: \n)", "package:flutter/material.dart"),
                ("Divider", "Divider()", "package:flutter/material.dart"),
                ("CircularProgressIndicator", "const CircularProgressIndicator()", "package:flutter/material.dart"),
                ("FutureBuilder", "FutureBuilder(\n  future: null,\n  builder: (context, snapshot) {\n    return const SizedBox();\n  },\n)", "package:flutter/widgets.dart"),
                ("StreamBuilder", "StreamBuilder(\n  stream: null,\n  builder: (context, snapshot) {\n    return const SizedBox();\n  },\n)", "package:flutter/widgets.dart"),
                ("EdgeInsets.all", "const EdgeInsets.all(8.0)", "package:flutter/painting.dart"),
                ("EdgeInsets.symmetric", "const EdgeInsets.symmetric(horizontal: 16.0, vertical: 8.0)", "package:flutter/painting.dart"),
                ("BoxDecoration", "BoxDecoration(\n  color: ,\n  borderRadius: BorderRadius.circular(8.0),\n)", "package:flutter/painting.dart"),
                ("TextStyle", "TextStyle(\n  fontSize: 14.0,\n  fontWeight: FontWeight.w500,\n)", "package:flutter/painting.dart"),
                ("MainAxisAlignment", "MainAxisAlignment.center", "package:flutter/rendering.dart"),
                ("CrossAxisAlignment", "CrossAxisAlignment.start", "package:flutter/rendering.dart"),
                ("Colors", "Colors.blue", "package:flutter/material.dart"),
                ("Icons", "Icons.check", "package:flutter/material.dart"),
                ("Theme.of", "Theme.of(context)", "package:flutter/material.dart"),
                ("MediaQuery.of", "MediaQuery.of(context)", "package:flutter/widgets.dart"),
                ("Navigator.of", "Navigator.of(context)", "package:flutter/widgets.dart"),
            ];
            if !prefix_lower.is_empty() {
                for &(label, insert, pkg) in FLUTTER_WIDGETS {
                    if label != prefix
                        && label.to_lowercase().contains(&prefix_lower)
                        && seen.insert(label.to_string())
                    {
                        results.push(CompletionEntry {
                            label: label.to_string(),
                            insert_text: insert.to_string(),
                            detail: Some(pkg.to_string()),
                            kind: CompletionKindBadge::Type,
                            additional_edits: Vec::new(),
                        });
                    }
                }
            }
        }

        const KEYWORDS: &[&str] = &[
            "abstract", "async", "await", "break", "case", "catch", "class", "const", "continue",
            "covariant", "default", "deferred", "do", "dynamic", "else", "enum", "export",
            "extends", "extension", "external", "factory", "false", "final", "finally", "for",
            "Function", "get", "hide", "if", "implements", "import", "in", "interface", "is",
            "late", "library", "mixin", "new", "null", "on", "operator", "part", "required",
            "rethrow", "return", "sealed", "set", "show", "static", "super", "switch", "sync",
            "this", "throw", "true", "try", "typedef", "var", "void", "when", "while", "with",
            "yield", "Widget", "BuildContext", "StatelessWidget", "StatefulWidget", "State",
            "Future", "Stream", "List", "Map", "Set", "String", "int", "double", "bool",
            "override", "fn", "pub", "struct", "impl", "let", "mut", "match", "Some", "None",
            "Ok", "Err",
        ];

        if !after_dot && !prefix.is_empty() {
            for &kw in KEYWORDS {
                if kw != prefix && kw.to_lowercase().starts_with(&prefix_lower) && seen.insert(kw.to_string()) {
                    let is_type = kw.bytes().next().is_some_and(|b| b.is_ascii_uppercase());
                    results.push(CompletionEntry {
                        label: kw.to_string(),
                        insert_text: kw.to_string(),
                        detail: Some(if is_type { "type" } else { "keyword" }.to_string()),
                        kind: if is_type {
                            CompletionKindBadge::Type
                        } else {
                            CompletionKindBadge::Keyword
                        },
                        additional_edits: Vec::new(),
                    });
                }
            }
        }

        // Scan buffer identifiers (when buffer <= 256 KiB)
        if should_parse_syntax(&self.buffer) {
            let text = self.buffer.text();
            let bytes = text.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if is_word_byte(bytes[i]) && !bytes[i].is_ascii_digit() {
                    let start = i;
                    let preceded_by_dot = start > 0 && bytes[start - 1] == b'.';
                    while i < bytes.len() && is_word_byte(bytes[i]) {
                        i += 1;
                    }
                    let word = &text[start..i];
                    if word.len() >= 2
                        && word != prefix
                        && (prefix_lower.is_empty()
                            || word.to_lowercase().contains(&prefix_lower))
                        && (!after_dot || preceded_by_dot || !prefix.is_empty())
                        && seen.insert(word.to_string())
                    {
                        let followed_by_paren = i < bytes.len() && bytes[i] == b'(';
                        let is_type = word.bytes().next().is_some_and(|b| b.is_ascii_uppercase());
                        let kind = if followed_by_paren {
                            CompletionKindBadge::Function
                        } else if preceded_by_dot {
                            CompletionKindBadge::Property
                        } else if is_type {
                            CompletionKindBadge::Type
                        } else {
                            CompletionKindBadge::Variable
                        };
                        results.push(CompletionEntry {
                            label: word.to_string(),
                            insert_text: word.to_string(),
                            detail: None,
                            kind,
                            additional_edits: Vec::new(),
                        });
                    }
                } else {
                    i += 1;
                }
            }
        }

        results.sort_by_key(|e| {
            let lower = e.label.to_lowercase();
            let exact_prefix = !prefix.is_empty() && e.label.starts_with(prefix);
            let ci_prefix = !prefix_lower.is_empty() && lower.starts_with(&prefix_lower);
            (!exact_prefix, !ci_prefix, e.label.len(), e.label.clone())
        });
        results.truncate(24);
        results
    }

    /// Compute VS Code Flutter extension style closing labels (`// WidgetName`)
    /// for lines that close a multi-line PascalCase constructor call (`Divider(`,
    /// `Scaffold(`, `Column(`, etc.).
    fn compute_closing_widget_labels(text: &str) -> std::collections::HashMap<usize, String> {
        let mut labels_by_line: std::collections::HashMap<usize, Vec<String>> =
            std::collections::HashMap::new();
        let bytes = text.as_bytes();
        // Stack of `(open_line_idx, Option<widget_name>)` for each `(` or `[`
        let mut stack: Vec<(usize, Option<String>, u8)> = Vec::new();
        let mut line = 0usize;
        let mut i = 0usize;
        let mut in_single = false;
        let mut in_double = false;
        let mut in_line_comment = false;

        while i < bytes.len() {
            let b = bytes[i];
            if b == b'\n' {
                line += 1;
                in_line_comment = false;
                i += 1;
                continue;
            }
            if in_line_comment {
                i += 1;
                continue;
            }
            if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_line_comment = true;
                i += 2;
                continue;
            }
            if (in_single || in_double) && b == b'\\' {
                i += 2;
                continue;
            }
            if !in_double && b == b'\'' {
                in_single = !in_single;
                i += 1;
                continue;
            }
            if !in_single && b == b'"' {
                in_double = !in_double;
                i += 1;
                continue;
            }
            if in_single || in_double {
                i += 1;
                continue;
            }

            if b == b'(' {
                // Check if preceded by a PascalCase constructor name (`Divider(`, `ListView.builder(`)
                let mut end = i;
                // Skip optional `<TypeArgs>` before `(`
                if end > 0 && bytes[end - 1] == b'>' {
                    let mut angle = 1i32;
                    end -= 1;
                    while end > 0 && angle > 0 {
                        end -= 1;
                        if bytes[end] == b'>' {
                            angle += 1;
                        } else if bytes[end] == b'<' {
                            angle -= 1;
                        }
                    }
                }
                let ident_end = end;
                let mut start = ident_end;
                while start > 0
                    && (bytes[start - 1].is_ascii_alphanumeric()
                        || bytes[start - 1] == b'_'
                        || bytes[start - 1] == b'.')
                {
                    start -= 1;
                }
                let raw_name = &text[start..ident_end];
                let first_seg = raw_name.split('.').next().unwrap_or("");
                let is_widget = first_seg
                    .bytes()
                    .next()
                    .is_some_and(|c| c.is_ascii_uppercase())
                    && first_seg.bytes().any(|c| c.is_ascii_lowercase());
                let widget_name = is_widget.then(|| raw_name.to_string());
                stack.push((line, widget_name, b'('));
            } else if b == b'[' {
                // Check if preceded by `children:`
                let before = text[..i].trim_end();
                let is_children = before.ends_with("children:");
                stack.push((line, is_children.then(|| "[]".to_string()), b'['));
            } else if b == b')' || b == b']' {
                let expected = if b == b')' { b'(' } else { b'[' };
                if let Some((open_line, maybe_name, open_ch)) = stack.pop() {
                    if open_ch == expected && line > open_line + 1 {
                        if let Some(name) = maybe_name {
                            labels_by_line.entry(line).or_default().push(name);
                        }
                    }
                }
            }
            i += 1;
        }

        labels_by_line
            .into_iter()
            .map(|(ln, mut names)| {
                names.reverse();
                (ln, format!("// {}", names.join(", ")))
            })
            .collect()
    }

    pub(crate) fn accept_completion(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.completions.get(ix).cloned() else {
            return;
        };
        let (prefix_range, _prefix, _after_dot) = self.current_completion_prefix();
        let insert_at = prefix_range.start;
        self.buffer.edit(prefix_range, &entry.insert_text);
        let mut cursor_pos = insert_at + entry.insert_text.len();

        // Apply any additional edits (e.g. Dart LSP auto-import headers)
        if !entry.additional_edits.is_empty() {
            let mut edits = entry.additional_edits;
            edits.sort_by_key(|edit| (edit.range.start.line, edit.range.start.character));
            for edit in edits.into_iter().rev() {
                let start = self.buffer.line_to_byte(edit.range.start.line as usize)
                    + edit.range.start.character as usize;
                let end = self.buffer.line_to_byte(edit.range.end.line as usize)
                    + edit.range.end.character as usize;
                let end = end.min(self.buffer.len_bytes());
                let start = start.min(end);
                self.buffer.edit(start..end, &edit.new_text);
                if end <= insert_at {
                    let removed = end - start;
                    let added = edit.new_text.len();
                    cursor_pos = cursor_pos.saturating_sub(removed) + added;
                }
            }
        }

        self.completions.clear();
        self.completion_selected = 0;
        self.set_cursor(cursor_pos.min(self.buffer.len_bytes()), cx);
        self.ensure_cursor_visible();
    }

    /// Toggle `// ` line comments across the current line or selected lines (`⌘/`).
    pub fn toggle_line_comment(&mut self, cx: &mut Context<Self>) {
        let primary = self.buffer.selections().primary();
        let range = primary.range();
        let start_line = self.buffer.byte_to_line(range.start);
        let end_line = if range.end > range.start {
            let l = self.buffer.byte_to_line(range.end);
            if self.buffer.line_to_byte(l) == range.end && l > start_line {
                l - 1
            } else {
                l
            }
        } else {
            start_line
        };

        let all_commented = (start_line..=end_line).all(|line_idx| {
            let line = self.buffer.line(line_idx);
            let trimmed = line.trim_start_matches(' ').trim_end_matches(['\n', '\r']);
            !trimmed.is_empty() && trimmed.starts_with("//")
        });

        for line_idx in (start_line..=end_line).rev() {
            let line_start = self.buffer.line_to_byte(line_idx);
            let line = self.buffer.line(line_idx);
            let leading = line.as_bytes().iter().take_while(|&&b| b == b' ').count();
            let trimmed = line[leading..].trim_end_matches(['\n', '\r']);
            if trimmed.is_empty() {
                continue;
            }
            let comment_at = line_start + leading;
            if all_commented {
                let remove_len = if trimmed.starts_with("// ") { 3 } else { 2 };
                self.buffer.remove(comment_at..(comment_at + remove_len));
            } else {
                self.buffer.insert(comment_at, "// ");
            }
        }
        cx.notify();
    }

    fn delete_forward(&mut self, cx: &mut Context<Self>) {
        let primary = self.buffer.selections().primary();
        if !primary.is_empty() {
            let start = primary.start();
            self.buffer.remove(primary.range());
            self.set_cursor(start, cx);
            return;
        }
        let end = self.buffer.next_char_boundary(primary.head);
        if end == primary.head {
            return;
        }
        self.buffer.remove(primary.head..end);
        self.set_cursor(primary.head, cx);
    }

    fn move_left(&mut self, extend: bool, cx: &mut Context<Self>) {
        let primary = self.buffer.selections().primary();
        if !extend && !primary.is_empty() {
            self.set_cursor(primary.start(), cx);
            return;
        }
        let new_head = self.buffer.prev_char_boundary(primary.head);
        self.move_head_or_cursor(new_head, extend, cx);
    }

    fn move_right(&mut self, extend: bool, cx: &mut Context<Self>) {
        let primary = self.buffer.selections().primary();
        if !extend && !primary.is_empty() {
            self.set_cursor(primary.end(), cx);
            return;
        }
        let new_head = self.buffer.next_char_boundary(primary.head);
        self.move_head_or_cursor(new_head, extend, cx);
    }

    fn move_vertical(&mut self, delta: isize, extend: bool, cx: &mut Context<Self>) {
        let head = self.buffer.selections().primary().head;
        let line = self.buffer.byte_to_line(head);
        let column = head - self.buffer.line_to_byte(line);

        let target_line = line as isize + delta;
        if target_line < 0 || target_line as usize >= self.buffer.line_count() {
            return;
        }
        let target_line = target_line as usize;

        let line_start = self.buffer.line_to_byte(target_line);
        let line_text = self.buffer.line(target_line);
        let line_len_without_ending = line_text.trim_end_matches(['\n', '\r']).len();
        let new_offset = line_start + column.min(line_len_without_ending);
        self.move_head_or_cursor(new_offset, extend, cx);
    }

    fn move_home(&mut self, extend: bool, cx: &mut Context<Self>) {
        let head = self.buffer.selections().primary().head;
        let line = self.buffer.byte_to_line(head);
        self.move_head_or_cursor(self.buffer.line_to_byte(line), extend, cx);
    }

    fn move_end(&mut self, extend: bool, cx: &mut Context<Self>) {
        let head = self.buffer.selections().primary().head;
        let line = self.buffer.byte_to_line(head);
        let line_text = self.buffer.line(line);
        let line_start = self.buffer.line_to_byte(line);
        let len_without_ending = line_text.trim_end_matches(['\n', '\r']).len();
        self.move_head_or_cursor(line_start + len_without_ending, extend, cx);
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        let len = self.buffer.len_bytes();
        self.buffer
            .selections_mut()
            .set_primary(Selection::new(0, len));
        cx.notify();
    }

    fn copy(&mut self, cx: &mut App) {
        let primary = self.buffer.selections().primary();
        if primary.is_empty() {
            return;
        }
        let text = self.buffer.slice(primary.range());
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn cut(&mut self, cx: &mut Context<Self>) {
        let primary = self.buffer.selections().primary();
        if primary.is_empty() {
            return;
        }
        let text = self.buffer.slice(primary.range());
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        let start = primary.start();
        self.buffer.remove(primary.range());
        self.set_cursor(start, cx);
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        self.insert_at_cursor(&text, cx);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.clear_hover(cx);
        let keystroke = &event.keystroke;
        let primary_mod = keystroke.modifiers.platform || keystroke.modifiers.control;

        // `Ctrl+Space` manually triggers autocomplete
        if keystroke.modifiers.control && keystroke.key.as_str() == "space" {
            self.trigger_completions(true, cx);
            return;
        }

        // Navigate / accept / dismiss active autocomplete popup
        if !self.completions.is_empty() && !primary_mod {
            match keystroke.key.as_str() {
                "down" => {
                    self.completion_selected =
                        (self.completion_selected + 1) % self.completions.len();
                    cx.notify();
                    return;
                }
                "up" => {
                    self.completion_selected = if self.completion_selected == 0 {
                        self.completions.len() - 1
                    } else {
                        self.completion_selected - 1
                    };
                    cx.notify();
                    return;
                }
                "tab" | "enter" => {
                    self.accept_completion(self.completion_selected, cx);
                    return;
                }
                "escape" => {
                    self.dismiss_completions(cx);
                    return;
                }
                _ => {}
            }
        }

        if primary_mod {
            match keystroke.key.as_str() {
                "c" => self.copy(cx),
                "x" => self.cut(cx),
                "v" => self.paste(cx),
                "a" => self.select_all(cx),
                "s" => {
                    self.save(cx);
                    cx.notify();
                }
                "f" if keystroke.modifiers.shift => {
                    self.request_format();
                }
                "r" => self.start_rename(cx),
                "." => self.request_code_actions(cx),
                "/" => self.toggle_line_comment(cx),
                "z" => {
                    if keystroke.modifiers.shift {
                        self.buffer.redo();
                    } else {
                        self.buffer.undo();
                    }
                    cx.notify();
                }
                _ => {
                    // Let app-level shortcuts (`⌘F`, `⌘P`, `⌘L`, `⌘G`, `⌘B`, `⌘J`, `⌘W`, `⌘N`, `⌘O`)
                    // bubble up to `DartIde::on_key_down`.
                    return;
                }
            }
            return;
        }

        let extend = keystroke.modifiers.shift;
        match keystroke.key.as_str() {
            "f2" => self.start_rename(cx),
            "backspace" => self.backspace(cx),
            "delete" => self.delete_forward(cx),
            "enter" => self.insert_newline(cx),
            "tab" => {
                let spaces = " ".repeat(self.indent_width);
                self.insert_at_cursor(&spaces, cx);
            }
            "left" => {
                self.dismiss_completions(cx);
                self.move_left(extend, cx);
            }
            "right" => {
                self.dismiss_completions(cx);
                self.move_right(extend, cx);
            }
            "up" => {
                self.dismiss_completions(cx);
                self.move_vertical(-1, extend, cx);
            }
            "down" => {
                self.dismiss_completions(cx);
                self.move_vertical(1, extend, cx);
            }
            "home" => {
                self.dismiss_completions(cx);
                self.move_home(extend, cx);
            }
            "end" => {
                self.dismiss_completions(cx);
                self.move_end(extend, cx);
            }
            _ => {
                if let Some(text) = &keystroke.key_char {
                    self.handle_char_input(text, cx);
                }
            }
        }
    }

    /// Highlight spans intersecting `line_text`'s byte range, run-length
    /// encoded into `(byte_range_within_line, color)` runs.
    ///
    /// Note: selection and word-occurrence highlights are intentionally NOT
    /// split into separate `div()` spans here — splitting a single `TextSystem`
    /// shaped line into extra `div()` boxes causes subpixel kerning/advance
    /// rounding differences in GPUI's flex layout, shifting the text when a
    /// word or selection is highlighted. Instead, selection and word-match
    /// backgrounds are painted directly on the line canvas using exact glyph
    /// `x_for_index` coordinates.
    fn line_runs(
        line_start: usize,
        line_text: &str,
        highlights: &[Highlight],
    ) -> Vec<(Range<usize>, Option<HighlightKind>)> {
        let line_end = line_start + line_text.len();
        let mut colors: Vec<Option<HighlightKind>> = vec![None; line_text.len()];

        for h in highlights {
            if h.range.end <= line_start || h.range.start >= line_end {
                continue;
            }
            let start = h.range.start.max(line_start) - line_start;
            let end = h.range.end.min(line_end) - line_start;
            for slot in &mut colors[start..end] {
                *slot = Some(h.kind);
            }
        }

        let mut runs = Vec::new();
        let mut i = 0;
        while i < colors.len() {
            let kind = colors[i];
            let start = i;
            while i < colors.len() && colors[i] == kind {
                i += 1;
            }
            runs.push((start..i, kind));
        }
        if runs.is_empty() {
            runs.push((0..0, None));
        }
        runs
    }

    fn word_range_at(text: &str, col: usize) -> Option<Range<usize>> {
        let bytes = text.as_bytes();
        if col > bytes.len() {
            return None;
        }
        let is_word = |i: usize| i < bytes.len() && is_word_byte(bytes[i]);
        let mut idx = col;
        if !is_word(idx) {
            if idx > 0 && is_word(idx - 1) {
                idx -= 1;
            } else {
                return None;
            }
        }
        let mut start = idx;
        while start > 0 && is_word(start - 1) {
            start -= 1;
        }
        let mut end = idx;
        while is_word(end) {
            end += 1;
        }
        Some(start..end)
    }

    /// The word (identifier-ish run of alphanumerics/underscores)
    /// touching byte column `col` in `text`, preferring the word to the
    /// right of the cursor but falling back to the one just behind it —
    /// the same convention every editor uses for "what am I pointing at"
    /// when the cursor sits exactly between two characters.
    fn word_at(text: &str, col: usize) -> Option<&str> {
        let range = Self::word_range_at(text, col)?;
        Some(&text[range])
    }

    /// Every whole-word occurrence of `word` in `text`, as local byte
    /// ranges — "whole-word" meaning neither neighbor is itself a word
    /// character, so searching for `name` doesn't also light up the
    /// middle of `rename` or `name2`.
    fn word_occurrences(text: &str, word: &str) -> Vec<Range<usize>> {
        if word.is_empty() {
            return Vec::new();
        }
        let bytes = text.as_bytes();
        let mut ranges = Vec::new();
        let mut from = 0;
        while let Some(pos) = text[from..].find(word) {
            let start = from + pos;
            let end = start + word.len();
            let left_ok = start == 0 || !is_word_byte(bytes[start - 1]);
            let right_ok = end == bytes.len() || !is_word_byte(bytes[end]);
            if left_ok && right_ok {
                ranges.push(start..end);
            }
            from = start + 1;
            if from >= bytes.len() {
                break;
            }
        }
        ranges
    }

    fn render_line(
        &self,
        index: usize,
        line_text: &str,
        cursor_line: Option<usize>,
        highlights: &[Highlight],
        selection: Option<Range<usize>>,
        diagnostics_by_line: &std::collections::HashMap<u32, lsp_types::DiagnosticSeverity>,
        diagnostic_messages: &std::collections::HashMap<
            u32,
            (lsp_types::DiagnosticSeverity, String),
        >,
        line_changes: &std::collections::HashMap<u32, LineChange>,
        indent_width: usize,
        current_word: Option<&str>,
        bracket_pair: Option<(usize, usize)>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_active_line = cursor_line == Some(index);
        let diagnostic_severity = diagnostics_by_line.get(&(index as u32)).copied();
        let diagnostic_msg = diagnostic_messages.get(&(index as u32));
        let gutter_color: Hsla = match diagnostic_severity {
            Some(sev) if sev == lsp_types::DiagnosticSeverity::ERROR => cx.theme().danger,
            Some(sev) if sev == lsp_types::DiagnosticSeverity::WARNING => cx.theme().warning,
            _ if is_active_line => cx.theme().foreground.opacity(0.95),
            _ => cx.theme().muted_foreground.opacity(0.45),
        };
        let mono_family = cx.theme().mono_font_family.clone();
        let gutter = h_flex()
            .w(px(GUTTER_WIDTH))
            .h_full()
            .flex_shrink_0()
            .items_center()
            .font_family(mono_family.clone())
            .text_size(px(GUTTER_FONT_SIZE))
            .text_color(gutter_color)
            .child(
                div()
                    .w(px(38.))
                    .text_right()
                    .child((index + 1).to_string()),
            )
            .child(
                div()
                    .w(px(GUTTER_WIDTH - 38.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(is_active_line, |el| {
                        let view = cx.entity();
                        el.cursor_pointer()
                            .child(
                                gpui_kit::component::Icon::new(
                                    gpui_kit::assets::IconName::Lightbulb,
                                )
                                .xsmall()
                                .text_color(cx.theme().warning),
                            )
                            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                                view.update(cx, |this, cx| {
                                    window.focus(&this.focus_handle, cx);
                                    this.request_code_actions(cx);
                                });
                            })
                    }),
            );

        // A thin bar flush against the window's left edge for a line
        // that differs from `HEAD` — green for a new line, blue for an
        // edited one, and a short amber notch at the top of the line
        // that now sits where something was deleted. On the active line,
        // fall back to a 2px accent bar (`docs/design-spec.md` §3.19).
        let change_marker = line_changes
            .get(&(index as u32))
            .map(|change| {
                let (color, full_height) = match change {
                    LineChange::Added => (cx.theme().success, true),
                    LineChange::Modified => (cx.theme().info, true),
                    LineChange::Removed => (cx.theme().warning, false),
                };
                let mut marker = div().absolute().left_0().top_0().w(px(2.)).bg(color);
                marker = if full_height { marker.bottom_0() } else { marker.h(px(5.)) };
                marker
            })
            .or_else(|| {
                is_active_line.then(|| {
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(2.))
                        .bg(cx.theme().primary)
                })
            });

        let active_right_marker = is_active_line.then(|| {
            div()
                .absolute()
                .right(px(2.))
                .top(px(2.))
                .bottom(px(2.))
                .w(px(3.))
                .rounded(px(1.5))
                .bg(cx.theme().success.opacity(0.75))
        });

        let line_start = self.buffer.line_to_byte(index);
        let display_text = line_text.trim_end_matches(['\n', '\r']);
        let cursor_col = (cursor_line == Some(index)).then(|| {
            let head = self.buffer.selections().primary().head;
            (head - line_start).min(display_text.len())
        });

        let selection_bg = cx.theme().selection.opacity(0.35);
        let word_match_bg = cx.theme().blue.opacity(0.16);
        let search_match_bg = cx.theme().warning.opacity(0.22);
        let active_search_bg = cx.theme().primary.opacity(0.38);
        let active_search_border = cx.theme().primary.opacity(0.85);
        let default_fg: Hsla = cx.theme().foreground;
        let is_doc_line = display_text.trim_start().starts_with("///");
        let caret_color = cx.theme().caret;

        // Build `TextRun`s for the single `shape_line` call that both paints
        // the text and computes exact `x_for_index` positions for the cursor,
        // selection, and word-match highlights — guaranteeing 0.0px drift
        // between characters and the caret!
        let text_runs: Vec<TextRun> = Self::line_runs(line_start, display_text, highlights)
            .into_iter()
            .filter(|(range, _)| !range.is_empty())
            .map(|(range, kind)| {
                let mut run_font = font(mono_family.clone());
                if let Some(k) = kind {
                    if k == HighlightKind::Comment
                        || (is_doc_line && k == HighlightKind::Attribute)
                    {
                        run_font.style = FontStyle::Italic;
                    }
                }
                TextRun {
                    len: range.len(),
                    font: run_font,
                    color: kind.map(|k| highlight_color(k).into()).unwrap_or(default_fg),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }
            })
            .collect();

        // Local byte ranges within `display_text` for word-occurrences and matching brackets.
        let mut local_word_matches: Vec<Range<usize>> = current_word
            .map(|word| Self::word_occurrences(display_text, word))
            .unwrap_or_default();
        if let Some((b1, b2)) = bracket_pair {
            let line_end = line_start + display_text.len();
            for b in [b1, b2] {
                if b >= line_start && b < line_end {
                    let col = b - line_start;
                    local_word_matches.push(col..(col + 1));
                }
            }
        }

        let local_selection: Option<Range<usize>> = selection.and_then(|sel| {
            let line_end = line_start + display_text.len();
            (sel.end > line_start && sel.start < line_end).then(|| {
                let start = sel.start.max(line_start) - line_start;
                let end = sel.end.min(line_end) - line_start;
                start..end
            })
        });

        let line_end = line_start + display_text.len();
        let active_search_range = self.search_matches.get(self.active_search_match).cloned();
        let local_search_matches: Vec<(Range<usize>, bool)> = self
            .search_matches
            .iter()
            .filter(|m| m.end > line_start && m.start < line_end)
            .map(|m| {
                let is_active_match = active_search_range.as_ref() == Some(m);
                let start = m.start.max(line_start) - line_start;
                let end = m.end.min(line_end) - line_start;
                (start..end, is_active_match)
            })
            .collect();

        let diag_pill = diagnostic_msg.map(|(sev, msg)| {
            let diag_color = match *sev {
                s if s == lsp_types::DiagnosticSeverity::ERROR => cx.theme().danger,
                s if s == lsp_types::DiagnosticSeverity::WARNING => cx.theme().warning,
                _ => cx.theme().info,
            };
            h_flex()
                .ml_4()
                .px_2()
                .h(px(17.))
                .items_center()
                .gap_1()
                .rounded(px(4.))
                .bg(diag_color.opacity(0.12))
                .border_1()
                .border_color(diag_color.opacity(0.28))
                .text_size(px(11.))
                .text_color(diag_color)
                .child("\u{25cf}")
                .child(msg.clone())
        });

        // Single unified line canvas: shapes `display_text` ONCE with `text_runs`
        // and uses that exact `ShapedLine` for:
        // 1. Indent guides (using actual shaped space width)
        // 2. Word-occurrence & matching-bracket background pills
        // 3. Selection highlight
        // 4. Painting the syntax-highlighted line (`shaped.paint(...)`)
        // 5. Painting the vertical caret at `shaped.x_for_index(cursor_col)`
        // 6. Mouse click/drag/hover column hit-testing (`shaped.closest_index_for_x`)
        let line_shared: SharedString = display_text.to_string().into();
        let line_len = line_shared.len();
        let leading_spaces = display_text.as_bytes().iter().take_while(|&&b| b == b' ').count();
        let closing_label_shared: Option<SharedString> = self
            .closing_labels
            .get(&index)
            .filter(|_| !display_text.contains("//"))
            .map(|s| SharedString::from(s.clone()));
        let closing_font = {
            let mut f = font(mono_family.clone());
            f.style = FontStyle::Italic;
            f
        };
        let closing_color: Hsla = cx.theme().muted_foreground.opacity(0.42);
        let view = cx.entity();
        let line_canvas = canvas(
            move |bounds, window, _cx| {
                let shaped = window.text_system().shape_line(
                    line_shared.clone(),
                    px(LINE_FONT_SIZE),
                    &text_runs,
                    None,
                );
                let shaped_closing = closing_label_shared.as_ref().map(|lbl| {
                    let run = TextRun {
                        len: lbl.len(),
                        font: closing_font.clone(),
                        color: closing_color,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    window.text_system().shape_line(
                        lbl.clone(),
                        px(LINE_FONT_SIZE - 1.),
                        &[run],
                        None,
                    )
                });
                let text_end_x = shaped.width;
                (bounds, shaped, shaped_closing, text_end_x)
            },
            move |bounds, (_, shaped, shaped_closing, _text_end_x), window, cx| {
                // 1. Faint vertical indent guides at exact shaped column offsets
                for column in (indent_width..leading_spaces).step_by(indent_width).take(16) {
                    let x = shaped.x_for_index(column.min(line_len));
                    let guide_rect = Bounds::new(
                        point(bounds.origin.x + x, bounds.origin.y + px(3.)),
                        size(px(1.), bounds.size.height - px(6.)),
                    );
                    window.paint_quad(fill(guide_rect, cx.theme().border.opacity(0.35)));
                }

                // 2. Word-occurrence, bracket, & search-match highlight pills
                let box_h = px(LINE_FONT_SIZE + 4.);
                let box_y = bounds.origin.y + (bounds.size.height - box_h) * 0.5;
                for range in &local_word_matches {
                    let x1 = shaped.x_for_index(range.start.min(line_len));
                    let x2 = shaped.x_for_index(range.end.min(line_len));
                    if x2 > x1 {
                        let rect = Bounds::new(
                            point(bounds.origin.x + x1, box_y),
                            size(x2 - x1, box_h),
                        );
                        window.paint_quad(fill(rect, word_match_bg).corner_radii(px(3.)));
                    }
                }
                for (range, is_active_match) in &local_search_matches {
                    let x1 = shaped.x_for_index(range.start.min(line_len));
                    let x2 = shaped.x_for_index(range.end.min(line_len));
                    if x2 > x1 {
                        let rect = Bounds::new(
                            point(bounds.origin.x + x1, box_y),
                            size(x2 - x1, box_h),
                        );
                        if *is_active_match {
                            window.paint_quad(gpui_kit::quad(
                                rect,
                                px(3.),
                                active_search_bg,
                                Edges::all(px(1.)),
                                active_search_border,
                                BorderStyle::Solid,
                            ));
                        } else {
                            window.paint_quad(fill(rect, search_match_bg).corner_radii(px(3.)));
                        }
                    }
                }

                // 3. Selection highlight
                if let Some(sel) = &local_selection {
                    let x1 = shaped.x_for_index(sel.start.min(line_len));
                    let x2 = shaped.x_for_index(sel.end.min(line_len));
                    if x2 > x1 {
                        let rect = Bounds::new(
                            point(bounds.origin.x + x1, bounds.origin.y + px(1.)),
                            size(x2 - x1, bounds.size.height - px(2.)),
                        );
                        window.paint_quad(fill(rect, selection_bg).corner_radii(px(2.)));
                    }
                }

                // 4. Paint syntax-highlighted text & optional Flutter closing label (`// WidgetName`)
                let text_y = bounds.origin.y + (bounds.size.height - px(LINE_HEIGHT)) * 0.5;
                let _ = shaped.paint(
                    point(bounds.origin.x, text_y),
                    px(LINE_HEIGHT),
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
                if let Some(closing) = &shaped_closing {
                    let _ = closing.paint(
                        point(bounds.origin.x + shaped.width + px(12.), text_y),
                        px(LINE_HEIGHT),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }

                // 5. Paint caret on top of text at the exact shaped glyph boundary
                if let Some(col) = cursor_col {
                    let x = shaped.x_for_index(col.min(line_len));
                    let caret_h = px(LINE_FONT_SIZE + 3.);
                    let y = bounds.origin.y + (bounds.size.height - caret_h) * 0.5;
                    let caret_rect =
                        Bounds::new(point(bounds.origin.x + x, y), size(px(1.5), caret_h));
                    window.paint_quad(fill(caret_rect, caret_color).corner_radii(px(0.75)));
                }

                // 6. Mouse interactions using the exact same `shaped` line
                let down_shaped = shaped.clone();
                let down_view = view.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble
                        || event.button != MouseButton::Left
                        || !bounds.contains(&event.position)
                    {
                        return;
                    }
                    let rel_x = (event.position.x - bounds.origin.x).max(px(0.));
                    let column = down_shaped.closest_index_for_x(rel_x);
                    let go_to_definition = event.modifiers.platform || event.modifiers.control;
                    let click_count = event.click_count;
                    down_view.update(cx, |this, cx| {
                        window.focus(&this.focus_handle, cx);
                        this.clear_hover(cx);
                        this.dismiss_completions(cx);
                        if go_to_definition {
                            this.request_definition(index, column);
                        } else if click_count >= 3 {
                            this.select_line(index, cx);
                        } else if click_count == 2 {
                            this.select_word_on_line(index, column, cx);
                        } else if event.modifiers.shift {
                            this.extend_to(line_start + column, cx);
                        } else {
                            this.set_cursor(line_start + column, cx);
                        }
                    });
                });

                let move_shaped = shaped.clone();
                let move_view = view.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
                    if phase != DispatchPhase::Bubble
                        || event.pressed_button != Some(MouseButton::Left)
                        || !bounds.contains(&event.position)
                    {
                        return;
                    }
                    let rel_x = (event.position.x - bounds.origin.x).max(px(0.));
                    let column = move_shaped.closest_index_for_x(rel_x);
                    move_view.update(cx, |this, cx| {
                        this.extend_to(line_start + column, cx);
                    });
                });

                let hover_shaped = shaped.clone();
                let hover_view = view;
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
                    if phase != DispatchPhase::Bubble
                        || event.pressed_button.is_some()
                        || !bounds.contains(&event.position)
                    {
                        return;
                    }
                    let rel_x = (event.position.x - bounds.origin.x).max(px(0.));
                    let column = hover_shaped.closest_index_for_x(rel_x);
                    hover_view.update(cx, |this, cx| {
                        this.note_hover_position(index, column, cx);
                    });
                });
            },
        )
        .size_full();

        div()
            .id(("editor-line", index))
            .relative()
            .flex()
            .w_full()
            .h(px(LINE_HEIGHT))
            .flex_shrink_0()
            .font_family(mono_family.clone())
            .text_size(px(LINE_FONT_SIZE))
            // The active line gets a full-width, flat wash of colour —
            // subtle enough to read as a highlight, not a second panel.
            // That's the whole cue; the gutter number brightening to match
            // is enough to say "and here's where", without a second marker
            // competing for the same job.
            .when(is_active_line, |el| el.bg(rgb(ACTIVE_LINE_BG)))
            .when_some(change_marker, |el, marker| el.child(marker))
            .when_some(active_right_marker, |el, marker| el.child(marker))
            .child(gutter)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .h_full()
                    .child(line_canvas)
                    .when_some(diag_pill, |el, pill| {
                        el.child(
                            div()
                                .absolute()
                                .right_4()
                                .top_0()
                                .bottom_0()
                                .flex()
                                .items_center()
                                .child(pill),
                        )
                    })
                    .when(
                        is_active_line && !self.completions.is_empty(),
                        |el| {
                            let col = cursor_col.unwrap_or(0);
                            let left_offset = px((col as f32 * 7.5).min(360.));
                            let popup = self.render_completion_popup(&mono_family, cx);
                            el.child(
                                deferred(
                                    div()
                                        .occlude()
                                        .absolute()
                                        .top(px(LINE_HEIGHT + 2.))
                                        .left(left_offset)
                                        .w(px(340.))
                                        .child(popup),
                                )
                                .with_priority(20),
                            )
                        },
                    )
                    .when_some(
                        self.hover_to_render()
                            .filter(|(line, _)| *line == index && self.completions.is_empty()),
                        |el, (_, text)| {
                            let hover_col = self
                                .hover_point
                                .filter(|(l, _)| *l == index)
                                .map(|(_, c)| c)
                                .unwrap_or(0);
                            let left_offset = px((hover_col as f32 * 7.5).min(280.));
                            let card = Self::render_hover_card(text, &mono_family, cx);
                            el.child(
                                deferred(
                                    div()
                                        .occlude()
                                        .absolute()
                                        .top(px(LINE_HEIGHT + 2.))
                                        .left(left_offset)
                                        .w(px(500.))
                                        .child(card),
                                )
                                .with_priority(10),
                            )
                        },
                    ),
            )
    }

    /// Format an LSP markdown hover payload (` ```dart ... ``` `, `Type: ...`,
    /// `---`, doc prose) into a structured, opaque popover card with syntax-
    /// highlighted code signatures and clean inline code pills.
    fn render_hover_card(
        raw: &str,
        mono_family: &SharedString,
        cx: &Context<Self>,
    ) -> Div {
        let mut code_blocks: Vec<String> = Vec::new();
        let mut sections: Vec<Vec<String>> = vec![Vec::new()];
        let mut in_fence = false;
        let mut current_code = Vec::new();

        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("```") {
                if in_fence {
                    let block = current_code.join("\n");
                    if !block.trim().is_empty() {
                        code_blocks.push(block);
                    }
                    current_code.clear();
                    in_fence = false;
                } else {
                    in_fence = true;
                }
                continue;
            }
            if in_fence {
                current_code.push(line.to_string());
            } else if trimmed == "---" || trimmed == "***" {
                sections.push(Vec::new());
            } else if let Some(last) = sections.last_mut() {
                last.push(line.to_string());
            }
        }

        let fg = cx.theme().popover_foreground;
        let muted = cx.theme().muted_foreground;
        let cyan = cx.theme().cyan;
        let border = cx.theme().border;
        let code_bg = cx.theme().background;
        let ui_family = cx.theme().font_family.clone();

        let mut card = v_flex()
            .w_full()
            .p_3()
            .gap_2()
            .rounded(px(8.))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(border)
            .shadow_xl()
            .font_family(ui_family)
            .text_size(px(12.))
            .text_color(fg);

        for block in code_blocks {
            let tree = SyntaxTree::parse(&block);
            let highlights = tree.highlights(&block);
            let mut block_el = v_flex()
                .w_full()
                .px_2p5()
                .py_2()
                .gap_0p5()
                .rounded(px(6.))
                .bg(code_bg)
                .border_1()
                .border_color(border)
                .font_family(mono_family.clone())
                .text_size(px(12.));

            let mut byte_offset = 0;
            for code_line in block.split('\n') {
                let runs = Self::line_runs(byte_offset, code_line, &highlights);
                let mut row = h_flex().w_full().flex_wrap().items_center();
                for (range, kind) in runs {
                    if range.is_empty() {
                        continue;
                    }
                    let slice = code_line[range].to_string();
                    let color: Hsla = kind.map(|k| highlight_color(k).into()).unwrap_or(fg);
                    row = row.child(div().text_color(color).child(slice));
                }
                block_el = block_el.child(row);
                byte_offset += code_line.len() + 1;
            }
            card = card.child(block_el);
        }

        let non_empty_sections: Vec<Vec<String>> = sections
            .into_iter()
            .map(|sec| {
                sec.into_iter()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|sec| !sec.is_empty())
            .collect();

        for (sec_ix, sec) in non_empty_sections.into_iter().enumerate() {
            if sec_ix > 0 {
                card = card.child(div().w_full().h(px(1.)).bg(border));
            }
            let mut sec_col = v_flex().w_full().gap_1();
            for line in sec {
                let clean = Self::clean_hover_markdown_line(&line);
                let is_meta = clean.starts_with("Type:") || clean.starts_with("Declared in");
                sec_col = sec_col.child(
                    div()
                        .w_full()
                        .text_size(px(if is_meta { 11.5 } else { 12. }))
                        .text_color(if is_meta { muted } else { fg })
                        .when(clean.contains('['), |el| el.text_color(fg))
                        .child(clean),
                );
            }
            let _ = cyan;
            card = card.child(sec_col);
        }

        card
    }

    fn clean_hover_markdown_line(line: &str) -> String {
        let mut out = String::with_capacity(line.len());
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '`' || chars[i] == '*' {
                i += 1;
                continue;
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }

    fn render_completion_popup(
        &self,
        mono_family: &SharedString,
        cx: &mut Context<Self>,
    ) -> Div {
        let view = cx.entity();
        let total = self.completions.len();
        let selected = self.completion_selected.min(total.saturating_sub(1));
        let window_size = 8usize;
        let start_ix = if selected >= window_size {
            selected + 1 - window_size
        } else {
            0
        };
        let end_ix = (start_ix + window_size).min(total);

        let fg = cx.theme().popover_foreground;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let primary = cx.theme().primary;

        let rows: Vec<AnyElement> = (start_ix..end_ix)
            .map(|ix| {
                let entry = &self.completions[ix];
                let is_sel = ix == selected;
                let (badge_text, badge_color) = match entry.kind {
                    CompletionKindBadge::Function => ("fn", rgb(0x61afef)),
                    CompletionKindBadge::Property => ("pr", rgb(0xe06c75)),
                    CompletionKindBadge::Type => ("ty", rgb(0xe5c07b)),
                    CompletionKindBadge::Variable => ("vr", rgb(0x56b6c2)),
                    CompletionKindBadge::Keyword => ("kw", rgb(0xc678dd)),
                    CompletionKindBadge::Constant => ("ct", rgb(0xd19a66)),
                };
                let badge_hsla: Hsla = badge_color.into();
                let view_click = view.clone();

                h_flex()
                    .id(("completion-item", ix))
                    .relative()
                    .w_full()
                    .h(px(24.))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .justify_between()
                    .rounded(px(5.))
                    .border_1()
                    .border_color(if is_sel {
                        rgba(0xea7a3c40)
                    } else {
                        rgba(0x00000000)
                    })
                    .when(is_sel, |el| {
                        el.bg(rgba(0xea7a3c1a)).child(
                            div()
                                .absolute()
                                .left(px(1.5))
                                .top(px(4.))
                                .bottom(px(4.))
                                .w(px(2.))
                                .rounded(px(1.))
                                .bg(primary),
                        )
                    })
                    .when(!is_sel, |el| el.hover(|s| s.bg(rgba(0xffffff08))))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                        cx.stop_propagation();
                        view_click.update(cx, |this, cx| {
                            this.accept_completion(ix, cx);
                        });
                    })
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .w(px(18.))
                                    .h(px(15.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(3.))
                                    .bg(badge_hsla.opacity(0.16))
                                    .font_family(mono_family.clone())
                                    .text_size(px(9.5))
                                    .text_color(badge_hsla)
                                    .child(badge_text),
                            )
                            .child(
                                div()
                                    .font_family(mono_family.clone())
                                    .text_size(px(12.))
                                    .text_color(fg)
                                    . truncate()
                                    .child(entry.label.clone()),
                            ),
                    )
                    .when_some(entry.detail.clone(), |el, detail| {
                        el.child(
                            div()
                                .max_w(px(120.))
                                .truncate()
                                .font_family(mono_family.clone())
                                .text_size(px(10.5))
                                .text_color(muted)
                                .child(detail),
                        )
                    })
                    .into_any_element()
            })
            .collect();

        v_flex()
            .w_full()
            .p_1()
            .gap_0p5()
            .rounded(px(8.))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(border)
            .shadow_xl()
            .children(rows)
            .child(
                h_flex()
                    .w_full()
                    .pt_1()
                    .px_2()
                    .border_t_1()
                    .border_color(border.opacity(0.6))
                    .items_center()
                    .justify_between()
                    .font_family(cx.theme().font_family.clone())
                    .text_size(px(10.))
                    .text_color(muted)
                    .child("\u{2191}\u{2193} select   \u{21e5}/\u{21b5} accept   esc close")
                    .child(format!("{}/{}", selected + 1, total)),
            )
    }
}

/// The smallest [`InputEdit`] that turns `old` into `new`, found by
/// trimming the common prefix and common suffix and treating whatever's
/// left in the middle as replaced. This doesn't know *which* edit
/// happened (a keystroke, an undo, a whole-file open) — it only needs the
/// before/after text, so every caller gets incremental reparsing for
/// free without threading edit details through every mutation path.
fn diff_as_edit(old: &str, new: &str) -> Option<InputEdit> {
    if old == new {
        return None;
    }

    let old_bytes = old.as_bytes();
    let new_bytes = new.as_bytes();

    let common_prefix = old_bytes
        .iter()
        .zip(new_bytes.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let max_suffix = (old_bytes.len() - common_prefix).min(new_bytes.len() - common_prefix);
    let common_suffix = (0..max_suffix)
        .take_while(|&i| old_bytes[old_bytes.len() - 1 - i] == new_bytes[new_bytes.len() - 1 - i])
        .count();

    let start_byte = common_prefix;
    let old_end_byte = old_bytes.len() - common_suffix;
    let new_end_byte = new_bytes.len() - common_suffix;

    Some(InputEdit {
        start_byte,
        old_end_byte,
        new_end_byte,
        start_position: point_at(old, start_byte),
        old_end_position: point_at(old, old_end_byte),
        new_end_position: point_at(new, new_end_byte),
    })
}

/// Whether `b` can be part of an identifier — ASCII alphanumerics and
/// underscore, plus (conservatively) every non-ASCII byte, so a run never
/// gets split in the middle of a multi-byte UTF-8 character.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// A quiet, One Dark-derived palette: each kind gets its own hue family
/// (purple/gold/blue/cyan/red/green/orange) so two unrelated kinds never
/// compete for the same colour the way the previous pass's keyword and
/// constant (both orange) or property and variable (both blue) did. This
/// is the same family the `cutoff-studio` reference leans on for its own
/// `--syn-*` tokens — not copied line for line, since that's CSS for a
/// web layout and this is GPUI, but the same idea: keywords read as
/// purple, types/classes as gold, calls as blue, namespaces/attributes
/// as cyan, member access as a warm red, literals as green/orange, and
/// everything structural (operators, punctuation) recedes to a dim
/// blue-gray instead of competing with any of it.
fn highlight_color(kind: HighlightKind) -> Rgba {
    match kind {
        HighlightKind::Keyword => rgb(0xc678dd),
        HighlightKind::Type => rgb(0xe5c07b),
        HighlightKind::BuiltinType => rgb(0x56b6c2),
        HighlightKind::Function => rgb(0x61afef),
        HighlightKind::Attribute => rgb(0x56b6c2),
        HighlightKind::Property => rgb(0xe06c75),
        HighlightKind::String => rgb(0x98c379),
        HighlightKind::Number | HighlightKind::Constant => rgb(0xd19a66),
        HighlightKind::Comment => rgb(0x6c7686),
        HighlightKind::Variable => rgb(0xc8ccd4),
        HighlightKind::Operator | HighlightKind::Punctuation | HighlightKind::Other => rgb(0x9aa3b2),
    }
}

#[cfg(test)]
mod tests {
    use super::diff_as_edit;

    #[test]
    fn no_change_is_no_edit() {
        assert!(diff_as_edit("same", "same").is_none());
    }

    #[test]
    fn pure_insertion_in_the_middle() {
        let edit = diff_as_edit("ab", "aXb").unwrap();
        assert_eq!(edit.start_byte, 1);
        assert_eq!(edit.old_end_byte, 1);
        assert_eq!(edit.new_end_byte, 2);
    }

    #[test]
    fn pure_deletion_in_the_middle() {
        let edit = diff_as_edit("aXb", "ab").unwrap();
        assert_eq!(edit.start_byte, 1);
        assert_eq!(edit.old_end_byte, 2);
        assert_eq!(edit.new_end_byte, 1);
    }

    #[test]
    fn full_replacement_shares_no_prefix_or_suffix() {
        let edit = diff_as_edit("hello", "goodbye").unwrap();
        assert_eq!(edit.start_byte, 0);
        assert_eq!(edit.old_end_byte, "hello".len());
        assert_eq!(edit.new_end_byte, "goodbye".len());
    }

    #[test]
    fn whole_file_replace_still_produces_one_edit() {
        let old = "line one\nline two";
        let new = "totally different file\ncontent here";
        let edit = diff_as_edit(old, new).unwrap();
        assert_eq!(edit.start_byte, 0);
        assert_eq!(edit.old_end_byte, old.len());
        assert_eq!(edit.new_end_byte, new.len());
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<EditorEvent> for EditorView {}

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Only touch the full buffer string or re-run tree-sitter when an edit
        // actually incremented `buffer.revision()`. Hovering, scrolling, or
        // moving the cursor does zero full-buffer work.
        if self.buffer.revision() != self.synced_revision {
            self.synced_revision = self.buffer.revision();
            self.indent_width = self.buffer.detect_indent_width();
            if !self.search_query.is_empty() {
                self.recompute_search_matches();
            }

            if should_parse_syntax(&self.buffer) {
                let text = self.buffer.text();
                if let Some(tree) = &mut self.syntax_tree {
                    if let Some(edit) = diff_as_edit(&self.synced_text, &text) {
                        tree.reparse(&text, edit);
                    }
                } else {
                    self.syntax_tree = Some(SyntaxTree::parse(&text));
                }
                self.closing_labels = Self::compute_closing_widget_labels(&text);
                self.sync_lsp_text(&text);
                self.synced_text = text;
            } else {
                self.syntax_tree = None;
                self.closing_labels.clear();
                self.synced_text.clear();
            }
        }

        let primary = self.buffer.selections().primary();
        let head = primary.head;
        let cursor_line = Some(self.buffer.byte_to_line(head));
        let selection_range = (!primary.is_empty()).then(|| primary.range());

        let current_word = if selection_range.is_none() && self.search_query.is_empty() {
            let line_idx = self.buffer.byte_to_line(head);
            let line_start = self.buffer.line_to_byte(line_idx);
            let line_text = self.buffer.line(line_idx);
            let display = line_text.trim_end_matches(['\n', '\r']);
            let col = (head - line_start).min(display.len());
            Self::word_at(display, col).map(str::to_string)
        } else {
            None
        };

        let diagnostics_by_line = self.diagnostics_by_line();
        let diagnostic_messages = self.diagnostic_messages_by_line();
        let indent_width = self.indent_width;
        let bracket_pair = self.syntax_tree.as_ref().and_then(|tree| {
            tree.matching_bracket(&self.synced_text, head)
                .map(|other| (head, other))
                .or_else(|| {
                    (head > 0).then(|| {
                        tree.matching_bracket(&self.synced_text, head - 1)
                            .map(|other| (head - 1, other))
                    })?
                })
        });

        let line_count = self.buffer.line_count().max(1);
        let list = uniform_list(
            "editor-lines",
            line_count,
            cx.processor(move |this, visible_range: Range<usize>, _window, cx| {
                if visible_range.is_empty() {
                    return Vec::new();
                }
                let start_line = visible_range.start;
                let end_line = visible_range.end.min(this.buffer.line_count());
                let start_byte = this.buffer.line_to_byte(start_line);
                let end_byte = if end_line < this.buffer.line_count() {
                    this.buffer.line_to_byte(end_line)
                } else {
                    this.buffer.len_bytes()
                };

                let highlights = this
                    .syntax_tree
                    .as_ref()
                    .map(|tree| {
                        let clamped_start = start_byte.min(this.synced_text.len());
                        let clamped_end = end_byte.min(this.synced_text.len()).max(clamped_start);
                        tree.highlights_in_range(&this.synced_text, clamped_start..clamped_end)
                    })
                    .unwrap_or_default();

                visible_range
                    .map(|ix| {
                        let line = if this.buffer.is_empty() && ix == 0 {
                            String::new()
                        } else if ix < this.buffer.line_count() {
                            this.buffer.line(ix)
                        } else {
                            String::new()
                        };
                        this.render_line(
                            ix,
                            &line,
                            cursor_line,
                            &highlights,
                            selection_range.clone(),
                            &diagnostics_by_line,
                            &diagnostic_messages,
                            &this.line_changes,
                            indent_width,
                            current_word.as_deref(),
                            bracket_pair,
                            cx,
                        )
                        .into_any_element()
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.scroll_handle)
        .size_full();

        div()
            .id("editor-view")
            .key_context("Editor")
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event, window, cx| {
                    window.focus(&this.focus_handle, cx);
                }),
            )
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .pt_2()
            .child(list)
    }
}
