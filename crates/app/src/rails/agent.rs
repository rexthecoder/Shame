//! The Agent rail (`docs/design-spec.md` §3.23–31): its own standalone panel (`self.agent_open`),
//! completely separated from the Simulator rail (`self.sim.attached`).

use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, tokens};

#[derive(Clone)]
pub(crate) struct AgentTask {
    pub(crate) label: String,
    pub(crate) done: bool,
}

#[derive(Clone)]
pub(crate) enum AgentToolKind {
    File,
    Terminal,
}

#[derive(Clone)]
pub(crate) struct AgentToolCall {
    pub(crate) kind: AgentToolKind,
    pub(crate) label: String,
    pub(crate) target_path: Option<String>,
}

#[derive(Clone)]
pub(crate) enum AgentItem {
    ToolGroup {
        id: usize,
        header: String,
        collapsed: bool,
        calls: Vec<AgentToolCall>,
    },
    Message {
        is_user: bool,
        text: String,
    },
}

#[derive(Clone)]
pub(crate) struct AgentPendingChange {
    pub(crate) label: String,
    pub(crate) path: String,
    pub(crate) added: u32,
}

pub(crate) struct AgentState {
    pub(crate) title: String,
    pub(crate) mode_ix: usize,
    pub(crate) model_ix: usize,
    pub(crate) tasks: Vec<AgentTask>,
    pub(crate) items: Vec<AgentItem>,
    pub(crate) pending: Vec<AgentPendingChange>,
    pub(crate) input: Entity<InputState>,
    pub(crate) scroll: ScrollHandle,
}

const MODES: &[&str] = &["Default", "Plan", "Auto-Edit"];
const MODELS: &[&str] = &["demo-large", "claude-sonnet-4", "gpt-5"];

impl AgentState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<DartIde>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Message agent\u{2026}"));
        let mut state = Self {
            title: "Demo Agent".to_string(),
            mode_ix: 0,
            model_ix: 0,
            tasks: Vec::new(),
            items: Vec::new(),
            pending: Vec::new(),
            input,
            scroll: ScrollHandle::new(),
        };
        state.seed_reference_conversation();
        state
    }

    pub(crate) fn seed_reference_conversation(&mut self) {
        self.title = "Demo Agent".to_string();
        self.tasks = vec![
            AgentTask {
                label: "Add movePane to PaneController".to_string(),
                done: true,
            },
            AgentTask {
                label: "Cover movePane with tests".to_string(),
                done: true,
            },
            AgentTask {
                label: "Run the tests".to_string(),
                done: true,
            },
        ];
        self.items = vec![
            AgentItem::ToolGroup {
                id: 0,
                header: "Agent used a tool".to_string(),
                collapsed: false,
                calls: vec![AgentToolCall {
                    kind: AgentToolKind::File,
                    label: "Read pane_controller.dart".to_string(),
                    target_path: Some("lib/src/pane_controller.dart".to_string()),
                }],
            },
            AgentItem::Message {
                is_user: false,
                text: "`PaneController` keeps its panes in `_entries`. I'll add `movePane(id, toIndex)` next to `addPane`, clamping the index the same way, then cover it with a test.".to_string(),
            },
            AgentItem::ToolGroup {
                id: 1,
                header: "Agent used 3 tools".to_string(),
                collapsed: false,
                calls: vec![AgentToolCall {
                    kind: AgentToolKind::Terminal,
                    label: "flutter test test/move_pane_test.dart".to_string(),
                    target_path: None,
                }],
            },
            AgentItem::Message {
                is_user: false,
                text: "Done. `movePane` reorders a pane and notifies listeners once; sizes and visibility move with it. Both new tests pass.".to_string(),
            },
        ];
        self.pending = vec![
            AgentPendingChange {
                label: "Edit pane_controller.dart".to_string(),
                path: "lib/src/pane_controller.dart".to_string(),
                added: 18,
            },
            AgentPendingChange {
                label: "Create move_pane_test.dart".to_string(),
                path: "test/move_pane_test.dart".to_string(),
                added: 34,
            },
        ];
    }
}

