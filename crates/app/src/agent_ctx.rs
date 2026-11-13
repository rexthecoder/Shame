//! Pure building blocks of the agent composer: what the agent is allowed to do
//! (modes), what the user attached (files and selections), how that becomes
//! prompt text, and the small `/commands`. No UI here, so it is all testable.

use std::path::Path;

/// How much the agent may do, and who decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModeId {
    /// Manual: it asks before it edits a file or runs a command, and you
    /// approve or deny each one. (Where a tool can't ask, it is read-only.)
    Ask,
    /// Investigate and propose a step-by-step plan; never touch files.
    Plan,
    /// Like Claude's "accept edits": file edits go through; commands still ask.
    Edit,
    /// Edit and run commands without asking.
    Auto,
}

pub(crate) struct AgentMode {
    pub id: ModeId,
    pub label: &'static str,
}

pub(crate) const AGENT_MODES: [AgentMode; 4] = [
    AgentMode { id: ModeId::Ask, label: "Ask" },
    AgentMode { id: ModeId::Plan, label: "Plan" },
    AgentMode { id: ModeId::Edit, label: "Edit" },
    AgentMode { id: ModeId::Auto, label: "Auto" },
];

impl ModeId {
    /// Stable name used when saving preferences (not the position in the list,
    /// which changes when modes are added).
    pub(crate) fn key(self) -> &'static str {
        match self {
            ModeId::Ask => "ask",
            ModeId::Plan => "plan",
            ModeId::Edit => "edit",
            ModeId::Auto => "auto",
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<ModeId> {
        AGENT_MODES.iter().map(|m| m.id).find(|id| id.key() == key)
    }

    /// Index into [`AGENT_MODES`].
    pub(crate) fn index(self) -> usize {
        AGENT_MODES.iter().position(|m| m.id == self).unwrap_or(0)
    }

    /// `claude --permission-mode` value.
    pub(crate) fn claude_permission(self) -> &'static str {
        match self {
            ModeId::Ask => "manual",
            ModeId::Plan => "plan",
            ModeId::Edit => "acceptEdits",
            ModeId::Auto => "bypassPermissions",
        }
    }

    /// Codex `sandbox_mode` config value.
    pub(crate) fn codex_sandbox(self) -> &'static str {
        match self {
            ModeId::Ask | ModeId::Plan => "read-only",
            ModeId::Edit | ModeId::Auto => "workspace-write",
        }
    }

    /// Codex `approval_policy`: only Auto runs without asking.
    pub(crate) fn codex_approval(self) -> &'static str {
        match self {
            ModeId::Auto => "never",
            _ => "on-request",
        }
    }

    /// Leading instruction for every harness. CLIs without a read-only switch
    /// (OpenCode) rely on this alone, so it is worded as a hard rule.
    /// `can_prompt` is whether the tool can ask you for approval mid-run; if it can,
    /// Ask needs no rule (it will simply ask), and if not it is read-only.
    pub(crate) fn instruction(self, can_prompt: bool) -> Option<&'static str> {
        match self {
            ModeId::Ask if can_prompt => None,
            ModeId::Ask => Some(
                "[Mode: Ask] This tool cannot ask the user for approval, so work read-only: answer and explain, and do NOT create, edit or delete any file or run commands that change anything.",
            ),
            ModeId::Plan => Some(
                "[Mode: Plan] Investigate as needed, then reply with a concise step-by-step plan. Do NOT create, edit or delete any file; wait for approval before changing anything.",
            ),
            ModeId::Edit | ModeId::Auto => None,
        }
    }
}

/// Something the user attached to the next message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Attachment {
    /// A whole file, by workspace-relative path.
    File { rel: String },
    /// Lines `start_line..=end_line` (1-indexed) of a file.
    Selection { rel: String, start_line: usize, end_line: usize, text: String },
    /// An image on disk (absolute path): pasted, dropped or chosen. The agent
    /// is pointed at the file rather than sent pixels.
    Image { path: String },
    /// A folder: the agent gets a listing and reads what it needs.
    Folder { rel: String },
    /// A declaration, sent with the lines that follow it (`line` is 1-indexed).
    Symbol { name: String, rel: String, line: usize },
    /// Live IDE state, resolved when the message is sent.
    Context(ContextKind),
    /// A run from the Runs tab: its command, outcome and output (the tail,
    /// already captured when attached).
    Run { title: String, command: String, cwd: String, status: String, output: String },
}

/// IDE state a mention can pull in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContextKind {
    /// Current errors and warnings in open files.
    Problems,
    /// Uncommitted changes (`git diff HEAD`).
    GitDiff,
    /// Recent terminal output.
    Terminal,
}

