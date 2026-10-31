//! Multi-cursor editing: ⌘D select-next-occurrence, ⌥⌘↑/↓ add cursor on
//! the adjacent line, and typing / backspace / delete applied to every
//! cursor at once.
//!
//! Edits from all cursors are merged into **one** buffer edit spanning the
//! first to the last cursor, so a multi-cursor edit is a single undo step.
//! Any single-cursor movement collapses the set back to one selection
//! (`EditorView::set_cursor` replaces the whole set).

use std::ops::Range;

use core_buffer::Selection;
use gpui_kit::Context;

use crate::EditorView;

/// Merge sorted, non-overlapping `(range, replacement)` edits against `text`
/// into one replaced `span` plus its new text, and return the byte offset
/// (in the new buffer) just after each replacement.
pub(crate) fn merge_edits(
    original: impl Fn(Range<usize>) -> String,
    edits: &[(Range<usize>, String)],
) -> Option<(Range<usize>, String, Vec<usize>)> {
    let first = edits.first()?;
    let last = edits.last()?;
    let span = first.0.start..last.0.end;
    let mut rebuilt = String::new();
    let mut cursors = Vec::with_capacity(edits.len());
    let mut at = span.start;
    for (range, replacement) in edits {
        rebuilt.push_str(&original(at..range.start));
        rebuilt.push_str(replacement);
        cursors.push(span.start + rebuilt.len());
        at = range.end;
    }
    Some((span, rebuilt, cursors))
}

impl EditorView {
    pub(crate) fn has_multiple_cursors(&self) -> bool {
        self.buffer.selections().len() > 1
    }

    /// Collapse to just the primary selection (Esc).
    pub(crate) fn collapse_selections(&mut self, cx: &mut Context<Self>) {
        if self.has_multiple_cursors() {
            let primary = self.buffer.selections().primary();
            self.buffer.selections_mut().set_primary(primary);
            cx.notify();
        }
    }

    /// ⌘D: with an empty cursor, select the word under it; otherwise add the
    /// next occurrence of the selected text as another selection (wrapping).
    pub(crate) fn select_next_occurrence(&mut self, cx: &mut Context<Self>) {
        let primary = self.buffer.selections().primary();
        if primary.is_empty() {
            let line = self.buffer.byte_to_line(primary.head);
            let line_start = self.buffer.line_to_byte(line);
            let text = self.buffer.line(line);
            let display = text.trim_end_matches(['\n', '\r']);
            let col = (primary.head - line_start).min(display.len());
            if let Some(word) = Self::word_range_at(display, col) {
                self.buffer
                    .selections_mut()
                    .set_primary(Selection::new(line_start + word.start, line_start + word.end));
                cx.notify();
            }
            return;
        }

        let needle = self.buffer.slice(primary.range());
        let text = self.buffer.text();
        let after = self
            .buffer
            .selections()
            .as_slice()
            .iter()
            .map(|s| s.end())
            .max()
            .unwrap_or(0);
        let existing: Vec<Range<usize>> =
            self.buffer.selections().as_slice().iter().map(|s| s.range()).collect();
        let found = text[after..]
            .find(&needle)
            .map(|i| after + i)
            .or_else(|| text.find(&needle))
            .filter(|&start| !existing.iter().any(|r| r.start == start));
        if let Some(start) = found {
            self.buffer
                .selections_mut()
                .add(Selection::new(start, start + needle.len()));
            self.ensure_cursor_visible();
            cx.notify();
        }
    }

