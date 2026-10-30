//! Source-control operations behind the editor's Git panel: a structured status (staged / unstaged
//! / conflicts, branch, upstream, ahead/behind) and the actions on it — stage, unstage, discard,
//! commit, fetch, pull, push, branches.
//!
//! Everything shells out to `git`, like the rest of this crate. Commands never prompt (no TTY for
//! credentials or an editor), so a failure comes back as the text git printed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Conflicted,
}

impl ChangeKind {
    /// The one-letter badge shown next to a file.
    pub fn letter(self) -> char {
        match self {
            ChangeKind::Added => 'A',
            ChangeKind::Modified => 'M',
            ChangeKind::Deleted => 'D',
            ChangeKind::Renamed => 'R',
            ChangeKind::Copied => 'C',
            ChangeKind::TypeChanged => 'T',
            ChangeKind::Untracked => 'U',
            ChangeKind::Conflicted => '!',
        }
    }

    fn from_code(c: char) -> Option<ChangeKind> {
        Some(match c {
            'A' => ChangeKind::Added,
            'M' => ChangeKind::Modified,
            'D' => ChangeKind::Deleted,
            'R' => ChangeKind::Renamed,
            'C' => ChangeKind::Copied,
            'T' => ChangeKind::TypeChanged,
            _ => return None,
        })
    }
}

/// One changed path, relative to the repository root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    /// The old path of a rename or copy.
    pub orig_path: Option<String>,
    pub kind: ChangeKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoStatus {
    pub root: PathBuf,
    /// Where the directory the status was asked for sits below `root` (`"crates/app/"`, or empty).
    pub prefix: String,
    /// `None` while HEAD is detached.
    pub branch: Option<String>,
    /// Abbreviated commit id of HEAD; `None` before the first commit.
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub conflicts: Vec<ChangedFile>,
    pub staged: Vec<ChangedFile>,
    /// Working-tree changes, including untracked files.
    pub unstaged: Vec<ChangedFile>,
}

impl RepoStatus {
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty() && self.staged.is_empty() && self.unstaged.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchInfo {
    pub name: String,
    pub current: bool,
}

/// Run `git` in `dir`, returning stdout, or what git printed when it failed.
pub(crate) fn git(dir: &Path, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_PAGER", "cat")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("could not run git: {e}"))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A write error just means git exited early; its own message explains why.
        let _ = pipe.write_all(text.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let msg = if err.is_empty() {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        } else {
            err
        };
        Err(if msg.is_empty() {
            format!("git {} failed", args.first().unwrap_or(&""))
        } else {
            msg
        })
    }
}

/// The root of the repository containing `dir`, or `None` outside one.
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    git(dir, &["rev-parse", "--show-toplevel"], None)
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| !p.as_os_str().is_empty())
}

/// The structured status of the repository containing `dir`, or `None` outside one.
/// Two git processes in total (locate the repo, read the status), however much the UI wants
/// from it.
pub fn repo_status(dir: &Path) -> Option<RepoStatus> {
    let located = git(dir, &["rev-parse", "--show-toplevel", "--show-prefix"], None).ok()?;
    let mut lines = located.lines();
    let root = PathBuf::from(lines.next()?.trim());
    let prefix = lines.next().unwrap_or("").trim().to_string();
    let out = git(
        &root,
        &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"],
        None,
    )
    .ok()?;
    let mut status = parse_status(&out, root);
    status.prefix = prefix;
    Some(status)
}

impl RepoStatus {
    /// Per-file badge letters keyed by absolute path under `dir` (the folder the editor has
    /// open), for the file tree: `U` untracked / conflicted, `M`, `A`, `D`, `R`. A working-tree
    /// change wins over a staged one. Built from this status, so it costs no extra git process.
    pub fn badge_map(&self, dir: &Path) -> std::collections::HashMap<PathBuf, char> {
        let mut map = std::collections::HashMap::new();
        let mut put = |file: &ChangedFile, letter: char| {
            let relative = file.path.strip_prefix(self.prefix.as_str()).unwrap_or(&file.path);
            map.entry(dir.join(relative)).or_insert(letter);
        };
        for f in &self.conflicts {
            put(f, 'U');
        }
        for f in &self.unstaged {
            put(f, f.kind.letter());
        }
        for f in &self.staged {
            put(f, f.kind.letter());
        }
        map
    }
}

