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
//! watcher, and settings are still unbuilt — the app crate walks the
//! filesystem directly for its tree/Quick Open list, while global search
//! calls `search_text` here for its background scan.

mod diff;
mod git;
mod history;
mod message;
mod repo;
mod search;
mod stats;
mod symbols;

pub use diff::{
    Block, DiffCell, DiffRow, FileDiff, RowKind, blocks, commit_file_diff, file_diff, hunk_patch,
    intraline, parse_unified, revert_hunk, stage_hunk, unstage_hunk,
};
pub use git::{
    LineChange, ahead_behind, current_branch, has_uncommitted_changes, line_blame, line_changes,
    status_map,
};
pub use message::{apply_commit_type, commit_type_of, commit_types, suggest_message};
pub use repo::{
    BranchInfo, ChangeKind, ChangedFile, CommitInfo, RepoStatus, StashEntry, branches, checkout,
    commit, commit_files, create_branch, delete_branch, discard, discard_files, fetch, init,
    last_commit_message, pull, push, recent_commits, repo_root, repo_status, stage, stage_all,
    stash_apply, stash_drop, stash_pop, stash_save, stashes, uncommit_last, unstage, unstage_all,
};
pub use history::{Snapshot, list_snapshots, record_snapshot};
pub use stats::{change_stats, parse_numstat};
pub use symbols::{WorkspaceSymbol, filter_symbols, index_workspace_symbols, symbols_in_text};
pub use search::{SearchMatch, SearchResults, replace_text, search_text};