impl ContextKind {
    pub(crate) const ALL: [ContextKind; 3] =
        [ContextKind::Problems, ContextKind::GitDiff, ContextKind::Terminal];

    pub(crate) fn name(self) -> &'static str {
        match self {
            ContextKind::Problems => "problems",
            ContextKind::GitDiff => "diff",
            ContextKind::Terminal => "terminal",
        }
    }

    /// One-line description shown in the picker.
    pub(crate) fn description(self) -> &'static str {
        match self {
            ContextKind::Problems => "errors and warnings in open files",
            ContextKind::GitDiff => "your uncommitted changes",
            ContextKind::Terminal => "recent terminal output",
        }
    }
}

impl Attachment {
    pub(crate) fn label(&self) -> String {
        let name = |rel: &str| {
            Path::new(rel)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(rel)
                .to_string()
        };
        match self {
            Attachment::File { rel } => name(rel),
            Attachment::Image { path } => name(path),
            Attachment::Folder { rel } => {
                let trimmed = rel.trim_end_matches('/');
                format!("{}/", name(trimmed))
            }
            Attachment::Symbol { name, .. } => name.clone(),
            Attachment::Context(kind) => kind.name().to_string(),
            Attachment::Run { title, .. } => format!("run: {title}"),
            Attachment::Selection { rel, start_line, end_line, .. } if start_line == end_line => {
                format!("{}:{start_line}", name(rel))
            }
            Attachment::Selection { rel, start_line, end_line, .. } => {
                format!("{}:{start_line}\u{2013}{end_line}", name(rel))
            }
        }
    }
}

/// Image formats the agents can look at.
pub(crate) fn is_image_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "bmp" | "tif" | "tiff")
    )
}

/// What to attach for a path the user dropped, pasted or picked. Files inside
/// the workspace are referenced relatively; anything else by absolute path.
/// Directories can't be attached.
pub(crate) fn classify_path(path: &Path, root: &Path) -> Option<Attachment> {
    if path.is_dir() {
        return None;
    }
    if is_image_path(path) {
        return Some(Attachment::Image { path: path.to_string_lossy().to_string() });
    }
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    Some(Attachment::File { rel })
}

/// Add `new` unless an identical attachment is already there.
pub(crate) fn add_attachment(list: &mut Vec<Attachment>, new: Attachment) {
    if !list.contains(&new) {
        list.push(new);
    }
}

/// Entries listed for a mentioned folder.
const MAX_FOLDER_ENTRIES: usize = 80;
/// Lines sent after a mentioned symbol's declaration.
const SYMBOL_CONTEXT_LINES: usize = 40;

/// Folders never worth listing.
const SKIPPED_DIRS: &[&str] = &[".git", "target", "build", "node_modules", ".dart_tool", "Pods"];

/// Files under `dir` (paths relative to it), breadth-first so shallow files
/// come first, at most `limit` of them, skipping build output and dotfiles.
pub(crate) fn folder_listing(dir: &Path, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut queue = std::collections::VecDeque::from([(dir.to_path_buf(), 0usize)]);
    while let Some((current, depth)) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(&current) else { continue };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || SKIPPED_DIRS.contains(&name.as_str()) {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                if depth < 3 {
                    queue.push_back((path, depth + 1));
                }
            } else if let Ok(rel) = path.strip_prefix(dir) {
                out.push(rel.to_string_lossy().to_string());
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }
    out
}

/// Largest file inlined into the prompt; bigger ones are sent as a path only
/// (the agent can read them itself).
const MAX_INLINE_BYTES: usize = 24 * 1024;
/// Total inlined bytes across all attachments of one message.
const MAX_TOTAL_BYTES: usize = 96 * 1024;

fn fence_lang(rel: &str) -> &'static str {
    match Path::new(rel).extension().and_then(|e| e.to_str()).unwrap_or("") {
        "dart" => "dart",
        "rs" => "rust",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "md" => "markdown",
        "js" | "mjs" => "javascript",
        "ts" => "typescript",
        "py" => "python",
        "swift" => "swift",
        "kt" => "kotlin",
        "html" => "html",
        "css" => "css",
        "sh" => "bash",
        _ => "",
    }
}

