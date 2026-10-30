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
        let has_sim_frame = self.simulator_frame.is_some();

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
                // Centered iPhone 16 Pro Hardware Frame (exact 19.5:9 aspect ratio,
                // Natural Titanium outer ring, physical Action/Volume/Side buttons,
                // uniform 7px edge-to-edge bezel, and Dynamic Island + Home Indicator
                // overlaid INSIDE the full-bleed display).
                v_flex()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .p(px(tokens::SPACE_3))
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .relative()
                            .w(px(278.))
                            .h(px(592.))
                            .flex_shrink_0()
                            // Left hardware buttons: Action Button + Volume Up + Volume Down
                            .child(
                                div()
                                    .absolute()
                                    .left(px(-2.5))
                                    .top(px(94.))
                                    .w(px(3.))
                                    .h(px(20.))
                                    .rounded_l(px(2.))
                                    .bg(rgb(0x4a4b52)),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .left(px(-2.5))
                                    .top(px(130.))
                                    .w(px(3.))
                                    .h(px(38.))
                                    .rounded_l(px(2.))
                                    .bg(rgb(0x4a4b52)),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .left(px(-2.5))
                                    .top(px(178.))
                                    .w(px(3.))
                                    .h(px(38.))
                                    .rounded_l(px(2.))
                                    .bg(rgb(0x4a4b52)),
                            )
                            // Right hardware button: Side / Power Button
                            .child(
                                div()
                                    .absolute()
                                    .right(px(-2.5))
                                    .top(px(146.))
                                    .w(px(3.))
                                    .h(px(56.))
                                    .rounded_r(px(2.))
                                    .bg(rgb(0x4a4b52)),
                            )
                            // Outer Natural Titanium band
                            .child(
                                div()
                                    .size_full()
                                    .p(px(2.5))
                                    .rounded(px(48.))
                                    .bg(rgb(0x38393f))
                                    .border_1()
                                    .border_color(rgba(0xffffff33))
                                    .shadow_xl()
                                    // Inner 7px uniform black glass bezel
                                    .child(
                                        div()
                                            .size_full()
                                            .p(px(7.))
                                            .rounded(px(45.5))
                                            .bg(rgb(0x050507))
                                            .border_1()
                                            .border_color(rgba(0x000000dd))
                                            // Full-bleed edge-to-edge iOS OLED Display
                                            .child(
                                                div()
                                                    .relative()
                                                    .size_full()
                                                    .rounded(px(38.))
                                                    .overflow_hidden()
                                                    .bg(rgb(0x000000))
                                                    // 1. Edge-to-edge current screen content + interactive tap forwarding
                                                    .child(
                                                        if let Some(frame) =
                                                            self.simulator_frame.clone()
                                                        {
                                                            let tap_view = cx.entity();
                                                            div()
                                                                .relative()
                                                                .size_full()
                                                                .rounded(px(38.))
                                                                .overflow_hidden()
                                                                .cursor_pointer()
                                                                .child(
                                                                    img(frame)
                                                                        .size_full()
                                                                        .rounded(px(38.))
                                                                        .object_fit(ObjectFit::Fill),
                                                                )
                                                                .child(
                                                                    canvas(
                                                                        move |bounds, _window, _cx| bounds,
                                                                        move |bounds, _, window, _cx| {
                                                                            // Normalized position inside the screen, clamped to it.
                                                                            let norm = move |pos: Point<Pixels>| -> Option<(f32, f32)> {
                                                                                let w: f32 = bounds.size.width.into();
                                                                                let h: f32 = bounds.size.height.into();
                                                                                if w <= 1.0 || h <= 1.0 {
                                                                                    return None;
                                                                                }
                                                                                let rx: f32 = (pos.x - bounds.origin.x).into();
                                                                                let ry: f32 = (pos.y - bounds.origin.y).into();
                                                                                Some(((rx / w).clamp(0.0, 1.0), (ry / h).clamp(0.0, 1.0)))
                                                                            };
                                                                            let view = tap_view.clone();
                                                                            window.on_mouse_event(
                                                                                move |event: &MouseDownEvent, phase, _window, cx| {
                                                                                    if phase != DispatchPhase::Bubble
                                                                                        || event.button != MouseButton::Left
                                                                                        || !bounds.contains(&event.position)
                                                                                    {
                                                                                        return;
                                                                                    }
                                                                                    if let Some((nx, ny)) = norm(event.position) {
                                                                                        view.update(cx, |this, cx| {
                                                                                            this.simulator_touch(core_flutter::TouchPhase::Down, nx, ny, cx);
                                                                                        });
                                                                                    }
                                                                                },
                                                                            );
                                                                            let view = tap_view.clone();
                                                                            window.on_mouse_event(
                                                                                move |event: &MouseMoveEvent, phase, _window, cx| {
                                                                                    if phase != DispatchPhase::Bubble
                                                                                        || event.pressed_button != Some(MouseButton::Left)
                                                                                    {
                                                                                        return;
                                                                                    }
                                                                                    if let Some((nx, ny)) = norm(event.position) {
                                                                                        view.update(cx, |this, cx| {
                                                                                            this.simulator_touch(core_flutter::TouchPhase::Move, nx, ny, cx);
                                                                                        });
                                                                                    }
                                                                                },
                                                                            );
                                                                            let view = tap_view.clone();
                                                                            window.on_mouse_event(
                                                                                move |event: &MouseUpEvent, phase, _window, cx| {
                                                                                    if phase != DispatchPhase::Bubble
                                                                                        || event.button != MouseButton::Left
                                                                                    {
                                                                                        return;
                                                                                    }
                                                                                    if let Some((nx, ny)) = norm(event.position) {
                                                                                        view.update(cx, |this, cx| {
                                                                                            this.simulator_touch(core_flutter::TouchPhase::Up, nx, ny, cx);
                                                                                        });
                                                                                    }
                                                                                },
                                                                            );
                                                                        },
                                                                    )
                                                                    .absolute()
                                                                    .top_0()
                                                                    .left_0()
                                                                    .size_full(),
                                                                )
                                                                .into_any_element()
                                                        } else {
                                                            v_flex()
                                                                .size_full()
                                                                .pt(px(48.))
                                                                .pb(px(28.))
                                                                .px_4()
                                                                .gap_2p5()
                                                                .items_center()
                                                                .justify_center()
                                                                .bg(rgb(0x0c0d12))
                                                                .child(
                                                                    div()
                                                                        .w(px(48.))
                                                                        .h(px(48.))
                                                                        .rounded(px(14.))
                                                                        .bg(rgba(0xea7a3c18))
                                                                        .border_1()
                                                                        .border_color(rgba(
                                                                            0xea7a3c44,
                                                                        ))
                                                                        .flex()
                                                                        .items_center()
                                                                        .justify_center()
                                                                        .child(
                                                                            Icon::new(
                                                                                IconName::Smartphone,
                                                                            )
                                                                            .with_size(px(22.))
                                                                            .text_color(primary),
                                                                        ),
                                                                )
                                                                .child(
                                                                    div()
                                                                        .text_size(px(tokens::UI))
                                                                        .font_medium()
                                                                        .text_color(fg)
                                                                        .child("iOS Simulator"),
                                                                )
                                                                .child(
                                                                    div()
                                                                        .text_center()
                                                                        .text_size(px(
                                                                            tokens::UI_XS,
                                                                        ))
                                                                        .text_color(muted)
                                                                        .child(
                                                                            self.simulator_status.clone().unwrap_or_else(|| {
                                                                                "Tap an iPhone below to boot & stream inside the bezel."
                                                                                    .to_string()
                                                                            }),
                                                                        ),
                                                                )
                                                                .child(
                                                                    v_flex()
                                                                        .w_full()
                                                                        .gap_1p5()
                                                                        .mt_2()
                                                                        .children(
                                                                            self.ios_simulators
                                                                                .iter()
                                                                                .take(5)
                                                                                .enumerate()
                                                                                .map(|(ix, sim)| {
                                                                                    let sim_clone =
                                                                                        sim.clone();
                                                                                    let booted = sim
                                                                                        .is_booted();
                                                                                    h_flex()
                                                                                        .id((
                                                                                            "bezel-sim-boot",
                                                                                            ix,
                                                                                        ))
                                                                                        .w_full()
                                                                                        .h(px(30.))
                                                                                        .px_3()
                                                                                        .items_center()
                                                                                        .justify_between()
                                                                                        .rounded(px(10.))
                                                                                        .bg(rgba(0xffffff08))
                                                                                        .border_1()
                                                                                        .border_color(rgba(0xffffff12))
                                                                                        .cursor_pointer()
                                                                                        .hover(move |s| {
                                                                                            s.bg(rgba(0xea7a3c22))
                                                                                                .border_color(rgba(0xea7a3c55))
                                                                                        })
                                                                                        .on_click(cx.listener(
                                                                                            move |this, _event, window, cx| {
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
                                                                                }),
                                                                        ),
                                                                )
                                                                .into_any_element()
                                                        },
                                                    )
                                                    // 2. Overlaid iOS Status Bar + Dynamic Island hardware cutout at top of display
                                                    .child(
                                                        h_flex()
                                                            .absolute()
                                                            .top(px(8.))
                                                            .left_0()
                                                            .right_0()
                                                            .h(px(22.))
                                                            .px(px(20.))
                                                            .items_center()
                                                            .justify_between()
                                                            // Left ear: iOS time ("9:41" when standby, empty spacer when screenshot provides it)
                                                            .child(
                                                                div()
                                                                    .w(px(44.))
                                                                    .text_size(px(11.))
                                                                    .font_semibold()
                                                                    .text_color(rgb(0xffffff))
                                                                    .when(
                                                                        self.simulator_frame
                                                                            .is_none(),
                                                                        |el| el.child("9:41"),
                                                                    ),
                                                            )
                                                            // Center: Dynamic Island pill with TrueDepth camera lens
                                                            .child(
                                                                h_flex()
                                                                    .w(px(80.))
                                                                    .h(px(21.))
                                                                    .px(px(7.))
                                                                    .rounded(px(11.))
                                                                    .bg(rgb(0x000000))
                                                                    .border_1()
                                                                    .border_color(rgba(0xffffff14))
                                                                    .items_center()
                                                                    .justify_end()
                                                                    .child(
                                                                        div()
                                                                            .w(px(8.))
                                                                            .h(px(8.))
                                                                            .rounded(px(4.))
                                                                            .bg(rgb(0x0b1021))
                                                                            .border_1()
                                                                            .border_color(rgba(
                                                                                0x4a6fa544,
                                                                            )),
                                                                    ),
                                                            )
                                                            // Right ear: iOS cellular/wifi/battery capsule in standby
                                                            .child(
                                                                h_flex()
                                                                    .w(px(44.))
                                                                    .items_center()
                                                                    .justify_end()
                                                                    .gap(px(4.))
                                                                    .when(
                                                                        self.simulator_frame
                                                                            .is_none(),
                                                                        |el| {
                                                                            el.child(
                                                                                div()
                                                                                    .text_size(px(
                                                                                        9.5,
                                                                                    ))
                                                                                    .font_semibold()
                                                                                    .text_color(rgb(
                                                                                        0xffffff,
                                                                                    ))
                                                                                    .child("5G"),
                                                                            )
                                                                            .child(
                                                                                div()
                                                                                    .w(px(16.))
                                                                                    .h(px(8.5))
                                                                                    .rounded(px(2.5))
                                                                                    .border_1()
                                                                                    .border_color(rgba(
                                                                                        0xffffffaa,
                                                                                    ))
                                                                                    .p(px(1.))
                                                                                    .child(
                                                                                        div()
                                                                                            .size_full()
                                                                                            .rounded(px(1.))
                                                                                            .bg(rgb(
                                                                                                0xffffff,
                                                                                            )),
                                                                                    ),
                                                                            )
                                                                        },
                                                                    ),
                                                            ),
                                                    )
                                                    // 3. Overlaid iOS Home Indicator bar floating at the bottom of the screen
                                                    .when(
                                                        self.simulator_frame.is_none(),
                                                        |el| {
                                                            el.child(
                                                                h_flex()
                                                                    .absolute()
                                                                    .bottom(px(6.))
                                                                    .left_0()
                                                                    .right_0()
                                                                    .justify_center()
                                                                    .child(
                                                                        div()
                                                                            .w(px(96.))
                                                                            .h(px(4.))
                                                                            .rounded(px(2.))
                                                                            .bg(rgba(0xffffffcc)),
                                                                    ),
                                                            )
                                                        },
                                                    ),
                                            ),
                                    ),
                            ),
                    )
                    // Bottom iOS Hardware Control Pill (Home, Hot Reload, Hot Restart, Dock Native Window)
                    .child(
                        h_flex()
                            .mt_2()
                            .h(px(26.))
                            .px(px(tokens::SPACE_2))
                            .gap_1()
                            .items_center()
                            .rounded(px(tokens::RADIUS_PILL))
                            .bg(cx.theme().tab_bar)
                            .border_1()
                            .border_color(cx.theme().border)
                            .child(
                                h_flex()
                                    .id("sim-hw-home")
                                    .h(px(20.))
                                    .px_2()
                                    .items_center()
                                    .rounded(px(tokens::RADIUS_PILL))
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(list_hover).text_color(fg))
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(muted)
                                    .child("\u{2302} Home")
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.press_simulator_home(cx);
                                    })),
                            )
                            .child(div().w(px(1.)).h(px(12.)).bg(cx.theme().border))
                            .child(
                                h_flex()
                                    .id("sim-hw-reload")
                                    .h(px(20.))
                                    .px_2()
                                    .gap_1()
                                    .items_center()
                                    .rounded(px(tokens::RADIUS_PILL))
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(list_hover).text_color(fg))
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(warning)
                                    .child(Icon::new(IconName::Zap).with_size(px(10.5)))
                                    .child("Reload")
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.trigger_hot_reload(cx);
                                        this.refresh_simulator_screenshot(cx);
                                    })),
                            )
                            .child(
                                h_flex()
                                    .id("sim-hw-restart")
                                    .h(px(20.))
                                    .px_2()
                                    .gap_1()
                                    .items_center()
                                    .rounded(px(tokens::RADIUS_PILL))
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(list_hover).text_color(fg))
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(info)
                                    .child(Icon::new(IconName::RotateCw).with_size(px(10.5)))
                                    .child("Restart")
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.trigger_hot_restart(cx);
                                        this.refresh_simulator_screenshot(cx);
                                    })),
                            ),
                    ),
            )
    }
}
