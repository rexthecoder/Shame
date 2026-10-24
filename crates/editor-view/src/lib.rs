//! Own GPUI editor element: cursor, selection, scroll, gutter, text drawing (M1).
//!
//! Built only on raw `gpui-kit` primitives (`gpui_kit::*`), never on
//! `gpui_kit::component`'s pre-built `Editor`/`Textarea` widgets — those
//! are read as a reference (see `NOTES.md` at the repo root), not a
//! dependency. `core-buffer` and `core-syntax` are the other two
//! dependencies, kept behind this crate's own API so the UI layer can
//! change without touching either of them.
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
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use lsp_integration::LspState;

const GUTTER_WIDTH: f32 = 48.;
const LINE_FONT_SIZE: f32 = 14.;

/// Emitted when this editor wants something handled above it — so far,
/// just "go to definition landed in a different file." The `app` crate
/// subscribes to this per tab; `editor-view` has no idea tabs exist.
#[derive(Debug, Clone)]
pub enum EditorEvent {
    OpenAt { path: PathBuf, line: u32, column: u32 },
}

/// The exact byte column within `text` closest to `x` pixels from the
/// start of the text area, found with GPUI's own text shaper rather than
/// approximated — correct for proportional glyph widths too, even though
/// this editor only uses a monospace font today.
fn column_for_x(window: &mut Window, text: &SharedString, x: Pixels) -> usize {
    let run = TextRun {
        len: text.len(),
        font: font("Menlo"),
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

    /// Highlight spans and the selection intersecting `line_text`'s byte
    /// range, run-length encoded into `(byte_range_within_line, color,
    /// is_selected)` runs. A `None` color run inherits the editor's base
    /// text color.
    fn line_runs(
        line_start: usize,
        line_text: &str,
        highlights: &[Highlight],
        selection: Option<Range<usize>>,
    ) -> Vec<(Range<usize>, Option<HighlightKind>, bool)> {
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

        let mut runs = Vec::new();
        let mut i = 0;
        while i < colors.len() {
            let key = (colors[i], selected[i]);
            let start = i;
            while i < colors.len() && (colors[i], selected[i]) == key {
                i += 1;
            }
            runs.push((start..i, key.0, key.1));
        }
        if runs.is_empty() {
            runs.push((0..0, None, false));
        }
        runs
    }

    fn render_line(
        &self,
        index: usize,
        line_text: &str,
        cursor_line: Option<usize>,
        highlights: &[Highlight],
        selection: Option<Range<usize>>,
        diagnostics_by_line: &std::collections::HashMap<u32, lsp_types::DiagnosticSeverity>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let gutter_color = match diagnostics_by_line.get(&(index as u32)) {
            Some(&sev) if sev == lsp_types::DiagnosticSeverity::ERROR => rgb(0xf14c4c),
            Some(&sev) if sev == lsp_types::DiagnosticSeverity::WARNING => rgb(0xcca700),
            _ => rgb(0x6b7280),
        };
        let gutter = div()
            .w(px(GUTTER_WIDTH))
            .flex_shrink_0()
            .text_color(gutter_color)
            .text_right()
            .pr_2()
            .child((index + 1).to_string());

        let line_start = self.buffer.line_to_byte(index);
        let display_text = line_text.trim_end_matches(['\n', '\r']);
        let cursor_col = (cursor_line == Some(index)).then(|| {
            let head = self.buffer.selections().primary().head;
            (head - line_start).min(display_text.len())
        });

        let span = |text: &str, kind: Option<HighlightKind>, selected: bool| {
            let mut el = div();
            if let Some(kind) = kind {
                el = el.text_color(highlight_color(kind));
            }
            if selected {
                el = el.bg(rgb(0x264f78));
            }
            el.child(text.to_string())
        };

        let cursor_el = || div().w(px(2.)).h(px(16.)).bg(rgb(0xffffff));

        let mut content = div().flex().items_center();
        let mut cursor_inserted = false;
        for (range, kind, selected) in Self::line_runs(line_start, display_text, highlights, selection.clone()) {
            if let Some(col) = cursor_col {
                if !cursor_inserted && col >= range.start && col < range.end {
                    content = content
                        .child(span(&display_text[range.start..col], kind, selected))
                        .child(cursor_el())
                        .child(span(&display_text[col..range.end], kind, selected));
                    cursor_inserted = true;
                    continue;
                }
            }
            content = content.child(span(&display_text[range], kind, selected));
        }
        if cursor_col.is_some() && !cursor_inserted {
            content = content.child(cursor_el());
        }

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
            .flex()
            .w_full()
            .h(px(20.))
            .flex_shrink_0()
            .font_family("Menlo")
            .text_size(px(LINE_FONT_SIZE))
            .child(gutter)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .h_full()
                    .child(content)
                    .child(click_overlay)
                    .when_some(
                        self.hover_to_render().filter(|(line, _)| *line == index),
                        |el, (_, text)| {
                            el.child(
                                div()
                                    .absolute()
                                    .top(px(20.))
                                    .left_0()
                                    .max_w(px(480.))
                                    .p_2()
                                    .rounded(px(4.))
                                    .bg(rgb(0x252526))
                                    .border_1()
                                    .border_color(rgb(0x454545))
                                    .text_size(px(12.))
                                    .text_color(rgb(0xd4d4d4))
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

fn highlight_color(kind: HighlightKind) -> Rgba {
    match kind {
        HighlightKind::Keyword => rgb(0xc586c0),
        HighlightKind::String => rgb(0xce9178),
        HighlightKind::Comment => rgb(0x6a9955),
        HighlightKind::Number => rgb(0xb5cea8),
        HighlightKind::Function => rgb(0xdcdcaa),
        HighlightKind::Type => rgb(0x4ec9b0),
        HighlightKind::Constant => rgb(0x569cd6),
        HighlightKind::Property => rgb(0x9cdcfe),
        HighlightKind::Attribute => rgb(0x4ec9b0),
        HighlightKind::Variable | HighlightKind::Operator | HighlightKind::Punctuation | HighlightKind::Other => {
            rgb(0xd4d4d4)
        }
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
                    cx,
                )
                    .into_any_element()
            })
            .collect();

        div()
            .id("editor-view")
            .key_context("Editor")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .bg(rgb(0x1e1e1e))
            .text_color(rgb(0xd4d4d4))
            .flex()
            .flex_col()
            .children(rendered_lines)
    }
}