/// Cut `text` to at most `max` bytes on a char boundary.
fn clip(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

/// `text` cut to at most `max` bytes (on a char boundary), and whether it was cut.
pub(crate) fn clip_text(text: &str, max: usize) -> (&str, bool) {
    clip(text, max)
}

/// Something that holds terminal output: the live terminal's screen and
/// scrollback, or recorded lines.
pub(crate) trait TerminalText {
    /// The last `max_lines` lines as plain text.
    fn recent_text(&self, max_lines: usize) -> String;
}

impl TerminalText for crate::editor::terminal::TermHandle {
    fn recent_text(&self, max_lines: usize) -> String {
        crate::editor::terminal::TermHandle::recent_text(self, max_lines)
    }
}

/// Recorded lines (the Debug Console's), without colors.
impl TerminalText for [crate::editor::dock::TerminalLine] {
    fn recent_text(&self, max_lines: usize) -> String {
        use crate::editor::dock::TerminalLine;
        let skip = self.len().saturating_sub(max_lines);
        self[skip..]
            .iter()
            .map(|line| match line {
                TerminalLine::Output { spans, .. } => spans.iter().map(|s| s.text.as_str()).collect(),
            })
            .collect::<Vec<String>>()
            .join("\n")
    }
}

impl TerminalText for Vec<crate::editor::dock::TerminalLine> {
    fn recent_text(&self, max_lines: usize) -> String {
        self.as_slice().recent_text(max_lines)
    }
}

/// The last `max_lines` lines of a terminal session as plain text.
pub(crate) fn terminal_text<T: TerminalText + ?Sized>(lines: &T, max_lines: usize) -> String {
    lines.recent_text(max_lines)
}

/// The "[Attached context]" block for the prompt: file contents and selected
/// lines in fenced blocks, within a size budget. Empty when nothing is attached.
pub(crate) fn attachment_prompt(root: &Path, attachments: &[Attachment]) -> String {
    if attachments.is_empty() {
        return String::new();
    }
    let mut out = String::from("[Attached context]\n");
    let mut budget = MAX_TOTAL_BYTES;
    for attachment in attachments {
        match attachment {
            Attachment::File { rel } => {
                let text = std::fs::read(root.join(rel))
                    .ok()
                    .filter(|bytes| !bytes.contains(&0))
                    .and_then(|bytes| String::from_utf8(bytes).ok());
                match text {
                    Some(text) if text.len() <= MAX_INLINE_BYTES.min(budget) => {
                        budget -= text.len();
                        out.push_str(&format!(
                            "File `{rel}`:\n```{}\n{}\n```\n",
                            fence_lang(rel),
                            text.trim_end_matches('\n')
                        ));
                    }
                    Some(text) => {
                        let (head, _) = clip(&text, MAX_INLINE_BYTES.min(budget));
                        budget -= head.len();
                        out.push_str(&format!(
                            "File `{rel}` (first part only, {} bytes total; read the rest yourself if needed):\n```{}\n{}\n```\n",
                            text.len(),
                            fence_lang(rel),
                            head.trim_end_matches('\n')
                        ));
                    }
                    None => out.push_str(&format!("File `{rel}` (binary or unreadable; not inlined)\n")),
                }
            }
            Attachment::Image { path } => out.push_str(&format!(
                "Image `{path}` is attached: open it with your file-reading tool to look at it.\n"
            )),
            Attachment::Folder { rel } => {
                let listing = folder_listing(&root.join(rel), MAX_FOLDER_ENTRIES);
                out.push_str(&format!(
                    "Folder `{rel}` contains (read what you need):\n{}\n",
                    if listing.is_empty() { "(empty)".to_string() } else { listing.join("\n") }
                ));
            }
            Attachment::Symbol { name, rel, line } => {
                let snippet = std::fs::read_to_string(root.join(rel))
                    .ok()
                    .map(|text| {
                        text.lines()
                            .skip(line.saturating_sub(1))
                            .take(SYMBOL_CONTEXT_LINES)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let (snippet, _) = clip(&snippet, MAX_INLINE_BYTES.min(budget));
                budget -= snippet.len();
                out.push_str(&format!(
                    "Symbol `{name}` defined at `{rel}` line {line}:\n```{}\n{}\n```\n",
                    fence_lang(rel),
                    snippet.trim_end_matches('\n')
                ));
            }
            // Resolved from live IDE state when the message is sent.
            Attachment::Context(_) => {}
            Attachment::Run { title, command, cwd, status, output } => {
                let (output, cut) = clip(output, MAX_INLINE_BYTES.min(budget));
                budget -= output.len();
                out.push_str(&format!(
                    "Run `{title}` ({status}), from `{cwd}`:\n$ {command}\nOutput{}:\n```text\n{}\n```\n",
                    if cut { " (truncated)" } else { "" },
                    output.trim_end_matches('\n')
                ));
            }
            Attachment::Selection { rel, start_line, end_line, text } => {
                let (text, cut) = clip(text, MAX_INLINE_BYTES.min(budget));
                budget -= text.len();
                out.push_str(&format!(
                    "Selected lines {start_line}\u{2013}{end_line} of `{rel}`{}:\n```{}\n{}\n```\n",
                    if cut { " (truncated)" } else { "" },
                    fence_lang(rel),
                    text.trim_end_matches('\n')
                ));
            }
        }
    }
    out
}

/// A `/command` typed as the whole message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlashCommand {
    NewThread,
    SetMode(ModeId),
    PickModel,
    AddContext,
    /// Summarize the conversation so far to free up context.
    Compact,
    /// Run the last message again.
    Retry,
    /// Put back the files the last turn changed.
    Undo,
}

pub(crate) fn parse_slash_command(input: &str) -> Option<SlashCommand> {
    let word = input.trim().strip_prefix('/')?;
    Some(match word.to_ascii_lowercase().as_str() {
        "clear" | "new" => SlashCommand::NewThread,
        "ask" => SlashCommand::SetMode(ModeId::Ask),
        "plan" => SlashCommand::SetMode(ModeId::Plan),
        "edit" => SlashCommand::SetMode(ModeId::Edit),
        "auto" => SlashCommand::SetMode(ModeId::Auto),
        "model" => SlashCommand::PickModel,
        "file" | "files" | "context" => SlashCommand::AddContext,
        "compact" => SlashCommand::Compact,
        "retry" | "regenerate" => SlashCommand::Retry,
        "undo" => SlashCommand::Undo,
        _ => return None,
    })
}

/// The `@query` being typed at `cursor`: the byte range from the `@` to the
/// cursor, if that `@` starts a word and nothing but non-space characters sit
/// between. `tokens` are the byte ranges of finished inline mentions, which
/// are never treated as a new one.
pub(crate) fn active_mention(
    text: &str,
    cursor: usize,
    tokens: &[std::ops::Range<usize>],
) -> Option<std::ops::Range<usize>> {
    if cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }
    let before = &text[..cursor];
    let at = before.rfind(|c: char| c == '@' || c.is_whitespace())?;
    if !before[at..].starts_with('@') {
        return None;
    }
    if before[..at].chars().last().is_some_and(|c| !c.is_whitespace()) {
        return None; // `me@example.com`, not a mention
    }
    let range = at..cursor;
    if tokens.iter().any(|t| t.start < range.end && range.start < t.end) {
        return None;
    }
    Some(range)
}

/// A message typed while the agent was still busy, with the context that was
/// attached to it at the time.
#[derive(Clone, Debug, Default)]
pub(crate) struct QueuedPrompt {
    pub prompt: String,
    pub shelf: Vec<Attachment>,
    pub mentions: Vec<Attachment>,
}

/// How a finished turn is summarized under its reply: `12s · $0.082`.
pub(crate) fn format_turn_stats(duration_ms: u64, cost_usd: Option<f64>) -> String {
    let secs = duration_ms as f64 / 1000.0;
    let time = if duration_ms < 1000 {
        "<1s".to_string()
    } else if secs < 10.0 {
        format!("{secs:.1}s")
    } else if secs < 60.0 {
        format!("{}s", secs.round() as u64)
    } else {
        let total = secs.round() as u64;
        format!("{}m {:02}s", total / 60, total % 60)
    };
    match cost_usd.filter(|c| *c > 0.0) {
        Some(c) if c < 1.0 => format!("{time} \u{b7} ${c:.3}"),
        Some(c) => format!("{time} \u{b7} ${c:.2}"),
        None => time,
    }
}

/// A token count the way people say it: `950`, `12.4k`, `200k`, `1.2M`.
pub(crate) fn format_tokens(tokens: u64) -> String {
    match tokens {
        0..=999 => tokens.to_string(),
        1_000..=9_999 => format!("{:.1}k", tokens as f64 / 1000.0),
        10_000..=999_999 => format!("{}k", (tokens as f64 / 1000.0).round() as u64),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.0),
    }
}

