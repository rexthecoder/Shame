//! State and actions behind the Git panel (the "Git" tab of the left sidebar): the structured
//! repository status, stage / unstage / discard, commit, fetch / pull / push and branches.
//!
//! The git work is in `core_workspace` and always runs on a background thread, one command at a
//! time, so the UI never blocks and rapid clicks cannot trip over git's index lock. The panel UI
//! is `rails/git.rs`.

use std::path::PathBuf;
use std::sync::Mutex;

use core_workspace::{BranchInfo, ChangedFile, RepoStatus};
use gpui_kit::component::input::{InputState, TextareaState};
use gpui_kit::*;

use crate::DartIde;

/// Drop AI-touched paths that had a working-tree change in `previous` and have
/// none in `current`: the agent reverted them. A path that was never changed
/// yet is kept, because tool calls stream just before they run.
pub(crate) fn retain_reverted_ai_paths(
    paths: &mut Vec<String>,
    previous: &std::collections::HashSet<String>,
    current: &std::collections::HashSet<String>,
) -> bool {
    let before = paths.len();
    paths.retain(|p| !previous.contains(p) || current.contains(p));
    paths.len() != before
}

/// Drop every tracked path that is not currently modified. Used at the end of
/// a turn, once every tool effect has landed.
pub(crate) fn retain_pending_ai_paths(
    paths: &mut Vec<String>,
    current: &std::collections::HashSet<String>,
) -> bool {
    let before = paths.len();
    paths.retain(|p| current.contains(p));
    paths.len() != before
}

/// Which view the left sidebar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarTab {
    Files,
    Search,
    Git,
}

/// One line under the commit box: what just happened.
#[derive(Clone)]
pub(crate) struct GitNotice {
    pub(crate) text: String,
    pub(crate) is_error: bool,
}

/// A git action waiting for dirty buffers to finish saving, so the index and
/// the commit include exactly what the editor shows.
#[derive(Clone)]
pub(crate) enum PendingGit {
    Stage(Vec<ChangedFile>),
    StageAll,
    Commit { message: String, amend: bool },
    CommitAll { message: String },
    Push,
}

pub(crate) struct GitState {
    /// `None` outside a repository.
    pub(crate) status: Option<RepoStatus>,
    /// `(added, removed)` line counts per repo-relative path, vs `HEAD`.
    pub(crate) stats: std::collections::HashMap<String, (u32, u32)>,
    /// A long action in flight ("Pushing…"); other long actions wait for it.
    pub(crate) busy: Option<&'static str>,
    pub(crate) notice: Option<GitNotice>,
    pub(crate) message: Entity<TextareaState>,
    pub(crate) branch_input: Entity<InputState>,
    pub(crate) branches: Vec<BranchInfo>,
    pub(crate) branches_open: bool,
    pub(crate) branch_selected: usize,
    pub(crate) amend: bool,
    /// Push as soon as the commit in flight lands ("Commit & Push").
    pub(crate) push_after_commit: bool,
    pub(crate) staged_collapsed: bool,
    pub(crate) changes_collapsed: bool,
    pub(crate) stashes_collapsed: bool,
    pub(crate) commits_collapsed: bool,
    /// Changed files grouped under their folders instead of one flat list.
    pub(crate) group_by_folder: bool,
    /// Branch and dirty state of each recent project, for the welcome screen.
    pub(crate) recent_info: std::collections::HashMap<std::path::PathBuf, (Option<String>, bool)>,
    pub(crate) recent_info_requested: bool,
    /// A repaint is scheduled while a language server is busy.
    pub(crate) lsp_poll: bool,
    /// Hash of the commit currently expanded in the Recent Commits list, if any.
    pub(crate) expanded_commit: Option<String>,
    pub(crate) expanded_commit_files: Vec<ChangedFile>,
    /// Whether the diff view shows side-by-side (`false`) or unified inline (`true`) rows.
    pub(crate) unified_diff: bool,
    /// Left-column width fraction (`0.18..=0.82`) in side-by-side diff mode (default `0.5`).
    pub(crate) diff_split_ratio: f32,
    pub(crate) diff_dragging_split: bool,
    /// Discard is destructive: the first click arms it (for a few seconds), the second runs it.
    pub(crate) confirm_discard: Option<String>,
    /// Files (path, staged?) the developer has marked reviewed in the diff view.
    pub(crate) reviewed: std::collections::HashSet<(String, bool)>,
    /// Conventional-commit types this repo uses (`None` until loaded; empty if it has no convention).
    pub(crate) commit_types: Option<Vec<String>>,
    refreshing: bool,
    /// Something asked for a refresh while one was running; run another when it finishes.
    refresh_queued: bool,
}

