//! Workspace lifecycle: open/close folders, file tree, session persistence.

use std::hash::{Hash, Hasher};

use super::*;

impl DartIde {
    /// One config directory per workspace root, for the search index's
    /// frecency databases. Losing it only costs warm ranking, never results.
    fn search_index_state_dir(root: &Path) -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        root.hash(&mut hasher);
        Some(
            Path::new(&home)
                .join(".config/shame/index")
                .join(format!("{:016x}", hasher.finish())),
        )
    }

    /// Start the warm search index for the open folder (or restart it after
    /// switching folders). fff walks the tree and watches it on its own
    /// threads; until it is ready `search_index` stays `None` and project
    /// search falls back to reading files from disk.
    pub(crate) fn start_search_index(&mut self, cx: &mut Context<Self>) {
        self.search_index = None;
        if !self.has_workspace {
            return;
        }
        let root = self.root.clone();
        let Some(state_dir) = Self::search_index_state_dir(&root) else {
            return;
        };
        let (tx, rx) = async_channel::bounded(1);
        let index_root = root.clone();
        std::thread::spawn(move || {
            let _ = tx.send_blocking(core_workspace::SearchIndex::open(&index_root, &state_dir));
        });
        cx.spawn(async move |weak, cx| {
            let Ok(index) = rx.recv().await else { return };
            let Some(index) = index else { return };
            let _ = weak.update(cx, |this, cx| {
                // A different folder may have been opened while this one was
                // being set up; never attach an index to the wrong root.
                if this.has_workspace && this.root == root {
                    this.search_index = Some(index);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Background check: if files were added, renamed, or deleted on disk outside the editor,
    /// refresh `all_files` and `tree_state` automatically without cloning `all_files` on the UI thread.
    pub(crate) fn sync_workspace_files_if_changed(&mut self, cx: &mut Context<Self>) {
        if !self.has_workspace || self.syncing_files {
            return;
        }
        self.syncing_files = true;
        let root = self.root.clone();
        let prev_hash = self.all_files_hash;
        let show_hidden = self.show_hidden;
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let files = flatten_files(&root);
            let h = hash_paths(&files);
            if h != prev_hash {
                let nodes = crate::rails::files::scan_tree(&root, show_hidden);
                let _ = tx.send_blocking(Some((files, h, nodes)));
            } else {
                let _ = tx.send_blocking(None);
            }
        });
        cx.spawn(async move |weak, cx| {
            let res = rx.recv().await;
            let _ = weak.update(cx, |this, cx| {
                this.syncing_files = false;
                if let Ok(Some((files, h, nodes))) = res {
                    this.all_files = files;
                    this.all_files_hash = h;
                    // A file changed on disk: keep the folders the user has open.
                    this.set_tree_items(crate::rails::files::to_tree_items(nodes), cx);
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

        // The agent thread belongs to the workspace being left: save it under
        // the old root, then load the new workspace's own thread below.
        self.save_agent_conversation();
        self.stop_agent_run(cx);
        self.clear_agent_conversation(cx);

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

        // Hang up on the old project's shells (dropping a session does) and
        // start a fresh one rooted at the new workspace
        self.terminals.clear();
        let session = self.spawn_terminal(root.clone());
        self.terminals = vec![session];
        self.active_terminal = 0;
        self.debug_lines.clear();
        self.dock_tab = DockTab::Terminal;

        // Clear the old project's tree now; the new one is scanned off the
        // UI thread so opening a large folder doesn't freeze the window.
        self.all_files.clear();
        self.all_files_hash = 0;
        self.tree_state.update(cx, |state, cx| {
            state.set_items(Vec::new(), cx);
        });
        self.root = root;
        self.rebuild_file_tree(cx);
        self.restore_agent_conversation(cx);
        // A different folder is a different repository.
        self.git.status = None;
        self.git.commit_types = None;
        self.git_branch = None;
        self.git_status.clear();
        self.diff = None;
        self.refresh_git(cx);
        self.reload_launch_configs(cx);
        self.start_search_index(cx);
        if self.is_flutter_or_dart_workspace() {
            self.refresh_devices(cx);
        }
        cx.notify();
    }

    pub(crate) fn close_workspace(&mut self, cx: &mut Context<Self>) {
        self.stop_flutter_run(cx);
        self.stop_dap_debug(cx);
        // Keep the thread on disk so reopening the folder brings it back.
        self.save_agent_conversation();
        self.clear_agent_conversation(cx);
        self.has_workspace = false;
        self.sidebar_open = false;
        self.terminal_open = false;
        self.tabs.clear();
        self.active_tab = 0;
        self.split_tab = None;
        self.diff = None;
        self.all_files.clear();
        self.all_files_hash = 0;
        self.search_index = None;
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

    /// The folder a new file or folder goes into, as in VS Code: the selected
    /// folder, else the selected file's folder, else the workspace root.
    pub(crate) fn file_tree_target_dir(&self, cx: &App) -> PathBuf {
        let selected = self
            .tree_state
            .read(cx)
            .selected_item()
            .map(|item| PathBuf::from(item.id.as_str()))
            .filter(|p| p.starts_with(&self.root));
        match selected {
            // On disk, not `is_folder()`: GPUI counts an empty folder as a leaf.
            Some(p) if p.is_dir() => p,
            Some(p) => p.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone()),
            None => self.root.clone(),
        }
    }

    /// Start an inline create or rename in the file tree. New entries get an
    /// editable row at the top of their folder (opened if closed); a rename
    /// turns the row's label into an input with the name, minus its
    /// extension, selected.
    pub(crate) fn open_file_tree_prompt(
        &mut self,
        op: FileTreeOp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Only one inline edit at a time; drop a pending new-entry row.
        if self.file_tree_op.take().is_some() {
            self.remove_new_entry_row(cx);
        }
        self.file_tree_error = None;
        let (initial, stem_len) = match &op {
            FileTreeOp::Rename(path) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let stem = match name.rfind('.') {
                    Some(dot) if dot > 0 && !path.is_dir() => dot,
                    _ => name.len(),
                };
                let stem_len = name[..stem].chars().count();
                (name, stem_len)
            }
            FileTreeOp::NewFile { parent } | FileTreeOp::NewFolder { parent } => {
                self.insert_new_entry_row(parent.clone(), cx);
                (String::new(), 0)
            }
        };
        let row_id: SharedString = match &op {
            FileTreeOp::Rename(path) => path.to_string_lossy().to_string().into(),
            _ => crate::rails::files::NEW_ENTRY_ID.into(),
        };
        self.tree_state.update(cx, |state, cx| {
            state.reveal_item(&row_id, ScrollStrategy::Center, cx);
        });
        self.file_tree_op = Some(op);
        self.file_tree_input.update(cx, |s, cx| {
            s.set_value(initial, window, cx);
            s.focus(window, cx);
        });
        // Select the stem once the input is on screen (two frames: the next
        // one renders it, the one after can route the selection action).
        if stem_len > 0 {
            let input = self.file_tree_input.clone();
            window.on_next_frame(move |window, _cx| {
                window.on_next_frame(move |window, cx| {
                    input.update(cx, |s, cx| {
                        s.set_cursor_position(
                            gpui_kit::component::input::Position::new(0, stem_len as u32),
                            window,
                            cx,
                        );
                    });
                    window.dispatch_action(
                        Box::new(gpui_kit::component::input::SelectToStartOfLine),
                        cx,
                    );
                });
            });
        }
        cx.notify();
    }

    /// Put the editable "new entry" row at the top of `parent` in the tree.
    fn insert_new_entry_row(&mut self, parent: PathBuf, cx: &mut Context<Self>) {
        let placeholder = || {
            gpui_kit::component::tree::TreeItem::new(crate::rails::files::NEW_ENTRY_ID, "")
        };
        let parent_id = parent.to_string_lossy().to_string();
        // Edit the tree in memory: no disk scan, so the row appears at once.
        let current = crate::rails::files::root_items(self.tree_state.read(cx));
        let items = crate::rails::files::without_id(&current, crate::rails::files::NEW_ENTRY_ID);
        let items = if parent == self.root {
            std::iter::once(placeholder()).chain(items).collect()
        } else {
            crate::rails::files::with_child_first(&items, &parent_id, placeholder())
        };
        self.set_tree_items(items, cx);
    }

    fn remove_new_entry_row(&mut self, cx: &mut Context<Self>) {
        let current = crate::rails::files::root_items(self.tree_state.read(cx));
        let items = crate::rails::files::without_id(&current, crate::rails::files::NEW_ENTRY_ID);
        self.set_tree_items(items, cx);
    }

    /// Leave an inline create/rename without touching the disk.
    pub(crate) fn cancel_file_tree_prompt(&mut self, cx: &mut Context<Self>) {
        let Some(op) = self.file_tree_op.take() else {
            return;
        };
        self.file_tree_error = None;
        if !matches!(op, FileTreeOp::Rename(_)) {
            self.remove_new_entry_row(cx);
        }
        cx.notify();
    }

    /// Why `name` can't be used for the pending create/rename, if it can't.
    fn file_tree_name_error(&self, op: &FileTreeOp, name: &str) -> Option<String> {
        if name.is_empty() {
            return Some("A file or folder name must be provided.".into());
        }
        if name.starts_with('/') {
            return Some("A file or folder name cannot start with a slash.".into());
        }
        if name.contains('\0') || name.split('/').any(|seg| seg == "." || seg == "..") {
            return Some(format!(
                "The name {name} is not valid as a file or folder name. Please choose a different name."
            ));
        }
        let target = match op {
            FileTreeOp::NewFile { parent } | FileTreeOp::NewFolder { parent } => {
                parent.join(name.trim_end_matches('/'))
            }
            FileTreeOp::Rename(old) => {
                let target = old.parent().unwrap_or(&self.root).join(name);
                // Renaming to itself, or only changing case, is allowed.
                let same = target == *old
                    || target.to_string_lossy().to_lowercase()
                        == old.to_string_lossy().to_lowercase();
                if same {
                    return None;
                }
                target
            }
        };
        target.symlink_metadata().is_ok().then(|| {
            format!(
                "A file or folder {} already exists at this location. Please choose a different name.",
                name.trim_end_matches('/')
            )
        })
    }

    pub(crate) fn validate_file_tree_input(&mut self, cx: &mut Context<Self>) {
        let Some(op) = self.file_tree_op.clone() else {
            return;
        };
        let raw = self.file_tree_input.read(cx).value().to_string();
        let name = raw.trim();
        self.file_tree_error = if name.is_empty() {
            None
        } else {
            self.file_tree_name_error(&op, name)
        };
        cx.notify();
    }

    /// Clicking away keeps a valid name and drops an empty or invalid one.
    pub(crate) fn commit_file_tree_on_blur(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(op) = self.file_tree_op.clone() else {
            return;
        };
        let raw = self.file_tree_input.read(cx).value().to_string();
        let name = raw.trim();
        if name.is_empty() || self.file_tree_name_error(&op, name).is_some() {
            self.cancel_file_tree_prompt(cx);
        } else {
            self.submit_file_tree_prompt(window, cx);
        }
    }

    pub(crate) fn submit_file_tree_prompt(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(op) = self.file_tree_op.clone() else {
            return;
        };
        let raw = self.file_tree_input.read(cx).value().to_string();
        let name = raw.trim().to_string();
        if name.is_empty() {
            self.cancel_file_tree_prompt(cx);
            return;
        }
        if let Some(err) = self.file_tree_name_error(&op, &name) {
            self.file_tree_error = Some(err);
            cx.notify();
            return;
        }

        let result: std::io::Result<(PathBuf, bool)> = (|| match &op {
            FileTreeOp::NewFile { parent } => {
                // `a/b/` makes folders; `a/b.dart` makes `a/` then the file.
                let target = parent.join(name.trim_end_matches('/'));
                if name.ends_with('/') {
                    std::fs::create_dir_all(&target)?;
                    return Ok((target, false));
                }
                if let Some(dir) = target.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::OpenOptions::new().write(true).create_new(true).open(&target)?;
                Ok((target, true))
            }
            FileTreeOp::NewFolder { parent } => {
                let target = parent.join(name.trim_end_matches('/'));
                std::fs::create_dir_all(&target)?;
                Ok((target, false))
            }
            // Validated above; the move happens in `run_file_moves`, after
            // the language server has had its say about imports.
            FileTreeOp::Rename(old) => Ok((old.parent().unwrap_or(&self.root).join(&name), false)),
        })();

        let (target, open_it) = match result {
            Ok(done) => done,
            Err(err) => {
                self.file_tree_error = Some(err.to_string());
                cx.notify();
                return;
            }
        };
        self.file_tree_op = None;
        self.file_tree_error = None;

        if let FileTreeOp::Rename(old) = &op {
            if *old != target {
                self.run_file_moves(vec![(old.clone(), target.clone())], target, cx);
            }
            cx.notify();
            return;
        }
        self.finish_tree_change(&target, cx);
        if open_it {
            self.open_path(target, window, cx);
        }
    }

    /// Open tabs follow a moved file, or every file under a moved folder
    /// (path, title and language server all switch to the new location).
    fn retarget_tabs(&mut self, old: &Path, new: &Path, cx: &mut Context<Self>) {
        for tab in &mut self.tabs {
            let Some(path) = tab.path.clone() else { continue };
            let Ok(rel) = path.strip_prefix(old) else { continue };
            let moved = if rel.as_os_str().is_empty() { new.to_path_buf() } else { new.join(rel) };
            tab.path = Some(moved.clone());
            tab.editor.update(cx, |ed, cx| ed.rebind_path(moved, cx));
        }
        self.save_session_state();
    }

    /// Rescan (off the UI thread), then reveal and select `target`.
    fn finish_tree_change(&mut self, target: &Path, cx: &mut Context<Self>) {
        self.rescan_tree_then_reveal(Some(target.to_path_buf()), cx);
        self.refresh_git(cx);
        cx.notify();
    }

    /// Rename or move files the way VS Code does: first ask the language
    /// servers for the edits that keep imports pointing at the right place
    /// (`workspace/willRenameFiles`, off the UI thread, capped at a few
    /// seconds), apply them, then move the files, then tell the servers.
    pub(crate) fn run_file_moves(
        &mut self,
        renames: Vec<(PathBuf, PathBuf)>,
        reveal: PathBuf,
        cx: &mut Context<Self>,
    ) {
        self.file_tree_busy = Some("Updating imports\u{2026}");
        cx.notify();
        let asked = renames.clone();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let edits = editor_view::will_rename_files(&asked, std::time::Duration::from_secs(5));
            let _ = tx.send_blocking(edits);
        });
        cx.spawn(async move |weak, cx| {
            let edits = rx.recv().await.unwrap_or_default();
            let _ = weak.update(cx, |this, cx| {
                this.file_tree_busy = None;
                let edited = edits.len();
                for (path, edits) in edits {
                    this.apply_external_file_edits(path, edits, cx);
                }
                let mut moved = Vec::new();
                for (old, new) in &renames {
                    if let Some(dir) = new.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    match std::fs::rename(old, new) {
                        Ok(()) => {
                            this.retarget_tabs(old, new, cx);
                            moved.push((old.clone(), new.clone()));
                        }
                        Err(err) => {
                            this.set_git_notice(format!("Couldn't move: {err}"), true, cx);
                        }
                    }
                }
                editor_view::did_rename_files(&moved);
                if edited > 0 && !moved.is_empty() {
                    let files = if edited == 1 { "file" } else { "files" };
                    this.set_git_notice(format!("Updated imports in {edited} {files}"), false, cx);
                }
                this.finish_tree_change(&reveal, cx);
            });
        })
        .detach();
    }

    /// Move `src` into the folder `dest_dir` (drag and drop, or cut + paste).
    /// Refuses to overwrite, and to move a folder into itself.
    pub(crate) fn move_tree_path(&mut self, src: PathBuf, dest_dir: PathBuf, cx: &mut Context<Self>) {
        let Some(name) = src.file_name() else { return };
        if src.parent() == Some(dest_dir.as_path()) {
            return;
        }
        if dest_dir.starts_with(&src) {
            self.set_git_notice("Can't move a folder into itself.", true, cx);
            return;
        }
        let target = dest_dir.join(name);
        if target.symlink_metadata().is_ok() {
            self.set_git_notice(
                format!(
                    "{} already exists in {}.",
                    name.to_string_lossy(),
                    dest_dir.file_name().unwrap_or(dest_dir.as_os_str()).to_string_lossy()
                ),
                true,
                cx,
            );
            return;
        }
        self.run_file_moves(vec![(src, target.clone())], target, cx);
    }

    /// Copy `src` into `dest_dir`. A clash gets VS Code's names:
    /// `main copy.dart`, then `main copy 2.dart`, …
    pub(crate) fn copy_tree_path(&mut self, src: PathBuf, dest_dir: PathBuf, cx: &mut Context<Self>) {
        if dest_dir.starts_with(&src) && src.is_dir() {
            self.set_git_notice("Can't copy a folder into itself.", true, cx);
            return;
        }
        let target = free_copy_name(&src, &dest_dir);
        if let Err(err) = copy_recursively(&src, &target) {
            self.set_git_notice(format!("Couldn't copy: {err}"), true, cx);
            return;
        }
        self.finish_tree_change(&target, cx);
    }

    /// Remember a file or folder for Paste (`cut` moves it, else copies).
    pub(crate) fn set_tree_clipboard(&mut self, path: PathBuf, cut: bool, cx: &mut Context<Self>) {
        self.tree_clipboard = Some(crate::TreeClipboard { path, cut });
        cx.notify();
    }

    /// Paste what was cut or copied in the tree into the target folder.
    pub(crate) fn paste_into_tree(&mut self, cx: &mut Context<Self>) {
        let Some(clip) = self.tree_clipboard.clone() else { return };
        let dest = self.file_tree_target_dir(cx);
        if clip.cut {
            // A cut is used up by its paste, as in VS Code and Finder.
            self.tree_clipboard = None;
            self.move_tree_path(clip.path, dest, cx);
        } else {
            self.copy_tree_path(clip.path, dest, cx);
        }
    }

    /// Copy files dropped in from Finder into a folder of the tree.
    pub(crate) fn import_external_paths(&mut self, paths: &[PathBuf], dest_dir: PathBuf, cx: &mut Context<Self>) {
        let mut last = None;
        for src in paths {
            if src.starts_with(&self.root) {
                // Dragged from inside the workspace (e.g. another window): move.
                self.move_tree_path(src.clone(), dest_dir.clone(), cx);
                continue;
            }
            let target = free_copy_name(src, &dest_dir);
            match copy_recursively(src, &target) {
                Ok(()) => last = Some(target),
                Err(err) => self.set_git_notice(format!("Couldn't copy: {err}"), true, cx),
            }
        }
        if let Some(target) = last {
            self.finish_tree_change(&target, cx);
        }
    }

    pub(crate) fn delete_tree_path_clicked(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let key = format!("tree-del:{}", path.to_string_lossy());
        if self.git.confirm_discard.as_deref() == Some(key.as_str()) {
            self.git.confirm_discard = None;
            // To the Trash, as in VS Code and Finder: recoverable, unlike rm.
            if let Err(err) = trash::delete(&path) {
                self.set_git_notice(format!("Couldn't move to Trash: {err}"), true, cx);
                return;
            }
            // Close the file's tab, or every tab under a deleted folder.
            while let Some(ix) = self
                .tabs
                .iter()
                .position(|t| t.path.as_ref().is_some_and(|p| p.starts_with(&path)))
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
        self.rescan_tree_then_reveal(None, cx);
    }

    /// Scan the folder on a background thread and swap the result in,
    /// keeping open folders, a pending new-entry row, and (optionally)
    /// revealing and selecting `reveal`. A big project no longer stalls the
    /// UI after each create, rename, move or paste.
    pub(crate) fn rescan_tree_then_reveal(&mut self, reveal: Option<PathBuf>, cx: &mut Context<Self>) {
        self.tree_scan_gen += 1;
        let generation = self.tree_scan_gen;
        let root = self.root.clone();
        let show_hidden = self.show_hidden;
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let nodes = crate::rails::files::scan_tree(&root, show_hidden);
            let files = flatten_files(&root);
            let _ = tx.send_blocking((nodes, files));
        });
        let scanned_root = self.root.clone();
        cx.spawn(async move |weak, cx| {
            let Ok((nodes, files)) = rx.recv().await else { return };
            let _ = weak.update(cx, |this, cx| {
                // A newer scan was started, or the folder changed: drop this one.
                if this.tree_scan_gen != generation || this.root != scanned_root {
                    return;
                }
                this.all_files_hash = hash_paths(&files);
                this.all_files = files;
                let mut items = crate::rails::files::to_tree_items(nodes);
                if let Some(FileTreeOp::NewFile { parent } | FileTreeOp::NewFolder { parent }) =
                    this.file_tree_op.clone()
                {
                    let placeholder = gpui_kit::component::tree::TreeItem::new(
                        crate::rails::files::NEW_ENTRY_ID,
                        "",
                    );
                    items = if parent == this.root {
                        std::iter::once(placeholder).chain(items).collect()
                    } else {
                        crate::rails::files::with_child_first(
                            &items,
                            &parent.to_string_lossy(),
                            placeholder,
                        )
                    };
                }
                this.set_tree_items(items, cx);
                if let Some(target) = &reveal {
                    let id: SharedString = target.to_string_lossy().to_string().into();
                    this.tree_state.update(cx, |state, cx| {
                        state.reveal_item(&id, ScrollStrategy::Center, cx);
                        if let Some(ix) = state.index_of(&id) {
                            state.set_selected_index(Some(ix), cx);
                        }
                    });
                }
                cx.notify();
            });
        })
        .detach();
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
        let harness_str = crate::agent_store::harness_key(self.agent.harness);
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
            "indent_guides": self.editor_options.indent_guides,
            "preview_keep_logins": self.web_preview_keep_logins,
            "bracket_colors": self.editor_options.bracket_colors,
        });
        if let Ok(json) = serde_json::to_vec_pretty(&val) {
            let _ = std::fs::write(path, json);
        }
    }

    /// The saved "show dotfiles" choice, read before the first tree build so
    /// startup shows the right files straight away.
    pub(crate) fn saved_show_hidden() -> Option<bool> {
        let home = std::env::var("HOME").ok()?;
        let bytes = std::fs::read(Path::new(&home).join(".config/shame/session.json")).ok()?;
        serde_json::from_slice::<serde_json::Value>(&bytes).ok()?.get("show_hidden")?.as_bool()
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
            // The tree was built before preferences loaded: rebuild it if the
            // saved choice differs, or it shows the wrong files and the eye
            // button's first click appears to do nothing.
            if self.show_hidden != sh {
                self.show_hidden = sh;
                if self.has_workspace {
                    self.rescan_file_tree(cx);
                }
            }
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
        if let Some(v) = val.get("preview_keep_logins").and_then(|v| v.as_bool()) {
            self.web_preview_keep_logins = v;
        }
        if let Some(v) = val.get("indent_guides").and_then(|v| v.as_bool()) {
            self.editor_options.indent_guides = v;
        }
        if let Some(v) = val.get("bracket_colors").and_then(|v| v.as_bool()) {
            self.editor_options.bracket_colors = v;
        }
        self.apply_save_options(cx);
        self.apply_editor_options(cx);
        if let Some(h) = val.get("agent_harness").and_then(|v| v.as_str()) {
            let tool = crate::agent_store::harness_from_key(h)
                .unwrap_or(rails::agent::AgentHarness::ClaudeCode);
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
            self.set_agent_open(ab);
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
        // The agent thread is saved per workspace, not in session.json.
        self.restore_agent_conversation(cx);
    }
}

/// Where a copy of `src` lands in `dest_dir`: its own name when free, else
/// VS Code's `name copy.ext`, `name copy 2.ext`, …
pub(crate) fn free_copy_name(src: &Path, dest_dir: &Path) -> PathBuf {
    let name = src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let first = dest_dir.join(&name);
    if first.symlink_metadata().is_err() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 && !src.is_dir() => (&name[..dot], &name[dot..]),
        _ => (name.as_str(), ""),
    };
    (1..)
        .map(|n| {
            let suffix = if n == 1 { " copy".to_string() } else { format!(" copy {n}") };
            dest_dir.join(format!("{stem}{suffix}{ext}"))
        })
        .find(|p| p.symlink_metadata().is_err())
        .expect("an unused name exists")
}

/// `cp -R` for one file or folder (symlinks copied as links).
pub(crate) fn copy_recursively(src: &Path, dst: &Path) -> std::io::Result<()> {
    let meta = src.symlink_metadata()?;
    if meta.file_type().is_symlink() {
        let link = std::fs::read_link(src)?;
        return std::os::unix::fs::symlink(link, dst);
    }
    if meta.is_dir() {
        std::fs::create_dir(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursively(&entry.path(), &dst.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dst).map(|_| ())
    }
}

#[cfg(test)]
mod file_op_tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shame-fileops-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[core::prelude::v1::test]
    fn copy_names_follow_vscode() {
        let dir = scratch("names");
        let src = dir.join("main.dart");
        std::fs::write(&src, "x").unwrap();
        assert_eq!(free_copy_name(&src, &dir), dir.join("main copy.dart"));
        std::fs::write(dir.join("main copy.dart"), "").unwrap();
        assert_eq!(free_copy_name(&src, &dir), dir.join("main copy 2.dart"));
        // Elsewhere, the plain name is free.
        let other = dir.join("other");
        std::fs::create_dir(&other).unwrap();
        assert_eq!(free_copy_name(&src, &other), other.join("main.dart"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[core::prelude::v1::test]
    fn folders_copy_with_their_contents() {
        let dir = scratch("tree");
        std::fs::create_dir_all(dir.join("lib/src")).unwrap();
        std::fs::write(dir.join("lib/src/a.dart"), "a").unwrap();
        let target = free_copy_name(&dir.join("lib"), &dir);
        assert_eq!(target, dir.join("lib copy"));
        copy_recursively(&dir.join("lib"), &target).unwrap();
        assert_eq!(std::fs::read_to_string(target.join("src/a.dart")).unwrap(), "a");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