/// Render inline backtick spans (`code`) as small monospace pills inside a message bubble.
fn render_rich_message(text: &str, cx: &App) -> Div {
    let fg = cx.theme().foreground;
    let code_bg = cx.theme().background;
    let border = cx.theme().border;
    let mono = cx.theme().mono_font_family.clone();

    let mut row = h_flex()
        .w_full()
        .flex_wrap()
        .items_center()
        .gap_x(px(3.))
        .gap_y(px(3.))
        .text_size(px(tokens::UI))
        .line_height(px(19.))
        .text_color(fg);

    for (seg_ix, segment) in text.split('`').enumerate() {
        if segment.is_empty() {
            continue;
        }
        if seg_ix % 2 == 1 {
            row = row.child(
                div()
                    .px(px(5.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .bg(code_bg)
                    .border_1()
                    .border_color(border)
                    .font_family(mono.clone())
                    .text_size(px(tokens::UI_SM))
                    .text_color(fg)
                    .child(segment.to_string()),
            );
        } else {
            for word in segment.split_whitespace() {
                row = row.child(div().child(word.to_string()));
            }
        }
    }
    row
}

impl DartIde {
    pub(crate) fn agent_send_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.agent.input.read(cx).value().trim().to_string();
        if prompt.is_empty() {
            return;
        }
        self.agent
            .input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.agent.items.push(AgentItem::Message {
            is_user: true,
            text: prompt.clone(),
        });

        let active_file = self
            .active_editor()
            .and_then(|ed| ed.read(cx).buffer().path())
            .and_then(|p| p.strip_prefix(&self.root).ok().map(|r| r.to_path_buf()))
            .map(|p| p.to_string_lossy().to_string());
        let next_group_id = self.agent.items.len();
        if let Some(file) = &active_file {
            self.agent.items.push(AgentItem::ToolGroup {
                id: next_group_id,
                header: "Agent used a tool".to_string(),
                collapsed: false,
                calls: vec![AgentToolCall {
                    kind: AgentToolKind::File,
                    label: format!("Read {file}"),
                    target_path: Some(file.clone()),
                }],
            });
        }
        let reply = match active_file {
            Some(file) => format!(
                "Inspected `{file}` for `{prompt}`. Ready to apply edits or run workspace checks."
            ),
            None => format!(
                "Analyzed workspace for `{prompt}`. Open a file or select a change below to inspect."
            ),
        };
        self.agent.items.push(AgentItem::Message {
            is_user: false,
            text: reply,
        });
        cx.notify();
    }

    /// Standalone Agent rail (`docs/design-spec.md` §3.23–31).
    pub(crate) fn render_agent_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let green = cx.theme().green;
        let red = cx.theme().red;
        let card_bg = cx.theme().muted;
        let border = cx.theme().border;
        let list_hover = cx.theme().list_hover;
        let mode_label = MODES[self.agent.mode_ix % MODES.len()];
        let model_label = MODELS[self.agent.model_ix % MODELS.len()];

        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .border_1()
            .border_color(border)
            .rounded(px(tokens::RADIUS_CARD))
            .overflow_hidden()
            // 23. AgentTabBar (`Demo Agent ×` + `+`)
            .child(
                h_flex()
                    .w_full()
                    .h(px(tokens::TABSTRIP_H))
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .px(px(tokens::SPACE_3))
                    .border_b_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .child(
                                h_flex()
                                    .id("rail-tab-agent")
                                    .items_center()
                                    .gap(px(tokens::SPACE_2))
                                    .h(px(30.))
                                    .px(px(tokens::SPACE_3))
                                    .rounded(px(tokens::RADIUS_PANEL))
                                    .border_1()
                                    .border_color(tokens::accent_dim())
                                    .bg(cx.theme().tab_active)
                                    .text_size(px(tokens::UI))
                                    .text_color(fg)
                                    .child(Icon::new(IconName::Bot).small().text_color(primary))
                                    .child(self.agent.title.clone())
                                    .child(
                                        div()
                                            .id("agent-tab-close")
                                            .cursor_pointer()
                                            .text_color(muted)
                                            .hover(move |s| s.text_color(fg))
                                            .child(Icon::new(IconName::X).xsmall())
                                            .on_click(cx.listener(|this, _e, _w, cx| {
                                                this.agent_open = false;
                                                cx.notify();
                                            })),
                                    ),
                            )
                            .child(
                                Button::new("agent-new-thread")
                                    .ghost()
                                    .small()
                                    .icon(Icon::new(IconName::Plus).xsmall().text_color(muted))
                                    .tooltip("Reset / New agent thread")
                                    .on_click(cx.listener(|this, _e, _w, cx| {
                                        this.agent.seed_reference_conversation();
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        Button::new("close-right-rail")
                            .ghost()
                            .small()
                            .icon(Icon::new(IconName::X).xsmall().text_color(muted))
                            .tooltip("Close Agent panel")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.agent_open = false;
                                cx.notify();
                            })),
                    ),
            )
            // 24. AgentHeader (`Demo Agent ✎` + actions)
            .child(
                h_flex()
                    .w_full()
                    .h(px(32.))
                    .flex_shrink_0()
                    .px(px(tokens::SPACE_5))
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(border.opacity(0.6))
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .child(
                                div()
                                    .text_size(px(tokens::UI))
                                    .font_medium()
                                    .text_color(fg)
                                    .child(self.agent.title.clone()),
                            )
                            .child(
                                Icon::new(IconName::PenTool)
                                    .with_size(px(11.))
                                    .text_color(muted),
                            ),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(tokens::SPACE_1))
                            .child(
                                Button::new("agent-clear-chat")
                                    .ghost()
                                    .xsmall()
                                    .icon(
                                        Icon::new(IconName::RotateCcwClock)
                                            .with_size(px(12.))
                                            .text_color(muted),
                                    )
                                    .tooltip("Reset conversation")
                                    .on_click(cx.listener(|this, _e, _w, cx| {
                                        this.agent.seed_reference_conversation();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("agent-more")
                                    .ghost()
                                    .xsmall()
                                    .icon(
                                        Icon::new(IconName::Ellipsis)
                                            .with_size(px(12.))
                                            .text_color(muted),
                                    )
                                    .tooltip("Agent options"),
                            ),
                    ),
            )
            // Scrollable conversation body (TaskChecklist + ToolCallCards + MessageBubbles)
            .child(
                v_flex()
                    .id("agent-scroll-body")
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .track_scroll(&self.agent.scroll)
                    .p(px(tokens::SPACE_4))
                    .gap(px(tokens::SPACE_4))
                    // 25. TaskChecklist card
                    .when(!self.agent.tasks.is_empty(), |el| {
                        el.child(
                            v_flex()
                                .w_full()
                                .p(px(tokens::SPACE_4))
                                .gap(px(tokens::SPACE_1))
                                .rounded(px(tokens::RADIUS_CARD))
                                .bg(card_bg)
                                .border_1()
                                .border_color(border)
                                .children(self.agent.tasks.iter().enumerate().map(|(ix, task)| {
                                    let done = task.done;
                                    h_flex()
                                        .id(("agent-task", ix))
                                        .w_full()
                                        .h(px(26.))
                                        .px(px(tokens::SPACE_2))
                                        .items_center()
                                        .gap(px(tokens::SPACE_3))
                                        .rounded(px(tokens::RADIUS_SM))
                                        .cursor_pointer()
                                        .hover(move |s| s.bg(list_hover))
                                        .child(
                                            div()
                                                .size(px(15.))
                                                .rounded(px(tokens::RADIUS_PILL))
                                                .border_1()
                                                .border_color(if done {
                                                    primary
                                                } else {
                                                    muted
                                                })
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .when(done, |c| {
                                                    c.child(
                                                        Icon::new(IconName::Check)
                                                            .with_size(px(9.5))
                                                            .text_color(primary),
                                                    )
                                                }),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w(px(0.))
                                                .truncate()
                                                .text_size(px(12.))
                                                .text_color(if done { muted } else { fg })
                                                .when(done, |s| s.line_through())
                                                .child(task.label.clone()),
                                        )
                                        .on_click(cx.listener(move |this, _e, _w, cx| {
                                            if let Some(t) = this.agent.tasks.get_mut(ix) {
                                                t.done = !t.done;
                                                cx.notify();
                                            }
                                        }))
                                })),
                        )
                    })
                    // 26–28. ToolCallCard, CollapsedGroup, MessageBubble
                    .children(self.agent.items.iter().enumerate().map(|(item_ix, item)| {
                        match item {
                            AgentItem::ToolGroup {
                                id,
                                header,
                                collapsed,
                                calls,
                            } => {
                                let is_collapsed = *collapsed;
                                let group_id = *id;
                                v_flex()
                                    .w_full()
                                    .gap(px(tokens::SPACE_2))
                                    .child(
                                        h_flex()
                                            .id(("agent-tool-group", group_id))
                                            .items_center()
                                            .gap(px(tokens::SPACE_2))
                                            .cursor_pointer()
                                            .text_size(px(tokens::UI_SM))
                                            .text_color(muted)
                                            .hover(move |s| s.text_color(fg))
                                            .child(
                                                Icon::new(IconName::Settings)
                                                    .with_size(px(11.5))
                                                    .text_color(muted),
                                            )
                                            .child(header.clone())
                                            .child(
                                                Icon::new(if is_collapsed {
                                                    IconName::ChevronRight
                                                } else {
                                                    IconName::ChevronDown
                                                })
                                                .with_size(px(11.))
                                                .text_color(muted),
                                            )
                                            .on_click(cx.listener(move |this, _e, _w, cx| {
                                                if let Some(AgentItem::ToolGroup {
                                                    collapsed, ..
                                                }) = this.agent.items.get_mut(item_ix)
                                                {
                                                    *collapsed = !*collapsed;
                                                    cx.notify();
                                                }
                                            })),
                                    )
                                    .when(!is_collapsed, |el| {
                                        el.children(calls.iter().enumerate().map(
                                            |(call_ix, call)| {
                                                let target = call.target_path.clone();
                                                let is_term = matches!(
                                                    call.kind,
                                                    AgentToolKind::Terminal
                                                );
                                                h_flex()
                                                    .id((
                                                        "agent-tool-card",
                                                        item_ix * 16 + call_ix,
                                                    ))
                                                    .w_full()
                                                    .min_h(px(34.))
                                                    .px(px(tokens::SPACE_5))
                                                    .py(px(tokens::SPACE_3))
                                                    .items_center()
                                                    .gap(px(tokens::SPACE_3))
                                                    .rounded(px(tokens::RADIUS_CARD))
                                                    .bg(card_bg)
                                                    .border_1()
                                                    .border_color(border)
                                                    .when(target.is_some(), |s| {
                                                        s.cursor_pointer()
                                                            .hover(move |h| h.bg(list_hover))
                                                    })
                                                    .child(if is_term {
                                                        div()
                                                            .font_family(
                                                                cx.theme()
                                                                    .mono_font_family
                                                                    .clone(),
                                                            )
                                                            .text_size(px(tokens::UI_XS))
                                                            .text_color(primary)
                                                            .child(">_")
                                                            .into_any_element()
                                                    } else {
                                                        Icon::new(IconName::FileCode)
                                                            .with_size(px(13.))
                                                            .text_color(cx.theme().info)
                                                            .into_any_element()
                                                    })
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w(px(0.))
                                                            .truncate()
                                                            .text_size(px(tokens::UI_SM))
                                                            .text_color(fg)
                                                            .child(call.label.clone()),
                                                    )
                                                    .when_some(target, |s, rel_path| {
                                                        s.on_click(cx.listener(
                                                            move |this, _e, window, cx| {
                                                                let full =
                                                                    this.root.join(&rel_path);
                                                                if full.exists() {
                                                                    this.open_path(
                                                                        full, window, cx,
                                                                    );
                                                                }
                                                            },
                                                        ))
                                                    })
                                            },
                                        ))
                                    })
                                    .into_any_element()
                            }
                            AgentItem::Message { is_user, text } => div()
                                .w_full()
                                .px(px(tokens::SPACE_5))
                                .py(px(10.))
                                .rounded(px(tokens::RADIUS_CARD))
                                .bg(if *is_user {
                                    tokens::accent_dim()
                                } else {
                                    card_bg
                                })
                                .border_1()
                                .border_color(if *is_user {
                                    cx.theme().list_active_border
                                } else {
                                    border
                                })
                                .child(render_rich_message(text, cx))
                                .into_any_element(),
                        }
                    })),
            )
            // 29. PendingChanges card (pinned above the composer)
            .when(!self.agent.pending.is_empty(), |el| {
                let count = self.agent.pending.len();
                el.child(
                    v_flex()
                        .w_auto()
                        .flex_shrink_0()
                        .mx(px(tokens::SPACE_4))
                        .mb(px(tokens::SPACE_3))
                        .p(px(tokens::SPACE_3))
                        .gap(px(tokens::SPACE_2))
                        .rounded(px(tokens::RADIUS_CARD))
                        .bg(card_bg)
                        .border_1()
                        .border_color(border)
                        .child(
                            h_flex()
                                .w_full()
                                .px(px(tokens::SPACE_2))
                                .items_center()
                                .justify_between()
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap(px(tokens::SPACE_2))
                                        .child(
                                            Icon::new(IconName::FilePlus)
                                                .with_size(px(13.))
                                                .text_color(primary),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(tokens::UI))
                                                .font_medium()
                                                .text_color(fg)
                                                .child(format!(
                                                    "{count} Pending Change{}",
                                                    if count == 1 { "" } else { "s" }
                                                )),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap(px(tokens::SPACE_2))
                                        .child(
                                            div()
                                                .id("agent-reject-all")
                                                .size(px(22.))
                                                .rounded(px(tokens::RADIUS_PILL))
                                                .bg(cx.theme().background)
                                                .border_1()
                                                .border_color(border)
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .hover(move |s| s.border_color(red))
                                                .child(
                                                    Icon::new(IconName::X)
                                                        .with_size(px(11.))
                                                        .text_color(red),
                                                )
                                                .on_click(cx.listener(|this, _e, _w, cx| {
                                                    this.agent.pending.clear();
                                                    cx.notify();
                                                })),
                                        )
                                        .child(
                                            div()
                                                .id("agent-accept-all")
                                                .size(px(22.))
                                                .rounded(px(tokens::RADIUS_PILL))
                                                .bg(primary)
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .hover(|s| s.opacity(0.9))
                                                .child(
                                                    Icon::new(IconName::Check)
                                                        .with_size(px(12.))
                                                        .text_color(cx.theme().primary_foreground),
                                                )
                                                .on_click(cx.listener(|this, _e, _w, cx| {
                                                    this.agent.pending.clear();
                                                    cx.notify();
                                                })),
                                        ),
                                ),
                        )
                        .child(div().w_full().h(px(1.)).bg(border.opacity(0.6)))
                        .children(self.agent.pending.iter().enumerate().map(|(ix, item)| {
                            let rel_path = item.path.clone();
                            h_flex()
                                .id(("agent-pending-row", ix))
                                .w_full()
                                .h(px(32.))
                                .px(px(tokens::SPACE_2))
                                .items_center()
                                .justify_between()
                                .gap(px(tokens::SPACE_2))
                                .rounded(px(tokens::RADIUS_PANEL))
                                .cursor_pointer()
                                .hover(move |s| s.bg(list_hover))
                                .child(
                                    h_flex()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .items_center()
                                        .gap(px(tokens::SPACE_2))
                                        .child(
                                            Icon::new(IconName::FileCode)
                                                .with_size(px(13.))
                                                .text_color(muted),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(px(12.))
                                                .text_color(fg)
                                                .child(item.label.clone()),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .flex_shrink_0()
                                        .items_center()
                                        .gap(px(tokens::SPACE_3))
                                        .child(
                                            div()
                                                .font_family(cx.theme().mono_font_family.clone())
                                                .text_size(px(tokens::UI_SM))
                                                .text_color(green)
                                                .child(format!("+{}", item.added)),
                                        )
                                        .child(
                                            div()
                                                .id(("agent-reject-row", ix))
                                                .cursor_pointer()
                                                .text_color(red)
                                                .hover(|s| s.opacity(0.8))
                                                .child(Icon::new(IconName::X).with_size(px(11.)))
                                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                    cx.stop_propagation()
                                                })
                                                .on_click(cx.listener(move |this, _e, _w, cx| {
                                                    if ix < this.agent.pending.len() {
                                                        this.agent.pending.remove(ix);
                                                        cx.notify();
                                                    }
                                                })),
                                        )
                                        .child(
                                            div()
                                                .id(("agent-accept-row", ix))
                                                .cursor_pointer()
                                                .text_color(primary)
                                                .hover(|s| s.opacity(0.8))
                                                .child(
                                                    Icon::new(IconName::Check).with_size(px(11.)),
                                                )
                                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                    cx.stop_propagation()
                                                })
                                                .on_click(cx.listener(move |this, _e, _w, cx| {
                                                    if ix < this.agent.pending.len() {
                                                        this.agent.pending.remove(ix);
                                                        cx.notify();
                                                    }
                                                })),
                                        ),
                                )
                                .on_click(cx.listener(move |this, _e, window, cx| {
                                    let full = this.root.join(&rel_path);
                                    if full.exists() {
                                        this.open_path(full, window, cx);
                                    }
                                }))
                        })),
                )
            })
            // 30. Composer (`Message agent…`, Mode selector, Model selector, Send circle)
            .child(
                v_flex()
                    .w_auto()
                    .flex_shrink_0()
                    .mx(px(tokens::SPACE_4))
                    .mb(px(tokens::SPACE_4))
                    .p(px(tokens::SPACE_3))
                    .gap(px(tokens::SPACE_3))
                    .rounded(px(tokens::RADIUS_CARD))
                    .bg(cx.theme().input)
                    .border_1()
                    .border_color(border)
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key.as_str() == "enter"
                            && !event.keystroke.modifiers.shift
                        {
                            this.agent_send_message(window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        div()
                            .w_full()
                            .child(Input::new(&self.agent.input).appearance(false).small()),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(tokens::SPACE_4))
                                    .child(
                                        h_flex()
                                            .id("agent-mode-select")
                                            .items_center()
                                            .gap(px(tokens::SPACE_1))
                                            .cursor_pointer()
                                            .text_size(px(tokens::UI_SM))
                                            .child(div().text_color(muted).child("Mode:"))
                                            .child(
                                                div()
                                                    .font_medium()
                                                    .text_color(fg)
                                                    .child(mode_label),
                                            )
                                            .child(
                                                Icon::new(IconName::ChevronDown)
                                                    .with_size(px(10.))
                                                    .text_color(muted),
                                            )
                                            .on_click(cx.listener(|this, _e, _w, cx| {
                                                this.agent.mode_ix =
                                                    (this.agent.mode_ix + 1) % MODES.len();
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        h_flex()
                                            .id("agent-model-select")
                                            .items_center()
                                            .gap(px(tokens::SPACE_1))
                                            .cursor_pointer()
                                            .text_size(px(tokens::UI_SM))
                                            .child(div().text_color(muted).child("Model:"))
                                            .child(
                                                div()
                                                    .font_medium()
                                                    .text_color(fg)
                                                    .child(model_label),
                                            )
                                            .child(
                                                Icon::new(IconName::ChevronDown)
                                                    .with_size(px(10.))
                                                    .text_color(muted),
                                            )
                                            .on_click(cx.listener(|this, _e, _w, cx| {
                                                this.agent.model_ix =
                                                    (this.agent.model_ix + 1) % MODELS.len();
                                                cx.notify();
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .id("agent-send-btn")
                                    .size(px(28.))
                                    .rounded(px(tokens::RADIUS_PILL))
                                    .bg(primary.opacity(0.18))
                                    .border_1()
                                    .border_color(primary.opacity(0.55))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(primary.opacity(0.3)))
                                    .child(
                                        Icon::new(IconName::ArrowRight)
                                            .with_size(px(13.))
                                            .text_color(primary),
                                    )
                                    .on_click(cx.listener(|this, _e, window, cx| {
                                        this.agent_send_message(window, cx);
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}
