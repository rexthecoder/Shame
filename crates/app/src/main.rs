//! A GPUI window with a dock-style split layout: a file tree panel reading
//! the real project directory, and our own `editor-view` (M1) in place of
//! the M0 `Textarea` stand-in.
//!
//! See `NOTES.md` at the repo root for the measured startup time, idle RAM,
//! and binary size from the M0 spike.

use std::path::{Path, PathBuf};

use core_buffer::Buffer;
use core_lsp::lsp_types;
use editor_view::{EditorEvent, EditorView};
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
    rename_target: Option<(Entity<EditorView>, u32, u32)>,
    rename_input: Entity<InputState>,
    code_actions: Option<(Entity<EditorView>, Vec<lsp_types::CodeActionOrCommand>)>,
    _subscriptions: Vec<Subscription>,
}

impl DartIde {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let items = build_tree_items(&root);
        let all_files = flatten_files(&root);

        let tree_state = cx.new(|cx| TreeState::new(cx).items(items));
        let palette_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Go to file..."));
        let rename_input = cx.new(|cx| InputState::new(window, cx).placeholder("New name..."));

        let buffer = Buffer::from_str(
            "// Lightweight Dart/Flutter IDE — M1\n\
             //\n\
             // This is our own editor-view (core-buffer + a from-scratch\n\
             // GPUI element), not gpui-component's Textarea. Try typing,\n\
             // arrow keys, backspace/delete, Home/End, and cmd+Z/cmd+Shift+Z.\n\
             // Click a file in the tree (or cmd+P to go to one) to open it.\n\
             // cmd+O to open a different folder. cmd+click a symbol to go\n\
             // to its definition.\n",
        );
        let editor = cx.new(|cx| EditorView::from_buffer(buffer, cx));

