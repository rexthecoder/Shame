use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    h_flex,
    input::Input,
    resizable::{h_resizable, resizable_panel, v_resizable},
    text::markdown,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::rails::files::{file_icon_color, icon_for_file};
use crate::{DartIde, MarkdownMode, tokens};

/// A shortcut hint: one cap per key (`⇧⌘P` → three caps), macOS-style, so
/// the glyphs stay legible instead of being squeezed into a single chip.
fn keycap(label: &'static str, cx: &App) -> Div {
    let bg = cx.theme().secondary;
    let border = cx.theme().border;
    let fg = cx.theme().muted_foreground;
    h_flex().gap(px(tokens::SPACE_1)).children(label.chars().map(move |ch| {
        div()
            .min_w(px(20.))
            .h(px(20.))
            .px(px(tokens::SPACE_2))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(tokens::RADIUS_BADGE))
            .bg(bg)
            .border_1()
            .border_color(border)
            .text_size(px(tokens::UI_SM))
            .text_color(fg)
            .child(ch.to_string())
    }))
}

/// A small on/off chip inside the find input (match case, whole word).
fn search_toggle(
    id: &'static str,
    label: &'static str,
    tooltip: &'static str,
    on: bool,
    toggle: impl Fn(&mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let fg = cx.theme().foreground;
    h_flex()
        .id(id)
        .h(px(18.))
        .px(px(4.))
        .items_center()
        .rounded(px(tokens::RADIUS_SM))
        .border_1()
        .border_color(if on { cx.theme().primary.opacity(0.85) } else { transparent_black() })
        .when(on, |el| el.bg(cx.theme().primary.opacity(0.22)))
        .cursor_pointer()
        .hover(move |s| s.bg(rgba(0xffffff0c)).text_color(fg))
        .font_family("SF Mono")
        .text_size(px(tokens::UI_XS))
        .text_color(if on { fg } else { cx.theme().muted_foreground })
        .child(label)
        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tooltip).build(window, cx))
        .on_click(move |_e, _w, cx| toggle(cx))
}

