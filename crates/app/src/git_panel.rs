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

/// Which view the left sidebar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarTab {
    Files,
    Git,
}

/// One line under the commit box: what just happened.
#[derive(Clone)]
pub(crate) struct GitNotice {
    pub(crate) text: String,
    pub(crate) is_error: bool,
}

pub(crate) struct GitState {
    /// `None` outside a repository.
    pub(crate) status: Option<RepoStatus>,
    /// A long action in flight ("Pushing…"); other long actions wait for it.
    pub(crate) busy: Option<&'static str>,
    pub(crate) notice: Option<GitNotice>,
    pub(crate) message: Entity<TextareaState>,
    pub(crate) branch_input: Entity<InputState>,
    pub(crate) branches: Vec<BranchInfo>,
    pub(crate) branches_open: bool,
    pub(crate) amend: bool,
    pub(crate) staged_collapsed: bool,
    pub(crate) changes_collapsed: bool,
    /// Discard is destructive: the first click arms it (for a few seconds), the second runs it.
    pub(crate) confirm_discard: Option<String>,
    refreshing: bool,
}

impl GitState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<DartIde>) -> Self {
        Self {
            status: None,
            busy: None,
            notice: None,
            message: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .placeholder("Commit message")
                    .auto_grow(2, 8)
            }),
            branch_input: cx.new(|cx| InputState::new(window, cx).placeholder("New branch name…")),
            branches: Vec::new(),
            branches_open: false,
            amend: false,
            staged_collapsed: false,
            changes_collapsed: false,
            confirm_discard: None,
            refreshing: false,
        }
    }
}

/// Git commands from the panel run one at a time.
static GIT_LOCK: Mutex<()> = Mutex::new(());

impl DartIde {
    /// Re-read the repository status (and the branch / file badges the rest of the UI shows).
    pub(crate) fn refresh_git(&mut self, cx: &mut Context<Self>) {
        if self.git.refreshing {
            return;
        }
        self.git.refreshing = true;
        let root = self.root.clone();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _guard = GIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let status = core_workspace::repo_status(&root);
            let branch = core_workspace::current_branch(&root);
            let badges = core_workspace::status_map(&root);
            let _ = tx.send_blocking((status, branch, badges));
        });
        cx.spawn(async move |weak, cx| {
            let result = rx.recv().await;
            let _ = weak.update(cx, |this, cx| {
                this.git.refreshing = false;
                let Ok((status, branch, badges)) = result else {
                    return;
                };
                let changed = this.git.status != status
                    || this.git_branch != branch
                    || this.git_status != badges;
                this.git.status = status;
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
                        if this.sidebar_open && this.sidebar_tab == SidebarTab::Git {
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

    /// Run `job` against the repository root off the UI thread, then refresh. `label` marks a
    /// long action ("Pushing…") as busy and reports `Ok` text as the notice; `None` runs quietly
    /// (stage / unstage), showing only errors.
    fn git_run(
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
                        }
                        this.git.notice = Some(GitNotice {
                            text,
                            is_error: false,
                        });
                    }
                    Err(text) => {
                        this.git.notice = Some(GitNotice {
                            text,
                            is_error: true,
                        })
                    }
                    Ok(_) => {}
                }
                this.refresh_git(cx);
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
        let path = file.path.clone();
        self.git.confirm_discard = Some(path.clone());
        cx.notify();
        cx.spawn(async move |weak, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(3))
                .await;
            let _ = weak.update(cx, |this, cx| {
                if this.git.confirm_discard.as_deref() == Some(path.as_str()) {
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
                    this.git.notice = Some(GitNotice {
                        text,
                        is_error: true,
                    });
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

    /// Commit the staged changes with the message in the box.
    pub(crate) fn git_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.git.message.read(cx).value().to_string();
        let amend = self.git.amend;
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

    pub(crate) fn git_push(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.git_run(window, Some("Pushing…"), false, cx, |root, status| {
            core_workspace::push(root, status)
        });
    }

    pub(crate) fn git_toggle_branches(&mut self, cx: &mut Context<Self>) {
        self.git.branches_open = !self.git.branches_open;
        if self.git.branches_open {
            if let Some(root) = self.git.status.as_ref().map(|s| s.root.clone()) {
                self.git.branches = core_workspace::branches(&root);
            }
        }
        cx.notify();
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
