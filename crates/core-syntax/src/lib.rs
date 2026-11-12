//! Tree-sitter wrapper, highlight queries, and folding (M1/M2).
//!
//! Grammars: Dart (`tree-sitter-dart`'s own query) and Python
//! (`tree-sitter-python`'s query plus `queries/python-extra.scm`). Other
//! languages use the editor's lexical highlighter. Has no UI dependency;
//! `editor-view` is the only consumer, and it stays free to render
//! highlights however it likes.

mod highlight;
mod syntax_tree;

pub use highlight::{Highlight, HighlightKind};
pub use syntax_tree::{FoldRange, SyntaxLanguage, SyntaxTree, point_at};
pub use tree_sitter::{InputEdit, Point};
