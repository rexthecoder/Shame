use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, tokens};

fn icon_button(
    id: &'static str,
    icon: gpui_kit::assets::IconName,
    color: Option<Hsla>,
    tooltip: &'static str,
) -> Button {
    let mut ic = Icon::new(icon);
    if let Some(c) = color {
        ic = ic.text_color(c);
    }
    Button::new(id).ghost().small().icon(ic).tooltip(tooltip)
}

impl DartIde {
    pub(crate) fn render_titlebar(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let traffic_light_inset = if window.is_fullscreen() { px(12.) } else { px(78.) };

        let root_name = self
            .root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("/")
            .to_string();

        let active_file_name = self
            .active_editor()
            .and_then(|ed| ed.read(cx).buffer().path())
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .map(str::to_string);

        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let green = cx.theme().green;
        let info = cx.theme().info;
        let has_changes = !self.git_status.is_empty();

        h_flex()
            .w_full()
            .h(px(tokens::TITLEBAR_H))
            .flex_shrink_0()
            .pl(traffic_light_inset)
            .pr_2()
            .items_center()
            .bg(cx.theme().title_bar)
            .border_b_1()
            .border_color(cx.theme().title_bar_border)
            .child(
                h_flex()
                    .flex_1()
                    .items_center()
                    .gap_1()
                    .child(icon_button(
                        "menu",
                        gpui_kit::assets::IconName::Menu,
                        Some(muted),
                        "Application menu",
                    ))
                    .child(
                        Button::new("layout-sidebar")
                            .ghost()
                            .small()
                            .icon(
                                Icon::new(if self.sidebar_open {
                                    gpui_kit::assets::IconName::PanelLeftClose
                                } else {
                                    gpui_kit::assets::IconName::PanelLeftOpen
                                })
                                .text_color(if self.sidebar_open { fg } else { muted }),
                            )
                            .tooltip("Toggle sidebar (cmd/ctrl+B)")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.sidebar_open = !this.sidebar_open;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("layout-panel")
                            .ghost()
                            .small()
                            .icon(
                                Icon::new(gpui_kit::assets::IconName::PanelBottom)
                                    .text_color(if self.terminal_open { fg } else { muted }),
                            )
                            .tooltip("Toggle terminal dock (cmd/ctrl+J)")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.terminal_open = !this.terminal_open;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("layout-columns")
                            .ghost()
                            .small()
                            .icon(
                                Icon::new(gpui_kit::assets::IconName::Columns2)
                                    .text_color(if self.agent_open { fg } else { muted }),
                            )
                            .tooltip("Toggle agent rail")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.agent_open = !this.agent_open;
                                cx.notify();
                            })),
                    )
                    .child(icon_button(
                        "add-person",
                        gpui_kit::assets::IconName::UserPlus,
                        Some(muted),
                        "Invite collaborator",
                    )),
            )
            .child(
                h_flex()
                    .id("titlebar-project-pill")
                    .flex_shrink_0()
                    .h(px(26.))
                    .px(px(tokens::SPACE_4))
                    .rounded(px(tokens::RADIUS_PILL))
                    .bg(rgba(0xffffff05))
                    .border_1()
                    .border_color(cx.theme().border.opacity(0.6))
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .hover(|s| s.bg(rgba(0xffffff0c)))
                    .child(
                        div()
                            .text_size(px(tokens::UI))
                            .font_medium()
                            .text_color(fg)
                            .child(root_name),
                    )
                    .when_some(self.git_branch.clone(), |el, branch| {
                        let ahead = core_workspace::ahead_behind(&self.root)
                            .map(|(ahead, _)| ahead)
                            .unwrap_or(0);
                        el.child(
                            h_flex()
                                .items_center()
                                .gap_1()
                                .text_size(px(tokens::UI_SM))
                                .child(
                                    Icon::new(gpui_kit::assets::IconName::GitBranch)
                                        .small()
                                        .text_color(green),
                                )
                                .child(div().text_color(fg).child(branch))
                                .when(has_changes, |el| {
                                    el.child(
                                        div()
                                            .text_size(px(8.))
                                            .text_color(cx.theme().primary)
                                            .child("\u{25cf}"),
                                    )
                                })
                                .when(ahead > 0, |el| {
                                    el.child(
                                        div()
                                            .text_color(info)
                                            .child(format!("\u{2191}{ahead}")),
                                    )
                                }),
                        )
                    })
                    .on_click(cx.listener(|this, _event, window, cx| {
                        this.palette_open = true;
                        this.palette_selected = 0;
                        this.palette_input
                            .update(cx, |state, cx| state.focus(window, cx));
                        cx.notify();
                    })),
            )
            .child(
                h_flex()
                    .flex_1()
                    .justify_end()
                    .items_center()
                    .gap_1p5()
                    .when_some(self.flutter_status.clone(), |el, status| {
                        let is_running = status == "Running";
                        let dot_color = if is_running {
                            green
                        } else {
                            cx.theme().warning
                        };
                        el.child(
                            h_flex()
                                .h(px(24.))
                                .px(px(tokens::SPACE_3))
                                .gap(px(tokens::SPACE_2))
                                .items_center()
                                .rounded(px(tokens::RADIUS_PILL))
                                .bg(dot_color.opacity(0.12))
                                .border_1()
                                .border_color(dot_color.opacity(0.32))
                                .font_family("SF Mono")
                                .text_size(px(10.5))
                                .text_color(fg)
                                .child(
                                    div()
                                        .w(px(6.))
                                        .h(px(6.))
                                        .rounded(px(3.))
                                        .bg(dot_color),
                                )
                                .child(
                                    div()
                                        .max_w(px(160.))
                                        .truncate()
                                        .child(status),
                                ),
                        )
                    })
                    .child({
                        let active_cfg = self
                            .selected_launch_config
                            .and_then(|ix| self.launch_configs.get(ix));
                        let cfg_name = active_cfg
                            .map(|c| c.name.clone())
                            .unwrap_or_else(|| "launch.json".to_string());
                        let flavor = active_cfg.and_then(|c| c.flavor());
                        let is_open = self.launch_picker_open;
                        let has_configs = !self.launch_configs.is_empty()
                            || !self.test_launch_templates.is_empty();

                        h_flex()
                            .id("titlebar-launch-picker")
                            .h(px(24.))
                            .px(px(tokens::SPACE_3))
                            .items_center()
                            .gap(px(tokens::SPACE_1))
                            .rounded(px(tokens::RADIUS_PILL))
                            .border_1()
                            .border_color(if is_open {
                                rgba(0xea7a3c55).into()
                            } else {
                                cx.theme().border
                            })
                            .bg(if is_open {
                                rgba(0xea7a3c1a)
                            } else {
                                rgba(0xffffff05)
                            })
                            .cursor_pointer()
                            .hover(|s| s.bg(rgba(0xffffff0f)))
                            .text_size(px(tokens::UI_SM))
                            .text_color(if has_configs { fg } else { muted })
                            .child(
                                Icon::new(gpui_kit::assets::IconName::Play)
                                    .with_size(px(10.))
                                    .text_color(if has_configs {
                                        cx.theme().primary
                                    } else {
                                        muted
                                    }),
                            )
                            .child(
                                div()
                                    .max_w(px(140.))
                                    .truncate()
                                    .child(cfg_name),
                            )
                            .when_some(flavor, |el, flv| {
                                el.child(
                                    div()
                                        .px_1()
                                        .rounded(px(3.))
                                        .bg(info.opacity(0.16))
                                        .font_family("SF Mono")
                                        .text_size(px(9.5))
                                        .text_color(info)
                                        .child(flv),
                                )
                            })
                            .child(
                                Icon::new(gpui_kit::assets::IconName::ChevronDown)
                                    .with_size(px(10.))
                                    .text_color(muted),
                            )
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.launch_picker_open = !this.launch_picker_open;
                                this.device_picker_open = false;
                                if this.launch_picker_open {
                                    this.reload_launch_configs(cx);
                                }
                                cx.notify();
                            }))
                    })
                    .child({
                        let dev_name = self
                            .selected_device
                            .as_ref()
                            .map(|d| d.name.clone())
                            .unwrap_or_else(|| "macOS".to_string());
                        let dev_icon = self
                            .selected_device
                            .as_ref()
                            .map(|d| {
                                if d.target_platform.contains("ios")
                                    || d.target_platform.contains("android")
                                    || d.emulator
                                {
                                    gpui_kit::assets::IconName::Smartphone
                                } else if d.target_platform.contains("web") || d.id == "chrome" {
                                    gpui_kit::assets::IconName::Globe
                                } else {
                                    gpui_kit::assets::IconName::Monitor
                                }
                            })
                            .unwrap_or(gpui_kit::assets::IconName::Monitor);
                        let is_open = self.device_picker_open;

                        h_flex()
                            .id("titlebar-device-picker")
                            .h(px(24.))
                            .px(px(tokens::SPACE_3))
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .rounded(px(tokens::RADIUS_PILL))
                            .border_1()
                            .border_color(if is_open {
                                rgba(0xea7a3c55).into()
                            } else {
                                cx.theme().border
                            })
                            .bg(if is_open {
                                rgba(0xea7a3c1a)
                            } else {
                                rgba(0xffffff05)
                            })
                            .cursor_pointer()
                            .hover(|s| s.bg(rgba(0xffffff0f)))
                            .text_size(px(tokens::UI_SM))
                            .text_color(fg)
                            .child(Icon::new(dev_icon).xsmall().text_color(green))
                            .child(dev_name)
                            .child(
                                Icon::new(gpui_kit::assets::IconName::ChevronDown)
                                    .with_size(px(10.))
                                    .text_color(muted),
                            )
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.device_picker_open = !this.device_picker_open;
                                if this.device_picker_open {
                                    this.refresh_devices(cx);
                                }
                                cx.notify();
                            }))
                    })
                    .child({
                        let sim_active = self.agent_open
                            && self.right_rail_tab == crate::RightRailTab::Simulator;
                        h_flex()
                            .id("titlebar-sim-attach")
                            .h(px(24.))
                            .px(px(tokens::SPACE_2))
                            .gap(px(4.))
                            .items_center()
                            .rounded(px(tokens::RADIUS_PILL))
                            .border_1()
                            .border_color(if sim_active {
                                rgba(0xea7a3c55).into()
                            } else {
                                cx.theme().border
                            })
                            .bg(if sim_active {
                                rgba(0xea7a3c1c)
                            } else {
                                rgba(0xffffff05)
                            })
                            .cursor_pointer()
                            .hover(|s| s.bg(rgba(0xffffff0f)))
                            .text_size(px(tokens::UI_XS))
                            .text_color(if sim_active { fg } else { muted })
                            .child(
                                Icon::new(gpui_kit::assets::IconName::Smartphone)
                                    .with_size(px(11.))
                                    .text_color(if sim_active {
                                        cx.theme().primary
                                    } else {
                                        muted
                                    }),
                            )
                            .child("Simulator")
                            .on_click(cx.listener(|this, _event, window, cx| {
                                if this.agent_open
                                    && this.right_rail_tab == crate::RightRailTab::Simulator
                                {
                                    this.refresh_simulator_screenshot(cx);
                                } else {
                                    this.attach_simulator(window, cx);
                                }
                            }))
                    })
                    .when_some(active_file_name, |el, file| {
                        el.child(
                            div()
                                .h(px(24.))
                                .px(px(tokens::SPACE_3))
                                .flex()
                                .items_center()
                                .gap(px(tokens::SPACE_2))
                                .rounded(px(tokens::RADIUS_PILL))
                                .bg(cx.theme().secondary)
                                .border_1()
                                .border_color(cx.theme().border)
                                .text_size(px(tokens::UI_SM))
                                .text_color(fg)
                                .child(
                                    Icon::new(gpui_kit::assets::IconName::Feather)
                                        .xsmall()
                                        .text_color(info),
                                )
                                .child(file),
                        )
                    })
                    .child(
                        h_flex()
                            .h(px(28.))
                            .px(px(tokens::SPACE_1))
                            .items_center()
                            .gap(px(tokens::SPACE_1))
                            .rounded(px(tokens::RADIUS_PANEL))
                            .bg(rgba(0xffffff05))
                            .border_1()
                            .border_color(cx.theme().border.opacity(0.6))
                            .child(
                                icon_button(
                                    "run",
                                    gpui_kit::assets::IconName::Play,
                                    Some(green),
                                    "Run Flutter project on selected device",
                                )
                                .on_click(cx.listener(|this, _event, window, cx| {
                                    this.start_flutter_run(window, cx);
                                })),
                            )
                            .child(
                                icon_button(
                                    "hot-restart",
                                    gpui_kit::assets::IconName::RotateCw,
                                    Some(if self.flutter_app_id.is_some() {
                                        info
                                    } else {
                                        muted
                                    }),
                                    "Hot restart Flutter app",
                                )
                                .on_click(cx.listener(|this, _event, _window, cx| {
                                    this.trigger_hot_restart(cx);
                                })),
                            )
                            .child(div().w(px(1.)).h(px(12.)).bg(cx.theme().border))
                            .child(
                                icon_button(
                                    "fast",
                                    gpui_kit::assets::IconName::Zap,
                                    Some(if self.flutter_app_id.is_some() {
                                        cx.theme().warning
                                    } else {
                                        green
                                    }),
                                    "Hot reload (also auto-triggers on Save \u{2318}S)",
                                )
                                .on_click(cx.listener(|this, _event, _window, cx| {
                                    this.trigger_hot_reload(cx);
                                })),
                            )
                            .when(self.flutter_daemon.is_some(), |el| {
                                el.child(
                                    icon_button(
                                        "stop-flutter",
                                        gpui_kit::assets::IconName::X,
                                        Some(cx.theme().danger),
                                        "Stop running Flutter app",
                                    )
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.stop_flutter_run(cx);
                                    })),
                                )
                            }),
                    )
                    .child(icon_button(
                        "agent-wand",
                        gpui_kit::assets::IconName::WandSparkles,
                        Some(info),
                        "Inline assistant",
                    ))
                    .child(
                        Button::new("quick-open")
                            .ghost()
                            .small()
                            .icon(
                                Icon::new(gpui_kit::assets::IconName::Search).text_color(muted),
                            )
                            .tooltip("Go to file (cmd/ctrl+P)")
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.palette_open = true;
                                this.palette_selected = 0;
                                this.palette_input
                                    .update(cx, |state, cx| state.focus(window, cx));
                                cx.notify();
                            })),
                    )
                    .child(icon_button(
                        "settings",
                        gpui_kit::assets::IconName::Settings,
                        Some(muted),
                        "Settings",
                    )),
            )
    }
}
