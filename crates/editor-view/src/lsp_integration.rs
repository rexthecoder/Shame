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
//! - Positions sent to and read from the server are converted between
//!   buffer bytes and LSP's UTF-16 columns (`Buffer::lsp_position` /
//!   `Buffer::offset_from_lsp`) at this boundary; everything inside the
//!   editor stays in bytes.
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use core_lsp::{LspClient, ServerMessage, lsp_types};
use gpui_kit::Context;
use smol::Timer;
use lsp_types::{
    ApplyWorkspaceEditParams, ClientCapabilities, CodeAction, CodeActionClientCapabilities,
    CodeActionContext, CodeActionKind, CodeActionLiteralSupport, CodeActionOrCommand, CodeActionParams, Command,
    CompletionClientCapabilities, CompletionItem, CompletionItemCapability, CompletionItemKind,
    CompletionParams, CompletionResponse, CompletionTextEdit, Diagnostic, DiagnosticSeverity,
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, DocumentChangeOperation,
    DocumentChanges, DocumentFormattingParams, ExecuteCommandParams, FormattingOptions,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents, HoverParams,
    InitializeParams, InlayHint, InlayHintClientCapabilities, InlayHintKind, InlayHintLabel, InlayHintParams,
    LanguageString, Location, MarkedString, OneOf, ParameterLabel, Position,
    PublishDiagnosticsParams, Range, ReferenceContext, ReferenceParams, RenameParams,
    SignatureHelp, SignatureHelpParams, SignatureInformation, TextDocumentClientCapabilities,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, TextEdit, Uri, VersionedTextDocumentIdentifier,
    WorkspaceClientCapabilities, WorkspaceEdit, WorkspaceEditClientCapabilities, WorkspaceFolder,
};

use crate::code_actions;
use crate::inlay_hints::InlayHintView;
use crate::{EditorEvent, EditorView};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKindBadge {
    Function,
    Property,
    Type,
    Variable,
    Keyword,
    Constant,
}

#[derive(Clone, Debug)]
pub struct CompletionEntry {
    pub label: String,
    pub insert_text: String,
    pub detail: Option<String>,
    pub kind: CompletionKindBadge,
    pub additional_edits: Vec<TextEdit>,
    /// Tab stops (byte ranges within `insert_text`) from an LSP snippet.
    pub snippet_stops: Vec<std::ops::Range<usize>>,
    /// Other occurrences of each stop (same index), filled with the same text on Tab.
    pub snippet_mirrors: Vec<Vec<std::ops::Range<usize>>>,
}

struct SharedLspInner {
    ready: bool,
    pending_opens: Vec<DidOpenTextDocumentParams>,
    subscribers: Vec<async_channel::Sender<ServerMessage>>,
    latest_diagnostics: HashMap<String, Vec<Diagnostic>>,
}

/// A shared workspace LSP server multiplexed across all open `EditorView` tabs
/// that share the same `(language_id, project_root)` pair.
pub(crate) struct SharedLspServer {
    client: LspClient,
    inner: Mutex<SharedLspInner>,
}

impl SharedLspServer {
    fn request(&self, method: &str, params: serde_json::Value) -> std::io::Result<i64> {
        self.client.request(method, params)
    }

    fn notify(&self, method: &str, params: serde_json::Value) -> std::io::Result<()> {
        self.client.notify(method, params)
    }

    fn register_document(
        &self,
        open_params: DidOpenTextDocumentParams,
    ) -> (async_channel::Receiver<ServerMessage>, bool, Vec<Diagnostic>) {
        let (tx, rx) = async_channel::unbounded();
        let uri_str = open_params.text_document.uri.as_str().to_string();
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.subscribers.push(tx);
        let cached_diags = guard
            .latest_diagnostics
            .get(&uri_str)
            .cloned()
            .unwrap_or_default();
        if guard.ready {
            drop(guard);
            let _ = self.client.notify(
                "textDocument/didOpen",
                serde_json::to_value(open_params).unwrap_or_default(),
            );
            (rx, true, cached_diags)
        } else {
            guard.pending_opens.push(open_params);
            (rx, false, cached_diags)
        }
    }
}

