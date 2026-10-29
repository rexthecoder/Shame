//! A quiet, warm dark theme in the spirit of the reference editor: near-black
//! blue-grey surfaces, hairline borders, and a single warm accent instead of a
//! bright primary hue.

use std::borrow::Cow;

use gpui_kit::component::Theme;
use gpui_kit::*;

/// Load macOS's genuine `SF Mono` font faces from `Terminal.app`'s font bundle
/// into GPUI's in-memory font source (`cx.text_system().add_fonts(...)`).
///
/// Why this is needed: Apple keeps `SF-Mono-*.otf` inside
/// `/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts/`
/// rather than `/System/Library/Fonts/`. Without registering them in
/// `text_system()`, CoreText's `select_family_by_name("SF Mono")` fails and
/// GPUI silently falls back to `.AppleSystemUIFont` (a proportional UI font!).
fn load_sf_mono_fonts(cx: &App) -> bool {
    const SF_MONO_DIR: &str =
        "/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts";
    const FACES: &[&str] = &[
        "SF-Mono-Regular.otf",
        "SF-Mono-RegularItalic.otf",
        "SF-Mono-Medium.otf",
        "SF-Mono-MediumItalic.otf",
        "SF-Mono-Semibold.otf",
        "SF-Mono-SemiboldItalic.otf",
        "SF-Mono-Bold.otf",
        "SF-Mono-BoldItalic.otf",
    ];

    let mut font_bytes: Vec<Cow<'static, [u8]>> = Vec::new();
    for face in FACES {
        let path = std::path::Path::new(SF_MONO_DIR).join(face);
        if let Ok(bytes) = std::fs::read(path) {
            font_bytes.push(Cow::Owned(bytes));
        }
    }
    if font_bytes.is_empty() {
        return false;
    }
    cx.text_system().add_fonts(font_bytes).is_ok()
}

// Values match `docs/design-spec.md` §2. Only these constants may change to
// retheme; widgets never carry their own hex.
const BG: u32 = 0x0e0e10;
const BG_DEEP: u32 = 0x08080a;
const BG_PANEL: u32 = 0x121214;
const BG_RAISED: u32 = 0x17171a;
const BG_RAISED_2: u32 = 0x1c1c20;
const BORDER: u32 = 0x242428;
const BORDER_SOFT: u32 = 0x1e1e22;
const FG: u32 = 0xc9c9ce;
const FG_STRONG: u32 = 0xe8e8ec;
const FG_MUTED: u32 = 0x86868f;
const ACCENT: u32 = 0xea7a3c;
const ACCENT_HOVER: u32 = 0xf08a50;
const ACCENT_ACTIVE: u32 = 0xd46a31;
const GREEN: u32 = 0x3fb950;
const RED: u32 = 0xe5484d;
const YELLOW: u32 = 0xd5a54a;
const SEL_BG: u32 = 0x24303c;
const BLUE: u32 = 0x61afef;
const CYAN: u32 = 0x56b6c2;
const MAGENTA: u32 = 0xc678dd;

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn a(hex: u32) -> Hsla {
    rgba(hex).into()
}

pub fn apply(cx: &mut App) {
    let mono_family = if load_sf_mono_fonts(cx) {
        "SF Mono"
    } else {
        "Menlo"
    };
    Theme::update(cx, |theme| {
        let col = &mut theme.colors;

        col.background = c(BG);
        col.foreground = c(FG);
        col.border = c(BORDER);
        col.muted = c(BG_RAISED);
        col.muted_foreground = c(FG_MUTED);
        col.selection = c(SEL_BG);
        col.caret = c(0xd9dee6);
        col.overlay = a(0x000000b3);
        col.ring = c(ACCENT);

        col.accent = a(0xffffff0d);
        col.accent_foreground = c(FG_STRONG);

        col.sidebar = c(BG_PANEL);
        col.sidebar_foreground = c(0xb9bec7);
        col.sidebar_border = c(BORDER_SOFT);
        col.sidebar_accent = a(0xffffff0f);
        col.sidebar_accent_foreground = c(FG_STRONG);
        col.sidebar_primary = c(ACCENT);
        col.sidebar_primary_foreground = c(0x1a120e);

        col.title_bar = c(BG_DEEP);
        col.title_bar_border = c(BORDER_SOFT);
        col.status_bar = c(BG_DEEP);
        col.status_bar_border = c(BORDER_SOFT);

        col.tab_bar = c(BG_PANEL);
        col.tab = a(0x00000000);
        col.tab_active = c(BG_RAISED);
        col.tab_foreground = c(0x8a929d);
        col.tab_active_foreground = c(FG_STRONG);
        col.tab_bar_segmented = c(BG_PANEL);

        col.list = c(BG);
        col.list_hover = a(0xffffff0a);
        col.list_active = a(0xffffff12);
        col.list_active_border = a(0xea7a3c66);
        col.list_even = c(BG);
        col.list_head = c(BG_PANEL);

        col.popover = c(BG_RAISED);
        col.popover_foreground = c(FG_STRONG);
        col.input = c(BG_RAISED_2);
        col.secondary = c(BG_RAISED);
        col.secondary_hover = c(BG_RAISED_2);
        col.secondary_active = c(0x262c35);
        col.secondary_foreground = c(0xd5dae1);
        col.button = c(BG_RAISED);
        col.button_hover = c(BG_RAISED_2);
        col.button_active = c(0x262c35);
        col.button_foreground = c(FG);

        col.primary = c(ACCENT);
        col.primary_hover = c(ACCENT_HOVER);
        col.primary_active = c(ACCENT_ACTIVE);
        col.primary_foreground = c(0x1a120e);

        col.danger = c(RED);
        col.danger_hover = c(0xe8828a);
        col.danger_active = c(0xcf5b64);
        col.danger_foreground = c(0x1a0f10);
        col.success = c(GREEN);
        col.success_hover = c(0xa1cc93);
        col.success_active = c(0x7dab6e);
        col.success_foreground = c(0x0f160c);
        col.warning = c(YELLOW);
        col.warning_hover = c(0xefcf95);
        col.warning_active = c(0xd2ac63);
        col.warning_foreground = c(0x1a1508);
        col.info = c(BLUE);
        col.info_hover = c(0x83c0f4);
        col.info_active = c(0x4f97d2);
        col.info_foreground = c(0x0a141f);

        col.progress_bar = c(ACCENT);
        col.scrollbar = a(0x00000000);
        col.scrollbar_thumb = c(0x3a424e);
        col.scrollbar_thumb_hover = c(0x4a5462);
        col.skeleton = c(BG_RAISED);
        col.slider_bar = c(0x2f3742);
        col.slider_thumb = c(FG_STRONG);
        col.switch = c(0x2f3742);
        col.switch_thumb = c(FG_STRONG);
        col.drag_border = c(ACCENT);
        col.drop_target = a(0xea7a3c40);
        col.window_border = c(0x1a1e24);

        col.red = c(RED);
        col.red_light = c(0xe8828a);
        col.green = c(GREEN);
        col.green_light = c(0xa1cc93);
        col.blue = c(BLUE);
        col.blue_light = c(0x83c0f4);
        col.yellow = c(YELLOW);
        col.yellow_light = c(0xefcf95);
        col.magenta = c(MAGENTA);
        col.magenta_light = c(0xd89be6);
        col.cyan = c(CYAN);
        col.cyan_light = c(0x7fcbd4);

        theme.radius = px(6.);
        theme.radius_lg = px(10.);
        theme.mono_font_family = mono_family.into();
        theme.mono_font_size = px(12.5);
    });
}
