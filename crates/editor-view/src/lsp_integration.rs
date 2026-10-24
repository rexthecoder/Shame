//! Lazily-started `dart language-server` per Dart buffer, wired to
//! diagnostics, hover, go-to-definition, and formatting (M2 so far — see
//! the module docs at the top of `lib.rs` for what's deliberately not
//! here yet: completion, rename, code actions).
//!
//! Scope cuts, to revisit once more of M2 lands:
//! - One analysis server per *open Dart tab*, not one shared per
//!   project. Simple, but wasteful if several Dart files are open at
//!   once — each `dart language-server` process is not cheap.
//! - Diagnostics are shown per *line* only (a colored gutter number),
//!   not as an underline at the exact column — good enough to prove the
//!   pipe end-to-end works, not a real diagnostics UI yet.
//! - Full-document sync on every edit (`textDocument/didChange` with no
//!   `range`, i.e. "replace everything"). Always valid LSP regardless of
//!   what the server negotiated, and far simpler than incremental sync,
//!   at the cost of re-sending the whole buffer on every keystroke.
//! - Hover positions (and everything else here) use byte offsets as LSP
//!   "character" columns, same approximation `editor-view` already makes
//!   elsewhere — correct for ASCII, wrong for a line with non-ASCII text
//!   before the hovered column (LSP's default position encoding is
//!   UTF-16 code units, which this doesn't convert to).
//! - The hover popup renders at a fixed spot below the hovered line, not
//!   anchored to the exact hovered column.
//! - The popup is dismissed on any keystroke, but not when the mouse
//!   simply leaves the editor without typing — each line's hover
//!   tracking only fires while the mouse is over *that* line, so moving
//!   off the text area entirely (onto the gutter, another panel, or out
//!   of the window) leaves a stale popup until something else changes it.
//! - Formatting (cmd/ctrl+shift+F) is manually triggered only — there's
//!   no format-on-save. Wiring that up means waiting for the format
//!   response before the save actually happens, which `save()` doesn't
//!   do yet (it saves synchronously, immediately).

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use core_lsp::{LspClient, ServerMessage, lsp_types};
use gpui_kit::Context;
use smol::Timer;
use lsp_types::{
    ClientCapabilities, Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams,
    DidOpenTextDocumentParams, DocumentFormattingParams, FormattingOptions, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverContents, HoverParams, InitializeParams, LanguageString,
    Location, MarkedString, Position, PublishDiagnosticsParams, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, TextEdit, Uri,
    VersionedTextDocumentIdentifier,
};

use crate::{EditorEvent, EditorView};

pub(crate) struct LspState {
    client: LspClient,
    uri: Uri,
    version: i32,
    initialize_id: i64,
    ready: bool,
    diagnostics: Vec<Diagnostic>,
    pending_hover_id: Option<i64>,
    pending_definition_id: Option<i64>,
    pending_format_id: Option<i64>,
}

