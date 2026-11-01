//! The agent conversation, saved per workspace.
//!
//! A thread used to disappear when the window closed: the transcript was
//! gone, and so was the CLI's session id, which meant the agent forgot the
//! conversation too. Saving both means reopening a project picks the thread
//! up where it stopped, and the next message resumes the same CLI session.
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
use crate::rails::agent::{
    AgentHarness, AgentItem, AgentState, AgentTask, AgentToolCall, AgentToolKind,
};

/// Bumped when the on-disk shape changes; older files are still read, newer
/// ones are ignored rather than guessed at.
const VERSION: u32 = 1;

/// A thread longer than this keeps only its most recent turns on disk. The
/// CLI still holds the full session; this bounds the file, not the memory.
const MAX_ITEMS: usize = 400;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct StoredConversation {
    pub(crate) version: u32,
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredTask {
    pub(crate) label: String,
    pub(crate) done: bool,
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
                ..
            } => Some(StoredItem::ToolGroup {
                header: header.clone(),
                collapsed: *collapsed,
                calls: calls.iter().map(StoredToolCall::from_call).collect(),
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
            } => AgentItem::ToolGroup {
                id: 0,
                header,
                collapsed,
                calls: calls.into_iter().map(StoredToolCall::into_call).collect(),
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
                })
                .collect(),
            touched_paths: agent.ai_touched_paths.clone(),
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

/// Stable name for a harness in saved state.
pub(crate) fn harness_key(harness: AgentHarness) -> &'static str {
    match harness {
        AgentHarness::ClaudeCode => "claude",
        AgentHarness::OpenCode => "opencode",
        AgentHarness::Codex => "codex",
    }
}

pub(crate) fn harness_from_key(key: &str) -> Option<AgentHarness> {
    match key {
        "claude" => Some(AgentHarness::ClaudeCode),
        "opencode" => Some(AgentHarness::OpenCode),
        "codex" => Some(AgentHarness::Codex),
        _ => None,
    }
}

/// `~/.config/shame/agent/<hash>.json`, one file per workspace.
pub(crate) fn conversation_path(root: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(conversation_path_in(Path::new(&home), root))
}

fn conversation_path_in(home: &Path, root: &Path) -> PathBuf {
    // Canonical when possible so `./app` and `/abs/app` share a thread.
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut hasher = DefaultHasher::new();
    root.to_string_lossy().hash(&mut hasher);
    home.join(".config")
        .join("shame")
        .join("agent")
        .join(format!("{:016x}.json", hasher.finish()))
}

impl DartIde {
    /// Write the current thread next to the workspace's other saved state.
    /// An empty thread removes the file, so "New thread" really forgets it.
    pub(crate) fn save_agent_conversation(&self) {
        if !self.has_workspace {
            return;
        }
        let Some(path) = conversation_path(&self.root) else {
            return;
        };
        let conv = StoredConversation::from_state(&self.agent, &self.root);
        if conv.items.is_empty() && conv.session_id.is_none() {
            let _ = std::fs::remove_file(path);
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_vec_pretty(&conv) {
            let _ = std::fs::write(path, json);
        }
    }

    /// Bring back the thread saved for this workspace, if any. Called after
    /// `restore_session_state`, so the saved harness and model win.
    pub(crate) fn restore_agent_conversation(&mut self, cx: &mut Context<Self>) {
        if !self.has_workspace {
            return;
        }
        let Some(path) = conversation_path(&self.root) else {
            return;
        };
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let Some(conv) = StoredConversation::from_bytes(&bytes) else {
            return;
        };
        if conv.items.is_empty() {
            return;
        }
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
            })
            .collect();
        self.agent.ai_touched_paths = conv.touched_paths;
        self.agent.session_id = conv.session_id;
        self.agent.question_inputs.clear();
        self.agent.scroll.scroll_to_bottom();
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
        self.agent.question_inputs.clear();
        self.agent.turn_stats.clear();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StoredConversation {
        StoredConversation {
            version: VERSION,
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
                    calls: vec![
                        StoredToolCall {
                            file: true,
                            label: "Edit theme.dart".into(),
                            target_path: Some("lib/theme.dart".into()),
                        },
                        StoredToolCall {
                            file: false,
                            label: "Run flutter test".into(),
                            target_path: None,
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
            }],
            touched_paths: vec!["lib/theme.dart".into()],
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
            calls: vec![StoredToolCall {
                file: true,
                label: "Write a.dart".into(),
                target_path: Some("a.dart".into()),
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
    fn each_workspace_gets_its_own_file() {
        let home = Path::new("/home/me");
        let a = conversation_path_in(home, Path::new("/projects/a"));
        let b = conversation_path_in(home, Path::new("/projects/b"));
        assert_ne!(a, b);
        assert_eq!(a, conversation_path_in(home, Path::new("/projects/a")));
        assert!(a.starts_with(home.join(".config/shame/agent")));
    }

    #[test]
    fn harness_names_are_stable() {
        for harness in [
            AgentHarness::ClaudeCode,
            AgentHarness::OpenCode,
            AgentHarness::Codex,
        ] {
            assert_eq!(harness_from_key(harness_key(harness)), Some(harness));
        }
        assert_eq!(harness_from_key("nope"), None);
    }
}