    /// ⌥⌘↑ / ⌥⌘↓: add a cursor on the adjacent line at the same column.
    pub(crate) fn add_cursor_vertical(&mut self, delta: isize, cx: &mut Context<Self>) {
        let head = self.buffer.selections().primary().head;
        let line = self.buffer.byte_to_line(head);
        let col = head - self.buffer.line_to_byte(line);
        let target = line as isize + delta;
        if target < 0 || target as usize >= self.buffer.line_count() {
            return;
        }
        let target = target as usize;
        let target_text = self.buffer.line(target);
        let len = target_text.trim_end_matches(['\n', '\r']).len();
        let offset = self.buffer.line_to_byte(target) + col.min(len);
        self.buffer.selections_mut().add(Selection::cursor(offset));
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Apply per-cursor `edits` (sorted, non-overlapping) as one undo step
    /// and leave a cursor after each replacement.
    fn apply_multi_edits(&mut self, edits: Vec<(Range<usize>, String)>, cx: &mut Context<Self>) {
        let Some((span, rebuilt, cursors)) = merge_edits(|r| self.buffer.slice(r), &edits) else {
            return;
        };
        self.buffer.edit(span, &rebuilt);
        let mut cursors = cursors.into_iter();
        if let Some(first) = cursors.next() {
            self.buffer.selections_mut().set_primary(Selection::cursor(first));
        }
        for offset in cursors {
            self.buffer.selections_mut().add(Selection::cursor(offset));
        }
        self.ensure_cursor_visible();
        cx.notify();
    }

    pub(crate) fn multi_insert(&mut self, text: &str, cx: &mut Context<Self>) {
        let edits = self
            .buffer
            .selections()
            .as_slice()
            .iter()
            .map(|s| (s.range(), text.to_string()))
            .collect();
        self.apply_multi_edits(edits, cx);
    }

    pub(crate) fn multi_backspace(&mut self, cx: &mut Context<Self>) {
        let edits = self
            .buffer
            .selections()
            .as_slice()
            .iter()
            .map(|s| {
                let range = if s.is_empty() {
                    self.buffer.prev_char_boundary(s.head)..s.head
                } else {
                    s.range()
                };
                (range, String::new())
            })
            .collect();
        self.apply_multi_edits(edits, cx);
    }

    pub(crate) fn multi_delete_forward(&mut self, cx: &mut Context<Self>) {
        let edits = self
            .buffer
            .selections()
            .as_slice()
            .iter()
            .map(|s| {
                let range = if s.is_empty() {
                    s.head..self.buffer.next_char_boundary(s.head)
                } else {
                    s.range()
                };
                (range, String::new())
            })
            .collect();
        self.apply_multi_edits(edits, cx);
    }

    /// Cursor columns and selection ranges of the *non-primary* selections
    /// that fall on `line`, as byte offsets within the line, for painting.
    pub(crate) fn secondary_line_marks(
        &self,
        line_start: usize,
        line_len: usize,
    ) -> (Vec<usize>, Vec<Range<usize>>) {
        let selections = self.buffer.selections();
        if selections.len() < 2 {
            return (Vec::new(), Vec::new());
        }
        let primary = selections.primary();
        let line_end = line_start + line_len;
        let mut carets = Vec::new();
        let mut ranges = Vec::new();
        for s in selections.as_slice() {
            if *s == primary {
                continue;
            }
            if s.head >= line_start && s.head <= line_end {
                carets.push(s.head - line_start);
            }
            if !s.is_empty() && s.end() > line_start && s.start() < line_end {
                ranges.push(s.start().max(line_start) - line_start..s.end().min(line_end) - line_start);
            }
        }
        (carets, ranges)
    }
}

#[cfg(test)]
mod tests {
    use super::merge_edits;

    #[test]
    fn merges_cursor_inserts_into_one_span() {
        let text = "ab cd ef";
        let edits = vec![(1..1, "X".to_string()), (4..4, "X".to_string()), (7..7, "X".to_string())];
        let (span, rebuilt, cursors) =
            merge_edits(|r| text[r].to_string(), &edits).unwrap();
        assert_eq!(span, 1..7);
        let mut result = text.to_string();
        result.replace_range(span, &rebuilt);
        assert_eq!(result, "aXb cXd eXf");
        assert_eq!(cursors, vec![2, 6, 10]);
    }

    #[test]
    fn merges_deletions() {
        let text = "abcdef";
        let edits = vec![(1..2, String::new()), (4..5, String::new())];
        let (span, rebuilt, _) = merge_edits(|r| text[r].to_string(), &edits).unwrap();
        let mut result = text.to_string();
        result.replace_range(span, &rebuilt);
        assert_eq!(result, "acdf");
    }

    #[test]
    fn no_edits_is_none() {
        assert!(merge_edits(|_| String::new(), &[]).is_none());
    }
}
