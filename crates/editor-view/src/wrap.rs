//! Soft wrap: where a long line breaks into display rows.
//!
//! Positions are display columns (tabs and wide characters counted as the
//! editor draws them), so the same numbers drive layout, hit-testing and
//! vertical movement. A wrapped row is the whole line painted shifted left by
//! the row's start column and clipped to the row's width, which keeps carets,
//! selections, diagnostics and search matches working in line coordinates.

/// Width of one character in editor cells; mirrors the editor's own rule.
fn cells(ch: char) -> usize {
    crate::char_columns(ch)
}

/// Start column of every display row of `text`, the first always `0`.
///
/// Breaks after the last whitespace that fits, and falls back to a hard break
/// when a word is longer than a row. `cols` is the row width in cells;
/// anything below 2 disables wrapping.
pub(crate) fn row_starts(text: &str, cols: usize) -> Vec<usize> {
    let mut starts = vec![0usize];
    if cols < 2 || text.len() <= 1 {
        return starts;
    }
    // Fast path: ASCII without tabs is one cell per byte.
    if text.is_ascii() && !text.as_bytes().contains(&b'\t') && text.len() <= cols {
        return starts;
    }
    let mut row_start = 0usize;
    // Column just after the last whitespace seen in the current row.
    let mut last_break: Option<usize> = None;
    let mut col = 0usize;
    for ch in text.chars() {
        let w = cells(ch);
        while col + w > row_start + cols && col > row_start {
            let at = match last_break {
                Some(b) if b > row_start => b,
                _ => col,
            };
            starts.push(at);
            row_start = at;
            last_break = None;
        }
        col += w;
        if ch == ' ' || ch == '\t' {
            last_break = Some(col);
        }
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_lines_stay_on_one_row() {
        assert_eq!(row_starts("hello world", 40), vec![0]);
        assert_eq!(row_starts("", 40), vec![0]);
    }

    #[test]
    fn breaks_after_the_last_space_that_fits() {
        // "aaa bbb ccc" at 8 columns: "aaa bbb " | "ccc".
        assert_eq!(row_starts("aaa bbb ccc", 8), vec![0, 8]);
    }

    #[test]
    fn a_long_word_breaks_hard() {
        assert_eq!(row_starts("abcdefghij", 4), vec![0, 4, 8]);
    }

    #[test]
    fn wide_characters_count_two_cells() {
        // Five full-width characters are ten cells: rows of 4.
        assert_eq!(row_starts("日本語です", 4), vec![0, 4, 8]);
    }

    #[test]
    fn tabs_count_as_four_cells() {
        assert_eq!(row_starts("\t\t\tx", 8), vec![0, 8]);
    }

    #[test]
    fn every_row_makes_progress() {
        // A character wider than the row must not loop forever.
        let starts = row_starts("日日日", 2);
        assert!(starts.windows(2).all(|w| w[0] < w[1]));
    }
}
