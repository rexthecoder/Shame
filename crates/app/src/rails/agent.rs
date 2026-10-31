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
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            AgentHarness::ClaudeCode => "Claude Code",
            AgentHarness::OpenCode => "OpenCode",
            AgentHarness::Codex => "Codex",
        }
    }

    pub(crate) fn short_label(self) -> &'static str {
        match self {
            AgentHarness::ClaudeCode => "Claude",
            AgentHarness::OpenCode => "OpenCode",
            AgentHarness::Codex => "Codex",
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

#[derive(Clone)]
pub(crate) struct AgentTask {
    pub(crate) label: String,
    pub(crate) done: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentToolKind {
    File,
    Terminal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentToolCall {
    pub(crate) kind: AgentToolKind,
    pub(crate) label: String,
    pub(crate) target_path: Option<String>,
}

#[derive(Clone)]
pub(crate) enum AgentItem {
    ToolGroup {
        id: usize,
        header: String,
        collapsed: bool,
        calls: Vec<AgentToolCall>,
    },
    Message {
        is_user: bool,
        text: String,
    },
}

#[derive(Debug, PartialEq)]
pub(crate) enum StreamFragment {
    Tool(AgentToolCall),
    TextDelta(String),
    ReplaceText(String),
    /// The CLI's id for this conversation, used to resume it on the next message.
    SessionId(String),
    /// What the CLI reported about the finished turn (Claude Code only).
    Stats { cost_usd: Option<f64>, duration_ms: Option<u64> },
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

    match ty {
        // 1. Claude Code `stream-json` assistant message
        "assistant" => {
            if let Some(content) = val
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            {
                for part in content {
                    match part.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                        "text" => {
                            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                                if !t.trim().is_empty() {
                                    out.push(StreamFragment::ReplaceText(t.to_string()));
                                }
                            }
                        }
                        "tool_use" => {
                            let name = part
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Tool");
                            let input = part.get("input");
                            out.push(StreamFragment::Tool(tool_call_from_name_and_input(
                                name, input,
                            )));
                        }
                        _ => {}
                    }
                }
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
            if let Some(res) = val.get("result").and_then(|v| v.as_str()) {
                if !res.trim().is_empty() {
                    out.push(StreamFragment::ReplaceText(res.to_string()));
                }
            }
        }
        // 2. OpenCode `--format json` events
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
        "item.started" | "item.completed" => {
            if let Some(item) = val.get("item") {
                let item_ty = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match item_ty {
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
    }
}

pub(crate) enum AgentStreamEvent {
    Started { pid: u32 },
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
    pub(crate) attach_open: bool,
    pub(crate) attach_search: Entity<InputState>,
    pub(crate) attach_selected: usize,
    pub(crate) model_search: Entity<InputState>,
    pub(crate) model_selected: usize,
    /// Previously sent prompts (oldest first) and the one being recalled with ↑.
    pub(crate) history: Vec<String>,
    pub(crate) history_ix: Option<usize>,
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
            turn_stats: std::collections::HashMap::new(),
            queue: Vec::new(),
            last_mention_query: String::new(),
            include_active_file: true,
            include_problems: true,
            attach_open: false,
            attach_search,
            attach_selected: 0,
            model_search,
            model_selected: 0,
            history: Vec::new(),
            history_ix: None,
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
            AgentItem::ToolGroup { .. } => None,
        })
        .collect();
    let skip = turns.len().saturating_sub(max_messages);
    turns.drain(..skip);
    turns.join("\n\n")
}

/// A macOS notification, fire and forget.
fn notify_desktop(title: &str, body: &str) {
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        applescript_escape(body),
        applescript_escape(title)
    );
    let _ = Command::new("osascript")
        .args(["-e", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

impl DartIde {
    pub(crate) fn stop_agent_run(&mut self, cx: &mut Context<Self>) {
        // Stopping means "stop", not "go on to the next queued message".
        self.agent.queue.clear();
        if let Some(pid) = self.agent.running_pid.take() {
            let _ = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status();
            cx.notify();
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
        let resume_id = match self.agent.harness {
            AgentHarness::ClaudeCode | AgentHarness::Codex => self.agent.session_id.clone(),
            AgentHarness::OpenCode => None,
        };
        let prior_transcript = if self.agent.harness == AgentHarness::OpenCode {
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
        self.agent.items.push(AgentItem::Message {
            is_user: true,
            text: shown_prompt,
        });
        self.agent.scroll.scroll_to_bottom();
        let include_active_file = self.agent.include_active_file;
        let include_problems = self.agent.include_problems;

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

        // Order: environment, mode rule, attachments, earlier turns, the request.
        let mut sections = vec![ctx_header];
        if let Some(rule) = mode.instruction() {
            sections.push(rule.to_string());
        }
        let everything: Vec<Attachment> =
            attachments.iter().chain(mentions.iter()).cloned().collect();
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
        let full_prompt = sections.join("\n\n");

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

        // Reserve a tool group + assistant response bubble for live streaming
        let group_id = self.agent.items.len();
        self.agent.items.push(AgentItem::ToolGroup {
            id: group_id,
            header: format!("{} is working\u{2026}", harness.label()),
            collapsed: false,
            calls: Vec::new(),
        });
        let msg_ix = self.agent.items.len();
        self.agent.items.push(AgentItem::Message {
            is_user: false,
            text: "Thinking\u{2026}".to_string(),
        });
        cx.notify();

        let pre_run_unstaged: std::collections::HashSet<String> = self
            .git
            .status
            .as_ref()
            .map(|s| s.unstaged.iter().map(|f| f.path.clone()).collect())
            .unwrap_or_default();

        let (tx, rx) = async_channel::unbounded::<AgentStreamEvent>();
        std::thread::spawn(move || {
            let mut cmd = Command::new(&bin_path);
            let home = std::env::var("HOME").unwrap_or_default();
            let current_path = std::env::var("PATH").unwrap_or_default();
            let enriched_path = format!(
                "{home}/.local/bin:{home}/.bun/bin:{home}/.opencode/bin:{home}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:{current_path}"
            );
            cmd.current_dir(&root)
                .env("PATH", enriched_path)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());

            match harness {
                AgentHarness::ClaudeCode => {
                    cmd.args(["-p", "--verbose", "--output-format", "stream-json"]);
                    if let Some(id) = &resume_id {
                        cmd.args(["--resume", id]);
                    }
                    if model_id != "default" {
                        cmd.args(["--model", &model_id]);
                    }
                    cmd.args(["--permission-mode", mode.claude_permission(), &full_prompt]);
                }
                AgentHarness::OpenCode => {
                    cmd.args(["run", "--format", "json"]);
                    if !model_id.is_empty() && model_id != "default" {
                        cmd.args(["-m", &model_id]);
                    }
                    cmd.arg(&full_prompt);
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
            let _ = tx.send_blocking(AgentStreamEvent::Finished { error: err_msg });
        });

        let started_at = std::time::Instant::now();
        cx.spawn(async move |weak, cx| {
            let mut streamed_text = String::new();
            let mut stderr_lines: Vec<String> = Vec::new();
            let mut reported_cost: Option<f64> = None;
            while let Ok(evt) = rx.recv().await {
                let alive = weak
                    .update(cx, |this, cx| {
                        let pinned = this.agent.is_pinned_to_bottom();
                        match evt {
                            AgentStreamEvent::Started { pid } => {
                                this.agent.running_pid = Some(pid);
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
                                StreamFragment::Tool(call) => {
                                    if (call.label.starts_with("Edit ")
                                        || call.label.starts_with("Write "))
                                        && let Some(ref target_path) = call.target_path
                                    {
                                        this.agent
                                            .record_ai_touched_path(&this.root, target_path);
                                    }
                                    if let Some(AgentItem::ToolGroup { header, calls, .. }) =
                                        this.agent.items.get_mut(group_id)
                                    {
                                        calls.push(call);
                                        let n = calls.len();
                                        *header = format!(
                                            "Agent used {n} tool{}",
                                            if n == 1 { "" } else { "s" }
                                        );
                                    }
                                }
                                StreamFragment::TextDelta(delta) => {
                                    streamed_text.push_str(&delta);
                                    if let Some(AgentItem::Message { text, .. }) =
                                        this.agent.items.get_mut(msg_ix)
                                    {
                                        *text = streamed_text.trim().to_string();
                                    }
                                }
                                StreamFragment::ReplaceText(new_text) => {
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
                                let failed = error.is_some();
                                if error.is_some() && was_resumed {
                                    // The session may be gone; start fresh next time.
                                    this.agent.session_id = None;
                                }
                                if let Some(AgentItem::ToolGroup { header, calls, .. }) =
                                    this.agent.items.get_mut(group_id)
                                {
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
                                if streamed_text.trim().is_empty() {
                                    let fallback = if !stderr_lines.is_empty() {
                                        Some(stderr_lines.join(" "))
                                    } else {
                                        error
                                    };
                                    if let Some(msg) = fallback {
                                        if let Some(AgentItem::Message { text, .. }) =
                                            this.agent.items.get_mut(msg_ix)
                                        {
                                            *text = msg;
                                        }
                                    }
                                }
                                if let Some(post_status) = core_workspace::repo_status(&this.root) {
                                    for f in &post_status.unstaged {
                                        if !pre_run_unstaged.contains(&f.path) {
                                            this.agent
                                                .record_ai_touched_path(&this.root, &f.path);
                                        }
                                    }
                                }
                                this.refresh_git(cx);
                                this.sync_workspace_files_if_changed(cx);
                                if this.web_preview_open {
                                    this.refresh_web_preview(cx);
                                }
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
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .child(
                                h_flex()
                                    .id("rail-tab-agent")
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
                                this.agent_open = false;
                                cx.notify();
                            })),
                    ),
            )
            // Scrollable conversation body
            .child(
                v_flex()
                    .id("agent-scroll-body")
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .track_scroll(&self.agent.scroll)
                    .p(px(tokens::SPACE_4))
                    .gap(px(tokens::SPACE_4))
                    // Live empty state when no messages have been sent yet
                    .when(self.agent.items.is_empty() && self.agent.tasks.is_empty(), |el| {
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
                                                            "CLI Ready"
                                                        } else {
                                                            "Missing CLI"
                                                        }),
                                                ),
                                        ),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_size(px(tokens::UI_XS))
                                        .text_color(muted)
                                        .child(cli_status),
                                )
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
                                                        .px(px(5.))
                                                        .py(px(1.))
                                                        .rounded(px(3.))
                                                        .bg(cx.theme().background)
                                                        .border_1()
                                                        .border_color(border)
                                                        .text_size(px(9.5))
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
                    // 25. TaskChecklist card
                    .when(!self.agent.tasks.is_empty(), |el| {
                        el.child(
                            v_flex()
                                .w_full()
                                .p(px(tokens::SPACE_4))
                                .gap(px(tokens::SPACE_1))
                                .rounded(px(tokens::RADIUS_CARD))
                                .bg(card_bg)
                                .border_1()
                                .border_color(border)
                                .children(self.agent.tasks.iter().enumerate().map(|(ix, task)| {
                                    let done = task.done;
                                    h_flex()
                                        .id(("agent-task", ix))
                                        .w_full()
                                        .h(px(26.))
                                        .px(px(tokens::SPACE_2))
                                        .items_center()
                                        .gap(px(tokens::SPACE_3))
                                        .rounded(px(tokens::RADIUS_SM))
                                        .cursor_pointer()
                                        .hover(move |s| s.bg(list_hover))
                                        .child(
                                            div()
                                                .size(px(15.))
                                                .rounded(px(tokens::RADIUS_PILL))
                                                .border_1()
                                                .border_color(if done { primary } else { muted })
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .when(done, |c| {
                                                    c.child(
                                                        Icon::new(IconName::Check)
                                                            .with_size(px(9.5))
                                                            .text_color(primary),
                                                    )
                                                }),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w(px(0.))
                                                .truncate()
                                                .text_size(px(12.))
                                                .text_color(if done { muted } else { fg })
                                                .when(done, |s| s.line_through())
                                                .child(task.label.clone()),
                                        )
                                        .on_click(cx.listener(move |this, _e, _w, cx| {
                                            if let Some(t) = this.agent.tasks.get_mut(ix) {
                                                t.done = !t.done;
                                                cx.notify();
                                            }
                                        }))
                                })),
                        )
                    })
                    // 26–28. ToolCallCard, CollapsedGroup, MessageBubble
                    .children(self.agent.items.iter().enumerate().map(|(item_ix, item)| {
                        match item {
                            AgentItem::ToolGroup {
                                id,
                                header,
                                collapsed,
                                calls,
                            } => {
                                let is_collapsed = *collapsed;
                                let group_id = *id;
                                v_flex()
                                    .w_full()
                                    .gap(px(tokens::SPACE_2))
                                    .child(
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
                                    .when(!is_collapsed, |el| {
                                        el.children(calls.iter().enumerate().map(
                                            |(call_ix, call)| {
                                                let target = call.target_path.clone();
                                                let is_term = matches!(
                                                    call.kind,
                                                    AgentToolKind::Terminal
                                                );
                                                h_flex()
                                                    .id((
                                                        "agent-tool-card",
                                                        item_ix * 32 + call_ix,
                                                    ))
                                                    .w_full()
                                                    .min_h(px(32.))
                                                    .px(px(tokens::SPACE_4))
                                                    .py(px(tokens::SPACE_2))
                                                    .items_center()
                                                    .gap(px(tokens::SPACE_3))
                                                    .rounded(px(tokens::RADIUS_CARD))
                                                    .bg(card_bg)
                                                    .border_1()
                                                    .border_color(border)
                                                    .when(target.is_some(), |s| {
                                                        s.cursor_pointer()
                                                            .hover(move |h| h.bg(list_hover))
                                                    })
                                                    .child(if is_term {
                                                        div()
                                                            .font_family(
                                                                cx.theme()
                                                                    .mono_font_family
                                                                    .clone(),
                                                            )
                                                            .text_size(px(tokens::UI_XS))
                                                            .text_color(primary)
                                                            .child(">_")
                                                            .into_any_element()
                                                    } else {
                                                        Icon::new(IconName::FileCode)
                                                            .with_size(px(13.))
                                                            .text_color(cx.theme().info)
                                                            .into_any_element()
                                                    })
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w(px(0.))
                                                            .truncate()
                                                            .text_size(px(tokens::UI_SM))
                                                            .text_color(fg)
                                                            .child(call.label.clone()),
                                                    )
                                                    .when_some(target, |s, rel_path| {
                                                        s.on_click(cx.listener(
                                                            move |this, _e, window, cx| {
                                                                let p = Path::new(&rel_path);
                                                                let full = if p.is_absolute() {
                                                                    p.to_path_buf()
                                                                } else {
                                                                    this.root.join(p)
                                                                };
                                                                if full.exists() {
                                                                    this.open_path(
                                                                        full, window, cx,
                                                                    );
                                                                }
                                                            },
                                                        ))
                                                    })
                                            },
                                        ))
                                    })
                                    .into_any_element()
                            }
                            AgentItem::Message { is_user, text } => {
                                let streaming = is_running
                                    && !*is_user
                                    && item_ix + 1 == self.agent.items.len();
                                let can_copy = !*is_user && !streaming && !text.trim().is_empty();
                                let to_copy = text.clone();
                                let stats_text = (!*is_user && !streaming)
                                    .then(|| self.agent.turn_stats.get(&item_ix))
                                    .flatten()
                                    .map(|st| {
                                        crate::agent_ctx::format_turn_stats(st.duration_ms, st.cost_usd)
                                    });
                                v_flex()
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
                                    } else {
                                        crate::markdown::render_markdown(text, item_ix, cx)
                                    })
                                    .when(can_copy, |el| {
                                        el.child(
                                            h_flex()
                                                .w_full()
                                                .items_center()
                                                .justify_between()
                                                .child(
                                                    div()
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
                                            ),
                                        )
                                    })
                                    .into_any_element()
                            }
                        }
                    })),
            )
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
                                                        for f in &to_revert {
                                                            this.git_discard_clicked(
                                                                f.clone(),
                                                                window,
                                                                cx,
                                                            );
                                                        }
                                                        this.agent.ai_touched_paths.clear();
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
                                                            this.agent
                                                                .ai_touched_paths
                                                                .retain(|p| p != &discard_file.path);
                                                            this.git_discard_clicked(
                                                                discard_file.clone(),
                                                                window,
                                                                cx,
                                                            );
                                                            cx.notify();
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
            // 30. Composer: mode switch, context chips, input, tool / model pickers
            .child(self.render_agent_composer(cx))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            })]
        );

        let codex_line = r#"{"type":"item.completed","item":{"type":"agent_message","text":"All tests pass."}}"#;
        assert_eq!(
            parse_agent_stream_line(codex_line),
            vec![StreamFragment::ReplaceText("All tests pass.".into())]
        );
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
            AgentItem::ToolGroup { id: 1, header: String::new(), collapsed: false, calls: vec![] },
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
