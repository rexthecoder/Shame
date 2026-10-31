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
    input::{Input, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

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
            AgentHarness::ClaudeCode => vec![
                ModelOption {
                    id: "sonnet".into(),
                    label: "Claude Sonnet 4.6".into(),
                },
                ModelOption {
                    id: "opus".into(),
                    label: "Claude Opus 4.7".into(),
                },
                ModelOption {
                    id: "haiku".into(),
                    label: "Claude Haiku 4.5".into(),
                },
                ModelOption {
                    id: "default".into(),
                    label: "Default (Account)".into(),
                },
            ],
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

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StreamFragment {
    Tool(AgentToolCall),
    TextDelta(String),
    ReplaceText(String),
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
    pub(crate) input: Entity<InputState>,
    pub(crate) scroll: ScrollHandle,
}

const MODES: &[&str] = &["Default", "Plan", "Auto-Edit"];

impl AgentState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<DartIde>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Message agent\u{2026}"));
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
            mode_ix: 0,
            tool_picker_open: false,
            model_picker_open: false,
            running_pid: None,
            tasks: Vec::new(),
            items: Vec::new(),
            input,
            scroll: ScrollHandle::new(),
        }
    }

    pub(crate) fn set_harness(&mut self, harness: AgentHarness) {
        self.harness = harness;
        self.models = harness.models();
        self.model_ix = 0;
        self.tool_picker_open = false;
    }

    pub(crate) fn active_model(&self) -> (&str, &str) {
        self.models
            .get(self.model_ix)
            .map(|m| (m.id.as_str(), m.label.as_str()))
            .unwrap_or(("default", "Default"))
    }
}

