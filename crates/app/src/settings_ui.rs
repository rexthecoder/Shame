//! Settings and About modals.

use super::*;

impl DartIde {
    pub(crate) fn render_settings_modal(
        &self,
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        if !self.settings_open {
            return None;
        }
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let info = cx.theme().info;
        let border = cx.theme().border;
        let list_hover = cx.theme().list_hover;
        let show_hidden = self.show_hidden;
        let light_theme = self.light_theme;
        let sidebar_open = self.sidebar_open;
        let terminal_open = self.terminal_open;
        let sim_attached = self.sim.attached;
        let web_open = self.web_preview_open;
        let agent_open = self.agent_open;
        let current_harness = self.agent.harness;
        let current_mode_ix = self.agent.mode_ix;
        let (_, active_model_label) = self.agent.active_model();
        let active_model_label = active_model_label.to_string();
        let query = self.settings_search.read(cx).value().trim().to_lowercase();
        let active_cat = self.settings_tab;

        const CATEGORIES: &[(&str, &str)] = &[
            ("General", "General workspace and application behavior"),
            ("Appearance", "Theme mode, chrome, and visual presentation"),
            ("Editor", "Code folding, diagnostics, and formatting"),
            ("Search & Files", "File explorer visibility and ignored directories"),
            ("Panels & Layout", "Sidebar, terminal dock, web preview, and agent rails"),
            ("AI", "Coding harness, default model, and permission mode"),
            ("Flutter & Simulator", "Hot reload, embedded iOS Simulator, and VM overlays"),
        ];

        // Helper for a Zed-style pill toggle switch (`38×20` track with `14×14` thumb circle)
        let toggle_switch = |id: &'static str, enabled: bool| {
            h_flex()
                .id(id)
                .w(px(38.))
                .h(px(20.))
                .px(px(3.))
                .items_center()
                .justify_between()
                .rounded(px(999.))
                .bg(if enabled {
                    info.opacity(0.65)
                } else {
                    cx.theme().background
                })
                .border_1()
                .border_color(if enabled { info } else { border })
                .cursor_pointer()
                .child(if enabled {
                    div().w(px(14.)).h(px(1.))
                } else {
                    div()
                        .size(px(13.))
                        .rounded(px(999.))
                        .bg(muted.opacity(0.7))
                })
                .child(if enabled {
                    div().size(px(13.)).rounded(px(999.)).bg(fg)
                } else {
                    div().w(px(14.)).h(px(1.))
                })
        };

