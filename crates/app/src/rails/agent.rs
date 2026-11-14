//! Multi-harness AI Agent rail (`docs/design-spec.md` §3.23–31): lets the developer choose
//! between **Claude Code** (`claude`), **OpenCode** (`opencode`), and **Codex** (`codex`), pick a
//! model under the selected tool, stream tool calls and assistant responses live, and review
//! pending workspace changes.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{InputState, TextareaState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::agent_ctx::{
    AGENT_MODES, Attachment, ModeId, QueuedPrompt, applescript_escape, attachment_prompt,
    notification_body, parse_slash_command,
};
use crate::{DartIde, tokens};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentHarness {
    ClaudeCode,
    OpenCode,
    Codex,
    Antigravity,
}

#[derive(Clone, Debug)]
pub(crate) struct ModelOption {
    pub(crate) id: String,
    pub(crate) label: String,
}

impl AgentHarness {
    pub(crate) const ALL: &[AgentHarness] = &[
        AgentHarness::ClaudeCode,
        AgentHarness::OpenCode,
        AgentHarness::Codex,
        AgentHarness::Antigravity,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            AgentHarness::ClaudeCode => "Claude Code",
            AgentHarness::OpenCode => "OpenCode",
            AgentHarness::Codex => "Codex",
            AgentHarness::Antigravity => "Gemini (Antigravity)",
        }
    }

    pub(crate) fn short_label(self) -> &'static str {
        match self {
            AgentHarness::ClaudeCode => "Claude",
            AgentHarness::OpenCode => "OpenCode",
            AgentHarness::Codex => "Codex",
            AgentHarness::Antigravity => "Gemini",
        }
    }

    /// Resolve the executable path for this harness on the user's machine, if installed.
    pub(crate) fn resolve_binary(self) -> Option<PathBuf> {
        let home = std::env::var("HOME").ok().map(PathBuf::from);
        let candidates: Vec<PathBuf> = match self {
            AgentHarness::ClaudeCode => {
                let mut v = Vec::new();
                if let Some(h) = &home {
                    v.push(h.join(".local/bin/claude"));
                    v.push(h.join(".claude/local/claude"));
                }
                v.push(PathBuf::from("/opt/homebrew/bin/claude"));
                v.push(PathBuf::from("/usr/local/bin/claude"));
                v
            }
            AgentHarness::OpenCode => {
                let mut v = vec![PathBuf::from(
                    "/Applications/OpenChamber.app/Contents/Resources/opencode-cli/opencode",
                )];
                if let Some(h) = &home {
                    v.push(h.join(".opencode/bin/opencode"));
                    v.push(h.join(".local/bin/opencode"));
                }
                v.push(PathBuf::from("/opt/homebrew/bin/opencode"));
                v.push(PathBuf::from("/usr/local/bin/opencode"));
                v
            }
            AgentHarness::Codex => {
                let mut v = Vec::new();
                if let Some(h) = &home {
                    v.push(h.join(".bun/bin/codex"));
                    v.push(h.join(".local/bin/codex"));
                }
                v.push(PathBuf::from(
                    "/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex",
                ));
                v.push(PathBuf::from("/opt/homebrew/bin/codex"));
                v.push(PathBuf::from("/usr/local/bin/codex"));
                v
            }
            AgentHarness::Antigravity => {
                let mut v = Vec::new();
                if let Some(h) = &home {
                    // Google's installer drops `agy` here and does not always put it on PATH.
                    v.push(h.join(".gemini/bin/agy"));
                    v.push(h.join(".local/bin/agy"));
                }
                v.push(PathBuf::from("/opt/homebrew/bin/agy"));
                v.push(PathBuf::from("/usr/local/bin/agy"));
                v
            }
        };
        for p in candidates {
            if p.is_file() {
                return Some(p);
            }
        }
        let bin_name = match self {
            AgentHarness::ClaudeCode => "claude",
            AgentHarness::OpenCode => "opencode",
            AgentHarness::Codex => "codex",
            AgentHarness::Antigravity => "agy",
        };
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(bin_name))
                .find(|p| p.is_file())
        })
    }

    /// Available models under this coding tool (discovering cached models from disk when present).
    pub(crate) fn models(self) -> Vec<ModelOption> {
        match self {
            AgentHarness::ClaudeCode => {
                // (id passed to `--model`, label). The bare aliases always track
                // the newest release of that family; the dated ids pin one.
                [
                    ("default", "Default (your account)"),
                    ("claude-fable-5-1", "Claude Fable 5.1"),
                    ("claude-opus-5-5", "Claude Opus 5.5"),
                    ("claude-sonnet-5-5", "Claude Sonnet 5.5"),
                    ("claude-haiku-5-5", "Claude Haiku 5.5"),
                    ("opus", "Opus (latest alias)"),
                    ("sonnet", "Sonnet (latest alias)"),
                    ("haiku", "Haiku (latest alias)"),
                    ("claude-opus-4-5", "Claude Opus 4.5"),
                    ("claude-sonnet-4-5", "Claude Sonnet 4.5"),
                    ("claude-opus-4-1", "Claude Opus 4.1"),
                    ("claude-haiku-4-5", "Claude Haiku 4.5"),
                ]
                .into_iter()
                .map(|(id, label)| ModelOption { id: id.into(), label: label.into() })
                .collect()
            }
            AgentHarness::OpenCode => vec![
                ModelOption {
                    id: "opencode-go/gemini-4-argon".into(),
                    label: "Gemini 4 Argon".into(),
                },
                ModelOption {
                    id: "opencode-go/deepseek-v4.1-flash".into(),
                    label: "DeepSeek V4.1 Flash".into(),
                },
                ModelOption {
                    id: "opencode-go/deepseek-v4-pro".into(),
                    label: "DeepSeek V4 Pro".into(),
                },
                ModelOption {
                    id: "opencode-go/claude-haiku-5-5".into(),
                    label: "Claude Haiku 5.5".into(),
                },
                ModelOption {
                    id: "opencode/big-pickle".into(),
                    label: "Big Pickle".into(),
                },
            ],
            AgentHarness::Antigravity => [
                // Ids as printed by `agy models`.
                ("gemini-4-argon-medium", "Gemini 4 Argon (Medium)"),
                ("gemini-4-argon-high", "Gemini 4 Argon (High)"),
                ("gemini-4-argon-low", "Gemini 4 Argon (Low)"),
                ("gemini-4-hydrogen-high", "Gemini 4 Hydrogen (High)"),
                ("gemini-4-hydrogen-xhigh", "Gemini 4 Hydrogen (XHigh)"),
                ("gemini-4-hydrogen-medium", "Gemini 4 Hydrogen (Medium)"),
                ("gemini-4-hydrogen-low", "Gemini 4 Hydrogen (Low)"),
                ("gemini-3.8-flash-high", "Gemini 3.8 Flash (High)"),
                ("gemini-3.8-flash-medium", "Gemini 3.8 Flash (Medium)"),
                ("gemini-3.8-flash-low", "Gemini 3.8 Flash (Low)"),
                ("gemini-pro-orchestrator-g1-pro", "Orchestrator (Argon)"),
            ]
            .into_iter()
            .map(|(id, label)| ModelOption { id: id.into(), label: label.into() })
            .collect(),
            AgentHarness::Codex => {
                let mut list = load_codex_cached_models();
                if list.is_empty() {
                    list = vec![
                        ModelOption {
                            id: "gpt-6-luna".into(),
                            label: "GPT-6 Luna".into(),
                        },
                        ModelOption {
                            id: "gpt-5.6-terra".into(),
                            label: "GPT-5.6 Terra".into(),
                        },
                        ModelOption {
                            id: "gpt-5.6-luna".into(),
                            label: "GPT-5.6 Luna".into(),
                        },
                        ModelOption {
                            id: "gpt-reserve".into(),
                            label: "GPT-Reserve".into(),
                        },
                    ];
                }
                list
            }
        }
    }
}

fn load_codex_cached_models() -> Vec<ModelOption> {
    let Ok(home) = std::env::var("HOME") else {
        return Vec::new();
    };
    let path = Path::new(&home).join(".codex/models_cache.json");
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    let Some(arr) = val.get("models").and_then(|m| m.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|m| {
            let slug = m.get("slug")?.as_str()?.trim();
            if slug.is_empty() || slug == "codex-auto-review" {
                return None;
            }
            let display = m
                .get("display_name")
                .and_then(|d| d.as_str())
                .unwrap_or(slug);
            Some(ModelOption {
                id: slug.to_string(),
                label: display.to_string(),
            })
        })
        .collect()
}

/// Indices of `models` matching `query` (case-insensitive, over label and id).
/// Every word of the query must appear, so `sonnet 4.5` finds "Claude Sonnet 4.5".
pub(crate) fn filter_models(models: &[ModelOption], query: &str) -> Vec<usize> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    models
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            let hay = format!("{} {}", m.label, m.id).to_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .map(|(ix, _)| ix)
        .collect()
}

/// One entry of the agent's own task list (Claude's `TodoWrite`, Codex's
/// `todo_list`), shown pinned above the composer while it works.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentTask {
    pub(crate) label: String,
    pub(crate) done: bool,
    /// What it is doing right now ("Running the tests"), for the one in progress.
    pub(crate) active: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum AgentToolKind {
    #[default]
    File,
    Terminal,
}

/// How a tool call ended, once the CLI reports its result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ToolStatus {
    #[default]
    Running,
    Done,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentToolCall {
    pub(crate) kind: AgentToolKind,
    pub(crate) label: String,
    pub(crate) target_path: Option<String>,
    /// The CLI's id for the call, to match its result and its helper's calls.
    pub(crate) id: Option<String>,
    /// Set on calls made by a helper agent: the id of the `Task` call that started it.
    pub(crate) parent_id: Option<String>,
    pub(crate) status: ToolStatus,
}

/// Why a turn failed, so the card can offer the right way out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ErrorKind {
    RateLimit,
    Auth,
    Overloaded,
    Network,
    Other,
}

impl ErrorKind {
    pub(crate) fn classify(message: &str) -> Self {
        let m = message.to_ascii_lowercase();
        let has = |words: &[&str]| words.iter().any(|w| m.contains(w));
        if has(&["rate limit", "rate_limit", "429", "usage limit", "quota", "too many requests"]) {
            ErrorKind::RateLimit
        } else if has(&[
            "not logged in",
            "authentication",
            "unauthorized",
            "401",
            "api key",
            "invalid_api_key",
            "/login",
        ]) {
            ErrorKind::Auth
        } else if has(&["overloaded", "529", "503", "capacity"]) {
            ErrorKind::Overloaded
        } else if has(&["network", "connection", "timed out", "timeout", "econn", "dns", "offline"]) {
            ErrorKind::Network
        } else {
            ErrorKind::Other
        }
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            ErrorKind::RateLimit => "Rate limit reached",
            ErrorKind::Auth => "Not signed in",
            ErrorKind::Overloaded => "The model is overloaded",
            ErrorKind::Network => "Connection problem",
            ErrorKind::Other => "The agent stopped with an error",
        }
    }

    pub(crate) fn hint(self, harness: AgentHarness) -> String {
        match self {
            ErrorKind::RateLimit => "Wait a moment and retry, or switch to another model.".to_string(),
            ErrorKind::Auth => format!(
                "Sign in to {} in a terminal (run `{}` and use /login), then retry.",
                harness.label(),
                harness.short_label()
            ),
            ErrorKind::Overloaded => "Retry in a few seconds, or pick a different model.".to_string(),
            ErrorKind::Network => "Check your connection, then retry.".to_string(),
            ErrorKind::Other => "Retry, or switch the model or tool.".to_string(),
        }
    }
}

/// Token use the CLI reported, for the context meter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TokenUsage {
    /// Tokens the model read on its latest request: how full the context is.
    pub(crate) context: Option<u64>,
    pub(crate) output: Option<u64>,
    /// The model's context window, when the CLI says.
    pub(crate) window: Option<u64>,
}

/// What the app sends to a running Claude Code process on its stdin.
pub(crate) enum AgentInput {
    Line(String),
    /// No more input: the turn is over, let the process exit.
    Close,
}

#[derive(Clone)]
pub(crate) enum AgentItem {
    /// One turn's work: the tools it used, what it thought, and the state of
    /// the workspace before it started.
    ToolGroup {
        id: usize,
        header: String,
        collapsed: bool,
        calls: Vec<AgentToolCall>,
        /// The model's reasoning, when the CLI shares it.
        thinking: String,
        thinking_open: bool,
        /// The workspace before this turn, to undo it or rewind to it.
        checkpoint: Option<crate::agent_checkpoint::Checkpoint>,
    },
    /// What one turn changed in the workspace, with undo.
    Changes {
        before: crate::agent_checkpoint::Checkpoint,
        after: String,
        files: Vec<crate::agent_checkpoint::TurnFile>,
        collapsed: bool,
    },
    /// A turn that failed, with a way to retry it.
    Error {
        kind: ErrorKind,
        message: String,
        /// The user message that started the failed turn.
        prompt_ix: Option<usize>,
    },
    Message {
        is_user: bool,
        text: String,
    },
    /// The agent asking to edit a file or run a command (Ask mode).
    Permission {
        request: crate::agent_permission::PermissionRequest,
        summary: crate::agent_permission::PermissionSummary,
        state: crate::agent_permission::PermissionState,
    },
    /// The agent asking you a multiple-choice question (`AskUserQuestion`).
    Question {
        request: crate::agent_permission::PermissionRequest,
        questions: Vec<crate::agent_permission::Question>,
        /// What you have chosen, per question.
        choices: Vec<crate::agent_permission::Choice>,
        state: crate::agent_permission::PermissionState,
    },
    /// The agent's plan, waiting for approval (Plan mode).
    Plan {
        text: String,
        /// The plan file the agent wrote, if it wrote one.
        path: Option<PathBuf>,
        state: crate::agent_plan::PlanState,
        /// Whether the body is open once the plan has been decided.
        expanded: bool,
    },
}

#[derive(Debug, PartialEq)]
pub(crate) enum StreamFragment {
    Tool(AgentToolCall),
    TextDelta(String),
    ReplaceText(String),
    /// The CLI's id for this conversation, used to resume it on the next message.
    SessionId(String),
    /// The plan the agent passed when it finished planning (Claude Code's `ExitPlanMode`).
    Plan(String),
    /// The agent is waiting to be told it may do something.
    Permission(crate::agent_permission::PermissionRequest),
    /// What the CLI reported about the finished turn (Claude Code only).
    Stats { cost_usd: Option<f64>, duration_ms: Option<u64> },
    /// More of the model's reasoning.
    Thinking(String),
    /// The agent's task list, in full, as it stands now.
    Todos(Vec<AgentTask>),
    /// Tokens used so far.
    Usage(TokenUsage),
    /// A tool call finished (matched by id).
    ToolResult { id: String, is_error: bool },
    /// The CLI reported the turn failed.
    TurnError(String),
    /// Something worth a line in the transcript ("Conversation compacted").
    Notice(String),
}

fn u64_at(val: &serde_json::Value, key: &str) -> Option<u64> {
    val.get(key).and_then(|v| v.as_u64())
}

/// Context tokens from an Anthropic-style `usage` object: everything the
/// model read, cached or not.
fn claude_context_tokens(usage: &serde_json::Value) -> Option<u64> {
    let total = u64_at(usage, "input_tokens").unwrap_or(0)
        + u64_at(usage, "cache_creation_input_tokens").unwrap_or(0)
        + u64_at(usage, "cache_read_input_tokens").unwrap_or(0);
    (total > 0).then_some(total)
}

