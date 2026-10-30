use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How a line in the working tree compares to `HEAD`, for the gutter's
/// git-change marker — the same three states every editor with this
/// feature shows (an editor's own `+`/`~`/`-` bar, not a full diff view).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineChange {
    /// A line that doesn't exist in `HEAD` at all.
    Added,
    /// A line that exists in both, but its content differs.
    Modified,
    /// No line here is new or changed — but one or more lines that used
    /// to come right before this one were deleted.
    Removed,
}

/// How many commits the current branch is ahead/behind its upstream, as
/// `(ahead, behind)` — `None` if `dir` isn't a git repo or the current
/// branch has no upstream configured (a fresh local-only branch, say).
pub fn ahead_behind(dir: &Path) -> Option<(u32, u32)> {
    let output = Command::new("git")
        .args(["rev-list", "--left-right", "--count", "@{upstream}...HEAD"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let (behind, ahead) = text.trim().split_once('\t')?;
    Some((ahead.parse().ok()?, behind.parse().ok()?))
}

/// Every changed line in `file`'s current working-tree contents versus
/// `HEAD`, as 0-indexed line numbers (matching the rest of this
/// codebase's convention — LSP diagnostics included). Empty if `file`
/// isn't in a git repo, isn't tracked, or has no differences.
pub fn line_changes(file: &Path) -> HashMap<u32, LineChange> {
    let mut changes = HashMap::new();
    let Some(dir) = file.parent() else {
        return changes;
    };
    let Ok(output) = Command::new("git")
        .args(["diff", "--no-color", "--unified=0", "HEAD", "--"])
        .arg(file)
        .current_dir(dir)
        .output()
    else {
        return changes;
    };
    if !output.status.success() {
        return changes;
    }

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some(header) = line.strip_prefix("@@ ") else {
            continue;
        };
        let Some((old_count, new_start, new_count)) = parse_hunk_header(header) else {
            continue;
        };

        if new_count == 0 {
            // A pure deletion: no new line was added, so the marker goes
            // on whichever new-file line now sits where the old content
            // used to be (clamped to 1 — a deletion at the very start of
            // the file still has to land on a real line).
            changes.insert(new_start.max(1) - 1, LineChange::Removed);
            continue;
        }

        let status = if old_count == 0 { LineChange::Added } else { LineChange::Modified };
        for changed_line in new_start..new_start + new_count {
            changes.insert(changed_line - 1, status);
        }
    }

    changes
}

/// Parses a unified-diff hunk header's body (the text after `"@@ "`, up
/// to and including the closing `"@@"` — any trailing function context
/// is ignored) into `(old_line_count, new_start_line, new_line_count)`.
fn parse_hunk_header(header: &str) -> Option<(u32, u32, u32)> {
    let mut parts = header.split_whitespace();
    let (_, old_count) = parse_hunk_range(parts.next()?.strip_prefix('-')?)?;
    let (new_start, new_count) = parse_hunk_range(parts.next()?.strip_prefix('+')?)?;
    Some((old_count, new_start, new_count))
}

/// `"12,3"` -> `(12, 3)`; `"12"` (count omitted means exactly one line,
/// per the unified diff format) -> `(12, 1)`.
fn parse_hunk_range(range: &str) -> Option<(u32, u32)> {
    match range.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((range.parse().ok()?, 1)),
    }
}

/// The current branch name for the git repo at (or containing) `dir`,
/// or `None` if `dir` isn't inside a git repo, has no commits yet, or
/// HEAD is detached (not actually on a branch).
pub fn current_branch(dir: &Path) -> Option<String> {
    let output = Command::new("git").args(["rev-parse", "--abbrev-ref", "HEAD"]).current_dir(dir).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // `--abbrev-ref HEAD` prints the literal string "HEAD" when detached
    // (checked out a commit/tag directly, not a branch) — not a real
    // branch name to show.
    if branch.is_empty() || branch == "HEAD" { None } else { Some(branch) }
}

