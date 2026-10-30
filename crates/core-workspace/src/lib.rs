//! File tree, file watcher, search, and settings (M1+).
//!
//! Has no UI dependency by design, so the GPUI-based UI layer (`app`,
//! `editor-view`) can be replaced later without touching this crate.
//!
//! `git::{current_branch, has_uncommitted_changes}` and
//! `search::search_text` are the pieces built here so far — small,
//! backend-only reads (shell out to `git`, same pattern `core-flutter`
//! uses for the `flutter`/`dart` CLIs; walk the filesystem for search,
//! same pattern the file tree already uses) rather than a redesign of
//! everything this crate's doc comment promises. The file tree, file
//! watcher, and settings are still unbuilt — the app crate currently
//! walks the filesystem directly for its tree/Quick Open list rather
//! than through here, and nothing calls `search_text` yet (a results
//! panel is UI work, staying deferred like everything else UI-shaped).

mod diff;
mod git;
mod repo;
mod search;

pub use diff::{DiffCell, DiffRow, FileDiff, RowKind, file_diff, parse_unified};
pub use git::{
    LineChange, ahead_behind, current_branch, has_uncommitted_changes, line_changes, status_map,
};
pub use repo::{
    BranchInfo, ChangeKind, ChangedFile, RepoStatus, branches, checkout, commit, create_branch, discard, fetch,
    init, last_commit_message, pull, push, repo_root, repo_status, stage, stage_all, unstage, unstage_all,
};
pub use search::{SearchMatch, search_text};
