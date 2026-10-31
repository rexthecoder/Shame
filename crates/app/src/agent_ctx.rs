//! Pure building blocks of the agent composer: what the agent is allowed to do
//! (modes), what the user attached (files and selections), how that becomes
//! prompt text, and the small `/commands`. No UI here, so it is all testable.

use std::path::Path;

/// How much the agent may change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModeId {
    /// Answer questions and explain; never touch files.
    Ask,
    /// Investigate and propose a step-by-step plan; never touch files.
    Plan,
    /// Edit files in the workspace; anything riskier is not auto-approved.
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
    /// Index into [`AGENT_MODES`].
    pub(crate) fn index(self) -> usize {
        AGENT_MODES.iter().position(|m| m.id == self).unwrap_or(0)
    }

    /// `claude --permission-mode` value.
    pub(crate) fn claude_permission(self) -> &'static str {
        match self {
            ModeId::Ask => "default",
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
    pub(crate) fn instruction(self) -> Option<&'static str> {
        match self {
            ModeId::Ask => Some(
                "[Mode: Ask] Answer the question and explain. Do NOT create, edit or delete any file, and do not run commands that change anything.",
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
        assert_eq!(ModeId::Ask.claude_permission(), "default");
        assert_eq!(ModeId::Plan.claude_permission(), "plan");
        assert_eq!(ModeId::Edit.claude_permission(), "acceptEdits");
        assert_eq!(ModeId::Auto.claude_permission(), "bypassPermissions");
        assert_eq!(ModeId::Ask.codex_sandbox(), "read-only");
        assert_eq!(ModeId::Edit.codex_sandbox(), "workspace-write");
        assert_eq!(ModeId::Auto.codex_approval(), "never");
        assert!(ModeId::Ask.instruction().is_some() && ModeId::Plan.instruction().is_some());
        assert!(ModeId::Edit.instruction().is_none());
        for (ix, mode) in AGENT_MODES.iter().enumerate() {
            assert_eq!(mode.id.index(), ix);
        }
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
    fn slash_commands_parse_only_whole_known_words() {
        assert_eq!(parse_slash_command("/clear"), Some(SlashCommand::NewThread));
        assert_eq!(parse_slash_command(" /Plan "), Some(SlashCommand::SetMode(ModeId::Plan)));
        assert_eq!(parse_slash_command("/model"), Some(SlashCommand::PickModel));
        assert_eq!(parse_slash_command("/files"), Some(SlashCommand::AddContext));
        assert_eq!(parse_slash_command("/etc/hosts is odd"), None);
        assert_eq!(parse_slash_command("/unknown"), None);
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
    fn history_steps_back_and_forth() {
        assert_eq!(history_step(0, None, true), None);
        assert_eq!(history_step(3, None, true), Some(2));
        assert_eq!(history_step(3, Some(2), true), Some(1));
        assert_eq!(history_step(3, Some(0), true), Some(0));
        assert_eq!(history_step(3, Some(1), false), Some(2));
        assert_eq!(history_step(3, Some(2), false), None, "past the newest returns to a blank draft");
    }
}