fn claude_todos(input: Option<&serde_json::Value>) -> Option<Vec<AgentTask>> {
    let todos = input?.get("todos")?.as_array()?;
    Some(
        todos
            .iter()
            .filter_map(|t| {
                let label = t.get("content").and_then(|v| v.as_str())?.to_string();
                let status = t.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
                let active = (status == "in_progress").then(|| {
                    t.get("activeForm")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&label)
                        .to_string()
                });
                Some(AgentTask { label, done: status == "completed", active })
            })
            .collect(),
    )
}

/// Parse one line of output (either `claude --output-format stream-json`, `opencode run --format json`,
/// `codex exec --json`, or plain text fallback) into structured `StreamFragment`s.
pub(crate) fn parse_agent_stream_line(raw: &str) -> Vec<StreamFragment> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return vec![StreamFragment::TextDelta(format!("{trimmed}\n"))];
    };

    // Antigravity (`agy --output-format stream-json`) keys its events by `event`, not `type`.
    if let Some(event) = val.get("event").and_then(|v| v.as_str()) {
        return parse_antigravity_event(event, &val);
    }

    let mut out = Vec::new();
    let ty = val.get("type").and_then(|v| v.as_str()).unwrap_or("");

    // Claude Code stamps `session_id` on its `system` and `result` events;
    // Codex announces its conversation as `thread.started` / `thread_id`.
    let session_id = match ty {
        "system" | "result" => val.get("session_id").and_then(|v| v.as_str()),
        "thread.started" => val.get("thread_id").and_then(|v| v.as_str()),
        _ => None,
    };
    if let Some(id) = session_id.filter(|id| !id.is_empty()) {
        out.push(StreamFragment::SessionId(id.to_string()));
    }

    // A helper agent's messages carry the id of the `Task` call that started it.
    let parent_id = val
        .get("parent_tool_use_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    match ty {
        // 1. Claude Code `stream-json` assistant message
        "assistant" => {
            let message = val.get("message");
            if parent_id.is_none()
                && let Some(usage) = message.and_then(|m| m.get("usage"))
                && let Some(context) = claude_context_tokens(usage)
            {
                out.push(StreamFragment::Usage(TokenUsage {
                    context: Some(context),
                    output: u64_at(usage, "output_tokens"),
                    window: None,
                }));
            }
            if let Some(content) = message.and_then(|m| m.get("content")).and_then(|c| c.as_array()) {
                for part in content {
                    match part.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                        // A helper's narration is not the answer: only its tool calls show.
                        "text" if parent_id.is_none() => {
                            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                                if !t.trim().is_empty() {
                                    out.push(StreamFragment::ReplaceText(t.to_string()));
                                }
                            }
                        }
                        "thinking" if parent_id.is_none() => {
                            if let Some(t) = part.get("thinking").and_then(|v| v.as_str())
                                && !t.trim().is_empty()
                            {
                                out.push(StreamFragment::Thinking(t.to_string()));
                            }
                        }
                        "tool_use" => {
                            let name = part
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Tool");
                            let input = part.get("input");
                            if name == "ExitPlanMode" {
                                if let Some(plan) = input
                                    .and_then(|i| i.get("plan"))
                                    .and_then(|p| p.as_str())
                                    .filter(|p| !p.trim().is_empty())
                                {
                                    out.push(StreamFragment::Plan(plan.to_string()));
                                }
                            }
                            if name == "TodoWrite" && parent_id.is_none() {
                                if let Some(todos) = claude_todos(input) {
                                    out.push(StreamFragment::Todos(todos));
                                }
                                continue; // the checklist shows it; a tool row would be noise
                            }
                            let mut call = tool_call_from_name_and_input(name, input);
                            call.id = part.get("id").and_then(|v| v.as_str()).map(str::to_string);
                            call.parent_id = parent_id.clone();
                            out.push(StreamFragment::Tool(call));
                        }
                        _ => {}
                    }
                }
            }
        }
        // Claude Code tool results come back as a `user` message.
        "user" => {
            if let Some(content) = val
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            {
                for part in content {
                    if part.get("type").and_then(|v| v.as_str()) == Some("tool_result")
                        && let Some(id) = part.get("tool_use_id").and_then(|v| v.as_str())
                    {
                        out.push(StreamFragment::ToolResult {
                            id: id.to_string(),
                            is_error: part.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false),
                        });
                    }
                }
            }
        }
        "system" => {
            if val.get("subtype").and_then(|v| v.as_str()) == Some("compact_boundary") {
                let before = val
                    .get("compact_metadata")
                    .and_then(|m| u64_at(m, "pre_tokens"))
                    .map(|t| format!(" (was {})", crate::agent_ctx::format_tokens(t)))
                    .unwrap_or_default();
                out.push(StreamFragment::Notice(format!("Conversation compacted{before}")));
                out.push(StreamFragment::Usage(TokenUsage { context: Some(0), ..Default::default() }));
            }
        }
        // Claude Code asking permission to use a tool (`--permission-prompt-tool stdio`)
        "control_request" => {
            if let Some(request) = crate::agent_permission::parse_permission_request(&val) {
                out.push(StreamFragment::Permission(request));
            }
        }
        // Claude Code final result summary
        "result" => {
            let cost_usd = val
                .get("total_cost_usd")
                .or_else(|| val.get("cost_usd"))
                .and_then(|v| v.as_f64());
            let duration_ms = val.get("duration_ms").and_then(|v| v.as_u64());
            if cost_usd.is_some() || duration_ms.is_some() {
                out.push(StreamFragment::Stats { cost_usd, duration_ms });
            }
            // `modelUsage` names each model's context window.
            let window = val
                .get("modelUsage")
                .and_then(|m| m.as_object())
                .and_then(|models| models.values().filter_map(|m| u64_at(m, "contextWindow")).max());
            if window.is_some() {
                out.push(StreamFragment::Usage(TokenUsage { window, ..Default::default() }));
            }
            let text = val.get("result").and_then(|v| v.as_str()).unwrap_or("");
            if val.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false) {
                let reason = val
                    .get("terminal_reason")
                    .or_else(|| val.get("subtype"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("error");
                let message = if text.trim().is_empty() {
                    match reason {
                        "error_max_turns" => "It reached the maximum number of turns.".to_string(),
                        "api_error" => "The model API returned an error.".to_string(),
                        other => other.replace('_', " "),
                    }
                } else {
                    text.trim().to_string()
                };
                out.push(StreamFragment::TurnError(message));
            } else if !text.trim().is_empty() {
                out.push(StreamFragment::ReplaceText(text.to_string()));
            }
        }
        // 2. OpenCode `--format json` events
        "reasoning" => {
            if let Some(t) = val
                .get("part")
                .and_then(|p| p.get("text"))
                .or_else(|| val.get("text"))
                .and_then(|v| v.as_str())
                .filter(|t| !t.trim().is_empty())
            {
                out.push(StreamFragment::Thinking(t.to_string()));
            }
        }
        "step_finish" => {
            if let Some(tokens) = val.get("part").and_then(|p| p.get("tokens")) {
                let cache = tokens.get("cache");
                let context = u64_at(tokens, "input").unwrap_or(0)
                    + cache.and_then(|c| u64_at(c, "read")).unwrap_or(0)
                    + cache.and_then(|c| u64_at(c, "write")).unwrap_or(0);
                if context > 0 {
                    out.push(StreamFragment::Usage(TokenUsage {
                        context: Some(context),
                        output: u64_at(tokens, "output"),
                        window: None,
                    }));
                }
            }
        }
        "error" => {
            let message = val
                .get("message")
                .or_else(|| val.get("error").and_then(|e| e.get("message").or(Some(e))))
                .and_then(|v| v.as_str())
                .unwrap_or("The agent reported an error.");
            out.push(StreamFragment::TurnError(message.to_string()));
        }
        // Codex
        "turn.failed" => {
            let message = val
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("The turn failed.");
            out.push(StreamFragment::TurnError(message.to_string()));
        }
        "turn.completed" => {
            if let Some(usage) = val.get("usage") {
                out.push(StreamFragment::Usage(TokenUsage {
                    context: u64_at(usage, "input_tokens"),
                    output: u64_at(usage, "output_tokens"),
                    window: None,
                }));
            }
        }
        "text" => {
            if let Some(t) = val
                .get("part")
                .and_then(|p| p.get("text"))
                .or_else(|| val.get("text"))
                .and_then(|v| v.as_str())
            {
                if !t.is_empty() {
                    out.push(StreamFragment::TextDelta(t.to_string()));
                }
            }
        }
        "tool_use" | "tool" => {
            let part = val.get("part").unwrap_or(&val);
            let name = part
                .get("tool")
                .or_else(|| part.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("tool");
            let input = part
                .get("state")
                .and_then(|s| s.get("input"))
                .or_else(|| part.get("input"));
            out.push(StreamFragment::Tool(tool_call_from_name_and_input(
                name, input,
            )));
        }
        // 3. Codex `--json` events (`item.started` / `item.completed`)
        "item.started" | "item.updated" | "item.completed" => {
            if let Some(item) = val.get("item") {
                let item_ty = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match item_ty {
                    "reasoning" if ty == "item.completed" => {
                        if let Some(t) = item.get("text").and_then(|v| v.as_str())
                            && !t.trim().is_empty()
                        {
                            out.push(StreamFragment::Thinking(t.to_string()));
                        }
                    }
                    "todo_list" => {
                        if let Some(items) = item.get("items").and_then(|v| v.as_array()) {
                            let mut first_open = true;
                            let todos = items
                                .iter()
                                .filter_map(|t| {
                                    let label = t.get("text").and_then(|v| v.as_str())?.to_string();
                                    let done = t.get("completed").and_then(|v| v.as_bool()).unwrap_or(false);
                                    // Codex has no "in progress": the first open item is it.
                                    let active = (!done && std::mem::take(&mut first_open)).then(|| label.clone());
                                    Some(AgentTask { label, done, active })
                                })
                                .collect();
                            out.push(StreamFragment::Todos(todos));
                        }
                    }
                    "error" if ty == "item.completed" => {
                        if let Some(m) = item.get("message").and_then(|v| v.as_str()) {
                            out.push(StreamFragment::Notice(m.to_string()));
                        }
                    }
                    "agent_message" | "message" if ty == "item.completed" => {
                        if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                            if !t.trim().is_empty() {
                                out.push(StreamFragment::ReplaceText(t.to_string()));
                            }
                        }
                    }
                    "command_execution" if ty == "item.started" => {
                        let cmd = item
                            .get("command")
                            .and_then(|v| v.as_str())
                            .unwrap_or("command");
                        out.push(StreamFragment::Tool(AgentToolCall {
                            kind: AgentToolKind::Terminal,
                            label: cmd.to_string(),
                            target_path: None,
                            ..Default::default()
                        }));
                    }
                    "file_change" if ty == "item.completed" => {
                        let path = item
                            .get("file_path")
                            .or_else(|| item.get("path"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("file");
                        out.push(StreamFragment::Tool(AgentToolCall {
                            kind: AgentToolKind::File,
                            label: format!("Edit {path}"),
                            target_path: Some(path.to_string()),
                            ..Default::default()
                        }));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}

/// One NDJSON line from `agy --output-format stream-json`: `init`, `step_update`, `result`.
fn parse_antigravity_event(event: &str, val: &serde_json::Value) -> Vec<StreamFragment> {
    let mut out = Vec::new();
    match event {
        "init" => {
            if let Some(id) = val
                .get("conversation_id")
                .and_then(|v| v.as_str())
                .filter(|id| !id.is_empty())
            {
                out.push(StreamFragment::SessionId(id.to_string()));
            }
        }
        "step_update" => {
            let Some(step) = val.get("step_update") else {
                return out;
            };
            let state = step.get("state").and_then(|v| v.as_str()).unwrap_or("");
            match step.get("step_type").and_then(|v| v.as_str()).unwrap_or("") {
                "agent_response" => {
                    if let Some(t) = step.get("text_delta").and_then(|v| v.as_str()) {
                        if !t.is_empty() {
                            out.push(StreamFragment::TextDelta(t.to_string()));
                        }
                    }
                }
                // Report each tool once, when it starts.
                "tool" if state == "ACTIVE" => {
                    let info = step.get("tool_info");
                    let name = step
                        .get("tool_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool");
                    let params = info.and_then(|i| i.get("parameters"));
                    let param = |keys: &[&str]| {
                        params.and_then(|p| {
                            keys.iter().find_map(|k| p.get(*k).and_then(|v| v.as_str()))
                        })
                    };
                    if name == "run_command" {
                        let cmd = param(&["CommandLine", "command"]).unwrap_or(name);
                        out.push(StreamFragment::Tool(AgentToolCall {
                            kind: AgentToolKind::Terminal,
                            label: cmd.lines().next().unwrap_or(cmd).to_string(),
                            target_path: None,
                            ..Default::default()
                        }));
                    } else if let Some(path) = param(&[
                        "AbsolutePath",
                        "TargetFile",
                        "file_path",
                        "path",
                        "DirectoryPath",
                    ]) {
                        let short = Path::new(path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(path);
                        let verb = match name {
                            "view_file" => "Read",
                            "list_dir" => "List",
                            "write_to_file" => "Write",
                            "replace_file_content" | "multi_replace_file_content" | "sed_file" => {
                                "Edit"
                            }
                            _ => name,
                        };
                        out.push(StreamFragment::Tool(AgentToolCall {
                            kind: AgentToolKind::File,
                            label: format!("{verb} {short}"),
                            target_path: Some(path.to_string()),
                            ..Default::default()
                        }));
                    } else {
                        out.push(StreamFragment::Tool(tool_call_from_name_and_input(name, params)));
                    }
                }
                _ => {}
            }
        }
        "result" => {
            let duration_ms = val
                .get("result")
                .and_then(|r| r.get("duration_seconds"))
                .and_then(|v| v.as_f64())
                .map(|s| (s * 1000.0) as u64);
            if duration_ms.is_some() {
                out.push(StreamFragment::Stats { cost_usd: None, duration_ms });
            }
        }
        _ => {}
    }
    out
}

fn tool_call_from_name_and_input(
    name: &str,
    input: Option<&serde_json::Value>,
) -> AgentToolCall {
    let lower = name.to_ascii_lowercase();
    if lower.contains("bash") || lower.contains("shell") || lower.contains("exec") {
        let cmd = input
            .and_then(|i| i.get("command").or_else(|| i.get("cmd")))
            .and_then(|v| v.as_str())
            .unwrap_or(name);
        return AgentToolCall {
            kind: AgentToolKind::Terminal,
            label: cmd.lines().next().unwrap_or(cmd).to_string(),
            target_path: None,
            ..Default::default()
        };
    }
    let file_path = input
        .and_then(|i| {
            i.get("file_path")
                .or_else(|| i.get("path"))
                .or_else(|| i.get("pattern"))
        })
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let short_file = file_path
        .as_deref()
        .and_then(|p| Path::new(p).file_name().and_then(|n| n.to_str()))
        .or(file_path.as_deref())
        .unwrap_or("");
    let verb = if lower.contains("read") {
        "Read"
    } else if lower.contains("edit") || lower.contains("patch") {
        "Edit"
    } else if lower.contains("write") {
        "Write"
    } else {
        name
    };
    let label = if short_file.is_empty() {
        verb.to_string()
    } else {
        format!("{verb} {short_file}")
    };
    AgentToolCall {
        kind: AgentToolKind::File,
        label,
        target_path: file_path,
        ..Default::default()
    }
}

/// What to tell the user the agent is doing, from the tool call it just made.
pub(crate) fn activity_label(call: Option<&AgentToolCall>) -> String {
    let Some(call) = call else { return "Thinking".to_string() };
    if call.kind == AgentToolKind::Terminal {
        return "Running a command".to_string();
    }
    let (verb, rest) = call.label.split_once(' ').unwrap_or((call.label.as_str(), ""));
    let doing = match verb {
        "Read" => "Reading",
        "Edit" => "Editing",
        "Write" => "Writing",
        "Grep" | "Glob" | "Search" => "Searching",
        "ExitPlanMode" => "Finishing the plan",
        "AskUserQuestion" => "Preparing a question",
        _ => return "Working".to_string(),
    };
    if rest.is_empty() { doing.to_string() } else { format!("{doing} {rest}") }
}

pub(crate) enum AgentStreamEvent {
    Started { pid: u32 },
    /// The workspace as it was before the turn (git repositories only).
    Checkpoint(crate::agent_checkpoint::Checkpoint),
    /// What the turn changed, measured once it ended.
    TurnChanges {
        before: crate::agent_checkpoint::Checkpoint,
        after: String,
        files: Vec<crate::agent_checkpoint::TurnFile>,
    },
    Fragment(StreamFragment),
    StderrLine(String),
    Finished { error: Option<String> },
}

/// How long a turn took, and what it cost when the CLI says so.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TurnStats {
    pub(crate) duration_ms: u64,
    pub(crate) cost_usd: Option<f64>,
}

pub(crate) struct AgentState {
    pub(crate) harness: AgentHarness,
    pub(crate) models: Vec<ModelOption>,
    pub(crate) model_ix: usize,
    pub(crate) mode_ix: usize,
    pub(crate) tool_picker_open: bool,
    pub(crate) model_picker_open: bool,
    pub(crate) running_pid: Option<u32>,
    pub(crate) tasks: Vec<AgentTask>,
    pub(crate) items: Vec<AgentItem>,
    pub(crate) ai_touched_paths: Vec<String>,
    /// Conversation id of the running thread (Claude Code / Codex), so the next
    /// message resumes it instead of starting a fresh, memoryless process.
    pub(crate) session_id: Option<String>,
    pub(crate) input: Entity<TextareaState>,
    pub(crate) scroll: ScrollHandle,
    /// Files and selections attached to the *next* message.
    pub(crate) attachments: Vec<Attachment>,
    /// Files and selections mentioned inline (`@name` pills in the text),
    /// keyed by the pill's id, plus the next id to hand out.
    pub(crate) mentions: std::collections::HashMap<String, Attachment>,
    pub(crate) next_mention: usize,
    /// Mentions resolved from the text of the message being sent.
    pub(crate) pending_mentions: Vec<Attachment>,
    /// While an `@query` is being typed: its byte range in the input.
    pub(crate) mention_range: Option<std::ops::Range<usize>>,
    /// The animated "working" row shown while a run is in progress.
    pub(crate) indicator: Entity<crate::agent_indicator::ThinkingIndicator>,
    /// The way to answer the running Claude Code process (permission answers).
    pub(crate) control_tx: Option<async_channel::Sender<AgentInput>>,
    /// The text boxes behind the Other answers, by (message index, question index).
    pub(crate) question_inputs: std::collections::HashMap<(usize, usize), Entity<InputState>>,
    /// Ask mode was explained once for a tool that can't ask for approval.
    pub(crate) ask_notice_shown: bool,
    /// Time and cost of each finished turn, by the index of its reply.
    pub(crate) turn_stats: std::collections::HashMap<usize, TurnStats>,
    /// Messages typed while the agent was busy; sent one by one when it finishes.
    pub(crate) queue: Vec<QueuedPrompt>,
    pub(crate) last_mention_query: String,
    /// Tell the agent which file is open (and which tabs). Toggled by the chip
    /// in the composer header.
    pub(crate) include_active_file: bool,
    /// Send current editor diagnostics as context.
    pub(crate) include_problems: bool,
    /// Send the active selection automatically, without attaching it by hand.
    pub(crate) include_selection: bool,
    pub(crate) attach_open: bool,
    pub(crate) attach_search: Entity<InputState>,
    pub(crate) attach_selected: usize,
    pub(crate) model_search: Entity<InputState>,
    pub(crate) model_selected: usize,
    /// Previously sent prompts (oldest first) and the one being recalled with ↑.
    pub(crate) history: Vec<String>,
    pub(crate) history_ix: Option<usize>,
    /// How many leading items came back from disk, and when that thread was
    /// saved: the transcript shows a "restored" banner above them.
    pub(crate) restored_items: usize,
    pub(crate) restored_at_ms: Option<u64>,
    /// When the thread last changed, so an idle quit doesn't make an old
    /// conversation look new on the next launch.
    pub(crate) last_activity_ms: Option<u64>,
    /// A turn finished, or the agent needs an answer, while the rail was
    /// closed; the titlebar icon shows a dot until it is opened.
    pub(crate) unread_reply: bool,
    /// The saved thread on screen, once it has a message (or a CLI session).
    pub(crate) thread_id: Option<String>,
    /// The history panel: whether it is open, and the rows last read from disk.
    pub(crate) history_open: bool,
    pub(crate) threads: Vec<crate::agent_store::ThreadSummary>,
    /// How full the model's context is, from the latest usage the CLI reported.
    pub(crate) usage: TokenUsage,
    /// What each user message asked, with its attachments, by item index, so
    /// it can be edited or retried exactly.
    pub(crate) turn_inputs: std::collections::HashMap<usize, TurnInput>,
    /// The conversation was rewound or edited: the CLI's session still holds
    /// the turns that were removed, so the next message starts a new session
    /// and carries the remaining conversation as text.
    pub(crate) replay_transcript: bool,
    /// Send the next prompt to the CLI as typed (`/compact`), without the IDE context.
    pub(crate) raw_next: bool,
    /// The user pressed Stop: the run ending is not an error.
    pub(crate) stop_requested: bool,
    pub(crate) tasks_collapsed: bool,
    /// Helper-agent (`Task`) calls whose own calls are shown, by call id.
    pub(crate) open_subagents: std::collections::HashSet<String>,
}

/// A sent message, as typed, with what was attached to it.
#[derive(Clone, Debug)]
pub(crate) struct TurnInput {
    pub(crate) prompt: String,
    pub(crate) attachments: Vec<Attachment>,
}

impl AgentState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<DartIde>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Ask anything\u{2026}")
                .auto_grow(1, 8)
        });
        let attach_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search files to attach\u{2026}"));
        let model_search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search models, or type a model id\u{2026}")
        });
        let default_harness = if AgentHarness::ClaudeCode.resolve_binary().is_some() {
            AgentHarness::ClaudeCode
        } else if AgentHarness::OpenCode.resolve_binary().is_some() {
            AgentHarness::OpenCode
        } else {
            AgentHarness::Codex
        };
        let models = default_harness.models();
        Self {
            harness: default_harness,
            models,
            model_ix: 0,
            mode_ix: ModeId::Edit.index(),
            tool_picker_open: false,
            model_picker_open: false,
            running_pid: None,
            tasks: Vec::new(),
            items: Vec::new(),
            ai_touched_paths: Vec::new(),
            session_id: None,
            input,
            scroll: ScrollHandle::new(),
            attachments: Vec::new(),
            mentions: std::collections::HashMap::new(),
            next_mention: 0,
            pending_mentions: Vec::new(),
            mention_range: None,
            indicator: cx.new(crate::agent_indicator::ThinkingIndicator::new),
            control_tx: None,
            question_inputs: std::collections::HashMap::new(),
            ask_notice_shown: false,
            turn_stats: std::collections::HashMap::new(),
            queue: Vec::new(),
            last_mention_query: String::new(),
            include_active_file: true,
            include_problems: true,
            include_selection: true,
            attach_open: false,
            attach_search,
            attach_selected: 0,
            model_search,
            model_selected: 0,
            history: Vec::new(),
            history_ix: None,
            restored_items: 0,
            restored_at_ms: None,
            last_activity_ms: None,
            unread_reply: false,
            thread_id: None,
            history_open: false,
            threads: Vec::new(),
            usage: TokenUsage::default(),
            turn_inputs: std::collections::HashMap::new(),
            replay_transcript: false,
            raw_next: false,
            stop_requested: false,
            tasks_collapsed: false,
            open_subagents: std::collections::HashSet::new(),
        }
    }

    pub(crate) fn record_ai_touched_path(&mut self, root: &Path, raw_path: &str) {
        let p = Path::new(raw_path);
        let rel = if p.is_absolute() {
            p.strip_prefix(root)
                .unwrap_or(p)
                .to_string_lossy()
                .to_string()
        } else {
            raw_path.trim_start_matches("./").to_string()
        };
        if !rel.is_empty() && !self.ai_touched_paths.iter().any(|existing| existing == &rel) {
            self.ai_touched_paths.push(rel);
        }
    }

    pub(crate) fn set_harness(&mut self, harness: AgentHarness) {
        if self.harness != harness {
            // A session id only means something to the CLI that issued it.
            self.session_id = None;
        }
        self.harness = harness;
        self.models = harness.models();
        self.model_ix = 0;
        self.tool_picker_open = false;
    }

    /// Whether the conversation is scrolled to its end, so streamed text should
    /// keep following; a reader who scrolled up is left alone.
    pub(crate) fn is_pinned_to_bottom(&self) -> bool {
        near_bottom(
            self.scroll.offset().y.to_f64(),
            self.scroll.max_offset().y.to_f64(),
            40.0,
        )
    }

    pub(crate) fn active_model(&self) -> (&str, &str) {
        self.models
            .get(self.model_ix)
            .map(|m| (m.id.as_str(), m.label.as_str()))
            .unwrap_or(("default", "Default"))
    }

    /// A short title for the thread: the first thing you asked, one line.
    pub(crate) fn thread_title(&self) -> Option<String> {
        thread_title_from(&self.items)
    }
}

