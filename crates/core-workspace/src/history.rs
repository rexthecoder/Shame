//! Local history: a bounded set of timestamped copies of a file, taken on
//! each save, so an earlier version can be restored without git.
//!
//! Snapshots live outside the project (under a caller-chosen `base` dir,
//! one subdirectory per source file) so they never show up in the file
//! tree, search, or `git status`.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One saved copy of a file.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub path: PathBuf,
    /// Milliseconds since the Unix epoch.
    pub taken_at_ms: u128,
}

/// FNV-1a over the file's path: a stable, dependency-free directory name.
fn dir_name(file: &Path) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in file.to_string_lossy().as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn file_dir(base: &Path, file: &Path) -> PathBuf {
    base.join(dir_name(file))
}

/// Snapshots of `file`, newest first.
pub fn list_snapshots(base: &Path, file: &Path) -> Vec<Snapshot> {
    let Ok(entries) = std::fs::read_dir(file_dir(base, file)) else {
        return Vec::new();
    };
    let mut snapshots: Vec<Snapshot> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let taken_at_ms = path.file_stem()?.to_str()?.parse().ok()?;
            Some(Snapshot { path, taken_at_ms })
        })
        .collect();
    snapshots.sort_by(|a, b| b.taken_at_ms.cmp(&a.taken_at_ms));
    snapshots
}

/// Copy `file`'s current contents into history, unless identical to the
/// newest snapshot, and prune to the `keep` most recent. Returns whether a
/// new snapshot was written.
pub fn record_snapshot(base: &Path, file: &Path, keep: usize) -> io::Result<bool> {
    let text = std::fs::read(file)?;
    let existing = list_snapshots(base, file);
    if let Some(newest) = existing.first() {
        if std::fs::read(&newest.path).ok().as_deref() == Some(text.as_slice()) {
            return Ok(false);
        }
    }
    let dir = file_dir(base, file);
    std::fs::create_dir_all(&dir)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    // Never collide with (or sort before) an existing snapshot taken in the same millisecond.
    let stamp = existing.first().map_or(now, |n| now.max(n.taken_at_ms + 1));
    std::fs::write(dir.join(format!("{stamp}.snap")), &text)?;
    for old in list_snapshots(base, file).into_iter().skip(keep) {
        let _ = std::fs::remove_file(old.path);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn records_dedupes_and_lists_newest_first() {
        let root = temp("basic");
        let base = root.join("hist");
        let file = root.join("a.dart");

        std::fs::write(&file, "one").unwrap();
        assert!(record_snapshot(&base, &file, 10).unwrap());
        assert!(!record_snapshot(&base, &file, 10).unwrap(), "identical content is skipped");

        std::fs::write(&file, "two").unwrap();
        assert!(record_snapshot(&base, &file, 10).unwrap());

        let snaps = list_snapshots(&base, &file);
        assert_eq!(snaps.len(), 2);
        assert_eq!(std::fs::read_to_string(&snaps[0].path).unwrap(), "two");
        assert_eq!(std::fs::read_to_string(&snaps[1].path).unwrap(), "one");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prunes_to_keep_limit() {
        let root = temp("prune");
        let base = root.join("hist");
        let file = root.join("a.dart");
        for i in 0..5 {
            std::fs::write(&file, format!("v{i}")).unwrap();
            record_snapshot(&base, &file, 3).unwrap();
        }
        let snaps = list_snapshots(&base, &file);
        assert_eq!(snaps.len(), 3);
        assert_eq!(std::fs::read_to_string(&snaps[0].path).unwrap(), "v4");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn different_files_have_separate_histories() {
        let root = temp("sep");
        let base = root.join("hist");
        let (a, b) = (root.join("a.dart"), root.join("b.dart"));
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        record_snapshot(&base, &a, 5).unwrap();
        assert_eq!(list_snapshots(&base, &a).len(), 1);
        assert!(list_snapshots(&base, &b).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
