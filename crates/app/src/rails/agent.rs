use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, RightRailTab, tokens};

impl DartIde {
    /// Right rail: houses the Attached Simulator (`RightRailTab::Simulator`)
    /// and the AI Agent panel (`RightRailTab::Agent`).
    pub(crate) fn render_agent_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let green = cx.theme().green;
        let primary = cx.theme().primary;
        let list_hover = cx.theme().list_hover;
        let is_sim_tab = self.right_rail_tab == RightRailTab::Simulator;
        let has_sim_frame = self.sim.frame.is_some();

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
                    .justify_between()
                    .px(px(tokens::SPACE_3))
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .child(
                                h_flex()
                                    .id("rail-tab-simulator")
                                    .items_center()
                                    .gap(px(tokens::SPACE_2))
                                    .h(px(30.))
                                    .px(px(tokens::SPACE_3))
                                    .rounded(px(tokens::RADIUS_PANEL))
                                    .border_1()
                                    .border_color(if is_sim_tab {
                                        tokens::accent_dim()
                                    } else {
                                        gpui_kit::transparent_black()
                                    })
                                    .when(is_sim_tab, |s| s.bg(cx.theme().tab_active))
                                    .when(!is_sim_tab, |s| {
                                        s.hover(move |h| h.bg(list_hover).text_color(fg))
                                    })
                                    .cursor_pointer()
                                    .text_size(px(tokens::UI))
                                    .text_color(if is_sim_tab { fg } else { muted })
                                    .child(
                                        Icon::new(IconName::Smartphone)
                                            .small()
                                            .text_color(if is_sim_tab { primary } else { muted }),
                                    )
                                    .child("Simulator")
                                    .when(has_sim_frame, |el| {
                                        el.child(
                                            div()
                                                .w(px(6.))
                                                .h(px(6.))
                                                .rounded(px(3.))
                                                .bg(green),
                                        )
                                    })
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.right_rail_tab = RightRailTab::Simulator;
                                        this.refresh_simulator_screenshot(cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                h_flex()
                                    .id("rail-tab-agent")
                                    .items_center()
                                    .gap(px(tokens::SPACE_2))
                                    .h(px(30.))
                                    .px(px(tokens::SPACE_3))
                                    .rounded(px(tokens::RADIUS_PANEL))
                                    .border_1()
                                    .border_color(if !is_sim_tab {
                                        tokens::accent_dim()
                                    } else {
                                        gpui_kit::transparent_black()
                                    })
                                    .when(!is_sim_tab, |s| s.bg(cx.theme().tab_active))
                                    .when(is_sim_tab, |s| {
                                        s.hover(move |h| h.bg(list_hover).text_color(fg))
                                    })
                                    .cursor_pointer()
                                    .text_size(px(tokens::UI))
                                    .text_color(if !is_sim_tab { fg } else { muted })
                                    .child(
                                        Icon::new(IconName::Bot)
                                            .small()
                                            .text_color(if !is_sim_tab { primary } else { muted }),
                                    )
                                    .child("Agent")
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.right_rail_tab = RightRailTab::Agent;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        Button::new("close-right-rail")
                            .ghost()
                            .small()
                            .icon(Icon::new(IconName::X).xsmall().text_color(muted))
                            .tooltip("Close panel")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.agent_open = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(if is_sim_tab {
                self.render_simulator_panel(cx).into_any_element()
            } else {
                div()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(tokens::UI_SM))
                    .text_color(muted)
                    .child("Agent panel \u{2014} Phase 6")
                    .into_any_element()
            })
            .into_any_element()
    }

}
