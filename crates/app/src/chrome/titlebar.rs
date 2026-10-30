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
                        // From the cached status: never run `git` while rendering.
                        let ahead = self.git.status.as_ref().map_or(0, |s| s.ahead);
                        let behind = self.git.status.as_ref().map_or(0, |s| s.behind);
                        el.child(
                            h_flex()
                                .id("titlebar-branch-chip")
                                .items_center()
                                .gap_1()
                                .px_1p5()
                                .py_0p5()
                                .rounded(px(tokens::RADIUS_PILL))
                                .hover(|s| s.bg(rgba(0xffffff10)))
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
                                })
                                .when(behind > 0, |el| {
                                    el.child(
                                        div()
                                            .text_color(cx.theme().warning)
                                            .child(format!("\u{2193}{behind}")),
                                    )
                                })
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .on_click(cx.listener(|this, _event, _window, cx| {
                                    cx.stop_propagation();
                                    this.sidebar_open = true;
                                    this.set_sidebar_tab(crate::git_panel::SidebarTab::Git, cx);
                                })),
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
                    .gap(px(tokens::SPACE_2))
                    // Unified Run Capsule: [ Device ▾ | Launch Config ▾ | Contextual Actions ]
                    .child({
                        let is_running = self.flutter_daemon.is_some();
                        let dev_name = self
                            .flutter_status
                            .clone()
                            .filter(|s| s != "Running")
                            .or_else(|| self.selected_device.as_ref().map(|d| d.name.clone()))
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
                        let dev_open = self.device_picker_open;

                        let active_cfg = self
                            .selected_launch_config
                            .and_then(|ix| self.launch_configs.get(ix));
                        let cfg_name = active_cfg
                            .map(|c| c.name.clone())
                            .unwrap_or_else(|| "launch.json".to_string());
                        let flavor = active_cfg.and_then(|c| c.flavor());
                        let cfg_open = self.launch_picker_open;
                        let has_configs = !self.launch_configs.is_empty()
                            || !self.test_launch_templates.is_empty();

                        h_flex()
                            .h(px(26.))
                            .px(px(tokens::SPACE_1))
                            .items_center()
                            .gap(px(tokens::SPACE_1))
                            .rounded(px(tokens::RADIUS_PILL))
                            .bg(cx.theme().sidebar)
                            .border_1()
                            .border_color(if is_running {
                                green.opacity(0.38)
                            } else if dev_open || cfg_open {
                                cx.theme().list_active_border
                            } else {
                                cx.theme().border
                            })
                            // 1. Device segment
                            .child(
                                h_flex()
                                    .id("titlebar-device-picker")
                                    .h(px(20.))
                                    .px(px(tokens::SPACE_3))
                                    .items_center()
                                    .gap(px(tokens::SPACE_2))
                                    .rounded(px(tokens::RADIUS_PILL))
                                    .cursor_pointer()
                                    .when(dev_open, |s| s.bg(tokens::accent_dim()))
                                    .hover(|s| s.bg(cx.theme().list_hover))
                                    .text_size(px(tokens::UI_SM))
                                    .text_color(fg)
                                    .child(if is_running {
                                        div()
                                            .size(px(6.))
                                            .rounded(px(3.))
                                            .bg(green)
                                            .into_any_element()
                                    } else {
                                        Icon::new(dev_icon)
                                            .with_size(px(12.))
                                            .text_color(green)
                                            .into_any_element()
                                    })
                                    .child(
                                        div()
                                            .max_w(px(130.))
                                            .truncate()
                                            .child(dev_name),
                                    )
                                    .child(
                                        Icon::new(gpui_kit::assets::IconName::ChevronDown)
                                            .with_size(px(9.5))
                                            .text_color(muted),
                                    )
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.device_picker_open = !this.device_picker_open;
                                        this.launch_picker_open = false;
                                        if this.device_picker_open {
                                            this.refresh_devices(cx);
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(div().w(px(1.)).h(px(12.)).bg(cx.theme().border))
                            // 2. Launch config segment (hidden while running to keep the running bar extra clean)
                            .when(!is_running, |el| {
                                el.child(
                                    h_flex()
                                        .id("titlebar-launch-picker")
                                        .h(px(20.))
                                        .px(px(tokens::SPACE_3))
                                        .items_center()
                                        .gap(px(tokens::SPACE_1))
                                        .rounded(px(tokens::RADIUS_PILL))
                                        .cursor_pointer()
                                        .when(cfg_open, |s| s.bg(tokens::accent_dim()))
                                        .hover(|s| s.bg(cx.theme().list_hover))
                                        .text_size(px(tokens::UI_SM))
                                        .text_color(if has_configs { fg } else { muted })
                                        .child(
                                            div()
                                                .max_w(px(110.))
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
                                                .with_size(px(9.5))
                                                .text_color(muted),
                                        )
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            this.launch_picker_open = !this.launch_picker_open;
                                            this.device_picker_open = false;
                                            if this.launch_picker_open {
                                                this.reload_launch_configs(cx);
                                            }
                                            cx.notify();
                                        })),
                                )
                                .child(div().w(px(1.)).h(px(12.)).bg(cx.theme().border))
                            })
                            // 3. Contextual Action Segment: (Run + Debug when idle) vs (Hot Reload + Restart + Stop when running)
                            .child(if is_running {
                                h_flex()
                                    .items_center()
                                    .px(px(tokens::SPACE_1))
                                    .gap(px(2.))
                                    .child(
                                        Button::new("fast")
                                            .ghost()
                                            .xsmall()
                                            .icon(
                                                Icon::new(gpui_kit::assets::IconName::Zap)
                                                    .with_size(px(12.))
                                                    .text_color(cx.theme().warning),
                                            )
                                            .tooltip("Hot reload (\u{2318}S)")
                                            .on_click(cx.listener(|this, _event, _window, cx| {
                                                this.trigger_hot_reload(cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("hot-restart")
                                            .ghost()
                                            .xsmall()
                                            .icon(
                                                Icon::new(gpui_kit::assets::IconName::RotateCw)
                                                    .with_size(px(12.))
                                                    .text_color(info),
                                            )
                                            .tooltip("Hot restart Flutter app")
                                            .on_click(cx.listener(|this, _event, _window, cx| {
                                                this.trigger_hot_restart(cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("stop-flutter")
                                            .ghost()
                                            .xsmall()
                                            .icon(
                                                Icon::new(gpui_kit::assets::IconName::X)
                                                    .with_size(px(12.))
                                                    .text_color(cx.theme().danger),
                                            )
                                            .tooltip("Stop running Flutter app")
                                            .on_click(cx.listener(|this, _event, _window, cx| {
                                                this.stop_flutter_run(cx);
                                            })),
                                    )
                            } else {
                                h_flex()
                                    .items_center()
                                    .px(px(tokens::SPACE_1))
                                    .gap(px(2.))
                                    .child(
                                        Button::new("run")
                                            .ghost()
                                            .xsmall()
                                            .icon(
                                                Icon::new(gpui_kit::assets::IconName::Play)
                                                    .with_size(px(12.))
                                                    .text_color(green),
                                            )
                                            .tooltip("Run project on selected device")
                                            .on_click(cx.listener(|this, _event, window, cx| {
                                                this.start_flutter_run(window, cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("debug-dap")
                                            .ghost()
                                            .xsmall()
                                            .icon(
                                                Icon::new(gpui_kit::assets::IconName::Bug)
                                                    .with_size(px(12.))
                                                    .text_color(if self.dap_client.is_some() {
                                                        cx.theme().primary
                                                    } else {
                                                        muted
                                                    }),
                                            )
                                            .tooltip("Debug Dart file with breakpoints (DAP)")
                                            .on_click(cx.listener(|this, _event, window, cx| {
                                                this.start_dap_debug(window, cx);
                                            })),
                                    )
                            })
                    })
                    // Right-hand tool icons: Simulator, Agent, Quick Open, Settings
                    .child(
                        icon_button(
                            "titlebar-sim-attach",
                            gpui_kit::assets::IconName::Smartphone,
                            Some(if self.sim.attached {
                                cx.theme().primary
                            } else {
                                muted
                            }),
                            "Toggle iOS Simulator panel",
                        )
                        .on_click(cx.listener(|this, _event, window, cx| {
                            if this.sim.attached {
                                this.detach_simulator(cx);
                            } else {
                                this.attach_simulator(window, cx);
                            }
                        })),
                    )
                    .child(
                        icon_button(
                            "agent-wand",
                            gpui_kit::assets::IconName::WandSparkles,
                            Some(if self.agent_open {
                                cx.theme().primary
                            } else {
                                info
                            }),
                            "Toggle Agent rail",
                        )
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.agent_open = !this.agent_open;
                            cx.notify();
                        })),
                    )
                    .child(
                        Button::new("quick-open")
                            .ghost()
                            .small()
                            .icon(
                                Icon::new(gpui_kit::assets::IconName::Search).text_color(muted),
                            )
                            .tooltip("Go to file / Run command (\u{2318}P / \u{2318}\u{21e7}P)")
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
