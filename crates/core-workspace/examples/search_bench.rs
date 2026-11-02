//! Time project search against the on-disk walk it replaced.
//!
//! ```bash
//! cargo run --release -p core-workspace --example search_bench -- ~/path/to/flutter_app
//! ```
//!
//! Prints, for a few sample queries, how long the warm fff index takes versus
//! re-reading every file, plus the first-scan time and Quick Open latency.
//! The numbers in `docs/benchmarks.md` were taken with this.

use std::path::Path;
use std::time::{Duration, Instant};

fn main() {
    let Some(root) = std::env::args().nth(1) else {
        eprintln!("usage: search_bench <workspace directory>");
        std::process::exit(2);
    };
    let root = Path::new(&root);
    let state = std::env::temp_dir().join("shame-search-bench");
    let _ = std::fs::remove_dir_all(&state);

    let index = core_workspace::SearchIndex::open(root, &state).expect("search index should open");
    let started = Instant::now();
    if !index.wait_ready(Duration::from_secs(120)) {
        eprintln!("index did not finish its first scan");
        std::process::exit(1);
    }
    println!("first scan: {:?}", started.elapsed());

    for query in ["FutureBuilder", "text style", "Widget build"] {
        let started = Instant::now();
        let indexed = index.search_text(query).expect("index should be ready");
        let index_time = started.elapsed();

        let started = Instant::now();
        let walked = core_workspace::search_text(root, query);
        let walk_time = started.elapsed();

        println!(
            "{query:?}: {} matches — index {:?}, walk {:?}",
            indexed.matches.len(),
            index_time,
            walk_time
        );
        if indexed.matches.len() != walked.matches.len() {
            // Expected for multi-word queries the file wraps differently.
            println!("  (walk found {} matches)", walked.matches.len());
        }
    }

    for query in ["", "main", "mainscreen"] {
        let started = Instant::now();
        let files = index.find_files(query, 20).expect("index should be ready");
        println!("quick open {query:?}: {} files in {:?}", files.len(), started.elapsed());
    }

    let _ = std::fs::remove_dir_all(&state);
}
