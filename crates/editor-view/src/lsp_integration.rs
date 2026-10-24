//! Lazily-started `dart language-server` per Dart buffer, wired to
//! diagnostics only (M2 first slice — see the module docs at the top of
//! `lib.rs` for what's deliberately not here yet: completion, hover,
//! go-to-definition, rename, format, code actions).
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

use std::collections::HashMap;
use std::str::FromStr;

use core_lsp::{LspClient, ServerMessage, lsp_types};
use gpui_kit::Context;
use lsp_types::{
    ClientCapabilities, Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams,
    DidOpenTextDocumentParams, InitializeParams, PublishDiagnosticsParams,
    TextDocumentContentChangeEvent, TextDocumentItem, Uri, VersionedTextDocumentIdentifier,
};

use crate::EditorView;

pub(crate) struct LspState {
    client: LspClient,
    uri: Uri,
    version: i32,
    initialize_id: i64,
    ready: bool,
    diagnostics: Vec<Diagnostic>,
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
        });
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
