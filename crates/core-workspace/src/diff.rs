//! Side-by-side file diffs for the editor's diff view: git's unified diff (with the whole file as
//! context) turned into aligned left / right rows, the way split diff views show them.

use std::ops::Range;
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

/// A run of consecutive changed rows (everything between two stretches of unchanged lines).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    /// Row range `start..end`, all of them changes.
    pub start: usize,
    pub end: usize,
}

/// The change blocks of a diff, in order.
pub fn blocks(rows: &[DiffRow]) -> Vec<Block> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        if rows[i].is_change() {
            let start = i;
            while i < rows.len() && rows[i].is_change() {
                i += 1;
            }
            out.push(Block { start, end: i });
        } else {
            i += 1;
        }
    }
    out
}

const PATCH_CONTEXT: usize = 3;

/// A unified-diff patch containing just `block` (plus up to three unchanged lines either side),
/// ready for `git apply`.
pub fn hunk_patch(path: &str, rows: &[DiffRow], block: Block) -> String {
    let is_context = |r: &DiffRow| r.kind == RowKind::Context;
    let mut pre = block.start;
    while pre > 0 && block.start - pre < PATCH_CONTEXT && is_context(&rows[pre - 1]) {
        pre -= 1;
    }
    let mut post = block.end;
    while post < rows.len() && post - block.end < PATCH_CONTEXT && is_context(&rows[post]) {
        post += 1;
    }
    let context = |range: Range<usize>| rows[range].iter().map(|r| r.left.as_ref().map_or("", |c| c.text.as_str()));

    let mut body = String::new();
    let (mut old_len, mut new_len) = (0, 0);
    for text in context(pre..block.start) {
        body.push_str(&format!(" {text}\n"));
        old_len += 1;
        new_len += 1;
    }
    for cell in rows[block.start..block.end].iter().filter_map(|r| r.left.as_ref()) {
        body.push_str(&format!("-{}\n", cell.text));
        old_len += 1;
    }
    for cell in rows[block.start..block.end].iter().filter_map(|r| r.right.as_ref()) {
        body.push_str(&format!("+{}\n", cell.text));
        new_len += 1;
    }
    for text in context(block.end..post) {
        body.push_str(&format!(" {text}\n"));
        old_len += 1;
        new_len += 1;
    }

    // Start lines: the first line of that side in the patch; for an empty side, the line before.
    let window = &rows[pre..post];
    let before = &rows[..pre];
    let old_start = window
        .iter()
        .find_map(|r| r.left.as_ref().map(|c| c.line))
        .unwrap_or_else(|| before.iter().rev().find_map(|r| r.left.as_ref().map(|c| c.line)).unwrap_or(0));
    let new_start = window
        .iter()
        .find_map(|r| r.right.as_ref().map(|c| c.line))
        .unwrap_or_else(|| before.iter().rev().find_map(|r| r.right.as_ref().map(|c| c.line)).unwrap_or(0));
    format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -{old_start},{old_len} +{new_start},{new_len} @@\n{body}")
}

fn apply(root: &Path, patch: &str, flags: &[&str]) -> Result<(), String> {
    let mut args = vec!["apply", "--recount", "--whitespace=nowarn"];
    args.extend_from_slice(flags);
    args.push("-");
    git(root, &args, Some(patch)).map(|_| ())
}

/// Stage only `block` of an unstaged diff.
pub fn stage_hunk(root: &Path, file: &ChangedFile, rows: &[DiffRow], block: Block) -> Result<(), String> {
    apply(root, &hunk_patch(&file.path, rows, block), &["--cached"])
}

/// Unstage only `block` of a staged diff.
pub fn unstage_hunk(root: &Path, file: &ChangedFile, rows: &[DiffRow], block: Block) -> Result<(), String> {
    apply(root, &hunk_patch(&file.path, rows, block), &["--cached", "--reverse"])
}

/// Throw away `block` of an unstaged diff (restores those lines in the working tree).
pub fn revert_hunk(root: &Path, file: &ChangedFile, rows: &[DiffRow], block: Block) -> Result<(), String> {
    apply(root, &hunk_patch(&file.path, rows, block), &["--reverse"])
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TokenClass {
    Word,
    Space,
    Other,
}

fn tokens(text: &str) -> Vec<Range<usize>> {
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            TokenClass::Word
        } else if c.is_whitespace() {
            TokenClass::Space
        } else {
            TokenClass::Other
        }
    };
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut last: Option<TokenClass> = None;
    for (i, c) in text.char_indices() {
        let k = class(c);
        match (out.last_mut(), last) {
            (Some(prev), Some(l)) if l == k && k != TokenClass::Other => prev.end = i + c.len_utf8(),
            _ => out.push(i..i + c.len_utf8()),
        }
        last = Some(k);
    }
    out
}

