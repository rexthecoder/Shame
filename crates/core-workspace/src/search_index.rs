//! The workspace's warm search index.
//!
//! One long-lived [`fff_search`] file picker per workspace: it walks the
//! project once, keeps a background watcher on it, and answers filename and
//! content queries from memory. Project search (⇧⌘F) and Quick Open (⌘P) both
//! go through here; [`crate::search_text`] stays as the on-disk fallback for
//! the seconds before the first scan finishes, and for queries it cannot
//! express (a pasted block matched across line breaks).
//!
//! The index is deliberately independent of the UI: `app` opens one when a
//! folder opens, hands it a state directory for the frecency databases, and
//! calls it from background threads.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fff_search::{
    Casing, FFFMode, FilePicker, FilePickerOptions, FrecencyTracker, FuzzySearchOptions, GrepMode,
    GrepSearchOptions, PaginationArgs, QueryParser, QueryTracker, SharedFilePicker, SharedFrecency,
    SharedQueryTracker, parse_grep_query,
};

use crate::search::{MAX_MATCHES, SearchMatch, SearchResults, is_visible_path, sort_matches};

/// A workspace indexed by fff.
pub struct SearchIndex {
    root: PathBuf,
    picker: SharedFilePicker,
    frecency: SharedFrecency,
    query_tracker: SharedQueryTracker,
}

impl SearchIndex {
    /// Start indexing `root`. The scan and the file watcher run on fff's own
    /// background threads, so this returns immediately; searches made before
    /// the first scan finishes return `None` and callers fall back to
    /// [`crate::search_text`].
    ///
    /// `state_dir` holds the two LMDB databases (file frecency and query
    /// history). Losing them is harmless — ranking just starts cold again.
    pub fn open(root: &Path, state_dir: &Path) -> Option<Arc<Self>> {
        let _ = std::fs::create_dir_all(state_dir);

        let frecency = SharedFrecency::default();
        if let Ok(tracker) = FrecencyTracker::open(state_dir.join("frecency")) {
            let _ = frecency.init(tracker);
        }

        let query_tracker = SharedQueryTracker::default();
        if let Ok(tracker) = QueryTracker::open(state_dir.join("queries")) {
            let _ = query_tracker.init(tracker);
        }

        let picker = SharedFilePicker::default();
        FilePicker::new_with_shared_state(
            picker.clone(),
            frecency.clone(),
            FilePickerOptions {
                base_path: root.to_string_lossy().into_owned(),
                // Opening a file should warm the content cache only for files
                // people actually open, and grep wants the content index.
                enable_mmap_cache: true,
                enable_content_indexing: true,
                mode: FFFMode::Neovim,
                watch: true,
                ..Default::default()
            },
        )
        .ok()?;

        Some(Arc::new(Self {
            root: root.to_path_buf(),
            picker,
            frecency,
            query_tracker,
        }))
    }

    /// True once the initial walk is done and the index can answer for the
    /// whole workspace.
    pub fn is_ready(&self) -> bool {
        self.with_picker(|picker| !picker.is_scan_active()).unwrap_or(false)
    }

    /// Block until the first scan finishes, or `timeout` elapses.
    pub fn wait_ready(&self, timeout: std::time::Duration) -> bool {
        self.picker.wait_for_scan(timeout) && self.is_ready()
    }

