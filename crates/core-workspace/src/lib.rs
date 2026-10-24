//! File tree, file watcher, search, and settings (M1+).
//!
//! Has no UI dependency by design, so the GPUI-based UI layer (`app`,
//! `editor-view`) can be replaced later without touching this crate.
//!
//! `git::current_branch` is the first piece built here: a small,
//! backend-only read (shell out to `git rev-parse --abbrev-ref HEAD`,
//! same pattern `core-flutter` uses for the `flutter`/`dart` CLIs)
//! for showing the open project's branch in the title bar, the way
//! real IDEs do. The file tree, file watcher, search, and settings
//! this crate's doc comment promises are still unbuilt — the app
//! crate currently walks the filesystem directly for its tree/Quick
//! Open list rather than through here.

mod git;

pub use git::current_branch;
