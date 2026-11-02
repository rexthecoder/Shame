use std::path::{Path, PathBuf};

/// Directory names never worth descending into — matches the file
/// tree's own ignore list (`app::IGNORED_NAMES`, not shared since
/// `core-workspace` has no dependency on `app`).
const IGNORED_NAMES: &[&str] = &[
    ".git",
    "target",
    "dist",
    "node_modules",
    ".dart_tool",
    "build",
    "Pods",
    "DerivedData",
    ".gradle",
    ".next",
    "__pycache__",
    ".venv",
    "Library",
    ".Trash",
];

/// Files larger than this are generated or vendored, not source.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Stop after this many hits. The panel only shows the first 150, but a
/// common query in a monorepo can otherwise produce a million matches and
/// stall the UI.
const MAX_MATCHES: usize = 5_000;

const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "ico", "bmp", "tiff", "psd", "mp3", "mp4", "mov", "avi",
    "mkv", "webm", "wav", "flac", "ogg", "zip", "gz", "tgz", "bz2", "xz", "rar", "7z", "jar",
    "class", "so", "dylib", "dll", "exe", "wasm", "pdf", "ttf", "otf", "woff", "woff2", "eot",
];

/// One line, in one file, that contains the search text.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchMatch {
    pub path: PathBuf,
    /// 1-indexed, the way every editor shows line numbers.
    pub line_number: usize,
    pub line_text: String,
    /// The query was a multi-word phrase, so it was matched with whitespace
    /// collapsed; the editor's single-line find bar cannot use it directly.
    pub phrase: bool,
}

/// Everything one search found, plus whether the cap was reached.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchResults {
    pub matches: Vec<SearchMatch>,
    pub truncated: bool,
}

/// Search every text file under `root` for `query`. Matching is a
/// case-insensitive substring search, and a query with several lines matches
/// those lines in order — pasting a block of code or a doc comment works the
/// way people expect.
pub fn search_text(root: &Path, query: &str) -> SearchResults {
    let Some(query) = Query::new(query) else {
        return SearchResults::default();
    };
    let mut files = Vec::new();
    collect_files(root, &mut files);
    search_files_with(&files, &query)
}

struct Query {
    /// One whitespace-free token, matched within a single line.
    single_token: Option<String>,
    /// Several words, lowercased with every whitespace run collapsed to one
    /// space, so it matches regardless of line wrapping or indentation.
    phrase: Option<String>,
    /// The longest word, used to cheaply reject files before normalizing them.
    longest_word: String,
}

impl Query {
    fn new(query: &str) -> Option<Self> {
        let words: Vec<String> = query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        if words.is_empty() {
            return None;
        }
        let longest_word = words
            .iter()
            .max_by_key(|word| word.len())
            .cloned()
            .unwrap_or_default();
        let (single_token, phrase) = if words.len() == 1 {
            (words.first().cloned(), None)
        } else {
            (None, Some(words.join(" ")))
        };
        Some(Self {
            single_token,
            phrase,
            longest_word,
        })
    }
}

fn search_files_with(paths: &[PathBuf], query: &Query) -> SearchResults {
    // A monorepo file list is thousands of small reads; one core leaves the
    // result panel blank for about a second. Spread those reads across cores,
    // then sort once so the list is stable.
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8);
    if paths.len() < 256 || threads == 1 {
        return search_files_sequential(paths, query);
    }

    let cursor = std::sync::atomic::AtomicUsize::new(0);
    let found = std::sync::atomic::AtomicUsize::new(0);
    let mut matches = Vec::new();
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(threads);
        for _ in 0..threads {
            handles.push(scope.spawn(|| {
                let mut local = Vec::new();
                loop {
                    if found.load(std::sync::atomic::Ordering::Relaxed) >= MAX_MATCHES {
                        break;
                    }
                    let Some(path) = paths.get(cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                    else {
                        break;
                    };
                    let file_matches = search_one_file(path, query);
                    if !file_matches.is_empty() {
                        found.fetch_add(
                            file_matches.len(),
                            std::sync::atomic::Ordering::Relaxed,
                        );
                        local.extend(file_matches);
                    }
                }
                local
            }));
        }
        for handle in handles {
            if let Ok(mut local) = handle.join() {
                matches.append(&mut local);
            }
        }
    });

    let truncated = matches.len() >= MAX_MATCHES;
    matches.truncate(MAX_MATCHES);
    sort_matches(&mut matches);
    SearchResults { matches, truncated }
}

fn search_files_sequential(paths: &[PathBuf], query: &Query) -> SearchResults {
    let mut matches = Vec::new();
    for path in paths {
        matches.extend(search_one_file(path, query));
        if matches.len() >= MAX_MATCHES {
            break;
        }
    }
    let truncated = matches.len() >= MAX_MATCHES;
    matches.truncate(MAX_MATCHES);
    sort_matches(&mut matches);
    SearchResults { matches, truncated }
}

