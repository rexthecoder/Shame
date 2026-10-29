//! Lazily-started `dart language-server` per Dart buffer, wired to
//! diagnostics, hover, go-to-definition, formatting, rename, and code
//! actions — every M2 item from the project plan except completion,
//! which stays deferred to the UI pass (see the module docs at the top
//! of `lib.rs`).
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
    ApplyWorkspaceEditParams, ClientCapabilities, CodeAction, CodeActionContext,
    CodeActionOrCommand, CodeActionParams, Command, CompletionItem, CompletionItemKind,
    CompletionParams, CompletionResponse, CompletionTextEdit, Diagnostic, DiagnosticSeverity,
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, DocumentFormattingParams,
    ExecuteCommandParams, FormattingOptions, GotoDefinitionParams, GotoDefinitionResponse, Hover,
    HoverContents, HoverParams, InitializeParams, LanguageString, Location, MarkedString,
    Position, PublishDiagnosticsParams, Range, RenameParams, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, TextEdit, Uri,
    VersionedTextDocumentIdentifier, WorkspaceEdit,
};

use crate::{EditorEvent, EditorView};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionKindBadge {
    Function,
    Property,
    Type,
    Variable,
    Keyword,
    Constant,
}

#[derive(Clone, Debug)]
pub(crate) struct CompletionEntry {
    pub(crate) label: String,
    pub(crate) insert_text: String,
    pub(crate) detail: Option<String>,
    pub(crate) kind: CompletionKindBadge,
}

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
    pending_rename_id: Option<i64>,
    pending_code_action_id: Option<i64>,
    pending_completion_id: Option<i64>,
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
            pending_rename_id: None,
            pending_code_action_id: None,
            pending_completion_id: None,
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

    /// Ask `app` to collect a new name for the symbol at the cursor (see
    /// the module docs at the top of `lib.rs` for why the prompt itself
    /// isn't here). No-op if there's no ready LSP server for this buffer.
    pub(crate) fn start_rename(&mut self, cx: &mut Context<Self>) {
        let is_ready = self.lsp.as_ref().is_some_and(|lsp| lsp.ready);
        if !is_ready {
            return;
        }
        let head = self.buffer.selections().primary().head;
        let line = self.buffer.byte_to_line(head) as u32;
        let column = (head - self.buffer.line_to_byte(line as usize)) as u32;
        cx.emit(EditorEvent::RequestRename { line, column });
    }

    /// Send `textDocument/rename` for `(line, column)` with `new_name`.
    /// Called by `app` once it's collected the new name from the user.
    pub fn send_rename(&mut self, line: u32, column: u32, new_name: String) {
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        let params = RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: lsp.uri.clone() },
                position: Position { line, character: column },
            },
            new_name,
            work_done_progress_params: Default::default(),
        };
        if let Ok(id) = lsp.client.request("textDocument/rename", serde_json::to_value(params).unwrap_or_default()) {
            lsp.pending_rename_id = Some(id);
        }
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

    /// Send `textDocument/codeAction` for the cursor's current line,
    /// including whatever diagnostics on that line the server already
    /// told us about (so quick-fixes for a visible error/warning show
    /// up). No-op if there's no ready LSP server for this buffer.
    pub fn request_code_actions(&mut self) {
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        if !lsp.ready {
            return;
        }
        let head = self.buffer.selections().primary().head;
        let line = self.buffer.byte_to_line(head) as u32;
        let column = (head - self.buffer.line_to_byte(line as usize)) as u32;
        let position = Position { line, character: column };

        let diagnostics: Vec<Diagnostic> =
            lsp.diagnostics.iter().filter(|d| d.range.start.line == line).cloned().collect();

        let params = CodeActionParams {
            text_document: TextDocumentIdentifier { uri: lsp.uri.clone() },
            range: Range { start: position, end: position },
            context: CodeActionContext { diagnostics, only: None, trigger_kind: None },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        if let Ok(id) =
            lsp.client.request("textDocument/codeAction", serde_json::to_value(params).unwrap_or_default())
        {
            lsp.pending_code_action_id = Some(id);
        }
    }

    /// Send `textDocument/completion` for the current cursor position.
    pub(crate) fn request_lsp_completion(&mut self, line: usize, column: usize) {
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        if !lsp.ready {
            return;
        };
        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: lsp.uri.clone(),
                },
                position: Position {
                    line: line as u32,
                    character: column as u32,
                },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };
        if let Ok(id) = lsp.client.request(
            "textDocument/completion",
            serde_json::to_value(params).unwrap_or_default(),
        ) {
            lsp.pending_completion_id = Some(id);
        }
    }

    /// Apply a chosen code action: a direct edit if it has one, otherwise
    /// a server command (most Dart quick-fixes are commands — the
    /// server replies with its own `workspace/applyEdit` *request*,
    /// handled in `handle_lsp_message`, rather than returning an edit
    /// directly).
    pub fn apply_code_action(&mut self, action: CodeActionOrCommand, cx: &mut Context<Self>) {
        match action {
            CodeActionOrCommand::CodeAction(CodeAction { edit: Some(edit), .. }) => {
                self.apply_workspace_edit(edit, cx);
            }
            CodeActionOrCommand::CodeAction(CodeAction { command: Some(command), .. }) => {
                self.execute_command(command);
            }
            CodeActionOrCommand::CodeAction(_) => {}
            CodeActionOrCommand::Command(command) => self.execute_command(command),
        }
    }

    fn execute_command(&mut self, command: Command) {
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        let params = ExecuteCommandParams {
            command: command.command,
            arguments: command.arguments.unwrap_or_default(),
            work_done_progress_params: Default::default(),
        };
        let _ =
            lsp.client.request("workspace/executeCommand", serde_json::to_value(params).unwrap_or_default());
    }

    /// Apply a `WorkspaceEdit`'s changes to this buffer's own file only.
    /// An edit touching other files leaves them unchanged — see the
    /// module docs; the same gap rename already has.
    fn apply_workspace_edit(&mut self, edit: WorkspaceEdit, cx: &mut Context<Self>) {
        let Some(own_uri) = self.lsp.as_ref().map(|lsp| lsp.uri.clone()) else {
            return;
        };
        if let Some(changes) = edit.changes {
            for (uri, edits) in changes {
                if uri.as_str() == own_uri.as_str() {
                    self.apply_text_edits(edits, cx);
                }
            }
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
            ServerMessage::Response { id, result } if lsp.pending_rename_id == Some(id) => {
                lsp.pending_rename_id = None;
                if let Ok(Some(edit)) =
                    result.map(|v| serde_json::from_value::<Option<WorkspaceEdit>>(v).unwrap_or(None))
                {
                    self.apply_workspace_edit(edit, cx);
                }
            }
            ServerMessage::Response { id, result } if lsp.pending_code_action_id == Some(id) => {
                lsp.pending_code_action_id = None;
                let Ok(Some(actions)) = result
                    .map(|v| serde_json::from_value::<Option<Vec<CodeActionOrCommand>>>(v).unwrap_or(None))
                else {
                    return;
                };
                let mut actions = actions;
                match actions.len() {
                    0 => {}
                    1 => self.apply_code_action(actions.remove(0), cx),
                    _ => cx.emit(EditorEvent::ShowCodeActions { actions }),
                }
            }
            ServerMessage::Response { id, result } if lsp.pending_completion_id == Some(id) => {
                lsp.pending_completion_id = None;
                let Ok(value) = result else {
                    return;
                };
                let Ok(Some(resp)) = serde_json::from_value::<Option<CompletionResponse>>(value) else {
                    return;
                };
                let items = match resp {
                    CompletionResponse::Array(items) => items,
                    CompletionResponse::List(list) => list.items,
                };
                self.merge_lsp_completions(items, cx);
            }
            // A request *from* the server, not a reply to one of ours.
            // So far just workspace/applyEdit, sent after we ask it to
            // run a Dart quick-fix command — apply the edit to our own
            // file (see apply_workspace_edit's own gap note) and tell
            // the server we did, which it expects a reply to.
            ServerMessage::Request { id, method, params } if method == "workspace/applyEdit" => {
                if let Ok(apply_params) = serde_json::from_value::<ApplyWorkspaceEditParams>(params) {
                    self.apply_workspace_edit(apply_params.edit, cx);
                }
                if let Some(lsp) = &self.lsp {
                    let _ = lsp.client.respond(id, serde_json::json!({"applied": true}));
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

    /// The primary diagnostic message (from the most severe diagnostic) on each
    /// 0-indexed line, used for inline Error-Lens style annotations.
    pub(crate) fn diagnostic_messages_by_line(&self) -> HashMap<u32, (DiagnosticSeverity, String)> {
        let mut by_line: HashMap<u32, (DiagnosticSeverity, String)> = HashMap::new();
        let Some(lsp) = &self.lsp else {
            return by_line;
        };
        for diagnostic in &lsp.diagnostics {
            let severity = diagnostic.severity.unwrap_or(DiagnosticSeverity::INFORMATION);
            let line = diagnostic.range.start.line;
            let first_line = diagnostic
                .message
                .lines()
                .next()
                .unwrap_or(&diagnostic.message)
                .trim()
                .to_string();
            if first_line.is_empty() {
                continue;
            }
            match by_line.get(&line) {
                Some((existing_sev, _)) if *existing_sev <= severity => {}
                _ => {
                    by_line.insert(line, (severity, first_line));
                }
            }
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

pub(crate) fn completion_item_to_entry(item: CompletionItem) -> Option<CompletionEntry> {
    let label = item.label.trim().to_string();
    if label.is_empty() {
        return None;
    }
    let raw_insert = item
        .text_edit
        .and_then(|te| match te {
            CompletionTextEdit::Edit(e) => Some(e.new_text),
            CompletionTextEdit::InsertAndReplace(e) => Some(e.new_text),
        })
        .or(item.insert_text)
        .unwrap_or_else(|| label.clone());
    let insert_text = strip_lsp_snippet(&raw_insert);
    if insert_text.is_empty() {
        return None;
    }
    let kind = match item.kind {
        Some(
            CompletionItemKind::FUNCTION
            | CompletionItemKind::METHOD
            | CompletionItemKind::CONSTRUCTOR,
        ) => CompletionKindBadge::Function,
        Some(CompletionItemKind::FIELD | CompletionItemKind::PROPERTY) => {
            CompletionKindBadge::Property
        }
        Some(
            CompletionItemKind::CLASS
            | CompletionItemKind::INTERFACE
            | CompletionItemKind::STRUCT
            | CompletionItemKind::ENUM
            | CompletionItemKind::TYPE_PARAMETER,
        ) => CompletionKindBadge::Type,
        Some(CompletionItemKind::KEYWORD) => CompletionKindBadge::Keyword,
        Some(CompletionItemKind::CONSTANT | CompletionItemKind::ENUM_MEMBER) => {
            CompletionKindBadge::Constant
        }
        _ => CompletionKindBadge::Variable,
    };
    let detail = item
        .detail
        .map(|d| d.lines().next().unwrap_or(&d).trim().to_string())
        .filter(|d| !d.is_empty());
    Some(CompletionEntry {
        label,
        insert_text,
        detail,
        kind,
    })
}

/// Strip LSP snippet placeholders (`$0`, `$1`, `${1:default}`) so inserting a
/// completion never leaves raw `$0` syntax in the buffer.
pub(crate) fn strip_lsp_snippet(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && i + 1 < chars.len() {
            if chars[i + 1].is_ascii_digit() {
                i += 2;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                continue;
            } else if chars[i + 1] == '{' {
                i += 2;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                if i < chars.len() && chars[i] == ':' {
                    i += 1;
                    while i < chars.len() && chars[i] != '}' {
                        out.push(chars[i]);
                        i += 1;
                    }
                }
                if i < chars.len() && chars[i] == '}' {
                    i += 1;
                }
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
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

    /// Also real captured output — `textDocument/rename` on `greet` in a
    /// small sample file, renaming it to `sayHello`. Confirms
    /// `WorkspaceEdit.changes` (a map of URI string -> `TextEdit[]`, the
    /// shape this server actually uses, not `documentChanges`)
    /// deserializes the way the rename response handler expects, and
    /// that both occurrences (the declaration and the one call site)
    /// came back in the single same-file entry.
    #[test]
    fn deserializes_a_real_server_rename_response() {
        let json = serde_json::json!({
            "changes": {
                "file:///a/sample_rename.dart": [
                    {"newText":"sayHello","range":{"end":{"character":14,"line":1},"start":{"character":9,"line":1}}},
                    {"newText":"sayHello","range":{"end":{"character":15,"line":8},"start":{"character":10,"line":8}}}
                ]
            }
        });
        let edit: WorkspaceEdit = serde_json::from_value(json).expect("should deserialize");
        let changes = edit.changes.expect("expected a changes map");
        assert_eq!(changes.len(), 1);
        let (_uri, edits) = changes.into_iter().next().unwrap();
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].new_text, "sayHello");
        assert_eq!(edits[1].range.start.line, 8);
    }

    /// Real captured output — `textDocument/codeAction` on an unused
    /// `import 'dart:math';` line. Confirms the important, easy-to-get-
    /// wrong fact about this server: every single quick-fix came back
    /// as a `Command` (`CodeActionOrCommand::Command`), never a
    /// `CodeAction` with a direct `edit` — so `apply_code_action` has to
    /// handle the command path to do anything useful for Dart at all.
    #[test]
    fn deserializes_a_real_server_code_action_response_as_commands() {
        let json = serde_json::json!([
            {
                "title": "Remove unused import",
                "command": "dart.edit.codeAction.apply",
                "arguments": [{"kind": "quickfix.remove.unusedImport"}],
            },
            {
                "title": "Ignore 'unused_import' for this line",
                "command": "dart.edit.codeAction.apply",
                "arguments": [{"kind": "quickfix.ignore.line"}],
            },
        ]);
        let actions: Vec<CodeActionOrCommand> = serde_json::from_value(json).expect("should deserialize");
        assert_eq!(actions.len(), 2);
        for action in &actions {
            match action {
                CodeActionOrCommand::Command(command) => {
                    assert_eq!(command.command, "dart.edit.codeAction.apply");
                }
                CodeActionOrCommand::CodeAction(_) => panic!("expected a Command, not a CodeAction"),
            }
        }
        assert_eq!(
            match &actions[0] {
                CodeActionOrCommand::Command(c) => c.title.as_str(),
                _ => unreachable!(),
            },
            "Remove unused import"
        );
    }

    /// Real captured output — the `workspace/applyEdit` *request* (not a
    /// response) the server sends after `workspace/executeCommand` for
    /// `dart.edit.codeAction.apply`. Confirms `ApplyWorkspaceEditParams`
    /// deserializes correctly and that this really is a request (has an
    /// `id`, needs a reply via `LspClient::respond`), which is why
    /// `core_lsp::ServerMessage` needed its own `Request` variant.
    #[test]
    fn deserializes_a_real_server_apply_edit_request() {
        let message = serde_json::json!({
            "id": 1,
            "jsonrpc": "2.0",
            "method": "workspace/applyEdit",
            "params": {
                "edit": {
                    "changes": {
                        "file:///a/sample_ca.dart": [
                            {"newText":"","range":{"end":{"character":0,"line":1},"start":{"character":0,"line":0}}}
                        ]
                    }
                },
                "label": "Apply Code Action",
            },
        });
        assert_eq!(message["method"], "workspace/applyEdit");
        assert!(message.get("id").is_some(), "expected this to carry an id, like a request");
        assert!(message.get("result").is_none() && message.get("error").is_none());

        let params: ApplyWorkspaceEditParams =
            serde_json::from_value(message["params"].clone()).expect("should deserialize");
        let changes = params.edit.changes.expect("expected a changes map");
        assert_eq!(changes.len(), 1);
    }
}