/// Render inline backtick spans (`code`) as small monospace pills inside a message bubble.
fn render_rich_message(text: &str, cx: &App) -> Div {
    let fg = cx.theme().foreground;
    let code_bg = cx.theme().background;
    let border = cx.theme().border;
    let mono = cx.theme().mono_font_family.clone();

    let mut row = h_flex()
        .w_full()
        .flex_wrap()
        .items_center()
        .gap_x(px(3.))
        .gap_y(px(3.))
        .text_size(px(tokens::UI))
        .line_height(px(19.))
        .text_color(fg);

    for (seg_ix, segment) in text.split('`').enumerate() {
        if segment.is_empty() {
            continue;
        }
        if seg_ix % 2 == 1 {
            row = row.child(
                div()
                    .px(px(5.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .bg(code_bg)
                    .border_1()
                    .border_color(border)
                    .font_family(mono.clone())
                    .text_size(px(tokens::UI_SM))
                    .text_color(fg)
                    .child(segment.to_string()),
            );
        } else {
            for word in segment.split_whitespace() {
                row = row.child(div().child(word.to_string()));
            }
        }
    }
    row
}

impl DartIde {
    pub(crate) fn stop_agent_run(&mut self, cx: &mut Context<Self>) {
        if let Some(pid) = self.agent.running_pid.take() {
            let _ = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status();
            cx.notify();
        }
    }

    pub(crate) fn agent_send_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.agent.running_pid.is_some() {
            return;
        }
        let prompt = self.agent.input.read(cx).value().trim().to_string();
        if prompt.is_empty() {
            return;
        }
        self.agent
            .input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.agent_run_prompt(prompt, window, cx);
    }

    pub(crate) fn agent_run_prompt(
        &mut self,
        prompt: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.agent.running_pid.is_some() {
            return;
        }
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        self.agent.tool_picker_open = false;
        self.agent.model_picker_open = false;
        self.agent.items.push(AgentItem::Message {
            is_user: true,
            text: prompt.clone(),
        });

        let harness = self.agent.harness;
        let (model_id, _) = self.agent.active_model();
        let model_id = model_id.to_string();
        let mode = MODES[self.agent.mode_ix % MODES.len()];
        let root = self.root.clone();
        let active_file = self
            .active_editor()
            .and_then(|ed| ed.read(cx).buffer().path().map(Path::to_path_buf))
            .and_then(|p| p.strip_prefix(&self.root).ok().map(Path::to_path_buf))
            .map(|p| p.to_string_lossy().to_string());

        let full_prompt = match &active_file {
            Some(file) => format!("[Active file in IDE: {file}]\n{prompt}"),
            None => prompt,
        };

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
                    if model_id != "default" {
                        cmd.args(["--model", &model_id]);
                    }
                    let perm = match mode {
                        "Plan" => "plan",
                        "Auto-Edit" => "bypassPermissions",
                        _ => "acceptEdits",
                    };
                    cmd.args(["--permission-mode", perm, &full_prompt]);
                }
                AgentHarness::OpenCode => {
                    cmd.args(["run", "--format", "json"]);
                    if !model_id.is_empty() && model_id != "default" {
                        cmd.args(["-m", &model_id]);
                    }
                    cmd.arg(&full_prompt);
                }
                AgentHarness::Codex => {
                    cmd.args(["exec", "--json", "--skip-git-repo-check"]);
                    if !model_id.is_empty() && model_id != "default" {
                        cmd.args(["-c", &format!("model=\"{model_id}\"")]);
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

        cx.spawn(async move |weak, cx| {
            let mut streamed_text = String::new();
            let mut stderr_lines: Vec<String> = Vec::new();
            while let Ok(evt) = rx.recv().await {
                let alive = weak
                    .update(cx, |this, cx| {
                        match evt {
                            AgentStreamEvent::Started { pid } => {
                                this.agent.running_pid = Some(pid);
                            }
                            AgentStreamEvent::StderrLine(line) => {
                                stderr_lines.push(line);
                            }
                            AgentStreamEvent::Fragment(frag) => match frag {
                                StreamFragment::Tool(call) => {
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
                                this.refresh_git(cx);
                                this.sync_workspace_files_if_changed(cx);
                            }
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
        let mode_label = MODES[self.agent.mode_ix % MODES.len()];
        let harness = self.agent.harness;
        let (model_id, model_display) = self.agent.active_model();
        let model_short = model_id.rsplit('/').next().unwrap_or(model_display).to_string();
        let is_running = self.agent.running_pid.is_some();
        let bin_resolved = harness.resolve_binary();
        let active_rel_file = self
            .active_editor()
            .and_then(|ed| ed.read(cx).buffer().path().map(Path::to_path_buf))
            .and_then(|p| p.strip_prefix(&self.root).ok().map(Path::to_path_buf))
            .map(|p| p.to_string_lossy().to_string());

        // Live unstaged git changes from the workspace repository
        let real_unstaged: Vec<core_workspace::ChangedFile> = self
            .git
            .status
            .as_ref()
            .map(|s| s.unstaged.clone())
            .unwrap_or_default();
        let has_real_git = !real_unstaged.is_empty();

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
                                        this.stop_agent_run(cx);
                                        this.agent.tasks.clear();
                                        this.agent.items.clear();
                                        cx.notify();
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
            // 24. AgentHeader (shows active tool + model badge + clear button)
            .child(
                h_flex()
                    .w_full()
                    .h(px(30.))
                    .flex_shrink_0()
                    .px(px(tokens::SPACE_4))
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(border.opacity(0.6))
                    .child(
                        h_flex()
                            .min_w(px(0.))
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .child(
                                div()
                                    .px(px(6.))
                                    .py(px(1.))
                                    .rounded(px(4.))
                                    .bg(primary.opacity(0.15))
                                    .text_size(px(tokens::UI_XS))
                                    .font_medium()
                                    .text_color(primary)
                                    .child(harness.short_label()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(muted)
                                    .child(model_display.to_string()),
                            ),
                    )
                    .child(
                        Button::new("agent-clear-chat")
                            .ghost()
                            .xsmall()
                            .icon(
                                Icon::new(IconName::RotateCcwClock)
                                    .with_size(px(12.))
                                    .text_color(muted),
                            )
                            .tooltip("Clear conversation")
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.stop_agent_run(cx);
                                this.agent.tasks.clear();
                                this.agent.items.clear();
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
                                            .on_click(cx.listener(move |this, _e, window, cx| {
                                                this.agent_run_prompt(
                                                    prompt_owned.clone(),
                                                    window,
                                                    cx,
                                                );
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
                            AgentItem::Message { is_user, text } => div()
                                .w_full()
                                .px(px(tokens::SPACE_5))
                                .py(px(10.))
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
                                .child(render_rich_message(text, cx))
                                .into_any_element(),
                        }
                    })),
            )
            // 29. PendingChanges card (strictly reflects real unstaged git files in the workspace)
            .when(has_real_git, |el| {
                let count = real_unstaged.len();
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
                                                    "{count} Pending Change{}",
                                                    if count == 1 { "" } else { "s" }
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
                                                .on_click(cx.listener(
                                                    move |this, _e, window, cx| {
                                                        this.git_discard_all_clicked(window, cx);
                                                    },
                                                )),
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
                                                    move |this, _e, window, cx| {
                                                        this.git_stage_all(window, cx);
                                                    },
                                                )),
                                        ),
                                ),
                        )
                        .child(div().w_full().h(px(1.)).bg(border.opacity(0.6)))
                        .children(real_unstaged.into_iter().take(6).enumerate().map(
                            |(ix, file)| {
                                let name = Path::new(&file.path)
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or(&file.path)
                                    .to_string();
                                let diff_file = file.clone();
                                let stage_file = file.clone();
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
                                            ),
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
                                                            this.git_discard_clicked(
                                                                discard_file.clone(),
                                                                window,
                                                                cx,
                                                            );
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
                                                        move |this, _e, window, cx| {
                                                            this.git_stage(
                                                                std::slice::from_ref(&stage_file),
                                                                window,
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            ),
                                    )
                                    .on_click(cx.listener(move |this, _e, _w, cx| {
                                        this.open_git_diff(diff_file.clone(), false, cx);
                                    }))
                            },
                        )),
                )
            })
            // 30. Composer (`Message agent…`, Tool selector, Model selector, Mode selector, Send / Stop button)
            .child(
                v_flex()
                    .w_auto()
                    .flex_shrink_0()
                    .mx(px(tokens::SPACE_4))
                    .mb(px(tokens::SPACE_4))
                    .p(px(tokens::SPACE_3))
                    .gap(px(tokens::SPACE_3))
                    .rounded(px(tokens::RADIUS_CARD))
                    .bg(cx.theme().input)
                    .border_1()
                    .border_color(border)
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key.as_str() == "enter"
                            && !event.keystroke.modifiers.shift
                        {
                            this.agent_send_message(window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        div()
                            .w_full()
                            .child(Input::new(&self.agent.input).appearance(false).small()),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .gap(px(tokens::SPACE_2))
                            .child(
                                h_flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap(px(tokens::SPACE_2))
                                    // 1. Coding Tooling selector (`Claude Code` / `OpenCode` / `Codex`)
                                    .child(
                                        h_flex()
                                            .id("agent-tool-select")
                                            .h(px(22.))
                                            .px(px(tokens::SPACE_2))
                                            .items_center()
                                            .gap(px(tokens::SPACE_1))
                                            .rounded(px(tokens::RADIUS_SM))
                                            .bg(cx.theme().background)
                                            .border_1()
                                            .border_color(if self.agent.tool_picker_open {
                                                primary
                                            } else {
                                                border
                                            })
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(list_hover))
                                            .text_size(px(tokens::UI_XS))
                                            .child(
                                                Icon::new(IconName::Bot)
                                                    .with_size(px(11.))
                                                    .text_color(primary),
                                            )
                                            .child(
                                                div()
                                                    .font_medium()
                                                    .text_color(fg)
                                                    .child(harness.short_label()),
                                            )
                                            .child(
                                                Icon::new(IconName::ChevronDown)
                                                    .with_size(px(9.5))
                                                    .text_color(muted),
                                            )
                                            .on_click(cx.listener(|this, _e, _w, cx| {
                                                this.agent.tool_picker_open =
                                                    !this.agent.tool_picker_open;
                                                this.agent.model_picker_open = false;
                                                cx.notify();
                                            })),
                                    )
                                    // 2. Per-tool Model selector
                                    .child(
                                        h_flex()
                                            .id("agent-model-select")
                                            .h(px(22.))
                                            .px(px(tokens::SPACE_2))
                                            .items_center()
                                            .gap(px(tokens::SPACE_1))
                                            .rounded(px(tokens::RADIUS_SM))
                                            .bg(cx.theme().background)
                                            .border_1()
                                            .border_color(if self.agent.model_picker_open {
                                                primary
                                            } else {
                                                border
                                            })
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(list_hover))
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_size(px(tokens::UI_XS))
                                            .child(
                                                div()
                                                    .max_w(px(110.))
                                                    .truncate()
                                                    .text_color(fg)
                                                    .child(model_short),
                                            )
                                            .child(
                                                Icon::new(IconName::ChevronDown)
                                                    .with_size(px(9.5))
                                                    .text_color(muted),
                                            )
                                            .on_click(cx.listener(|this, _e, _w, cx| {
                                                this.agent.model_picker_open =
                                                    !this.agent.model_picker_open;
                                                this.agent.tool_picker_open = false;
                                                cx.notify();
                                            })),
                                    )
                                    // 3. Mode selector (`Default` / `Plan` / `Auto-Edit`)
                                    .child(
                                        h_flex()
                                            .id("agent-mode-select")
                                            .h(px(22.))
                                            .px(px(tokens::SPACE_2))
                                            .items_center()
                                            .gap(px(tokens::SPACE_1))
                                            .rounded(px(tokens::RADIUS_SM))
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(list_hover))
                                            .text_size(px(tokens::UI_XS))
                                            .text_color(muted)
                                            .child(mode_label)
                                            .on_click(cx.listener(|this, _e, _w, cx| {
                                                this.agent.mode_ix =
                                                    (this.agent.mode_ix + 1) % MODES.len();
                                                cx.notify();
                                            })),
                                    ),
                            )
                            // Send or Stop circle button
                            .child(if is_running {
                                div()
                                    .id("agent-stop-btn")
                                    .size(px(26.))
                                    .rounded(px(tokens::RADIUS_PILL))
                                    .bg(red.opacity(0.2))
                                    .border_1()
                                    .border_color(red)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .child(Icon::new(IconName::X).with_size(px(12.)).text_color(red))
                                    .on_click(cx.listener(|this, _e, _w, cx| {
                                        this.stop_agent_run(cx);
                                    }))
                            } else {
                                div()
                                    .id("agent-send-btn")
                                    .size(px(26.))
                                    .rounded(px(tokens::RADIUS_PILL))
                                    .bg(primary.opacity(0.18))
                                    .border_1()
                                    .border_color(primary.opacity(0.55))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(primary.opacity(0.3)))
                                    .child(
                                        Icon::new(IconName::ArrowRight)
                                            .with_size(px(12.5))
                                            .text_color(primary),
                                    )
                                    .on_click(cx.listener(|this, _e, window, cx| {
                                        this.agent_send_message(window, cx);
                                    }))
                            }),
                    ),
            )
            // Popover 1: Coding Tooling Picker (`Claude Code`, `OpenCode`, `Codex`)
            .when(self.agent.tool_picker_open, |el| {
                el.child(
                    v_flex()
                        .id("agent-tool-popover")
                        .absolute()
                        .bottom(px(86.))
                        .left(px(tokens::SPACE_4))
                        .w(px(220.))
                        .p(px(tokens::SPACE_2))
                        .gap(px(tokens::SPACE_1))
                        .rounded(px(tokens::RADIUS_CARD))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(border)
                        .shadow_xl()
                        .child(
                            div()
                                .px(px(tokens::SPACE_2))
                                .py(px(2.))
                                .text_size(px(tokens::UI_XS))
                                .font_medium()
                                .text_color(muted)
                                .child("CODING TOOLING"),
                        )
                        .children(AgentHarness::ALL.iter().copied().enumerate().map(
                            |(ix, tool)| {
                                let is_sel = tool == harness;
                                let installed = tool.resolve_binary().is_some();
                                h_flex()
                                    .id(("agent-tool-opt", ix))
                                    .w_full()
                                    .h(px(28.))
                                    .px(px(tokens::SPACE_3))
                                    .items_center()
                                    .justify_between()
                                    .rounded(px(tokens::RADIUS_SM))
                                    .cursor_pointer()
                                    .when(is_sel, |s| s.bg(tokens::accent_dim()))
                                    .when(!is_sel, |s| s.hover(move |h| h.bg(list_hover)))
                                    .child(
                                        h_flex()
                                            .items_center()
                                            .gap(px(tokens::SPACE_2))
                                            .child(
                                                div()
                                                    .size(px(6.))
                                                    .rounded(px(3.))
                                                    .bg(if installed { green } else { muted }),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(tokens::UI_SM))
                                                    .font_medium()
                                                    .text_color(fg)
                                                    .child(tool.label()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_size(px(tokens::UI_XS))
                                            .text_color(if installed { green } else { muted })
                                            .child(if installed { "ready" } else { "not found" }),
                                    )
                                    .on_click(cx.listener(move |this, _e, _w, cx| {
                                        this.agent.set_harness(tool);
                                        cx.notify();
                                    }))
                            },
                        )),
                )
            })
            // Popover 2: Model Picker under the selected Coding Tool
            .when(self.agent.model_picker_open, |el| {
                let current_ix = self.agent.model_ix;
                el.child(
                    v_flex()
                        .id("agent-model-popover")
                        .absolute()
                        .bottom(px(86.))
                        .left(px(tokens::SPACE_4))
                        .right(px(tokens::SPACE_4))
                        .p(px(tokens::SPACE_2))
                        .gap(px(tokens::SPACE_1))
                        .rounded(px(tokens::RADIUS_CARD))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(border)
                        .shadow_xl()
                        .child(
                            div()
                                .px(px(tokens::SPACE_2))
                                .py(px(2.))
                                .text_size(px(tokens::UI_XS))
                                .font_medium()
                                .text_color(muted)
                                .child(format!("{} MODELS", harness.label().to_uppercase())),
                        )
                        .children(self.agent.models.iter().enumerate().map(|(ix, m)| {
                            let is_sel = ix == current_ix;
                            h_flex()
                                .id(("agent-model-opt", ix))
                                .w_full()
                                .h(px(28.))
                                .px(px(tokens::SPACE_3))
                                .items_center()
                                .justify_between()
                                .gap(px(tokens::SPACE_2))
                                .rounded(px(tokens::RADIUS_SM))
                                .cursor_pointer()
                                .when(is_sel, |s| s.bg(tokens::accent_dim()))
                                .when(!is_sel, |s| s.hover(move |h| h.bg(list_hover)))
                                .child(
                                    h_flex()
                                        .min_w(px(0.))
                                        .items_center()
                                        .gap(px(tokens::SPACE_2))
                                        .child(
                                            div()
                                                .w(px(12.))
                                                .when(is_sel, |c| {
                                                    c.child(
                                                        Icon::new(IconName::Check)
                                                            .xsmall()
                                                            .text_color(primary),
                                                    )
                                                }),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(px(tokens::UI_SM))
                                                .text_color(fg)
                                                .child(m.label.clone()),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_size(px(tokens::UI_XS))
                                        .text_color(muted)
                                        .child(m.id.clone()),
                                )
                                .on_click(cx.listener(move |this, _e, _w, cx| {
                                    this.agent.model_ix = ix;
                                    this.agent.model_picker_open = false;
                                    cx.notify();
                                }))
                        })),
                )
            })
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
    fn each_harness_exposes_its_model_list() {
        for h in AgentHarness::ALL {
            assert!(!h.models().is_empty(), "{h:?} should have models");
        }
    }
}