impl EditorView {
    /// Start `dart language-server` for this buffer if it's a `.dart`
    /// file and one isn't already running for it. Safe to call on every
    /// edit/open; it's a no-op once `self.lsp` is set.
    pub(crate) fn maybe_start_lsp(&mut self, cx: &mut Context<Self>) {
        if self.lsp.is_some() {
            return;
        }
        let Some(path) = self.buffer.path() else {
            return;
        };
        if path.extension().and_then(|e| e.to_str()) != Some("dart") {
            return;
        }
        let Ok(uri) = Uri::from_str(&format!("file://{}", path.display())) else {
            return;
        };
        let Ok(client) = LspClient::spawn("dart", &["language-server"]) else {
            return;
        };

        let init_params = InitializeParams {
            process_id: Some(std::process::id()),
            capabilities: ClientCapabilities::default(),
            ..Default::default()
        };
        let Ok(initialize_id) =
            client.request("initialize", serde_json::to_value(init_params).unwrap_or_default())
        else {
            return;
        };

        let events = client.events.clone();
        cx.spawn(async move |weak, cx| {
            loop {
                let Ok(message) = events.recv().await else {
                    break;
                };
                if weak.update(cx, |this, cx| this.handle_lsp_message(message, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        self.lsp = Some(LspState {
            client,
            uri,
            version: 1,
            initialize_id,
            ready: false,
            diagnostics: Vec::new(),
            pending_hover_id: None,
            pending_definition_id: None,
            pending_format_id: None,
        });
    }

    /// Note that the mouse is hovering over `(line, column)` in the
    /// text, debouncing a `textDocument/hover` request behind a short
    /// delay so moving the mouse across a line doesn't fire one request
    /// per pixel. No-op if there's no ready LSP server for this buffer.
    pub(crate) fn note_hover_position(&mut self, line: usize, column: usize, cx: &mut Context<Self>) {
        let is_ready = self.lsp.as_ref().is_some_and(|lsp| lsp.ready);
        if !is_ready {
            return;
        }
        if self.hover_point == Some((line, column)) {
            return;
        }
        self.hover_point = Some((line, column));
        let had_result = self.hover_result.take().is_some();
        self.hover_sequence += 1;
        let sequence = self.hover_sequence;
        if had_result {
            cx.notify();
        }

        cx.spawn(async move |weak, cx| {
            Timer::after(Duration::from_millis(300)).await;
            let _ = weak.update(cx, |this, cx| {
                if this.hover_sequence == sequence {
                    this.request_hover(line, column, cx);
                }
            });
        })
        .detach();
    }

    /// Clear any pending/shown hover (e.g. the mouse left the text area).
    pub(crate) fn clear_hover(&mut self, cx: &mut Context<Self>) {
        self.hover_point = None;
        self.hover_sequence += 1;
        if self.hover_result.take().is_some() {
            cx.notify();
        }
    }

    fn request_hover(&mut self, line: usize, column: usize, _cx: &mut Context<Self>) {
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        let params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: lsp.uri.clone() },
                position: Position {
                    line: line as u32,
                    character: column as u32,
                },
            },
            work_done_progress_params: Default::default(),
        };
        if let Ok(id) = lsp.client.request("textDocument/hover", serde_json::to_value(params).unwrap_or_default()) {
            lsp.pending_hover_id = Some(id);
        }
    }

    /// The hover text to show (if any) and the 0-indexed line it belongs
    /// to, for `render` to place it under.
    pub(crate) fn hover_to_render(&self) -> Option<(usize, &str)> {
        self.hover_result.as_ref().map(|(line, text)| (*line, text.as_str()))
    }

    /// Send `textDocument/definition` for `(line, column)`, if a server
    /// is running and ready. No-op otherwise (e.g. cmd+click on a buffer
    /// with no LSP, or before `initialize` finished).
    pub(crate) fn request_definition(&mut self, line: usize, column: usize) {
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        if !lsp.ready {
            return;
        }
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: lsp.uri.clone() },
                position: Position {
                    line: line as u32,
                    character: column as u32,
                },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        if let Ok(id) =
            lsp.client.request("textDocument/definition", serde_json::to_value(params).unwrap_or_default())
        {
            lsp.pending_definition_id = Some(id);
        }
    }

    /// Send `textDocument/formatting` for the whole buffer. No-op if
    /// there's no ready LSP server for this buffer. Returns whether a
    /// request was actually sent, so callers (e.g. "format then save")
    /// know whether to wait for a reply or just proceed.
    pub fn request_format(&mut self) -> bool {
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        if !lsp.ready {
            return false;
        }
        let params = DocumentFormattingParams {
            text_document: TextDocumentIdentifier { uri: lsp.uri.clone() },
            options: FormattingOptions {
                tab_size: 2,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        };
        match lsp.client.request("textDocument/formatting", serde_json::to_value(params).unwrap_or_default()) {
            Ok(id) => {
                lsp.pending_format_id = Some(id);
                true
            }
            Err(_) => false,
        }
    }

    /// Apply a server's `TextEdit`s to the buffer. Edits are applied from
    /// the end of the document backwards so that earlier edits' (line,
    /// character) positions stay valid as later ones are applied —
    /// standard multi-edit application, needed because each edit's
    /// position is defined relative to the *original* document, not to
    /// the result of previous edits in the same batch.
    fn apply_text_edits(&mut self, mut edits: Vec<TextEdit>, cx: &mut Context<Self>) {
        edits.sort_by_key(|edit| (edit.range.start.line, edit.range.start.character));
        for edit in edits.into_iter().rev() {
            let start = self.buffer.line_to_byte(edit.range.start.line as usize)
                + edit.range.start.character as usize;
            let end = self.buffer.line_to_byte(edit.range.end.line as usize)
                + edit.range.end.character as usize;
            self.buffer.edit(start..end, &edit.new_text);
        }
        cx.notify();
    }

    /// Tell the server about the buffer's current text, if a server is
    /// running and past its `initialize` handshake. Called whenever the
    /// buffer's text actually changed (same check `render` already does
    /// for incremental syntax reparsing).
    pub(crate) fn sync_lsp_text(&mut self, text: &str) {
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        if !lsp.ready {
            return;
        }
        lsp.version += 1;
        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: lsp.uri.clone(),
                version: lsp.version,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: text.to_string(),
            }],
        };
        let _ = lsp
            .client
            .notify("textDocument/didChange", serde_json::to_value(params).unwrap_or_default());
    }

    fn handle_lsp_message(&mut self, message: ServerMessage, cx: &mut Context<Self>) {
        let Some(lsp) = &mut self.lsp else {
            return;
        };

        match message {
            ServerMessage::Response { id, result } if id == lsp.initialize_id => {
                if result.is_ok() {
                    let _ = lsp.client.notify("initialized", serde_json::json!({}));
                    let params = DidOpenTextDocumentParams {
                        text_document: TextDocumentItem {
                            uri: lsp.uri.clone(),
                            language_id: "dart".to_string(),
                            version: lsp.version,
                            text: self.buffer.text(),
                        },
                    };
                    let _ = lsp.client.notify(
                        "textDocument/didOpen",
                        serde_json::to_value(params).unwrap_or_default(),
                    );
                    lsp.ready = true;
                }
            }
            ServerMessage::Notification { method, params } if method == "textDocument/publishDiagnostics" => {
                if let Ok(parsed) = serde_json::from_value::<PublishDiagnosticsParams>(params) {
                    lsp.diagnostics = parsed.diagnostics;
                    cx.notify();
                }
            }
            ServerMessage::Response { id, result } if lsp.pending_hover_id == Some(id) => {
                lsp.pending_hover_id = None;
                let Some(line) = self.hover_point.map(|(line, _)| line) else {
                    return;
                };
                if let Ok(Some(hover)) = result.map(|v| serde_json::from_value::<Option<Hover>>(v).unwrap_or(None)) {
                    self.hover_result = Some((line, hover_contents_text(&hover.contents)));
                    cx.notify();
                }
            }
            ServerMessage::Response { id, result } if lsp.pending_definition_id == Some(id) => {
                lsp.pending_definition_id = None;
                let own_uri = lsp.uri.clone();
                let Ok(value) = result else {
                    return;
                };
                let Ok(Some(response)) = serde_json::from_value::<Option<GotoDefinitionResponse>>(value) else {
                    return;
                };
                let Some((target_uri, line, column)) = first_definition_target(&response) else {
                    return;
                };

                if target_uri.as_str() == own_uri.as_str() {
                    let offset = self.buffer.line_to_byte(line as usize) + column as usize;
                    self.set_cursor(offset, cx);
                } else if let Some(path) = uri_to_path(&target_uri) {
                    cx.emit(EditorEvent::OpenAt { path, line, column });
                }
            }
            ServerMessage::Response { id, result } if lsp.pending_format_id == Some(id) => {
                lsp.pending_format_id = None;
                if let Ok(Some(edits)) = result.map(|v| serde_json::from_value::<Option<Vec<TextEdit>>>(v).unwrap_or(None)) {
                    self.apply_text_edits(edits, cx);
                }
            }
            _ => {}
        }
    }

    /// `(error_count, warning_count)` from the most recent diagnostics
    /// the server sent, or `(0, 0)` if there's no server for this buffer
    /// (not a `.dart` file, or it hasn't responded yet).
    pub fn diagnostic_summary(&self) -> (usize, usize) {
        let Some(lsp) = &self.lsp else {
            return (0, 0);
        };
        let errors = lsp.diagnostics.iter().filter(|d| d.severity == Some(DiagnosticSeverity::ERROR)).count();
        let warnings = lsp
            .diagnostics
            .iter()
            .filter(|d| d.severity == Some(DiagnosticSeverity::WARNING))
            .count();
        (errors, warnings)
    }

    /// The worst severity reported for each 0-indexed line that has at
    /// least one diagnostic, for coloring the gutter.
    pub(crate) fn diagnostics_by_line(&self) -> HashMap<u32, DiagnosticSeverity> {
        let mut by_line = HashMap::new();
        let Some(lsp) = &self.lsp else {
            return by_line;
        };
        for diagnostic in &lsp.diagnostics {
            let Some(severity) = diagnostic.severity else {
                continue;
            };
            let line = diagnostic.range.start.line;
            // ERROR < WARNING < INFORMATION < HINT, so the smaller value
            // is the more severe one.
            by_line
                .entry(line)
                .and_modify(|existing: &mut DiagnosticSeverity| *existing = (*existing).min(severity))
                .or_insert(severity);
        }
        by_line
    }
}

