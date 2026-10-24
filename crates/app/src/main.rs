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
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    list::ListItem,
    resizable::{h_resizable, resizable_panel},
    tree::{TreeItem, TreeState, tree},
    v_flex,
};
use gpui_kit::base::StyledExt as _;
use gpui_kit::prelude::FluentBuilder as _;
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

/// Every file under `dir`, for the Quick Open palette.
fn flatten_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') || IGNORED_NAMES.contains(&name) {
            continue;
        }
        if path.is_dir() {
            files.extend(flatten_files(&path));
        } else {
            files.push(path);
        }
    }

    files
}

struct Tab {
    /// `None` for the initial scratch buffer; every file opened from the
    /// tree gets a path, which also doubles as the tab's dedup key.
    path: Option<PathBuf>,
    editor: Entity<EditorView>,
}

impl Tab {
    fn title(&self) -> String {
        match &self.path {
            Some(path) => path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("untitled")
                .to_string(),
            None => "untitled".to_string(),
        }
    }
}

pub struct DartIde {
    root: PathBuf,
    tree_state: Entity<TreeState>,
    tabs: Vec<Tab>,
    active_tab: usize,
    all_files: Vec<PathBuf>,
    palette_open: bool,
    palette_input: Entity<InputState>,
}

impl DartIde {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let items = build_tree_items(&root);
        let all_files = flatten_files(&root);

        let tree_state = cx.new(|cx| TreeState::new(cx).items(items));
        let palette_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Go to file..."));

        let buffer = Buffer::from_str(
            "// Lightweight Dart/Flutter IDE — M1\n\
             //\n\
             // This is our own editor-view (core-buffer + a from-scratch\n\
             // GPUI element), not gpui-component's Textarea. Try typing,\n\
             // arrow keys, backspace/delete, Home/End, and cmd+Z/cmd+Shift+Z.\n\
             // Click a file in the tree (or cmd+P to go to one) to open it.\n\
             // cmd+O to open a different folder.\n",
        );
        let editor = cx.new(|cx| EditorView::from_buffer(buffer, cx));

        Self {
            root,
            tree_state,
            tabs: vec![Tab { path: None, editor }],
            active_tab: 0,
            all_files,
            palette_open: false,
            palette_input,
        }
    }

    /// Show a native folder picker and re-root the file tree and Quick
    /// Open file list to whatever the user picks. Leaves open tabs alone.
    fn open_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Folder".into()),
        });

        let view = cx.entity();
        cx.spawn_in(window, async move |_, window| {
            let root = paths.await.ok()?.ok()??.into_iter().next()?;
            window
                .update(|_window, cx| {
                    view.update(cx, |this, cx| this.set_root(root, cx));
                })
                .ok()
        })
        .detach();
    }

    fn set_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let items = build_tree_items(&root);
        self.all_files = flatten_files(&root);
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
        });
        self.root = root;
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let primary_mod = keystroke.modifiers.platform || keystroke.modifiers.control;

        if primary_mod && keystroke.key.as_str() == "o" {
            self.open_folder(window, cx);
            return;
        }

        if primary_mod && keystroke.key.as_str() == "p" {
            self.palette_open = true;
            self.palette_input
                .update(cx, |state, cx| state.focus(window, cx));
            cx.notify();
            return;
        }

        if self.palette_open && keystroke.key.as_str() == "escape" {
            self.palette_open = false;
            cx.notify();
        }
    }

    fn render_palette(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> Option<Div> {
        if !self.palette_open {
            return None;
        }

        let query = self.palette_input.read(cx).value().to_lowercase();
        let matches: Vec<&PathBuf> = self
            .all_files
            .iter()
            .filter(|p| query.is_empty() || p.to_string_lossy().to_lowercase().contains(&query))
            .take(20)
            .collect();

        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(96.))
                .bg(rgba(0x000000aa))
                .child(
                    v_flex()
                        .w(px(480.))
                        .max_h(px(360.))
                        .bg(cx.theme().background)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(cx.theme().radius)
                        .child(div().p_2().child(Input::new(&self.palette_input)))
                        .child(
                            v_flex().id("palette-list").overflow_y_scroll().children(matches.into_iter().enumerate().map(
                                |(ix, path)| {
                                    let label = path.to_string_lossy().to_string();
                                    let path = path.clone();
                                    div()
                                        .id(("palette-item", ix))
                                        .w_full()
                                        .px_3()
                                        .py_1()
                                        .text_sm()
                                        .child(label)
                                        .on_click({
                                            let view = view.clone();
                                            move |_event, window, cx| {
                                                view.update(cx, |this, cx| {
                                                    this.open_path(path.clone(), window, cx);
                                                    this.palette_open = false;
                                                    cx.notify();
                                                });
                                            }
                                        })
                                },
                            )),
                        ),
                ),
        )
    }

    fn active_editor(&self) -> &Entity<EditorView> {
        &self.tabs[self.active_tab].editor
    }

    /// Open `path` in a new tab, or switch to it if it's already open.
    fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.tabs.iter().position(|t| t.path.as_deref() == Some(path.as_path())) {
            self.active_tab = ix;
        } else {
            let buffer = match Buffer::open(&path) {
                Ok(buffer) => buffer,
                Err(err) => {
                    eprintln!("failed to open {}: {err}", path.display());
                    return;
                }
            };
            let editor = cx.new(|cx| EditorView::from_buffer(buffer, cx));
            self.tabs.push(Tab {
                path: Some(path),
                editor,
            });
            self.active_tab = self.tabs.len() - 1;
        }

        let focus_handle = self.active_editor().read(cx).focus_handle(cx);
        window.focus(&focus_handle, cx);
        cx.notify();
    }

    fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.tabs.remove(ix);
        if self.tabs.is_empty() {
            let editor = cx.new(|cx| EditorView::new(cx));
            self.tabs.push(Tab { path: None, editor });
            self.active_tab = 0;
        } else if self.active_tab > ix {
            self.active_tab -= 1;
        } else {
            self.active_tab = self.active_tab.min(self.tabs.len() - 1);
        }
        cx.notify();
    }
}

