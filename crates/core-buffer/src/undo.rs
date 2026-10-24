/// A single edit: replace the bytes at `start..start+removed.len()` with
/// `inserted`. Storing both directions lets the same struct serve as its
/// own inverse (swap `removed` and `inserted`).
#[derive(Debug, Clone)]
pub(crate) struct EditOp {
    pub start: usize,
    pub removed: String,
    pub inserted: String,
}

impl EditOp {
    fn inverse(&self) -> EditOp {
        EditOp {
            start: self.start,
            removed: self.inserted.clone(),
            inserted: self.removed.clone(),
        }
    }
}

#[derive(Debug)]
struct Node {
    parent: Option<usize>,
    children: Vec<usize>,
    /// The edit that transforms `parent`'s text into this node's text.
    edit: EditOp,
}

/// An undo **tree**, not a stack: undoing and then making a new edit opens a
/// new branch instead of discarding the old redo path. `redo` always follows
/// the most recently created branch from the current node.
#[derive(Debug)]
pub struct UndoTree {
    nodes: Vec<Node>,
    current: usize,
}

impl Default for UndoTree {
    fn default() -> Self {
        Self {
            nodes: vec![Node {
                parent: None,
                children: Vec::new(),
                edit: EditOp {
                    start: 0,
                    removed: String::new(),
                    inserted: String::new(),
                },
            }],
            current: 0,
        }
    }
}

impl UndoTree {
    pub(crate) fn record(&mut self, edit: EditOp) {
        let parent = self.current;
        let idx = self.nodes.len();
        self.nodes.push(Node {
            parent: Some(parent),
            children: Vec::new(),
            edit,
        });
        self.nodes[parent].children.push(idx);
        self.current = idx;
    }

    pub(crate) fn undo(&mut self) -> Option<EditOp> {
        let parent = self.nodes[self.current].parent?;
        let inverse = self.nodes[self.current].edit.inverse();
        self.current = parent;
        Some(inverse)
    }

    pub(crate) fn redo(&mut self) -> Option<EditOp> {
        let child = *self.nodes[self.current].children.last()?;
        let edit = self.nodes[child].edit.clone();
        self.current = child;
        Some(edit)
    }

    pub fn can_undo(&self) -> bool {
        self.nodes[self.current].parent.is_some()
    }

    pub fn can_redo(&self) -> bool {
        !self.nodes[self.current].children.is_empty()
    }

    /// Total number of edits ever recorded, across every branch (including
    /// ones no longer reachable by `redo` after a branch). Mainly useful for
    /// tests and debugging.
    pub fn node_count(&self) -> usize {
        self.nodes.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(start: usize, removed: &str, inserted: &str) -> EditOp {
        EditOp {
            start,
            removed: removed.to_string(),
            inserted: inserted.to_string(),
        }
    }

    #[test]
    fn fresh_tree_cannot_undo_or_redo() {
        let tree = UndoTree::default();
        assert!(!tree.can_undo());
        assert!(!tree.can_redo());
    }

    #[test]
    fn undo_reverses_the_recorded_edit() {
        let mut tree = UndoTree::default();
        tree.record(edit(0, "", "hi"));

        assert!(tree.can_undo());
        let inverse = tree.undo().unwrap();
        assert_eq!(inverse.removed, "hi");
        assert_eq!(inverse.inserted, "");
        assert!(!tree.can_undo());
    }

    #[test]
    fn redo_replays_the_original_edit() {
        let mut tree = UndoTree::default();
        tree.record(edit(0, "", "hi"));
        tree.undo();

        assert!(tree.can_redo());
        let replay = tree.redo().unwrap();
        assert_eq!(replay.removed, "");
        assert_eq!(replay.inserted, "hi");
    }

    #[test]
    fn branching_keeps_old_history_instead_of_discarding_it() {
        let mut tree = UndoTree::default();
        tree.record(edit(0, "", "a")); // node 1
        tree.record(edit(1, "", "b")); // node 2
        tree.undo(); // back to node 1

        // A fresh edit from node 1 opens a *second* child (a branch),
        // instead of erasing node 2's branch the way a plain undo stack
        // would.
        tree.record(edit(1, "", "c")); // node 3, sibling of node 2

        assert_eq!(tree.node_count(), 3);
        assert!(tree.can_undo());
        // redo() follows the newest branch (node 3), not the old one.
        assert!(!tree.can_redo());
    }
}