fn hover_contents_text(contents: &HoverContents) -> String {
    match contents {
        HoverContents::Scalar(marked) => marked_string_text(marked),
        HoverContents::Array(list) => {
            list.iter().map(marked_string_text).collect::<Vec<_>>().join("\n\n")
        }
        HoverContents::Markup(markup) => markup.value.clone(),
    }
}

fn marked_string_text(marked: &MarkedString) -> String {
    match marked {
        MarkedString::String(s) => s.clone(),
        MarkedString::LanguageString(LanguageString { value, .. }) => value.clone(),
    }
}

/// The first target `(uri, line, column)` out of a `textDocument/definition`
/// response, whichever of its three possible shapes the server used.
fn first_definition_target(response: &GotoDefinitionResponse) -> Option<(Uri, u32, u32)> {
    let location_target = |location: &Location| {
        (location.uri.clone(), location.range.start.line, location.range.start.character)
    };
    match response {
        GotoDefinitionResponse::Scalar(location) => Some(location_target(location)),
        GotoDefinitionResponse::Array(locations) => locations.first().map(location_target),
        GotoDefinitionResponse::Link(links) => links.first().map(|link| {
            (link.target_uri.clone(), link.target_selection_range.start.line, link.target_selection_range.start.character)
        }),
    }
}

