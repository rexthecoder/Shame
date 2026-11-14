//! Peek: look at where a symbol is defined, or everywhere it is used, without
//! leaving the file. A list of locations on the left, a syntax-colored preview
//! of the selected one on the right; ↑/↓ choose, ↩ opens it, Esc closes.
//!
//! Opened by `EditorEvent::ShowPeek` (⌥F12 definition, ⇧F12 references).

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use core_lsp::lsp_types::Location;
use core_syntax::HighlightKind;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, tokens};

/// Cap on rows; a symbol used thousands of times would otherwise build thousands of elements.
const MAX_ITEMS: usize = 200;
/// Lines shown above and below the target in the preview.
const CONTEXT_LINES: usize = 40;
/// Files bigger than this are previewed without syntax colors.
const MAX_HIGHLIGHT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReferenceItem {
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
    /// The trimmed source line, for the list.
    pub preview: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PreviewLine {
    /// 1-indexed.
    pub number: usize,
    pub text: String,
    pub spans: Vec<(Range<usize>, HighlightKind)>,
    pub is_target: bool,
}

pub(crate) struct PeekState {
    pub title: String,
    pub items: Vec<ReferenceItem>,
    pub selected: usize,
    pub preview: Vec<PreviewLine>,
    /// Scroll position of the code preview.
    pub scroll: ScrollHandle,
    /// The row whose target line the preview has been scrolled to.
    positioned: std::cell::Cell<Option<usize>>,
}

impl PeekState {
    pub(crate) fn new(title: String, items: Vec<ReferenceItem>) -> Self {
        let mut state = Self {
            title,
            items,
            selected: 0,
            preview: Vec::new(),
            scroll: ScrollHandle::new(),
            positioned: std::cell::Cell::new(None),
        };
        state.refresh_preview();
        state
    }

    pub(crate) fn select(&mut self, ix: usize) {
        let ix = ix.min(self.items.len().saturating_sub(1));
        if ix != self.selected {
            self.selected = ix;
            self.positioned.set(None);
            self.refresh_preview();
        }
    }

    fn refresh_preview(&mut self) {
        self.preview = match self.items.get(self.selected) {
            Some(item) => build_preview(&item.path, item.line),
            None => Vec::new(),
        };
    }
}

/// Turn LSP locations into list rows, reading each referenced line from disk.
/// Non-`file://` locations are dropped.
pub(crate) fn reference_items(locations: &[Location]) -> Vec<ReferenceItem> {
    let mut files: HashMap<PathBuf, Vec<String>> = HashMap::new();
    let mut items = Vec::new();
    for location in locations.iter().take(MAX_ITEMS) {
        let Some(path) = location.uri.as_str().strip_prefix("file://").map(percent_decode) else {
            continue;
        };
        let path = PathBuf::from(path);
        let lines = files.entry(path.clone()).or_insert_with(|| {
            std::fs::read_to_string(&path)
                .map(|text| text.lines().map(str::to_owned).collect())
                .unwrap_or_default()
        });
        let line = location.range.start.line;
        let preview = lines.get(line as usize).map(|l| l.trim().to_string()).unwrap_or_default();
        items.push(ReferenceItem { path, line, column: location.range.start.character, preview });
    }
    items.sort_by(|a, b| (&a.path, a.line, a.column).cmp(&(&b.path, b.line, b.column)));
    items
}

/// Decode `%XX` escapes in a `file://` URI path (e.g. `%20` in directory names).
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The lines (0-indexed, end exclusive) to preview around `target`: `context`
/// above and below, slid so the window keeps its size near the file's ends.
pub(crate) fn preview_window(target: usize, total: usize, context: usize) -> Range<usize> {
    let size = 2 * context + 1;
    let start = target.saturating_sub(context).min(total.saturating_sub(size));
    start..(start + size).min(total)
}

/// The preview of `path` around the 0-indexed `target_line`.
pub(crate) fn build_preview(path: &Path, target_line: u32) -> Vec<PreviewLine> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let window = preview_window(target_line as usize, lines.len(), CONTEXT_LINES);
    let colored = path.extension().and_then(|e| e.to_str()) == Some("dart")
        && text.len() <= MAX_HIGHLIGHT_BYTES;
    let spans = if colored {
        crate::diff_view::line_spans_for(&text, core_syntax::SyntaxLanguage::Dart)
    } else {
        Vec::new()
    };
    window
        .map(|ix| PreviewLine {
            number: ix + 1,
            text: lines[ix].to_string(),
            spans: spans.get(ix).cloned().unwrap_or_default(),
            is_target: ix == target_line as usize,
        })
        .collect()
}

