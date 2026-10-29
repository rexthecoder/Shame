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
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use lsp_integration::LspState;

/// Second pass on this editor's look. The first pass piled on chrome —
/// a bordered gutter column with its own background tint, a left-edge
/// accent bar on the active line, an underline under diagnostic text —
/// and the result fought itself instead of reading as one surface. This
/// pass goes the other way: fewer elements, each one doing one clear job,
/// which is the actual throughline in the `cutoff-studio` reference this
/// crate is meant to be learning from ("a flat, quiet ... layout") rather
/// than any specific color or border it draws.
const GUTTER_WIDTH: f32 = 48.;
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
}

/// The exact byte column within `text` closest to `x` pixels from the
/// start of the text area, found with GPUI's own text shaper rather than
/// approximated — correct for proportional glyph widths too, even though
/// this editor only uses a monospace font today.
fn column_for_x(window: &mut Window, text: &SharedString, x: Pixels) -> usize {
    let run = TextRun {
        len: text.len(),
        font: font(FONT_FAMILY),
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let shaped = window
        .text_system()
        .shape_line(text.clone(), px(LINE_FONT_SIZE), &[run], None);
    shaped.closest_index_for_x(x.max(px(0.)))
}

pub struct EditorView {
    buffer: Buffer,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    syntax_tree: SyntaxTree,
    /// The text `syntax_tree` was last parsed/reparsed from, so `render`
    /// can diff against the buffer's current text to build a single
    /// [`InputEdit`] rather than reparsing from scratch every frame.
    synced_text: String,
    /// `dart language-server` state for this buffer, if it's a `.dart`
    /// file. See `lsp_integration.rs`.
    lsp: Option<LspState>,
    /// `(line, column)` the mouse is currently hovering, for debouncing
    /// `textDocument/hover` requests. See `lsp_integration.rs`.
    hover_point: Option<(usize, usize)>,
    hover_sequence: u64,
    hover_result: Option<(usize, String)>,
}

impl EditorView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::from_buffer(Buffer::new(), cx)
    }

    pub fn from_buffer(buffer: Buffer, cx: &mut Context<Self>) -> Self {
        let text = buffer.text();
        let syntax_tree = SyntaxTree::parse(&text);
        let mut this = Self {
            buffer,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            syntax_tree,
            synced_text: text,
            lsp: None,
            hover_point: None,
            hover_sequence: 0,
            hover_result: None,
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
        let text = self.buffer.text();
        self.syntax_tree = SyntaxTree::parse(&text);
        self.synced_text = text;
        self.lsp = None;
        self.maybe_start_lsp(cx);
        cx.notify();
        Ok(())
    }

    fn save(&mut self) {
        let _ = self.buffer.save();
    }

    fn set_cursor(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.buffer
            .selections_mut()
            .set_primary(Selection::cursor(offset));
        cx.notify();
    }

    /// Move the cursor to a 0-indexed (line, column) — e.g. where a
    /// go-to-definition in a newly-opened file landed. `column` is
    /// treated as a byte offset within the line, the same approximation
    /// used everywhere else in this crate.
    pub fn set_cursor_to_position(&mut self, line: u32, column: u32, cx: &mut Context<Self>) {
        let offset = self.buffer.line_to_byte(line as usize) + column as usize;
        self.set_cursor(offset, cx);
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
    }

    fn insert_at_cursor(&mut self, text: &str, cx: &mut Context<Self>) {
        let primary = self.buffer.selections().primary();
        let range = primary.range();
        let insert_at = range.start;
        self.buffer.edit(range, text);
        self.set_cursor(insert_at + text.len(), cx);
    }

    fn backspace(&mut self, cx: &mut Context<Self>) {
        let primary = self.buffer.selections().primary();
        if !primary.is_empty() {
            let start = primary.start();
            self.buffer.remove(primary.range());
            self.set_cursor(start, cx);
            return;
        }
        let start = self.buffer.prev_char_boundary(primary.head);
        if start == primary.head {
            return;
        }
        self.buffer.remove(start..primary.head);
        self.set_cursor(start, cx);
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

        if primary_mod {
            match keystroke.key.as_str() {
                "c" => self.copy(cx),
                "x" => self.cut(cx),
                "v" => self.paste(cx),
                "a" => self.select_all(cx),
                "s" => {
                    self.save();
                    cx.notify();
                }
                "f" if keystroke.modifiers.shift => {
                    self.request_format();
                }
                "r" => self.start_rename(cx),
                "." => self.request_code_actions(),
                "z" => {
                    if keystroke.modifiers.shift {
                        self.buffer.redo();
                    } else {
                        self.buffer.undo();
                    }
                    cx.notify();
                }
                _ => {}
            }
            return;
        }

        let extend = keystroke.modifiers.shift;
        match keystroke.key.as_str() {
            "f2" => self.start_rename(cx),
            "backspace" => self.backspace(cx),
            "delete" => self.delete_forward(cx),
            "enter" => self.insert_at_cursor("\n", cx),
            "tab" => self.insert_at_cursor("    ", cx),
            "left" => self.move_left(extend, cx),
            "right" => self.move_right(extend, cx),
            "up" => self.move_vertical(-1, extend, cx),
            "down" => self.move_vertical(1, extend, cx),
            "home" => self.move_home(extend, cx),
            "end" => self.move_end(extend, cx),
            _ => {
                if let Some(text) = &keystroke.key_char {
                    self.insert_at_cursor(text, cx);
                }
            }
        }
    }

    /// Highlight spans, the selection, and any word-occurrence matches
    /// intersecting `line_text`'s byte range, run-length encoded into
    /// `(byte_range_within_line, color, is_selected, is_word_match)`
    /// runs. A `None` color run inherits the editor's base text color.
    fn line_runs(
        line_start: usize,
        line_text: &str,
        highlights: &[Highlight],
        selection: Option<Range<usize>>,
        word_matches: &[Range<usize>],
    ) -> Vec<(Range<usize>, Option<HighlightKind>, bool, bool)> {
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

        let mut selected = vec![false; line_text.len()];
        if let Some(sel) = selection {
            if sel.end > line_start && sel.start < line_end {
                let start = sel.start.max(line_start) - line_start;
                let end = sel.end.min(line_end) - line_start;
                for slot in &mut selected[start..end] {
                    *slot = true;
                }
            }
        }

        let mut word_hit = vec![false; line_text.len()];
        for m in word_matches {
            if m.end <= line_start || m.start >= line_end {
                continue;
            }
            let start = m.start.max(line_start) - line_start;
            let end = m.end.min(line_end) - line_start;
            for slot in &mut word_hit[start..end] {
                *slot = true;
            }
        }

        let mut runs = Vec::new();
        let mut i = 0;
        while i < colors.len() {
            let key = (colors[i], selected[i], word_hit[i]);
            let start = i;
            while i < colors.len() && (colors[i], selected[i], word_hit[i]) == key {
                i += 1;
            }
            runs.push((start..i, key.0, key.1, key.2));
        }
        if runs.is_empty() {
            runs.push((0..0, None, false, false));
        }
        runs
    }

    /// The word (identifier-ish run of alphanumerics/underscores)
    /// touching byte column `col` in `text`, preferring the word to the
    /// right of the cursor but falling back to the one just behind it —
    /// the same convention every editor uses for "what am I pointing at"
    /// when the cursor sits exactly between two characters.
    fn word_at(text: &str, col: usize) -> Option<&str> {
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
        Some(&text[start..end])
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
        line_changes: &std::collections::HashMap<u32, LineChange>,
        indent_width: usize,
        current_word: Option<&str>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_active_line = cursor_line == Some(index);
        let diagnostic_severity = diagnostics_by_line.get(&(index as u32)).copied();
        // A warm amber rather than a plain gray — line numbers get their
        // own identity instead of borrowing the theme's generic muted
        // color, the same way the `cutoff-studio`/reference editors both
        // give the gutter a distinct hue instead of flattening it into
        // "just dim text".
        let gutter_color: Hsla = match diagnostic_severity {
            Some(sev) if sev == lsp_types::DiagnosticSeverity::ERROR => cx.theme().danger,
            Some(sev) if sev == lsp_types::DiagnosticSeverity::WARNING => cx.theme().warning,
            _ if is_active_line => cx.theme().foreground.opacity(0.8),
            _ => cx.theme().muted_foreground.opacity(0.55),
        };
        let gutter = h_flex()
            .w(px(GUTTER_WIDTH))
            .h_full()
            .flex_shrink_0()
            .items_center()
            .text_size(px(GUTTER_FONT_SIZE))
            .text_color(gutter_color)
            .child(
                div()
                    .w(px(30.))
                    .text_right()
                    .child((index + 1).to_string()),
            )
            .child(
                div()
                    .w(px(GUTTER_WIDTH - 30.))
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
                            .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                                view.update(cx, |this, _cx| this.request_code_actions());
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

        // A translucent selection wash, not the theme's fully opaque
        // selection colour — the code underneath stays readable while the
        // selection is still unmistakable.
        let selection_bg = cx.theme().selection.opacity(0.35);
        // Every other occurrence of the word the cursor is sitting in,
        // picked out with a flat tint — the "where else does this name
        // show up" cue every mature editor gives for free, and one of
        // the first things a code editor gets asked why it's missing.
        let word_match_bg = cx.theme().blue.opacity(0.14);
        let is_doc_line = display_text.trim_start().starts_with("///");
        let span = move |text: &str, kind: Option<HighlightKind>, selected: bool, word_match: bool| {
            let mut el = div();
            if let Some(kind) = kind {
                el = el.text_color(highlight_color(kind));
                if kind == HighlightKind::Comment || (is_doc_line && kind == HighlightKind::Attribute) {
                    el = el.italic();
                }
            }
            if selected {
                el = el.bg(selection_bg);
            } else if word_match {
                el = el.bg(word_match_bg);
            }
            el.child(text.to_string())
        };

        let caret_color = cx.theme().caret;
        let cursor_el = move || {
            div()
                .w(px(1.5))
                .h(px(LINE_FONT_SIZE + 2.))
                .bg(caret_color)
                .rounded(px(0.75))
        };

        let word_matches: Vec<Range<usize>> = current_word
            .map(|word| {
                Self::word_occurrences(display_text, word)
                    .into_iter()
                    .map(|r| (r.start + line_start)..(r.end + line_start))
                    .collect()
            })
            .unwrap_or_default();

        let mut content = div().flex().items_center();
        let mut cursor_inserted = false;
        for (range, kind, selected, word_match) in
            Self::line_runs(line_start, display_text, highlights, selection.clone(), &word_matches)
        {
            if let Some(col) = cursor_col {
                if !cursor_inserted && col >= range.start && col < range.end {
                    content = content
                        .child(span(&display_text[range.start..col], kind, selected, word_match))
                        .child(cursor_el())
                        .child(span(&display_text[col..range.end], kind, selected, word_match));
                    cursor_inserted = true;
                    continue;
                }
            }
            content = content.child(span(&display_text[range], kind, selected, word_match));
        }
        if cursor_col.is_some() && !cursor_inserted {
            content = content.child(cursor_el());
        }

        // Faint vertical indent guides, drawn at multiples of the file's
        // own indent width up to this line's leading spaces — the standard
        // editor treatment, and correct for both 2-space Dart and 4-space
        // Rust files.
        let leading_spaces = display_text.as_bytes().iter().take_while(|&&b| b == b' ').count();
        let guides: Vec<_> = (indent_width..leading_spaces)
            .step_by(indent_width)
            .take(16)
            .map(|column| {
                // This font's advance width is 0.6 em, so one space column
                // sits `0.6 * font_size` from the text area's left edge.
                // Dimmed to a third of the border color — a guide you
                // only notice when you look for it, not a grid line.
                div()
                    .absolute()
                    .top(px(3.))
                    .bottom(px(3.))
                    .left(px(LINE_FONT_SIZE * 0.6 * column as f32))
                    .w(px(1.))
                    .bg(cx.theme().border.opacity(0.35))
                    .into_any_element()
            })
            .collect();

        // A transparent overlay the exact size of the text area, used only
        // to turn a click's pixel x-coordinate into an exact column via
        // real text shaping (`shape_line`/`closest_index_for_x`) — the
        // visible text above is still rendered as plain div/span children,
        // not painted by this canvas.
        let hit_test_text: SharedString = display_text.to_string().into();
        let view = cx.entity();
        let click_overlay = canvas(
            |_bounds, _window, _cx| (),
            move |bounds, (), window, _cx| {
                let text = hit_test_text.clone();
                let down_view = view.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble
                        || event.button != MouseButton::Left
                        || !bounds.contains(&event.position)
                    {
                        return;
                    }
                    let column = column_for_x(window, &text, event.position.x - bounds.origin.x);
                    let go_to_definition = event.modifiers.platform || event.modifiers.control;
                    down_view.update(cx, |this, cx| {
                        if go_to_definition {
                            this.request_definition(index, column);
                        } else {
                            this.set_cursor(line_start + column, cx);
                        }
                    });
                });

                let text = hit_test_text.clone();
                let move_view = view.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble
                        || event.pressed_button != Some(MouseButton::Left)
                        || !bounds.contains(&event.position)
                    {
                        return;
                    }
                    let column = column_for_x(window, &text, event.position.x - bounds.origin.x);
                    move_view.update(cx, |this, cx| {
                        this.extend_to(line_start + column, cx);
                    });
                });

                let text = hit_test_text.clone();
                let hover_view = view.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble
                        || event.pressed_button.is_some()
                        || !bounds.contains(&event.position)
                    {
                        return;
                    }
                    let column = column_for_x(window, &text, event.position.x - bounds.origin.x);
                    hover_view.update(cx, |this, cx| {
                        this.note_hover_position(index, column, cx);
                    });
                });
            },
        )
        .absolute()
        .inset_0();

        div()
            .id(("editor-line", index))
            .relative()
            .flex()
            .w_full()
            .h(px(LINE_HEIGHT))
            .flex_shrink_0()
            .font_family(FONT_FAMILY)
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
                    .children(guides)
                    .child(content)
                    .child(click_overlay)
                    .when_some(
                        self.hover_to_render().filter(|(line, _)| *line == index),
                        |el, (_, text)| {
                            el.child(
                                div()
                                    .absolute()
                                    .top(px(LINE_HEIGHT))
                                    .left_0()
                                    .max_w(px(480.))
                                    .p_2()
                                    .rounded(cx.theme().radius)
                                    .shadow_lg()
                                    .bg(cx.theme().popover)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .text_size(px(12.))
                                    .text_color(cx.theme().popover_foreground)
                                    .child(text.to_string()),
                            )
                        },
                    ),
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