pub(crate) fn thread_title_from(items: &[AgentItem]) -> Option<String> {
    let text = items.iter().find_map(|item| match item {
        AgentItem::Message { is_user: true, text } => Some(text),
        _ => None,
    })?;
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(truncate_chars(line, 42))
}

pub(crate) fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// Whether a scroll view is at (or within `slack` px of) its bottom. `offset_y`
/// is the content offset (zero or negative) and `max_y` the scrollable extent.
pub(crate) fn near_bottom(offset_y: f64, max_y: f64, slack: f64) -> bool {
    max_y <= 0.0 || -offset_y >= max_y - slack
}

/// Earlier turns as plain text, for CLIs we can't resume natively: the last
/// `max_messages` user / assistant messages, each cut to `max_chars`.
/// Placeholders ("Thinking…") and empty messages are skipped.
pub(crate) fn conversation_transcript(
    items: &[AgentItem],
    max_messages: usize,
    max_chars: usize,
) -> String {
    let mut turns: Vec<String> = items
        .iter()
        .filter_map(|item| match item {
            AgentItem::Message { is_user, text } => {
                let text = text.trim();
                if text.is_empty() || text == "Thinking\u{2026}" {
                    return None;
                }
                let clipped: String = text.chars().take(max_chars).collect();
                let ellipsis = if text.chars().count() > max_chars { "\u{2026}" } else { "" };
                Some(format!("{}: {clipped}{ellipsis}", if *is_user { "User" } else { "Assistant" }))
            }
            AgentItem::ToolGroup { .. }
            | AgentItem::Changes { .. }
            | AgentItem::Error { .. }
            | AgentItem::Plan { .. }
            | AgentItem::Permission { .. }
            | AgentItem::Question { .. } => None,
        })
        .collect();
    let skip = turns.len().saturating_sub(max_messages);
    turns.drain(..skip);
    turns.join("\n\n")
}

/// The bundle id of the `.app` this process runs from, read from its
/// Info.plist. `None` for an unbundled build (`cargo run`).
fn app_bundle_id() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let plist = exe.parent()?.parent()?.join("Info.plist");
    let text = std::fs::read_to_string(plist).ok()?;
    let after = &text[text.find("CFBundleIdentifier</key>")?..];
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")? + start;
    Some(after[start..end].trim().to_string())
}

/// A macOS notification, fire and forget. Sent natively under the app's own
/// bundle id so it shows as Shame; `osascript` (which macOS attributes to
/// "Script Editor") is only the fallback for unbundled dev runs.
fn notify_desktop(title: &str, body: &str) {
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        #[cfg(target_os = "macos")]
        if let Some(id) = app_bundle_id() {
            if mac_notification_sys::set_application(&id).is_ok()
                && mac_notification_sys::send_notification(&title, None, &body, None).is_ok()
            {
                return;
            }
        }
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            applescript_escape(&body),
            applescript_escape(&title)
        );
        let _ = Command::new("osascript")
            .args(["-e", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    });
}

impl DartIde {
    /// Open or close the agent rail. Opening it counts as reading whatever
    /// the titlebar dot was pointing at.
    pub(crate) fn set_agent_open(&mut self, open: bool) {
        self.agent_open = open;
        if open {
            self.agent.unread_reply = false;
            // Opening the rail shows the thread, not whichever panel was up.
            self.agent.history_open = false;
        }
    }

