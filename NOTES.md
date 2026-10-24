# Milestone notes

## Drop the traffic-light inset in native fullscreen

Follow-up to the git-branch title bar: the user pointed at Zed
specifically removing the titlebar's left gap once the window goes
native fullscreen. macOS hides the traffic lights once a window is
actually in that state (the real green-button fullscreen, not just a
large window), so the 78px this titlebar reserves for them is wasted
space there — `window.is_fullscreen()` now switches that inset down
to a plain 12px whenever it's true, matching what the space is
actually needed for in each state. Still reserves the full 78px in
ordinary windowed mode, where the real traffic lights are still there
and still need the room.

## Git branch in the title bar (`core-workspace` starts for real)

User asked for a branch indicator in the title bar like other editors
show (referenced another app's title bar as the look to aim for). This
is `core-workspace`'s first real code — that crate was a stub since M0
("file tree, file watcher, search, and settings"), and the app's file
tree/Quick Open list still walk the filesystem directly in `app`
rather than through it; `current_branch()` is the first piece actually
built there, not a redesign of the rest.

Same shelling-out pattern `core-flutter` uses for `flutter`/`dart`:
`git rev-parse --abbrev-ref HEAD` in the project directory. The one
real wrinkle, confirmed by testing rather than assumed: a **detached**
HEAD (checked out a commit directly, not a branch) makes that command
print the literal string `"HEAD"`, not a real branch name — worth
treating as "no branch to show" rather than literally displaying
"HEAD" as if it meant something to the user. Verified all three cases
against the real `git` CLI in temp repos: a fresh commit on its
default branch, an explicit `checkout -b` onto a named branch, and a
real detached HEAD (not simulated) — the title bar now shows the
project name plus a small branch pill next to it when `git_branch` is
`Some`, and nothing extra when it's `None` (not a repo, or detached).

67 tests total now.

## M4: stepping and expression evaluation

Rounds out `DapClient`'s session methods: `next` (step over), `step_in`,
`step_out`, and `evaluate` (watch/REPL/hover expressions), alongside
the `launch`/breakpoints/`continue_thread` set from the previous
commit.

Verified against the real `dart debug_adapter` the same way as the
earlier breakpoint test: drove a real paused session, then `next` →
watched the real `continued` → `stopped (reason: "step")` event pair
arrive (same shape for `stepIn`/`stepOut`, confirmed too) — locked into
`deserializes_a_real_step_response_and_event_sequence`. `evaluate`
turned up a genuine error worth keeping as a real captured shape
rather than inventing one: sent `evaluate` for `a + b` right after a
step and got back `success: false`, `"Evaluation is only supported
when the debugger is paused unless you have a Dart file active in the
editor"` — same error envelope (`message` + structured `body.error`)
`core-dap` already handles for any other failed request, now locked
into `deserializes_a_real_evaluate_error_response`.

64 tests total now. This closes out `core-dap`'s backend session
surface — breakpoints, run control, stepping, and expression
evaluation are all there; what's left in M4 (breakpoint gutter,
variables panel, debug console) is UI work.

## UI pass: a real custom titlebar, language-aware status bar, code icons

Third round of the chrome work. The previous round reverted to the
plain *system* titlebar to fix a double-bar look, but that left the
titlebar looking "incomplete" — most likely this specific macOS
version (`gpui-pre`'s own source has comments about titlebar bugs
"first observed on macOS 27", which is this machine's exact Darwin
version) rendering a default NSWindow titlebar oddly. Rather than keep
fighting the native one, switched to drawing a single custom titlebar
ourselves — `appears_transparent: true` plus an inset
`traffic_light_position`, the same thing Zed and Fleet both actually
do, for the same reason: it renders identically regardless of how a
given OS version happens to draw its own chrome, instead of depending
on it.

This is the *same* approach the previous round tried and reverted —
but that revert mis-attributed the cause. The real bug (CPU pegged,
window never becoming visible at all) was isolated by testing each
variable independently: it reproduced with the plain system titlebar
too, at a window sized to the display's own bounds — so it was the
*bounds*, not `appears_transparent`. With the fixed-size window
(1600×1000, not display-bounds-derived) already in place from that
fix, combining it with the custom titlebar is safe: verified via CPU
and log health (0% CPU at idle, no error output) rather than a
screenshot, since the user asked not to keep spending their screen
access on this — a legitimate ask; screenshots are a diagnostic tool
for me, not something to lean on by default.

