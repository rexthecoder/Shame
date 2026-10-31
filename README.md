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

Daily-usable. Implemented: rope buffer with undo tree and multi-cursor,
tree-sitter highlighting, LSP (completion, hover, definition, references,
signature help, rename, formatting, code actions, organize imports,
diagnostics), DAP debugging, Flutter run / hot reload / devices / tests,
git panel and diff view, terminal dock, iOS simulator rail, web preview,
and an agent rail. See [`NOTES.md`](NOTES.md) for the development log and
[`docs/benchmarks.md`](docs/benchmarks.md) for performance numbers.

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

## Keyboard shortcuts

| Shortcut | Action |
|---|---|
| ⌘P / ⇧⌘O / ⌘G | Quick open / go to symbol / go to line |
| ⇧⌘F | Find in project |
| ⌘T | Go to symbol in workspace |
| ⌘D | Select next occurrence (multi-cursor) |
| ⌥⌘↑ / ⌥⌘↓ | Add cursor above / below |
| ⇧F12 | Find references |
| F2 | Rename symbol |
| ⌘. | Code actions |
| Tab | Next snippet tab stop |

Everything else is in the command palette.

## License

MIT, see [`LICENSE`](LICENSE).
