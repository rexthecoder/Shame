//! The "Find References" result picker: a modal list of `file:line` rows
//! with a one-line source preview, opened by `EditorEvent::ShowReferences`
//! (⇧F12 in the editor).

use std::collections::HashMap;
use std::path::PathBuf;

use core_lsp::lsp_types::Location;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, tokens};

/// Cap on rows shown; a symbol used thousands of times would otherwise
/// build thousands of elements.
const MAX_REFERENCES: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReferenceItem {
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
    pub preview: String,
}

/// Turn LSP locations into picker rows, reading each referenced line from
/// disk for the preview. Non-`file://` locations are dropped.
pub(crate) fn reference_items(locations: &[Location]) -> Vec<ReferenceItem> {
    let mut files: HashMap<PathBuf, Vec<String>> = HashMap::new();
    let mut items = Vec::new();
    for location in locations.iter().take(MAX_REFERENCES) {
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
        let preview = lines
            .get(line as usize)
            .map(|l| l.trim().to_string())
            .unwrap_or_default();
        items.push(ReferenceItem {
            path,
            line,
            column: location.range.start.character,
            preview,
        });
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

impl DartIde {
    pub(crate) fn render_references(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> Option<Div> {
        let items = self.references.as_ref()?;
        let root = std::env::current_dir().ok();

        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(84.))
                .bg(rgba(0x00000066))
                .on_key_down({
                    let view = view.clone();
                    move |event, _window, cx| {
                        if event.keystroke.key.as_str() == "escape" {
                            view.update(cx, |this, cx| {
                                this.references = None;
                                cx.notify();
                            });
                        }
                    }
                })
                .child(
                    v_flex()
                        .w(px(560.))
                        .max_h(px(420.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .child(
                            h_flex()
                                .px_3()
                                .py_2()
                                .justify_between()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("{} references", items.len()))
                                .child(
                                    div()
                                        .id("references-close")
                                        .cursor_pointer()
                                        .child("Close")
                                        .on_click({
                                            let view = view.clone();
                                            move |_event, _window, cx| {
                                                view.update(cx, |this, cx| {
                                                    this.references = None;
                                                    cx.notify();
                                                });
                                            }
                                        }),
                                ),
                        )
                        .child(
                            v_flex()
                                .id("references-list")
                                .overflow_y_scroll()
                                .children(items.iter().enumerate().map(|(ix, item)| {
                                    let shown = root
                                        .as_ref()
                                        .and_then(|r| item.path.strip_prefix(r).ok())
                                        .unwrap_or(&item.path)
                                        .display()
                                        .to_string();
                                    div()
                                        .id(("reference", ix))
                                        .px_3()
                                        .py_1p5()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(cx.theme().list_hover))
                                        .child(
                                            div()
                                                .text_sm()
                                                .child(format!("{shown}:{}", item.line + 1)),
                                        )
                                        .when(!item.preview.is_empty(), |el| {
                                            el.child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(item.preview.clone()),
                                            )
                                        })
                                        .on_click({
                                            let view = view.clone();
                                            move |_event, window, cx| {
                                                view.update(cx, |this, cx| {
                                                    this.open_reference(ix, window, cx)
                                                });
                                            }
                                        })
                                })),
                        ),
                ),
        )
    }

    fn open_reference(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.references.take().and_then(|mut items| {
            (index < items.len()).then(|| items.swap_remove(index))
        }) else {
            return;
        };
        self.open_path(item.path, window, cx);
        if let Some(editor) = self.active_editor().cloned() {
            editor.update(cx, |editor, cx| {
                editor.set_cursor_to_position(item.line, item.column, cx)
            });
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{Location, percent_decode, reference_items};

    #[test]
    fn percent_decode_handles_spaces_and_plain_paths() {
        assert_eq!(percent_decode("/a/my%20dir/x.dart"), "/a/my dir/x.dart");
        assert_eq!(percent_decode("/a/b.dart"), "/a/b.dart");
        assert_eq!(percent_decode("/a/100%"), "/a/100%");
    }

    #[test]
    fn reference_items_reads_previews_and_sorts() {
        let dir = std::env::temp_dir().join(format!("refs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.dart");
        std::fs::write(&file, "void main() {\n  greet();\n}\n").unwrap();
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
        std::fs::remove_dir_all(&dir).ok();
    }
}