Two smaller, concretely-justified polish items alongside it:
- File tree icons now distinguish source code (`.rs`, `.dart`, `.ts`,
  `.py`, etc. — `FileCode`) from everything else (`File`) — the same
  "this is code, not config/docs" at-a-glance cue every mainstream
  editor's file tree gives, even without per-language icon art
  (`gpui-component`'s bundled icon set has no Rust/Dart logos to use).
  `FileCode` isn't in `gpui-component`'s own curated icon subset, so
  this reaches into the full asset set
  (`gpui_kit::assets::IconName`) instead — both implement the same
  `IconNamed` trait `Icon::new` accepts, so they mix freely.
- The status bar's language tag is no longer Dart-only
  (`language_label()`) — every mainstream editor's status bar names
  the current file's language unconditionally, not just for the one
  language the project happens to care about most.

## UI pass: fixing the double titlebar + a real window-creation bug

Follow-up to the first chrome pass, after the user reported the window
looking small/cramped. Root-caused two separate real bugs, both found
by actually getting eyes on the running app rather than guessing from
code:

**Double titlebar.** The first pass added a 40px custom-drawn header
row (app name, folder breadcrumb, Open Folder button) *below* the
default system titlebar — meaning the window had two stacked bars,
the real one (with "dart-ide" as its title, from the binary name) and
the fake one, which read as broken/cramped rather than "styled."
Tried the standard fix for this (`titlebar: { appears_transparent:
true, traffic_light_position: ... }`, the approach Zed itself uses,
hiding the system bar so the custom one is the only one) — but see
below, reverted. Landed on the simpler fix instead: dropped the custom
header row entirely and kept the system titlebar (given a real title
via `TitlebarOptions { title: Some("Dart/Flutter IDE".into()), ..
Default::default() }`), moving the folder name + Open Folder button
into the sidebar's header row instead, where they fit naturally next
to each other.

**A real window-creation bug, found while chasing the above.** Setting
`appears_transparent: true` and sizing the window to
`WindowBounds::Maximized(...)` or to `display.bounds()` directly both
reliably reproduced the same failure on this machine: the process
shows as frontmost (confirmed via the menu bar) but no window ever
becomes visible on screen, while a CPU core pegs at 100% and memory
climbs without bound — a real resize/relayout feedback loop, not a
cosmetic issue. Verified this is unrelated to `appears_transparent`
specifically (reproduced with the plain system titlebar too) and is
specific to the window's bounds matching the *display's own* bounds —
a window sized to a large but arbitrary rectangle (1600×1000, centered)
behaves fine. `gpui-pre`'s own source carries comments about titlebar
bugs "first observed on macOS 27", this machine's exact Darwin
version, so this reads as a real platform compatibility gap in this
GPUI version rather than something to keep chasing a styling fix for.
Landed on a large fixed-size centered window instead of anything
display-bounds-derived — still reads as a real, substantial app window,
without touching the broken code path.

**How this got found**: wrapped the release binary in a minimal,
throwaway `.app` bundle (`target/DartIDE.app`, registered with
`lsregister`, never committed) purely so `computer-use`'s screenshot
tooling could target it by bundle id — a bare `cargo run`/binary isn't
indexed and can't be granted access. That unblocked actually seeing
the running window instead of guessing from the user's cropped
screenshots, which is how both bugs above were actually diagnosed
rather than guessed at. Also surfaced a separate, *unexplained*
oddity worth flagging rather than quietly working around: the same
binary launched via that `.app` bundle (through `open`/LaunchServices)
never shows a visible window at all regardless of size/titlebar
settings, while launching the identical binary directly from a shell
(`./target/release/dart-ide`, how the user actually runs it and how
`cargo run` works) is completely healthy. Did not chase this further —
it's a quirk of the throwaway bundle-launch path, not the app itself.

## UI pass: Zed/Fleet-inspired chrome (first iteration)

First real pass at the visual design that's been deliberately deferred
since M0 — "defer UI work" meant *new interactive widgets*, not that the
shipped chrome had to stay a bare M0 scaffold forever, and the user
asked directly for this, inspired by Zed and Fleet.

Switched to `gpui-component`'s dark theme by default
(`Theme::change(ThemeMode::Dark, ...)` in `main()`) rather than hand-
rolling a palette: its default dark theme already sits close to Zed/
Fleet's own look (near-black `neutral-950` backgrounds, a blue
accent/selection), and it comes with a full set of purpose-built
tokens — `title_bar`/`title_bar_border`, `sidebar`/`sidebar_border`/
`sidebar_foreground`, `tab_bar`/`tab`/`tab_active`/`tab_foreground`,
`status_bar`/`status_bar_border`, `selection`, `caret` — that this app
wasn't using at all before (everything was `cx.theme().background`/
`border` or flat-out hardcoded hex). Re-themed every chrome surface
through those tokens instead of improvising new colors, so dark/light
mode and any future custom theme file actually work app-wide, not just
on the parts someone remembered to update.

Concretely, in `app`:
- A slim 40px title bar (was a bulky header with the full folder path
  crammed into a button label) — app name, a `/ <folder name>` instead
  of the full path, an icon-only Open Folder button with a tooltip.
- The file tree now sits in an actual sidebar panel (`sidebar`
  background + border, distinct from the editor background) with an
  uppercase folder-name header, VS Code/Zed/Fleet's shared convention.
- Tabs redesigned Zed-style: the active tab gets a 2px top accent
  border instead of a flat highlight fill; the unsaved indicator is a
  small dot (color-coded) that swaps to a close "×" on hover, instead
  of a permanent "× unsaved" text suffix competing with the title.
- The status bar is now an actual status bar — `Ln/Col` (new
  `EditorView::cursor_position()`), colored error/warning counts with
  icons instead of a plain-text suffix, a `Dart` language tag — laid
  out left (file + dirty dot) / right (diagnostics, position,
  language), the same left/right split Zed and Fleet both use.
- The three floating overlays (Quick Open, rename, code actions) now
  use the theme's `popover` token and a real drop shadow
  (`shadow_xl()`) instead of a flat bordered box with no elevation.

In `editor-view`: the editor's own background/foreground, selection
highlight, and caret now pull from the same theme tokens
(`background`, `foreground`, `selection`, `caret`) instead of
hardcoded VS-Code-dark hex, so the editor pane and the chrome around
it are visually one surface, not two different apps glued together.
Added a subtle active-line highlight (`theme().muted` at low opacity)
— present in every mainstream editor and previously missing here
entirely. Syntax token colors (keyword/string/comment/etc.) are left
as hardcoded hex for now — that's a separate, genuinely independent
concern (a syntax theme, not a UI chrome theme) and VS-Code-Dark+-like
colors read fine against the new near-black background; revisiting
that is its own task, not bundled into this pass.

