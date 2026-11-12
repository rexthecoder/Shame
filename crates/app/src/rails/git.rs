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
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
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

/// Right-click menu on a changed-file row, as in VS Code's Source Control view.
/// Discard is left to the row's own two-step button, so a stray click in a menu
/// can't throw work away.
fn git_file_context_menu(
    menu: PopupMenu,
    view: Entity<DartIde>,
    file: ChangedFile,
    section: Section,
    root: &Path,
) -> PopupMenu {
    let full = root.join(&file.path);
    let abs = full.to_string_lossy().to_string();
    let rel = file.path.clone();
    let can_open_file = file.kind != ChangeKind::Deleted;
    let staged = section == Section::Staged;
    let mut menu = menu;
    if section != Section::Conflicts {
        menu = menu.item(PopupMenuItem::new("Open Changes").on_click({
            let (view, file) = (view.clone(), file.clone());
            move |_, _, cx| view.update(cx, |this, cx| this.open_git_diff(file.clone(), staged, cx))
        }));
    }
    menu = menu.item(PopupMenuItem::new("Open File").disabled(!can_open_file).on_click({
        let (view, full) = (view.clone(), full.clone());
        move |_, window, cx| view.update(cx, |this, cx| this.open_path(full.clone(), window, cx))
    }));
    match section {
        Section::Changes => {
            menu = menu.separator().item(PopupMenuItem::new("Stage Changes").on_click({
                let (view, file) = (view.clone(), file.clone());
                move |_, window, cx| {
                    view.update(cx, |this, cx| this.git_stage(std::slice::from_ref(&file), window, cx))
                }
            }));
        }
        Section::Staged => {
            menu = menu.separator().item(PopupMenuItem::new("Unstage Changes").on_click({
                let (view, file) = (view.clone(), file.clone());
                move |_, window, cx| {
                    view.update(cx, |this, cx| this.git_unstage(std::slice::from_ref(&file), window, cx))
                }
            }));
        }
        Section::Conflicts => {}
    }
    menu.separator()
        .item(PopupMenuItem::new("Copy Path").on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(abs.clone()));
        }))
        .item(PopupMenuItem::new("Copy Relative Path").on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(rel.clone()));
        }))
        .item(PopupMenuItem::new("Reveal in Finder").on_click(move |_, _, _| {
            let _ = std::process::Command::new("open").arg("-R").arg(&full).spawn();
        }))
}

/// Right-click menu on a commit row: the things you reach for when talking
/// about a commit (its hash, its message) plus expanding its files.
fn git_commit_context_menu(
    menu: PopupMenu,
    view: Entity<DartIde>,
    commit: &CommitInfo,
    expanded: bool,
) -> PopupMenu {
    let hash = commit.hash.clone();
    let view_for_revert = view.clone();
    let short: String = commit.hash.chars().take(7).collect();
    let subject = commit.subject.clone();
    menu.item(PopupMenuItem::new(if expanded { "Hide Files" } else { "Show Files" }).on_click({
        let hash = hash.clone();
        move |_, _, cx| view.update(cx, |this, cx| this.git_toggle_commit(hash.clone(), cx))
    }))
    .separator()
    .item(PopupMenuItem::new("Copy Commit Hash").on_click({
        let hash = hash.clone();
        move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(hash.clone()))
    }))
    .item(PopupMenuItem::new(format!("Copy Short Hash ({short})")).on_click(move |_, _, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(short.clone()))
    }))
    .item(PopupMenuItem::new("Copy Message").on_click(move |_, _, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(subject.clone()))
    }))
    .separator()
    .item(PopupMenuItem::new("Revert Commit").on_click({
        let view = view_for_revert;
        move |_, window, cx| view.update(cx, |this, cx| this.git_revert_commit(hash.clone(), window, cx))
    }))
}

/// Where a commit falls in the timeline, from git's relative date
/// ("3 hours ago", "2 days ago", "3 weeks ago", ...).
fn time_bucket(relative: &str) -> &'static str {
    let mut words = relative.split_whitespace();
    let n: u32 = words.next().and_then(|w| w.parse().ok()).unwrap_or(0);
    let unit = words.next().unwrap_or("");
    match unit.trim_end_matches('s') {
        "second" | "minute" | "hour" => "Today",
        "day" if n <= 1 => "Yesterday",
        "day" => "This week",
        "week" if n <= 1 => "Last week",
        "week" => "This month",
        "month" => "Earlier this year",
        _ => "Older",
    }
}

/// A timeline heading between groups of commits: a small label on a hairline.
fn commit_bucket_heading(label: &'static str, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    h_flex()
        .w_full()
        .items_center()
        .gap(px(tokens::SPACE_3))
        .pl(px(tokens::SPACE_5 + 18.))
        .pr(px(tokens::SPACE_5))
        .pt(px(tokens::SPACE_4))
        .pb(px(tokens::SPACE_1))
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(tokens::UI_XS - 0.5))
                .font_medium()
                .text_color(muted.opacity(0.75))
                .child(label),
        )
        .child(div().flex_1().h(px(1.)).bg(cx.theme().border.opacity(0.5)))
}

/// Initials in a small round chip, tinted from the name so each author keeps
/// one colour.
fn author_avatar(author: &str) -> Div {
    let initials: String = author
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase();
    let hue = author.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) % 360;
    let tint = hsla(hue as f32 / 360., 0.55, 0.62, 1.);
    div()
        .flex_shrink_0()
        .size(px(14.))
        .rounded(px(tokens::RADIUS_PILL))
        .flex()
        .items_center()
        .justify_center()
        .bg(tint.opacity(0.18))
        .text_color(tint)
        .text_size(px(7.5))
        .font_medium()
        .child(initials)
}

/// "↑2" / "↓1" beside the branch name.
fn sync_count(arrow: &'static str, n: u32, tint: Hsla, cx: &App) -> Div {
    div()
        .flex_shrink_0()
        .px(px(tokens::SPACE_2))
        .rounded(px(tokens::RADIUS_PILL))
        .bg(tint.opacity(0.14))
        .text_color(tint)
        .text_size(px(tokens::UI_XS))
        .font_family(cx.theme().mono_font_family.clone())
        .child(format!("{arrow}{n}"))
}

/// The added/removed split of a section's lines as one thin bar.
fn churn_bar(added: usize, removed: usize, cx: &App) -> Div {
    let total = (added + removed).max(1) as f32;
    h_flex()
        .mx(px(tokens::SPACE_5))
        .mb(px(tokens::SPACE_2))
        .h(px(2.))
        .rounded(px(tokens::RADIUS_PILL))
        .overflow_hidden()
        .bg(cx.theme().border.opacity(0.4))
        .when(added > 0, |bar| {
            bar.child(div().h_full().w(relative(added as f32 / total)).bg(cx.theme().green.opacity(0.75)))
        })
        .when(removed > 0, |bar| {
            bar.child(div().h_full().w(relative(removed as f32 / total)).bg(cx.theme().red.opacity(0.75)))
        })
}

/// Horizontal inset shared by every row inside the commit card, so their edges line up.
const CARD_INSET: f32 = 10.;