impl Render for DartIde {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();

        v_flex()
            .size_full()
            .relative()
            .key_context("DartIde")
            .on_key_down(cx.listener(Self::on_key_down))
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .w_full()
                    .px_4()
                    .py_2()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().font_bold().child("Dart/Flutter IDE — M1"))
                    .child(
                        Button::new("open-folder")
                            .ghost()
                            .small()
                            .label(format!("Open Folder\u{2026} ({})", self.root.display()))
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.open_folder(window, cx);
                            })),
                    ),
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
                                                let view = view.clone();
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
                                                            let view = view.clone();
                                                            let is_folder = item.is_folder();
                                                            let path = PathBuf::from(item.id.as_str());
                                                            move |_event, window, cx| {
                                                                if is_folder {
                                                                    return;
                                                                }
                                                                view.update(cx, |this, cx| {
                                                                    this.open_path(
                                                                        path.clone(),
                                                                        window,
                                                                        cx,
                                                                    );
                                                                });
                                                            }
                                                        })
                                                }
                                            })
                                            .size_full(),
                                        ),
                                ),
                        )
                        .child(
                            v_flex()
                                .h(relative(1.))
                                .child(self.render_tab_bar(&view, cx))
                                .child(
                                    div()
                                        .flex_1()
                                        .w_full()
                                        .min_h(px(0.))
                                        .child(self.active_editor().clone()),
                                )
                                .into_any_element(),
                        ),
                ),
            )
            .child(self.render_status_bar(cx))
            .when_some(self.render_palette(&view, cx), |el, palette| el.child(palette))
    }
}

impl DartIde {
    fn render_tab_bar(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .border_b_1()
            .border_color(cx.theme().border)
            .children(self.tabs.iter().enumerate().map(|(ix, tab)| {
                let is_active = ix == self.active_tab;

                h_flex()
                    .id(("tab", ix))
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .when(is_active, |d| d.bg(cx.theme().accent))
                    .child(
                        div()
                            .id(("tab-label", ix))
                            .child(tab.title())
                            .on_click({
                                let view = view.clone();
                                move |_event, _window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.active_tab = ix;
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        div()
                            .id(("tab-close", ix))
                            .text_color(cx.theme().muted_foreground)
                            .child("×")
                            .on_click({
                                let view = view.clone();
                                move |_event, _window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.close_tab(ix, cx);
                                    });
                                }
                            }),
                    )
            }))
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = self.active_editor().read(cx);
        let buffer = editor.buffer();
        let mut status = match buffer.path() {
            Some(path) => format!(
                "{}{}",
                path.display(),
                if buffer.is_dirty() { " \u{2022} unsaved" } else { "" }
            ),
            None => "untitled".to_string(),
        };

        let (errors, warnings) = editor.diagnostic_summary();
        if errors > 0 || warnings > 0 {
            status.push_str(&format!("  \u{2022}  {errors} error(s), {warnings} warning(s)"));
        }

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