/// Escape text for use inside an AppleScript string literal.
pub(crate) fn applescript_escape(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

/// A one-line preview of a reply for a notification: whitespace collapsed,
/// markdown fences and backticks dropped, cut to `max` characters.
pub(crate) fn notification_body(reply: &str, max: usize) -> String {
    let flat: String = reply
        .replace("```", " ")
        .replace('`', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max).collect();
    format!("{}\u{2026}", cut.trim_end())
}

/// The "working" spinner's frames, as in Claude Code's: a glyph that grows and shrinks.
const SPINNER: [&str; 6] = ["\u{b7}", "\u{2722}", "\u{2733}", "\u{2736}", "\u{273b}", "\u{273d}"];

/// Which spinner glyph to show `elapsed_ms` into a run: it cycles up through the
/// frames and back down again, a new frame every 120 ms.
pub(crate) fn spinner_glyph(elapsed_ms: u64) -> &'static str {
    let n = SPINNER.len();
    let period = 2 * (n - 1);
    let step = (elapsed_ms / 120) as usize % period;
    SPINNER[if step < n { step } else { period - step }]
}

/// Brightness (0.45 to 1.0) of the `index`th character of the activity label: a
/// soft wave that travels along the text, so it reads as alive while it waits.
pub(crate) fn shimmer_alpha(index: usize, elapsed_ms: u64) -> f32 {
    let phase = elapsed_ms as f32 / 1600.0 * std::f32::consts::TAU - index as f32 * 0.55;
    0.725 + 0.275 * phase.sin()
}