/// Parse `git status --porcelain=v2 --branch -z` output.
fn parse_status(out: &str, root: PathBuf) -> RepoStatus {
    let mut status = RepoStatus {
        root,
        ..Default::default()
    };
    let mut records = out.split('\0').filter(|r| !r.is_empty());
    while let Some(rec) = records.next() {
        if let Some(header) = rec.strip_prefix("# ") {
            let mut parts = header.splitn(2, ' ');
            match (parts.next(), parts.next()) {
                (Some("branch.oid"), Some(oid)) if oid != "(initial)" => {
                    status.head = Some(oid.chars().take(7).collect());
                }
                (Some("branch.head"), Some(head)) if head != "(detached)" => {
                    status.branch = Some(head.to_string());
                }
                (Some("branch.upstream"), Some(up)) => status.upstream = Some(up.to_string()),
                (Some("branch.ab"), Some(ab)) => {
                    let mut nums = ab.split(' ');
                    status.ahead = nums
                        .next()
                        .and_then(|a| a.trim_start_matches('+').parse().ok())
                        .unwrap_or(0);
                    status.behind = nums
                        .next()
                        .and_then(|b| b.trim_start_matches('-').parse().ok())
                        .unwrap_or(0);
                }
                _ => {}
            }
            continue;
        }
        let kind = rec.as_bytes()[0];
        match kind {
            b'1' | b'2' => {
                // 1 XY sub mH mI mW hH hI path        2 XY sub mH mI mW hH hI Xscore path <NUL> orig
                let fields = if kind == b'1' { 9 } else { 10 };
                let parts: Vec<&str> = rec.splitn(fields, ' ').collect();
                if parts.len() < fields {
                    continue;
                }
                let xy = parts[1].as_bytes();
                let path = parts[fields - 1].to_string();
                let orig_path = if kind == b'2' {
                    records.next().map(str::to_string)
                } else {
                    None
                };
                let (x, y) = (xy[0] as char, xy[1] as char);
                if let Some(k) = ChangeKind::from_code(x) {
                    status.staged.push(ChangedFile {
                        path: path.clone(),
                        orig_path: orig_path.clone(),
                        kind: k,
                    });
                }
                if let Some(k) = ChangeKind::from_code(y) {
                    status.unstaged.push(ChangedFile {
                        path,
                        orig_path,
                        kind: k,
                    });
                }
            }
            b'u' => {
                // u XY sub m1 m2 m3 mW h1 h2 h3 path
                if let Some(path) = rec.splitn(11, ' ').nth(10) {
                    status.conflicts.push(ChangedFile {
                        path: path.to_string(),
                        orig_path: None,
                        kind: ChangeKind::Conflicted,
                    });
                }
            }
            b'?' => {
                if let Some(path) = rec.strip_prefix("? ") {
                    status.unstaged.push(ChangedFile {
                        path: path.to_string(),
                        orig_path: None,
                        kind: ChangeKind::Untracked,
                    });
                }
            }
            _ => {}
        }
    }
    status
}

/// Turn `dir` into a git repository.
pub fn init(dir: &Path) -> Result<(), String> {
    git(dir, &["init", "-q"], None).map(|_| ())
}

/// Stage `paths` (additions, edits and deletions alike).
pub fn stage(root: &Path, paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec!["add", "-A", "--"];
    args.extend(paths.iter().map(String::as_str));
    git(root, &args, None).map(|_| ())
}

pub fn stage_all(root: &Path) -> Result<(), String> {
    git(root, &["add", "-A"], None).map(|_| ())
}

/// Remove `paths` from the index, keeping the working-tree changes.
pub fn unstage(root: &Path, paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec!["reset", "-q", "--"];
    args.extend(paths.iter().map(String::as_str));
    git(root, &args, None).map(|_| ())
}

pub fn unstage_all(root: &Path) -> Result<(), String> {
    git(root, &["reset", "-q"], None).map(|_| ())
}

/// Throw away the working-tree change to `file`: restore it from the index, or delete it when
/// it is untracked. Staged changes are untouched. Not undoable.
pub fn discard(root: &Path, file: &ChangedFile) -> Result<(), String> {
    if file.kind == ChangeKind::Untracked {
        git(root, &["clean", "-fdq", "--", &file.path], None).map(|_| ())
    } else {
        git(root, &["restore", "--", &file.path], None).map(|_| ())
    }
}

