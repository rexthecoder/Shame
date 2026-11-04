use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    spinner::Spinner,
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

/// A Flutter daemon exists from the moment `flutter run` is spawned, but the
/// session is only ready once the CLI reports "Running". Everything in between
/// (building, installing, hot reload, restart) is a loading state.
fn flutter_session_busy(is_running: bool, status: Option<&str>) -> bool {
    is_running && status != Some("Running")
}

impl DartIde {
    pub(crate) fn render_titlebar(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let traffic_light_inset = if window.is_fullscreen() { px(12.) } else { px(78.) };

        let root_name = if self.has_workspace {
            self.root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("workspace")
                .to_string()
        } else {
            "Shame \u{00b7} Open Folder\u{2026}".to_string()
        };

        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let green = cx.theme().green;
        let info = cx.theme().info;
        // A dot on the agent icon: accent while a run is going, green when a
        // reply (or a question) landed while the rail was closed.
        let agent_dot = if self.agent.running_pid.is_some() {
            Some(cx.theme().primary)
        } else if self.agent.unread_reply && !self.agent_open {
            Some(green)
        } else {
            None
        };

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
                            .tooltip("Toggle sidebar (\u{2318}B)")
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
                            .tooltip("Toggle terminal dock (\u{2318}J)")
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
                                this.set_agent_open(!this.agent_open);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex()
                    .id("titlebar-project-pill")
                    .group("titlebar-project-pill")
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
                    .when_some(
                        self.git_branch.clone().filter(|_| self.has_workspace),
                        |el, branch| {
                            el.child(
                                h_flex()
                                    .items_center()
                                    .gap_1()
                                    .text_size(px(tokens::UI_SM))
                                    .text_color(muted)
                                    .child(
                                        Icon::new(gpui_kit::assets::IconName::GitBranch)
                                            .small()
                                            .text_color(muted),
                                    )
                                    .child(branch),
                            )
                        },
                    )
                    .when(self.has_workspace, |el| {
                        el.child(
                            div()
                                .id("titlebar-close-folder")
                                .cursor_pointer()
                                // Only on hover: a permanent × next to the
                                // branch read as "close branch", not "close
                                // folder". It keeps its space so the pill
                                // doesn't jump.
                                .invisible()
                                .group_hover("titlebar-project-pill", |s| s.visible())
                                .tooltip(|window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new("Close folder")
                                        .build(window, cx)
                                })
                                .text_color(muted)
                                .hover(move |s| s.text_color(fg))
                                .child(
                                    Icon::new(gpui_kit::assets::IconName::X).with_size(px(10.5)),
                                )
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .on_click(cx.listener(|this, _event, _window, cx| {
                                    cx.stop_propagation();
                                    this.request_close_workspace(cx);
                                })),
                        )
                    })
                    .on_click(cx.listener(|this, _event, window, cx| {
                        if this.has_workspace {
                            this.palette_open = true;
                            this.palette_selected = 0;
                            this.palette_input
                                .update(cx, |state, cx| state.focus(window, cx));
                            cx.notify();
                        } else {
                            this.open_folder(window, cx);
                        }
                    })),
            )
            .child(
                h_flex()
                    .flex_1()
                    .justify_end()
                    .items_center()
                    .gap(px(tokens::SPACE_2))
                    // Option A: Single Target Button (`🖥 macOS ▾`) + Dedicated Green `▶ Run` Button
                    // Only shown when the opened workspace is a Dart / Flutter project.
                    .when(self.is_flutter_or_dart_workspace(), |row| {
                        let is_running = self.flutter_daemon.is_some();
                        // A daemon exists from the moment `flutter run` is spawned,
                        // but the app is only ready when the CLI says "Running".
                        // Until then the controls are a loading state, not hot
                        // reload/restart buttons the user can press into a build.
                        let flutter_busy =
                            flutter_session_busy(is_running, self.flutter_status.as_deref());
                        // The device name always stays; what the session is doing sits beside it.
                        let dev_name = self
                            .selected_device
                            .as_ref()
                            .map(|d| d.name.clone())
                            .unwrap_or_else(|| "macOS".to_string());
                        let status_badge: Option<(String, Hsla)> = if flutter_busy {
                            self.flutter_status
                                .clone()
                                .map(|s| (s, cx.theme().warning))
                        } else if is_running {
                            Some(("Running".to_string(), green))
                        } else {
                            None
                        };
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

                        let active_cfg = self
                            .selected_launch_config
                            .and_then(|ix| self.launch_configs.get(ix));
                        let cfg_badge =
                            active_cfg.and_then(|c| c.flavor().or_else(|| Some(c.name.clone())));

                        row.child(
                            h_flex()
                                .id("titlebar-device-picker")
                                .h(px(26.))
                                .px(px(tokens::SPACE_3))
                                .items_center()
                                .gap(px(tokens::SPACE_2))
                                .rounded(px(tokens::RADIUS_SM))
                                .bg(if is_open {
                                    tokens::accent_dim()
                                } else if is_running && !flutter_busy {
                                    green.opacity(0.09)
                                } else {
                                    cx.theme().sidebar
                                })
                                .border_1()
                                .border_color(if is_open {
                                    cx.theme().primary
                                } else if flutter_busy {
                                    cx.theme().warning.opacity(0.45)
                                } else if is_running {
                                    green.opacity(0.45)
                                } else {
                                    cx.theme().border
                                })
                                .cursor_pointer()
                                .hover(|s| s.bg(cx.theme().list_hover))
                                .text_size(px(tokens::UI_SM))
                                .text_color(fg)
                                .child(if flutter_busy {
                                    Spinner::new()
                                        .xsmall()
                                        .color(cx.theme().warning)
                                        .into_any_element()
                                } else if is_running {
                                    // A dot with a soft halo, so "live" reads at a glance.
                                    div()
                                        .size(px(12.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(6.))
                                        .bg(green.opacity(0.22))
                                        .child(div().size(px(6.)).rounded(px(3.)).bg(green))
                                        .into_any_element()
                                } else {
                                    Icon::new(dev_icon)
                                        .with_size(px(12.))
                                        .text_color(green)
                                        .into_any_element()
                                })
                                .child(
                                    div()
                                        .max_w(px(140.))
                                        .truncate()
                                        .font_medium()
                                        .child(dev_name),
                                )
                                .when_some(status_badge, |el, (text, color)| {
                                    el.child(
                                        div()
                                            .max_w(px(150.))
                                            .truncate()
                                            .px(px(5.))
                                            .rounded(px(3.))
                                            .bg(color.opacity(0.16))
                                            .text_size(px(10.))
                                            .font_medium()
                                            .text_color(color)
                                            .child(text),
                                    )
                                })
                                .when_some(cfg_badge, |el, badge| {
                                    el.child(
                                        div()
                                            .px_1()
                                            .rounded(px(3.))
                                            .bg(info.opacity(0.16))
                                            .font_family("SF Mono")
                                            .text_size(px(9.5))
                                            .text_color(info)
                                            .child(badge),
                                    )
                                })
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
                                        this.reload_launch_configs(cx);
                                    }
                                    cx.notify();
                                })),
                        )
                        // Dedicated Run Pill (when idle) or Contextual Reload / Restart / Stop (when running)
                        .child(if self.flutter_daemon.is_some() {
                            h_flex()
                                .h(px(26.))
                                .px(px(tokens::SPACE_1))
                                .items_center()
                                .gap(px(2.))
                                .rounded(px(tokens::RADIUS_SM))
                                .bg(cx.theme().sidebar)
                                .border_1()
                                .border_color(cx.theme().border)
                                .child(if flutter_busy {
                                    // Building/launching (or mid reload): show that the
                                    // session is busy instead of buttons that would
                                    // queue a reload into an app that is not up yet.
                                    div()
                                        .w(px(50.))
                                        .h(px(22.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            Spinner::new()
                                                .xsmall()
                                                .color(cx.theme().warning),
                                        )
                                        .into_any_element()
                                } else {
                                    h_flex()
                                        .items_center()
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
                                        .into_any_element()
                                })
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
                                .into_any_element()
                        } else {
                            h_flex()
                                .items_center()
                                .gap(px(tokens::SPACE_1))
                                .child(
                                    h_flex()
                                        .id("run")
                                        .h(px(26.))
                                        .px(px(tokens::SPACE_3))
                                        .items_center()
                                        .gap(px(tokens::SPACE_2))
                                        .rounded(px(tokens::RADIUS_SM))
                                        .bg(green.opacity(0.15))
                                        .border_1()
                                        .border_color(green.opacity(0.38))
                                        .cursor_pointer()
                                        .hover(move |s| s.bg(green.opacity(0.24)))
                                        .text_size(px(tokens::UI_SM))
                                        .font_medium()
                                        .text_color(green)
                                        .child(
                                            Icon::new(gpui_kit::assets::IconName::Play)
                                                .with_size(px(11.))
                                                .text_color(green),
                                        )
                                        .child("Run")
                                        .on_click(cx.listener(|this, _event, window, cx| {
                                            this.start_flutter_run(window, cx);
                                        })),
                                )
                                .child(
                                    icon_button(
                                        "debug-dap",
                                        gpui_kit::assets::IconName::Bug,
                                        Some(if self.dap_client.is_some() {
                                            cx.theme().primary
                                        } else {
                                            muted
                                        }),
                                        "Debug Dart file with breakpoints (DAP)",
                                    )
                                    .on_click(cx.listener(|this, _event, window, cx| {
                                        this.start_dap_debug(window, cx);
                                    })),
                                )
                                .into_any_element()
                        })
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
                    })
                    // Right-hand tool icons: Web Preview, Agent, Quick Open, Settings
                    .child(
                        icon_button(
                            "titlebar-web-preview",
                            gpui_kit::assets::IconName::Globe,
                            Some(if self.web_preview_open {
                                cx.theme().primary
                            } else {
                                muted
                            }),
                            "Toggle Web Preview rail",
                        )
                        .on_click(cx.listener(|this, _event, window, cx| {
                            this.toggle_web_preview(window, cx);
                        })),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_shrink_0()
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
                                    this.set_agent_open(!this.agent_open);
                                    cx.notify();
                                })),
                            )
                            .when_some(agent_dot, |el, color| {
                                el.child(
                                    div()
                                        .absolute()
                                        .top(px(4.))
                                        .right(px(4.))
                                        .size(px(5.))
                                        .rounded(px(2.5))
                                        .bg(color),
                                )
                            }),
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
                    .child(
                        icon_button(
                            "settings",
                            gpui_kit::assets::IconName::Settings,
                            Some(if self.settings_open {
                                cx.theme().primary
                            } else {
                                muted
                            }),
                            "Settings & Preferences (\u{2318},)",
                        )
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.toggle_settings(cx);
                        })),
                    ),
            )
    }
}

#[cfg(test)]
mod loading_tests {
    use super::*;

    #[core::prelude::v1::test]
    fn the_flutter_controls_are_loading_until_the_cli_says_running() {
        assert!(!flutter_session_busy(false, None));
        // Spawned but not started yet.
        assert!(flutter_session_busy(true, None));
        assert!(flutter_session_busy(true, Some("Launching dev on iPhone 16\u{2026}")));
        assert!(flutter_session_busy(true, Some("Building for iOS\u{2026}")));
        assert!(flutter_session_busy(true, Some("Hot reloading\u{2026}")));
        assert!(flutter_session_busy(true, Some("Restarting\u{2026}")));
        assert!(!flutter_session_busy(true, Some("Running")));
    }
}
