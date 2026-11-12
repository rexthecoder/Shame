//! The `.ipynb` (nbformat 4) model.
//!
//! Only what Shame edits is typed: cell kind, source, outputs and execution
//! count. Every other field (notebook and cell metadata, attachments, ids)
//! is kept as JSON and written back unchanged, so opening and saving a
//! notebook never drops what another tool put there. Saving follows
//! Jupyter's own layout (sorted keys, one-space indent, source as a list of
//! lines) so diffs stay small.

use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellKind {
    Code,
    Markdown,
    Raw,
}

impl CellKind {
    fn as_str(self) -> &'static str {
        match self {
            CellKind::Code => "code",
            CellKind::Markdown => "markdown",
            CellKind::Raw => "raw",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub kind: CellKind,
    pub source: String,
    /// nbformat output objects (`stream`, `execute_result`, `display_data`,
    /// `error`), exactly as saved and as the kernel sends them.
    pub outputs: Vec<Value>,
    pub execution_count: Option<i64>,
    /// Every other field of the cell (`id`, `metadata`, `attachments`, ...).
    pub extra: Map<String, Value>,
}

impl Cell {
    pub fn new(kind: CellKind, source: impl Into<String>) -> Self {
        let mut extra = Map::new();
        extra.insert("id".into(), Value::String(new_cell_id()));
        extra.insert("metadata".into(), json!({}));
        Self { kind, source: source.into(), outputs: Vec::new(), execution_count: None, extra }
    }

    /// The cell's nbformat id, if it has one (nbformat 4.5+).
    pub fn id(&self) -> Option<&str> {
        self.extra.get("id").and_then(Value::as_str)
    }

    /// Append a kernel output, merging consecutive text of the same stream
    /// the way Jupyter does.
    pub fn push_output(&mut self, output: Value) {
        if output.get("output_type").and_then(Value::as_str) == Some("stream") {
            if let Some(last) = self.outputs.last_mut() {
                if last.get("output_type").and_then(Value::as_str) == Some("stream")
                    && last.get("name") == output.get("name")
                {
                    let mut text = multiline(last.get("text"));
                    text.push_str(&multiline(output.get("text")));
                    last["text"] = Value::String(text);
                    return;
                }
            }
        }
        self.outputs.push(output);
    }

    fn to_value(&self) -> Value {
        let mut obj = self.extra.clone();
        obj.insert("cell_type".into(), Value::String(self.kind.as_str().into()));
        obj.insert("source".into(), lines_value(&self.source));
        obj.entry("metadata").or_insert_with(|| json!({}));
        if self.kind == CellKind::Code {
            let outputs = self.outputs.iter().map(normalize_output).collect();
            obj.insert("outputs".into(), Value::Array(outputs));
            obj.insert("execution_count".into(), self.execution_count.map_or(Value::Null, Value::from));
        }
        Value::Object(obj)
    }
}

/// A short id for new cells (nbformat wants 1–64 chars of `[a-zA-Z0-9-_]`).
fn new_cell_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let n = nanos ^ COUNTER.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    format!("{:08x}", (n ^ (n >> 29)) as u32)
}