        let mut this = Self {
            root,
            tree_state,
            tabs: vec![Tab { path: None, editor: editor.clone() }],
            active_tab: 0,
            all_files,
            palette_open: false,
            palette_input,
            rename_target: None,
            rename_input,
            code_actions: None,
            _subscriptions: Vec::new(),
        };
        this.watch_editor(&editor, window, cx);
        this
    }

    /// Subscribe to an [`EditorEvent`] from one tab's editor: so far,
    /// go-to-definition landing in a different file (needs `DartIde`'s
    /// tabs to actually open it) and rename asking for a new name (needs
    /// `DartIde`'s input prompt — `editor-view` won't build its own, see
    /// its module docs).
    fn watch_editor(&mut self, editor: &Entity<EditorView>, window: &mut Window, cx: &mut Context<Self>) {
        let subscription = cx.subscribe_in(editor, window, |this, editor, event, window, cx| match event {
            EditorEvent::OpenAt { path, line, column } => {
                let (line, column) = (*line, *column);
                this.open_path(path.clone(), window, cx);
                let target = this.active_editor().clone();
                target.update(cx, |editor, cx| editor.set_cursor_to_position(line, column, cx));
            }
            EditorEvent::RequestRename { line, column } => {
                this.rename_target = Some((editor.clone(), *line, *column));
                this.rename_input.update(cx, |state, cx| {
                    state.set_value("", window, cx);
                    state.focus(window, cx);
                });
                cx.notify();
            }
            EditorEvent::ShowCodeActions { actions } => {
                this.code_actions = Some((editor.clone(), actions.clone()));
                cx.notify();
            }
        });
        self._subscriptions.push(subscription);
    }

    fn render_rename_prompt(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> Option<Div> {
        self.rename_target.as_ref()?;

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
                        .w(px(360.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(cx.theme().radius)
                        .shadow_xl()
                        .child(
                            div()
                                .px_3()
                                .py_2()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Rename symbol"),
                        )
                        .child(
                            div()
                                .px_3()
                                .pb_3()
                                .on_key_down({
                                    let view = view.clone();
                                    move |event, window, cx| {
                                        let key = event.keystroke.key.as_str();
                                        if key == "escape" {
                                            view.update(cx, |this, cx| {
                                                this.rename_target = None;
                                                cx.notify();
                                            });
                                        } else if key == "enter" {
                                            view.update(cx, |this, cx| this.submit_rename(window, cx));
                                        }
                                    }
                                })
                                .child(Input::new(&self.rename_input)),
                        ),
                ),
        )
    }

    /// A small list overlay for picking one of several code actions —
    /// same `ListItem`-in-an-overlay pattern as the file tree and Quick
    /// Open, not a new design. Only shown when there's more than one
    /// action; `editor-view` applies a single action immediately
    /// without asking.
    fn render_code_actions(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> Option<Div> {
        let (_, actions) = self.code_actions.as_ref()?;

        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(96.))
                .bg(rgba(0x000000aa))
                .on_key_down({
                    let view = view.clone();
                    move |event, _window, cx| {
                        if event.keystroke.key.as_str() == "escape" {
                            view.update(cx, |this, cx| {
                                this.code_actions = None;
                                cx.notify();
                            });
                        }
                    }
                })
                .child(
                    v_flex()
                        .w(px(420.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(cx.theme().radius)
                        .shadow_xl()
                        .children(actions.iter().enumerate().map(|(ix, action)| {
                            let title = match action {
                                lsp_types::CodeActionOrCommand::Command(c) => c.title.clone(),
                                lsp_types::CodeActionOrCommand::CodeAction(a) => a.title.clone(),
                            };
                            div()
                                .id(("code-action", ix))
                                .px_3()
                                .py_2()
                                .text_sm()
                                .child(title)
                                .on_click({
                                    let view = view.clone();
                                    move |_event, _window, cx| {
                                        view.update(cx, |this, cx| this.apply_code_action(ix, cx));
                                    }
                                })
                        })),
                ),
        )
    }

    fn apply_code_action(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some((editor, mut actions)) = self.code_actions.take() else {
            return;
        };
        if index < actions.len() {
            let action = actions.remove(index);
            editor.update(cx, |editor, cx| editor.apply_code_action(action, cx));
        }
        cx.notify();
    }

    fn submit_rename(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some((editor, line, column)) = self.rename_target.take() else {
            return;
        };
        let new_name = self.rename_input.read(cx).value().to_string();
        if !new_name.is_empty() {
            editor.update(cx, |editor, _cx| editor.send_rename(line, column, new_name));
        }
        cx.notify();
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
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(cx.theme().radius)
                        .shadow_xl()
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
            self.watch_editor(&editor, window, cx);
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

        let root_name = self
            .root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("/")
            .to_string();

        v_flex()
            .size_full()
            .relative()
            .key_context("DartIde")
            .on_key_down(cx.listener(Self::on_key_down))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div().flex_1().w_full().min_h(px(0.)).child(
                    h_resizable("main-split")
                        .child(
                            resizable_panel()
                                .size(px(240.))
                                .size_range(px(180.)..px(480.))
                                .child(
                                    v_flex()
                                        .size_full()
                                        .bg(cx.theme().sidebar)
                                        .border_r_1()
                                        .border_color(cx.theme().sidebar_border)
                                        .child(
                                            h_flex()
                                                .items_center()
                                                .justify_between()
                                                .pl_3()
                                                .pr_1()
                                                .py_2()
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .font_bold()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(root_name.to_uppercase()),
                                                )
                                                .child(
                                                    Button::new("open-folder")
                                                        .ghost()
                                                        .small()
                                                        .icon(IconName::FolderOpen)
                                                        .tooltip("Open Folder\u{2026} (cmd/ctrl+O)")
                                                        .on_click(cx.listener(|this, _event, window, cx| {
                                                            this.open_folder(window, cx);
                                                        })),
                                                ),
                                        )
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
                                                        .text_color(cx.theme().sidebar_foreground)
                                                        .px_3()
                                                        .py_0p5()
                                                        .pl(px(16.) * entry.depth() + px(12.))
                                                        .child(
                                                            h_flex()
                                                                .gap_2()
                                                                .text_sm()
                                                                .child(
                                                                    Icon::new(icon)
                                                                        .text_color(cx.theme().muted_foreground)
                                                                        .small(),
                                                                )
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
                                            .flex_1()
                                            .w_full(),
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
            .when_some(self.render_rename_prompt(&view, cx), |el, prompt| el.child(prompt))
            .when_some(self.render_code_actions(&view, cx), |el, picker| el.child(picker))
    }
}

impl DartIde {
    fn render_tab_bar(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> impl IntoElement {
        let accent = cx.theme().blue;
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;

        h_flex()
            .w_full()
            .h(px(34.))
            .flex_shrink_0()
            .bg(cx.theme().tab_bar)
            .border_b_1()
            .border_color(cx.theme().border)
            .children(self.tabs.iter().enumerate().map(|(ix, tab)| {
                let is_active = ix == self.active_tab;
                let is_dirty = tab.editor.read(cx).buffer().is_dirty();

                h_flex()
                    .id(("tab", ix))
                    .h_full()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .cursor_pointer()
                    .border_t(px(2.))
                    .border_color(if is_active { accent } else { gpui_kit::transparent_black() })
                    .bg(if is_active { cx.theme().tab_active } else { cx.theme().tab })
                    .text_color(if is_active { cx.theme().tab_active_foreground } else { cx.theme().tab_foreground })
                    .child(
                        div()
                            .id(("tab-label", ix))
                            .text_sm()
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
                            .w(px(14.))
                            .text_center()
                            .text_color(if is_dirty { accent } else { muted })
                            .hover(move |s| s.text_color(foreground))
                            .child(if is_dirty { "\u{25cf}" } else { "\u{00d7}" })
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
        let path = match buffer.path() {
            Some(path) => path.display().to_string(),
            None => "untitled".to_string(),
        };
        let is_dirty = buffer.is_dirty();
        let (line, column) = editor.cursor_position();
        let (errors, warnings) = editor.diagnostic_summary();
        let is_dart = buffer.path().is_some_and(|p| p.extension().is_some_and(|e| e == "dart"));

        h_flex()
            .w_full()
            .h(px(24.))
            .flex_shrink_0()
            .px_3()
            .items_center()
            .justify_between()
            .bg(cx.theme().status_bar)
            .border_t_1()
            .border_color(cx.theme().status_bar_border)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .when(is_dirty, |el| {
                        el.child(div().text_color(cx.theme().blue).child("\u{25cf}"))
                    })
                    .child(path),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .when(errors > 0, |el| {
                        el.child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .text_color(cx.theme().danger)
                                .child(Icon::new(IconName::CircleX).small())
                                .child(errors.to_string()),
                        )
                    })
                    .when(warnings > 0, |el| {
                        el.child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .text_color(cx.theme().warning)
                                .child(Icon::new(IconName::TriangleAlert).small())
                                .child(warnings.to_string()),
                        )
                    })
                    .child(format!("Ln {line}, Col {column}"))
                    .when(is_dart, |el| el.child("Dart")),
            )
    }
}

fn main() {
    let app = gpui_kit::application().with_assets(Assets);

    app.run(move |cx| {
        // This must be called before using any GPUI Component features.
        gpui_kit::init(cx);
        // Dark by default — a code editor living in a bright-white chrome
        // looks like nothing in this space (Zed, Fleet, VS Code all default
        // dark), and `gpui-component`'s own default dark theme already sits
        // close to it (near-black backgrounds, a blue selection/accent).
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);

        // A large centered window, not a `WindowBounds::Maximized` or a
        // window sized to `display.bounds()` directly — both of those
        // reliably reproduced a real bug on this machine: the window never
        // became visible (frontmost in the menu bar, no content reachable
        // on screen at all) while a CPU core pegged at 100% and memory grew
        // without bound, independent of the titlebar settings tested
        // alongside each. 1600×1000 is still a large, substantial window —
        // just not exactly the display's own bounds, which is what
        // triggered the bug.
        let bounds = Bounds::centered(None, size(px(1600.), px(1000.)), cx);

        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Dart/Flutter IDE".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        gpui_kit::open_window(window_options, cx, |window, cx| {
            cx.new(|cx| DartIde::new(window, cx))
        })
        .expect("Failed to open window");
    });
}
