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
and an agent rail whose thread is saved per workspace. See
[`NOTES.md`](NOTES.md) for the development log and
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

## Agent modes

| Mode | What happens |
|---|---|
| **Ask** | Manual. Claude stops before it edits a file or runs a command and shows what it wants to do; you Allow, Allow for the session, or Deny. (Codex and OpenCode can't ask mid-run, so Ask is read-only for them.) |
| **Plan** | It investigates and writes a plan you can read in full, then you approve it (Edit or Auto) or send it back. Nothing changes until you approve. |
| **Edit** | Like Claude's "accept edits": file edits go through without asking; commands still ask. |
| **Auto** | Edits and runs commands without asking. |

Esc stops a running agent.

## Keyboard shortcuts

| Shortcut | Action |
|---|---|
| ⌘P / ⇧⌘O / ⌘G | Quick open / go to symbol / go to line |
| ⇧⌘F | Find in project |
| ⌘T | Go to symbol in workspace |
| ⌘L | Add selection (or file) to the agent chat |
| `@` in the agent input | Mention a file, folder, symbol, `@problems`, `@diff` or `@terminal` |
| Enter while the agent runs | Queue the message to send when it finishes |
| ⌘D | Select next occurrence (multi-cursor) |
| ⌥⌘↑ / ⌥⌘↓ | Add cursor above / below |
| ⇧F12 | Find references |
| F2 | Rename symbol |
| ⌘. | Code actions |
| Tab | Next snippet tab stop (names typed into a repeated placeholder are copied) |
| ⌥F12 | Peek definition (↩ opens it, ⌘↩ opens to the side) |
| ⇧⌘R | Refactor (wrap with widget, extract widget, ...) |
| ⌥⌘S | Save all |
| ⌥↓ / ⌥↑ | Next / previous change in a diff |
| ⌥→ / ⌥← | Next / previous file in a diff |
| ⌥↩ | Mark the open diff file reviewed |

Everything else is in the command palette.

## License

MIT, see [`LICENSE`](LICENSE).