    /// Search file contents. `None` when the index is still scanning, so the
    /// caller can run the on-disk search instead of showing a half-empty list.
    ///
    /// The query may use fff's constraint syntax (`*.dart foo`, `!test/`); a
    /// constrained query that finds nothing is retried as plain text by fff
    /// itself, so a stray `*` in normal code never hides results.
    pub fn search_text(&self, query: &str) -> Option<SearchResults> {
        let query = query.trim();
        if query.is_empty() {
            return Some(SearchResults::default());
        }

        self.with_picker(|picker| {
            let parsed = parse_grep_query(query);
            let result = picker.grep(
                &parsed,
                &GrepSearchOptions {
                    // Case-insensitive always, like the on-disk search it
                    // replaces: a pasted identifier in the wrong case should
                    // still be found.
                    casing: Some(Casing::Insensitive),
                    mode: GrepMode::PlainText,
                    page_limit: MAX_MATCHES,
                    // One generated file with thousands of hits must not stop
                    // the list at fff's per-file default (200) while the
                    // header still says the search is complete.
                    max_matches_per_file: MAX_MATCHES,
                    // Stop a pathological query (a single letter in a monorepo)
                    // from occupying the search thread indefinitely.
                    time_budget_ms: 5_000,
                    enforce_time_budget: true,
                    ..Default::default()
                },
            );

            let mut matches: Vec<SearchMatch> = result
                .matches
                .iter()
                .filter_map(|hit| {
                    let file = result.files.get(hit.file_index)?;
                    let path = file.absolute_path(picker, &self.root);
                    if !is_visible_path(&self.root, &path) {
                        return None;
                    }
                    Some(SearchMatch {
                        path,
                        line_number: hit.line_number as usize,
                        line_text: hit.line_content.clone(),
                        phrase: false,
                    })
                })
                .collect();
            let truncated = result.next_file_offset != 0 || matches.len() > MAX_MATCHES;
            matches.truncate(MAX_MATCHES);
            // fff returns matches in index order; sorting keeps the panel
            // stable when the same file is reported by parallel workers.
            sort_matches(&mut matches);
            SearchResults {
                matches,
                truncated,
                // What fff actually matched, minus `*.dart`-style constraints,
                // so the editor's find bar can highlight it.
                query_text: parsed.grep_text(),
            }
        })
    }

    /// Fuzzy filename search for Quick Open, ranked by match quality,
    /// frecency and query history. `None` while the index is still scanning.
    ///
    /// An empty query returns the most-used files, so ⌘P opens on something
    /// useful instead of an arbitrary alphabetical slice.
    pub fn find_files(&self, query: &str, limit: usize) -> Option<Vec<PathBuf>> {
        let parsed_query = QueryParser::default().parse(query);
        self.with_picker(|picker| {
            let tracker = self.query_tracker.read().ok();
            let result = picker.fuzzy_search(
                &parsed_query,
                tracker.as_ref().and_then(|t| t.as_ref()),
                FuzzySearchOptions {
                    // Over-fetch: a plain folder's index also holds generated
                    // directories, which are dropped below.
                    pagination: PaginationArgs {
                        offset: 0,
                        limit: limit.saturating_mul(4).max(limit + 8),
                    },
                    project_path: Some(&self.root),
                    ..Default::default()
                },
            );
            result
                .items
                .iter()
                .map(|file| file.absolute_path(picker, &self.root))
                .filter(|path| is_visible_path(&self.root, path))
                .take(limit)
                .collect()
        })
    }

    /// Record that the user opened `path`. Frecency lives in a database on
    /// disk, so this runs off the UI thread and ignores failures.
    pub fn note_opened(&self, path: &Path) {
        if !path.starts_with(&self.root) {
            return;
        }
        let path = path.to_path_buf();
        let picker = self.picker.clone();
        let frecency = self.frecency.clone();

        std::thread::spawn(move || {
            if let Ok(guard) = frecency.read()
                && let Some(tracker) = guard.as_ref()
            {
                let _ = tracker.track_access(&path);
            }
            if let Ok(mut guard) = picker.write()
                && let Some(picker) = guard.as_mut()
                && let Ok(tracker) = frecency.read()
                && let Some(tracker) = tracker.as_ref()
            {
                // Refresh the in-memory score too, so this file ranks first
                // in the very next Quick Open instead of after a rescan.
                let _ = picker.update_single_file_frecency(&path, tracker);
            }
        });
    }

    /// Remember that `path` was the file picked for `query` in Quick Open, so
    /// the next search for the same text puts it on top.
    pub fn note_query_pick(&self, query: &str, path: &Path) {
        if query.trim().is_empty() || !path.starts_with(&self.root) {
            return;
        }
        let query = query.to_string();
        let path = path.to_path_buf();
        let root = self.root.clone();
        let query_tracker = self.query_tracker.clone();

        std::thread::spawn(move || {
            if let Ok(mut guard) = query_tracker.write()
                && let Some(tracker) = guard.as_mut()
            {
                let _ = tracker.track_query_completion(&query, &root, &path);
            }
        });
    }

