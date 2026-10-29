use std::fs::File;
use std::io::{self, BufReader, BufWriter};
use std::ops::Range;
use std::path::{Path, PathBuf};

use ropey::{LineType, Rope};

use crate::selection::SelectionSet;
use crate::undo::{EditOp, UndoTree};

/// Line-ending convention used for every line-based query below: lines end
/// at `\n`, with an optional preceding `\r` treated as part of the ending.
const LINES: LineType = LineType::LF_CR;

/// A rope-backed text buffer with selections and an undo tree.
///
/// All offsets (`insert`, `remove`, `edit`, [`Selection`] fields) are byte
/// offsets into the buffer's UTF-8 text. Char- and UTF-16-based positions
/// (needed once `core-lsp` speaks the Language Server Protocol) are a
/// conversion layer to add later, not something this crate needs yet.
pub struct Buffer {
    rope: Rope,
    selections: SelectionSet,
    undo_tree: UndoTree,
    path: Option<PathBuf>,
    dirty: bool,
    revision: u64,
}

impl Default for Buffer {
    fn default() -> Self {
        Self {
            rope: Rope::new(),
            selections: SelectionSet::default(),
            undo_tree: UndoTree::default(),
            path: None,
            dirty: false,
            revision: 0,
        }
    }
}

impl Buffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_str(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
            ..Self::default()
        }
    }

    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let rope = Rope::from_reader(BufReader::new(File::open(path)?))?;
        Ok(Self {
            rope,
            path: Some(path.to_path_buf()),
            ..Self::default()
        })
    }

    pub fn save(&mut self) -> io::Result<()> {
        let path = self.path.clone().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "buffer has no associated path; call save_as instead",
            )
        })?;
        self.save_as(path)
    }

    pub fn save_as(&mut self, path: impl Into<PathBuf>) -> io::Result<()> {
        let path = path.into();
        self.rope.write_to(BufWriter::new(File::create(&path)?))?;
        self.path = Some(path);
        self.dirty = false;
        Ok(())
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn len_bytes(&self) -> usize {
        self.rope.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rope.len() == 0
    }

    /// Detect the file's indentation width (clamped to `2..=8`) by inspecting
    /// up to the first 100 lines, avoiding a full-buffer allocation.
    pub fn detect_indent_width(&self) -> usize {
        let limit = self.line_count().min(100);
        (0..limit)
            .filter_map(|i| {
                let line = self.line(i);
                let trimmed = line.trim_start_matches(' ');
                if trimmed.trim_end_matches(['\n', '\r']).is_empty() {
                    return None;
                }
                let indent = line.len() - trimmed.len();
                (indent > 0).then_some(indent)
            })
            .min()
            .unwrap_or(4)
            .clamp(2, 8)
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn slice(&self, range: Range<usize>) -> String {
        self.rope.slice(range).to_string()
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines(LINES)
    }

    /// The byte offset of the start of `line` (clamped to the last line).
    pub fn line_to_byte(&self, line: usize) -> usize {
        let line = line.min(self.line_count().saturating_sub(1));
        self.rope.line_to_byte_idx(line, LINES)
    }

    /// The line containing `byte` (clamped to the buffer's length).
    pub fn byte_to_line(&self, byte: usize) -> usize {
        self.rope.byte_to_line_idx(byte.min(self.len_bytes()), LINES)
    }

    /// The text of `line`, including its line ending if any.
    pub fn line(&self, line: usize) -> String {
        let start = self.line_to_byte(line);
        let end = if line + 1 < self.line_count() {
            self.line_to_byte(line + 1)
        } else {
            self.len_bytes()
        };
        self.slice(start..end)
    }

    /// The byte offset one full UTF-8 character before `byte` (clamped to 0).
    pub fn prev_char_boundary(&self, byte: usize) -> usize {
        let end = byte.min(self.len_bytes());
        if end == 0 {
            return 0;
        }
        let mut start = end.saturating_sub(4);
        while start > 0 && !self.rope.is_char_boundary(start) {
            start -= 1;
        }
        let window = self.slice(start..end);
        match window.chars().next_back() {
            Some(c) => end - c.len_utf8(),
            None => 0,
        }
    }

    /// The byte offset one full UTF-8 character after `byte` (clamped to the
    /// buffer's length).
    pub fn next_char_boundary(&self, byte: usize) -> usize {
        let len = self.len_bytes();
        let start = byte.min(len);
        if start >= len {
            return len;
        }
        let mut end = (start + 4).min(len);
        while end < len && !self.rope.is_char_boundary(end) {
            end += 1;
        }
        let window = self.slice(start..end);
        match window.chars().next() {
            Some(c) => start + c.len_utf8(),
            None => len,
        }
    }

    pub fn selections(&self) -> &SelectionSet {
        &self.selections
    }

    pub fn selections_mut(&mut self) -> &mut SelectionSet {
        &mut self.selections
    }

    /// Replace `range` (byte offsets) with `text`. Records the edit on the
    /// undo tree and shifts every selection to account for the size change.
    pub fn edit(&mut self, range: Range<usize>, text: &str) {
        let removed = self.rope.slice(range.clone()).to_string();
        let op = EditOp {
            start: range.start,
            removed,
            inserted: text.to_string(),
        };
        self.apply(&op);
        self.undo_tree.record(op);
    }

    pub fn insert(&mut self, at: usize, text: &str) {
        self.edit(at..at, text);
    }

    pub fn remove(&mut self, range: Range<usize>) {
        self.edit(range, "");
    }

    pub fn can_undo(&self) -> bool {
        self.undo_tree.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.undo_tree.can_redo()
    }

    pub fn undo(&mut self) -> bool {
        let Some(op) = self.undo_tree.undo() else {
            return false;
        };
        self.apply(&op);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(op) = self.undo_tree.redo() else {
            return false;
        };
        self.apply(&op);
        true
    }

    fn apply(&mut self, op: &EditOp) {
        self.rope.remove(op.start..op.start + op.removed.len());
        self.rope.insert(op.start, &op.inserted);
        self.selections
            .apply_edit(op.start, op.removed.len(), op.inserted.len());
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::Selection;

    #[test]
    fn insert_and_remove() {
        let mut buf = Buffer::from_str("hello world");
        buf.insert(5, ",");
        assert_eq!(buf.text(), "hello, world");

        buf.remove(0..7);
        assert_eq!(buf.text(), "world");
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut buf = Buffer::from_str("abc");
        buf.insert(3, "def");
        assert_eq!(buf.text(), "abcdef");

        assert!(buf.undo());
        assert_eq!(buf.text(), "abc");
        assert!(!buf.can_undo());

        assert!(buf.redo());
        assert_eq!(buf.text(), "abcdef");
        assert!(!buf.can_redo());
    }

    #[test]
    fn multiple_edits_undo_in_reverse_order() {
        let mut buf = Buffer::from_str("");
        buf.insert(0, "a");
        buf.insert(1, "b");
        buf.insert(2, "c");
        assert_eq!(buf.text(), "abc");

        buf.undo();
        assert_eq!(buf.text(), "ab");
        buf.undo();
        assert_eq!(buf.text(), "a");
        buf.undo();
        assert_eq!(buf.text(), "");
        assert!(!buf.undo());
    }

    #[test]
    fn selections_shift_with_edits() {
        let mut buf = Buffer::from_str("hello world");
        buf.selections_mut().set_primary(Selection::cursor(6)); // before "world"
        buf.selections_mut().add(Selection::cursor(11)); // end of buffer

        buf.insert(0, ">> ");

        let selections = buf.selections().as_slice();
        assert_eq!(selections[0].start(), 6 + 3);
        assert_eq!(selections[1].start(), 11 + 3);
    }

    #[test]
    fn line_helpers() {
        let buf = Buffer::from_str("abc\ndef\nghi");
        assert_eq!(buf.line_count(), 3);
        assert_eq!(buf.line(0), "abc\n");
        assert_eq!(buf.line(1), "def\n");
        assert_eq!(buf.line(2), "ghi");

        assert_eq!(buf.line_to_byte(0), 0);
        assert_eq!(buf.line_to_byte(1), 4);
        assert_eq!(buf.line_to_byte(2), 8);

        assert_eq!(buf.byte_to_line(0), 0);
        assert_eq!(buf.byte_to_line(4), 1);
        assert_eq!(buf.byte_to_line(10), 2);
    }

    #[test]
    fn char_boundaries_skip_whole_utf8_characters() {
        let buf = Buffer::from_str("a😀b");
        // "a" (1 byte) + "😀" (4 bytes) + "b" (1 byte).
        let emoji_start = 1;
        let emoji_end = 5;

        assert_eq!(buf.next_char_boundary(emoji_start), emoji_end);
        assert_eq!(buf.prev_char_boundary(emoji_end), emoji_start);
    }

    #[test]
    fn save_and_reopen_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "core-buffer-test-{}-{}",
            std::process::id(),
            "save_and_reopen_round_trip"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("note.txt");

        let mut buf = Buffer::from_str("saved contents\n");
        buf.save_as(&path).unwrap();
        assert!(!buf.is_dirty());

        let reopened = Buffer::open(&path).unwrap();
        assert_eq!(reopened.text(), "saved contents\n");
        assert_eq!(reopened.path(), Some(path.as_path()));

        std::fs::remove_dir_all(&dir).ok();
    }
}
