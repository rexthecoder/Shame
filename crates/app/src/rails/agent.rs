use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
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
        let has_sim_frame = self.simulator_frame_path.is_some();

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
                self.render_attached_simulator_body(cx).into_any_element()
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

    fn render_attached_simulator_body(&self, cx: &mut Context<Self>) -> Div {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let green = cx.theme().green;
        let info = cx.theme().info;
        let primary = cx.theme().primary;
        let warning = cx.theme().warning;
        let list_hover = cx.theme().list_hover;

        let active_sim_label = self
            .selected_device
            .as_ref()
            .filter(|d| d.target_platform.contains("ios") || d.emulator)
            .map(|d| d.name.clone())
            .or_else(|| {
                self.ios_simulators
                    .iter()
                    .find(|s| s.is_booted())
                    .map(|s| s.name.clone())
            })
            .unwrap_or_else(|| "iOS Simulator".to_string());

        let live_sync = self.simulator_live_sync;

        v_flex()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .child(
                // Simulator control strip
                h_flex()
                    .w_full()
                    .h(px(34.))
                    .px(px(tokens::SPACE_3))
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().tab_bar)
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(tokens::UI_SM))
                                    .font_medium()
                                    .text_color(fg)
                                    .child(active_sim_label),
                            )
                            .child(
                                h_flex()
                                    .id("sim-live-toggle")
                                    .h(px(20.))
                                    .px_2()
                                    .gap_1()
                                    .items_center()
                                    .rounded(px(tokens::RADIUS_PILL))
                                    .border_1()
                                    .border_color(if live_sync {
                                        green.opacity(0.45)
                                    } else {
                                        cx.theme().border
                                    })
                                    .bg(if live_sync {
                                        green.opacity(0.14)
                                    } else {
                                        rgba(0xffffff05).into()
                                    })
                                    .cursor_pointer()
                                    .font_family("SF Mono")
                                    .text_size(px(9.5))
                                    .text_color(if live_sync { green } else { muted })
                                    .child(if live_sync { "\u{25cf} LIVE" } else { "\u{25cb} PAUSED" })
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.simulator_live_sync = !this.simulator_live_sync;
                                        if this.simulator_live_sync {
                                            this.refresh_simulator_screenshot(cx);
                                        }
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap_1()
                            .child(
                                h_flex()
                                    .id("sim-snap-window")
                                    .h(px(22.))
                                    .px_2()
                                    .gap_1()
                                    .items_center()
                                    .rounded(px(tokens::RADIUS_SM))
                                    .bg(cx.theme().background)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(list_hover).text_color(fg))
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(info)
                                    .child(Icon::new(IconName::Columns2).with_size(px(11.)))
                                    .child("Dock App")
                                    .on_click(cx.listener(|this, _event, window, cx| {
                                        this.attach_simulator(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("sim-hot-reload")
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(IconName::Zap).with_size(px(12.)).text_color(warning))
                                    .tooltip("Hot Reload (\u{2318}S)")
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.trigger_hot_reload(cx);
                                    })),
                            )
                            .child(
                                Button::new("sim-refresh-shot")
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(IconName::RotateCw).with_size(px(12.)).text_color(muted))
                                    .tooltip("Capture simulator frame")
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.refresh_simulator_screenshot(cx);
                                    })),
                            ),
                    ),
            )
            .child(
                // Centered iPhone Simulator Hardware Bezel
                v_flex()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .p(px(tokens::SPACE_4))
                    .items_center()
                    .justify_center()
                    .child(
                        v_flex()
                            .w(px(284.))
                            .h_full()
                            .max_h(px(610.))
                            .p(px(8.))
                            .gap(px(6.))
                            .items_center()
                            .rounded(px(34.))
                            .bg(rgb(0x08090c))
                            .border_2()
                            .border_color(rgba(0xffffff22))
                            .shadow_xl()
                            .child(
                                // Dynamic Island capsule
                                div()
                                    .w(px(74.))
                                    .h(px(14.))
                                    .flex_shrink_0()
                                    .rounded(px(7.))
                                    .bg(rgb(0x000000))
                                    .border_1()
                                    .border_color(rgba(0xffffff12)),
                            )
                            .child(if let Some(frame_path) = self.simulator_frame_path.clone() {
                                div()
                                    .flex_1()
                                    .w_full()
                                    .min_h(px(0.))
                                    .rounded(px(24.))
                                    .overflow_hidden()
                                    .bg(rgb(0x000000))
                                    .child(
                                        img(frame_path)
                                            .size_full()
                                            .object_fit(ObjectFit::Contain),
                                    )
                                    .into_any_element()
                            } else {
                                v_flex()
                                    .flex_1()
                                    .w_full()
                                    .min_h(px(0.))
                                    .px_4()
                                    .py_5()
                                    .gap_3()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(24.))
                                    .bg(cx.theme().background)
                                    .child(
                                        Icon::new(IconName::Smartphone)
                                            .with_size(px(28.))
                                            .text_color(primary),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(tokens::UI))
                                            .font_medium()
                                            .text_color(fg)
                                            .child("Simulator Attached"),
                                    )
                                    .child(
                                        div()
                                            .text_center()
                                            .text_size(px(tokens::UI_XS))
                                            .text_color(muted)
                                            .child(
                                                "Select an iOS Simulator below to boot, dock alongside the editor, and stream live frames.",
                                            ),
                                    )
                                    .child(
                                        v_flex()
                                            .w_full()
                                            .gap_1()
                                            .mt_2()
                                            .children(
                                                self.ios_simulators.iter().take(5).enumerate().map(
                                                    |(ix, sim)| {
                                                        let sim_clone = sim.clone();
                                                        let booted = sim.is_booted();
                                                        h_flex()
                                                            .id(("bezel-sim-boot", ix))
                                                            .w_full()
                                                            .h(px(28.))
                                                            .px_3()
                                                            .items_center()
                                                            .justify_between()
                                                            .rounded(px(tokens::RADIUS_SM))
                                                            .bg(cx.theme().sidebar)
                                                            .border_1()
                                                            .border_color(cx.theme().border)
                                                            .cursor_pointer()
                                                            .hover(move |s| {
                                                                s.bg(rgba(0xea7a3c1a))
                                                                    .border_color(rgba(0xea7a3c44))
                                                            })
                                                            .on_click(cx.listener(
                                                                move |this, _event, window, cx| {
                                                                    let _ = core_flutter::boot_ios_simulator(
                                                                        &sim_clone.udid,
                                                                    );
                                                                    this.selected_device =
                                                                        Some(core_flutter::Device {
                                                                            id: sim_clone.udid.clone(),
                                                                            name: sim_clone.name.clone(),
                                                                            target_platform: "ios"
                                                                                .to_string(),
                                                                            emulator: true,
                                                                            is_supported: true,
                                                                        });
                                                                    this.attach_simulator(window, cx);
                                                                    this.refresh_devices(cx);
                                                                },
                                                            ))
                                                            .child(
                                                                div()
                                                                    .truncate()
                                                                    .text_size(px(tokens::UI_SM))
                                                                    .text_color(fg)
                                                                    .child(sim.name.clone()),
                                                            )
                                                            .child(
                                                                div()
                                                                    .font_family("SF Mono")
                                                                    .text_size(px(9.5))
                                                                    .text_color(if booted {
                                                                        green
                                                                    } else {
                                                                        info
                                                                    })
                                                                    .child(if booted {
                                                                        "Booted"
                                                                    } else {
                                                                        "Boot"
                                                                    }),
                                                            )
                                                    },
                                                ),
                                            ),
                                    )
                                    .into_any_element()
                            })
                            .child(
                                // iOS Home Indicator bar
                                div()
                                    .w(px(92.))
                                    .h(px(4.))
                                    .flex_shrink_0()
                                    .rounded(px(2.))
                                    .bg(rgba(0xffffff38)),
                            ),
                    ),
            )
    }
}
