//! Markdown for agent replies: a small, streaming-safe parser plus a GPUI
//! renderer. Covers what coding agents actually emit: paragraphs, headings,
//! bullet / numbered lists, block quotes, rules, fenced code blocks (with a
//! language label and a Copy button) and `code`, **bold**, *italic* and
//! [link](url) text inside lines.
//!
//! Replies are rendered while they stream, so the parser never fails: an
//! unclosed fence is a code block that runs to the end of the text, and stray
//! markers are kept as plain text.

use std::ops::Range;

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::tokens;

/// Inline styling of one run of text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: bool,
    pub link_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Block {
    Paragraph(Vec<Span>),
    Heading { level: u8, spans: Vec<Span> },
    /// `marker` is `"\u{2022}"` for bullets or `"1."` etc. for numbered items.
    ListItem { marker: String, indent: usize, spans: Vec<Span> },
    Quote(Vec<Span>),
    Code { lang: String, code: String },
    /// A pipe table: a header row and body rows, each padded to the header's width.
    Table { header: Vec<Vec<Span>>, rows: Vec<Vec<Vec<Span>>> },
    Rule,
}

pub(crate) fn parse_markdown(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut lines = text.lines().peekable();

    fn flush(paragraph: &mut Vec<&str>, blocks: &mut Vec<Block>) {
        if !paragraph.is_empty() {
            blocks.push(Block::Paragraph(parse_inline(&paragraph.join(" "))));
            paragraph.clear();
        }
    }

    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();

        if let Some(rest) = trimmed.strip_prefix("```") {
            flush(&mut paragraph, &mut blocks);
            let lang = rest.trim().to_string();
            let mut code = String::new();
            for body in lines.by_ref() {
                if body.trim_start().starts_with("```") {
                    break;
                }
                if !code.is_empty() {
                    code.push('\n');
                }
                code.push_str(body);
            }
            blocks.push(Block::Code { lang, code });
            continue;
        }

        if trimmed.is_empty() {
            flush(&mut paragraph, &mut blocks);
            continue;
        }

        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Heading {
                level: hashes as u8,
                spans: parse_inline(trimmed[hashes..].trim()),
            });
            continue;
        }

        if is_rule(trimmed) {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Rule);
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix('>') {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Quote(parse_inline(rest.trim())));
            continue;
        }

        if trimmed.contains('|') && lines.peek().is_some_and(|next| is_table_separator(next)) {
            flush(&mut paragraph, &mut blocks);
            let header_cells = split_cells(trimmed);
            lines.next(); // the separator row
            let mut body: Vec<Vec<String>> = Vec::new();
            while let Some(next) = lines.peek() {
                let t = next.trim_start();
                if t.is_empty() || !t.contains('|') {
                    break;
                }
                body.push(split_cells(t));
                lines.next();
            }
            let width = header_cells.len();
            let cells = |row: &[String]| -> Vec<Vec<Span>> {
                (0..width).map(|c| parse_inline(row.get(c).map_or("", String::as_str))).collect()
            };
            blocks.push(Block::Table {
                header: cells(&header_cells),
                rows: body.iter().map(|r| cells(r)).collect(),
            });
            continue;
        }

        if let Some((marker, rest)) = list_marker(trimmed) {
            flush(&mut paragraph, &mut blocks);
            let indent = (line.len() - trimmed.len()) / 2;
            blocks.push(Block::ListItem { marker, indent, spans: parse_inline(rest) });
            continue;
        }

        paragraph.push(trimmed);
    }
    flush(&mut paragraph, &mut blocks);
    blocks
}

fn is_rule(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.len() >= 3 && compact.chars().all(|c| matches!(c, '-' | '*' | '_'))
        && compact.chars().all(|c| c == compact.chars().next().unwrap_or('-'))
}

/// The cells of one `| a | b |` row (an escaped `\|` stays inside its cell).
fn split_cells(line: &str) -> Vec<String> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let line = line.strip_suffix('|').filter(|_| !line.ends_with("\\|")).unwrap_or(line);
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'|') {
            cell.push('|');
            chars.next();
        } else if c == '|' {
            cells.push(cell.trim().to_string());
            cell.clear();
        } else {
            cell.push(c);
        }
    }
    cells.push(cell.trim().to_string());
    cells
}

