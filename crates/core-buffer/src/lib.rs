//! Rope-based text buffer, selections, and undo tree (M1).
//!
//! Has no UI dependency by design, so the GPUI-based UI layer (`app`,
//! `editor-view`) can be replaced later without touching this crate.

mod buffer;
mod selection;
mod undo;

pub use buffer::{Buffer, DiskState};
pub use selection::{Selection, SelectionSet};
pub use undo::UndoTree;
