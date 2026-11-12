//! Line coverage from an lcov file (`flutter test --coverage` writes
//! `coverage/lcov.info`): for each source file, how many times each line ran.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Hit counts per 1-based line, for each file the report covers. Relative
/// `SF:` paths are resolved against the package the tests ran in.
pub type LineCoverage = HashMap<PathBuf, HashMap<u32, u32>>;

pub fn parse_lcov(text: &str, package_dir: &Path) -> LineCoverage {
    let mut out: LineCoverage = HashMap::new();
    let mut current: Option<PathBuf> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(path) = line.strip_prefix("SF:") {
            let path = PathBuf::from(path);
            current = Some(if path.is_absolute() { path } else { package_dir.join(path) });
        } else if let Some(rest) = line.strip_prefix("DA:") {
            let mut parts = rest.split(',');
            let (Some(line_no), Some(hits)) = (parts.next(), parts.next()) else { continue };
            let (Ok(line_no), Ok(hits)) = (line_no.parse::<u32>(), hits.parse::<u32>()) else {
                continue;
            };
            if let Some(file) = &current {
                *out.entry(file.clone()).or_default().entry(line_no).or_insert(0) += hits;
            }
        } else if line == "end_of_record" {
            current = None;
        }
    }
    out
}

/// Covered and coverable lines across the whole report.
pub fn coverage_totals(coverage: &LineCoverage) -> (usize, usize) {
    coverage.values().fold((0, 0), |(hit, all), lines| {
        (hit + lines.values().filter(|&&h| h > 0).count(), all + lines.len())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_hits_per_line_and_resolves_relative_paths() {
        let lcov = "SF:lib/main.dart\nDA:3,1\nDA:4,0\nDA:7,2\nLF:3\nLH:2\nend_of_record\nSF:/abs/x.dart\nDA:1,0\nend_of_record\n";
        let cov = parse_lcov(lcov, Path::new("/proj"));
        let main = &cov[&PathBuf::from("/proj/lib/main.dart")];
        assert_eq!(main[&3], 1);
        assert_eq!(main[&4], 0);
        assert_eq!(main[&7], 2);
        assert_eq!(cov[&PathBuf::from("/abs/x.dart")][&1], 0);
        assert_eq!(coverage_totals(&cov), (2, 4));
    }
}