**Not independently screenshot-verified this round**: `cargo build`/
`cargo run -p app` both succeed and the full 61-test suite still
passes (this was a styling-only change, no logic touched), but the
running window is a bare `target/debug/dart-ide` binary with no `.app`
bundle, so the computer-use screenshot tool couldn't be granted access
to it to confirm the pixels — that needs an actual look from a human,
or bundling the binary as a proper `.app` first.

## M4: session convenience methods + a real breakpoint hit

Added typed convenience methods on `DapClient` for the session flow a
real debugger needs — `launch`, `set_breakpoints`, `configuration_done`,
`threads`, `stack_trace`, `scopes`, `variables`, `continue_thread`,
`disconnect` — each a thin wrapper over `request()` building the right
`arguments` shape, same pattern as `FlutterDaemon::hot_reload`/
`hot_restart` wrapping `request()` in `core-flutter`. Plus
`wait_for_event()` alongside the existing `wait_for_response()`.

Verified by actually hitting a breakpoint, not just sending requests
and hoping: wrote a throwaway two-function Dart script by hand, drove
`dart debug_adapter` through a full real session (a small Python
driver script, piping framed JSON directly rather than through a FIFO,
made this far easier to iterate on than the bash+FIFO technique used
for earlier crates) — `initialize` → `launch` → `setBreakpoints` →
`configurationDone` → watched the **real sequence of events** arrive:
`thread` (started) → `stopped` (`reason: "entry"`) → `breakpoint`
(`reason: "changed"`, `verified: true` — breakpoints start out
*unverified* until the VM actually resolves them) → `continued` →
`stopped` (`reason: "breakpoint"`, this is the one that matters) — then
sent `threads`, `stackTrace` (top frame really was `add`, the function
the breakpoint was in), `scopes`/`variables` (both came back empty —
turns out `frameId`/`variablesReference` aren't `0`/`1`-style indices,
they're opaque ids the adapter hands back in `stackTrace`'s response
and would need to be threaded through for real use, not hardcoded —
noted but not followed up further since that's exactly the kind of
detail a real UI integration will need to get right, not this
verification pass), `continue`, then `disconnect`.

That same shape — launch a real scratch script, set a real breakpoint,
run to it, inspect the real stack — is now an automated test
(`hits_a_real_breakpoint_in_a_real_dart_script`), not just a manual
one-off: it writes its own throwaway `.dart` file to a temp dir at test
time (no checked-in fixture needed, unlike the Flutter project
`core-flutter`'s pub tests needed — a plain `.dart` file is instant to
write, no `flutter create` involved), asserts the adapter actually
stops with `reason: "breakpoint"` and that the top stack frame is
really `add`, then cleans up. 61 tests total now.

## M4 start: `core-dap` (real DAP client, no UI)

Same approach M2 (`core-lsp`) and M3 (`core-flutter`) both started
with: build and verify the real protocol client first, zero GPUI
wiring. Breakpoints, stepping, variable inspection, a debug console —
all UI work, staying deferred.

The Debug Adapter Protocol's base framing is *literally* LSP's
`Content-Length` "Base Protocol" (the DAP spec says so directly), so
`core-dap::transport` is a byte-for-byte duplicate of
`core-lsp::transport` rather than a shared dependency — keeping this
crate independent the same way `core-flutter` doesn't depend on
`core-lsp` either, even though both speak JSON-over-stdio. Where DAP
actually differs is the message envelope once you're past framing,
confirmed by hand against the real `dart debug_adapter` (piped
through a FIFO, same technique as every other protocol crate this
project has built):

- **Every message carries its own `seq`, assigned by whichever side
  sent it** — not a single shared id space like LSP's `id`. Requests
  are matched to replies via `request_seq` on the response, which
  echoes the request's `seq`.
- **Responses carry `success: bool` plus `command`**, not an
  `error`/`result` split: a real captured success looks like
  `{"seq":1,"type":"response","body":{...},"command":"initialize","request_seq":1,"success":true}`;
  a real captured failure (sent an unrecognized command on purpose to
  see the shape) is
  `{"seq":3,"type":"response","body":{"error":{...}},"command":"notARealCommand","message":"Unknown command notARealCommand","request_seq":2,"success":false}`
  — note the failure has *both* a top-level `message` string and a
  structured `body.error`, so `DapMessage::Response`'s `Err` case
  keeps both rather than picking one.