/// Commit the index with `message` (or, when amending with an empty message, keep the old one).
/// Returns a one-line summary such as `a1b2c3d Fix the thing`.
pub fn commit(root: &Path, message: &str, amend: bool) -> Result<String, String> {
    let message = message.trim();
    if message.is_empty() && !amend {
        return Err("Enter a commit message".to_string());
    }
    let mut args = vec!["commit", "-q"];
    if amend {
        args.push("--amend");
    }
    if message.is_empty() {
        args.push("--no-edit");
        git(root, &args, None)?;
    } else {
        args.extend(["-F", "-"]);
        git(root, &args, Some(message))?;
    }
    git(root, &["log", "-1", "--format=%h %s"], None).map(|s| s.trim().to_string())
}

/// The full message of the last commit, to prefill an amend.
pub fn last_commit_message(root: &Path) -> Option<String> {
    git(root, &["log", "-1", "--format=%B"], None)
        .ok()
        .map(|s| s.trim().to_string())
}

pub fn fetch(root: &Path) -> Result<String, String> {
    git(root, &["fetch", "--prune"], None).map(|_| "Fetched".to_string())
}

/// Fast-forward only: never creates a merge commit behind the user's back.
pub fn pull(root: &Path) -> Result<String, String> {
    git(root, &["pull", "--ff-only"], None).map(|out| {
        if out.contains("Already up to date") {
            "Already up to date".to_string()
        } else {
            "Pulled".to_string()
        }
    })
}

/// Push the current branch, creating its upstream on the first remote when it has none.
pub fn push(root: &Path, status: &RepoStatus) -> Result<String, String> {
    let Some(branch) = status.branch.as_deref() else {
        return Err("HEAD is detached; switch to a branch to push".to_string());
    };
    if status.upstream.is_some() {
        git(root, &["push"], None)?;
    } else {
        let remotes = git(root, &["remote"], None)?;
        let Some(remote) = remotes.lines().find(|r| !r.trim().is_empty()) else {
            return Err("No remote configured for this repository".to_string());
        };
        git(root, &["push", "-u", remote.trim(), branch], None)?;
    }
    Ok("Pushed".to_string())
}

/// Local branches, most recently committed first.
pub fn branches(root: &Path) -> Vec<BranchInfo> {
    git(
        root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(HEAD)%(refname:short)",
            "refs/heads",
        ],
        None,
    )
    .map(|out| {
        out.lines()
            .filter(|l| l.len() > 1)
            .map(|l| BranchInfo {
                name: l[1..].to_string(),
                current: l.starts_with('*'),
            })
            .collect()
    })
    .unwrap_or_default()
}

pub fn checkout(root: &Path, branch: &str) -> Result<(), String> {
    git(root, &["switch", branch], None).map(|_| ())
}

