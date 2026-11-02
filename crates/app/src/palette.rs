//! Command palette, Quick Open, symbol outline and project search.

use std::ops::Range;

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;

use super::*;

impl DartIde {
    /// Wait for a pause in typing before scanning. Without this a fast typist
    /// starts a whole-workspace scan for every intermediate query.
    pub(crate) fn schedule_project_search(&mut self, query: String, cx: &mut Context<Self>) {
        self.project_search_matches.clear();
        self.project_search_truncated = false;
        self.project_search_literal.clear();
        self.project_search_error = None;
        self.project_search_collapsed.clear();
        self.project_search_dismissed.clear();
        if query.is_empty() {
            self.project_search_pending = false;
            cx.notify();
            return;
        }
        self.project_search_pending = true;
        cx.notify();
        cx.spawn(async move |weak, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(180))
                .await;
            let _ = weak.update(cx, |this, cx| {
                // A newer keystroke replaced this query while we waited.
                if this.project_search_input.read(cx).value().trim() == query {
                    this.run_project_search(query, cx);
                }
            });
        })
        .detach();
    }

    pub(crate) fn run_project_search(&mut self, query: String, cx: &mut Context<Self>) {
        if query.is_empty() {
            self.project_search_matches.clear();
            self.project_search_truncated = false;
            self.project_search_pending = false;
            cx.notify();
            return;
        }

        let root = self.root.clone();
        let index = self.search_index.clone();
        self.project_search_pending = true;
        self.project_search_matches.clear();
        self.project_search_truncated = false;
        self.project_search_literal.clear();
        cx.notify();

        let q = query.clone();
        let options = self.project_search_options;
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(core_workspace::search_workspace(
                index.as_deref(),
                &root,
                &q,
                options,
            ));
        });
        cx.spawn(async move |weak, cx| {
            let Ok(results) = rx.recv().await else { return };
            let _ = weak.update(cx, |this, cx| {
                if this.project_search_input.read(cx).value().trim() == query
                    && this.project_search_options == options
                {
                    this.project_search_error = results.error;
                    this.project_search_matches = results.matches;
                    this.project_search_truncated = results.truncated;
                    this.project_search_literal = results.query_text;
                    this.project_search_pending = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Flip one search option and search again.
    pub(crate) fn toggle_project_search_option(
        &mut self,
        flip: fn(&mut core_workspace::SearchOptions),
        cx: &mut Context<Self>,
    ) {
        flip(&mut self.project_search_options);
        let query = self.project_search_input.read(cx).value().trim().to_string();
        self.schedule_project_search(query, cx);
    }

    /// Remember a query that led somewhere, newest first.
    pub(crate) fn remember_project_search(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() {
            return;
        }
        self.project_recent_searches.retain(|q| q != query);
        self.project_recent_searches.insert(0, query.to_string());
        self.project_recent_searches.truncate(6);
    }

    /// Put `query` in the search box (from a recent search or a filter
    /// chip) and focus it.
    pub(crate) fn set_project_search_query(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.project_search_input.update(cx, |state, cx| {
            state.set_value(query.clone(), window, cx);
            state.focus(window, cx);
        });
        self.schedule_project_search(query.trim().to_string(), cx);
    }

    /// Replace matches on the listed result lines — every shown one when
    /// `only` is `None`, or just that `(file, line)` — then reload any open
    /// tabs whose files changed. Refuses while a tab has unsaved edits, so a
    /// replace can't clobber them.
    pub(crate) fn replace_in_project(&mut self, only: Option<(PathBuf, usize)>, cx: &mut Context<Self>) {
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
        let mut edits: Vec<(PathBuf, Vec<usize>)> = Vec::new();
        for m in &self.project_search_matches {
            let key = (m.path.clone(), m.line_number);
            let wanted = match &only {
                Some(only) => *only == key,
                None => !self.project_search_dismissed.contains(&key),
            };
            if !wanted {
                continue;
            }
            match edits.last_mut() {
                Some((path, lines)) if *path == m.path => lines.push(m.line_number),
                _ => edits.push((m.path.clone(), vec![m.line_number])),
            }
        }
        let text = core_workspace::split_query_filters(&query).0;
        match core_workspace::replace_in_lines(
            &edits,
            &text,
            &replacement,
            self.project_search_options,
        ) {
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
                self.remember_project_search(&query);
                self.project_replace_status = (only.is_none()).then(|| {
                    let mut status = format!(
                        "Replaced {total} in {} file{}",
                        changed.len(),
                        if changed.len() == 1 { "" } else { "s" },
                    );
                    if self.project_search_truncated {
                        status.push_str(" \u{b7} results were capped, run again for the rest");
                    }
                    status
                });
                self.schedule_project_search(query, cx);
            }
            Err(err) => self.project_replace_status = Some(err),
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
    /// Index the workspace's declarations in the background once (for `@symbol`
    /// mentions); does nothing if already indexed or in progress.
    pub(crate) fn ensure_symbol_index(&mut self, cx: &mut Context<Self>) {
        if !self.workspace_symbols.is_empty() || self.symbols_indexing {
            return;
        }
        self.symbols_indexing = true;
        let root = self.root.clone();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(core_workspace::index_workspace_symbols(&root));
        });
        cx.spawn(async move |weak, cx| {
            let Ok(symbols) = rx.recv().await else { return };
            let _ = weak.update(cx, |this, cx| {
                this.workspace_symbols = symbols;
                this.symbols_indexing = false;
                cx.notify();
            });
        })
        .detach();
    }

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

    /// The text after the `>` that puts the palette in command mode.
    pub(crate) fn palette_command_query(&self, cx: &Context<Self>) -> String {
        let raw = self.palette_input.read(cx).value().to_string();
        raw.trim_start()
            .strip_prefix('>')
            .unwrap_or(&raw)
            .trim()
            .to_lowercase()
    }

    /// Commands in the order the palette lists them. Every word of the query
    /// must appear in the title ("git stage" finds "Git: Stage All
    /// Changes"). With no query, recently run commands come first.
    pub(crate) fn palette_command_matches(&self, cx: &Context<Self>) -> Vec<IdeCommand> {
        let query = self.palette_command_query(cx);
        let words: Vec<&str> = query.split_whitespace().collect();
        let is_flutter = self.is_flutter_or_dart_workspace();
        let available = |cmd: &&IdeCommand| !cmd.flutter_only || is_flutter;
        if words.is_empty() {
            let recent: Vec<IdeCommand> = self
                .recent_commands
                .iter()
                .filter_map(|id| IDE_COMMANDS.iter().filter(available).find(|c| c.id == *id))
                .copied()
                .collect();
            let rest = IDE_COMMANDS
                .iter()
                .filter(available)
                .filter(|cmd| !self.recent_commands.contains(&cmd.id))
                .copied();
            return recent.into_iter().chain(rest).collect();
        }
        IDE_COMMANDS
            .iter()
            .filter(available)
            .filter(|cmd| words.iter().all(|word| contains_ascii_ci(cmd.title, word)))
            .copied()
            .collect()
    }

    /// How many leading entries of an empty-query command list are recent.
    pub(crate) fn palette_recent_count(&self, cx: &Context<Self>) -> usize {
        if !self.palette_command_query(cx).is_empty() {
            return 0;
        }
        let is_flutter = self.is_flutter_or_dart_workspace();
        self.recent_commands
            .iter()
            .filter(|id| {
                IDE_COMMANDS
                    .iter()
                    .any(|c| c.id == **id && (!c.flutter_only || is_flutter))
            })
            .count()
    }

    pub(crate) fn palette_matches(&self, cx: &Context<Self>) -> Vec<PathBuf> {
        let raw = self.palette_input.read(cx).value();
        // fff ranks by match quality, recency and query history; an empty
        // query opens on the files used most.
        let mut matches = self
            .search_index
            .as_ref()
            .and_then(|index| index.find_files(&raw, 20))
            .filter(|matches| !matches.is_empty());

        if raw.trim().is_empty() {
            return matches.unwrap_or_else(|| {
                self.all_files.iter().take(20).cloned().collect()
            });
        }

        // Files fff does not index — dot-directories in a git repo, anything
        // in a folder it skipped — are still reachable through the tree, so
        // append the ones whose path matches instead of losing them.
        let matches = matches.get_or_insert_with(Vec::new);
        let query = raw.to_lowercase();
        for path in &self.all_files {
            if matches.len() >= 20 {
                break;
            }
            if !matches.contains(path)
                && path
                    .to_str()
                    .is_some_and(|s| contains_ascii_ci(s, &query))
            {
                matches.push(path.clone());
            }
        }
        std::mem::take(matches)
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
        let query = self.palette_input.read(cx).value().to_string();
        self.open_path(path.clone(), window, cx);
        if let Some(index) = &self.search_index {
            index.note_query_pick(&query, &path);
        }
        self.palette_open = false;
        cx.notify();
    }

    pub(crate) fn execute_ide_command(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(cmd) = IDE_COMMANDS.iter().find(|c| c.id == id) {
            self.recent_commands.retain(|recent| *recent != cmd.id);
            self.recent_commands.insert(0, cmd.id);
            self.recent_commands.truncate(5);
        }
        match id {
            "test-all" => self.start_test_run(crate::test_runner::TestTarget::All, None, None, cx),
            "test-file" => self.run_tests_in_active_file(cx),
            "test-rerun-failed" => self.rerun_failed_tests(cx),
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
            "toggle-agent" => self.set_agent_open(!self.agent_open),
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
            "save-all" => self.save_all(cx),
            "open-keymap" => {
                let Some(path) = editor_view::keymap::Keymap::user_path() else {
                    return;
                };
                if !path.exists() {
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    let _ = std::fs::write(&path, editor_view::keymap::template());
                }
                self.open_path(path, window, cx);
            }
            "save-no-format" => {
                if let Some(ed) = self.active_editor().cloned() {
                    ed.update(cx, |editor, cx| editor.save_without_format(cx));
                }
            }
            "autosave-cycle" => {
                let next = self.autosave.next();
                self.set_autosave(next, cx);
            }
            "local-history" => self.open_local_history(cx),
            "organize-imports" => {
                if let Some(ed) = self.active_editor().cloned() {
                    ed.update(cx, |editor, _cx| {
                        editor.request_organize_imports();
                    });
                }
            }
            "refactor" => {
                if let Some(ed) = self.active_editor().cloned() {
                    ed.update(cx, |editor, _cx| {
                        editor.request_refactors();
                    });
                }
            }
            "peek-definition" => {
                if let Some(ed) = self.active_editor().cloned() {
                    ed.update(cx, |editor, _cx| editor.request_peek_definition());
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
            "close-folder" => self.request_close_workspace(cx),
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
            let query = self.palette_command_query(cx);
            let recent = self.palette_recent_count(cx);
            let mut rows = Vec::new();
            let mut last_section: Option<&str> = None;
            for (ix, cmd) in cmd_matches.into_iter().enumerate() {
                let (category, label) = cmd.title.split_once(": ").unwrap_or(("Other", cmd.title));
                let section = if ix < recent { "Recent" } else { category };
                if last_section != Some(section) {
                    rows.push(palette_section(section, rows.is_empty(), cx).into_any_element());
                    last_section = Some(section);
                }
                let cmd_id = cmd.id;
                let view = view.clone();
                // The keymap's binding wins; the static label stays for
                // descriptive ones like Hot Reload's "on ⌘S".
                let shortcut: SharedString = cx
                    .try_global::<editor_view::keymap::Keymap>()
                    .and_then(|k| k.label_for(cmd.id))
                    .map(SharedString::from)
                    .unwrap_or_else(|| SharedString::from(cmd.shortcut));
                // Recent rows keep their category, since there's no header
                // naming it.
                let label = if ix < recent { cmd.title } else { label };
                let highlights = word_highlights(label, &query, cx);
                rows.push(
                    palette_row(("palette-cmd", ix), ix == selected, cx)
                        .child(
                            Icon::new(category_icon(category))
                                .with_size(px(14.))
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .text_size(px(tokens::UI))
                                .text_color(cx.theme().foreground)
                                .child(StyledText::new(label).with_highlights(highlights)),
                        )
                        .when(!shortcut.is_empty(), |el| el.child(key_hint(shortcut, cx)))
                        .on_click(move |_event, window, cx| {
                            view.update(cx, |this, cx| {
                                this.palette_open = false;
                                this.execute_ide_command(cmd_id, window, cx);
                            });
                        })
                        .into_any_element(),
                );
            }
            if rows.is_empty() {
                rows.push(palette_empty("No matching commands", cx).into_any_element());
            }
            rows
        } else {
            let query = self.palette_input.read(cx).value().trim().to_string();
            let mut rows = Vec::new();
            if query.is_empty() && !matches.is_empty() {
                rows.push(palette_section("Recently opened", true, cx).into_any_element());
            }
            for (ix, path) in matches.into_iter().enumerate() {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string();
                let directory = path
                    .parent()
                    .and_then(|p| p.strip_prefix(&self.root).ok())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                let badge = self.git_status.get(&path).copied();
                let highlights = fuzzy_highlights(&name, &query, cx);
                let row_path = path.clone();
                let view = view.clone();
                rows.push(
                    palette_row(("palette-item", ix), ix == selected, cx)
                        .child(render_file_icon(&name))
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w(px(0.))
                                .items_baseline()
                                .gap(px(tokens::SPACE_4))
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(tokens::UI))
                                        .text_color(cx.theme().foreground)
                                        .child(StyledText::new(name).with_highlights(highlights)),
                                )
                                .child(
                                    div()
                                        .min_w(px(0.))
                                        .truncate()
                                        .text_size(px(tokens::UI_SM))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(directory),
                                ),
                        )
                        .when_some(badge, |el, letter| {
                            el.child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(px(10.))
                                    .font_medium()
                                    .text_color(cx.theme().warning)
                                    .child(letter.to_string()),
                            )
                        })
                        .on_click(move |_event, window, cx| {
                            view.update(cx, |this, cx| {
                                this.open_path(row_path.clone(), window, cx);
                                this.palette_open = false;
                                cx.notify();
                            });
                        })
                        .into_any_element(),
                );
            }
            if rows.is_empty() {
                rows.push(palette_empty("No matching files", cx).into_any_element());
            }
            rows
        };

        let (mode_icon, mode) = if is_ws_mode {
            (IconName::Hash, PaletteMode::Workspace)
        } else if is_sym_mode {
            (IconName::AtSign, PaletteMode::Symbols)
        } else if is_cmd_mode {
            (IconName::Command, PaletteMode::Commands)
        } else {
            (IconName::Search, PaletteMode::Files)
        };

        Some(
            div()
                .occlude()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                // Without this the card stretches to its max height and a
                // short list leaves a void under the footer.
                .items_start()
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
                                .flex_shrink_0()
                                .px(px(16.))
                                .gap(px(10.))
                                .items_center()
                                .border_b_1()
                                .border_color(cx.theme().border)
                                .child(
                                    Icon::new(mode_icon)
                                        .with_size(px(15.))
                                        .text_color(cx.theme().muted_foreground),
                                )
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
                                // Only the list gives way when the card hits
                                // its max height; header and footer keep
                                // their size.
                                .flex_1()
                                .min_h(px(0.))
                                .px(px(6.))
                                .pb(px(6.))
                                .gap(px(1.))
                                .overflow_y_scroll()
                                .children(rows),
                        )
                        .child(
                            h_flex()
                                .h(px(40.))
                                .flex_shrink_0()
                                .px(px(12.))
                                .gap(px(6.))
                                .items_center()
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .text_size(px(tokens::UI_XS))
                                .text_color(cx.theme().muted_foreground)
                                .children(PaletteMode::ALL.iter().map(|m| mode_chip(*m, *m == mode, cx)))
                                .child(div().flex_1())
                                .child(key_hint("\u{2191}\u{2193}".into(), cx))
                                .child(div().mr(px(6.)).child("move"))
                                .child(key_hint("\u{21b5}".into(), cx))
                                .child(div().mr(px(6.)).child("open"))
                                .child(key_hint("esc".into(), cx))
                                .child("close"),
                        ),
                ),
        )
    }
}

