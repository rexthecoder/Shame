//! What a language server adds on top of tree-sitter: semantic tokens
//! (colours that know a class from a variable), real document symbols (the
//! outline), and folding ranges. Pure decoding and bookkeeping live here, so
//! the request plumbing in `lsp_integration.rs` stays small and this part can
//! be unit tested without a server.

use core_lsp::lsp_types::{self, SymbolKind};
use core_syntax::{FoldRange, Highlight, HighlightKind};
use std::ops::Range;

/// The token vocabulary a server declared in `semanticTokensProvider.legend`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Legend {
    pub types: Vec<String>,
    pub modifiers: Vec<String>,
}

impl Legend {
    /// The legend from an `initialize` result's `capabilities`, or `None` when
    /// the server has no full-document semantic tokens.
    pub fn from_capabilities(capabilities: &serde_json::Value) -> Option<Self> {
        let provider = capabilities.get("semanticTokensProvider")?;
        // `full` may be `true`, `{ "delta": .. }`, or absent (range only).
        let full = provider.get("full")?;
        if full.is_null() || full == &serde_json::Value::Bool(false) {
            return None;
        }
        let names = |key: &str| -> Vec<String> {
            provider
                .get("legend")
                .and_then(|l| l.get(key))
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
                .unwrap_or_default()
        };
        let legend = Legend { types: names("tokenTypes"), modifiers: names("tokenModifiers") };
        (!legend.types.is_empty()).then_some(legend)
    }
}

/// Map a token to the editor's own palette. Only tokens that say something
/// tree-sitter cannot (what an identifier *is*) are mapped; keywords, strings
/// and comments stay with the syntax highlighter, which is faster and never
/// stale.
pub(crate) fn kind_for_token(type_name: &str, modifiers: &[&str]) -> Option<HighlightKind> {
    let has = |m: &str| modifiers.contains(&m);
    Some(match type_name {
        "class" | "interface" | "struct" | "typeParameter" | "type" | "enum" | "namespace" | "decorator" => {
            if has("defaultLibrary") && matches!(type_name, "class" | "type" | "struct") {
                HighlightKind::BuiltinType
            } else {
                HighlightKind::Type
            }
        }
        "enumMember" | "event" => HighlightKind::Constant,
        "function" | "method" | "macro" => HighlightKind::Function,
        "property" => HighlightKind::Property,
        "variable" | "parameter" => {
            if has("readonly") && has("static") {
                HighlightKind::Constant
            } else {
                HighlightKind::Variable
            }
        }
        _ => return None,
    })
}

/// Decode the relative `data` array of a `semanticTokens/full` reply into
/// highlights. `offset_of(line, utf16_col)` turns a position into a byte
/// offset. Tokens are in document order, so the result is sorted and the
/// ranges do not overlap.
pub(crate) fn decode_tokens(
    data: &[u32],
    legend: &Legend,
    offset_of: impl Fn(u32, u32) -> usize,
) -> Vec<Highlight> {
    let mut out = Vec::with_capacity(data.len() / 5);
    let (mut line, mut col) = (0u32, 0u32);
    for token in data.chunks_exact(5) {
        let (delta_line, delta_start, length, type_ix, modifier_bits) =
            (token[0], token[1], token[2], token[3] as usize, token[4]);
        line += delta_line;
        col = if delta_line == 0 { col + delta_start } else { delta_start };
        let Some(type_name) = legend.types.get(type_ix) else { continue };
        let modifiers: Vec<&str> = legend
            .modifiers
            .iter()
            .enumerate()
            .filter(|(bit, _)| bit < &32 && modifier_bits & (1 << bit) != 0)
            .map(|(_, name)| name.as_str())
            .collect();
        let Some(kind) = kind_for_token(type_name, &modifiers) else { continue };
        let start = offset_of(line, col);
        let end = offset_of(line, col + length);
        if end > start {
            out.push(Highlight { range: start..end, kind });
        }
    }
    out
}