        // Helper for a Zed-style select pill (`Label  ⌄`)
        let select_pill = |id: &'static str, label: String| {
            h_flex()
                .id(id)
                .h(px(28.))
                .px(px(12.))
                .items_center()
                .gap(px(8.))
                .rounded(px(6.))
                .bg(cx.theme().secondary)
                .border_1()
                .border_color(border)
                .cursor_pointer()
                .hover(move |s| s.bg(list_hover))
                .text_size(px(12.5))
                .text_color(fg)
                .child(label)
                .child(
                    gpui_kit::component::Icon::new(gpui_kit::assets::IconName::ChevronDown)
                        .with_size(px(10.))
                        .text_color(muted),
                )
        };

        // Helper for a single Zed-style setting row (`Title` + `Description` on left, control on right)
        let setting_row = |title: &'static str, desc: String, control: AnyElement| -> Div {
            h_flex()
                .w_full()
                .py(px(16.))
                .items_center()
                .justify_between()
                .gap(px(24.))
                .border_b_1()
                .border_color(border.opacity(0.55))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w(px(0.))
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(13.5))
                                .font_medium()
                                .text_color(fg)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(muted)
                                .child(desc),
                        ),
                )
                .child(control)
        };

        let mut rows: Vec<AnyElement> = Vec::new();
        let show_cat = |cat_ix: usize, text_match: &str| -> bool {
            if !query.is_empty() {
                text_match.to_lowercase().contains(&query)
            } else {
                active_cat == cat_ix
            }
        };

        // Category 0: General
        if show_cat(0, "workspace folder project open close restore") {
            let ws_desc = if self.has_workspace {
                format!("Active workspace: {}", self.root.display())
            } else {
                "No project folder open. Open a folder to enable file explorer, Git, and LSP."
                    .to_string()
            };
            let v1 = view.clone();
            let ctrl = select_pill("settings-open-folder", "Open Folder\u{2026}".to_string())
                .on_click(move |_e, window, cx| {
                    v1.update(cx, |this, cx| {
                        this.settings_open = false;
                        this.open_folder(window, cx);
                    });
                })
                .into_any_element();
            rows.push(setting_row("Workspace Folder", ws_desc, ctrl).into_any_element());
        }
        if show_cat(0, "on new window welcome restore folder") {
            let v1 = view.clone();
            let has_ws = self.has_workspace;
            let ctrl = select_pill(
                "settings-close-ws",
                if has_ws {
                    "Restore Last Workspace".to_string()
                } else {
                    "Welcome Screen".to_string()
                },
            )
            .on_click(move |_e, _w, cx| {
                if has_ws {
                    v1.update(cx, |this, cx| this.close_workspace(cx));
                }
            })
            .into_any_element();
            rows.push(
                setting_row(
                    "On Startup",
                    "Automatically restore the last opened folder on startup, or show the Welcome screen when closed."
                        .to_string(),
                    ctrl,
                )
                .into_any_element(),
            );
        }

        // Category 1: Appearance
        if show_cat(1, "appearance theme dark light color mode") {
            let v1 = view.clone();
            let ctrl = select_pill(
                "settings-theme-toggle",
                if light_theme {
                    "Shame Light".to_string()
                } else {
                    "Shame Dark".to_string()
                },
            )
            .on_click(move |_e, window, cx| {
                v1.update(cx, |this, cx| {
                    this.light_theme = !this.light_theme;
                    if this.light_theme {
                        gpui_kit::component::Theme::change(
                            gpui_kit::component::ThemeMode::Light,
                            Some(window),
                            cx,
                        );
                    } else {
                        gpui_kit::component::Theme::change(
                            gpui_kit::component::ThemeMode::Dark,
                            Some(window),
                            cx,
                        );
                        crate::theme::apply(cx);
                    }
                    cx.notify();
                });
            })
            .into_any_element();
            rows.push(
                setting_row(
                    "Theme Mode",
                    "Switch between Shame's calibrated dark obsidian theme and light theme."
                        .to_string(),
                    ctrl,
                )
                .into_any_element(),
            );
        }

        // Category 2: Editor
        if show_cat(2, "editor diff split unified view side by side") {
            let unified = self.git.unified_diff;
            let v1 = view.clone();
            let ctrl = select_pill(
                "settings-diff-mode",
                if unified {
                    "Unified Inline".to_string()
                } else {
                    "Side-by-Side Split".to_string()
                },
            )
            .on_click(move |_e, _w, cx| {
                v1.update(cx, |this, cx| this.toggle_diff_unified(cx));
            })
            .into_any_element();
            rows.push(
                setting_row(
                    "Default Diff View",
                    "Choose between side-by-side split diff (with draggable column divider) or unified inline diff."
                        .to_string(),
                    ctrl,
                )
                .into_any_element(),
            );
        }
        if show_cat(2, "editor split pane side by side") {
            let is_split = self.split_tab.is_some();
            let v1 = view.clone();
            let ctrl = toggle_switch("settings-split-editor", is_split)
                .on_click(move |_e, _w, cx| {
                    v1.update(cx, |this, cx| this.toggle_split_editor(cx));
                })
                .into_any_element();
            rows.push(
                setting_row(
                    "Split Editor Pane",
                    "Show two editor buffers side by side in the center workspace."
                        .to_string(),
                    ctrl,
                )
                .into_any_element(),
            );
        }

        // Category 3: Search & Files
        if show_cat(3, "files dotfiles hidden gitignore env explorer") {
            let v1 = view.clone();
            let ctrl = toggle_switch("settings-dotfiles-toggle", show_hidden)
                .on_click(move |_e, _w, cx| {
                    v1.update(cx, |this, cx| {
                        this.show_hidden = !this.show_hidden;
                        if this.has_workspace {
                            this.rebuild_file_tree(cx);
                        }
                        cx.notify();
                    });
                })
                .into_any_element();
            rows.push(
                setting_row(
                    "Show Hidden Dotfiles",
                    "Display dotfiles and dot-directories (.gitignore, .env, .vscode) in the File Explorer."
                        .to_string(),
                    ctrl,
                )
                .into_any_element(),
            );
        }

        // Category 4: Panels & Layout
        if show_cat(4, "panels layout left sidebar explorer git") {
            let v1 = view.clone();
            let ctrl = toggle_switch("settings-sidebar-toggle", sidebar_open)
                .on_click(move |_e, _w, cx| {
                    v1.update(cx, |this, cx| {
                        this.sidebar_open = !this.sidebar_open;
                        cx.notify();
                    });
                })
                .into_any_element();
            rows.push(
                setting_row(
                    "Left Sidebar (Files / Search / Git)",
                    "Show or hide the left project navigation rail (\u{2318}B).".to_string(),
                    ctrl,
                )
                .into_any_element(),
            );
        }
        if show_cat(4, "panels layout terminal dock problems debug") {
            let v1 = view.clone();
            let ctrl = toggle_switch("settings-terminal-toggle", terminal_open)
                .on_click(move |_e, _w, cx| {
                    v1.update(cx, |this, cx| {
                        this.terminal_open = !this.terminal_open;
                        cx.notify();
                    });
                })
                .into_any_element();
            rows.push(
                setting_row(
                    "Bottom Terminal & Problems Dock",
                    "Show or hide the integrated shell, Problems list, and Debug Console (\u{2318}J)."
                        .to_string(),
                    ctrl,
                )
                .into_any_element(),
            );
        }
        if show_cat(4, "panels layout web preview browser localhost") {
            let v1 = view.clone();
            let ctrl = toggle_switch("settings-web-toggle", web_open)
                .on_click(move |_e, window, cx| {
                    v1.update(cx, |this, cx| this.toggle_web_preview(window, cx));
                })
                .into_any_element();
            rows.push(
                setting_row(
                    "Web Preview Rail",
                    "Show the live localhost & HTML preview rail alongside the editor."
                        .to_string(),
                    ctrl,
                )
                .into_any_element(),
            );
        }

        // Category 5: AI
        if show_cat(5, "ai agent harness claude code opencode codex tool") {
            let v1 = view.clone();
            let ctrl = select_pill("settings-harness-cycle", current_harness.label().to_string())
                .on_click(move |_e, _w, cx| {
                    v1.update(cx, |this, cx| {
                        let all = rails::agent::AgentHarness::ALL;
                        let pos = all
                            .iter()
                            .position(|h| *h == this.agent.harness)
                            .unwrap_or(0);
                        let next = all[(pos + 1) % all.len()];
                        this.agent.set_harness(next);
                        cx.notify();
                    });
                })
                .into_any_element();
            rows.push(
                setting_row(
                    "Coding Agent Harness",
                    format!(
                        "Select the CLI coding agent used in the Agent rail (Claude Code, OpenCode, or Codex). Active model: {active_model_label}."
                    ),
                    ctrl,
                )
                .into_any_element(),
            );
        }
        if show_cat(5, "ai permission mode default plan auto edit") {
            let modes = ["Default", "Plan", "Auto-Edit"];
            let mode_lbl = modes[current_mode_ix % modes.len()].to_string();
            let v1 = view.clone();
            let ctrl = select_pill("settings-mode-cycle", mode_lbl)
                .on_click(move |_e, _w, cx| {
                    v1.update(cx, |this, cx| {
                        this.agent.mode_ix = (this.agent.mode_ix + 1) % 3;
                        cx.notify();
                    });
                })
                .into_any_element();
            rows.push(
                setting_row(
                    "Default Permission Mode",
                    "Control whether the AI agent asks before editing, stays in read-only Plan mode, or Auto-Edits."
                        .to_string(),
                    ctrl,
                )
                .into_any_element(),
            );
        }
        if show_cat(5, "ai agent rail open panel") {
            let v1 = view.clone();
            let ctrl = toggle_switch("settings-agent-open", agent_open)
                .on_click(move |_e, _w, cx| {
                    v1.update(cx, |this, cx| {
                        this.agent_open = !this.agent_open;
                        cx.notify();
                    });
                })
                .into_any_element();
            rows.push(
                setting_row(
                    "Show AI Agent Rail",
                    "Dock the multi-harness AI Agent conversation and AI Changes reviewer on the right."
                        .to_string(),
                    ctrl,
                )
                .into_any_element(),
            );
        }

        // Category 6: Flutter & Simulator
        if show_cat(6, "flutter simulator ios docked embedded") {
            let v1 = view.clone();
            let ctrl = toggle_switch("settings-sim-toggle", sim_attached)
                .on_click(move |_e, window, cx| {
                    v1.update(cx, |this, cx| {
                        if this.sim.attached {
                            this.detach_simulator(cx);
                        } else {
                            this.attach_simulator(window, cx);
                        }
                    });
                })
                .into_any_element();
            rows.push(
                setting_row(
                    "Embedded iOS Simulator Rail",
                    "Stream and interact with the booted iOS Simulator directly inside the IDE window."
                        .to_string(),
                    ctrl,
                )
                .into_any_element(),
            );
        }

        let (cat_title, cat_sub) = if !query.is_empty() {
            ("Search Results", "Matching settings across all categories")
        } else {
            CATEGORIES
                .get(active_cat)
                .copied()
                .unwrap_or(("General", "General Settings"))
        };

        Some(
            div()
                .id("settings-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x00000072))
                .on_mouse_down(MouseButton::Left, {
                    let view = view.clone();
                    move |_e, _w, cx| {
                        view.update(cx, |this, cx| {
                            this.settings_open = false;
                            this.save_session_state();
                            cx.notify();
                        });
                    }
                })
                .child(
                    h_flex()
                        .id("settings-card")
                        .w(px(860.))
                        .h(px(560.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(border)
                        .rounded(px(12.))
                        .shadow_xl()
                        .overflow_hidden()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        // Left Navigation Sidebar (Zed-style)
                        .child(
                            v_flex()
                                .w(px(215.))
                                .h_full()
                                .flex_shrink_0()
                                .bg(cx.theme().sidebar)
                                .border_r_1()
                                .border_color(border)
                                .p(px(12.))
                                .justify_between()
                                .child(
                                    v_flex()
                                        .w_full()
                                        .gap(px(10.))
                                        // Search settings input
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .h(px(30.))
                                                .px(px(10.))
                                                .gap(px(8.))
                                                .items_center()
                                                .rounded(px(6.))
                                                .bg(cx.theme().background)
                                                .border_1()
                                                .border_color(border)
                                                .child(
                                                    gpui_kit::component::Icon::new(
                                                        gpui_kit::assets::IconName::Search,
                                                    )
                                                    .with_size(px(12.))
                                                    .text_color(muted),
                                                )
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w(px(0.))
                                                        .child(
                                                            Input::new(&self.settings_search)
                                                                .appearance(false)
                                                                .xsmall(),
                                                        ),
                                                ),
                                        )
                                        // Category list
                                        .child(
                                            v_flex().w_full().gap(px(2.)).children(
                                                CATEGORIES.iter().enumerate().map(
                                                    |(ix, (name, _))| {
                                                        let is_sel =
                                                            query.is_empty() && ix == active_cat;
                                                        let view = view.clone();
                                                        h_flex()
                                                            .id(("settings-cat", ix))
                                                            .w_full()
                                                            .h(px(30.))
                                                            .px(px(10.))
                                                            .gap(px(8.))
                                                            .items_center()
                                                            .rounded(px(6.))
                                                            .cursor_pointer()
                                                            .when(is_sel, |s| {
                                                                s.bg(cx.theme().tab_active)
                                                                    .text_color(fg)
                                                            })
                                                            .when(!is_sel, |s| {
                                                                s.text_color(muted).hover(
                                                                    move |h| {
                                                                        h.bg(list_hover)
                                                                            .text_color(fg)
                                                                    },
                                                                )
                                                            })
                                                            .child(
                                                                gpui_kit::component::Icon::new(
                                                                    gpui_kit::assets::IconName::ChevronRight,
                                                                )
                                                                .with_size(px(10.))
                                                                .text_color(muted),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_size(px(13.))
                                                                    .when(is_sel, |d| {
                                                                        d.font_medium()
                                                                    })
                                                                    .child(*name),
                                                            )
                                                            .on_click(move |_e, window, cx| {
                                                                view.update(cx, |this, cx| {
                                                                    this.settings_tab = ix;
                                                                    this.settings_search.update(
                                                                        cx,
                                                                        |s, cx| {
                                                                            s.set_value(
                                                                                "", window, cx,
                                                                            )
                                                                        },
                                                                    );
                                                                    cx.notify();
                                                                });
                                                            })
                                                    },
                                                ),
                                            ),
                                        ),
                                )
                                // Bottom-left shortcut hint
                                .child(
                                    h_flex()
                                        .px(px(6.))
                                        .items_center()
                                        .gap(px(8.))
                                        .font_family("SF Mono")
                                        .text_size(px(11.))
                                        .text_color(muted.opacity(0.75))
                                        .child("\u{2318},")
                                        .child("Close Settings"),
                                ),
                        )
                        // Right Content Pane (Zed-style)
                        .child(
                            v_flex()
                                .id("settings-content-scroll")
                                .flex_1()
                                .h_full()
                                .min_w(px(0.))
                                .overflow_y_scroll()
                                .px(px(32.))
                                .py(px(24.))
                                .gap(px(6.))
                                // Top bar: "User" pill on left, "Close" button on right
                                .child(
                                    h_flex()
                                        .w_full()
                                        .pb(px(12.))
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div()
                                                .px(px(8.))
                                                .py(px(2.))
                                                .rounded(px(4.))
                                                .bg(info.opacity(0.18))
                                                .text_size(px(11.5))
                                                .font_medium()
                                                .text_color(info)
                                                .child("User"),
                                        )
                                        .child(
                                            h_flex()
                                                .id("close-settings-btn")
                                                .h(px(26.))
                                                .px(px(10.))
                                                .items_center()
                                                .rounded(px(6.))
                                                .bg(cx.theme().secondary)
                                                .border_1()
                                                .border_color(border)
                                                .cursor_pointer()
                                                .hover(move |s| s.bg(list_hover))
                                                .text_size(px(12.))
                                                .text_color(fg)
                                                .child("Done")
                                                .on_click({
                                                    let view = view.clone();
                                                    move |_e, _w, cx| {
                                                        view.update(cx, |this, cx| {
                                                            this.settings_open = false;
                                                            cx.notify();
                                                        });
                                                    }
                                                }),
                                        ),
                                )
                                // Section Header + Subtitle
                                .child(
                                    v_flex()
                                        .w_full()
                                        .pb(px(12.))
                                        .gap(px(4.))
                                        .border_b_1()
                                        .border_color(border.opacity(0.6))
                                        .child(
                                            div()
                                                .text_size(px(17.))
                                                .font_medium()
                                                .text_color(fg)
                                                .child(cat_title),
                                        )
                                        .child(
                                            div()
                                                .font_family("SF Mono")
                                                .text_size(px(11.5))
                                                .text_color(muted)
                                                .child(cat_sub),
                                        ),
                                )
                                .children(rows),
                        ),
                ),
        )
    }

    pub(crate) fn render_about_modal(
        &self,
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        if !self.about_open {
            return None;
        }
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let border = cx.theme().border;

        Some(
            div()
                .id("about-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(110.))
                .bg(rgba(0x00000066))
                .on_mouse_down(MouseButton::Left, {
                    let view = view.clone();
                    move |_e, _w, cx| {
                        view.update(cx, |this, cx| {
                            this.about_open = false;
                            cx.notify();
                        });
                    }
                })
                .child(
                    v_flex()
                        .id("about-card")
                        .w(px(360.))
                        .p(px(tokens::SPACE_5))
                        .gap(px(tokens::SPACE_3))
                        .items_center()
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .px(px(10.))
                                .py(px(4.))
                                .rounded(px(8.))
                                .bg(primary.opacity(0.18))
                                .border_1()
                                .border_color(primary.opacity(0.45))
                                .font_family("SF Mono")
                                .font_semibold()
                                .text_size(px(18.))
                                .text_color(primary)
                                .child("//>"),
                        )
                        .child(
                            div()
                                .text_size(px(16.))
                                .font_semibold()
                                .text_color(fg)
                                .child("Shame IDE"),
                        )
                        .child(
                            div()
                                .font_family("SF Mono")
                                .text_size(px(tokens::UI_XS))
                                .text_color(muted)
                                .child("Version 0.1.0 \u{00b7} Native GPUI macOS IDE"),
                        )
                        .child(
                            div()
                                .text_size(px(tokens::UI_SM))
                                .text_color(muted)
                                .child("21 languages \u{00b7} Flutter & iOS Simulator \u{00b7} Multi-Harness AI Agent"),
                        ),
                ),
        )
    }
}
