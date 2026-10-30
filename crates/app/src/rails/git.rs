//! The Git panel (the "Git" tab of the left sidebar): branch bar with fetch / pull / push, commit
//! box, and the Staged / Changes lists. State and actions live in `crate::git_panel`.

use std::path::Path;

use core_workspace::{ChangeKind, ChangedFile, RepoStatus};
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

/// Solid accent with dark text: the one primary action of the panel.
fn accent_solid(cx: &App) -> ButtonCustomVariant {
    ButtonCustomVariant::new(cx)
        .color(cx.theme().primary)
        .foreground(rgb(0x1a120e).into())
        .hover(cx.theme().primary_hover)
        .active(cx.theme().primary_active)
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
                .h(px(30.))
                .px(px(tokens::SPACE_3))
                .rounded(px(tokens::RADIUS_PANEL))
                .border_1()
                .border_color(if active {
                    tokens::accent_dim()
                } else {
                    gpui_kit::transparent_black()
                })
                .when(active, |s| s.bg(cx.theme().tab_active))
                .cursor_pointer()
                .child(Icon::new(icon).small().text_color(if active {
                    cx.theme().primary
                } else {
                    muted
                }))
                .child(
                    div()
                        .text_size(px(tokens::UI))
                        .text_color(if active { cx.theme().foreground } else { muted })
                        .child(label),
                )
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
            .justify_between()
            .px(px(tokens::SPACE_4))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .items_center()
                    .gap(px(tokens::SPACE_2))
                    .child(tab(
                        "sidebar-tab-files",
                        "Files",
                        IconName::Folder,
                        SidebarTab::Files,
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
                    ),
            )
            .child(
                div()
                    .id("close-rail")
                    .cursor_pointer()
                    .text_color(muted)
                    .hover(|s| s.text_color(cx.theme().foreground))
                    .child(Icon::new(IconName::X).xsmall())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.sidebar_open = false;
                        cx.notify();
                    })),
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
            .when(self.git.branches_open, |el| {
                el.child(self.render_git_branch_list(cx))
            })
            .when(!status.is_clean() || self.git.amend, |el| {
                el.child(self.render_git_commit_box(status, cx))
            })
            .child(self.render_git_notice(cx))
            .child(self.render_git_changes(status, cx))
    }

    fn render_git_branch_bar(&self, status: &RepoStatus, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let busy = self.git.busy.is_some();
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
            .child(
                h_flex()
                    .id("git-branch-toggle")
                    .min_w(px(0.))
                    .items_center()
                    .gap(px(tokens::SPACE_2))
                    .h(px(24.))
                    .px(px(tokens::SPACE_4))
                    .rounded(px(tokens::RADIUS_PILL))
                    .bg(cx.theme().list_hover.opacity(0.55))
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().list_hover))
                    .child(
                        Icon::new(IconName::GitBranch)
                            .small()
                            .text_color(cx.theme().green),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(tokens::UI_SM))
                            .text_color(fg)
                            .child(name),
                    )
                    .child(Icon::new(IconName::ChevronDown).xsmall().text_color(muted))
                    .on_click(
                        cx.listener(|this, _event, _window, cx| this.git_toggle_branches(cx)),
                    ),
            )
            .child(
                // Only what applies: fetch always; pull when behind; push when ahead; publish
                // when the branch has no upstream yet.
                h_flex()
                    .items_center()
                    .flex_shrink_0()
                    .gap(px(tokens::SPACE_2))
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

    fn render_git_branch_list(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .w_full()
            .flex_shrink_0()
            .max_h(px(220.))
            .mx(px(tokens::SPACE_4))
            .mb(px(tokens::SPACE_3))
            .p(px(tokens::SPACE_2))
            .gap(px(tokens::SPACE_1))
            .rounded(px(tokens::RADIUS_PANEL))
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .items_center()
                    .gap(px(tokens::SPACE_2))
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.git.branch_input).small()),
                    )
                    .child(
                        Button::new("git-create-branch")
                            .small()
                            .label("Create")
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.git_create_branch(window, cx)
                            })),
                    ),
            )
            .child(
                v_flex()
                    .id("git-branch-scroll")
                    .overflow_y_scroll()
                    .children(self.git.branches.iter().enumerate().map(|(ix, branch)| {
                        let name = branch.name.clone();
                        let current = branch.current;
                        h_flex()
                            .id(("git-branch", ix))
                            .h(px(24.))
                            .px(px(tokens::SPACE_3))
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .rounded(px(tokens::RADIUS_SM))
                            .cursor_pointer()
                            .hover(|s| s.bg(cx.theme().list_hover))
                            .text_size(px(tokens::UI_SM))
                            .text_color(if current {
                                cx.theme().foreground
                            } else {
                                muted
                            })
                            .child(div().w(px(12.)).when(current, |el| {
                                el.child(
                                    Icon::new(IconName::Check)
                                        .xsmall()
                                        .text_color(cx.theme().green),
                                )
                            }))
                            .child(div().truncate().child(branch.name.clone()))
                            .when(!current, |el| {
                                el.on_click(cx.listener(move |this, _event, window, cx| {
                                    this.git_checkout(name.clone(), window, cx)
                                }))
                            })
                    })),
            )
    }

    /// The commit composer: type chips, the message, a suggestion, and one button that always
    /// offers the next sensible step (commit, commit all, amend) instead of a dead control.
    fn render_git_commit_box(&self, status: &RepoStatus, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let theme_fg = cx.theme().foreground;
        let staged = status.staged.len();
        let unstaged = status.unstaged.len();
        let message = self.git.message.read(cx).value().to_string();
        let has_message = !message.trim().is_empty();
        let amend = self.git.amend;
        let busy = self.git.busy.is_some();
        let subject_len = message.lines().next().unwrap_or("").chars().count();
        let plural = |n: usize| if n == 1 { "" } else { "s" };

        enum Action {
            Ready(String, &'static str),
            NeedsMessage,
            Nothing,
        }
        let action = if amend {
            Action::Ready(
                "Amend commit".into(),
                "Replace the last commit (\u{2318}\u{21b5})",
            )
        } else if staged > 0 && has_message {
            Action::Ready(
                format!("Commit {staged} file{}", plural(staged)),
                "Commit the staged files (\u{2318}\u{21b5})",
            )
        } else if staged == 0 && unstaged > 0 && has_message {
            Action::Ready(
                format!("Commit all {unstaged}"),
                "Stage everything and commit (\u{2318}\u{21b5})",
            )
        } else if staged + unstaged > 0 {
            Action::NeedsMessage
        } else {
            Action::Nothing
        };

        let types = self.git.commit_types.clone().unwrap_or_default();
        let current_type = core_workspace::commit_type_of(message.lines().next().unwrap_or(""))
            .map(str::to_string);
        let suggestion = (!has_message && !amend)
            .then(|| core_workspace::suggest_message(self.commit_candidates(), &types))
            .flatten();

        let chip =
            |id: (&'static str, usize), label: String, active: bool, cx: &mut Context<Self>| {
                let (primary, border) = (cx.theme().primary, cx.theme().border);
                div()
                    .id(id)
                    .px(px(8.))
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .rounded(px(tokens::RADIUS_PILL))
                    .border_1()
                    .border_color(if active { primary.opacity(0.6) } else { border })
                    .bg(if active {
                        primary.opacity(0.16)
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .text_size(px(tokens::UI_XS))
                    .text_color(if active { primary } else { muted })
                    .cursor_pointer()
                    .hover(move |s| s.text_color(theme_fg))
                    .child(label)
            };
        let show_chips = suggestion.is_some() || (!types.is_empty() && !amend);

        v_flex()
            .w_full()
            .flex_shrink_0()
            .px(px(tokens::SPACE_4))
            .pb(px(tokens::SPACE_4))
            .child(
                // One surface: message, quick fills, then the footer with the action.
                v_flex()
                    .w_full()
                    .rounded(px(tokens::RADIUS_PANEL))
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .overflow_hidden()
                    // Cmd/Ctrl+Enter runs the primary action from inside the message box.
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        let m = event.keystroke.modifiers;
                        if (m.platform || m.control) && event.keystroke.key == "enter" {
                            this.git_primary_action(window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        div()
                            .w_full()
                            .px(px(CARD_INSET))
                            .pt(px(8.))
                            .pb(px(4.))
                            // No field chrome or padding of its own: the card is the field.
                            .child(
                                Textarea::new(&self.git.message)
                                    .appearance(false)
                                    .bordered(false)
                                    .px(px(0.))
                                    .py(px(0.)),
                            ),
                    )
                    // Quick fills: a suggested subject and this repo's commit types.
                    .when(show_chips, |el| {
                        el.child(
                            h_flex()
                                .w_full()
                                .flex_wrap()
                                .items_center()
                                .gap(px(6.))
                                .px(px(CARD_INSET))
                                .pb(px(8.))
                                .when_some(suggestion, |row, text| {
                                    row.child(
                                        h_flex()
                                            .id("git-suggestion")
                                            .max_w_full()
                                            .items_center()
                                            .gap(px(tokens::SPACE_2))
                                            .px(px(8.))
                                            .h(px(20.))
                                            .rounded(px(tokens::RADIUS_PILL))
                                            .bg(cx.theme().primary.opacity(0.14))
                                            .cursor_pointer()
                                            .hover(|s| s.bg(cx.theme().primary.opacity(0.24)))
                                            .text_size(px(tokens::UI_XS))
                                            .text_color(cx.theme().primary)
                                            .child(Icon::new(IconName::Sparkles).with_size(px(11.)))
                                            .child(div().truncate().child(text))
                                            .on_click(cx.listener(|this, _e, window, cx| {
                                                this.git_apply_suggestion(window, cx)
                                            })),
                                    )
                                })
                                .when(!types.is_empty() && !amend, |row| {
                                    row.children(types.iter().enumerate().map(|(i, ty)| {
                                        let active = current_type.as_deref() == Some(ty.as_str());
                                        let ty_owned = ty.clone();
                                        chip(("git-type", i), ty.clone(), active, cx).on_click(
                                            cx.listener(move |this, _e, window, cx| {
                                                this.git_set_commit_type(&ty_owned, window, cx)
                                            }),
                                        )
                                    }))
                                }),
                        )
                    })
                    .child(
                        h_flex()
                            .w_full()
                            .h(px(36.))
                            .items_center()
                            .justify_between()
                            .px(px(CARD_INSET))
                            .border_t_1()
                            .border_color(cx.theme().border.opacity(0.6))
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(tokens::SPACE_3))
                                    .child(
                                        h_flex()
                                            .id("git-amend")
                                            .items_center()
                                            .gap(px(tokens::SPACE_2))
                                            .h(px(22.))
                                            .ml(px(-6.))
                                            .px(px(6.))
                                            .rounded(px(tokens::RADIUS_PILL))
                                            .cursor_pointer()
                                            .when(amend, |el| {
                                                el.bg(cx.theme().warning.opacity(0.16))
                                            })
                                            .hover(|s| s.bg(cx.theme().list_hover))
                                            .text_size(px(tokens::UI_XS))
                                            .text_color(if amend {
                                                cx.theme().warning
                                            } else {
                                                muted
                                            })
                                            .child(Icon::new(IconName::Undo2).with_size(px(11.)))
                                            .child("Amend")
                                            .on_click(cx.listener(|this, _e, window, cx| {
                                                this.git_toggle_amend(window, cx)
                                            })),
                                    )
                                    .when(subject_len > 50, |el| {
                                        el.child(
                                            div()
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
                            .child(match action {
                                Action::Ready(label, tip) => Button::new("git-commit")
                                    .xsmall()
                                    .custom(accent_solid(cx))
                                    .disabled(busy)
                                    .label(label)
                                    .tooltip(tip)
                                    .on_click(cx.listener(|this, _e, window, cx| {
                                        this.git_primary_action(window, cx)
                                    }))
                                    .into_any_element(),
                                Action::NeedsMessage => div()
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(muted)
                                    .child("Write a message")
                                    .into_any_element(),
                                Action::Nothing => div().into_any_element(),
                            }),
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
        if status.is_clean() {
            return list.child(self.render_git_clean_state(status, cx));
        }
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
            .gap(px(tokens::SPACE_4))
            .p(px(tokens::SPACE_7))
            .child(
                Icon::new(if cta.is_some() {
                    IconName::GitBranch
                } else {
                    IconName::Check
                })
                .with_size(px(20.))
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
                    Button::new("git-amend-link")
                        .ghost()
                        .xsmall()
                        .icon(
                            Icon::new(IconName::Undo2)
                                .with_size(px(11.))
                                .text_color(muted),
                        )
                        .label("Amend last commit")
                        .on_click(
                            cx.listener(|this, _e, window, cx| this.git_toggle_amend(window, cx)),
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
    /// and swaps to the Stage / Unstage / Discard buttons while the row is hovered.
    fn render_git_file_row(
        &self,
        section: Section,
        ix: usize,
        file: ChangedFile,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let muted = cx.theme().muted_foreground;
        let color = match file.kind {
            ChangeKind::Added | ChangeKind::Untracked => cx.theme().green,
            ChangeKind::Modified | ChangeKind::TypeChanged => cx.theme().warning,
            ChangeKind::Deleted | ChangeKind::Conflicted => cx.theme().red,
            ChangeKind::Renamed | ChangeKind::Copied => cx.theme().info,
        };
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
        let discard_armed = self.git.confirm_discard.as_deref() == Some(file.path.as_str());
        let reviewed = self
            .git
            .reviewed
            .contains(&(file.path.clone(), section == Section::Staged));
        let slot_width = if section == Section::Changes {
            50.
        } else {
            26.
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

        h_flex()
            .id(id)
            .group(group.clone())
            .w_full()
            .h(px(24.))
            .items_center()
            .gap(px(tokens::SPACE_3))
            .pl(px(tokens::SPACE_5))
            .pr(px(tokens::SPACE_2))
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().list_hover))
            .child(crate::rails::files::render_file_icon(&name))
            .child(
                h_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .items_baseline()
                    .gap(px(tokens::SPACE_3))
                    .child(
                        div()
                            .flex_shrink_0()
                            .max_w(px(160.))
                            .truncate()
                            .text_size(px(tokens::UI_SM))
                            .text_color(if file.kind == ChangeKind::Deleted {
                                muted
                            } else {
                                cx.theme().foreground
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
                            .pr(px(tokens::SPACE_3))
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
                                        action("git-discard", IconName::Undo2, "Discard changes")
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
                                        action("git-stage", IconName::Plus, "Stage").on_click(
                                            cx.listener(move |this, _event, window, cx| {
                                                this.git_stage(
                                                    std::slice::from_ref(&stage_file),
                                                    window,
                                                    cx,
                                                )
                                            }),
                                        ),
                                    )
                                })
                                .when(section == Section::Staged, |row| {
                                    row.child(
                                        action("git-unstage", IconName::Minus, "Unstage").on_click(
                                            cx.listener(move |this, _event, window, cx| {
                                                this.git_unstage(
                                                    std::slice::from_ref(&unstage_file),
                                                    window,
                                                    cx,
                                                )
                                            }),
                                        ),
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