    /// Run `f` against the picker, or `None` if it is gone or still scanning.
    fn with_picker<R>(&self, f: impl FnOnce(&FilePicker) -> R) -> Option<R> {
        let guard = self.picker.read().ok()?;
        let picker = guard.as_ref()?;
        if picker.is_scan_active() {
            return None;
        }
        Some(f(picker))
    }
}

/// Project-wide content search, preferring the warm index.
///
/// fff matches inside a single line. A multi-word query may also be a pasted
/// block that the file wraps differently, so when the index finds nothing the
/// on-disk walk's whitespace-collapsed phrase match runs as a fallback. The
/// same fallback keeps search working during the index's first scan.
pub fn search_workspace(index: Option<&SearchIndex>, root: &Path, query: &str) -> SearchResults {
    let from_index = index.and_then(|index| index.search_text(query));
    match from_index {
        Some(results) if !results.matches.is_empty() || query.split_whitespace().count() <= 1 => {
            results
        }
        _ => crate::search_text(root, query),
    }
}

impl Drop for SearchIndex {
    fn drop(&mut self) {
        // Dropping the picker stops its background watcher; the workspace is
        // closed, so nothing should keep watching the old tree.
        if let Ok(mut guard) = self.picker.write() {
            *guard = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "core-workspace-index-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::create_dir_all(dir.join("build")).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, contents: &str) {
        std::fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn content_search_is_ranked_and_skips_ignored_files() {
        let dir = temp_dir("grep");
        write(&dir, "main.dart", "void main() {\n  print('hello world');\n}\n");
        write(&dir.join("lib"), "util.dart", "String greet() => 'Hello World';\n");
        write(
            &dir.join("build"),
            "generated.dart",
            "hello world generated\n",
        );

        let index = SearchIndex::open(&dir, &dir.join(".index")).expect("index should open");
        assert!(index.wait_ready(std::time::Duration::from_secs(20)));

        let results = index.search_text("hello world").unwrap();
        // `build/` is git-ignored territory for fff's walker.
        assert_eq!(results.matches.len(), 2, "got {:?}", results.matches);
        let main = results
            .matches
            .iter()
            .find(|m| m.path.ends_with("main.dart"))
            .expect("main.dart should match");
        assert_eq!(main.line_number, 2);
        assert_eq!(main.line_text, "  print('hello world');");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quick_open_finds_files_fuzzily() {
        let dir = temp_dir("find");
        write(&dir, "pubspec.yaml", "name: demo\n");
        write(&dir.join("lib"), "main_screen.dart", "class MainScreen {}\n");

        let index = SearchIndex::open(&dir, &dir.join(".index")).expect("index should open");
        assert!(index.wait_ready(std::time::Duration::from_secs(20)));

        let files = index.find_files("mainscreen", 10).unwrap();
        assert!(
            files.iter().any(|p| p.ends_with("main_screen.dart")),
            "got {files:?}"
        );
        // An empty Quick Open should list something rather than nothing.
        assert_eq!(index.find_files("", 10).unwrap().len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pasted_block_the_index_cannot_match_still_falls_back_to_the_disk_walk() {
        let dir = temp_dir("phrase");
        write(
            &dir,
            "socket.dart",
            "class Socket {\n  /// A ticker frame captured from the exchange stream\n  /// socket. The exchange sends the ticker fields at the top level.\n  void read() {}\n}\n",
        );

        let index = SearchIndex::open(&dir, &dir.join(".index")).expect("index should open");
        assert!(index.wait_ready(std::time::Duration::from_secs(20)));

        // The block wraps differently in the file; fff's per-line grep cannot
        // see it, so the whitespace-collapsed fallback has to answer.
        let pasted = "/// A ticker frame captured from the exchange stream\n  /// socket. The exchange sends the ticker fields at the top level.";
        let collapsed = pasted.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(index.search_text(&collapsed).unwrap().matches.is_empty());

        let results = search_workspace(Some(index.as_ref()), &dir, &collapsed);
        assert_eq!(results.matches.len(), 1);
        assert_eq!(results.matches[0].line_number, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