/// A soft orange pill: the accent as a tint, for secondary calls to action (push, publish).
pub(crate) fn accent_tint(cx: &App) -> ButtonCustomVariant {
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

/// The history keeps at least this much room while a section is open, so a
/// long change list can never bury stashes or recent commits.
const GIT_HISTORY_MIN: f32 = 140.;
/// Two collapsed section rows (Stashes, Recent commits).
const GIT_HISTORY_HEADERS_H: f32 = 56.;

impl DartIde {
    /// The "Files | Search | Git" tab strip at the top of the left sidebar, shared by all views.
    pub(crate) fn render_sidebar_tabs(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let primary = cx.theme().primary;
        let strong = cx.theme().tab_active_foreground;
        let border = cx.theme().border;

        let changes = self
            .git
            .status
            .as_ref()
            .map_or(0, |s| s.staged.len() + s.unstaged.len() + s.conflicts.len());

        let tab = |id: &'static str,
                   label: &'static str,
                   icon: IconName,
                   which: SidebarTab,
                   show_badge: bool,
                   cx: &mut Context<Self>| {
            let active = self.sidebar_tab == which;
            h_flex()
                .id(id)
                .relative()
                .items_center()
                .gap(px(tokens::SPACE_2))
                .h(px(36.))
                .cursor_pointer()
                .child(
                    Icon::new(icon)
                        .small()
                        .text_color(if active { primary } else { muted }),
                )
                .child(
                    div()
                        .text_size(px(tokens::UI_SM))
                        .when(active, |el| el.font_medium().text_color(strong))
                        .when(!active, |el| {
                            el.text_color(muted).hover(|h| h.text_color(fg))
                        })
                        .child(label),
                )
                .when(show_badge && changes > 0, |el| {
                    el.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .h(px(15.))
                            .min_w(px(15.))
                            .px(px(3.))
                            .rounded(px(tokens::RADIUS_BADGE))
                            .bg(primary.opacity(0.15))
                            .text_size(px(9.5))
                            .line_height(px(9.5))
                            .font_medium()
                            .text_color(primary)
                            .child(changes.to_string()),
                    )
                })
                .when(active, |el| {
                    el.child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom(px(-1.))
                            .h(px(2.))
                            .rounded(px(2.))
                            .bg(primary),
                    )
                })
                .on_click(
                    cx.listener(move |this, _event, _window, cx| this.set_sidebar_tab(which, cx)),
                )
        };

        h_flex()
            .w_full()
            .h(px(36.))
            .flex_shrink_0()
            .items_center()
            .gap(px(tokens::SPACE_6))
            .px(px(tokens::SPACE_5))
            .border_b_1()
            .border_color(border)
            .child(tab(
                "sidebar-tab-files",
                "Files",
                IconName::Folder,
                SidebarTab::Files,
                false,
                cx,
            ))
            .child(tab(
                "sidebar-tab-search",
                "Search",
                IconName::Search,
                SidebarTab::Search,
                false,
                cx,
            ))
            .child(tab(
                "sidebar-tab-git",
                "Git",
                IconName::GitBranch,
                SidebarTab::Git,
                true,
                cx,
            ))
    }

    pub(crate) fn render_git_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // Below the sidebar tabs: fill what is left, not the whole card, or
        // the bottom (the commit button) is pushed out of view.
        v_flex().flex_1().w_full().min_h(px(0.)).child(match self.git.status.clone() {
            Some(status) => self.render_git_body(&status, cx).into_any_element(),
            None => self.render_git_not_a_repo(cx).into_any_element(),
        })
    }

    pub(crate) fn render_search_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let border = cx.theme().border;
        let query = self
            .project_search_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        let (_, filters) = core_workspace::split_query_filters(&query);
        let options = self.project_search_options;
        let replace_open = self.project_replace_open;

        // A field: bordered row with a leading icon, the input, trailing bits.
        let field_bg = cx.theme().background;
        let field = |leading: IconName| {
            h_flex()
                .w_full()
                .h(px(28.))
                .pl(px(tokens::SPACE_2))
                .pr(px(3.))
                .gap(px(tokens::SPACE_2))
                .items_center()
                .rounded(px(tokens::RADIUS_SM))
                .bg(field_bg)
                .border_1()
                .border_color(border)
                .child(Icon::new(leading).with_size(px(13.)).text_color(muted))
        };
        let toggle =
            |id: &'static str,
             icon: IconName,
             tip: &'static str,
             on: bool,
             flip: fn(&mut core_workspace::SearchOptions),
             cx: &mut Context<Self>| {
                div()
                    .id(id)
                    .size(px(20.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(tokens::RADIUS_SM))
                    .cursor_pointer()
                    .when(on, |el| {
                        el.bg(tokens::accent_dim())
                            .border_1()
                            .border_color(cx.theme().primary.opacity(0.5))
                    })
                    .when(!on, |el| el.hover(|s| s.bg(cx.theme().list_hover)))
                    .child(Icon::new(icon).with_size(px(13.)).text_color(if on {
                        cx.theme().primary
                    } else {
                        muted
                    }))
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tip).build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _e, _w, cx| {
                        this.toggle_project_search_option(flip, cx)
                    }))
            };

        let search_state = self.project_search_input.clone();
        let search_field = field(IconName::Search)
            .child(
                div().flex_1().min_w(px(0.)).child(
                    Input::new(&self.project_search_input)
                        .appearance(false)
                        .small()
                        // A single-line input drops pasted newlines, gluing
                        // `sends///` together. Keep a block as separate words so
                        // the multiline matcher sees the same text the file has.
                        .on_paste(move |item, window, cx| {
                            let Some(text) = item.text() else {
                                return false;
                            };
                            if !text.contains(['\n', '\r']) {
                                return false;
                            }
                            let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
                            search_state.update(cx, |state, cx| state.replace(cleaned, window, cx));
                            true
                        }),
                ),
            )
            .child(toggle(
                "search-opt-case",
                IconName::CaseSensitive,
                "Match case",
                options.case_sensitive,
                |o| o.case_sensitive = !o.case_sensitive,
                cx,
            ))
            .child(toggle(
                "search-opt-word",
                IconName::WholeWord,
                "Match whole word",
                options.whole_word,
                |o| o.whole_word = !o.whole_word,
                cx,
            ))
            .child(toggle(
                "search-opt-regex",
                IconName::Regex,
                "Use regular expression",
                options.regex,
                |o| o.regex = !o.regex,
                cx,
            ))
            .child(
                div()
                    .w(px(1.))
                    .h(px(14.))
                    .flex_shrink_0()
                    .bg(border),
            )
            .child(
                div()
                    .id("project-replace-toggle")
                    .size(px(20.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(tokens::RADIUS_SM))
                    .cursor_pointer()
                    .when(replace_open, |el| {
                        el.bg(tokens::accent_dim())
                            .border_1()
                            .border_color(cx.theme().primary.opacity(0.5))
                    })
                    .when(!replace_open, |el| el.hover(|s| s.bg(cx.theme().list_hover)))
                    .child(Icon::new(IconName::Replace).with_size(px(12.)).text_color(
                        if replace_open {
                            cx.theme().primary
                        } else {
                            muted
                        },
                    ))
                    .tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new("Toggle replace")
                            .build(window, cx)
                    })
                    .on_click(cx.listener(|this, _e, window, cx| {
                        this.project_replace_open = !this.project_replace_open;
                        if this.project_replace_open {
                            this.project_replace_input
                                .update(cx, |state, cx| state.focus(window, cx));
                        }
                        cx.notify();
                    })),
            );

        let replace_field = field(IconName::Replace)
            .child(
                div().flex_1().min_w(px(0.)).child(
                    Input::new(&self.project_replace_input)
                        .appearance(false)
                        .small(),
                ),
            )
            .child(
                Button::new("project-replace-all")
                    .ghost()
                    .xsmall()
                    .icon(
                        Icon::new(IconName::ReplaceAll)
                            .with_size(px(13.))
                            .text_color(fg),
                    )
                    .tooltip("Replace all shown results")
                    .on_click(cx.listener(|this, _e, _w, cx| this.replace_in_project(None, cx))),
            );

        let header = v_flex()
            .w_full()
            .px(px(tokens::PANEL_INSET))
            .py(px(tokens::SPACE_4))
            .gap(px(tokens::SPACE_2))
            .border_b_1()
            .border_color(border)
            .child(
                v_flex()
                    .w_full()
                    .gap(px(tokens::SPACE_3))
                    .child(search_field)
                    .when(replace_open, |el| el.child(replace_field)),
            )
            .when(!filters.is_empty(), |el| {
                el.child(
                    h_flex()
                        .w_full()
                        .pl(px(20.))
                        .flex_wrap()
                        .gap(px(tokens::SPACE_1))
                        .children(filters.iter().enumerate().map(|(ix, token)| {
                            let token = token.clone();
                            let remove = token.clone();
                            self.render_search_chip(("search-filter", ix), token, true, cx)
                                .on_click(cx.listener(move |this, _e, window, cx| {
                                    let query =
                                        this.project_search_input.read(cx).value().to_string();
                                    let mut removed = false;
                                    let rest: Vec<&str> = query
                                        .split_whitespace()
                                        .filter(|t| {
                                            let drop = !removed && *t == remove;
                                            removed |= drop;
                                            !drop
                                        })
                                        .collect();
                                    this.set_project_search_query(rest.join(" "), window, cx);
                                }))
                        })),
                )
            })
            .when_some(self.project_replace_status.clone(), |el, status| {
                el.child(
                    div()
                        .pl(px(20.))
                        .text_size(px(tokens::UI_XS))
                        .text_color(muted)
                        .child(status),
                )
            });

        let body = if query.is_empty() {
            self.render_search_empty(cx).into_any_element()
        } else if !self.project_search_pending
            && self.project_search_error.is_none()
            && self.project_search_matches.is_empty()
        {
            self.render_search_no_results(&query, cx).into_any_element()
        } else {
            self.render_search_results(&query, cx).into_any_element()
        };

        v_flex()
            .size_full()
            .child(header)
            .child(
                v_flex()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .child(body),
            )
    }

    /// A filter token as a small mono pill; removable ones carry an ×.
    fn render_search_chip(
        &self,
        id: (&'static str, usize),
        token: String,
        removable: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let muted = cx.theme().muted_foreground;
        let hover = cx.theme().list_hover;
        let accent = cx.theme().primary;
        h_flex()
            .id(id)
            .h(px(22.))
            .px(px(7.))
            .gap(px(4.))
            .items_center()
            .rounded(px(tokens::RADIUS_SM))
            .bg(cx.theme().background)
            .border_1()
            .border_color(cx.theme().border)
            .cursor_pointer()
            .hover(move |s| s.bg(hover).border_color(accent.opacity(0.4)))
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(px(tokens::UI_XS))
            .text_color(cx.theme().foreground)
            .child(token)
            .when(removable, |el| {
                el.child(Icon::new(IconName::X).with_size(px(10.)).text_color(muted))
            })
    }

    /// No query yet: Zed-style landing page -- a centered heading, one
    /// sentence, then the actions worth knowing before you type. No hero
    /// icon, no filter-syntax lecture.
    fn render_search_empty(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let options = self.project_search_options;
        let replace_open = self.project_replace_open;
        let option_row = |id: &'static str,
                          icon: IconName,
                          label: &'static str,
                          on: bool,
                          flip: fn(&mut core_workspace::SearchOptions),
                          cx: &mut Context<Self>| {
            let row = crate::rails::rail_action(id, icon, label, on, cx).on_click(cx.listener(
                move |this, _e, _w, cx| this.toggle_project_search_option(flip, cx),
            ));
            if on {
                row.child(div().flex_1()).child(
                    Icon::new(IconName::Check)
                        .with_size(px(12.))
                        .text_color(cx.theme().primary),
                )
            } else {
                row
            }
        };

        let replace_row = crate::rails::rail_action(
            "search-landing-replace",
            IconName::Replace,
            "Find and replace",
            replace_open,
            cx,
        )
        .on_click(cx.listener(|this, _e, window, cx| {
            this.project_replace_open = !this.project_replace_open;
            if this.project_replace_open {
                this.project_replace_input
                    .update(cx, |state, cx| state.focus(window, cx));
            }
            cx.notify();
        }));
        let replace_row = if replace_open {
            replace_row.child(div().flex_1()).child(
                Icon::new(IconName::Check)
                    .with_size(px(12.))
                    .text_color(cx.theme().primary),
            )
        } else {
            replace_row
        };
        let mut actions = v_flex()
            .w_full()
            .gap(px(tokens::SPACE_1))
            .child(replace_row)
            .child(option_row(
                "search-landing-case",
                IconName::CaseSensitive,
                "Match case",
                options.case_sensitive,
                |o| o.case_sensitive = !o.case_sensitive,
                cx,
            ))
            .child(option_row(
                "search-landing-word",
                IconName::WholeWord,
                "Match whole word",
                options.whole_word,
                |o| o.whole_word = !o.whole_word,
                cx,
            ))
            .child(option_row(
                "search-landing-regex",
                IconName::Regex,
                "Use regular expression",
                options.regex,
                |o| o.regex = !o.regex,
                cx,
            ));
        actions = actions.when(!self.project_recent_searches.is_empty(), |el| {
            el.child(
                v_flex()
                    .w_full()
                    .pt(px(tokens::SPACE_5))
                    .gap(px(tokens::SPACE_1))
                    .child(
                        div()
                            .pb(px(tokens::SPACE_1))
                            .text_size(px(tokens::UI_XS))
                            .font_medium()
                            .text_color(muted)
                            .child("Recent"),
                    )
                    .children(self.project_recent_searches.iter().take(3).enumerate().map(
                        |(ix, recent)| {
                            let pick = recent.clone();
                            crate::rails::rail_action(
                                ("search-recent", ix),
                                IconName::Clock,
                                recent.clone(),
                                false,
                                cx,
                            )
                            .on_click(cx.listener(move |this, _e, window, cx| {
                                this.set_project_search_query(pick.clone(), window, cx);
                            }))
                        },
                    )),
            )
        });

        v_flex()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .items_center()
            .justify_center()
            .p(px(tokens::SPACE_6))
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(280.))
                    .gap(px(tokens::SPACE_1))
                    .child(
                        div()
                            .w_full()
                            .text_center()
                            .text_size(px(tokens::UI_LG))
                            .font_semibold()
                            .text_color(fg)
                            .child("Search all files"),
                    )
                    .child(
                        div()
                            .w_full()
                            .pb(px(tokens::SPACE_4))
                            .text_center()
                            .text_size(px(tokens::UI_SM))
                            .text_color(muted)
                            .child("Start typing to search. For more options:"),
                    )
                    .child(actions),
            )
    }

    /// No results, in the same centered landing language: say what missed and
    /// offer the two ways out, without turning it into an error panel.
    fn render_search_no_results(&self, query: &str, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let (text, filters) = core_workspace::split_query_filters(query);
        let needle = text.trim();
        let shown = if needle.chars().count() > 32 {
            format!("{}\u{2026}", needle.chars().take(32).collect::<String>())
        } else {
            needle.to_string()
        };
        let detail = if needle.is_empty() {
            "The filters matched nothing. Remove one, or clear the search.".to_string()
        } else {
            format!(
                "Nothing matched \u{201c}{shown}\u{201d}. Check the spelling, or remove a filter."
            )
        };
        let mut actions = v_flex()
            .w_full()
            .gap(px(tokens::SPACE_1))
            .child(
                crate::rails::rail_action(
                    "search-no-results-clear",
                    IconName::X,
                    "Clear search",
                    false,
                    cx,
                )
                .on_click(cx.listener(|this, _e, window, cx| {
                    this.set_project_search_query(String::new(), window, cx);
                })),
            );
        if !filters.is_empty() {
            actions = actions.child(
                crate::rails::rail_action(
                    "search-no-results-filters",
                    IconName::ListFilter,
                    "Remove filters",
                    true,
                    cx,
                )
                .on_click(cx.listener(|this, _e, window, cx| {
                    let query = this
                        .project_search_input
                        .read(cx)
                        .value()
                        .trim()
                        .to_string();
                    let (text, _) = core_workspace::split_query_filters(&query);
                    this.set_project_search_query(text.trim().to_string(), window, cx);
                })),
            );
        }
        v_flex()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .items_center()
            .justify_center()
            .p(px(tokens::SPACE_6))
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(280.))
                    .gap(px(tokens::SPACE_1))
                    .child(
                        div()
                            .w_full()
                            .text_center()
                            .text_size(px(tokens::UI_LG))
                            .font_semibold()
                            .text_color(cx.theme().foreground)
                            .child("No results"),
                    )
                    .child(
                        div()
                            .w_full()
                            .pb(px(tokens::SPACE_4))
                            .text_center()
                            .text_size(px(tokens::UI_SM))
                            .text_color(muted)
                            .child(detail),
                    )
                    .child(actions),
            )
    }

    /// Results grouped by file: a header per file, then each matching line
    /// with its number and the hits highlighted.
    fn render_search_results(&self, query: &str, cx: &mut Context<Self>) -> impl IntoElement {
        /// Lines drawn; the rest are still counted, opened and replaced.
        const MAX_SHOWN: usize = 400;
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let pending = self.project_search_pending;

        let mut groups: Vec<(&Path, Vec<&core_workspace::SearchMatch>)> = Vec::new();
        for m in &self.project_search_matches {
            if self
                .project_search_dismissed
                .contains(&(m.path.clone(), m.line_number))
            {
                continue;
            }
            match groups.last_mut() {
                Some((path, lines)) if *path == m.path.as_path() => lines.push(m),
                _ => groups.push((m.path.as_path(), vec![m])),
            }
        }
        let total: usize = groups.iter().map(|(_, lines)| lines.len()).sum();
        let all_collapsed = !groups.is_empty()
            && groups
                .iter()
                .all(|(path, _)| self.project_search_collapsed.contains(*path));

        let summary = if let Some(error) = &self.project_search_error {
            div().text_color(cx.theme().red).child(error.clone())
        } else if pending {
            div()
                .text_color(cx.theme().primary)
                .child("Searching\u{2026}")
        } else if total == 0 {
            div().child("No results")
        } else {
            let plus = if self.project_search_truncated {
                "+"
            } else {
                ""
            };
            h_flex()
                .gap(px(4.))
                .child(div().text_color(fg).child(format!(
                    "{total}{plus} result{}",
                    if total == 1 { "" } else { "s" }
                )))
                .child(format!(
                    "in {} file{}",
                    groups.len(),
                    if groups.len() == 1 { "" } else { "s" }
                ))
        };

        let literal = if self.project_search_literal.is_empty() {
            query.to_string()
        } else {
            self.project_search_literal.clone()
        };
        let mut shown = 0usize;
        let mut rows: Vec<AnyElement> = Vec::new();
        for (group_ix, (path, lines)) in groups.iter().enumerate() {
            if shown >= MAX_SHOWN {
                break;
            }
            let collapsed = self.project_search_collapsed.contains(*path);
            rows.push(
                self.render_search_file_header(group_ix, path, lines.len(), collapsed, cx)
                    .into_any_element(),
            );
            if collapsed {
                continue;
            }
            for m in lines {
                if shown >= MAX_SHOWN {
                    break;
                }
                rows.push(
                    self.render_search_line(shown, m, &literal, cx)
                        .into_any_element(),
                );
                shown += 1;
            }
        }

        v_flex()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .child(
                h_flex()
                    .w_full()
                    .h(px(30.))
                    .flex_shrink_0()
                    .pl(px(tokens::SPACE_4))
                    .pr(px(tokens::SPACE_2))
                    .gap(px(tokens::SPACE_1))
                    .items_center()
                    .text_size(px(tokens::UI_XS))
                    .text_color(muted)
                    .child(div().flex_1().min_w(px(0.)).truncate().child(summary))
                    .when(!groups.is_empty(), |el| {
                        let paths: Vec<std::path::PathBuf> = groups.iter().map(|(p, _)| p.to_path_buf()).collect();
                        el.child(
                            Button::new("search-collapse-all")
                                .ghost()
                                .xsmall()
                                .icon(
                                    Icon::new(if all_collapsed {
                                        IconName::ChevronsUpDown
                                    } else {
                                        IconName::ChevronsDownUp
                                    })
                                    .with_size(px(12.))
                                    .text_color(muted),
                                )
                                .tooltip(if all_collapsed { "Expand all" } else { "Collapse all" })
                                .on_click(cx.listener(move |this, _e, _w, cx| {
                                    if all_collapsed {
                                        this.project_search_collapsed.clear();
                                    } else {
                                        this.project_search_collapsed.extend(paths.iter().cloned());
                                    }
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        Button::new("search-clear")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::X).with_size(px(12.)).text_color(muted))
                            .tooltip("Clear search")
                            .on_click(cx.listener(|this, _e, window, cx| {
                                this.project_replace_status = None;
                                this.set_project_search_query(String::new(), window, cx);
                            })),
                    ),
            )
            .child(
                v_flex()
                    .id("project-search-results")
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .pb(px(tokens::SPACE_4))
                    .children(rows)
                    .when(shown >= MAX_SHOWN, |el| {
                        el.child(
                            div()
                                .px(px(tokens::SPACE_4))
                                .py(px(tokens::SPACE_2))
                                .text_size(px(tokens::UI_XS))
                                .text_color(muted)
                                .child(format!("First {MAX_SHOWN} shown \u{b7} narrow the search to see the rest")),
                        )
                    }),
            )
    }

    fn render_search_file_header(
        &self,
        ix: usize,
        path: &Path,
        count: usize,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let muted = cx.theme().muted_foreground;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let dir = path
            .parent()
            .and_then(|p| p.strip_prefix(&self.root).ok())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let toggle_path = path.to_path_buf();
        h_flex()
            .id(("search-file", ix))
            .w_full()
            .h(px(tokens::TREE_ROW_H))
            .pl(px(tokens::SPACE_2))
            .pr(px(tokens::SPACE_3))
            .gap(px(6.))
            .items_center()
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().list_hover))
            .child(
                Icon::new(if collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                })
                .xsmall()
                .text_color(muted),
            )
            .child(crate::rails::files::render_file_icon(&name))
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(tokens::UI_SM))
                    .text_color(cx.theme().foreground)
                    .child(name),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_size(px(tokens::UI_XS))
                    .text_color(muted.opacity(0.7))
                    .child(dir),
            )
            .child(crate::rails::count_pill(count, cx))
            .on_click(cx.listener(move |this, _e, _w, cx| {
                if !this.project_search_collapsed.remove(&toggle_path) {
                    this.project_search_collapsed.insert(toggle_path.clone());
                }
                cx.notify();
            }))
    }

    /// One matching line: number, text with hits highlighted, and on hover
    /// replace-this (when replace is open) and dismiss.
    fn render_search_line(
        &self,
        ix: usize,
        m: &core_workspace::SearchMatch,
        literal: &str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let muted = cx.theme().muted_foreground;
        let group = format!("search-line-{ix}");
        let (text, ranges) = search_preview(&m.line_text, &m.ranges);
        let highlight = HighlightStyle {
            color: Some(cx.theme().primary),
            background_color: Some(cx.theme().primary.opacity(0.18)),
            ..Default::default()
        };
        let highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> =
            ranges.into_iter().map(|(a, b)| (a..b, highlight)).collect();

        let path = m.path.clone();
        let line_0 = m.line_number.saturating_sub(1) as u32;
        let phrase = m.phrase;
        let line_text = m.line_text.clone();
        let q = literal.to_string();
        let key = (m.path.clone(), m.line_number);
        let dismiss_key = key.clone();
        let row_bg = cx.theme().list_hover;
        let action = |id: (&'static str, usize), icon: IconName, tip: &'static str| {
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(Icon::new(icon).with_size(px(12.)).text_color(muted))
                .tooltip(tip)
        };

        h_flex()
            .id(("search-line", ix))
            .group(group.clone())
            .relative()
            .w_full()
            .h(px(22.))
            .pl(px(28.))
            .pr(px(tokens::SPACE_3))
            .gap(px(tokens::SPACE_2))
            .items_center()
            .cursor_pointer()
            .hover(move |s| s.bg(row_bg))
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(px(tokens::UI_XS))
            .child(
                div()
                    .flex_shrink_0()
                    .min_w(px(26.))
                    .flex()
                    .justify_end()
                    .text_color(muted.opacity(0.6))
                    .child(m.line_number.to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_color(cx.theme().foreground.opacity(0.85))
                    .child(StyledText::new(text).with_highlights(highlights)),
            )
            .child(
                h_flex()
                    .absolute()
                    .right(px(tokens::SPACE_2))
                    .top_0()
                    .bottom_0()
                    .items_center()
                    .gap(px(2.))
                    .pl(px(tokens::SPACE_1))
                    .bg(row_bg)
                    .invisible()
                    .group_hover(group, |s| s.visible())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .when(self.project_replace_open, |el| {
                        let only = key.clone();
                        el.child(
                            action(("search-replace-one", ix), IconName::Replace, "Replace")
                                .on_click(cx.listener(move |this, _e, _w, cx| {
                                    cx.stop_propagation();
                                    this.replace_in_project(Some(only.clone()), cx);
                                })),
                        )
                    })
                    .child(
                        action(("search-dismiss", ix), IconName::X, "Dismiss").on_click(
                            cx.listener(move |this, _e, _w, cx| {
                                cx.stop_propagation();
                                this.project_search_dismissed.insert(dismiss_key.clone());
                                cx.notify();
                            }),
                        ),
                    ),
            )
            .on_click(cx.listener(move |this, _e, window, cx| {
                let query = this
                    .project_search_input
                    .read(cx)
                    .value()
                    .trim()
                    .to_string();
                this.remember_project_search(&query);
                this.open_path(path.clone(), window, cx);
                if let Some(ed) = this.active_editor().cloned() {
                    // The editor's find bar is single-line. A pasted block is
                    // located by the start of the file line the match begins on.
                    let find_query = if phrase {
                        line_text.trim().chars().take(48).collect::<String>()
                    } else {
                        q.lines().next().unwrap_or("").trim().to_string()
                    };
                    ed.update(cx, |editor, cx| {
                        editor.set_search_query(&find_query, cx);
                        editor.set_cursor_to_position(line_0, 0, cx);
                    });
                }
            }))
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
        // The composer is docked under the scrolling lists, not above them: it
        // stays reachable while the developer reads a long diff list, and the
        // panel opens on the changes rather than on an empty text box.
        v_flex()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .child(self.render_git_branch_bar(status, cx))
            .child(self.render_git_publish_banner(status, cx))
            .child(self.render_git_changes(status, cx))
            .child(self.render_git_notice(cx))
            .when(!status.is_clean() || self.git.amend, |el| {
                el.child(self.render_git_commit_box(status, cx))
            })
    }

    /// An unpublished branch has no upstream, so no push control above can
    /// send anything: this banner is the one place that offers the first push.
    fn render_git_publish_banner(&self, status: &RepoStatus, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let text = cx.theme().foreground;
        let primary = cx.theme().primary;
        let busy = self.git.busy.is_some();
        let show = status.upstream.is_none() && status.head.is_some() && status.branch.is_some();
        if !show {
            return div().into_any_element();
        }
        // Every commit on an unpublished branch is unpushed; the list is capped
        // at 15, so say "15+" rather than a number we cannot confirm.
        let unpushed = status.recent_commits.iter().filter(|c| c.unpushed).count();
        let count = if unpushed >= status.recent_commits.len() && unpushed >= 15 {
            "15+ commits".to_string()
        } else {
            format!("{unpushed} commit{}", if unpushed == 1 { "" } else { "s" })
        };
        h_flex()
            .mx(px(tokens::SPACE_4))
            .mt(px(tokens::SPACE_3))
            .p(px(tokens::SPACE_3))
            .gap(px(tokens::SPACE_3))
            .items_center()
            .rounded(px(tokens::RADIUS_SM))
            .border_1()
            .border_color(primary.opacity(0.28))
            .bg(primary.opacity(0.06))
            .child(
                Icon::new(IconName::GitBranch)
                    .with_size(px(13.))
                    .text_color(primary),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(px(tokens::SPACE_1))
                    .child(
                        div()
                            .text_size(px(tokens::UI_SM))
                            .text_color(text)
                            .child("This branch isn't on origin yet"),
                    )
                    .child(
                        div()
                            .text_size(px(tokens::UI_XS))
                            .text_color(muted)
                            .child(format!("Publish to push {count}")),
                    ),
            )
            .child(
                Button::new("git-publish")
                    .xsmall()
                    .custom(accent_tint(cx))
                    .disabled(busy)
                    .label("Publish")
                    .tooltip("Publish this branch")
                    .on_click(cx.listener(|this, _e, window, cx| this.git_push(window, cx))),
            )
            .into_any_element()
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
        h_flex()
            .w_full()
            .h(px(tokens::PANEL_BAR_H))
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .gap(px(tokens::SPACE_2))
            .px(px(tokens::PANEL_INSET))
            .border_b_1()
            .border_color(border.opacity(0.6))
            .child(
                h_flex()
                    .id("git-branch-toggle")
                    .flex_1()
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
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(tokens::UI_SM))
                            .font_medium()
                            .text_color(fg)
                            .child(name),
                    )
                    // How far this branch is from its upstream, right in the chip:
                    // accent for commits waiting to go up, amber for ones to pull.
                    .when(status.ahead > 0, |el| {
                        el.child(sync_count("\u{2191}", status.ahead, primary, cx))
                    })
                    .when(status.behind > 0, |el| {
                        el.child(sync_count("\u{2193}", status.behind, cx.theme().warning, cx))
                    })
                    .child(Icon::new(IconName::ChevronDown).xsmall().text_color(muted))
                    .on_click(
                        cx.listener(|this, _event, window, cx| {
                            this.git_toggle_branches(window, cx)
                        }),
                    ),
            )
            .child(
                // Only what applies: fetch always; pull when behind; push only
                // while the tree is clean. With changes present the docked
                // composer carries Push next to Commit, so it is not offered twice.
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
                                    Icon::new(IconName::Archive)
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
                    .when(status.ahead > 0 && status.is_clean(), |el| {
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
        let mut filtered: Vec<_> = self
            .git
            .branches
            .iter()
            .filter(|b| query.is_empty() || b.name.to_lowercase().contains(&query))
            .cloned()
            .collect();
        // The current branch leads the list, so opening the switcher always
        // shows where you are (and the selection is on screen) even in a repo
        // with dozens of branches.
        filtered.sort_by_key(|b| !b.current);
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
        let has_remote = filtered.iter().any(|b| b.name.starts_with("origin/"));
        let local_count = filtered
            .iter()
            .filter(|b| !b.name.starts_with("origin/"))
            .count();
        let remote_count = filtered_count - local_count;

        let keycap = |key: &'static str| {
            h_flex()
                .h(px(18.))
                .min_w(px(18.))
                .px(px(4.))
                .items_center()
                .justify_center()
                .rounded(px(tokens::RADIUS_BADGE))
                .bg(cx.theme().background)
                .border_1()
                .border_color(border)
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(px(9.5))
                .text_color(muted)
                .child(key)
        };

        // Build the list with quiet group labels, so a remote branch is not
        // just another row in a flat wall.
        let mut list_rows: Vec<AnyElement> = Vec::new();
        if has_query && !exact_exists {
            let new_name = raw_query.clone();
            list_rows.push(
                h_flex()
                    .id("git-create-branch")
                    .w_full()
                    .flex_shrink_0()
                    .h(px(34.))
                    .px(px(tokens::SPACE_3))
                    .gap(px(tokens::SPACE_2))
                    .items_center()
                    .rounded(px(tokens::RADIUS_SM))
                    .bg(tokens::accent_dim())
                    .border_1()
                    .border_color(primary.opacity(0.32))
                    .cursor_pointer()
                    .hover(move |s| s.bg(primary.opacity(0.22)))
                    .child(Icon::new(IconName::Plus).with_size(px(12.)).text_color(primary))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(tokens::UI_SM))
                            .font_medium()
                            .text_color(primary)
                            .child(format!("Create branch \u{201c}{new_name}\u{201d}")),
                    )
                    .child(keycap("\u{21b5}"))
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.git_create_branch(window, cx)
                    }))
                    .into_any_element(),
            );
        }
        let mut last_remote: Option<bool> = None;
        for (ix, branch) in filtered.iter().enumerate() {
            let name = branch.name.clone();
            let del_name = branch.name.clone();
            let current = branch.current;
            let is_remote = name.starts_with("origin/");
            let is_selected = ix == sel_ix;
            let del_key = format!("branch-del:{name}");
            let del_armed = confirm_del.as_deref() == Some(del_key.as_str());
            let row_group = format!("git-branch-grp-{ix}");

            if has_remote && last_remote != Some(is_remote) {
                let label = if is_remote {
                    format!("Remote {remote_count}")
                } else {
                    format!("Local {local_count}")
                };
                list_rows.push(
                    h_flex()
                        .w_full()
                        .flex_shrink_0()
                        .h(px(20.))
                        .px(px(tokens::SPACE_3))
                        .pt(px(tokens::SPACE_2))
                        .items_center()
                        .text_size(px(tokens::UI_XS))
                        .font_medium()
                        .text_color(muted)
                        .child(label)
                        .into_any_element(),
                );
            }
            last_remote = Some(is_remote);

            list_rows.push(
                h_flex()
                    .id(("git-branch", ix))
                    .group(row_group.clone())
                    .relative()
                    .w_full()
                    .flex_shrink_0()
                    .h(px(34.))
                    .px(px(tokens::SPACE_3))
                    .gap(px(tokens::SPACE_2))
                    .items_center()
                    .justify_between()
                    .rounded(px(tokens::RADIUS_SM))
                    .border_1()
                    .border_color(gpui_kit::transparent_black())
                    .cursor_pointer()
                    .when(is_selected, |s| {
                        s.bg(tokens::accent_dim())
                            .border_color(primary.opacity(0.28))
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
                    .text_size(px(tokens::UI_SM))
                    .text_color(if is_remote { muted } else { fg })
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
                                    .child(branch.name.clone()),
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
                                        .rounded(px(tokens::RADIUS_BADGE))
                                        .bg(info.opacity(0.15))
                                        .font_family(cx.theme().mono_font_family.clone())
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
                                        .rounded(px(tokens::RADIUS_BADGE))
                                        .bg(green.opacity(0.15))
                                        .font_family(cx.theme().mono_font_family.clone())
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
                                            d.invisible()
                                                .group_hover(row_group.clone(), |s| s.visible())
                                        })
                                        .text_size(px(9.5))
                                        .text_color(if del_armed { red } else { muted })
                                        .hover(move |s| s.text_color(red))
                                        .child(if del_armed {
                                            div().child("Del?").into_any_element()
                                        } else {
                                            Icon::new(IconName::X)
                                                .with_size(px(11.))
                                                .into_any_element()
                                        })
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation()
                                        })
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
                        el.on_click(cx.listener(move |this, _event, window, cx| {
                            this.git_checkout(name.clone(), window, cx)
                        }))
                    })
                    .into_any_element(),
            );
        }

        Some(
            div()
                .id("branch-switcher-backdrop")
                .occlude()
                .absolute()
                .inset_0()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(96.))
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
                        .w(px(560.))
                        .max_h(px(480.))
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
                        // A real field, with room for the text and a visible exit.
                        .child(
                            h_flex()
                                .w_full()
                                .h(px(54.))
                                .flex_shrink_0()
                                .px(px(tokens::SPACE_4))
                                .gap(px(tokens::SPACE_3))
                                .items_center()
                                .border_b_1()
                                .border_color(border)
                                .child(
                                    h_flex()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .h(px(34.))
                                        .px(px(tokens::SPACE_2))
                                        .gap(px(tokens::SPACE_2))
                                        .items_center()
                                        .rounded(px(tokens::RADIUS_SM))
                                        .bg(cx.theme().background)
                                        .border_1()
                                        .border_color(border)
                                        .child(
                                            Icon::new(IconName::Search)
                                                .with_size(px(13.))
                                                .text_color(muted),
                                        )
                                        .child(
                                            div().flex_1().min_w(px(0.)).child(
                                                Input::new(&self.git.branch_input)
                                                    .appearance(false)
                                                    .small(),
                                            ),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("branch-switcher-close")
                                        .size(px(26.))
                                        .flex_shrink_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(tokens::RADIUS_SM))
                                        .cursor_pointer()
                                        .hover(move |s| s.bg(list_hover))
                                        .tooltip(|window, cx| {
                                            gpui_kit::component::tooltip::Tooltip::new(
                                                "Close branch switcher",
                                            )
                                            .build(window, cx)
                                        })
                                        .child(
                                            Icon::new(IconName::X)
                                                .with_size(px(12.))
                                                .text_color(muted),
                                        )
                                        .on_click(cx.listener(|this, _e, _window, cx| {
                                            this.git.branches_open = false;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(
                            v_flex()
                                .id("git-branch-scroll")
                                .flex_shrink_1()
                                .min_h(px(0.))
                                .max_h(px(360.))
                                .p(px(tokens::SPACE_2))
                                .gap(px(1.))
                                .overflow_y_scroll()
                                .children(list_rows),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .h(px(32.))
                                .flex_shrink_0()
                                .px(px(tokens::SPACE_4))
                                .gap(px(tokens::SPACE_3))
                                .items_center()
                                .border_t_1()
                                .border_color(border)
                                .text_size(px(tokens::UI_XS))
                                .text_color(muted)
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap(px(tokens::SPACE_1))
                                        .child(keycap("\u{2191}\u{2193}"))
                                        .child("Navigate"),
                                )
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap(px(tokens::SPACE_1))
                                        .child(keycap("\u{21b5}"))
                                        .child("Switch / create"),
                                )
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap(px(tokens::SPACE_1))
                                        .child(keycap("esc"))
                                        .child("Dismiss"),
                                )
                                .child(div().flex_1())
                                .child(div().child(format!(
                                    "{filtered_count} branch{}",
                                    if filtered_count == 1 { "" } else { "es" }
                                ))),
                        ),
                ),
        )
    }

    /// The commit composer, docked under the scrolling lists. The input carries
    /// its own staging context ("2 staged · 1 unstaged") so the button never has
    /// to guess what the commit will contain, and Push sits next to Commit: the
    /// two steps that finish a piece of work.
    fn render_git_commit_box(&self, status: &RepoStatus, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let primary_hover = cx.theme().primary_hover;
        let border = cx.theme().border;
        let staged = status.staged.len();
        let unstaged = status.unstaged.len();
        let ahead = status.ahead;
        let message = self.git.message.read(cx).value().to_string();
        let has_message = !message.trim().is_empty();
        let amend = self.git.amend;
        let busy = self.git.busy.is_some();
        let subject_len = message.lines().next().unwrap_or("").chars().count();
        let plural = |n: usize| if n == 1 { "" } else { "s" };

        let can_commit = !busy && (amend || (has_message && (staged + unstaged > 0)));

        let types = self.git.commit_types.clone().unwrap_or_default();
        let suggestion = (!has_message && !amend)
            .then(|| core_workspace::suggest_message(self.commit_candidates(), &types))
            .flatten();

        // What this commit will carry: the staged files, or everything when
        // nothing is staged ("Commit all").
        let going_in: Vec<&ChangedFile> = if staged > 0 {
            status.staged.iter().collect()
        } else {
            status.unstaged.iter().collect()
        };
        let (added, removed) = going_in
            .iter()
            .filter_map(|f| self.git.stats.get(&f.path))
            .fold((0usize, 0usize), |(a, r), (fa, fr)| (a + *fa as usize, r + *fr as usize));
        let left_out = if staged > 0 { unstaged } else { 0 };
        let n = going_in.len();
        let has_upstream = status.upstream.is_some();
        let upstream = status.upstream.clone().unwrap_or_else(|| "origin".into());
        let branch = status.branch.clone().unwrap_or_else(|| "HEAD".into());
        let recent: Vec<String> = status
            .recent_commits
            .iter()
            .map(|c| c.subject.clone())
            .filter(|s| !s.is_empty())
            .take(8)
            .collect();
        let view = cx.entity();
        let live = has_message || amend;
        let what = if amend {
            "Amend the last commit".to_string()
        } else if staged > 0 {
            format!("Commit {staged} staged file{} to {branch}", plural(staged))
        } else {
            format!("Commit all {n} file{} to {branch}", plural(n))
        };
        let commit_tip = if can_commit {
            format!("{what} (\u{2318}\u{21b5})")
        } else {
            format!("Write a message first, then {}", what.to_lowercase())
        };

        // One quiet icon button for the card's footer.
        let foot_icon = |id: &'static str, icon: IconName, tint: Hsla| {
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(Icon::new(icon).with_size(px(13.)).text_color(tint))
        };

        let menu_view = view.clone();
        let menu_upstream = upstream.clone();
        let more_menu = move |menu: PopupMenu, _w: &mut Window, _cx: &mut Context<PopupMenu>| {
            let (v1, v2, v3, v4) = (
                menu_view.clone(),
                menu_view.clone(),
                menu_view.clone(),
                menu_view.clone(),
            );
            menu.item(
                PopupMenuItem::new(if ahead > 0 {
                    format!("Push {ahead} commit{} to {menu_upstream}", plural(ahead as usize))
                } else if !has_upstream {
                    "Publish branch".to_string()
                } else {
                    "Push".to_string()
                })
                .on_click(move |_, window, cx| v1.update(cx, |this, cx| this.git_push(window, cx))),
            )
            .item(
                PopupMenuItem::new("Commit & Push  \u{21e7}\u{2318}\u{21b5}")
                    .disabled(!can_commit)
                    .on_click(move |_, window, cx| {
                        v2.update(cx, |this, cx| this.git_commit_and_push(window, cx))
                    }),
            )
            .separator()
            .item(
                PopupMenuItem::new(if amend { "Stop amending" } else { "Amend last commit" })
                    .on_click(move |_, window, cx| {
                        v3.update(cx, |this, cx| this.git_toggle_amend(window, cx))
                    }),
            )
            .item(
                PopupMenuItem::new("Force push (with lease)")
                    .disabled(!has_upstream)
                    .on_click(move |_, window, cx| {
                        v4.update(cx, |this, cx| this.git_force_push(window, cx))
                    }),
            )
        };

        // The split button: Commit on the left, its menu on the right. Quiet
        // (outlined) until there is a message, accent once there is.
        let (btn_bg, btn_fg, btn_border) = if can_commit {
            (primary, cx.theme().primary_foreground, primary)
        } else {
            (gpui_kit::transparent_black(), muted, border)
        };
        let divider = if can_commit {
            cx.theme().primary_foreground.opacity(0.3)
        } else {
            border
        };
        let ahead_tint = if can_commit {
            cx.theme().primary_foreground
        } else {
            primary
        };
        let split = h_flex()
            .flex_shrink_0()
            .h(px(24.))
            .items_center()
            .rounded(px(tokens::RADIUS_SM))
            .border_1()
            .border_color(btn_border)
            .bg(btn_bg)
            .text_color(btn_fg)
            .text_size(px(tokens::UI_SM))
            .font_medium()
            .overflow_hidden()
            .child(
                h_flex()
                    .id("git-commit")
                    .h_full()
                    .px(px(tokens::SPACE_4))
                    .items_center()
                    .child(if amend { "Amend" } else { "Commit" })
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(commit_tip.clone()).build(window, cx)
                    })
                    .when(can_commit, |el| {
                        el.cursor_pointer()
                            .hover(move |s| s.bg(primary_hover))
                            .on_click(cx.listener(|this, _e, window, cx| {
                                this.git_primary_action(window, cx)
                            }))
                    }),
            )
            .child(div().w(px(1.)).h(px(14.)).bg(divider))
            .child({
                // The right half: commits waiting to go up ("↑2"), or a chevron.
                // Either way it opens the push / amend menu.
                let more = Button::new("git-commit-more")
                    .ghost()
                    .xsmall()
                    .h_full()
                    .rounded(px(0.))
                    .disabled(busy)
                    .tooltip(if ahead > 0 {
                        format!("{ahead} commit{} to push \u{b7} push and more", plural(ahead as usize))
                    } else {
                        "Push, amend and more".to_string()
                    });
                let more = if ahead > 0 {
                    more.label(format!("\u{2191}{ahead}")).text_color(ahead_tint)
                } else {
                    more.icon(Icon::new(IconName::ChevronDown).with_size(px(11.)).text_color(btn_fg))
                };
                more.dropdown_menu_with_anchor(Anchor::BottomRight, more_menu)
            });

        div()
            .w_full()
            .flex_shrink_0()
            .border_t_1()
            .border_color(border.opacity(0.6))
            .p(px(tokens::SPACE_4))
            // ⌘↵ commits; ⇧⌘↵ commits and pushes.
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let m = event.keystroke.modifiers;
                if (m.platform || m.control) && event.keystroke.key == "enter" {
                    if m.shift {
                        this.git_commit_and_push(window, cx);
                    } else {
                        this.git_primary_action(window, cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .child(
                v_flex()
                    .w_full()
                    .rounded(px(tokens::RADIUS_PANEL))
                    .border_1()
                    .border_color(if live { primary.opacity(0.5) } else { border })
                    .bg(cx.theme().background)
                    .overflow_hidden()
                    .child(
                        div()
                            .w_full()
                            .px(px(CARD_INSET))
                            .pt(px(tokens::SPACE_3))
                            .pb(px(tokens::SPACE_2))
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
                            .h(px(36.))
                            .pl(px(tokens::SPACE_2))
                            .pr(px(tokens::SPACE_3))
                            .gap(px(tokens::SPACE_1))
                            .items_center()
                            .border_t_1()
                            .border_color(border.opacity(0.5))
                            .when_some(suggestion.clone(), |el, suggested| {
                                el.child(
                                    foot_icon("git-suggestion", IconName::Sparkles, primary)
                                        .tooltip(format!("Write it for me: {suggested}"))
                                        .on_click(cx.listener(|this, _e, window, cx| {
                                            this.git_apply_suggestion(window, cx)
                                        })),
                                )
                            })
                            .when(!recent.is_empty(), |el| {
                                let view = view.clone();
                                el.child(
                                    foot_icon("git-recent-messages", IconName::Clock, muted)
                                        .tooltip("Reuse a recent message")
                                        .dropdown_menu(move |menu, _w, _cx| {
                                            let mut menu = menu;
                                            for subject in recent.clone() {
                                                let view = view.clone();
                                                let label: String = if subject.chars().count() > 48 {
                                                    format!(
                                                        "{}\u{2026}",
                                                        subject.chars().take(47).collect::<String>()
                                                    )
                                                } else {
                                                    subject.clone()
                                                };
                                                menu = menu.item(PopupMenuItem::new(label).on_click(
                                                    move |_, window, cx| {
                                                        let subject = subject.clone();
                                                        view.update(cx, |this, cx| {
                                                            this.git.message.update(cx, |s, cx| {
                                                                s.set_value(subject, window, cx)
                                                            });
                                                            cx.notify();
                                                        });
                                                    },
                                                ));
                                            }
                                            menu
                                        }),
                                )
                            })
                            // The middle says what goes in: the size, a dot
                            // when some changes stay out, or "Amending".
                            .child(
                                h_flex()
                                    .id("git-commit-summary")
                                    .flex_1()
                                    .min_w(px(0.))
                                    .px(px(tokens::SPACE_2))
                                    .gap(px(tokens::SPACE_2))
                                    .items_center()
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(muted)
                                    .when(amend, |el| {
                                        el.child(div().truncate().text_color(primary).child("Amending"))
                                    })
                                    .when(!amend && subject_len > 72, |el| {
                                        el.child(
                                            div()
                                                .text_color(cx.theme().red)
                                                .child(format!("{subject_len}/72")),
                                        )
                                    })
                                    .when(!amend && subject_len <= 72 && added + removed > 0, |el| {
                                        el.child(crate::diff_view::stat_chip(added as _, removed as _, cx))
                                    })
                                    .when(left_out > 0 && !amend, |el| {
                                        el.child(div().size(px(5.)).rounded(px(3.)).bg(cx.theme().yellow))
                                    })
                                    .tooltip({
                                        let text = if left_out > 0 {
                                            format!(
                                                "{n} file{} go in \u{b7} {left_out} not staged stay out",
                                                plural(n)
                                            )
                                        } else {
                                            format!("{n} file{} go in", plural(n))
                                        };
                                        move |window, cx| {
                                            gpui_kit::component::tooltip::Tooltip::new(text.clone())
                                                .build(window, cx)
                                        }
                                    }),
                            )
                            .child(split),
                    ),
            )
    }

    /// Working changes on top, history (stashes and recent commits) below.
    /// The list sizes to its content and the history takes the slack, so a
    /// short change list does not leave a dead middle in the panel; both
    /// halves still scroll on their own. A clean tree has nothing to list, so
    /// it shows a quiet status block and lets the history take the rest.
    fn render_git_changes(&self, status: &RepoStatus, cx: &mut Context<Self>) -> impl IntoElement {
        let has_history = !(status.stashes.is_empty() && status.recent_commits.is_empty());
        let history_open = (!status.stashes.is_empty() && !self.git.stashes_collapsed)
            || (!status.recent_commits.is_empty() && !self.git.commits_collapsed);
        let history_min = if history_open {
            GIT_HISTORY_MIN
        } else {
            GIT_HISTORY_HEADERS_H
        };
        let history = v_flex()
            .id("git-history")
            .size_full()
            .min_h(px(0.))
            .overflow_y_scroll()
            .pb(px(tokens::SPACE_4))
            .when(!status.stashes.is_empty(), |el| {
                el.child(self.render_git_stashes(&status.stashes, cx))
            })
            .when(!status.recent_commits.is_empty(), |el| {
                el.child(self.render_git_commits(status, cx))
            });
        let frame = v_flex()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .border_t_1()
            .border_color(cx.theme().border);

        if status.is_clean() {
            return frame
                .child(self.render_git_clean_state(status, cx))
                .when(has_history, |el| {
                    el.child(
                        div()
                            .flex_1()
                            .w_full()
                            .min_h(px(history_min))
                            .child(history),
                    )
                });
        }

        let list = v_flex()
            .id("git-changes")
            .w_full()
            .flex_shrink_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .pb(px(tokens::SPACE_2))
            .when(!status.conflicts.is_empty(), |el| {
                el.child(self.render_git_section(
                    "Merge conflicts",
                    Section::Conflicts,
                    &status.conflicts,
                    false,
                    cx,
                ))
            })
            .when(!status.staged.is_empty(), |el| {
                el.child(self.render_git_section(
                    "Staged changes",
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
            });
        if !has_history {
            return frame.child(list);
        }
        frame.child(list).child(
            div()
                .w_full()
                .flex_1()
                .min_h(px(history_min))
                .child(history),
        )
    }

    /// Nothing to commit. Fleet's empty states are flat and text-first, so this
    /// is a glyph, one sentence about the branch, and at most two actions -- no
    /// card chrome on top of a panel that is already boxed in. Push, pull and
    /// publish also live in the branch bar above; amend/undo on the HEAD commit
    /// below.
    fn render_git_clean_state(&self, status: &RepoStatus, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let green = cx.theme().green;
        let busy = self.git.busy.is_some();
        let has_commit = status.head.is_some();
        let has_upstream = status.upstream.is_some();
        let ahead = status.ahead;
        let behind = status.behind;
        let plural = |n: u32| if n == 1 { "" } else { "s" };

        let remote = status
            .upstream
            .as_deref()
            .and_then(|upstream| upstream.split_once('/'))
            .map(|(remote, _)| remote)
            .unwrap_or("origin");

        let (title, detail) = if !has_commit {
            (
                "No commits yet",
                "Stage a file and make the first commit.".to_string(),
            )
        } else if has_upstream {
            let detail = match (ahead, behind) {
                (a, b) if a > 0 && b > 0 => {
                    format!("Everything is committed. The branch has diverged from {remote}.")
                }
                (_, _) if ahead > 0 || behind > 0 => "Everything is committed.".to_string(),
                _ => format!("Everything is committed and in sync with {remote}."),
            };
            ("No changes", detail)
        } else if status.branch.is_some() {
            // The publish banner above already offers the first push.
            (
                "No changes",
                "Everything is committed. This branch isn't on a remote yet.".to_string(),
            )
        } else {
            (
                "No changes",
                format!(
                    "Everything is committed. HEAD is detached at {}.",
                    status.head.clone().unwrap_or_default()
                ),
            )
        };

        // The action that has work to do gets the accent; an in-sync branch
        // only offers a quiet fetch.
        let mut actions: Vec<AnyElement> = Vec::new();
        if has_commit && has_upstream {
            if behind > 0 {
                actions.push(
                    crate::rails::rail_action(
                        "git-clean-pull",
                        IconName::ArrowDown,
                        format!("Pull {} commit{} from {remote}", behind, plural(behind)),
                        true,
                        cx,
                    )
                    .when(!busy, |el| {
                        el.on_click(cx.listener(|this, _e, window, cx| this.git_pull(window, cx)))
                    })
                    .when(busy, |el| el.opacity(0.5))
                    .into_any_element(),
                );
            }
            if ahead > 0 {
                // With a diverged branch, pulling first is the safer primary.
                actions.push(
                    crate::rails::rail_action(
                        "git-clean-push",
                        IconName::ArrowUp,
                        format!("Push {} commit{} to {remote}", ahead, plural(ahead)),
                        behind == 0,
                        cx,
                    )
                    .when(!busy, |el| {
                        el.on_click(cx.listener(|this, _e, window, cx| this.git_push(window, cx)))
                    })
                    .when(busy, |el| el.opacity(0.5))
                    .into_any_element(),
                );
            }
            if ahead == 0 && behind == 0 {
                actions.push(
                    crate::rails::rail_action(
                        "git-clean-fetch",
                        IconName::RefreshCw,
                        format!("Fetch from {remote}"),
                        false,
                        cx,
                    )
                    .when(!busy, |el| {
                        el.on_click(cx.listener(|this, _e, window, cx| this.git_fetch(window, cx)))
                    })
                    .when(busy, |el| el.opacity(0.5))
                    .into_any_element(),
                );
            }
        }

        v_flex()
            .w_full()
            .flex_shrink_0()
            .px(px(tokens::SPACE_5))
            .pt(px(tokens::SPACE_6))
            .pb(px(tokens::SPACE_5))
            .gap(px(tokens::SPACE_4))
            .child(crate::rails::quiet_state(
                if has_commit {
                    IconName::CircleCheck
                } else {
                    IconName::GitBranch
                },
                if has_commit { green } else { muted },
                title,
                detail,
                cx,
            ))
            .when(!actions.is_empty(), |el| {
                el.child(v_flex().w_full().pt(px(tokens::SPACE_2)).children(actions))
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
        let discard_all_armed = section == Section::Changes
            && self.git.confirm_discard.as_deref() == Some("discard-all");
        // The size of this section's change at a glance: summed line stats
        // in the header, and their added/removed split as a thin bar under it.
        let (added, removed) = files
            .iter()
            .filter_map(|f| self.git.stats.get(&f.path))
            .fold((0usize, 0usize), |(a, r), (fa, fr)| (a + *fa as usize, r + *fr as usize));
        let totals = (added + removed > 0).then_some((added, removed));
        v_flex()
            .w_full()
            .child(
                crate::rails::section_row()
                    .child(
                        crate::rails::section_label(
                            ("git-section", section as usize),
                            title,
                            Some(files.len()),
                            collapsed,
                            cx,
                        )
                        .on_click(cx.listener(
                            move |this, _event, _window, cx| {
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
                            },
                        )),
                    )
                    .when(section != Section::Conflicts, |el| {
                        el.child(
                            h_flex()
                                .items_center()
                                .gap(px(tokens::SPACE_1))
                                .when_some(totals, |row, (a, r)| {
                                    row.child(
                                        div()
                                            .mr(px(tokens::SPACE_2))
                                            .child(crate::diff_view::stat_chip(a as _, r as _, cx)),
                                    )
                                })
                                .child(
                                    Button::new(("git-group-toggle", section as usize))
                                        .ghost()
                                        .xsmall()
                                        .icon(
                                            Icon::new(if self.git.group_by_folder {
                                                IconName::FolderTree
                                            } else {
                                                IconName::List
                                            })
                                            .with_size(px(12.))
                                            .text_color(if self.git.group_by_folder {
                                                cx.theme().primary
                                            } else {
                                                muted
                                            }),
                                        )
                                        .tooltip(if self.git.group_by_folder {
                                            "Show as a flat list"
                                        } else {
                                            "Group by folder"
                                        })
                                        .on_click(cx.listener(|this, _e, _w, cx| {
                                            this.git.group_by_folder = !this.git.group_by_folder;
                                            cx.notify();
                                        })),
                                )
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
                let el = el.when_some(totals, |el, (a, r)| el.child(churn_bar(a, r, cx)));
                if self.git.group_by_folder && section != Section::Conflicts {
                    el.children(self.render_git_folder_groups(section, files, cx))
                } else {
                    el.children(
                        files
                            .iter()
                            .cloned()
                            .enumerate()
                            .map(|(ix, file)| self.render_git_file_row(section, ix, file, true, cx)),
                    )
                }
            })
    }

    /// One file: icon, name, dimmed folder, and a right-hand slot that shows its status letter
    /// and swaps to the Open / Stage / Unstage / Discard buttons while the row is hovered.
    /// Changed files under one header per folder: the folder, how many files
    /// and lines changed there, and a hover button that stages (or unstages)
    /// the whole folder. Rows inside drop their own folder label.
    fn render_git_folder_groups(
        &self,
        section: Section,
        files: &[ChangedFile],
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut groups: Vec<(String, Vec<(usize, ChangedFile)>)> = Vec::new();
        for (ix, file) in files.iter().enumerate() {
            let dir = Path::new(&file.path)
                .parent()
                .and_then(|p| p.to_str())
                .unwrap_or("")
                .to_string();
            match groups.iter_mut().find(|(d, _)| *d == dir) {
                Some((_, list)) => list.push((ix, file.clone())),
                None => groups.push((dir, vec![(ix, file.clone())])),
            }
        }
        groups.sort_by(|a, b| a.0.cmp(&b.0));
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let staged = section == Section::Staged;
        let mut out = Vec::new();
        for (gix, (dir, members)) in groups.into_iter().enumerate() {
            let (a, r) = members
                .iter()
                .filter_map(|(_, f)| self.git.stats.get(&f.path))
                .fold((0u32, 0u32), |(a, r), (fa, fr)| (a + fa, r + fr));
            let group: SharedString = format!("git-folder-{}-{gix}", section as usize).into();
            let batch: Vec<ChangedFile> = members.iter().map(|(_, f)| f.clone()).collect();
            let label = if dir.is_empty() { "(root)".to_string() } else { format!("{dir}/") };
            out.push(
                h_flex()
                    .id(("git-folder", section as usize * 10_000 + gix))
                    .group(group.clone())
                    .w_full()
                    .h(px(24.))
                    .mt(px(if gix == 0 { 0. } else { tokens::SPACE_2 }))
                    .px(px(tokens::SPACE_5))
                    .items_center()
                    .gap(px(tokens::SPACE_2))
                    .text_size(px(tokens::UI_XS))
                    .text_color(muted)
                    .child(Icon::new(IconName::FolderOpen).with_size(px(12.)).text_color(muted))
                    .child(div().flex_1().min_w(px(0.)).truncate().text_color(fg.opacity(0.8)).child(label))
                    .child(
                        div()
                            .flex_shrink_0()
                            .group_hover(group.clone(), |s| s.invisible())
                            .when(a + r > 0, |el| el.child(crate::diff_view::stat_chip(a, r, cx))),
                    )
                    .child(
                        div().absolute().right(px(tokens::SPACE_5)).invisible().group_hover(group, |s| s.visible()).child(
                            Button::new(("git-folder-stage", section as usize * 10_000 + gix))
                                .ghost()
                                .xsmall()
                                .icon(
                                    Icon::new(if staged { IconName::Minus } else { IconName::Plus })
                                        .with_size(px(12.))
                                        .text_color(muted),
                                )
                                .tooltip(if staged { "Unstage this folder" } else { "Stage this folder" })
                                .on_click(cx.listener(move |this, _e, window, cx| {
                                    if staged {
                                        this.git_unstage(&batch, window, cx);
                                    } else {
                                        this.git_stage(&batch, window, cx);
                                    }
                                })),
                        ),
                    )
                    .relative()
                    .into_any_element(),
            );
            for (ix, file) in members {
                out.push(self.render_git_file_row(section, ix, file, false, cx).into_any_element());
            }
        }
        out
    }

    fn render_git_file_row(
        &self,
        section: Section,
        ix: usize,
        file: ChangedFile,
        show_dir: bool,
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
            .filter(|p| !p.is_empty() && show_dir)
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
        let menu_file = file.clone();
        let menu_root = root.clone();
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
                    // Stats sit right against the status letter; the slot keeps
                    // at least the width the hover actions need, and they cover
                    // it on hover.
                    .child(
                        div()
                            .relative()
                            .flex_shrink_0()
                            .min_w(px(slot_width))
                            .h_full()
                            .child(
                                h_flex()
                                    .h_full()
                                    .items_center()
                                    .justify_end()
                                    .gap(px(tokens::SPACE_2))
                                    .pr(px(tokens::SPACE_2))
                                    .group_hover(group.clone(), |s| s.invisible())
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
                                            .w(px(8.))
                                            .flex()
                                            .justify_center()
                                            .text_size(px(10.))
                                            .font_medium()
                                            .text_color(color)
                                            .child(file.kind.letter().to_string()),
                                    ),
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
                                        .when(
                                            section == Section::Changes && !discard_armed,
                                            |row| {
                                                row.child(
                                                    action("git-stage", IconName::Plus, "Stage")
                                                        .on_click(cx.listener(
                                                            move |this, _event, window, cx| {
                                                                this.git_stage(
                                                                    std::slice::from_ref(
                                                                        &stage_file,
                                                                    ),
                                                                    window,
                                                                    cx,
                                                                )
                                                            },
                                                        )),
                                                )
                                            },
                                        )
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
                    }))
                    .context_menu({
                        let view = cx.entity();
                        let file = menu_file;
                        let root = menu_root;
                        move |menu, _window, _cx| {
                            git_file_context_menu(menu, view.clone(), file.clone(), section, &root)
                        }
                    }),
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
                crate::rails::section_row().child(
                    crate::rails::section_label(
                        "git-section-stashes",
                        "Stashes",
                        Some(stashes.len()),
                        collapsed,
                        cx,
                    )
                    .on_click(cx.listener(|this, _e, _w, cx| {
                        this.git.stashes_collapsed = !this.git.stashes_collapsed;
                        cx.notify();
                    })),
                ),
            )
            .when(!collapsed, |el| {
                el.children(stashes.iter().map(|stash| {
                    let index = stash.index;
                    let group = format!("git-stash-{index}");
                    let drop_key = format!("stash-drop:{index}");
                    let drop_armed = self.git.confirm_discard.as_deref() == Some(drop_key.as_str());
                    let (branch, message) = crate::git_panel::parse_stash_message(&stash.message);
                    let tooltip = match branch {
                        Some(branch) => format!("stash@{{{index}}} on {branch}\n{message}"),
                        None => format!("stash@{{{index}}}\n{message}"),
                    };
                    let row_bg = cx.theme().list_hover;
                    let icon_button =
                        |id: (&'static str, usize), icon: IconName, tip: &'static str| {
                            Button::new(id)
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(icon).with_size(px(12.)).text_color(muted))
                                .tooltip(tip)
                        };
                    // Same box as a file row: 28px slot, 26px row, icon at 14px.
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
                                .relative()
                                .w_full()
                                .h(px(26.))
                                .pl(px(tokens::SPACE_4))
                                .pr(px(tokens::SPACE_2))
                                .gap(px(tokens::SPACE_2))
                                .items_center()
                                .rounded(px(tokens::RADIUS_SM))
                                .hover(move |s| s.bg(row_bg))
                                .tooltip(move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(tooltip.clone())
                                        .build(window, cx)
                                })
                                .child(
                                    Icon::new(IconName::Archive)
                                        .with_size(px(13.))
                                        .text_color(muted)
                                        .flex_shrink_0(),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .truncate()
                                        .text_size(px(tokens::UI_SM))
                                        .text_color(fg)
                                        .child(message.to_string()),
                                )
                                // Actions keep their own space at the right end and
                                // show on hover (and while a drop waits for
                                // confirmation); `stash@{n}` and the branch are in
                                // the tooltip.
                                .child(
                                    h_flex()
                                        .flex_shrink_0()
                                        .items_center()
                                        .gap(px(2.))
                                        .when(!drop_armed, |row| {
                                            row.invisible()
                                                .group_hover(group.clone(), |s| s.visible())
                                        })
                                        .child(
                                            icon_button(
                                                ("git-stash-apply", index),
                                                IconName::ArchiveRestore,
                                                "Apply (keep the stash)",
                                            )
                                            .on_click(
                                                cx.listener(move |this, _e, window, cx| {
                                                    this.git_stash_apply(index, window, cx)
                                                }),
                                            ),
                                        )
                                        .child(
                                            Button::new(("git-stash-pop", index))
                                                .ghost()
                                                .xsmall()
                                                .label("Pop")
                                                .tooltip("Apply and remove the stash")
                                                .on_click(cx.listener(
                                                    move |this, _e, window, cx| {
                                                        this.git_stash_pop(index, window, cx)
                                                    },
                                                )),
                                        )
                                        .child(if drop_armed {
                                            Button::new(("git-stash-drop-confirm", index))
                                                .xsmall()
                                                .danger()
                                                .label("Drop?")
                                                .on_click(cx.listener(
                                                    move |this, _e, window, cx| {
                                                        this.git_stash_drop_clicked(
                                                            index, window, cx,
                                                        )
                                                    },
                                                ))
                                                .into_any_element()
                                        } else {
                                            icon_button(
                                                ("git-stash-drop", index),
                                                IconName::Trash,
                                                "Drop the stash",
                                            )
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
        let collapsed = self.git.commits_collapsed;
        let has_upstream = status.upstream.is_some();

        v_flex()
            .w_full()
            .border_t_1()
            .border_color(cx.theme().border.opacity(0.6))
            .child(
                crate::rails::section_row().child(
                    crate::rails::section_label(
                        "git-section-commits",
                        "Recent commits",
                        Some(status.recent_commits.len()),
                        collapsed,
                        cx,
                    )
                    .on_click(cx.listener(|this, _e, _w, cx| {
                        this.git.commits_collapsed = !this.git.commits_collapsed;
                        cx.notify();
                    })),
                ),
            )
            .when(!collapsed, |el| {
                let commits = &status.recent_commits;
                el.children(commits.iter().enumerate().flat_map(|(ix, commit)| {
                    let last = ix + 1 == commits.len();
                    let prev = ix.checked_sub(1).map(|p| &commits[p]);
                    // A quiet heading where the day bucket changes ("Today",
                    // "This week", ...) replaces the time on every row.
                    let bucket = time_bucket(&commit.relative_time);
                    let heading = (prev.map(|p| time_bucket(&p.relative_time)) != Some(bucket))
                        .then(|| commit_bucket_heading(bucket, cx).into_any_element());
                    // The author is noise when it is the same person all the
                    // way down; show it only where it changes.
                    let show_author = prev.is_none_or(|p| p.author != commit.author);
                    // Where the remote branch sits: between the last unpushed
                    // commit and the first pushed one.
                    let divider =
                        (has_upstream && ix > 0 && commits[ix - 1].unpushed && !commit.unpushed)
                            .then(|| {
                                self.render_git_upstream_divider(status, cx)
                                    .into_any_element()
                            });
                    heading.into_iter().chain(divider).chain(std::iter::once(
                        self.render_git_commit_row(ix, last, commit, has_upstream, show_author, cx)
                            .into_any_element(),
                    ))
                }))
            })
    }

    /// Amend and undo, on the HEAD row. Amend stays lit while the composer is
    /// open for it.
    fn render_git_head_actions(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let busy = self.git.busy.is_some();
        let amend = self.git.amend;
        h_flex()
            .flex_shrink_0()
            .self_center()
            .gap(px(2.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Button::new("git-head-amend")
                    .ghost()
                    .xsmall()
                    .disabled(busy)
                    .icon(
                        Icon::new(IconName::Pencil)
                            .with_size(px(12.))
                            .text_color(if amend { cx.theme().primary } else { muted }),
                    )
                    .tooltip(if amend {
                        "Cancel amend"
                    } else {
                        "Amend this commit"
                    })
                    .on_click(cx.listener(|this, _e, window, cx| {
                        cx.stop_propagation();
                        this.git_toggle_amend(window, cx)
                    })),
            )
            .child(
                Button::new("git-head-undo")
                    .ghost()
                    .xsmall()
                    .disabled(busy)
                    .icon(
                        Icon::new(IconName::Undo2)
                            .with_size(px(12.))
                            .text_color(muted),
                    )
                    .tooltip("Undo this commit (keep changes staged)")
                    .on_click(cx.listener(|this, _e, window, cx| {
                        cx.stop_propagation();
                        this.git_uncommit_last(window, cx)
                    })),
            )
    }

    /// A labelled break in the rail marking where the upstream branch points.
    fn render_git_upstream_divider(&self, status: &RepoStatus, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let rule = cx.theme().border.opacity(0.6);
        div().w_full().px(px(tokens::SPACE_3)).child(
            h_flex()
                .w_full()
                .h(px(18.))
                .pr(px(tokens::SPACE_3))
                .gap(px(tokens::SPACE_2))
                .items_center()
                .child(
                    div().relative().w(px(18.)).h_full().flex_shrink_0().child(
                        div()
                            .absolute()
                            .left(px(8.5))
                            .top_0()
                            .bottom_0()
                            .w(px(1.))
                            .bg(cx.theme().border),
                    ),
                )
                .child(div().flex_1().h(px(1.)).bg(rule))
                .child(
                    div()
                        .flex_shrink_0()
                        .max_w(px(180.))
                        .truncate()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_size(px(tokens::UI_XS - 0.5))
                        .text_color(muted.opacity(0.8))
                        .child(status.upstream.clone().unwrap_or_default()),
                )
                .child(div().flex_1().h(px(1.)).bg(rule)),
        )
    }

    /// One commit on the history rail: the plain subject first (type tag
    /// and ticket pulled out of it), then ticket · author · time · hash.
    /// The dot is filled orange when unpushed and hollow for merges.
    fn render_git_commit_row(
        &self,
        ix: usize,
        last: bool,
        commit: &CommitInfo,
        has_upstream: bool,
        show_author: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let expanded = self.git.expanded_commit.as_deref() == Some(commit.hash.as_str());
        let hash = commit.hash.clone();
        let unpushed = commit.unpushed && has_upstream;

        let subject = crate::git_panel::parse_commit_subject(&commit.subject);
        let kind_color = match subject.kind {
            Some("feat") => cx.theme().green,
            Some("fix") => cx.theme().warning,
            _ => muted,
        };
        let rail_color = cx.theme().border;
        let dot_color = if unpushed {
            cx.theme().primary
        } else {
            muted.opacity(0.8)
        };
        // Where the rail line runs within this row: from the dot down for
        // the newest commit, up to the dot for the oldest.
        const DOT_Y: f32 = 8.;
        let first = ix == 0;

        v_flex()
            .w_full()
            .child(
                div().w_full().px(px(tokens::SPACE_3)).child(
                    h_flex()
                        .id(("git-commit-row", ix))
                        .w_full()
                        .pr(px(tokens::SPACE_3))
                        .gap(px(tokens::SPACE_2))
                        .rounded(px(tokens::RADIUS_SM))
                        .cursor_pointer()
                        .when(expanded, |s| s.bg(cx.theme().list_hover))
                        .hover(|s| s.bg(cx.theme().list_hover))
                        // The rail.
                        .child(
                            div()
                                .relative()
                                .w(px(18.))
                                .flex_shrink_0()
                                .self_stretch()
                                .when(!(first && last), |rail| {
                                    let line =
                                        div().absolute().left(px(8.5)).w(px(1.)).bg(rail_color);
                                    let line = match (first, last) {
                                        (true, _) => line.top(px(DOT_Y + 4.)).bottom_0(),
                                        (_, true) => line.top_0().h(px(DOT_Y + 4.)),
                                        _ => line.top_0().bottom_0(),
                                    };
                                    rail.child(line)
                                })
                                .child(
                                    div()
                                        .absolute()
                                        .left(px(5.))
                                        .top(px(DOT_Y))
                                        .size(px(8.))
                                        .rounded(px(4.))
                                        .when(subject.is_merge && !unpushed, |d| {
                                            d.border(px(1.5))
                                                .border_color(dot_color)
                                                .bg(cx.theme().sidebar)
                                        })
                                        .when(!subject.is_merge || unpushed, |d| d.bg(dot_color)),
                                ),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w(px(0.))
                                .py(px(4.))
                                .gap(px(1.))
                                .child(
                                    h_flex()
                                        .min_w(px(0.))
                                        .items_center()
                                        .gap(px(6.))
                                        .when_some(subject.kind, |row, kind| {
                                            row.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .text_size(px(tokens::UI_XS))
                                                    .font_medium()
                                                    .text_color(kind_color)
                                                    .child(kind.to_string()),
                                            )
                                        })
                                        .when_some(subject.ticket, |row, ticket| {
                                            row.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .font_family(cx.theme().mono_font_family.clone())
                                                    .text_size(px(tokens::UI_XS - 0.5))
                                                    .text_color(muted)
                                                    .child(ticket.to_string()),
                                            )
                                        })
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w(px(0.))
                                                .truncate()
                                                .text_size(px(tokens::UI_SM))
                                                .text_color(if subject.is_merge { muted } else { fg })
                                                .child(subject.text.to_string()),
                                        )
                                        // HEAD wears its badge; every other row ends in its
                                        // hash, right-aligned so the hashes form a column.
                                        .when(first, |row| {
                                            let tint = if unpushed {
                                                cx.theme().primary
                                            } else {
                                                cx.theme().green
                                            };
                                            row.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .px(px(4.))
                                                    .rounded(px(tokens::RADIUS_SM))
                                                    .border_1()
                                                    .border_color(tint.opacity(0.45))
                                                    .font_family(cx.theme().mono_font_family.clone())
                                                    .text_size(px(tokens::UI_XS - 1.5))
                                                    .text_color(tint)
                                                    // The hover actions take its place.
                                                    .group_hover("git-head-row", |s| s.invisible())
                                                    .child("HEAD"),
                                            )
                                        })
                                        .when(!first, |row| {
                                            let copy_hash = commit.hash.clone();
                                            let when = commit.relative_time.clone();
                                            row.child(
                                                div()
                                                    .id(("git-copy-sha", ix))
                                                    .flex_shrink_0()
                                                    .font_family(cx.theme().mono_font_family.clone())
                                                    .text_size(px(tokens::UI_XS - 0.5))
                                                    .text_color(if unpushed {
                                                        cx.theme().primary
                                                    } else {
                                                        muted.opacity(0.7)
                                                    })
                                                    .hover(move |s| s.text_color(fg))
                                                    .child(commit.hash.clone())
                                                    .tooltip(move |window, cx| {
                                                        let text = if unpushed {
                                                            format!("{when} \u{b7} not pushed yet \u{b7} click to copy")
                                                        } else {
                                                            format!("{when} \u{b7} click to copy")
                                                        };
                                                        gpui_kit::component::tooltip::Tooltip::new(text)
                                                            .build(window, cx)
                                                    })
                                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                        cx.stop_propagation();
                                                    })
                                                    .on_click(cx.listener(move |this, _e, _w, cx| {
                                                        cx.stop_propagation();
                                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                                            copy_hash.clone(),
                                                        ));
                                                        this.set_git_notice(
                                                            format!("Copied commit {copy_hash}"),
                                                            false,
                                                            cx,
                                                        );
                                                    })),
                                            )
                                        }),
                                )
                                .when(show_author, |col| {
                                    col.child(
                                        h_flex()
                                            .min_w(px(0.))
                                            .items_center()
                                            .gap(px(5.))
                                            .text_size(px(tokens::UI_XS))
                                            .text_color(muted)
                                            .child(author_avatar(&commit.author))
                                            .child(div().min_w(px(0.)).truncate().child(commit.author.clone())),
                                    )
                                }),
                        )
                        // HEAD carries the actions that rewrite it. They float
                        // over the row on hover instead of permanently taking
                        // width, which squeezed the subject and author to "…".
                        .when(first, |row| {
                            row.relative().group("git-head-row").child(
                                // Opaque base + the row's hover tint on top, so
                                // the subject underneath never shows through.
                                div()
                                    .absolute()
                                    .right(px(tokens::SPACE_2))
                                    .top(px(tokens::SPACE_1))
                                    .rounded(px(tokens::RADIUS_SM))
                                    .bg(cx.theme().sidebar)
                                    .invisible()
                                    .group_hover("git-head-row", |s| s.visible())
                                    .child(
                                        h_flex()
                                            .pl(px(tokens::SPACE_2))
                                            .rounded(px(tokens::RADIUS_SM))
                                            .bg(cx.theme().list_hover)
                                            .child(self.render_git_head_actions(cx)),
                                    ),
                            )
                        })
                        .on_click(cx.listener(move |this, _e, _w, cx| {
                            this.git_toggle_commit(hash.clone(), cx);
                        }))
                        .context_menu({
                            let view = cx.entity();
                            let commit = commit.clone();
                            move |menu, _window, _cx| git_commit_context_menu(menu, view.clone(), &commit, expanded)
                        }),
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
    /// One quiet line rather than a second card: the clean state below is the persistent message.
    fn render_git_notice(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let card = h_flex()
            .w_full()
            .flex_shrink_0()
            .items_center()
            .gap(px(tokens::SPACE_2))
            .px(px(tokens::SPACE_4))
            .py(px(tokens::SPACE_2))
            .text_size(px(tokens::UI_XS));
        if let Some(label) = self.git.busy {
            return card
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
                    .hover(|s| s.bg(cx.theme().list_hover))
                    .child(
                        Icon::new(if notice.is_error {
                            IconName::X
                        } else {
                            IconName::Check
                        })
                        .xsmall()
                        .text_color(tone),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_color(cx.theme().foreground)
                            .child(notice.text),
                    )
                    .child(Icon::new(IconName::X).with_size(px(10.)).text_color(muted))
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

/// The line as the results list shows it: leading indentation dropped, and a
/// long lead-in before the first hit cut to an ellipsis so the hit is visible
/// in a narrow panel. Ranges are shifted to match.
fn search_preview(line: &str, ranges: &[(usize, usize)]) -> (String, Vec<(usize, usize)>) {
    const LEAD: usize = 24;
    let trimmed = line.trim_start();
    let mut cut = line.len() - trimmed.len();
    let mut prefix = "";
    if let Some(&(first, _)) = ranges.first()
        && first > cut + LEAD * 2
    {
        let mut at = first - LEAD;
        while !line.is_char_boundary(at) {
            at -= 1;
        }
        cut = at;
        prefix = "\u{2026}";
    }
    let text = format!("{prefix}{}", &line[cut..]);
    let shift = |x: usize| x.saturating_sub(cut) + prefix.len();
    let ranges = ranges
        .iter()
        .filter(|(a, b)| *a >= cut && *b <= line.len())
        .map(|&(a, b)| (shift(a), shift(b)))
        .collect();
    (text, ranges)
}
