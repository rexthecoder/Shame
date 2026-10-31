use std::fs::File;
use std::io::{self, BufReader, BufWriter};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ropey::{LineType, Rope};

use crate::selection::SelectionSet;
use crate::undo::{EditOp, UndoTree};

/// Line-ending convention used for every line-based query below: lines end
/// at `\n`, with an optional preceding `\r` treated as part of the ending.
const LINES: LineType = LineType::LF_CR;

/// What the file on disk looked like when this buffer last read or wrote it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DiskStamp {
    modified: Option<SystemTime>,
    len: u64,
}

fn stamp_of(path: &Path) -> Option<DiskStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(DiskStamp { modified: meta.modified().ok(), len: meta.len() })
}

/// How the file on disk compares with what the buffer last saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskState {
    /// Same as when the buffer last read or wrote it (or the buffer has no file).
    Unchanged,
    /// Changed by something else (another program, git, an agent).
    Modified,
    /// No longer exists.
    Deleted,
}

/// Write `rope` to `path` without ever leaving a half-written file: write a
/// temporary file next to it, then rename it over the target. Symlinks are
/// followed (the link stays a link) and the existing permissions are kept.
/// If the directory refuses a new file, fall back to writing in place.
fn write_atomic(path: &Path, rope: &Rope) -> io::Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.shame-tmp-{}", std::process::id()));

    let attempt = (|| -> io::Result<()> {
        let mut writer = BufWriter::new(File::create(&tmp)?);
        rope.write_to(&mut writer)?;
        let file = writer.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        drop(file);
        if let Ok(meta) = std::fs::metadata(&target) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }
        std::fs::rename(&tmp, &target)
    })();

    match attempt {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&tmp);
            if err.kind() == io::ErrorKind::PermissionDenied {
                rope.write_to(BufWriter::new(File::create(&target)?))
            } else {
                Err(err)
            }
        }
    }
}

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
    disk_stamp: Option<DiskStamp>,
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
            disk_stamp: None,
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
            disk_stamp: stamp_of(path),
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
        write_atomic(&path, &self.rope)?;
        self.disk_stamp = stamp_of(&path);
        self.path = Some(path);
        self.dirty = false;
        Ok(())
    }

    /// Whether the file changed (or vanished) since this buffer last read or
    /// wrote it. Cheap: one `stat`.
    pub fn disk_state(&self) -> DiskState {
        let (Some(path), Some(known)) = (&self.path, self.disk_stamp) else {
            return DiskState::Unchanged;
        };
        match stamp_of(path) {
            None => DiskState::Deleted,
            Some(now) if now != known => DiskState::Modified,
            Some(_) => DiskState::Unchanged,
        }
    }

    /// Accept the current state of the file on disk as the baseline, e.g. when
    /// the user chooses to keep their version: the next save overwrites it
    /// knowingly, and `disk_state` stops reporting the change.
    pub fn acknowledge_disk(&mut self) {
        self.disk_stamp = self.path.as_deref().and_then(stamp_of);
    }

    /// Replace the text with what is on disk, dropping local edits and undo
    /// history. The cursor stays at its offset (clamped to the new length).
    pub fn reload_from_disk(&mut self) -> io::Result<()> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "buffer has no associated path"))?;
        let fresh = Rope::from_reader(BufReader::new(File::open(&path)?))?;
        let head = self.selections.primary().head;
        self.rope = fresh;
        self.undo_tree = UndoTree::default();
        self.dirty = false;
        self.revision = self.revision.wrapping_add(1);
        let head = self.floor_char_boundary(head);
        self.selections = SelectionSet::single(head);
        self.disk_stamp = stamp_of(&path);
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
    /// The largest char boundary at or before `byte` (clamped to the length).
    fn floor_char_boundary(&self, byte: usize) -> usize {
        let len = self.rope.len();
        let mut at = byte.min(len);
        let is_boundary = |this: &Self, at: usize| {
            at == 0 || at == len || this.next_char_boundary(this.prev_char_boundary(at)) == at
        };
        while at > 0 && !is_boundary(self, at) {
            at -= 1;
        }
        at
    }

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

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("core-buffer-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_writes_the_file_and_leaves_no_temp_files() {
        let dir = temp_dir("atomic");
        let file = dir.join("a.txt");
        std::fs::write(&file, "old").unwrap();
        let mut buffer = Buffer::open(&file).unwrap();
        buffer.edit(0..3, "brand new");
        buffer.save().unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "brand new");
        assert!(!buffer.is_dirty());
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(leftovers, vec!["a.txt"], "no .shame-tmp file may remain");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn save_keeps_permissions_and_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("perm");
        let real = dir.join("real.sh");
        std::fs::write(&real, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = dir.join("link.sh");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let mut buffer = Buffer::open(&link).unwrap();
        buffer.insert(buffer.len_bytes(), "echo hi\n");
        buffer.save().unwrap();

        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link survives");
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "#!/bin/sh\necho hi\n");
        assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o755);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_external_changes_and_deletion() {
        let dir = temp_dir("disk");
        let file = dir.join("a.txt");
        std::fs::write(&file, "one").unwrap();
        let mut buffer = Buffer::open(&file).unwrap();
        assert_eq!(buffer.disk_state(), DiskState::Unchanged);

        std::fs::write(&file, "something longer").unwrap();
        assert_eq!(buffer.disk_state(), DiskState::Modified);

        // Our own save is not an external change.
        buffer.edit(0..3, "mine");
        buffer.save().unwrap();
        assert_eq!(buffer.disk_state(), DiskState::Unchanged);

        // Keeping our version acknowledges whatever is on disk now.
        std::fs::write(&file, "theirs again, and longer").unwrap();
        assert_eq!(buffer.disk_state(), DiskState::Modified);
        buffer.acknowledge_disk();
        assert_eq!(buffer.disk_state(), DiskState::Unchanged);

        std::fs::remove_file(&file).unwrap();
        assert_eq!(buffer.disk_state(), DiskState::Deleted);
        assert_eq!(Buffer::from_str("scratch").disk_state(), DiskState::Unchanged, "no file, nothing to compare");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reload_replaces_text_clears_dirty_and_keeps_the_cursor() {
        let dir = temp_dir("reload");
        let file = dir.join("a.txt");
        std::fs::write(&file, "hello world").unwrap();
        let mut buffer = Buffer::open(&file).unwrap();
        buffer.edit(0..5, "HELLO"); // dirty local edit
        buffer.selections_mut().set_primary(crate::selection::Selection::cursor(8));

        std::fs::write(&file, "disk text that is longer").unwrap();
        buffer.reload_from_disk().unwrap();
        assert_eq!(buffer.text(), "disk text that is longer");
        assert!(!buffer.is_dirty());
        assert!(!buffer.can_undo(), "undo history belongs to the old text");
        assert_eq!(buffer.selections().primary().head, 8);
        assert_eq!(buffer.disk_state(), DiskState::Unchanged);

        // A shorter file clamps the cursor.
        buffer.selections_mut().set_primary(crate::selection::Selection::cursor(20));
        std::fs::write(&file, "tiny").unwrap();
        buffer.reload_from_disk().unwrap();
        assert_eq!(buffer.selections().primary().head, 4);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