/// "7s", "42s", "1m 05s" for a running time.
pub(crate) fn format_elapsed(elapsed_ms: u64) -> String {
    let secs = elapsed_ms / 1000;
    if secs < 60 { format!("{secs}s") } else { format!("{}m {:02}s", secs / 60, secs % 60) }
}

/// Whether `path` is a plan file Claude Code wrote (`~/.claude/plans/<name>.md`).
pub(crate) fn is_plan_file(path: &str) -> bool {
    path.ends_with(".md") && path.contains("/.claude/plans/")
}

/// Whether a reply reads like a plan (several steps or sections) rather than an
/// answer to a question: used for harnesses that have no plan file.
pub(crate) fn looks_like_plan(text: &str) -> bool {
    let steps = text
        .lines()
        .map(str::trim_start)
        .filter(|l| {
            l.starts_with("- ")
                || l.starts_with("* ")
                || l.starts_with('#')
                || l.split_once(". ").is_some_and(|(n, _)| !n.is_empty() && n.len() <= 2 && n.chars().all(|c| c.is_ascii_digit()))
        })
        .count();
    steps >= 3 && text.len() >= 200
}

/// The text of the plan to show: the plan file's contents if one was written,
/// else the plan the agent passed when it finished planning, else (if it looks
/// like a plan) the reply itself.
pub(crate) fn choose_plan_text(
    file_text: Option<String>,
    tool_plan: Option<String>,
    reply: &str,
) -> Option<String> {
    let non_empty = |t: Option<String>| t.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    non_empty(file_text)
        .or_else(|| non_empty(tool_plan))
        .or_else(|| looks_like_plan(reply).then(|| reply.trim().to_string()))
}

