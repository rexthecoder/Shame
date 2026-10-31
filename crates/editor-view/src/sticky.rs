//! Sticky scroll: pin the header lines of the scopes (class, method, ...)
//! that enclose the top visible line, so you always see where you are.

use core_syntax::FoldRange;

/// Clamp a (possibly rubber-banded) scroll offset to the real scrollable
/// range `0..=max_scroll`. While a trackpad overscrolls past the end the
/// list's offset briefly exceeds the maximum before it is clamped on the
/// next layout; reading it raw would jump the top line ahead of the content.
pub(crate) fn clamped_scroll(scrolled_px: f64, max_scroll: Option<f64>) -> f64 {
    let upper = max_scroll.map_or(f64::INFINITY, |m| m.max(0.0));
    scrolled_px.clamp(0.0, upper)
}

/// The row at the top of a list scrolled down by `scrolled_px` pixels with
/// rows `row_height` tall.
pub(crate) fn top_row(scrolled_px: f64, row_height: f64) -> usize {
    (scrolled_px / row_height.max(1.0)).floor().max(0.0) as usize
}

/// Start lines of the scopes that begin above `top_line` but are still open
/// at it, outermost first, keeping only the innermost `max` of them.
/// `folds` must be sorted by start byte (as `SyntaxTree::fold_ranges` returns).
pub(crate) fn sticky_scope_lines(folds: &[FoldRange], top_line: usize, max: usize) -> Vec<usize> {
    let mut lines: Vec<usize> = Vec::new();
    for fold in folds {
        if fold.start_line < top_line && fold.end_line >= top_line && lines.last() != Some(&fold.start_line) {
            lines.push(fold.start_line);
        }
    }
    let skip = lines.len().saturating_sub(max);
    lines.split_off(skip)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fold(start_line: usize, end_line: usize) -> FoldRange {
        FoldRange { start_line, end_line, start_byte: start_line * 10, end_byte: end_line * 10 }
    }

    #[test]
    fn overscroll_is_clamped_to_the_scrollable_range() {
        assert_eq!(clamped_scroll(500.0, Some(400.0)), 400.0, "bounce past the end");
        assert_eq!(clamped_scroll(-30.0, Some(400.0)), 0.0, "bounce past the top");
        assert_eq!(clamped_scroll(120.0, Some(400.0)), 120.0);
        assert_eq!(clamped_scroll(120.0, None), 120.0, "unknown extent: leave as is");
        assert_eq!(clamped_scroll(50.0, Some(-10.0)), 0.0, "content shorter than viewport");
    }

    #[test]
    fn top_row_follows_the_pixel_offset() {
        assert_eq!(top_row(0.0, 22.0), 0);
        assert_eq!(top_row(21.9, 22.0), 0);
        assert_eq!(top_row(22.0, 22.0), 1);
        assert_eq!(top_row(220.5, 22.0), 10);
        assert_eq!(top_row(-5.0, 22.0), 0, "overscroll never goes negative");
    }

    #[test]
    fn picks_enclosing_scopes_outermost_first() {
        // class 0..40 { method 5..20 { if 8..12 } method 25..35 }
        let folds = vec![fold(0, 40), fold(5, 20), fold(8, 12), fold(25, 35)];
        assert_eq!(sticky_scope_lines(&folds, 10, 3), vec![0, 5, 8]);
        assert_eq!(sticky_scope_lines(&folds, 30, 3), vec![0, 25]);
    }

    #[test]
    fn top_line_inside_the_header_line_pins_nothing_for_that_scope() {
        let folds = vec![fold(0, 40)];
        assert!(sticky_scope_lines(&folds, 0, 3).is_empty());
        assert_eq!(sticky_scope_lines(&folds, 1, 3), vec![0]);
    }

    #[test]
    fn keeps_only_the_innermost_max_scopes() {
        let folds = vec![fold(0, 90), fold(1, 80), fold(2, 70), fold(3, 60)];
        assert_eq!(sticky_scope_lines(&folds, 10, 2), vec![2, 3]);
    }

    #[test]
    fn scopes_that_ended_above_are_not_pinned() {
        let folds = vec![fold(0, 5), fold(10, 20)];
        assert_eq!(sticky_scope_lines(&folds, 12, 3), vec![10]);
    }
}