impl DartIde {
    pub(crate) fn open_peek(
        &mut self,
        title: String,
        locations: &[Location],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let items = reference_items(locations);
        if items.is_empty() {
            return;
        }
        self.peek = Some(PeekState::new(title, items));
        window.focus(&self.peek_focus, cx);
        cx.notify();
    }

    pub(crate) fn close_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.peek = None;
        if let Some(editor) = self.active_editor().cloned() {
            let focus = editor.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn peek_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(peek) = &mut self.peek {
            let last = peek.items.len().saturating_sub(1) as isize;
            peek.select((peek.selected as isize + delta).clamp(0, last) as usize);
            cx.notify();
        }
    }

    /// Go into the file: open the selected location in the editor, or (`to_side`)
    /// in a split pane next to the file you were editing.
    fn open_peek_item(&mut self, to_side: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.peek.take().and_then(|p| p.items.into_iter().nth(p.selected)) else {
            return;
        };
        let before = self.active_tab;
        let had_tabs = !self.tabs.is_empty();
        self.open_path(item.path, window, cx);
        let opened = self.active_tab;
        if to_side && had_tabs && opened != before {
            // The new file goes to the right pane; the editor you were in stays put.
            self.split_tab = Some(opened);
            self.active_tab = before.min(self.tabs.len().saturating_sub(1));
        }
        if let Some(tab) = self.tabs.get(opened) {
            let editor = tab.editor.clone();
            editor.update(cx, |editor, cx| editor.set_cursor_to_position(item.line, item.column, cx));
            let focus = editor.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    pub(crate) fn render_peek(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let peek = self.peek.as_ref()?;
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let mono = cx.theme().mono_font_family.clone();
        let root = std::env::current_dir().ok();
        let shorten = |path: &Path| -> String {
            root.as_ref()
                .and_then(|r| path.strip_prefix(r).ok())
                .unwrap_or(path)
                .display()
                .to_string()
        };
        let many = peek.items.len() > 1;
        let current = &peek.items[peek.selected];
        let current_path = shorten(&current.path);

        let list = v_flex()
            .id("peek-list")
            .w(px(300.))
            .flex_shrink_0()
            .h_full()
            .overflow_y_scroll()
            .border_r_1()
            .border_color(border)
            .children(peek.items.iter().enumerate().map(|(ix, item)| {
                let selected = ix == peek.selected;
                v_flex()
                    .id(("peek-item", ix))
                    .w_full()
                    .px(px(10.))
                    .py(px(5.))
                    .gap(px(1.))
                    .cursor_pointer()
                    .when(selected, |r| r.bg(tokens::accent_dim()))
                    .when(!selected, |r| r.hover(|h| h.bg(cx.theme().list_hover)))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(tokens::UI_SM))
                            .text_color(fg)
                            .child(format!("{}:{}", shorten(&item.path), item.line + 1)),
                    )
                    .when(!item.preview.is_empty(), |r| {
                        r.child(
                            div()
                                .truncate()
                                .font_family(mono.clone())
                                .text_size(px(tokens::UI_XS))
                                .text_color(muted)
                                .child(item.preview.clone()),
                        )
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if let Some(peek) = &mut this.peek {
                            peek.select(ix);
                        }
                        if event.click_count() >= 2 {
                            this.open_peek_item(false, window, cx);
                        } else {
                            cx.notify();
                        }
                    }))
            }));

