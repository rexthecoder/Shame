//! Design tokens for the "panes" layout.
//!
//! The rule from `docs/design-spec.md`: no widget outside `theme.rs` or this
//! module may contain a literal colour, radius, spacing step, or font size.
//! `theme.rs` still owns the `gpui-kit` `Theme` mapping; this module owns the
//! values that theme does not model (panel metrics, extra surfaces) plus the
//! spacing/type scales every new widget should use.

use gpui_kit::*;

// ---- spacing scale (the only allowed steps) --------------------------------

pub const SPACE_1: f32 = 2.;
pub const SPACE_2: f32 = 4.;
pub const SPACE_3: f32 = 6.;
pub const SPACE_4: f32 = 8.;
pub const SPACE_5: f32 = 12.;
pub const SPACE_6: f32 = 16.;
pub const SPACE_7: f32 = 24.;

// ---- radii ----------------------------------------------------------------

/// Controls: icon buttons, tree rows, inputs.
pub const RADIUS_SM: f32 = 6.;
/// Panels and inner sections.
pub const RADIUS_PANEL: f32 = 8.;
/// Cards: tool calls, messages, pending-changes, terminal dock.
pub const RADIUS_CARD: f32 = 10.;
/// Chips and avatars.
pub const RADIUS_PILL: f32 = 999.;

// ---- type -----------------------------------------------------------------

pub const UI: f32 = 12.5;
pub const UI_SM: f32 = 11.5;
pub const UI_XS: f32 = 10.5;
pub const MONO: f32 = 12.5;

// ---- layout metrics -------------------------------------------------------

pub const TITLEBAR_H: f32 = 44.;
pub const STATUS_H: f32 = 26.;
pub const TABSTRIP_H: f32 = 38.;
pub const RAIL_LEFT: f32 = 236.;
pub const RAIL_LEFT_MIN: f32 = 200.;
pub const RAIL_LEFT_MAX: f32 = 420.;
pub const RAIL_RIGHT: f32 = 340.;
pub const RAIL_RIGHT_MIN: f32 = 300.;
pub const RAIL_RIGHT_MAX: f32 = 460.;
pub const GUTTER: f32 = 8.;
pub const TERMINAL_H: f32 = 240.;
pub const TERMINAL_MIN: f32 = 120.;
pub const TERMINAL_MAX: f32 = 480.;
pub const TREE_ROW_H: f32 = 28.;
pub const CODE_LINE_H: f32 = 22.;

// ---- surfaces the gpui-kit Theme does not model ---------------------------

/// Code surface, one step above the window background.
pub const BG_EDITOR: u32 = 0x0e0e10;
/// Fill behind the cursor's line in the editor.
pub const LINE_ACTIVE: u32 = 0x15161a;
/// Added-line gutter wash.
pub const LINE_ADD: u32 = 0x3fb95026;
/// Deleted-line gutter wash.
pub const LINE_DEL: u32 = 0xe5484d26;
/// 12% accent: active-row glow, tool-card edge.
pub const ACCENT_DIM: u32 = 0xea7a3c1f;

fn opaque(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn alpha(hex: u32) -> Hsla {
    rgba(hex).into()
}

pub fn bg_editor() -> Hsla {
    opaque(BG_EDITOR)
}

pub fn line_active() -> Hsla {
    opaque(LINE_ACTIVE)
}

pub fn line_add() -> Hsla {
    alpha(LINE_ADD)
}

pub fn line_del() -> Hsla {
    alpha(LINE_DEL)
}

pub fn accent_dim() -> Hsla {
    alpha(ACCENT_DIM)
}
