//! Indent guides: how many guides each line gets, and which block the caret
//! is in. Computed once per edit from the whole text, so drawing a line is a
//! lookup, not a scan of its neighbours.

/// Guides per line: its indent level (leading columns over `width`, a tab
/// counting as `width` columns), and for a blank line the smaller of the
/// nearest non-blank lines above and below, so guides run through gaps.
pub(crate) fn guide_counts(text: &str, width: usize) -> Vec<u16> {
    let width = width.max(1);
    let levels: Vec<Option<u16>> = text
        .split('\n')
        .map(|line| {
            let line = line.trim_end_matches('\r');
            if line.trim().is_empty() {
                return None;
            }
            let columns: usize = line
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .map(|c| if c == '\t' { width } else { 1 })
                .sum();
            Some((columns / width).min(u16::MAX as usize) as u16)
        })
        .collect();
    let mut above = vec![0u16; levels.len()];
    let mut last = 0;
    for (ix, level) in levels.iter().enumerate() {
        if let Some(level) = level {
            last = *level;
        }
        above[ix] = last;
    }
    let mut counts = vec![0u16; levels.len()];
    let mut next = 0;
    for ix in (0..levels.len()).rev() {
        counts[ix] = match levels[ix] {
            Some(level) => {
                next = level;
                level
            }
            None => above[ix].min(next),
        };
    }
    counts
}

/// The guide for the block the caret is about: `(guide level, first line,
/// last line)`, where guide level `n` sits at column `n * width`.
///
/// On a line that opens a block (the next line is deeper, as after
/// `.map(|line| {`) or closes one (the line before is deeper, as on `})`),
/// that block is lit. Anywhere else it is the block the caret sits inside:
/// the run of lines around it indented deeper than its guide.
pub(crate) fn active_guide(counts: &[u16], cursor: usize) -> Option<(u16, usize, usize)> {
    let here = *counts.get(cursor)?;
    let deeper = |ix: usize| counts.get(ix).is_some_and(|c| *c > here);
    let run = |from: usize, level: u16| {
        let mut first = from;
        while first > 0 && counts[first - 1] > level {
            first -= 1;
        }
        let mut last = from;
        while last + 1 < counts.len() && counts[last + 1] > level {
            last += 1;
        }
        (level, first, last)
    };
    if deeper(cursor + 1) {
        return Some(run(cursor + 1, here));
    }
    if cursor > 0 && deeper(cursor - 1) {
        return Some(run(cursor - 1, here));
    }
    Some(run(cursor, here.checked_sub(1)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: &str = "class A {\n  void f() {\n    x;\n\n    y;\n  }\n}";

    #[test]
    fn blank_lines_take_the_smaller_neighbour() {
        assert_eq!(guide_counts(CODE, 2), vec![0, 1, 2, 2, 2, 1, 0]);
        assert_eq!(guide_counts("\tx\n\n\t\ty", 4), vec![1, 1, 2]);
    }

    #[test]
    fn the_active_block_is_the_one_the_caret_opens_closes_or_sits_in() {
        let counts = guide_counts(CODE, 2);
        // Inside f's body.
        assert_eq!(active_guide(&counts, 2), Some((1, 2, 4)));
        // On `void f() {` and on its `}`: f's body, not A's.
        assert_eq!(active_guide(&counts, 1), Some((1, 2, 4)));
        assert_eq!(active_guide(&counts, 5), Some((1, 2, 4)));
        // On `class A {`: A's body.
        assert_eq!(active_guide(&counts, 0), Some((0, 1, 5)));
        // A flat file has no block.
        assert_eq!(active_guide(&guide_counts("a\nb", 2), 0), None);
    }
}