- **Events** (`stopped`, `output`, `terminated`, and in this case the
  handshake's own `initialized`) are `{"seq":N,"type":"event","body":
  {...},"event":"name"}` — real captured example:
  `{"seq":2,"type":"event","body":{},"event":"initialized"}`.
- **The adapter can send its own requests to the client**
  (`runInTerminal`, `startDebugging`) the same way Dart's analysis
  server does with `workspace/applyEdit` in M2 — `DapMessage::Request`
  covers this from the start this time, rather than needing a
  follow-up fix like `core-lsp` did.

Verified a real `initialize` handshake against `dart debug_adapter`
end-to-end (not just the parser tests against the captured JSON above):
spawned it for real, sent `initialize`, got back
`supportsConfigurationDoneRequest: true` plus the full
`exceptionBreakpointFilters` list, then watched the `initialized` event
arrive right after. 60 tests total now.

## M3: SDK detection

The last item on M3's plan list. `flutter --version --machine` gives a
single JSON object (not an array, unlike `devices --machine`) with the
Flutter/Dart SDK versions, channel, and `flutterRoot` — confirmed by
running it for real on this machine (`fvm`-managed Flutter 3.47.2,
Dart 3.13.2, under `/Users/bushadigitalltd/fvm/versions/3.47.2`).

The actual point of this isn't just displaying a version somewhere —
it's `FlutterSdk::dart_sdk_dir()`, which derives
`<flutterRoot>/bin/cache/dart-sdk`, the real on-disk location of the
Dart SDK bundled *inside* that specific Flutter SDK. Confirmed that
directory exists for real on this machine. `core-lsp` currently just
spawns a bare `dart` and hopes it's on `PATH`; once there's reason to
touch that code path, this is what should drive which `dart` (and
which `analysis_server.dart.snapshot`) actually gets launched, instead
of silently trusting whatever `dart` resolves to — the whole reason
this crate's doc comment calls `flutter`/`dart` on `PATH` an
assumption rather than a guarantee.

`detect_sdk()` follows the same one-shot-command shape as
`list_devices()`: no daemon, no streaming, just run it and parse the
captured stdout. Real JSON locked into
`deserializes_real_flutter_version_machine_output`;
`detects_the_real_sdk_on_this_machine` re-runs the actual CLI on every
test run (skips gracefully if `flutter` isn't on `PATH`) and asserts
`flutter_root` is a real directory, not just a string. 49 tests total
now — this closes out M3's entire backend-only scope; everything left
in the plan's M3 list (device picker, Run/Stop button, log panel) is
UI work staying deferred.

## M3: pubspec.yaml helpers (`pub add` / `pub get`)

Rounded out M3's plan item "pubspec.yaml helpers: add package, pub get"
in `core-flutter`. Both are one-shot commands (`flutter pub add
<package>` / `flutter pub add dev:<package>`, `flutter pub get`), not
part of the daemon protocol, so this is a plain `Command::output()`
wrapper, same shape as `list_devices()` — no streaming, no daemon
state. `PubOutput { success, stdout, stderr }` keeps the raw output
rather than collapsing to a bool, since a future UI showing *why* a
`pub get` failed (version conflict, network error) needs the real
text, not just pass/fail.

**Verified against the real CLI**, not just written against docs: ran
`flutter pub add path` and `flutter pub add dev:very_good_analysis`
by hand against a real `flutter create`d scratch project, confirmed
`pubspec.yaml` actually gained the `path:`/`very_good_analysis:`
entries each time, then ran `flutter pub get` and watched it succeed.
That same sequence is captured as an automated test
(`adds_a_real_package_and_runs_pub_get_against_the_real_cli`), but
gated behind a `FLUTTER_TEST_PROJECT` env var pointing at a real
`flutter create`d project rather than running unconditionally — a
`flutter create` at test time would fetch packages and take too long
to belong in routine `cargo test`, and a scratch project checked into
the repo would be the kind of generated, disposable fixture that
doesn't belong there either. Skips gracefully (prints why, doesn't
fail) when that env var isn't set or `flutter` isn't on `PATH`. 47
tests total now.

## M3 start: `core-flutter` (backend only)

Started M3 the same way M2 started with `core-lsp`: build the
real-verified backend crate first, no GPUI wiring — a device picker,
Run/Stop button, and log panel are all UI work, staying deferred along
with everything else UI-shaped. `flutter` (3.47.2, via `fvm`) and a
real `flutter create --platforms=macos` project were both already on
this machine.

`flutter run --machine` speaks a genuinely different wire protocol from
LSP, confirmed by hand before writing any Rust:
- **Framing is line-delimited, not `Content-Length`-framed.** Each
  structured message is one line: a JSON *array* holding one object —
  `[{"event": "...", "params": {...}}]` for events,
  `[{"id": N, "result": ...}]` for replies to our commands. Other lines
  (build progress text, the app's own `print()` output, a checkmark
  line like `✓ Built ...`) are plain text, not JSON at all — kept as
  `DaemonOutput::Log` rather than discarded, since that's exactly what
  a future log panel needs, interleaved in the same order they arrived.
- **`flutter devices --machine` is a *different* one-shot command**,
  not part of the long-running daemon: it pretty-prints a single
  multi-line JSON array and exits, so `list_devices()` just waits for
  the process and parses the whole captured stdout, no line-by-line
  streaming needed there.
- **Commands and their id namespace are per-process**, same shape as
  LSP's request/response correlation but wrapped in that one-element
  array: `[{"id": 1, "method": "app.restart", "params": {"appId":
  "...", "fullRestart": false, "pause": false}}]\n` is hot reload
  (`fullRestart: true` is hot restart); `app.stop` ends the run.

**Verified against the real toolchain, full lifecycle, by hand**: ran
`flutter devices --machine` for real (it found the iOS Simulator,
`macos` itself, and Chrome); ran `flutter run --machine -d macos`
against the scratch project through to `app.started` (~12s, matching
the real build), sent a real hot reload and watched the
`app.progress`/`hot.reload` events and the `{"code":0,"message":
"Reloaded 0 libraries"}` response arrive; then on a second run, sent
`app.stop` and confirmed both the `app.stop` *event* and the
`{"result":true}` *response* came back, and that the daemon process
exited cleanly on its own afterward. None of that full-lifecycle
sequence is automated (a real `flutter run -d macos` run is 10-15+
seconds and needs a buildable project — too slow/fragile for routine
`cargo test`), but the exact lines captured during it are locked into
`parses_real_captured_daemon_lines`, and `flutter devices --machine`
itself *is* re-run for real on every test run
(`lists_real_devices_on_this_machine`), same balance core-lsp struck
between manual full-protocol verification and automated parser tests.
46 tests total now.

## M2: code actions (cmd/ctrl+.) — the last item on the original list

`textDocument/codeAction` turned up something the plan didn't call out
and that would have been easy to get wrong without checking: **the real
Dart analysis server never returns a direct edit for a quick-fix.**
Every single result is `CodeActionOrCommand::Command` — `{"command":
"dart.edit.codeAction.apply", "arguments": [...], "title": "..."}` — not
a `CodeAction` with an `edit` field. Applying one means a second round
trip:

1. `workspace/executeCommand` with that same command + arguments.
2. The server replies to *that*, but the actual edit shows up
   separately, as the server making **its own request to us**:
   `workspace/applyEdit`, carrying the `WorkspaceEdit` and expecting a
   `{"applied": true}` reply.

Step 2 needed a real extension to `core-lsp`, not just new code in
`editor-view`: the client only knew how to parse `Response`s (ours,
matched by id) and `Notification`s (no id). A message with an `id` but
no `result`/`error` used to be silently dropped — treated as noise
rather than what it actually is here. Added `ServerMessage::Request`
for this case, plus `LspClient::respond()` to answer it. The existing
test for "message with id, no result/error" had its premise checked
against this change and rewritten as a test for the new variant, rather
than left asserting the old (now wrong) behavior.

`EditorView::apply_code_action` handles both shapes anyway (direct edit
or command) so the logic isn't Dart-specific, even though in practice
every response seen so far from this server is a command. `cmd/ctrl+.`
sends the request for whatever diagnostics are on the cursor's line; a
single result applies immediately, more than one emits
`EditorEvent::ShowCodeActions` for `app` to show a picker — same
`ListItem`-in-an-overlay pattern as the file tree and Quick Open, not a
new design.

**Verified against the real server, including the full round trip**: an
unused `import 'dart:math';` produced 3 commands from
`textDocument/codeAction`; manually running `workspace/executeCommand`
for the first one confirmed the server really does send back an
unsolicited `workspace/applyEdit` *request* (not a response) with the
edit that removes the import line. Locked in with two permanent tests
(the command-shaped code action list, and the `applyEdit` request
params) — 42 tests total now.

This closes every item on the plan's original M2 list except
completion, which stays deferred to the UI pass.

## M2: rename (F2 / cmd+ctrl+R)

`textDocument/rename` is wired up, but it's also the first feature that
genuinely needed user text input (a new name) — which `editor-view`
deliberately won't build itself (a text field is exactly the kind of
pre-built widget that crate avoids; see its module docs). So the flow
splits across the boundary that `EditorEvent` already established for
go-to-definition:

- `EditorView::start_rename` (F2, or cmd/ctrl+R as a hedge in case this
  GPUI version doesn't name the F2 key `"f2"` — both are wired) computes
  the cursor's line/column and emits `EditorEvent::RequestRename`.
- `app::DartIde` catches it and opens a small prompt — same `Input` +
  overlay pattern as the Quick Open palette, not a new design, since the
  user explicitly OK'd reusing established patterns rather than building
  new UI for this.
- Enter calls a new public `EditorView::send_rename(line, column,
  new_name)`; Escape just clears the prompt.
- The response (`WorkspaceEdit`) only has `changes`, not
  `documentChanges`, for the Dart server — `apply_text_edits` (already
  built for formatting) handles each file's edit list, but **only the
  edits for this buffer's own URI are applied**. A rename that also
  touches other files (very possible — rename a method and every call
  site across the project comes back) silently leaves those other files
  unchanged. This is a real, user-visible limitation, not a nitpick:
  worth fixing before relying on this for anything but a local rename.

**Verified against the real server**: renamed `greet` to `sayHello` in a
sample file with both a declaration and a call site; the response
correctly contained both edits in one same-file entry. Added a
permanent test (same approach as formatting's) using that exact
captured JSON to lock in that `WorkspaceEdit.changes` deserializes
correctly — 39 tests total now.

## M2: formatting (cmd/ctrl+shift+F)

`textDocument/formatting` is wired up: the shortcut sends a format
request for the whole buffer; the response's `TextEdit`s are applied to
`core-buffer` via a new `apply_text_edits`, sorted and then applied
*last edit first* — each edit's (line, character) position is defined
relative to the original document, not to the result of earlier edits
in the same batch, so applying from the bottom up is what keeps
not-yet-applied edits' positions valid (standard technique for
multi-edit application, not specific to this server).

**Verified against the real server**: a deliberately badly-spaced
sample (`class Greeter{` with no space, over-indented body) produced
exactly 3 edits from the real `dart language-server` — two single-space
insertions and one indent trim. Went a step further than the earlier
protocol-level checks: added a permanent unit test
(`deserializes_a_real_server_format_response`) using that *exact*
captured JSON (not synthetic data) to lock in that `newText`/`range`
deserializes into `TextEdit` the way `apply_text_edits` expects — this
one stays in the suite rather than being a one-off check, since format
responses are exactly the kind of "camelCase JSON edit list" shape that
silently rotting serde derives would get wrong without anyone noticing
until a real edit came out garbled.

Known gap: formatting is manual-trigger only (cmd/ctrl+shift+F) — no
format-on-save. Wiring that up means waiting for the format response
before `save()` actually runs, and `save()` currently saves
synchronously/immediately; a real "format then save" needs its own
small state machine, not attempted here.

## M2: go-to-definition (cmd+click)

`textDocument/definition` is wired up: cmd/ctrl+click a symbol sends the
request at that line/column. The response can come back three different
shapes (`GotoDefinitionResponse::{Scalar, Array, Link}` — a single
location, several, or `LocationLink`s with separate target/selection
ranges); `first_definition_target` picks the first one regardless of
shape, since this app doesn't yet have UI for "pick one of several"
(documented as a gap below).

The actually interesting part is what happens next, since Dart lets you
jump *within* the same file or *into a different one*:
- **Same file**: just move the cursor — `EditorView` already has
  everything it needs.
- **Different file**: `EditorView` has no idea tabs exist (by design —
  see the crate's module docs), so it `cx.emit`s a new `EditorEvent::
  OpenAt { path, line, column }`. `app::DartIde` subscribes to this per
  tab (`cx.subscribe_in`, kept alive in a new `_subscriptions` field) and
  handles it by calling its own existing `open_path` (same one the file
  tree and Quick Open already use — opens-or-switches-to a tab) and then
  calling `EditorView::set_cursor_to_position` on whichever tab ends up
  active. This is the first thing in the codebase that needed a GPUI
  event to cross the `editor-view` → `app` boundary, and it's a clean
  precedent for whatever needs it next (diagnostics click-to-jump,
  "Peek Definition", etc.).

**Verified against the real server, protocol-level, both directions**:
built a two-file sample project (`greeter.dart` + `main.dart` importing
it) and drove the real `dart language-server` by hand through the full
sequence for a definition request:
- *Within* `greeter.dart` (method call inside the same file) — resolved
  correctly to the method's own declaration, same file.
- *From* `main.dart` to `greeter.dart` (a `Greeter()` constructor call)
  — resolved correctly across files, confirming
  `GotoDefinitionResponse::Array` deserializes and the cross-file path
  works.
- Re-ran the cross-file case with `rootUri: null` — exactly what this
  app's own `InitializeParams` actually sends (no project root is set
  up yet) — and it still resolved correctly, so the real app's
  simplified initialize call isn't silently breaking this.

Known gaps: no visual indication a symbol is cmd+click-able before you
click it (no underline-on-hover-with-modifier, which most editors show);
multiple definition candidates always pick the first one silently
rather than letting you choose.

## M2: hover

`textDocument/hover` is wired up: hovering over code for ~300ms sends a
hover request at that line/column; the response renders as a small box
under the hovered line. Reuses the same per-line `canvas()` overlay
built for column-accurate clicks — it already turns a pixel position
into a byte column, so hover just adds a third `MouseMoveEvent` listener
(no button held) alongside the existing click/drag ones, debounced via
`smol::Timer::after` + a sequence number (a new hover position bumps the
sequence; when the timer fires, it only sends the request if the
sequence hasn't moved on since).

**Verified against the real `dart language-server`, protocol-level**:
rather than trying to simulate an actual mouse hover in the GUI (not
practical without display/input access in this environment), drove the
real server by hand through `initialize` → `initialized` → `didOpen` →
`textDocument/hover` for a known position (`greet` in a small sample
file) via the same FIFO-piping approach used earlier for `initialize`.
The response's `contents` came back as a bare markdown string —
confirming it deserializes as `HoverContents::Scalar(MarkedString::
String(..))`, exactly the shape `hover_contents_text` already handles.
Combined with the already-proven `cx.spawn`/`handle_lsp_message`
dispatch pattern from diagnostics (same mechanism, different message),
this gives strong confidence without needing a second full rebuild +
simulated-interaction cycle.

Known gaps: the popup shows raw markdown (code fences and all), not
rendered; it's positioned at a fixed spot below the line, not anchored
to the exact column; and it doesn't clear if the mouse leaves the editor
entirely without the user typing first (only on keystroke, or moving to
a new hover position) — see the scope-cut list at the top of
`lsp_integration.rs`.

## Open Folder (cmd+O)

Found while trying to let the user test M2's diagnostics: the app always
rooted the file tree and Quick Open at `std::env::current_dir()` with no
way to point it anywhere else — a real gap, not a corner case, since
most people won't launch this from a terminal already sitting in the
Dart/Flutter project they want to edit.

Added a native folder picker (`cx.prompt_for_paths` with
`directories: true, files: false`) behind cmd/ctrl+O and an "Open
Folder…" button in the header (showing the current root). Picking a
folder rebuilds the file tree (`TreeState::set_items`) and the Quick
Open file list for the new root; open tabs are left alone rather than
closed out from under the user.

## M2 first slice: live diagnostics from the real `dart language-server`

Wired `core-lsp` into `editor-view` (new `lsp_integration.rs` module) and
verified it against the real analysis server, not just by reading the
code:

- A `.dart` buffer lazily spawns `dart language-server`, does the
  `initialize` → `initialized` → `textDocument/didOpen` handshake, then
  sends `textDocument/didChange` (full-document sync, no incremental
  diffing — always valid LSP, much simpler) whenever the buffer's text
  actually changes, reusing the same "did the text change since last
  render" check already there for syntax reparsing.
- `textDocument/publishDiagnostics` notifications are parsed into
  `lsp_types::Diagnostic` and used two ways: `EditorView::
  diagnostic_summary()` (error/warning counts, shown in `app`'s status
  bar) and a per-line "worst severity" map that colors each line's
  gutter number red (error) or yellow (warning) — column-level
  underlines are a later increment, not this one.

**Verified for real**, not assumed: wrote `sample_error.dart` (a class
with a missing semicolon) in the scratchpad, temporarily pointed the
app's startup buffer at it, rebuilt, and ran it for real. Confirmed two
things from the outside, without touching the UI:
1. `ps` showed a real `dart language-server` process as a **child** of
   the running `dart-ide` process — proving `maybe_start_lsp` actually
   fires, not just compiles.
2. A temporary `eprintln!` in the diagnostics handler, with stderr
   captured to a log file, showed the exact diagnostics the real
   analysis server sent back: two "Expected to find ';'." errors and one
   "Dead code" warning — exactly what that file should produce. Removed
   the logging and the temporary startup change afterward; this was a
   one-off verification, not a permanent behavior change.

Known gaps for the rest of M2: no completion, hover, go-to-definition,
rename, formatting, or code actions yet — diagnostics only. One
analysis server per open Dart *tab*, not one shared per project (simple,
but wasteful with several Dart files open at once). No automated test
exercises this path end-to-end inside the test suite (it needs
`gpui-kit`'s `test-support` + a real subprocess, which is a bigger lift
than the manual verification above) — `editor-view` still has zero
automated tests of its own, same gap noted after M1's scroll fixes.

## M2 start: `core-lsp`

`core-lsp` is a real crate now: our own JSON-RPC client for the LSP "Base
Protocol" (per the plan — write the client ourselves, use `lsp_types` for
the protocol's own types rather than hand-rolling those too). It's a
plain transport + request/notification/event client, with zero Dart- or
LSP-method-specific logic and zero UI dependency:

- `transport::{read_message, write_message}` — `Content-Length`-framed
  JSON over any `Read`/`Write`, tested against in-memory byte streams
  (no process needed).
- `client::LspClient` — spawns a server process, owns its stdin, and
  runs a background thread that frames/parses stdout into
  `ServerMessage::{Response, Notification}`, delivered over an
  `async_channel` (not `std::sync::mpsc`) so a GPUI entity can `.recv()`
  them with real `.await` later instead of blocking an executor thread.

**Verified against the real `dart language-server`, not just mocks**:
`dart language-server` (bundled with the Dart SDK already on this
machine, via `fvm`) defaults to speaking LSP already — no extra flags
needed. Confirmed by hand first (a raw `printf`-framed `initialize`
request piped through a FIFO) before writing any Rust, then again as an
actual `#[test]` that spawns the real process, sends `initialize`, and
asserts the real capabilities come back — skipped, not failed, on a
machine without `dart` on `PATH`. 10 tests total, including that one.

## M1 close-out: column-accurate click and drag-select

The last open M1 item: mouse clicks and drag now hit the exact character,
not just the clicked line. This needed GPUI's real text shaper, not a
monospace-width guess:

- Each line's text area gets an invisible `canvas()` overlay the same
  size as the text, used only for hit-testing (nothing from it is ever
  painted). `canvas()` is GPUI's "low-level paint access without a whole
  custom `Element`" escape hatch — the right tool here since the simple
  `.on_mouse_down()` builder API doesn't hand the listener the element's
  own on-screen bounds, which is what's needed to turn an absolute
  window-space click position into a position relative to the line.
- Inside the canvas's `paint` closure (which runs every frame, same as
  internal scroll handling does), `window.on_mouse_event` registers a
  `MouseDownEvent` and a `MouseMoveEvent` listener scoped to that line's
  bounds. On mouse down: shape the line's text with
  `window.text_system().shape_line(...)`, call `.closest_index_for_x()`
  to get the exact byte column, and set the cursor there (collapsing any
  selection — mousedown always starts fresh). On mouse move *while the
  left button is held*: same column lookup, but call `extend_to`
  instead, which only moves the selection's head, keeping whatever
  anchor was set at mouse-down.