/// `|---|:--:|` : the row under a table header.
fn is_table_separator(line: &str) -> bool {
    let cells = split_cells(line);
    !cells.is_empty()
        && line.contains('-')
        && cells.iter().all(|c| !c.is_empty() && c.contains('-') && c.chars().all(|ch| matches!(ch, '-' | ':')))
}

/// `- item`, `* item`, `+ item` or `12. item`; `- [ ]` and `- [x]` become checkboxes.
fn list_marker(line: &str) -> Option<(String, &str)> {
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(bullet) {
            let rest = rest.trim_start();
            for (box_text, glyph) in [("[ ] ", "\u{2610}"), ("[x] ", "\u{2611}"), ("[X] ", "\u{2611}")] {
                if let Some(after) = rest.strip_prefix(box_text) {
                    return Some((glyph.to_string(), after.trim_start()));
                }
            }
            return Some(("\u{2022}".to_string(), rest));
        }
    }
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    if (1..=3).contains(&digits) && line[digits..].starts_with(". ") {
        return Some((format!("{}.", &line[..digits]), line[digits + 2..].trim_start()));
    }
    None
}

/// Split one line of text into styled spans.
pub(crate) fn parse_inline(text: &str) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<Span> = Vec::new();
    let mut plain = String::new();
    let (mut bold, mut italic) = (false, false);
    let mut i = 0;

    fn push_plain(plain: &mut String, spans: &mut Vec<Span>, bold: bool, italic: bool) {
        if !plain.is_empty() {
            spans.push(Span { text: std::mem::take(plain), bold, italic, ..Span::default() });
        }
    }

    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            if let Some(close) = chars[i + 1..].iter().position(|&x| x == '`') {
                push_plain(&mut plain, &mut spans, bold, italic);
                let code: String = chars[i + 1..i + 1 + close].iter().collect();
                if !code.is_empty() {
                    spans.push(Span { text: code, code: true, ..Span::default() });
                }
                i += close + 2;
                continue;
            }
        } else if c == '*' && chars.get(i + 1) == Some(&'*') && closes(&chars, i + 2, "**", bold) {
            push_plain(&mut plain, &mut spans, bold, italic);
            bold = !bold;
            i += 2;
            continue;
        } else if (c == '*' || c == '_') && is_emphasis_edge(&chars, i, italic) {
            push_plain(&mut plain, &mut spans, bold, italic);
            italic = !italic;
            i += 1;
            continue;
        } else if c == '[' {
            if let Some((label, url, end)) = link_at(&chars, i) {
                push_plain(&mut plain, &mut spans, bold, italic);
                spans.push(Span {
                    text: label,
                    bold,
                    italic,
                    link: true,
                    link_url: Some(url),
                    ..Span::default()
                });
                i = end;
                continue;
            }
        }
        plain.push(c);
        i += 1;
    }
    push_plain(&mut plain, &mut spans, bold, italic);
    spans
}

/// Whether a `marker` opened at `from - len` is closed later (or we are
/// already inside it, in which case this one closes it).
fn closes(chars: &[char], from: usize, marker: &str, inside: bool) -> bool {
    if inside {
        return true;
    }
    let marker: Vec<char> = marker.chars().collect();
    chars[from.min(chars.len())..]
        .windows(marker.len())
        .any(|w| w == marker.as_slice())
}

/// A single `*` / `_` toggles italics only at a word edge (so `snake_case`
/// and `2 * 3` stay literal) and only if it will be closed.
fn is_emphasis_edge(chars: &[char], i: usize, italic: bool) -> bool {
    let marker = chars[i];
    let prev = i.checked_sub(1).and_then(|p| chars.get(p)).copied();
    let next = chars.get(i + 1).copied();
    if italic {
        // closing: previous char is not a space
        return prev.is_some_and(|p| !p.is_whitespace()) && prev != Some(marker);
    }
    let opens = next.is_some_and(|n| !n.is_whitespace() && n != marker)
        && prev.is_none_or(|p| !p.is_alphanumeric());
    opens && chars[i + 1..].iter().any(|&x| x == marker)
}

/// `[label](url)` starting at `chars[i] == '['`: the label, URL, and the index after `)`.
fn link_at(chars: &[char], i: usize) -> Option<(String, String, usize)> {
    let close = chars[i + 1..].iter().position(|&c| c == ']')? + i + 1;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let paren = chars[close + 2..].iter().position(|&c| c == ')')? + close + 2;
    let label: String = chars[i + 1..close].iter().collect();
    let url: String = chars[close + 2..paren].iter().collect();
    (!label.is_empty()).then_some((label, url.trim().to_string(), paren + 1))
}

