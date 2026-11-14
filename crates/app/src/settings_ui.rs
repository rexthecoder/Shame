//! Settings and About modals.

use super::*;
use gpui_kit::component::button::Button;

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
        let primary = cx.theme().primary;
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
            (
                "Language Servers",
                "Installed language servers and their install commands",
            ),
        ];

        // A quiet switch: the accent is the only "on" colour in the modal.
        let toggle_switch = |id: &'static str, enabled: bool| {
            h_flex()
                .id(id)
                .w(px(38.))
                .h(px(20.))
                .px(px(3.))
                .items_center()
                .justify_between()
                .rounded(px(tokens::RADIUS_PILL))
                .bg(if enabled {
                    primary.opacity(0.28)
                } else {
                    cx.theme().background
                })
                .border_1()
                .border_color(if enabled { primary.opacity(0.7) } else { border })
                .cursor_pointer()
                .child(if enabled {
                    div().w(px(14.)).h(px(1.))
                } else {
                    div()
                        .size(px(13.))
                        .rounded(px(tokens::RADIUS_PILL))
                        .bg(muted.opacity(0.7))
                })
                .child(if enabled {
                    div()
                        .size(px(13.))
                        .rounded(px(tokens::RADIUS_PILL))
                        .bg(primary)
                } else {
                    div().w(px(14.)).h(px(1.))
                })
        };

        // The control a row ends with: a compact, bordered, obviously clickable
        // pill (the label is the current value, not a menu title).
        let select_pill = |id: &'static str, label: String| {
            h_flex()
                .id(id)
                .flex_shrink_0()
                .h(px(26.))
                .px(px(9.))
                .items_center()
                .gap(px(tokens::SPACE_2))
                .rounded(px(tokens::RADIUS_SM))
                .bg(cx.theme().background)
                .border_1()
                .border_color(border)
                .cursor_pointer()
                .hover(move |s| s.bg(list_hover).border_color(primary.opacity(0.45)))
                .text_size(px(tokens::UI_SM))
                .text_color(fg)
                .child(label)
                .child(
                    gpui_kit::component::Icon::new(gpui_kit::assets::IconName::ChevronDown)
                        .with_size(px(10.))
                        .text_color(muted),
                )
        };

        // An action in the same pill shape, but without the chevron: these
        // open a picker or perform something, they are not current values.
        let action_pill = |id: &'static str, label: &'static str| {
            h_flex()
                .id(id)
                .flex_shrink_0()
                .h(px(26.))
                .px(px(9.))
                .items_center()
                .rounded(px(tokens::RADIUS_SM))
                .bg(cx.theme().background)
                .border_1()
                .border_color(border)
                .cursor_pointer()
                .hover(move |s| s.bg(list_hover).border_color(primary.opacity(0.45)))
                .text_size(px(tokens::UI_SM))
                .text_color(fg)
                .child(label)
        };

        // One setting: label and explanation on the left, one control on the
        // right, with a hairline between rows.
        let setting_row = |title: &'static str, desc: String, control: AnyElement| -> Div {
            h_flex()
                .w_full()
                .flex_shrink_0()
                .py(px(14.))
                .items_center()
                .justify_between()
                .gap(px(20.))
                .border_b_1()
                .border_color(border.opacity(0.55))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w(px(0.))
                        .gap(px(tokens::SPACE_1))
                        .child(
                            div()
                                .text_size(px(tokens::UI))
                                .font_medium()
                                .text_color(fg)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(px(tokens::UI_SM))
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
            let v1 = view.clone();
            let ctrl = action_pill("settings-open-folder", "Open Folder\u{2026}")
                .on_click(move |_e, window, cx| {
                    v1.update(cx, |this, cx| {
                        this.settings_open = false;
                        this.open_folder(window, cx);
                    });
                })
                .into_any_element();
            let mut left = v_flex()
                .flex_1()
                .min_w(px(0.))
                .gap(px(tokens::SPACE_1))
                .child(
                    div()
                        .text_size(px(tokens::UI))
                        .font_medium()
                        .text_color(fg)
                        .child("Workspace Folder"),
                );
            if self.has_workspace {
                let path = self.root.display().to_string();
                let path_tip = path.clone();
                left = left
                    .child(
                        div()
                            .text_size(px(tokens::UI_SM))
                            .text_color(muted)
                            .child("Active workspace"),
                    )
                    .child(
                        div()
                            .id("settings-workspace-path")
                            .min_w(px(0.))
                            .truncate()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(tokens::UI_XS))
                            .text_color(muted.opacity(0.8))
                            .child(path)
                            .tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(path_tip.clone())
                                    .build(window, cx)
                            }),
                    );
            } else {
                left = left.child(
                    div()
                        .text_size(px(tokens::UI_SM))
                        .text_color(muted)
                        .child(
                            "No project folder open. Open a folder to enable the explorer, Git, and LSP.",
                        ),
                );
            }
            rows.push(
                h_flex()
                    .w_full()
                    .flex_shrink_0()
                    .py(px(14.))
                    .items_center()
                    .justify_between()
                    .gap(px(20.))
                    .border_b_1()
                    .border_color(border.opacity(0.55))
                    .child(left)
                    .child(ctrl)
                    .into_any_element(),
            );
        }
        if self.has_workspace
            && show_cat(0, "close workspace welcome recent folder startup")
        {
            let v1 = view.clone();
            let ctrl = action_pill("settings-close-ws", "Close Workspace")
                .on_click(move |_e, _w, cx| {
                    v1.update(cx, |this, cx| this.request_close_workspace(cx));
                })
                .into_any_element();
            rows.push(
                setting_row(
                    "Close Workspace",
                    "Return to the Welcome screen. The folder stays in Recent Projects."
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
        if show_cat(2, "editor save autosave auto save files") {
            let mode = self.autosave;
            let v1 = view.clone();
            let ctrl = select_pill("settings-autosave", mode.label().to_string())
                .on_click(move |_e, _w, cx| {
                    v1.update(cx, |this, cx| {
                        let next = this.autosave.next();
                        this.set_autosave(next, cx);
                    });
                })
                .into_any_element();
            rows.push(
                setting_row(
                    "Auto Save",
                    "Save modified files automatically: a second after you stop typing, or when you switch to another app."
                        .to_string(),
                    ctrl,
                )
                .into_any_element(),
            );
        }
        if show_cat(2, "editor save format on save formatter") {
            let on = self.save_options.format_on_save;
            let v1 = view.clone();
            let ctrl = toggle_switch("settings-format-on-save", on)
                .on_click(move |_e, _w, cx| {
                    v1.update(cx, |this, cx| {
                        this.update_save_options(|o| o.format_on_save = !o.format_on_save, cx)
                    });
                })
                .into_any_element();
            rows.push(
                setting_row(
                    "Format on Save",
                    "Run the language server's formatter before writing the file (\u{2318}S). \"Save Without Formatting\" skips it once."
                        .to_string(),
                    ctrl,
                )
                .into_any_element(),
            );
        }
        if show_cat(2, "editor save trim trailing whitespace") {
            let on = self.save_options.trim_trailing_whitespace;
            let v1 = view.clone();
            let ctrl = toggle_switch("settings-trim-whitespace", on)
                .on_click(move |_e, _w, cx| {
                    v1.update(cx, |this, cx| {
                        this.update_save_options(
                            |o| o.trim_trailing_whitespace = !o.trim_trailing_whitespace,
                            cx,
                        )
                    });
                })
                .into_any_element();
            rows.push(
                setting_row(
                    "Trim Trailing Whitespace",
                    "Remove spaces and tabs at the end of each line when saving.".to_string(),
                    ctrl,
                )
                .into_any_element(),
            );
        }
        if show_cat(2, "editor save final newline end of file") {
            let on = self.save_options.insert_final_newline;
            let v1 = view.clone();
            let ctrl = toggle_switch("settings-final-newline", on)
                .on_click(move |_e, _w, cx| {
                    v1.update(cx, |this, cx| {
                        this.update_save_options(|o| o.insert_final_newline = !o.insert_final_newline, cx)
                    });
                })
                .into_any_element();
            rows.push(
                setting_row(
                    "Insert Final Newline",
                    "End every non-empty file with a single newline when saving.".to_string(),
                    ctrl,
                )
                .into_any_element(),
            );
        }
        if show_cat(2, "editor indent guides lines nesting") {
            let on = self.editor_options.indent_guides;
            let v1 = view.clone();
            let ctrl = toggle_switch("settings-indent-guides", on)
                .on_click(move |_e, _w, cx| {
                    v1.update(cx, |this, cx| {
                        this.update_editor_options(|o| o.indent_guides = !o.indent_guides, cx)
                    });
                })
                .into_any_element();
            rows.push(
                setting_row(
                    "Indent Guides",
                    "Faint vertical lines at each indent level, with the block around the cursor lit."
                        .to_string(),
                    ctrl,
                )
                .into_any_element(),
            );
        }
        if show_cat(2, "editor bracket pair colors colorization nesting") {
            let on = self.editor_options.bracket_colors;
            let v1 = view.clone();
            let ctrl = toggle_switch("settings-bracket-colors", on)
                .on_click(move |_e, _w, cx| {
                    v1.update(cx, |this, cx| {
                        this.update_editor_options(|o| o.bracket_colors = !o.bracket_colors, cx)
                    });
                })
                .into_any_element();
            rows.push(
                setting_row(
                    "Bracket Pair Colors",
                    "Color (), [] and {} by how deeply they're nested, so matching pairs share a color."
                        .to_string(),
                    ctrl,
                )
                .into_any_element(),
            );
        }
        if show_cat(2, "editor word wrap soft wrap long lines") {
            let on = self.editor_options.soft_wrap;
            let v1 = view.clone();
            let ctrl = toggle_switch("settings-soft-wrap", on)
                .on_click(move |_e, _w, cx| {
                    v1.update(cx, |this, cx| {
                        this.update_editor_options(|o| o.soft_wrap = !o.soft_wrap, cx)
                    });
                })
                .into_any_element();
            rows.push(
                setting_row(
                    "Word Wrap",
                    "Break long lines at the editor's width instead of scrolling sideways."
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
        if show_cat(4, "web preview browser logins cookies profile sign in") {
            let on = self.web_preview_keep_logins;
            let v1 = view.clone();
            let ctrl = toggle_switch("settings-preview-logins", on)
                .on_click(move |_e, _w, cx| {
                    v1.update(cx, |this, cx| {
                        this.web_preview_keep_logins = !this.web_preview_keep_logins;
                        this.save_session_state();
                        // Takes effect when the preview's browser next starts.
                        this.stop_web_preview(cx);
                    });
                })
                .into_any_element();
            rows.push(
                setting_row(
                    "Keep Preview Sign-ins",
                    "Remember cookies and logins in the web preview between sessions. Off, every session starts clean."
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
                        this.set_agent_open(!this.agent_open);
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

        // Category 7: Language Servers
        if show_cat(7, "language server lsp pyright gopls rust-analyzer clangd diagnostics") {
            let total = self.lsp_servers.len();
            let ready = self
                .lsp_servers
                .iter()
                .filter(|s| s.is_ready())
                .count();
            let missing = total.saturating_sub(ready);
            let summary = if total == 0 {
                "No language servers configured for this build.".to_string()
            } else if missing == 0 {
                format!("{ready} of {total} language servers available")
            } else {
                format!("{ready} of {total} available · {missing} missing")
            };
            let v1 = view.clone();
            let ctrl = select_pill("settings-lsp-rescan", "Rescan".to_string())
                .on_click(move |_e, _window, cx| {
                    v1.update(cx, |this, cx| {
                        this.refresh_lsp_health();
                        cx.notify();
                    });
                })
                .into_any_element();
            rows.push(
                setting_row("Language Servers", summary, ctrl).into_any_element(),
            );

            for server in &self.lsp_servers {
                let ready_row = server.is_ready();
                let detail = if let Some(path) = &server.path {
                    let mut cmd = crate::lsp_health::compact_path(path);
                    if !server.args.is_empty() {
                        cmd.push(' ');
                        cmd.push_str(&server.args);
                    }
                    cmd
                } else {
                    format!("Install: {}", server.install)
                };
                let (chip, chip_color) = if ready_row {
                    (
                        "Ready",
                        cx.theme().success,
                    )
                } else {
                    ("Missing", cx.theme().warning)
                };
                let chip_el = div()
                    .h(px(22.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(999.))
                    .bg(chip_color.opacity(0.14))
                    .text_size(px(11.))
                    .font_medium()
                    .text_color(chip_color)
                    .child(chip)
                    .into_any_element();
                rows.push(
                    setting_row(
                        server.name,
                        detail,
                        chip_el,
                    )
                    .into_any_element(),
                );
            }
        }

        let (cat_title, cat_sub) = if !query.is_empty() {
            ("Search Results", "Matching settings across all categories")
        } else {
            CATEGORIES
                .get(active_cat)
                .copied()
                .unwrap_or(("General", "General Settings"))
        };

        let category_icons = |ix: usize| match ix {
            0 => gpui_kit::assets::IconName::Settings,
            1 => gpui_kit::assets::IconName::Palette,
            2 => gpui_kit::assets::IconName::Code,
            3 => gpui_kit::assets::IconName::Search,
            4 => gpui_kit::assets::IconName::LayoutPanelLeft,
            5 => gpui_kit::assets::IconName::Sparkles,
            6 => gpui_kit::assets::IconName::Smartphone,
            _ => gpui_kit::assets::IconName::Server,
        };

        Some(
            div()
                .id("settings-backdrop")
                .occlude()
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
                    v_flex()
                        .id("settings-card")
                        .w(px(880.))
                        .h(px(600.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .overflow_hidden()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        // One modal header, not a second titlebar inside the pane.
                        .child(
                            h_flex()
                                .w_full()
                                .h(px(52.))
                                .flex_shrink_0()
                                .items_center()
                                .justify_between()
                                .px(px(tokens::SPACE_5))
                                .border_b_1()
                                .border_color(border)
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap(px(tokens::SPACE_3))
                                        .child(
                                            gpui_kit::component::Icon::new(
                                                gpui_kit::assets::IconName::Settings,
                                            )
                                            .with_size(px(15.))
                                            .text_color(muted),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(tokens::UI_LG))
                                                .font_semibold()
                                                .text_color(fg)
                                                .child("Settings"),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap(px(tokens::SPACE_3))
                                        .child(
                                            h_flex()
                                                .items_center()
                                                .gap(px(tokens::SPACE_2))
                                                .font_family(
                                                    cx.theme().mono_font_family.clone(),
                                                )
                                                .text_size(px(tokens::UI_XS))
                                                .text_color(muted.opacity(0.75))
                                                .child("\u{2318},")
                                                .child("to close"),
                                        )
                                        .child(
                                            div()
                                                .id("close-settings-btn")
                                                .size(px(26.))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .rounded(px(tokens::RADIUS_SM))
                                                .cursor_pointer()
                                                .hover(move |s| s.bg(list_hover))
                                                .tooltip(|window, cx| {
                                                    gpui_kit::component::tooltip::Tooltip::new(
                                                        "Close settings",
                                                    )
                                                    .build(window, cx)
                                                })
                                                .child(
                                                    gpui_kit::component::Icon::new(
                                                        gpui_kit::assets::IconName::X,
                                                    )
                                                    .with_size(px(13.))
                                                    .text_color(muted),
                                                )
                                                .on_click({
                                                    let view = view.clone();
                                                    move |_e, _w, cx| {
                                                        view.update(cx, |this, cx| {
                                                            this.settings_open = false;
                                                            this.save_session_state();
                                                            cx.notify();
                                                        });
                                                    }
                                                }),
                                        ),
                                ),
                        )
                        .child(
                            h_flex()
                                .flex_1()
                                .w_full()
                                .min_h(px(0.))
                                // Navigation: a flat list with icons, no disclosure chevrons.
                                .child(
                                    v_flex()
                                        .w(px(220.))
                                        .h_full()
                                        .flex_shrink_0()
                                        .bg(cx.theme().sidebar)
                                        .border_r_1()
                                        .border_color(border)
                                        .p(px(tokens::SPACE_5))
                                        .justify_between()
                                        .child(
                                            v_flex()
                                                .w_full()
                                                .gap(px(tokens::SPACE_4))
                                                .child(
                                                    h_flex()
                                                        .w_full()
                                                        .h(px(30.))
                                                        .px(px(tokens::SPACE_2))
                                                        .gap(px(tokens::SPACE_2))
                                                        .items_center()
                                                        .rounded(px(tokens::RADIUS_SM))
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
                                                                    Input::new(
                                                                        &self.settings_search,
                                                                    )
                                                                    .appearance(false)
                                                                    .xsmall(),
                                                                ),
                                                        ),
                                                )
                                                .child(
                                                    v_flex().w_full().gap(px(2.)).children(
                                                        CATEGORIES.iter().enumerate().map(
                                                            |(ix, (name, _))| {
                                                                let is_sel = query.is_empty()
                                                                    && ix == active_cat;
                                                                let view = view.clone();
                                                                h_flex()
                                                                    .id(("settings-cat", ix))
                                                                    .w_full()
                                                                    .h(px(30.))
                                                                    .px(px(tokens::SPACE_2))
                                                                    .gap(px(tokens::SPACE_2))
                                                                    .items_center()
                                                                    .rounded(px(tokens::RADIUS_SM))
                                                                    .cursor_pointer()
                                                                    .when(is_sel, |s| {
                                                                        s.bg(tokens::accent_dim())
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
                                                                            category_icons(ix),
                                                                        )
                                                                        .with_size(px(13.))
                                                                        .text_color(if is_sel {
                                                                            primary
                                                                        } else {
                                                                            muted
                                                                        }),
                                                                    )
                                                                    .child(
                                                                        div()
                                                                            .text_size(px(tokens::UI_SM))
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
                                        .child(
                                            h_flex()
                                                .px(px(tokens::SPACE_2))
                                                .items_center()
                                                .gap(px(tokens::SPACE_2))
                                                .font_family(
                                                    cx.theme().mono_font_family.clone(),
                                                )
                                                .text_size(px(tokens::UI_XS))
                                                .text_color(muted.opacity(0.7))
                                                .child(
                                                    gpui_kit::component::Icon::new(
                                                        gpui_kit::assets::IconName::Check,
                                                    )
                                                    .with_size(px(11.)),
                                                )
                                                .child("Saved automatically"),
                                        ),
                                )
                                // Content: one readable column, not a full-width
                                // label-control gap across the whole modal.
                                .child(
                                    v_flex()
                                        .id("settings-content-scroll")
                                        .flex_1()
                                        .h_full()
                                        .min_w(px(0.))
                                        .overflow_y_scroll()
                                        .px(px(28.))
                                        .py(px(tokens::SPACE_6))
                                        .child(
                                            v_flex()
                                                .w_full()
                                                .max_w(px(620.))
                                                .gap(px(tokens::SPACE_1))
                                                .child(
                                                    v_flex()
                                                        .w_full()
                                                        .pb(px(tokens::SPACE_5))
                                                        .gap(px(tokens::SPACE_1))
                                                        .border_b_1()
                                                        .border_color(border.opacity(0.6))
                                                        .child(
                                                            div()
                                                                .text_size(px(tokens::UI_LG))
                                                                .font_semibold()
                                                                .text_color(fg)
                                                                .child(cat_title),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_size(px(tokens::UI_SM))
                                                                .text_color(muted)
                                                                .child(cat_sub),
                                                        ),
                                                )
                                                .children(rows),
                                        ),
                                ),
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
        let card_bg = cx.theme().popover;
        let subtle = cx.theme().background;
        let info = cx.theme().info;

        let stat = |label: &'static str, value: &'static str| {
            v_flex()
                .flex_1()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(tokens::UI_XS))
                        .text_color(muted.opacity(0.75))
                        .child(label),
                )
                .child(
                    div()
                        .text_size(px(tokens::UI_XS))
                        .font_semibold()
                        .text_color(fg)
                        .child(value),
                )
        };

        Some(
            div()
                .id("about-backdrop")
                .occlude()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x00000080))
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
                        .w(px(420.))
                        .overflow_hidden()
                        .bg(card_bg)
                        .border_1()
                        .border_color(border)
                        .rounded(px(14.))
                        .shadow_xl()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        // Hero band
                        .child(
                            v_flex()
                                .w_full()
                                .items_center()
                                .gap(px(tokens::SPACE_3))
                                .pt(px(tokens::SPACE_6))
                                .pb(px(tokens::SPACE_5))
                                .px(px(tokens::SPACE_5))
                                .bg(subtle.opacity(0.55))
                                .child(
                                    // Terracotta squircle mark with the `//>` glyph
                                    div()
                                        .w(px(64.))
                                        .h(px(64.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(16.))
                                        .bg(primary)
                                        .shadow_lg()
                                        .child(
                                            div()
                                                .font_family("SF Mono")
                                                .font_bold()
                                                .text_size(px(24.))
                                                .text_color(cx.theme().primary_foreground)
                                                .child("//>"),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_size(px(20.))
                                        .font_semibold()
                                        .text_color(fg)
                                        .child("Shame"),
                                )
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .child(
                                            div()
                                                .px(px(7.))
                                                .py(px(2.))
                                                .rounded(px(999.))
                                                .bg(info.opacity(0.14))
                                                .border_1()
                                                .border_color(info.opacity(0.30))
                                                .font_family("SF Mono")
                                                .text_size(px(10.))
                                                .text_color(info)
                                                .child(env!("CARGO_PKG_VERSION")),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(tokens::UI_XS))
                                                .text_color(muted)
                                                .child("Native GPUI macOS IDE"),
                                        ),
                                ),
                        )
                        // Body
                        .child(
                            v_flex()
                                .w_full()
                                .gap(px(tokens::SPACE_4))
                                .p(px(tokens::SPACE_5))
                                .child(
                                    div()
                                        .text_size(px(tokens::UI_SM))
                                        .line_height(px(19.))
                                        .text_color(muted)
                                        .child(
                                            "A fast, keyboard-first code editor built for Flutter and Dart, \
                                             with an embedded simulator, a Git workflow, and a live \
                                             multi-harness AI agent.",
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .w_full()
                                        .gap(px(tokens::SPACE_4))
                                        .p(px(tokens::SPACE_3))
                                        .rounded(px(tokens::RADIUS_SM))
                                        .bg(subtle.opacity(0.6))
                                        .border_1()
                                        .border_color(border.opacity(0.6))
                                        .child(stat("Languages", "21"))
                                        .child(stat("Harnesses", "3"))
                                        .child(stat("Engine", "GPUI")),
                                )
                                .child(
                                    h_flex()
                                        .w_full()
                                        .justify_end()
                                        .gap(px(tokens::SPACE_2))
                                        .child(
                                            Button::new("about-close")
                                                .small()
                                                .label("Close")
                                                .on_click(cx.listener(
                                                    |this, _e, _w, cx| {
                                                        this.about_open = false;
                                                        cx.notify();
                                                    },
                                                )),
                                        ),
                                ),
                        ),
                ),
        )
    }
}
