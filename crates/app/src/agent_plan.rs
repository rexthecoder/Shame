//! Plan mode, made visible: when the agent finishes planning, the plan appears
//! as a card you can read in full, then approve (and let it implement the plan)
//! or send back to keep planning.

use std::path::{Path, PathBuf};

use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::agent_ctx::ModeId;
use crate::{DartIde, tokens};

/// Where a plan stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlanState {
    /// Waiting for you to decide.
    Pending,
    /// Approved; the agent was told to implement it in this mode.
    Approved(ModeId),
    /// Sent back: you are giving feedback and it will plan again.
    KeepPlanning,
}

impl PlanState {
    fn label(self) -> String {
        match self {
            PlanState::Pending => "Waiting for your approval".to_string(),
            PlanState::Approved(ModeId::Auto) => "Approved \u{b7} running in Auto mode".to_string(),
            PlanState::Approved(_) => "Approved \u{b7} running in Edit mode".to_string(),
            PlanState::KeepPlanning => "Sent back for changes".to_string(),
        }
    }
}

/// Where plans without a file of their own are saved to be opened: `~/.shame/plans`.
fn plans_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".shame").join("plans"))
}

/// A short file-name-safe slug from a plan's title: `"# Add Dark Mode!"` -> `add-dark-mode`.
pub(crate) fn plan_slug(text: &str) -> String {
    let title = text
        .lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .find(|l| !l.is_empty())
        .unwrap_or("plan");
    let title = title.strip_prefix("Plan:").map(str::trim).unwrap_or(title);
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() { "plan".to_string() } else { slug }
}

/// Save `text` as `<dir>/<timestamp>-<slug>.md` and return the path.
pub(crate) fn write_plan_file(dir: &Path, text: &str, now_ms: u128) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{now_ms}-{}.md", plan_slug(text)));
    std::fs::write(&path, text)?;
    Ok(path)
}

/// The message sent when you approve a plan.
const APPROVAL_MESSAGE: &str = "The plan is approved. Go ahead and implement it.";

impl DartIde {
    /// Approve the plan and have the agent implement it, in `mode`.
    pub(crate) fn approve_plan(&mut self, item_ix: usize, mode: ModeId, cx: &mut Context<Self>) {
        if self.agent.running_pid.is_some() {
            return;
        }
        if let Some(crate::rails::agent::AgentItem::Plan { state, .. }) = self.agent.items.get_mut(item_ix) {
            *state = PlanState::Approved(mode);
        }
        self.agent.mode_ix = mode.index();
        self.save_session_state();
        self.agent_run_prompt(APPROVAL_MESSAGE.to_string(), cx);
    }

