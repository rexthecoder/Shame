//! Bracket-pair colours.
//!
//! Pure text measurements kept apart from rendering: how deep the brackets
//! are at the start of each line (one pass per edit), and each bracket's
//! depth within a line. `render_line` turns the depths into text colours.

use std::ops::Range;

use core_syntax::HighlightKind;

/// Bracket nesting depth at the start of every line of `text`, skipping
/// brackets inside strings and comments. Unbalanced closers never take the
/// depth below zero.
pub(crate) fn bracket_depths(text: &str) -> Vec<u32> {
    let bytes = text.as_bytes();
    let mut depths = vec![0u32];
    let mut depth = 0u32;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => depths.push(depth),
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i + 1 < bytes.len() && bytes[i + 1] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    if bytes[i] == b'\n' {
                        depths.push(depth);
                    }
                    i += 1;
                }
                i += 1;
            }
            quote @ (b'"' | b'\'' | b'`') => {
                // A Dart/Python triple quote spans lines; a single one ends
                // at its line.
                let triple = bytes.get(i + 1) == Some(&quote) && bytes.get(i + 2) == Some(&quote);
                i += if triple { 3 } else { 1 };
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 1,
                        b'\n' if triple => depths.push(depth),
                        b'\n' => {
                            depths.push(depth);
                            break;
                        }
                        c if c == quote
                            && (!triple
                                || (bytes.get(i + 1) == Some(&quote)
                                    && bytes.get(i + 2) == Some(&quote))) =>
                        {
                            if triple {
                                i += 2;
                            }
                            break;
                        }
                        _ => {}
                    }
                    i += 1;
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    depths
}

/// A line's highlight runs split so every bracket outside strings and
/// comments is its own run, tagged with its nesting depth (0 = outermost).
/// `depth` is the depth at the start of the line.
pub(crate) fn split_bracket_runs(
    line: &str,
    runs: Vec<(Range<usize>, Option<HighlightKind>)>,
    mut depth: u32,
) -> Vec<(Range<usize>, Option<HighlightKind>, Option<u32>)> {
    let bytes = line.as_bytes();
    let mut out = Vec::with_capacity(runs.len());
    for (range, kind) in runs {
        if matches!(kind, Some(HighlightKind::String | HighlightKind::Comment)) {
            out.push((range, kind, None));
            continue;
        }
        let mut start = range.start;
        for at in range.clone() {
            let level = match bytes.get(at) {
                Some(b'(' | b'[' | b'{') => {
                    depth += 1;
                    depth - 1
                }
                Some(b')' | b']' | b'}') => {
                    depth = depth.saturating_sub(1);
                    depth
                }
                _ => continue,
            };
            if start < at {
                out.push((start..at, kind, None));
            }
            out.push((at..at + 1, kind, Some(level)));
            start = at + 1;
        }
        if start < range.end {
            out.push((start..range.end, kind, None));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_at_each_line_skips_strings_and_comments() {
        let text = "a(b{\n  '(' // {\n  c]\n}) /* ( */\nd";
        assert_eq!(bracket_depths(text), vec![0, 2, 2, 1, 0]);
    }

    #[test]
    fn brackets_become_their_own_runs_with_depth() {
        let runs = split_bracket_runs("f(a[0])", vec![(0..7, None)], 0);
        let brackets: Vec<_> = runs.iter().filter_map(|(r, _, d)| d.map(|d| (r.start, d))).collect();
        assert_eq!(brackets, vec![(1, 0), (3, 1), (5, 1), (6, 0)]);
        // Inside a string nothing is split.
        let runs = split_bracket_runs("'('", vec![(0..3, Some(HighlightKind::String))], 0);
        assert_eq!(runs.len(), 1);
    }

}