/// Text of `spans` joined, and the highlight ranges for it.
fn flatten(spans: &[Span], base: &HighlightStyle, code_style: &HighlightStyle, link_color: Hsla) -> (String, Vec<(Range<usize>, HighlightStyle)>) {
    let mut text = String::new();
    let mut runs = Vec::new();
    for span in spans {
        let start = text.len();
        text.push_str(&span.text);
        let mut style = if span.code { code_style.clone() } else { base.clone() };
        if span.bold {
            style.font_weight = Some(FontWeight::BOLD);
        }
        if span.italic {
            style.font_style = Some(FontStyle::Italic);
        }
        if span.link {
            style.color = Some(link_color);
            style.underline = Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(link_color),
                wavy: false,
            });
        }
        runs.push((start..text.len(), style));
    }
    (text, runs)
}

/// Sizes for rendered markdown: compact for chat replies, roomier for documents (plans).
pub(crate) struct MdStyle {
    body: f32,
    line: f32,
    gap: f32,
    h1: f32,
    h2: f32,
    h3: f32,
    /// Underline the two biggest heading levels, like a document.
    rules: bool,
}

const CHAT_STYLE: MdStyle = MdStyle { body: tokens::UI, line: 19., gap: 6., h1: tokens::UI + 3., h2: tokens::UI + 1.5, h3: tokens::UI, rules: false };
const DOC_STYLE: MdStyle = MdStyle { body: 14., line: 23., gap: 11., h1: 21., h2: 17.5, h3: 15., rules: true };

/// What a code block's Insert button does with the code.
pub(crate) type InsertCode = std::rc::Rc<dyn Fn(String, &mut Window, &mut App)>;
/// What a shell code block's Run button does with the command.
pub(crate) type RunCode = std::rc::Rc<dyn Fn(String, &mut Window, &mut App)>;
/// What clicking a markdown `[label](url)` link does.
pub(crate) type OpenLink = std::rc::Rc<dyn Fn(String, &mut Window, &mut App)>;

#[derive(Clone, Default)]
pub(crate) struct MdCallbacks {
    pub on_insert: Option<InsertCode>,
    pub on_run: Option<RunCode>,
    pub on_link: Option<OpenLink>,
}

/// Chat markdown with full interactive callbacks (Insert, Run in Terminal, and Open Link).
pub(crate) fn render_markdown_interactive(
    text: &str,
    id: usize,
    callbacks: MdCallbacks,
    cx: &App,
) -> Div {
    render_markdown_with(text, id, &CHAT_STYLE, callbacks, cx)
}

/// Render a document (a plan): larger text, more air, underlined headings.
pub(crate) fn render_markdown_doc(text: &str, id: usize, cx: &App) -> Div {
    render_markdown_with(text, id, &DOC_STYLE, MdCallbacks::default(), cx)
}

/// Whether `lang` is an interactive shell fence that can be executed in the terminal dock.
pub(crate) fn is_shell_lang(lang: &str) -> bool {
    matches!(
        lang.trim().to_ascii_lowercase().as_str(),
        "sh" | "bash" | "zsh" | "shell" | "console"
    )
}

/// Map a markdown code-fence tag to a tree-sitter grammar and produce disjoint
/// `StyledText` highlight ranges.
pub(crate) fn code_syntax_highlights(
    lang: &str,
    code: &str,
) -> Vec<(Range<usize>, HighlightStyle)> {
    if code.is_empty() || code.len() > 64 * 1024 {
        return Vec::new();
    }
    let tag = lang
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let syntax_lang = match tag.as_str() {
        "dart" | "flutter" => core_syntax::SyntaxLanguage::Dart,
        "rs" | "rust" => core_syntax::SyntaxLanguage::Rust,
        "ts" | "typescript" => core_syntax::SyntaxLanguage::TypeScript,
        "tsx" => core_syntax::SyntaxLanguage::Tsx,
        "js" | "javascript" | "jsx" | "mjs" | "cjs" => core_syntax::SyntaxLanguage::JavaScript,
        "py" | "python" => core_syntax::SyntaxLanguage::Python,
        _ => return Vec::new(),
    };
    let mut spans = core_syntax::SyntaxTree::parse_as(syntax_lang, code).highlights(code);
    spans.sort_by_key(|s| (s.range.start, s.range.end));
    let mut runs = Vec::with_capacity(spans.len());
    let mut cursor = 0;
    for span in spans {
        let start = span.range.start.max(cursor);
        let end = span.range.end;
        if start < end && end <= code.len() {
            cursor = end;
            runs.push((
                start..end,
                HighlightStyle {
                    color: Some(editor_view::highlight_color(span.kind).into()),
                    ..HighlightStyle::default()
                },
            ));
        }
    }
    runs
}