/// Whether the git repo at (or containing) `dir` has any uncommitted
/// changes — staged, unstaged, or untracked. `false` for a directory
/// that isn't a git repo at all, same as a clean one; callers that
/// care about the difference should check [`current_branch`] first.
pub fn has_uncommitted_changes(dir: &Path) -> bool {
    let Ok(output) = Command::new("git").args(["status", "--porcelain"]).current_dir(dir).output() else {
        return false;
    };
    output.status.success() && !output.stdout.is_empty()
}

/// Per-path working-tree status for the repo containing `dir`, keyed by
/// absolute path: `U` untracked, `M` modified, `A` added, `D` deleted,
/// `R` renamed. Empty for a clean tree or a directory outside git — the
/// file tree uses it for its per-row status badge.
pub fn status_map(dir: &Path) -> HashMap<PathBuf, char> {
    let mut map = HashMap::new();

    let Ok(prefix) = Command::new("git")
        .args(["rev-parse", "--show-prefix"])
        .current_dir(dir)
        .output()
    else {
        return map;
    };
    if !prefix.status.success() {
        return map;
    }
    // `status --porcelain` reports paths relative to the repo root, not
    // to `dir`. `--show-prefix` gives how far `dir` sits below that root
    // (e.g. `""` or `"crates/app/"`), so stripping it lets us rebuild ids
    // under `dir` itself — no canonicalization, so no symlink surprises
    // (`/var` vs `/private/var`) between these keys and the tree's own.
    let prefix = String::from_utf8_lossy(&prefix.stdout).trim().to_string();

    let Ok(output) = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .current_dir(dir)
        .output()
    else {
        return map;
    };
    if !output.status.success() {
        return map;
    }

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if line.len() < 4 {
            continue;
        }
        let xy = &line[..2];
        let raw = &line[3..];
        // Renames are `R  old -> new`; keep the destination.
        let raw = raw.rsplit(" -> ").next().unwrap_or(raw).trim_matches('"');
        let (x, y) = (xy.as_bytes()[0] as char, xy.as_bytes()[1] as char);
        // Worktree state wins over index state; `??` is untracked.
        let letter = if x == '?' {
            'U'
        } else if y != ' ' {
            y
        } else {
            x
        };
        let relative = raw.strip_prefix(prefix.as_str()).unwrap_or(raw);
        map.insert(dir.join(relative), letter);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git").args(args).current_dir(dir).status().expect("git should run");
        assert!(status.success(), "git {args:?} failed in {dir:?}");
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "--quiet"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("a.txt"), "hello").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "--quiet", "-m", "initial"]);
    }

    #[test]
    fn maps_modified_and_untracked_paths() {
        let dir = std::env::temp_dir()
            .join(format!("core-workspace-status-map-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        init_repo(&dir);

        std::fs::write(dir.join("a.txt"), "changed").unwrap();
        std::fs::write(dir.join("new.txt"), "new").unwrap();

        let map = status_map(&dir);
        assert_eq!(map.get(&dir.join("a.txt")), Some(&'M'));
        assert_eq!(map.get(&dir.join("new.txt")), Some(&'U'));
    }

    #[test]
    fn counts_commits_ahead_of_a_local_upstream() {
        let dir = std::env::temp_dir().join(format!("core-workspace-ahead-behind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        init_repo(&dir);

        // A plain local branch standing in for a remote-tracking one —
        // `@{upstream}` doesn't care which kind it is.
        git(&dir, &["branch", "base"]);
        git(&dir, &["branch", "--set-upstream-to=base"]);
        assert_eq!(ahead_behind(&dir), Some((0, 0)));

        std::fs::write(dir.join("a.txt"), "changed").unwrap();
        git(&dir, &["commit", "--quiet", "-am", "second"]);
        assert_eq!(ahead_behind(&dir), Some((1, 0)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_upstream_reports_none() {
        let dir = std::env::temp_dir().join(format!("core-workspace-no-upstream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        init_repo(&dir);

        assert_eq!(ahead_behind(&dir), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_the_real_branch_of_a_real_repo() {
        let dir = std::env::temp_dir().join(format!("core-workspace-git-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        init_repo(&dir);

        // Whatever the default branch is called locally (master or
        // main depending on the machine's git config), it should come
        // back as a real name, not None.
        let branch = current_branch(&dir).expect("a fresh commit should be on a real branch");
        assert!(!branch.is_empty());

        git(&dir, &["checkout", "--quiet", "-b", "feature/widget-inspector"]);
        assert_eq!(current_branch(&dir), Some("feature/widget-inspector".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detached_head_reports_no_branch() {
        let dir = std::env::temp_dir().join(format!("core-workspace-git-detached-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        init_repo(&dir);

        let output = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&dir).output().unwrap();
        let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
        git(&dir, &["checkout", "--quiet", &commit]);

        assert_eq!(current_branch(&dir), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn not_a_git_repo_reports_no_branch() {
        let dir = std::env::temp_dir().join(format!("core-workspace-not-a-repo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        assert_eq!(current_branch(&dir), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_real_uncommitted_changes() {
        let dir = std::env::temp_dir().join(format!("core-workspace-dirty-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        init_repo(&dir);

        assert!(!has_uncommitted_changes(&dir), "a fresh commit should be clean");

        std::fs::write(dir.join("a.txt"), "changed").unwrap();
        assert!(has_uncommitted_changes(&dir), "an edited tracked file should be dirty");

        git(&dir, &["checkout", "--quiet", "--", "a.txt"]);
        assert!(!has_uncommitted_changes(&dir), "reverting the edit should be clean again");

        std::fs::write(dir.join("untracked.txt"), "new").unwrap();
        assert!(has_uncommitted_changes(&dir), "an untracked file should count as dirty too");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod line_changes_tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git").args(args).current_dir(dir).status().expect("git should run");
        assert!(status.success(), "git {args:?} failed in {dir:?}");
    }

    fn init_repo_with(dir: &Path, contents: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "--quiet"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
        let file = dir.join("a.txt");
        std::fs::write(&file, contents).unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "--quiet", "-m", "initial"]);
        file
    }

    #[test]
    fn a_purely_appended_line_is_added() {
        let dir = std::env::temp_dir().join(format!("core-workspace-linechanges-added-{}", std::process::id()));
        let file = init_repo_with(&dir, "one\ntwo\n");
        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();

        let changes = line_changes(&file);
        assert_eq!(changes.get(&2), Some(&LineChange::Added), "got {changes:?}");
        assert_eq!(changes.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_edited_line_is_modified() {
        let dir = std::env::temp_dir().join(format!("core-workspace-linechanges-modified-{}", std::process::id()));
        let file = init_repo_with(&dir, "one\ntwo\nthree\n");
        std::fs::write(&file, "one\nTWO\nthree\n").unwrap();

        let changes = line_changes(&file);
        assert_eq!(changes.get(&1), Some(&LineChange::Modified), "got {changes:?}");
        assert_eq!(changes.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_line_deleted_with_nothing_else_changed_marks_the_gap() {
        let dir = std::env::temp_dir().join(format!("core-workspace-linechanges-removed-{}", std::process::id()));
        let file = init_repo_with(&dir, "one\ntwo\nthree\nfour\n");
        std::fs::write(&file, "one\ntwo\nfour\n").unwrap();

        let changes = line_changes(&file);
        // "three" (old line 3) is gone; the marker lands on the new-file
        // line now sitting in that gap, which is "four" at new line 2
        // (0-indexed 1).
        assert_eq!(changes.get(&1), Some(&LineChange::Removed), "got {changes:?}");
        assert_eq!(changes.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clean_file_has_no_changes() {
        let dir = std::env::temp_dir().join(format!("core-workspace-linechanges-clean-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "--quiet"]);
        git(&dir, &["config", "user.email", "test@example.com"]);
        git(&dir, &["config", "user.name", "Test"]);
        git(&dir, &["config", "commit.gpgsign", "false"]);
        let file = dir.join("a.txt");
        std::fs::write(&file, "unchanged\n").unwrap();
        git(&dir, &["add", "a.txt"]);
        git(&dir, &["commit", "--quiet", "-m", "initial"]);

        assert!(line_changes(&file).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