    pub(crate) fn stop_agent_run(&mut self, cx: &mut Context<Self>) {
        // Stopping means "stop", not "go on to the next queued message".
        self.agent.queue.clear();
        if self.agent.running_pid.is_some() {
            self.agent.stop_requested = true;
        }
        self.cancel_pending_permissions();
        if let Some(tx) = self.agent.control_tx.take() {
            let _ = tx.send_blocking(AgentInput::Close);
        }
        if let Some(pid) = self.agent.running_pid.take() {
            // The CLI runs in its own process group, so a stop reaches the
            // commands it spawned (test runners, dev servers, MCP servers),
            // not just the CLI itself.
            #[cfg(unix)]
            unsafe {
                libc::kill(-(pid as i32), libc::SIGTERM);
            }
            #[cfg(not(unix))]
            {
                let _ = Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .status();
            }
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(1200));
                #[cfg(unix)]
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            });
            cx.notify();
        }
    }

    /// Permission cards still waiting when a run ends or is stopped can no longer be answered.
    fn cancel_pending_permissions(&mut self) {
        for item in &mut self.agent.items {
            if let AgentItem::Permission { state, .. } | AgentItem::Question { state, .. } = item {
                if *state == crate::agent_permission::PermissionState::Pending {
                    *state = crate::agent_permission::PermissionState::Cancelled;
                }
            }
        }
    }

    pub(crate) fn agent_send_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.agent.input.read(cx).value().trim().to_string();
        if prompt.is_empty() {
            return;
        }
        // Inline @mentions in the text become attachments for this message.
        let mut mentioned: Vec<Attachment> = Vec::new();
        for span in self.agent.input.read(cx).tokens() {
            if let Some(found) = self.agent.mentions.get(span.token().id().as_ref()) {
                crate::agent_ctx::add_attachment(&mut mentioned, found.clone());
            }
        }
        self.agent
            .input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.agent.mentions.clear();
        self.agent.mention_range = None;
        self.agent.attach_open = false;
        if let Some(command) = parse_slash_command(&prompt) {
            self.run_slash_command(command, window, cx);
            return;
        }
        // Sending means you are back in the conversation, not browsing it.
        self.agent.history_open = false;
        if self.agent.history.last() != Some(&prompt) {
            self.agent.history.push(prompt.clone());
        }
        self.agent.history_ix = None;
        if self.agent.running_pid.is_some() {
            // Busy: keep the message (with its attachments) for when the run ends.
            let shelf = std::mem::take(&mut self.agent.attachments);
            self.agent.queue.push(QueuedPrompt { prompt, shelf, mentions: mentioned });
            cx.notify();
            return;
        }
        self.agent.pending_mentions = mentioned;
        self.agent_run_prompt(prompt, cx);
    }

    /// Send the next queued message, if any.
    fn run_next_queued(&mut self, cx: &mut Context<Self>) {
        if self.agent.running_pid.is_some() || self.agent.queue.is_empty() {
            return;
        }
        let next = self.agent.queue.remove(0);
        self.agent.attachments = next.shelf;
        self.agent.pending_mentions = next.mentions;
        self.agent_run_prompt(next.prompt, cx);
    }

    /// The text for one live-context mention, ending in a newline.
    fn context_block(&self, kind: crate::agent_ctx::ContextKind, cx: &App) -> String {
        use crate::agent_ctx::ContextKind;
        match kind {
            ContextKind::Problems => {
                let mut lines: Vec<String> = Vec::new();
                for tab in &self.tabs {
                    let Some(path) = tab.path.as_ref() else { continue };
                    let rel = path.strip_prefix(&self.root).unwrap_or(path).to_string_lossy();
                    for d in tab.editor.read(cx).diagnostics() {
                        let sev = match d.severity {
                            Some(core_lsp::lsp_types::DiagnosticSeverity::ERROR) => "ERROR",
                            Some(core_lsp::lsp_types::DiagnosticSeverity::WARNING) => "WARN",
                            _ => "INFO",
                        };
                        let msg = d.message.lines().next().unwrap_or(&d.message).trim();
                        lines.push(format!(
                            "{sev} {rel}:{}:{}: {msg}",
                            d.range.start.line + 1,
                            d.range.start.character + 1
                        ));
                        if lines.len() >= 60 {
                            break;
                        }
                    }
                }
                if lines.is_empty() {
                    "Problems: none reported in the open files.\n".to_string()
                } else {
                    format!("Problems in open files:\n{}\n", lines.join("\n"))
                }
            }
            ContextKind::GitDiff => {
                let out = Command::new("git")
                    .args(["diff", "HEAD", "--no-color"])
                    .current_dir(&self.root)
                    .stdin(Stdio::null())
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default();
                if out.trim().is_empty() {
                    return "Uncommitted changes: none.\n".to_string();
                }
                let (shown, cut) = crate::agent_ctx::clip_text(&out, 64 * 1024);
                format!(
                    "Uncommitted changes (`git diff HEAD`{}):\n```diff\n{}\n```\n",
                    if cut { ", truncated" } else { "" },
                    shown.trim_end()
                )
            }
            ContextKind::Terminal => {
                let text = self
                    .terminals
                    .get(self.active_terminal)
                    .map(|t| crate::agent_ctx::terminal_text(&t.lines, 120))
                    .unwrap_or_default();
                if text.trim().is_empty() {
                    return "Terminal: no output yet.\n".to_string();
                }
                let (shown, cut) = crate::agent_ctx::clip_text(&text, 24 * 1024);
                format!(
                    "Recent terminal output{}:\n```\n{}\n```\n",
                    if cut { " (truncated)" } else { "" },
                    shown.trim_end()
                )
            }
        }
    }

    pub(crate) fn agent_run_prompt(&mut self, prompt: String, cx: &mut Context<Self>) {
        if self.agent.running_pid.is_some() {
            return;
        }
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        self.agent.tool_picker_open = false;
        self.agent.model_picker_open = false;
        // Claude Code and Codex resume their own conversation by id. OpenCode
        // is stateless per run here, so it gets the earlier turns as text.
        let replay = std::mem::take(&mut self.agent.replay_transcript);
        let raw = std::mem::take(&mut self.agent.raw_next);
        let resume_id = match self.agent.harness {
            _ if replay => None,
            AgentHarness::ClaudeCode | AgentHarness::Codex | AgentHarness::Antigravity => {
                self.agent.session_id.clone()
            }
            AgentHarness::OpenCode => None,
        };
        let prior_transcript = if replay {
            conversation_transcript(&self.agent.items, 16, 4000)
        } else if self.agent.harness == AgentHarness::OpenCode {
            conversation_transcript(&self.agent.items, 8, 1500)
        } else {
            String::new()
        };
        let was_resumed = resume_id.is_some();
        // Attachments belong to this one message; show them in the transcript.
        let attachments = std::mem::take(&mut self.agent.attachments);
        let mentions = std::mem::take(&mut self.agent.pending_mentions);
        let shown_prompt = if attachments.is_empty() {
            prompt.clone()
        } else {
            let names: Vec<String> =
                attachments.iter().map(|a| format!("`{}`", a.label())).collect();
            format!("{prompt}\n\n_Attached:_ {}", names.join(", "))
        };
        self.agent.attach_open = false;
        let user_ix = self.agent.items.len();
        self.agent.turn_inputs.insert(
            user_ix,
            TurnInput {
                prompt: prompt.clone(),
                attachments: attachments.iter().chain(mentions.iter()).cloned().collect(),
            },
        );
        self.agent.stop_requested = false;
        self.agent.items.push(AgentItem::Message {
            is_user: true,
            text: shown_prompt,
        });
        self.agent.scroll.scroll_to_bottom();
        self.agent.last_activity_ms = Some(crate::agent_store::now_ms());
        // The first message gives the thread an identity in the history list.
        if self.agent.thread_id.is_none() {
            self.agent.thread_id = Some(crate::agent_store::new_thread_id());
        }
        self.agent.history_open = false;
        // Save the prompt now: if the app is quit mid-run, the turn is still
        // in the thread when it comes back.
        self.save_agent_conversation();
        let include_active_file = self.agent.include_active_file;
        let include_problems = self.agent.include_problems;
        // The selection is the most specific thing the user is looking at, and asking
        // for it by hand every turn is noise. Share it by default; the composer chip
        // turns it off for the next message.
        let auto_selection = if self.agent.include_selection && include_active_file {
            self.active_editor().and_then(|editor| {
                let ed = editor.read(cx);
                let path = ed.buffer().path()?;
                let (start_line, end_line, text) = ed.selection_for_context()?;
                let rel = path
                    .strip_prefix(&self.root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_string();
                Some(Attachment::Selection { rel, start_line, end_line, text })
            })
        } else {
            None
        };

        let harness = self.agent.harness;
        let (model_id, _) = self.agent.active_model();
        let model_id = model_id.to_string();
        let mode = AGENT_MODES[self.agent.mode_ix % AGENT_MODES.len()].id;
        let root = self.root.clone();
        let active_file = self
            .active_editor()
            .filter(|_| include_active_file)
            .and_then(|ed| ed.read(cx).buffer().path().map(Path::to_path_buf))
            .and_then(|p| p.strip_prefix(&self.root).ok().map(Path::to_path_buf))
            .map(|p| p.to_string_lossy().to_string());

        let open_tabs: Vec<String> = self
            .tabs
            .iter()
            .filter(|_| include_active_file)
            .filter_map(|t| t.path.as_ref())
            .filter_map(|p| p.strip_prefix(&self.root).ok())
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        let mut diag_lines: Vec<String> = Vec::new();
        for tab in self.tabs.iter().filter(|_| include_problems) {
            let Some(path) = tab.path.as_ref() else {
                continue;
            };
            let rel = path
                .strip_prefix(&self.root)
                .unwrap_or(path)
                .to_string_lossy();
            for d in tab.editor.read(cx).diagnostics().iter().take(6) {
                let sev = match d.severity {
                    Some(core_lsp::lsp_types::DiagnosticSeverity::ERROR) => "ERROR",
                    Some(core_lsp::lsp_types::DiagnosticSeverity::WARNING) => "WARN",
                    _ => "INFO",
                };
                let l = d.range.start.line + 1;
                let c = d.range.start.character + 1;
                let msg = d.message.lines().next().unwrap_or(&d.message).trim();
                diag_lines.push(format!("{sev} {rel}:{l}:{c}: {msg}"));
                if diag_lines.len() >= 10 {
                    break;
                }
            }
            if diag_lines.len() >= 10 {
                break;
            }
        }

        let full_header = format!(
            "[Shame IDE Environment & Tooling]\n- Workspace root: {}\n- Git branch: {}\n- Active file: {}\n- Open editor tabs: {}\n- Available IDE & CLI tooling you have full access to:\n  1. Workspace Files & Diff Review: Read/Edit/Write any file in the workspace; edited files automatically appear in Shame's AI Changes diff reviewer.\n  2. Multi-Language Toolchains (21 languages): `dart` / `flutter` (including `.fvm/flutter_sdk/bin/flutter`, `flutter test`, `dart run build_runner build --delete-conflicting-outputs`, `flutter gen-l10n`), `cargo` / `rustc`, `bun` / `node` / `npm` / `tsc`, `python3`, `go`, `swift` / `xcodebuild`, `clang++`, `ruby` / `pod`, `zig`.\n  3. iOS Simulator & Mobile Tooling: `xcrun simctl` (list/boot devices, `xcrun simctl io booted screenshot` for visual verification).\n  4. Web Preview & Headless Browser: Shame's built-in Web Preview panel (`{}`) and headless Chrome (`/Applications/Google Chrome.app/Contents/MacOS/Google Chrome --headless=new --screenshot`).\n  5. Git & Shell: Full `git` CLI and shell command execution in `{}`.",
            self.root.display(),
            self.git_branch.as_deref().unwrap_or("detached"),
            active_file.as_deref().unwrap_or("none"),
            if open_tabs.is_empty() {
                "none".to_string()
            } else {
                open_tabs.join(", ")
            },
            self.web_preview_url.read(cx).value(),
            self.root.display(),
        );
        // A resumed conversation already has the long tooling briefing; send only
        // what changes between turns.
        let mut ctx_header = if was_resumed {
            format!(
                "[Shame IDE context update]\n- Git branch: {}\n- Active file: {}\n- Open editor tabs: {}",
                self.git_branch.as_deref().unwrap_or("detached"),
                active_file.as_deref().unwrap_or("none"),
                if open_tabs.is_empty() { "none".to_string() } else { open_tabs.join(", ") },
            )
        } else {
            full_header
        };
        if !diag_lines.is_empty() {
            ctx_header.push_str("\n- Active LSP Diagnostics in open files:\n  ");
            ctx_header.push_str(&diag_lines.join("\n  "));
        }
        let bridge_prompt = self.agent_bridge_prompt();
        if !bridge_prompt.is_empty() {
            ctx_header.push_str("\n\n");
            ctx_header.push_str(&bridge_prompt);
        }

        // Order: environment, mode rule, attachments, earlier turns, the request.
        let mut sections = vec![ctx_header];
        if let Some(rule) = mode.instruction(harness == AgentHarness::ClaudeCode) {
            sections.push(rule.to_string());
        }
        let mut everything: Vec<Attachment> =
            attachments.iter().chain(mentions.iter()).cloned().collect();
        if let Some(selection) = auto_selection
            && !everything.contains(&selection)
        {
            everything.push(selection);
        }
        let mut attached = attachment_prompt(&self.root, &everything);
        // Live IDE state (`@problems`, `@diff`, `@terminal`) is read now, at send time.
        for attachment in &everything {
            if let Attachment::Context(kind) = attachment {
                attached.push_str(&self.context_block(*kind, cx));
            }
        }
        if !attached.is_empty() {
            sections.push(attached.trim_end().to_string());
        }
        if !prior_transcript.is_empty() {
            sections.push(format!("[Conversation so far]\n{prior_transcript}"));
        }
        sections.push(format!("[User Request]\n{prompt}"));
        // `/compact` and friends go to the CLI exactly as typed.
        let full_prompt = if raw { prompt.clone() } else { sections.join("\n\n") };

        let Some(bin_path) = harness.resolve_binary() else {
            self.agent.items.push(AgentItem::Message {
                is_user: false,
                text: format!(
                    "`{}` CLI was not found on this machine. Switch tooling or install `{}`.",
                    harness.label(),
                    harness.short_label()
                ),
            });
            cx.notify();
            return;
        };

        if mode == ModeId::Ask && harness != AgentHarness::ClaudeCode && !self.agent.ask_notice_shown {
            self.agent.ask_notice_shown = true;
            self.agent.items.push(AgentItem::Message {
                is_user: false,
                text: format!(
                    "Ask mode is read-only with {}: it can\u{2019}t stop and ask you to approve changes the way Claude Code can. Switch to Edit to let it change files.",
                    harness.label()
                ),
            });
        }

        // Reserve a tool group + assistant response bubble for live streaming
        let group_id = self.agent.items.len();
        self.agent.items.push(AgentItem::ToolGroup {
            id: group_id,
            header: format!("{} is working\u{2026}", harness.label()),
            collapsed: false,
            calls: Vec::new(),
            thinking: String::new(),
            thinking_open: false,
            checkpoint: None,
        });
        let msg_ix = self.agent.items.len();
        self.agent.items.push(AgentItem::Message {
            is_user: false,
            text: "Thinking\u{2026}".to_string(),
        });
        self.agent.indicator.update(cx, |indicator, cx| indicator.start("Thinking", cx));
        cx.notify();

        let pre_run_unstaged: std::collections::HashSet<String> = self
            .git
            .status
            .as_ref()
            .map(|s| s.unstaged.iter().map(|f| f.path.clone()).collect())
            .unwrap_or_default();

        let (tx, rx) = async_channel::unbounded::<AgentStreamEvent>();
        // Claude Code is run with streaming input so it can stop and ask for approval:
        // we write the message on its stdin, and later the answers to its questions.
        let (in_tx, in_rx) = async_channel::unbounded::<AgentInput>();
        self.agent.control_tx = Some(in_tx.clone());
        let first_line = crate::agent_permission::user_message_line(&full_prompt);
        let bridge_env = self.agent_bridge_env(&root);
        std::thread::spawn(move || {
            let mut cmd = Command::new(&bin_path);
            let home = std::env::var("HOME").unwrap_or_default();
            let current_path = std::env::var("PATH").unwrap_or_default();
            let enriched_path = format!(
                "{home}/.local/bin:{home}/.bun/bin:{home}/.opencode/bin:{home}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:{current_path}"
            );
            cmd.current_dir(&root)
                .env("PATH", enriched_path)
                .stdin(if harness == AgentHarness::ClaudeCode { Stdio::piped() } else { Stdio::null() })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for (key, value) in &bridge_env {
                cmd.env(key, value);
            }
            // A CLI that spawns test runners or dev servers can only be stopped
            // cleanly if it heads its own group (see `stop_agent_run`).
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt as _;
                cmd.process_group(0);
            }

            match harness {
                AgentHarness::ClaudeCode => {
                    cmd.args(["-p", "--verbose", "--output-format", "stream-json"]);
                    if let Some(id) = &resume_id {
                        cmd.args(["--resume", id]);
                    }
                    if model_id != "default" {
                        cmd.args(["--model", &model_id]);
                    }
                    cmd.args([
                        "--permission-mode",
                        mode.claude_permission(),
                        "--input-format",
                        "stream-json",
                        "--permission-prompt-tool",
                        "stdio",
                    ]);
                }
                AgentHarness::OpenCode => {
                    cmd.args(["run", "--format", "json"]);
                    if !model_id.is_empty() && model_id != "default" {
                        cmd.args(["-m", &model_id]);
                    }
                    cmd.arg(&full_prompt);
                }
                AgentHarness::Antigravity => {
                    cmd.args(["-p", &full_prompt, "--output-format", "stream-json"]);
                    if let Some(id) = &resume_id {
                        cmd.args(["--conversation", id]);
                    }
                    if !model_id.is_empty() && model_id != "default" {
                        cmd.args(["--model", &model_id]);
                    }
                }
                AgentHarness::Codex => {
                    cmd.arg("exec");
                    if resume_id.is_some() {
                        cmd.arg("resume");
                    }
                    cmd.args(["--json", "--skip-git-repo-check"]);
                    cmd.args(["-c", &format!("sandbox_mode=\"{}\"", mode.codex_sandbox())]);
                    cmd.args(["-c", &format!("approval_policy=\"{}\"", mode.codex_approval())]);
                    if !model_id.is_empty() && model_id != "default" {
                        cmd.args(["-c", &format!("model=\"{model_id}\"")]);
                    }
                    if let Some(id) = &resume_id {
                        cmd.arg(id);
                    }
                    cmd.arg(&full_prompt);
                }
            }

            // The workspace before the agent touches it: what an undo goes back to.
            let before = crate::agent_checkpoint::snapshot(&root);
            if let Some(cp) = &before {
                let _ = tx.send_blocking(AgentStreamEvent::Checkpoint(cp.clone()));
            }

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(err) => {
                    let _ = tx.send_blocking(AgentStreamEvent::Finished {
                        error: Some(format!("Failed to launch {}: {err}", harness.label())),
                    });
                    return;
                }
            };

            let _ = tx.send_blocking(AgentStreamEvent::Started { pid: child.id() });

            if harness == AgentHarness::ClaudeCode {
                if let Some(mut stdin) = child.stdin.take() {
                    std::thread::spawn(move || {
                        use std::io::Write;
                        let _ = writeln!(stdin, "{first_line}");
                        let _ = stdin.flush();
                        while let Ok(message) = in_rx.recv_blocking() {
                            match message {
                                AgentInput::Line(line) => {
                                    let _ = writeln!(stdin, "{line}");
                                    let _ = stdin.flush();
                                }
                                AgentInput::Close => break,
                            }
                        }
                        // Dropping stdin tells the process there is no more input.
                    });
                }
            }

            let stderr_handle = child.stderr.take().map(|stderr| {
                let tx_err = tx.clone();
                std::thread::spawn(move || {
                    let reader = BufReader::new(stderr);
                    for line in reader.lines().map_while(Result::ok) {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            let _ = tx_err
                                .send_blocking(AgentStreamEvent::StderrLine(trimmed.to_string()));
                        }
                    }
                })
            });

            if let Some(stdout) = child.stdout.take() {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    for frag in parse_agent_stream_line(&line) {
                        let _ = tx.send_blocking(AgentStreamEvent::Fragment(frag));
                    }
                    // The turn is over: with streaming input the process waits for more
                    // messages unless we close its stdin.
                    if crate::agent_permission::is_result_line(&line) {
                        let _ = in_tx.send_blocking(AgentInput::Close);
                    }
                }
            }
            if let Some(h) = stderr_handle {
                let _ = h.join();
            }
            let status = child.wait();
            let err_msg = match status {
                Ok(s) if s.success() => None,
                Ok(s) => Some(format!("{} exited with {s}", harness.label())),
                Err(e) => Some(e.to_string()),
            };
            if let Some(before) = before
                && let Some(after) = crate::agent_checkpoint::snapshot(&root)
                && after.tree != before.tree
            {
                let files = crate::agent_checkpoint::changed_files(&before.repo, &before.tree, &after.tree);
                if !files.is_empty() {
                    let _ = tx.send_blocking(AgentStreamEvent::TurnChanges { before, after: after.tree, files });
                }
            }
            let _ = tx.send_blocking(AgentStreamEvent::Finished { error: err_msg });
        });

        let started_at = std::time::Instant::now();
        let was_plan_mode = mode == ModeId::Plan;
        cx.spawn(async move |weak, cx| {
            // Plan mode: where the plan turned up (a file the agent wrote, or the
            // plan it passed when it finished planning).
            let mut plan_file: Option<PathBuf> = None;
            let mut tool_plan: Option<String> = None;
            let mut streamed_text = String::new();
            let mut stderr_lines: Vec<String> = Vec::new();
            let mut reported_cost: Option<f64> = None;
            let mut turn_error: Option<String> = None;
            while let Ok(evt) = rx.recv().await {
                let alive = weak
                    .update(cx, |this, cx| {
                        let pinned = this.agent.is_pinned_to_bottom();
                        match evt {
                            AgentStreamEvent::Started { pid } => {
                                this.agent.running_pid = Some(pid);
                            }
                            AgentStreamEvent::Checkpoint(cp) => {
                                if let Some(AgentItem::ToolGroup { checkpoint, .. }) =
                                    this.agent.items.get_mut(group_id)
                                {
                                    *checkpoint = Some(cp);
                                }
                            }
                            AgentStreamEvent::TurnChanges { before, after, files } => {
                                for f in &files {
                                    if !crate::agent_ctx::is_plan_file(&f.path)
                                        && !this.agent.ai_touched_paths.contains(&f.path)
                                    {
                                        this.agent.ai_touched_paths.push(f.path.clone());
                                    }
                                }
                                this.agent.items.push(AgentItem::Changes {
                                    before,
                                    after,
                                    files,
                                    collapsed: false,
                                });
                            }
                            AgentStreamEvent::StderrLine(line) => {
                                stderr_lines.push(line);
                            }
                            AgentStreamEvent::Fragment(frag) => match frag {
                                StreamFragment::SessionId(id) => {
                                    if this.agent.harness == harness {
                                        this.agent.session_id = Some(id);
                                    }
                                }
                                StreamFragment::Stats { cost_usd, .. } => {
                                    reported_cost = cost_usd.or(reported_cost);
                                }
                                StreamFragment::Plan(plan) => {
                                    tool_plan = Some(plan);
                                }
                                StreamFragment::Thinking(more) => {
                                    this.agent.indicator.update(cx, |i, cx| i.set_label("Thinking", cx));
                                    if let Some(AgentItem::ToolGroup { thinking, .. }) =
                                        this.agent.items.get_mut(group_id)
                                    {
                                        if !thinking.is_empty() {
                                            thinking.push_str("\n\n");
                                        }
                                        thinking.push_str(more.trim());
                                    }
                                }
                                StreamFragment::Todos(todos) => {
                                    if let Some(active) = todos.iter().find_map(|t| t.active.clone()) {
                                        this.agent.indicator.update(cx, |i, cx| i.set_label(&active, cx));
                                    }
                                    this.agent.tasks = todos;
                                }
                                StreamFragment::Usage(usage) => {
                                    if usage.context.is_some() {
                                        this.agent.usage.context = usage.context;
                                    }
                                    if usage.output.is_some() {
                                        this.agent.usage.output = usage.output;
                                    }
                                    if usage.window.is_some() {
                                        this.agent.usage.window = usage.window;
                                    }
                                }
                                StreamFragment::ToolResult { id, is_error } => {
                                    if let Some(AgentItem::ToolGroup { calls, .. }) =
                                        this.agent.items.get_mut(group_id)
                                        && let Some(call) =
                                            calls.iter_mut().find(|c| c.id.as_deref() == Some(id.as_str()))
                                    {
                                        call.status =
                                            if is_error { ToolStatus::Failed } else { ToolStatus::Done };
                                    }
                                }
                                StreamFragment::TurnError(message) => {
                                    turn_error = Some(message);
                                }
                                StreamFragment::Notice(text) => {
                                    this.agent.items.push(AgentItem::Message {
                                        is_user: false,
                                        text: format!("_{text}_"),
                                    });
                                }
                                StreamFragment::Permission(request) => {
                                    if request.tool_name == "ExitPlanMode" {
                                        // The plan card is where you approve a plan, so end
                                        // the turn here instead of asking twice.
                                        if let Some(plan) = request
                                            .input
                                            .get("plan")
                                            .and_then(|p| p.as_str())
                                            .filter(|p| !p.trim().is_empty())
                                        {
                                            tool_plan = Some(plan.to_string());
                                        }
                                        if let Some(tx) = &this.agent.control_tx {
                                            let _ = tx.send_blocking(AgentInput::Line(
                                                crate::agent_permission::control_response(
                                                    &request,
                                                    &crate::agent_permission::Decision::Deny(
                                                        "The plan has been shown to the user, who will approve it or send it back. Stop now and wait for their decision.".to_string(),
                                                    ),
                                                ),
                                            ));
                                        }
                                    } else if request.tool_name == "AskUserQuestion"
                                        && !crate::agent_permission::parse_questions(&request.input).is_empty()
                                    {
                                        let questions = crate::agent_permission::parse_questions(&request.input);
                                        let first = questions[0].question.clone();
                                        this.agent.items.push(AgentItem::Question {
                                            choices: vec![crate::agent_permission::Choice::default(); questions.len()],
                                            questions,
                                            request,
                                            state: crate::agent_permission::PermissionState::Pending,
                                        });
                                        this.agent
                                            .indicator
                                            .update(cx, |i, cx| i.set_label("Waiting for your answer", cx));
                                        if !this.agent_open {
                                            this.agent.unread_reply = true;
                                        }
                                        if cx.active_window().is_none() {
                                            notify_desktop("Shame", &format!("Claude asks: {first}"));
                                        }
                                    } else {
                                        let summary =
                                            crate::agent_permission::summarize_permission(&request, &this.root);
                                        let title = summary.title.clone();
                                        this.agent.items.push(AgentItem::Permission {
                                            request,
                                            summary,
                                            state: crate::agent_permission::PermissionState::Pending,
                                        });
                                        this.agent
                                            .indicator
                                            .update(cx, |i, cx| i.set_label("Waiting for your approval", cx));
                                        if !this.agent_open {
                                            this.agent.unread_reply = true;
                                        }
                                        if cx.active_window().is_none() {
                                            notify_desktop("Shame", &format!("Needs your approval: {title}"));
                                        }
                                    }
                                }
                                StreamFragment::Tool(call) => {
                                    let label = activity_label(Some(&call));
                                    this.agent.indicator.update(cx, |i, cx| i.set_label(&label, cx));
                                    if (call.label.starts_with("Edit ")
                                        || call.label.starts_with("Write "))
                                        && let Some(ref target_path) = call.target_path
                                    {
                                        if crate::agent_ctx::is_plan_file(target_path) {
                                            // The plan itself, not a change to your project.
                                            plan_file = Some(PathBuf::from(target_path));
                                        } else {
                                            this.agent
                                                .record_ai_touched_path(&this.root, target_path);
                                        }
                                    }
                                    if let Some(AgentItem::ToolGroup { header, calls, .. }) =
                                        this.agent.items.get_mut(group_id)
                                    {
                                        // CLIs that don't report results: a call is over
                                        // once the next one starts.
                                        if call.id.is_none() {
                                            for prev in calls.iter_mut().filter(|c| c.id.is_none()) {
                                                if prev.status == ToolStatus::Running {
                                                    prev.status = ToolStatus::Done;
                                                }
                                            }
                                        }
                                        calls.push(call);
                                        let n = calls.len();
                                        *header = format!(
                                            "Agent used {n} tool{}",
                                            if n == 1 { "" } else { "s" }
                                        );
                                    }
                                }
                                StreamFragment::TextDelta(delta) => {
                                    this.agent.indicator.update(cx, |i, cx| i.set_label("Writing the reply", cx));
                                    streamed_text.push_str(&delta);
                                    if let Some(AgentItem::Message { text, .. }) =
                                        this.agent.items.get_mut(msg_ix)
                                    {
                                        *text = streamed_text.trim().to_string();
                                    }
                                }
                                StreamFragment::ReplaceText(new_text) => {
                                    this.agent.indicator.update(cx, |i, cx| i.set_label("Writing the reply", cx));
                                    streamed_text = new_text;
                                    if let Some(AgentItem::Message { text, .. }) =
                                        this.agent.items.get_mut(msg_ix)
                                    {
                                        *text = streamed_text.trim().to_string();
                                    }
                                }
                            },
                            AgentStreamEvent::Finished { error } => {
                                this.agent.running_pid = None;
                                this.agent.last_activity_ms = Some(crate::agent_store::now_ms());
                                if !this.agent_open {
                                    // A reply landed while the rail was hidden.
                                    this.agent.unread_reply = true;
                                }
                                this.agent.indicator.update(cx, |i, cx| i.stop(cx));
                                this.cancel_pending_permissions();
                                this.agent.control_tx = None;
                                let elapsed = started_at.elapsed();
                                this.agent.turn_stats.insert(
                                    msg_ix,
                                    TurnStats {
                                        duration_ms: elapsed.as_millis() as u64,
                                        cost_usd: reported_cost,
                                    },
                                );
                                // A long run finishing while you're in another app is worth a nudge.
                                if elapsed.as_secs() >= 10 && cx.active_window().is_none() {
                                    let body = if streamed_text.trim().is_empty() {
                                        "The agent finished.".to_string()
                                    } else {
                                        notification_body(&streamed_text, 110)
                                    };
                                    notify_desktop("Shame", &body);
                                }
                                let stopped = std::mem::take(&mut this.agent.stop_requested);
                                // A stop is the user's choice, not a failure to report.
                                let error = if stopped { None } else { error };
                                let failure = if stopped {
                                    None
                                } else {
                                    turn_error.take().or_else(|| {
                                        error.as_ref().map(|e| {
                                            if stderr_lines.is_empty() {
                                                e.clone()
                                            } else {
                                                let joined = stderr_lines.join("\n");
                                                crate::agent_ctx::clip_text(&joined, 1200).0.to_string()
                                            }
                                        })
                                    })
                                };
                                let failed = failure.is_some();
                                // A rate limit or a dropped connection leaves the CLI's
                                // session intact; anything else may mean it is gone.
                                let session_suspect = failure.as_deref().is_some_and(|m| {
                                    !matches!(
                                        ErrorKind::classify(m),
                                        ErrorKind::RateLimit | ErrorKind::Overloaded | ErrorKind::Network
                                    )
                                });
                                // A finished plan becomes a card to read and approve.
                                if was_plan_mode && !failed {
                                    let file_text = plan_file.as_ref().and_then(|p| std::fs::read_to_string(p).ok());
                                    if let Some(plan) =
                                        crate::agent_ctx::choose_plan_text(file_text, tool_plan.take(), &streamed_text)
                                    {
                                        if let Some(item) = this.agent.items.get_mut(msg_ix) {
                                            *item = AgentItem::Plan {
                                                text: plan,
                                                path: plan_file.take(),
                                                state: crate::agent_plan::PlanState::Pending,
                                                expanded: false,
                                            };
                                        }
                                    }
                                }
                                if session_suspect && was_resumed {
                                    // The session may be gone; start fresh next time.
                                    this.agent.session_id = None;
                                }
                                if let Some(AgentItem::ToolGroup { header, calls, .. }) =
                                    this.agent.items.get_mut(group_id)
                                {
                                    for call in calls.iter_mut() {
                                        if call.status == ToolStatus::Running {
                                            call.status = ToolStatus::Done;
                                        }
                                    }
                                    let n = calls.len();
                                    *header = if n == 0 {
                                        format!("{} completed", harness.label())
                                    } else {
                                        format!(
                                            "Agent used {n} tool{}",
                                            if n == 1 { "" } else { "s" }
                                        )
                                    };
                                }
                                let prompt_ix = msg_ix.checked_sub(2).filter(|ix| {
                                    matches!(this.agent.items.get(*ix), Some(AgentItem::Message { is_user: true, .. }))
                                });
                                let prompt_ix = prompt_ix.or_else(|| {
                                    this.agent.items[..msg_ix].iter().rposition(|item| {
                                        matches!(item, AgentItem::Message { is_user: true, .. })
                                    })
                                });
                                let empty_reply = streamed_text.trim().is_empty();
                                if let Some(message) = failure {
                                    let card = AgentItem::Error {
                                        kind: ErrorKind::classify(&message),
                                        message,
                                        prompt_ix,
                                    };
                                    // The card replaces an empty reply; a partial one stays.
                                    if empty_reply
                                        && matches!(this.agent.items.get(msg_ix), Some(AgentItem::Message { .. }))
                                    {
                                        this.agent.items[msg_ix] = card;
                                    } else {
                                        this.agent.items.push(card);
                                    }
                                } else if empty_reply
                                    && let Some(AgentItem::Message { text, .. }) = this.agent.items.get_mut(msg_ix)
                                {
                                    *text = if stopped {
                                        "_Stopped._".to_string()
                                    } else if !stderr_lines.is_empty() {
                                        stderr_lines.join(" ")
                                    } else {
                                        "_Done._".to_string()
                                    };
                                }
                                if let Some(post_status) = core_workspace::repo_status(&this.root) {
                                    for f in &post_status.unstaged {
                                        if !pre_run_unstaged.contains(&f.path) {
                                            this.agent
                                                .record_ai_touched_path(&this.root, &f.path);
                                        }
                                    }
                                    // The turn is over, so every tool effect has landed:
                                    // anything no longer modified is a reverted/undone edit,
                                    // not a pending AI change.
                                    let pending: std::collections::HashSet<String> = post_status
                                        .unstaged
                                        .iter()
                                        .map(|f| f.path.clone())
                                        .collect();
                                    crate::git_panel::retain_pending_ai_paths(
                                        &mut this.agent.ai_touched_paths,
                                        &pending,
                                    );
                                }
                                this.refresh_git(cx);
                                this.sync_workspace_files_if_changed(cx);
                                if this.web_preview_open {
                                    this.web_preview_after_save(cx);
                                }
                                this.save_agent_conversation();
                                if failed {
                                    this.agent.queue.clear();
                                } else {
                                    this.run_next_queued(cx);
                                }
                            }
                        }
                        if pinned {
                            this.agent.scroll.scroll_to_bottom();
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// Standalone Agent rail (`docs/design-spec.md` §3.23–31) with multi-harness tooling & model pickers.
    pub(crate) fn render_agent_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let green = cx.theme().green;
        let red = cx.theme().red;
        let card_bg = cx.theme().muted;
        let border = cx.theme().border;
        let list_hover = cx.theme().list_hover;
        let harness = self.agent.harness;
        let is_running = self.agent.running_pid.is_some();
        let bin_resolved = harness.resolve_binary();
        let thread_title = self.agent.thread_title();
        // A reader who scrolled up is not dragged along by streaming; this is
        // the way back to the end of the thread.
        let show_jump = !self.agent.items.is_empty() && !self.agent.is_pinned_to_bottom();
        let restored_note = (self.agent.restored_items > 0).then(|| {
            let ago = self
                .agent
                .restored_at_ms
                .and_then(|ms| crate::agent_store::format_ago(ms, crate::agent_store::now_ms()))
                .map(|ago| format!(" \u{b7} {ago}"))
                .unwrap_or_default();
            format!("Restored from your last session{ago}")
        });
        let active_rel_file = self
            .active_editor()
            .and_then(|ed| ed.read(cx).buffer().path().map(Path::to_path_buf))
            .and_then(|p| p.strip_prefix(&self.root).ok().map(Path::to_path_buf))
            .map(|p| p.to_string_lossy().to_string());

        // Only show files that the AI agent itself modified/created during its runs,
        // never the user's own pre-existing uncommitted files.
        let ai_unstaged: Vec<core_workspace::ChangedFile> = self
            .git
            .status
            .as_ref()
            .map(|s| {
                s.unstaged
                    .iter()
                    .filter(|f| {
                        self.agent
                            .ai_touched_paths
                            .iter()
                            .any(|ai_p| ai_p == &f.path)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let has_ai_changes = !ai_unstaged.is_empty();
        let (ai_added, ai_removed) = ai_unstaged
            .iter()
            .filter_map(|f| self.git.stats.get(&f.path))
            .fold((0u32, 0u32), |(a, r), (fa, fr)| (a + fa, r + fr));

        v_flex()
            .relative()
            .size_full()
            .bg(cx.theme().sidebar)
            .border_1()
            .border_color(border)
            .rounded(px(tokens::RADIUS_CARD))
            .overflow_hidden()
            // 23. AgentTabBar
            .child(
                h_flex()
                    .w_full()
                    .h(px(tokens::TABSTRIP_H))
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .px(px(tokens::SPACE_3))
                    .border_b_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w(px(0.))
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .child(
                                h_flex()
                                    .id("rail-tab-agent")
                                    .min_w(px(0.))
                                    .overflow_hidden()
                                    .items_center()
                                    .gap(px(tokens::SPACE_2))
                                    .h(px(28.))
                                    .px(px(tokens::SPACE_3))
                                    .rounded(px(tokens::RADIUS_SM))
                                    .border_1()
                                    .border_color(border)
                                    .bg(cx.theme().tab_active)
                                    .text_size(px(tokens::UI_SM))
                                    .font_medium()
                                    .text_color(fg)
                                    .child(Icon::new(IconName::Bot).small().text_color(primary))
                                    .child(harness.label())
                                    .when_some(thread_title, |el, title| {
                                        el.child(
                                            div()
                                                .min_w(px(0.))
                                                .truncate()
                                                .text_size(px(tokens::UI_XS))
                                                .text_color(muted)
                                                .child(format!("\u{b7} {title}")),
                                        )
                                    })
                                    .when(is_running, |el| {
                                        el.child(
                                            div()
                                                .size(px(6.))
                                                .rounded(px(3.))
                                                .bg(green),
                                        )
                                    }),
                            )
                            .child(
                                Button::new("agent-history")
                                    .ghost()
                                    .small()
                                    .icon(
                                        Icon::new(IconName::Clock).xsmall().text_color(
                                            if self.agent.history_open { primary } else { muted },
                                        ),
                                    )
                                    .tooltip("Thread history")
                                    .on_click(cx.listener(|this, _e, _w, cx| {
                                        if this.agent.history_open {
                                            this.close_agent_history(cx);
                                        } else {
                                            this.open_agent_history(cx);
                                        }
                                    })),
                            )
                            .child(
                                Button::new("agent-new-thread")
                                    .ghost()
                                    .small()
                                    .icon(Icon::new(IconName::Plus).xsmall().text_color(muted))
                                    .tooltip("New agent thread")
                                    .on_click(cx.listener(|this, _e, _w, cx| {
                                        this.agent_new_thread(cx);
                                    })),
                            ),
                    )
                    .child(
                        Button::new("close-right-rail")
                            .ghost()
                            .small()
                            .icon(Icon::new(IconName::X).xsmall().text_color(muted))
                            .tooltip("Close Agent panel")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.set_agent_open(false);
                                cx.notify();
                            })),
                    ),
            )
            // History (when open) or the scrollable conversation body.
            .child(if self.agent.history_open {
                self.render_agent_history(cx).into_any_element()
            } else {
                v_flex()
                    .relative()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .child(
                v_flex()
                    .id("agent-scroll-body")
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .track_scroll(&self.agent.scroll)
                    .on_scroll_wheel(cx.listener(|_this, _e: &ScrollWheelEvent, _w, cx| {
                        // The "Jump to latest" pill follows whether the reader
                        // is at the end, which only changes while scrolling.
                        cx.notify();
                    }))
                    .p(px(tokens::SPACE_4))
                    .gap(px(tokens::SPACE_4))
                    // Live empty state when no messages have been sent yet
                    .when(self.agent.items.is_empty(), |el| {
                        let cli_ready = bin_resolved.is_some();
                        let cli_status = match &bin_resolved {
                            Some(p) => p.to_string_lossy().to_string(),
                            None => format!("{} CLI not found in PATH", harness.short_label()),
                        };
                        let active_ctx = active_rel_file.clone().unwrap_or_else(|| {
                            self.root
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("workspace")
                                .to_string()
                        });
                        let starters: [(&str, &str); 3] = [
                            (
                                "Explain the active file",
                                "Concise overview of the active file's responsibilities and key types.",
                            ),
                            (
                                "Review uncommitted git changes",
                                "Inspect git status/diff in this workspace and flag bugs or improvements.",
                            ),
                            (
                                "Find & fix issues in this project",
                                "Check for compiler/analyzer errors or failing tests and fix them.",
                            ),
                        ];

                        el.child(
                            v_flex()
                                .w_full()
                                .p(px(tokens::SPACE_4))
                                .gap(px(tokens::SPACE_3))
                                .rounded(px(tokens::RADIUS_CARD))
                                .bg(card_bg)
                                .border_1()
                                .border_color(border)
                                .child(
                                    h_flex()
                                        .w_full()
                                        .items_center()
                                        .justify_between()
                                        .gap(px(tokens::SPACE_2))
                                        .child(
                                            h_flex()
                                                .items_center()
                                                .gap(px(tokens::SPACE_2))
                                                .child(
                                                    Icon::new(IconName::Bot)
                                                        .small()
                                                        .text_color(primary),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(px(tokens::UI))
                                                        .font_medium()
                                                        .text_color(fg)
                                                        .child(harness.label()),
                                                ),
                                        )
                                        .child(
                                            h_flex()
                                                .items_center()
                                                .gap(px(tokens::SPACE_1))
                                                .px(px(6.))
                                                .py(px(2.))
                                                .rounded(px(tokens::RADIUS_PILL))
                                                .bg(if cli_ready {
                                                    green.opacity(0.15)
                                                } else {
                                                    red.opacity(0.15)
                                                })
                                                .child(
                                                    div()
                                                        .size(px(6.))
                                                        .rounded(px(3.))
                                                        .bg(if cli_ready { green } else { red }),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(px(tokens::UI_XS))
                                                        .text_color(if cli_ready {
                                                            green
                                                        } else {
                                                            red
                                                        })
                                                        .child(if cli_ready {
                                                            "Ready"
                                                        } else {
                                                            "Missing CLI"
                                                        }),
                                                ),
                                        ),
                                )
                                // The resolved binary path is a diagnostic: only
                                // worth the space when the CLI can't be found.
                                .when(!cli_ready, |el| {
                                    el.child(
                                        div()
                                            .truncate()
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_size(px(tokens::UI_XS))
                                            .text_color(muted)
                                            .child(cli_status),
                                    )
                                })
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap(px(tokens::SPACE_2))
                                        .px(px(tokens::SPACE_2))
                                        .py(px(4.))
                                        .rounded(px(tokens::RADIUS_SM))
                                        .bg(cx.theme().background)
                                        .border_1()
                                        .border_color(border)
                                        .child(
                                            Icon::new(IconName::FileCode)
                                                .with_size(px(12.))
                                                .text_color(primary),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(px(tokens::UI_XS))
                                                .text_color(fg)
                                                .child(format!("Context: {active_ctx}")),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .flex_wrap()
                                        .items_center()
                                        .gap(px(4.))
                                        .child(
                                            div()
                                                .text_size(px(tokens::UI_XS))
                                                .text_color(muted)
                                                .child("Tooling:"),
                                        )
                                        .children(
                                            ["Files", "LSP", "Git", "Shell", "Simulator", "Web"]
                                                .into_iter()
                                                .map(|cap| {
                                                    div()
                                                        .px(px(tokens::SPACE_2))
                                                        .py(px(tokens::SPACE_1 / 2.))
                                                        .rounded(px(tokens::RADIUS_BADGE))
                                                        .bg(cx.theme().background)
                                                        .border_1()
                                                        .border_color(border)
                                                        .text_size(px(tokens::UI_XS))
                                                        .text_color(muted)
                                                        .child(cap)
                                                }),
                                        ),
                                )
                                .child(div().w_full().h(px(1.)).bg(border.opacity(0.6)))
                                .children(starters.into_iter().enumerate().map(
                                    |(ix, (title, prompt_text))| {
                                        let prompt_owned = prompt_text.to_string();
                                        h_flex()
                                            .id(("agent-starter", ix))
                                            .w_full()
                                            .px(px(tokens::SPACE_3))
                                            .py(px(6.))
                                            .items_center()
                                            .justify_between()
                                            .gap(px(tokens::SPACE_2))
                                            .rounded(px(tokens::RADIUS_SM))
                                            .bg(cx.theme().background)
                                            .border_1()
                                            .border_color(border)
                                            .cursor_pointer()
                                            .hover(move |s| {
                                                s.bg(list_hover).border_color(primary.opacity(0.45))
                                            })
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_size(px(tokens::UI_SM))
                                                    .text_color(fg)
                                                    .child(title),
                                            )
                                            .child(
                                                Icon::new(IconName::ArrowRight)
                                                    .with_size(px(11.))
                                                    .text_color(muted),
                                            )
                                            .on_click(cx.listener(move |this, _e, _window, cx| {
                                                this.agent_run_prompt(prompt_owned.clone(), cx);
                                            }))
                                    },
                                )),
                        )
                    })
                    // 26–28. ToolCallCard, CollapsedGroup, MessageBubble
                    .when_some(restored_note, |el, note| {
                        el.child(
                            h_flex()
                                .w_full()
                                .items_center()
                                .gap(px(tokens::SPACE_2))
                                .child(div().flex_1().h(px(1.)).bg(border))
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(tokens::UI_XS))
                                        .text_color(muted)
                                        .child(note),
                                )
                                .child(div().flex_1().h(px(1.)).bg(border)),
                        )
                    })
                    .children(self.agent.items.iter().enumerate().map(|(item_ix, item)| {
                        match item {
                            AgentItem::ToolGroup {
                                id,
                                header,
                                collapsed,
                                calls,
                                thinking,
                                thinking_open,
                                ..
                            } => {
                                let is_collapsed = *collapsed;
                                let group_id = *id;
                                // Still thinking: this turn is running and has no reply text yet.
                                let live_thinking = is_running
                                    && self.agent.items[item_ix + 1..]
                                        .iter()
                                        .all(|it| !matches!(it, AgentItem::Message { text, .. } if text != "Thinking\u{2026}"));
                                if calls.is_empty() && thinking.is_empty() && !is_running {
                                    // A turn that used no tools has nothing to show here.
                                    return div().into_any_element();
                                }
                                v_flex()
                                    .w_full()
                                    .gap(px(tokens::SPACE_2))
                                    .when(!thinking.is_empty(), |el| {
                                        el.child(self.render_turn_thinking(
                                            item_ix,
                                            thinking,
                                            *thinking_open,
                                            live_thinking,
                                            cx,
                                        ))
                                    })
                                    .when(!calls.is_empty(), |el| {
                                        el.child(
                                        h_flex()
                                            .id(("agent-tool-group", group_id))
                                            .items_center()
                                            .gap(px(tokens::SPACE_2))
                                            .cursor_pointer()
                                            .text_size(px(tokens::UI_SM))
                                            .text_color(muted)
                                            .hover(move |s| s.text_color(fg))
                                            .child(
                                                Icon::new(IconName::Settings)
                                                    .with_size(px(11.5))
                                                    .text_color(muted),
                                            )
                                            .child(header.clone())
                                            .child(
                                                Icon::new(if is_collapsed {
                                                    IconName::ChevronRight
                                                } else {
                                                    IconName::ChevronDown
                                                })
                                                .with_size(px(11.))
                                                .text_color(muted),
                                            )
                                            .on_click(cx.listener(move |this, _e, _w, cx| {
                                                if let Some(AgentItem::ToolGroup {
                                                    collapsed, ..
                                                }) = this.agent.items.get_mut(item_ix)
                                                {
                                                    *collapsed = !*collapsed;
                                                    cx.notify();
                                                }
                                            })),
                                        )
                                    })
                                    .when(!is_collapsed, |el| {
                                        el.children(self.render_tool_rows(item_ix, calls, cx))
                                    })
                                    .into_any_element()
                            }
                            AgentItem::Changes { files, collapsed, .. } => {
                                self.render_changes_card(item_ix, files, *collapsed, cx)
                            }
                            AgentItem::Error { kind, message, prompt_ix } => {
                                self.render_error_card(item_ix, *kind, message, *prompt_ix, cx)
                            }
                            AgentItem::Question { questions, choices, state, .. } => {
                                self.render_question_card(item_ix, questions, choices, *state, cx)
                            }
                            AgentItem::Permission { request, summary, state } => {
                                self.render_permission_card(item_ix, request, summary, *state, cx)
                            }
                            AgentItem::Plan { text, path, state, expanded } => {
                                self.render_plan_card(item_ix, text, path.as_ref(), *state, *expanded, cx)
                            }
                            // The animated indicator stands in for the "Thinking…" placeholder.
                            AgentItem::Message { is_user: false, text }
                                if is_running && text.as_str() == "Thinking\u{2026}" =>
                            {
                                div().into_any_element()
                            }
                            AgentItem::Message { is_user, text } => {
                                let streaming = is_running
                                    && !*is_user
                                    && item_ix + 1 == self.agent.items.len();
                                let can_copy = !*is_user && !streaming && !text.trim().is_empty();
                                let to_copy = text.clone();
                                // Retry the latest reply: the user message that started it.
                                let retry_from = (!*is_user && !is_running)
                                    .then(|| {
                                        let later_reply = self.agent.items[item_ix + 1..].iter().any(|it| {
                                            matches!(it, AgentItem::Message { .. } | AgentItem::Error { .. })
                                        });
                                        (!later_reply).then(|| {
                                            self.agent.items[..item_ix].iter().rposition(|it| {
                                                matches!(it, AgentItem::Message { is_user: true, .. })
                                            })
                                        })
                                    })
                                    .flatten()
                                    .flatten();
                                let msg_group: SharedString = format!("agent-msg-{item_ix}").into();
                                let weak = cx.entity().downgrade();
                                let on_insert: crate::markdown::InsertCode =
                                    std::rc::Rc::new(move |code, window, cx| {
                                        let _ = weak.update(cx, |this, cx| this.agent_insert_code(code, window, cx));
                                    });
                                let stats_text = (!*is_user && !streaming)
                                    .then(|| self.agent.turn_stats.get(&item_ix))
                                    .flatten()
                                    .map(|st| {
                                        crate::agent_ctx::format_turn_stats(st.duration_ms, st.cost_usd)
                                    });
                                let bubble = v_flex()
                                    .w_full()
                                    .px(px(tokens::SPACE_5))
                                    .py(px(10.))
                                    .gap(px(6.))
                                    .rounded(px(tokens::RADIUS_CARD))
                                    .bg(if *is_user {
                                        tokens::accent_dim()
                                    } else {
                                        card_bg
                                    })
                                    .border_1()
                                    .border_color(if *is_user {
                                        cx.theme().list_active_border
                                    } else {
                                        border
                                    })
                                    .child(if text.as_str() == "Thinking\u{2026}" {
                                        div()
                                            .text_size(px(tokens::UI))
                                            .text_color(muted)
                                            .child("Thinking\u{2026}")
                                    } else if *is_user {
                                        crate::markdown::render_markdown(text, item_ix, cx)
                                    } else {
                                        crate::markdown::render_markdown_insertable(text, item_ix, on_insert, cx)
                                    })
                                    .when(can_copy, |el| {
                                        el.child(
                                            h_flex()
                                                .w_full()
                                                .items_center()
                                                .gap(px(2.))
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .text_size(px(tokens::UI_XS))
                                                        .text_color(muted.opacity(0.8))
                                                        .child(stats_text.unwrap_or_default()),
                                                )
                                                .child(
                                                div()
                                                    .id(("agent-copy-reply", item_ix))
                                                    .px(px(6.))
                                                    .py(px(2.))
                                                    .rounded(px(tokens::RADIUS_SM))
                                                    .cursor_pointer()
                                                    .text_size(px(tokens::UI_XS))
                                                    .text_color(muted)
                                                    .hover(|s| s.bg(list_hover))
                                                    .child("Copy reply")
                                                    .on_click(move |_e, _w, cx| {
                                                        cx.write_to_clipboard(
                                                            ClipboardItem::new_string(
                                                                to_copy.clone(),
                                                            ),
                                                        );
                                                    }),
                                            )
                                                .when_some(retry_from, |el, user_ix| {
                                                    el.child(
                                                        div()
                                                            .id(("agent-retry-reply", item_ix))
                                                            .px(px(6.))
                                                            .py(px(2.))
                                                            .rounded(px(tokens::RADIUS_SM))
                                                            .cursor_pointer()
                                                            .text_size(px(tokens::UI_XS))
                                                            .text_color(muted)
                                                            .hover(|s| s.bg(list_hover))
                                                            .child("Retry")
                                                            .tooltip(|window, cx| {
                                                                gpui_kit::component::tooltip::Tooltip::new(
                                                                    "Ask again with the current model; this reply is replaced",
                                                                )
                                                                .build(window, cx)
                                                            })
                                                            .on_click(cx.listener(move |this, _e, _w, cx| {
                                                                this.agent_retry(user_ix, false, cx);
                                                            })),
                                                    )
                                                }),
                                        )
                                    });
                                if *is_user && !is_running {
                                    v_flex()
                                        .w_full()
                                        .gap(px(2.))
                                        .group(msg_group.clone())
                                        .child(bubble)
                                        .child(self.render_user_message_actions(item_ix, msg_group, cx))
                                        .into_any_element()
                                } else {
                                    bubble.into_any_element()
                                }
                            }
                        }
                    }))
                    .when(is_running, |el| el.child(self.agent.indicator.clone())),
                )
                // Shown only when the reader has scrolled away from the end.
                .when(show_jump, |el| {
                    el.child(
                        div()
                            .absolute()
                            .left(px(0.))
                            .right(px(0.))
                            .bottom(px(tokens::SPACE_3))
                            .flex()
                            .justify_center()
                            .child(
                                h_flex()
                                    .id("agent-jump-latest")
                                    .h(px(24.))
                                    .px(px(tokens::SPACE_3))
                                    .gap(px(tokens::SPACE_1))
                                    .items_center()
                                    .rounded(px(tokens::RADIUS_PILL))
                                    .bg(cx.theme().tab_active)
                                    .border_1()
                                    .border_color(border)
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(fg)
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(list_hover))
                                    .child(
                                        Icon::new(IconName::ArrowDown)
                                            .with_size(px(10.))
                                            .text_color(muted),
                                    )
                                    .child("Jump to latest")
                                    .on_click(cx.listener(|this, _e, _w, cx| {
                                        this.agent.scroll.scroll_to_bottom();
                                        cx.notify();
                                    })),
                            ),
                    )
                })
                    .into_any_element()
            })
            // 29. PendingChanges card (strictly reflects files modified/created by the AI agent)
            .when(has_ai_changes, |el| {
                let count = ai_unstaged.len();
                let all_ai_files = ai_unstaged.clone();
                el.child(
                    v_flex()
                        .w_auto()
                        .flex_shrink_0()
                        .mx(px(tokens::SPACE_4))
                        .mb(px(tokens::SPACE_3))
                        .p(px(tokens::SPACE_3))
                        .gap(px(tokens::SPACE_2))
                        .rounded(px(tokens::RADIUS_CARD))
                        .bg(card_bg)
                        .border_1()
                        .border_color(border)
                        .child(
                            h_flex()
                                .w_full()
                                .px(px(tokens::SPACE_2))
                                .items_center()
                                .justify_between()
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap(px(tokens::SPACE_2))
                                        .child(
                                            Icon::new(IconName::FilePlus)
                                                .with_size(px(13.))
                                                .text_color(primary),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(tokens::UI))
                                                .font_medium()
                                                .text_color(fg)
                                                .child(format!(
                                                    "{count} AI Change{}{}",
                                                    if count == 1 { "" } else { "s" },
                                                    if ai_added + ai_removed > 0 {
                                                        format!(" \u{b7} +{ai_added} \u{2212}{ai_removed}")
                                                    } else {
                                                        String::new()
                                                    }
                                                )),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap(px(tokens::SPACE_2))
                                        .child(
                                            div()
                                                .id("agent-reject-all")
                                                .size(px(22.))
                                                .rounded(px(tokens::RADIUS_PILL))
                                                .bg(cx.theme().background)
                                                .border_1()
                                                .border_color(border)
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .hover(move |s| s.border_color(red))
                                                .child(
                                                    Icon::new(IconName::X)
                                                        .with_size(px(11.))
                                                        .text_color(red),
                                                )
                                                .on_click(cx.listener({
                                                    let to_revert = all_ai_files.clone();
                                                    move |this, _e, window, cx| {
                                                        // Each file goes back to before the agent
                                                        // touched it; the user's own edits stay.
                                                        for f in &to_revert {
                                                            this.agent_reject_file(f.clone(), window, cx);
                                                        }
                                                        cx.notify();
                                                    }
                                                })),
                                        )
                                        .child(
                                            div()
                                                .id("agent-accept-all")
                                                .size(px(22.))
                                                .rounded(px(tokens::RADIUS_PILL))
                                                .bg(primary)
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .hover(|s| s.opacity(0.9))
                                                .child(
                                                    Icon::new(IconName::Check)
                                                        .with_size(px(12.))
                                                        .text_color(cx.theme().primary_foreground),
                                                )
                                                .on_click(cx.listener(
                                                    move |this, _e, _window, cx| {
                                                        this.agent.ai_touched_paths.clear();
                                                        cx.notify();
                                                    },
                                                )),
                                        ),
                                ),
                        )
                        .child(div().w_full().h(px(1.)).bg(border.opacity(0.6)))
                        .children(ai_unstaged.into_iter().take(6).enumerate().map(
                            |(ix, file)| {
                                let name = Path::new(&file.path)
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or(&file.path)
                                    .to_string();
                                let file_stat = self
                                    .git
                                    .stats
                                    .get(&file.path)
                                    .copied()
                                    .filter(|(added, removed)| added + removed > 0);
                                let diff_file = file.clone();
                                let accept_path = file.path.clone();
                                let discard_file = file.clone();
                                h_flex()
                                    .id(("agent-git-pending-row", ix))
                                    .w_full()
                                    .h(px(30.))
                                    .px(px(tokens::SPACE_2))
                                    .items_center()
                                    .justify_between()
                                    .gap(px(tokens::SPACE_2))
                                    .rounded(px(tokens::RADIUS_PANEL))
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(list_hover))
                                    .child(
                                        h_flex()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .items_center()
                                            .gap(px(tokens::SPACE_2))
                                            .child(crate::rails::files::render_file_icon(&name))
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_size(px(12.))
                                                    .text_color(fg)
                                                    .child(name),
                                            )
                                            .when_some(file_stat, |el, (added, removed)| {
                                                el.child(crate::diff_view::stat_chip(
                                                    added, removed, cx,
                                                ))
                                            }),
                                    )
                                    .child(
                                        h_flex()
                                            .flex_shrink_0()
                                            .items_center()
                                            .gap(px(tokens::SPACE_3))
                                            .child(
                                                div()
                                                    .id(("agent-git-reject", ix))
                                                    .cursor_pointer()
                                                    .text_color(red)
                                                    .child(
                                                        Icon::new(IconName::X).with_size(px(11.)),
                                                    )
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        |_, _, cx| cx.stop_propagation(),
                                                    )
                                                    .on_click(cx.listener(
                                                        move |this, _e, window, cx| {
                                                            this.agent_reject_file(discard_file.clone(), window, cx);
                                                        },
                                                    )),
                                            )
                                            .child(
                                                div()
                                                    .id(("agent-git-accept", ix))
                                                    .cursor_pointer()
                                                    .text_color(primary)
                                                    .child(
                                                        Icon::new(IconName::Check)
                                                            .with_size(px(11.)),
                                                    )
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        |_, _, cx| cx.stop_propagation(),
                                                    )
                                                    .on_click(cx.listener(
                                                        move |this, _e, _window, cx| {
                                                            this.agent
                                                                .ai_touched_paths
                                                                .retain(|p| p != &accept_path);
                                                            cx.notify();
                                                        },
                                                    )),
                                            ),
                                    )
                                    .on_click(cx.listener(move |this, _e, _w, cx| {
                                        this.open_git_diff(diff_file.clone(), false, cx);
                                    }))
                            },
                        ))
                        .when(count > 6, |el| {
                            el.child(
                                div()
                                    .px(px(tokens::SPACE_2))
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(muted)
                                    .child(format!("+{} more files", count - 6)),
                            )
                        }),
                )
            })
            // The agent's own task list, pinned where it stays in view.
            .when_some(self.render_agent_tasks(cx), |el, tasks| el.child(tasks))
            // 30. Composer: mode switch, context chips, input, tool / model pickers
            .child(self.render_agent_composer(cx))
            .into_any_element()
    }

    /// Threads saved for this workspace, newest first. A row switches the
    /// thread; the × removes it from disk.
    fn render_agent_history(&self, cx: &mut Context<Self>) -> Div {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let border = cx.theme().border;
        let list_hover = cx.theme().list_hover;
        let red = cx.theme().red;
        let now = crate::agent_store::now_ms();
        let running = self.agent.running_pid.is_some();

        let rows: Vec<AnyElement> = self
            .agent
            .threads
            .iter()
            .enumerate()
            .map(|(ix, thread)| {
                let is_current = self.agent.thread_id.as_deref() == Some(thread.id.as_str());
                let title = if thread.title.is_empty() {
                    "Untitled thread".to_string()
                } else {
                    thread.title.clone()
                };
                let ago = crate::agent_store::format_ago(thread.saved_at_ms, now)
                    .unwrap_or_else(|| "earlier".to_string());
                let mut meta = format!(
                    "{ago} \u{b7} {} message{}",
                    thread.messages,
                    if thread.messages == 1 { "" } else { "s" }
                );
                if let Some(harness) = thread.harness {
                    meta.push_str(&format!(" \u{b7} {}", harness.label()));
                }
                let load_id = thread.id.clone();
                let delete_id = thread.id.clone();
                h_flex()
                    .id(("agent-thread-row", ix))
                    .w_full()
                    .min_w(px(0.))
                    .items_center()
                    .gap(px(tokens::SPACE_2))
                    .px(px(tokens::SPACE_2))
                    .py(px(tokens::SPACE_2))
                    .rounded(px(tokens::RADIUS_SM))
                    .cursor_pointer()
                    .when(is_current, |el| el.bg(cx.theme().tab_active))
                    .when(!is_current, |el| el.hover(move |s| s.bg(list_hover)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w(px(0.))
                            .gap(px(1.))
                            .child(
                                h_flex()
                                    .min_w(px(0.))
                                    .items_center()
                                    .gap(px(tokens::SPACE_1))
                                    .child(
                                        div()
                                            .min_w(px(0.))
                                            .truncate()
                                            .text_size(px(tokens::UI_SM))
                                            .text_color(fg)
                                            .child(title),
                                    )
                                    .when(is_current, |el| {
                                        el.child(
                                            Icon::new(IconName::Check)
                                                .with_size(px(10.))
                                                .text_color(primary),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(muted)
                                    .child(meta),
                            ),
                    )
                    .child(
                        div()
                            .id(("agent-thread-delete", ix))
                            .size(px(16.))
                            .flex_shrink_0()
                            .rounded(px(tokens::RADIUS_PILL))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_color(muted)
                            .hover(move |s| s.bg(rgba(0xffffff1f)).text_color(red))
                            .child(Icon::new(IconName::Trash).with_size(px(10.)))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _e, _w, cx| {
                                this.delete_agent_thread(&delete_id, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _e, _w, cx| {
                        this.load_agent_thread(&load_id, cx);
                    }))
                    .into_any_element()
            })
            .collect();

        v_flex()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .child(
                h_flex()
                    .w_full()
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .px(px(tokens::SPACE_3))
                    .py(px(tokens::SPACE_2))
                    .border_b_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .child(
                                Icon::new(IconName::Clock)
                                    .with_size(px(12.))
                                    .text_color(primary),
                            )
                            .child(
                                div()
                                    .text_size(px(tokens::UI_SM))
                                    .font_medium()
                                    .text_color(fg)
                                    .child("History"),
                            ),
                    )
                    .child(
                        Button::new("agent-history-close")
                            .ghost()
                            .small()
                            .icon(Icon::new(IconName::X).xsmall().text_color(muted))
                            .tooltip("Back to the thread")
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.close_agent_history(cx);
                            })),
                    ),
            )
            .when(running, |el| {
                el.child(
                    div()
                        .px(px(tokens::SPACE_3))
                        .py(px(tokens::SPACE_1))
                        .text_size(px(tokens::UI_XS))
                        .text_color(muted)
                        .child("Switching stops the current run"),
                )
            })
            .child(
                v_flex()
                    .id("agent-history-list")
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .p(px(tokens::SPACE_3))
                    .gap(px(tokens::SPACE_1))
                    .when(rows.is_empty(), |el| {
                        el.child(
                            div()
                                .w_full()
                                .py(px(tokens::SPACE_6))
                                .text_size(px(tokens::UI_SM))
                                .text_color(muted)
                                .child("No earlier threads yet."),
                        )
                    })
                    .children(rows),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn thread_title_comes_from_the_first_question() {
        let items = vec![
            AgentItem::ToolGroup {
                id: 0,
                header: "Agent used 1 tool".into(),
                collapsed: false,
                calls: vec![],
                thinking: String::new(),
                thinking_open: false,
                checkpoint: None,
            },
            AgentItem::Message {
                is_user: false,
                text: "I looked at the tree.".into(),
            },
            AgentItem::Message {
                is_user: true,
                text: "\n  Make the sidebar collapsible and keep the tree state\nsecond line".into(),
            },
            AgentItem::Message {
                is_user: true,
                text: "a later question".into(),
            },
        ];
        let title = thread_title_from(&items).expect("a title");
        assert!(title.starts_with("Make the sidebar collapsible"));
        assert!(title.ends_with('\u{2026}'));
        assert_eq!(title.chars().count(), 42);
        assert_eq!(thread_title_from(&[]), None);
        assert_eq!(
            thread_title_from(&[AgentItem::Message {
                is_user: false,
                text: "only a reply".into()
            }]),
            None
        );
    }

    #[core::prelude::v1::test]
    fn parses_antigravity_stream_json_events() {
        assert_eq!(
            parse_agent_stream_line(r#"{"event":"init","conversation_id":"abc","init":{}}"#),
            vec![StreamFragment::SessionId("abc".into())]
        );
        assert_eq!(
            parse_agent_stream_line(
                r#"{"event":"step_update","step_update":{"state":"ACTIVE","step_type":"agent_response","text_delta":"hi"}}"#
            ),
            vec![StreamFragment::TextDelta("hi".into())]
        );
        let started = parse_agent_stream_line(
            r#"{"event":"step_update","step_update":{"state":"ACTIVE","step_type":"tool","tool_name":"run_command","tool_info":{"parameters":{"CommandLine":"ls -la"}}}}"#,
        );
        assert!(matches!(
            started.as_slice(),
            [StreamFragment::Tool(t)] if t.kind == AgentToolKind::Terminal && t.label == "ls -la"
        ));
        // The same tool finishing must not be reported twice.
        assert!(parse_agent_stream_line(
            r#"{"event":"step_update","step_update":{"state":"DONE","step_type":"tool","tool_name":"run_command","tool_info":{"parameters":{"CommandLine":"ls -la"}}}}"#
        )
        .is_empty());
    }

    #[core::prelude::v1::test]
    fn parses_claude_opencode_and_codex_stream_json_events() {
        let claude_line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"lib/main.dart"}},{"type":"text","text":"Updated `main.dart`."}]}}"#;
        let frags = parse_agent_stream_line(claude_line);
        assert_eq!(
            frags,
            vec![
                StreamFragment::Tool(AgentToolCall {
                    kind: AgentToolKind::File,
                    label: "Edit main.dart".into(),
                    target_path: Some("lib/main.dart".into()),
                    ..Default::default()
                }),
                StreamFragment::ReplaceText("Updated `main.dart`.".into()),
            ]
        );

        let opencode_line = r#"{"type":"tool_use","part":{"tool":"bash","state":{"input":{"command":"cargo test"}}}}"#;
        assert_eq!(
            parse_agent_stream_line(opencode_line),
            vec![StreamFragment::Tool(AgentToolCall {
                kind: AgentToolKind::Terminal,
                label: "cargo test".into(),
                target_path: None,
                ..Default::default()
            })]
        );

        let codex_line = r#"{"type":"item.completed","item":{"type":"agent_message","text":"All tests pass."}}"#;
        assert_eq!(
            parse_agent_stream_line(codex_line),
            vec![StreamFragment::ReplaceText("All tests pass.".into())]
        );
    }

    #[core::prelude::v1::test]
    fn a_finished_claude_plan_is_captured_from_exitplanmode() {
        let line = r###"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"ExitPlanMode","input":{"plan":"## Plan\n1. Do it"}}]}}"###;
        let frags = parse_agent_stream_line(line);
        assert_eq!(frags[0], StreamFragment::Plan("## Plan\n1. Do it".into()));
        assert!(matches!(&frags[1], StreamFragment::Tool(call) if call.label == "ExitPlanMode"));
        // An ExitPlanMode with no plan (newer versions read the plan file) only reports the tool.
        let bare = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"ExitPlanMode","input":{}}]}}"#;
        assert_eq!(parse_agent_stream_line(bare).len(), 1);
    }

    #[core::prelude::v1::test]
    fn the_indicator_says_what_the_agent_is_doing() {
        let tool = |kind, label: &str| AgentToolCall { kind, label: label.into(), ..Default::default() };
        assert_eq!(activity_label(None), "Thinking");
        assert_eq!(activity_label(Some(&tool(AgentToolKind::File, "Read main.dart"))), "Reading main.dart");
        assert_eq!(activity_label(Some(&tool(AgentToolKind::File, "Edit lib/a.dart"))), "Editing lib/a.dart");
        assert_eq!(activity_label(Some(&tool(AgentToolKind::File, "Write plan.md"))), "Writing plan.md");
        assert_eq!(activity_label(Some(&tool(AgentToolKind::Terminal, "cargo test"))), "Running a command");
        assert_eq!(activity_label(Some(&tool(AgentToolKind::File, "ExitPlanMode"))), "Finishing the plan");
        assert_eq!(activity_label(Some(&tool(AgentToolKind::File, "TodoWrite"))), "Working");
    }

    #[core::prelude::v1::test]
    fn extracts_conversation_ids_from_claude_and_codex() {
        let claude = r#"{"type":"system","subtype":"init","session_id":"abc-123"}"#;
        assert_eq!(parse_agent_stream_line(claude), vec![StreamFragment::SessionId("abc-123".into())]);

        let claude_result = r#"{"type":"result","result":"Done.","session_id":"abc-123"}"#;
        assert_eq!(
            parse_agent_stream_line(claude_result),
            vec![
                StreamFragment::SessionId("abc-123".into()),
                StreamFragment::ReplaceText("Done.".into()),
            ]
        );

        let with_cost = r#"{"type":"result","result":"Ok.","session_id":"s","total_cost_usd":0.0824,"duration_ms":12400}"#;
        assert_eq!(
            parse_agent_stream_line(with_cost),
            vec![
                StreamFragment::SessionId("s".into()),
                StreamFragment::Stats { cost_usd: Some(0.0824), duration_ms: Some(12400) },
                StreamFragment::ReplaceText("Ok.".into()),
            ]
        );

        let codex = r#"{"type":"thread.started","thread_id":"0199-uuid"}"#;
        assert_eq!(parse_agent_stream_line(codex), vec![StreamFragment::SessionId("0199-uuid".into())]);

        // An assistant event carries a session id too, but only system/result are trusted.
        let assistant = r#"{"type":"assistant","session_id":"x","message":{"content":[]}}"#;
        assert!(parse_agent_stream_line(assistant).is_empty());
    }

    #[core::prelude::v1::test]
    fn transcript_keeps_recent_turns_and_skips_placeholders() {
        let msg = |is_user, text: &str| AgentItem::Message { is_user, text: text.to_string() };
        let items = vec![
            msg(true, "first question"),
            msg(false, "first answer"),
            AgentItem::ToolGroup {
                id: 1,
                header: String::new(),
                collapsed: false,
                calls: vec![],
                thinking: String::new(),
                thinking_open: false,
                checkpoint: None,
            },
            msg(true, "second question"),
            msg(false, "Thinking\u{2026}"),
        ];
        assert_eq!(
            conversation_transcript(&items, 8, 100),
            "User: first question\n\nAssistant: first answer\n\nUser: second question"
        );
        assert_eq!(conversation_transcript(&items, 2, 100), "Assistant: first answer\n\nUser: second question");
        assert_eq!(
            conversation_transcript(&[msg(true, "abcdefghij")], 8, 4),
            "User: abcd\u{2026}"
        );
    }

    #[core::prelude::v1::test]
    fn follows_the_stream_only_when_near_the_bottom() {
        assert!(near_bottom(0.0, 0.0, 40.0), "nothing to scroll");
        assert!(near_bottom(-500.0, 500.0, 40.0), "exactly at the end");
        assert!(near_bottom(-470.0, 500.0, 40.0), "within the slack");
        assert!(!near_bottom(-200.0, 500.0, 40.0), "scrolled up to read");
    }

    #[core::prelude::v1::test]
    fn parses_thinking_todos_usage_results_and_helpers() {
        let line = r#"{"type":"assistant","parent_tool_use_id":null,"message":{"usage":{"input_tokens":10,"cache_read_input_tokens":40000,"cache_creation_input_tokens":2000,"output_tokens":300},"content":[{"type":"thinking","thinking":"Check the tests first."},{"type":"tool_use","id":"t1","name":"TodoWrite","input":{"todos":[{"content":"Run tests","status":"in_progress","activeForm":"Running tests"},{"content":"Fix bug","status":"pending","activeForm":"Fixing bug"}]}},{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"cargo test"}}]}}"#;
        let frags = parse_agent_stream_line(line);
        assert_eq!(
            frags[0],
            StreamFragment::Usage(TokenUsage { context: Some(42_010), output: Some(300), window: None })
        );
        assert_eq!(frags[1], StreamFragment::Thinking("Check the tests first.".into()));
        let StreamFragment::Todos(todos) = &frags[2] else { panic!("{:?}", frags[2]) };
        assert_eq!(todos[0].active.as_deref(), Some("Running tests"));
        assert!(!todos[1].done && todos[1].active.is_none());
        let StreamFragment::Tool(call) = &frags[3] else { panic!() };
        assert_eq!((call.id.as_deref(), call.label.as_str()), (Some("t2"), "cargo test"));
        assert_eq!(frags.len(), 4, "TodoWrite becomes the checklist, not a tool row");

        // A helper agent's text is not the answer; its tool calls are nested.
        let helper = r#"{"type":"assistant","parent_tool_use_id":"task1","message":{"content":[{"type":"text","text":"Looking around"},{"type":"tool_use","id":"t9","name":"Read","input":{"file_path":"/p/a.rs"}}]}}"#;
        let frags = parse_agent_stream_line(helper);
        assert_eq!(frags.len(), 1);
        let StreamFragment::Tool(call) = &frags[0] else { panic!() };
        assert_eq!(call.parent_id.as_deref(), Some("task1"));

        let result = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","is_error":true,"content":"failed"}]}}"#;
        assert_eq!(
            parse_agent_stream_line(result),
            vec![StreamFragment::ToolResult { id: "t2".into(), is_error: true }]
        );
    }

    #[core::prelude::v1::test]
    fn failed_turns_become_errors_not_replies() {
        let claude = r#"{"type":"result","is_error":true,"subtype":"success","terminal_reason":"api_error","result":"API Error: 429 rate_limit_error","session_id":"s","modelUsage":{"m":{"contextWindow":200000}}}"#;
        let frags = parse_agent_stream_line(claude);
        assert!(frags.contains(&StreamFragment::TurnError("API Error: 429 rate_limit_error".into())));
        assert!(frags.contains(&StreamFragment::Usage(TokenUsage { window: Some(200_000), ..Default::default() })));
        assert!(!frags.iter().any(|f| matches!(f, StreamFragment::ReplaceText(_))));

        let codex = r#"{"type":"turn.failed","error":{"message":"stream disconnected"}}"#;
        assert_eq!(parse_agent_stream_line(codex), vec![StreamFragment::TurnError("stream disconnected".into())]);
        let codex_usage = r#"{"type":"turn.completed","usage":{"input_tokens":5000,"cached_input_tokens":4000,"output_tokens":20}}"#;
        assert_eq!(
            parse_agent_stream_line(codex_usage),
            vec![StreamFragment::Usage(TokenUsage { context: Some(5000), output: Some(20), window: None })]
        );
        let codex_todo = r#"{"type":"item.updated","item":{"type":"todo_list","items":[{"text":"a","completed":true},{"text":"b","completed":false},{"text":"c","completed":false}]}}"#;
        let StreamFragment::Todos(todos) = &parse_agent_stream_line(codex_todo)[0] else { panic!() };
        assert_eq!(todos.iter().filter(|t| t.active.is_some()).count(), 1);
        assert_eq!(todos[1].active.as_deref(), Some("b"));
    }

    #[core::prelude::v1::test]
    fn model_search_matches_every_word_over_label_and_id() {
        let models = AgentHarness::ClaudeCode.models();
        let hit = |q: &str| -> Vec<&str> {
            filter_models(&models, q).into_iter().map(|ix| models[ix].id.as_str()).collect()
        };
        assert!(hit("sonnet 4.5").contains(&"claude-sonnet-4-5"), "the model that was missing from the picker");
        assert!(hit("SONNET").len() >= 3, "aliases and dated ids all match");
        assert_eq!(hit("haiku 5.5"), vec!["claude-haiku-5-5"]);
        assert_eq!(hit("").len(), models.len(), "empty query lists everything");
        assert!(hit("no-such-model").is_empty());
    }

    #[core::prelude::v1::test]
    fn each_harness_exposes_its_model_list() {
        for h in AgentHarness::ALL {
            assert!(!h.models().is_empty(), "{h:?} should have models");
        }
    }
}