        // Start with the target a few lines below the top, so the code before it is a scroll away.
        if peek.positioned.get() != Some(peek.selected) {
            let target = peek.preview.iter().position(|l| l.is_target).unwrap_or(0);
            let top = (target as f32 * 20. - 60.).max(0.);
            peek.scroll.set_offset(point(px(0.), px(-top)));
            peek.positioned.set(Some(peek.selected));
        }

        let number_width = peek.preview.last().map_or(1, |l| l.number.to_string().len());
        let code = v_flex()
            .id("peek-preview")
            .track_scroll(&peek.scroll)
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .overflow_scroll()
            .py(px(6.))
            .font_family(mono.clone())
            .text_size(px(12.5))
            .children(peek.preview.iter().map(|line| {
                let highlights: Vec<(Range<usize>, HighlightStyle)> = line
                    .spans
                    .iter()
                    .filter(|(range, _)| range.end <= line.text.len())
                    .map(|(range, kind)| {
                        let color: Hsla = editor_view::highlight_color(*kind).into();
                        (range.clone(), HighlightStyle { color: Some(color), ..Default::default() })
                    })
                    .collect();
                h_flex()
                    .w_full()
                    .h(px(20.))
                    .flex_shrink_0()
                    .items_center()
                    .when(line.is_target, |row| row.bg(tokens::accent_dim()))
                    .child(
                        div()
                            .w(px(12. + 8. * number_width as f32))
                            .flex_shrink_0()
                            .pr(px(10.))
                            .text_color(muted.opacity(0.7))
                            .text_align(TextAlign::Right)
                            .child(line.number.to_string()),
                    )
                    .child(
                        div()
                            .whitespace_nowrap()
                            .text_color(fg)
                            .child(StyledText::new(line.text.clone()).with_highlights(highlights)),
                    )
            }));

