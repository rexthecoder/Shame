use std::path::{Path, PathBuf};

use gpui_kit::base::{StyledExt as _, Tree};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem},
    tree::TreeItem,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, IGNORED_NAMES, tokens};

pub(crate) fn build_tree_items(dir: &Path, show_hidden: bool) -> Vec<TreeItem> {
    to_tree_items(scan_tree(dir, show_hidden))
}

/// One file or folder from a disk scan. Unlike `TreeItem` (which holds
/// `Rc` state) this is `Send`, so the scan can run off the UI thread.
pub(crate) struct FsNode {
    id: String,
    name: String,
    /// `Some` for folders, even empty ones.
    children: Option<Vec<FsNode>>,
}

/// Walk `dir` (six levels deep, skipping ignored names and, unless
/// `show_hidden`, dotfiles), folders first, then by name.
pub(crate) fn scan_tree(dir: &Path, show_hidden: bool) -> Vec<FsNode> {
    scan_tree_bounded(dir, show_hidden, 0)
}

fn scan_tree_bounded(dir: &Path, show_hidden: bool, depth: usize) -> Vec<FsNode> {
    let mut nodes = Vec::new();
    if depth > 6 {
        return nodes;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return nodes;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == ".git" || IGNORED_NAMES.contains(&name) {
            continue;
        }
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let children = path.is_dir().then(|| scan_tree_bounded(&path, show_hidden, depth + 1));
        nodes.push(FsNode { id: path.to_string_lossy().to_string(), name: name.to_string(), children });
    }
    nodes.sort_by(|a, b| b.children.is_some().cmp(&a.children.is_some()).then_with(|| a.name.cmp(&b.name)));
    nodes
}

pub(crate) fn to_tree_items(nodes: Vec<FsNode>) -> Vec<TreeItem> {
    nodes
        .into_iter()
        .map(|node| match node.children {
            Some(children) => TreeItem::new(node.id, node.name).children(to_tree_items(children)),
            None => TreeItem::new(node.id, node.name),
        })
        .collect()
}

/// Tree id of the editable row shown while naming a new file or folder.
/// Not a path, so it can never collide with a real entry.
pub(crate) const NEW_ENTRY_ID: &str = "\u{0}shame-new-entry";

/// `items` with `child` inserted first under the folder `parent_id`, with
/// that folder and its ancestors opened so the new row is visible.
pub(crate) fn with_child_first(items: &[TreeItem], parent_id: &str, child: TreeItem) -> Vec<TreeItem> {
    let parent = Path::new(parent_id);
    items
        .iter()
        .map(|item| {
            if item.id.as_ref() == parent_id {
                let children = std::iter::once(child.clone()).chain(item.children.iter().cloned());
                TreeItem::new(item.id.clone(), item.label.clone()).children(children).expanded(true)
            } else if parent.starts_with(item.id.as_ref()) && !item.children.is_empty() {
                TreeItem::new(item.id.clone(), item.label.clone())
                    .children(with_child_first(&item.children, parent_id, child.clone()))
                    .expanded(true)
            } else {
                item.clone()
            }
        })
        .collect()
}

/// Ids of every expanded folder in `items`, including ones inside
/// collapsed parents (they reopen as they were when the parent does).
fn expanded_ids(items: &[TreeItem], out: &mut std::collections::HashSet<SharedString>) {
    for item in items {
        // Not `is_folder()`: GPUI counts an empty folder as a leaf.
        if item.is_expanded() {
            out.insert(item.id.clone());
        }
        expanded_ids(&item.children, out);
    }
}

fn restore_expanded(items: &[TreeItem], ids: &std::collections::HashSet<SharedString>) {
    for item in items {
        if ids.contains(&item.id) {
            // Clones share expansion state, so this marks the item itself.
            let _ = item.clone().expanded(true);
        }
        restore_expanded(&item.children, ids);
    }
}

fn collapse_all(items: &[TreeItem]) {
    for item in items {
        let _ = item.clone().expanded(false);
        collapse_all(&item.children);
    }
}

/// The tree's top-level items (shared with the state, not copies).
fn root_items(state: &gpui_kit::component::tree::TreeState) -> Vec<TreeItem> {
    (0..)
        .map_while(|ix| state.entry(ix))
        .filter(|entry| entry.depth() == 0)
        .map(|entry| entry.item().clone())
        .collect()
}