type PoolKey = (&'static str, Option<PathBuf>);

fn server_pool() -> &'static Mutex<HashMap<PoolKey, Arc<SharedLspServer>>> {
    static POOL: OnceLock<Mutex<HashMap<PoolKey, Arc<SharedLspServer>>>> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether servers are told to compute inlay hints. Global because servers
/// are shared across editors; editors mirror `EditorOptions` into it.
static INLAY_HINTS_SETTING: AtomicBool = AtomicBool::new(false);

/// Record the inlay-hint setting and, when it changed, tell every running
/// server to re-read its configuration (it asks back via
/// `workspace/configuration`, answered in `shared_lsp_server`).
pub(crate) fn set_inlay_hints_setting(enabled: bool) {
    if INLAY_HINTS_SETTING.swap(enabled, Ordering::Relaxed) == enabled {
        return;
    }
    let servers: Vec<Arc<SharedLspServer>> =
        server_pool().lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
    for server in servers {
        let _ = server
            .client
            .notify("workspace/didChangeConfiguration", serde_json::json!({ "settings": null }));
    }
}

/// The reply to a `workspace/configuration` request: one entry per item
/// asked for. Only the `dart` section is ours to set (inlay hints); anything
/// else gets `null`, meaning "use your defaults".
fn configuration_response(params: &serde_json::Value) -> serde_json::Value {
    let items = params.get("items").and_then(|v| v.as_array()).map_or(&[][..], Vec::as_slice);
    let inlay = INLAY_HINTS_SETTING.load(Ordering::Relaxed);
    serde_json::Value::Array(
        items
            .iter()
            .map(|item| match item.get("section").and_then(|s| s.as_str()) {
                Some("dart") => serde_json::json!({ "inlayHints": inlay }),
                _ => serde_json::Value::Null,
            })
            .collect(),
    )
}

fn shared_lsp_server(
    language_id: &'static str,
    project_root: Option<PathBuf>,
    program: &str,
    args: &[String],
) -> Option<Arc<SharedLspServer>> {
    let pool = server_pool();
    let key = (language_id, project_root.clone());
    if let Some(existing) = pool
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned()
    {
        return Some(existing);
    }

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let client = LspClient::spawn(program, &arg_refs).ok()?;

    let root_uri = project_root
        .as_ref()
        .and_then(|root| Uri::from_str(&format!("file://{}", root.display())).ok());
    let workspace_folders = project_root.as_ref().and_then(|root| {
        let folder_uri = root_uri.clone()?;
        let name = root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("workspace")
            .to_string();
        Some(vec![WorkspaceFolder {
            uri: folder_uri,
            name,
        }])
    });

    #[allow(deprecated)]
    let init_params = InitializeParams {
        process_id: Some(std::process::id()),
        root_uri,
        workspace_folders,
        capabilities: ClientCapabilities {
            workspace: Some(WorkspaceClientCapabilities {
                apply_edit: Some(true),
                // Lets the server ask for settings (inlay hints) instead of
                // assuming its defaults.
                configuration: Some(true),
                workspace_edit: Some(WorkspaceEditClientCapabilities {
                    document_changes: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            text_document: Some(TextDocumentClientCapabilities {
                completion: Some(CompletionClientCapabilities {
                    completion_item: Some(CompletionItemCapability {
                        snippet_support: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                code_action: Some(CodeActionClientCapabilities {
                    code_action_literal_support: Some(CodeActionLiteralSupport::default()),
                    ..Default::default()
                }),
                inlay_hint: Some(InlayHintClientCapabilities::default()),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };

    let initialize_id = client
        .request(
            "initialize",
            serde_json::to_value(init_params).unwrap_or_default(),
        )
        .ok()?;
    let events = client.events.clone();

    let server = Arc::new(SharedLspServer {
        client,
        inner: Mutex::new(SharedLspInner {
            ready: false,
            pending_opens: Vec::new(),
            subscribers: Vec::new(),
            latest_diagnostics: HashMap::new(),
        }),
    });

    let server_bg = server.clone();
    std::thread::spawn(move || {
        while let Ok(message) = events.recv_blocking() {
            match &message {
                ServerMessage::Response { id, result } if *id == initialize_id => {
                    if result.is_ok() {
                        let _ = server_bg.client.notify("initialized", serde_json::json!({}));
                        let opens = {
                            let mut guard = server_bg
                                .inner
                                .lock()
                                .unwrap_or_else(|e| e.into_inner());
                            guard.ready = true;
                            std::mem::take(&mut guard.pending_opens)
                        };
                        for open in opens {
                            let _ = server_bg.client.notify(
                                "textDocument/didOpen",
                                serde_json::to_value(open).unwrap_or_default(),
                            );
                        }
                    }
                    let mut guard = server_bg.inner.lock().unwrap_or_else(|e| e.into_inner());
                    guard.subscribers.retain(|sub| {
                        sub.send_blocking(ServerMessage:: Notification {
                            method: "$ide/lspReady".to_string(),
                            params: serde_json::Value::Null,
                        })
                        .is_ok()
                    });
                }
                ServerMessage::Notification { method, params }
                    if method == "textDocument/publishDiagnostics" =>
                {
                    if let Ok(parsed) =
                        serde_json::from_value::<PublishDiagnosticsParams>(params.clone())
                    {
                        let mut guard =
                            server_bg.inner.lock().unwrap_or_else(|e| e.into_inner());
                        guard.latest_diagnostics.insert(
                            parsed.uri.as_str().to_string(),
                            parsed.diagnostics,
                        );
                        guard
                            .subscribers
                            .retain(|sub| sub.send_blocking(message.clone()).is_ok());
                    }
                }
                ServerMessage::Request { id, method, params } => {
                    if method == "workspace/configuration" {
                        let _ = server_bg.client.respond(*id, configuration_response(params));
                    } else if method == "workspace/applyEdit" {
                        let _ = server_bg
                            .client
                            .respond(*id, serde_json::json!({"applied": true}));
                        let mut guard =
                            server_bg.inner.lock().unwrap_or_else(|e| e.into_inner());
                        // Deliver workspace/applyEdit to the first active subscriber so it runs once.
                        let mut delivered = false;
                        guard.subscribers.retain(|sub| {
                            if !delivered {
                                if sub.send_blocking(message.clone()).is_ok() {
                                    delivered = true;
                                    true
                                } else {
                                    false
                                }
                            } else {
                                !sub.is_closed()
                            }
                        });
                    } else {
                        let _ = server_bg.client.respond(*id, serde_json::Value::Null);
                    }
                }
                _ => {
                    let mut guard = server_bg.inner.lock().unwrap_or_else(|e| e.into_inner());
                    guard
                        .subscribers
                        .retain(|sub| sub.send_blocking(message.clone()).is_ok());
                }
            }
        }
    });

    pool.lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, server.clone());
    Some(server)
}

pub(crate) struct LspState {
    client: Arc<SharedLspServer>,
    uri: Uri,
    version: i32,
    last_synced_revision: u64,
    ready: bool,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pending_hover_id: Option<i64>,
    pending_definition_id: Option<i64>,
    /// The next definition reply should open a peek, not jump.
    peek_definition: bool,
    pending_format_id: Option<i64>,
    pending_rename_id: Option<i64>,
    pending_code_action_id: Option<i64>,
    /// What the in-flight `textDocument/codeAction` was for.
    code_action_request: Option<CodeActionRequest>,
    pending_completion_id: Option<i64>,
    pending_references_id: Option<i64>,
    pending_signature_id: Option<i64>,
    pending_organize_id: Option<i64>,
    pending_inlay_id: Option<i64>,
    /// Buffer revision the in-flight inlay-hint request was sent for.
    inlay_request_revision: u64,
}

/// The cursor position a code-action request was sent for, and what to do
/// with the reply.
struct CodeActionRequest {
    revision: u64,
    head: usize,
    /// Open the menu when the reply arrives (`⌘.` / lightbulb click).
    open_menu: bool,
    /// Store the reply as the lightbulb's set for this position.
    cache: bool,
}

impl EditorView {
    /// Attach this buffer to the shared workspace LSP server for its `LanguageAdapter`
    /// (spawning the server only once per `(language_id, project_root)` pair).
    pub(crate) fn maybe_start_lsp(&mut self, cx: &mut Context<Self>) {
        if self.lsp.is_some() {
            return;
        }
        let Some(path) = self.buffer.path() else {
            return;
        };
        let project_root = self.language.find_project_root(path);
        let Some((program, args)) = self.language.lsp_command(project_root.as_deref()) else {
            return;
        };
        let Ok(uri) = Uri::from_str(&format!("file://{}", path.display())) else {
            return;
        };
        let Some(server) =
            shared_lsp_server(self.language.language_id(), project_root, &program, &args)
        else {
            return;
        };

        let open_params = DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: self.language.language_id().to_string(),
                version: 1,
                text: self.buffer.text(),
            },
        };
        let (events, ready, initial_diagnostics) = server.register_document(open_params);

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
            client: server,
            uri,
            version: 1,
            last_synced_revision: self.buffer.revision(),
            ready,
            diagnostics: initial_diagnostics,
            pending_hover_id: None,
            pending_definition_id: None,
            peek_definition: false,
            pending_format_id: None,
            pending_rename_id: None,
            pending_code_action_id: None,
            code_action_request: None,
            pending_completion_id: None,
            pending_references_id: None,
            pending_signature_id: None,
            pending_organize_id: None,
            pending_inlay_id: None,
            inlay_request_revision: 0,
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

        // Only trigger hover on meaningful identifiers (types, widgets, functions, methods,
        // properties) — never on whitespace, punctuation, comments, or basic keywords.
        let line_text = if line < self.buffer.line_count() {
            self.buffer.line(line)
        } else {
            String::new()
        };
        let display = line_text.trim_end_matches(['\n', '\r']);
        let trimmed = display.trim_start();
        if column >= display.len()
            || trimmed.starts_with("//")
            || trimmed.starts_with('#')
            || !display
                .as_bytes()
                .get(column)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            self.clear_hover(cx);
            return;
        }
        let Some(word) = Self::word_at(display, column) else {
            self.clear_hover(cx);
            return;
        };
        const SKIP_HOVER_KEYWORDS: &[&str] = &[
            "if", "else", "for", "while", "do", "switch", "case", "default", "break", "continue",
            "return", "true", "false", "null", "void", "var", "final", "const", "late", "required",
            "async", "await", "yield", "import", "export", "part", "library", "show", "hide", "as",
            "is", "in", "class", "enum", "mixin", "extension", "extends", "implements", "with",
            "super", "this", "new", "try", "catch", "finally", "throw", "rethrow", "assert",
            "static", "covariant", "operator", "get", "set", "fn", "let", "mut", "pub", "use",
            "mod", "impl", "struct", "trait", "where", "match", "loop", "self", "Self",
        ];
        if word.len() < 2 || SKIP_HOVER_KEYWORDS.contains(&word) {
            self.clear_hover(cx);
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
            Timer::after(Duration::from_millis(450)).await;
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
        let position = self.lsp_position_at(line, column);
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        let params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: lsp.uri.clone() },
                position,
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
        let (line, column) = self.buffer.lsp_position(head);
        cx.emit(EditorEvent::RequestRename { line, column });
    }

    /// Send `textDocument/rename` for `(line, column)` with `new_name`.
    /// Called by `app` once it's collected the new name from the user.
    /// `(line, column)` is the LSP position `RequestRename` carried.
    pub fn send_rename(&mut self, line: u32, column: u32, new_name: String) {
        let position = Position { line, character: column };
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        let params = RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: lsp.uri.clone() },
                position,
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
        let position = self.lsp_position_at(line, column);
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        if !lsp.ready {
            return;
        }
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: lsp.uri.clone() },
                position,
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

    /// Send `textDocument/references` for the symbol at the cursor. The
    /// reply arrives as `EditorEvent::ShowPeek`. No-op without a
    /// ready server.
    pub fn request_references(&mut self) {
        let head = self.buffer.selections().primary().head;
        let (line, column) = self.buffer.lsp_position(head);
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        if !lsp.ready {
            return;
        }
        let params = ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: lsp.uri.clone() },
                position: Position { line, character: column },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: ReferenceContext { include_declaration: true },
        };
        if let Ok(id) =
            lsp.client.request("textDocument/references", serde_json::to_value(params).unwrap_or_default())
        {
            lsp.pending_references_id = Some(id);
        }
    }

    /// Send `textDocument/signatureHelp` for the cursor, typically right
    /// after `(` or `,` is typed. No-op without a ready server.
    pub(crate) fn request_signature_help(&mut self) {
        let head = self.buffer.selections().primary().head;
        let (line, column) = self.buffer.lsp_position(head);
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        if !lsp.ready {
            return;
        }
        let params = SignatureHelpParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: lsp.uri.clone() },
                position: Position { line, character: column },
            },
            work_done_progress_params: Default::default(),
            context: None,
        };
        if let Ok(id) =
            lsp.client.request("textDocument/signatureHelp", serde_json::to_value(params).unwrap_or_default())
        {
            lsp.pending_signature_id = Some(id);
        }
    }

    /// The LSP position of byte `column` on `line` (UTF-16 columns).
    fn lsp_position_at(&self, line: usize, column: usize) -> Position {
        let (line, character) = self.buffer.lsp_position(self.buffer.line_to_byte(line) + column);
        Position { line, character }
    }

    /// Schedule a debounced `textDocument/inlayHint` request when the text
    /// changed since the last one, or when diagnostics reset the schedule.
    /// Cheap to call on every `render`.
    pub(crate) fn maybe_schedule_inlay_hints(&mut self, cx: &mut Context<Self>) {
        let revision = self.buffer.revision();
        // Above the syntax size limit there's no `synced_text` diff to
        // shift hints by, so they'd be cleared on every edit anyway.
        if !self.inlay_hints_enabled
            || self.inlay_scheduled_revision == Some(revision)
            || !self.lsp.as_ref().is_some_and(|lsp| lsp.ready)
            || !crate::should_parse_syntax(&self.buffer)
        {
            return;
        }
        self.inlay_scheduled_revision = Some(revision);
        self.inlay_sequence += 1;
        let sequence = self.inlay_sequence;
        cx.spawn(async move |weak, cx| {
            Timer::after(Duration::from_millis(300)).await;
            let _ = weak.update(cx, |this, _cx| {
                if this.inlay_sequence == sequence {
                    this.request_inlay_hints();
                }
            });
        })
        .detach();
    }

    fn request_inlay_hints(&mut self) {
        let revision = self.buffer.revision();
        let end_line = self.buffer.line_count() as u32;
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        if !lsp.ready {
            return;
        }
        let params = InlayHintParams {
            work_done_progress_params: Default::default(),
            text_document: TextDocumentIdentifier { uri: lsp.uri.clone() },
            range: Range {
                start: Position { line: 0, character: 0 },
                end: Position { line: end_line, character: 0 },
            },
        };
        if let Ok(id) =
            lsp.client.request("textDocument/inlayHint", serde_json::to_value(params).unwrap_or_default())
        {
            lsp.pending_inlay_id = Some(id);
            lsp.inlay_request_revision = revision;
        }
    }

    /// Turn inlay hints on or off (e.g. from settings). Off clears them.
    pub fn set_inlay_hints_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.inlay_hints_enabled == enabled {
            return;
        }
        self.inlay_hints_enabled = enabled;
        set_inlay_hints_setting(enabled);
        self.inlay_hints.clear();
        self.inlay_scheduled_revision = None;
        cx.notify();
    }

    /// A server hint as a buffer byte offset, using the same byte-column
    /// approximation as the rest of this module.
    fn inlay_hint_view(&self, hint: InlayHint) -> Option<InlayHintView> {
        // Type hints (`final Result<…> result`, `(Failure<…> r) =>`) crowd
        // the line more than they help; keep parameter names only for now.
        if hint.kind == Some(InlayHintKind::TYPE) {
            return None;
        }
        let line = hint.position.line as usize;
        if line >= self.buffer.line_count() {
            return None;
        }
        let offset = self.buffer.offset_from_lsp(hint.position.line, hint.position.character);
        let label = match hint.label {
            InlayHintLabel::String(s) => s,
            InlayHintLabel::LabelParts(parts) => parts.into_iter().map(|p| p.value).collect(),
        };
        let label = label.trim().to_string();
        if label.is_empty() {
            return None;
        }
        Some(InlayHintView {
            offset,
            label,
            pad_left: hint.padding_left.unwrap_or(false),
            pad_right: hint.padding_right.unwrap_or(false),
        })
    }

    /// Dismiss the signature-help popup (e.g. on `)` or Escape).
    pub(crate) fn clear_signature_help(&mut self, cx: &mut Context<Self>) {
        if self.signature_help.take().is_some() {
            cx.notify();
        }
    }

    /// Peek the definition of the symbol at the cursor instead of jumping to
    /// it: the reply arrives as `EditorEvent::ShowPeek`. No-op without a ready server.
    pub fn request_peek_definition(&mut self) {
        let head = self.buffer.selections().primary().head;
        let line = self.buffer.byte_to_line(head);
        let column = head - self.buffer.line_to_byte(line);
        self.request_definition(line, column);
        if let Some(lsp) = &mut self.lsp {
            lsp.peek_definition = lsp.ready && lsp.pending_definition_id.is_some();
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

    /// `⌘.`, the gutter lightbulb, or the tab-bar button: open the code
    /// action menu at the cursor. Instant when the actions were already
    /// prefetched for this cursor position; otherwise the menu opens when
    /// the server replies. Without a ready server, falls back to the
    /// language adapter's built-in assists.
    pub fn request_code_actions(&mut self, cx: &mut Context<Self>) {
        let head = self.buffer.selections().primary().head;
        let revision = self.buffer.revision();
        if let Some(set) = self.code_actions.as_ref().filter(|s| s.revision == revision && s.head == head) {
            let items = set.items.clone();
            self.open_code_action_menu(items, cx);
            return;
        }
        // A prefetch for this exact position is already in flight: just
        // have its reply open the menu instead of asking twice.
        if let Some(lsp) = &mut self.lsp {
            if lsp.pending_code_action_id.is_some()
                && lsp.code_action_request.as_ref().is_some_and(|r| r.revision == revision && r.head == head)
            {
                if let Some(request) = &mut lsp.code_action_request {
                    request.open_menu = true;
                }
                return;
            }
        }
        if self.send_code_action_request(true) {
            return;
        }
        let line = self.buffer.byte_to_line(head) as u32;
        let items = code_actions::organize(self.builtin_language_code_actions(line));
        self.open_code_action_menu(items, cx);
    }

    /// Prefetch code actions ~250 ms after the cursor or text stops
    /// changing, so the lightbulb reflects what's really available. Cheap
    /// to call on every `render`.
    pub(crate) fn maybe_prefetch_code_actions(&mut self, cx: &mut Context<Self>) {
        let head = self.buffer.selections().primary().head;
        let key = (self.buffer.revision(), head);
        if self.code_action_scheduled == Some(key) {
            return;
        }
        self.code_action_scheduled = Some(key);
        // The cursor moved or the text changed: a menu for the old spot
        // would apply its edits in the wrong place.
        if self.code_action_menu.is_some() {
            self.code_action_menu = None;
        }
        if !self.lsp.as_ref().is_some_and(|lsp| lsp.ready) {
            return;
        }
        self.code_action_sequence += 1;
        let sequence = self.code_action_sequence;
        cx.spawn(async move |weak, cx| {
            Timer::after(Duration::from_millis(250)).await;
            let _ = weak.update(cx, |this, _cx| {
                if this.code_action_sequence == sequence {
                    this.send_code_action_request(false);
                }
            });
        })
        .detach();
    }

    /// Send `textDocument/codeAction` for the cursor (or selection) with
    /// the diagnostics it touches. When the cursor isn't on a diagnostic
    /// but its line has one, ask about that diagnostic's range instead so
    /// the line's quick fixes still show. Returns whether a request went out.
    fn send_code_action_request(&mut self, open_menu: bool) -> bool {
        let selection = self.buffer.selections().primary();
        let head = selection.head;
        let revision = self.buffer.revision();
        let position = |buffer: &core_buffer::Buffer, offset: usize| {
            let (line, character) = buffer.lsp_position(offset);
            Position { line, character }
        };
        let cursor = position(&self.buffer, head);
        let mut range = Range { start: position(&self.buffer, selection.start()), end: position(&self.buffer, selection.end()) };
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        if !lsp.ready {
            return false;
        }
        let touches = |d: &Diagnostic, r: &Range| {
            (d.range.start.line, d.range.start.character) <= (r.end.line, r.end.character)
                && (r.start.line, r.start.character) <= (d.range.end.line, d.range.end.character)
        };
        if selection.is_empty() && !lsp.diagnostics.iter().any(|d| touches(d, &range)) {
            if let Some(d) = lsp
                .diagnostics
                .iter()
                .filter(|d| d.range.start.line == cursor.line)
                .min_by_key(|d| d.severity.unwrap_or(DiagnosticSeverity::HINT))
            {
                range = d.range;
            }
        }
        let diagnostics: Vec<Diagnostic> = lsp.diagnostics.iter().filter(|d| touches(d, &range)).cloned().collect();
        let params = CodeActionParams {
            text_document: TextDocumentIdentifier { uri: lsp.uri.clone() },
            range,
            context: CodeActionContext { diagnostics, only: None, trigger_kind: None },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        match lsp.client.request("textDocument/codeAction", serde_json::to_value(params).unwrap_or_default()) {
            Ok(id) => {
                lsp.pending_code_action_id = Some(id);
                lsp.code_action_request = Some(CodeActionRequest { revision, head, open_menu, cache: true });
                true
            }
            Err(_) => false,
        }
    }

    /// Ask the server for refactorings (wrap with a widget, extract widget or method,
    /// and so on) for the selection, or the cursor if nothing is selected. They come
    /// back through the same picker as quick fixes.
    pub fn request_refactors(&mut self) -> bool {
        let selection = self.buffer.selections().primary();
        let revision = self.buffer.revision();
        let position = |buffer: &core_buffer::Buffer, offset: usize| {
            let (line, character) = buffer.lsp_position(offset);
            Position { line, character }
        };
        let range = Range {
            start: position(&self.buffer, selection.start()),
            end: position(&self.buffer, selection.end()),
        };
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        if !lsp.ready {
            return false;
        }
        let params = CodeActionParams {
            text_document: TextDocumentIdentifier { uri: lsp.uri.clone() },
            range,
            context: CodeActionContext {
                diagnostics: Vec::new(),
                only: Some(vec![CodeActionKind::REFACTOR]),
                trigger_kind: None,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        match lsp.client.request("textDocument/codeAction", serde_json::to_value(params).unwrap_or_default()) {
            Ok(id) => {
                lsp.pending_code_action_id = Some(id);
                lsp.code_action_request =
                    Some(CodeActionRequest { revision, head: selection.head, open_menu: true, cache: false });
                true
            }
            Err(_) => false,
        }
    }

    /// Ask the server to organize (sort and prune) imports via a
    /// `source.organizeImports` code action; the first action returned is
    /// applied when the reply arrives. Returns whether a request was sent.
    pub fn request_organize_imports(&mut self) -> bool {
        let end = self.buffer.byte_to_line(self.buffer.len_bytes()) as u32;
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        if !lsp.ready {
            return false;
        }
        let params = CodeActionParams {
            text_document: TextDocumentIdentifier { uri: lsp.uri.clone() },
            range: Range {
                start: Position { line: 0, character: 0 },
                end: Position { line: end, character: 0 },
            },
            context: CodeActionContext {
                diagnostics: Vec::new(),
                only: Some(vec![CodeActionKind::SOURCE_ORGANIZE_IMPORTS]),
                trigger_kind: None,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        match lsp.client.request("textDocument/codeAction", serde_json::to_value(params).unwrap_or_default()) {
            Ok(id) => {
                lsp.pending_organize_id = Some(id);
                true
            }
            Err(_) => false,
        }
    }

    /// Delegate to the buffer's `LanguageAdapter::builtin_code_actions` for
    /// language-specific AST/text code assists on `line`.
    pub(crate) fn builtin_language_code_actions(&self, line: u32) -> Vec<CodeActionOrCommand> {
        let Some(own_uri) = self
            .lsp
            .as_ref()
            .map(|lsp| lsp.uri.clone())
            .or_else(|| {
                let path = self.buffer.path()?;
                Uri::from_str(&format!("file://{}", path.display())).ok()
            })
        else {
            return Vec::new();
        };
        let text = self.buffer.text();
        self.language.builtin_code_actions(
            &text,
            &self.buffer,
            line,
            self.indent_width,
            &own_uri,
        )
    }

    /// Send `textDocument/completion` for the current cursor position.
    pub(crate) fn request_lsp_completion(&mut self, line: usize, column: usize) {
        let position = self.lsp_position_at(line, column);
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
                position,
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

    /// Apply a `WorkspaceEdit`'s changes (`changes` or `document_changes`) to
    /// this buffer's own file, and emit `EditorEvent::ExternalFileEdits` for any
    /// other files in the workspace so cross-file renames and refactors apply
    /// project-wide.
    fn apply_workspace_edit(&mut self, edit: WorkspaceEdit, cx: &mut Context<Self>) {
        let Some(own_uri) = self
            .lsp
            .as_ref()
            .map(|lsp| lsp.uri.clone())
            .or_else(|| {
                let path = self.buffer.path()?;
                Uri::from_str(&format!("file://{}", path.display())).ok()
            })
        else {
            return;
        };
        if let Some(changes) = edit.changes {
            for (uri, edits) in changes {
                if uri.as_str() == own_uri.as_str() {
                    self.apply_text_edits(edits, cx);
                } else if let Some(path) = uri_to_path(&uri) {
                    cx.emit(EditorEvent::ExternalFileEdits { path, edits });
                }
            }
        }
        if let Some(doc_changes) = edit.document_changes {
            match doc_changes {
                DocumentChanges::Edits(text_doc_edits) => {
                    for tde in text_doc_edits {
                        let edits: Vec<TextEdit> = tde
                            .edits
                            .into_iter()
                            .map(|e| match e {
                                OneOf::Left(te) => te,
                                OneOf::Right(ate) => ate.text_edit,
                            })
                            .collect();
                        if tde.text_document.uri.as_str() == own_uri.as_str() {
                            self.apply_text_edits(edits, cx);
                        } else if let Some(path) = uri_to_path(&tde.text_document.uri) {
                            cx.emit(EditorEvent::ExternalFileEdits { path, edits });
                        }
                    }
                }
                DocumentChanges::Operations(ops) => {
                    for op in ops {
                        if let DocumentChangeOperation::Edit(tde) = op {
                            let edits: Vec<TextEdit> = tde
                                .edits
                                .into_iter()
                                .map(|e| match e {
                                    OneOf::Left(te) => te,
                                    OneOf::Right(ate) => ate.text_edit,
                                })
                                .collect();
                            if tde.text_document.uri.as_str() == own_uri.as_str() {
                                self.apply_text_edits(edits, cx);
                            } else if let Some(path) = uri_to_path(&tde.text_document.uri) {
                                cx.emit(EditorEvent::ExternalFileEdits { path, edits });
                            }
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn apply_text_edits(&mut self, mut edits: Vec<TextEdit>, cx: &mut Context<Self>) {
        edits.sort_by_key(|edit| (edit.range.start.line, edit.range.start.character));
        // A format or quick fix is one undo step, however many edits it is.
        self.buffer.begin_transaction();
        for edit in edits.into_iter().rev() {
            let start = self.buffer.offset_from_lsp(edit.range.start.line, edit.range.start.character);
            let end = self.buffer.offset_from_lsp(edit.range.end.line, edit.range.end.character);
            self.buffer.edit(start.min(end)..end, &edit.new_text);
        }
        self.buffer.end_transaction();
        // Formatting/import edits can change any line, so the horizontal
        // extent is recomputed here rather than inferred from the caret line.
        self.recompute_content_cols();
        cx.notify();
    }

    /// Tell the server about the buffer's current text, if a server is
    /// running and past its `initialize` handshake. Deduplicated by
    /// `buffer.revision()` so calling `sync_lsp_text` in `trigger_completions`
    /// never triggers a second `didChange` on the next `render()` frame that
    /// would cause `dart language-server` to cancel the in-flight completion!
    pub(crate) fn sync_lsp_text(&mut self, text: &str) {
        let rev = self.buffer.revision();
        let Some(lsp) = &mut self.lsp else {
            return;
        };
        if !lsp.ready || lsp.last_synced_revision == rev {
            return;
        }
        lsp.last_synced_revision = rev;
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
            ServerMessage::Notification { method, .. } if method == "$ide/lspReady" => {
                lsp.ready = true;
            }
            ServerMessage::Notification { method, params } if method == "textDocument/publishDiagnostics" => {
                if let Ok(parsed) = serde_json::from_value::<PublishDiagnosticsParams>(params) {
                    if parsed.uri.as_str() == lsp.uri.as_str() {
                        lsp.diagnostics = parsed.diagnostics;
                        // Diagnostics mean the server finished analysing:
                        // hints asked for earlier may have come back empty.
                        self.inlay_scheduled_revision = None;
                        cx.notify();
                    }
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
                let peek = std::mem::take(&mut lsp.peek_definition);
                let own_uri = lsp.uri.clone();
                let Ok(value) = result else {
                    return;
                };
                let Ok(Some(response)) = serde_json::from_value::<Option<GotoDefinitionResponse>>(value) else {
                    return;
                };
                if peek {
                    let locations = all_definition_targets(&response);
                    if !locations.is_empty() {
                        cx.emit(EditorEvent::ShowPeek { title: "Definition".into(), locations });
                    }
                    return;
                }
                let Some((target_uri, line, column)) = first_definition_target(&response) else {
                    return;
                };

                if target_uri.as_str() == own_uri.as_str() {
                    let offset = self.buffer.offset_from_lsp(line, column);
                    self.set_cursor(offset, cx);
                } else if let Some(path) = uri_to_path(&target_uri) {
                    cx.emit(EditorEvent::OpenAt { path, line, column });
                }
            }
            ServerMessage::Response { id, result } if lsp.pending_organize_id == Some(id) => {
                lsp.pending_organize_id = None;
                let first = result
                    .ok()
                    .and_then(|v| serde_json::from_value::<Option<Vec<CodeActionOrCommand>>>(v).ok())
                    .flatten()
                    .and_then(|actions| actions.into_iter().next());
                if let Some(action) = first {
                    self.apply_code_action(action, cx);
                }
            }
            ServerMessage::Response { id, result } if lsp.pending_references_id == Some(id) => {
                lsp.pending_references_id = None;
                let locations = result
                    .ok()
                    .and_then(|v| serde_json::from_value::<Option<Vec<Location>>>(v).ok())
                    .flatten()
                    .unwrap_or_default();
                if !locations.is_empty() {
                    cx.emit(EditorEvent::ShowPeek { title: "References".into(), locations });
                }
            }
            ServerMessage::Response { id, result } if lsp.pending_signature_id == Some(id) => {
                lsp.pending_signature_id = None;
                let help = result
                    .ok()
                    .and_then(|v| serde_json::from_value::<Option<SignatureHelp>>(v).ok())
                    .flatten();
                let head = self.buffer.selections().primary().head;
                let line = self.buffer.byte_to_line(head);
                let column = head - self.buffer.line_to_byte(line);
                self.signature_help = help
                    .and_then(|h| signature_view(&h))
                    .map(|(label, active)| SignatureView { line, column, label, active });
                cx.notify();
            }
            ServerMessage::Response { id, result } if lsp.pending_format_id == Some(id) => {
                lsp.pending_format_id = None;
                let save_after = std::mem::take(&mut self.save_after_format);
                let edits = result
                    .ok()
                    .and_then(|v| serde_json::from_value::<Option<Vec<TextEdit>>>(v).ok())
                    .flatten()
                    .unwrap_or_default();
                if !edits.is_empty() {
                    self.apply_text_edits(edits, cx);
                }
                // Whether or not the formatter changed anything (or failed), a
                // pending save still has to happen, exactly once.
                if save_after {
                    self.finish_save(cx);
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
                let Some(request) = lsp.code_action_request.take() else {
                    return;
                };
                let head = self.buffer.selections().primary().head;
                if request.revision != self.buffer.revision() || request.head != head {
                    // Stale: its edits target text or a spot that moved.
                    if request.open_menu {
                        self.request_code_actions(cx);
                    }
                    return;
                }
                let actions: Vec<CodeActionOrCommand> = result
                    .ok()
                    .and_then(|v| serde_json::from_value::<Option<Vec<CodeActionOrCommand>>>(v).ok())
                    .flatten()
                    .unwrap_or_default();
                let items = code_actions::organize(actions);
                if request.cache {
                    self.code_actions = Some(code_actions::CodeActionSet {
                        revision: request.revision,
                        head,
                        line: self.buffer.byte_to_line(head),
                        items: items.clone(),
                    });
                    cx.notify();
                }
                if request.open_menu {
                    self.open_code_action_menu(items, cx);
                }
            }
            ServerMessage::Response { id, result } if lsp.pending_completion_id == Some(id) => {
                lsp.pending_completion_id = None;
                let Ok(value) = result else {
                    return;
                };
                let entries = parse_completion_entries_from_value(value);
                if !entries.is_empty() {
                    self.merge_lsp_completion_entries(entries, cx);
                }
            }
            ServerMessage::Response { id, result } if lsp.pending_inlay_id == Some(id) => {
                lsp.pending_inlay_id = None;
                // Edited since the request went out: its positions are for
                // old text, and `render` has already scheduled a new one.
                if lsp.inlay_request_revision != self.buffer.revision() {
                    return;
                }
                let hints = result
                    .ok()
                    .and_then(|v| serde_json::from_value::<Option<Vec<InlayHint>>>(v).ok())
                    .flatten()
                    .unwrap_or_default();
                let mut views: Vec<InlayHintView> =
                    hints.into_iter().filter_map(|h| self.inlay_hint_view(h)).collect();
                views.sort_by_key(|h| h.offset);
                if views != self.inlay_hints {
                    self.inlay_hints = views;
                    cx.notify();
                }
            }
            // A request *from* the server (already replied to by `SharedLspServer`).
            ServerMessage::Request { method, params, .. } if method == "workspace/applyEdit" => {
                if let Ok(apply_params) = serde_json::from_value::<ApplyWorkspaceEditParams>(params) {
                    self.apply_workspace_edit(apply_params.edit, cx);
                }
            }
            _ => {}
        }
    }

    /// All current LSP diagnostics for this buffer, used by the Problems dock tab.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        self.lsp
            .as_ref()
            .map_or(&[], |lsp| lsp.diagnostics.as_slice())
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

    /// The primary diagnostic (most severe) on each 0-indexed line:
    /// `(severity, column_range_on_line, first_line_message)`, used for both
    /// inline Error-Lens annotations right after the line's text and exact
    /// column underlines under the offending span.
    pub(crate) fn diagnostic_messages_by_line(
        &self,
    ) -> HashMap<u32, (DiagnosticSeverity, std::ops::Range<usize>, String)> {
        let mut by_line: HashMap<u32, (DiagnosticSeverity, std::ops::Range<usize>, String)> =
            HashMap::new();
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
            let line_text = self.buffer.line(line as usize);
            let start_col = core_buffer::utf16_to_byte_col(&line_text, diagnostic.range.start.character);
            let end_col = if diagnostic.range.end.line == line {
                core_buffer::utf16_to_byte_col(&line_text, diagnostic.range.end.character).max(start_col + 1)
            } else {
                usize::MAX
            };
            match by_line.get(&line) {
                Some((existing_sev, _, _)) if *existing_sev <= severity => {}
                _ => {
                    by_line.insert(line, (severity, start_col..end_col, first_line));
                }
            }
        }
        by_line
    }
}

/// A shown signature-help popup: the active signature's label, with the
/// byte range of the active parameter inside it (if the server named one).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SignatureView {
    pub line: usize,
    pub column: usize,
    pub label: String,
    pub active: Option<std::ops::Range<usize>>,
}

/// Pick the active signature out of a `SignatureHelp` reply and locate its
/// active parameter. `None` if the server returned no signatures.
pub(crate) fn signature_view(help: &SignatureHelp) -> Option<(String, Option<std::ops::Range<usize>>)> {
    let sig = help
        .signatures
        .get(help.active_signature.unwrap_or(0) as usize)
        .or_else(|| help.signatures.first())?;
    let active_param = sig.active_parameter.or(help.active_parameter);
    Some((sig.label.clone(), active_parameter_range(sig, active_param)))
}

fn active_parameter_range(sig: &SignatureInformation, active: Option<u32>) -> Option<std::ops::Range<usize>> {
    let param = sig.parameters.as_ref()?.get(active? as usize)?;
    match &param.label {
        // Offsets are UTF-16 code units; map them to byte offsets in the label.
        ParameterLabel::LabelOffsets([start, end]) => {
            let to_byte = |units: u32| -> usize {
                let mut seen = 0u32;
                for (byte, ch) in sig.label.char_indices() {
                    if seen >= units {
                        return byte;
                    }
                    seen += ch.len_utf16() as u32;
                }
                sig.label.len()
            };
            Some(to_byte(*start)..to_byte(*end))
        }
        ParameterLabel::Simple(text) => sig.label.find(text.as_str()).map(|at| at..at + text.len()),
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

/// Every target of a `textDocument/definition` reply as plain locations.
pub(crate) fn all_definition_targets(response: &GotoDefinitionResponse) -> Vec<Location> {
    match response {
        GotoDefinitionResponse::Scalar(location) => vec![location.clone()],
        GotoDefinitionResponse::Array(locations) => locations.clone(),
        GotoDefinitionResponse::Link(links) => links
            .iter()
            .map(|link| Location { uri: link.target_uri.clone(), range: link.target_selection_range })
            .collect(),
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
    let snippet = crate::snippet::parse_snippet(&raw_insert);
    let insert_text = snippet.text;
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
        .or_else(|| {
            item.label_details.and_then(|ld| {
                ld.description
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| ld.detail.filter(|s| !s.trim().is_empty()))
            })
        })
        .map(|d| d.lines().next().unwrap_or(&d).trim().to_string())
        .filter(|d| !d.is_empty());
    let additional_edits = item.additional_text_edits.unwrap_or_default();
    Some(CompletionEntry {
        label,
        insert_text,
        detail,
        kind,
        additional_edits,
        snippet_stops: snippet.stops,
        snippet_mirrors: snippet.mirrors,
    })
}

/// Fault-tolerantly parse completion entries from an LSP `textDocument/completion`
/// JSON response, supporting both strict `CompletionResponse` and per-item fallback
/// when `dart language-server` sends extended `itemDefaults` or `labelDetails` fields.
pub(crate) fn parse_completion_entries_from_value(value: serde_json::Value) -> Vec<CompletionEntry> {
    if let Ok(Some(resp)) = serde_json::from_value::<Option<CompletionResponse>>(value.clone()) {
        let items = match resp {
            CompletionResponse::Array(items) => items,
            CompletionResponse::List(list) => list.items,
        };
        let entries: Vec<CompletionEntry> =
            items.into_iter().filter_map(completion_item_to_entry).collect();
        if !entries.is_empty() {
            return entries;
        }
    }

    let raw_items = value
        .as_array()
        .or_else(|| value.get("items").and_then(|v| v.as_array()));
    let Some(arr) = raw_items else {
        return Vec::new();
    };
    let mut entries = Vec::with_capacity(arr.len().min(60));
    for raw in arr.iter().take(80) {
        if let Ok(item) = serde_json::from_value::<CompletionItem>(raw.clone()) {
            if let Some(entry) = completion_item_to_entry(item) {
                entries.push(entry);
                continue;
            }
        }
        let Some(label) = raw.get("label").and_then(|v| v.as_str()).map(str::trim) else {
            continue;
        };
        if label.is_empty() {
            continue;
        }
        let insert_raw = raw
            .get("textEdit")
            .and_then(|te| te.get("newText"))
            .and_then(|v| v.as_str())
            .or_else(|| raw.get("insertText").and_then(|v| v.as_str()))
            .unwrap_or(label);
        let snippet = crate::snippet::parse_snippet(insert_raw);
        let insert_text = snippet.text;
        if insert_text.is_empty() {
            continue;
        }
        let detail = raw
            .get("detail")
            .and_then(|v| v.as_str())
            .or_else(|| {
                raw.get("labelDetails")
                    .and_then(|ld| ld.get("description").or_else(|| ld.get("detail")))
                    .and_then(|v| v.as_str())
            })
            .map(|s| s.lines().next().unwrap_or(s).trim().to_string())
            .filter(|s| !s.is_empty());
        let kind_num = raw.get("kind").and_then(|v| v.as_u64()).unwrap_or(6);
        let kind = match kind_num {
            2 | 3 | 4 => CompletionKindBadge::Function,
            5 | 10 => CompletionKindBadge::Property,
            7 | 8 | 13 | 22 | 25 => CompletionKindBadge::Type,
            14 => CompletionKindBadge::Keyword,
            20 | 21 => CompletionKindBadge::Constant,
            _ => CompletionKindBadge::Variable,
        };
        entries.push(CompletionEntry {
            label: label.to_string(),
            insert_text,
            detail,
            kind,
            additional_edits: Vec::new(),
            snippet_stops: snippet.stops,
            snippet_mirrors: snippet.mirrors,
        });
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_answers_each_item_and_only_sets_the_dart_section() {
        let params = serde_json::json!({
            "items": [{"section": "dart"}, {"section": "editor"}, {"scopeUri": "file:///x", "section": "dart"}]
        });
        let reply = configuration_response(&params);
        let expected_inlay = INLAY_HINTS_SETTING.load(Ordering::Relaxed);
        assert_eq!(
            reply,
            serde_json::json!([{"inlayHints": expected_inlay}, null, {"inlayHints": expected_inlay}]),
            "one entry per item, in order"
        );
        assert_eq!(configuration_response(&serde_json::json!({})), serde_json::json!([]));
    }

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

    #[test]
    fn signature_view_marks_the_active_parameter_by_offsets() {
        let help: SignatureHelp = serde_json::from_value(serde_json::json!({
            "signatures": [{
                "label": "greet(String name, int times)",
                "parameters": [
                    {"label": [6, 17]},
                    {"label": [19, 28]}
                ]
            }],
            "activeSignature": 0,
            "activeParameter": 1
        }))
        .unwrap();
        let (label, active) = signature_view(&help).unwrap();
        assert_eq!(&label[active.unwrap()], "int times");
    }

    #[test]
    fn signature_view_marks_the_active_parameter_by_name() {
        let help: SignatureHelp = serde_json::from_value(serde_json::json!({
            "signatures": [{
                "label": "greet(String name, int times)",
                "parameters": [{"label": "String name"}, {"label": "int times"}],
                "activeParameter": 0
            }]
        }))
        .unwrap();
        let (label, active) = signature_view(&help).unwrap();
        assert_eq!(&label[active.unwrap()], "String name");
    }

    #[test]
    fn signature_view_is_none_without_signatures() {
        let help: SignatureHelp =
            serde_json::from_value(serde_json::json!({"signatures": []})).unwrap();
        assert!(signature_view(&help).is_none());
    }

    #[test]
    fn deserializes_a_references_response() {
        let value = serde_json::json!([
            {"uri": "file:///a.dart", "range": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 7}}}
        ]);
        let locations: Option<Vec<Location>> = serde_json::from_value(value).unwrap();
        assert_eq!(locations.unwrap()[0].range.start.line, 1);
    }

    #[test]
    fn definition_targets_come_out_of_all_three_reply_shapes() {
        let loc = |line: u32| serde_json::json!({
            "uri": "file:///a.dart",
            "range": {"start": {"line": line, "character": 0}, "end": {"line": line, "character": 3}}
        });
        let scalar: GotoDefinitionResponse = serde_json::from_value(loc(1)).unwrap();
        assert_eq!(all_definition_targets(&scalar).len(), 1);

        let array: GotoDefinitionResponse = serde_json::from_value(serde_json::json!([loc(1), loc(9)])).unwrap();
        let targets = all_definition_targets(&array);
        assert_eq!((targets[0].range.start.line, targets[1].range.start.line), (1, 9));

        let link: GotoDefinitionResponse = serde_json::from_value(serde_json::json!([{
            "targetUri": "file:///b.dart",
            "targetRange": {"start": {"line": 4, "character": 0}, "end": {"line": 8, "character": 1}},
            "targetSelectionRange": {"start": {"line": 5, "character": 6}, "end": {"line": 5, "character": 12}}
        }]))
        .unwrap();
        let targets = all_definition_targets(&link);
        assert_eq!(targets[0].uri.as_str(), "file:///b.dart");
        assert_eq!(targets[0].range.start.line, 5, "the symbol's name, not the whole declaration");
    }
}
