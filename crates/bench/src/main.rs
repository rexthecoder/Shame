//! Headless benchmarks for the per-keystroke and open-file paths.
//!
//! Run with `cargo run --release -p bench` (optionally pass a size in KB,
//! default `100 1000`). Each row is the median of several runs, so numbers
//! are comparable across commits; see `docs/benchmarks.md` for how to read
//! them. Everything measured here is UI-free: it times the work the editor
//! does *around* GPUI, which is where size-proportional cost lives.

use std::path::Path;
use std::time::{Duration, Instant};

use core_buffer::Buffer;
use core_syntax::{InputEdit, Point, SyntaxTree, point_at};
use editor_view::language::language_for_path;

const CLASS_TEMPLATE: &str = r#"class Widget{n} extends StatelessWidget {
  final String title;
  final int count;
  const Widget{n}({super.key, required this.title, this.count = 0});

  @override
  Widget build(BuildContext context) {
    final items = List.generate(count, (i) => 'Item $i of $title');
    return Column(
      children: [
        Text(title, style: Theme.of(context).textTheme.headlineMedium),
        for (final item in items) Padding(
          padding: const EdgeInsets.all(8),
          child: Text(item),
        ),
        if (count > 3) const Text('many'),
      ],
    );
  }
}

"#;

fn generate_dart(target_bytes: usize) -> String {
    let mut out = String::from("import 'package:flutter/material.dart';\n\n");
    let mut n = 0;
    while out.len() < target_bytes {
        out.push_str(&CLASS_TEMPLATE.replace("{n}", &n.to_string()));
        n += 1;
    }
    out
}

/// Median wall time of `runs` executions of `f`.
fn median(runs: usize, mut f: impl FnMut()) -> Duration {
    let mut times: Vec<Duration> = (0..runs)
        .map(|_| {
            let start = Instant::now();
            f();
            start.elapsed()
        })
        .collect();
    times.sort();
    times[times.len() / 2]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Copy of `editor-view`'s `diff_as_edit` (private there): the prefix/suffix
/// scan run on every edit to turn two full strings into one `InputEdit`.
fn diff_as_edit(old: &str, new: &str) -> Option<InputEdit> {
    if old == new {
        return None;
    }
    let (a, b) = (old.as_bytes(), new.as_bytes());
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let max_suffix = (a.len() - prefix).min(b.len() - prefix);
    let suffix = (0..max_suffix)
        .take_while(|&i| a[a.len() - 1 - i] == b[b.len() - 1 - i])
        .count();
    Some(InputEdit {
        start_byte: prefix,
        old_end_byte: a.len() - suffix,
        new_end_byte: b.len() - suffix,
        start_position: point_at(old, prefix),
        old_end_position: point_at(old, a.len() - suffix),
        new_end_position: point_at(new, b.len() - suffix),
    })
}

fn bench_size(kb: usize) {
    let source = generate_dart(kb * 1024);
    let size = source.len();
    let mid = size / 2;
    // Land the edit on a line start so it is a plausible keystroke site.
    let mid = source[..mid].rfind('\n').map_or(mid, |i| i + 1);
    println!("\n== {kb} KB Dart file ({} bytes, {} lines) ==", size, source.lines().count());
    println!("{:<46} {:>10}", "step", "median ms");

    let row = |name: &str, d: Duration| println!("{name:<46} {:>10.3}", ms(d));

    // --- open path ---
    row("buffer: load from string", median(7, || {
        std::hint::black_box(Buffer::from_str(&source));
    }));
    row("syntax: full parse", median(5, || {
        std::hint::black_box(SyntaxTree::parse(&source));
    }));

    // --- per-keystroke path (what sync_buffer_state does) ---
    let mut buffer = Buffer::from_str(&source);
    let mut tree = SyntaxTree::parse(&source);

    row("buffer: insert 1 char (mid-file)", median(101, || {
        buffer.insert(mid, "x");
        buffer.remove(mid..mid + 1);
    }) / 2);

    let before = buffer.text();
    let mut after = before.clone();
    after.insert(mid, 'x');

    row("buffer: text() full copy", median(21, || {
        std::hint::black_box(buffer.text());
    }));
    row("diff_as_edit (prefix/suffix scan)", median(21, || {
        std::hint::black_box(diff_as_edit(&before, &after));
    }));
    let edit = diff_as_edit(&before, &after).unwrap();
    row("syntax: incremental reparse (1 char)", {
        let mut times: Vec<Duration> = (0..9)
            .map(|_| {
                let mut t = SyntaxTree::parse(&before);
                let started = Instant::now();
                t.reparse(&after, edit);
                started.elapsed()
            })
            .collect();
        times.sort();
        times[times.len() / 2]
    });
    tree.reparse(&before, InputEdit { start_byte: 0, old_end_byte: 0, new_end_byte: 0, start_position: Point::new(0, 0), old_end_position: Point::new(0, 0), new_end_position: Point::new(0, 0) });
    row("syntax: fold_ranges (whole tree)", median(21, || {
        std::hint::black_box(tree.fold_ranges());
    }));
    let lang = language_for_path(Some(Path::new("main.dart")));
    row("language: closing_labels (whole text)", median(21, || {
        std::hint::black_box(lang.closing_labels(&before));
    }));
    row("lsp: didChange JSON (full document)", median(21, || {
        let params = serde_json::json!({
            "textDocument": {"uri": "file:///x.dart", "version": 2},
            "contentChanges": [{"text": before}],
        });
        std::hint::black_box(serde_json::to_vec(&params).unwrap());
    }));

    // --- per-frame path ---
    let view_start = mid;
    let view_end = (mid + 60 * 60).min(before.len());
    let view_end = before.floor_char_boundary(view_end);
    row("syntax: highlights for ~60 visible lines", median(21, || {
        std::hint::black_box(tree.highlights_in_range(&before, view_start..view_end));
    }));
    // `render()` asks for the bracket pair at the cursor on every frame.
    let brace = before[mid..].find('{').map_or(mid, |i| mid + i);
    row("syntax: matching_bracket (cursor on `{`)", median(21, || {
        std::hint::black_box(tree.matching_bracket(&before, brace));
    }));
}

fn main() {
    let sizes: Vec<usize> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let sizes = if sizes.is_empty() { vec![100, 1000] } else { sizes };
    for kb in sizes {
        bench_size(kb);
    }
}
