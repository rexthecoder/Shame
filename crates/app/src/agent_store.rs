//! The agent conversation, saved per workspace.
//!
//! A thread used to disappear when the window closed: the transcript was
//! gone, and so was the CLI's session id, which meant the agent forgot the
//! conversation too. Saving both means reopening a project picks the thread
//! up where it stopped, and the next message resumes the same CLI session.
//!
//! Each workspace keeps a folder of threads (`~/.config/shame/agent/<hash>/`):
//! starting a new one archives the old, and the rail's history panel switches
//! between them. The single file earlier builds wrote is migrated on first
//! read.
//!
//! Pending permission prompts and questions are deliberately not restored:
//! they belong to a process that is no longer running.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use gpui_kit::Context;

use crate::DartIde;
use crate::agent_ctx::ModeId;
use crate::agent_plan::PlanState;
use crate::agent_checkpoint::{Checkpoint, FileChange, TurnFile};
use crate::rails::agent::{
    AgentHarness, AgentItem, AgentState, AgentTask, AgentToolCall, AgentToolKind, ToolStatus,
};

/// Bumped when the on-disk shape changes; older files are still read, newer
/// ones are ignored rather than guessed at.
const VERSION: u32 = 1;

/// A thread longer than this keeps only its most recent turns on disk. The
/// CLI still holds the full session; this bounds the file, not the memory.
const MAX_ITEMS: usize = 400;

