//! Checkpoints of the workspace around each agent turn.
//!
//! Before a turn starts, the whole working tree (tracked changes and untracked
//! files, minus what `.gitignore` hides) is written into git's object store as
//! a tree, using a throwaway index. Nothing the user sees changes: not the
//! index, not the branch, not the stash. When the turn ends a second tree is
//! taken, and the difference between the two is exactly what that turn did.
//!
//! That makes three things possible:
//! - a per-turn "Changed 3 files" card whose diffs show only the agent's work,
//!   even in files the user had already edited;
//! - undoing one turn, or one file of it, back to how it was before the turn
//!   (not back to HEAD, which would also throw away the user's own edits);
//! - rewinding the workspace to before any earlier message.
//!
//! Trees are unreferenced objects, so `git gc` eventually prunes old ones;
//! restoring from a pruned checkpoint fails cleanly.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The state of the workspace at one moment, as a git tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Checkpoint {
    /// The repository's top level; every path below is relative to it.
    pub(crate) repo: PathBuf,
    pub(crate) tree: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileChange {
    Added,
    Modified,
    Deleted,
}

impl FileChange {
    pub(crate) fn letter(self) -> &'static str {
        match self {
            FileChange::Added => "A",
            FileChange::Modified => "M",
            FileChange::Deleted => "D",
        }
    }
}

/// One file a turn changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TurnFile {
    /// Relative to the repository top level.
    pub(crate) path: String,
    pub(crate) change: FileChange,
    pub(crate) added: u32,
    pub(crate) removed: u32,
    /// Put back to how it was before the turn.
    pub(crate) reverted: bool,
}

fn git(repo: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo)
        .args(["-c", "core.quotepath=off"])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

fn stdout_of(mut cmd: Command) -> Option<String> {
    let out = cmd.stdout(Stdio::piped()).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The git top level containing `dir`, if it is in a repository.
pub(crate) fn repo_root(dir: &Path) -> Option<PathBuf> {
    let mut cmd = git(dir);
    cmd.args(["rev-parse", "--show-toplevel"]);
    stdout_of(cmd).filter(|s| !s.is_empty()).map(PathBuf::from)
}

/// Write the working tree of the repository containing `dir` as a tree object.
pub(crate) fn snapshot(dir: &Path) -> Option<Checkpoint> {
    let repo = repo_root(dir)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("shame-ckpt-{}-{nanos}", std::process::id()));
    // Start from a copy of the real index so files that haven't changed are
    // not hashed again: on a large repository this is the difference between
    // milliseconds and seconds.
    let mut index_cmd = git(&repo);
    index_cmd.args(["rev-parse", "--git-path", "index"]);
    if let Some(index) = stdout_of(index_cmd) {
        let index = PathBuf::from(index);
        let index = if index.is_absolute() { index } else { repo.join(index) };
        let _ = std::fs::copy(index, &tmp);
    }
    let mut add = git(&repo);
    add.env("GIT_INDEX_FILE", &tmp)
        .args(["add", "-A", "--", "."])
        .stdout(Stdio::null());
    let added = add.status().is_ok_and(|s| s.success());
    let tree = added
        .then(|| {
            let mut write = git(&repo);
            write.env("GIT_INDEX_FILE", &tmp).arg("write-tree");
            stdout_of(write)
        })
        .flatten()
        .filter(|t| !t.is_empty());
    let _ = std::fs::remove_file(&tmp);
    tree.map(|tree| Checkpoint { repo, tree })
}

/// What changed between two trees of the same repository.
pub(crate) fn changed_files(repo: &Path, before: &str, after: &str) -> Vec<TurnFile> {
    if before == after {
        return Vec::new();
    }
    let mut status_cmd = git(repo);
    status_cmd.args(["diff", "--no-renames", "--name-status", before, after]);
    let mut numstat_cmd = git(repo);
    numstat_cmd.args(["diff", "--no-renames", "--numstat", before, after]);
    let numstat = stdout_of(numstat_cmd).unwrap_or_default();
    parse_changes(&stdout_of(status_cmd).unwrap_or_default(), &numstat)
}

/// Combine `git diff --name-status` and `--numstat` output.
pub(crate) fn parse_changes(name_status: &str, numstat: &str) -> Vec<TurnFile> {
    let counts: std::collections::HashMap<&str, (u32, u32)> = numstat
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let added = parts.next()?.parse().unwrap_or(0);
            let removed = parts.next()?.parse().unwrap_or(0);
            Some((parts.next()?, (added, removed)))
        })
        .collect();
    name_status
        .lines()
        .filter_map(|line| {
            let (status, path) = line.split_once('\t')?;
            let change = match status.chars().next()? {
                'A' => FileChange::Added,
                'D' => FileChange::Deleted,
                _ => FileChange::Modified,
            };
            let (added, removed) = counts.get(path).copied().unwrap_or((0, 0));
            Some(TurnFile { path: path.to_string(), change, added, removed, reverted: false })
        })
        .collect()
}

