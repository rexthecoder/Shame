# UI Replica Spec — "panes" target design

Source of truth: `docs/reference/panes.webp` (the 1600×1060 screenshot).
This document is the *only* input an implementing agent should need besides
that image. If something is not in here, it does not get invented — ask.

Colour values are eyeballed from the screenshot. Before Phase 1, sample
them with Digital Color Meter and correct the table; do not "improve" them.

---

## 0. Non-negotiable rules

1. **No literal colour, radius, spacing, or font size outside
   `crates/app/src/theme.rs`.** Every widget reads a token.
2. **One accent.** `accent` is used only for: live agent activity, primary
   action, and the active-file marker. Never for decoration.
3. **Three radii only:** `radius_sm` (controls), `radius` (panels/cards),
   `radius_pill` (chips/avatars).
4. **One border colour** (`border`) at 1px; no per-surface border shades.
5. **No dead controls.** Every icon either works or is rendered disabled
   with a tooltip explaining why.
6. **Truncate, never wrap,** in chrome (tabs, tree rows, branch chip,
   status items). Wrap is allowed only inside agent message bubbles.

---

## 1. Layout metrics

Window coordinate system, 1600×1060 reference.

| Region              | Size / rule                                  |
|---------------------|----------------------------------------------|
| Outer window        | full, `radius 12`, 1px `border`              |
| Titlebar            | h **44**, full width                         |
| Left rail           | w **236** fixed (range 200–420)              |
| Center column       | flex, min 480                                |
| Agent rail          | w **340** fixed (range 300–460)              |
| Rail gutter         | **8** between regions                        |
| Outer padding       | **8** around the content area                |
| Tab strip           | h **38**, center column only                 |
| Terminal dock       | h **240** (range 120–480), center column only|
| Status bar          | h **26**, full width, below all columns      |
| Tree row            | h **28**                                     |
| Code line           | h **22**, mono 12.5                          |
| UI text             | 12.5 default, 11.5 small, 10.5 micro         |

Vertical order: titlebar → [left rail | (tabs → editor → terminal dock) | agent rail] → status bar.

The left rail and agent rail run full height between titlebar and status bar.
The terminal dock is *inside* the center column; it never spans the rails.

---

## 2. Target tokens (delta vs `theme.rs`)

Current theme is ~90% there. Only these move.

| Token        | Current    | Target     | Why |
|--------------|-----------:|-----------:|-----|
| `BG`         | `0x101216` | `0x0B0B0D` | current is blue-grey, reference is neutral black |
| `BG_DEEP`    | `0x0c0e12` | `0x08080A` | titlebar/status bar sit *below* window bg |
| `BG_PANEL`   | `0x13161b` | `0x121214` | left rail |
| `BG_RAISED`  | `0x181c22` | `0x17171A` | cards, tool calls, terminal |
| `BG_RAISED_2`| `0x1e232b` | `0x1C1C20` | inputs, hover fill |
| `BORDER`     | `0x232830` | `0x242428` | single hairline, neutral |
| `BORDER_SOFT`| `0x1b1f26` | `0x1E1E22` | dividers only |
| `FG`         | `0xc7ccd4` | `0xC9C9CE` | neutral |
| `FG_STRONG`  | `0xe3e7ed` | `0xE8E8EC` | neutral |
| `FG_MUTED`   | `0x7c8590` | `0x86868F` | neutral |
| `ACCENT`     | `0xd97757` | `0xEA7A3C` | reference is brighter/more orange |
| `ACCENT_HOVER`| `0xe2896b`| `0xF08A50` | |
| `GREEN`      | `0x8fbf7f` | `0x3FB950` | additions; reference green is colder |
| `RED`        | `0xe06c75` | `0xE5484D` | |
| `YELLOW`     | `0xe5c07b` | `0xD5A54A` | |

New tokens to add:

```
BG_EDITOR     0x0E0E10   // code surface, one step above window
ACCENT_DIM    0xEA7A3C1F // 12% accent: active-row glow, tool-card edge
SEL_BG        0x24303C   // selection inside code
LINE_ACTIVE   0x15161A   // active code line fill
LINE_ADD      0x3FB95026 // added-line gutter wash
LINE_DEL      0xE5484D26 // deleted-line gutter wash
```

Radii: `radius_sm = 6`, `radius = 10`, `radius_lg = 12`, `radius_pill = 999`.
Spacing scale: `2, 4, 6, 8, 12, 16, 24` — no other values.
Fonts: `mono = "SF Mono" 12.5`, `ui = system 12.5`.

---

## 3. Component inventory

Each entry: name → anatomy → states → notes.

### Chrome

1. **WindowControls** — traffic lights left, min/max/close right.
   States: normal, hover. Left inset 78 (12 in native fullscreen).
2. **AppMenuButton** — hamburger, 28×28, `radius_sm`.
3. **LayoutToggles** — 3 icons (sidebar, split, panels). Icon-only, 24px,
   active one tinted `FG`, inactive `FG_MUTED`.