/// The words that differ between two versions of a line, as byte ranges in each. Empty when the
/// lines have nothing meaningful in common (then the whole line is the change) or are identical.
pub fn intraline(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    if old == new {
        return Default::default();
    }
    let (a, b) = (tokens(old), tokens(new));
    if a.is_empty() || b.is_empty() || a.len() > 400 || b.len() > 400 {
        return Default::default();
    }
    // Longest common subsequence of tokens.
    let mut lcs = vec![vec![0u16; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if old[a[i].clone()] == new[b[j].clone()] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut keep_a, mut keep_b) = (vec![false; a.len()], vec![false; b.len()]);
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if old[a[i].clone()] == new[b[j].clone()] {
            keep_a[i] = true;
            keep_b[j] = true;
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let collect = |toks: &[Range<usize>], keep: &[bool]| -> Vec<Range<usize>> {
        let mut out: Vec<Range<usize>> = Vec::new();
        for (t, kept) in toks.iter().zip(keep) {
            if *kept {
                continue;
            }
            match out.last_mut() {
                Some(prev) if prev.end == t.start => prev.end = t.end,
                _ => out.push(t.clone()),
            }
        }
        out
    };
    let (ra, rb) = (collect(&a, &keep_a), collect(&b, &keep_b));
    let share = |ranges: &[Range<usize>], text: &str| ranges.iter().map(|r| r.len()).sum::<usize>() as f32 / text.len().max(1) as f32;
    // If most of the line changed, highlighting "the changed words" is just noise.
    if share(&ra, old) > 0.6 || share(&rb, new) > 0.6 {
        return Default::default();
    }
    (ra, rb)
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

    #[test]
    fn intraline_marks_only_the_changed_words() {
        let (old, new) = intraline("let x = foo(1);", "let x = bar(1);");
        assert_eq!(old, vec![8..11]);
        assert_eq!(new, vec![8..11]);
        let (old, new) = intraline("a b c", "a b c d");
        assert!(old.is_empty());
        assert_eq!(new, vec![5..7]); // " d"
        // Nothing in common: no word-level noise.
        assert_eq!(intraline("alpha beta", "zzz yyy"), Default::default());
        assert_eq!(intraline("same", "same"), Default::default());
    }

    #[test]
    fn blocks_split_at_unchanged_lines() {
        let d = parse_unified(sample()).unwrap();
        // Changed + Changed + Added form one block, then a lone Added after two context lines.
        assert_eq!(blocks(&d.rows), vec![Block { start: 1, end: 4 }, Block { start: 6, end: 7 }]);
    }

    #[test]
    fn hunk_patch_covers_one_block_with_context() {
        let d = parse_unified(sample()).unwrap();
        let patch = hunk_patch("f.txt", &d.rows, blocks(&d.rows)[1]);
        assert!(patch.contains("@@ -4,3 +5,4 @@"), "{patch}");
        assert!(patch.ends_with(" four\n five\n+six\n seven\n"), "{patch}");
    }

    fn hunk_repo(name: &str) -> (std::path::PathBuf, ChangedFile) {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("core-workspace-hunk-{name}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"], None).unwrap();
        for (k, v) in [("user.email", "t@e.com"), ("user.name", "T"), ("commit.gpgsign", "false")] {
            git(&dir, &["config", k, v], None).unwrap();
        }
        let root = repo_root(&dir).unwrap();
        let original: String = (1..=12).map(|n| format!("line {n}\n")).collect();
        std::fs::write(root.join("f.txt"), &original).unwrap();
        stage_all(&root).unwrap();
        commit(&root, "init", false).unwrap();
        // Two separate edits: line 2 changed, line 11 deleted and a line appended after 12.
        let edited = original.replace("line 2\n", "LINE TWO\n").replace("line 11\n", "") + "line 13\n";
        std::fs::write(root.join("f.txt"), edited).unwrap();
        let file = repo_status(&root).unwrap().unstaged[0].clone();
        (root, file)
    }

    fn git_out(root: &Path, args: &[&str]) -> String {
        git(root, args, None).unwrap()
    }

    #[test]
    fn staging_one_hunk_leaves_the_other_unstaged() {
        let (root, file) = hunk_repo("stage");
        let diff = file_diff(&root, &file, false).unwrap();
        let bs = blocks(&diff.rows);
        // Line 2 edited, line 11 deleted, line 13 appended (line 12 separates the last two).
        assert_eq!(bs.len(), 3);

        stage_hunk(&root, &file, &diff.rows, bs[0]).unwrap();
        let cached = git_out(&root, &["diff", "--cached", "--no-color"]);
        assert!(cached.contains("+LINE TWO") && !cached.contains("line 13"), "{cached}");
        let worktree = git_out(&root, &["diff", "--no-color"]);
        assert!(!worktree.contains("LINE TWO") && worktree.contains("+line 13"), "{worktree}");

        // The staged view now has exactly the first block; unstaging it puts things back.
        let staged_file = repo_status(&root).unwrap().staged[0].clone();
        let staged = file_diff(&root, &staged_file, true).unwrap();
        let sb = blocks(&staged.rows);
        assert_eq!(sb.len(), 1);
        unstage_hunk(&root, &staged_file, &staged.rows, sb[0]).unwrap();
        assert!(git_out(&root, &["diff", "--cached", "--no-color"]).is_empty());
    }

    #[test]
    fn reverting_one_hunk_restores_only_those_lines() {
        let (root, file) = hunk_repo("revert");
        let diff = file_diff(&root, &file, false).unwrap();
        let bs = blocks(&diff.rows);
        revert_hunk(&root, &file, &diff.rows, bs[1]).unwrap();
        let text = std::fs::read_to_string(root.join("f.txt")).unwrap();
        assert!(text.contains("LINE TWO"), "first edit is kept: {text}");
        assert!(text.contains("line 11\n"), "the deleted line is back: {text}");
        assert!(text.contains("line 13\n"), "the appended line is untouched: {text}");
    }
}