- Drag across multiple lines works without any extra bookkeeping: each
  line governs its own pixel range independently, and `extend_to` reads
  the *current* anchor fresh every time it's called, so whichever line's
  overlay the cursor is currently over just keeps moving the head while
  the original anchor (set by mousedown) stays put.

This closes every M1 item from the original plan and the "first tasks"
list. Remaining gaps are now cosmetic/advanced (no double/triple-click
word or line selection, no multi-cursor visual highlight, no line
virtualization) rather than missing core interactions.

## M1 close-out: Quick Open and incremental reparse

Two of the three items left on M1's list after the scroll/visual fixes
(the third, mouse drag-select / column-accurate clicks, is still open —
see the scope-cut list in `crates/editor-view/src/lib.rs`):

- **Quick Open (cmd/ctrl+P)** stands in for M1's "command palette" item.
  `DartIde` lists every file under the project root once at startup
  (`flatten_files`), and cmd+P opens a filter-as-you-type overlay (built
  from `gpui_kit::component::input::Input` + plain `div`s — no custom
  modal/palette widget needed) that opens the selected file in a tab,
  same as clicking it in the tree. Escape closes it. Not a full command
  palette (no arbitrary commands, just file search) — scoped down
  deliberately rather than built half-way.
- **Incremental reparse** is wired up for real now: `EditorView` keeps
  one persistent `core_syntax::SyntaxTree` plus the text it was last
  parsed from, and on each render diffs that against the buffer's
  current text with a common-prefix/common-suffix trim
  (`diff_as_edit`) to get a single `InputEdit`, then calls
  `SyntaxTree::reparse` instead of `SyntaxTree::parse`. This doesn't
  know *which* keystroke happened — it only looks at before/after
  text — so every edit path (typing, undo, redo, paste) gets incremental
  reparsing for free without threading edit details through each one.
  5 unit tests on `diff_as_edit` (insertion, deletion, full replacement,
  no-op).

