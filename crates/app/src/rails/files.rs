use std::path::{Path, PathBuf};

use gpui_kit::base::{StyledExt as _, Tree};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    tree::TreeItem,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, IGNORED_NAMES, tokens};

pub(crate) fn build_tree_items(dir: &Path, show_hidden: bool) -> Vec<TreeItem> {
    let mut items = Vec::new();

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
            let children = build_tree_items(&path, show_hidden);
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
    match name {
        ".gitignore" | ".metadata" | ".gitattributes" | "LICENSE" => return rgb(0x86868f),
        "CHANGELOG.md" => return rgb(0x8fa3b8),
        _ => {}
    }
    match Path::new(name).extension().and_then(|e| e.to_str()) {
        Some("rs") => rgb(0xdea584),
        Some("dart") => rgb(0x40c4ff),
        Some("ts") | Some("tsx") => rgb(0x4f9cf9),
        Some("js") | Some("jsx") => rgb(0xf0db4f),
        Some("json") => rgb(0xcbcb41),
        Some("toml") => rgb(0xe07b53),
        Some("yaml") | Some("yml") => rgb(0x8fa3b8),
        Some("md") => rgb(0x8fa3b8),
        Some("lock") => rgb(0x86868f),
        _ => rgb(0x9aa0a6),
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
            .child(
                h_flex()
                    .w_full()
                    .h(px(tokens::TABSTRIP_H))
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(tokens::SPACE_2))
                    .px(px(tokens::SPACE_4))
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .h(px(30.))
                            .px(px(tokens::SPACE_3))
                            .rounded(px(tokens::RADIUS_PANEL))
                            .bg(cx.theme().tab_active)
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
                                div()
                                    .id("close-rail")
                                    .cursor_pointer()
                                    .text_color(muted)
                                    .hover(|s| s.text_color(cx.theme().foreground))
                                    .child(Icon::new(gpui_kit::assets::IconName::X).xsmall())
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.sidebar_open = false;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        Button::new("rail-new")
                            .ghost()
                            .small()
                            .icon(Icon::new(IconName::Plus).text_color(muted))
                            .tooltip("New panel tab")
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.new_scratch_tab(window, cx);
                            })),
                    ),
            )
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
                            .child(
                                Button::new("rail-filter")
                                    .ghost()
                                    .small()
                                    .icon(
                                        Icon::new(gpui_kit::assets::IconName::Search)
                                            .xsmall()
                                            .text_color(muted),
                                    )
                                    .tooltip("Go to file (cmd/ctrl+P)")
                                    .on_click(cx.listener(|this, _event, window, cx| {
                                        this.palette_open = true;
                                        this.palette_selected = 0;
                                        this.palette_input.update(cx, |state, cx| {
                                            state.focus(window, cx);
                                        });
                                        cx.notify();
                                    })),
                            ),
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
                                    .tooltip("New file")
                                    .on_click(cx.listener(|this, _event, window, cx| {
                                        this.new_scratch_tab(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("open-folder")
                                    .ghost()
                                    .small()
                                    .icon(
                                        Icon::new(gpui_kit::assets::IconName::FolderPlus)
                                            .xsmall()
                                            .text_color(muted),
                                    )
                                    .tooltip("Open Folder\u{2026} (cmd/ctrl+O)")
                                    .on_click(cx.listener(|this, _event, window, cx| {
                                        this.open_folder(window, cx);
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
                                        this.git_status = core_workspace::status_map(&this.root);
                                        this.tree_state.update(cx, |state, cx| {
                                            state.set_items(items, cx);
                                        });
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                Tree::new(&self.tree_state)
                    .item({
                        let view = view.clone();
                        let active_path = self
                            .active_editor()
                            .and_then(|ed| ed.read(cx).buffer().path())
                            .map(|path| path.to_string_lossy().to_string());
                        let git_status = self.git_status.clone();
                        move |ix, entry, _entry_state, _window, cx| {
                            let item = entry.item();
                            let item_path = PathBuf::from(item.id.as_str());
                            let is_folder = entry.is_folder();
                            let depth = entry.depth();
                            let icon = if !is_folder {
                                icon_for_file(&item.label)
                            } else if entry.is_expanded() {
                                Icon::new(IconName::FolderOpen)
                            } else {
                                Icon::new(IconName::Folder)
                            };
                            let icon_color: Hsla = if is_folder {
                                cx.theme().muted_foreground
                            } else {
                                file_icon_color(&item.label).into()
                            };
                            let is_active_file =
                                active_path.as_deref() == Some(item.id.as_str());

                            let badge = if is_folder {
                                git_status
                                    .keys()
                                    .any(|changed| changed.starts_with(&item_path))
                                    .then_some('M')
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
                                .id(("tree-row", ix))
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
                                        .px(px(tokens::SPACE_3))
                                        .pl(px(depth as f32 * 16. + tokens::SPACE_3))
                                        .items_center()
                                        .gap(px(tokens::SPACE_2))
                                        .rounded(px(tokens::RADIUS_SM))
                                        .cursor_pointer()
                                        .text_color(row_fg)
                                        .children(guides)
                                        .when(is_active_file, |el| {
                                            el.font_medium()
                                                .bg(rgba(0xea7a3c16))
                                                .border_1()
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
                                                s.bg(rgba(0xffffff08)).text_color(fg)
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
                                        .child(icon.text_color(icon_color).small())
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w(px(0.))
                                                .truncate()
                                                .text_size(px(tokens::UI))
                                                .child(item.label.clone()),
                                        )
                                        .when_some(badge, |el, letter| {
                                            el.child(status_badge(letter, cx))
                                        }),
                                )
                                .on_click({
                                    let view = view.clone();
                                    let path = PathBuf::from(item.id.as_str());
                                    move |_event, window, cx| {
                                        if is_folder {
                                            return;
                                        }
                                        view.update(cx, |this, cx| {
                                            this.open_path(path.clone(), window, cx);
                                        });
                                    }
                                })
                                .into_any_element()
                        }
                    })
                    .flex_1()
                    .w_full(),
            )
    }
}
