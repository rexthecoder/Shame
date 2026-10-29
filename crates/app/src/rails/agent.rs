use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_kit::*;

use crate::{DartIde, tokens};

impl DartIde {
    /// Right rail. Phase 1 draws the card and its tab header; Phase 6 fills
    /// the conversation body.
    pub(crate) fn render_agent_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
                            .text_size(px(tokens::UI))
                            .text_color(cx.theme().foreground)
                            .child(
                                Icon::new(gpui_kit::assets::IconName::Bot)
                                    .small()
                                    .text_color(cx.theme().primary),
                            )
                            .child("Demo Agent")
                            .child(
                                div()
                                    .id("close-agent")
                                    .cursor_pointer()
                                    .text_color(muted)
                                    .hover(|s| s.text_color(cx.theme().foreground))
                                    .child(Icon::new(gpui_kit::assets::IconName::X).xsmall())
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.agent_open = false;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        Button::new("new-agent-tab")
                            .ghost()
                            .small()
                            .icon(Icon::new(IconName::Plus).text_color(muted))
                            .tooltip("New agent thread"),
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
                    .text_color(muted)
                    .child("Agent panel \u{2014} Phase 6"),
            )
    }
}