One build gotcha worth remembering: a bare `#[test]` inside a module that
does `use super::*;` in a crate that also does `use gpui_kit::*;` at the
crate root hit "recursion limit reached while expanding `#[test]`" — some
re-export in that glob fights with the `#[test]` attribute's own
resolution. Fix was importing only the specific function under test
(`use super::diff_as_edit;`) instead of the whole glob.

## M1: fixed real rendering/scrolling bugs

Found by actually using the app, not by inspection: scrolling a long file
did nothing, and small visual misalignment ("glitches") between the
gutter and text. Root causes, found by reading `gpui-pre`'s (the actual
engine behind `gpui-kit`) own source rather than guessing from the docs:

- **`font_family("monospace")` isn't a real font name.** GPUI isn't CSS;
  it needs an actual installed font (`"Menlo"` on macOS). Every line's
  row also got a fixed `h(px(20.))` so rows stop having slightly
  inconsistent heights.
- **Scrolling needs the flex child to be allowed to shrink.** The editor
  pane's wrapper had `flex_1()` but no `min_h(px(0.))`. A flex item's
  default minimum height is "big enough for its content," which silently
  defeats `overflow_y_scroll()` — the box just keeps growing to fit every
  line instead of clipping, so there's nothing to scroll *to*.
- **The growing side of `h_resizable` needs an explicit height.** Per
  `gpui-component`'s own `examples/editor/src/main.rs`, the non-panel
  child of `h_resizable` is sized with `.h(relative(1.))`, not
  `flex_1()`/`h_full()`. Matched that pattern on the column holding the
  tab bar + editor.

