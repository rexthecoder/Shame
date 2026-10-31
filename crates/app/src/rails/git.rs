//! The Git panel (the "Git" tab of the left sidebar): branch bar with fetch / pull / push, commit
//! box, and the Staged / Changes lists. State and actions live in `crate::git_panel`.

use std::path::Path;

use core_workspace::{ChangeKind, ChangedFile, CommitInfo, RepoStatus, StashEntry};
use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    h_flex,
    input::{Input, Textarea},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::git_panel::SidebarTab;
use crate::{DartIde, tokens};

/// Which list a file row belongs to; decides its actions.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Conflicts = 0,
    Staged = 1,
    Changes = 2,
}

/// Horizontal inset shared by every row inside the commit card, so their edges line up.
const CARD_INSET: f32 = 10.;

/// A soft orange pill: the accent as a tint, for secondary calls to action (push, publish).
fn accent_tint(cx: &App) -> ButtonCustomVariant {
    let accent = cx.theme().primary;
    ButtonCustomVariant::new(cx)
        .color(accent.opacity(0.16))
        .foreground(accent)
        .hover(accent.opacity(0.26))
        .active(accent.opacity(0.34))
        .shadow(false)
}

/// A quiet neutral pill (pull).
fn neutral_pill(cx: &App) -> ButtonCustomVariant {
    ButtonCustomVariant::new(cx)
        .color(cx.theme().list_hover)
        .foreground(cx.theme().foreground)
        .hover(cx.theme().list_hover.opacity(0.8))
        .active(cx.theme().list_hover)
        .shadow(false)
}

