//! Parsing for the data-file views: CSV/TSV tables and JSONL records,
//! including chat-style training datasets. No UI here; `data_view` renders it.
//!
//! CSV cells keep the byte range of their raw text, so an edit replaces just
//! that range in the buffer: the rest of the file (quoting style, line
//! endings, rows past the display limit) stays byte-for-byte as it was.

use std::ops::Range;
use std::path::Path;

use serde_json::Value;

/// Rows shown at most; editing the shown rows stays safe past it because
/// edits are byte-range replacements.
pub(crate) const MAX_ROWS: usize = 200_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DataKind {
    /// Delimited text, with its delimiter.
    Csv(u8),
    Jsonl,
}

impl DataKind {
    pub(crate) fn for_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "csv" => Some(Self::Csv(b',')),
            "tsv" | "tab" => Some(Self::Csv(b'\t')),
            "jsonl" | "ndjson" => Some(Self::Jsonl),
            _ => None,
        }
    }

    /// The label for the view in the tab bar's mode switch.
    pub(crate) fn view_label(self) -> &'static str {
        match self {
            Self::Csv(_) => "Table",
            Self::Jsonl => "Preview",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cell {
    /// The raw text in the source, quotes included.
    pub(crate) raw: Range<usize>,
    /// The decoded value.
    pub(crate) value: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CsvTable {
    pub(crate) rows: Vec<Vec<Cell>>,
    /// More rows exist past [`MAX_ROWS`].
    pub(crate) truncated: bool,
}

impl CsvTable {
    pub(crate) fn column_count(&self) -> usize {
        self.rows.iter().map(Vec::len).max().unwrap_or(0)
    }
}

/// Parse RFC 4180 delimited text: quoted fields may hold delimiters, `""`
/// escapes and newlines. A trailing newline does not make an empty row.
pub(crate) fn parse_csv(src: &str, delim: u8) -> CsvTable {
    let bytes = src.as_bytes();
    let mut table = CsvTable::default();
    let mut row: Vec<Cell> = Vec::new();
    let mut i = 0;
    // Skip a UTF-8 byte-order mark.
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        i = 3;
    }
    let len = bytes.len();
    if i >= len {
        return table;
    }
    loop {
        // One field starting at `i`.
        let start = i;
        let mut value = String::new();
        if i < len && bytes[i] == b'"' {
            i += 1;
            let mut seg = i;
            loop {
                if i >= len {
                    value.push_str(&src[seg..len]);
                    break;
                }
                if bytes[i] == b'"' {
                    value.push_str(&src[seg..i]);
                    if i + 1 < len && bytes[i + 1] == b'"' {
                        value.push('"');
                        i += 2;
                        seg = i;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            // Anything between the closing quote and the delimiter is kept.
            let tail = i;
            while i < len && bytes[i] != delim && bytes[i] != b'\n' && bytes[i] != b'\r' {
                i += 1;
            }
            value.push_str(&src[tail..i]);
        } else {
            while i < len && bytes[i] != delim && bytes[i] != b'\n' && bytes[i] != b'\r' {
                i += 1;
            }
            value.push_str(&src[start..i]);
        }
        row.push(Cell { raw: start..i, value });

        if i < len && bytes[i] == delim {
            i += 1;
            continue;
        }
        // End of row (newline or end of input).
        table.rows.push(std::mem::take(&mut row));
        if i < len && bytes[i] == b'\r' {
            i += 1;
        }
        if i < len && bytes[i] == b'\n' {
            i += 1;
        }
        if i >= len {
            break;
        }
        if table.rows.len() >= MAX_ROWS {
            table.truncated = true;
            break;
        }
    }
    table
}

/// `value` as a field: quoted only when it has to be.
pub(crate) fn quote_field(value: &str, delim: u8) -> String {
    let needs = value.bytes().any(|b| b == delim || b == b'"' || b == b'\n' || b == b'\r')
        || value.starts_with(' ')
        || value.ends_with(' ');
    if needs {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// One line of a JSONL file.
#[derive(Debug, Clone)]
pub(crate) struct JsonlRecord {
    /// 0-indexed source line.
    pub(crate) line: usize,
    pub(crate) value: Result<Value, String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct JsonlFile {
    pub(crate) records: Vec<JsonlRecord>,
    pub(crate) truncated: bool,
}

pub(crate) fn parse_jsonl(src: &str) -> JsonlFile {
    let mut file = JsonlFile::default();
    for (line, text) in src.lines().enumerate() {
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if file.records.len() >= MAX_ROWS {
            file.truncated = true;
            break;
        }
        let value = serde_json::from_str::<Value>(text).map_err(|e| e.to_string());
        file.records.push(JsonlRecord { line, value });
    }
    file
}

/// One turn of a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatTurn {
    pub(crate) role: String,
    pub(crate) content: String,
}

/// Text of a message `content`: a string, or OpenAI-style parts
/// (`[{"type": "text", "text": ...}, {"type": "image_url", ...}]`).
fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|p| match p {
                Value::String(s) => s.clone(),
                Value::Object(o) => match o.get("text").and_then(Value::as_str) {
                    Some(t) => t.to_string(),
                    None => format!("[{}]", o.get("type").and_then(Value::as_str).unwrap_or("part")),
                },
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn str_field<'a>(o: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| o.get(*k).and_then(Value::as_str))
}

/// Read a record as a conversation, if it is one of the common training
/// formats: OpenAI/HF chat (`messages: [{role, content}]`), ShareGPT
/// (`conversations: [{from, value}]`), prompt/completion, or Alpaca
/// (`instruction`/`input`/`output`). Tool calls show as a short marker.
pub(crate) fn chat_turns(value: &Value) -> Option<Vec<ChatTurn>> {
    let o = value.as_object()?;
    for key in ["messages", "conversations", "conversation", "chat"] {
        if let Some(list) = o.get(key).and_then(Value::as_array) {
            let mut turns = Vec::new();
            if let Some(system) = str_field(o, &["system", "system_prompt"]) {
                turns.push(ChatTurn { role: "system".into(), content: system.into() });
            }
            for m in list {
                let m = m.as_object()?;
                let role = str_field(m, &["role", "from", "speaker"])?;
                let role = match role {
                    "human" | "user" => "user",
                    "gpt" | "assistant" | "model" | "bot" => "assistant",
                    other => other,
                };
                let mut content = m
                    .get("content")
                    .or_else(|| m.get("value"))
                    .or_else(|| m.get("text"))
                    .map(content_text)
                    .unwrap_or_default();
                if let Some(calls) = m.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let name = call
                            .pointer("/function/name")
                            .or_else(|| call.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("tool");
                        if !content.is_empty() {
                            content.push('\n');
                        }
                        content.push_str(&format!("→ {name}()"));
                    }
                }
                turns.push(ChatTurn { role: role.into(), content });
            }
            return (!turns.is_empty()).then_some(turns);
        }
    }
    if let (Some(prompt), Some(completion)) = (
        str_field(o, &["prompt", "question", "query"]),
        str_field(o, &["completion", "response", "answer", "chosen"]),
    ) {
        return Some(vec![
            ChatTurn { role: "user".into(), content: prompt.into() },
            ChatTurn { role: "assistant".into(), content: completion.into() },
        ]);
    }
    if let (Some(instruction), Some(output)) =
        (str_field(o, &["instruction"]), str_field(o, &["output"]))
    {
        let mut prompt = instruction.to_string();
        if let Some(input) = str_field(o, &["input"]).filter(|s| !s.is_empty()) {
            prompt.push_str("\n\n");
            prompt.push_str(input);
        }
        return Some(vec![
            ChatTurn { role: "user".into(), content: prompt },
            ChatTurn { role: "assistant".into(), content: output.into() },
        ]);
    }
    None
}

/// Whether the file is a chat dataset: most of its first records read as
/// conversations.
pub(crate) fn is_chat_dataset(file: &JsonlFile) -> bool {
    let sample: Vec<&Value> =
        file.records.iter().filter_map(|r| r.value.as_ref().ok()).take(50).collect();
    !sample.is_empty() && sample.iter().filter(|v| chat_turns(v).is_some()).count() * 2 > sample.len()
}

/// Top-level keys across the first records, in first-seen order: the
/// columns of a plain (non-chat) JSONL table.
pub(crate) fn jsonl_columns(file: &JsonlFile) -> Vec<String> {
    let mut cols: Vec<String> = Vec::new();
    for r in file.records.iter().take(500) {
        if let Ok(Value::Object(o)) = &r.value {
            for k in o.keys() {
                if !cols.iter().any(|c| c == k) {
                    cols.push(k.clone());
                }
            }
        }
    }
    cols
}

/// A JSON value as one line of cell text.
pub(crate) fn compact(value: &Value) -> String {
    match value {
        Value::String(s) => s.replace('\n', " ⏎ "),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(t: &CsvTable) -> Vec<Vec<&str>> {
        t.rows.iter().map(|r| r.iter().map(|c| c.value.as_str()).collect()).collect()
    }

    #[test]
    fn parses_quotes_escapes_and_newlines() {
        let src = "name,notes\n\"Ada, L.\",\"said \"\"hi\"\"\nthen left\"\nBob,\n";
        let t = parse_csv(src, b',');
        assert_eq!(
            values(&t),
            vec![vec!["name", "notes"], vec!["Ada, L.", "said \"hi\"\nthen left"], vec!["Bob", ""]]
        );
        assert_eq!(&src[t.rows[1][0].raw.clone()], "\"Ada, L.\"");
        assert!(!t.truncated);
    }

    #[test]
    fn handles_crlf_tsv_and_bom() {
        let t = parse_csv("\u{feff}a\tb\r\n1\t2\r\n", b'\t');
        assert_eq!(values(&t), vec![vec!["a", "b"], vec!["1", "2"]]);
        assert_eq!(parse_csv("", b',').rows.len(), 0);
        assert_eq!(values(&parse_csv("x", b',')), vec![vec!["x"]]);
    }

    #[test]
    fn quoting_round_trips() {
        for v in ["plain", "a,b", "say \"x\"", "two\nlines", " padded "] {
            let q = quote_field(v, b',');
            let t = parse_csv(&q, b',');
            assert_eq!(t.rows[0][0].value, v, "{q}");
        }
        assert_eq!(quote_field("plain", b','), "plain");
        assert_eq!(quote_field("a\tb", b','), "a\tb");
        assert_eq!(quote_field("a\tb", b'\t'), "\"a\tb\"");
    }

    #[test]
    fn editing_a_cell_by_range_keeps_the_rest() {
        let src = "a,\"b\"\n1,2\n";
        let t = parse_csv(src, b',');
        let cell = &t.rows[1][1];
        let edited = format!("{}{}{}", &src[..cell.raw.start], quote_field("x,y", b','), &src[cell.raw.end..]);
        assert_eq!(edited, "a,\"b\"\n1,\"x,y\"\n");
    }

    #[test]
    fn reads_chat_formats() {
        let openai: Value = serde_json::from_str(
            r#"{"messages":[{"role":"system","content":"Be brief."},{"role":"user","content":[{"type":"text","text":"Hi"}]},{"role":"assistant","content":null,"tool_calls":[{"function":{"name":"search"}}]}]}"#,
        )
        .unwrap();
        let turns = chat_turns(&openai).unwrap();
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[1], ChatTurn { role: "user".into(), content: "Hi".into() });
        assert_eq!(turns[2].content, "→ search()");

        let sharegpt: Value =
            serde_json::from_str(r#"{"conversations":[{"from":"human","value":"Q"},{"from":"gpt","value":"A"}]}"#).unwrap();
        assert_eq!(chat_turns(&sharegpt).unwrap()[1].role, "assistant");

        let alpaca: Value =
            serde_json::from_str(r#"{"instruction":"Add","input":"1+1","output":"2"}"#).unwrap();
        assert_eq!(chat_turns(&alpaca).unwrap()[0].content, "Add\n\n1+1");

        let plain: Value = serde_json::from_str(r#"{"id":1,"score":0.5}"#).unwrap();
        assert!(chat_turns(&plain).is_none());
    }

    #[test]
    fn jsonl_detects_datasets_and_columns() {
        let chat = parse_jsonl("{\"messages\":[{\"role\":\"user\",\"content\":\"a\"}]}\n\n{\"messages\":[{\"role\":\"user\",\"content\":\"b\"}]}\n");
        assert_eq!(chat.records.len(), 2);
        assert_eq!(chat.records[1].line, 2);
        assert!(is_chat_dataset(&chat));

        let plain = parse_jsonl("{\"id\":1,\"text\":\"x\"}\n{\"id\":2,\"label\":true}\nnot json\n");
        assert!(!is_chat_dataset(&plain));
        assert_eq!(jsonl_columns(&plain), vec!["id", "text", "label"]);
        assert!(plain.records[2].value.is_err());
    }
}