impl DartIde {
    /// Replace the file tree's items, keeping open folders open and the
    /// selected row selected (a plain `set_items` collapses everything).
    pub(crate) fn set_tree_items(&mut self, items: Vec<TreeItem>, cx: &mut Context<Self>) {
        self.tree_state.update(cx, |state, cx| {
            let mut open = std::collections::HashSet::new();
            expanded_ids(&root_items(state), &mut open);
            let selected = state.selected_item().map(|item| item.id.clone());
            restore_expanded(&items, &open);
            state.set_items(items, cx);
            if let Some(ix) = selected.and_then(|id| state.index_of(&id)) {
                state.set_selected_index(Some(ix), cx);
            }
        });
    }

    /// Collapse every folder in place: no disk scan, so it's instant.
    pub(crate) fn collapse_file_tree(&mut self, cx: &mut Context<Self>) {
        self.tree_state.update(cx, |state, cx| {
            let roots = root_items(state);
            collapse_all(&roots);
            state.set_items(roots, cx);
        });
    }

    /// Rescan the tree on a background thread (e.g. after toggling
    /// dotfiles) and swap it in, keeping open folders.
    pub(crate) fn rescan_file_tree(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let show_hidden = self.show_hidden;
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(scan_tree(&root, show_hidden));
        });
        cx.spawn(async move |weak, cx| {
            let Ok(nodes) = rx.recv().await else {
                return;
            };
            let _ = weak.update(cx, |this, cx| {
                // Toggled again while scanning: that scan's result wins.
                if this.show_hidden != show_hidden {
                    return;
                }
                this.set_tree_items(to_tree_items(nodes), cx);
                cx.notify();
            });
        })
        .detach();
    }
}

pub(crate) fn icon_for_file(name: &str) -> Icon {
    match name {
        ".gitignore" | ".metadata" | ".gitattributes" => {
            return Icon::new(gpui_kit::assets::IconName::GitBranch);
        }
        "CHANGELOG.md" => {
            return Icon::new(gpui_kit::assets::IconName::RotateCcwClock);
        }
        "LICENSE" | "LICENSE.md" | "LICENSE.txt" => {
            return Icon::new(gpui_kit::assets::IconName::Scale);
        }
        _ => {}
    }

    match Path::new(name).extension().and_then(|e| e.to_str()) {
        Some("rs" | "dart" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "c" | "cpp" | "h") => {
            Icon::new(gpui_kit::assets::IconName::FileCode)
        }
        Some("yaml" | "yml" | "toml") => Icon::new(gpui_kit::assets::IconName::Settings),
        Some("lock") => Icon::new(gpui_kit::assets::IconName::Lock),
        Some("md") => Icon::new(gpui_kit::assets::IconName::BookOpen),
        _ => Icon::new(IconName::File),
    }
}

