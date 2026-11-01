//! Inlay hints: virtual text the language server asks us to show between
//! characters (inferred types after `var x`, parameter names before call
//! arguments).
//!
//! Hints are kept as byte offsets into the buffer, sorted. The line itself
//! is still shaped from the real buffer text, so every buffer column keeps
//! its meaning; the renderer only shifts x positions right by the width of
//! the hints that sit before a column (see [`InlayLayout`]).

use std::ops::Range;

use gpui_kit::Pixels;

/// One hint, positioned in buffer bytes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct InlayHintView {
    /// Byte offset the hint sits in front of.
    pub offset: usize,
    pub label: String,
    pub pad_left: bool,
    pub pad_right: bool,
}

/// Move hints to follow a single text edit that replaced
/// `start..old_end` with text ending at `new_end`. Hints after the edit
/// shift by the length change; hints inside the replaced span are dropped
/// (the server sends fresh ones once the debounced request comes back).
pub(crate) fn shift_inlay_hints(hints: &mut Vec<InlayHintView>, start: usize, old_end: usize, new_end: usize) {
    hints.retain_mut(|hint| {
        if hint.offset < start {
            true
        } else if hint.offset > old_end || (hint.offset == old_end && old_end > start) {
            hint.offset = hint.offset - old_end + new_end;
            true
        } else {
            // `start..=old_end` for an insertion (start == old_end) means a
            // hint glued to the cursor: keep it in front of the new text.
            start == old_end
        }
    });
}

/// The hints that fall on the line spanning `line` (buffer bytes), as
/// columns relative to the line start.
pub(crate) fn hints_on_line(hints: &[InlayHintView], line: Range<usize>) -> impl Iterator<Item = (usize, &InlayHintView)> {
    let first = hints.partition_point(|h| h.offset < line.start);
    hints[first..]
        .iter()
        .take_while(move |h| h.offset <= line.end)
        .map(move |h| (h.offset - line.start, h))
}

/// Horizontal space taken by each hint on one line, in column order:
/// `(column, advance)`, where `advance` includes padding.
#[derive(Clone)]
pub(crate) struct InlayLayout {
    pub items: Vec<(usize, Pixels)>,
}

impl InlayLayout {
    /// Total width of hints in front of `column`. With `inclusive`, a
    /// hint sitting exactly at `column` counts too: use that for the left
    /// edge of a character (caret, range start) so it lands after the
    /// hint, and the exclusive form for a range end so it stops before it.
    pub fn shift(&self, column: usize, inclusive: bool) -> Pixels {
        self.items
            .iter()
            .take_while(|(col, _)| *col < column || (inclusive && *col == column))
            .map(|(_, advance)| *advance)
            .sum()
    }

    pub fn total(&self) -> Pixels {
        self.items.iter().map(|(_, advance)| *advance).sum()
    }

    /// Map an x position on screen back to a buffer column, given the
    /// text-only `x_for_index` of the shaped line and its
    /// `closest_index_for_x`. A click on a hint snaps to the column the
    /// hint sits in front of.
    pub fn column_for_x(
        &self,
        x: Pixels,
        x_for_index: impl Fn(usize) -> Pixels,
        closest_index_for_x: impl Fn(Pixels) -> usize,
    ) -> usize {
        let mut shift = Pixels::ZERO;
        for (col, advance) in &self.items {
            let hint_left = x_for_index(*col) + shift;
            if x < hint_left {
                break;
            }
            if x < hint_left + *advance {
                return *col;
            }
            shift += *advance;
        }
        closest_index_for_x(x - shift)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::px;

    fn hint(offset: usize) -> InlayHintView {
        InlayHintView { offset, label: ": int".into(), pad_left: false, pad_right: false }
    }

    #[test]
    fn hints_after_an_insertion_move_right() {
        let mut hints = vec![hint(2), hint(10)];
        shift_inlay_hints(&mut hints, 5, 5, 8);
        assert_eq!(hints.iter().map(|h| h.offset).collect::<Vec<_>>(), vec![2, 13]);
    }

    #[test]
    fn hints_inside_a_replaced_span_are_dropped() {
        let mut hints = vec![hint(2), hint(6), hint(10)];
        shift_inlay_hints(&mut hints, 4, 8, 5);
        assert_eq!(hints.iter().map(|h| h.offset).collect::<Vec<_>>(), vec![2, 7]);
    }

    #[test]
    fn hint_at_the_end_of_a_deletion_follows_the_text() {
        let mut hints = vec![hint(8)];
        shift_inlay_hints(&mut hints, 4, 8, 4);
        assert_eq!(hints[0].offset, 4);
    }

    #[test]
    fn hint_at_an_insertion_point_stays_put() {
        let mut hints = vec![hint(5)];
        shift_inlay_hints(&mut hints, 5, 5, 6);
        assert_eq!(hints[0].offset, 5);
    }

    #[test]
    fn hints_on_line_are_relative_to_the_line() {
        let hints = vec![hint(3), hint(12), hint(15), hint(30)];
        let cols: Vec<usize> = hints_on_line(&hints, 10..20).map(|(c, _)| c).collect();
        assert_eq!(cols, vec![2, 5]);
    }

    #[test]
    fn layout_shifts_columns_past_each_hint() {
        let layout = InlayLayout { items: vec![(3, px(20.)), (6, px(10.))] };
        assert_eq!(layout.shift(2, true), px(0.));
        assert_eq!(layout.shift(3, false), px(0.));
        assert_eq!(layout.shift(3, true), px(20.));
        assert_eq!(layout.shift(7, true), px(30.));
        assert_eq!(layout.total(), px(30.));
    }

    #[test]
    fn clicks_map_back_to_buffer_columns() {
        // Monospace: 10px per column; a 20px hint before column 3.
        let layout = InlayLayout { items: vec![(3, px(20.))] };
        let x_for = |i: usize| px(i as f32 * 10.);
        let closest = |x: Pixels| (f32::from(x) / 10.).round() as usize;
        assert_eq!(layout.column_for_x(px(12.), x_for, closest), 1);
        assert_eq!(layout.column_for_x(px(35.), x_for, closest), 3, "click on the hint");
        assert_eq!(layout.column_for_x(px(60.), x_for, closest), 4, "past the hint");
    }
}