/// nbformat text: a string, or a list of lines to concatenate.
pub fn multiline(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

/// Text as Jupyter saves it: a list of lines, each keeping its `\n`.
fn lines_value(text: &str) -> Value {
    Value::Array(text.split_inclusive('\n').map(|l| Value::String(l.into())).collect())
}

/// Kernel messages carry plain strings; saved notebooks use line lists for
/// `text` and for textual `data` entries (not for base64 images).
fn normalize_output(output: &Value) -> Value {
    let mut out = output.clone();
    if let Some(obj) = out.as_object_mut() {
        if let Some(text) = obj.get("text").and_then(Value::as_str).map(str::to_string) {
            obj.insert("text".into(), lines_value(&text));
        }
        if let Some(Value::Object(data)) = obj.get_mut("data") {
            for (mime, v) in data.iter_mut() {
                if mime.starts_with("text/") || mime == "image/svg+xml" {
                    if let Some(s) = v.as_str().map(str::to_string) {
                        *v = lines_value(&s);
                    }
                }
            }
        }
        let otype = obj.get("output_type").and_then(Value::as_str).unwrap_or("").to_string();
        if matches!(otype.as_str(), "execute_result" | "display_data") {
            obj.entry("metadata").or_insert_with(|| json!({}));
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notebook {
    pub cells: Vec<Cell>,
    /// Every other top-level field (`metadata`, `nbformat`, ...).
    pub extra: Map<String, Value>,
}

impl Default for Notebook {
    fn default() -> Self {
        Self::empty()
    }
}

impl Notebook {
    /// A new notebook with one empty code cell and a Python kernel.
    pub fn empty() -> Self {
        let mut extra = Map::new();
        extra.insert(
            "metadata".into(),
            json!({
                "kernelspec": { "display_name": "Python 3", "language": "python", "name": "python3" },
                "language_info": { "name": "python" }
            }),
        );
        extra.insert("nbformat".into(), json!(4));
        extra.insert("nbformat_minor".into(), json!(5));
        Self { cells: vec![Cell::new(CellKind::Code, "")], extra }
    }

    /// Parse `.ipynb` text. Empty text is a new, empty notebook.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.trim().is_empty() {
            return Ok(Self::empty());
        }
        let value: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
        let Value::Object(mut top) = value else {
            return Err("a notebook is a JSON object".into());
        };
        let major = top.get("nbformat").and_then(Value::as_i64).unwrap_or(4);
        if major < 4 {
            return Err(format!(
                "nbformat {major} notebooks aren't supported; convert with `jupyter nbconvert --to notebook`"
            ));
        }
        let cells = match top.remove("cells") {
            Some(Value::Array(cells)) => cells,
            Some(_) => return Err("`cells` must be a list".into()),
            None => Vec::new(),
        };
        let cells = cells
            .into_iter()
            .filter_map(|c| match c {
                Value::Object(mut obj) => {
                    let kind = match obj.remove("cell_type").as_ref().and_then(Value::as_str) {
                        Some("markdown") => CellKind::Markdown,
                        Some("raw") => CellKind::Raw,
                        _ => CellKind::Code,
                    };
                    let source = multiline(obj.remove("source").as_ref());
                    let outputs = match obj.remove("outputs") {
                        Some(Value::Array(o)) => o,
                        _ => Vec::new(),
                    };
                    let execution_count = obj.remove("execution_count").and_then(|v| v.as_i64());
                    Some(Cell { kind, source, outputs, execution_count, extra: obj })
                }
                _ => None,
            })
            .collect();
        Ok(Self { cells, extra: top })
    }

    /// Serialize the way Jupyter does (sorted keys, one-space indent,
    /// trailing newline).
    pub fn to_json(&self) -> String {
        let mut top = self.extra.clone();
        top.insert("cells".into(), Value::Array(self.cells.iter().map(Cell::to_value).collect()));
        top.entry("metadata").or_insert_with(|| json!({}));
        top.entry("nbformat").or_insert(json!(4));
        top.entry("nbformat_minor").or_insert(json!(5));
        let mut buf = Vec::new();
        let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
        serde::Serialize::serialize(&Value::Object(top), &mut ser).expect("JSON values always serialize");
        let mut s = String::from_utf8(buf).expect("serde_json writes UTF-8");
        s.push('\n');
        s
    }

    /// The kernel the notebook asks for (`metadata.kernelspec.name`).
    pub fn kernel_name(&self) -> Option<&str> {
        self.extra.get("metadata")?.pointer("/kernelspec/name")?.as_str()
    }

    pub fn set_kernelspec(&mut self, name: &str, display_name: &str, language: &str) {
        let meta = self.extra.entry("metadata").or_insert_with(|| json!({}));
        if let Value::Object(meta) = meta {
            meta.insert(
                "kernelspec".into(),
                json!({ "name": name, "display_name": display_name, "language": language }),
            );
        }
    }
}

/// What to show for one output.
#[derive(Debug, Clone, PartialEq)]
pub enum OutputPart {
    /// `stdout` / `stderr` text.
    Stream { stderr: bool, text: String },
    /// `text/plain` (or `text/markdown`, `text/html` shown as source).
    Text(String),
    /// A base64-encoded PNG or JPEG.
    Image { mime: String, base64: String },
    Error { ename: String, evalue: String, traceback: String },
}

/// The richest part of an output this UI can show.
pub fn output_parts(output: &Value) -> Vec<OutputPart> {
    let otype = output.get("output_type").and_then(Value::as_str).unwrap_or("");
    match otype {
        "stream" => vec![OutputPart::Stream {
            stderr: output.get("name").and_then(Value::as_str) == Some("stderr"),
            text: strip_ansi(&multiline(output.get("text"))),
        }],
        "error" => {
            let traceback = output
                .get("traceback")
                .and_then(Value::as_array)
                .map(|lines| {
                    lines.iter().filter_map(Value::as_str).map(strip_ansi).collect::<Vec<_>>().join("\n")
                })
                .unwrap_or_default();
            vec![OutputPart::Error {
                ename: output.get("ename").and_then(Value::as_str).unwrap_or("Error").into(),
                evalue: output.get("evalue").and_then(Value::as_str).unwrap_or("").into(),
                traceback,
            }]
        }
        "execute_result" | "display_data" => {
            let Some(data) = output.get("data").and_then(Value::as_object) else { return Vec::new() };
            for mime in ["image/png", "image/jpeg"] {
                if let Some(b64) = data.get(mime) {
                    let base64: String = multiline(Some(b64)).chars().filter(|c| !c.is_whitespace()).collect();
                    return vec![OutputPart::Image { mime: mime.into(), base64 }];
                }
            }
            for mime in ["text/plain", "text/markdown", "text/html"] {
                if let Some(t) = data.get(mime) {
                    return vec![OutputPart::Text(strip_ansi(&multiline(Some(t))))];
                }
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// Drop ANSI colour codes (IPython colours its tracebacks).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NB: &str = r##"{
 "cells": [
  {"cell_type": "markdown", "id": "a1", "metadata": {"tags": ["intro"]}, "source": ["# Title\n", "Text"]},
  {"cell_type": "code", "execution_count": 3, "id": "b2", "metadata": {}, "outputs": [
    {"output_type": "stream", "name": "stdout", "text": ["hi\n"]},
    {"output_type": "execute_result", "execution_count": 3, "metadata": {}, "data": {"text/plain": ["42"]}}
  ], "source": "print('hi')\n42"}
 ],
 "metadata": {"kernelspec": {"name": "python3", "display_name": "Python 3", "language": "python"}, "custom": 1},
 "nbformat": 4,
 "nbformat_minor": 5
}"##;

    #[test]
    fn parses_cells_and_keeps_unknown_fields() {
        let nb = Notebook::parse(NB).unwrap();
        assert_eq!(nb.cells.len(), 2);
        assert_eq!(nb.cells[0].kind, CellKind::Markdown);
        assert_eq!(nb.cells[0].source, "# Title\nText");
        assert_eq!(nb.cells[1].execution_count, Some(3));
        assert_eq!(nb.cells[1].id(), Some("b2"));
        assert_eq!(nb.kernel_name(), Some("python3"));

        let saved = nb.to_json();
        let again = Notebook::parse(&saved).unwrap();
        assert_eq!(again.cells.len(), 2);
        assert_eq!(again.cells[1].source, nb.cells[1].source);
        assert_eq!(again.to_json(), saved, "saving is stable");
        assert!(saved.contains("\"custom\": 1"));
        assert!(saved.contains("\"tags\""));
        // Jupyter's layout: one-space indent, source as a list of lines.
        assert!(saved.contains("\n \"cells\": ["));
        assert!(saved.contains("\"print('hi')\\n\","));
    }

    #[test]
    fn empty_text_is_a_new_notebook_and_old_formats_are_refused() {
        let nb = Notebook::parse("").unwrap();
        assert_eq!(nb.cells.len(), 1);
        assert_eq!(nb.cells[0].kind, CellKind::Code);
        assert!(Notebook::parse("{\"nbformat\": 3}").is_err());
        assert!(Notebook::parse("[]").is_err());
    }

    #[test]
    fn stream_outputs_merge_and_parts_render() {
        let mut cell = Cell::new(CellKind::Code, "x");
        cell.push_output(json!({"output_type": "stream", "name": "stdout", "text": "a\n"}));
        cell.push_output(json!({"output_type": "stream", "name": "stdout", "text": "b\n"}));
        cell.push_output(json!({"output_type": "stream", "name": "stderr", "text": "warn\n"}));
        assert_eq!(cell.outputs.len(), 2);
        assert_eq!(output_parts(&cell.outputs[0]), vec![OutputPart::Stream { stderr: false, text: "a\nb\n".into() }]);

        let err = json!({"output_type": "error", "ename": "ZeroDivisionError", "evalue": "division by zero",
                         "traceback": ["\u{1b}[0;31mZeroDivisionError\u{1b}[0m", "line 1"]});
        assert_eq!(
            output_parts(&err),
            vec![OutputPart::Error {
                ename: "ZeroDivisionError".into(),
                evalue: "division by zero".into(),
                traceback: "ZeroDivisionError\nline 1".into()
            }]
        );
        let img = json!({"output_type": "display_data", "data": {"image/png": "iVBO\nRw==", "text/plain": "<Figure>"}});
        assert_eq!(output_parts(&img), vec![OutputPart::Image { mime: "image/png".into(), base64: "iVBORw==".into() }]);
    }
}
