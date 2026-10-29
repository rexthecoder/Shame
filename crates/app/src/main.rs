//! A GPUI window with a dock-style split layout: a file tree panel reading
//! the real project directory, and our own `editor-view` (M1) in place of
//! the M0 `Textarea` stand-in.
//!
//! See `NOTES.md` at the repo root for the measured startup time, idle RAM,
//! and binary size from the M0 spike.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

mod theme;
// Roll-out in progress (docs/design-plan.md): new components consume these
// as they land, so some tokens are unused until their phase completes.
#[allow(dead_code)]
mod tokens;

use core_buffer::Buffer;
use core_lsp::lsp_types;
use editor_view::{EditorEvent, EditorView};
// The full icon catalog, not the default component subset: the chrome uses
// icons (Columns2, Bug, Wand, Zap, UserPlus, PenTool...) that only exist in
// `AllAssets`, and the default `Assets` renders those paths blank.
use gpui_kit::assets::AllAssets;
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
const IGNORED_NAMES: &[&str] = &["target", "node_modules", ".DS_Store"];

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

        // Dotfiles are shown (the reference tree lists `.dart_tool`,
        // `.gitignore`, `.metadata`); only `.git` itself stays hidden.
        if name == ".git" || IGNORED_NAMES.contains(&name) {
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

/// Source-code extensions get `FileCode` instead of the generic `File`
/// icon — the same at-a-glance "this is code, not config/docs" cue Zed
/// and Fleet both give their file trees, even without per-language icon
/// art (`gpui-component`'s bundled icon set has no Rust/Dart logos).
/// `FileCode` isn't in `gpui_component::IconName`'s curated subset, so
/// this reaches into the full asset set (`gpui_kit::assets::IconName`)
/// instead — both implement the same `IconNamed` trait `Icon::new`
/// accepts, so they mix freely.
fn icon_for_file(name: &str) -> Icon {
    let is_code = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| matches!(ext, "rs" | "dart" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "c" | "cpp" | "h"));
    if is_code {
        Icon::new(gpui_kit::assets::IconName::FileCode)
    } else {
        Icon::new(IconName::File)
    }
}

/// Chrome icon button used by the titlebar. Several are placeholders for
/// phases that have not landed yet; the reference bar is dense on purpose.
fn icon_button(id: &'static str, icon: gpui_kit::assets::IconName) -> Button {
    Button::new(id).ghost().small().icon(Icon::new(icon))
}

