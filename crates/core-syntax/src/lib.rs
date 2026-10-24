//! Tree-sitter wrapper, highlight queries, and folding (M1/M2).
//!
//! Dart-specific for now (`tree-sitter-dart`'s own grammar and highlights
//! query, not anything we authored) — multi-language support is a config
//! format to add later, per the project's non-goals. Has no UI dependency;
//! `editor-view` is the only consumer, and it stays free to render
//! highlights however it likes.

mod highlight;
mod syntax_tree;

pub use highlight::{Highlight, HighlightKind};
pub use syntax_tree::{FoldRange, SyntaxTree, point_at};
pub use tree_sitter::{InputEdit, Point};
