use std::path::{Path, PathBuf};

/// Directory names never worth descending into — matches the file
/// tree's own ignore list (`app::IGNORED_NAMES`, not shared since
/// `core-workspace` has no dependency on `app`) plus `.git`, which the
/// tree never shows at all but a naive recursive search would still
/// walk into otherwise.
const IGNORED_NAMES: &[&str] = &[".git", "target", "node_modules"];

/// One line, in one file, that contains the search text.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchMatch {
    pub path: PathBuf,
    /// 1-indexed, the way every editor shows line numbers.
    pub line_number: usize,
    pub line_text: String,
}

/// Search every text file under `root` for `query` (plain substring,
/// case-insensitive — no regex, matching what Quick Open's own
/// filtering already does), skipping ignored directories and dotfiles.
/// A file that isn't valid UTF-8 (a binary, most likely) is skipped
/// rather than erroring the whole search, same as the file tree
/// already silently skips anything `fs::read_dir` can't look at.
pub fn search_text(root: &Path, query: &str) -> Vec<SearchMatch> {
    if query.is_empty() {
        return Vec::new();
    }
    let query_lower = query.to_lowercase();
    let mut matches = Vec::new();
    search_dir(root, &query_lower, &mut matches);
    matches
}

fn search_dir(dir: &Path, query_lower: &str, matches: &mut Vec<SearchMatch>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') || IGNORED_NAMES.contains(&name) {
            continue;
        }

        if path.is_dir() {
            search_dir(&path, query_lower, matches);
            continue;
        }

        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (ix, line) in text.lines().enumerate() {
            if line.to_lowercase().contains(query_lower) {
                matches.push(SearchMatch {
                    path: path.clone(),
                    line_number: ix + 1,
                    line_text: line.to_string(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, contents: &str) {
        std::fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn finds_real_matches_across_real_files_on_disk() {
        let dir = std::env::temp_dir().join(format!("core-workspace-search-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();

        write(&dir, "main.dart", "void main() {\n  print('hello world');\n}\n");
        write(&dir.join("lib"), "util.dart", "String greet() => 'Hello World';\n");
        write(&dir.join("target"), "ignored.dart", "hello world should not be found here\n");
        write(&dir.join(".git"), "ignored2", "hello world should not be found here either\n");

        let matches = search_text(&dir, "hello world");
        let mut paths: Vec<String> = matches.iter().map(|m| m.path.display().to_string()).collect();
        paths.sort();

        assert_eq!(matches.len(), 2, "expected exactly the two real matches outside ignored dirs, got {matches:?}");
        assert!(paths[0].ends_with("lib/util.dart"));
        assert!(paths[1].ends_with("main.dart"));

        let main_match = matches.iter().find(|m| m.path.ends_with("main.dart")).unwrap();
        assert_eq!(main_match.line_number, 2);
        assert_eq!(main_match.line_text, "  print('hello world');");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn matching_is_case_insensitive() {
        let dir = std::env::temp_dir().join(format!("core-workspace-search-case-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write(&dir, "a.txt", "FooBar\n");

        assert_eq!(search_text(&dir, "foobar").len(), 1);
        assert_eq!(search_text(&dir, "FOOBAR").len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_query_matches_nothing() {
        let dir = std::env::temp_dir().join(format!("core-workspace-search-empty-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write(&dir, "a.txt", "anything at all\n");

        assert!(search_text(&dir, "").is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