/// The small git-status chip on a file-tree row: a single letter on a
/// tinted square, the same idiom the reference tree uses.
fn status_badge(letter: char, cx: &App) -> Div {
    let color = match letter {
        'M' => cx.theme().warning,
        'A' => cx.theme().success,
        'D' => cx.theme().danger,
        'R' => cx.theme().info,
        'U' => cx.theme().info,
        _ => cx.theme().muted_foreground,
    };
    div()
        .flex_shrink_0()
        .size(px(16.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .bg(color.opacity(0.16))
        .text_size(px(9.5))
        .font_medium()
        .text_color(color)
        .child(letter.to_string())
}

/// A friendly language name for the status bar — every editor shows one
/// for the current file, not just when it happens to be the one
/// language the project cares about most.
fn language_label(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("dart") => "Dart".to_string(),
        Some("rs") => "Rust".to_string(),
        Some("toml") => "TOML".to_string(),
        Some("yaml") | Some("yml") => "YAML".to_string(),
        Some("json") => "JSON".to_string(),
        Some("md") => "Markdown".to_string(),
        Some(ext) => ext.to_uppercase(),
        None => "Plain Text".to_string(),
    }
}

/// `project / crates / app / src / main.rs`, not a raw absolute path —
/// the status-bar breadcrumb Fleet (and most other editors) shows,
/// rooted at the open project rather than the filesystem root.
fn breadcrumb_path(root: &Path, path: &Path) -> String {
    let root_name = root.file_name().and_then(|n| n.to_str()).unwrap_or("/");
    let relative = path.strip_prefix(root).unwrap_or(path);
    let mut segments = vec![root_name.to_string()];
    segments.extend(relative.components().filter_map(|c| c.as_os_str().to_str().map(str::to_string)));
    segments.join(" \u{203A} ")
}

/// A subtle per-language tint for a file's icon — the way Zed and Fleet
/// let you recognise a file's type at a glance without a loud text badge.
/// `untitled`/extension-less files fall back to a neutral gray.
fn file_icon_color(name: &str) -> Rgba {
    match Path::new(name).extension().and_then(|e| e.to_str()) {
        Some("rs") => rgb(0xdea584),
        Some("dart") => rgb(0x40c4ff),
        Some("ts") | Some("tsx") => rgb(0x4f9cf9),
        Some("js") | Some("jsx") => rgb(0xf0db4f),
        Some("json") => rgb(0xcbcb41),
        Some("toml") => rgb(0xe07b53),
        Some("yaml") | Some("yml") => rgb(0xe05252),
        Some("md") => rgb(0x8fa3b8),
        _ => rgb(0x9aa0a6),
    }
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
    git_branch: Option<String>,
    /// Cached `git status` for the tree's row badges. Recomputed on
    /// refresh and folder change, never on every render — otherwise
    /// typing in Quick Open would shell out to git on each keystroke.
    git_status: HashMap<PathBuf, char>,
    sidebar_open: bool,
    tree_state: Entity<TreeState>,
    tabs: Vec<Tab>,
    active_tab: usize,
    all_files: Vec<PathBuf>,
    palette_open: bool,
    palette_input: Entity<InputState>,
    palette_selected: usize,
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

        let git_branch = core_workspace::current_branch(&root);
        let git_status = core_workspace::status_map(&root);
        let mut this = Self {
            root,
            git_branch,
            git_status,
            sidebar_open: true,
            tree_state,
            tabs: vec![Tab { path: None, editor: editor.clone() }],
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
        this.watch_editor(&editor, window, cx);

        // The palette list is drawn by `DartIde` but the query lives in a
        // child `InputState`, so `DartIde` has to be told when the text
        // changes — otherwise results (and the highlighted row) would stay
        // frozen at whatever was on screen when the palette opened.
        let palette_changes = cx.subscribe(&this.palette_input, |this, _, event: &gpui_kit::component::input::InputEvent, cx| {
            if matches!(event, gpui_kit::component::input::InputEvent::Change) {
                this.palette_selected = 0;
                cx.notify();
            }
        });
        this._subscriptions.push(palette_changes);
        this
    }

    /// Files matching the Quick Open query, in the order they're shown.
    fn palette_matches(&self, cx: &Context<Self>) -> Vec<PathBuf> {
        let query = self.palette_input.read(cx).value().to_lowercase();
        self.all_files
            .iter()
            .filter(|path| query.is_empty() || path.to_string_lossy().to_lowercase().contains(&query))
            .take(20)
            .cloned()
            .collect()
    }

    /// Open whichever Quick Open row is currently highlighted.
    fn open_palette_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let matches = self.palette_matches(cx);
        let Some(path) = matches.get(self.palette_selected).cloned() else {
            return;
        };
        self.open_path(path, window, cx);
        self.palette_open = false;
        cx.notify();
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
                .pt(px(84.))
                .bg(rgba(0x00000066))
                .child(
                    v_flex()
                        .w(px(360.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(10.))
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
                        .rounded(px(10.))
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
                        .rounded(px(10.))
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

    /// A fresh, empty "untitled" tab — the `+` next to the open tabs,
    /// same as every other editor's new-scratch-buffer affordance.
    /// Always adds a new one rather than reusing an existing blank tab,
    /// matching how `close_tab` already treats "no tabs left" (it, too,
    /// creates a fresh blank tab rather than special-casing zero).
    fn new_scratch_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = cx.new(|cx| EditorView::new(cx));
        self.watch_editor(&editor, window, cx);
        self.tabs.push(Tab { path: None, editor });
        self.active_tab = self.tabs.len() - 1;

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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();

        // macOS hides the traffic lights once the window is actually in
        // native fullscreen, so the space this titlebar reserves for them
        // is wasted there — drop the inset in that state instead of
        // leaving a permanent gap, the same thing Zed does.
        let traffic_light_inset = if window.is_fullscreen() { px(12.) } else { px(78.) };

        let root_name = self
            .root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("/")
            .to_string();

        let active_file_name = self
            .active_editor()
            .read(cx)
            .buffer()
            .path()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .map(str::to_string);

        v_flex()
            .size_full()
            .relative()
            .key_context("DartIde")
            .on_key_down(cx.listener(Self::on_key_down))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                h_flex()
                    .w_full()
                    .h(px(tokens::TITLEBAR_H))
                    .flex_shrink_0()
                    .pl(traffic_light_inset)
                    .pr_2()
                    .items_center()
                    .bg(cx.theme().title_bar)
                    .border_b_1()
                    .border_color(cx.theme().title_bar_border)
                    .child(
                        h_flex()
                            .flex_1()
                            .items_center()
                            .gap_1()
                            .child(icon_button("menu", gpui_kit::assets::IconName::Menu))
                            .child(
                                Button::new("layout-sidebar")
                                    .ghost()
                                    .small()
                                    .icon(Icon::new(if self.sidebar_open {
                                        gpui_kit::assets::IconName::PanelLeftClose
                                    } else {
                                        gpui_kit::assets::IconName::PanelLeftOpen
                                    }))
                                    .tooltip("Toggle sidebar (cmd/ctrl+B)")
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.sidebar_open = !this.sidebar_open;
                                        cx.notify();
                                    })),
                            )
                            .child(icon_button("layout-panel", gpui_kit::assets::IconName::PanelTop))
                            .child(icon_button("layout-columns", gpui_kit::assets::IconName::Columns2))
                            .child(icon_button("add-person", gpui_kit::assets::IconName::UserPlus)),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(tokens::UI))
                                    .font_medium()
                                    .text_color(cx.theme().foreground)
                                    .child(root_name.clone()),
                            )
                            .when_some(self.git_branch.clone(), |el, branch| {
                                let ahead = core_workspace::ahead_behind(&self.root)
                                    .map(|(ahead, _)| ahead)
                                    .unwrap_or(0);
                                el.child(
                                    h_flex()
                                        .items_center()
                                        .gap_1()
                                        .text_size(px(tokens::UI_SM))
                                        .text_color(cx.theme().green)
                                        .child(Icon::new(gpui_kit::assets::IconName::GitBranch).small())
                                        .child(branch)
                                        .when(ahead > 0, |el| {
                                            el.child(format!("\u{2191}{ahead}"))
                                        }),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .justify_end()
                            .items_center()
                            .gap_1()
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(tokens::SPACE_1))
                                    .text_size(px(tokens::UI_SM))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(Icon::new(gpui_kit::assets::IconName::Monitor).small())
                                    .child("macOS"),
                            )
                            .when_some(active_file_name, |el, file| {
                                el.child(
                                    div()
                                        .h(px(22.))
                                        .px(px(tokens::SPACE_3))
                                        .flex()
                                        .items_center()
                                        .gap(px(tokens::SPACE_1))
                                        .rounded(px(tokens::RADIUS_PILL))
                                        .bg(cx.theme().secondary)
                                        .text_size(px(tokens::UI_SM))
                                        .text_color(cx.theme().foreground)
                                        .child(
                                            Icon::new(gpui_kit::assets::IconName::PenTool)
                                                .small()
                                                .text_color(cx.theme().info),
                                        )
                                        .child(file),
                                )
                            })
                            .child(icon_button("run", gpui_kit::assets::IconName::Play))
                            .child(icon_button("debug", gpui_kit::assets::IconName::Bug))
                            .child(icon_button("agent-wand", gpui_kit::assets::IconName::Wand))
                            .child(
                                Button::new("quick-open")
                                    .ghost()
                                    .small()
                                    .icon(Icon::new(gpui_kit::assets::IconName::Search))
                                    .tooltip("Go to file (cmd/ctrl+P)")
                                    .on_click(cx.listener(|this, _event, window, cx| {
                                        this.palette_open = true;
                                        this.palette_selected = 0;
                                        this.palette_input
                                            .update(cx, |state, cx| state.focus(window, cx));
                                        cx.notify();
                                    })),
                            )
                            .child(icon_button("fast", gpui_kit::assets::IconName::Zap))
                            .child(icon_button("settings", gpui_kit::assets::IconName::Settings))
                            // Menu / layout / run / debug / wand / bolt /
                            // settings are layout placeholders until the
                            // phase that gives each one a handler; only the
                            // sidebar toggle and search are wired today.
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .p(px(tokens::GUTTER))
                    .child({
                    let editor_card = v_flex()
                                .flex_1()
                                .h_full()
                                .min_h(px(0.))
                                .bg(cx.theme().background)
                                .border_1()
                                .border_color(cx.theme().border)
                                .rounded(px(tokens::RADIUS_CARD))
                                .overflow_hidden()
                                .child(self.render_tabs(&view, cx))
                                .child(
                                    div()
                                        .flex_1()
                                        .w_full()
                                        .min_h(px(0.))
                                        .child(self.active_editor().clone()),
                                )
                        .into_any_element();

                    let editor_column = v_flex()
                        .flex_1()
                        .w_full()
                        .h(relative(1.))
                        .min_h(px(0.))
                        .px(px(tokens::GUTTER / 2.))
                        .gap(px(tokens::GUTTER))
                        .child(editor_card)
                        .child(self.render_terminal(cx))
                        .into_any_element();

                    if !self.sidebar_open {
                        h_flex()
                            .h_full()
                            .w_full()
                            .min_h(px(0.))
                            .child(editor_column)
                            .child(
                                div()
                                    .h_full()
                                    .w(px(tokens::RAIL_RIGHT))
                                    .pl(px(tokens::GUTTER / 2.))
                                    .child(self.render_agent_rail(cx)),
                            )
                            .into_any_element()
                    } else {
                        let right_rail = resizable_panel()
                            .size(px(tokens::RAIL_RIGHT))
                            .size_range(px(tokens::RAIL_RIGHT_MIN)..px(tokens::RAIL_RIGHT_MAX))
                            .pl(px(tokens::GUTTER / 2.))
                            .child(self.render_agent_rail(cx));

                        h_resizable("shell-split")
                            .child(
                                resizable_panel()
                                    .size(px(tokens::RAIL_LEFT))
                                    .size_range(
                                        px(tokens::RAIL_LEFT_MIN)..px(tokens::RAIL_LEFT_MAX),
                                    )
                                    .pr(px(tokens::GUTTER / 2.))
                                    .child(
                                        v_flex()
                                            .size_full()
                                            .bg(cx.theme().sidebar)
                                            .border_1()
                                            .border_color(cx.theme().border)
                                            .rounded(px(tokens::RADIUS_CARD))
                                            .overflow_hidden()
                                            .child(
                                                h_flex()
                                                    .w_full()
                                                    .h(px(34.))
                                                    .flex_shrink_0()
                                                    .items_center()
                                                    .justify_between()
                                                    .px(px(tokens::SPACE_4))
                                                    .child(
                                                        h_flex()
                                                            .items_center()
                                                            .gap(px(tokens::SPACE_2))
                                                            .h(px(26.))
                                                            .px(px(tokens::SPACE_3))
                                                            .rounded(px(tokens::RADIUS_PANEL))
                                                            .bg(cx.theme().secondary)
                                                            .border_1()
                                                            .border_color(tokens::accent_dim())
                                                            .child(
                                                                Icon::new(IconName::Folder)
                                                                    .small()
                                                                    .text_color(cx.theme().primary),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_size(px(tokens::UI))
                                                                    .text_color(cx.theme().foreground)
                                                                    .child("Files"),
                                                            )
                                                            .child(
                                                                Button::new("close-rail")
                                                                    .ghost()
                                                                    .small()
                                                                    .icon(Icon::new(gpui_kit::assets::IconName::X))
                                                                    .tooltip("Hide the file tree")
                                                                    .on_click(cx.listener(
                                                                        |this, _event, _window, cx| {
                                                                            this.sidebar_open = false;
                                                                            cx.notify();
                                                                        },
                                                                    )),
                                                            ),
                                                    )
                                                    .child(
                                                        Button::new("rail-new")
                                                            .ghost()
                                                            .small()
                                                            .icon(Icon::new(IconName::Plus))
                                                            .tooltip("New scratch tab")
                                                            .on_click(cx.listener(
                                                                |this, _event, window, cx| {
                                                                    this.new_scratch_tab(window, cx);
                                                                },
                                                            )),
                                                    ),
                                            )
                                            .child(
                                                h_flex()
                                                    .w_full()
                                                    .h(px(28.))
                                                    .flex_shrink_0()
                                                    .items_center()
                                                    .justify_between()
                                                    .px(px(tokens::SPACE_4))
                                                    .child(
                                                        h_flex()
                                                            .items_center()
                                                            .gap(px(tokens::SPACE_2))
                                                            .child(
                                                                div()
                                                                    .text_size(px(tokens::UI_SM))
                                                                    .text_color(cx.theme().muted_foreground)
                                                                    .child(root_name.clone()),
                                                            )
                                                            .child(
                                                                Button::new("rail-filter")
                                                                    .ghost()
                                                                    .small()
                                                                    .icon(Icon::new(gpui_kit::assets::IconName::Search))
                                                                    .tooltip("Go to file (cmd/ctrl+P)")
                                                                    .on_click(cx.listener(
                                                                        |this, _event, window, cx| {
                                                                            this.palette_open = true;
                                                                            this.palette_selected = 0;
                                                                            this.palette_input.update(cx, |state, cx| {
                                                                                state.focus(window, cx);
                                                                            });
                                                                            cx.notify();
                                                                        },
                                                                    )),
                                                            ),
                                                    )
                                                    .child(
                                                        h_flex()
                                                            .items_center()
                                                            .gap(px(tokens::SPACE_1))
                                                            .child(
                                                                Button::new("open-folder")
                                                                    .ghost()
                                                                    .small()
                                                                    .icon(Icon::new(IconName::FolderOpen))
                                                                    .tooltip("Open Folder\u{2026} (cmd/ctrl+O)")
                                                                    .on_click(cx.listener(
                                                                        |this, _event, window, cx| {
                                                                            this.open_folder(window, cx);
                                                                        },
                                                                    )),
                                                            )
                                                            .child(
                                                                Button::new("rail-refresh")
                                                                    .ghost()
                                                                    .small()
                                                                    .icon(Icon::new(gpui_kit::assets::IconName::RefreshCw))
                                                                    .tooltip("Refresh file tree")
                                                                    .on_click(cx.listener(
                                                                        |this, _event, _window, cx| {
                                                                            let items = build_tree_items(&this.root);
                                                                            this.git_status = core_workspace::status_map(&this.root);
                                                                            this.tree_state.update(cx, |state, cx| {
                                                                                state.set_items(items, cx);
                                                                            });
                                                                            cx.notify();
                                                                        },
                                                                    )),
                                                            ),
                                                    ),
                                            )
                                            .child(
                                                tree(&self.tree_state, {
                                                    let view = view.clone();
                                                    // The file open in the
                                                    // editor is marked in the
                                                    // tree, so the sidebar and
                                                    // the editor always agree
                                                    // about where you are.
                                                    let active_path = self
                                                        .active_editor()
                                                        .read(cx)
                                                        .buffer()
                                                        .path()
                                                        .map(|path| {
                                                            path.to_string_lossy().to_string()
                                                        });
                                                    let git_status = self.git_status.clone();
                                                    move |ix, entry, _selected, _window, cx| {
                                                        let item = entry.item();
                                                        let item_path = PathBuf::from(item.id.as_str());
                                                        let icon = if !entry.is_folder() {
                                                            icon_for_file(&item.label)
                                                        } else if entry.is_expanded() {
                                                            Icon::new(IconName::FolderOpen)
                                                        } else {
                                                            Icon::new(IconName::Folder)
                                                        };
                                                        let icon_color = if entry.is_folder() {
                                                            cx.theme().muted_foreground
                                                        } else {
                                                            file_icon_color(&item.label).into()
                                                        };
                                                        let is_active_file =
                                                            active_path.as_deref()
                                                                == Some(item.id.as_str());

                                                        // A file shows its own
                                                        // git letter; a folder
                                                        // reads as modified
                                                        // when anything under
                                                        // it changed.
                                                        let badge = if entry.is_folder() {
                                                            git_status
                                                                .keys()
                                                                .any(|changed| {
                                                                    changed.starts_with(&item_path)
                                                                })
                                                                .then_some('M')
                                                        } else {
                                                            git_status.get(&item_path).copied()
                                                        };

                                                        ListItem::new(ix)
                                                            .w_full()
                                                            .h(px(tokens::TREE_ROW_H))
                                                            .rounded(px(tokens::RADIUS_SM))
                                                            .text_color(cx.theme().sidebar_foreground)
                                                            .px(px(tokens::SPACE_2))
                                                            .py_0()
                                                            .pl(px(16.) * entry.depth()
                                                                + px(tokens::SPACE_4))
                                                            .selected(is_active_file)
                                                            .when(is_active_file, |el| {
                                                                el.font_medium()
                                                                    .bg(tokens::accent_dim())
                                                            })
                                                            .child(
                                                                h_flex()
                                                                    .w_full()
                                                                    .items_center()
                                                                    .gap(px(tokens::SPACE_2))
                                                                    .child(
                                                                        icon.text_color(icon_color).small(),
                                                                    )
                                                                    .child(
                                                                        div()
                                                                            .flex_1()
                                                                            .min_w(px(0.))
                                                                            .truncate()
                                                                            .text_size(px(tokens::UI))
                                                                            .child(item.label.clone()),
                                                                    )
                                                                    .when_some(badge, |el, letter| {
                                                                        el.child(status_badge(letter, &*cx))
                                                                    }),
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
                            .child(editor_column)
                            .child(right_rail)
                            .into_any_element()
                    }
                }),
            )
            .child(self.render_status_bar(cx))
            .when_some(self.render_palette(&view, cx), |el, palette| el.child(palette))
            .when_some(self.render_rename_prompt(&view, cx), |el, prompt| el.child(prompt))
            .when_some(self.render_code_actions(&view, cx), |el, picker| el.child(picker))
    }
}

impl DartIde {
    /// The open-file tabs, in their own row above the editor — scoped
    /// to the editor column, not spanning the sidebar, and not merged
    /// into the titlebar either. The active tab is a filled chip on the
    /// tab-bar surface (the standard Zed/Fleet treatment), not a bright
    /// outline; inactive tabs stay transparent until hovered.
    fn render_tabs(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> impl IntoElement {
        let accent = cx.theme().primary;
        let muted = cx.theme().muted_foreground;
        let active_fg = cx.theme().tab_active_foreground;
        let inactive_fg = cx.theme().tab_foreground;

        h_flex()
            .w_full()
            .h(px(tokens::TABSTRIP_H))
            .flex_shrink_0()
            .items_center()
            .gap(px(tokens::SPACE_2))
            .px(px(tokens::SPACE_4))
            .bg(cx.theme().tab_bar)
            .border_b_1()
            .border_color(cx.theme().border)
            .overflow_hidden()
            .children(self.tabs.iter().enumerate().map(|(ix, tab)| {
                let is_active = ix == self.active_tab;
                let is_dirty = tab.editor.read(cx).buffer().is_dirty();
                let title = tab.title();

                h_flex()
                    .id(("tab", ix))
                    .h(px(30.))
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(tokens::SPACE_2))
                    .px(px(tokens::SPACE_4))
                    .cursor_pointer()
                    .rounded(px(tokens::RADIUS_PANEL))
                    .when(is_active, |el| {
                        el.bg(cx.theme().tab_active)
                            .border_1()
                            .border_color(cx.theme().border)
                    })
                    .when(!is_active, |el| {
                        el.hover(|s| s.bg(cx.theme().list_hover))
                    })
                    .text_color(if is_active { active_fg } else { inactive_fg })
                    .child(
                        icon_for_file(&title)
                            .text_color(file_icon_color(&title))
                            .small(),
                    )
                    .child(
                        div()
                            .id(("tab-label", ix))
                            .text_size(px(tokens::UI))
                            .child(title)
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
                    // Only the tab you can actually act on shows its
                    // affordance: a dirty tab keeps its dot, the active
                    // tab gets the close cross, and every other tab is
                    // left clean until you reach for it.
                    .when(is_dirty || is_active, |el| {
                        el.child(
                            div()
                                .id(("tab-close", ix))
                                .w(px(14.))
                                .text_center()
                                .text_color(if is_dirty { accent } else { muted })
                                .hover(move |s| s.text_color(active_fg))
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
                    })
            }))
            .child(
                div()
                    .id("new-tab")
                    .flex_shrink_0()
                    .cursor_pointer()
                    .p_1()
                    .rounded(px(tokens::RADIUS_SM))
                    .hover(|s| s.bg(cx.theme().list_hover))
                    .child(
                        Icon::new(gpui_kit::assets::IconName::Plus)
                            .text_color(cx.theme().muted_foreground)
                            .small(),
                    )
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.new_scratch_tab(window, cx);
                    })),
            )
    }

    /// Bottom dock of the center column. Phase 1 draws the card and its
    /// header only; the shell (Phase 5) supplies real scrollback.
    fn render_terminal(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .h(px(tokens::TERMINAL_H))
            .flex_shrink_0()
            .bg(cx.theme().secondary)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(tokens::RADIUS_CARD))
            .overflow_hidden()
            .child(
                h_flex()
                    .w_full()
                    .h(px(32.))
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .h(px(22.))
                            .rounded(px(tokens::RADIUS_SM))
                            .bg(cx.theme().background)
                            .text_size(px(tokens::UI_SM))
                            .text_color(cx.theme().foreground)
                            .child(div().text_color(cx.theme().primary).child(">_"))
                            .child("panes \u{2014} bash"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .p(px(tokens::SPACE_5))
                    .text_size(px(tokens::MONO))
                    .text_color(cx.theme().muted_foreground)
                    .child("Terminal dock \u{2014} Phase 5"),
            )
    }

    /// Right rail. Phase 1 draws the card and its tab header only; the agent
    /// conversation (Phase 6) fills the body.
    fn render_agent_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(tokens::RADIUS_CARD))
            .overflow_hidden()
            .child(
                h_flex()
                    .w_full()
                    .h(px(tokens::TABSTRIP_H))
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .items_center()
                            .gap_1()
                            .h(px(30.))
                            .px(px(tokens::SPACE_4))
                            .rounded(px(tokens::RADIUS_PANEL))
                            .bg(cx.theme().tab_active)
                            .border_1()
                            .border_color(tokens::accent_dim())
                            .text_size(px(tokens::UI))
                            .text_color(cx.theme().foreground)
                            .child(
                                Icon::new(gpui_kit::assets::IconName::Bot)
                                    .small()
                                    .text_color(cx.theme().primary),
                            )
                            .child("Demo Agent"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(tokens::UI_SM))
                    .text_color(cx.theme().muted_foreground)
                    .child("Agent panel \u{2014} Phase 6"),
            )
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = self.active_editor().read(cx);
        let buffer = editor.buffer();
        let path = match buffer.path() {
            Some(path) => breadcrumb_path(&self.root, path),
            None => "untitled".to_string(),
        };
        let is_dirty = buffer.is_dirty();
        let (line, column) = editor.cursor_position();
        let (errors, warnings) = editor.diagnostic_summary();
        let language = buffer.path().map(|p| language_label(&p));

        h_flex()
            .w_full()
            .h(px(26.))
            .flex_shrink_0()
            .px_3()
            .items_center()
            .justify_between()
            .bg(cx.theme().status_bar)
            .border_t_1()
            .border_color(cx.theme().status_bar_border)
            .text_size(px(tokens::UI_SM))
            .text_color(cx.theme().muted_foreground)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .when_some(self.git_branch.as_ref(), |el, branch| {
                        let repo_dirty = core_workspace::has_uncommitted_changes(&self.root);
                        let ahead_behind = core_workspace::ahead_behind(&self.root);
                        el.child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .text_color(cx.theme().green)
                                .child(Icon::new(gpui_kit::assets::IconName::GitBranch).small())
                                .child(branch.clone())
                                .when(repo_dirty, |el| {
                                    el.child(div().text_color(cx.theme().primary).child("\u{25cf}"))
                                })
                                .when_some(ahead_behind, |el, (ahead, behind)| {
                                    el.when(ahead > 0, |el| {
                                        el.child(format!("\u{2191}{ahead}"))
                                    })
                                    .when(behind > 0, |el| {
                                        el.child(format!("\u{2193}{behind}"))
                                    })
                                }),
                        )
                        .child(div().w(px(1.)).h(px(11.)).bg(cx.theme().border))
                    })
                    .when(is_dirty, |el| {
                        el.child(div().text_color(cx.theme().primary).child("\u{25cf}"))
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
                    .child(format!("{line}:{column}"))
                    .child("LF")
                    .when_some(language, |el, language| {
                        el.child(div().w(px(1.)).h(px(11.)).bg(cx.theme().border))
                            .child(language)
                    })
                    .child("UTF-8"),
            )
    }
}

fn main() {
    let app = gpui_kit::application().with_assets(AllAssets);

    app.run(move |cx| {
        // This must be called before using any GPUI Component features.
        gpui_kit::init(cx);
        // Dark by default — a code editor living in a bright-white chrome
        // looks like nothing in this space (Zed, Fleet, VS Code all default
        // dark), and `gpui-component`'s own default dark theme already sits
        // close to it (near-black backgrounds, a blue selection/accent).
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
        theme::apply(cx);

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
                title: None,
                // Draw our own titlebar instead of leaning on the native
                // one (the thing the earlier "incomplete titlebar" look
                // came from) — this is the actual Zed/Fleet approach, not
                // a styling preference: it renders identically everywhere
                // instead of depending on how this specific macOS version
                // happens to draw a default NSWindow titlebar. Safe to
                // combine with the fixed-size window above — the earlier
                // freeze was caused by the window's *bounds* matching the
                // display exactly, confirmed independent of this setting
                // (it also froze with the plain system titlebar).
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