impl DartIde {
    fn render_find_bar(
        &self,
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if !self.find_open {
            return None;
        }
        let editor = self.active_editor()?.clone();
        let (current_match, total_matches) = editor.read(cx).search_status();
        let (case_sensitive, whole_word) = editor.read(cx).search_options();
        let query_empty = self.find_input.read(cx).value().is_empty();
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let badge_label = if query_empty {
            "Type to search".to_string()
        } else if total_matches == 0 {
            "0 results".to_string()
        } else {
            format!("{current_match} / {total_matches}")
        };

        Some(
            h_flex()
                .w_full()
                .h(px(36.))
                .px(px(tokens::SPACE_4))
                .gap(px(tokens::SPACE_2))
                .items_center()
                .justify_between()
                .bg(cx.theme().tab_bar)
                .border_b_1()
                .border_color(cx.theme().border)
                .on_key_down({
                    let view = view.clone();
                    let editor = editor.clone();
                    move |event, window, cx| {
                        let key = event.keystroke.key.as_str();
                        let mods = &event.keystroke.modifiers;
                        if key == "escape" {
                            view.update(cx, |this, cx| this.close_find(window, cx));
                        } else if mods.platform && mods.alt && (key == "c" || key == "w") {
                            editor.update(cx, |ed, cx| {
                                let (case, word) = ed.search_options();
                                if key == "c" {
                                    ed.set_search_options(!case, word, cx);
                                } else {
                                    ed.set_search_options(case, !word, cx);
                                }
                            });
                        } else if key == "enter" {
                            let shift = event.keystroke.modifiers.shift;
                            editor.update(cx, |ed, cx| {
                                if shift {
                                    ed.find_prev(cx);
                                } else {
                                    ed.find_next(cx);
                                }
                            });
                        }
                    }
                })
                .child(
                    h_flex()
                        .flex_1()
                        .items_center()
                        .gap(px(tokens::SPACE_2))
                        // Find input
                        .child(
                            h_flex()
                                .w(px(270.))
                                .h(px(24.))
                                .px(px(tokens::SPACE_3))
                                .gap(px(tokens::SPACE_2))
                                .items_center()
                                .rounded(px(tokens::RADIUS_SM))
                                .bg(cx.theme().background)
                                .border_1()
                                .border_color(cx.theme().border)
                                .child(
                                    Icon::new(gpui_kit::assets::IconName::Search)
                                        .with_size(px(12.))
                                        .text_color(muted),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .child(
                                            Input::new(&self.find_input)
                                                .appearance(false)
                                                .xsmall(),
                                        ),
                                )
                                .child(search_toggle(
                                    "find-case-sensitive",
                                    "Aa",
                                    "Match Case (⌥⌘C)",
                                    case_sensitive,
                                    {
                                        let editor = editor.clone();
                                        move |cx: &mut App| {
                                            editor.update(cx, |ed, cx| {
                                                let (case, word) = ed.search_options();
                                                ed.set_search_options(!case, word, cx);
                                            })
                                        }
                                    },
                                    cx,
                                ))
                                .child(search_toggle(
                                    "find-whole-word",
                                    "ab",
                                    "Match Whole Word (⌥⌘W)",
                                    whole_word,
                                    {
                                        let editor = editor.clone();
                                        move |cx: &mut App| {
                                            editor.update(cx, |ed, cx| {
                                                let (case, word) = ed.search_options();
                                                ed.set_search_options(case, !word, cx);
                                            })
                                        }
                                    },
                                    cx,
                                )),
                        )
                        // Replace input
                        .child(
                            h_flex()
                                .w(px(200.))
                                .h(px(24.))
                                .px(px(tokens::SPACE_3))
                                .gap(px(tokens::SPACE_2))
                                .items_center()
                                .rounded(px(tokens::RADIUS_SM))
                                .bg(cx.theme().background)
                                .border_1()
                                .border_color(cx.theme().border)
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .child(
                                            Input::new(&self.replace_input)
                                                .appearance(false)
                                                .xsmall(),
                                        ),
                                ),
                        )
                        // Replace Next button
                        .child({
                            let editor = editor.clone();
                            let view = view.clone();
                            h_flex()
                                .id("find-replace-next")
                                .h(px(24.))
                                .px(px(8.))
                                .items_center()
                                .rounded(px(tokens::RADIUS_SM))
                                .bg(cx.theme().secondary)
                                .border_1()
                                .border_color(cx.theme().border)
                                .cursor_pointer()
                                .hover(move |s| s.bg(rgba(0xffffff0c)).text_color(fg))
                                .text_size(px(tokens::UI_XS))
                                .text_color(fg)
                                .child("Replace")
                                .on_click(move |_e, _w, cx| {
                                    let repl = view.read(cx).replace_input.read(cx).value().to_string();
                                    editor.update(cx, |ed, cx| ed.replace_next(&repl, cx));
                                })
                        })
                        // Replace All button
                        .child({
                            let editor = editor.clone();
                            let view = view.clone();
                            h_flex()
                                .id("find-replace-all")
                                .h(px(24.))
                                .px(px(8.))
                                .items_center()
                                .rounded(px(tokens::RADIUS_SM))
                                .bg(cx.theme().secondary)
                                .border_1()
                                .border_color(cx.theme().border)
                                .cursor_pointer()
                                .hover(move |s| s.bg(rgba(0xffffff0c)).text_color(fg))
                                .text_size(px(tokens::UI_XS))
                                .text_color(fg)
                                .child("Replace All")
                                .on_click(move |_e, _w, cx| {
                                    let repl = view.read(cx).replace_input.read(cx).value().to_string();
                                    editor.update(cx, |ed, cx| ed.replace_all(&repl, cx));
                                })
                        }),
                )
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(tokens::SPACE_2))
                        .child(
                            div()
                                .px(px(tokens::SPACE_2))
                                .py(px(1.))
                                .rounded(px(tokens::RADIUS_SM))
                                .font_family("SF Mono")
                                .text_size(px(tokens::UI_XS))
                                .text_color(if !query_empty && total_matches == 0 {
                                    cx.theme().danger
                                } else {
                                    muted
                                })
                                .child(badge_label),
                        )
                        .child(
                            div()
                                .w(px(1.))
                                .h(px(14.))
                                .bg(cx.theme().border),
                        )
                        .child(
                            h_flex()
                                .id("find-prev")
                                .w(px(22.))
                                .h(px(22.))
                                .items_center()
                                .justify_center()
                                .rounded(px(tokens::RADIUS_SM))
                                .cursor_pointer()
                                .hover(move |s| s.bg(rgba(0xffffff0c)).text_color(fg))
                                .child(
                                    Icon::new(gpui_kit::assets::IconName::ChevronUp)
                                        .with_size(px(12.))
                                        .text_color(muted),
                                )
                                .on_click({
                                    let editor = editor.clone();
                                    move |_event, _window, cx| {
                                        editor.update(cx, |ed, cx| ed.find_prev(cx));
                                    }
                                }),
                        )
                        .child(
                            h_flex()
                                .id("find-next")
                                .w(px(22.))
                                .h(px(22.))
                                .items_center()
                                .justify_center()
                                .rounded(px(tokens::RADIUS_SM))
                                .cursor_pointer()
                                .hover(move |s| s.bg(rgba(0xffffff0c)).text_color(fg))
                                .child(
                                    Icon::new(gpui_kit::assets::IconName::ChevronDown)
                                        .with_size(px(12.))
                                        .text_color(muted),
                                )
                                .on_click({
                                    let editor = editor.clone();
                                    move |_event, _window, cx| {
                                        editor.update(cx, |ed, cx| ed.find_next(cx));
                                    }
                                }),
                        )
                        .child(
                            h_flex()
                                .id("find-close")
                                .w(px(22.))
                                .h(px(22.))
                                .items_center()
                                .justify_center()
                                .rounded(px(tokens::RADIUS_SM))
                                .cursor_pointer()
                                .hover(move |s| s.bg(rgba(0xffffff0c)).text_color(fg))
                                .child(
                                    Icon::new(gpui_kit::assets::IconName::X)
                                        .with_size(px(12.))
                                        .text_color(muted),
                                )
                                .on_click({
                                    let view = view.clone();
                                    move |_event, window, cx| {
                                        view.update(cx, |this, cx| this.close_find(window, cx));
                                    }
                                }),
                        ),
                ),
        )
    }

    /// Read each recent project's branch and whether it has uncommitted
    /// changes, once, in the background.
    fn load_recent_project_info(&mut self, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        if self.git.recent_info_requested {
            return;
        }
        self.git.recent_info_requested = true;
        cx.spawn(async move |weak, cx| {
            let info = cx
                .background_executor()
                .spawn(async move {
                    paths
                        .into_iter()
                        .map(|p| {
                            let branch = core_workspace::current_branch(&p);
                            let dirty = branch.is_some() && core_workspace::has_uncommitted_changes(&p);
                            (p, (branch, dirty))
                        })
                        .collect::<std::collections::HashMap<_, _>>()
                })
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.git.recent_info = info;
                cx.notify();
            });
        })
        .detach();
    }

    fn render_empty_editor(
        &self,
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let border = cx.theme().border;
        let home_prefix = std::env::var("HOME").ok();

        // Inside a project with uncommitted work, the empty editor picks up
        // where you left off: the files you were changing, biggest first,
        // and a way into reviewing them. Otherwise a few source files.
        let changed: Vec<core_workspace::ChangedFile> = self
            .git
            .status
            .as_ref()
            .filter(|_| self.has_workspace)
            .map(|s| {
                let mut files: Vec<_> = s
                    .staged
                    .iter()
                    .chain(s.unstaged.iter())
                    .filter(|f| f.kind != core_workspace::ChangeKind::Deleted)
                    .cloned()
                    .collect();
                files.sort_by_key(|f| {
                    std::cmp::Reverse(self.git.stats.get(&f.path).map_or(0, |(a, r)| a + r))
                });
                files.dedup_by(|a, b| a.path == b.path);
                files
            })
            .unwrap_or_default();
        let change_count = changed.len();
        let review_file = changed.first().cloned();
        let repo_root = self.git.status.as_ref().map(|s| s.root.clone());
        let from_changes = !changed.is_empty();
        let quick_files: Vec<_> = if from_changes {
            let root = repo_root.clone().unwrap_or_else(|| self.root.clone());
            changed.iter().take(4).map(|f| root.join(&f.path)).collect()
        } else if self.has_workspace {
            self.all_files
                .iter()
                .filter(|p| {
                    p.extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|ext| matches!(ext, "dart" | "rs" | "toml" | "md" | "ts" | "py"))
                })
                .take(4)
                .cloned()
                .collect()
        } else {
            Vec::new()
        };

        let recent_folders: Vec<std::path::PathBuf> = self
            .recent_workspaces
            .iter()
            .filter(|p| !self.has_workspace || *p != &self.root)
            .take(5)
            .cloned()
            .collect();
        // Branch and uncommitted-changes state per recent project, read once
        // off the UI thread (each is a couple of `git` calls).
        if !self.git.recent_info_requested && !recent_folders.is_empty() && !self.has_workspace {
            let view = view.clone();
            let paths = recent_folders.clone();
            cx.defer(move |cx| {
                view.update(cx, |this, cx| this.load_recent_project_info(paths, cx));
            });
        }

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(28.))
            // Original Shame Hero Lockup
            .child(
                h_flex()
                    .items_center()
                    .gap(px(16.))
                    .child(
                        div()
                            .size(px(44.))
                            .rounded(px(10.))
                            .bg(cx.theme().primary.opacity(0.14))
                            .border_1()
                            .border_color(cx.theme().primary.opacity(0.42))
                            .flex()
                            .items_center()
                            .justify_center()
                            .font_family("SF Mono")
                            .font_semibold()
                            .text_size(px(14.))
                            .text_color(cx.theme().primary)
                            .child("//>"),
                    )
                    .child(
                        v_flex()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(20.))
                                    .font_medium()
                                    .text_color(fg.opacity(0.94))
                                    .child(if self.has_workspace {
                                        self.root
                                            .file_name()
                                            .and_then(|n| n.to_str())
                                            .unwrap_or("Shame")
                                            .to_string()
                                    } else {
                                        "Shame".to_string()
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(muted)
                                    .child(if self.has_workspace {
                                        "\u{2318}P to open a file \u{b7} \u{21e7}\u{2318}P for commands"
                                    } else {
                                        "Built for speed, craft, and flow"
                                    }),
                            ),
                    ),
            )
            // Clean, borderless Zed-style Action Rows. Inside a project they
            // are noise (the subtitle names the two shortcuts that matter),
            // so they show only on the welcome screen.
            .child(
                v_flex()
                    .when(self.has_workspace, |el| el.hidden())
                    .w(px(320.))
                    .gap(px(2.))
                    .child(
                        h_flex()
                            .id("empty-open-folder")
                            .w_full()
                            .h(px(30.))
                            .px(px(10.))
                            .rounded(px(tokens::RADIUS_SM))
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(0xffffff08)).text_color(fg))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(muted)
                                    .child("Open Folder\u{2026}"),
                            )
                            .child(keycap("\u{2318}O", cx))
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.open_folder(window, cx);
                            })),
                    )
                    .child(
                        h_flex()
                            .id("empty-new-project")
                            .w_full()
                            .h(px(30.))
                            .px(px(10.))
                            .rounded(px(tokens::RADIUS_SM))
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(0xffffff08)).text_color(fg))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(muted)
                                    .child("New Flutter Project\u{2026}"),
                            )
                            .child(
                                div()
                                    .font_family("SF Mono")
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(muted.opacity(0.6))
                                    .child("flutter create"),
                            )
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.open_new_project(window, cx);
                            })),
                    )
                    .child(
                        h_flex()
                            .id("empty-new-file")
                            .w_full()
                            .h(px(30.))
                            .px(px(10.))
                            .rounded(px(tokens::RADIUS_SM))
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(0xffffff08)).text_color(fg))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(muted)
                                    .child("New File"),
                            )
                            .child(keycap("\u{2318}N", cx))
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.new_scratch_tab(window, cx);
                            })),
                    )
                    .when(self.has_workspace, |el| {
                        el.child(
                            h_flex()
                                .id("empty-go-file")
                                .w_full()
                                .h(px(30.))
                                .px(px(10.))
                                .rounded(px(tokens::RADIUS_SM))
                                .items_center()
                                .justify_between()
                                .cursor_pointer()
                                .hover(move |s| s.bg(rgba(0xffffff08)).text_color(fg))
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .text_color(muted)
                                        .child("Go to File\u{2026}"),
                                )
                                .child(keycap("\u{2318}P", cx))
                                .on_click(cx.listener(|this, _event, window, cx| {
                                    this.palette_open = true;
                                    this.palette_selected = 0;
                                    this.palette_input.update(cx, |state, cx| {
                                        state.set_value("", window, cx);
                                        state.focus(window, cx);
                                    });
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        h_flex()
                            .id("empty-cmd-palette")
                            .w_full()
                            .h(px(30.))
                            .px(px(10.))
                            .rounded(px(tokens::RADIUS_SM))
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(0xffffff08)).text_color(fg))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(muted)
                                    .child("Run Command\u{2026}"),
                            )
                            .child(keycap("\u{21e7}\u{2318}P", cx))
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.palette_open = true;
                                this.palette_selected = 0;
                                this.palette_input.update(cx, |state, cx| {
                                    state.set_value("> ", window, cx);
                                    state.focus(window, cx);
                                });
                                cx.notify();
                            })),
                    )
                    .child(
                        h_flex()
                            .id("empty-open-settings")
                            .w_full()
                            .h(px(30.))
                            .px(px(10.))
                            .rounded(px(tokens::RADIUS_SM))
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(0xffffff08)).text_color(fg))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(muted)
                                    .child("Open Settings"),
                            )
                            .child(keycap("\u{2318},", cx))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.toggle_settings(cx);
                            })),
                    ),
            )
            // Recent Projects Section (Folder Restoration)
            .when(!recent_folders.is_empty() && !self.has_workspace, |el| {
                el.child(
                    v_flex()
                        .w(px(320.))
                        .pt(px(10.))
                        .gap(px(2.))
                        .border_t_1()
                        .border_color(border.opacity(0.6))
                        .child(
                            div()
                                .px(px(10.))
                                .pb(px(4.))
                                .text_size(px(tokens::UI_XS))
                                .font_medium()
                                .text_color(muted.opacity(0.8))
                                .child("Recent Projects"),
                        )
                        .children(recent_folders.into_iter().enumerate().map(|(ix, path)| {
                            let name = path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("folder")
                                .to_string();
                            let full_str = path.to_string_lossy().to_string();
                            let short_dir = match &home_prefix {
                                Some(h) if full_str.starts_with(h) => {
                                    format!("~{}", &full_str[h.len()..])
                                }
                                _ => full_str,
                            };
                            let open_p = path.clone();
                            let rm_p = path.clone();
                            let view_open = view.clone();
                            let view_rm = view.clone();
                            let row_group: SharedString = format!("recent-folder-{ix}").into();
                            h_flex()
                                .id(("recent-folder-row", ix))
                                .group(row_group.clone())
                                .w_full()
                                .h(px(28.))
                                .px(px(10.))
                                .rounded(px(tokens::RADIUS_SM))
                                .items_center()
                                .justify_between()
                                .gap(px(tokens::SPACE_2))
                                .cursor_pointer()
                                .hover(move |s| s.bg(rgba(0xffffff08)))
                                .child(
                                    h_flex()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .items_center()
                                        .gap(px(tokens::SPACE_2))
                                        .child(
                                            div()
                                                .font_medium()
                                                .text_size(px(13.))
                                                .text_color(fg)
                                                .child(name),
                                        )
                                        .when_some(self.git.recent_info.get(&path).cloned(), |el, (branch, dirty)| {
                                            el.when_some(branch, |el, branch| {
                                                el.child(
                                                    h_flex()
                                                        .flex_shrink_0()
                                                        .items_center()
                                                        .gap(px(3.))
                                                        .text_size(px(11.))
                                                        .text_color(muted)
                                                        .child(
                                                            Icon::new(gpui_kit::assets::IconName::GitBranch)
                                                                .with_size(px(10.)),
                                                        )
                                                        .child(div().max_w(px(150.)).truncate().child(branch))
                                                        .when(dirty, |el| {
                                                            el.child(
                                                                div()
                                                                    .size(px(5.))
                                                                    .rounded(px(3.))
                                                                    .bg(cx.theme().warning),
                                                            )
                                                        }),
                                                )
                                            })
                                        })
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w(px(0.))
                                                .truncate()
                                                .text_size(px(11.5))
                                                .text_color(muted.opacity(0.75))
                                                .child(short_dir),
                                        ),
                                )
                                .child(
                                    div()
                                        .id(("rm-recent-folder", ix))
                                        .cursor_pointer()
                                        .invisible()
                                        .group_hover(row_group, |s| s.visible())
                                        .text_color(muted)
                                        .hover(move |s| s.text_color(fg))
                                        .child(
                                            Icon::new(gpui_kit::assets::IconName::X)
                                                .with_size(px(10.5)),
                                        )
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation();
                                        })
                                        .on_click(move |_e, _w, cx| {
                                            cx.stop_propagation();
                                            let p = rm_p.clone();
                                            view_rm.update(cx, |this, cx| {
                                                this.remove_recent_workspace(p, cx);
                                            });
                                        }),
                                )
                                .on_click(move |_e, _w, cx| {
                                    let p = open_p.clone();
                                    view_open.update(cx, |this, cx| {
                                        this.set_root(p, cx);
                                    });
                                })
                        })),
                )
            })
            .when(!quick_files.is_empty(), |el| {
                // Several `Cargo.toml`s in a row are indistinguishable, so a
                // name that repeats gets its parent folder as a prefix.
                let base = |p: &std::path::Path| {
                    p.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string()
                };
                let names: Vec<String> = quick_files.iter().map(|p| base(p)).collect();
                let el = el.when(from_changes, |el| {
                    el.child(
                        h_flex()
                            .items_center()
                            .gap(px(tokens::SPACE_3))
                            .child(
                                div()
                                    .text_size(px(tokens::UI_XS))
                                    .font_medium()
                                    .text_color(muted.opacity(0.8))
                                    .child("Continue where you left off"),
                            )
                            .when_some(review_file.clone(), |row, file| {
                                let view = view.clone();
                                row.child(
                                    h_flex()
                                        .id("review-changes")
                                        .items_center()
                                        .gap(px(tokens::SPACE_1))
                                        .cursor_pointer()
                                        .text_size(px(tokens::UI_XS))
                                        .text_color(cx.theme().primary)
                                        .hover(|s| s.opacity(0.8))
                                        .child(format!(
                                            "Review {change_count} change{} \u{2192}",
                                            if change_count == 1 { "" } else { "s" }
                                        ))
                                        .on_click(move |_e, _w, cx| {
                                            view.update(cx, |this, cx| {
                                                this.set_sidebar_tab(crate::git_panel::SidebarTab::Git, cx);
                                                this.open_git_diff(file.clone(), false, cx);
                                            });
                                        }),
                                )
                            }),
                    )
                });
                el.child(
                    h_flex()
                        .flex_wrap()
                        .justify_center()
                        .items_center()
                        .gap(px(tokens::SPACE_2))
                        .children(quick_files.into_iter().enumerate().map(|(ix, path)| {
                            let name = names[ix].clone();
                            let parent = (names.iter().filter(|n| **n == name).count() > 1)
                                .then(|| path.parent().map(|p| base(p)))
                                .flatten()
                                .filter(|p| !p.is_empty())
                                .map(|p| format!("{p}/"));
                            let view = view.clone();
                            h_flex()
                                .id(("quick-file", ix))
                                .h(px(26.))
                                .px(px(tokens::SPACE_3))
                                .gap(px(tokens::SPACE_2))
                                .items_center()
                                .rounded(px(tokens::RADIUS_PILL))
                                .bg(cx.theme().sidebar)
                                .border_1()
                                .border_color(cx.theme().border)
                                .cursor_pointer()
                                .hover(move |s| {
                                    s.bg(rgba(0xea7a3c14)).border_color(rgba(0xea7a3c40))
                                })
                                .child(
                                    icon_for_file(&name)
                                        .text_color(file_icon_color(&name))
                                        .xsmall(),
                                )
                                .child(
                                    h_flex()
                                        .text_size(px(tokens::UI_SM))
                                        .when_some(parent, |el, parent| {
                                            el.child(div().text_color(muted).child(parent))
                                        })
                                        .child(div().text_color(fg).child(name)),
                                )
                                .on_click(move |_event, window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.open_path(path.clone(), window, cx);
                                    });
                                })
                        })),
                )
            })
    }

    fn render_image_viewer(
        &self,
        tab_ix: usize,
        preview: &crate::ImagePreview,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let magenta = cx.theme().magenta;
        let primary = cx.theme().primary;
        let list_hover = cx.theme().list_hover;
        let zoom_pct = preview.zoom_percent;
        let light_bg = preview.light_backdrop;
        let dims = format!("{} \u{00d7} {} px", preview.width, preview.height);
        let size_str = crate::format_byte_size(preview.byte_size);
        let fmt_label = preview.format_label.clone();
        let zoom_label = match zoom_pct {
            Some(pct) => format!("{pct}%"),
            None => "Fit".to_string(),
        };
        let canvas_bg: Hsla = if light_bg {
            rgb(0xe8e8ec).into()
        } else {
            tokens::bg_editor()
        };

        v_flex()
            .size_full()
            .min_h(px(0.))
            .bg(cx.theme().background)
            // Toolbar strip: dimensions, format, file size, backdrop toggle, and zoom controls
            .child(
                h_flex()
                    .w_full()
                    .h(px(32.))
                    .flex_shrink_0()
                    .px(px(tokens::SPACE_5))
                    .items_center()
                    .justify_between()
                    .bg(cx.theme().tab_bar)
                    .border_b_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(tokens::SPACE_3))
                            .child(
                                div()
                                    .px(px(6.))
                                    .py(px(1.))
                                    .rounded(px(4.))
                                    .bg(magenta.opacity(0.16))
                                    .font_family("SF Mono")
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(magenta)
                                    .child(fmt_label),
                            )
                            .child(
                                div()
                                    .font_family("SF Mono")
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(fg)
                                    .child(dims),
                            )
                            .child(div().w(px(1.)).h(px(12.)).bg(border))
                            .child(
                                div()
                                    .font_family("SF Mono")
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(muted)
                                    .child(size_str),
                            ),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .child(
                                h_flex()
                                    .id("img-backdrop-toggle")
                                    .h(px(22.))
                                    .px(px(tokens::SPACE_3))
                                    .items_center()
                                    .rounded(px(tokens::RADIUS_SM))
                                    .border_1()
                                    .border_color(if light_bg { primary } else { border })
                                    .bg(cx.theme().background)
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(list_hover))
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(if light_bg { primary } else { muted })
                                    .child(if light_bg { "Light bg" } else { "Dark bg" })
                                    .on_click(cx.listener(move |this, _e, _w, cx| {
                                        if let Some(img) =
                                            this.tabs.get_mut(tab_ix).and_then(|t| t.image.as_mut())
                                        {
                                            img.light_backdrop = !img.light_backdrop;
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(div().w(px(1.)).h(px(12.)).bg(border))
                            .child(
                                h_flex()
                                    .id("img-zoom-out")
                                    .w(px(22.))
                                    .h(px(22.))
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(tokens::RADIUS_SM))
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(list_hover).text_color(fg))
                                    .text_size(px(tokens::UI))
                                    .text_color(muted)
                                    .child("\u{2212}")
                                    .on_click(cx.listener(move |this, _e, _w, cx| {
                                        if let Some(img) =
                                            this.tabs.get_mut(tab_ix).and_then(|t| t.image.as_mut())
                                        {
                                            let cur = img.zoom_percent.unwrap_or(100);
                                            img.zoom_percent = Some(cur.saturating_sub(25).max(25));
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(
                                div()
                                    .id("img-zoom-reset")
                                    .min_w(px(44.))
                                    .h(px(22.))
                                    .px(px(tokens::SPACE_2))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(tokens::RADIUS_SM))
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(list_hover).text_color(fg))
                                    .font_family("SF Mono")
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(fg)
                                    .child(zoom_label)
                                    .on_click(cx.listener(move |this, _e, _w, cx| {
                                        if let Some(img) =
                                            this.tabs.get_mut(tab_ix).and_then(|t| t.image.as_mut())
                                        {
                                            img.zoom_percent = match img.zoom_percent {
                                                None => Some(100),
                                                Some(_) => None,
                                            };
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(
                                h_flex()
                                    .id("img-zoom-in")
                                    .w(px(22.))
                                    .h(px(22.))
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(tokens::RADIUS_SM))
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(list_hover).text_color(fg))
                                    .text_size(px(tokens::UI))
                                    .text_color(muted)
                                    .child("+")
                                    .on_click(cx.listener(move |this, _e, _w, cx| {
                                        if let Some(img) =
                                            this.tabs.get_mut(tab_ix).and_then(|t| t.image.as_mut())
                                        {
                                            let cur = img.zoom_percent.unwrap_or(100);
                                            img.zoom_percent = Some((cur + 25).min(400));
                                            cx.notify();
                                        }
                                    })),
                            ),
                    ),
            )
            // Image canvas
            .child(match zoom_pct {
                None => div()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .p(px(tokens::SPACE_6))
                    .bg(canvas_bg)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        img(preview.render_image.clone())
                            .size_full()
                            .object_fit(ObjectFit::Contain),
                    )
                    .into_any_element(),
                Some(pct) => {
                    let scaled_w = (preview.width as f32 * pct as f32 / 100.0).max(16.0);
                    let scaled_h = (preview.height as f32 * pct as f32 / 100.0).max(16.0);
                    div()
                        .id("img-scroll-canvas")
                        .flex_1()
                        .w_full()
                        .min_h(px(0.))
                        .p(px(tokens::SPACE_6))
                        .bg(canvas_bg)
                        .overflow_scroll()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            img(preview.render_image.clone())
                                .w(px(scaled_w))
                                .h(px(scaled_h))
                                .object_fit(ObjectFit::Fill),
                        )
                        .into_any_element()
                }
            })
            .into_any_element()
    }

    pub(crate) fn render_shell(
        &self,
        view: &Entity<Self>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active_tab = self.tabs.get(self.active_tab);
        let active_editor = active_tab.map(|t| t.editor.clone());
        let active_image = active_tab.and_then(|t| t.image.clone());
        let active_data = active_tab.and_then(|t| t.data.clone());
        let active_md_mode = active_tab
            .filter(|t| t.is_markdown())
            .map(|t| t.md_mode)
            .unwrap_or(MarkdownMode::Code);
        let has_tabs = !self.tabs.is_empty();

        let render_md_pane = |editor: &Entity<editor_view::EditorView>, cx: &App| {
            let source: SharedString = editor.read(cx).buffer().text().into();
            div()
                .flex_1()
                .h_full()
                .min_w(px(0.))
                .min_h(px(0.))
                .bg(cx.theme().background)
                .p(px(tokens::SPACE_6))
                .child(markdown(source).scrollable(true).selectable(true))
        };

        let primary_body = match (active_editor, active_md_mode) {
            _ if self.diff.is_some() => self.render_diff_view(cx),
            _ if active_image.is_some() => {
                self.render_image_viewer(self.active_tab, active_image.as_ref().unwrap(), cx)
            }
            (Some(editor), MarkdownMode::Code) => editor.into_any_element(),
            (Some(_), MarkdownMode::Preview) if active_data.is_some() => {
                active_data.clone().unwrap().into_any_element()
            }
            (Some(editor), MarkdownMode::Preview) => render_md_pane(&editor, cx).into_any_element(),
            (Some(editor), MarkdownMode::Split) => {
                let preview: AnyElement = match &active_data {
                    Some(data) => div()
                        .flex_1()
                        .h_full()
                        .min_w(px(0.))
                        .min_h(px(0.))
                        .child(data.clone())
                        .into_any_element(),
                    None => render_md_pane(&editor, cx).into_any_element(),
                };
                h_flex()
                    .size_full()
                    .min_h(px(0.))
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .min_w(px(0.))
                            .min_h(px(0.))
                            .border_r_1()
                            .border_color(cx.theme().border)
                            .child(editor),
                    )
                    .child(preview)
                    .into_any_element()
            }
            (None, _) => self.render_empty_editor(view, cx).into_any_element(),
        };

        let editor_body = if let (None, Some(right_ix)) = (&self.diff, self.split_tab) {
            if let Some(right_tab) = self.tabs.get(right_ix) {
                let right_title = right_tab.title();
                let right_pane: AnyElement = if let Some(img) = &right_tab.image {
                    self.render_image_viewer(right_ix, img, cx)
                } else {
                    right_tab.editor.clone().into_any_element()
                };
                let muted = cx.theme().muted_foreground;
                let fg = cx.theme().foreground;
                let right_card = v_flex()
                    .size_full()
                    .min_w(px(0.))
                    .min_h(px(0.))
                    .border_l_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .w_full()
                            .h(px(28.))
                            .px(px(tokens::SPACE_4))
                            .items_center()
                            .justify_between()
                            .bg(cx.theme().tab_bar)
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(tokens::SPACE_2))
                                    .child(crate::rails::files::render_file_icon(&right_title))
                                    .child(
                                        div()
                                            .text_size(px(tokens::UI_SM))
                                            .text_color(fg)
                                            .child(right_title),
                                    ),
                            )
                            .child(
                                div()
                                    .id("close-split-pane")
                                    .cursor_pointer()
                                    .text_color(muted)
                                    .hover(move |s| s.text_color(fg))
                                    .child(
                                        Icon::new(gpui_kit::assets::IconName::X).with_size(px(11.)),
                                    )
                                    .on_click(cx.listener(|this, _e, _w, cx| {
                                        this.split_tab = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(div().flex_1().w_full().min_h(px(0.)).child(right_pane));

                h_resizable("editor-split-panes")
                    .child(resizable_panel().child(primary_body))
                    .child(resizable_panel().child(right_card))
                    .into_any_element()
            } else {
                primary_body
            }
        } else {
            primary_body
        };

        let editor_card = v_flex()
            .relative()
            .flex_1()
            .h_full()
            .min_h(px(0.))
            .bg(cx.theme().background)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(tokens::RADIUS_CARD))
            .overflow_hidden()
            .when(has_tabs, |el| el.child(self.render_tabs(view, cx)))
            .when_some(self.render_file_notice(cx), |el, banner| el.child(banner))
            .when_some(self.render_find_bar(view, cx), |el, bar| el.child(bar))
            .child(div().flex_1().w_full().min_h(px(0.)).child(editor_body))
            .when_some(self.render_tab_overflow_popover(view, cx), |el, pop| {
                el.child(pop)
            })
            .into_any_element();

        let editor_column = if self.terminal_open && !self.terminal_maximized {
            div()
                .flex_1()
                .w_full()
                .h(relative(1.))
                .min_h(px(0.))
                .px(px(tokens::GUTTER / 2.))
                .child(
                    v_resizable("center-vertical-split")
                        .child(
                            resizable_panel()
                                .pb(px(tokens::GUTTER / 2.))
                                .child(editor_card),
                        )
                        .child(
                            resizable_panel()
                                .size(px(tokens::TERMINAL_H))
                                .size_range(px(tokens::TERMINAL_MIN)..px(tokens::TERMINAL_MAX))
                                .fixed()
                                .pt(px(tokens::GUTTER / 2.))
                                .child(self.render_terminal(view, window, cx)),
                        ),
                )
                .into_any_element()
        } else if self.terminal_open && self.terminal_maximized {
            v_flex()
                .flex_1()
                .w_full()
                .h(relative(1.))
                .min_h(px(0.))
                .px(px(tokens::GUTTER / 2.))
                .child(self.render_terminal(view, window, cx))
                .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .w_full()
                .h(relative(1.))
                .min_h(px(0.))
                .px(px(tokens::GUTTER / 2.))
                .child(editor_card)
                .into_any_element()
        };

        let sidebar_visible = self.sidebar_open && self.has_workspace;
        let split_id = match (
            sidebar_visible,
            self.sim.attached,
            self.web_preview_open,
            self.agent_open,
        ) {
            (true, false, false, false) => "shell-split-left",
            (true, false, false, true) => "shell-split-both",
            (true, true, false, false) => "shell-split-left-sim",
            (true, true, false, true) => "shell-split-all",
            (true, false, true, false) => "shell-split-left-web",
            (true, false, true, true) => "shell-split-left-web-agent",
            (true, true, true, _) => "shell-split-left-sim-web",
            (false, false, false, true) => "shell-split-right",
            (false, true, false, false) => "shell-split-sim",
            (false, true, false, true) => "shell-split-sim-agent",
            (false, false, true, false) => "shell-split-web",
            (false, false, true, true) => "shell-split-web-agent",
            (false, true, true, _) => "shell-split-sim-web",
            (false, false, false, false) => "shell-split-center",
        };
        // Saved split sizes are per layout, so the Flutter rail gets its own ids.
        let flutter_open = self.flutter_panel.open && self.has_workspace;
        let split_id: SharedString = if flutter_open {
            format!("{split_id}-flutter").into()
        } else {
            split_id.into()
        };
        let mut split = h_resizable(split_id);

        if sidebar_visible {
            split = split.child(
                resizable_panel()
                    .size(px(tokens::RAIL_LEFT))
                    .size_range(px(tokens::RAIL_LEFT_MIN)..px(tokens::RAIL_LEFT_MAX))
                    .fixed()
                    .pr(px(tokens::GUTTER / 2.))
                    .child(
                        v_flex()
                            .size_full()
                            .bg(cx.theme().sidebar)
                            .border_1()
                            .border_color(cx.theme().border)
                            .rounded(px(tokens::RADIUS_CARD))
                            .overflow_hidden()
                            .child(self.render_sidebar_tabs(cx))
                            .child(match self.sidebar_tab {
                                crate::git_panel::SidebarTab::Files => {
                                    self.render_files_rail(view, cx).into_any_element()
                                }
                                crate::git_panel::SidebarTab::Search => {
                                    self.render_search_rail(cx).into_any_element()
                                }
                                crate::git_panel::SidebarTab::Git => {
                                    self.render_git_rail(cx).into_any_element()
                                }
                            }),
                    ),
            );
        }

        split = split.child(resizable_panel().child(editor_column));

        if flutter_open {
            let more_right = self.sim.attached || self.web_preview_open || self.agent_open;
            split = split.child(
                resizable_panel()
                    .size(px(380.))
                    .size_range(px(320.)..px(560.))
                    .fixed()
                    .pl(px(tokens::GUTTER / 2.))
                    .when(more_right, |p| p.pr(px(tokens::GUTTER / 2.)))
                    .child(self.render_flutter_rail(cx)),
            );
        }

        if self.sim.attached {
            split = split.child(
                resizable_panel()
                    .size(px(312.))
                    .size_range(px(292.)..px(400.))
                    .fixed()
                    .pl(px(tokens::GUTTER / 2.))
                    .when(self.web_preview_open || self.agent_open, |p| {
                        p.pr(px(tokens::GUTTER / 2.))
                    })
                    .child(self.render_simulator_rail(cx)),
            );
        }

        if self.web_preview_open {
            split = split.child(
                resizable_panel()
                    .size(px(340.))
                    .size_range(px(280.)..px(520.))
                    .fixed()
                    .pl(px(tokens::GUTTER / 2.))
                    .when(self.agent_open, |p| p.pr(px(tokens::GUTTER / 2.)))
                    .child(self.render_web_preview_rail(cx)),
            );
        }

        if self.agent_open {
            split = split.child(
                resizable_panel()
                    .size(px(tokens::RAIL_RIGHT))
                    .size_range(px(tokens::RAIL_RIGHT_MIN)..px(tokens::RAIL_RIGHT_MAX))
                    .fixed()
                    .pl(px(tokens::GUTTER / 2.))
                    .child(self.render_agent_rail(cx)),
            );
        }

        div()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .p(px(tokens::GUTTER))
            .child(split)
    }
}
