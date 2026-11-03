//! Line-based three-way merge, used when a file changes on disk while the
//! buffer holds unsaved edits: both sides' changes are kept unless they touch
//! the same lines.

use std::ops::Range;

use similar::{DiffOp, TextDiff};

/// One side's change against the common ancestor: the base lines it replaces
/// (empty for a pure insertion) and the text that takes their place.
struct Hunk {
    base: Range<usize>,
    text: String,
}

fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

fn hunks(base: &str, other: &str) -> Vec<Hunk> {
    let other_lines = lines(other);
    let mut out = Vec::new();
    for op in TextDiff::from_lines(base, other).ops() {
        let (old_index, old_len, new_index, new_len) = match *op {
            DiffOp::Equal { .. } => continue,
            DiffOp::Delete { old_index, old_len, new_index } => (old_index, old_len, new_index, 0),
            DiffOp::Insert { old_index, new_index, new_len } => (old_index, 0, new_index, new_len),
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                (old_index, old_len, new_index, new_len)
            }
        };
        out.push(Hunk {
            base: old_index..old_index + old_len,
            text: other_lines[new_index..new_index + new_len].concat(),
        });
    }
    out
}

/// Two changes can't both apply when they overlap, or start at the same line
/// (their relative order would be a guess).
fn conflicts(a: &Hunk, b: &Hunk) -> bool {
    (a.base.start < b.base.end && b.base.start < a.base.end) || a.base.start == b.base.start
}

/// Merge `ours` and `theirs`, both derived from `base`. `None` when the two
/// sides changed the same lines differently.
pub(crate) fn merge3(base: &str, ours: &str, theirs: &str) -> Option<String> {
    if ours == theirs || theirs == base {
        return Some(ours.to_string());
    }
    if ours == base {
        return Some(theirs.to_string());
    }
    let base_lines = lines(base);
    let ours_hunks = hunks(base, ours);
    let theirs_hunks = hunks(base, theirs);

    let mut out = String::new();
    let mut pos = 0;
    let (mut i, mut j) = (0, 0);
    let mut emit = |h: &Hunk, out: &mut String| {
        if h.base.start >= pos {
            out.push_str(&base_lines[pos..h.base.start].concat());
        }
        out.push_str(&h.text);
        pos = pos.max(h.base.end);
    };
    while i < ours_hunks.len() || j < theirs_hunks.len() {
        match (ours_hunks.get(i), theirs_hunks.get(j)) {
            (Some(a), Some(b)) => {
                if conflicts(a, b) {
                    if a.base != b.base || a.text != b.text {
                        return None;
                    }
                    emit(b, &mut out);
                    i += 1;
                    j += 1;
                } else if a.base.start < b.base.start {
                    emit(a, &mut out);
                    i += 1;
                } else {
                    emit(b, &mut out);
                    j += 1;
                }
            }
            (Some(a), None) => {
                emit(a, &mut out);
                i += 1;
            }
            (None, Some(b)) => {
                emit(b, &mut out);
                j += 1;
            }
            (None, None) => break,
        }
    }
    out.push_str(&base_lines[pos.min(base_lines.len())..].concat());
    Some(out)
}

/// The edits turning `from` into `to`, as byte ranges of `from` with their
/// replacement text, in document order.
pub(crate) fn line_edits(from: &str, to: &str) -> Vec<(Range<usize>, String)> {
    let from_lines = lines(from);
    let to_lines = lines(to);
    let mut starts = Vec::with_capacity(from_lines.len() + 1);
    let mut at = 0;
    for line in &from_lines {
        starts.push(at);
        at += line.len();
    }
    starts.push(at);

    let mut out = Vec::new();
    for op in TextDiff::from_lines(from, to).ops() {
        let (old_index, old_len, new_index, new_len) = match *op {
            DiffOp::Equal { .. } => continue,
            DiffOp::Delete { old_index, old_len, new_index } => (old_index, old_len, new_index, 0),
            DiffOp::Insert { old_index, new_index, new_len } => (old_index, 0, new_index, new_len),
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                (old_index, old_len, new_index, new_len)
            }
        };
        out.push((
            starts[old_index]..starts[old_index + old_len],
            to_lines[new_index..new_index + new_len].concat(),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "a\nb\nc\nd\ne\nf\n";

    #[test]
    fn edits_in_different_places_are_both_kept() {
        let ours = "a\nB\nc\nd\ne\nf\n";
        let theirs = "a\nb\nc\nd\ne\nF\n";
        assert_eq!(merge3(BASE, ours, theirs).as_deref(), Some("a\nB\nc\nd\ne\nF\n"));
    }

    #[test]
    fn insertions_and_deletions_combine() {
        let ours = "top\na\nb\nc\nd\ne\nf\n";
        let theirs = "a\nb\nc\nd\nf\n";
        assert_eq!(merge3(BASE, ours, theirs).as_deref(), Some("top\na\nb\nc\nd\nf\n"));
    }

    #[test]
    fn the_same_change_on_both_sides_is_not_a_conflict() {
        let both = "a\nb\nX\nd\ne\nf\n";
        assert_eq!(merge3(BASE, both, both).as_deref(), Some(both));
    }

    #[test]
    fn different_changes_to_the_same_line_conflict() {
        assert_eq!(merge3(BASE, "a\nb\nONE\nd\ne\nf\n", "a\nb\nTWO\nd\ne\nf\n"), None);
    }

    #[test]
    fn line_edits_replay_to_the_target_text() {
        let to = "a\nB\nc\nnew\nd\nf\n";
        let mut text = BASE.to_string();
        for (range, replacement) in line_edits(BASE, to).into_iter().rev() {
            text.replace_range(range, &replacement);
        }
        assert_eq!(text, to);
    }
}