/// Put `paths` back to how they are in `checkpoint`: rewritten from the tree
/// when they exist there, deleted when they don't.
pub(crate) fn restore(checkpoint: &Checkpoint, paths: &[String]) -> Result<usize, String> {
    let mut restored = 0;
    for path in paths {
        let full = checkpoint.repo.join(path);
        let mut ls = git(&checkpoint.repo);
        ls.args(["ls-tree", &checkpoint.tree, "--", path]);
        let entry = stdout_of(ls).ok_or_else(|| {
            "That checkpoint is no longer in the repository (git may have pruned it).".to_string()
        })?;
        if entry.is_empty() {
            // Not in the checkpoint: the file was created after it.
            if full.is_file() {
                std::fs::remove_file(&full).map_err(|e| format!("{path}: {e}"))?;
            }
            restored += 1;
            continue;
        }
        // `<mode> blob <sha>\t<path>`
        let mut fields = entry.split_whitespace();
        let mode = fields.next().unwrap_or("100644");
        let kind = fields.next().unwrap_or("blob");
        if kind != "blob" || mode == "120000" {
            continue; // submodules and symlinks are left alone
        }
        let out = git(&checkpoint.repo)
            .args(["cat-file", "blob", &format!("{}:{path}", checkpoint.tree)])
            .stdout(Stdio::piped())
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!("{path}: could not read it from the checkpoint"));
        }
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{path}: {e}"))?;
        }
        std::fs::write(&full, &out.stdout).map_err(|e| format!("{path}: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let bits = if mode == "100755" { 0o755 } else { 0o644 };
            let _ = std::fs::set_permissions(&full, std::fs::Permissions::from_mode(bits));
        }
        restored += 1;
    }
    Ok(restored)
}

/// Bring the whole workspace back to `checkpoint`: every file that differs
/// from it now is restored. Returns how many files changed.
pub(crate) fn rewind_to(checkpoint: &Checkpoint) -> Result<usize, String> {
    let now = snapshot(&checkpoint.repo).ok_or("Could not read the workspace state.")?;
    let paths: Vec<String> = changed_files(&checkpoint.repo, &checkpoint.tree, &now.tree)
        .into_iter()
        .map(|f| f.path)
        .collect();
    restore(checkpoint, &paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn parses_name_status_with_counts() {
        let files = parse_changes(
            "M\tsrc/a.rs\nA\tnew.txt\nD\told.md\n",
            "3\t1\tsrc/a.rs\n2\t0\tnew.txt\n0\t9\told.md\n",
        );
        assert_eq!(files.len(), 3);
        assert_eq!((files[0].change, files[0].added, files[0].removed), (FileChange::Modified, 3, 1));
        assert_eq!(files[1].change, FileChange::Added);
        assert_eq!((files[2].change, files[2].removed), (FileChange::Deleted, 9));
    }

    #[test]
    fn snapshots_diff_and_restore_without_touching_the_index() {
        let dir = std::env::temp_dir().join(format!("shame-ckpt-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q"]);
        run(&dir, &["config", "user.email", "t@example.com"]);
        run(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        run(&dir, &["add", "a.txt"]);
        run(&dir, &["commit", "-qm", "init"]);
        // The user's own uncommitted edit, which an undo must keep.
        std::fs::write(dir.join("a.txt"), "one\nuser\n").unwrap();

        let before = snapshot(&dir).expect("snapshot");
        std::fs::write(dir.join("a.txt"), "one\nuser\nagent\n").unwrap();
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        let after = snapshot(&dir).expect("snapshot");

        let files = changed_files(&before.repo, &before.tree, &after.tree);
        let names: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(names, ["a.txt", "b.txt"]);
        assert_eq!(files[1].change, FileChange::Added);

        // The real index was never touched: b.txt is still untracked.
        let staged = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["diff", "--cached", "--name-only"])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&staged.stdout).trim().is_empty());

        let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
        assert_eq!(restore(&before, &paths), Ok(2));
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\nuser\n");
        assert!(!dir.join("b.txt").exists());

        std::fs::write(dir.join("c.txt"), "later\n").unwrap();
        assert_eq!(rewind_to(&before), Ok(1));
        assert!(!dir.join("c.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