/// Parse a markdown link target (`file:///path/to/file.rs#L42-L50` or `src/main.rs:42`)
/// into `(file_path, optional_1_based_line)`. Returns `None` for web URLs (`http://`, `https://`).
pub(crate) fn parse_file_link_target(url: &str) -> Option<(String, Option<u32>)> {
    let trimmed = url.trim();
    if trimmed.is_empty()
        || trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("mailto:")
        || trimmed.starts_with("conversation://")
    {
        return None;
    }
    let without_scheme = trimmed.strip_prefix("file://").unwrap_or(trimmed);
    if let Some((path_part, frag)) = without_scheme.split_once('#') {
        let line = frag
            .strip_prefix('L')
            .or_else(|| frag.strip_prefix('l'))
            .unwrap_or(frag)
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|digits| digits.parse::<u32>().ok())
            .filter(|&n| n >= 1);
        return (!path_part.is_empty()).then(|| (path_part.to_string(), line));
    }
    if let Some((path_part, tail)) = without_scheme.rsplit_once(':') {
        let line = tail
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|digits| digits.parse::<u32>().ok())
            .filter(|&n| n >= 1);
        if line.is_some() && !path_part.is_empty() {
            return Some((path_part.to_string(), line));
        }
    }
    Some((without_scheme.to_string(), None))
}

