//! Per-file line counts (`+added −removed`) for the change lists, so a
//! change can be judged before its diff is opened.

use std::collections::HashMap;
use std::path::Path;

use crate::repo::{ChangeKind, ChangedFile, git};

/// Files bigger than this are not read to count the lines of an untracked file.
const MAX_UNTRACKED_BYTES: u64 = 1024 * 1024;

/// `(added, removed)` line counts keyed by repo-relative path, for everything
/// that differs from `HEAD` (staged and unstaged together) plus the line
/// count of each untracked file in `untracked`.
pub fn change_stats(root: &Path, repo_root: &Path, untracked: &[ChangedFile]) -> HashMap<String, (u32, u32)> {
    let mut stats = git(
        root,
        &["-c", "core.quotepath=false", "diff", "HEAD", "--numstat", "--no-renames"],
        None,
    )
    .map(|out| parse_numstat(&out))
    .unwrap_or_default();

    for file in untracked.iter().filter(|f| f.kind == ChangeKind::Untracked) {
        if let Some(lines) = count_lines(&repo_root.join(&file.path)) {
            stats.insert(file.path.clone(), (lines, 0));
        }
    }
    stats
}

/// Parse `git diff --numstat` output (`added<TAB>removed<TAB>path`). Binary
/// files print `-` for both counts and are recorded as `(0, 0)`.
pub fn parse_numstat(output: &str) -> HashMap<String, (u32, u32)> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let added = parts.next()?;
            let removed = parts.next()?;
            let path = parts.next()?.to_string();
            Some((path, (added.parse().unwrap_or(0), removed.parse().unwrap_or(0))))
        })
        .collect()
}

/// Number of lines in a small text file; `None` for binary, unreadable or large files.
fn count_lines(path: &Path) -> Option<u32> {
    if std::fs::metadata(path).ok()?.len() > MAX_UNTRACKED_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    let newlines = bytes.iter().filter(|&&b| b == b'\n').count() as u32;
    let unterminated = u32::from(!bytes.is_empty() && bytes.last() != Some(&b'\n'));
    Some(newlines + unterminated)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    #[test]
    fn parses_numstat_including_binary_files() {
        let stats = parse_numstat("3\t1\tlib/main.dart\n-\t-\tassets/icon.png\n10\t0\tREADME.md\n");
        assert_eq!(stats["lib/main.dart"], (3, 1));
        assert_eq!(stats["assets/icon.png"], (0, 0));
        assert_eq!(stats["README.md"], (10, 0));
    }

    #[test]
    fn counts_lines_with_and_without_a_trailing_newline() {
        let dir = std::env::temp_dir().join(format!("stats-lines-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("b.txt"), "one\ntwo").unwrap();
        std::fs::write(dir.join("c.bin"), [0u8, 1, 2]).unwrap();
        assert_eq!(count_lines(&dir.join("a.txt")), Some(2));
        assert_eq!(count_lines(&dir.join("b.txt")), Some(2));
        assert_eq!(count_lines(&dir.join("c.bin")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stats_from_a_real_repository() {
        let dir = std::env::temp_dir().join(format!("stats-repo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            assert!(Command::new("git").args(args).current_dir(&dir).status().unwrap().success());
        };
        run(&["init", "--quiet"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "T"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("a.txt"), "1\n2\n3\n").unwrap();
        run(&["add", "a.txt"]);
        run(&["commit", "--quiet", "-m", "init"]);

        // Modify (one line replaced, one added) and add an untracked file.
        std::fs::write(dir.join("a.txt"), "1\nTWO\n3\n4\n").unwrap();
        std::fs::write(dir.join("new.txt"), "x\ny\n").unwrap();
        let untracked = vec![ChangedFile {
            path: "new.txt".into(),
            orig_path: None,
            kind: ChangeKind::Untracked,
        }];
        let stats = change_stats(&dir, &dir, &untracked);
        assert_eq!(stats["a.txt"], (2, 1));
        assert_eq!(stats["new.txt"], (2, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