pub(crate) fn file_icon_color(name: &str) -> Rgba {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with("_test.dart")
        || lower.ends_with("_test.rs")
        || lower.ends_with(".test.ts")
        || lower.ends_with(".spec.ts")
    {
        return rgb(0x3fb950);
    }
    if lower.ends_with(".g.dart") || lower.ends_with(".freezed.dart") || lower.ends_with(".gr.dart")
    {
        return rgb(0xc678dd);
    }
    match name {
        ".gitignore" | ".metadata" | ".gitattributes" | ".gitmodules" => return rgb(0xf05032),
        "LICENSE" | "LICENSE.md" | "LICENSE.txt" => return rgb(0xe5c07b),
        "CHANGELOG.md" => return rgb(0x56b6c2),
        "pubspec.yaml" | "pubspec.lock" => return rgb(0x38bdf8),
        "Cargo.toml" | "Cargo.lock" => return rgb(0xe07b53),
        "Podfile" | "Podfile.lock" | "Info.plist" => return rgb(0xf05138),
        _ => {}
    }
    match Path::new(name).extension().and_then(|e| e.to_str()) {
        Some("rs") => rgb(0xdea584),
        Some("dart") => rgb(0x29b6f6),
        Some("ts") => rgb(0x3178c6),
        Some("tsx" | "jsx") => rgb(0x61dafb),
        Some("js") => rgb(0xf0db4f),
        Some("json" | "arb") => rgb(0xcbcb41),
        Some("toml") => rgb(0xe07b53),
        Some("yaml" | "yml") => rgb(0xf26d7d),
        Some("md" | "mdx") => rgb(0x61afef),
        Some("swift" | "plist") => rgb(0xf05138),
        Some("kt" | "kts") => rgb(0xa97bff),
        Some("gradle") => rgb(0x3ddc84),
        Some("py" | "pyi") => rgb(0x4b8bbe),
        Some("go") => rgb(0x00add8),
        Some("java") => rgb(0xf89820),
        Some("rb" | "gemspec" | "rake") => rgb(0xcc342d),
        Some("zig") => rgb(0xf7a41d),
        Some("cs") => rgb(0x68217a),
        Some("php") => rgb(0x777bb4),
        Some("lua") => rgb(0x51a0cf),
        Some("c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "m" | "mm") => rgb(0x659ad2),
        Some("html" | "htm") => rgb(0xe34c26),
        Some("css" | "scss" | "less") => rgb(0x61afef),
        Some("sh" | "zsh" | "bash") => rgb(0x4eaa25),
        Some("sql") => rgb(0xe38c00),
        Some("png" | "jpg" | "jpeg" | "webp" | "svg" | "ico") => rgb(0xc678dd),
        Some("lock") => rgb(0xd19a66),
        _ => rgb(0x9aa0a6),
    }
}

/// Distinct 16×16 rounded-square file badge with a language monogram or
/// semantic vector glyph so `.dart`, `*_test.dart`, `*.g.dart`, `.rs`,
/// `pubspec.yaml`, `Cargo.toml`, `.json`, `.md`, etc. are immediately
/// distinguishable at a glance.
pub(crate) fn render_file_icon(name: &str) -> Div {
    render_file_icon_sized(name, 16.)
}

/// The same badge at another size (`16.` is the file tree's). Glyphs and
/// monogram text scale with it, so a small badge can sit inline with text.
pub(crate) fn render_file_icon_sized(name: &str, size: f32) -> Div {
    let k = size / 16.;
    let color: Hsla = file_icon_color(name).into();
    let lower = name.to_ascii_lowercase();

    let badge_base = div()
        .flex_shrink_0()
        .size(px(size))
        .rounded(px(4. * k))
        .bg(color.opacity(0.15))
        .border_1()
        .border_color(color.opacity(0.36))
        .flex()
        .items_center()
        .justify_center();

    // Special vector-icon files
    match name {
        ".gitignore" | ".metadata" | ".gitattributes" | ".gitmodules" => {
            return badge_base.child(
                Icon::new(gpui_kit::assets::IconName::GitBranch)
                    .with_size(px(10.5 * k))
                    .text_color(color),
            );
        }
        "CHANGELOG.md" => {
            return badge_base.child(
                Icon::new(gpui_kit::assets::IconName::RotateCcwClock)
                    .with_size(px(10.5 * k))
                    .text_color(color),
            );
        }
        "LICENSE" | "LICENSE.md" | "LICENSE.txt" => {
            return badge_base.child(
                Icon::new(gpui_kit::assets::IconName::Scale)
                    .with_size(px(10.5 * k))
                    .text_color(color),
            );
        }
        _ => {}
    }

    let monogram: Option<&'static str> = if lower.ends_with("_test.dart")
        || lower.ends_with("_test.rs")
        || lower.ends_with(".test.ts")
        || lower.ends_with(".spec.ts")
    {
        Some("T")
    } else if lower.ends_with(".g.dart")
        || lower.ends_with(".freezed.dart")
        || lower.ends_with(".gr.dart")
    {
        Some("G")
    } else if lower == "pubspec.yaml" {
        Some("P")
    } else if lower == "cargo.toml" {
        Some("C")
    } else if lower == "podfile" {
        Some("Sw")
    } else {
        match Path::new(name).extension().and_then(|e| e.to_str()) {
            Some("dart") => Some("D"),
            Some("rs") => Some("Rs"),
            Some("ts") => Some("TS"),
            Some("tsx") => Some("TX"),
            Some("js") => Some("JS"),
            Some("jsx") => Some("JX"),
            Some("json" | "arb") => Some("{}"),
            Some("yaml" | "yml") => Some("Y"),
            Some("toml") => Some("T"),
            Some("swift") => Some("Sw"),
            Some("kt" | "kts") => Some("Kt"),
            Some("gradle") => Some("Gr"),
            Some("py" | "pyi") => Some("Py"),
            Some("go") => Some("Go"),
            Some("java") => Some("Jv"),
            Some("rb" | "gemspec" | "rake") => Some("Rb"),
            Some("zig") => Some("Zg"),
            Some("cs") => Some("C#"),
            Some("php") => Some("Ph"),
            Some("lua") => Some("Lu"),
            Some("c") => Some("C"),
            Some("cpp" | "cc" | "cxx") => Some("C+"),
            Some("h" | "hpp") => Some("H"),
            Some("m" | "mm") => Some("Ob"),
            Some("html" | "htm") => Some("<>"),
            Some("css" | "scss" | "less") => Some("#"),
            Some("sh" | "zsh" | "bash") => Some("$"),
            Some("sql") => Some("DB"),
            Some("png" | "jpg" | "jpeg" | "webp" | "svg" | "ico") => Some("Im"),
            _ => None,
        }
    };

    if let Some(code) = monogram {
        let font_sz = (if code.len() > 1 { 8.0 } else { 9.5 }) * k;
        return badge_base.child(
            div()
                .font_family("SF Mono")
                .font_semibold()
                .text_size(px(font_sz))
                .line_height(px(10. * k))
                .text_color(color)
                .child(code),
        );
    }

    let fallback_icon = match Path::new(name).extension().and_then(|e| e.to_str()) {
        Some("md" | "mdx") => gpui_kit::assets::IconName::BookOpen,
        Some("lock") => gpui_kit::assets::IconName::Lock,
        Some("ini" | "env" | "plist") => gpui_kit::assets::IconName::Settings,
        _ => IconName::File.into(),
    };

    badge_base.child(
        Icon::new(fallback_icon)
            .with_size(px(10.5 * k))
            .text_color(color),
    )
}

pub(crate) fn folder_icon_color(name: &str, cx: &App) -> Hsla {
    if name.starts_with('.') {
        return cx.theme().muted_foreground.opacity(0.65);
    }
    match name {
        "lib" | "src" => rgb(0xea7a3c).into(),
        "ios" | "macos" | "Runner" => rgb(0x4f9cf9).into(),
        "android" => rgb(0x3ddc84).into(),
        "test" | "tests" | "integration_test" => rgb(0x3fb950).into(),
        "assets" | "images" | "fonts" | "icons" => rgb(0xc678dd).into(),
        "crates" | "packages" | "plugins" => rgb(0xe5c07b).into(),
        "web" | "build" | "target" | "scripts" => rgb(0x56b6c2).into(),
        _ => rgb(0xd4a373).into(),
    }
}

/// The small git-status chip on a file-tree row: a single letter on a
/// tinted square, matching the reference tree (`M` amber, `U` green).
fn status_badge(letter: char, cx: &App) -> Div {
    let color = match letter {
        'M' => cx.theme().warning,
        'A' | 'U' => cx.theme().success,
        'D' => cx.theme().danger,
        'R' => cx.theme().info,
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

/// The tree row while a name is being typed: same indent and icon as a
/// real row, the label swapped for an input, and the reason a name can't be
/// used shown just under it (VS Code's inline create/rename).
#[allow(clippy::too_many_arguments)]
fn render_tree_edit_row(
    depth: usize,
    is_new: bool,
    is_folder: bool,
    is_expanded: bool,
    label: SharedString,
    input: &Entity<InputState>,
    error: Option<String>,
    view: Entity<DartIde>,
    cx: &mut App,
) -> AnyElement {
    let typed = input.read(cx).value().to_string();
    let icon_name = if is_new { typed.trim().to_string() } else { label.to_string() };
    let icon: AnyElement = if is_folder {
        Icon::new(if is_expanded && !is_new { IconName::FolderOpen } else { IconName::Folder })
            .text_color(folder_icon_color(&icon_name, cx))
            .small()
            .into_any_element()
    } else {
        // Follows the name as it's typed, like VS Code's icon.
        render_file_icon(if icon_name.is_empty() { "file" } else { &icon_name }).into_any_element()
    };
    let accent = if error.is_some() { cx.theme().danger } else { cx.theme().primary };
    let indent = depth as f32 * 16. + tokens::SPACE_3;
    let cancel_view = view.clone();

    div()
        .w_full()
        .h(px(tokens::TREE_ROW_H))
        .px(px(tokens::SPACE_3))
        .flex()
        .items_center()
        .child(
            h_flex()
                .relative()
                .w_full()
                .h(px(26.))
                .pl(px(indent))
                .pr(px(tokens::SPACE_1))
                .items_center()
                .gap(px(tokens::SPACE_2))
                .child(div().w(px(12.)).flex_shrink_0())
                .child(icon)
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .h(px(22.))
                        .px(px(tokens::SPACE_1))
                        .flex()
                        .items_center()
                        .rounded(px(tokens::RADIUS_BADGE))
                        .border_1()
                        .border_color(accent)
                        .bg(cx.theme().background)
                        .text_size(px(tokens::UI))
                        // Keep the tree from treating clicks in the box as row clicks.
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_key_down(move |event: &KeyDownEvent, _window, cx| {
                            if event.keystroke.key == "escape" {
                                cx.stop_propagation();
                                cancel_view.update(cx, |this, cx| this.cancel_file_tree_prompt(cx));
                            }
                        })
                        .child(Input::new(input).appearance(false).xsmall()),
                )
                .when_some(error, |el, error| {
                    el.child(
                        deferred(
                            div()
                                .absolute()
                                .top(px(25.))
                                .left(px(indent + 12. + 16. + tokens::SPACE_2 * 2.))
                                .right(px(tokens::SPACE_1))
                                .px(px(tokens::SPACE_3))
                                .py(px(tokens::SPACE_2))
                                .rounded_b(px(tokens::RADIUS_BADGE))
                                .bg(cx.theme().background)
                                .border_1()
                                .border_color(cx.theme().danger)
                                .text_size(px(tokens::UI_SM))
                                .text_color(cx.theme().foreground)
                                .child(error),
                        )
                        .with_priority(2),
                    )
                }),
        )
        .into_any_element()
}

/// Right-click menu on a tree row, as in VS Code's explorer.
fn tree_context_menu(menu: PopupMenu, path: &Path, root: &Path, view: Entity<DartIde>) -> PopupMenu {
    let dir = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().map(Path::to_path_buf).unwrap_or_else(|| root.to_path_buf())
    };
    // Run after the menu closes, so its focus change can't end the edit.
    let later = |view: Entity<DartIde>, op: crate::FileTreeOp| {
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            let view = view.clone();
            let op = op.clone();
            window.defer(cx, move |window, cx| {
                view.update(cx, |this, cx| this.open_file_tree_prompt(op, window, cx));
            });
        }
    };
    let abs = path.to_string_lossy().to_string();
    let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().to_string();
    let reveal = path.to_path_buf();
    let del_path = path.to_path_buf();
    let del_view = view.clone();
    menu.item(
        PopupMenuItem::new("New File\u{2026}")
            .on_click(later(view.clone(), crate::FileTreeOp::NewFile { parent: dir.clone() })),
    )
    .item(
        PopupMenuItem::new("New Folder\u{2026}")
            .on_click(later(view.clone(), crate::FileTreeOp::NewFolder { parent: dir })),
    )
    .separator()
    .item(PopupMenuItem::new("Reveal in Finder").on_click(move |_, _, _| {
        let _ = std::process::Command::new("open").arg("-R").arg(&reveal).spawn();
    }))
    .item(PopupMenuItem::new("Copy Path").on_click(move |_, _, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(abs.clone()));
    }))
    .item(PopupMenuItem::new("Copy Relative Path").on_click(move |_, _, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(rel.clone()));
    }))
    .separator()
    .item(
        PopupMenuItem::new("Rename\u{2026}")
            .on_click(later(view, crate::FileTreeOp::Rename(path.to_path_buf()))),
    )
    .item(PopupMenuItem::new("Delete").on_click(move |_, _, cx| {
        let p = del_path.clone();
        del_view.update(cx, |this, cx| this.delete_tree_path_clicked(p, cx));
    }))
}