/// Keep highlights anchored to their text while a newer reply is on its way:
/// an edit replacing `start..old_end` with text ending at `new_end` drops the
/// tokens it touches and moves the ones after it.
pub(crate) fn shift_highlights(
    highlights: &mut Vec<Highlight>,
    start: usize,
    old_end: usize,
    new_end: usize,
) {
    highlights.retain_mut(|h| {
        if h.range.end <= start {
            true
        } else if h.range.start >= old_end {
            h.range = shifted(h.range.start, new_end, old_end)..shifted(h.range.end, new_end, old_end);
            true
        } else {
            false
        }
    });
}

fn shifted(pos: usize, new_end: usize, old_end: usize) -> usize {
    if new_end >= old_end { pos + (new_end - old_end) } else { pos - (old_end - new_end) }
}

/// The highlights of a sorted, non-overlapping list that touch `range`.
pub(crate) fn highlights_in_range(highlights: &[Highlight], range: Range<usize>) -> &[Highlight] {
    let first = highlights.partition_point(|h| h.range.end <= range.start);
    let last = first + highlights[first..].partition_point(|h| h.range.start < range.end);
    &highlights[first..last]
}

/// Badge shown next to a symbol in the outline.
pub(crate) fn symbol_label(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::CLASS => "class",
        SymbolKind::INTERFACE => "iface",
        SymbolKind::ENUM => "enum",
        SymbolKind::STRUCT => "struct",
        SymbolKind::METHOD | SymbolKind::CONSTRUCTOR => "method",
        SymbolKind::FUNCTION => "fn",
        SymbolKind::MODULE | SymbolKind::NAMESPACE | SymbolKind::PACKAGE => "mod",
        SymbolKind::FIELD | SymbolKind::PROPERTY => "field",
        SymbolKind::CONSTANT | SymbolKind::ENUM_MEMBER => "const",
        SymbolKind::VARIABLE => "var",
        SymbolKind::TYPE_PARAMETER => "type",
        _ => "sym",
    }
}

/// One outline entry: name (with its container, `Class.method`), badge, and
/// the 0-indexed line of the symbol's name.
pub(crate) type SymbolEntry = (String, &'static str, u32);

/// A symbol's extent, for finding what the caret is inside of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SymbolSpan {
    pub name: String,
    pub start_line: u32,
    pub end_line: u32,
}

/// The server's outline: flat entries for the symbol picker, and spans for
/// the breadcrumb.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Outline {
    pub entries: Vec<SymbolEntry>,
    pub spans: Vec<SymbolSpan>,
}

impl Outline {
    /// Names of the symbols containing `line`, outermost first.
    pub fn path_at(&self, line: u32) -> Vec<String> {
        let mut inside: Vec<&SymbolSpan> = self
            .spans
            .iter()
            .filter(|s| s.start_line <= line && line <= s.end_line)
            .collect();
        inside.sort_by_key(|s| (s.start_line, std::cmp::Reverse(s.end_line)));
        inside.into_iter().map(|s| s.name.clone()).collect()
    }
}

/// Flatten a `textDocument/documentSymbol` reply, hierarchical
/// (`DocumentSymbol[]`) or flat (`SymbolInformation[]`), into outline entries
/// in document order. Locals (variables inside functions) are skipped.
pub(crate) fn symbols_from_response(value: serde_json::Value) -> Option<Outline> {
    let items = value.as_array()?;
    let mut out = Vec::new();
    let mut spans = Vec::new();
    if items.first().is_some_and(|i| i.get("selectionRange").is_some()) {
        let symbols: Vec<lsp_types::DocumentSymbol> = serde_json::from_value(value).ok()?;
        fn walk(
            symbols: &[lsp_types::DocumentSymbol],
            container: Option<&str>,
            out: &mut Vec<SymbolEntry>,
            spans: &mut Vec<SymbolSpan>,
        ) {
            for s in symbols {
                let in_function = matches!(container, Some(c) if c.ends_with('\u{0}'));
                let local = s.kind == SymbolKind::VARIABLE && in_function;
                if !local {
                    let name = match container {
                        Some(c) => format!("{}.{}", c.trim_end_matches('\u{0}'), s.name),
                        None => s.name.clone(),
                    };
                    out.push((name, symbol_label(s.kind), s.selection_range.start.line));
                    spans.push(SymbolSpan {
                        name: s.name.clone(),
                        start_line: s.range.start.line,
                        end_line: s.range.end.line,
                    });
                }
                if let Some(children) = &s.children {
                    // Children of functions are locals: mark the container so
                    // they are skipped, but keep nested functions and types.
                    let tag = if matches!(s.kind, SymbolKind::FUNCTION | SymbolKind::METHOD | SymbolKind::CONSTRUCTOR)
                    {
                        format!("{}\u{0}", s.name)
                    } else {
                        s.name.clone()
                    };
                    walk(children, Some(&tag), out, spans);
                }
            }
        }
        walk(&symbols, None, &mut out, &mut spans);
    } else {
        let symbols: Vec<lsp_types::SymbolInformation> = serde_json::from_value(value).ok()?;
        for s in symbols {
            let name = match s.container_name.as_ref().filter(|c| !c.is_empty()) {
                Some(c) => format!("{c}.{}", s.name),
                None => s.name.clone(),
            };
            out.push((name, symbol_label(s.kind), s.location.range.start.line));
            spans.push(SymbolSpan {
                name: s.name,
                start_line: s.location.range.start.line,
                end_line: s.location.range.end.line,
            });
        }
    }
    out.sort_by_key(|(_, _, line)| *line);
    Some(Outline { entries: out, spans })
}

