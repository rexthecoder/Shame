//! A GPUI window with a dock-style split layout: a file tree panel reading
//! the real project directory, and our own `editor-view` (M1) in place of
//! the M0 `Textarea` stand-in.
//!
//! See `NOTES.md` at the repo root for the measured startup time, idle RAM,
//! and binary size from the M0 spike.

use std::path::{Path, PathBuf};

use core_buffer::Buffer;
use editor_view::EditorView;
use gpui_kit::assets::Assets;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName,
    h_flex,
    list::ListItem,
    resizable::{h_resizable, resizable_panel},
    tree::{TreeItem, TreeState, tree},
    v_flex,
};
use gpui_kit::base::StyledExt as _;
use gpui_kit::*;

/// Directory/file names we never want to show in the M0 file tree.
const IGNORED_NAMES: &[&str] = &["target", "node_modules"];

fn build_tree_items(dir: &Path) -> Vec<TreeItem> {
    let mut items = Vec::new();

    let Ok(entries) = std::fs::read_dir(dir) else {
        return items;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        if name.starts_with('.') || IGNORED_NAMES.contains(&name) {
            continue;
        }

        let id = path.to_string_lossy().to_string();
        if path.is_dir() {
            let children = build_tree_items(&path);
            items.push(TreeItem::new(id, name.to_string()).children(children));
        } else {
            items.push(TreeItem::new(id, name.to_string()));
        }
    }

    items.sort_by(|a, b| b.is_folder().cmp(&a.is_folder()).then(a.label.cmp(&b.label)));
    items
}

pub struct DartIde {
    tree_state: Entity<TreeState>,
    editor: Entity<EditorView>,
}

impl DartIde {
    fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let items = build_tree_items(&root);

        let tree_state = cx.new(|cx| TreeState::new(cx).items(items));

        let buffer = Buffer::from_str(
            "// Lightweight Dart/Flutter IDE — M1\n\
             //\n\
             // This is our own editor-view (core-buffer + a from-scratch\n\
             // GPUI element), not gpui-component's Textarea. Try typing,\n\
             // arrow keys, backspace/delete, Home/End, and cmd+Z/cmd+Shift+Z.\n",
        );
        let editor = cx.new(|cx| EditorView::from_buffer(buffer, cx));

        Self { tree_state, editor }
    }
}

impl Render for DartIde {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .w_full()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().font_bold().child("Dart/Flutter IDE — M1")),
            )
            .child(
                div().flex_1().w_full().child(
                    h_resizable("main-split")
                        .child(
                            resizable_panel()
                                .size(px(260.))
                                .size_range(px(180.)..px(480.))
                                .child(
                                    div()
                                        .size_full()
                                        .border_r_1()
                                        .border_color(cx.theme().border)
                                        .child(
                                            tree(&self.tree_state, {
                                                let editor = self.editor.clone();
                                                move |ix, entry, _selected, _window, cx| {
                                                    let item = entry.item();
                                                    let icon = if !entry.is_folder() {
                                                        IconName::File
                                                    } else if entry.is_expanded() {
                                                        IconName::FolderOpen
                                                    } else {
                                                        IconName::Folder
                                                    };

                                                    ListItem::new(ix)
                                                        .w_full()
                                                        .rounded(cx.theme().radius)
                                                        .px_3()
                                                        .pl(px(16.) * entry.depth() + px(12.))
                                                        .child(
                                                            h_flex()
                                                                .gap_2()
                                                                .child(Icon::new(icon))
                                                                .child(item.label.clone()),
                                                        )
                                                        .on_click({
                                                            let editor = editor.clone();
                                                            let is_folder = item.is_folder();
                                                            let path = PathBuf::from(item.id.as_str());
                                                            move |_event, window, cx| {
                                                                if is_folder {
                                                                    return;
                                                                }
                                                                let path = path.clone();
                                                                editor.update(cx, |editor, cx| {
                                                                    if let Err(err) =
                                                                        editor.open_file(&path, cx)
                                                                    {
                                                                        eprintln!(
                                                                            "failed to open {}: {err}",
                                                                            path.display()
                                                                        );
                                                                    }
                                                                });
                                                                window.focus(&editor.read(cx).focus_handle(cx), cx);
                                                            }
                                                        })
                                                }
                                            })
                                            .size_full(),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .h_full()
                                .child(self.editor.clone())
                                .into_any_element(),
                        ),
                ),
            )
            .child(self.render_status_bar(cx))
    }
}

impl DartIde {
    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = self.editor.read(cx);
        let buffer = editor.buffer();
        let status = match buffer.path() {
            Some(path) => format!(
                "{}{}",
                path.display(),
                if buffer.is_dirty() { " \u{2022} unsaved" } else { "" }
            ),
            None => "untitled".to_string(),
        };

        h_flex()
            .w_full()
            .px_4()
            .py_1()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(status)
    }
}

fn main() {
    let app = gpui_kit::application().with_assets(Assets);

    app.run(move |cx| {
        // This must be called before using any GPUI Component features.
        gpui_kit::init(cx);

        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1100.), px(720.)), cx)),
            ..Default::default()
        };

        gpui_kit::open_window(window_options, cx, |window, cx| {
            cx.new(|cx| DartIde::new(window, cx))
        })
        .expect("Failed to open window");
    });
}