/// The file's own indentation width — the smallest positive leading-space
/// count, clamped to a sane range. Guides are drawn at multiples of it,
/// which keeps a 2-space Dart file and a 4-space Rust file correct without
/// letting an oddly aligned continuation line invent a new guide column.
fn indent_width(lines: &[String]) -> usize {
    lines
        .iter()
        .filter_map(|line| {
            let trimmed = line.trim_start_matches(' ');
            if trimmed.is_empty() {
                return None;
            }
            let indent = line.len() - trimmed.len();
            (indent > 0).then_some(indent)
        })
        .min()
        .unwrap_or(4)
        .clamp(2, 8)
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
        let text = self.buffer.text();
        let primary = self.buffer.selections().primary();
        let head = primary.head;
        let cursor_line = Some(self.buffer.byte_to_line(head));
        let selection_range = (!primary.is_empty()).then(|| primary.range());

        // The word under a bare caret (no active selection) gets every
        // other occurrence of itself picked out across the whole file —
        // computed once here, up front, so every line below just asks
        // "does this range fall inside a match" instead of each
        // reaching back into the buffer separately.
        let current_word = if selection_range.is_none() {
            let line_idx = self.buffer.byte_to_line(head);
            let line_start = self.buffer.line_to_byte(line_idx);
            let line_text = self.buffer.line(line_idx);
            let display = line_text.trim_end_matches(['\n', '\r']);
            let col = (head - line_start).min(display.len());
            Self::word_at(display, col).map(str::to_string)
        } else {
            None
        };

        let lines: Vec<String> = if text.is_empty() {
            vec![String::new()]
        } else {
            let mut v = Vec::with_capacity(self.buffer.line_count());
            for i in 0..self.buffer.line_count() {
                v.push(self.buffer.line(i));
            }
            v
        };

        // Incrementally reparse: diff the text we last parsed against the
        // buffer's current text (common-prefix/common-suffix trim) to get
        // a single InputEdit, rather than reparsing from scratch every
        // frame. Whatever changed between renders — one keystroke or a
        // whole-file open — collapses to one edit either way.
        if text != self.synced_text {
            if let Some(edit) = diff_as_edit(&self.synced_text, &text) {
                self.syntax_tree.reparse(&text, edit);
            } else {
                self.syntax_tree = SyntaxTree::parse(&text);
            }
            self.sync_lsp_text(&text);
            self.synced_text = text.clone();
        }
        let highlights = self.syntax_tree.highlights(&text);
        let diagnostics_by_line = self.diagnostics_by_line();
        let line_changes = self.buffer.path().map(core_workspace::line_changes).unwrap_or_default();
        let indent_width = indent_width(&lines);

        let rendered_lines: Vec<_> = lines
            .iter()
            .enumerate()
            .map(|(ix, line)| {
                self.render_line(
                    ix,
                    line,
                    cursor_line,
                    &highlights,
                    selection_range.clone(),
                    &diagnostics_by_line,
                    &line_changes,
                    indent_width,
                    current_word.as_deref(),
                    cx,
                )
                    .into_any_element()
            })
            .collect();

        let rows = div()
            .relative()
            .flex()
            .flex_col()
            .children(rendered_lines);

        div()
            .id("editor-view")
            .key_context("Editor")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // A little breathing room above the first line and below the
            // last, so code never sits flush against the tab bar or the
            // status bar.
            .pt_3()
            .pb_8()
            .child(rows)
    }
}
