//! Helpers for writing commit messages: a suggested subject from the staged files, the
//! conventional-commit types a repository already uses, and applying one to a message.

use std::path::Path;

use crate::repo::{ChangeKind, ChangedFile, git};

/// The conventional-commit type of `subject` (`feat` in `feat(ui)!: add x`), if it has one.
pub fn commit_type_of(subject: &str) -> Option<&str> {
    let (head, _) = subject.split_once(": ")?;
    let ty = head.split(['(', '!']).next()?;
    (ty.len() >= 2 && ty.len() <= 10 && ty.chars().all(|c| c.is_ascii_lowercase())).then_some(ty)
}

/// The commit types this repository uses, most used first — empty unless its recent history
/// clearly follows the `type: subject` convention.
pub fn commit_types(root: &Path) -> Vec<String> {
    let Ok(log) = git(root, &["log", "-60", "--format=%s"], None) else {
        return Vec::new();
    };
    let subjects: Vec<&str> = log.lines().collect();
    let mut counts: Vec<(String, usize)> = Vec::new();
    for ty in subjects.iter().filter_map(|s| commit_type_of(s)) {
        match counts.iter_mut().find(|(t, _)| t == ty) {
            Some((_, n)) => *n += 1,
            None => counts.push((ty.to_string(), 1)),
        }
    }
    let typed: usize = counts.iter().map(|(_, n)| n).sum();
    if subjects.len() < 3 || typed * 10 < subjects.len() * 4 {
        return Vec::new();
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts.into_iter().take(5).map(|(t, _)| t).collect()
}

/// `message` with its type set to `ty`: added when missing, swapped when different, and removed
/// when it is already `ty` (so clicking a chip twice toggles it).
pub fn apply_commit_type(message: &str, ty: &str) -> String {
    let (first, rest) = match message.split_once('\n') {
        Some((f, r)) => (f, Some(r)),
        None => (message, None),
    };
    let existing = commit_type_of(first);
    let body = match existing {
        // Keep an existing "(scope)" and "!" when swapping the type.
        Some(old) => {
            let tail = &first[old.len()..];
            if old == ty {
                tail.split_once(": ").map_or(first, |(_, t)| t).to_string()
            } else {
                format!("{ty}{tail}")
            }
        }
        None => format!("{ty}: {first}"),
    };
    match rest {
        Some(rest) => format!("{body}\n{rest}"),
        None => body,
    }
}

fn file_name(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
}

/// The deepest directory all `files` share, as the last one or two path components
/// (`crates/app/src` → `app/src`), or `None` at the repository root.
fn common_dir(files: &[ChangedFile]) -> Option<String> {
    let dirs: Vec<Vec<&str>> = files
        .iter()
        .map(|f| {
            let mut parts: Vec<&str> = f.path.split('/').collect();
            parts.pop();
            parts
        })
        .collect();
    let first = dirs.first()?;
    let shared = (0..first.len())
        .take_while(|&i| dirs.iter().all(|d| d.get(i) == Some(&first[i])))
        .count();
    match shared {
        0 => None,
        1 => Some(first[0].to_string()),
        n => Some(format!("{}/{}", first[n - 2], first[n - 1])),
    }
}

/// A conventional type that plainly fits every file (docs, tests, chores), else `None`.
/// The part of the repo a path belongs to: the package or crate under a
/// container folder (`crates/app/...` → `app`, `packages/auth/...` → `auth`,
/// `lib/features/login/...` → `login`), else the top folder, else the file.
fn area_of(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    let containers = ["crates", "packages", "apps", "modules", "plugins", "features"];
    for (i, part) in parts.iter().enumerate() {
        if containers.contains(part) && i + 2 < parts.len() {
            return parts[i + 1].to_string();
        }
    }
    match parts.as_slice() {
        [file] => file.to_string(),
        ["lib" | "src" | "test", second, _, ..] => second.to_string(),
        [first, ..] => first.to_string(),
        [] => String::new(),
    }
}

/// The distinct areas touched, the most-changed first.
fn areas(files: &[ChangedFile]) -> Vec<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for f in files {
        let area = area_of(&f.path);
        match counts.iter_mut().find(|(a, _)| *a == area) {
            Some((_, n)) => *n += 1,
            None => counts.push((area, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    counts.into_iter().map(|(a, _)| a).collect()
}

fn guess_type(files: &[ChangedFile]) -> Option<&'static str> {
    let all = |pred: fn(&str) -> bool| files.iter().all(|f| pred(&f.path));
    if all(|p| p.ends_with(".md") || p.starts_with("docs/")) {
        Some("docs")
    } else if all(|p| p.contains("/test/") || p.starts_with("test/") || p.contains("_test.")) {
        Some("test")
    } else if all(|p| {
        let name = file_name(p);
        matches!(
            name,
            "pubspec.yaml"
                | "pubspec.lock"
                | "Cargo.toml"
                | "Cargo.lock"
                | ".gitignore"
                | "Makefile"
        ) || p.starts_with(".github/")
    }) {
        Some("chore")
    } else {
        None
    }
}

/// A starting subject for committing `files`, e.g. `Update app/src (3 files)` — or, in a
/// repository that uses conventional types (`types`), `docs: update README.md`.
pub fn suggest_message(files: &[ChangedFile], types: &[String]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let verb_for = |k: ChangeKind| match k {
        ChangeKind::Added | ChangeKind::Untracked => "Add",
        ChangeKind::Deleted => "Remove",
        ChangeKind::Renamed => "Rename",
        _ => "Update",
    };
    let subject = if let [f] = files {
        match (f.kind, f.orig_path.as_deref()) {
            (ChangeKind::Renamed, Some(orig)) => {
                format!("Rename {} to {}", file_name(orig), file_name(&f.path))
            }
            _ => format!("{} {}", verb_for(f.kind), file_name(&f.path)),
        }
    } else {
        let verb = if files.iter().all(|f| verb_for(f.kind) == "Add") {
            "Add"
        } else if files.iter().all(|f| verb_for(f.kind) == "Remove") {
            "Remove"
        } else {
            "Update"
        };
        match common_dir(files) {
            Some(dir) => format!("{verb} {dir} ({} files)", files.len()),
            // Spread across the repo: name the areas touched, most first, so
            // the subject says *what* changed rather than only how much.
            None => match areas(files).as_slice() {
                [] | [_] => format!("{verb} {} files", files.len()),
                [a, b] => format!("{verb} {a} and {b}"),
                [a, b, c] => format!("{verb} {a}, {b} and {c}"),
                [a, b, rest @ ..] => format!("{verb} {a}, {b} and {} more", rest.len()),
            },
        }
    };
    if types.is_empty() {
        return Some(subject);
    }
    // Conventional style: lower-case description, with a type when one clearly fits.
    let mut chars = subject.chars();
    let lowered: String = chars
        .next()
        .map(|c| c.to_lowercase().chain(chars).collect())
        .unwrap_or_default();
    Some(
        match guess_type(files).filter(|t| types.iter().any(|x| x == t)) {
            Some(t) => format!("{t}: {lowered}"),
            None => lowered,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::{commit, repo_root, stage_all};

    fn file(path: &str, kind: ChangeKind) -> ChangedFile {
        ChangedFile {
            path: path.into(),
            orig_path: None,
            kind,
        }
    }

    #[test]
    fn reads_the_type_from_a_subject() {
        assert_eq!(commit_type_of("feat: add support"), Some("feat"));
        assert_eq!(commit_type_of("fix(ui)!: crash"), Some("fix"));
        assert_eq!(commit_type_of("Update the thing"), None);
        assert_eq!(commit_type_of("Merge branch: main"), None); // capitalised
        assert_eq!(commit_type_of("x: too short"), None);
    }

    #[test]
    fn applying_a_type_adds_swaps_and_toggles() {
        assert_eq!(apply_commit_type("", "feat"), "feat: ");
        assert_eq!(apply_commit_type("add login", "feat"), "feat: add login");
        assert_eq!(
            apply_commit_type("feat: add login", "fix"),
            "fix: add login"
        );
        assert_eq!(
            apply_commit_type("feat(auth): add login", "fix"),
            "fix(auth): add login"
        );
        assert_eq!(apply_commit_type("feat: add login", "feat"), "add login"); // toggled off
        assert_eq!(
            apply_commit_type("add x\n\nlonger body", "fix"),
            "fix: add x\n\nlonger body"
        );
    }

    #[test]
    fn suggests_plain_subjects() {
        assert_eq!(suggest_message(&[], &[]), None);
        assert_eq!(
            suggest_message(&[file("lib/main.dart", ChangeKind::Modified)], &[]).unwrap(),
            "Update main.dart"
        );
        assert_eq!(
            suggest_message(&[file("a/new.rs", ChangeKind::Untracked)], &[]).unwrap(),
            "Add new.rs"
        );
        assert_eq!(
            suggest_message(&[file("old.txt", ChangeKind::Deleted)], &[]).unwrap(),
            "Remove old.txt"
        );
        let renamed = ChangedFile {
            path: "b.txt".into(),
            orig_path: Some("dir/a.txt".into()),
            kind: ChangeKind::Renamed,
        };
        assert_eq!(
            suggest_message(&[renamed], &[]).unwrap(),
            "Rename a.txt to b.txt"
        );
        let many = [
            file("crates/app/src/a.rs", ChangeKind::Modified),
            file("crates/app/src/b.rs", ChangeKind::Added),
        ];
        assert_eq!(
            suggest_message(&many, &[]).unwrap(),
            "Update app/src (2 files)"
        );
        let added = [
            file("x/a", ChangeKind::Added),
            file("y/b", ChangeKind::Added),
            file("c", ChangeKind::Added),
        ];
        assert_eq!(suggest_message(&added, &[]).unwrap(), "Add x, y and c");
    }

    #[test]
    fn changes_across_the_repo_name_the_areas_touched() {
        let spread = [
            file("crates/app/src/main.rs", ChangeKind::Modified),
            file("crates/app/src/git.rs", ChangeKind::Modified),
            file("crates/editor-view/src/lib.rs", ChangeKind::Modified),
            file("docs/benchmarks.md", ChangeKind::Modified),
        ];
        assert_eq!(suggest_message(&spread, &[]).unwrap(), "Update app, editor-view and docs");
        let many = [
            file("crates/app/a.rs", ChangeKind::Modified),
            file("crates/core/b.rs", ChangeKind::Modified),
            file("docs/c.md", ChangeKind::Modified),
            file("perf/d.py", ChangeKind::Modified),
        ];
        assert_eq!(suggest_message(&many, &[]).unwrap(), "Update app, core and 2 more");
        assert_eq!(area_of("lib/features/login/page.dart"), "login");
        assert_eq!(area_of("lib/main.dart"), "lib");
        assert_eq!(area_of("README.md"), "README.md");
    }

    #[test]
    fn suggests_conventional_subjects_only_when_a_type_clearly_fits() {
        let types = vec!["feat".to_string(), "docs".to_string()];
        let readme = [file("README.md", ChangeKind::Modified)];
        assert_eq!(
            suggest_message(&readme, &types).unwrap(),
            "docs: update README.md"
        );
        // `test` is a fitting type but this repo does not use it: no prefix.
        let t = [file("test/a_test.dart", ChangeKind::Modified)];
        assert_eq!(suggest_message(&t, &types).unwrap(), "update a_test.dart");
        let code = [file("lib/a.dart", ChangeKind::Modified)];
        assert_eq!(suggest_message(&code, &types).unwrap(), "update a.dart");
    }

    #[test]
    fn learns_commit_types_from_history() {
        let dir = std::env::temp_dir().join(format!("core-workspace-types-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"], None).unwrap();
        for (k, v) in [
            ("user.email", "t@e.com"),
            ("user.name", "T"),
            ("commit.gpgsign", "false"),
        ] {
            git(&dir, &["config", k, v], None).unwrap();
        }
        let root = repo_root(&dir).unwrap();
        assert!(commit_types(&root).is_empty(), "no history yet");
        for (i, subject) in [
            "feat: a",
            "feat: b",
            "fix: c",
            "Plain message",
            "refactor: d",
        ]
        .iter()
        .enumerate()
        {
            std::fs::write(root.join("f.txt"), i.to_string()).unwrap();
            stage_all(&root).unwrap();
            commit(&root, subject, false).unwrap();
        }
        assert_eq!(commit_types(&root), ["feat", "fix", "refactor"]);

        // A repository that does not use the convention gets no chips.
        let plain =
            std::env::temp_dir().join(format!("core-workspace-types-plain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&plain);
        std::fs::create_dir_all(&plain).unwrap();
        git(&plain, &["init", "-q", "-b", "main"], None).unwrap();
        for (k, v) in [
            ("user.email", "t@e.com"),
            ("user.name", "T"),
            ("commit.gpgsign", "false"),
        ] {
            git(&plain, &["config", k, v], None).unwrap();
        }
        let plain = repo_root(&plain).unwrap();
        for (i, subject) in ["Add a", "Fix b", "Update c", "feat: lone"]
            .iter()
            .enumerate()
        {
            std::fs::write(plain.join("f.txt"), i.to_string()).unwrap();
            stage_all(&plain).unwrap();
            commit(&plain, subject, false).unwrap();
        }
        assert!(commit_types(&plain).is_empty());
    }
}
