# Benchmarks

Headless timings for the work the editor does around GPUI on each edit and
on file open. Run them with:

```bash
cargo run --release -p bench          # 100 KB and 1000 KB files
cargo run --release -p bench -- 256   # any sizes, in KB
```

Each row is the median of several runs on a generated Dart file (Flutter
widget classes). These are **not** frame times: they omit GPUI layout and
paint, startup, and RAM. They exist to catch size-proportional regressions
in the edit path, which is where an editor loses to Zed's incremental
design. A 120 Hz frame is ~8.3 ms.

## Reading the rows

| Row | Runs when |
|---|---|
| buffer: load, syntax: full parse | opening a file |
| buffer insert, `text()` copy, `diff_as_edit`, incremental reparse, `fold_ranges`, `closing_labels`, LSP `didChange` JSON | every edit (`sync_buffer_state`) |
| highlights for ~60 visible lines | every frame while scrolling or typing |

Syntax parsing is disabled above `MAX_SYNTAX_BYTES` (256 KB), so the 1000 KB
column is a stress test of the algorithms, not a size the app parses today.

## Results (release build, this machine)

| Step (ms, median) | 100 KB | 1000 KB |
|---|---:|---:|
| buffer: load from string | 0.04 | 0.16 |
| syntax: full parse | 16.4 | 151 |
| buffer: insert 1 char | ~0 | ~0 |
| buffer: `text()` full copy | 0.005 | 0.04 |
| `diff_as_edit` (prefix/suffix scan) | 0.10 | 0.94 |
| syntax: incremental reparse (1 char) | 0.26 | 2.2 |
| syntax: `fold_ranges` | **0.59** (was 3.35) | **6.6** (was 33.1) |
| language: `closing_labels` | 0.38 | 3.6 |
| LSP `didChange` JSON (full document) | 0.05 | 0.47 |
| syntax: highlights, ~60 visible lines | 0.21 | 0.21 |
| syntax: `matching_bracket` (cursor on `{`) | 1.42 | — |

`matching_bracket` ran on every `render()` (hover, drag, cursor blink), up
to twice per frame. `EditorView` now caches the pair per cursor position
and text revision, so it only runs when the cursor moves or the text edits.

Sum of the per-edit rows: about 1.5 ms at 100 KB and about 14 ms at 1000 KB.

## What the numbers showed

- The full-buffer `text()` copy and the whole-document `didChange` JSON are
  **cheap** (well under 1 ms at 1 MB). Replacing them with incremental
  edits is not the first priority.
- `fold_ranges` walked every node of the syntax tree on each edit. Skipping
  single-line nodes (a fold can only live in a multi-line node) made it
  about 5x faster with identical output; a test compares it with the old
  walk.
- Remaining size-proportional costs are `fold_ranges`, `closing_labels`
  (both whole-document) and the incremental reparse itself. The next step
  for large files is to recompute folds and closing labels only for the
  edited region, or debounce them.
- Visible-range highlighting is flat in file size, as it should be.

## Project search: warm index vs. disk walk

Search is no longer measured on the edit path, so it gets its own numbers:

```bash
cargo run --release -p core-workspace --example search_bench -- ~/path/to/app
```

That opens `SearchIndex` for a real Flutter project, waits for the first
scan, then runs the same query through fff and through the old per-file walk
(one run per query; the walk re-reads every file each time):

| Project | Files (`.dart`) | Query | fff index | Disk walk |
|---|---:|---|---:|---:|
| luna | 1,912 | `Widget build` (1,728 hits) | 7.1 ms | 53 ms |
| luna | 1,912 | `text style` (17 hits) | 3.4 ms | 45 ms |
| warlock | 5,233 | `Widget build` (3,438 hits) | 15.9 ms | 129 ms |
| warlock | 5,233 | `text style` (1 hit) | 7.0 ms | 126 ms |

First scan of 5,233 files: 336 ms. Quick Open (fzf-style fuzzy match over the
whole index, ranked): 0.5 ms for an empty query and under 2 ms for a path
query, on both projects. The first grep after a scan pays for warming the
content cache (roughly a disk-walk's cost); every later query runs against it.

## Not measured yet

Startup time to first paint, keystroke-to-paint latency, scroll frame time,
and idle RAM need an instrumented GPUI run, not a headless harness.