4. **WindowTitle** — centered, 12.5 medium, `FG`, max 1 line, ellipsis.
5. **BranchChip** — icon + branch + `↑2`, 11.5, `GREEN`; pill, no border.
6. **EnvChip** — `Linux` / OS name, 11, `FG_MUTED`, with leading glyph.
7. **PrimaryFileChip** — `main.dart`, 11.5, mono, `BG_RAISED_2`, radius_pill.
8. **RunControls** — Play + Debug, 24px. Play = `ACCENT`, debug = `FG_MUTED`.
9. **ActionIcons** — search, extensions, settings, 24px, `FG_MUTED`.

### Left rail

10. **RailHeader** — `Files ×` chip + `+`. Chip has active border
    (`BORDER` + `ACCENT_DIM` outline), h 26, radius 8. `×` on hover only.
11. **SearchRow** — filter icon, input, then 4 icon actions (new file,
    new folder, collapse, refresh). h 28.
12. **TreeRow** — indent 16/depth + 12, icon 16, label 12.5, badges right.
    h 28, radius 6, hover `BG_RAISED`, active `ACCENT_DIM` + 2px left accent.
13. **TypeBadge** — 16×16 rounded 4, letter or glyph, per language colour.
    Reference uses letters: `M` orange, `U` blue.
14. **StateDot** — unsaved `ACCENT`, conflict `RED`. 6px.

### Center / editor

15. **TabStrip** — h 38, bg `BG_PANEL`, bottom border.
16. **Tab** — h 30, px 10, radius 8; inactive transparent, active
    `BG_RAISED` + full 1px `BORDER` outline (not a top accent). Title 12.5.
    Modified dot replaces `×` until hover.
17. **TabActions** — split-right, `+`, right-aligned.
18. **CodeGutter** — w 48, numbers 11.5 `FG_MUTED`, active line number
    `FG_STRONG`; add/del marks 2px at right edge of gutter.
19. **CodeLine** — h 22, mono 12.5, active line fill `LINE_ACTIVE` + 2px
    left accent bar. Selection `SEL_BG`.
20. **TerminalDock** — card, radius 10, 1px border, bg `BG_RAISED`.
21. **DockTabBar** — h 32: `>_ panes — bash ×` chip + `+`. Chip mono 11.5.
22. **TerminalBody** — mono 12, `GREEN` prompt, `FG` output, `FG_MUTED`
    timestamps. Padding 12. Scrollback, no wrap.

### Agent rail

23. **AgentTabBar** — `Demo Agent ×` + `+`, same treatment as editor tabs.
24. **AgentHeader** — title 12.5 medium, pencil icon, `%` and `…` actions.
25. **TaskChecklist** — rows h 26: status glyph (spinner/check/circle) +
    label 12. Done rows `FG_MUTED` + strikethrough.
26. **ToolCallCard** — bg `BG_RAISED`, radius 10, 1px border; header row
    with tool icon + filename + optional chevron; body optional. h 32–auto.
27. **MessageBubble** — bg `BG_RAISED`, radius 10, padding 10/12, body
    12.5, line-height 19, wraps, max width 100%.
28. **CollapsedGroup** — one row: `⚙ Agent used 3 tools ⌄`, 11.5 `FG_MUTED`.
29. **PendingChanges** — header row (`2 Pending Changes` + reject `×` +
    accept `✓` circles 22px) + file rows. Row: file icon, name 12,
    `+18` green, `+34` green, per-row `×`/`✓`. h 34, radius 8.
30. **Composer** — input bg `BG_RAISED_2`, radius 10, placeholder
    `Message agent…`, min h 44. Footer row: `Mode: Default ⌄`,
    `Model: demo-large ⌄`, send button 28 circle `ACCENT`.
31. **TokenFooter** — 10.5 `FG_MUTED`, left-aligned micro text.

### Status bar

32. **StatusBreadcrumb** — `lib › src › pane_controller.dart`, 11.5,
    separators `FG_MUTED`.
33. **StatusItem** — icon + text, 11.5, h 26, px 8. Kinds: git changes,
    LSP, timer, sync, cursor `83:1`, `LF`, `UTF-8`.

---

## 4. States matrix (must exist before a component is "done")

- hover / pressed / focus-visible for every interactive element
- tab: inactive, hover, active, active+modified, active+long-title
- tree row: default, hover, active-file, folder-open, folder-closed
- agent: idle, thinking, tool-running, waiting-for-approval, error
- pending change: pending, accepted, rejected
- empty states: no folder, no tabs, empty agent conversation, no changes
- long content: 40-char branch, 60-char filename, 9-line agent reply
- window: 1280×800 (min), 1600×1060 (reference), native fullscreen

**Empty and long states are part of the design, not an afterthought.** Most
"it looks off" feedback is really an unchecked state.

---

## 5. Deliberate deviations from the reference

- Agent prose: the reference shows two verbose paragraphs. Ship the
  `CollapsedGroup` summary by default; prose only on expand.
- Do not reproduce the strikethrough on code line 82 — it reads as a
  delete-decoration artifact, not intent.
- `WakaTime` in the status bar is third-party; render it as a plain
  `StatusItem` and leave it out until a timer exists.

---

## 6. Verification

A component is visually correct only when a screenshot at 1600×1060 diffed
against `docs/reference/panes.webp` shows no drift in **position, size,
radius, border, or type size** for its region. Colour drift is allowed
only where §2 says so.

Capture: run the app, screenshot, crop the region, compare side by side.
Do not declare done from code review.