/// Step through prompt history: `Up` goes to older prompts, `Down` to newer
/// ones and finally back to an empty draft (`None`).
pub(crate) fn history_step(len: usize, current: Option<usize>, older: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match (current, older) {
        (None, true) => Some(len - 1),
        (Some(ix), true) => Some(ix.saturating_sub(1)),
        (Some(ix), false) if ix + 1 < len => Some(ix + 1),
        (Some(_), false) | (None, false) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn modes_map_to_each_harness_setting() {
        assert_eq!(ModeId::Ask.claude_permission(), "manual", "Ask is Claude's manual mode");
        assert_eq!(ModeId::Plan.claude_permission(), "plan");
        assert_eq!(ModeId::Edit.claude_permission(), "acceptEdits");
        assert_eq!(ModeId::Auto.claude_permission(), "bypassPermissions");
        assert_eq!(ModeId::Ask.codex_sandbox(), "read-only");
        assert_eq!(ModeId::Edit.codex_sandbox(), "workspace-write");
        assert_eq!(ModeId::Auto.codex_approval(), "never");
        // Where the tool can ask, Ask is just "ask me"; where it can't, it is read-only.
        assert_eq!(ModeId::Ask.instruction(true), None);
        assert!(ModeId::Ask.instruction(false).unwrap().contains("read-only"));
        assert!(ModeId::Plan.instruction(true).is_some() && ModeId::Plan.instruction(false).is_some());
        assert!(ModeId::Edit.instruction(true).is_none() && ModeId::Auto.instruction(false).is_none());
        for (ix, mode) in AGENT_MODES.iter().enumerate() {
            assert_eq!(mode.id.index(), ix);
            assert_eq!(ModeId::from_key(mode.id.key()), Some(mode.id), "saved by name, restored by name");
        }
        assert_eq!(ModeId::from_key("nonsense"), None);
    }

    #[core::prelude::v1::test]
    fn attachments_deduplicate_and_label() {
        let mut list = Vec::new();
        add_attachment(&mut list, Attachment::File { rel: "lib/main.dart".into() });
        add_attachment(&mut list, Attachment::File { rel: "lib/main.dart".into() });
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].label(), "main.dart");
        let sel = |a, b| Attachment::Selection { rel: "lib/a.dart".into(), start_line: a, end_line: b, text: String::new() };
        assert_eq!(sel(4, 4).label(), "a.dart:4");
        assert_eq!(sel(4, 9).label(), "a.dart:4\u{2013}9");
    }

    #[core::prelude::v1::test]
    fn a_run_reaches_the_agent_with_its_command_and_output() {
        let run = Attachment::Run {
            title: "pytest".into(),
            command: ".venv/bin/python -m pytest -q".into(),
            cwd: "/p".into(),
            status: "Failed · exit 1".into(),
            output: "E   assert 1 == 2\n".into(),
        };
        assert_eq!(run.label(), "run: pytest");
        let prompt = attachment_prompt(Path::new("/p"), &[run]);
        assert!(prompt.contains("Run `pytest` (Failed · exit 1)"), "{prompt}");
        assert!(prompt.contains("$ .venv/bin/python -m pytest -q"));
        assert!(prompt.contains("E   assert 1 == 2"));
    }

    #[core::prelude::v1::test]
    fn prompt_inlines_small_files_and_selections() {
        let dir = std::env::temp_dir().join(format!("agent-ctx-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib/main.dart"), "void main() {}\n").unwrap();
        std::fs::write(dir.join("lib/blob.bin"), [0u8, 1, 2]).unwrap();
        let attachments = vec![
            Attachment::File { rel: "lib/main.dart".into() },
            Attachment::File { rel: "lib/blob.bin".into() },
            Attachment::File { rel: "lib/missing.dart".into() },
            Attachment::Selection { rel: "lib/main.dart".into(), start_line: 1, end_line: 1, text: "void main() {}".into() },
        ];
        let prompt = attachment_prompt(&dir, &attachments);
        assert!(prompt.starts_with("[Attached context]\n"));
        assert!(prompt.contains("File `lib/main.dart`:\n```dart\nvoid main() {}\n```"));
        assert!(prompt.contains("`lib/blob.bin` (binary or unreadable; not inlined)"));
        assert!(prompt.contains("`lib/missing.dart` (binary or unreadable; not inlined)"));
        assert!(prompt.contains("Selected lines 1\u{2013}1 of `lib/main.dart`:\n```dart\nvoid main() {}\n```"));
        assert_eq!(attachment_prompt(&dir, &[]), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[core::prelude::v1::test]
    fn large_files_are_cut_not_dropped() {
        let dir = std::env::temp_dir().join(format!("agent-ctx-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("big.txt"), "é".repeat(20_000)).unwrap(); // 40 KB of 2-byte chars
        let prompt = attachment_prompt(&dir, &[Attachment::File { rel: "big.txt".into() }]);
        assert!(prompt.contains("first part only, 40000 bytes total"));
        assert!(prompt.len() < MAX_INLINE_BYTES + 500, "cut near the inline limit, got {}", prompt.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[core::prelude::v1::test]
    fn classifies_dropped_paths() {
        let dir = std::env::temp_dir().join(format!("agent-ctx-drop-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib/a.dart"), "x").unwrap();
        std::fs::write(dir.join("shot.PNG"), "x").unwrap();
        // Inside the workspace -> relative; image -> absolute Image; directory -> none.
        assert_eq!(
            classify_path(&dir.join("lib/a.dart"), &dir),
            Some(Attachment::File { rel: "lib/a.dart".into() })
        );
        assert_eq!(
            classify_path(&dir.join("shot.PNG"), &dir),
            Some(Attachment::Image { path: dir.join("shot.PNG").to_string_lossy().to_string() })
        );
        assert_eq!(classify_path(&dir.join("lib"), &dir), None);
        // Outside the workspace keeps its absolute path.
        let outside = std::env::temp_dir().join(format!("agent-ctx-out-{}.txt", std::process::id()));
        std::fs::write(&outside, "hello").unwrap();
        assert_eq!(
            classify_path(&outside, &dir),
            Some(Attachment::File { rel: outside.to_string_lossy().to_string() })
        );
        let prompt = attachment_prompt(&dir, &[classify_path(&outside, &dir).unwrap()]);
        assert!(prompt.contains("hello"), "an absolute path is read as-is: {prompt}");
        let img = Attachment::Image { path: "/tmp/a/shot.png".into() };
        assert_eq!(img.label(), "shot.png");
        assert!(attachment_prompt(&dir, &[img]).contains("Image `/tmp/a/shot.png` is attached"));
        assert!(is_image_path(Path::new("a.JPG")) && !is_image_path(Path::new("a.dart")));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&outside);
    }

    #[core::prelude::v1::test]
    fn folders_and_symbols_resolve_from_disk() {
        let dir = std::env::temp_dir().join(format!("agent-ctx-mention-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib/src")).unwrap();
        std::fs::create_dir_all(dir.join("lib/build")).unwrap();
        std::fs::write(dir.join("lib/a.dart"), "class A {}\nclass Widget {\n  int x;\n}\n").unwrap();
        std::fs::write(dir.join("lib/src/b.dart"), "b").unwrap();
        std::fs::write(dir.join("lib/build/skip.dart"), "x").unwrap();
        std::fs::write(dir.join("lib/.hidden"), "x").unwrap();

        let listing = folder_listing(&dir.join("lib"), 10);
        assert_eq!(listing, vec!["a.dart", "src/b.dart"], "shallow first; build output and dotfiles skipped");
        assert_eq!(folder_listing(&dir.join("lib"), 1).len(), 1, "limit applies");

        let prompt = attachment_prompt(
            &dir,
            &[
                Attachment::Folder { rel: "lib".into() },
                Attachment::Symbol { name: "Widget".into(), rel: "lib/a.dart".into(), line: 2 },
                Attachment::Context(ContextKind::Problems),
            ],
        );
        assert!(prompt.contains("Folder `lib` contains"));
        assert!(prompt.contains("src/b.dart"));
        assert!(prompt.contains("Symbol `Widget` defined at `lib/a.dart` line 2:\n```dart\nclass Widget {\n  int x;\n}\n```"));
        assert!(!prompt.contains("problems"), "live context is resolved elsewhere");
        assert_eq!(Attachment::Folder { rel: "lib/src".into() }.label(), "src/");
        assert_eq!(Attachment::Context(ContextKind::GitDiff).label(), "diff");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[core::prelude::v1::test]
    fn terminal_text_keeps_recent_lines_as_plain_text() {
        use crate::editor::dock::{AnsiColor, AnsiSpan, TerminalLine};
        let out = |text: &str| TerminalLine::Output {
            spans: vec![
                AnsiSpan { text: text.to_string(), color: AnsiColor::Default, bold: false },
                AnsiSpan { text: "!".to_string(), color: AnsiColor::Default, bold: true },
            ],
            file_link: None,
        };
        let lines = vec![out("old"), out("new"), out("ok")];
        assert_eq!(terminal_text(&lines, 2), "new!\nok!");
        assert_eq!(terminal_text(&lines, 10), "old!\nnew!\nok!");
        assert_eq!(terminal_text(&Vec::<TerminalLine>::new(), 5), "");
        assert_eq!(clip_text("héllo", 2), ("h", true), "cuts on a char boundary");
    }

    #[core::prelude::v1::test]
    fn slash_commands_parse_only_whole_known_words() {
        assert_eq!(parse_slash_command("/clear"), Some(SlashCommand::NewThread));
        assert_eq!(parse_slash_command(" /Plan "), Some(SlashCommand::SetMode(ModeId::Plan)));
        assert_eq!(parse_slash_command("/model"), Some(SlashCommand::PickModel));
        assert_eq!(parse_slash_command("/files"), Some(SlashCommand::AddContext));
        assert_eq!(parse_slash_command("/etc/hosts is odd"), None);
        assert_eq!(parse_slash_command("/unknown"), None);
        assert_eq!(parse_slash_command("/compact"), Some(SlashCommand::Compact));
        assert_eq!(parse_slash_command("/retry"), Some(SlashCommand::Retry));
        assert_eq!(parse_slash_command("/undo"), Some(SlashCommand::Undo));
        assert_eq!(parse_slash_command("clear"), None);
    }

    #[core::prelude::v1::test]
    fn active_mention_finds_the_word_being_typed() {
        // "check this file @gha|"
        let text = "check this file @gha";
        assert_eq!(active_mention(text, text.len(), &[]), Some(16..20));
        // Just the `@`.
        assert_eq!(active_mention("look @", 6, &[]), Some(5..6));
        // The cursor in the middle of the query only counts up to the cursor.
        assert_eq!(active_mention("see @main now", 9, &[]), Some(4..9));
        // Email addresses and plain words are not mentions.
        assert_eq!(active_mention("mail me@example.com", 19, &[]), None);
        assert_eq!(active_mention("no mention here", 15, &[]), None);
        // Whitespace after the query ends it.
        assert_eq!(active_mention("see @main now", 13, &[]), None);
    }

    #[core::prelude::v1::test]
    fn finished_mentions_do_not_reopen_the_picker() {
        let text = "see @main.dart";
        assert_eq!(active_mention(text, text.len(), &[4..14]), None);
        // A different, new mention right after is still found.
        let text = "see @main.dart @te";
        assert_eq!(active_mention(text, text.len(), &[4..14]), Some(15..18));
    }

    #[core::prelude::v1::test]
    fn token_counts_read_naturally() {
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(12_400), "12k");
        assert_eq!(format_tokens(4_300), "4.3k");
        assert_eq!(format_tokens(1_200_000), "1.2M");
    }

    #[test]
    fn turn_stats_read_naturally() {
        assert_eq!(format_turn_stats(400, None), "<1s");
        assert_eq!(format_turn_stats(4_200, None), "4.2s");
        assert_eq!(format_turn_stats(12_400, Some(0.0824)), "12s \u{b7} $0.082");
        assert_eq!(format_turn_stats(65_000, Some(1.5)), "1m 05s \u{b7} $1.50");
        assert_eq!(format_turn_stats(90_000, Some(0.0)), "1m 30s", "zero cost is not shown");
    }

    #[core::prelude::v1::test]
    fn notification_text_is_safe_and_short() {
        assert_eq!(applescript_escape(r#"say "hi" \ there"#), r#"say \"hi\" \\ there"#);
        assert_eq!(applescript_escape("line\nbreak"), "linebreak");
        assert_eq!(notification_body("Done.\n\n```dart\nvoid main() {}\n```\nAll `tests` pass.", 200), "Done. dart void main() {} All tests pass.");
        let long = "word ".repeat(40);
        let body = notification_body(&long, 30);
        assert!(body.chars().count() <= 31 && body.ends_with('\u{2026}'), "{body}");
    }

    #[core::prelude::v1::test]
    fn the_spinner_cycles_up_and_back_and_the_label_shimmers() {
        let frames: Vec<&str> = (0..10).map(|i| spinner_glyph(i * 120)).collect();
        assert_eq!(frames[0], "\u{b7}");
        assert_eq!(frames[5], "\u{273d}", "the biggest frame");
        assert_eq!(frames[6], "\u{273b}", "then it shrinks again");
        assert_eq!(frames[0], spinner_glyph(10 * 120), "and the cycle repeats");
        for ms in (0..4000).step_by(37) {
            for i in 0..12 {
                let a = shimmer_alpha(i, ms);
                assert!((0.44..=1.01).contains(&a), "alpha {a} out of range");
            }
        }
        assert_ne!(shimmer_alpha(0, 0), shimmer_alpha(5, 0), "neighbouring characters differ, so it travels");
        assert_eq!(format_elapsed(7_400), "7s");
        assert_eq!(format_elapsed(65_000), "1m 05s");
    }

    #[core::prelude::v1::test]
    fn plans_are_found_in_files_tool_calls_or_plan_shaped_replies() {
        assert!(is_plan_file("/Users/me/.claude/plans/shame-ide-context-update-rosy-zebra.md"));
        assert!(!is_plan_file("/Users/me/project/PLAN.md"));
        assert!(!is_plan_file("/Users/me/.claude/plans/notes.txt"));

        let plan = "# Plan\n\n1. Add the model\n2. Wire the UI\n3. Write tests\n\nThis covers the whole change end to end, step by step, with the risky parts first so they are verified early. ".repeat(2);
        assert!(looks_like_plan(&plan));
        assert!(!looks_like_plan("Sure! The function returns the sum of both inputs."), "an answer is not a plan");
        assert!(!looks_like_plan("- one\n- two"), "too short to be a plan");

        // The file wins, then the tool's plan, then a plan-shaped reply.
        assert_eq!(
            choose_plan_text(Some("  from file ".into()), Some("from tool".into()), "reply").as_deref(),
            Some("from file")
        );
        assert_eq!(choose_plan_text(Some("  ".into()), Some("from tool".into()), "reply").as_deref(), Some("from tool"));
        assert_eq!(choose_plan_text(None, None, &plan).as_deref(), Some(plan.trim()));
        assert_eq!(choose_plan_text(None, None, "Just an answer."), None);
    }

    #[core::prelude::v1::test]
    fn history_steps_back_and_forth() {
        assert_eq!(history_step(0, None, true), None);
        assert_eq!(history_step(3, None, true), Some(2));
        assert_eq!(history_step(3, Some(2), true), Some(1));
        assert_eq!(history_step(3, Some(0), true), Some(0));
        assert_eq!(history_step(3, Some(1), false), Some(2));
        assert_eq!(history_step(3, Some(2), false), None, "past the newest returns to a blank draft");
    }
}