impl DartIde {
    pub(crate) fn render_files_rail(
        &self,
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let root_name = self
            .root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("/")
            .to_string();

        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let border = cx.theme().border;

        let tree_empty = self.tree_state.read(cx).entry(0).is_none();
        let empty = v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(tokens::SPACE_2))
            .child(
                Icon::new(gpui_kit::assets::IconName::FolderOpen)
                    .with_size(px(18.))
                    .text_color(muted.opacity(0.5)),
            )
            .child(
                div()
                    .text_size(px(tokens::UI_SM))
                    .text_color(muted)
                    .child("No files in this folder"),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap(px(tokens::SPACE_3))
                    .pt(px(tokens::SPACE_2))
                    .child(
                        div()
                            .id("files-empty-new-file")
                            .cursor_pointer()
                            .text_size(px(tokens::UI_SM))
                            .text_color(cx.theme().primary)
                            .hover(move |s| s.text_color(fg))
                            .child("New file\u{2026}")
                            .on_click(cx.listener(|this, _event, window, cx| {
                                let parent = this.root.clone();
                                this.open_file_tree_prompt(
                                    crate::FileTreeOp::NewFile { parent },
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .child(div().text_color(muted.opacity(0.4)).child("\u{b7}"))
                    .child(
                        div()
                            .id("files-empty-new-folder")
                            .cursor_pointer()
                            .text_size(px(tokens::UI_SM))
                            .text_color(muted)
                            .hover(move |s| s.text_color(fg))
                            .child("New folder\u{2026}")
                            .on_click(cx.listener(|this, _event, window, cx| {
                                let parent = this.root.clone();
                                this.open_file_tree_prompt(
                                    crate::FileTreeOp::NewFolder { parent },
                                    window,
                                    cx,
                                );
                            })),
                    ),
            );
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .w_full()
                    .h(px(tokens::PANEL_BAR_H))
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .gap(px(tokens::SPACE_2))
                    .px(px(tokens::PANEL_INSET))
                    .border_b_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w(px(0.))
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_size(px(tokens::UI_SM))
                                    .font_medium()
                                    .text_color(fg)
                                    .child(root_name),
                            )
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap(px(tokens::SPACE_1))
                            .child(
                                Button::new("rail-new-file")
                                    .ghost()
                                    .small()
                                    .icon(
                                        Icon::new(gpui_kit::assets::IconName::FilePlus)
                                            .with_size(px(12.))
                                            .text_color(muted),
                                    )
                                    .tooltip("New File\u{2026}")
                                    .on_click(cx.listener(|this, _event, window, cx| {
                                        let parent = this.file_tree_target_dir(cx);
                                        this.open_file_tree_prompt(
                                            crate::FileTreeOp::NewFile { parent },
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                Button::new("rail-new-folder")
                                    .ghost()
                                    .small()
                                    .icon(
                                        Icon::new(gpui_kit::assets::IconName::FolderPlus)
                                            .with_size(px(12.))
                                            .text_color(muted),
                                    )
                                    .tooltip("New Folder\u{2026}")
                                    .on_click(cx.listener(|this, _event, window, cx| {
                                        let parent = this.file_tree_target_dir(cx);
                                        this.open_file_tree_prompt(
                                            crate::FileTreeOp::NewFolder { parent },
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                Button::new("rail-collapse")
                                    .ghost()
                                    .small()
                                    .icon(
                                        Icon::new(gpui_kit::assets::IconName::CopyMinus)
                                            .with_size(px(12.))
                                            .text_color(muted),
                                    )
                                    .tooltip("Collapse all folders")
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.collapse_file_tree(cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("rail-toggle-hidden")
                                    .ghost()
                                    .small()
                                    .icon(
                                        Icon::new(if self.show_hidden {
                                            gpui_kit::assets::IconName::EyeOff
                                        } else {
                                            gpui_kit::assets::IconName::Eye
                                        })
                                        .with_size(px(12.))
                                        .text_color(muted),
                                    )
                                    .tooltip(if self.show_hidden {
                                        "Hide dotfiles"
                                    } else {
                                        "Show dotfiles"
                                    })
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.show_hidden = !this.show_hidden;
                                        this.rescan_file_tree(cx);
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if this.file_tree_op.is_some() {
                            return;
                        }
                        let Some(path) = this
                            .tree_state
                            .read(cx)
                            .selected_item()
                            .map(|item| PathBuf::from(item.id.as_str()))
                            .filter(|p| p.starts_with(&this.root) && *p != this.root)
                        else {
                            return;
                        };
                        let ks = &event.keystroke;
                        if ks.key == "f2" {
                            this.open_file_tree_prompt(crate::FileTreeOp::Rename(path), window, cx);
                            cx.stop_propagation();
                        } else if ks.key == "backspace" && ks.modifiers.platform {
                            this.delete_tree_path_clicked(path, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .when(tree_empty, |el| el.child(empty))
                    .when(!tree_empty, |el| {
                        el.child(
                            Tree::new(&self.tree_state)
                            .item({
                                let view = view.clone();
                                let active_path = self
                                    .active_editor()
                                    .and_then(|ed| ed.read(cx).buffer().path())
                                    .map(|path| path.to_string_lossy().to_string());
                                let mut dirty_folders = std::collections::HashSet::new();
                                for changed in self.git_status.keys() {
                                    for anc in changed.ancestors().skip(1) {
                                        if !anc.starts_with(&self.root) {
                                            break;
                                        }
                                        if !dirty_folders.insert(anc.to_path_buf()) {
                                            break;
                                        }
                                    }
                                }
                                let dirty_folders = std::rc::Rc::new(dirty_folders);
                                let git_status = std::rc::Rc::new(self.git_status.clone());
                                let confirm_del = self.git.confirm_discard.clone();
                                // The row being named inline, if any (VS Code style).
                                let editing_id: Option<SharedString> = match &self.file_tree_op {
                                    Some(crate::FileTreeOp::Rename(p)) => {
                                        Some(p.to_string_lossy().to_string().into())
                                    }
                                    Some(_) => Some(NEW_ENTRY_ID.into()),
                                    None => None,
                                };
                                let new_is_folder = matches!(
                                    self.file_tree_op,
                                    Some(crate::FileTreeOp::NewFolder { .. })
                                );
                                let edit_input = self.file_tree_input.clone();
                                let edit_error = self.file_tree_error.clone();
                                let root = self.root.clone();
                                move |ix, entry, _entry_state, _window, cx| {
                                    let item = entry.item();
                                    let item_path = PathBuf::from(item.id.as_str());
                                    let is_folder = entry.is_folder();
                                    let depth = entry.depth();
                                    if editing_id.as_ref() == Some(&item.id) {
                                        let is_new = item.id.as_ref() == NEW_ENTRY_ID;
                                        return render_tree_edit_row(
                                            depth,
                                            is_new,
                                            if is_new { new_is_folder } else { item_path.is_dir() },
                                            entry.is_expanded(),
                                            item.label.clone(),
                                            &edit_input,
                                            edit_error.clone(),
                                            view.clone(),
                                            cx,
                                        );
                                    }
                                    let row_group = format!("tree-row-grp-{ix}");
                                    let del_key = format!("tree-del:{}", item.id);
                                    let del_armed = confirm_del.as_deref() == Some(del_key.as_str());
                                    let icon_el: AnyElement = if !is_folder {
                                        render_file_icon(&item.label).into_any_element()
                                    } else {
                                        let folder_color = folder_icon_color(&item.label, cx);
                                        let folder_icon = if entry.is_expanded() {
                                            Icon::new(IconName::FolderOpen)
                                        } else {
                                            Icon::new(IconName::Folder)
                                        };
                                        folder_icon
                                            .text_color(folder_color)
                                            .small()
                                            .into_any_element()
                                    };
                                    let is_active_file =
                                        active_path.as_deref() == Some(item.id.as_str());

                                    let badge = if is_folder {
                                        dirty_folders.contains(&item_path).then_some('M')
                                    } else {
                                        git_status.get(&item_path).copied()
                                    };

                                    // Subtle vertical indent guides for nested levels
                                    let guides: Vec<AnyElement> = (0..depth)
                                        .map(|lvl| {
                                            div()
                                                .absolute()
                                                .top_0()
                                                .bottom_0()
                                                .left(px(14. + lvl as f32 * 16.))
                                                .w(px(1.))
                                                .bg(cx.theme().border.opacity(0.45))
                                                .into_any_element()
                                        })
                                        .collect();

                                    let fg = cx.theme().foreground;
                                    let row_fg = if is_active_file {
                                        fg
                                    } else {
                                        cx.theme().sidebar_foreground
                                    };

                                    div()
                                        .w_full()
                                        .h(px(tokens::TREE_ROW_H))
                                        .px(px(tokens::SPACE_3))
                                        .flex()
                                        .items_center()
                                        .child(
                                            h_flex()
                                                .id(("tree-row", ix))
                                                .group(row_group.clone())
                                                .relative()
                                                .w_full()
                                                .h(px(26.))
                                                .px(px(tokens::SPACE_3))
                                                .pl(px(depth as f32 * 16. + tokens::SPACE_3))
                                                .items_center()
                                                .gap(px(tokens::SPACE_2))
                                                .rounded(px(tokens::RADIUS_SM))
                                                .border_1()
                                                .border_color(gpui_kit::transparent_black())
                                                .cursor_pointer()
                                                .text_color(row_fg)
                                                .children(guides)
                                                .when(is_active_file, |el| {
                                                    el.bg(rgba(0xea7a3c16))
                                                        .border_color(rgba(0xea7a3c36))
                                                        .child(
                                                            div()
                                                                .absolute()
                                                                .left(px(2.))
                                                                .top(px(5.))
                                                                .bottom(px(5.))
                                                                .w(px(2.5))
                                                                .rounded(px(2.))
                                                                .bg(cx.theme().primary),
                                                        )
                                                })
                                                .when(!is_active_file, |el| {
                                                    el.hover(move |s| {
                                                        s.bg(rgba(0xffffff0c)).text_color(fg)
                                                    })
                                                })
                                                .child(
                                                    div()
                                                        .w(px(12.))
                                                        .flex_shrink_0()
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .when(is_folder, |el| {
                                                            el.child(
                                                                Icon::new(if entry.is_expanded() {
                                                                    gpui_kit::assets::IconName::ChevronDown
                                                                } else {
                                                                    gpui_kit::assets::IconName::ChevronRight
                                                                })
                                                                .xsmall()
                                                                .text_color(cx.theme().muted_foreground),
                                                            )
                                                        }),
                                                )
                                                .child(icon_el)
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w(px(0.))
                                                        .truncate()
                                                        .text_size(px(tokens::UI))
                                                        .child(item.label.clone()),
                                                )
                                                // Delete asks once, in the row (right-click
                                                // → Delete or ⌘⌫ arms it).
                                                .when(del_armed, |el| {
                                                    let view_del = view.clone();
                                                    let del_path = item_path.clone();
                                                    el.child(
                                                        div()
                                                            .id(("tree-delete", ix))
                                                            .flex_shrink_0()
                                                            .px(px(tokens::SPACE_2))
                                                            .rounded(px(tokens::RADIUS_BADGE))
                                                            .bg(cx.theme().danger.opacity(0.16))
                                                            .text_size(px(tokens::UI_XS))
                                                            .text_color(cx.theme().danger)
                                                            .child("Delete?")
                                                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                                cx.stop_propagation()
                                                            })
                                                            .on_click(move |_e, _w, cx| {
                                                                cx.stop_propagation();
                                                                let p = del_path.clone();
                                                                view_del.update(cx, |this, cx| {
                                                                    this.delete_tree_path_clicked(p, cx);
                                                                });
                                                            }),
                                                    )
                                                })
                                                .when_some(badge, |el, letter| {
                                                    el.child(status_badge(letter, cx))
                                                })
                                                .when(!is_folder, |el| {
                                                    let view = view.clone();
                                                    let path = PathBuf::from(item.id.as_str());
                                                    el.on_click(move |event, window, cx| {
                                                        let as_preview = event.click_count() < 2;
                                                        view.update(cx, |this, cx| {
                                                            this.open_path_with_mode(
                                                                path.clone(),
                                                                as_preview,
                                                                window,
                                                                cx,
                                                            );
                                                        });
                                                    })
                                                }),
                                        )
                                        .context_menu({
                                            let view = view.clone();
                                            let path = item_path.clone();
                                            let root = root.clone();
                                            move |menu, _window, _cx| {
                                                tree_context_menu(menu, &path, &root, view.clone())
                                            }
                                        })
                                        .into_any_element()
                                }
                            })
                                .list_style(
                                    StyleRefinement::default().flex_grow_1().size_full(),
                                )
                                .relative()
                                .size_full(),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tree_state_tests {
    use super::*;

    fn folder(id: &str, children: Vec<TreeItem>) -> TreeItem {
        TreeItem::new(id.to_string(), id.to_string()).children(children)
    }

    fn file(id: &str) -> TreeItem {
        TreeItem::new(id.to_string(), id.to_string())
    }

    #[core::prelude::v1::test]
    fn open_folders_survive_a_rebuild_even_inside_collapsed_parents() {
        let old = vec![folder("lib", vec![folder("lib/src", vec![file("lib/src/a.dart")])]), folder("test", vec![])];
        // `lib/src` is open but `lib` itself is closed; `test` is open.
        let _ = old[0].children[0].clone().expanded(true);
        let _ = old[1].clone().expanded(true);

        let mut open = std::collections::HashSet::new();
        expanded_ids(&old, &mut open);
        let fresh = vec![folder("lib", vec![folder("lib/src", vec![file("lib/src/a.dart")])]), folder("test", vec![])];
        restore_expanded(&fresh, &open);

        assert!(!fresh[0].is_expanded());
        assert!(fresh[0].children[0].is_expanded());
        assert!(fresh[1].is_expanded());
    }

    #[core::prelude::v1::test]
    fn new_entry_row_goes_first_in_its_folder_and_opens_the_path() {
        let items = vec![
            folder("/w/lib", vec![folder("/w/lib/src", vec![file("/w/lib/src/a.dart")])]),
            folder("/w/lib2", vec![file("/w/lib2/b.dart")]),
        ];
        let out = with_child_first(&items, "/w/lib/src", file(NEW_ENTRY_ID));
        assert!(out[0].is_expanded());
        let src = &out[0].children[0];
        assert!(src.is_expanded());
        assert_eq!(src.children[0].id.as_ref(), NEW_ENTRY_ID);
        assert_eq!(src.children[1].id.as_ref(), "/w/lib/src/a.dart");
        // `/w/lib2` merely shares a string prefix with `/w/lib`: untouched.
        assert!(!out[1].is_expanded());
        assert_eq!(out[1].children.len(), 1);
    }

    #[core::prelude::v1::test]
    fn collapse_all_closes_every_level() {
        let items = vec![folder("lib", vec![folder("lib/src", vec![])])];
        let _ = items[0].clone().expanded(true);
        let _ = items[0].children[0].clone().expanded(true);
        collapse_all(&items);
        assert!(!items[0].is_expanded());
        assert!(!items[0].children[0].is_expanded());
    }

    #[core::prelude::v1::test]
    fn scan_lists_folders_first_and_respects_dotfiles() {
        let dir = std::env::temp_dir().join(format!("shame-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib/main.dart"), "").unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        std::fs::write(dir.join(".env"), "").unwrap();

        let names = |nodes: &[FsNode]| nodes.iter().map(|n| n.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&scan_tree(&dir, false)), vec!["lib", "a.txt"]);
        assert_eq!(names(&scan_tree(&dir, true)), vec!["lib", ".env", "a.txt"]);
        let items = to_tree_items(scan_tree(&dir, false));
        assert!(items[0].is_folder());
        assert_eq!(items[0].children[0].label.as_ref(), "main.dart");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
