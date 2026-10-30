//! Side-by-side file diffs for the editor's diff view: git's unified diff (with the whole file as
//! context) turned into aligned left / right rows, the way split diff views show them.

use std::path::Path;

use crate::repo::{ChangeKind, ChangedFile, git};

/// One side of a row: the 1-based line number in that version of the file, and its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffCell {
    pub line: u32,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// The same line on both sides.
    Context,
    /// Only on the right (new).
    Added,
    /// Only on the left (old).
    Removed,
    /// A line that was replaced: old text on the left, new text on the right.
    Changed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffRow {
    pub left: Option<DiffCell>,
    pub right: Option<DiffCell>,
    pub kind: RowKind,
}

impl DiffRow {
    pub fn is_change(&self) -> bool {
        self.kind != RowKind::Context
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDiff {
    pub rows: Vec<DiffRow>,
    pub binary: bool,
    pub added: u32,
    pub removed: u32,
}

impl FileDiff {
    /// The old version of the file, rebuilt from the left column.
    pub fn old_text(&self) -> String {
        join_side(self.rows.iter().filter_map(|r| r.left.as_ref()))
    }

    /// The new version of the file, rebuilt from the right column.
    pub fn new_text(&self) -> String {
        join_side(self.rows.iter().filter_map(|r| r.right.as_ref()))
    }
}

fn join_side<'a>(cells: impl Iterator<Item = &'a DiffCell>) -> String {
    let mut out = String::new();
    for cell in cells {
        out.push_str(&cell.text);
        out.push('\n');
    }
    out
}

/// The diff of `file` for the section it is shown in: `staged` compares HEAD with the index,
/// otherwise the index with the working tree. Untracked files diff against nothing.
pub fn file_diff(root: &Path, file: &ChangedFile, staged: bool) -> Result<FileDiff, String> {
    if file.kind == ChangeKind::Untracked {
        let bytes = std::fs::read(root.join(&file.path)).map_err(|e| e.to_string())?;
        if bytes.contains(&0) {
            return Ok(FileDiff { binary: true, ..Default::default() });
        }
        let text = String::from_utf8_lossy(&bytes);
        let rows: Vec<DiffRow> = text
            .lines()
            .enumerate()
            .map(|(i, l)| DiffRow {
                left: None,
                right: Some(DiffCell { line: i as u32 + 1, text: l.trim_end_matches('\r').to_string() }),
                kind: RowKind::Added,
            })
            .collect();
        let added = rows.len() as u32;
        return Ok(FileDiff { rows, binary: false, added, removed: 0 });
    }
    let mut args = vec!["-c", "core.quotepath=off", "diff", "-U1000000", "--no-color", "--no-ext-diff", "-M"];
    if staged {
        args.push("--cached");
    }
    args.push("--");
    if let Some(orig) = file.orig_path.as_deref() {
        args.push(orig);
    }
    args.push(&file.path);
    parse_unified(&git(root, &args, None)?)
}

/// Parse a unified diff of a single file into aligned rows.
pub fn parse_unified(diff: &str) -> Result<FileDiff, String> {
    let mut out = FileDiff::default();
    let (mut old_no, mut new_no) = (0u32, 0u32);
    let mut in_hunk = false;
    let mut removed: Vec<DiffCell> = Vec::new();
    let mut added: Vec<DiffCell> = Vec::new();

    fn flush(out: &mut FileDiff, removed: &mut Vec<DiffCell>, added: &mut Vec<DiffCell>) {
        let (mut r, mut a) = (std::mem::take(removed).into_iter(), std::mem::take(added).into_iter());
        loop {
            match (r.next(), a.next()) {
                (None, None) => break,
                (Some(left), Some(right)) => {
                    out.rows.push(DiffRow { left: Some(left), right: Some(right), kind: RowKind::Changed })
                }
                (Some(left), None) => out.rows.push(DiffRow { left: Some(left), right: None, kind: RowKind::Removed }),
                (None, Some(right)) => out.rows.push(DiffRow { left: None, right: Some(right), kind: RowKind::Added }),
            }
        }
    }

    for line in diff.lines() {
        if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            return Ok(FileDiff { binary: true, ..Default::default() });
        }
        if let Some(rest) = line.strip_prefix("@@ ") {
            flush(&mut out, &mut removed, &mut added);
            // @@ -old_start[,len] +new_start[,len] @@
            let mut parts = rest.split(' ');
            let start = |s: Option<&str>, sign: char| -> Result<u32, String> {
                s.and_then(|s| s.strip_prefix(sign))
                    .and_then(|s| s.split(',').next())
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| format!("unreadable hunk header: {line}"))
            };
            old_no = start(parts.next(), '-')?;
            new_no = start(parts.next(), '+')?;
            // A zero-length range names the line *before* the change.
            if rest.split(' ').next().is_some_and(|s| s.ends_with(",0")) {
                old_no += 1;
            }
            if rest.split(' ').nth(1).is_some_and(|s| s.ends_with(",0")) {
                new_no += 1;
            }
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            continue; // file headers
        }
        let (tag, text) = line.split_at(line.chars().next().map_or(0, |c| c.len_utf8()));
        let text = text.trim_end_matches('\r').to_string();
        match tag {
            "-" => {
                removed.push(DiffCell { line: old_no, text });
                old_no += 1;
                out.removed += 1;
            }
            "+" => {
                added.push(DiffCell { line: new_no, text });
                new_no += 1;
                out.added += 1;
            }
            " " => {
                flush(&mut out, &mut removed, &mut added);
                out.rows.push(DiffRow {
                    left: Some(DiffCell { line: old_no, text: text.clone() }),
                    right: Some(DiffCell { line: new_no, text }),
                    kind: RowKind::Context,
                });
                old_no += 1;
                new_no += 1;
            }
            _ => {} // "\ No newline at end of file"
        }
    }
    flush(&mut out, &mut removed, &mut added);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::{commit, repo_root, repo_status, stage_all};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn sample() -> &'static str {
        "diff --git a/f.txt b/f.txt\nindex 1..2 100644\n--- a/f.txt\n+++ b/f.txt\n@@ -1,6 +1,7 @@\n one\n-two\n-three\n+TWO\n+THREE\n+THREE-B\n four\n five\n+six\n seven\n"
    }

    #[test]
    fn pairs_replaced_lines_and_leaves_pure_additions_unpaired() {
        let d = parse_unified(sample()).unwrap();
        let kinds: Vec<RowKind> = d.rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            [
                RowKind::Context,
                RowKind::Changed,
                RowKind::Changed,
                RowKind::Added,
                RowKind::Context,
                RowKind::Context,
                RowKind::Added,
                RowKind::Context
            ]
        );
        assert_eq!((d.added, d.removed), (4, 2));
        // Line numbers advance independently on each side.
        let third = &d.rows[3];
        assert_eq!(third.left, None);
        assert_eq!(third.right, Some(DiffCell { line: 4, text: "THREE-B".into() }));
        let last = d.rows.last().unwrap();
        assert_eq!((last.left.as_ref().unwrap().line, last.right.as_ref().unwrap().line), (6, 8));
    }

    #[test]
    fn old_and_new_text_roundtrip() {
        let d = parse_unified(sample()).unwrap();
        assert_eq!(d.old_text(), "one\ntwo\nthree\nfour\nfive\nseven\n");
        assert_eq!(d.new_text(), "one\nTWO\nTHREE\nTHREE-B\nfour\nfive\nsix\nseven\n");
    }

    #[test]
    fn binary_and_empty_diffs() {
        assert!(parse_unified("Binary files a/x and b/x differ\n").unwrap().binary);
        assert!(parse_unified("").unwrap().rows.is_empty());
    }

    #[test]
    fn deleted_and_created_files_use_zero_length_ranges() {
        let created = parse_unified("--- /dev/null\n+++ b/n\n@@ -0,0 +1,2 @@\n+a\n+b\n").unwrap();
        assert_eq!(created.rows.len(), 2);
        assert_eq!(created.rows[0].right.as_ref().unwrap().line, 1);
        let deleted = parse_unified("--- a/n\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n").unwrap();
        assert_eq!(deleted.rows[1].left.as_ref().unwrap().line, 2);
        assert!(deleted.rows.iter().all(|r| r.right.is_none()));
    }

    #[test]
    fn real_repo_staged_unstaged_untracked_and_deleted() {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("core-workspace-diff-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"], None).unwrap();
        for (k, v) in [("user.email", "t@e.com"), ("user.name", "T"), ("commit.gpgsign", "false")] {
            git(&dir, &["config", k, v], None).unwrap();
        }
        let root = repo_root(&dir).unwrap();
        std::fs::write(root.join("a.txt"), "1\n2\n3\n").unwrap();
        std::fs::write(root.join("gone.txt"), "x\ny\n").unwrap();
        stage_all(&root).unwrap();
        commit(&root, "init", false).unwrap();

        std::fs::write(root.join("a.txt"), "1\ntwo\n3\n4\n").unwrap();
        std::fs::remove_file(root.join("gone.txt")).unwrap();
        std::fs::write(root.join("new.txt"), "n1\nn2\n").unwrap();
        let s = repo_status(&root).unwrap();
        let by = |name: &str| s.unstaged.iter().find(|f| f.path == name).unwrap().clone();

        let a = file_diff(&root, &by("a.txt"), false).unwrap();
        assert_eq!((a.added, a.removed), (2, 1));
        assert_eq!(a.new_text(), "1\ntwo\n3\n4\n");
        assert_eq!(a.old_text(), "1\n2\n3\n");

        let gone = file_diff(&root, &by("gone.txt"), false).unwrap();
        assert_eq!((gone.added, gone.removed), (0, 2));

        let new = file_diff(&root, &by("new.txt"), false).unwrap();
        assert_eq!((new.added, new.removed, new.rows.len()), (2, 0, 2));

        // Staged view compares HEAD with the index: nothing staged yet means no rows.
        assert!(file_diff(&root, &by("a.txt"), true).unwrap().rows.is_empty());
        stage_all(&root).unwrap();
        let staged = file_diff(&root, &by("a.txt"), true).unwrap();
        assert_eq!((staged.added, staged.removed), (2, 1));
    }
}
