use std::path::Path;
use std::process::Command;

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
        std::fs::write(dir.join("a.txt"), "hello").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "--quiet", "-m", "initial"]);
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
}