impl GitState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<DartIde>) -> Self {
        Self {
            status: None,
            busy: None,
            notice: None,
            message: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .placeholder("Describe your change\u{2026}")
                    .auto_grow(1, 5)
            }),
            branch_input: cx
                .new(|cx| InputState::new(window, cx).placeholder("Filter or create branch…")),
            branches: Vec::new(),
            branches_open: false,
            branch_selected: 0,
            amend: false,
            push_after_commit: false,
            staged_collapsed: false,
            changes_collapsed: false,
            stashes_collapsed: false,
            commits_collapsed: false,
            group_by_folder: false,
            recent_info: std::collections::HashMap::new(),
            recent_info_requested: false,
            lsp_poll: false,
            expanded_commit: None,
            expanded_commit_files: Vec::new(),
            unified_diff: false,
            diff_split_ratio: 0.5,
            diff_dragging_split: false,
            confirm_discard: None,
            reviewed: Default::default(),
            commit_types: None,
            stats: std::collections::HashMap::new(),
            refreshing: false,
            refresh_queued: false,
        }
    }
}

/// Git commands from the panel run one at a time.
static GIT_LOCK: Mutex<()> = Mutex::new(());

impl DartIde {
    /// Re-read the repository status (and the branch / file badges the rest of the UI shows).
    pub(crate) fn refresh_git(&mut self, cx: &mut Context<Self>) {
        // No project open: there is no repository to read.
        if !self.has_workspace {
            return;
        }
        if self.git.refreshing {
            self.git.refresh_queued = true;
            return;
        }
        self.git.refreshing = true;
        let root = self.root.clone();
        let need_types = self.git.commit_types.is_none();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _guard = GIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            // One status read feeds the panel, the titlebar branch and the file-tree badges.
            let status = core_workspace::repo_status(&root);
            let branch = status.as_ref().and_then(|s| s.branch.clone());
            let badges = status
                .as_ref()
                .map(|s| s.badge_map(&root))
                .unwrap_or_default();
            let types =
                (need_types && status.is_some()).then(|| core_workspace::commit_types(&root));
            let stats = status
                .as_ref()
                .map(|s| core_workspace::change_stats(&root, &s.root, &s.unstaged))
                .unwrap_or_default();
            let _ = tx.send_blocking((status, branch, badges, types, stats));
        });
        cx.spawn(async move |weak, cx| {
            let result = rx.recv().await;
            let _ = weak.update(cx, |this, cx| {
                this.git.refreshing = false;
                if std::mem::take(&mut this.git.refresh_queued) {
                    this.refresh_git(cx);
                }
                let Ok((status, branch, badges, types, stats)) = result else {
                    return;
                };
                let types_changed = types.is_some();
                if types.is_some() {
                    this.git.commit_types = types;
                }
                // A file the agent touched and then reverted is no longer a pending
                // change. Only prune paths that *were* changed in the previous status
                // and are clean now: a tool call is streamed just before it runs, so
                // a brand-new path may not be in git yet on the first refresh. The
                // first successful read is different: everything in the restored
                // thread is old, so a clean path there is a ghost.
                let first_read = this.git.status.is_none();
                let previous: std::collections::HashSet<String> = this
                    .git
                    .status
                    .as_ref()
                    .map(|s| s.unstaged.iter().map(|f| f.path.clone()).collect())
                    .unwrap_or_default();
                let mut pruned = false;
                if let Some(status) = &status {
                    let current: std::collections::HashSet<String> =
                        status.unstaged.iter().map(|f| f.path.clone()).collect();
                    pruned = if first_read {
                        retain_pending_ai_paths(&mut this.agent.ai_touched_paths, &current)
                    } else {
                        retain_reverted_ai_paths(
                            &mut this.agent.ai_touched_paths,
                            &previous,
                            &current,
                        )
                    };
                }
                let changed = pruned
                    || types_changed
                    || this.git.stats != stats
                    || this.git.status != status
                    || this.git_branch != branch
                    || this.git_status != badges;
                this.git.status = status;
                this.git.stats = stats;
                this.git_branch = branch;
                this.git_status = badges;
                if changed {
                    this.refresh_open_diff(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Keep the panel current while it is on screen (edits and terminal commands change the repo
    /// behind our back).
    pub(crate) fn start_git_refresh_loop(&mut self, cx: &mut Context<Self>) {
        let (tx, rx) = async_channel::bounded::<()>(1);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                if tx.send_blocking(()).is_err() {
                    break;
                }
            }
        });
        cx.spawn(async move |weak, cx| {
            while rx.recv().await.is_ok() {
                let alive = weak
                    .update(cx, |this, cx| {
                        this.sync_workspace_files_if_changed(cx);
                        // The Git panel needs a refresh while it is on screen; the
                        // agent rail needs one too while it is showing AI Changes,
                        // even when the sidebar is on Files or closed.
                        let git_visible =
                            this.sidebar_open && this.sidebar_tab == SidebarTab::Git;
                        let ai_pending = !this.agent.ai_touched_paths.is_empty();
                        if this.has_workspace && (git_visible || ai_pending) {
                            this.refresh_git(cx);
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn set_sidebar_tab(&mut self, tab: SidebarTab, cx: &mut Context<Self>) {
        self.sidebar_tab = tab;
        if tab == SidebarTab::Git {
            self.refresh_git(cx);
        }
        cx.notify();
    }

    /// Set the one-line notice under the commit box. A success clears itself after a few
    /// seconds so the panel returns to its resting state; an error stays until dismissed.
    pub(crate) fn set_git_notice(
        &mut self,
        text: impl Into<String>,
        is_error: bool,
        cx: &mut Context<Self>,
    ) {
        let text = text.into();
        if !is_error {
            let expected = text.clone();
            cx.spawn(async move |weak, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(4))
                    .await;
                let _ = weak.update(cx, |this, cx| {
                    if this
                        .git
                        .notice
                        .as_ref()
                        .is_some_and(|n| !n.is_error && n.text == expected)
                    {
                        this.git.notice = None;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        self.git.notice = Some(GitNotice { text, is_error });
        cx.notify();
    }

    /// Run `job` against the repository root off the UI thread, then refresh. `label` marks a
    /// long action ("Pushing…") as busy and reports `Ok` text as the notice; `None` runs quietly
    /// (stage / unstage), showing only errors.
    pub(crate) fn git_run(
        &mut self,
        window: &Window,
        label: Option<&'static str>,
        clear_message: bool,
        cx: &mut Context<Self>,
        job: impl FnOnce(&PathBuf, &RepoStatus) -> Result<String, String> + Send + 'static,
    ) {
        if label.is_some() && self.git.busy.is_some() {
            return;
        }
        let Some(status) = self.git.status.clone() else {
            return;
        };
        // Any other action disarms a pending discard.
        self.git.confirm_discard = None;
        if label.is_some() {
            self.git.busy = label;
            self.git.notice = None;
            cx.notify();
        }
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _guard = GIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let _ = tx.send_blocking(job(&status.root.clone(), &status));
        });
        cx.spawn_in(window, async move |weak, cx| {
            let Ok(result) = rx.recv().await else { return };
            let _ = weak.update_in(cx, |this, window, cx| {
                if label.is_some() {
                    this.git.busy = None;
                }
                match result {
                    Ok(text) if label.is_some() => {
                        if clear_message {
                            // A failed commit keeps its text; a successful one empties the box.
                            this.git
                                .message
                                .update(cx, |state, cx| state.set_value("", window, cx));
                            this.git.amend = false;
                            this.git.reviewed.clear();
                            this.git.commit_types = None;
                        }
                        this.set_git_notice(text, false, cx);
                        if clear_message && std::mem::take(&mut this.git.push_after_commit) {
                            this.git_push_now(window, cx);
                        }
                    }
                    Err(text) => {
                        this.git.push_after_commit = false;
                        this.set_git_notice(text, true, cx)
                    }
                    Ok(_) => {}
                }
                this.refresh_git(cx);
                // The open diff shows what just changed (a staged hunk, a reverted one, ...).
                if let Some((file, staged, commit)) = this
                    .diff
                    .as_ref()
                    .map(|d| (d.file.clone(), d.staged, d.commit.clone()))
                {
                    if let Some(hash) = commit {
                        this.open_git_commit_diff(hash, file, cx);
                    } else {
                        this.open_git_diff(file, staged, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn git_stage(
        &mut self,
        files: &[ChangedFile],
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.defer_git(PendingGit::Stage(files.to_vec()), window, cx);
    }

    fn git_stage_now(&mut self, files: &[ChangedFile], window: &Window, cx: &mut Context<Self>) {
        let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
        self.git_run(window, None, false, cx, move |root, _| {
            core_workspace::stage(root, &paths).map(|_| String::new())
        });
    }

    pub(crate) fn git_unstage(
        &mut self,
        files: &[ChangedFile],
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        // A rename stages as delete + add; both halves must go back together.
        let paths: Vec<String> = files
            .iter()
            .flat_map(|f| std::iter::once(f.path.clone()).chain(f.orig_path.clone()))
            .collect();
        self.git_run(window, None, false, cx, move |root, _| {
            core_workspace::unstage(root, &paths).map(|_| String::new())
        });
    }

    pub(crate) fn git_stage_all(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.defer_git(PendingGit::StageAll, window, cx);
    }

    fn git_stage_all_now(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.git_run(window, None, false, cx, |root, _| {
            core_workspace::stage_all(root).map(|_| String::new())
        });
    }

    pub(crate) fn git_unstage_all(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.git_run(window, None, false, cx, |root, _| {
            core_workspace::unstage_all(root).map(|_| String::new())
        });
    }

    /// First click arms the discard (the row asks "Discard?"); a second click on the same file
    /// within a few seconds runs it.
    pub(crate) fn git_discard_clicked(
        &mut self,
        file: ChangedFile,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.git.confirm_discard.as_deref() == Some(file.path.as_str()) {
            self.git_discard(file, window, cx);
            return;
        }
        self.arm_confirm(file.path.clone(), cx);
    }

    /// Arm a destructive action identified by `key` for a few seconds.
    pub(crate) fn arm_confirm(&mut self, key: String, cx: &mut Context<Self>) {
        self.git.confirm_discard = Some(key.clone());
        cx.notify();
        cx.spawn(async move |weak, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(3))
                .await;
            let _ = weak.update(cx, |this, cx| {
                if this.git.confirm_discard.as_deref() == Some(key.as_str()) {
                    this.git.confirm_discard = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn git_discard(&mut self, file: ChangedFile, window: &Window, cx: &mut Context<Self>) {
        self.git_run(window, None, false, cx, move |root, _| {
            core_workspace::discard(root, &file).map(|_| String::new())
        });
    }

    pub(crate) fn git_discard_all_clicked(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.git.confirm_discard.as_deref() == Some("discard-all") {
            let Some(files) = self.git.status.as_ref().map(|s| s.unstaged.clone()) else {
                return;
            };
            self.git_run(
                window,
                Some("Discarding changes\u{2026}"),
                false,
                cx,
                move |root, _| {
                    core_workspace::discard_files(root, &files)
                        .map(|_| "Discarded unstaged changes".to_string())
                },
            );
            return;
        }
        self.arm_confirm("discard-all".to_string(), cx);
    }

    pub(crate) fn git_stash_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let msg = self.git.message.read(cx).value().trim().to_string();
        let has_msg = !msg.is_empty();
        self.git_run(
            window,
            Some("Stashing\u{2026}"),
            has_msg,
            cx,
            move |root, _| {
                core_workspace::stash_save(root, has_msg.then_some(msg.as_str()))
            },
        );
    }

    pub(crate) fn git_stash_pop(&mut self, index: usize, window: &Window, cx: &mut Context<Self>) {
        self.git_run(window, Some("Popping stash\u{2026}"), false, cx, move |root, _| {
            core_workspace::stash_pop(root, index)
        });
    }

    pub(crate) fn git_stash_apply(
        &mut self,
        index: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.git_run(window, Some("Applying stash\u{2026}"), false, cx, move |root, _| {
            core_workspace::stash_apply(root, index)
        });
    }

    pub(crate) fn git_stash_drop_clicked(
        &mut self,
        index: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let key = format!("stash-drop:{index}");
        if self.git.confirm_discard.as_deref() == Some(key.as_str()) {
            self.git_run(window, Some("Dropping stash\u{2026}"), false, cx, move |root, _| {
                core_workspace::stash_drop(root, index)
            });
            return;
        }
        self.arm_confirm(key, cx);
    }

    /// Undo the last commit (`HEAD`), keeping its changes staged and restoring its commit message.
    pub(crate) fn git_uncommit_last(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.git.busy.is_some() {
            return;
        }
        let Some(status) = self.git.status.clone() else {
            return;
        };
        self.git.confirm_discard = None;
        self.git.busy = Some("Undoing commit\u{2026}");
        self.git.notice = None;
        cx.notify();

        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _guard = GIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let _ = tx.send_blocking(core_workspace::uncommit_last(&status.root));
        });
        cx.spawn_in(window, async move |weak, cx| {
            let Ok(result) = rx.recv().await else { return };
            let _ = weak.update_in(cx, |this, window, cx| {
                this.git.busy = None;
                match result {
                    Ok(restored_msg) => {
                        if this.git.message.read(cx).value().trim().is_empty()
                            && !restored_msg.is_empty()
                        {
                            this.git.message.update(cx, |state, cx| {
                                state.set_value(restored_msg, window, cx)
                            });
                        }
                        this.git.expanded_commit = None;
                        this.git.expanded_commit_files.clear();
                        this.set_git_notice("Undid last commit (changes kept staged)", false, cx);
                    }
                    Err(text) => this.set_git_notice(text, true, cx),
                }
                this.refresh_git(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Expand or collapse a commit row in the Recent Commits list to inspect the files it changed.
    pub(crate) fn git_toggle_commit(&mut self, hash: String, cx: &mut Context<Self>) {
        if self.git.expanded_commit.as_deref() == Some(hash.as_str()) {
            self.git.expanded_commit = None;
            self.git.expanded_commit_files.clear();
            if self.diff.as_ref().is_some_and(|d| d.commit.is_some()) {
                self.close_diff(cx);
            }
            cx.notify();
            return;
        }
        let Some(root) = self.git.status.as_ref().map(|s| s.root.clone()) else {
            return;
        };
        self.git.expanded_commit = Some(hash.clone());
        self.git.expanded_commit_files.clear();
        cx.notify();

        let (tx, rx) = async_channel::bounded(1);
        let job_hash = hash.clone();
        std::thread::spawn(move || {
            let _ = tx.send_blocking(core_workspace::commit_files(&root, &job_hash));
        });
        cx.spawn(async move |weak, cx| {
            let Ok(result) = rx.recv().await else { return };
            let _ = weak.update(cx, |this, cx| {
                if this.git.expanded_commit.as_deref() != Some(hash.as_str()) {
                    return;
                }
                if let Ok(files) = result {
                    this.git.expanded_commit_files = files;
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn git_init(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(core_workspace::init(&root));
        });
        cx.spawn(async move |weak, cx| {
            let Ok(result) = rx.recv().await else { return };
            let _ = weak.update(cx, |this, cx| {
                if let Err(text) = result {
                    this.set_git_notice(text, true, cx);
                }
                this.refresh_git(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn git_toggle_amend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.git.amend = !self.git.amend;
        let message_empty = self.git.message.read(cx).value().trim().is_empty();
        if self.git.amend && message_empty {
            // Start the amend from the message being replaced.
            if let Some(root) = self.git.status.as_ref().map(|s| s.root.clone()) {
                if let Some(last) = core_workspace::last_commit_message(&root) {
                    self.git
                        .message
                        .update(cx, |state, cx| state.set_value(last, window, cx));
                }
            }
        }
        cx.notify();
    }

    /// The files a commit would include: the staged ones, or everything when nothing is staged.
    pub(crate) fn commit_candidates(&self) -> &[ChangedFile] {
        match &self.git.status {
            Some(s) if !s.staged.is_empty() => &s.staged,
            Some(s) => &s.unstaged,
            None => &[],
        }
    }

    /// Fill the message box with a subject suggested from the files being committed.
    pub(crate) fn git_apply_suggestion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let types = self.git.commit_types.clone().unwrap_or_default();
        if let Some(text) = core_workspace::suggest_message(self.commit_candidates(), &types) {
            self.git
                .message
                .update(cx, |state, cx| state.set_value(text, window, cx));
            cx.notify();
        }
    }

    /// The composer's one button: commit what is staged, commit everything when nothing is
    /// staged, or amend — whichever the current state calls for.
    pub(crate) fn git_primary_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(status) = &self.git.status else {
            return;
        };
        let has_message = !self.git.message.read(cx).value().trim().is_empty();
        if self.git.amend {
            self.git_commit(window, cx);
        } else if has_message && !status.staged.is_empty() {
            self.git_commit(window, cx);
        } else if has_message && !status.unstaged.is_empty() {
            self.git_commit_all(window, cx);
        }
    }

    /// Stage everything, then commit it.
    fn git_commit_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.git.message.read(cx).value().to_string();
        self.defer_git(PendingGit::CommitAll { message }, window, cx);
    }

    fn git_commit_all_now(&mut self, message: String, window: &Window, cx: &mut Context<Self>) {
        self.git_run(
            window,
            Some("Committing\u{2026}"),
            true,
            cx,
            move |root, _| {
                core_workspace::stage_all(root)?;
                core_workspace::commit(root, &message, false)
            },
        );
    }

    /// Commit the staged changes with the message in the box.
    pub(crate) fn git_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.git.message.read(cx).value().to_string();
        let amend = self.git.amend;
        self.defer_git(PendingGit::Commit { message, amend }, window, cx);
    }

    fn git_commit_now(
        &mut self,
        message: String,
        amend: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.git_run(window, Some("Committing…"), true, cx, move |root, _| {
            core_workspace::commit(root, &message, amend)
        });
    }

    pub(crate) fn git_fetch(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.git_run(window, Some("Fetching…"), false, cx, |root, _| {
            core_workspace::fetch(root)
        });
    }

    pub(crate) fn git_pull(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.git_run(window, Some("Pulling…"), false, cx, |root, _| {
            core_workspace::pull(root)
        });
    }

    /// Commit (as the main button would), then push once it lands.
    pub(crate) fn git_commit_and_push(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.git.push_after_commit = true;
        self.git_primary_action(window, cx);
        // Nothing was started (no message, nothing to commit): don't push later by surprise.
        if self.git.busy.is_none() && self.pending_git.is_none() {
            self.git.push_after_commit = false;
        }
    }

    pub(crate) fn git_force_push(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.git_run(window, Some("Force-pushing\u{2026}"), false, cx, |root, status| {
            core_workspace::push_force_with_lease(root, status)
        });
    }

    pub(crate) fn git_revert_commit(&mut self, hash: String, window: &Window, cx: &mut Context<Self>) {
        self.git_run(window, Some("Reverting\u{2026}"), false, cx, move |root, _| {
            core_workspace::revert_commit(root, &hash)
        });
    }

    pub(crate) fn git_push(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.defer_git(PendingGit::Push, window, cx);
    }

    fn git_push_now(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.git_run(window, Some("Pushing…"), false, cx, |root, status| {
            core_workspace::push(root, status)
        });
    }

    /// Save every dirty buffer, then run the git action. Format-on-save is
    /// asynchronous, so running git immediately could commit the old disk
    /// content while the save landed a moment later — the change then looked
    /// new again right after the push.
    pub(crate) fn defer_git(
        &mut self,
        action: PendingGit,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        // One deferred action at a time; a second click while saving does not
        // queue a duplicate commit.
        if self.pending_git.is_some() {
            return;
        }
        let waiting: std::collections::HashSet<EntityId> = self
            .tabs
            .iter()
            .filter(|tab| {
                let buffer = tab.editor.read(cx).buffer();
                // An untitled buffer cannot be saved, and would never emit
                // `Saved`, so it must not hold the action forever.
                buffer.is_dirty() && buffer.path().is_some()
            })
            .map(|tab| tab.editor.entity_id())
            .collect();
        if waiting.is_empty() {
            self.run_pending_git(action, window, cx);
            return;
        }
        self.pending_git = Some((action, waiting.clone()));
        self.set_git_notice("Saving your changes first\u{2026}", false, cx);
        for tab in &self.tabs {
            if waiting.contains(&tab.editor.entity_id()) {
                tab.editor.update(cx, |editor, cx| editor.save(cx));
            }
        }
        cx.notify();
    }

    pub(crate) fn run_pending_git(
        &mut self,
        action: PendingGit,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            PendingGit::Stage(files) => self.git_stage_now(&files, window, cx),
            PendingGit::StageAll => self.git_stage_all_now(window, cx),
            PendingGit::Commit { message, amend } => {
                self.git_commit_now(message, amend, window, cx)
            }
            PendingGit::CommitAll { message } => self.git_commit_all_now(message, window, cx),
            PendingGit::Push => self.git_push_now(window, cx),
        }
    }

    pub(crate) fn git_toggle_branches(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.git.branches_open = !self.git.branches_open;
        if self.git.branches_open {
            if let Some(root) = self.git.status.as_ref().map(|s| s.root.clone()) {
                self.git.branches = core_workspace::branches(&root);
            }
            // The current branch is sorted first by the switcher.
            self.git.branch_selected = 0;
            self.git.branch_input.update(cx, |s, cx| {
                s.set_value("", window, cx);
                s.focus(window, cx);
            });
        }
        cx.notify();
    }

    pub(crate) fn git_open_branch_switcher(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.git.branches_open = true;
        let root = self
            .git
            .status
            .as_ref()
            .map(|s| s.root.clone())
            .unwrap_or_else(|| self.root.clone());
        self.git.branches = core_workspace::branches(&root);
        // The current branch is sorted first by the switcher.
        self.git.branch_selected = 0;
        self.git.branch_input.update(cx, |s, cx| {
            s.set_value("", window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn git_delete_branch_clicked(
        &mut self,
        name: String,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let key = format!("branch-del:{name}");
        if self.git.confirm_discard.as_deref() == Some(key.as_str()) {
            self.git.confirm_discard = None;
            let branch_name = name.clone();
            self.git_run(
                window,
                Some("Deleting branch\u{2026}"),
                false,
                cx,
                move |root, _| core_workspace::delete_branch(root, &branch_name),
            );
            self.git.branches.retain(|b| b.name != name);
            cx.notify();
            return;
        }
        self.arm_confirm(key, cx);
    }

    pub(crate) fn git_checkout(&mut self, name: String, window: &Window, cx: &mut Context<Self>) {
        self.git.branches_open = false;
        self.git_run(
            window,
            Some("Switching branch…"),
            false,
            cx,
            move |root, _| {
                core_workspace::checkout(root, &name).map(|_| format!("Switched to {name}"))
            },
        );
    }

    pub(crate) fn git_create_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.git.branch_input.read(cx).value().to_string();
        if name.trim().is_empty() {
            return;
        }
        self.git
            .branch_input
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.git.branches_open = false;
        self.git_run(
            window,
            Some("Creating branch…"),
            false,
            cx,
            move |root, _| {
                core_workspace::create_branch(root, &name)
                    .map(|_| format!("Created {}", name.trim()))
            },
        );
    }
}

/// A commit subject split for display: `feat: [MOBL-4665] show the RFIs`
/// becomes kind `feat`, ticket `MOBL-4665`, text `show the RFIs`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CommitSubject<'a> {
    /// Conventional-commit type (`feat`, `fix`, `build`, …), scope dropped.
    pub kind: Option<&'a str>,
    /// A leading `[TICKET-123]` reference.
    pub ticket: Option<&'a str>,
    pub text: &'a str,
    /// `Merge branch …` / `Merge pull request …`: shown quieter.
    pub is_merge: bool,
}

pub(crate) fn parse_commit_subject(subject: &str) -> CommitSubject<'_> {
    let mut rest = subject.trim();
    let is_merge = rest.starts_with("Merge ");
    let mut kind = None;
    // `type(scope)!: ` with a short, lowercase type.
    if let Some(colon) = rest.find(": ") {
        let head = &rest[..colon];
        let ty = head.split(['(', '!']).next().unwrap_or(head);
        let scope_ok = head[ty.len()..].is_empty()
            || head[ty.len()..].trim_end_matches('!').starts_with('(') && head.trim_end_matches('!').ends_with(')')
            || &head[ty.len()..] == "!";
        if !ty.is_empty() && ty.len() <= 10 && ty.bytes().all(|b| b.is_ascii_lowercase()) && scope_ok {
            kind = Some(ty);
            rest = rest[colon + 2..].trim_start();
        }
    }
    let mut ticket = None;
    if let Some(inner) = rest.strip_prefix('[') {
        if let Some(close) = inner.find(']') {
            let t = &inner[..close];
            if !t.is_empty() && !t.contains(' ') {
                ticket = Some(t);
                rest = inner[close + 1..].trim_start();
            }
        }
    }
    CommitSubject { kind, ticket, text: if rest.is_empty() { subject } else { rest }, is_merge }
}

/// `git stash list` messages look like `On main: tidy` or
/// `WIP on main: 1a2b3c4 last commit`; split out the branch.
pub(crate) fn parse_stash_message(message: &str) -> (Option<&str>, &str) {
    for prefix in ["WIP on ", "On "] {
        if let Some(rest) = message.strip_prefix(prefix) {
            if let Some((branch, text)) = rest.split_once(": ") {
                return (Some(branch), text.trim());
            }
        }
    }
    (None, message.trim())
}

#[cfg(test)]
mod display_tests {
    use super::*;

    #[core::prelude::v1::test]
    fn conventional_subjects_split_into_kind_ticket_and_text() {
        let s = parse_commit_subject("feat: [MOBL-4665] show the RFIs a customer");
        assert_eq!(s, CommitSubject { kind: Some("feat"), ticket: Some("MOBL-4665"), text: "show the RFIs a customer", is_merge: false });
        let s = parse_commit_subject("build(deps): bump the bundler group");
        assert_eq!((s.kind, s.ticket, s.text), (Some("build"), None, "bump the bundler group"));
        let s = parse_commit_subject("fix!: drop old API");
        assert_eq!((s.kind, s.text), (Some("fix"), "drop old API"));
    }

    #[core::prelude::v1::test]
    fn plain_and_merge_subjects_are_left_alone() {
        let s = parse_commit_subject("Merge branch 'main' into feature-pro");
        assert!(s.is_merge);
        assert_eq!((s.kind, s.text), (None, "Merge branch 'main' into feature-pro"));
        // A capitalised word before a colon isn't a commit type.
        let s = parse_commit_subject("Note: this is prose");
        assert_eq!((s.kind, s.text), (None, "Note: this is prose"));
    }

    #[core::prelude::v1::test]
    fn stash_messages_split_out_the_branch() {
        assert_eq!(parse_stash_message("On main: tidy up"), (Some("main"), "tidy up"));
        assert_eq!(parse_stash_message("WIP on feat/x: 1a2b3c4 last"), (Some("feat/x"), "1a2b3c4 last"));
        assert_eq!(parse_stash_message("pro trade tab design"), (None, "pro trade tab design"));
    }
}

#[cfg(test)]
mod ai_path_tests {
    use super::*;
    use std::collections::HashSet;

    #[core::prelude::v1::test]
    fn a_reverted_file_leaves_the_pending_list() {
        let mut paths = vec!["a.dart".to_string(), "b.dart".to_string()];
        let previous = HashSet::from(["a.dart".to_string(), "b.dart".to_string()]);
        let current = HashSet::from(["b.dart".to_string()]);
        assert!(retain_reverted_ai_paths(&mut paths, &previous, &current));
        assert_eq!(paths, vec!["b.dart"]);
    }

    #[core::prelude::v1::test]
    fn a_path_not_modified_yet_is_kept() {
        // The tool call is streamed before it runs, so a new path is not in the
        // status read that immediately follows.
        let mut paths = vec!["new.dart".to_string()];
        let previous = HashSet::new();
        let current = HashSet::new();
        assert!(!retain_reverted_ai_paths(&mut paths, &previous, &current));
        assert_eq!(paths, vec!["new.dart"]);
    }

    #[core::prelude::v1::test]
    fn finishing_a_turn_keeps_only_what_is_still_modified() {
        let mut paths = vec!["a.dart".to_string(), "b.dart".to_string()];
        let current = HashSet::from(["b.dart".to_string()]);
        assert!(retain_pending_ai_paths(&mut paths, &current));
        assert_eq!(paths, vec!["b.dart"]);
        assert!(!retain_pending_ai_paths(&mut paths, &current));
    }
}
