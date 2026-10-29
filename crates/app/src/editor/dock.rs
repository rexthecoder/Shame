use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_kit::*;

use crate::{DartIde, tokens};

impl DartIde {
    /// Bottom dock of the center column. Phase 1 draws the card and its
    /// header; Phase 5 supplies real scrollback.
    pub(crate) fn render_terminal(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;

        v_flex()
            .w_full()
            .h(px(tokens::TERMINAL_H))
            .flex_shrink_0()
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
                            .px(px(tokens::SPACE_3))
                            .h(px(30.))
                            .rounded(px(tokens::RADIUS_PANEL))
                            .bg(cx.theme().tab_active)
                            .border_1()
                            .border_color(tokens::accent_dim())
                            .text_size(px(tokens::UI))
                            .text_color(cx.theme().foreground)
                            .child(
                                div()
                                    .font_family("SF Mono")
                                    .text_size(px(tokens::UI_SM))
                                    .text_color(cx.theme().primary)
                                    .child(">_"),
                            )
                            .child("panes \u{2014} bash")
                            .child(
                                div()
                                    .id("close-terminal")
                                    .cursor_pointer()
                                    .text_color(muted)
                                    .hover(|s| s.text_color(cx.theme().foreground))
                                    .child(Icon::new(gpui_kit::assets::IconName::X).xsmall())
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.terminal_open = false;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        Button::new("new-terminal-tab")
                            .ghost()
                            .small()
                            .icon(Icon::new(IconName::Plus).text_color(muted))
                            .tooltip("New terminal tab"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .p(px(tokens::SPACE_5))
                    .text_size(px(tokens::MONO))
                    .text_color(muted)
                    .child("Terminal dock \u{2014} Phase 5"),
            )
    }
}

