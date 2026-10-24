# Milestone notes

## M1: tabs

`DartIde` now holds `Vec<Tab>` (each a path + its own `Entity<EditorView>`)
instead of a single editor. Clicking a file in the tree opens it in a new
tab, or switches to it if it's already open (deduped by path); a tab bar
above the editor shows each tab's filename with a close (×) button.
Closing the last tab leaves a fresh blank one rather than an empty window.

Each tab gets its own `EditorView`/`Buffer`, so switching tabs doesn't lose
undo history, cursor position, or unsaved edits in the other tab — that
falls out of the design for free rather than needing extra state.

Not done: no "unsaved changes" confirmation when closing a dirty tab, no
keyboard shortcut to close/cycle tabs (Cmd+W, Cmd+1..9), and no
persistence of which tabs were open across restarts.

## M1: file tree wired to the editor

Clicking a file in the left tree now actually opens it in `editor-view`
(`EditorView::open_file`, calling `Buffer::open`) and focuses the editor;
clicking a folder still just toggles it, unaffected. Cmd/Ctrl+S saves back
to the open file's path via `Buffer::save` (silently does nothing for an
unsaved/untitled buffer — no "Save As" dialog yet). A one-line status bar
at the bottom of the window shows the open file's path and an "unsaved"
marker when `buffer.is_dirty()`.

This is the last of M1's "first tasks" list item 7 ("Add file open/save
wired to the file tree"). Not done yet: multiple tabs (opening a second
file replaces the buffer rather than opening a new tab — there's only one
`EditorView` / `Buffer` right now), and a Save-As flow for new files.

## M1: `editor-view`

`editor-view` is a real crate now too: a GPUI `Entity`/`Render` view
(`EditorView`) wrapping a `core-buffer::Buffer`, built only on raw
`gpui-kit` primitives (`div`, `Render`, `on_key_down`, `on_mouse_down`,
`FocusHandle`, clipboard) — **not** on `gpui_kit::component::input`'s
`Editor`/`Textarea`. It replaces the M0 `Textarea` stand-in in `app`.