/// Recover the filesystem path from one of our own `file://` URIs (the
/// only kind this app ever constructs or expects back from the server).
fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    uri.as_str().strip_prefix("file://").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not synthetic JSON — this is the exact `result` array the real
    /// `dart language-server` sent back for `textDocument/formatting` on
    /// a small badly-formatted sample file (captured by hand via a FIFO,
    /// same technique used elsewhere to verify this crate against the
    /// real server). Confirms `newText`/`range` camelCase deserializes
    /// into `TextEdit` the way `apply_text_edits` expects.
    #[test]
    fn deserializes_a_real_server_format_response() {
        let json = serde_json::json!([
            {"newText":" ","range":{"end":{"character":13,"line":0},"start":{"character":13,"line":0}}},
            {"newText":" ","range":{"end":{"character":27,"line":1},"start":{"character":27,"line":1}}},
            {"newText":"","range":{"end":{"character":6,"line":2},"start":{"character":4,"line":2}}}
        ]);
        let edits: Vec<TextEdit> = serde_json::from_value(json).expect("should deserialize");
        assert_eq!(edits.len(), 3);
        assert_eq!(edits[0].new_text, " ");
        assert_eq!(edits[0].range.start.character, 13);
        assert_eq!(edits[2].new_text, "");
        assert_eq!(edits[2].range, lsp_types::Range {
            start: Position { line: 2, character: 4 },
            end: Position { line: 2, character: 6 },
        });
    }
}