/// Fold ranges from a `textDocument/foldingRange` reply. `line_to_byte` maps
/// a 0-indexed line to its first byte; ranges spanning a single line are
/// dropped, and the result is sorted by start line with one range per start.
pub(crate) fn folds_from_response(
    value: serde_json::Value,
    line_count: usize,
    line_to_byte: impl Fn(usize) -> usize,
    len_bytes: usize,
) -> Option<Vec<FoldRange>> {
    let ranges: Vec<lsp_types::FoldingRange> = serde_json::from_value(value).ok()?;
    let mut folds: Vec<FoldRange> = ranges
        .into_iter()
        .filter(|r| r.end_line > r.start_line && (r.end_line as usize) < line_count)
        .map(|r| {
            let (start_line, end_line) = (r.start_line as usize, r.end_line as usize);
            let end_byte = if end_line + 1 < line_count { line_to_byte(end_line + 1) } else { len_bytes };
            FoldRange { start_line, end_line, start_byte: line_to_byte(start_line), end_byte }
        })
        .collect();
    folds.sort_by_key(|f| (f.start_line, std::cmp::Reverse(f.end_line)));
    folds.dedup_by_key(|f| f.start_line);
    Some(folds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn legend() -> Legend {
        Legend {
            types: vec!["class".into(), "variable".into(), "keyword".into(), "function".into()],
            modifiers: vec!["declaration".into(), "readonly".into(), "static".into(), "defaultLibrary".into()],
        }
    }

    // One byte per column, 10 columns per line: offset = line * 10 + col.
    fn offset(line: u32, col: u32) -> usize {
        (line * 10 + col) as usize
    }

    #[test]
    fn tokens_decode_relative_positions_and_skip_unmapped_types() {
        // class at 0:2 len 3; keyword at 0:6 len 2 (skipped); function at 1:1 len 4.
        let data = [0, 2, 3, 0, 0, 0, 4, 2, 2, 0, 1, 1, 4, 3, 0];
        let got = decode_tokens(&data, &legend(), offset);
        assert_eq!(
            got,
            vec![
                Highlight { range: 2..5, kind: HighlightKind::Type },
                Highlight { range: 11..15, kind: HighlightKind::Function },
            ]
        );
    }

    #[test]
    fn modifiers_refine_the_kind() {
        let l = legend();
        // variable + readonly + static is a constant.
        let data = [0, 0, 3, 1, 0b110];
        assert_eq!(decode_tokens(&data, &l, offset)[0].kind, HighlightKind::Constant);
        // a default-library class is a built-in type.
        let data = [0, 0, 3, 0, 0b1000];
        assert_eq!(decode_tokens(&data, &l, offset)[0].kind, HighlightKind::BuiltinType);
    }

    #[test]
    fn a_legend_needs_full_document_support() {
        let with_full = json!({ "semanticTokensProvider": {
            "legend": { "tokenTypes": ["class"], "tokenModifiers": [] }, "full": true } });
        assert!(Legend::from_capabilities(&with_full).is_some());
        let range_only = json!({ "semanticTokensProvider": {
            "legend": { "tokenTypes": ["class"], "tokenModifiers": [] }, "range": true } });
        assert!(Legend::from_capabilities(&range_only).is_none());
        assert!(Legend::from_capabilities(&json!({})).is_none());
    }

    #[test]
    fn edits_drop_touched_tokens_and_move_later_ones() {
        let mut hs = vec![
            Highlight { range: 0..3, kind: HighlightKind::Type },
            Highlight { range: 5..8, kind: HighlightKind::Function },
            Highlight { range: 12..15, kind: HighlightKind::Variable },
        ];
        // Replace 4..9 (touches the middle token) with 2 bytes.
        shift_highlights(&mut hs, 4, 9, 6);
        assert_eq!(
            hs,
            vec![
                Highlight { range: 0..3, kind: HighlightKind::Type },
                Highlight { range: 9..12, kind: HighlightKind::Variable },
            ]
        );
    }

    #[test]
    fn range_lookup_returns_only_touching_tokens() {
        let hs = vec![
            Highlight { range: 0..3, kind: HighlightKind::Type },
            Highlight { range: 5..8, kind: HighlightKind::Function },
            Highlight { range: 12..15, kind: HighlightKind::Variable },
        ];
        assert_eq!(highlights_in_range(&hs, 4..9).len(), 1);
        assert_eq!(highlights_in_range(&hs, 3..5).len(), 0);
        assert_eq!(highlights_in_range(&hs, 0..100).len(), 3);
    }

    #[test]
    fn hierarchical_symbols_flatten_with_containers_and_drop_locals() {
        let reply = json!([{
            "name": "Widget", "kind": 5,
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 9, "character": 1}},
            "selectionRange": {"start": {"line": 0, "character": 6}, "end": {"line": 0, "character": 12}},
            "children": [{
                "name": "build", "kind": 6,
                "range": {"start": {"line": 2, "character": 2}, "end": {"line": 8, "character": 3}},
                "selectionRange": {"start": {"line": 2, "character": 2}, "end": {"line": 2, "character": 7}},
                "children": [{
                    "name": "tmp", "kind": 13,
                    "range": {"start": {"line": 3, "character": 4}, "end": {"line": 3, "character": 12}},
                    "selectionRange": {"start": {"line": 3, "character": 4}, "end": {"line": 3, "character": 7}}
                }]
            }]
        }]);
        let got = symbols_from_response(reply).unwrap();
        assert_eq!(
            got.entries,
            vec![("Widget".to_string(), "class", 0), ("Widget.build".to_string(), "method", 2)]
        );
        // The caret in `build`'s body is inside both; outside it, only the class.
        assert_eq!(got.path_at(5), vec!["Widget".to_string(), "build".to_string()]);
        assert_eq!(got.path_at(9), vec!["Widget".to_string()]);
        assert!(got.path_at(20).is_empty());
    }

    #[test]
    fn flat_symbol_information_is_supported() {
        let reply = json!([{
            "name": "main", "kind": 12, "containerName": "app",
            "location": { "uri": "file:///a.go",
                "range": {"start": {"line": 4, "character": 0}, "end": {"line": 6, "character": 1}} }
        }]);
        assert_eq!(
            symbols_from_response(reply).unwrap().entries,
            vec![("app.main".to_string(), "fn", 4)]
        );
    }

    #[test]
    fn folding_ranges_become_sorted_unique_folds() {
        let reply = json!([
            {"startLine": 5, "endLine": 9},
            {"startLine": 1, "endLine": 3},
            {"startLine": 5, "endLine": 7},
            {"startLine": 2, "endLine": 2},
            {"startLine": 8, "endLine": 99}
        ]);
        let folds = folds_from_response(reply, 12, |l| l * 10, 120).unwrap();
        let spans: Vec<_> = folds.iter().map(|f| (f.start_line, f.end_line)).collect();
        assert_eq!(spans, vec![(1, 3), (5, 9)]);
        assert_eq!(folds[0].end_byte, 40);
    }
}