What it does for real: renders every line with a gutter line number,
inserts/deletes text at the cursor, moves the cursor with the arrow keys
(up/down preserve column), Home/End, Backspace/Delete (char-boundary aware
via `core-buffer`'s new `prev_char_boundary`/`next_char_boundary`), Enter,
Tab (inserts 4 spaces), cmd/ctrl+C/X/V (clipboard), and cmd/ctrl+Z /
cmd/ctrl+shift+Z (undo/redo, via `core-buffer`'s undo tree). Clicking a
line moves the cursor there.

Verified: `cargo build`/`cargo test --workspace` pass (15 tests, all in
`core-buffer`; `editor-view` has none yet — see gap below), and the
release binary (19 MB) launches and stays resident with no panics. As
with M0, there was no way in this environment to drive real keystrokes
or mouse clicks into the unbundled binary's window to verify interaction
end-to-end by eye — that still needs an interactive session or a packaged
`.app` plus a window-automation tool that can reach it.

Known M1 scope cuts (see also the doc comment at the top of
`crates/editor-view/src/lib.rs`):
- No syntax highlighting yet (that's `core-syntax`, not started).
- No visual highlight for a non-empty selection range — only the cursor
  is drawn. Multi-character selection (shift+arrows, drag-select) isn't
  wired up at all yet, even though `core-buffer`'s `SelectionSet` already
  supports it.
- Mouse clicks snap to the start of the clicked *line*, not the clicked
  *column* — real column hit-testing needs text measurement (`shape_line`)
  that wasn't built yet.
- Every line is rendered on every frame; no virtualization, so a large
  file will eventually get slow. Matches the M1 performance budget's "open
  a 10MB file without freezing" item being unmet — flagging it rather than
  claiming it's handled.
- No test coverage in `editor-view` itself (a view/`Entity` is awkward to
  unit test without a GPUI test harness — `gpui-kit`'s `test-support`
  feature is the path there, not yet wired up). The logic it leans on
  (char boundaries, line/byte conversion, undo) is covered in
  `core-buffer`'s 15 tests instead.

## M1: `core-buffer`

`core-buffer` is now a real crate (no longer a stub): a `ropey`-backed
`Buffer` with byte-offset selections (`SelectionSet`, multi-cursor aware)
and an actual **undo tree** (`UndoTree`) — undoing and then making a new
edit opens a new branch instead of discarding the old redo path, which is
the real difference between a tree and a stack. 13 unit tests cover
insert/remove, undo/redo round-trips, branching, selection shifting across
edits, and a save/reopen round-trip through a temp file.

`ropey` 2.0.0-beta.1 turned out to index everything by **byte offset** by
default (char/UTF-16 indexing live behind `metric_chars`/`metric_utf16`
feature flags, with conversion methods, not inherent char-indexed
insert/remove/slice). `core-buffer`'s public API follows suit and works in
byte offsets throughout for M1; char/UTF-16 position mapping is deferred
to `core-lsp` (M2), which will need it for LSP's own position encoding
anyway.

## M0 notes

## What was built

A single binary crate (`crates/app`, binary name `dart-ide`) opens a GPUI
window via `gpui-kit` 0.8.0 with:

- A resizable split (`h_resizable`/`resizable_panel` from
  `gpui_kit::component::resizable`) with a 260px left pane and a flexible
  right pane.
- A file tree in the left pane (`gpui_kit::component::tree`), built from a
  real recursive scan of the project directory at launch (`.git` and
  `target` skipped).
- A plain multi-line text area in the right pane (`gpui_kit::component::
  input::Textarea`/`TextareaState`), pre-filled with a placeholder note.

All seven `core-*`/`editor-view` crates are stub libraries with no
dependencies yet (per "core crates have no UI dependency"); `app` is the
only crate wired to `gpui-kit`.

## Measured numbers (this machine: macOS/Darwin, Apple Silicon)

| Metric | Target | Measured | Verdict |
|---|---|---|---|
| Release binary size | < 80 MB installed | **23 MB** (`target/release/dart-ide`) | Pass, with large margin |
| Idle RAM (no LSP) | < 150 MB | **~60 MB** RSS after 38s idle | Pass, with large margin |
| Cold release build | — (not a runtime budget) | **1m 32s** wall (704s user / 804% CPU — parallel across cores) | Informational only |
| Startup to editable window | < 300 ms | **Not measured precisely** | See gap below |

### Startup-time gap

The binary is an unbundled executable (no `.app` bundle, no
`Info.plist`/bundle identifier), so it doesn't register as a normal macOS
application: it doesn't show up for window-level automation tools in this
environment, and there was no way here to capture "time from process start
to first paint" precisely. What was verified instead: the process starts,
stays resident, and produces no panics or errors over a 38-second idle run.

Follow-up for a real number: package `app` as a minimal `.app` bundle (or
just run it interactively) and time it by eye/stopwatch, or wire up GPUI's
`profiler` feature (`gpui-kit/profiler`) for frame-level timing. Flagging
this now per M0's own exit rule — the other two budgets (binary size, idle
RAM) are comfortably met, so there's no sign GPUI is a bad fit; the startup
number just needs a real measurement environment, not a reason to stop.

## Reading `gpui-component`'s own editor (reference for `editor-view`, M1)

Cloned `github.com/longbridge/gpui-component` (tag matching the published
`0.8.0` crates) to read the source directly rather than guess at the API
from docs.rs, which only shows partial/untested snippets for this crate.

The real editor is **not** `Textarea` (that's a plain multi-line field,
used as the M0 stand-in here) — it's `gpui_base::input::Editor` /
`EditorState`, exposed through `gpui_kit::component::input::{Editor,
EditorState}`. Per `crates/base/src/input/README.md`, the input module is
organized as:

- `base/` — the shared text-editing engine: state, layout, cursor,
  selection, movement, touch selection, masking, native text-input
  integration, and painting. This is the part most relevant to our own
  `editor-view` (cursor/selection/scroll/gutter/text drawing).
- `input/` — the single-line `Input` facade built on `base/`.
- `textarea/` — the multi-line `Textarea` facade built on `base/`.
- `editor/` — `Editor`/`EditorState`: display mapping, syntax highlighting,
  search, diagnostics, decorations, indentation, and LSP integration, all
  layered on top of the same `base/` engine.

So all three widgets (`Input`, `Textarea`, `Editor`) share one editing
engine and differ only in the facade on top — a strong argument for giving
our own `editor-view` the same shape later: one core text-interaction
engine, with a thin facade for "plain field" vs. "full code editor".

`EditorState` already demonstrates the shape of the LSP/syntax
integration we'll eventually build ourselves in `core-lsp`/`core-syntax`:
it takes pluggable `CompletionProvider`, `HoverProvider`,
`DefinitionProvider`, `CodeActionProvider`, and `DocumentColorProvider`
traits (see `examples/editor/src/main.rs`), each returning a `Task<Result<_>>`
(async, backed by `cx.background_spawn`), plus `LanguageRegistry` /
`LanguageConfig` for tree-sitter-based syntax highlighting, tab size,
soft wrap, line numbers, folding, and indent guides as state toggled via
`EditorState::set_*` methods. Useful as a reference for the *shape* of
`core-lsp` and `core-syntax`'s public API in M2, even though we'll write
our own engine rather than depend on this one.

## Gaps / corrections from the plan as written

- **`gpui-kit` re-exports, not raw `gpui`/`gpui-component` paths.** Docs
  examples and some docs.rs pages reference `gpui_component::...` paths
  directly, but with `gpui-kit` as the single dependency (as the plan's own
  stack section asks for — "GPUI Kit, which pins a matching GPUI release"),
  the real paths are `gpui_kit::component::...` (`gpui-component`
  re-exported as `component`) and `gpui_kit::base::...` (`gpui-base`
  re-exported as `base`, needed e.g. for `StyledExt`).
- **docs.rs is unreliable for this crate.** Its generated pages show
  partial, explicitly "untested" examples for `dock`, `tree`, and
  `input`/`Textarea` with no working end-to-end code. Cloning the actual
  repository and reading `examples/*/src/main.rs` plus
  `crates/story/src/stories/*.rs` was necessary to get compiling code —
  worth doing again for M1/M2 rather than trusting docs.rs alone.
- **Full `DockArea`/`Panel` dock system was not used for M0.** It exists
  (`gpui_kit::component::dock`) but its working example
  (`examples/dock/src/main.rs`) is built on the project's own internal
  `story` crate and panel-registry plumbing, not something a plain
  consumer binary sets up in a few lines. M0 uses the simpler
  `h_resizable`/`resizable_panel` split instead, which is real,
  documented-by-example, and sufficient for "a file tree panel and a plain
  text area." Revisit `DockArea` when M1+ needs persisted, draggable,
  closable panels (tabs, multiple open files).
