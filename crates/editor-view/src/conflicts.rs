//! Git merge-conflict resolution: find the `<<<<<<<` / `=======` / `>>>>>>>`
//! block around the cursor and replace it with one side or both.

use std::ops::Range;

use gpui_kit::Context;

use crate::EditorView;

/// Which side of a conflict to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    Current,
    Incoming,
    Both,
}

/// A conflict block, as 0-indexed line numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Conflict {
    pub start: usize,
    pub middle: usize,
    pub end: usize,
}

/// The conflict whose block (`<<<<<<<` through `>>>>>>>`) contains `line`.
pub(crate) fn conflict_at(lines: &[&str], line: usize) -> Option<Conflict> {
    let mut start = None;
    let mut middle = None;
    for (ix, text) in lines.iter().enumerate() {
        if text.starts_with("<<<<<<<") {
            start = Some(ix);
            middle = None;
        } else if text.starts_with("=======") && start.is_some() {
            middle = Some(ix);
        } else if text.starts_with(">>>>>>>") {
            if let (Some(s), Some(m)) = (start, middle) {
                if (s..=ix).contains(&line) {
                    return Some(Conflict { start: s, middle: m, end: ix });
                }
            }
            start = None;
            middle = None;
        }
    }
    None
}

/// Number of unresolved conflict blocks in `text`.
pub(crate) fn conflict_count(text: &str) -> usize {
    let mut count = 0;
    let mut open = false;
    let mut saw_middle = false;
    for line in text.lines() {
        if line.starts_with("<<<<<<<") {
            open = true;
            saw_middle = false;
        } else if open && line.starts_with("=======") {
            saw_middle = true;
        } else if line.starts_with(">>>>>>>") {
            if open && saw_middle {
                count += 1;
            }
            open = false;
        }
    }
    count
}

/// The replacement text (with trailing newline per kept line) for `choice`.
pub(crate) fn resolve(lines: &[&str], conflict: &Conflict, choice: ConflictChoice) -> String {
    let current = &lines[conflict.start + 1..conflict.middle];
    let incoming = &lines[conflict.middle + 1..conflict.end];
    let kept: Vec<&str> = match choice {
        ConflictChoice::Current => current.to_vec(),
        ConflictChoice::Incoming => incoming.to_vec(),
        ConflictChoice::Both => current.iter().chain(incoming.iter()).copied().collect(),
    };
    kept.iter().map(|l| format!("{l}\n")).collect()
}

impl EditorView {
    /// Number of unresolved merge conflicts in this buffer (for the status bar).
    pub fn conflict_count(&self) -> usize {
        conflict_count(&self.buffer.text())
    }

    /// Resolve the conflict under the cursor. Returns whether one was found.
    pub fn resolve_conflict_at_cursor(&mut self, choice: ConflictChoice, cx: &mut Context<Self>) -> bool {
        let text = self.buffer.text();
        let lines: Vec<&str> = text.lines().collect();
        let head = self.buffer.selections().primary().head;
        let line = self.buffer.byte_to_line(head);
        let Some(conflict) = conflict_at(&lines, line) else {
            return false;
        };
        let replacement = resolve(&lines, &conflict, choice);
        let range: Range<usize> = self.buffer.line_to_byte(conflict.start)
            ..self.buffer.line_to_byte(conflict.end + 1).min(self.buffer.len_bytes());
        self.buffer.edit(range.clone(), &replacement);
        self.set_cursor(range.start, cx);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "a\n<<<<<<< HEAD\nmine\n=======\ntheirs\n>>>>>>> branch\nz\n";

    fn lines() -> Vec<&'static str> {
        SAMPLE.lines().collect()
    }

    #[test]
    fn finds_conflict_around_any_line_of_the_block() {
        let l = lines();
        let c = conflict_at(&l, 3).unwrap();
        assert_eq!((c.start, c.middle, c.end), (1, 3, 5));
        assert!(conflict_at(&l, 0).is_none());
        assert!(conflict_at(&l, 6).is_none());
    }

    #[test]
    fn resolves_each_choice() {
        let l = lines();
        let c = conflict_at(&l, 2).unwrap();
        assert_eq!(resolve(&l, &c, ConflictChoice::Current), "mine\n");
        assert_eq!(resolve(&l, &c, ConflictChoice::Incoming), "theirs\n");
        assert_eq!(resolve(&l, &c, ConflictChoice::Both), "mine\ntheirs\n");
    }

    #[test]
    fn counts_only_complete_blocks() {
        assert_eq!(conflict_count(SAMPLE), 1);
        assert_eq!(conflict_count("<<<<<<< x\nfoo\n"), 0);
        assert_eq!(conflict_count(&format!("{SAMPLE}{SAMPLE}")), 2);
    }
}
