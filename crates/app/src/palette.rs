//! Command palette, Quick Open, symbol outline and project search.

use super::*;

impl DartIde {
    pub(crate) fn run_project_search(&mut self, query: String, cx: &mut Context<Self>) {
        if query.is_empty() {
            self.project_search_matches.clear();
            cx.notify();
            return;
        }
        let root = self.root.clone();
        let q = query.clone();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(core_workspace::search_text(&root, &q));
        });
        cx.spawn(async move |weak, cx| {
            let Ok(matches) = rx.recv().await else { return };
            let _ = weak.update(cx, |this, cx| {
                if this.project_search_input.read(cx).value().trim() == query {
                    this.project_search_matches = matches;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Replace the search query with the replace input's text across the
    /// workspace, then reload any open tabs whose files changed. Refuses
    /// while a tab has unsaved edits, so a replace can't clobber them.
    pub(crate) fn replace_in_project(&mut self, cx: &mut Context<Self>) {
        let query = self.project_search_input.read(cx).value().trim().to_string();
        let replacement = self.project_replace_input.read(cx).value().to_string();
        if query.is_empty() {
            return;
        }
        if self.tabs.iter().any(|t| t.editor.read(cx).buffer().is_dirty()) {
            self.project_replace_status =
                Some("Save or close tabs with unsaved changes first.".to_string());
            cx.notify();
            return;
        }
        match core_workspace::replace_text(&self.root, &query, &replacement) {
            Ok(changed) => {
                let total: usize = changed.iter().map(|(_, n)| n).sum();
                for (path, _) in &changed {
                    for tab in &self.tabs {
                        if tab.path.as_deref() == Some(path.as_path()) {
                            tab.editor.update(cx, |editor, cx| {
                                let _ = editor.open_file(path, cx);
                            });
                        }
                    }
                }
                self.project_replace_status = Some(format!(
                    "Replaced {total} occurrence{} in {} file{}.",
                    if total == 1 { "" } else { "s" },
                    changed.len(),
                    if changed.len() == 1 { "" } else { "s" },
                ));
                self.run_project_search(query, cx);
            }
            Err(err) => self.project_replace_status = Some(format!("Replace failed: {err}")),
        }
        cx.notify();
    }

    pub(crate) fn palette_is_command_mode(&self, cx: &Context<Self>) -> bool {
        self.palette_input
            .read(cx)
            .value()
            .trim_start()
            .starts_with('>')
    }

    pub(crate) fn palette_is_symbol_mode(&self, cx: &Context<Self>) -> bool {
        self.palette_input
            .read(cx)
            .value()
            .trim_start()
            .starts_with('@')
    }

    pub(crate) fn palette_is_workspace_symbol_mode(&self, cx: &Context<Self>) -> bool {
        self.palette_input
            .read(cx)
            .value()
            .trim_start()
            .starts_with('#')
    }

    pub(crate) fn palette_workspace_symbol_matches(
        &self,
        cx: &Context<Self>,
    ) -> Vec<core_workspace::WorkspaceSymbol> {
        let raw = self.palette_input.read(cx).value().to_string();
        let query = raw.trim_start().strip_prefix('#').unwrap_or(&raw).to_string();
        core_workspace::filter_symbols(&self.workspace_symbols, &query, 40)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Open the palette in `#` mode and (re)index the workspace's
    /// declarations on a background thread.
    pub(crate) fn open_workspace_symbols(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_open = true;
        self.palette_selected = 0;
        self.palette_input.update(cx, |s, cx| {
            s.set_value("# ", window, cx);
            s.focus(window, cx);
        });
        let root = self.root.clone();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(core_workspace::index_workspace_symbols(&root));
        });
        cx.spawn(async move |weak, cx| {
            let Ok(symbols) = rx.recv().await else { return };
            let _ = weak.update(cx, |this, cx| {
                this.workspace_symbols = symbols;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_workspace_symbol_at(
        &mut self,
        symbol: core_workspace::WorkspaceSymbol,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.palette_open = false;
        self.open_path(symbol.path, window, cx);
        if let Some(ed) = self.active_editor().cloned() {
            ed.update(cx, |editor, cx| editor.set_cursor_to_position(symbol.line, 0, cx));
        }
        cx.notify();
    }

    pub(crate) fn palette_symbol_matches(&self, cx: &Context<Self>) -> Vec<(String, &'static str, u32)> {
        let Some(ed) = self.active_editor() else {
            return Vec::new();
        };
        let raw = self.palette_input.read(cx).value().to_string();
        let query = raw
            .trim_start()
            .strip_prefix('@')
            .unwrap_or(&raw)
            .trim()
            .to_lowercase();
        ed.read(cx)
            .document_symbols()
            .into_iter()
            .filter(|(name, _, _)| query.is_empty() || contains_ascii_ci(name, &query))
            .take(40)
            .collect()
    }

    pub(crate) fn open_symbol_outline(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_editor().is_none() {
            return;
        }
        self.palette_open = true;
        self.palette_selected = 0;
        self.palette_input.update(cx, |s, cx| {
            s.set_value("@ ", window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn palette_command_matches(&self, cx: &Context<Self>) -> Vec<IdeCommand> {
        let raw = self.palette_input.read(cx).value().to_string();
        let query = raw
            .trim_start()
            .strip_prefix('>')
            .unwrap_or(&raw)
            .trim()
            .to_lowercase();
        let is_flutter = self.is_flutter_or_dart_workspace();
        IDE_COMMANDS
            .iter()
            .filter(|cmd| !cmd.flutter_only || is_flutter)
            .filter(|cmd| query.is_empty() || contains_ascii_ci(cmd.title, &query))
            .copied()
            .collect()
    }

    pub(crate) fn palette_matches(&self, cx: &Context<Self>) -> Vec<PathBuf> {
        let query = self.palette_input.read(cx).value().to_lowercase();
        self.all_files
            .iter()
            .filter(|path| {
                query.is_empty()
                    || path
                        .to_str()
                        .is_some_and(|s| contains_ascii_ci(s, &query))
            })
            .take(20)
            .cloned()
            .collect()
    }

    pub(crate) fn open_palette_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette_is_workspace_symbol_mode(cx) {
            let syms = self.palette_workspace_symbol_matches(cx);
            if let Some(symbol) = syms.get(self.palette_selected).cloned() {
                self.open_workspace_symbol_at(symbol, window, cx);
            }
            return;
        }
        if self.palette_is_symbol_mode(cx) {
            let syms = self.palette_symbol_matches(cx);
            if let Some((_, _, line_0)) = syms.get(self.palette_selected).cloned() {
                self.palette_open = false;
                if let Some(ed) = self.active_editor().cloned() {
                    ed.update(cx, |editor, cx| {
                        editor.set_cursor_to_position(line_0, 0, cx);
                    });
                    let fh = ed.read(cx).focus_handle(cx);
                    window.focus(&fh, cx);
                }
                cx.notify();
            }
            return;
        }
        if self.palette_is_command_mode(cx) {
            let cmds = self.palette_command_matches(cx);
            if let Some(cmd) = cmds.get(self.palette_selected).copied() {
                self.palette_open = false;
                self.execute_ide_command(cmd.id, window, cx);
            }
            return;
        }
        let matches = self.palette_matches(cx);
        let Some(path) = matches.get(self.palette_selected).cloned() else {
            return;
        };
        self.open_path(path, window, cx);
        self.palette_open = false;
        cx.notify();
    }

    pub(crate) fn execute_ide_command(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match id {
            "run-flutter" => self.start_flutter_run(window, cx),
            "hot-reload" => self.trigger_hot_reload(cx),
            "hot-restart" => self.trigger_hot_restart(cx),
            "pub-get" => self.run_pub_get(cx),
            "pub-upgrade" => {
                self.terminal_open = true;
                self.dock_tab = DockTab::Terminal;
                self.execute_terminal_command("flutter pub upgrade".to_string(), cx);
            }
            "flutter-clean" => {
                self.terminal_open = true;
                self.dock_tab = DockTab::Terminal;
                self.execute_terminal_command(
                    "flutter clean && flutter pub get".to_string(),
                    cx,
                );
            }
            "build-runner" => {
                self.terminal_open = true;
                self.dock_tab = DockTab::Terminal;
                self.execute_terminal_command(
                    "dart run build_runner build --delete-conflicting-outputs".to_string(),
                    cx,
                );
            }
            "build-runner-watch" => {
                self.terminal_open = true;
                self.dock_tab = DockTab::Terminal;
                self.execute_terminal_command(
                    "dart run build_runner watch --delete-conflicting-outputs".to_string(),
                    cx,
                );
            }
            "gen-l10n" => {
                self.terminal_open = true;
                self.dock_tab = DockTab::Terminal;
                self.execute_terminal_command("flutter gen-l10n".to_string(), cx);
            }
            "flutter-doctor" => {
                self.terminal_open = true;
                self.dock_tab = DockTab::Terminal;
                self.execute_terminal_command("flutter doctor -v".to_string(), cx);
            }
            "open-devtools" => self.open_flutter_devtools(cx),
            "toggle-perf-overlay" => self.toggle_flutter_vm_ext(
                "ext.flutter.showPerformanceOverlay",
                "Performance Overlay",
                cx,
            ),
            "toggle-debug-paint" => self.toggle_flutter_vm_ext(
                "ext.flutter.debugPaint",
                "Debug Paint (Widget Bounds)",
                cx,
            ),
            "toggle-slow-animations" => self.toggle_flutter_vm_ext(
                "ext.flutter.timeDilation",
                "Slow Animations",
                cx,
            ),
            "add-package" => self.open_add_package(window, cx),
            "debug-dap" => self.start_dap_debug(window, cx),
            "toggle-sim" => {
                if self.sim.attached {
                    self.detach_simulator(cx);
                } else {
                    self.attach_simulator(window, cx);
                }
            }
            "open-git" => {
                self.sidebar_open = true;
                self.set_sidebar_tab(git_panel::SidebarTab::Git, cx);
            }
            "git-switch-branch" => self.git_open_branch_switcher(window, cx),
            "git-stage-all" => self.git_stage_all(window, cx),
            "git-unstage-all" => self.git_unstage_all(window, cx),
            "git-stash" => self.git_stash_save(window, cx),
            "git-uncommit" => self.git_uncommit_last(window, cx),
            "diff-unified" => self.toggle_diff_unified(cx),
            "project-search" => {
                self.sidebar_open = true;
                self.set_sidebar_tab(git_panel::SidebarTab::Search, cx);
                self.project_search_input
                    .update(cx, |s, cx| s.focus(window, cx));
            }
            "toggle-problems" => {
                if self.terminal_open && self.dock_tab == DockTab::Problems {
                    self.terminal_open = false;
                } else {
                    self.terminal_open = true;
                    self.dock_tab = DockTab::Problems;
                }
            }
            "toggle-terminal" => self.terminal_open = !self.terminal_open,
            "toggle-sidebar" => self.sidebar_open = !self.sidebar_open,
            "toggle-web-preview" => self.toggle_web_preview(window, cx),
            "toggle-agent" => self.agent_open = !self.agent_open,
            "format-doc" => {
                if let Some(ed) = self.active_editor().cloned() {
                    ed.update(cx, |editor, _cx| {
                        editor.request_format();
                    });
                }
            }
            "conflict-current" | "conflict-incoming" | "conflict-both" => {
                let choice = match id {
                    "conflict-current" => ConflictChoice::Current,
                    "conflict-incoming" => ConflictChoice::Incoming,
                    _ => ConflictChoice::Both,
                };
                if let Some(ed) = self.active_editor().cloned() {
                    ed.update(cx, |editor, cx| {
                        editor.resolve_conflict_at_cursor(choice, cx);
                    });
                }
            }
            "debug-add-watch" => self.open_debug_prompt(debug::DebugPrompt::Watch, window, cx),
            "debug-clear-watches" => {
                self.dap_watches.clear();
                self.dap_watch_values.clear();
                cx.notify();
            }
            "debug-breakpoint-condition" => self.open_breakpoint_condition_prompt(window, cx),
            "local-history" => self.open_local_history(cx),
            "organize-imports" => {
                if let Some(ed) = self.active_editor().cloned() {
                    ed.update(cx, |editor, _cx| {
                        editor.request_organize_imports();
                    });
                }
            }
            "find-references" => {
                if let Some(ed) = self.active_editor().cloned() {
                    ed.update(cx, |editor, _cx| editor.request_references());
                }
            }
            "goto-symbol" => self.open_symbol_outline(window, cx),
            "goto-workspace-symbol" => self.open_workspace_symbols(window, cx),
            "goto-line" => self.open_go_to_line(window, cx),
            "new-file" => self.new_scratch_tab(window, cx),
            "open-folder" => self.open_folder(window, cx),
            "close-folder" => self.close_workspace(cx),
            "open-settings" => self.toggle_settings(cx),
            "about-shame" => {
                self.about_open = true;
            }
            _ => {}
        }
        cx.notify();
    }

    pub(crate) fn render_palette(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> Option<Div> {
        if !self.palette_open {
            return None;
        }

        let is_ws_mode = self.palette_is_workspace_symbol_mode(cx);
        let ws_matches = if is_ws_mode {
            self.palette_workspace_symbol_matches(cx)
        } else {
            Vec::new()
        };
        let is_sym_mode = !is_ws_mode && self.palette_is_symbol_mode(cx);
        let is_cmd_mode = !is_ws_mode && !is_sym_mode && self.palette_is_command_mode(cx);
        let sym_matches = if is_sym_mode {
            self.palette_symbol_matches(cx)
        } else {
            Vec::new()
        };
        let cmd_matches = if is_cmd_mode {
            self.palette_command_matches(cx)
        } else {
            Vec::new()
        };
        let matches = if is_ws_mode || is_sym_mode || is_cmd_mode {
            Vec::new()
        } else {
            self.palette_matches(cx)
        };
        let total_items = if is_ws_mode {
            ws_matches.len()
        } else if is_sym_mode {
            sym_matches.len()
        } else if is_cmd_mode {
            cmd_matches.len()
        } else {
            matches.len()
        };
        let selected = self.palette_selected.min(total_items.saturating_sub(1));

        let rows: Vec<AnyElement> = if is_ws_mode {
            ws_matches
                .into_iter()
                .enumerate()
                .map(|(ix, symbol)| {
                    let is_selected = ix == selected;
                    let view = view.clone();
                    let rel = symbol
                        .path
                        .strip_prefix(&self.root)
                        .unwrap_or(&symbol.path)
                        .display()
                        .to_string();
                    let (name, kind) = (symbol.name.clone(), symbol.kind);
                    h_flex()
                        .id(("palette-wsym", ix))
                        .relative()
                        .w_full()
                        .h(px(34.))
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .px(px(14.))
                        .rounded(px(tokens::RADIUS_SM))
                        .cursor_pointer()
                        .when(is_selected, |el| el.bg(rgba(0xea7a3c16)))
                        .when(!is_selected, |el| el.hover(|s| s.bg(rgba(0xffffff08))))
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .min_w(px(0.))
                                .child(
                                    div()
                                        .px_1p5()
                                        .py_0p5()
                                        .rounded(px(4.))
                                        .bg(cx.theme().info.opacity(0.16))
                                        .font_family("SF Mono")
                                        .text_size(px(10.))
                                        .text_color(cx.theme().info)
                                        .child(kind),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .font_family("SF Mono")
                                        .text_size(px(tokens::UI))
                                        .text_color(cx.theme().foreground)
                                        .child(name),
                                ),
                        )
                        .child(
                            div()
                                .truncate()
                                .font_family("SF Mono")
                                .text_size(px(tokens::UI_XS))
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("{rel}:{}", symbol.line + 1)),
                        )
                        .on_click(move |_event, window, cx| {
                            let symbol = symbol.clone();
                            view.update(cx, |this, cx| this.open_workspace_symbol_at(symbol, window, cx));
                        })
                        .into_any_element()
                })
                .collect()
        } else if is_sym_mode {
            sym_matches
                .into_iter()
                .enumerate()
                .map(|(ix, (sym_name, kind_lbl, line_0))| {
                    let is_selected = ix == selected;
                    let view = view.clone();
                    h_flex()
                        .id(("palette-sym", ix))
                        .relative()
                        .w_full()
                        .h(px(34.))
                        .items_center()
                        .justify_between()
                        .px(px(14.))
                        .rounded(px(tokens::RADIUS_SM))
                        .border_1()
                        .border_color(gpui_kit::transparent_black())
                        .cursor_pointer()
                        .when(is_selected, |el| {
                            el.bg(rgba(0xea7a3c16))
                                .border_color(rgba(0xea7a3c36))
                                .child(
                                    div()
                                        .absolute()
                                        .left(px(2.))
                                        .top(px(6.))
                                        .bottom(px(6.))
                                        .w(px(2.5))
                                        .rounded(px(2.))
                                        .bg(cx.theme().primary),
                                )
                        })
                        .when(!is_selected, |el| el.hover(|s| s.bg(rgba(0xffffff08))))
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .min_w(px(0.))
                                .child(
                                    div()
                                        .px_1p5()
                                        .py_0p5()
                                        .rounded(px(4.))
                                        .bg(cx.theme().info.opacity(0.16))
                                        .font_family("SF Mono")
                                        .text_size(px(10.))
                                        .text_color(cx.theme().info)
                                        .child(kind_lbl),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .font_family("SF Mono")
                                        .text_size(px(tokens::UI))
                                        .text_color(cx.theme().foreground)
                                        .child(sym_name),
                                ),
                        )
                        .child(
                            div()
                                .font_family("SF Mono")
                                .text_size(px(tokens::UI_XS))
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(":{}", line_0 + 1)),
                        )
                        .on_click(move |_event, window, cx| {
                            view.update(cx, |this, cx| {
                                this.palette_open = false;
                                if let Some(ed) = this.active_editor().cloned() {
                                    ed.update(cx, |editor, cx| {
                                        editor.set_cursor_to_position(line_0, 0, cx);
                                    });
                                    let fh = ed.read(cx).focus_handle(cx);
                                    window.focus(&fh, cx);
                                }
                                cx.notify();
                            });
                        })
                        .into_any_element()
                })
                .collect()
        } else if is_cmd_mode {
            cmd_matches
                .into_iter()
                .enumerate()
                .map(|(ix, cmd)| {
                    let is_selected = ix == selected;
                    let cmd_id = cmd.id;
                    let view = view.clone();
                    h_flex()
                        .id(("palette-cmd", ix))
                        .relative()
                        .w_full()
                        .h(px(34.))
                        .items_center()
                        .justify_between()
                        .px(px(14.))
                        .rounded(px(tokens::RADIUS_SM))
                        .border_1()
                        .border_color(gpui_kit::transparent_black())
                        .cursor_pointer()
                        .when(is_selected, |el| {
                            el.bg(rgba(0xea7a3c16))
                                .border_color(rgba(0xea7a3c36))
                                .child(
                                    div()
                                        .absolute()
                                        .left(px(2.))
                                        .top(px(6.))
                                        .bottom(px(6.))
                                        .w(px(2.5))
                                        .rounded(px(2.))
                                        .bg(cx.theme().primary),
                                )
                        })
                        .when(!is_selected, |el| el.hover(|s| s.bg(rgba(0xffffff08))))
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .min_w(px(0.))
                                .child(
                                    gpui_kit::component::Icon::new(
                                        gpui_kit::assets::IconName::Sparkles,
                                    )
                                    .with_size(px(12.))
                                    .text_color(cx.theme().primary),
                                )
                                .child(
                                    div()
                                        .text_size(px(tokens::UI))
                                        .text_color(cx.theme().foreground)
                                        .child(cmd.title),
                                ),
                        )
                        .when(!cmd.shortcut.is_empty(), |el| {
                            el.child(
                                div()
                                    .px_1p5()
                                    .py_0p5()
                                    .rounded(px(4.))
                                    .bg(cx.theme().background)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .font_family("SF Mono")
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(cmd.shortcut),
                            )
                        })
                        .on_click(move |_event, window, cx| {
                            view.update(cx, |this, cx| {
                                this.palette_open = false;
                                this.execute_ide_command(cmd_id, window, cx);
                            });
                        })
                        .into_any_element()
                })
                .collect()
        } else {
            matches
                .into_iter()
                .enumerate()
                .map(|(ix, path)| {
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default()
                        .to_string();
                    let directory = path
                        .parent()
                        .and_then(|p| p.strip_prefix(&self.root).ok())
                        .map(|p| p.to_string_lossy().to_string())
                        .filter(|p| !p.is_empty())
                        .unwrap_or_else(|| ".".to_string());
                    let badge = self.git_status.get(&path).copied();
                    let row_path = path.clone();
                    let is_selected = ix == selected;

                    h_flex()
                        .id(("palette-item", ix))
                        .relative()
                        .w_full()
                        .h(px(34.))
                        .items_center()
                        .justify_between()
                        .px(px(14.))
                        .rounded(px(tokens::RADIUS_SM))
                        .border_1()
                        .border_color(gpui_kit::transparent_black())
                        .cursor_pointer()
                        .when(is_selected, |el| {
                            el.bg(rgba(0xea7a3c16))
                                .border_color(rgba(0xea7a3c36))
                                .child(
                                    div()
                                        .absolute()
                                        .left(px(2.))
                                        .top(px(6.))
                                        .bottom(px(6.))
                                        .w(px(2.5))
                                        .rounded(px(2.))
                                        .bg(cx.theme().primary),
                                )
                        })
                        .when(!is_selected, |el| el.hover(|s| s.bg(rgba(0xffffff08))))
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .min_w(px(0.))
                                .child(render_file_icon(&name))
                                .child(
                                    div()
                                        .text_size(px(tokens::UI))
                                        .text_color(cx.theme().foreground)
                                        .child(name),
                                ),
                        )
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_size(px(tokens::UI_SM))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(directory),
                                )
                                .when_some(badge, |el, letter| {
                                    el.child(
                                        div()
                                            .text_size(px(10.))
                                            .font_medium()
                                            .text_color(cx.theme().warning)
                                            .child(letter.to_string()),
                                    )
                                }),
                        )
                        .on_click({
                            let view = view.clone();
                            move |_event, window, cx| {
                                view.update(cx, |this, cx| {
                                    this.open_path(row_path.clone(), window, cx);
                                    this.palette_open = false;
                                    cx.notify();
                                });
                            }
                        })
                        .into_any_element()
                })
                .collect()
        };

        Some(
            div()
                .occlude()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(80.))
                .bg(rgba(0x00000066))
                .child(
                    v_flex()
                        .w(px(640.))
                        .max_h(px(460.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .overflow_hidden()
                        .child(
                            h_flex()
                                .h(px(48.))
                                .px(px(16.))
                                .gap_2()
                                .items_center()
                                .border_b_1()
                                .border_color(cx.theme().border)
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .child(
                                            Input::new(&self.palette_input).appearance(false),
                                        ),
                                ),
                        )
                        .child(
                            v_flex()
                                .id("palette-list")
                                .p_2()
                                .gap(px(tokens::SPACE_1))
                                .overflow_y_scroll()
                                .children(rows),
                        )
                        .child(
                            h_flex()
                                .px_3()
                                .py_1p5()
                                .gap_3()
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .text_size(px(tokens::UI_XS))
                                .text_color(cx.theme().muted_foreground)
                                .child("\u{2191}\u{2193} Navigate")
                                .child("\u{21b5} Execute / Open")
                                .child("> Commands (\u{2318}\u{21e7}P)")
                                .child("Esc Dismiss"),
                        ),
                ),
        )
    }
}
