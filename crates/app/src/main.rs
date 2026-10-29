//! A GPUI window with a dock-style split layout: a file tree panel reading
//! the real project directory, and our own `editor-view` (M1) in place of
//! the M0 `Textarea` stand-in.
//!
//! See `NOTES.md` at the repo root for the measured startup time, idle RAM,
//! and binary size from the M0 spike.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

mod chrome;
mod editor;
mod rails;
mod shell;
mod theme;
#[allow(dead_code)]
mod tokens;

use core_buffer::Buffer;
use core_lsp::lsp_types;
use editor_view::{EditorEvent, EditorView};
use gpui_kit::assets::AllAssets;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    h_flex,
    input::{Input, InputState},
    tree::TreeState,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use rails::files::{build_tree_items, file_icon_color, icon_for_file};

/// Directory/file names we never want to show in the file tree.
pub(crate) const IGNORED_NAMES: &[&str] = &["target", "node_modules", ".DS_Store"];

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
        if name == ".git" || IGNORED_NAMES.contains(&name) {
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

pub(crate) struct Tab {
    /// `None` for the initial scratch buffer; every file opened from the
    /// tree gets a path, which also doubles as the tab's dedup key.
    pub(crate) path: Option<PathBuf>,
    pub(crate) editor: Entity<EditorView>,
}

impl Tab {
    pub(crate) fn title(&self) -> String {
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
    pub(crate) root: PathBuf,
    pub(crate) git_branch: Option<String>,
    /// Cached `git status` for the tree's row badges.
    pub(crate) git_status: HashMap<PathBuf, char>,
    pub(crate) sidebar_open: bool,
    pub(crate) terminal_open: bool,
    pub(crate) agent_open: bool,
    pub(crate) show_hidden: bool,
    pub(crate) tree_state: Entity<TreeState>,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active_tab: usize,
    pub(crate) all_files: Vec<PathBuf>,
    pub(crate) palette_open: bool,
    pub(crate) palette_input: Entity<InputState>,
    pub(crate) palette_selected: usize,
    pub(crate) rename_target: Option<(Entity<EditorView>, u32, u32)>,
    pub(crate) rename_input: Entity<InputState>,
    pub(crate) code_actions: Option<(Entity<EditorView>, Vec<lsp_types::CodeActionOrCommand>)>,
    _subscriptions: Vec<Subscription>,
}

impl DartIde {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let show_hidden = true;
        let items = build_tree_items(&root, show_hidden);
        let all_files = flatten_files(&root);

        let tree_state = cx.new(|cx| TreeState::new(cx).items(items));
        let palette_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Go to file..."));
        let rename_input = cx.new(|cx| InputState::new(window, cx).placeholder("New name..."));

        let git_branch = core_workspace::current_branch(&root);
        let git_status = core_workspace::status_map(&root);
        let mut this = Self {
            root,
            git_branch,
            git_status,
            sidebar_open: true,
            terminal_open: true,
            agent_open: true,
            show_hidden,
            tree_state,
            tabs: Vec::new(),
            active_tab: 0,
            all_files,
            palette_open: false,
            palette_input,
            palette_selected: 0,
            rename_target: None,
            rename_input,
            code_actions: None,
            _subscriptions: Vec::new(),
        };

        let palette_changes = cx.subscribe(
            &this.palette_input,
            |this, _, event: &gpui_kit::component::input::InputEvent, cx| {
                if matches!(event, gpui_kit::component::input::InputEvent::Change) {
                    this.palette_selected = 0;
                    cx.notify();
                }
            },
        );
        this._subscriptions.push(palette_changes);
        this
    }

    fn palette_matches(&self, cx: &Context<Self>) -> Vec<PathBuf> {
        let query = self.palette_input.read(cx).value().to_lowercase();
        self.all_files
            .iter()
            .filter(|path| {
                query.is_empty() || path.to_string_lossy().to_lowercase().contains(&query)
            })
            .take(20)
            .cloned()
            .collect()
    }

    fn open_palette_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let matches = self.palette_matches(cx);
        let Some(path) = matches.get(self.palette_selected).cloned() else {
            return;
        };
        self.open_path(path, window, cx);
        self.palette_open = false;
        cx.notify();
    }

    fn watch_editor(
        &mut self,
        editor: &Entity<EditorView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let subscription =
            cx.subscribe_in(editor, window, |this, editor, event, window, cx| match event {
                EditorEvent::OpenAt { path, line, column } => {
                    let (line, column) = (*line, *column);
                    this.open_path(path.clone(), window, cx);
                    if let Some(target) = this.active_editor().cloned() {
                        target.update(cx, |editor, cx| {
                            editor.set_cursor_to_position(line, column, cx)
                        });
                    }
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
                .pt(px(84.))
                .bg(rgba(0x00000066))
                .child(
                    v_flex()
                        .w(px(360.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(tokens::RADIUS_CARD))
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
                                            view.update(cx, |this, cx| {
                                                this.submit_rename(window, cx)
                                            });
                                        }
                                    }
                                })
                                .child(Input::new(&self.rename_input)),
                        ),
                ),
        )
    }

    fn render_code_actions(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> Option<Div> {
        let (_, actions) = self.code_actions.as_ref()?;

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
                        .rounded(px(tokens::RADIUS_CARD))
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
                                .cursor_pointer()
                                .hover(|s| s.bg(cx.theme().list_hover))
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

    pub(crate) fn open_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let items = build_tree_items(&root, self.show_hidden);
        self.all_files = flatten_files(&root);
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
        });
        self.git_branch = core_workspace::current_branch(&root);
        self.git_status = core_workspace::status_map(&root);
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

        if primary_mod && keystroke.key.as_str() == "n" {
            self.new_scratch_tab(window, cx);
            return;
        }

        if primary_mod && keystroke.key.as_str() == "w" {
            if !self.tabs.is_empty() {
                self.close_tab(self.active_tab, cx);
            }
            return;
        }

        if primary_mod && keystroke.key.as_str() == "p" {
            self.palette_open = true;
            self.palette_selected = 0;
            self.palette_input
                .update(cx, |state, cx| state.focus(window, cx));
            cx.notify();
            return;
        }

        if primary_mod && keystroke.key.as_str() == "b" {
            self.sidebar_open = !self.sidebar_open;
            cx.notify();
            return;
        }

        if primary_mod && keystroke.key.as_str() == "j" {
            self.terminal_open = !self.terminal_open;
            cx.notify();
            return;
        }

        if self.palette_open {
            match keystroke.key.as_str() {
                "escape" => {
                    self.palette_open = false;
                    cx.notify();
                }
                "up" => {
                    self.palette_selected = self.palette_selected.saturating_sub(1);
                    cx.notify();
                }
                "down" => {
                    let last = self.palette_matches(cx).len().saturating_sub(1);
                    self.palette_selected = (self.palette_selected + 1).min(last);
                    cx.notify();
                }
                "enter" => self.open_palette_selection(window, cx),
                _ => {}
            }
        }
    }

    fn render_palette(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> Option<Div> {
        if !self.palette_open {
            return None;
        }

        let matches = self.palette_matches(cx);
        let selected = self.palette_selected.min(matches.len().saturating_sub(1));

        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(84.))
                .bg(rgba(0x00000066))
                .child(
                    v_flex()
                        .w(px(560.))
                        .max_h(px(420.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .overflow_hidden()
                        .child(
                            div()
                                .p_2()
                                .border_b_1()
                                .border_color(cx.theme().border)
                                .child(Input::new(&self.palette_input)),
                        )
                        .child(
                            v_flex()
                                .id("palette-list")
                                .py_1()
                                .overflow_y_scroll()
                                .children(matches.into_iter().enumerate().map(|(ix, path)| {
                                    let name = path
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or_default()
                                        .to_string();
                                    let directory = path
                                        .parent()
                                        .and_then(|p| p.strip_prefix(&self.root).ok())
                                        .map(|p| p.to_string_lossy().to_string())
                                        .filter(|p| !p.is_empty())
                                        .unwrap_or_else(|| ".".to_string());
                                    let row_path = path.clone();
                                    let is_selected = ix == selected;

                                    h_flex()
                                        .id(("palette-item", ix))
                                        .w_full()
                                        .items_center()
                                        .gap_2()
                                        .px_3()
                                        .py_2()
                                        .cursor_pointer()
                                        .when(is_selected, |el| el.bg(cx.theme().list_active))
                                        .when(!is_selected, |el| {
                                            el.hover(|s| s.bg(cx.theme().list_hover))
                                        })
                                        .child(
                                            icon_for_file(&name)
                                                .text_color(file_icon_color(&name))
                                                .small(),
                                        )
                                        .child(
                                            v_flex()
                                                .gap_0p5()
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .text_color(cx.theme().foreground)
                                                        .child(name),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(directory),
                                                ),
                                        )
                                        .on_click({
                                            let view = view.clone();
                                            move |_event, window, cx| {
                                                view.update(cx, |this, cx| {
                                                    this.open_path(row_path.clone(), window, cx);
                                                    this.palette_open = false;
                                                    cx.notify();
                                                });
                                            }
                                        })
                                })),
                        )
                        .child(
                            h_flex()
                                .px_3()
                                .py_1()
                                .gap_3()
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("↑↓ Navigate")
                                .child("↵ Open")
                                .child("Esc Close"),
                        ),
                ),
        )
    }

    pub(crate) fn active_editor(&self) -> Option<&Entity<EditorView>> {
        self.tabs.get(self.active_tab).map(|t| &t.editor)
    }

    pub(crate) fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self
            .tabs
            .iter()
            .position(|t| t.path.as_deref() == Some(path.as_path()))
        {
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

        if let Some(editor) = self.active_editor() {
            let focus_handle = editor.read(cx).focus_handle(cx);
            window.focus(&focus_handle, cx);
        }
        cx.notify();
    }

    pub(crate) fn new_scratch_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = cx.new(|cx| EditorView::new(cx));
        self.watch_editor(&editor, window, cx);
        self.tabs.push(Tab { path: None, editor });
        self.active_tab = self.tabs.len() - 1;

        if let Some(editor) = self.active_editor() {
            let focus_handle = editor.read(cx).focus_handle(cx);
            window.focus(&focus_handle, cx);
        }
        cx.notify();
    }

    pub(crate) fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.tabs.remove(ix);
        if self.tabs.is_empty() {
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();

        v_flex()
            .size_full()
            .relative()
            .key_context("DartIde")
            .on_key_down(cx.listener(Self::on_key_down))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_titlebar(window, cx))
            .child(self.render_shell(&view, cx))
            .child(self.render_status_bar(cx))
            .when_some(self.render_palette(&view, cx), |el, palette| el.child(palette))
            .when_some(self.render_rename_prompt(&view, cx), |el, prompt| {
                el.child(prompt)
            })
            .when_some(self.render_code_actions(&view, cx), |el, picker| {
                el.child(picker)
            })
    }
}

fn main() {
    let app = gpui_kit::application().with_assets(AllAssets);

    app.run(move |cx| {
        gpui_kit::init(cx);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
        theme::apply(cx);

        let bounds = Bounds::centered(None, size(px(1600.), px(1000.)), cx);

        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: Some(point(px(12.), px(12.))),
            }),
            ..Default::default()
        };

        gpui_kit::open_window(window_options, cx, |window, cx| {
            cx.new(|cx| DartIde::new(window, cx))
        })
        .expect("Failed to open window");
    });
}
