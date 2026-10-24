use std::ops::Range;

/// A single cursor or range selection, as byte offsets into a [`crate::Buffer`].
///
/// `anchor` is where the selection started and `head` is where it currently
/// ends (and where the cursor is drawn); a selection with `anchor == head`
/// is just a cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head }
    }

    pub fn cursor(offset: usize) -> Self {
        Self {
            anchor: offset,
            head: offset,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn range(&self) -> Range<usize> {
        self.start()..self.end()
    }

    /// Move this selection's offsets to account for an edit that replaced
    /// `removed_len` bytes at `edit_start` with `inserted_len` bytes.
    fn shift_for_edit(&self, edit_start: usize, removed_len: usize, inserted_len: usize) -> Self {
        let shift = |offset: usize| -> usize {
            if offset <= edit_start {
                offset
            } else if offset <= edit_start + removed_len {
                // The offset was inside the replaced region: collapse it to
                // the end of whatever was inserted in its place.
                edit_start + inserted_len
            } else {
                offset - removed_len + inserted_len
            }
        };

        Self {
            anchor: shift(self.anchor),
            head: shift(self.head),
        }
    }
}

/// A set of one or more [`Selection`]s (multi-cursor), always non-empty and
/// kept sorted with overlapping/touching selections merged.
#[derive(Debug, Clone)]
pub struct SelectionSet {
    selections: Vec<Selection>,
    primary: usize,
}

impl Default for SelectionSet {
    fn default() -> Self {
        Self::single(0)
    }
}

impl SelectionSet {
    pub fn single(offset: usize) -> Self {
        Self {
            selections: vec![Selection::cursor(offset)],
            primary: 0,
        }
    }

    pub fn as_slice(&self) -> &[Selection] {
        &self.selections
    }

    pub fn len(&self) -> usize {
        self.selections.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn primary(&self) -> Selection {
        self.selections[self.primary]
    }

    /// Replace the whole set with a single selection.
    pub fn set_primary(&mut self, selection: Selection) {
        self.selections = vec![selection];
        self.primary = 0;
    }

    /// Add another cursor/selection to the set (multi-cursor editing).
    pub fn add(&mut self, selection: Selection) {
        self.selections.push(selection);
        self.primary = self.selections.len() - 1;
        self.normalize();
    }

    pub(crate) fn apply_edit(&mut self, edit_start: usize, removed_len: usize, inserted_len: usize) {
        for selection in &mut self.selections {
            *selection = selection.shift_for_edit(edit_start, removed_len, inserted_len);
        }
        self.normalize();
    }

    /// Sort by position and merge any selections that now overlap or touch,
    /// keeping `primary` pointing at the same logical selection.
    fn normalize(&mut self) {
        let primary_start = self.selections[self.primary].start();

        self.selections.sort_by_key(|s| s.start());

        let mut merged: Vec<Selection> = Vec::with_capacity(self.selections.len());
        for selection in self.selections.drain(..) {
            match merged.last_mut() {
                Some(last) if selection.start() <= last.end() => {
                    *last = Selection::new(
                        last.start().min(selection.start()),
                        last.end().max(selection.end()),
                    );
                }
                _ => merged.push(selection),
            }
        }
        self.selections = merged;

        self.primary = self
            .selections
            .iter()
            .position(|s| s.start() <= primary_start && primary_start <= s.end())
            .unwrap_or(self.selections.len() - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_is_empty() {
        assert!(Selection::cursor(5).is_empty());
        assert!(!Selection::new(2, 5).is_empty());
    }

    #[test]
    fn shift_after_insert_before_cursor() {
        let sel = Selection::cursor(10);
        let shifted = sel.shift_for_edit(0, 0, 3);
        assert_eq!(shifted, Selection::cursor(13));
    }

    #[test]
    fn shift_after_delete_overlapping_selection() {
        // Selection at 5..8, delete bytes 4..10 (swallows it).
        let sel = Selection::new(5, 8);
        let shifted = sel.shift_for_edit(4, 6, 0);
        assert_eq!(shifted, Selection::cursor(4));
    }

    #[test]
    fn multi_cursor_add_and_merge() {
        let mut set = SelectionSet::single(0);
        set.add(Selection::cursor(5));
        assert_eq!(set.len(), 2);

        // Overlapping selection should merge the two into one.
        set.add(Selection::new(0, 6));
        assert_eq!(set.len(), 1);
        assert_eq!(set.as_slice()[0].range(), 0..6);
    }
}
