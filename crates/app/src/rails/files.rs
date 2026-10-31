use std::path::{Path, PathBuf};

use gpui_kit::base::{StyledExt as _, Tree};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::Input,
    tree::TreeItem,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, IGNORED_NAMES, tokens};

pub(crate) fn build_tree_items(dir: &Path, show_hidden: bool) -> Vec<TreeItem> {
    build_tree_items_bounded(dir, show_hidden, 0)
}

fn build_tree_items_bounded(dir: &Path, show_hidden: bool, depth: usize) -> Vec<TreeItem> {
    let mut items = Vec::new();
    if depth > 6 {
        return items;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return items;
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

        let id = path.to_string_lossy().to_string();
        if path.is_dir() {
            let children = build_tree_items_bounded(&path, show_hidden, depth + 1);
            items.push(TreeItem::new(id, name.to_string()).children(children));
        } else {
            items.push(TreeItem::new(id, name.to_string()));
        }
    }

    items.sort_by(|a, b| b.is_folder().cmp(&a.is_folder()).then(a.label.cmp(&b.label)));
    items
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

        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(tokens::RADIUS_CARD))
            .overflow_hidden()
            .child(self.render_sidebar_tabs(cx))
            .child(
                h_flex()
                    .w_full()
                    .h(px(30.))
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
                                    .text_color(muted)
                                    .child(root_name),
                            )
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(tokens::SPACE_1))
                            .child(
                                Button::new("rail-new-file")
                                    .ghost()
                                    .small()
                                    .icon(
                                        Icon::new(gpui_kit::assets::IconName::FilePlus)
                                            .xsmall()
                                            .text_color(muted),
                                    )
                                    .tooltip("New File on Disk\u{2026}")
                                    .on_click(cx.listener(|this, _event, window, cx| {
                                        this.open_file_tree_prompt(
                                            crate::FileTreeOp::NewFile,
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
                                            .xsmall()
                                            .text_color(muted),
                                    )
                                    .tooltip("New Folder on Disk\u{2026}")
                                    .on_click(cx.listener(|this, _event, window, cx| {
                                        this.open_file_tree_prompt(
                                            crate::FileTreeOp::NewFolder,
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
                                            .xsmall()
                                            .text_color(muted),
                                    )
                                    .tooltip("Collapse all folders")
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        let items = build_tree_items(&this.root, this.show_hidden);
                                        this.tree_state.update(cx, |state, cx| {
                                            state.set_items(items, cx);
                                        });
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
                                        .xsmall()
                                        .text_color(muted),
                                    )
                                    .tooltip(if self.show_hidden {
                                        "Hide dotfiles"
                                    } else {
                                        "Show dotfiles"
                                    })
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.show_hidden = !this.show_hidden;
                                        let items = build_tree_items(&this.root, this.show_hidden);
                                        this.tree_state.update(cx, |state, cx| {
                                            state.set_items(items, cx);
                                        });
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .when(self.file_tree_op.is_some(), |el| {
                let label = match &self.file_tree_op {
                    Some(crate::FileTreeOp::NewFile) => "New file:",
                    Some(crate::FileTreeOp::NewFolder) => "New folder:",
                    Some(crate::FileTreeOp::Rename(_)) => "Rename to:",
                    None => "",
                };
                el.child(
                    v_flex()
                        .w_auto()
                        .mx(px(tokens::SPACE_3))
                        .mb(px(tokens::SPACE_2))
                        .p(px(tokens::SPACE_2))
                        .gap(px(tokens::SPACE_1))
                        .rounded(px(tokens::RADIUS_SM))
                        .bg(cx.theme().background)
                        .border_1()
                        .border_color(cx.theme().primary.opacity(0.6))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            let key = event.keystroke.key.as_str();
                            if key == "escape" {
                                this.file_tree_op = None;
                                cx.stop_propagation();
                                cx.notify();
                            } else if key == "enter" {
                                this.submit_file_tree_prompt(window, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .child(
                            h_flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(tokens::UI_XS))
                                        .text_color(cx.theme().primary)
                                        .child(label),
                                )
                                .child(
                                    div()
                                        .text_size(px(tokens::UI_XS))
                                        .text_color(muted)
                                        .child("\u{21b5} save \u{00b7} esc"),
                                ),
                        )
                        .child(
                            Input::new(&self.file_tree_input)
                                .appearance(false)
                                .xsmall(),
                        ),
                )
            })
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .child(
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
                                move |ix, entry, _entry_state, _window, cx| {
                                    let item = entry.item();
                                    let item_path = PathBuf::from(item.id.as_str());
                                    let is_folder = entry.is_folder();
                                    let depth = entry.depth();
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
                                                .child({
                                                    let view_ren = view.clone();
                                                    let view_del = view.clone();
                                                    let ren_path = item_path.clone();
                                                    let del_path = item_path.clone();
                                                    h_flex()
                                                        .items_center()
                                                        .gap(px(4.))
                                                        .when(!del_armed, |r| {
                                                            r.invisible()
                                                                .group_hover(row_group.clone(), |s| {
                                                                    s.visible()
                                                                })
                                                        })
                                                        .child(
                                                            div()
                                                                .id(("tree-rename", ix))
                                                                .text_color(cx.theme().muted_foreground)
                                                                .hover(move |s| s.text_color(fg))
                                                                .child(
                                                                    Icon::new(
                                                                        gpui_kit::assets::IconName::PenTool,
                                                                    )
                                                                    .with_size(px(11.)),
                                                                )
                                                                .on_mouse_down(
                                                                    MouseButton::Left,
                                                                    |_, _, cx| cx.stop_propagation(),
                                                                )
                                                                .on_click(move |_e, window, cx| {
                                                                    cx.stop_propagation();
                                                                    let p = ren_path.clone();
                                                                    view_ren.update(cx, |this, cx| {
                                                                        this.open_file_tree_prompt(
                                                                            crate::FileTreeOp::Rename(p),
                                                                            window,
                                                                            cx,
                                                                        );
                                                                    });
                                                                }),
                                                        )
                                                        .child(
                                                            div()
                                                                .id(("tree-delete", ix))
                                                                .text_size(px(9.5))
                                                                .text_color(if del_armed {
                                                                    cx.theme().danger
                                                                } else {
                                                                    cx.theme().muted_foreground
                                                                })
                                                                .hover(|s| {
                                                                    s.text_color(cx.theme().danger)
                                                                })
                                                                .child(if del_armed {
                                                                    div().child("Del?").into_any_element()
                                                                } else {
                                                                    Icon::new(
                                                                        gpui_kit::assets::IconName::X,
                                                                    )
                                                                    .with_size(px(11.))
                                                                    .into_any_element()
                                                                })
                                                                .on_mouse_down(
                                                                    MouseButton::Left,
                                                                    |_, _, cx| cx.stop_propagation(),
                                                                )
                                                                .on_click(move |_e, _window, cx| {
                                                                    cx.stop_propagation();
                                                                    let p = del_path.clone();
                                                                    view_del.update(cx, |this, cx| {
                                                                        this.delete_tree_path_clicked(
                                                                            p, cx,
                                                                        );
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
                                        .into_any_element()
                                }
                            })
                            .list_style(StyleRefinement::default().flex_grow_1().size_full())
                            .relative()
                            .size_full(),
                    ),
            )
    }
}