/// The palette's modes, by the prefix that selects each.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PaletteMode {
    Files,
    Commands,
    Symbols,
    Workspace,
}

impl PaletteMode {
    const ALL: [PaletteMode; 4] = [Self::Files, Self::Commands, Self::Symbols, Self::Workspace];

    fn label(self) -> (&'static str, &'static str) {
        match self {
            Self::Files => ("", "Files"),
            Self::Commands => (">", "Commands"),
            Self::Symbols => ("@", "Symbols"),
            Self::Workspace => ("#", "Workspace"),
        }
    }
}

/// A footer chip naming a mode and the prefix that opens it; the current
/// mode is lit.
fn mode_chip(mode: PaletteMode, active: bool, cx: &App) -> Div {
    let (prefix, label) = mode.label();
    h_flex()
        .h(px(22.))
        .px(px(8.))
        .gap(px(4.))
        .items_center()
        .rounded(px(tokens::RADIUS_BADGE))
        .when(active, |el| el.bg(cx.theme().list_hover).text_color(cx.theme().foreground))
        .when(!prefix.is_empty(), |el| {
            el.child(
                div()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_color(cx.theme().primary)
                    .child(prefix),
            )
        })
        .child(label)
}

/// A small keycap.
fn key_hint(text: SharedString, cx: &App) -> Div {
    div()
        .flex_shrink_0()
        .px(px(5.))
        .h(px(18.))
        .flex()
        .items_center()
        .rounded(px(tokens::RADIUS_BADGE))
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().background)
        .font_family(cx.theme().mono_font_family.clone())
        .text_size(px(tokens::UI_XS))
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// One selectable palette row; callers add icon, text and trailing bits.
fn palette_row(id: (&'static str, usize), selected: bool, cx: &App) -> Stateful<Div> {
    let hover = cx.theme().list_hover;
    h_flex()
        .id(id)
        .w_full()
        .h(px(32.))
        .flex_shrink_0()
        .px(px(10.))
        .gap(px(10.))
        .items_center()
        .rounded(px(tokens::RADIUS_SM))
        .cursor_pointer()
        .when(selected, |el| el.bg(tokens::accent_dim()))
        .when(!selected, move |el| el.hover(move |s| s.bg(hover)))
}

fn palette_section(label: &'static str, first: bool, cx: &App) -> Div {
    div()
        .px(px(10.))
        .pt(px(if first { 8. } else { 12. }))
        .pb(px(4.))
        .text_size(px(tokens::UI_XS))
        .font_medium()
        .text_color(cx.theme().muted_foreground)
        .child(label)
}

fn palette_empty(text: &'static str, cx: &App) -> Div {
    div()
        .px(px(10.))
        .py(px(18.))
        .text_size(px(tokens::UI_SM))
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

fn category_icon(category: &str) -> IconName {
    match category {
        "Git" => IconName::GitBranch,
        "Diff" => IconName::GitCompare,
        "View" => IconName::PanelsTopLeft,
        "Search" => IconName::Search,
        "Editor" => IconName::Code,
        "File" => IconName::File,
        "Flutter" => IconName::Zap,
        "Test" => IconName::FlaskConical,
        "Debug" => IconName::Bug,
        "Simulator" => IconName::Smartphone,
        "Preferences" => IconName::Settings,
        "Help" => IconName::Info,
        _ => IconName::Command,
    }
}

fn match_style(cx: &App) -> HighlightStyle {
    HighlightStyle {
        color: Some(cx.theme().primary),
        font_weight: Some(FontWeight::MEDIUM),
        ..Default::default()
    }
}

/// Each query word's first case-insensitive occurrence in `text`, merged
/// and sorted for `StyledText`.
fn word_highlights(text: &str, query: &str, cx: &App) -> Vec<(Range<usize>, HighlightStyle)> {
    let lower = text.to_lowercase();
    if lower.len() != text.len() {
        return Vec::new();
    }
    let mut ranges: Vec<Range<usize>> = query
        .split_whitespace()
        .filter_map(|word| lower.find(&word.to_lowercase()).map(|at| at..at + word.len()))
        .collect();
    merge_ranges(&mut ranges);
    let style = match_style(cx);
    ranges.into_iter().map(|r| (r, style)).collect()
}

/// The query's characters picked out of `text` in order (the way fuzzy file
/// matching reads), or nothing when they don't all appear.
fn fuzzy_highlights(text: &str, query: &str, cx: &App) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut chars = text.char_indices();
    for q in query.chars().filter(|c| !c.is_whitespace()) {
        let found = chars.by_ref().find(|(_, c)| c.eq_ignore_ascii_case(&q));
        match found {
            Some((at, c)) => ranges.push(at..at + c.len_utf8()),
            None => return Vec::new(),
        }
    }
    merge_ranges(&mut ranges);
    let style = match_style(cx);
    ranges.into_iter().map(|r| (r, style)).collect()
}

fn merge_ranges(ranges: &mut Vec<Range<usize>>) {
    ranges.sort_by_key(|r| r.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for r in ranges.drain(..) {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    *ranges = merged;
}
