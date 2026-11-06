# UI Replica Plan — "panes" target design

Read `docs/design-spec.md` first. This is the execution order.

Current state, for grounding:

- `crates/app/src/theme.rs` — tokens exist and are ~90% correct.
- `crates/app/src/main.rs` (1254 lines) — titlebar, 2-way split
  (`h_resizable`: sidebar 260 | editor column), tabs, status bar, three
  overlays. **No agent rail, no terminal dock.**
- `crates/editor-view/src/lib.rs` — custom GPUI element, own render cycle.

The gap is not theme. It is **two new regions + ~18 components + a module
split**, and the reason previous attempts drifted is that they were done
as one giant render edit with no spec.

---

## Phase 0 — Spec + tokens (no visual change)

**Do**
- Apply only the §2 deltas to `theme.rs`: colour values, add the 7 new
  tokens, set `radius_sm/radius/radius_lg/radius_pill`, spacing consts.
- Add a `mod tokens;` with `space`, `radii`, `type_scale` consts so widgets
  never pass bare `px(8.)` — they pass `space::SM`.

**Acceptance**
- `cargo build -p app` clean.
- App looks identical except colours are marginally more neutral.
- No widget file changed.

**Why first:** every later phase cites these tokens. If Phase 0 is skipped,
Phase 5 invents `Radius::circular(11.)` and the drift starts.

---

## Phase 1 — Module split + shell geometry

**Do**
- Split `main.rs` into:
  ```
  crates/app/src/
    main.rs          // window setup only
    theme.rs
    tokens.rs
    shell.rs         // the 4-region layout
    chrome/titlebar.rs
    chrome/status_bar.rs
    rails/files.rs
    rails/agent.rs
    editor/tabs.rs
    editor/dock.rs
  ```
  Move code, do not rewrite. `main.rs` drops under ~200 lines.
- Replace the `h_resizable` 2-way split with the 3-column shell:
  `left rail 236 | center flex | agent rail 340`, gutter 8, outer pad 8.
- Center column becomes `v_flex`: tab strip 38 → editor flex → terminal
  dock 240.
- All new regions render **empty placeholders** with the correct
  width/height, bg, radius, and border from the spec.

**Acceptance**
- At 1600×1060 the placeholder boxes sit exactly where the reference's
  regions sit (measure with a screenshot overlay).
- All three splits resize; min widths enforced; nothing collapses.
- No content yet. That is correct.

**Why before components:** wrong box geometry cannot be fixed by styling
components. This is where "almost right but off" usually originates.

---

## Phase 2 — Chrome (titlebar + status bar)

**Do**
- Titlebar: menu, 3 layout toggles, centered title, branch chip, env chip,
  primary-file chip, run/debug, search/extensions/settings, window
  controls. Reuse `Button`, `Icon`, `tooltip`; do not build new primitives
  except `Chip`.
- Status bar: breadcrumb, git item, LSP item, cursor, `LF`, `UTF-8`.
  Right-align the last four.
- Wire what exists (sidebar toggle, search → quick open, branch, cursor).
  Everything else renders **disabled + tooltip**.

**Gotcha (hit while doing this):** register `gpui_kit::assets::AllAssets`,
not `Assets`. `Assets` embeds only the component's default icon subset, so
any icon outside it (Columns2, Bug, Wand, Zap, UserPlus, PenTool, …) loads
as a blank path and renders invisibly — no warning, no error.

**Acceptance**
- Every titlebar icon either works or is visibly disabled.
- Title truncates at narrow widths without pushing chips off.
- Status bar never exceeds 26px with long file names.

---

## Phase 3 — Left rail

**Do**
- `RailHeader` Files chip (active outline) + `+`.
- `SearchRow` with filter input and 4 actions (wire collapse/refresh to
  `TreeState`; disable the rest).
- Restyle `tree(...)` rows to spec: 28px, 6 radius, hover, active-file
  accent wash + 2px left bar, `TypeBadge`, `StateDot`.
- Add badges for `.md`/`.yaml`/`.dart` per `file_icon_color`.

**Acceptance**
- Deep nesting (5+ levels) still reads; labels ellipsize.
- Active file is identifiable without hovering.
- Keyboard nav in the tree still works.

---

## Phase 4 — Center column

**Do**
- `Tab` restyle: 30px chip, full 1px outline when active, modified dot ⇄
  close on hover, ellipsis.
- `TabActions`: split-right, `+`.
- `CodeGutter`: 48px, active line number bright, add/del marks.
- `CodeLine`: active-line fill + 2px left accent bar, `SEL_BG` selection.
- Breadcrumb stays in the status bar (already built), not a new row.

**Acceptance**
- 8 open tabs degrade gracefully (scroll or overflow menu; pick one).
- Active line is obvious at a glance in a dim room.
- Selection and active-line fill do not fight visually.

---

## Phase 5 — Terminal dock

**Do**
- `TerminalDock` card + `DockTabBar` + `TerminalBody`.
- Render **static scrollback** first (the reference's git log), styled
  exactly. No process.
- Then wire a real shell via the existing process pattern used in
  `core-flutter`/`core-lsp` (spawn, line reader, channel → entity).
- Collapse/expand toggle persisted per session.

**Acceptance**
- Static pass matches the reference exactly.
- Real pass: type `cargo --version`, see output, `clear` works.
- Dock never steals editor keyboard focus unless clicked.

**Do not** put the agent's tool output here — the reference keeps them
separate on purpose.

---

## Phase 6 — Agent rail (hardest; do last)

**Do**, in this order, screenshotting after each:
1. `AgentTabBar` + `AgentHeader`.
2. `TaskChecklist` (3 states).
3. `ToolCallCard` + `CollapsedGroup`.
4. `MessageBubble`.
5. `PendingChanges` + per-file rows + accept/reject.
6. `Composer` + mode/model selectors + send.

Drive it from a **static `AgentState` struct** (tasks, tool calls,
messages, pending changes) in the entity. No backend first. When the
visual is locked, swap the struct's producer for real data.

**Acceptance**
- A 9-line message, 6 tasks, and 12 pending changes all scroll without
  breaking the composer pin.
- Accept/reject updates the list and the count.
- Composer never jumps when messages arrive.

**Why last:** it is the only region with genuinely unsolved UX. Locking
the other three first means every agent pass after this has a stable
surround.

---

## Phase 7 — Visual QA loop

Repeat until a region diff is empty:

1. Run `cargo run -p app`.
2. Screenshot at 1600×1060.
3. Crop the region being checked; place beside the reference crop.
4. List differences as *position / size / radius / border / type*.
5. Fix only those; re-shoot.

Stop declaring "done" from code review. A phase is done at pixel
agreement, not at compile.

---

## How to feed this to an agent

One phase per session. Prompt template:

```
Read docs/design-spec.md and docs/design-plan.md.
Constraint: no colour/radius/spacing literals outside theme.rs/tokens.rs.
Reuse existing primitives; list any new primitive you must add.
Do not touch files outside the phase's listed targets.
When finished, state what you could not verify.
```

Create the crops once in Phase 0 (5 crops: chrome, left rail, center,
terminal, agent). Crops are what make the difference — one agent pass
against the full screenshot always regresses to "plausible IDE."

---

## Ordering rule

If a phase slips, ship the earlier one anyway. A shell with correct
geometry and empty rails is a usable editor. Three pretty panels inside
wrong geometry is never fixable by more prompting.
