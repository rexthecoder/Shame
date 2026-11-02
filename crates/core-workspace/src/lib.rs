//! File tree, file watcher, search, and settings (M1+).
//!
//! Has no UI dependency by design, so the GPUI-based UI layer (`app`,
//! `editor-view`) can be replaced later without touching this crate.
//!
//! `git::{current_branch, has_uncommitted_changes}` are small backend-only
//! reads that shell out to `git`, the same pattern `core-flutter` uses for
//! the `flutter`/`dart` CLIs. [`SearchIndex`] is the one long-lived piece:
//! an [`fff_search`] picker per workspace that walks once, watches for
//! changes, and answers content grep and fuzzy filename search from memory.
//! [`search_text`] stays as the on-disk walk used before that first scan
//! finishes and for queries the index cannot express. The file tree, file
//! watcher, and settings are still unbuilt — the app crate walks the
//! filesystem directly for its tree.

mod diff;
mod git;
mod history;
mod message;
mod repo;
mod search;
mod search_index;
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
pub use search::{
    SearchMatch, SearchOptions, SearchResults, build_matcher, match_ranges, replace_in_lines,
    replace_text, search_text, search_text_with,
};
pub use search_index::{SearchIndex, search_workspace, split_query_filters};