Lesson for later milestones: these were silent failures (no panic, no
warning, no error — just "doesn't scroll" / "looks slightly off"), and
docs.rs / partial examples didn't surface them. Reading the actual engine
source (`~/.cargo/registry/src/.../gpui-pre-*/src/elements/div.rs`) when
a layout/interaction bug doesn't make sense is worth doing early, not as
a last resort.

## M1: selection highlighting and keyboard selection

`editor-view` can now actually create a multi-character selection, not
just move a cursor: shift+Left/Right/Up/Down/Home/End extends the
selection (anchor stays put, only the head moves), plain Left/Right with
an existing selection collapses to its start/end first (standard editor
behavior), and cmd/ctrl+A selects the whole buffer. All of this reuses
`core-buffer`'s existing `Selection`/`SelectionSet` — no changes needed
there, since multi-character selections were already modeled, just never
driven by anything other than a bare cursor.

Rendering: `line_runs` now tracks two independent per-byte dimensions
(syntax highlight color, and whether a byte is selected) and run-length
encodes both together, so a selection spanning part of a syntax-colored
token still shows the right text color *and* the selection background on
exactly the right sub-range — not an approximation. The run containing the
cursor is still split further so the caret renders inline with correct
color/background on both sides.

Known gap: selection is keyboard-only. There's no mouse drag-select, and
double/triple-click word/line selection isn't implemented either — both
noted as follow-ups rather than attempted here.