pub fn create_branch(root: &Path, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a branch name".to_string());
    }
    git(root, &["switch", "-c", name], None).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scratch(name: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "core-workspace-repo-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A repo whose commits never try to sign (the host's gpg setup must not matter to tests).
    fn init_repo(name: &str) -> PathBuf {
        let dir = scratch(name);
        git(&dir, &["init", "-q", "-b", "main"], None).unwrap();
        for (k, v) in [
            ("user.email", "t@example.com"),
            ("user.name", "Test"),
            ("commit.gpgsign", "false"),
        ] {
            git(&dir, &["config", k, v], None).unwrap();
        }
        // macOS temp dirs are symlinks; use the path git reports.
        repo_root(&dir).unwrap()
    }

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    fn names(files: &[ChangedFile]) -> Vec<&str> {
        files.iter().map(|f| f.path.as_str()).collect()
    }

    #[test]
    fn parses_branch_headers_and_every_entry_kind() {
        let out = "# branch.oid 1234567890abcdef\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -3\0\
                   1 M. N... 100644 100644 100644 aaaa bbbb staged only.txt\0\
                   1 .M N... 100644 100644 100644 aaaa bbbb dirty.txt\0\
                   1 MM N... 100644 100644 100644 aaaa bbbb both.txt\0\
                   2 R. N... 100644 100644 100644 aaaa bbbb R100 new name.txt\0old name.txt\0\
                   u UU N... 100644 100644 100644 100644 aaaa bbbb cccc clash.txt\0\
                   ? new.txt\0";
        let s = parse_status(out, PathBuf::from("/r"));
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.head.as_deref(), Some("1234567"));
        assert_eq!(s.upstream.as_deref(), Some("origin/main"));
        assert_eq!((s.ahead, s.behind), (2, 3));
        assert_eq!(
            names(&s.staged),
            ["staged only.txt", "both.txt", "new name.txt"]
        );
        assert_eq!(names(&s.unstaged), ["dirty.txt", "both.txt", "new.txt"]);
        assert_eq!(names(&s.conflicts), ["clash.txt"]);
        let renamed = &s.staged[2];
        assert_eq!(renamed.kind, ChangeKind::Renamed);
        assert_eq!(renamed.orig_path.as_deref(), Some("old name.txt"));
        assert_eq!(s.unstaged[2].kind, ChangeKind::Untracked);
    }

    #[test]
    fn detached_and_unborn_heads_have_no_branch_name() {
        let s = parse_status(
            "# branch.oid (initial)\0# branch.head (detached)\0",
            PathBuf::new(),
        );
        assert_eq!((s.branch, s.head), (None, None));
    }

    #[test]
    fn stage_commit_edit_unstage_discard_roundtrip() {
        let root = init_repo("flow");
        assert_eq!(repo_status(&root).unwrap().branch.as_deref(), Some("main"));

        write(&root, "a.txt", "one");
        let s = repo_status(&root).unwrap();
        assert_eq!(s.unstaged[0].kind, ChangeKind::Untracked);

        // Staging then unstaging works even before the very first commit.
        stage(&root, &["a.txt".to_string()]).unwrap();
        let s = repo_status(&root).unwrap();
        assert_eq!(
            (names(&s.staged), s.staged[0].kind),
            (vec!["a.txt"], ChangeKind::Added)
        );
        unstage(&root, &["a.txt".to_string()]).unwrap();
        assert!(repo_status(&root).unwrap().staged.is_empty());

        stage_all(&root).unwrap();
        assert!(
            commit(&root, "   ", false).is_err(),
            "blank message is refused"
        );
        let summary = commit(&root, "Add a\n\nlonger body", false).unwrap();
        assert!(summary.ends_with("Add a"), "{summary}");
        assert!(repo_status(&root).unwrap().is_clean());

        write(&root, "a.txt", "two");
        let s = repo_status(&root).unwrap();
        assert_eq!(
            (names(&s.unstaged), s.unstaged[0].kind),
            (vec!["a.txt"], ChangeKind::Modified)
        );

        stage(&root, &["a.txt".to_string()]).unwrap();
        unstage_all(&root).unwrap();
        let s = repo_status(&root).unwrap();
        assert!(s.staged.is_empty() && s.unstaged.len() == 1);

        discard(&root, &s.unstaged[0]).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "one");
        assert!(repo_status(&root).unwrap().is_clean());

        write(&root, "scratch.txt", "x");
        let s = repo_status(&root).unwrap();
        discard(&root, &s.unstaged[0]).unwrap();
        assert!(!root.join("scratch.txt").exists());
    }

    #[test]
    fn deletions_and_spaces_in_names_are_staged() {
        let root = init_repo("names");
        write(&root, "my file.txt", "x");
        stage_all(&root).unwrap();
        commit(&root, "init", false).unwrap();
        std::fs::remove_file(root.join("my file.txt")).unwrap();
        let s = repo_status(&root).unwrap();
        assert_eq!(
            (names(&s.unstaged), s.unstaged[0].kind),
            (vec!["my file.txt"], ChangeKind::Deleted)
        );
        stage(&root, &["my file.txt".to_string()]).unwrap();
        let s = repo_status(&root).unwrap();
        assert_eq!(
            (names(&s.staged), s.staged[0].kind),
            (vec!["my file.txt"], ChangeKind::Deleted)
        );
    }

    #[test]
    fn amend_replaces_the_last_commit() {
        let root = init_repo("amend");
        write(&root, "a.txt", "1");
        stage_all(&root).unwrap();
        commit(&root, "first", false).unwrap();
        assert_eq!(last_commit_message(&root).as_deref(), Some("first"));
        commit(&root, "reworded", true).unwrap();
        assert_eq!(last_commit_message(&root).as_deref(), Some("reworded"));
        let count = git(&root, &["rev-list", "--count", "HEAD"], None).unwrap();
        assert_eq!(count.trim(), "1");
    }

    #[test]
    fn branches_create_and_switch() {
        let root = init_repo("branches");
        write(&root, "a.txt", "1");
        stage_all(&root).unwrap();
        commit(&root, "first", false).unwrap();
        assert!(create_branch(&root, " ").is_err());
        create_branch(&root, "feature").unwrap();
        let list = branches(&root);
        assert!(list.contains(&BranchInfo {
            name: "feature".into(),
            current: true
        }));
        assert!(list.contains(&BranchInfo {
            name: "main".into(),
            current: false
        }));
        checkout(&root, "main").unwrap();
        assert_eq!(repo_status(&root).unwrap().branch.as_deref(), Some("main"));
        assert!(checkout(&root, "nope").is_err());
    }

    #[test]
    fn push_sets_upstream_then_pull_brings_changes_back() {
        let remote = scratch("remote");
        git(&remote, &["init", "-q", "--bare", "-b", "main"], None).unwrap();
        let a = init_repo("clone-a");
        git(
            &a,
            &["remote", "add", "origin", remote.to_str().unwrap()],
            None,
        )
        .unwrap();
        write(&a, "a.txt", "1");
        stage_all(&a).unwrap();
        commit(&a, "first", false).unwrap();

        let s = repo_status(&a).unwrap();
        assert_eq!(s.upstream, None);
        push(&a, &s).unwrap();
        let s = repo_status(&a).unwrap();
        assert_eq!(s.upstream.as_deref(), Some("origin/main"));
        assert_eq!((s.ahead, s.behind), (0, 0));

        write(&a, "a.txt", "2");
        stage_all(&a).unwrap();
        commit(&a, "second", false).unwrap();
        assert_eq!(repo_status(&a).unwrap().ahead, 1);
        push(&a, &repo_status(&a).unwrap()).unwrap();

        // A second clone sees the commits; after a third lands in A, B pulls it.
        let b_dir = scratch("clone-b");
        git(
            &b_dir,
            &["clone", "-q", remote.to_str().unwrap(), "b"],
            None,
        )
        .unwrap();
        let b = repo_root(&b_dir.join("b")).unwrap();
        for (k, v) in [
            ("user.email", "t@example.com"),
            ("user.name", "Test"),
            ("commit.gpgsign", "false"),
        ] {
            git(&b, &["config", k, v], None).unwrap();
        }
        write(&a, "a.txt", "3");
        stage_all(&a).unwrap();
        commit(&a, "third", false).unwrap();
        push(&a, &repo_status(&a).unwrap()).unwrap();

        fetch(&b).unwrap();
        assert_eq!(repo_status(&b).unwrap().behind, 1);
        pull(&b).unwrap();
        assert_eq!(std::fs::read_to_string(b.join("a.txt")).unwrap(), "3");
        assert_eq!(pull(&b).unwrap(), "Already up to date");
    }

    #[test]
    fn push_without_a_remote_or_branch_explains_itself() {
        let root = init_repo("noremote");
        write(&root, "a.txt", "1");
        stage_all(&root).unwrap();
        commit(&root, "first", false).unwrap();
        let s = repo_status(&root).unwrap();
        assert_eq!(
            push(&root, &s).unwrap_err(),
            "No remote configured for this repository"
        );
        let detached = RepoStatus { branch: None, ..s };
        assert!(push(&root, &detached).unwrap_err().contains("detached"));
    }

    #[test]
    fn outside_a_repo_there_is_no_status() {
        let dir = scratch("plain");
        assert!(repo_status(&dir).is_none());
    }

    #[test]
    fn badges_follow_the_status_and_the_prefix_of_the_open_folder() {
        let root = init_repo("badges");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        write(&root, "sub/a.txt", "1");
        write(&root, "top.txt", "1");
        stage_all(&root).unwrap();
        commit(&root, "init", false).unwrap();
        write(&root, "sub/a.txt", "2"); // modified, unstaged
        write(&root, "sub/new.txt", "n"); // untracked
        write(&root, "top.txt", "2");
        stage(&root, &["top.txt".to_string()]).unwrap(); // modified, staged
        write(&root, "top.txt", "3"); // ... and edited again: the working tree wins

        // Asked from inside `sub`, the keys live under that folder.
        let sub = root.join("sub");
        let status = repo_status(&sub).unwrap();
        assert_eq!(status.prefix, "sub/");
        let badges = status.badge_map(&sub);
        assert_eq!(badges.get(&sub.join("a.txt")), Some(&'M'));
        assert_eq!(badges.get(&sub.join("new.txt")), Some(&'U'));
        // Asked from the root, everything is keyed under the root.
        let whole = repo_status(&root).unwrap().badge_map(&root);
        assert_eq!(whole.get(&root.join("top.txt")), Some(&'M'));
        assert_eq!(whole.len(), 3);
    }
}