fn search_one_file(path: &Path, query: &Query) -> Vec<SearchMatch> {
    if !is_searchable_file(path) {
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let phrase = query.phrase.is_some();
    matches_in_text(&text, query, MAX_MATCHES)
        .into_iter()
        .map(|(line_ix, line_text)| SearchMatch {
            path: path.to_path_buf(),
            line_number: line_ix + 1,
            line_text,
            phrase,
        })
        .collect()
}

fn sort_matches(matches: &mut [SearchMatch]) {
    matches.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| a.line_number.cmp(&b.line_number))
    });
}

/// The 0-indexed lines of `text` that match `query`. A single-line query
/// matches within a line; a multi-line query is matched after collapsing
/// whitespace, so a pasted block still matches when the file wraps it
/// differently or indents it.
fn matches_in_text(text: &str, query: &Query, limit: usize) -> Vec<(usize, String)> {
    let lines: Vec<&str> = text.lines().collect();
    if let Some(needle) = &query.single_token {
        return lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.to_lowercase().contains(needle))
            .map(|(ix, line)| (ix, (*line).to_string()))
            .take(limit)
            .collect();
    }

    let Some(needle) = &query.phrase else {
        return Vec::new();
    };
    // Most files contain none of the query's distinctive words; normalizing
    // them would mean allocating a second copy of the whole file.
    if !contains_word(text, &query.longest_word) {
        return Vec::new();
    }

    let (haystack, line_starts) = normalize_whitespace(text);
    let mut out = Vec::new();
    for (at, _) in haystack.match_indices(needle) {
        let Some(line) = line_for_offset(&line_starts, at) else {
            continue;
        };
        if let Some(text) = lines.get(line) {
            out.push((line, (*text).to_string()));
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

fn contains_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return true;
    }
    if word.is_ascii() {
        let needle = word.as_bytes();
        return text
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle));
    }
    text.to_lowercase().contains(word)
}

/// Collapse every whitespace run to one space and lowercase the text, keeping
/// `(normalized_byte_offset, original_line)` for the first content on each
/// line. Offsets let a match be reported against the line it starts on.
fn normalize_whitespace(text: &str) -> (String, Vec<(usize, usize)>) {
    let mut out = String::with_capacity(text.len());
    let mut line_starts = Vec::new();
    let mut line = 0usize;
    let mut at_line_start = true;
    let mut pending_space = false;

    for ch in text.chars() {
        if ch == '\n' {
            line += 1;
            at_line_start = true;
            pending_space = true;
            continue;
        }
        if ch.is_whitespace() {
            pending_space = true;
            continue;
        }

        if pending_space && !out.is_empty() {
            out.push(' ');
        }
        if at_line_start {
            line_starts.push((out.len(), line));
            at_line_start = false;
        }
        pending_space = false;
        for lower in ch.to_lowercase() {
            out.push(lower);
        }
    }
    (out, line_starts)
}

fn line_for_offset(line_starts: &[(usize, usize)], offset: usize) -> Option<usize> {
    let ix = line_starts
        .partition_point(|(start, _)| *start <= offset)
        .checked_sub(1)?;
    line_starts.get(ix).map(|(_, line)| *line)
}

fn is_searchable_file(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && BINARY_EXTENSIONS
            .iter()
            .any(|binary| ext.eq_ignore_ascii_case(binary))
    {
        return false;
    }
    path.metadata()
        .map(|meta| meta.is_file() && meta.len() <= MAX_FILE_BYTES)
        .unwrap_or(false)
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // A symlink can point back into the tree or outside it; the file tree
        // does not follow them either.
        if file_type.is_symlink() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') || IGNORED_NAMES.contains(&name) {
            continue;
        }

        if file_type.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Replace every case-insensitive occurrence of `query` with `replacement`
/// in every text file under `root` (same walk and ignore rules as
/// [`search_text`]). Returns `(file, replacement_count)` for each file that
/// changed. Files whose lowercase form changes byte length (rare Unicode
/// case folding) are matched case-sensitively instead of risking a bad
/// offset.
pub fn replace_text(
    root: &Path,
    query: &str,
    replacement: &str,
) -> std::io::Result<Vec<(PathBuf, usize)>> {
    let mut changed = Vec::new();
    if query.trim().is_empty() {
        return Ok(changed);
    }
    if query.contains(['\n', '\r']) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Replace one line at a time, not a multi-line block.",
        ));
    }

    let mut files = Vec::new();
    collect_files(root, &mut files);
    for path in files {
        if !is_searchable_file(&path) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (updated, count) = replace_in_text(&text, query, replacement);
        if count > 0 {
            std::fs::write(&path, updated)?;
            changed.push((path, count));
        }
    }
    Ok(changed)
}