    /// Send the plan back: stay in Plan mode and put the cursor in the message box.
    pub(crate) fn keep_planning(&mut self, item_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(crate::rails::agent::AgentItem::Plan { state, .. }) = self.agent.items.get_mut(item_ix) {
            *state = PlanState::KeepPlanning;
        }
        self.agent.mode_ix = ModeId::Plan.index();
        self.agent.input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// Open the plan as a document in the editor, rendered like any markdown file.
    /// A plan that has no file yet is saved to `~/.shame/plans` first.
    pub(crate) fn open_plan_in_editor(&mut self, item_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(crate::rails::agent::AgentItem::Plan { text, path, .. }) = self.agent.items.get_mut(item_ix) else {
            return;
        };
        let target = match path.clone().filter(|p| p.is_file()) {
            Some(existing) => existing,
            None => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                let Some(saved) = plans_dir().and_then(|dir| write_plan_file(&dir, text, now).ok()) else {
                    return;
                };
                *path = Some(saved.clone());
                saved
            }
        };
        self.open_path(target, window, cx);
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            if tab.is_markdown() {
                tab.md_mode = crate::MarkdownMode::Preview;
            }
        }
        cx.notify();
    }

    fn toggle_plan_body(&mut self, item_ix: usize, cx: &mut Context<Self>) {
        if let Some(crate::rails::agent::AgentItem::Plan { expanded, .. }) = self.agent.items.get_mut(item_ix) {
            *expanded = !*expanded;
        }
        cx.notify();
    }

    pub(crate) fn render_plan_card(
        &self,
        item_ix: usize,
        text: &str,
        path: Option<&PathBuf>,
        state: PlanState,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let green = cx.theme().green;
        let border = cx.theme().border;
        let list_hover = cx.theme().list_hover;
        let pending = state == PlanState::Pending;
        let can_decide = pending && self.agent.running_pid.is_none();
        let _ = path;
        // A long pending plan is cut short with a "show full plan" link; once decided
        // it is hidden until you ask to see it.
        let long = text.lines().count() > 16;
        let capped = pending && !expanded && long;
        let show_body = pending || expanded;
        let to_copy = text.to_string();

        let button = |id: &'static str, label: &'static str, primary_style: bool| {
            div()
                .id(id)
                .h(px(28.))
                .px(px(12.))
                .flex()
                .items_center()
                .rounded(px(tokens::RADIUS_SM))
                .cursor_pointer()
                .text_size(px(tokens::UI_SM))
                .border_1()
                .when(primary_style, |b| {
                    b.bg(primary).border_color(primary).text_color(cx.theme().primary_foreground).hover(|h| h.opacity(0.9))
                })
                .when(!primary_style, |b| {
                    b.border_color(border).text_color(fg).hover(move |h| h.bg(list_hover))
                })
                .child(label)
        };

        v_flex()
            .w_full()
            .gap(px(tokens::SPACE_3))
            .p(px(tokens::SPACE_4))
            .rounded(px(tokens::RADIUS_CARD))
            .bg(cx.theme().muted)
            .border_1()
            .border_color(if pending { primary.opacity(0.55) } else { border })
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
                            .child(div().text_size(px(tokens::UI)).font_medium().text_color(fg).child("Plan"))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(if matches!(state, PlanState::Approved(_)) { green } else { muted })
                                    .child(state.label()),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap(px(tokens::SPACE_3))
                            .text_size(px(tokens::UI_XS))
                            .text_color(muted)
                            .child(
                                div()
                                    .id(("plan-open", item_ix))
                                    .cursor_pointer()
                                    .text_color(primary)
                                    .hover(|s| s.opacity(0.85))
                                    .child("Open in editor")
                                    .on_click(cx.listener(move |this, _e, window, cx| {
                                        this.open_plan_in_editor(item_ix, window, cx);
                                    })),
                            )
                            .child(
                                div()
                                    .id(("plan-copy", item_ix))
                                    .cursor_pointer()
                                    .hover(move |s| s.text_color(fg))
                                    .child("Copy")
                                    .on_click(move |_e, _w, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(to_copy.clone()));
                                    }),
                            )
                            .when(!pending, |row| {
                                row.child(
                                    div()
                                        .id(("plan-toggle", item_ix))
                                        .cursor_pointer()
                                        .hover(move |s| s.text_color(fg))
                                        .child(if expanded { "Hide plan" } else { "View plan" })
                                        .on_click(cx.listener(move |this, _e, _w, cx| this.toggle_plan_body(item_ix, cx))),
                                )
                            }),
                    ),
            )
            .when(show_body, |card| {
                card.child(
                    div()
                        .id(("plan-body", item_ix))
                        .w_full()
                        .when(capped, |body| body.max_h(px(440.)).overflow_hidden())
                        .child(crate::markdown::render_markdown_doc(text, item_ix, cx)),
                )
            })
            .when(pending && long, |card| {
                card.child(
                    div()
                        .id(("plan-show-full", item_ix))
                        .cursor_pointer()
                        .text_size(px(tokens::UI_SM))
                        .text_color(primary)
                        .hover(|s| s.opacity(0.85))
                        .child(if expanded { "Show less" } else { "Show full plan \u{2193}" })
                        .on_click(cx.listener(move |this, _e, _w, cx| this.toggle_plan_body(item_ix, cx))),
                )
            })
            .when(pending, |card| {
                card.child(
                    h_flex()
                        .w_full()
                        .flex_wrap()
                        .gap(px(tokens::SPACE_2))
                        .child(
                            button("plan-approve-edit", "Approve and edit", can_decide)
                                .on_click(cx.listener(move |this, _e, _w, cx| {
                                    this.approve_plan(item_ix, ModeId::Edit, cx);
                                })),
                        )
                        .child(
                            button("plan-approve-auto", "Approve in Auto mode", false)
                                .on_click(cx.listener(move |this, _e, _w, cx| {
                                    this.approve_plan(item_ix, ModeId::Auto, cx);
                                })),
                        )
                        .child(
                            button("plan-keep-planning", "Keep planning", false)
                                .on_click(cx.listener(move |this, _e, window, cx| {
                                    this.keep_planning(item_ix, window, cx);
                                })),
                        ),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{plan_slug, write_plan_file};

    #[core::prelude::v1::test]
    fn slugs_come_from_the_plan_title() {
        assert_eq!(plan_slug("# Add Dark Mode!\n\nbody"), "add-dark-mode");
        assert_eq!(plan_slug("# Plan: Enhance Claude's Effectiveness"), "enhance-claude-s-effectiveness");
        assert_eq!(plan_slug("\n\n  Fix the login bug  \n"), "fix-the-login-bug");
        assert_eq!(plan_slug(""), "plan");
        assert_eq!(plan_slug("### ???"), "plan", "nothing usable falls back to a default");
        assert!(plan_slug(&"word ".repeat(30)).len() <= 40);
    }

    #[core::prelude::v1::test]
    fn a_plan_is_saved_as_a_dated_markdown_file() {
        let dir = std::env::temp_dir().join(format!("plans-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = write_plan_file(&dir, "# Ship It\n\n1. Do the thing", 1234).unwrap();
        assert_eq!(path.file_name().unwrap().to_str().unwrap(), "1234-ship-it.md");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# Ship It\n\n1. Do the thing");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