fn render_markdown_with(
    text: &str,
    id: usize,
    style: &MdStyle,
    callbacks: MdCallbacks,
    cx: &App,
) -> Div {
    let fg = cx.theme().foreground;
    let muted = cx.theme().muted_foreground;
    let border = cx.theme().border;
    let primary = cx.theme().primary;
    let code_bg = cx.theme().background;
    let list_hover = cx.theme().list_hover;
    let mono = cx.theme().mono_font_family.clone();

    let base = HighlightStyle::default();
    let code_style = HighlightStyle {
        color: Some(primary),
        background_color: Some(code_bg),
        ..HighlightStyle::default()
    };
    let styled = |spans: &[Span]| {
        let (text, runs) = flatten(spans, &base, &code_style, primary);
        StyledText::new(text).with_highlights(runs)
    };
    let render_links = |spans: &[Span], block_ix: usize| -> Option<Div> {
        let on_link = callbacks.on_link.as_ref()?;
        let links: Vec<(String, String)> = spans
            .iter()
            .filter_map(|s| {
                let url = s.link_url.as_ref()?.trim();
                (!url.is_empty()).then(|| (s.text.clone(), url.to_string()))
            })
            .collect();
        if links.is_empty() {
            return None;
        }
        Some(
            h_flex()
                .flex_wrap()
                .gap(px(4.))
                .mt(px(2.))
                .children(links.into_iter().enumerate().map(|(link_ix, (label, url))| {
                    let on_link = on_link.clone();
                    h_flex()
                        .id(("md-link", id * 100_000 + block_ix * 100 + link_ix))
                        .px(px(6.))
                        .py(px(1.5))
                        .gap(px(4.))
                        .items_center()
                        .rounded(px(tokens::RADIUS_BADGE))
                        .bg(code_bg)
                        .border_1()
                        .border_color(border)
                        .cursor_pointer()
                        .text_size(px(tokens::UI_XS))
                        .text_color(primary)
                        .hover(move |s| s.bg(list_hover).border_color(primary.opacity(0.45)))
                        .child("\u{2197}")
                        .child(label)
                        .on_click(move |_event, window, cx| {
                            on_link(url.clone(), window, cx);
                        })
                })),
        )
    };

    let mut column = v_flex().w_full().gap(px(style.gap)).text_color(fg);
    for (ix, block) in parse_markdown(text).into_iter().enumerate() {
        column = column.child(match block {
            Block::Paragraph(spans) => {
                let link_row = render_links(&spans, ix);
                v_flex()
                    .w_full()
                    .gap(px(2.))
                    .child(
                        div()
                            .w_full()
                            .text_size(px(style.body))
                            .line_height(px(style.line))
                            .child(styled(&spans)),
                    )
                    .when_some(link_row, |el, row| el.child(row))
                    .into_any_element()
            }
            Block::Heading { level, spans } => {
                let size = match level {
                    1 => style.h1,
                    2 => style.h2,
                    _ => style.h3,
                };
                div()
                    .w_full()
                    .mt(px(if ix == 0 { 0. } else { style.gap + 3. }))
                    .when(style.rules && level <= 2, |h| h.pb(px(5.)).border_b_1().border_color(border))
                    .font_weight(FontWeight::BOLD)
                    .text_size(px(size))
                    .line_height(px(size + 5.))
                    .child(styled(&spans))
                    .into_any_element()
            }
            Block::ListItem { marker, indent, spans } => {
                let link_row = render_links(&spans, ix);
                h_flex()
                    .w_full()
                    .items_start()
                    .gap(px(6.))
                    .pl(px(indent as f32 * 16.))
                    .text_size(px(style.body))
                    .line_height(px(style.line))
                    .child(div().flex_shrink_0().min_w(px(18.)).text_color(muted).child(marker))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w(px(0.))
                            .gap(px(2.))
                            .child(div().w_full().child(styled(&spans)))
                            .when_some(link_row, |el, row| el.child(row)),
                    )
                    .into_any_element()
            }
            Block::Quote(spans) => div()
                .w_full()
                .pl(px(8.))
                .border_l_2()
                .border_color(border)
                .text_color(muted)
                .text_size(px(style.body))
                .line_height(px(style.line))
                .child(styled(&spans))
                .into_any_element(),
            Block::Table { header, rows } => {
                let cell = |spans: &[Span], first: bool, head: bool| {
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .px(px(9.))
                        .py(px(6.))
                        .when(!first, |c| c.border_l_1().border_color(border))
                        .when(head, |c| c.font_weight(FontWeight::BOLD))
                        .child(styled(spans))
                };
                v_flex()
                    .w_full()
                    .rounded(px(tokens::RADIUS_SM))
                    .border_1()
                    .border_color(border)
                    .overflow_hidden()
                    .text_size(px(style.body - 0.5))
                    .line_height(px(style.line - 2.))
                    .child(
                        h_flex()
                            .w_full()
                            .bg(muted.opacity(0.10))
                            .children(header.iter().enumerate().map(|(c, spans)| cell(spans, c == 0, true))),
                    )
                    .children(rows.iter().map(|row| {
                        h_flex()
                            .w_full()
                            .border_t_1()
                            .border_color(border)
                            .children(row.iter().enumerate().map(|(c, spans)| cell(spans, c == 0, false)))
                    }))
                    .into_any_element()
            }
            Block::Rule => div().w_full().h(px(1.)).my(px(4.)).bg(border).into_any_element(),
            Block::Code { lang, code } => {
                let to_copy = code.clone();
                let insert = callbacks.on_insert.clone().map(|f| (f, code.clone()));
                let run = callbacks
                    .on_run
                    .clone()
                    .filter(|_| is_shell_lang(&lang))
                    .map(|f| (f, code.clone()));
                let highlights = code_syntax_highlights(&lang, &code);
                let line_count = code.lines().count();
                let header_label = match (lang.is_empty(), line_count > 1) {
                    (true, true) => format!("code \u{b7} {line_count} lines"),
                    (true, false) => "code".to_string(),
                    (false, true) => format!("{lang} \u{b7} {line_count} lines"),
                    (false, false) => lang,
                };
                v_flex()
                    .w_full()
                    .rounded(px(tokens::RADIUS_SM))
                    .bg(code_bg)
                    .border_1()
                    .border_color(border)
                    .overflow_hidden()
                    .child(
                        h_flex()
                            .w_full()
                            .px(px(8.))
                            .py(px(3.))
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(border)
                            .text_size(px(tokens::UI_XS))
                            .text_color(muted)
                            .child(header_label)
                            .child(
                                h_flex()
                                    .gap(px(4.))
                                    .when_some(run, |row, (run, code)| {
                                        row.child(
                                            div()
                                                .id(("md-run", id * 10_000 + ix))
                                                .cursor_pointer()
                                                .px(px(5.))
                                                .rounded(px(3.))
                                                .text_color(primary)
                                                .hover(|s| s.bg(rgba(0xffffff12)))
                                                .child("\u{25b6} Run")
                                                .on_click(move |_event, window, cx| {
                                                    run(code.clone(), window, cx);
                                                }),
                                        )
                                    })
                                    .when_some(insert, |row, (insert, code)| {
                                        row.child(
                                            div()
                                                .id(("md-insert", id * 10_000 + ix))
                                                .cursor_pointer()
                                                .px(px(4.))
                                                .rounded(px(3.))
                                                .hover(|s| s.bg(rgba(0xffffff12)))
                                                .child("Insert")
                                                .on_click(move |_event, window, cx| {
                                                    insert(code.clone(), window, cx);
                                                }),
                                        )
                                    })
                                    .child(
                                        div()
                                            .id(("md-copy", id * 10_000 + ix))
                                            .cursor_pointer()
                                            .px(px(4.))
                                            .rounded(px(3.))
                                            .hover(|s| s.bg(rgba(0xffffff12)))
                                            .child("Copy")
                                            .on_click(move |_event, _window, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    to_copy.clone(),
                                                ));
                                            }),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id(("md-code", id * 10_000 + ix))
                            .w_full()
                            .overflow_x_scroll()
                            .px(px(8.))
                            .py(px(6.))
                            .font_family(mono.clone())
                            .text_size(px(tokens::UI_SM))
                            .line_height(px(17.))
                            .text_color(fg)
                            .whitespace_nowrap()
                            .child(StyledText::new(code).with_highlights(highlights)),
                    )
                    .into_any_element()
            }
        });
    }
    column
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[core::prelude::v1::test]
    fn splits_paragraphs_headings_and_lists() {
        let blocks = parse_markdown("# Title\n\nSome text\nover two lines.\n\n- one\n- two\n  - nested\n\n1. first\n2. second");
        assert!(matches!(&blocks[0], Block::Heading { level: 1, spans } if plain(spans) == "Title"));
        assert!(matches!(&blocks[1], Block::Paragraph(s) if plain(s) == "Some text over two lines."));
        assert!(matches!(&blocks[2], Block::ListItem { marker, indent: 0, .. } if marker == "\u{2022}"));
        assert!(matches!(&blocks[4], Block::ListItem { indent: 1, spans, .. } if plain(spans) == "nested"));
        assert!(matches!(&blocks[5], Block::ListItem { marker, .. } if marker == "1."));
        assert!(matches!(&blocks[6], Block::ListItem { marker, .. } if marker == "2."));
    }

    #[core::prelude::v1::test]
    fn fenced_code_keeps_language_and_inner_blank_lines() {
        let blocks = parse_markdown("Before\n```dart\nvoid main() {\n\n  run();\n}\n```\nAfter");
        assert_eq!(blocks.len(), 3);
        assert_eq!(
            blocks[1],
            Block::Code { lang: "dart".into(), code: "void main() {\n\n  run();\n}".into() }
        );
    }

    #[core::prelude::v1::test]
    fn an_unclosed_fence_streams_as_code_to_the_end() {
        let blocks = parse_markdown("Here:\n```rust\nfn main() {");
        assert_eq!(blocks[1], Block::Code { lang: "rust".into(), code: "fn main() {".into() });
    }

    #[core::prelude::v1::test]
    fn inline_styles() {
        let spans = parse_inline("Use `cargo test` and **really** *do* it");
        assert_eq!(plain(&spans), "Use cargo test and really do it");
        assert!(spans.iter().any(|s| s.code && s.text == "cargo test"));
        assert!(spans.iter().any(|s| s.bold && s.text == "really"));
        assert!(spans.iter().any(|s| s.italic && s.text == "do"));
    }

    #[core::prelude::v1::test]
    fn identifiers_and_math_are_not_emphasis() {
        let spans = parse_inline("call snake_case_name and 2 * 3 * 4");
        assert_eq!(plain(&spans), "call snake_case_name and 2 * 3 * 4");
        assert!(spans.iter().all(|s| !s.italic));
    }

    #[core::prelude::v1::test]
    fn unmatched_markers_stay_literal() {
        assert_eq!(plain(&parse_inline("a ** b")), "a ** b");
        assert_eq!(plain(&parse_inline("tick ` alone")), "tick ` alone");
    }

    #[core::prelude::v1::test]
    fn links_show_their_label() {
        let spans = parse_inline("see [the docs](https://example.com) now");
        assert_eq!(plain(&spans), "see the docs now");
        assert!(spans.iter().any(|s| {
            s.link && s.text == "the docs" && s.link_url.as_deref() == Some("https://example.com")
        }));
    }

    #[core::prelude::v1::test]
    fn parses_file_links_and_highlights_code_blocks() {
        assert_eq!(
            parse_file_link_target("file:///Users/me/project/src/main.rs#L42-L50"),
            Some(("/Users/me/project/src/main.rs".to_string(), Some(42)))
        );
        assert_eq!(
            parse_file_link_target("crates/app/src/main.rs:120"),
            Some(("crates/app/src/main.rs".to_string(), Some(120)))
        );
        assert_eq!(parse_file_link_target("https://example.com/a.rs"), None);
        assert!(is_shell_lang("bash") && is_shell_lang("zsh") && !is_shell_lang("rust"));

        let rust_runs = code_syntax_highlights("rust", "pub fn greet() -> usize { 42 }");
        assert!(!rust_runs.is_empty(), "rust code blocks get tree-sitter highlights");
        // Ranges must be strictly disjoint and ordered for StyledText.
        for pair in rust_runs.windows(2) {
            assert!(pair[0].0.end <= pair[1].0.start);
        }
    }

    #[core::prelude::v1::test]
    fn quotes_and_rules() {
        let blocks = parse_markdown("> careful\n\n---\n\ntext");
        assert!(matches!(&blocks[0], Block::Quote(s) if plain(s) == "careful"));
        assert_eq!(blocks[1], Block::Rule);
    }

    #[core::prelude::v1::test]
    fn pipe_tables_become_a_header_and_padded_rows() {
        let blocks = parse_markdown("Before\n\n| Area | Status |\n|------|:------:|\n| Auth | done |\n| UI |\n| Docs | todo | extra |\n\nAfter");
        assert_eq!(blocks.len(), 3, "paragraph, table, paragraph");
        let Block::Table { header, rows } = &blocks[1] else { panic!("expected a table: {blocks:?}") };
        assert_eq!(header.iter().map(|c| plain(c)).collect::<Vec<_>>(), ["Area", "Status"]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].iter().map(|c| plain(c)).collect::<Vec<_>>(), ["Auth", "done"]);
        assert_eq!(rows[1].iter().map(|c| plain(c)).collect::<Vec<_>>(), ["UI", ""], "a short row is padded");
        assert_eq!(rows[2].len(), 2, "an over-long row is cut to the header's width");
    }

    #[core::prelude::v1::test]
    fn a_pipe_in_ordinary_text_is_not_a_table() {
        let blocks = parse_markdown("Use a | b to mean either.\nSecond line.");
        assert!(matches!(&blocks[0], Block::Paragraph(s) if plain(s) == "Use a | b to mean either. Second line."));
        // A header row with no separator under it is just text too.
        assert!(matches!(&parse_markdown("| a | b |\n| 1 | 2 |")[0], Block::Paragraph(_)));
    }

    #[core::prelude::v1::test]
    fn table_cells_keep_inline_styles_and_escaped_pipes() {
        let blocks = parse_markdown("| Name | Note |\n|---|---|\n| `a.dart` | x \\| y |");
        let Block::Table { rows, .. } = &blocks[0] else { panic!("expected a table") };
        assert!(rows[0][0].iter().any(|s| s.code && s.text == "a.dart"));
        assert_eq!(plain(&rows[0][1]), "x | y");
    }

    #[core::prelude::v1::test]
    fn task_list_items_get_checkbox_markers() {
        let blocks = parse_markdown("- [ ] write tests\n- [x] add model\n- plain");
        let markers: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::ListItem { marker, .. } => Some(marker.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(markers, ["\u{2610}", "\u{2611}", "\u{2022}"]);
        assert!(matches!(&blocks[0], Block::ListItem { spans, .. } if plain(spans) == "write tests"));
    }
}