fn replace_in_text(text: &str, query: &str, replacement: &str) -> (String, usize) {
    let lower = text.to_lowercase();
    let query_lower = query.to_lowercase();
    let (haystack, needle) = if lower.len() == text.len() && query_lower.len() == query.len() {
        (lower.as_str(), query_lower.as_str())
    } else {
        (text, query)
    };
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut count = 0;
    for (at, _) in haystack.match_indices(needle) {
        if !text.is_char_boundary(at) || !text.is_char_boundary(at + needle.len()) {
            continue;
        }
        out.push_str(&text[last..at]);
        out.push_str(replacement);
        last = at + needle.len();
        count += 1;
    }
    out.push_str(&text[last..]);
    (out, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, contents: &str) {
        std::fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn finds_real_matches_across_real_files_on_disk() {
        let dir = std::env::temp_dir().join(format!(
            "core-workspace-search-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::create_dir_all(dir.join("build")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();

        write(&dir, "main.dart", "void main() {\n  print('hello world');\n}\n");
        write(&dir.join("lib"), "util.dart", "String greet() => 'Hello World';\n");
        write(
            &dir.join("target"),
            "ignored.dart",
            "hello world should not be found here\n",
        );
        write(
            &dir.join("build"),
            "ignored.dart",
            "hello world should not be found here either\n",
        );
        write(
            &dir.join(".git"),
            "ignored2",
            "hello world should not be found here at all\n",
        );

        let results = search_text(&dir, "hello world");
        assert_eq!(
            results.matches.len(),
            2,
            "expected exactly the two real matches outside ignored dirs, got {results:?}"
        );
        assert!(!results.truncated);

        let main_match = results
            .matches
            .iter()
            .find(|m| m.path.ends_with("main.dart"))
            .unwrap();
        assert_eq!(main_match.line_number, 2);
        assert_eq!(main_match.line_text, "  print('hello world');");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn matching_is_case_insensitive() {
        let dir = std::env::temp_dir().join(format!(
            "core-workspace-search-case-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        write(&dir, "a.txt", "FooBar\n");

        assert_eq!(search_text(&dir, "foobar").matches.len(), 1);
        assert_eq!(search_text(&dir, "FOOBAR").matches.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pasted_multi_line_comment_matches_despite_indentation_and_blank_lines() {
        let dir = std::env::temp_dir().join(format!(
            "core-workspace-search-multiline-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        write(
            &dir,
            "socket.dart",
            "class Socket {\n  /// A ticker frame captured from the exchange stream socket.\n\n  /// The exchange sends the ticker fields at the top level.\n  void read() {}\n}\n",
        );

        // The pasted block wraps differently from the file on disk; whitespace
        // is collapsed before matching, so that must not matter.
        let results = search_text(
            &dir,
            "/// A ticker frame captured from the exchange stream\nsocket.  /// The exchange sends the ticker fields\nat the top level.",
        );
        assert_eq!(results.matches.len(), 1);
        assert_eq!(results.matches[0].line_number, 2);
        assert!(results.matches[0].line_text.contains("A ticker frame"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_paste_that_lost_its_newlines_still_matches_a_phrase() {
        let dir = std::env::temp_dir().join(format!(
            "core-workspace-search-paste-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        write(
            &dir,
            "socket.dart",
            "class Socket {\n  /// A ticker frame captured from the exchange stream socket. The exchange sends\n  /// the ticker fields at the top level of the frame, next to `channel` and\n  /// `market`, rather than nesting them under `data`.\n}\n",
        );

        // The search box is single-line and used to delete pasted newlines:
        // `sends///` was glued together. The paste handler collapses the block
        // into words instead, which is what this simulates.
        let pasted = "/// A ticker frame captured from the exchange stream socket. The exchange sends\n/// the ticker fields at the top level of the frame, next to `channel` and\n/// `market`, rather than nesting them under `data`.";
        let collapsed = pasted.split_whitespace().collect::<Vec<_>>().join(" ");
        let results = search_text(&dir, &collapsed);
        assert_eq!(results.matches.len(), 1);
        assert!(results.matches[0].phrase);
        assert_eq!(results.matches[0].line_number, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_query_matches_nothing() {
        let dir = std::env::temp_dir().join(format!(
            "core-workspace-search-empty-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        write(&dir, "a.txt", "anything at all\n");

        assert!(search_text(&dir, "").matches.is_empty());
        assert!(search_text(&dir, " \n\t ").matches.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_in_text_is_case_insensitive_and_counts() {
        let (out, n) = replace_in_text("Foo foo FOO bar", "foo", "x");
        assert_eq!(out, "x x x bar");
        assert_eq!(n, 3);
    }

    #[test]
    fn replace_text_rewrites_files_and_skips_ignored_dirs() {
        let dir = std::env::temp_dir().join(format!(
            "core-workspace-replace-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::create_dir_all(dir.join("build")).unwrap();
        write(&dir, "a.dart", "old name, Old Name\n");
        write(&dir.join("target"), "b.dart", "old name\n");
        write(&dir.join("build"), "c.dart", "old name\n");
        let changed = replace_text(&dir, "old name", "new").unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].1, 2);
        assert_eq!(
            std::fs::read_to_string(dir.join("a.dart")).unwrap(),
            "new, new\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("target/b.dart")).unwrap(),
            "old name\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_text_rejects_a_multi_line_query() {
        let dir = std::env::temp_dir().join(format!(
            "core-workspace-replace-multiline-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        write(&dir, "a.txt", "one\ntwo\n");

        assert!(replace_text(&dir, "one\ntwo", "x").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
