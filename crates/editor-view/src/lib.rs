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
//! - Only the keyboard (shift+movement, cmd/ctrl+A) can create a
//!   multi-character selection; there's no mouse drag-select yet.
//! - Only the primary selection gets a visual highlight; a second
//!   multi-cursor selection (if one were added) wouldn't show one.
//! - Mouse clicks move the cursor to the start of the clicked line, not the
//!   clicked column (needs real text measurement).
//! - Every line is rendered on every frame; no virtualization for huge
//!   files, and syntax highlighting fully re-parses the buffer on every
//!   render rather than using `core_syntax::SyntaxTree::reparse`'s
//!   incremental path.

use std::io;
use std::ops::Range;
use std::path::Path;

use core_buffer::{Buffer, Selection};
use core_syntax::{Highlight, HighlightKind, SyntaxTree};
use gpui_kit::*;

pub struct EditorView {
    buffer: Buffer,
    focus_handle: FocusHandle,
}

impl EditorView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::from_buffer(Buffer::new(), cx)
    }

    pub fn from_buffer(buffer: Buffer, cx: &mut Context<Self>) -> Self {
        Self {
            buffer,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// Replace the buffer with the contents of `path`, discarding undo
    /// history and resetting the cursor to the start of the file.
    pub fn open_file(&mut self, path: impl AsRef<Path>, cx: &mut Context<Self>) -> io::Result<()> {
        self.buffer = Buffer::open(path)?;
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
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let gutter = div()
            .w(px(48.))
            .flex_shrink_0()
            .text_color(rgb(0x6b7280))
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

        let cursor_el = || div().w(px(2.)).h(px(18.)).bg(rgb(0xffffff));

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

        div()
            .id(("editor-line", index))
            .flex()
            .w_full()
            .font_family("monospace")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event, _window, cx| {
                    this.set_cursor(line_start, cx);
                }),
            )
            .child(gutter)
            .child(content)
    }
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

impl Focusable for EditorView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

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

        // Full reparse + highlight query on every render. Fine for the file
        // sizes M1 targets; see NOTES.md for the incremental-reparse gap.
        let syntax_tree = SyntaxTree::parse(&text);
        let highlights = syntax_tree.highlights(&text);

        let rendered_lines: Vec<_> = lines
            .iter()
            .enumerate()
            .map(|(ix, line)| {
                self.render_line(ix, line, cursor_line, &highlights, selection_range.clone(), cx)
                    .into_any_element()
            })
            .collect();

        div()
            .id("editor-view")
            .key_context("Editor")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .overflow_hidden()
            .bg(rgb(0x1e1e1e))
            .text_color(rgb(0xd4d4d4))
            .flex()
            .flex_col()
            .children(rendered_lines)
    }
}
