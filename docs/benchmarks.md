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
| buffer: load, syntax: full parse | opening a file (files > 128 KB parse on a background thread) |
| buffer insert, `text()` copy, `take_edits()`, incremental reparse, `update_fold_ranges`, LSP `didChange` JSON | every edit (`sync_buffer_state`) |
| highlights & `closing_labels_in_range` for ~60 visible lines | every frame while scrolling or typing |

Syntax parsing is enabled up to `MAX_SYNTAX_BYTES` (2 MB, up from 256 KB), with
files above `SYNC_PARSE_BYTES` (128 KB) parsed asynchronously on a background
thread when opened.

## Results (release build, this machine)

| Step (ms, median) | 100 KB | 1000 KB |
|---|---:|---:|
| buffer: load from string | 0.05 | 0.16 |
| syntax: full parse | 14.5 | 141.7 |
| buffer: insert 1 char | ~0 | ~0 |
| buffer: `text()` full copy | 0.005 | 0.037 |
| buffer: `take_edits()` -> `InputEdit` (was `diff_as_edit` 0.10 / 0.97) | **~0** | **~0** |
| syntax: incremental reparse (1 char) | 0.20 | 2.1 |
| syntax: `update_fold_ranges` (was whole-tree 0.59 / 5.87) | **0.022** | **0.20** |
| language: `closing_labels_in_range` (~60 lines, was whole-text 0.42 / 4.14) | **0.12** | **0.12** |
| LSP `didChange` JSON (incremental 1 char, was full doc 0.05 / 0.47) | **~0** | **~0** |
| syntax: highlights, ~60 visible lines | 0.21 | 0.21 |
| syntax: `matching_bracket` (cursor on `{`) | 1.40 | 1.40 |

`matching_bracket` ran on every `render()` (hover, drag, cursor blink), up
to twice per frame. `EditorView` now caches the pair per cursor position
and text revision, so it only runs when the cursor moves or the text edits.

Sum of the per-edit rows: about **0.28 ms** at 100 KB (was 1.5 ms) and about **2.8 ms** at 1000 KB (was 14 ms) — well inside a 120 Hz frame (~8.3 ms) even at 1 MB.

## What the numbers showed

- The full-buffer `text()` copy and the whole-document `didChange` JSON are
  **cheap** (well under 1 ms at 1 MB).
- `Buffer::take_edits()` records exact `(row, col)` byte positions from the
  rope on each edit in `O(log N)`, eliminating the `O(N)` `diff_as_edit`
  prefix/suffix scan.
- `SyntaxTree::update_fold_ranges` shifts untouched folds in `O(K)` and only
  re-walks syntax subtrees overlapping the edited lines (`0.022 ms` at 100 KB,
  `0.20 ms` at 1 MB — a **~30x speedup** over walking the whole tree).
- `LanguageAdapter::closing_labels_in_range` computes closing widget/list
  annotations for the visible viewport with a bounded 32 KB lookback window,
  staying flat at **0.12 ms** regardless of file size.
- Visible-range highlighting and closing labels are both flat in file size.

## Project search: warm index vs. disk walk

Search is not part of the edit-path harness, so it gets its own numbers:

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

## Startup, memory and quit (black box)

`perf/bench.py` launches the real app on a project and measures it from the
outside (macOS, standard library only). Quit Shame first; it refuses to run
if an instance is open.

```bash
python3 perf/bench.py --project ~/path/to/app            # 3 runs + a discarded warm-up
python3 perf/bench.py --project ~/p --app zed=/Applications/Zed.app/Contents/MacOS/zed
```

"Ready" is the first moment the process tree's CPU stays under 0.1 core for
3 s. Shame's own process is tracked apart from the Dart tooling it spawns
(`dart`/`flutter` processes), because the analyzer's indexing would otherwise
be counted as editor work. Memory is macOS's physical footprint.

| `luna` (1,912 `.dart` files, M3 Pro), median of 3 | Shame only | Dart tooling | Total |
|---|---:|---:|---:|
| Ready (CPU settled) | 1.7 s | n/a | 23.1 s |
| CPU time until ready | 1.9 s | 11.6 s | 15.7 s |
| Memory footprint | 114 MB | 687 MB | 805 MB |

Idle CPU is about 3% of one core and quit takes about 0.2 s. The total is
what a Flutter user waits for; the first column is the editor's own cost.
The window-appears time is not measured (it needs Screen Recording
permission).

## Not measured yet

Keystroke-to-paint latency and scroll frame time need an instrumented GPUI
run, not a headless harness.
