//! Workspace lifecycle: open/close folders, file tree, session persistence.

use super::*;

impl DartIde {
    /// Background check: if files were added, renamed, or deleted on disk outside the editor,
    /// refresh `all_files` and `tree_state` automatically without cloning `all_files` on the UI thread.
    pub(crate) fn sync_workspace_files_if_changed(&mut self, cx: &mut Context<Self>) {
        if !self.has_workspace || self.syncing_files {
            return;
        }
        self.syncing_files = true;
        let root = self.root.clone();
        let prev_hash = self.all_files_hash;
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let files = flatten_files(&root);
            let h = hash_paths(&files);
            if h != prev_hash {
                let _ = tx.send_blocking(Some((files, h)));
            } else {
                let _ = tx.send_blocking(None);
            }
        });
        cx.spawn(async move |weak, cx| {
            let res = rx.recv().await;
            let _ = weak.update(cx, |this, cx| {
                this.syncing_files = false;
                if let Ok(Some((files, h))) = res {
                    let items = build_tree_items(&this.root, this.show_hidden);
                    this.all_files = files;
                    this.all_files_hash = h;
                    this.tree_state.update(cx, |state, cx| {
                        state.set_items(items, cx);
                    });
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// True when the open workspace is a Dart / Flutter project (`pubspec.yaml` at root,
    /// `.vscode/launch.json` present, or an open `.dart` buffer).
    /// Non-Dart/Flutter projects hide the Flutter target picker, Simulator button, and
    /// Flutter-only palette commands.
    pub(crate) fn is_flutter_or_dart_workspace(&self) -> bool {
        (self.has_workspace
            && (self.root.join("pubspec.yaml").is_file() || !self.launch_configs.is_empty()))
            || self.tabs.iter().any(|t| {
                t.path
                    .as_ref()
                    .and_then(|p| p.extension())
                    .and_then(|e| e.to_str())
                    == Some("dart")
            })
    }

    pub(crate) fn open_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Folder".into()),
        });

        let view = cx.entity();
        cx.spawn_in(window, async move |_, window| {
            let root = paths.await.ok()?.ok()??.into_iter().next()?;
            window
                .update(|_window, cx| {
                    view.update(cx, |this, cx| this.set_root(root, cx));
                })
                .ok()
        })
        .detach();
    }

    pub(crate) fn set_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        let _ = std::env::set_current_dir(&root);
        self.has_workspace = true;
        self.sidebar_open = true;
        self.terminal_open = true;
        self.recent_workspaces.retain(|r| r != &root);
        self.recent_workspaces.insert(0, root.clone());
        self.recent_workspaces.truncate(10);
        save_workspace_store(Some(&root), &self.recent_workspaces);

        // Stop any running Flutter daemon from the previous project
        self.stop_flutter_run(cx);

        // Close tabs and overlays from the previous project workspace
        self.tabs.clear();
        self.active_tab = 0;
        self.split_tab = None;
        self.tab_overflow_open = false;
        self.device_picker_open = false;
        self.launch_picker_open = false;
        self.add_package_open = false;
        self.palette_open = false;
        self.find_open = false;
        self.go_to_line_open = false;
        self.rename_target = None;
        self.peek = None;

        // Kill any running terminal child processes from the old project and
        // start a fresh terminal session rooted at the new workspace
        for session in &mut self.terminals {
            if let Some(pid) = session.running_pid.take() {
                crate::editor::dock::signal_process_group(pid, "-TERM");
            }
        }
        let id = self.next_terminal_id;
        self.next_terminal_id += 1;
        self.terminals = vec![editor::dock::TerminalSession::new(id, root.clone(), true)];
        self.active_terminal = 0;
        self.debug_lines.clear();
        self.dock_tab = DockTab::Terminal;

        let items = build_tree_items(&root, self.show_hidden);
        self.all_files = flatten_files(&root);
        self.all_files_hash = hash_paths(&self.all_files);
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
        });
        self.root = root;
        // A different folder is a different repository.
        self.git.status = None;
        self.git.commit_types = None;
        self.git_branch = None;
        self.git_status.clear();
        self.diff = None;
        self.refresh_git(cx);
        self.reload_launch_configs(cx);
        if self.is_flutter_or_dart_workspace() {
            self.refresh_devices(cx);
        }
        cx.notify();
    }

    pub(crate) fn close_workspace(&mut self, cx: &mut Context<Self>) {
        self.stop_flutter_run(cx);
        self.stop_dap_debug(cx);
        self.has_workspace = false;
        self.sidebar_open = false;
        self.terminal_open = false;
        self.tabs.clear();
        self.active_tab = 0;
        self.split_tab = None;
        self.diff = None;
        self.all_files.clear();
        self.all_files_hash = 0;
        self.git.status = None;
        self.git.stats.clear();
        self.git.branches.clear();
        self.git.branches_open = false;
        self.git.expanded_commit = None;
        self.git.expanded_commit_files.clear();
        self.git.notice = None;
        self.git.commit_types = None;
        self.git_branch = None;
        self.git_status.clear();
        self.sidebar_tab = crate::git_panel::SidebarTab::Files;
        self.launch_configs.clear();
        self.test_launch_templates.clear();
        self.tree_state.update(cx, |state, cx| {
            state.set_items(Vec::new(), cx);
        });
        save_workspace_store(None, &self.recent_workspaces);
        cx.notify();
    }

    pub(crate) fn remove_recent_workspace(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.recent_workspaces.retain(|p| p != &path);
        let cur = self.has_workspace.then_some(self.root.as_path());
        save_workspace_store(cur, &self.recent_workspaces);
        cx.notify();
    }

    pub(crate) fn open_file_tree_prompt(
        &mut self,
        op: FileTreeOp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let initial = match &op {
            FileTreeOp::Rename(path) => path
                .strip_prefix(&self.root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string(),
            _ => String::new(),
        };
        self.file_tree_op = Some(op);
        self.file_tree_input.update(cx, |s, cx| {
            s.set_value(initial, window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn submit_file_tree_prompt(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(op) = self.file_tree_op.take() else {
            return;
        };
        let raw = self.file_tree_input.read(cx).value().trim().to_string();
        if raw.is_empty() {
            cx.notify();
            return;
        }
        let target = self.root.join(&raw);
        match op {
            FileTreeOp::NewFile => {
                if let Some(parent) = target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if !target.exists() {
                    let _ = std::fs::write(&target, "");
                }
                self.rebuild_file_tree(cx);
                self.open_path(target, window, cx);
            }
            FileTreeOp::NewFolder => {
                let _ = std::fs::create_dir_all(&target);
                self.rebuild_file_tree(cx);
            }
            FileTreeOp::Rename(old_path) => {
                if let Some(parent) = target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::rename(&old_path, &target).is_ok() {
                    self.rebuild_file_tree(cx);
                }
            }
        }
        self.refresh_git(cx);
        cx.notify();
    }

    pub(crate) fn delete_tree_path_clicked(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let key = format!("tree-del:{}", path.to_string_lossy());
        if self.git.confirm_discard.as_deref() == Some(key.as_str()) {
            self.git.confirm_discard = None;
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(&path);
            } else {
                let _ = std::fs::remove_file(&path);
            }
            if let Some(ix) = self
                .tabs
                .iter()
                .position(|t| t.path.as_ref() == Some(&path))
            {
                self.close_tab(ix, cx);
            }
            self.rebuild_file_tree(cx);
            self.refresh_git(cx);
            cx.notify();
            return;
        }
        self.arm_confirm(key, cx);
    }

    pub(crate) fn rebuild_file_tree(&mut self, cx: &mut Context<Self>) {
        let items = build_tree_items(&self.root, self.show_hidden);
        let files = flatten_files(&self.root);
        self.all_files_hash = hash_paths(&files);
        self.all_files = files;
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
        });
    }

    pub(crate) fn save_session_state(&self) {
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let path = Path::new(&home).join(".config/shame/session.json");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let open_tabs: Vec<String> = self
            .tabs
            .iter()
            .filter_map(|t| t.path.as_ref())
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let harness_str = match self.agent.harness {
            rails::agent::AgentHarness::ClaudeCode => "claude",
            rails::agent::AgentHarness::OpenCode => "opencode",
            rails::agent::AgentHarness::Codex => "codex",
        };
        let val = serde_json::json!({
            "workspace": self.has_workspace.then(|| self.root.to_string_lossy().to_string()),
            "open_tabs": open_tabs,
            "active_tab": self.active_tab,
            "split_tab": self.split_tab,
            "sidebar_open": self.sidebar_open,
            "terminal_open": self.terminal_open,
            "agent_open": self.agent_open,
            "show_hidden": self.show_hidden,
            "light_theme": self.light_theme,
            "unified_diff": self.git.unified_diff,
            "agent_harness": harness_str,
            // By name, so adding or reordering modes never changes a saved choice.
            "agent_mode": crate::agent_ctx::AGENT_MODES
                [self.agent.mode_ix % crate::agent_ctx::AGENT_MODES.len()]
            .id
            .key(),
            "autosave": self.autosave.key(),
            "format_on_save": self.save_options.format_on_save,
            "trim_trailing_whitespace": self.save_options.trim_trailing_whitespace,
            "insert_final_newline": self.save_options.insert_final_newline,
        });
        if let Ok(json) = serde_json::to_vec_pretty(&val) {
            let _ = std::fs::write(path, json);
        }
    }

    pub(crate) fn restore_preferences_only(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let path = Path::new(&home).join(".config/shame/session.json");
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let Ok(val) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return;
        };
        self.apply_saved_preferences(&val, window, cx);
    }

    pub(crate) fn apply_saved_preferences(
        &mut self,
        val: &serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(sh) = val.get("show_hidden").and_then(|v| v.as_bool()) {
            self.show_hidden = sh;
        }
        if let Some(ud) = val.get("unified_diff").and_then(|v| v.as_bool()) {
            self.git.unified_diff = ud;
        }
        if let Some(mode) = val
            .get("agent_mode")
            .and_then(|v| v.as_str())
            .and_then(crate::agent_ctx::ModeId::from_key)
        {
            self.agent.mode_ix = mode.index();
        }
        if let Some(key) = val.get("autosave").and_then(|v| v.as_str()) {
            self.autosave = crate::file_state::AutosaveMode::from_key(key);
        }
        if let Some(v) = val.get("format_on_save").and_then(|v| v.as_bool()) {
            self.save_options.format_on_save = v;
        }
        if let Some(v) = val.get("trim_trailing_whitespace").and_then(|v| v.as_bool()) {
            self.save_options.trim_trailing_whitespace = v;
        }
        if let Some(v) = val.get("insert_final_newline").and_then(|v| v.as_bool()) {
            self.save_options.insert_final_newline = v;
        }
        self.apply_save_options(cx);
        if let Some(h) = val.get("agent_harness").and_then(|v| v.as_str()) {
            let tool = match h {
                "opencode" => rails::agent::AgentHarness::OpenCode,
                "codex" => rails::agent::AgentHarness::Codex,
                _ => rails::agent::AgentHarness::ClaudeCode,
            };
            self.agent.set_harness(tool);
        }
        if let Some(lt) = val.get("light_theme").and_then(|v| v.as_bool()) {
            self.light_theme = lt;
            if lt {
                gpui_kit::component::Theme::change(
                    gpui_kit::component::ThemeMode::Light,
                    Some(window),
                    cx,
                );
            }
        }
    }

    pub(crate) fn restore_session_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let path = Path::new(&home).join(".config/shame/session.json");
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let Ok(val) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return;
        };
        self.apply_saved_preferences(&val, window, cx);

        let saved_ws = val
            .get("workspace")
            .and_then(|v| v.as_str())
            .map(PathBuf::from);
        if saved_ws.as_ref() != Some(&self.root) {
            return;
        }
        if let Some(sb) = val.get("sidebar_open").and_then(|v| v.as_bool()) {
            self.sidebar_open = sb;
        }
        if let Some(tb) = val.get("terminal_open").and_then(|v| v.as_bool()) {
            self.terminal_open = tb;
        }
        if let Some(ab) = val.get("agent_open").and_then(|v| v.as_bool()) {
            self.agent_open = ab;
        }
        if let Some(arr) = val.get("open_tabs").and_then(|v| v.as_array()) {
            for item in arr.iter().take(12) {
                if let Some(p_str) = item.as_str() {
                    let p = PathBuf::from(p_str);
                    if p.is_file() {
                        self.open_path_with_mode(p, false, window, cx);
                    }
                }
            }
        }
        if let Some(act) = val.get("active_tab").and_then(|v| v.as_u64())
            && !self.tabs.is_empty()
        {
            self.active_tab = (act as usize).min(self.tabs.len() - 1);
        }
        if let Some(sp) = val.get("split_tab").and_then(|v| v.as_u64()) {
            let sp_ix = sp as usize;
            if sp_ix < self.tabs.len() {
                self.split_tab = Some(sp_ix);
            }
        }
    }
}