        Some(
            div()
                .id("peek-overlay")
                .absolute()
                .inset_0()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(70.))
                .bg(rgba(0x00000066))
                // The editor underneath must not see clicks, scrolls or keys.
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, window, cx| {
                        this.close_peek(window, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                .track_focus(&self.peek_focus)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "escape" => this.close_peek(window, cx),
                        "down" => this.peek_move(1, cx),
                        "up" => this.peek_move(-1, cx),
                        "pagedown" => this.peek_move(8, cx),
                        "pageup" => this.peek_move(-8, cx),
                        "enter" => {
                            let to_side = event.keystroke.modifiers.platform || event.keystroke.modifiers.control;
                            this.open_peek_item(to_side, window, cx)
                        }
                        // Anything else is swallowed: typing must not reach the editor.
                        _ => {}
                    }
                    cx.stop_propagation();
                }))
                .child(
                    v_flex()
                        .id("peek-panel")
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w(px(if many { 940. } else { 700. }))
                        .h(px(420.))
                        .overflow_hidden()
                        .rounded(px(tokens::RADIUS_CARD))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(border)
                        .shadow_xl()
                        .child(
                            h_flex()
                                .w_full()
                                .h(px(34.))
                                .flex_shrink_0()
                                .px(px(12.))
                                .gap(px(10.))
                                .items_center()
                                .justify_between()
                                .border_b_1()
                                .border_color(border)
                                .child(
                                    h_flex()
                                        .min_w(px(0.))
                                        .gap(px(8.))
                                        .items_center()
                                        .child(
                                            div()
                                                .text_size(px(tokens::UI_SM))
                                                .font_medium()
                                                .text_color(fg)
                                                .child(if many {
                                                    format!("{} \u{b7} {}", peek.title, peek.items.len())
                                                } else {
                                                    peek.title.clone()
                                                }),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(px(tokens::UI_XS))
                                                .text_color(muted)
                                                .child(format!("{current_path}:{}", current.line + 1)),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .flex_shrink_0()
                                        .gap(px(10.))
                                        .items_center()
                                        .text_size(px(tokens::UI_XS))
                                        .text_color(muted)
                                        .child(if many {
                                            "\u{2191}\u{2193} choose \u{b7} \u{21a9} open \u{b7} \u{2318}\u{21a9} open to the side \u{b7} esc close"
                                        } else {
                                            "\u{21a9} open \u{b7} \u{2318}\u{21a9} open to the side \u{b7} esc close"
                                        })
                                        .child(
                                            div()
                                                .id("peek-open")
                                                .px(px(8.))
                                                .py(px(2.))
                                                .rounded(px(tokens::RADIUS_SM))
                                                .cursor_pointer()
                                                .bg(tokens::accent_dim())
                                                .text_color(fg)
                                                .hover(|s| s.opacity(0.85))
                                                .child("Open")
                                                .on_click(cx.listener(|this, _e, window, cx| {
                                                    this.open_peek_item(false, window, cx);
                                                })),
                                        )
                                        .child(
                                            div()
                                                .id("peek-close")
                                                .cursor_pointer()
                                                .hover(move |s| s.text_color(fg))
                                                .child("Close")
                                                .on_click(cx.listener(|this, _e, window, cx| {
                                                    this.close_peek(window, cx);
                                                })),
                                        ),
                                ),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .flex_1()
                                .min_h(px(0.))
                                .when(many, |body| body.child(list))
                                .child(code),
                        ),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Location, build_preview, percent_decode, preview_window, reference_items};

    fn temp_file(name: &str, text: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("peek-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.dart");
        std::fs::write(&file, text).unwrap();
        file
    }

    #[core::prelude::v1::test]
    fn percent_decode_handles_spaces_and_plain_paths() {
        assert_eq!(percent_decode("/a/my%20dir/x.dart"), "/a/my dir/x.dart");
        assert_eq!(percent_decode("/a/b.dart"), "/a/b.dart");
        assert_eq!(percent_decode("/a/100%"), "/a/100%");
    }

    #[core::prelude::v1::test]
    fn the_preview_window_keeps_its_size_near_the_ends_of_a_file() {
        assert_eq!(preview_window(50, 100, 7), 43..58);
        assert_eq!(preview_window(2, 100, 7), 0..15, "near the top it slides down");
        assert_eq!(preview_window(98, 100, 7), 85..100, "near the bottom it slides up");
        assert_eq!(preview_window(1, 4, 7), 0..4, "a short file is shown whole");
        assert_eq!(preview_window(0, 0, 7), 0..0);
    }

    #[core::prelude::v1::test]
    fn the_preview_marks_the_target_line_and_colors_dart() {
        let source: String = (1..=200).map(|n| format!("int v{n} = {n};\n")).collect();
        let file = temp_file("target", &source);
        let preview = build_preview(&file, 10);
        assert_eq!(preview.len(), 2 * super::CONTEXT_LINES + 1);
        let target: Vec<_> = preview.iter().filter(|l| l.is_target).collect();
        assert_eq!(target.len(), 1);
        assert_eq!(target[0].number, 11);
        assert_eq!(target[0].text, "int v11 = 11;");
        assert!(!target[0].spans.is_empty(), "Dart source gets syntax spans");
        assert!(build_preview(&file.with_file_name("missing.dart"), 0).is_empty());
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[core::prelude::v1::test]
    fn reference_items_read_previews_and_sort() {
        let file = temp_file("refs", "void main() {\n  greet();\n}\n");
        let uri = format!("file://{}", file.display());
        let loc = |line: u32| -> Location {
            serde_json::from_value(serde_json::json!({
                "uri": uri,
                "range": {"start": {"line": line, "character": 2}, "end": {"line": line, "character": 7}}
            }))
            .unwrap()
        };
        let items = reference_items(&[loc(1), loc(0)]);
        assert_eq!(items[0].line, 0);
        assert_eq!(items[1].preview, "greet();");
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }
}