## M1/M2: `core-syntax` (Dart highlighting)

`core-syntax` is a real crate now: a thin wrapper around `tree-sitter` +
`tree-sitter-dart` (0.2.0, published April 2026 — actively maintained,
~1.3M downloads). `SyntaxTree::parse` parses Dart source and
`SyntaxTree::highlights` runs the grammar's own `HIGHLIGHTS_QUERY`,
mapping tree-sitter's fine-grained dotted capture names (`function.method`,
`variable.builtin`, ...) down to a small `HighlightKind` enum so
`editor-view` only needs a handful of colors. `SyntaxTree::reparse` takes
an `InputEdit` and reuses the old tree for incremental re-parsing (API
exists; see the gap below for why `editor-view` doesn't call it yet).

Two things built on top of the same query, rather than needing separate
Dart-specific logic:
- **Bracket matching** (`matching_bracket`): a byte-scan with a depth
  counter, but skipping any byte inside a highlighted `Comment` or
  `String` span, so a `)` in a doc comment doesn't throw off matching in
  the real code around it.
- **Folding** (`fold_ranges`): walks the tree looking for any node whose
  first child is a literal `{` token and last child is a literal `}`
  token and which spans more than one line — a generic heuristic that
  doesn't need to know Dart's specific node names (`class_body`, `block`,
  etc.), so it should keep working if the grammar's node kinds change.

7 unit tests cover parsing, highlighting (keyword + comment), bracket
matching, fold ranges, and one incremental-reparse round trip.

`editor-view` now colors each line using `SyntaxTree::highlights`,
splitting each line into colored runs and further splitting the run the
cursor sits in so the cursor marker renders inline with the right color
on each side.

**Known gap**: `editor-view` calls `SyntaxTree::parse` fresh on every
render instead of keeping one `SyntaxTree` around and calling `reparse`
with a real `InputEdit` per keystroke (which the crate's
`point_at`/`InputEdit` re-export exist for). For M1-sized files this is
fine; wiring incremental reparse into every `Buffer` edit in
`editor-view` is follow-up work, not done here.

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
- Syntax highlighting landed afterwards — see the `core-syntax` section
  below; this entry is kept only as a record of the original gap.
- Selection highlighting landed afterwards too — see the dedicated
  section below; this entry is also kept only as a record.
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