/// How many threads a workspace keeps. Older ones are pruned when a new one
/// is saved, so history stays a list you can scan rather than an archive.
const MAX_THREADS: usize = 50;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct StoredConversation {
    pub(crate) version: u32,
    /// Identifies the thread inside the workspace's history folder. Empty in
    /// files written before history existed; those get one when migrated.
    #[serde(default)]
    pub(crate) thread_id: String,
    /// The first thing asked, so the history list doesn't need the transcript.
    #[serde(default)]
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) workspace: String,
    pub(crate) harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session_id: Option<String>,
    #[serde(default)]
    pub(crate) items: Vec<StoredItem>,
    #[serde(default)]
    pub(crate) tasks: Vec<StoredTask>,
    #[serde(default)]
    pub(crate) touched_paths: Vec<String>,
    /// Prompts sent in this thread, for ↑ recall after it is reopened.
    #[serde(default)]
    pub(crate) prompts: Vec<String>,
    /// When this file was written (ms since the epoch), so the transcript can
    /// say "restored · 2h ago".
    #[serde(default)]
    pub(crate) saved_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredTask {
    pub(crate) label: String,
    pub(crate) done: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) active: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum StoredItem {
    Message {
        user: bool,
        text: String,
    },
    ToolGroup {
        header: String,
        collapsed: bool,
        calls: Vec<StoredToolCall>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        thinking: String,
        /// The workspace before the turn: `(repository, tree)`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkpoint: Option<(String, String)>,
    },
    /// What one turn changed, so it can still be undone after a restart.
    Changes {
        repo: String,
        before: String,
        after: String,
        files: Vec<StoredTurnFile>,
    },
    Plan {
        text: String,
        path: Option<String>,
        state: StoredPlanState,
        expanded: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredToolCall {
    /// `true` for a file edit, `false` for a terminal command.
    pub(crate) file: bool,
    pub(crate) label: String,
    pub(crate) target_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) failed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredTurnFile {
    pub(crate) path: String,
    /// `"A"`, `"M"` or `"D"`.
    pub(crate) change: String,
    pub(crate) added: u32,
    pub(crate) removed: u32,
    #[serde(default)]
    pub(crate) reverted: bool,
}

impl StoredTurnFile {
    fn from_file(f: &TurnFile) -> Self {
        Self {
            path: f.path.clone(),
            change: f.change.letter().to_string(),
            added: f.added,
            removed: f.removed,
            reverted: f.reverted,
        }
    }

    fn into_file(self) -> TurnFile {
        TurnFile {
            path: self.path,
            change: match self.change.as_str() {
                "A" => FileChange::Added,
                "D" => FileChange::Deleted,
                _ => FileChange::Modified,
            },
            added: self.added,
            removed: self.removed,
            reverted: self.reverted,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StoredPlanState {
    Pending,
    ApprovedEdit,
    ApprovedAuto,
    KeepPlanning,
}

impl From<PlanState> for StoredPlanState {
    fn from(state: PlanState) -> Self {
        match state {
            PlanState::Pending => Self::Pending,
            PlanState::Approved(ModeId::Auto) => Self::ApprovedAuto,
            PlanState::Approved(_) => Self::ApprovedEdit,
            PlanState::KeepPlanning => Self::KeepPlanning,
        }
    }
}

impl From<StoredPlanState> for PlanState {
    fn from(state: StoredPlanState) -> Self {
        match state {
            StoredPlanState::Pending => PlanState::Pending,
            StoredPlanState::ApprovedEdit => PlanState::Approved(ModeId::Edit),
            StoredPlanState::ApprovedAuto => PlanState::Approved(ModeId::Auto),
            StoredPlanState::KeepPlanning => PlanState::KeepPlanning,
        }
    }
}

impl StoredToolCall {
    fn from_call(call: &AgentToolCall) -> Self {
        Self {
            file: call.kind == AgentToolKind::File,
            label: call.label.clone(),
            target_path: call.target_path.clone(),
            id: call.id.clone(),
            parent_id: call.parent_id.clone(),
            failed: call.status == ToolStatus::Failed,
        }
    }

    fn into_call(self) -> AgentToolCall {
        AgentToolCall {
            kind: if self.file {
                AgentToolKind::File
            } else {
                AgentToolKind::Terminal
            },
            label: self.label,
            target_path: self.target_path,
            id: self.id,
            parent_id: self.parent_id,
            // A call still running when the thread was saved never finished.
            status: if self.failed { ToolStatus::Failed } else { ToolStatus::Done },
        }
    }
}

impl StoredItem {
    /// `None` for the interactive items (permissions, questions) that only
    /// make sense while the process that asked is still alive.
    pub(crate) fn from_item(item: &AgentItem) -> Option<Self> {
        match item {
            AgentItem::Message { is_user, text } => Some(StoredItem::Message {
                user: *is_user,
                text: text.clone(),
            }),
            AgentItem::ToolGroup {
                header,
                collapsed,
                calls,
                thinking,
                checkpoint,
                ..
            } => Some(StoredItem::ToolGroup {
                header: header.clone(),
                collapsed: *collapsed,
                calls: calls.iter().map(StoredToolCall::from_call).collect(),
                thinking: thinking.clone(),
                checkpoint: checkpoint
                    .as_ref()
                    .map(|c| (c.repo.to_string_lossy().to_string(), c.tree.clone())),
            }),
            AgentItem::Changes { before, after, files, .. } => Some(StoredItem::Changes {
                repo: before.repo.to_string_lossy().to_string(),
                before: before.tree.clone(),
                after: after.clone(),
                files: files.iter().map(StoredTurnFile::from_file).collect(),
            }),
            // An error is kept as what it said; retrying belongs to the live thread.
            AgentItem::Error { kind, message, .. } => Some(StoredItem::Message {
                user: false,
                text: format!("**{}.** {message}", kind.title()),
            }),
            AgentItem::Plan {
                text,
                path,
                state,
                expanded,
            } => Some(StoredItem::Plan {
                text: text.clone(),
                path: path.as_ref().map(|p| p.to_string_lossy().to_string()),
                state: (*state).into(),
                expanded: *expanded,
            }),
            AgentItem::Permission { .. } | AgentItem::Question { .. } => None,
        }
    }

    pub(crate) fn into_item(self) -> AgentItem {
        match self {
            StoredItem::Message { user, text } => AgentItem::Message {
                is_user: user,
                text,
            },
            StoredItem::ToolGroup {
                header,
                collapsed,
                calls,
                thinking,
                checkpoint,
            } => AgentItem::ToolGroup {
                id: 0,
                header,
                collapsed,
                calls: calls.into_iter().map(StoredToolCall::into_call).collect(),
                thinking,
                thinking_open: false,
                checkpoint: checkpoint.map(|(repo, tree)| Checkpoint { repo: PathBuf::from(repo), tree }),
            },
            StoredItem::Changes { repo, before, after, files } => AgentItem::Changes {
                before: Checkpoint { repo: PathBuf::from(repo), tree: before },
                after,
                files: files.into_iter().map(StoredTurnFile::into_file).collect(),
                collapsed: true,
            },
            StoredItem::Plan {
                text,
                path,
                state,
                expanded,
            } => AgentItem::Plan {
                text,
                path: path.map(PathBuf::from),
                state: state.into(),
                expanded,
            },
        }
    }
}

impl StoredConversation {
    pub(crate) fn from_state(agent: &AgentState, root: &Path) -> Self {
        let mut items: Vec<StoredItem> = agent
            .items
            .iter()
            .filter_map(StoredItem::from_item)
            .collect();
        cap_items(&mut items);
        Self {
            version: VERSION,
            thread_id: agent.thread_id.clone().unwrap_or_default(),
            title: crate::rails::agent::thread_title_from(&agent.items).unwrap_or_default(),
            workspace: root.to_string_lossy().to_string(),
            harness: harness_key(agent.harness).to_string(),
            model: agent.models.get(agent.model_ix).map(|m| m.id.clone()),
            session_id: agent.session_id.clone(),
            items,
            tasks: agent
                .tasks
                .iter()
                .map(|t| StoredTask {
                    label: t.label.clone(),
                    done: t.done,
                    active: t.active.clone(),
                })
                .collect(),
            touched_paths: agent.ai_touched_paths.clone(),
            prompts: agent.history.clone(),
            saved_at_ms: agent.last_activity_ms.unwrap_or_else(now_ms),
        }
    }

    /// `None` for unreadable JSON or a file from a newer build.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let conv = serde_json::from_slice::<Self>(bytes).ok()?;
        (conv.version <= VERSION).then_some(conv)
    }
}

/// Keep only the most recent turns, so a very long thread doesn't grow the
/// saved file without bound.
fn cap_items(items: &mut Vec<StoredItem>) {
    if items.len() > MAX_ITEMS {
        items.drain(..items.len() - MAX_ITEMS);
    }
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// How long ago a thread was saved, in words: "just now", "12m ago", "3h ago".
/// `None` when the time is unknown (files written before this field existed).
pub(crate) fn format_ago(saved_ms: u64, now_ms: u64) -> Option<String> {
    if saved_ms == 0 {
        return None;
    }
    let secs = now_ms.saturating_sub(saved_ms) / 1000;
    Some(match secs {
        0..=59 => "just now".to_string(),
        60..=3_599 => format!("{}m ago", secs / 60),
        3_600..=86_399 => format!("{}h ago", secs / 3_600),
        _ => format!("{}d ago", secs / 86_400),
    })
}

/// Stable name for a harness in saved state.
pub(crate) fn harness_key(harness: AgentHarness) -> &'static str {
    match harness {
        AgentHarness::ClaudeCode => "claude",
        AgentHarness::OpenCode => "opencode",
        AgentHarness::Codex => "codex",
        AgentHarness::Antigravity => "antigravity",
    }
}

pub(crate) fn harness_from_key(key: &str) -> Option<AgentHarness> {
    match key {
        "claude" => Some(AgentHarness::ClaudeCode),
        "opencode" => Some(AgentHarness::OpenCode),
        "codex" => Some(AgentHarness::Codex),
        "antigravity" => Some(AgentHarness::Antigravity),
        _ => None,
    }
}

/// One row in the rail's history list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThreadSummary {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) saved_at_ms: u64,
    pub(crate) messages: usize,
    pub(crate) harness: Option<AgentHarness>,
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// `~/.config/shame/agent/<hash>/`, one folder of threads per workspace.
pub(crate) fn threads_dir(root: &Path) -> Option<PathBuf> {
    Some(threads_dir_in(&home_dir()?, root))
}

fn threads_dir_in(home: &Path, root: &Path) -> PathBuf {
    // Canonical when possible so `./app` and `/abs/app` share a history.
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut hasher = DefaultHasher::new();
    root.to_string_lossy().hash(&mut hasher);
    home.join(".config")
        .join("shame")
        .join("agent")
        .join(format!("{:016x}", hasher.finish()))
}

/// The single file earlier builds wrote: `.../agent/<hash>.json`.
fn legacy_path_in(home: &Path, root: &Path) -> PathBuf {
    threads_dir_in(home, root).with_extension("json")
}

fn thread_path_in(home: &Path, root: &Path, id: &str) -> PathBuf {
    threads_dir_in(home, root).join(format!("{id}.json"))
}

/// A fresh id for a thread. Milliseconds are enough: two threads cannot start
/// in the same instant from the same workspace.
pub(crate) fn new_thread_id() -> String {
    format!("{:x}", now_ms())
}

fn stored_title(items: &[StoredItem]) -> Option<String> {
    let text = items.iter().find_map(|item| match item {
        StoredItem::Message { user: true, text } => Some(text),
        _ => None,
    })?;
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(crate::rails::agent::truncate_chars(line, 42))
}

fn read_threads_in(dir: &Path) -> Vec<(StoredConversation, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                return None;
            }
            let bytes = std::fs::read(&path).ok()?;
            Some((StoredConversation::from_bytes(&bytes)?, path))
        })
        .collect()
}

fn list_threads_in(dir: &Path) -> Vec<ThreadSummary> {
    let mut threads: Vec<ThreadSummary> = read_threads_in(dir)
        .into_iter()
        .map(|(conv, path)| {
            let id = if conv.thread_id.is_empty() {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string()
            } else {
                conv.thread_id
            };
            let title = if conv.title.is_empty() {
                stored_title(&conv.items).unwrap_or_default()
            } else {
                conv.title
            };
            ThreadSummary {
                id,
                title,
                saved_at_ms: conv.saved_at_ms,
                messages: conv
                    .items
                    .iter()
                    .filter(|item| matches!(item, StoredItem::Message { .. }))
                    .count(),
                harness: harness_from_key(&conv.harness),
            }
        })
        .collect();
    threads.sort_by(|a, b| {
        b.saved_at_ms
            .cmp(&a.saved_at_ms)
            .then_with(|| a.id.cmp(&b.id))
    });
    threads
}

pub(crate) fn list_threads(root: &Path) -> Vec<ThreadSummary> {
    let Some(dir) = threads_dir(root) else {
        return Vec::new();
    };
    list_threads_in(&dir)
}

/// Move the pre-history single file into the workspace's thread folder, once.
fn migrate_legacy_in(home: &Path, root: &Path) {
    let legacy = legacy_path_in(home, root);
    if !legacy.is_file() {
        return;
    }
    let Ok(bytes) = std::fs::read(&legacy) else {
        return;
    };
    let Some(mut conv) = StoredConversation::from_bytes(&bytes) else {
        return;
    };
    if conv.thread_id.is_empty() {
        conv.thread_id = if conv.saved_at_ms > 0 {
            format!("{:x}", conv.saved_at_ms)
        } else {
            new_thread_id()
        };
    }
    let dir = threads_dir_in(home, root);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(format!("{}.json", conv.thread_id));
    if let Ok(json) = serde_json::to_vec_pretty(&conv)
        && std::fs::write(&path, json).is_ok()
    {
        let _ = std::fs::remove_file(&legacy);
    }
}

/// Keep the newest `max` threads, never the one just written.
fn prune_threads_in(dir: &Path, keep_id: &str, max: usize) {
    let mut threads = read_threads_in(dir);
    if threads.len() <= max {
        return;
    }
    threads.sort_by_key(|(conv, _)| conv.saved_at_ms);
    let mut excess = threads.len() - max;
    for (_, path) in threads {
        if excess == 0 {
            break;
        }
        if path.file_stem().and_then(|s| s.to_str()) == Some(keep_id) {
            continue;
        }
        if std::fs::remove_file(path).is_ok() {
            excess -= 1;
        }
    }
}

impl DartIde {
    /// Write the current thread into the workspace's history folder. A thread
    /// that never got a message (or a CLI session) has no file to write.
    pub(crate) fn save_agent_conversation(&self) {
        if !self.has_workspace {
            return;
        }
        let Some(id) = self.agent.thread_id.as_deref().filter(|id| !id.is_empty()) else {
            return;
        };
        let Some(home) = home_dir() else {
            return;
        };
        let path = thread_path_in(&home, &self.root, id);
        let conv = StoredConversation::from_state(&self.agent, &self.root);
        if conv.items.is_empty() && conv.session_id.is_none() {
            let _ = std::fs::remove_file(path);
            return;
        }
        let Some(dir) = path.parent().map(Path::to_path_buf) else {
            return;
        };
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(json) = serde_json::to_vec_pretty(&conv) {
            let _ = std::fs::write(path, json);
        }
        prune_threads_in(&dir, id, MAX_THREADS);
    }

    /// Bring back the most recently used thread for this workspace, if any.
    /// Called after `restore_session_state`, so the saved harness and model win.
    pub(crate) fn restore_agent_conversation(&mut self, cx: &mut Context<Self>) {
        if !self.has_workspace {
            return;
        }
        let Some(home) = home_dir() else {
            return;
        };
        migrate_legacy_in(&home, &self.root);
        let Some(dir) = threads_dir(&self.root) else {
            return;
        };
        let mut threads = read_threads_in(&dir);
        threads.sort_by_key(|(conv, _)| std::cmp::Reverse(conv.saved_at_ms));
        let Some((conv, _)) = threads.into_iter().next() else {
            return;
        };
        if conv.items.is_empty() && conv.session_id.is_none() {
            return;
        }
        self.apply_stored_conversation(conv, cx);
    }

    /// Replace the in-memory thread with a saved one. Shared by startup restore
    /// and by picking a row in the history list.
    pub(crate) fn apply_stored_conversation(
        &mut self,
        conv: StoredConversation,
        cx: &mut Context<Self>,
    ) {
        let saved_at_ms = conv.saved_at_ms;
        if let Some(harness) = harness_from_key(&conv.harness) {
            self.agent.set_harness(harness);
        }
        if let Some(id) = conv.model.as_deref()
            && let Some(ix) = self.agent.models.iter().position(|m| m.id == id)
        {
            self.agent.model_ix = ix;
        }
        // Tool groups are identified by their position in the transcript, so
        // give the restored ones the indexes they now have.
        self.agent.items = conv
            .items
            .into_iter()
            .enumerate()
            .map(|(ix, stored)| {
                let mut item = stored.into_item();
                if let AgentItem::ToolGroup { id, .. } = &mut item {
                    *id = ix;
                }
                item
            })
            .collect();
        self.agent.tasks = conv
            .tasks
            .into_iter()
            .map(|t| AgentTask {
                label: t.label,
                done: t.done,
                active: t.active,
            })
            .collect();
        self.agent.ai_touched_paths = conv.touched_paths;
        self.agent.session_id = conv.session_id;
        self.agent.thread_id = (!conv.thread_id.is_empty()).then_some(conv.thread_id);
        self.agent.history = conv.prompts;
        self.agent.history_ix = None;
        self.agent.restored_items = self.agent.items.len();
        self.agent.restored_at_ms = (saved_at_ms > 0).then_some(saved_at_ms);
        self.agent.last_activity_ms = self.agent.restored_at_ms;
        self.agent.unread_reply = false;
        self.agent.question_inputs.clear();
        self.agent.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Show the thread history, saving whatever is on screen first.
    pub(crate) fn open_agent_history(&mut self, cx: &mut Context<Self>) {
        self.save_agent_conversation();
        self.agent.threads = list_threads(&self.root);
        self.agent.history_open = true;
        cx.notify();
    }

    pub(crate) fn close_agent_history(&mut self, cx: &mut Context<Self>) {
        self.agent.history_open = false;
        cx.notify();
    }

    /// Switch to a saved thread. The thread being left is written first, so
    /// nothing is lost; a run in progress is stopped (it belongs to the old
    /// thread).
    pub(crate) fn load_agent_thread(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.agent.thread_id.as_deref() == Some(id) {
            self.agent.history_open = false;
            cx.notify();
            return;
        }
        let Some(home) = home_dir() else {
            return;
        };
        let Ok(bytes) = std::fs::read(thread_path_in(&home, &self.root, id)) else {
            return;
        };
        let Some(conv) = StoredConversation::from_bytes(&bytes) else {
            return;
        };
        if self.agent.running_pid.is_some() {
            self.stop_agent_run(cx);
        }
        self.save_agent_conversation();
        self.apply_stored_conversation(conv, cx);
        self.agent.history_open = false;
        self.agent.threads.clear();
        cx.notify();
    }

    /// Remove a thread from disk. Deleting the one on screen also clears it.
    pub(crate) fn delete_agent_thread(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(home) = home_dir() else {
            return;
        };
        let _ = std::fs::remove_file(thread_path_in(&home, &self.root, id));
        if self.agent.thread_id.as_deref() == Some(id) {
            if self.agent.running_pid.is_some() {
                self.stop_agent_run(cx);
            }
            self.clear_agent_conversation(cx);
        }
        self.agent.threads.retain(|thread| thread.id != id);
        cx.notify();
    }

    /// Drop the in-memory thread (on switching workspaces) without touching
    /// the file just saved for the workspace being left.
    pub(crate) fn clear_agent_conversation(&mut self, cx: &mut Context<Self>) {
        self.agent.tasks.clear();
        self.agent.items.clear();
        self.agent.ai_touched_paths.clear();
        self.agent.attachments.clear();
        self.agent.pending_mentions.clear();
        self.agent.session_id = None;
        self.agent.thread_id = None;
        self.agent.question_inputs.clear();
        self.agent.turn_stats.clear();
        self.agent.history.clear();
        self.agent.history_ix = None;
        self.agent.threads.clear();
        self.agent.history_open = false;
        self.agent.restored_items = 0;
        self.agent.restored_at_ms = None;
        self.agent.last_activity_ms = None;
        self.agent.unread_reply = false;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StoredConversation {
        StoredConversation {
            version: VERSION,
            thread_id: "t1".into(),
            title: "add a dark mode".into(),
            workspace: "/tmp/app".into(),
            harness: "claude".into(),
            model: Some("claude-sonnet-4-5".into()),
            session_id: Some("ses-1".into()),
            items: vec![
                StoredItem::Message {
                    user: true,
                    text: "add a dark mode".into(),
                },
                StoredItem::ToolGroup {
                    header: "Agent used 2 tools".into(),
                    collapsed: true,
                    thinking: String::new(),
                    checkpoint: None,
                    calls: vec![
                        StoredToolCall {
                            file: true,
                            label: "Edit theme.dart".into(),
                            target_path: Some("lib/theme.dart".into()),
                            id: None,
                            parent_id: None,
                            failed: false,
                        },
                        StoredToolCall {
                            file: false,
                            label: "Run flutter test".into(),
                            target_path: None,
                            id: None,
                            parent_id: None,
                            failed: false,
                        },
                    ],
                },
                StoredItem::Plan {
                    text: "# Dark mode".into(),
                    path: None,
                    state: StoredPlanState::ApprovedAuto,
                    expanded: false,
                },
            ],
            tasks: vec![StoredTask {
                label: "Add the toggle".into(),
                done: true,
                active: None,
            }],
            touched_paths: vec!["lib/theme.dart".into()],
            prompts: vec!["add a dark mode".into()],
            saved_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn a_conversation_round_trips_through_json() {
        let conv = sample();
        let bytes = serde_json::to_vec(&conv).unwrap();
        assert_eq!(StoredConversation::from_bytes(&bytes), Some(conv));
    }

    #[test]
    fn files_from_a_newer_build_and_corrupt_files_are_ignored() {
        assert_eq!(StoredConversation::from_bytes(b"{not json"), None);
        let mut conv = sample();
        conv.version = VERSION + 1;
        let bytes = serde_json::to_vec(&conv).unwrap();
        assert_eq!(StoredConversation::from_bytes(&bytes), None);
    }

    #[test]
    fn items_survive_the_round_trip_through_agent_items() {
        let group = StoredItem::ToolGroup {
            header: "Agent used 1 tool".into(),
            collapsed: false,
            thinking: String::new(),
            checkpoint: None,
            calls: vec![StoredToolCall {
                file: true,
                label: "Write a.dart".into(),
                target_path: Some("a.dart".into()),
                id: None,
                parent_id: None,
                failed: false,
            }],
        };
        let item = group.clone().into_item();
        let AgentItem::ToolGroup { header, calls, .. } = &item else {
            panic!("expected a tool group");
        };
        assert_eq!(header, "Agent used 1 tool");
        assert_eq!(calls[0].kind, AgentToolKind::File);
        assert_eq!(StoredItem::from_item(&item), Some(group));

        let plan = StoredItem::Plan {
            text: "# Do it".into(),
            path: Some("/tmp/plan.md".into()),
            state: StoredPlanState::KeepPlanning,
            expanded: true,
        };
        let item = plan.clone().into_item();
        let AgentItem::Plan {
            state, expanded, ..
        } = &item
        else {
            panic!("expected a plan");
        };
        assert_eq!(*state, PlanState::KeepPlanning);
        assert!(*expanded);
        assert_eq!(StoredItem::from_item(&item), Some(plan));
    }

    #[test]
    fn plan_states_map_both_ways() {
        for state in [
            PlanState::Pending,
            PlanState::Approved(ModeId::Edit),
            PlanState::Approved(ModeId::Auto),
            PlanState::KeepPlanning,
        ] {
            let stored = StoredPlanState::from(state);
            assert_eq!(PlanState::from(stored), state);
        }
    }

    #[test]
    fn very_long_threads_keep_their_most_recent_turns() {
        let mut items: Vec<StoredItem> = (0..MAX_ITEMS + 5)
            .map(|i| StoredItem::Message {
                user: i % 2 == 0,
                text: format!("message {i}"),
            })
            .collect();
        cap_items(&mut items);
        assert_eq!(items.len(), MAX_ITEMS);
        assert_eq!(
            items[0],
            StoredItem::Message {
                user: false,
                text: "message 5".into()
            }
        );
    }

    #[test]
    fn a_file_without_the_saved_time_still_loads() {
        let json = r#"{"version":1,"workspace":"/tmp/app","harness":"claude","items":[{"kind":"message","user":true,"text":"hi"}]}"#;
        let conv = StoredConversation::from_bytes(json.as_bytes()).unwrap();
        assert_eq!(conv.saved_at_ms, 0);
        assert_eq!(format_ago(conv.saved_at_ms, 1_700_000_000_000), None);
    }

    #[test]
    fn ages_are_short_and_relative() {
        let now = 1_700_000_000_000;
        assert_eq!(format_ago(0, now), None);
        assert_eq!(format_ago(now, now).as_deref(), Some("just now"));
        assert_eq!(
            format_ago(now - 5 * 60_000, now).as_deref(),
            Some("5m ago")
        );
        assert_eq!(
            format_ago(now - 3 * 3_600_000, now).as_deref(),
            Some("3h ago")
        );
        assert_eq!(
            format_ago(now - 2 * 86_400_000, now).as_deref(),
            Some("2d ago")
        );
    }

    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shame-agent-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_thread(dir: &Path, conv: &StoredConversation) {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{}.json", conv.thread_id));
        std::fs::write(path, serde_json::to_vec_pretty(conv).unwrap()).unwrap();
    }

    #[test]
    fn each_workspace_gets_its_own_history_folder() {
        let home = Path::new("/home/me");
        let a = threads_dir_in(home, Path::new("/projects/a"));
        let b = threads_dir_in(home, Path::new("/projects/b"));
        assert_ne!(a, b);
        assert_eq!(a, threads_dir_in(home, Path::new("/projects/a")));
        assert!(a.starts_with(home.join(".config/shame/agent")));
        assert!(thread_path_in(home, Path::new("/projects/a"), "t1").starts_with(&a));
        assert_eq!(
            legacy_path_in(home, Path::new("/projects/a")),
            a.with_extension("json")
        );
    }

    #[test]
    fn threads_are_listed_newest_first_with_titles() {
        let home = temp_home("history");
        let root = home.join("project");
        let dir = threads_dir_in(&home, &root);

        let mut older = sample();
        older.thread_id = "older".into();
        older.title = "Make the sidebar collapsible".into();
        older.saved_at_ms = 1_700_000_000_000;
        let mut newer = sample();
        newer.thread_id = "newer".into();
        newer.title = String::new(); // older files may not carry a title
        newer.saved_at_ms = 1_700_000_100_000;
        write_thread(&dir, &older);
        write_thread(&dir, &newer);

        let listed = list_threads_in(&dir);
        assert_eq!(
            listed.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["newer", "older"]
        );
        assert_eq!(listed[0].title, "add a dark mode");
        assert_eq!(listed[0].messages, 1);
        assert_eq!(listed[0].harness, Some(AgentHarness::ClaudeCode));
        assert_eq!(listed[1].title, "Make the sidebar collapsible");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_single_legacy_file_becomes_a_thread() {
        let home = temp_home("legacy");
        let root = home.join("project");
        let legacy = legacy_path_in(&home, &root);
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        let mut conv = sample();
        conv.thread_id = String::new();
        conv.title = String::new();
        std::fs::write(&legacy, serde_json::to_vec_pretty(&conv).unwrap()).unwrap();

        migrate_legacy_in(&home, &root);

        assert!(!legacy.exists(), "the old file is moved, not copied");
        let listed = list_threads_in(&threads_dir_in(&home, &root));
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, format!("{:x}", conv.saved_at_ms));
        assert_eq!(listed[0].title, "add a dark mode");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn only_the_newest_threads_are_kept() {
        let home = temp_home("prune");
        let root = home.join("project");
        let dir = threads_dir_in(&home, &root);
        for (ix, saved) in [
            1_700_000_000_000u64,
            1_700_000_100_000,
            1_700_000_200_000,
        ]
        .iter()
        .enumerate()
        {
            let mut conv = sample();
            conv.thread_id = format!("t{ix}");
            conv.saved_at_ms = *saved;
            write_thread(&dir, &conv);
        }

        prune_threads_in(&dir, "t2", 2);

        let listed = list_threads_in(&dir);
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(|t| t.id == "t2"), "the newest survives");
        assert!(listed.iter().any(|t| t.id == "t1"));
        assert!(!listed.iter().any(|t| t.id == "t0"));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn harness_names_are_stable() {
        for harness in [
            AgentHarness::ClaudeCode,
            AgentHarness::OpenCode,
            AgentHarness::Codex,
            AgentHarness::Antigravity,
        ] {
            assert_eq!(harness_from_key(harness_key(harness)), Some(harness));
        }
        assert_eq!(harness_from_key("nope"), None);
    }
}