impl DartIde {
    /// The "Files | Git" tab strip at the top of the left sidebar, shared by both views.
    pub(crate) fn render_sidebar_tabs(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let tab = |id: &'static str,
                   label: &'static str,
                   icon: IconName,
                   which: SidebarTab,
                   cx: &mut Context<Self>| {
            let active = self.sidebar_tab == which;
            h_flex()
                .id(id)
                .items_center()
                .gap(px(tokens::SPACE_2))
                .h(px(28.))
                .px(px(tokens::SPACE_2))
                .rounded(px(tokens::RADIUS_SM))
                .border_1()
                .border_color(if active {
                    cx.theme().border
                } else {
                    gpui_kit::transparent_black()
                })
                .when(active, |s| s.bg(cx.theme().tab_active))
                .when(!active, |s| s.hover(|h| h.bg(cx.theme().list_hover)))
                .cursor_pointer()
                .child(Icon::new(icon).small().text_color(if active {
                    cx.theme().primary
                } else {
                    muted
                }))
                .when(active, |el| {
                    el.child(
                        div()
                            .text_size(px(tokens::UI_SM))
                            .font_medium()
                            .text_color(cx.theme().foreground)
                            .child(label),
                    )
                })
                .on_click(
                    cx.listener(move |this, _event, _window, cx| this.set_sidebar_tab(which, cx)),
                )
        };
        let changes = self
            .git
            .status
            .as_ref()
            .map_or(0, |s| s.staged.len() + s.unstaged.len() + s.conflicts.len());
        h_flex()
            .w_full()
            .h(px(tokens::TABSTRIP_H))
            .flex_shrink_0()
            .items_center()
            .px(px(tokens::SPACE_4))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .items_center()
                    .gap(px(tokens::SPACE_1))
                    .child(tab(
                        "sidebar-tab-files",
                        "Files",
                        IconName::Folder,
                        SidebarTab::Files,
                        cx,
                    ))
                    .child(tab(
                        "sidebar-tab-search",
                        "Search",
                        IconName::Search,
                        SidebarTab::Search,
                        cx,
                    ))
                    .child(
                        tab(
                            "sidebar-tab-git",
                            "Git",
                            IconName::GitBranch,
                            SidebarTab::Git,
                            cx,
                        )
                        .when(
                            changes > 0 && self.sidebar_tab != SidebarTab::Git,
                            |el| {
                                el.child(
                                    div()
                                        .px(px(5.))
                                        .rounded(px(tokens::RADIUS_PILL))
                                        .bg(cx.theme().primary.opacity(0.2))
                                        .text_size(px(9.5))
                                        .text_color(cx.theme().primary)
                                        .child(changes.to_string()),
                                )
                            },
                        ),
                    )
            )
    }

    pub(crate) fn render_git_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(tokens::RADIUS_CARD))
            .overflow_hidden()
            .child(self.render_sidebar_tabs(cx))
            .child(match self.git.status.clone() {
                Some(status) => self.render_git_body(&status, cx).into_any_element(),
                None => self.render_git_not_a_repo(cx).into_any_element(),
            })
    }

    pub(crate) fn render_search_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let query = self.project_search_input.read(cx).value().trim().to_string();
        let count = self.project_search_matches.len();

        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(tokens::RADIUS_CARD))
            .overflow_hidden()
            .child(self.render_sidebar_tabs(cx))
            .child(
                v_flex()
                    .w_full()
                    .p(px(tokens::SPACE_4))
                    .gap(px(tokens::SPACE_2))
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .w_full()
                            .h(px(28.))
                            .px(px(tokens::SPACE_3))
                            .gap(px(tokens::SPACE_2))
                            .items_center()
                            .rounded(px(tokens::RADIUS_SM))
                            .bg(cx.theme().background)
                            .border_1()
                            .border_color(cx.theme().border)
                            .child(Icon::new(IconName::Search).xsmall().text_color(muted))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .child(
                                        Input::new(&self.project_search_input)
                                            .appearance(false)
                                            .small(),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .h(px(28.))
                            .px(px(tokens::SPACE_3))
                            .gap(px(tokens::SPACE_2))
                            .items_center()
                            .rounded(px(tokens::RADIUS_SM))
                            .bg(cx.theme().background)
                            .border_1()
                            .border_color(cx.theme().border)
                            .child(
                                div().flex_1().min_w(px(0.)).child(
                                    Input::new(&self.project_replace_input)
                                        .appearance(false)
                                        .small(),
                                ),
                            )
                            .child(
                                div()
                                    .id("project-replace-all")
                                    .px(px(tokens::SPACE_2))
                                    .text_size(px(tokens::UI_XS))
                                    .cursor_pointer()
                                    .text_color(fg)
                                    .hover(|s| s.bg(cx.theme().list_hover))
                                    .child("Replace all")
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.replace_in_project(cx);
                                    })),
                            ),
                    )
                    .when_some(self.project_replace_status.clone(), |el, status| {
                        el.child(
                            div()
                                .text_size(px(tokens::UI_XS))
                                .text_color(muted)
                                .child(status),
                        )
                    })
                    .when(!query.is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(px(tokens::UI_XS))
                                .text_color(muted)
                                .child(format!(
                                    "{count} match{} across workspace",
                                    if count == 1 { "" } else { "es" }
                                )),
                        )
                    }),
            )
            .child(
                v_flex()
                    .id("project-search-results")
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .p(px(tokens::SPACE_2))
                    .gap(px(tokens::SPACE_1))
                    .when(query.is_empty(), |el| {
                        el.child(
                            div()
                                .p(px(tokens::SPACE_5))
                                .text_center()
                                .text_size(px(tokens::UI_SM))
                                .text_color(muted)
                                .child("Type above to search across all files in the project (\u{2318}\u{21e7}F)."),
                        )
                    })
                    .children(
                        self.project_search_matches
                            .iter()
                            .take(150)
                            .enumerate()
                            .map(|(ix, m)| {
                                let name = m
                                    .path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("")
                                    .to_string();
                                let path = m.path.clone();
                                let line_0 = m.line_number.saturating_sub(1) as u32;
                                let preview = m.line_text.trim().to_string();
                                let q = query.clone();

                                v_flex()
                                    .id(("proj-search-hit", ix))
                                    .w_full()
                                    .px(px(tokens::SPACE_3))
                                    .py(px(tokens::SPACE_2))
                                    .gap(px(2.))
                                    .rounded(px(tokens::RADIUS_SM))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(cx.theme().list_hover))
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .items_center()
                                            .justify_between()
                                            .gap(px(tokens::SPACE_2))
                                            .child(
                                                h_flex()
                                                    .min_w(px(0.))
                                                    .items_center()
                                                    .gap(px(tokens::SPACE_2))
                                                    .child(crate::rails::files::render_file_icon(
                                                        &name,
                                                    ))
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .text_size(px(tokens::UI_SM))
                                                            .font_medium()
                                                            .text_color(fg)
                                                            .child(name),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex_shrink_0()
                                                    .font_family(
                                                        cx.theme().mono_font_family.clone(),
                                                    )
                                                    .text_size(px(tokens::UI_XS))
                                                    .text_color(cx.theme().primary)
                                                    .child(format!(":{}", m.line_number)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .w_full()
                                            .pl(px(20.))
                                            .truncate()
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_size(px(tokens::UI_XS))
                                            .text_color(muted)
                                            .child(preview),
                                    )
                                    .on_click(cx.listener(move |this, _e, window, cx| {
                                        this.open_path(path.clone(), window, cx);
                                        if let Some(ed) = this.active_editor().cloned() {
                                            let q = q.clone();
                                            ed.update(cx, |editor, cx| {
                                                editor.set_search_query(&q, cx);
                                                editor.set_cursor_to_position(line_0, 0, cx);
                                            });
                                        }
                                    }))
                            }),
                    ),
            )
    }

    fn render_git_not_a_repo(&self, cx: &mut Context<Self>) -> Div {
        v_flex()
            .flex_1()
            .w_full()
            .items_center()
            .justify_center()
            .gap(px(tokens::SPACE_4))
            .p(px(tokens::SPACE_6))
            .child(Icon::new(IconName::GitBranch).text_color(cx.theme().muted_foreground))
            .child(
                div()
                    .text_center()
                    .text_size(px(tokens::UI_SM))
                    .text_color(cx.theme().muted_foreground)
                    .child("This folder isn't a git repository."),
            )
            .child(
                Button::new("git-init")
                    .small()
                    .label("Initialize Repository")
                    .on_click(cx.listener(|this, _event, _window, cx| this.git_init(cx))),
            )
    }

    fn render_git_body(&self, status: &RepoStatus, cx: &mut Context<Self>) -> Div {
        v_flex()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .child(self.render_git_branch_bar(status, cx))
            .when(!status.is_clean() || self.git.amend, |el| {
                el.child(self.render_git_commit_box(status, cx))
            })
            .child(self.render_git_notice(cx))
            .child(self.render_git_changes(status, cx))
    }

    fn render_git_branch_bar(&self, status: &RepoStatus, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let primary = cx.theme().primary;
        let border = cx.theme().border;
        let busy = self.git.busy.is_some();
        let is_open = self.git.branches_open;
        let name = status
            .branch
            .clone()
            .or_else(|| status.head.as_ref().map(|h| format!("detached @ {h}")))
            .unwrap_or_else(|| "no commits yet".to_string());
        let show_publish =
            status.upstream.is_none() && status.head.is_some() && status.branch.is_some();
        h_flex()
            .w_full()
            .h(px(34.))
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .gap(px(tokens::SPACE_2))
            .px(px(tokens::SPACE_4))
            .border_b_1()
            .border_color(border.opacity(0.6))
            .child(
                h_flex()
                    .id("git-branch-toggle")
                    .min_w(px(0.))
                    .items_center()
                    .gap(px(tokens::SPACE_2))
                    .h(px(24.))
                    .px(px(tokens::SPACE_2))
                    .rounded(px(tokens::RADIUS_SM))
                    .bg(if is_open {
                        tokens::accent_dim()
                    } else {
                        cx.theme().background
                    })
                    .border_1()
                    .border_color(if is_open { primary } else { border })
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().list_hover))
                    .child(
                        Icon::new(IconName::GitBranch)
                            .xsmall()
                            .text_color(if is_open { primary } else { cx.theme().green }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(tokens::UI_SM))
                            .font_medium()
                            .text_color(fg)
                            .child(name),
                    )
                    .child(Icon::new(IconName::ChevronDown).xsmall().text_color(muted))
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.git_toggle_branches(window, cx)
                    })),
            )
            .child(
                // Only what applies: fetch always; pull when behind; push when ahead; publish
                // when the branch has no upstream yet.
                h_flex()
                    .items_center()
                    .flex_shrink_0()
                    .gap(px(tokens::SPACE_2))
                    .when(!status.is_clean(), |el| {
                        el.child(
                            Button::new("git-stash-save")
                                .ghost()
                                .xsmall()
                                .disabled(busy)
                                .icon(
                                    Icon::new(IconName::FolderPlus)
                                        .with_size(px(13.))
                                        .text_color(muted),
                                )
                                .tooltip("Stash uncommitted changes")
                                .on_click(cx.listener(|this, _e, window, cx| {
                                    this.git_stash_save(window, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new("git-fetch")
                            .ghost()
                            .xsmall()
                            .disabled(busy)
                            .icon(
                                Icon::new(IconName::RefreshCw)
                                    .with_size(px(13.))
                                    .text_color(muted),
                            )
                            .tooltip("Fetch")
                            .on_click(
                                cx.listener(|this, _e, window, cx| this.git_fetch(window, cx)),
                            ),
                    )
                    .when(status.behind > 0, |el| {
                        el.child(
                            Button::new("git-pull")
                                .xsmall()
                                .custom(neutral_pill(cx))
                                .disabled(busy)
                                .icon(Icon::new(IconName::ArrowDown).with_size(px(12.)))
                                .label(status.behind.to_string())
                                .tooltip(format!(
                                    "Pull {} commit{} from {}",
                                    status.behind,
                                    if status.behind == 1 { "" } else { "s" },
                                    status.upstream.clone().unwrap_or_default()
                                ))
                                .on_click(
                                    cx.listener(|this, _e, window, cx| this.git_pull(window, cx)),
                                ),
                        )
                    })
                    .when(status.ahead > 0, |el| {
                        el.child(
                            Button::new("git-push")
                                .xsmall()
                                .custom(accent_tint(cx))
                                .disabled(busy)
                                .icon(Icon::new(IconName::ArrowUp).with_size(px(12.)))
                                .label(status.ahead.to_string())
                                .tooltip(format!(
                                    "Push {} commit{} to {}",
                                    status.ahead,
                                    if status.ahead == 1 { "" } else { "s" },
                                    status.upstream.clone().unwrap_or_default()
                                ))
                                .on_click(
                                    cx.listener(|this, _e, window, cx| this.git_push(window, cx)),
                                ),
                        )
                    })
                    .when(show_publish, |el| {
                        el.child(
                            Button::new("git-publish")
                                .xsmall()
                                .custom(accent_tint(cx))
                                .disabled(busy)
                                .icon(Icon::new(IconName::ArrowUp).with_size(px(12.)))
                                .label("Publish")
                                .tooltip("Publish this branch")
                                .on_click(
                                    cx.listener(|this, _e, window, cx| this.git_push(window, cx)),
                                ),
                        )
                    }),
            )
    }

    pub(crate) fn render_branch_switcher_modal(
        &self,
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        if !self.git.branches_open {
            return None;
        }
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let primary = cx.theme().primary;
        let green = cx.theme().green;
        let red = cx.theme().red;
        let info = cx.theme().info;
        let border = cx.theme().border;
        let list_hover = cx.theme().list_hover;

        let raw_query = self.git.branch_input.read(cx).value().trim().to_string();
        let query = raw_query.to_lowercase();
        let filtered: Vec<_> = self
            .git
            .branches
            .iter()
            .filter(|b| query.is_empty() || b.name.to_lowercase().contains(&query))
            .cloned()
            .collect();
        let exact_exists = self
            .git
            .branches
            .iter()
            .any(|b| b.name.eq_ignore_ascii_case(&raw_query));
        let sel_ix = self
            .git
            .branch_selected
            .min(filtered.len().saturating_sub(1));
        let selected_branch_name = filtered.get(sel_ix).map(|b| b.name.clone());
        let filtered_count = filtered.len();
        let has_query = !raw_query.is_empty();
        let confirm_del = self.git.confirm_discard.clone();

        Some(
            div()
                .id("branch-switcher-backdrop")
                .occlude()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(84.))
                .bg(rgba(0x00000066))
                .on_mouse_down(MouseButton::Left, {
                    let view = view.clone();
                    move |_e, _w, cx| {
                        view.update(cx, |this, cx| {
                            this.git.branches_open = false;
                            cx.notify();
                        });
                    }
                })
                .child(
                    v_flex()
                        .id("branch-switcher-card")
                        .w(px(580.))
                        .max_h(px(440.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .overflow_hidden()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                            let key = event.keystroke.key.as_str();
                            match key {
                                "escape" => {
                                    this.git.branches_open = false;
                                    cx.stop_propagation();
                                    cx.notify();
                                }
                                "down" => {
                                    if filtered_count > 0 {
                                        this.git.branch_selected =
                                            (this.git.branch_selected + 1) % filtered_count;
                                        cx.stop_propagation();
                                        cx.notify();
                                    }
                                }
                                "up" => {
                                    if filtered_count > 0 {
                                        this.git.branch_selected = if this.git.branch_selected == 0
                                        {
                                            filtered_count - 1
                                        } else {
                                            this.git.branch_selected - 1
                                        };
                                        cx.stop_propagation();
                                        cx.notify();
                                    }
                                }
                                "enter" => {
                                    if let Some(ref b_name) = selected_branch_name {
                                        this.git
                                            .branch_input
                                            .update(cx, |s, cx| s.set_value("", window, cx));
                                        this.git_checkout(b_name.clone(), window, cx);
                                    } else if has_query {
                                        this.git_create_branch(window, cx);
                                    }
                                    cx.stop_propagation();
                                }
                                _ => {}
                            }
                        }))
                        // Spacious Zed-style Input Header (48px tall, medium input text)
                        .child(
                            h_flex()
                                .h(px(48.))
                                .px(px(16.))
                                .gap(px(10.))
                                .items_center()
                                .border_b_1()
                                .border_color(border)
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .child(
                                            Input::new(&self.git.branch_input).appearance(false),
                                        ),
                                ),
                        )
                        // Scrollable Body
                        .child(
                            v_flex()
                                .id("git-branch-scroll")
                                .p_1p5()
                                .gap(px(tokens::SPACE_1))
                                .overflow_y_scroll()
                                // Prominent "+ Create & switch to branch '<query>'" row when typing a new name
                                .when(!raw_query.is_empty() && !exact_exists, |el| {
                                    let new_name = raw_query.clone();
                                    el.child(
                                        h_flex()
                                            .id("git-create-branch")
                                            .w_full()
                                            .h(px(30.))
                                            .px_3()
                                            .items_center()
                                            .justify_between()
                                            .gap(px(tokens::SPACE_2))
                                            .rounded(px(tokens::RADIUS_SM))
                                            .bg(primary.opacity(0.14))
                                            .border_1()
                                            .border_color(primary.opacity(0.35))
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(primary.opacity(0.24)))
                                            .text_size(px(tokens::UI))
                                            .text_color(primary)
                                            .child(
                                                h_flex()
                                                    .min_w(px(0.))
                                                    .items_center()
                                                    .gap(px(tokens::SPACE_2))
                                                    .child(Icon::new(IconName::Plus).with_size(px(12.)))
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .font_medium()
                                                            .child(format!(
                                                                "Create & switch to branch \"{new_name}\""
                                                            )),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .font_family(cx.theme().mono_font_family.clone())
                                                    .text_size(px(tokens::UI_XS))
                                                    .child("\u{21b5}"),
                                            )
                                            .on_click(cx.listener(|this, _event, window, cx| {
                                                this.git_create_branch(window, cx)
                                            })),
                                    )
                                })
                                .children(filtered.into_iter().enumerate().map(|(ix, branch)| {
                                    let name = branch.name.clone();
                                    let del_name = branch.name.clone();
                                    let current = branch.current;
                                    let is_remote = name.starts_with("origin/");
                                    let is_selected = ix == sel_ix;
                                    let del_key = format!("branch-del:{name}");
                                    let del_armed =
                                        confirm_del.as_deref() == Some(del_key.as_str());
                                    let row_group = format!("git-branch-grp-{ix}");

                                    h_flex()
                                        .id(("git-branch", ix))
                                        .group(row_group.clone())
                                        .relative()
                                        .w_full()
                                        .h(px(30.))
                                        .px_3()
                                        .items_center()
                                        .justify_between()
                                        .gap(px(tokens::SPACE_2))
                                        .rounded(px(tokens::RADIUS_SM))
                                        .border_1()
                                        .border_color(gpui_kit::transparent_black())
                                        .cursor_pointer()
                                        .when(is_selected, |s| {
                                            s.bg(rgba(0xea7a3c16))
                                                .border_color(rgba(0xea7a3c36))
                                                .child(
                                                    div()
                                                        .absolute()
                                                        .left(px(2.))
                                                        .top(px(6.))
                                                        .bottom(px(6.))
                                                        .w(px(2.5))
                                                        .rounded(px(2.))
                                                        .bg(primary),
                                                )
                                        })
                                        .when(!is_selected, |s| s.hover(move |h| h.bg(list_hover)))
                                        .text_size(px(tokens::UI))
                                        .text_color(if current { fg } else { muted })
                                        .child(
                                            h_flex()
                                                .flex_1()
                                                .min_w(px(0.))
                                                .items_center()
                                                .gap(px(tokens::SPACE_2))
                                                .child(
                                                    Icon::new(if current {
                                                        IconName::Check
                                                    } else {
                                                        IconName::GitBranch
                                                    })
                                                    .xsmall()
                                                    .text_color(if current {
                                                        green
                                                    } else if is_remote {
                                                        info
                                                    } else {
                                                        muted
                                                    }),
                                                )
                                                .child(
                                                    div()
                                                        .truncate()
                                                        .when(current, |d| d.font_medium())
                                                        .child(branch.name),
                                                ),
                                        )
                                        .child(
                                            h_flex()
                                                .flex_shrink_0()
                                                .items_center()
                                                .gap(px(tokens::SPACE_2))
                                                .when(is_remote, |r| {
                                                    r.child(
                                                        div()
                                                            .px(px(5.))
                                                            .py(px(1.))
                                                            .rounded(px(3.))
                                                            .bg(info.opacity(0.15))
                                                            .font_family(
                                                                cx.theme().mono_font_family.clone(),
                                                            )
                                                            .text_size(px(9.5))
                                                            .text_color(info)
                                                            .child("remote"),
                                                    )
                                                })
                                                .when(current, |r| {
                                                    r.child(
                                                        div()
                                                            .px(px(5.))
                                                            .py(px(1.))
                                                            .rounded(px(3.))
                                                            .bg(green.opacity(0.15))
                                                            .font_family(
                                                                cx.theme().mono_font_family.clone(),
                                                            )
                                                            .text_size(px(9.5))
                                                            .text_color(green)
                                                            .child("current"),
                                                    )
                                                })
                                                .when(!current && !is_remote, |r| {
                                                    r.child(
                                                        div()
                                                            .id(("git-del-branch", ix))
                                                            .when(!del_armed, |d| {
                                                                d.invisible().group_hover(
                                                                    row_group.clone(),
                                                                    |s| s.visible(),
                                                                )
                                                            })
                                                            .text_size(px(9.5))
                                                            .text_color(if del_armed {
                                                                red
                                                            } else {
                                                                muted
                                                            })
                                                            .hover(move |s| s.text_color(red))
                                                            .child(if del_armed {
                                                                div()
                                                                    .child("Del?")
                                                                    .into_any_element()
                                                            } else {
                                                                Icon::new(IconName::X)
                                                                    .with_size(px(11.))
                                                                    .into_any_element()
                                                            })
                                                            .on_mouse_down(
                                                                MouseButton::Left,
                                                                |_, _, cx| cx.stop_propagation(),
                                                            )
                                                            .on_click(cx.listener(
                                                                move |this, _e, window, cx| {
                                                                    cx.stop_propagation();
                                                                    this.git_delete_branch_clicked(
                                                                        del_name.clone(),
                                                                        window,
                                                                        cx,
                                                                    );
                                                                },
                                                            )),
                                                    )
                                                }),
                                        )
                                        .when(!current, |el| {
                                            el.on_click(cx.listener(
                                                move |this, _event, window, cx| {
                                                    this.git_checkout(name.clone(), window, cx)
                                                },
                                            ))
                                        })
                                })),
                        )
                        // Footer Hints
                        .child(
                            h_flex()
                                .px_3()
                                .py_1p5()
                                .gap_3()
                                .border_t_1()
                                .border_color(border)
                                .text_size(px(tokens::UI_XS))
                                .text_color(muted)
                                .child("\u{2191}\u{2193} Navigate")
                                .child("\u{21b5} Switch / Create")
                                .child("Esc Dismiss"),
                        ),
                ),
        )
    }

    /// Option A commit composer: clean input box with inline `☐ Amend` and `✦ Auto-fill` row,
    /// followed by a full-width primary Commit button (`Commit N files   ⌘↵`).
    fn render_git_commit_box(&self, status: &RepoStatus, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let primary = cx.theme().primary;
        let primary_hover = cx.theme().primary_hover;
        let border = cx.theme().border;
        let staged = status.staged.len();
        let unstaged = status.unstaged.len();
        let message = self.git.message.read(cx).value().to_string();
        let has_message = !message.trim().is_empty();
        let amend = self.git.amend;
        let busy = self.git.busy.is_some();
        let subject_len = message.lines().next().unwrap_or("").chars().count();
        let plural = |n: usize| if n == 1 { "" } else { "s" };

        let button_label = if amend {
            "Amend commit".to_string()
        } else if staged > 0 {
            format!("Commit {staged} file{}", plural(staged))
        } else if unstaged > 0 {
            format!("Commit {unstaged} file{}", plural(unstaged))
        } else {
            "Commit".to_string()
        };
        let can_commit = !busy && (amend || (has_message && (staged + unstaged > 0)));

        let types = self.git.commit_types.clone().unwrap_or_default();
        let suggestion = (!has_message && !amend)
            .then(|| core_workspace::suggest_message(self.commit_candidates(), &types))
            .flatten();

        v_flex()
            .w_full()
            .flex_shrink_0()
            .px(px(tokens::SPACE_4))
            .py(px(tokens::SPACE_3))
            .gap(px(tokens::SPACE_3))
            // Cmd/Ctrl+Enter runs the primary action from inside the message box.
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let m = event.keystroke.modifiers;
                if (m.platform || m.control) && event.keystroke.key == "enter" {
                    this.git_primary_action(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                // Clean input card with inline footer (☐ Amend on left, ✦ Auto-fill on right)
                v_flex()
                    .w_full()
                    .rounded(px(tokens::RADIUS_SM))
                    .border_1()
                    .border_color(border)
                    .bg(cx.theme().background)
                    .overflow_hidden()
                    .child(
                        div()
                            .w_full()
                            .px(px(CARD_INSET))
                            .pt(px(6.))
                            .pb(px(2.))
                            .child(
                                div().mx(px(-8.)).child(
                                    Textarea::new(&self.git.message)
                                        .small()
                                        .appearance(false)
                                        .bordered(false),
                                ),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .h(px(24.))
                            .px(px(CARD_INSET))
                            .pb(px(4.))
                            .items_center()
                            .justify_between()
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(tokens::SPACE_3))
                                    .child(
                                        h_flex()
                                            .id("git-amend")
                                            .items_center()
                                            .gap(px(tokens::SPACE_2))
                                            .cursor_pointer()
                                            .text_size(px(tokens::UI_XS))
                                            .text_color(if amend { primary } else { muted })
                                            .hover(move |s| s.text_color(fg))
                                            .child(
                                                div()
                                                    .size(px(12.))
                                                    .rounded(px(3.))
                                                    .border_1()
                                                    .border_color(if amend {
                                                        primary
                                                    } else {
                                                        border
                                                    })
                                                    .when(amend, |b| b.bg(primary.opacity(0.2)))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .when(amend, |b| {
                                                        b.child(
                                                            Icon::new(IconName::Check)
                                                                .with_size(px(9.))
                                                                .text_color(primary),
                                                        )
                                                    }),
                                            )
                                            .child("Amend")
                                            .on_click(cx.listener(|this, _e, window, cx| {
                                                this.git_toggle_amend(window, cx)
                                            })),
                                    )
                                    .when(subject_len > 50, |el| {
                                        el.child(
                                            div()
                                                .font_family(cx.theme().mono_font_family.clone())
                                                .text_size(px(tokens::UI_XS))
                                                .text_color(if subject_len > 72 {
                                                    cx.theme().red
                                                } else {
                                                    muted
                                                })
                                                .child(format!("{subject_len}/72")),
                                        )
                                    }),
                            )
                            .when_some(suggestion, |el, suggested_text| {
                                el.child(
                                    h_flex()
                                        .id("git-suggestion")
                                        .items_center()
                                        .gap(px(tokens::SPACE_1))
                                        .px(px(tokens::SPACE_2))
                                        .h(px(18.))
                                        .rounded(px(tokens::RADIUS_SM))
                                        .cursor_pointer()
                                        .text_size(px(tokens::UI_XS))
                                        .text_color(primary)
                                        .hover(|s| s.bg(cx.theme().list_hover))
                                        .child(Icon::new(IconName::Sparkles).with_size(px(10.5)))
                                        .child("Auto-fill")
                                        .on_click(cx.listener(move |this, _e, window, cx| {
                                            let _ = &suggested_text;
                                            this.git_apply_suggestion(window, cx)
                                        })),
                                )
                            }),
                    ),
            )
            // Full-width Commit button
            .child(
                h_flex()
                    .id("git-commit")
                    .w_full()
                    .h(px(28.))
                    .px(px(tokens::SPACE_4))
                    .items_center()
                    .justify_center()
                    .gap(px(tokens::SPACE_3))
                    .rounded(px(tokens::RADIUS_SM))
                    .border_1()
                    .border_color(if can_commit {
                        primary
                    } else {
                        border
                    })
                    .bg(if can_commit {
                        primary
                    } else {
                        cx.theme().muted
                    })
                    .text_color(if can_commit {
                        cx.theme().primary_foreground
                    } else {
                        muted
                    })
                    .text_size(px(tokens::UI_SM))
                    .font_medium()
                    .when(can_commit, |el| {
                        el.cursor_pointer()
                            .hover(move |s| s.bg(primary_hover))
                            .on_click(cx.listener(|this, _e, window, cx| {
                                this.git_primary_action(window, cx)
                            }))
                    })
                    .child(button_label)
                    .child(
                        div()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(tokens::UI_XS))
                            .opacity(0.75)
                            .child("\u{2318}\u{21b5}"),
                    ),
            )
    }

    fn render_git_changes(&self, status: &RepoStatus, cx: &mut Context<Self>) -> impl IntoElement {
        let list = v_flex()
            .id("git-changes")
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .overflow_y_scroll()
            .border_t_1()
            .border_color(cx.theme().border)
            .pb(px(tokens::SPACE_4));
        let list = if status.is_clean() {
            list.child(self.render_git_clean_state(status, cx))
        } else {
            list.when(!status.conflicts.is_empty(), |el| {
                el.child(self.render_git_section(
                    "Merge Conflicts",
                    Section::Conflicts,
                    &status.conflicts,
                    false,
                    cx,
                ))
            })
            .when(!status.staged.is_empty(), |el| {
                el.child(self.render_git_section(
                    "Staged Changes",
                    Section::Staged,
                    &status.staged,
                    self.git.staged_collapsed,
                    cx,
                ))
            })
            .when(!status.unstaged.is_empty(), |el| {
                el.child(self.render_git_section(
                    "Changes",
                    Section::Changes,
                    &status.unstaged,
                    self.git.changes_collapsed,
                    cx,
                ))
            })
        };
        list.when(!status.stashes.is_empty(), |el| {
            el.child(self.render_git_stashes(&status.stashes, cx))
        })
        .when(!status.recent_commits.is_empty(), |el| {
            el.child(self.render_git_commits(status, cx))
        })
    }

    /// Nothing to commit: say so, and offer the one sensible next step (push, pull or publish).
    fn render_git_clean_state(&self, status: &RepoStatus, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let busy = self.git.busy.is_some();
        let (text, cta): (String, Option<(&'static str, String, bool)>) =
            if status.upstream.is_none() && status.head.is_some() {
                (
                    "This branch isn't published yet.".to_string(),
                    Some(("git-cta-publish", "Publish branch".to_string(), true)),
                )
            } else if status.ahead > 0 {
                (
                    format!(
                        "{} commit{} to push.",
                        status.ahead,
                        if status.ahead == 1 { "" } else { "s" }
                    ),
                    Some((
                        "git-cta-push",
                        format!(
                            "Push {} commit{}",
                            status.ahead,
                            if status.ahead == 1 { "" } else { "s" }
                        ),
                        true,
                    )),
                )
            } else if status.behind > 0 {
                (
                    format!(
                        "{} new commit{} upstream.",
                        status.behind,
                        if status.behind == 1 { "" } else { "s" }
                    ),
                    Some(("git-cta-pull", format!("Pull {}", status.behind), false)),
                )
            } else {
                ("Working tree clean".to_string(), None)
            };
        v_flex()
            .w_full()
            .items_center()
            .gap(px(tokens::SPACE_3))
            .py(px(tokens::SPACE_6))
            .px(px(tokens::SPACE_5))
            .border_b_1()
            .border_color(cx.theme().border.opacity(0.6))
            .child(
                Icon::new(if cta.is_some() {
                    IconName::GitBranch
                } else {
                    IconName::Check
                })
                .with_size(px(18.))
                .text_color(if cta.is_some() {
                    muted
                } else {
                    cx.theme().green
                }),
            )
            .child(
                div()
                    .text_center()
                    .text_size(px(tokens::UI_SM))
                    .text_color(muted)
                    .child(text),
            )
            .when_some(cta, |el, (id, label, push)| {
                el.child(
                    Button::new(id)
                        .small()
                        .primary()
                        .disabled(busy)
                        .label(label)
                        .on_click(cx.listener(move |this, _event, window, cx| {
                            if push {
                                this.git_push(window, cx);
                            } else {
                                this.git_pull(window, cx);
                            }
                        })),
                )
            })
            .when(status.head.is_some(), |el| {
                el.child(
                    h_flex()
                        .items_center()
                        .gap(px(tokens::SPACE_2))
                        .child(
                            Button::new("git-amend-link")
                                .ghost()
                                .xsmall()
                                .icon(
                                    Icon::new(IconName::Undo2)
                                        .with_size(px(11.))
                                        .text_color(muted),
                                )
                                .label("Amend")
                                .tooltip("Amend last commit")
                                .on_click(cx.listener(|this, _e, window, cx| {
                                    this.git_toggle_amend(window, cx)
                                })),
                        )
                        .child(
                            Button::new("git-uncommit-link")
                                .ghost()
                                .xsmall()
                                .disabled(busy)
                                .icon(
                                    Icon::new(IconName::RotateCcwClock)
                                        .with_size(px(11.))
                                        .text_color(muted),
                                )
                                .label("Undo commit")
                                .tooltip("Soft-reset HEAD~1 and keep changes staged")
                                .on_click(cx.listener(|this, _e, window, cx| {
                                    this.git_uncommit_last(window, cx)
                                })),
                        ),
                )
            })
    }

    fn render_git_section(
        &self,
        title: &'static str,
        section: Section,
        files: &[ChangedFile],
        collapsed: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let muted = cx.theme().muted_foreground;
        let staged = section == Section::Staged;
        let discard_all_armed =
            section == Section::Changes && self.git.confirm_discard.as_deref() == Some("discard-all");
        v_flex()
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .h(px(28.))
                    .items_center()
                    .justify_between()
                    .pl(px(tokens::SPACE_3))
                    .pr(px(tokens::SPACE_2))
                    .child(
                        h_flex()
                            .id(("git-section", section as usize))
                            .flex_1()
                            .h_full()
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .px(px(tokens::SPACE_2))
                            .cursor_pointer()
                            .text_size(px(tokens::UI_XS))
                            .text_color(muted)
                            .font_medium()
                            .hover(|s| s.text_color(cx.theme().foreground))
                            .child(
                                Icon::new(if collapsed {
                                    IconName::ChevronRight
                                } else {
                                    IconName::ChevronDown
                                })
                                .xsmall(),
                            )
                            .child(title)
                            .child(
                                div()
                                    .px(px(5.))
                                    .rounded(px(tokens::RADIUS_PILL))
                                    .bg(cx.theme().list_hover)
                                    .child(files.len().to_string()),
                            )
                            .on_click(cx.listener(move |this, _event, _window, cx| {
                                match section {
                                    Section::Staged => {
                                        this.git.staged_collapsed = !this.git.staged_collapsed
                                    }
                                    Section::Changes => {
                                        this.git.changes_collapsed = !this.git.changes_collapsed
                                    }
                                    Section::Conflicts => {}
                                }
                                cx.notify();
                            })),
                    )
                    .when(section != Section::Conflicts, |el| {
                        el.child(
                            h_flex()
                                .items_center()
                                .gap(px(tokens::SPACE_1))
                                .when(section == Section::Changes, |row| {
                                    row.child(if discard_all_armed {
                                        Button::new("git-discard-all-confirm")
                                            .xsmall()
                                            .danger()
                                            .label("Discard all?")
                                            .on_click(cx.listener(|this, _e, window, cx| {
                                                this.git_discard_all_clicked(window, cx)
                                            }))
                                            .into_any_element()
                                    } else {
                                        Button::new("git-discard-all")
                                            .ghost()
                                            .xsmall()
                                            .icon(
                                                Icon::new(IconName::Undo2)
                                                    .with_size(px(12.))
                                                    .text_color(muted),
                                            )
                                            .tooltip("Discard all unstaged changes")
                                            .on_click(cx.listener(|this, _e, window, cx| {
                                                this.git_discard_all_clicked(window, cx)
                                            }))
                                            .into_any_element()
                                    })
                                })
                                .child(
                                    Button::new(if staged {
                                        "git-unstage-all"
                                    } else {
                                        "git-stage-all"
                                    })
                                    .ghost()
                                    .xsmall()
                                    .icon(
                                        Icon::new(if staged {
                                            IconName::Minus
                                        } else {
                                            IconName::Plus
                                        })
                                        .with_size(px(12.))
                                        .text_color(muted),
                                    )
                                    .tooltip(if staged { "Unstage all" } else { "Stage all" })
                                    .on_click(cx.listener(
                                        move |this, _event, window, cx| {
                                            if staged {
                                                this.git_unstage_all(window, cx);
                                            } else {
                                                this.git_stage_all(window, cx);
                                            }
                                        },
                                    )),
                                ),
                        )
                    }),
            )
            .when(!collapsed, |el| {
                el.children(
                    files
                        .iter()
                        .cloned()
                        .enumerate()
                        .map(|(ix, file)| self.render_git_file_row(section, ix, file, cx)),
                )
            })
    }

    /// One file: icon, name, dimmed folder, and a right-hand slot that shows its status letter
    /// and swaps to the Open / Stage / Unstage / Discard buttons while the row is hovered.
    fn render_git_file_row(
        &self,
        section: Section,
        ix: usize,
        file: ChangedFile,
        cx: &mut Context<Self>,
    ) -> Div {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let color = match file.kind {
            ChangeKind::Added | ChangeKind::Untracked => cx.theme().green,
            ChangeKind::Modified | ChangeKind::TypeChanged => cx.theme().warning,
            ChangeKind::Deleted | ChangeKind::Conflicted => cx.theme().red,
            ChangeKind::Renamed | ChangeKind::Copied => cx.theme().info,
        };
        let row_stat = self
            .git
            .stats
            .get(&file.path)
            .copied()
            .filter(|(added, removed)| added + removed > 0);
        let path = Path::new(&file.path);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&file.path)
            .to_string();
        let mut dir = path
            .parent()
            .and_then(|p| p.to_str())
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        if let Some(orig) = file.orig_path.as_deref() {
            // A rename reads better as where it came from.
            dir = Some(format!("\u{2190} {orig}"));
        }
        let root = self
            .git
            .status
            .as_ref()
            .map(|s| s.root.clone())
            .unwrap_or_else(|| self.root.clone());
        let full_path = root.join(&file.path);
        let open_editor_path = full_path.clone();
        let can_open_file = file.kind != ChangeKind::Deleted;
        // Conflicted files are resolved in the editor; everything else opens its diff.
        let open_in_editor = file.kind == ChangeKind::Conflicted;
        let row_file = file.clone();
        let section_key = match section {
            Section::Conflicts => "conflict",
            Section::Staged => "staged",
            Section::Changes => "change",
        };
        let group = format!("git-row-{section_key}-{ix}");
        let id = (
            match section {
                Section::Conflicts => "git-conflict-row",
                Section::Staged => "git-staged-row",
                Section::Changes => "git-change-row",
            },
            ix,
        );
        let is_active_diff = self.diff.as_ref().is_some_and(|d| {
            d.commit.is_none()
                && d.file.path == file.path
                && d.staged == (section == Section::Staged)
        });
        let discard_armed = self.git.confirm_discard.as_deref() == Some(file.path.as_str());
        let reviewed = self
            .git
            .reviewed
            .contains(&(file.path.clone(), section == Section::Staged));
        let slot_width = if section == Section::Changes {
            72.
        } else {
            48.
        };
        let action = |suffix: &'static str, icon: IconName, tip: &'static str| {
            Button::new((suffix, ix))
                .ghost()
                .xsmall()
                .icon(Icon::new(icon).with_size(px(12.)).text_color(muted))
                .tooltip(tip)
        };
        let stage_file = file.clone();
        let discard_file = file.clone();
        let unstage_file = file.clone();

        div()
            .w_full()
            .h(px(tokens::TREE_ROW_H))
            .px(px(tokens::SPACE_3))
            .flex()
            .items_center()
            .child(
                h_flex()
                    .id(id)
                    .group(group.clone())
                    .relative()
                    .w_full()
                    .h(px(26.))
                    .items_center()
                    .gap(px(tokens::SPACE_2))
                    .pl(px(tokens::SPACE_4))
                    .pr(px(tokens::SPACE_2))
                    .rounded(px(tokens::RADIUS_SM))
                    .border_1()
                    .border_color(gpui_kit::transparent_black())
                    .cursor_pointer()
                    .when(is_active_diff, |el| {
                        el.bg(tokens::accent_dim())
                            .border_color(cx.theme().list_active_border)
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
                    .when(!is_active_diff, |el| {
                        el.hover(|s| s.bg(cx.theme().list_hover))
                    })
                    .child(crate::rails::files::render_file_icon(&name))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w(px(0.))
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(tokens::UI_SM))
                                    .text_color(if file.kind == ChangeKind::Deleted {
                                        muted
                                    } else {
                                        fg
                                    })
                                    .when(file.kind == ChangeKind::Deleted, |el| el.line_through())
                                    .child(name),
                            )
                            .when_some(dir, |el, dir| {
                                el.child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .truncate()
                                        .text_size(px(tokens::UI_XS))
                                        .text_color(muted.opacity(0.7))
                                        .child(dir),
                                )
                            }),
                    )
                    .when_some(row_stat, |el, (added, removed)| {
                        el.child(crate::diff_view::stat_chip(added, removed, cx))
                    })
                    .when(reviewed, |el| {
                        el.child(
                            Icon::new(IconName::Check)
                                .with_size(px(11.))
                                .text_color(cx.theme().green.opacity(0.8)),
                        )
                    })
                    .child(
                        div()
                            .relative()
                            .flex_shrink_0()
                            .w(px(slot_width))
                            .h_full()
                            .child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .pr(px(tokens::SPACE_2))
                                    .text_size(px(10.))
                                    .font_medium()
                                    .text_color(color)
                                    .group_hover(group.clone(), |s| s.invisible())
                                    .child(file.kind.letter().to_string()),
                            )
                            .when(section != Section::Conflicts, |el| {
                                el.child(
                                    h_flex()
                                        .absolute()
                                        .inset_0()
                                        .items_center()
                                        .justify_end()
                                        .invisible()
                                        .group_hover(group.clone(), |s| s.visible())
                                        .when(can_open_file && !discard_armed, |row| {
                                            row.child(
                                                action(
                                                    "git-open-file",
                                                    IconName::FileCode,
                                                    "Open file",
                                                )
                                                .on_click(cx.listener(
                                                    move |this, _event, window, cx| {
                                                        this.open_path(
                                                            open_editor_path.clone(),
                                                            window,
                                                            cx,
                                                        );
                                                    },
                                                )),
                                            )
                                        })
                                        .when(section == Section::Changes, |row| {
                                            row.child(if discard_armed {
                                                Button::new(("git-discard-confirm", ix))
                                                    .xsmall()
                                                    .danger()
                                                    .label("Discard?")
                                                    .on_click(cx.listener(
                                                        move |this, _event, window, cx| {
                                                            this.git_discard_clicked(
                                                                discard_file.clone(),
                                                                window,
                                                                cx,
                                                            )
                                                        },
                                                    ))
                                                    .into_any_element()
                                            } else {
                                                action(
                                                    "git-discard",
                                                    IconName::Undo2,
                                                    "Discard changes",
                                                )
                                                .on_click(cx.listener(
                                                    move |this, _event, window, cx| {
                                                        this.git_discard_clicked(
                                                            discard_file.clone(),
                                                            window,
                                                            cx,
                                                        )
                                                    },
                                                ))
                                                .into_any_element()
                                            })
                                        })
                                        .when(section == Section::Changes && !discard_armed, |row| {
                                            row.child(
                                                action("git-stage", IconName::Plus, "Stage")
                                                    .on_click(cx.listener(
                                                        move |this, _event, window, cx| {
                                                            this.git_stage(
                                                                std::slice::from_ref(&stage_file),
                                                                window,
                                                                cx,
                                                            )
                                                        },
                                                    )),
                                            )
                                        })
                                        .when(section == Section::Staged, |row| {
                                            row.child(
                                                action("git-unstage", IconName::Minus, "Unstage")
                                                    .on_click(cx.listener(
                                                        move |this, _event, window, cx| {
                                                            this.git_unstage(
                                                                std::slice::from_ref(&unstage_file),
                                                                window,
                                                                cx,
                                                            )
                                                        },
                                                    )),
                                            )
                                        }),
                                )
                            }),
                    )
                    .on_click(cx.listener(move |this, _event, window, cx| {
                        if open_in_editor {
                            this.open_path(full_path.clone(), window, cx);
                        } else {
                            this.open_git_diff(row_file.clone(), section == Section::Staged, cx);
                        }
                    })),
            )
    }

    fn render_git_stashes(&self, stashes: &[StashEntry], cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let collapsed = self.git.stashes_collapsed;
        v_flex()
            .w_full()
            .border_t_1()
            .border_color(cx.theme().border.opacity(0.6))
            .child(
                h_flex()
                    .id("git-section-stashes")
                    .w_full()
                    .h(px(28.))
                    .items_center()
                    .gap(px(tokens::SPACE_2))
                    .pl(px(tokens::SPACE_5))
                    .pr(px(tokens::SPACE_3))
                    .cursor_pointer()
                    .text_size(px(tokens::UI_XS))
                    .text_color(muted)
                    .font_medium()
                    .hover(|s| s.text_color(fg))
                    .child(
                        Icon::new(if collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .xsmall(),
                    )
                    .child("Stashes")
                    .child(
                        div()
                            .px(px(5.))
                            .rounded(px(tokens::RADIUS_PILL))
                            .bg(cx.theme().list_hover)
                            .child(stashes.len().to_string()),
                    )
                    .on_click(cx.listener(|this, _e, _w, cx| {
                        this.git.stashes_collapsed = !this.git.stashes_collapsed;
                        cx.notify();
                    })),
            )
            .when(!collapsed, |el| {
                el.children(stashes.iter().map(|stash| {
                    let index = stash.index;
                    let group = format!("git-stash-{index}");
                    let drop_key = format!("stash-drop:{index}");
                    let drop_armed =
                        self.git.confirm_discard.as_deref() == Some(drop_key.as_str());
                    div()
                        .w_full()
                        .h(px(tokens::TREE_ROW_H))
                        .px(px(tokens::SPACE_3))
                        .flex()
                        .items_center()
                        .child(
                            h_flex()
                                .id(("git-stash-row", index))
                                .group(group.clone())
                                .w_full()
                                .h(px(26.))
                                .pl(px(tokens::SPACE_4))
                                .pr(px(tokens::SPACE_2))
                                .items_center()
                                .justify_between()
                                .gap(px(tokens::SPACE_2))
                                .rounded(px(tokens::RADIUS_SM))
                                .hover(|s| s.bg(cx.theme().list_hover))
                                .child(
                                    h_flex()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .items_center()
                                        .gap(px(tokens::SPACE_2))
                                        .child(
                                            div()
                                                .flex_shrink_0()
                                                .font_family(cx.theme().mono_font_family.clone())
                                                .text_size(px(tokens::UI_XS))
                                                .text_color(cx.theme().info)
                                                .child(format!("#{index}")),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(px(tokens::UI_SM))
                                                .text_color(fg)
                                                .child(stash.message.clone()),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .flex_shrink_0()
                                        .items_center()
                                        .gap(px(tokens::SPACE_1))
                                        .child(
                                            Button::new(("git-stash-pop", index))
                                                .ghost()
                                                .xsmall()
                                                .label("Pop")
                                                .tooltip("Apply and remove stash")
                                                .on_click(cx.listener(move |this, _e, window, cx| {
                                                    this.git_stash_pop(index, window, cx)
                                                })),
                                        )
                                        .child(
                                            Button::new(("git-stash-apply", index))
                                                .ghost()
                                                .xsmall()
                                                .icon(
                                                    Icon::new(IconName::Check)
                                                        .with_size(px(11.))
                                                        .text_color(muted),
                                                )
                                                .tooltip("Apply stash (keep in list)")
                                                .on_click(cx.listener(move |this, _e, window, cx| {
                                                    this.git_stash_apply(index, window, cx)
                                                })),
                                        )
                                        .child(if drop_armed {
                                            Button::new(("git-stash-drop-confirm", index))
                                                .xsmall()
                                                .danger()
                                                .label("Drop?")
                                                .on_click(cx.listener(move |this, _e, window, cx| {
                                                    this.git_stash_drop_clicked(index, window, cx)
                                                }))
                                                .into_any_element()
                                        } else {
                                            Button::new(("git-stash-drop", index))
                                                .ghost()
                                                .xsmall()
                                                .icon(
                                                    Icon::new(IconName::X)
                                                        .with_size(px(11.))
                                                        .text_color(muted),
                                                )
                                                .tooltip("Drop stash")
                                                .on_click(cx.listener(move |this, _e, window, cx| {
                                                    this.git_stash_drop_clicked(index, window, cx)
                                                }))
                                                .into_any_element()
                                        }),
                                ),
                        )
                }))
            })
    }

    fn render_git_commits(&self, status: &RepoStatus, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let collapsed = self.git.commits_collapsed;
        let has_upstream = status.upstream.is_some();
        let busy = self.git.busy.is_some();

        v_flex()
            .w_full()
            .border_t_1()
            .border_color(cx.theme().border.opacity(0.6))
            .child(
                h_flex()
                    .w_full()
                    .h(px(28.))
                    .items_center()
                    .justify_between()
                    .pl(px(tokens::SPACE_3))
                    .pr(px(tokens::SPACE_2))
                    .child(
                        h_flex()
                            .id("git-section-commits")
                            .flex_1()
                            .h_full()
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .px(px(tokens::SPACE_2))
                            .cursor_pointer()
                            .text_size(px(tokens::UI_XS))
                            .text_color(muted)
                            .font_medium()
                            .hover(|s| s.text_color(fg))
                            .child(
                                Icon::new(if collapsed {
                                    IconName::ChevronRight
                                } else {
                                    IconName::ChevronDown
                                })
                                .xsmall(),
                            )
                            .child("Recent Commits")
                            .child(
                                div()
                                    .px(px(5.))
                                    .rounded(px(tokens::RADIUS_PILL))
                                    .bg(cx.theme().list_hover)
                                    .child(status.recent_commits.len().to_string()),
                            )
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.git.commits_collapsed = !this.git.commits_collapsed;
                                cx.notify();
                            })),
                    )
                    .when(!status.recent_commits.is_empty(), |el| {
                        el.child(
                            Button::new("git-undo-last-commit")
                                .ghost()
                                .xsmall()
                                .disabled(busy)
                                .icon(
                                    Icon::new(IconName::Undo2)
                                        .with_size(px(12.))
                                        .text_color(muted),
                                )
                                .tooltip("Undo last commit (keep changes staged)")
                                .on_click(cx.listener(|this, _e, window, cx| {
                                    this.git_uncommit_last(window, cx)
                                })),
                        )
                    }),
            )
            .when(!collapsed, |el| {
                el.children(
                    status
                        .recent_commits
                        .iter()
                        .enumerate()
                        .map(|(ix, commit)| self.render_git_commit_row(ix, commit, has_upstream, cx)),
                )
            })
    }

    fn render_git_commit_row(
        &self,
        ix: usize,
        commit: &CommitInfo,
        has_upstream: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let expanded = self.git.expanded_commit.as_deref() == Some(commit.hash.as_str());
        let hash = commit.hash.clone();
        let unpushed = commit.unpushed && has_upstream;

        v_flex()
            .w_full()
            .child(
                div()
                    .w_full()
                    .h(px(tokens::TREE_ROW_H))
                    .px(px(tokens::SPACE_3))
                    .flex()
                    .items_center()
                    .child(
                        h_flex()
                            .id(("git-commit-row", ix))
                            .w_full()
                            .h(px(26.))
                            .px(px(tokens::SPACE_3))
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .rounded(px(tokens::RADIUS_SM))
                            .cursor_pointer()
                            .when(expanded, |s| s.bg(cx.theme().list_hover))
                            .hover(|s| s.bg(cx.theme().list_hover))
                            .child(
                                Icon::new(if expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .xsmall()
                                .text_color(muted),
                            )
                            .when(unpushed, |el| {
                                el.child(
                                    div()
                                        .text_size(px(tokens::UI_XS))
                                        .text_color(cx.theme().primary)
                                        .child("\u{2191}"),
                                )
                            })
                            .child({
                                let copy_hash = commit.hash.clone();
                                div()
                                    .id(("git-copy-sha", ix))
                                    .flex_shrink_0()
                                    .px(px(4.))
                                    .rounded(px(3.))
                                    .bg(cx.theme().background)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(if unpushed {
                                        cx.theme().primary
                                    } else {
                                        cx.theme().info
                                    })
                                    .hover(|s| s.border_color(cx.theme().primary))
                                    .child(commit.hash.clone())
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .on_click(cx.listener(move |this, _e, _w, cx| {
                                        cx.stop_propagation();
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copy_hash.clone(),
                                        ));
                                        this.git.notice = Some(crate::git_panel::GitNotice {
                                            text: format!("Copied commit {copy_hash}"),
                                            is_error: false,
                                        });
                                        cx.notify();
                                    }))
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_size(px(tokens::UI_SM))
                                    .text_color(fg)
                                    .child(commit.subject.clone()),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(muted.opacity(0.75))
                                    .child(commit.relative_time.clone()),
                            )
                            .on_click(cx.listener(move |this, _e, _w, cx| {
                                this.git_toggle_commit(hash.clone(), cx);
                            })),
                    ),
            )
            .when(expanded, |el| {
                let commit_hash = commit.hash.clone();
                el.children(
                    self.git
                        .expanded_commit_files
                        .iter()
                        .cloned()
                        .enumerate()
                        .map(|(file_ix, file)| {
                            let color = match file.kind {
                                ChangeKind::Added | ChangeKind::Untracked => cx.theme().green,
                                ChangeKind::Modified | ChangeKind::TypeChanged => {
                                    cx.theme().warning
                                }
                                ChangeKind::Deleted | ChangeKind::Conflicted => cx.theme().red,
                                ChangeKind::Renamed | ChangeKind::Copied => cx.theme().info,
                            };
                            let name = Path::new(&file.path)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or(&file.path)
                                .to_string();
                            let is_active = self.diff.as_ref().is_some_and(|d| {
                                d.commit.as_deref() == Some(commit_hash.as_str())
                                    && d.file.path == file.path
                            });
                            let click_hash = commit_hash.clone();
                            let click_file = file.clone();

                            div()
                                .w_full()
                                .h(px(24.))
                                .px(px(tokens::SPACE_3))
                                .flex()
                                .items_center()
                                .child(
                                    h_flex()
                                        .id(("git-commit-file", file_ix))
                                        .relative()
                                        .w_full()
                                        .h(px(22.))
                                        .pl(px(tokens::SPACE_6))
                                        .pr(px(tokens::SPACE_3))
                                        .items_center()
                                        .justify_between()
                                        .gap(px(tokens::SPACE_2))
                                        .rounded(px(tokens::RADIUS_SM))
                                        .cursor_pointer()
                                        .when(is_active, |s| s.bg(tokens::accent_dim()))
                                        .when(!is_active, |s| {
                                            s.hover(|h| h.bg(cx.theme().list_hover))
                                        })
                                        .child(
                                            h_flex()
                                                .flex_1()
                                                .min_w(px(0.))
                                                .items_center()
                                                .gap(px(tokens::SPACE_2))
                                                .child(crate::rails::files::render_file_icon(&name))
                                                .child(
                                                    div()
                                                        .truncate()
                                                        .text_size(px(tokens::UI_XS))
                                                        .text_color(fg)
                                                        .child(name),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(10.))
                                                .font_medium()
                                                .text_color(color)
                                                .child(file.kind.letter().to_string()),
                                        )
                                        .on_click(cx.listener(move |this, _e, _w, cx| {
                                            this.open_git_commit_diff(
                                                click_hash.clone(),
                                                click_file.clone(),
                                                cx,
                                            );
                                        })),
                                )
                        }),
                )
            })
    }

    /// What just happened (or is happening), right under the commit box where the eye already is.
    fn render_git_notice(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let card = h_flex()
            .w_full()
            .flex_shrink_0()
            .items_start()
            .gap(px(tokens::SPACE_3))
            .mx(px(tokens::SPACE_4))
            .mb(px(tokens::SPACE_4))
            .px(px(tokens::SPACE_4))
            .py(px(tokens::SPACE_3))
            .rounded(px(tokens::RADIUS_SM))
            .text_size(px(tokens::UI_XS))
            .w(relative(1.));
        // `mx` on a full-width child would overflow; size against the padded column instead.
        let card = card.w_auto();
        if let Some(label) = self.git.busy {
            return card
                .bg(cx.theme().list_hover)
                .text_color(muted)
                .child(Icon::new(IconName::RefreshCw).xsmall())
                .child(label)
                .into_any_element();
        }
        match self.git.notice.clone() {
            Some(notice) => {
                let tone = if notice.is_error {
                    cx.theme().red
                } else {
                    cx.theme().green
                };
                card.id("git-notice")
                    .cursor_pointer()
                    .bg(tone.opacity(0.12))
                    .border_1()
                    .border_color(tone.opacity(0.3))
                    .text_color(cx.theme().foreground)
                    .child(
                        Icon::new(if notice.is_error {
                            IconName::X
                        } else {
                            IconName::Check
                        })
                        .xsmall()
                        .text_color(tone),
                    )
                    .child(div().flex_1().min_w(px(0.)).child(notice.text))
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.git.notice = None;
                        cx.notify();
                    }))
                    .into_any_element()
            }
            None => div().h(px(0.)).into_any_element(),
        }
    }
}
