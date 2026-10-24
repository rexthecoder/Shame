# Dart/Flutter IDE

A small, fast code editor, first-class for Dart and Flutter. Built for daily
use first; open-sourced from day one.

The core (text buffer, syntax highlighting, LSP/DAP clients, Flutter runner)
and the editor view are written from scratch. The window, panels, and
general-purpose widgets come from [GPUI] via [gpui-kit] and [gpui-component].

[GPUI]: https://github.com/zed-industries/zed
[gpui-kit]: https://github.com/longbridge/gpui-kit
[gpui-component]: https://github.com/longbridge/gpui-component

## Status

**M0 (GPUI spike)**: a window with a resizable split layout, a file tree
panel reading the real project directory, and a plain text area. No real
buffer, syntax, or language-server wiring yet. See [`NOTES.md`](NOTES.md)
for the measured startup time, idle RAM, and binary size, and the exit
decision for this milestone.

## Crate layout

```
/crates
  core-buffer     rope, selections, undo tree, edits
  core-syntax     tree-sitter wrapper, highlight queries, folding
  core-lsp        JSON-RPC, LSP client, capability handling
  core-dap        DAP client
  core-flutter    flutter run --machine, devices, hot reload
  core-workspace  file tree, watcher, search, settings
  editor-view     GPUI editor element built on core-buffer + core-syntax
  app             window, panels, actions, wiring
```

The `core-*` crates have no UI dependency, so the GPUI-based UI layer
(`app`, `editor-view`) can be replaced later without touching them.

## Building and running

```bash
cargo run -p app
```

## Roadmap

See the project plan for the full milestone breakdown (M1: editor core,
M2: language smarts, M3: running Flutter, M4: debugging, M5: widget
inspector and git basics).

## License

MIT, see [`LICENSE`](LICENSE).
