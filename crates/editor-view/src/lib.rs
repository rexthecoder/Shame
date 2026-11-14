//! Own GPUI editor element: cursor, selection, scroll, gutter, text drawing (M1).
//!
//! Built only on raw `gpui-kit` primitives (`gpui_kit::*`), never on
//! `gpui_kit::component`'s pre-built `Editor`/`Textarea` widgets — those
//! are read as a reference, not a dependency. `core-buffer` and `core-syntax` are the other two
//! dependencies, kept behind this crate's own API so the UI layer can
//! change without touching either of them.
//!
//! That boundary is also why rename has no input box *here*: a text
//! field is exactly the kind of pre-built widget this crate avoids, and
//! writing our own from scratch isn't worth it for one feature. Rename
//! starts in `EditorView` (F2/cmd+ctrl+R, computing the cursor position)
//! but the actual "type a new name" prompt is `app`'s job — see
//! `EditorEvent::RequestRename`.
//!
//! Interaction: column-accurate mouse hit-testing, word and line selection on
//! double/triple click, multi-cursor carets and selections, folding, and a
//! virtualised list that only builds the rows in view. Keyboard navigation
//! keeps the display column across short lines: ⌥←/→ move by word, Page
//! Up/Down page the viewport, and ⌘↑/↓ jump to the document edges.

pub mod language;
mod code_actions;
pub mod keymap;
mod conflicts;
mod inlay_hints;
mod brackets;
mod indent_guides;
mod lsp_integration;
mod margin_notes;
mod multicursor;
pub mod python;
mod snippet;

use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use core_buffer::{Buffer, BufferEdit, Selection};
use core_lsp::lsp_types;
use core_syntax::{Highlight, HighlightKind, InputEdit, Point, SyntaxLanguage, SyntaxTree, point_at};
use core_workspace::LineChange;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use language::{LanguageAdapter, language_for_path};
pub use conflicts::ConflictChoice;
pub use lsp_integration::{
    CompletionEntry, CompletionKindBadge, LspActivity, did_rename_files, lsp_activity,
    will_rename_files, workspace_edit_files,
};
pub use core_buffer::DiskState;
use lsp_integration::LspState;

/// Second pass on this editor's look. The first pass piled on chrome —
/// a bordered gutter column with its own background tint, a left-edge
/// accent bar on the active line, an underline under diagnostic text —
/// and the result fought itself instead of reading as one surface. This
/// pass goes the other way: fewer elements, each one doing one clear job,
/// which is the actual throughline in the `cutoff-studio` reference this
/// crate is meant to be learning from ("a flat, quiet ... layout") rather
/// than any specific color or border it draws.
const GUTTER_WIDTH: f32 = 68.;
const LINE_FONT_SIZE: f32 = 12.5;
/// Numbers run one step smaller than the code they label, the way every
/// reference editor's gutter does.
const GUTTER_FONT_SIZE: f32 = 11.5;
/// Enough breathing room to read as an editor without the file feeling
/// sparse (the first pass tried 26px, which did).
const LINE_HEIGHT: f32 = 22.;
/// Bracket pairs by depth: gold, orchid, sky — distinct from the syntax
/// colours and soft enough for dark backgrounds.
const BRACKET_COLORS: [u32; 3] = [0xe5c07b, 0xc678dd, 0x61afef];
/// Space above the first line.
const LIST_TOP_PAD: f32 = 8.;
/// At most this many enclosing blocks stick to the top while scrolling.
const STICKY_MAX: usize = 4;
/// Gap kept between an inlay hint pill and neighbouring text when the
/// server asks for padding, and the pill's own inner horizontal padding.
const INLAY_PAD: Pixels = px(4.);
const INLAY_INNER: Pixels = px(3.);
/// `tokens::LINE_ACTIVE` in the app crate — duplicated here because this
/// crate deliberately has no dependency on `app`.
const ACTIVE_LINE_BG: u32 = 0x15161a;
/// SF Mono over Menlo: still the system monospace macOS ships, but with
/// tighter, more even letterforms — the difference a long file's worth
/// of code actually shows, where a typeface pass alone wouldn't.
/// Fallback monospace family if theme is not initialized.
#[allow(dead_code)]
const FONT_FAMILY: &str = "SF Mono";

/// Emitted when this editor wants something handled above it that it has
/// no business doing itself: opening a different file (`editor-view` has
/// no idea tabs exist), collecting a new name for a rename, or showing a
/// picker for more than one code action (no text input or list widget
/// here — see the crate's module docs on why). `app` subscribes to all
/// three per tab.
/// A test CodeLens template configured via `.vscode/launch.json` (`"templateFor": "test"`).
#[derive(Debug, Clone)]
pub struct TestCodeLensTemplate {
    pub title: String,
    pub args: Vec<String>,
    pub for_test: bool,
    pub for_file: bool,
}

/// Test coverage for open files: hits per 1-based line, by absolute path.
/// The app sets it after a coverage run; every editor reads its own file.
#[derive(Default)]
pub struct Coverage(pub std::collections::HashMap<PathBuf, std::collections::HashMap<u32, u32>>);

impl Global for Coverage {}

#[derive(Debug, Clone)]
pub enum EditorEvent {
    OpenAt { path: PathBuf, line: u32, column: u32 },
    /// LSP position (UTF-16 column) of the symbol to rename; pass it back to `send_rename`.
    /// `current_name` is the identifier under the cursor, so the prompt can open
    /// prefilled and selected the way VS Code's does (empty if there is none).
    RequestRename { line: u32, column: u32, current_name: String },
    /// The server says the symbol under the cursor can't be renamed.
    RenameUnavailable,
    /// Something to tell the user that has no better place ("No implementations found").
    Notice(String),
    /// A set of locations to peek at (references, or a definition's targets).
    ShowPeek { title: String, locations: Vec<lsp_types::Location> },
    ExternalFileEdits { path: PathBuf, edits: Vec<lsp_types::TextEdit> },
    BreakpointsChanged { path: PathBuf, lines: Vec<u32> },
    Saved { path: PathBuf },
    /// Writing the file failed; nothing on disk changed.
    SaveFailed { path: PathBuf, message: String },
    RunTest {
        path: PathBuf,
        test_name: Option<String>,
        extra_args: Vec<String>,
        label: String,
    },
    /// "Ask" on a margin note: hand the problem and its code to the agent.
    /// Lines are 1-based; `code` is the text of `start_line..=end_line`.
    AskAboutProblem {
        path: PathBuf,
        line: usize,
        start_line: usize,
        end_line: usize,
        code: String,
        message: String,
        is_test: bool,
    },
}

/// Maximum file size (2 MiB) for tree-sitter parsing and syntax queries.
/// Files up to 128 KiB parse synchronously on open; larger files (up to 2 MiB)
/// parse on a background thread so opening a large file never blocks a frame,
/// and then update incrementally on every edit.
const MAX_SYNTAX_BYTES: usize = 2 * 1024 * 1024;
const SYNC_PARSE_BYTES: usize = 128 * 1024;

fn should_parse_syntax(buffer: &Buffer) -> bool {
    if buffer.len_bytes() > MAX_SYNTAX_BYTES {
        return false;
    }
    let Some(ext) = buffer.path().and_then(|p| p.extension()).and_then(|e| e.to_str()) else {
        return true;
    };
    !matches!(
        ext.to_ascii_lowercase().as_str(),
        "lock" | "txt" | "log" | "csv" | "tsv" | "svg" | "map"
    )
}

/// How saving behaves. The app sets one of these as a global; the editor reads
/// it each time it saves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveOptions {
    /// Run the language server's formatter first (⌘S; "save without formatting" skips it).
    pub format_on_save: bool,
    /// Strip spaces and tabs at the end of every line.
    pub trim_trailing_whitespace: bool,
    /// End a non-empty file with exactly one newline.
    pub insert_final_newline: bool,
}

impl Default for SaveOptions {
    fn default() -> Self {
        Self { format_on_save: true, trim_trailing_whitespace: false, insert_final_newline: false }
    }
}

impl Global for SaveOptions {}

/// How the editor displays code. The app sets one of these as a global;
/// each editor picks up changes on its next render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorOptions {
    /// Show language-server inlay hints (inferred types, parameter names).
    pub inlay_hints: bool,
    /// Faint vertical lines at each indent level, the caret's block lit.
    pub indent_guides: bool,
    /// Colour `()`, `[]` and `{}` by nesting depth.
    pub bracket_colors: bool,
}

impl Default for EditorOptions {
    fn default() -> Self {
        // Inlay hints off for now: both type and parameter-name hints read as
        // clutter.
        Self {
            inlay_hints: false,
            indent_guides: true,
            bracket_colors: true,
        }
    }
}

/// How a test declared on a line did in the last run, shown in the gutter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestMark {
    Running,
    Passed,
    Failed,
    Skipped,
}

impl Global for EditorOptions {}

/// `text` with trailing whitespace trimmed and/or a final newline ensured, or
/// `None` when nothing needs to change. Line endings (`\n` or `\r\n`) are kept.
pub(crate) fn normalize_for_save(text: &str, trim: bool, final_newline: bool) -> Option<String> {
    let mut out = String::with_capacity(text.len() + 1);
    for piece in text.split_inclusive('\n') {
        if trim {
            let (body, ending) = match piece.strip_suffix("\r\n") {
                Some(body) => (body, "\r\n"),
                None => match piece.strip_suffix('\n') {
                    Some(body) => (body, "\n"),
                    None => (piece, ""),
                },
            };
            out.push_str(body.trim_end_matches([' ', '\t']));
            out.push_str(ending);
        } else {
            out.push_str(piece);
        }
    }
    if final_newline && !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    (out != text).then_some(out)
}

/// The smallest single replacement turning `old` into `new`: `(range in old, replacement)`.
pub(crate) fn minimal_replacement<'a>(old: &str, new: &'a str) -> Option<(Range<usize>, &'a str)> {
    if old == new {
        return None;
    }
    let (a, b) = (old.as_bytes(), new.as_bytes());
    let mut prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let max_suffix = (a.len() - prefix).min(b.len() - prefix);
    let mut suffix = (0..max_suffix).take_while(|&i| a[a.len() - 1 - i] == b[b.len() - 1 - i]).count();
    while !old.is_char_boundary(a.len() - suffix) || !new.is_char_boundary(b.len() - suffix) {
        suffix -= 1;
    }
    Some((prefix..a.len() - suffix, &new[prefix..b.len() - suffix]))
}


/// A horizontal scrollbar thumb being dragged: where the pointer and the
/// scroll offset started, and the travel available to each.
struct HorizontalDrag {
    start_x: Pixels,
    start_scroll: Pixels,
    max_scroll: Pixels,
    span: Pixels,
}

pub struct EditorView {
    buffer: Buffer,
    focus_handle: FocusHandle,
    scroll_handle: UniformListScrollHandle,
    /// Width of the text area (right of the gutter) as last painted, so
    /// popups can be kept from running off its right edge.
    text_area_width: std::rc::Rc<std::cell::Cell<Pixels>>,
    /// How far the text column is panned right. The gutter never moves.
    scroll_x: Pixels,
    max_scroll_x: Pixels,
    /// Widest line in display columns, cached by buffer revision.
    content_cols: usize,
    content_revision: Option<u64>,
    /// Buffer length at the last full width scan, so a large deletion (undo,
    /// select-all delete) triggers one instead of leaving a stale wide bar.
    content_scan_len: usize,
    /// The cursor head the view last scrolled to reveal. Panning by hand must
    /// not be undone on the next frame, so only a moved cursor re-centres.
    revealed_head: Option<usize>,
    /// Display column Up/Down aims for. Kept while the cursor passes through
    /// shorter lines, so returning to a long line restores the same column.
    /// Any horizontal move, click or edit clears it.
    preferred_column: Option<usize>,
    /// Rows the list last built, used as the Page Up/Page Down step.
    viewport_lines: usize,
    /// Horizontal scrollbar thumb drag in progress.
    h_drag: Option<HorizontalDrag>,
    /// Track width minus thumb width, from the last painted scrollbar.
    h_span: Pixels,
    /// Logical line -> its row in the list (folds hide lines), rebuilt on
    /// every render so scrolling lands on the right row.
    display_line_index: Vec<usize>,
    /// Last test run's result per 0-indexed line, with the failure message.
    test_marks: std::collections::HashMap<usize, (TestMark, Option<String>)>,
    /// Bracket depth at the start of each line, rebuilt per edit.
    bracket_depths: Vec<u32>,
    /// Indent guides per line, rebuilt per edit (see `indent_guides.rs`).
    guide_counts: Vec<u16>,
    /// `(guide level, first line, last line)` of the block holding the caret.
    active_guide: Option<(u16, usize, usize)>,
    /// Width the margin-note column took last frame. The text area is
    /// measured after that reservation, so adding it back gives the editor's
    /// full width when deciding whether notes fit.
    notes_reserved: f32,
    language: Arc<dyn LanguageAdapter>,
    syntax_tree: Option<SyntaxTree>,
    /// The text `syntax_tree` was last parsed/reparsed from, so `render`
    /// can diff against the buffer's current text to build a single
    /// [`InputEdit`] rather than reparsing from scratch every frame.
    synced_text: String,
    synced_revision: u64,
    /// `(cursor head, matching bracket pair)` for `synced_text`. Bracket
    /// matching runs a highlights query over a 24 KB window (~1.4 ms), so
    /// `render` reuses this until the cursor moves or the text changes.
    bracket_pair_cache: Option<(usize, Option<(usize, usize)>)>,
    indent_width: usize,
    line_changes: std::collections::HashMap<u32, LineChange>,
    line_blame: std::collections::HashMap<u32, String>,
    /// Language server state for this buffer, spawned via `language.lsp_command(...)`.
    lsp: Option<LspState>,
    /// `(line, column)` the mouse is currently hovering, for debouncing
    /// `textDocument/hover` requests. See `lsp_integration.rs`.
    hover_point: Option<(usize, usize)>,
    hover_sequence: u64,
    hover_result: Option<(usize, String)>,
    signature_help: Option<lsp_integration::SignatureView>,
    snippet: Option<snippet::SnippetSession>,
    /// Active in-file search query (`⌘F`) and all case-insensitive byte ranges
    /// matching it across the buffer.
    search_query: String,
    search_matches: Vec<Range<usize>>,
    active_search_match: usize,
    /// Find-bar toggles: match case exactly / only match whole words.
    search_case_sensitive: bool,
    search_whole_word: bool,
    /// Active autocomplete candidates and selected row index.
    completions: Vec<CompletionEntry>,
    completion_selected: usize,
    /// Optional end-of-line closing annotations provided by `language.closing_labels(...)`.
    closing_labels: std::collections::HashMap<usize, String>,
    /// Test CodeLens templates loaded from `.vscode/launch.json` (`templateFor: "test"`).
    test_code_lenses: Vec<TestCodeLensTemplate>,
    /// 1-indexed line numbers where a debugger breakpoint is set.
    breakpoints: std::collections::BTreeSet<u32>,
    /// 1-indexed line where the debugger is currently paused, if any.
    active_debug_line: Option<u32>,
    /// Multi-line `{ ... }` fold candidates computed from `syntax_tree`.
    fold_ranges: Vec<core_syntax::FoldRange>,
    /// 0-indexed `start_line`s of blocks currently folded by the user.
    folded_starts: std::collections::HashSet<usize>,
    /// Display-row -> 0-indexed buffer line mapping when one or more blocks are folded.
    visible_lines: Vec<usize>,
    /// Whether a `save()` triggered an LSP format request that should re-flush once applied.
    pub(crate) save_after_format: bool,
    /// Language-server inlay hints, sorted by offset and shifted on every
    /// edit so they stay put while a fresh request is debounced.
    pub(crate) inlay_hints: Vec<inlay_hints::InlayHintView>,
    /// Bumped for each scheduled inlay-hint request; a debounced request
    /// only fires if no newer one was scheduled meanwhile.
    pub(crate) inlay_sequence: u64,
    /// Buffer revision the last inlay-hint request was scheduled for.
    pub(crate) inlay_scheduled_revision: Option<u64>,
    /// User setting; when off no requests are sent and nothing is drawn.
    inlay_hints_enabled: bool,
    /// Code actions prefetched for the cursor; drives the gutter lightbulb.
    code_actions: Option<code_actions::CodeActionSet>,
    /// The open code-action menu, anchored under the cursor.
    code_action_menu: Option<code_actions::CodeActionMenu>,
    /// Debounce token for code-action prefetches.
    code_action_sequence: u64,
    /// `(revision, cursor head)` the last prefetch was scheduled for.
    code_action_scheduled: Option<(u64, usize)>,
    /// The server's ranges for the symbol under the cursor: revision,
    /// cursor offset they were asked for, and the byte ranges.
    doc_highlights: Option<(u64, usize, Vec<Range<usize>>)>,
    doc_highlight_key: Option<(u64, usize)>,
    doc_highlight_seq: u64,
    /// Colour literals, one per line, from `documentColor`.
    doc_colors: Vec<(usize, Hsla)>,
    color_key: Option<u64>,
    color_seq: u64,
}

impl EditorView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::from_buffer(Buffer::new(), cx)
    }

    pub fn from_buffer(mut buffer: Buffer, cx: &mut Context<Self>) -> Self {
        let _ = buffer.take_edits();
        let language = language_for_path(buffer.path());
        let parse_syntax = should_parse_syntax(&buffer);
        let sync_parse = parse_syntax && buffer.len_bytes() <= SYNC_PARSE_BYTES;
        let grammar = syntax_language(language.language_id(), buffer.path());
        let synced_text = if parse_syntax {
            buffer.text()
        } else {
            String::new()
        };
        let syntax_tree = grammar
            .filter(|_| sync_parse)
            .map(|g| SyntaxTree::parse_as(g, &synced_text));
        let fold_ranges = if let Some(t) = &syntax_tree {
            t.fold_ranges()
        } else if parse_syntax {
            lexical_fold_ranges(&synced_text)
        } else {
            Vec::new()
        };
        let closing_labels = std::collections::HashMap::new();
        let synced_revision = buffer.revision();
        let indent_width = buffer.detect_indent_width();
        // Once per opened file: the widest line decides the horizontal extent.
        // Typing updates this from the edited line only (see `render`), so a
        // held key never rescans the whole buffer.
        let content_cols = (0..buffer.line_count())
            .map(|ix| display_columns(buffer.line(ix).trim_end_matches(['\n', '\r'])))
            .max()
            .unwrap_or(0);
        let content_len = buffer.len_bytes();
        let mut this = Self {
            buffer,
            focus_handle: cx.focus_handle(),
            scroll_handle: UniformListScrollHandle::new(),
            text_area_width: std::rc::Rc::new(std::cell::Cell::new(px(800.))),
            scroll_x: px(0.),
            max_scroll_x: px(0.),
            content_cols,
            content_revision: Some(synced_revision),
            content_scan_len: content_len,
            revealed_head: None,
            preferred_column: None,
            viewport_lines: 30,
            h_drag: None,
            h_span: px(1.),
            display_line_index: Vec::new(),
            test_marks: std::collections::HashMap::new(),
            bracket_depths: Vec::new(),
            guide_counts: Vec::new(),
            active_guide: None,
            notes_reserved: 0.,
            language,
            syntax_tree,
            synced_text,
            synced_revision,
            bracket_pair_cache: None,
            indent_width,
            line_changes: std::collections::HashMap::new(),
            line_blame: std::collections::HashMap::new(),
            lsp: None,
            hover_point: None,
            hover_sequence: 0,
            hover_result: None,
            signature_help: None,
            snippet: None,
            search_query: String::new(),
            search_matches: Vec::new(),
            active_search_match: 0,
            search_case_sensitive: false,
            search_whole_word: false,
            completions: Vec::new(),
            completion_selected: 0,
            closing_labels,
            test_code_lenses: Vec::new(),
            breakpoints: std::collections::BTreeSet::new(),
            active_debug_line: None,
            fold_ranges,
            folded_starts: std::collections::HashSet::new(),
            visible_lines: Vec::new(),
            save_after_format: false,
            inlay_hints: Vec::new(),
            inlay_sequence: 0,
            inlay_scheduled_revision: None,
            inlay_hints_enabled: false,
            code_actions: None,
            code_action_menu: None,
            code_action_sequence: 0,
            code_action_scheduled: None,
            doc_highlights: None,
            doc_highlight_key: None,
            doc_highlight_seq: 0,
            doc_colors: Vec::new(),
            color_key: None,
            color_seq: 0,
        };
        this.refresh_text_measures(&this.synced_text.clone());
        if parse_syntax && !sync_parse {
            if let Some(g) = grammar {
                this.spawn_background_parse(g, cx);
            }
        }
        this.maybe_start_lsp(cx);
        this.refresh_line_changes(cx);
        this
    }

    fn spawn_background_parse(&mut self, grammar: SyntaxLanguage, cx: &mut Context<Self>) {
        let revision = self.buffer.revision();
        let text = self.synced_text.clone();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let tree = SyntaxTree::parse_as(grammar, &text);
            let folds = tree.fold_ranges();
            let _ = tx.send_blocking((revision, tree, folds));
        });
        cx.spawn(async move |weak, cx| {
            let Ok((rev, tree, folds)) = rx.recv().await else {
                return;
            };
            let _ = weak.update(cx, |this, cx| {
                if this.buffer.revision() == rev && this.syntax_tree.is_none() {
                    this.syntax_tree = Some(tree);
                    this.fold_ranges = folds;
                    if !this.folded_starts.is_empty() {
                        this.rebuild_visible_lines();
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn refresh_line_changes(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.buffer.path().map(Path::to_path_buf) else {
            self.line_changes.clear();
            self.line_blame.clear();
            return;
        };
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let changes = core_workspace::line_changes(&path);
            let blame = core_workspace::line_blame(&path);
            let _ = tx.send_blocking((changes, blame));
        });
        cx.spawn(async move |weak, cx| {
            let Ok((changes, blame)) = rx.recv().await else {
                return;
            };
            let _ = weak.update(cx, |this, cx| {
                if this.line_changes != changes || this.line_blame != blame {
                    this.line_changes = changes;
                    this.line_blame = blame;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Synchronize `synced_text`, `syntax_tree`, `fold_ranges`, and LSP
    /// `didChange` at most once per `buffer.revision()`, avoiding duplicate
    /// full-buffer allocations across keystroke handlers and `render()`.
    /// Bracket depths and indent guides for the whole buffer, rebuilt
    /// whenever the text is (re)synced: on open, and after each edit.
    fn refresh_text_measures(&mut self, text: &str) {
        let text = if !text.is_empty() || self.buffer.is_empty() {
            std::borrow::Cow::Borrowed(text)
        } else {
            std::borrow::Cow::Owned(self.buffer.text())
        };
        self.bracket_depths = brackets::bracket_depths(&text);
        self.guide_counts = indent_guides::guide_counts(&text, self.indent_width);
    }

    pub(crate) fn sync_buffer_state(&mut self) {
        if self.buffer.revision() == self.synced_revision {
            return;
        }
        self.synced_revision = self.buffer.revision();
        self.bracket_pair_cache = None;
        self.indent_width = self.buffer.detect_indent_width();
        let buffer_edits = self.buffer.take_edits();
        let text = self.buffer.text();
        self.refresh_text_measures(&text);
        if !self.search_query.is_empty() {
            self.recompute_search_matches();
        }
        if should_parse_syntax(&self.buffer) {
            let edits: Vec<InputEdit> = if !buffer_edits.is_empty() {
                buffer_edits.iter().map(buffer_edit_to_input_edit).collect()
            } else {
                diff_as_edit(&self.synced_text, &text).into_iter().collect()
            };
            for edit in &edits {
                inlay_hints::shift_inlay_hints(
                    &mut self.inlay_hints,
                    edit.start_byte,
                    edit.old_end_byte,
                    edit.new_end_byte,
                );
            }
            let grammar = syntax_language(self.language.language_id(), self.buffer.path());
            if let Some(grammar) = grammar {
                match &mut self.syntax_tree {
                    Some(tree) if tree.language() == grammar => {
                        tree.reparse_edits(&text, &edits);
                        tree.update_fold_ranges(&mut self.fold_ranges, &edits);
                    }
                    _ => {
                        let tree = SyntaxTree::parse_as(grammar, &text);
                        self.fold_ranges = tree.fold_ranges();
                        self.syntax_tree = Some(tree);
                    }
                }
            } else {
                self.syntax_tree = None;
                self.fold_ranges = lexical_fold_ranges(&text);
            }
            self.sync_lsp_text(&text, &buffer_edits);
            self.synced_text = text;
        } else {
            // Too big to parse, but the language server still needs every
            // edit: without this its copy of the file freezes at open time
            // and diagnostics, rename and code actions act on stale text.
            self.sync_lsp_text(&text, &buffer_edits);
            self.syntax_tree = None;
            self.closing_labels.clear();
            self.fold_ranges.clear();
            self.synced_text.clear();
            self.inlay_hints.clear();
        }
        if !self.folded_starts.is_empty() {
            self.rebuild_visible_lines();
        }
    }

    pub fn toggle_fold(&mut self, start_line: usize, cx: &mut Context<Self>) {
        if !self.folded_starts.remove(&start_line) {
            self.folded_starts.insert(start_line);
        }
        self.rebuild_visible_lines();
        cx.notify();
    }

    fn fold_range_at(&self, start_line: usize) -> Option<core_syntax::FoldRange> {
        let ix = self
            .fold_ranges
            .binary_search_by_key(&start_line, |r| r.start_line)
            .ok()?;
        let r = self.fold_ranges[ix];
        (r.end_line > start_line).then_some(r)
    }

    /// The opening lines of the blocks that contain `top` (and whose
    /// opening line has scrolled away), outermost first. With more than
    /// [`STICKY_MAX`] levels, the two outermost (the class, the method) and
    /// the two innermost are kept.
    fn sticky_header_lines(&self, top: usize) -> Vec<usize> {
        if top == 0 || self.fold_ranges.is_empty() {
            return Vec::new();
        }
        let containing = |line: usize| -> Vec<usize> {
            let mut starts: Vec<usize> = self
                .fold_ranges
                .iter()
                .filter(|r| r.start_line < line && r.end_line > line)
                .map(|r| r.start_line)
                .collect();
            starts.sort_unstable();
            starts.dedup();
            starts
        };
        // The headers cover rows themselves; the line just below them is the
        // one whose context matters.
        let first = containing(top);
        if first.is_empty() {
            return first;
        }
        let mut lines = containing(top + first.len().min(STICKY_MAX));
        if lines.len() > STICKY_MAX {
            let k = lines.len();
            let mut kept = lines[..2].to_vec();
            kept.extend_from_slice(&lines[k - 2..]);
            lines = kept;
        }
        lines
    }

    fn rebuild_visible_lines(&mut self) {
        self.visible_lines.clear();
        if self.folded_starts.is_empty() {
            return;
        }
        let total = self.buffer.line_count().max(1);
        let mut line = 0usize;
        while line < total {
            self.visible_lines.push(line);
            if self.folded_starts.contains(&line) {
                if let Some(f) = self.fold_range_at(line) {
                    line = f.end_line + 1;
                    continue;
                }
            }
            line += 1;
        }
    }

    pub fn set_test_code_lenses(
        &mut self,
        lenses: Vec<TestCodeLensTemplate>,
        cx: &mut Context<Self>,
    ) {
        self.test_code_lenses = lenses;
        cx.notify();
    }

    pub fn set_breakpoints(&mut self, lines: Vec<u32>, cx: &mut Context<Self>) {
        self.breakpoints = lines.into_iter().collect();
        cx.notify();
    }

    pub fn breakpoints(&self) -> Vec<u32> {
        self.breakpoints.iter().copied().collect()
    }

    pub fn set_active_debug_line(&mut self, line: Option<u32>, cx: &mut Context<Self>) {
        if self.active_debug_line != line {
            self.active_debug_line = line;
            if let Some(l1) = line {
                self.scroll_to_line(l1.saturating_sub(1) as usize, ScrollStrategy::Center);
            }
            cx.notify();
        }
    }

    pub fn toggle_breakpoint(&mut self, line_1: u32, cx: &mut Context<Self>) {
        if !self.breakpoints.remove(&line_1) {
            self.breakpoints.insert(line_1);
        }
        if let Some(path) = self.buffer.path().map(Path::to_path_buf) {
            let lines = self.breakpoints.iter().copied().collect();
            cx.emit(EditorEvent::BreakpointsChanged { path, lines });
        }
        cx.notify();
    }

    pub fn apply_external_edits(&mut self, edits: Vec<lsp_types::TextEdit>, cx: &mut Context<Self>) {
        self.apply_text_edits(edits, cx);
        let _ = self.buffer.save();
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// 1-indexed (line, column) of the primary cursor, for a status bar —
    /// e.g. "Ln 4, Col 1" the way every other editor shows it, even though
    /// everywhere else in this crate works in 0-indexed byte offsets.
    pub fn cursor_position(&self) -> (u32, u32) {
        let head = self.buffer.selections().primary().head;
        let line = self.buffer.byte_to_line(head);
        let column = head - self.buffer.line_to_byte(line);
        (line as u32 + 1, column as u32 + 1)
    }

    /// How far the text column is panned right, in points. Zero when the file
    /// fits; the gutter never moves.
    pub fn horizontal_scroll(&self) -> f32 {
        self.scroll_x.to_f64() as f32
    }

    /// 1-indexed inclusive line range of the primary selection, if non-empty.
    /// Cheap (no text copied), so UI can show it every frame.
    pub fn selection_line_range(&self) -> Option<(usize, usize)> {
        let sel = self.buffer.selections().primary();
        if sel.is_empty() {
            return None;
        }
        let range = sel.range();
        let start_line = self.buffer.byte_to_line(range.start) + 1;
        let mut end_line = self.buffer.byte_to_line(range.end) + 1;
        // A selection that ends exactly at the start of a line doesn't include it.
        if end_line > start_line && self.buffer.line_to_byte(end_line - 1) == range.end {
            end_line -= 1;
        }
        Some((start_line, end_line))
    }

    /// The primary selection as `(start_line, end_line, text)` (1-indexed,
    /// inclusive), for attaching to an AI message.
    pub fn selection_for_context(&self) -> Option<(usize, usize, String)> {
        let (start_line, end_line) = self.selection_line_range()?;
        Some((start_line, end_line, self.buffer.slice(self.buffer.selections().primary().range())))
    }

    /// Replace the buffer with the contents of `path`, discarding undo
    /// history and resetting the cursor to the start of the file.
    pub fn open_file(&mut self, path: impl AsRef<Path>, cx: &mut Context<Self>) -> io::Result<()> {
        self.buffer = Buffer::open(path)?;
        let _ = self.buffer.take_edits();
        self.language = language_for_path(self.buffer.path());
        let parse_syntax = should_parse_syntax(&self.buffer);
        let sync_parse = parse_syntax && self.buffer.len_bytes() <= SYNC_PARSE_BYTES;
        let grammar = syntax_language(self.language.language_id(), self.buffer.path());
        if parse_syntax {
            let text = self.buffer.text();
            if let Some(grammar) = grammar {
                if sync_parse {
                    let tree = SyntaxTree::parse_as(grammar, &text);
                    self.fold_ranges = tree.fold_ranges();
                    self.syntax_tree = Some(tree);
                } else {
                    self.syntax_tree = None;
                    self.fold_ranges = lexical_fold_ranges(&text);
                }
            } else {
                self.syntax_tree = None;
                self.fold_ranges = lexical_fold_ranges(&text);
            }
            self.closing_labels.clear();
            self.synced_text = text;
        } else {
            self.syntax_tree = None;
            self.closing_labels.clear();
            self.fold_ranges.clear();
            self.synced_text.clear();
        }
        self.folded_starts.clear();
        self.visible_lines.clear();
        self.synced_revision = self.buffer.revision();
        self.bracket_pair_cache = None;
        self.indent_width = self.buffer.detect_indent_width();
        self.refresh_text_measures(&self.synced_text.clone());
        if parse_syntax && !sync_parse {
            if let Some(g) = grammar {
                self.spawn_background_parse(g, cx);
            }
        }
        self.line_changes.clear();
        self.inlay_hints.clear();
        self.inlay_scheduled_revision = None;
        self.code_actions = None;
        self.code_action_menu = None;
        self.lsp = None;
        self.maybe_start_lsp(cx);
        self.refresh_line_changes(cx);
        cx.notify();
        Ok(())
    }

    /// Treat a path-less buffer as the language of `sample_file` (e.g.
    /// `"cell.py"`): its highlighting, folding, keywords and snippets. Used
    /// by notebook cells, which have no file of their own.
    pub fn set_language_hint(&mut self, sample_file: &str, cx: &mut Context<Self>) {
        self.language = language_for_path(Some(Path::new(sample_file)));
        self.syntax_tree = None;
        self.synced_revision = u64::MAX;
        self.sync_buffer_state();
        cx.notify();
    }

    /// Replace `range` (byte offsets) with `text` as one undoable edit,
    /// leaving the cursor where it was (used by the CSV grid's cell edits).
    pub fn replace_range(&mut self, range: std::ops::Range<usize>, text: &str, cx: &mut Context<Self>) {
        let len = self.buffer.len_bytes();
        let range = range.start.min(len)..range.end.min(len);
        self.buffer.edit(range, text);
        self.recompute_content_cols();
        cx.notify();
    }

    /// Replace the whole buffer text as a single undoable edit (used by
    /// "restore from local history").
    pub fn replace_all_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let len = self.buffer.len_bytes();
        self.buffer.edit(0..len, text);
        self.set_cursor(0, cx);
        self.recompute_content_cols();
        cx.notify();
    }

    /// Save: format first when enabled (waiting for the formatter, but never
    /// longer than a couple of seconds), then write the file once.
    pub fn save(&mut self, cx: &mut Context<Self>) {
        self.save_with(true, cx);
    }

    /// Save exactly what is in the buffer: no formatter, no waiting.
    pub fn save_without_format(&mut self, cx: &mut Context<Self>) {
        self.save_with(false, cx);
    }

    fn save_with(&mut self, format: bool, cx: &mut Context<Self>) {
        self.sync_buffer_state();
        let options = cx.try_global::<SaveOptions>().copied().unwrap_or_default();
        if format
            && options.format_on_save
            && should_parse_syntax(&self.buffer)
            && self.request_format()
        {
            self.save_after_format = true;
            // A slow or dead language server must never leave the file unsaved.
            cx.spawn(async move |weak, cx| {
                smol::Timer::after(std::time::Duration::from_millis(2500)).await;
                let _ = weak.update(cx, |this, cx| {
                    if std::mem::take(&mut this.save_after_format) {
                        this.finish_save(cx);
                    }
                });
            })
            .detach();
            return;
        }
        self.finish_save(cx);
    }

    /// Apply the whitespace options, then write.
    pub(crate) fn finish_save(&mut self, cx: &mut Context<Self>) {
        let options = cx.try_global::<SaveOptions>().copied().unwrap_or_default();
        if options.trim_trailing_whitespace || options.insert_final_newline {
            let text = self.buffer.text();
            if let Some(clean) =
                normalize_for_save(&text, options.trim_trailing_whitespace, options.insert_final_newline)
            {
                if let Some((range, replacement)) = minimal_replacement(&text, &clean) {
                    self.buffer.edit(range, replacement);
                }
            }
        }
        self.flush_save(cx);
    }

    pub(crate) fn flush_save(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.buffer.path().map(Path::to_path_buf) {
            match self.buffer.save() {
                Ok(()) => cx.emit(EditorEvent::Saved { path }),
                Err(err) => cx.emit(EditorEvent::SaveFailed { path, message: err.to_string() }),
            }
        }
        self.refresh_line_changes(cx);
        cx.notify();
    }

    /// How the file on disk compares with what this editor last read or wrote.
    pub fn disk_state(&self) -> core_buffer::DiskState {
        self.buffer.disk_state()
    }

    /// Follow the file to its new path after a rename on disk.
    pub fn rebind_path(&mut self, path: impl Into<std::path::PathBuf>, cx: &mut Context<Self>) {
        self.buffer.rebind_path(path);
        self.lsp_follow_path();
        cx.notify();
    }

    /// Replace the text with the file's current contents (dropping local edits).
    pub fn reload_from_disk(&mut self, cx: &mut Context<Self>) -> io::Result<()> {
        self.buffer.reload_from_disk()?;
        self.recompute_content_cols();
        self.refresh_line_changes(cx);
        cx.notify();
        Ok(())
    }

    /// Fold changes made on disk into the open buffer, keeping unsaved edits.
    /// `Ok(Some(n))` is how many changed regions came in from disk; `Ok(None)` means
    /// the same lines changed on both sides and nothing was touched.
    pub fn merge_disk_changes(&mut self, cx: &mut Context<Self>) -> io::Result<Option<usize>> {
        let merged = self.buffer.merge_external()?;
        if merged.is_some() {
            self.recompute_content_cols();
            self.refresh_line_changes(cx);
            cx.notify();
        }
        Ok(merged)
    }

    /// Take back the last step if it is still the disk merge made at `revision`
    /// (any edit since then makes it no longer the last step, so nothing happens).
    pub fn undo_disk_merge(&mut self, revision: u64, cx: &mut Context<Self>) {
        if self.buffer.revision() == revision && self.buffer.undo() {
            self.recompute_content_cols();
            self.refresh_line_changes(cx);
            cx.notify();
        }
    }

    /// Keep this editor's version: treat the file on disk as seen, so the next
    /// save overwrites it knowingly.
    pub fn keep_my_version(&mut self, cx: &mut Context<Self>) {
        self.buffer.acknowledge_disk();
        cx.notify();
    }

    fn set_cursor(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.preferred_column = None;
        self.buffer
            .selections_mut()
            .set_primary(Selection::cursor(offset));
        cx.notify();
    }

    /// Scroll a logical line into view. Folded lines have no row, so the
    /// list index is not the line number.
    fn scroll_to_line(&self, line: usize, strategy: ScrollStrategy) {
        let row = self
            .display_line_index
            .get(line)
            .copied()
            .unwrap_or(line);
        self.scroll_handle.scroll_to_item(row, strategy);
    }

    fn ensure_cursor_visible(&mut self) {
        let head = self.buffer.selections().primary().head;
        let line = self.buffer.byte_to_line(head);
        self.scroll_to_line(line, ScrollStrategy::Nearest);
    }

    /// Pan the text column by `delta` points; negative moves the code left
    /// (towards the end of the line). Clamped to the content. The caller is
    /// responsible for `cx.notify()`.
    pub(crate) fn pan_horizontally(&mut self, delta: f64) {
        let max = self.max_scroll_x.to_f64();
        if max <= 0.0 || delta == 0.0 {
            return;
        }
        let next = (self.scroll_x.to_f64() - delta).clamp(0.0, max);
        self.scroll_x = px(next as f32);
    }

    /// Width of the widest line, recomputed over the whole buffer. Used after
    /// wholesale changes (reload, format, undo of a paste); ordinary typing
    /// only widens the edited line, which `render` folds in incrementally.
    pub(crate) fn recompute_content_cols(&mut self) {
        self.content_cols = (0..self.buffer.line_count())
            .map(|ix| display_columns(self.buffer.line(ix).trim_end_matches(['\n', '\r'])))
            .max()
            .unwrap_or(0);
        self.content_revision = Some(self.buffer.revision());
        self.content_scan_len = self.buffer.len_bytes();
    }

    /// Move the cursor to a 0-indexed (line, column) — e.g. where a
    /// go-to-definition in a newly-opened file landed. `column` is an LSP
    /// (UTF-16) column, which is also a plain character count for typical
    /// text; past the line's end it clamps to the end of that line.
    /// Results of the last test run for tests declared in this file, by
    /// 0-indexed line. Replaces whatever was shown before.
    pub fn set_test_marks(
        &mut self,
        marks: Vec<(usize, TestMark, Option<String>)>,
        cx: &mut Context<Self>,
    ) {
        let marks: std::collections::HashMap<usize, (TestMark, Option<String>)> = marks
            .into_iter()
            .map(|(line, mark, message)| (line, (mark, message)))
            .collect();
        if marks != self.test_marks {
            self.test_marks = marks;
            cx.notify();
        }
    }

    pub fn set_cursor_to_position(&mut self, line: u32, column: u32, cx: &mut Context<Self>) {
        let offset = self.buffer.offset_from_lsp(line, column);
        self.set_cursor(offset, cx);
        self.scroll_to_line(line as usize, ScrollStrategy::Center);
    }

    /// Update the active in-file search query (`⌘F`), recomputing all
    /// case-insensitive matches and jumping to the nearest match at or after
    /// the current cursor.
    pub fn set_search_query(&mut self, query: &str, cx: &mut Context<Self>) {
        if self.search_query == query {
            return;
        }
        self.search_query = query.to_string();
        self.recompute_search_matches();
        self.jump_to_nearest_search_match();
        cx.notify();
    }

    /// `(case_sensitive, whole_word)` for the find bar's toggle buttons.
    pub fn search_options(&self) -> (bool, bool) {
        (self.search_case_sensitive, self.search_whole_word)
    }

    pub fn set_search_options(&mut self, case_sensitive: bool, whole_word: bool, cx: &mut Context<Self>) {
        if (self.search_case_sensitive, self.search_whole_word) == (case_sensitive, whole_word) {
            return;
        }
        self.search_case_sensitive = case_sensitive;
        self.search_whole_word = whole_word;
        self.recompute_search_matches();
        self.jump_to_nearest_search_match();
        cx.notify();
    }

    /// Select the first match at or after the cursor and scroll to it.
    fn jump_to_nearest_search_match(&mut self) {
        if !self.search_matches.is_empty() {
            let head = self.buffer.selections().primary().start();
            self.active_search_match = self
                .search_matches
                .iter()
                .position(|r| r.start >= head)
                .unwrap_or(0);
            let target = self.search_matches[self.active_search_match].clone();
            self.buffer
                .selections_mut()
                .set_primary(Selection::cursor(target.start));
            let line = self.buffer.byte_to_line(target.start);
            self.scroll_to_line(line, ScrollStrategy::Center);
        }
    }

    pub fn clear_search(&mut self, cx: &mut Context<Self>) {
        if !self.search_query.is_empty() || !self.search_matches.is_empty() {
            self.search_query.clear();
            self.search_matches.clear();
            self.active_search_match = 0;
            cx.notify();
        }
    }

    /// `(current_1_indexed_match, total_matches)` for the find bar badge.
    pub fn search_status(&self) -> (usize, usize) {
        if self.search_matches.is_empty() {
            (0, 0)
        } else {
            (
                (self.active_search_match + 1).min(self.search_matches.len()),
                self.search_matches.len(),
            )
        }
    }

    pub fn find_next(&mut self, cx: &mut Context<Self>) {
        if self.search_matches.is_empty() {
            return;
        }
        self.active_search_match = (self.active_search_match + 1) % self.search_matches.len();
        let target = self.search_matches[self.active_search_match].clone();
        self.buffer
            .selections_mut()
            .set_primary(Selection::cursor(target.start));
        let line = self.buffer.byte_to_line(target.start);
        self.scroll_to_line(line, ScrollStrategy::Center);
        cx.notify();
    }

    pub fn find_prev(&mut self, cx: &mut Context<Self>) {
        if self.search_matches.is_empty() {
            return;
        }
        self.active_search_match = if self.active_search_match == 0 {
            self.search_matches.len() - 1
        } else {
            self.active_search_match - 1
        };
        let target = self.search_matches[self.active_search_match].clone();
        self.buffer
            .selections_mut()
            .set_primary(Selection::cursor(target.start));
        let line = self.buffer.byte_to_line(target.start);
        self.scroll_to_line(line, ScrollStrategy::Center);
        cx.notify();
    }

    pub fn replace_next(&mut self, replacement: &str, cx: &mut Context<Self>) {
        if self.search_matches.is_empty() {
            return;
        }
        let ix = self
            .active_search_match
            .min(self.search_matches.len().saturating_sub(1));
        let range = self.search_matches[ix].clone();
        self.buffer.edit(range.clone(), replacement);
        self.sync_buffer_state();
        if !self.search_matches.is_empty() {
            self.active_search_match = ix.min(self.search_matches.len() - 1);
            let next_target = self.search_matches[self.active_search_match].clone();
            self.buffer
                .selections_mut()
                .set_primary(Selection::cursor(next_target.start));
            let line = self.buffer.byte_to_line(next_target.start);
            self.scroll_to_line(line, ScrollStrategy::Center);
        }
        cx.notify();
    }

    pub fn replace_all(&mut self, replacement: &str, cx: &mut Context<Self>) {
        if self.search_matches.is_empty() {
            return;
        }
        let ranges: Vec<Range<usize>> = self.search_matches.iter().rev().cloned().collect();
        for range in ranges {
            self.buffer.edit(range, replacement);
        }
        self.sync_buffer_state();
        cx.notify();
    }

    /// Extract document symbols `(symbol_name, kind_badge, 0_indexed_line)` from the buffer
    /// for the Go to Symbol in File (`⌘⇧O` / `@`) overlay.
    pub fn document_symbols(&self) -> Vec<(String, &'static str, u32)> {
        let text = if !self.synced_text.is_empty() {
            std::borrow::Cow::Borrowed(self.synced_text.as_str())
        } else {
            std::borrow::Cow::Owned(self.buffer.text())
        };
        extract_document_symbols(&text)
    }

    fn recompute_search_matches(&mut self) {
        self.search_matches.clear();
        if self.search_query.is_empty() {
            self.active_search_match = 0;
            return;
        }
        let text = self.buffer.text();
        self.search_matches = find_matches(
            &text,
            &self.search_query,
            self.search_case_sensitive,
            self.search_whole_word,
        );
        if self.active_search_match >= self.search_matches.len() {
            self.active_search_match = 0;
        }
    }

    /// Select the word at `column` on `line_idx` (double-click behavior).
    fn select_word_on_line(&mut self, line_idx: usize, column: usize, cx: &mut Context<Self>) {
        let line_start = self.buffer.line_to_byte(line_idx);
        let line_text = self.buffer.line(line_idx);
        let display = line_text.trim_end_matches(['\n', '\r']);
        if let Some(range) = Self::word_range_at(display, column.min(display.len())) {
            self.preferred_column = None;
            self.buffer.selections_mut().set_primary(Selection::new(
                line_start + range.start,
                line_start + range.end,
            ));
            cx.notify();
        } else {
            self.set_cursor(line_start + column.min(display.len()), cx);
        }
    }

    /// Select the entire `line_idx` including its newline (triple-click behavior).
    fn select_line(&mut self, line_idx: usize, cx: &mut Context<Self>) {
        let start = self.buffer.line_to_byte(line_idx);
        let end = if line_idx + 1 < self.buffer.line_count() {
            self.buffer.line_to_byte(line_idx + 1)
        } else {
            self.buffer.len_bytes()
        };
        self.preferred_column = None;
        self.buffer
            .selections_mut()
            .set_primary(Selection::new(start, end));
        cx.notify();
    }

    /// Move the head to `offset`, keeping the current anchor fixed —
    /// i.e. extend (or shrink) the selection rather than collapsing it.
    fn extend_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let primary = self.buffer.selections().primary();
        // Drag fires on every mouse-move; skip the re-render while the
        // pointer stays within the same column.
        if primary.head == offset {
            return;
        }
        self.preferred_column = None;
        self.buffer
            .selections_mut()
            .set_primary(Selection::new(primary.anchor, offset));
        cx.notify();
    }

    fn move_head_or_cursor(&mut self, offset: usize, extend: bool, cx: &mut Context<Self>) {
        if extend {
            self.extend_to(offset, cx);
        } else {
            self.set_cursor(offset, cx);
        }
        self.ensure_cursor_visible();
    }

    /// Insert `text` at the cursor (replacing any selection), as if typed.
    pub fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.insert_at_cursor(text, cx);
        cx.notify();
    }

    fn insert_at_cursor(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.has_multiple_cursors() {
            self.multi_insert(text, cx);
            return;
        }
        let primary = self.buffer.selections().primary();
        let range = primary.range();
        let insert_at = range.start;
        self.buffer.edit(range, text);
        self.set_cursor(insert_at + text.len(), cx);
        self.ensure_cursor_visible();
    }

    fn backspace(&mut self, cx: &mut Context<Self>) {
        if self.has_multiple_cursors() {
            self.multi_backspace(cx);
            return;
        }
        let primary = self.buffer.selections().primary();
        if !primary.is_empty() {
            let start = primary.start();
            self.buffer.remove(primary.range());
            self.set_cursor(start, cx);
            return;
        }
        let head = primary.head;
        let start = self.buffer.prev_char_boundary(head);
        if start == head {
            return;
        }
        // Smart pair deletion: if the cursor sits right between an empty
        // pair `()`, `{}`, `[]`, `""`, or `''`, delete both characters at once.
        let end = self.buffer.next_char_boundary(head);
        if end > head {
            let pair = self.buffer.slice(start..end);
            if matches!(pair.as_str(), "()" | "{}" | "[]" | "\"\"" | "''" | "``") {
                self.buffer.remove(start..end);
                self.set_cursor(start, cx);
                return;
            }
        }
        self.buffer.remove(start..head);
        self.set_cursor(start, cx);
        if !self.completions.is_empty() {
            self.trigger_completions(false, cx);
        }
    }

    /// Smart newline on `Enter`: preserves the current line's leading
    /// indentation, adds one indent level after `{` / `(` / `[`, and splits an
    /// empty `{|}` block into three properly indented lines.
    fn insert_newline(&mut self, cx: &mut Context<Self>) {
        if self.has_multiple_cursors() {
            self.multi_insert("\n", cx);
            return;
        }
        let primary = self.buffer.selections().primary();
        let range = primary.range();
        let line_idx = self.buffer.byte_to_line(range.start);
        let line_start = self.buffer.line_to_byte(line_idx);
        let line_text = self.buffer.line(line_idx);
        let indent_len = line_text
            .as_bytes()
            .iter()
            .take_while(|&&b| b == b' ')
            .count();
        let base_indent = " ".repeat(indent_len);
        let step_indent = " ".repeat(self.indent_width);

        let before_cursor = self.buffer.slice(line_start..range.start);
        let trimmed_before = before_cursor.trim_end();
        let opens_block = trimmed_before.ends_with('{')
            || trimmed_before.ends_with('(')
            || trimmed_before.ends_with('[');

        let next_end = self.buffer.next_char_boundary(range.end);
        let next_char = (next_end > range.end)
            .then(|| self.buffer.slice(range.end..next_end))
            .unwrap_or_default();
        let closes_block = matches!(next_char.as_str(), "}" | ")" | "]");

        if opens_block && closes_block {
            let inserted = format!("\n{base_indent}{step_indent}\n{base_indent}");
            let cursor_target = range.start + 1 + base_indent.len() + step_indent.len();
            self.buffer.edit(range, &inserted);
            self.set_cursor(cursor_target, cx);
            self.ensure_cursor_visible();
        } else if opens_block {
            let inserted = format!("\n{base_indent}{step_indent}");
            self.insert_at_cursor(&inserted, cx);
        } else {
            let inserted = format!("\n{base_indent}");
            self.insert_at_cursor(&inserted, cx);
        }
    }

    /// Smart character input: handles auto-closing brackets/quotes, wrapping
    /// an active selection in brackets/quotes, and skipping over an existing
    /// closing bracket/quote.
    fn handle_char_input(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.has_multiple_cursors() {
            self.multi_insert(text, cx);
            return;
        }
        let primary = self.buffer.selections().primary();
        let close_for = match text {
            "(" => Some(")"),
            "{" => Some("}"),
            "[" => Some("]"),
            "\"" => Some("\""),
            "'" => Some("'"),
            "`" => Some("`"),
            _ => None,
        };

        // 1. Wrap a non-empty selection in the pair
        if !primary.is_empty() {
            if let Some(close) = close_for {
                let range = primary.range();
                let selected = self.buffer.slice(range.clone());
                let wrapped = format!("{text}{selected}{close}");
                self.buffer.edit(range.clone(), &wrapped);
                self.buffer.selections_mut().set_primary(Selection::new(
                    range.start + text.len(),
                    range.start + text.len() + selected.len(),
                ));
                cx.notify();
                return;
            }
        }

        // 2. Skip over an existing closing delimiter when typing `)`, `}`, `]`, `"`, `'`
        if primary.is_empty() && matches!(text, ")" | "}" | "]" | "\"" | "'" | "`") {
            let head = primary.head;
            let next_end = self.buffer.next_char_boundary(head);
            if next_end > head && self.buffer.slice(head..next_end) == text {
                self.set_cursor(next_end, cx);
                return;
            }
        }

        // 3. Auto-close opening delimiter if the character after the cursor is
        //    whitespace, a closing delimiter, or end-of-line.
        if primary.is_empty() {
            if let Some(close) = close_for {
                let head = primary.head;
                let next_end = self.buffer.next_char_boundary(head);
                let next_ch = (next_end > head)
                    .then(|| self.buffer.slice(head..next_end))
                    .unwrap_or_default();
                let should_autoclose = next_ch.is_empty()
                    || next_ch.starts_with([' ', '\t', '\n', '\r', ')', '}', ']', ',', ';']);
                if should_autoclose {
                    let pair = format!("{text}{close}");
                    self.buffer.edit(head..head, &pair);
                    self.set_cursor(head + text.len(), cx);
                    self.ensure_cursor_visible();
                    if text == "(" {
                        self.trigger_completions(true, cx);
                        self.request_signature_help();
                    }
                    return;
                }
            }
        }

        self.insert_at_cursor(text, cx);

        match text {
            "(" | "," => self.request_signature_help(),
            ")" => self.clear_signature_help(cx),
            _ => {}
        }

        let is_trigger = text == "."
            || text == ":"
            || text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if is_trigger {
            self.trigger_completions(text == ":", cx);
        } else {
            self.dismiss_completions(cx);
        }
    }

    pub(crate) fn dismiss_completions(&mut self, cx: &mut Context<Self>) {
        if !self.completions.is_empty() {
            self.completions.clear();
            self.completion_selected = 0;
            cx.notify();
        }
    }

    /// Return `(prefix_byte_range_in_buffer, prefix_string, is_after_dot)` at
    /// the current primary cursor.
    fn current_completion_prefix(&self) -> (Range<usize>, String, bool) {
        let head = self.buffer.selections().primary().head;
        let line_idx = self.buffer.byte_to_line(head);
        let line_start = self.buffer.line_to_byte(line_idx);
        let before = self.buffer.slice(line_start..head);
        let bytes = before.as_bytes();
        let mut word_start = bytes.len();
        while word_start > 0 && is_word_byte(bytes[word_start - 1]) {
            word_start -= 1;
        }
        let prefix = before[word_start..].to_string();
        let after_dot = word_start > 0 && bytes[word_start - 1] == b'.';
        ((line_start + word_start)..head, prefix, after_dot)
    }

    pub(crate) fn trigger_completions(&mut self, manual: bool, cx: &mut Context<Self>) {
        let (_range, prefix, after_dot) = self.current_completion_prefix();
        if !manual && prefix.is_empty() && !after_dot {
            self.dismiss_completions(cx);
            return;
        }

        // Sync buffer text once so both LSP completion and local identifier scan
        // reuse `self.synced_text` without extra full-buffer allocations.
        self.sync_buffer_state();
        if should_parse_syntax(&self.buffer) {
            let head = self.buffer.selections().primary().head;
            let line = self.buffer.byte_to_line(head);
            let col = head - self.buffer.line_to_byte(line);
            self.request_lsp_completion(line, col);
        }

        self.completions = self.compute_local_completions(&prefix, after_dot, manual);
        self.completion_selected = 0;
        cx.notify();
    }

    pub(crate) fn merge_lsp_completion_entries(
        &mut self,
        lsp_entries: Vec<CompletionEntry>,
        cx: &mut Context<Self>,
    ) {
        let (_range, prefix, after_dot) = self.current_completion_prefix();
        let prefix_lower = prefix.to_ascii_lowercase();
        let mut entries: Vec<CompletionEntry> = lsp_entries
            .into_iter()
            .filter(|e| {
                e.label != prefix
                    && (prefix_lower.is_empty()
                        || contains_ascii_ci(&e.label, &prefix_lower))
            })
            .collect();

        entries.sort_by_key(|e| {
            let exact = e.label.starts_with(&prefix);
            let starts = starts_with_ascii_ci(&e.label, &prefix_lower);
            (!exact, !starts, e.label.len(), e.label.clone())
        });

        let mut seen = std::collections::HashSet::new();
        let mut merged = Vec::new();
        for entry in entries
            .into_iter()
            .chain(self.compute_local_completions(&prefix, after_dot, true))
        {
            if seen.insert(entry.label.clone()) {
                merged.push(entry);
                if merged.len() >= 28 {
                    break;
                }
            }
        }
        self.completions = merged;
        self.completion_selected = 0;
        cx.notify();
    }

    fn compute_local_completions(
        &self,
        prefix: &str,
        after_dot: bool,
        manual: bool,
    ) -> Vec<CompletionEntry> {
        if prefix.is_empty() && !after_dot && !manual {
            return Vec::new();
        }
        let prefix_lower = prefix.to_lowercase();
        let mut seen = std::collections::HashSet::new();
        let mut results: Vec<CompletionEntry> = Vec::new();

        // 1. Language adapter snippets
        if !after_dot && !prefix.is_empty() {
            for snippet in self.language.snippets() {
                if snippet.trigger.starts_with(&prefix_lower)
                    && seen.insert(snippet.trigger.to_string())
                {
                    let parsed = snippet::parse_snippet(snippet.body);
                    results.push(CompletionEntry {
                        snippet_stops: parsed.stops,
                        snippet_mirrors: parsed.mirrors,
                        label: snippet.trigger.to_string(),
                        insert_text: parsed.text,
                        detail: Some(snippet.detail.to_string()),
                        kind: CompletionKindBadge::Keyword,
                        additional_edits: Vec::new(),
                    });
                }
            }
        }

        // 2. Language adapter built-in completions (constructors, named arguments, macros, etc.)
        for entry in self.language.builtin_completions(prefix, after_dot) {
            if seen.insert(entry.label.clone()) {
                results.push(entry);
            }
        }

        // 3. Language adapter keywords & primitive types
        if !after_dot && !prefix.is_empty() {
            for &kw in self.language.keywords() {
                if kw != prefix
                    && starts_with_ascii_ci(kw, &prefix_lower)
                    && seen.insert(kw.to_string())
                {
                    let is_type = kw.bytes().next().is_some_and(|b| b.is_ascii_uppercase());
                    results.push(CompletionEntry {
                        snippet_stops: Vec::new(),
                        snippet_mirrors: Vec::new(),
                        label: kw.to_string(),
                        insert_text: kw.to_string(),
                        detail: Some(if is_type { "type" } else { "keyword" }.to_string()),
                        kind: if is_type {
                            CompletionKindBadge::Type
                        } else {
                            CompletionKindBadge::Keyword
                        },
                        additional_edits: Vec::new(),
                    });
                }
            }
        }

        // 4. Scan buffer identifiers from `self.synced_text` (zero extra allocation)
        if should_parse_syntax(&self.buffer) && !self.synced_text.is_empty() {
            let text = &self.synced_text;
            let bytes = text.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if is_word_byte(bytes[i]) && !bytes[i].is_ascii_digit() {
                    let start = i;
                    let preceded_by_dot = start > 0 && bytes[start - 1] == b'.';
                    while i < bytes.len() && is_word_byte(bytes[i]) {
                        i += 1;
                    }
                    let word = &text[start..i];
                    if word.len() >= 2
                        && word != prefix
                        && (prefix_lower.is_empty() || contains_ascii_ci(word, &prefix_lower))
                        && (!after_dot || preceded_by_dot || !prefix.is_empty())
                        && seen.insert(word.to_string())
                    {
                        let followed_by_paren = i < bytes.len() && bytes[i] == b'(';
                        let is_type = word.bytes().next().is_some_and(|b| b.is_ascii_uppercase());
                        let kind = if followed_by_paren {
                            CompletionKindBadge::Function
                        } else if preceded_by_dot {
                            CompletionKindBadge::Property
                        } else if is_type {
                            CompletionKindBadge::Type
                        } else {
                            CompletionKindBadge::Variable
                        };
                        results.push(CompletionEntry {
                            snippet_stops: Vec::new(),
                            snippet_mirrors: Vec::new(),
                            label: word.to_string(),
                            insert_text: word.to_string(),
                            detail: None,
                            kind,
                            additional_edits: Vec::new(),
                        });
                    }
                } else {
                    i += 1;
                }
            }
        }

        results.sort_by_key(|e| {
            let exact_prefix = !prefix.is_empty() && e.label.starts_with(prefix);
            let ci_prefix = !prefix_lower.is_empty() && starts_with_ascii_ci(&e.label, &prefix_lower);
            (!exact_prefix, !ci_prefix, e.label.len(), e.label.clone())
        });
        results.truncate(24);
        results
    }

    pub(crate) fn accept_completion(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.completions.get(ix).cloned() else {
            return;
        };
        let (prefix_range, _prefix, _after_dot) = self.current_completion_prefix();
        let insert_at = prefix_range.start;
        // A multi-line snippet continues at the indentation of the line it lands on.
        let snippet = (!entry.snippet_stops.is_empty()).then(|| {
            let line = self.buffer.byte_to_line(insert_at);
            let line_start = self.buffer.line_to_byte(line);
            let indent: String = self
                .buffer
                .slice(line_start..insert_at)
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            snippet::indent_snippet(
                &snippet::ParsedSnippet {
                    text: entry.insert_text.clone(),
                    stops: entry.snippet_stops.clone(),
                    mirrors: entry.snippet_mirrors.clone(),
                },
                &indent,
            )
        });
        let insert_text = snippet.as_ref().map_or(entry.insert_text.clone(), |s| s.text.clone());
        self.buffer.edit(prefix_range, &insert_text);
        let mut cursor_pos = insert_at + insert_text.len();

        // Apply any additional edits (e.g. Dart LSP auto-import headers)
        if !entry.additional_edits.is_empty() {
            let mut edits = entry.additional_edits;
            edits.sort_by_key(|edit| (edit.range.start.line, edit.range.start.character));
            for edit in edits.into_iter().rev() {
                let end = self.buffer.offset_from_lsp(edit.range.end.line, edit.range.end.character);
                let start = self
                    .buffer
                    .offset_from_lsp(edit.range.start.line, edit.range.start.character)
                    .min(end);
                self.buffer.edit(start..end, &edit.new_text);
                if end <= insert_at {
                    let removed = end - start;
                    let added = edit.new_text.len();
                    cursor_pos = cursor_pos.saturating_sub(removed) + added;
                }
            }
        }

        self.completions.clear();
        self.completion_selected = 0;
        self.set_cursor(cursor_pos.min(self.buffer.len_bytes()), cx);
        self.ensure_cursor_visible();

        // Snippet tab stops: jump to the first and queue the rest for Tab.
        // Offsets are relative to the inserted text; account for additional
        // edits that landed before it.
        if let Some(snippet) = snippet {
            let inserted_end = insert_at + insert_text.len();
            let shift = cursor_pos as isize - inserted_end as isize;
            let at = |o: usize| ((insert_at + o) as isize + shift).max(0) as usize;
            let place = |r: &Range<usize>| at(r.start)..at(r.end);
            self.snippet = Some(snippet::SnippetSession::new(
                snippet.stops.iter().map(place).collect(),
                snippet.mirrors.iter().map(|ms| ms.iter().map(place).collect()).collect(),
                self.buffer.len_bytes(),
            ));
            self.advance_snippet(cx);
        }
    }

    /// Tab inside a snippet: fill in the mirrors of the placeholder being left,
    /// then select the next tab stop. Returns whether one was available
    /// (otherwise Tab falls through to indenting).
    pub(crate) fn advance_snippet(&mut self, cx: &mut Context<Self>) -> bool {
        let head = self.buffer.selections().primary().head;
        let Some(mut session) = self.snippet.take() else {
            return false;
        };
        if !session.cursor_in_active(head, self.buffer.len_bytes()) {
            return false;
        }
        // What was typed into the placeholder we are leaving goes into its mirrors,
        // last position first so the earlier ones stay valid.
        if let Some((typed, mirrors)) = session.begin_advance(self.buffer.len_bytes()) {
            let text = self.buffer.slice(typed);
            let mut mirrors = mirrors;
            mirrors.sort_by_key(|m| std::cmp::Reverse(m.start));
            for mirror in mirrors {
                let end = mirror.end.min(self.buffer.len_bytes());
                let start = mirror.start.min(end);
                self.buffer.edit(start..end, &text);
                session.mirror_applied(&(start..end), text.len());
            }
        }
        let len = self.buffer.len_bytes();
        let Some(stop) = session.finish_advance(len) else {
            return false;
        };
        if !(session.stops.is_empty() && session.active_mirrors.is_empty()) {
            self.snippet = Some(session);
        }
        let end = stop.end.min(len);
        let start = stop.start.min(end);
        self.buffer
            .selections_mut()
            .set_primary(Selection::new(start, end));
        self.ensure_cursor_visible();
        cx.notify();
        true
    }

    /// Toggle `// ` line comments across the current line or selected lines (`⌘/`).
    /// The lines the primary selection covers, as `(first, last)`. A
    /// selection ending at the very start of a line does not include it.
    fn selected_line_span(&self) -> (usize, usize) {
        let range = self.buffer.selections().primary().range();
        let start = self.buffer.byte_to_line(range.start);
        let mut end = self.buffer.byte_to_line(range.end);
        if range.end > range.start && end > start && self.buffer.line_to_byte(end) == range.end {
            end -= 1;
        }
        (start, end)
    }

    /// Byte range of lines `first..=last`, including the last line's break
    /// (when it has one).
    fn line_block(&self, first: usize, last: usize) -> Range<usize> {
        let start = self.buffer.line_to_byte(first);
        let end = if last + 1 < self.buffer.line_count() {
            self.buffer.line_to_byte(last + 1)
        } else {
            self.buffer.len_bytes()
        };
        start..end
    }

    /// Move the selected lines one line up (`-1`) or down (`1`), keeping
    /// the selection on them.
    pub fn move_lines(&mut self, delta: isize, cx: &mut Context<Self>) {
        let (first, last) = self.selected_line_span();
        let count = self.buffer.line_count();
        let other_line = if delta < 0 {
            if first == 0 {
                return;
            }
            first - 1
        } else {
            if last + 1 >= count {
                return;
            }
            last + 1
        };
        let block = self.line_block(first, last);
        let other = self.line_block(other_line, other_line);
        let mut block_text = self.buffer.slice(block.clone());
        let mut other_text = self.buffer.slice(other.clone());
        // The file's last line has no line break: give the moved text one
        // and take it off whatever becomes last.
        if !block_text.ends_with('\n') {
            block_text.push('\n');
            other_text = other_text.trim_end_matches('\n').to_string();
        } else if !other_text.ends_with('\n') {
            other_text.push('\n');
            block_text = block_text.trim_end_matches('\n').to_string();
        }
        let sel = self.buffer.selections().primary();
        let region = block.start.min(other.start)..block.end.max(other.end);
        let (new_text, shift): (String, isize) = if delta < 0 {
            (format!("{block_text}{other_text}"), -(other_text.len() as isize))
        } else {
            (format!("{other_text}{block_text}"), other_text.len() as isize)
        };
        self.buffer.begin_transaction();
        self.buffer.edit(region, &new_text);
        self.buffer.end_transaction();
        let max = self.buffer.len_bytes();
        let move_by = |o: usize| ((o as isize + shift).max(0) as usize).min(max);
        self.buffer
            .selections_mut()
            .set_primary(Selection::new(move_by(sel.anchor), move_by(sel.head)));
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Copy the selected lines below (`down`) or above them. Copying down
    /// moves the selection onto the copy; copying up leaves it on the upper one.
    pub fn duplicate_lines(&mut self, down: bool, cx: &mut Context<Self>) {
        let (first, last) = self.selected_line_span();
        let block = self.line_block(first, last);
        let text = self.buffer.slice(block.clone());
        let sel = self.buffer.selections().primary();
        self.buffer.begin_transaction();
        let shift = if text.ends_with('\n') {
            self.buffer.insert(block.start, &text);
            if down { text.len() } else { 0 }
        } else {
            // Last line of the file: add the break before the copy.
            self.buffer.insert(block.end, &format!("\n{text}"));
            if down { text.len() + 1 } else { 0 }
        };
        self.buffer.end_transaction();
        self.buffer
            .selections_mut()
            .set_primary(Selection::new(sel.anchor + shift, sel.head + shift));
        self.ensure_cursor_visible();
        cx.notify();
    }

    /// Delete the selected lines; the cursor lands at the start of the
    /// line that takes their place.
    pub fn delete_lines(&mut self, cx: &mut Context<Self>) {
        let (first, last) = self.selected_line_span();
        let mut block = self.line_block(first, last);
        // The file's last line: also take the break before it.
        if block.end == self.buffer.len_bytes() && block.start > 0 && !self.buffer.slice(block.clone()).ends_with('\n') {
            block.start -= 1;
        }
        let landing = block.start;
        self.buffer.begin_transaction();
        self.buffer.remove(block);
        self.buffer.end_transaction();
        let landing = landing.min(self.buffer.len_bytes());
        let line = self.buffer.byte_to_line(landing);
        let start = self.buffer.line_to_byte(line);
        self.buffer.selections_mut().set_primary(Selection::cursor(start));
        self.ensure_cursor_visible();
        cx.notify();
    }

    pub fn toggle_line_comment(&mut self, cx: &mut Context<Self>) {
        let primary = self.buffer.selections().primary();
        let range = primary.range();
        let start_line = self.buffer.byte_to_line(range.start);
        let end_line = if range.end > range.start {
            let l = self.buffer.byte_to_line(range.end);
            if self.buffer.line_to_byte(l) == range.end && l > start_line {
                l - 1
            } else {
                l
            }
        } else {
            start_line
        };

        let all_commented = (start_line..=end_line).all(|line_idx| {
            let line = self.buffer.line(line_idx);
            let trimmed = line.trim_start_matches(' ').trim_end_matches(['\n', '\r']);
            !trimmed.is_empty() && trimmed.starts_with("//")
        });

        // Every line's edit is one undo step, from a menu click too.
        self.buffer.begin_transaction();
        for line_idx in (start_line..=end_line).rev() {
            let line_start = self.buffer.line_to_byte(line_idx);
            let line = self.buffer.line(line_idx);
            let leading = line.as_bytes().iter().take_while(|&&b| b == b' ').count();
            let trimmed = line[leading..].trim_end_matches(['\n', '\r']);
            if trimmed.is_empty() {
                continue;
            }
            let comment_at = line_start + leading;
            if all_commented {
                let remove_len = if trimmed.starts_with("// ") { 3 } else { 2 };
                self.buffer.remove(comment_at..(comment_at + remove_len));
            } else {
                self.buffer.insert(comment_at, "// ");
            }
        }
        self.buffer.end_transaction();
        cx.notify();
    }

    fn delete_forward(&mut self, cx: &mut Context<Self>) {
        if self.has_multiple_cursors() {
            self.multi_delete_forward(cx);
            return;
        }
        let primary = self.buffer.selections().primary();
        if !primary.is_empty() {
            let start = primary.start();
            self.buffer.remove(primary.range());
            self.set_cursor(start, cx);
            return;
        }
        let end = self.buffer.next_char_boundary(primary.head);
        if end == primary.head {
            return;
        }
        self.buffer.remove(primary.head..end);
        self.set_cursor(primary.head, cx);
    }

    fn move_left(&mut self, extend: bool, cx: &mut Context<Self>) {
        let primary = self.buffer.selections().primary();
        if !extend && !primary.is_empty() {
            self.set_cursor(primary.start(), cx);
            return;
        }
        let new_head = self.buffer.prev_char_boundary(primary.head);
        self.move_head_or_cursor(new_head, extend, cx);
    }

    fn move_right(&mut self, extend: bool, cx: &mut Context<Self>) {
        let primary = self.buffer.selections().primary();
        if !extend && !primary.is_empty() {
            self.set_cursor(primary.end(), cx);
            return;
        }
        let new_head = self.buffer.next_char_boundary(primary.head);
        self.move_head_or_cursor(new_head, extend, cx);
    }

    fn move_vertical(&mut self, delta: isize, extend: bool, cx: &mut Context<Self>) {
        let head = self.buffer.selections().primary().head;
        let line = self.buffer.byte_to_line(head);
        let line_start = self.buffer.line_to_byte(line);
        let line_text = self.buffer.line(line);
        let display = line_text.trim_end_matches(['\n', '\r']);
        let column = (head - line_start).min(display.len());
        // Remember the display column, not the byte offset: tabs and
        // non-ASCII characters make the two differ, and a short line in
        // between must not shrink the column the cursor returns to.
        let goal = self
            .preferred_column
            .unwrap_or_else(|| display_column_at_byte(display, column));
        self.preferred_column = Some(goal);

        let target_line = line as isize + delta;
        if target_line < 0 || target_line as usize >= self.buffer.line_count() {
            return;
        }
        let target_line = target_line as usize;

        let line_start = self.buffer.line_to_byte(target_line);
        let line_text = self.buffer.line(target_line);
        let display = line_text.trim_end_matches(['\n', '\r']);
        let new_offset = line_start + byte_at_display_column(display, goal);
        self.move_head_or_cursor(new_offset, extend, cx);
        // `set_cursor`/`extend_to` clear the goal; keep it for the next
        // vertical step.
        self.preferred_column = Some(goal);
    }

    /// Move to the start of the previous word, or past the end of the next
    /// one, with the usual punctuation/whitespace skipping.
    fn move_word(&mut self, left: bool, extend: bool, cx: &mut Context<Self>) {
        let text = self.buffer.text();
        let head = self.buffer.selections().primary().head;
        let target = if left {
            prev_word_boundary(&text, head)
        } else {
            next_word_boundary(&text, head)
        };
        self.move_head_or_cursor(target, extend, cx);
    }

    /// Page Up/Page Down: move by the rows the list last built, keeping the
    /// goal column, and let `ensure_cursor_visible` bring the caret back.
    fn move_page(&mut self, direction: isize, extend: bool, cx: &mut Context<Self>) {
        let step = self.viewport_lines.saturating_sub(1).max(1) as isize;
        self.move_vertical(direction * step, extend, cx);
    }

    /// Cmd/Ctrl+Up and Cmd/Ctrl+Down: the first and last line.
    fn move_document_edge(&mut self, start: bool, extend: bool, cx: &mut Context<Self>) {
        self.preferred_column = None;
        let offset = if start {
            0
        } else {
            self.buffer.len_bytes()
        };
        self.move_head_or_cursor(offset, extend, cx);
    }

    fn move_home(&mut self, extend: bool, cx: &mut Context<Self>) {
        let head = self.buffer.selections().primary().head;
        let line = self.buffer.byte_to_line(head);
        self.move_head_or_cursor(self.buffer.line_to_byte(line), extend, cx);
    }

    fn move_end(&mut self, extend: bool, cx: &mut Context<Self>) {
        let head = self.buffer.selections().primary().head;
        let line = self.buffer.byte_to_line(head);
        let line_text = self.buffer.line(line);
        let line_start = self.buffer.line_to_byte(line);
        let len_without_ending = line_text.trim_end_matches(['\n', '\r']).len();
        self.move_head_or_cursor(line_start + len_without_ending, extend, cx);
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        let len = self.buffer.len_bytes();
        self.buffer
            .selections_mut()
            .set_primary(Selection::new(0, len));
        cx.notify();
    }

    fn copy(&mut self, cx: &mut App) {
        let primary = self.buffer.selections().primary();
        if primary.is_empty() {
            return;
        }
        let text = self.buffer.slice(primary.range());
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn cut(&mut self, cx: &mut Context<Self>) {
        let primary = self.buffer.selections().primary();
        if primary.is_empty() {
            return;
        }
        let text = self.buffer.slice(primary.range());
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        let start = primary.start();
        self.buffer.remove(primary.range());
        self.set_cursor(start, cx);
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        self.insert_at_cursor(&text, cx);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Whatever one key press does (comment a block, type at every
        // cursor, wrap a selection) undoes as one step.
        self.buffer.begin_transaction();
        self.handle_key_down(event, window, cx);
        self.buffer.end_transaction();
    }

    fn handle_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_hover(cx);
        let keystroke = &event.keystroke;
        let primary_mod = keystroke.modifiers.platform || keystroke.modifiers.control;
        let command = keymap::resolve(cx, keystroke);

        // ⌥↑/⌥↓ move lines and ⇧⌥↑/⇧⌥↓ copy them, as in VS Code. The editor
        // takes these itself, so the diff view keeps ⌥↑/⌥↓ for its changes.
        {
            let m = &keystroke.modifiers;
            if m.alt && !m.platform && !m.control && self.completions.is_empty() && self.code_action_menu.is_none() {
                match (keystroke.key.as_str(), m.shift) {
                    ("up", false) => return self.move_lines(-1, cx),
                    ("down", false) => return self.move_lines(1, cx),
                    ("up", true) => return self.duplicate_lines(false, cx),
                    ("down", true) => return self.duplicate_lines(true, cx),
                    _ => {}
                }
            }
        }

        // Manual autocomplete (Ctrl-Space by default) works even while a
        // popup is open, so it's checked before the popups take keys.
        if command.as_deref() == Some("trigger-completion") {
            self.trigger_completions(true, cx);
            return;
        }

        // Navigate / apply / dismiss the code-action menu. Any other key
        // closes it and is handled normally.
        if let Some(menu) = &mut self.code_action_menu {
            let count = menu.items.len();
            match keystroke.key.as_str() {
                "down" if count > 0 => {
                    menu.selected = (menu.selected + 1) % count;
                    cx.notify();
                    return;
                }
                "up" if count > 0 => {
                    menu.selected = (menu.selected + count - 1) % count;
                    cx.notify();
                    return;
                }
                "enter" | "tab" if count > 0 => {
                    let ix = menu.selected;
                    self.apply_code_action_from_menu(ix, cx);
                    return;
                }
                "escape" => {
                    self.code_action_menu = None;
                    cx.notify();
                    return;
                }
                _ => {
                    self.code_action_menu = None;
                    cx.notify();
                }
            }
        }

        // Navigate / accept / dismiss active autocomplete popup
        if !self.completions.is_empty() && !primary_mod {
            match keystroke.key.as_str() {
                "down" => {
                    self.completion_selected =
                        (self.completion_selected + 1) % self.completions.len();
                    cx.notify();
                    return;
                }
                "up" => {
                    self.completion_selected = if self.completion_selected == 0 {
                        self.completions.len() - 1
                    } else {
                        self.completion_selected - 1
                    };
                    cx.notify();
                    return;
                }
                "tab" | "enter" => {
                    self.accept_completion(self.completion_selected, cx);
                    return;
                }
                "escape" => {
                    self.dismiss_completions(cx);
                    return;
                }
                _ => {}
            }
        }
        if keystroke.key == "escape" {
            self.clear_signature_help(cx);
            self.collapse_selections(cx);
            self.snippet = None;
        }

        // Rebindable shortcuts (see `keymap.rs`).
        if let Some(command) = command.as_deref() {
            if self.run_editor_command(command, cx) {
                return;
            }
            // An app-level command (`⌘B`, `⌘P`, …): leave it for
            // `DartIde::on_key_down`. Without ⌘/Ctrl (e.g. `⌥↑`, which reviews a
            // diff in the app) the key still does its normal editing job too.
            if primary_mod {
                return;
            }
        } else if primary_mod {
            // Cmd/Ctrl+Up and Cmd/Ctrl+Down are navigation, not an unbound
            // app chord. Option is excluded because ⌥⌘↑/↓ add cursors.
            if !keystroke.modifiers.alt {
                match keystroke.key.as_str() {
                    "up" => {
                        self.dismiss_completions(cx);
                        self.move_document_edge(true, keystroke.modifiers.shift, cx);
                        return;
                    }
                    "down" => {
                        self.dismiss_completions(cx);
                        self.move_document_edge(false, keystroke.modifiers.shift, cx);
                        return;
                    }
                    _ => {}
                }
            }
            // Unbound ⌘/Ctrl chords never type a character.
            return;
        }
        let extend = keystroke.modifiers.shift;
        match keystroke.key.as_str() {
            "backspace" => self.backspace(cx),
            "delete" => self.delete_forward(cx),
            "enter" => self.insert_newline(cx),
            "tab" => {
                if self.advance_snippet(cx) {
                    return;
                }
                let spaces = " ".repeat(self.indent_width);
                self.insert_at_cursor(&spaces, cx);
            }
            "left" => {
                self.dismiss_completions(cx);
                if keystroke.modifiers.alt {
                    self.move_word(true, extend, cx);
                } else {
                    self.move_left(extend, cx);
                }
            }
            "right" => {
                self.dismiss_completions(cx);
                if keystroke.modifiers.alt {
                    self.move_word(false, extend, cx);
                } else {
                    self.move_right(extend, cx);
                }
            }
            "up" => {
                self.dismiss_completions(cx);
                self.move_vertical(-1, extend, cx);
            }
            "down" => {
                self.dismiss_completions(cx);
                self.move_vertical(1, extend, cx);
            }
            "pageup" => {
                self.dismiss_completions(cx);
                self.move_page(-1, extend, cx);
            }
            "pagedown" => {
                self.dismiss_completions(cx);
                self.move_page(1, extend, cx);
            }
            "home" => {
                self.dismiss_completions(cx);
                self.move_home(extend, cx);
            }
            "end" => {
                self.dismiss_completions(cx);
                self.move_end(extend, cx);
            }
            _ => {
                if let Some(text) = &keystroke.key_char {
                    // GPUI builds `key_char` from the key and Shift/Option
                    // only, so Caps Lock would otherwise type lowercase.
                    if window.capslock().on && !keystroke.modifiers.shift {
                        self.handle_char_input(&text.to_uppercase(), cx);
                    } else {
                        self.handle_char_input(text, cx);
                    }
                }
            }
        }
    }

    /// Run an editor-level keymap command. Returns `false` for commands the
    /// editor doesn't own (app-level ones such as `toggle-sidebar`).
    /// Run an editor command by id (the palette's way in). Returns `false`
    /// for ids the editor doesn't own.
    pub fn run_command(&mut self, command: &str, cx: &mut Context<Self>) -> bool {
        self.buffer.begin_transaction();
        let handled = self.run_editor_command(command, cx);
        self.buffer.end_transaction();
        handled
    }

    fn run_editor_command(&mut self, command: &str, cx: &mut Context<Self>) -> bool {
        match command {
            "copy" => self.copy(cx),
            "cut" => self.cut(cx),
            "paste" => self.paste(cx),
            "select-all" => self.select_all(cx),
            "save" => {
                self.save(cx);
                cx.notify();
            }
            "undo" => {
                self.buffer.undo();
                cx.notify();
            }
            "redo" => {
                self.buffer.redo();
                cx.notify();
            }
            "toggle-comment" => self.toggle_line_comment(cx),
            "move-line-up" => self.move_lines(-1, cx),
            "move-line-down" => self.move_lines(1, cx),
            "copy-line-up" => self.duplicate_lines(false, cx),
            "copy-line-down" => self.duplicate_lines(true, cx),
            "delete-line" => self.delete_lines(cx),
            "go-to-implementation" => self.request_goto("textDocument/implementation", "Implementations"),
            "go-to-type-definition" => self.request_goto("textDocument/typeDefinition", "Type definition"),
            "select-next-occurrence" => self.select_next_occurrence(cx),
            "add-cursor-above" => self.add_cursor_vertical(-1, cx),
            "add-cursor-below" => self.add_cursor_vertical(1, cx),
            "trigger-completion" => self.trigger_completions(true, cx),
            "code-actions" => self.request_code_actions(cx),
            "refactor" => {
                self.request_refactors(cx);
            }
            "rename" => self.start_rename(cx),
            "peek-definition" => self.request_peek_definition(),
            "find-references" => self.request_references(),
            "format-doc" => {
                self.request_format();
            }
            _ => return false,
        }
        true
    }

    /// Highlight spans intersecting `line_text`'s byte range, run-length
    /// encoded into `(byte_range_within_line, color)` runs.
    ///
    /// Note: selection and word-occurrence highlights are intentionally NOT
    /// split into separate `div()` spans here — splitting a single `TextSystem`
    /// shaped line into extra `div()` boxes causes subpixel kerning/advance
    /// rounding differences in GPUI's flex layout, shifting the text when a
    /// word or selection is highlighted. Instead, selection and word-match
    /// backgrounds are painted directly on the line canvas using exact glyph
    /// `x_for_index` coordinates.
    fn line_runs(
        line_start: usize,
        line_text: &str,
        highlights: &[Highlight],
    ) -> Vec<(Range<usize>, Option<HighlightKind>)> {
        let line_end = line_start + line_text.len();
        let mut colors: Vec<Option<HighlightKind>> = vec![None; line_text.len()];

        for h in highlights {
            if h.range.end <= line_start || h.range.start >= line_end {
                continue;
            }
            let start = h.range.start.max(line_start) - line_start;
            let end = h.range.end.min(line_end) - line_start;
            for slot in &mut colors[start..end] {
                *slot = Some(h.kind);
            }
        }

        let mut runs = Vec::new();
        let mut i = 0;
        while i < colors.len() {
            let kind = colors[i];
            let start = i;
            while i < colors.len() && colors[i] == kind {
                i += 1;
            }
            runs.push((start..i, kind));
        }
        if runs.is_empty() {
            runs.push((0..0, None));
        }
        runs
    }

    fn word_range_at(text: &str, col: usize) -> Option<Range<usize>> {
        if col > text.len() {
            return None;
        }
        let mut idx = col;
        while idx > 0 && !text.is_char_boundary(idx) {
            idx -= 1;
        }
        let word_at = |i: usize| text[i..].chars().next().is_some_and(is_word_char);
        if !word_at(idx) {
            if idx > 0 {
                let prev = text[..idx].chars().next_back().expect("idx > 0");
                if is_word_char(prev) {
                    idx -= prev.len_utf8();
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
        let mut start = idx;
        while start > 0 {
            let prev = text[..start].chars().next_back().expect("start > 0");
            if !is_word_char(prev) {
                break;
            }
            start -= prev.len_utf8();
        }
        let mut end = idx;
        while end < text.len() {
            let next = text[end..].chars().next().expect("end < len");
            if !is_word_char(next) {
                break;
            }
            end += next.len_utf8();
        }
        Some(start..end)
    }

    /// The word (identifier-ish run of alphanumerics/underscores)
    /// touching byte column `col` in `text`, preferring the word to the
    /// right of the cursor but falling back to the one just behind it —
    /// the same convention every editor uses for "what am I pointing at"
    /// when the cursor sits exactly between two characters.
    fn word_at(text: &str, col: usize) -> Option<&str> {
        let range = Self::word_range_at(text, col)?;
        Some(&text[range])
    }

    /// Every whole-word occurrence of `word` in `text`, as local byte
    /// ranges — "whole-word" meaning neither neighbor is itself a word
    /// character, so searching for `name` doesn't also light up the
    /// middle of `rename` or `name2`.
    fn word_occurrences(text: &str, word: &str) -> Vec<Range<usize>> {
        if word.is_empty() {
            return Vec::new();
        }
        let bytes = text.as_bytes();
        let mut ranges = Vec::new();
        let mut from = 0;
        while let Some(pos) = text[from..].find(word) {
            let start = from + pos;
            let end = start + word.len();
            let left_ok = start == 0 || !is_word_byte(bytes[start - 1]);
            let right_ok = end == bytes.len() || !is_word_byte(bytes[end]);
            if left_ok && right_ok {
                ranges.push(start..end);
            }
            from = start + 1;
            if from >= bytes.len() {
                break;
            }
        }
        ranges
    }

    fn render_line(
        &self,
        index: usize,
        line_text: &str,
        cursor_line: Option<usize>,
        highlights: &[Highlight],
        selection: Option<Range<usize>>,
        diagnostics_by_line: &std::collections::HashMap<u32, lsp_types::DiagnosticSeverity>,
        diagnostic_messages: &std::collections::HashMap<
            u32,
            (lsp_types::DiagnosticSeverity, Range<usize>, String),
        >,
        line_changes: &std::collections::HashMap<u32, LineChange>,
        current_word: Option<&str>,
        bracket_pair: Option<(usize, usize)>,
        scroll_x: Pixels,
        char_width: f64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_active_line = cursor_line == Some(index);
        let display_text_check = line_text.trim_end_matches(['\n', '\r']);
        let is_comment_or_blank = display_text_check.trim().is_empty()
            || display_text_check.trim_start().starts_with("//");
        let diagnostic_severity = (!is_comment_or_blank)
            .then(|| diagnostics_by_line.get(&(index as u32)).copied())
            .flatten();
        let diagnostic_msg = (!is_comment_or_blank)
            .then(|| diagnostic_messages.get(&(index as u32)))
            .flatten();
        let gutter_color: Hsla = match diagnostic_severity {
            Some(sev) if sev == lsp_types::DiagnosticSeverity::ERROR => cx.theme().danger,
            Some(sev) if sev == lsp_types::DiagnosticSeverity::WARNING => cx.theme().warning,
            _ if is_active_line => cx.theme().foreground.opacity(0.95),
            _ => cx.theme().muted_foreground.opacity(0.45),
        };
        let line_1 = (index + 1) as u32;
        let lightbulb: Option<bool> = self
            .code_actions
            .as_ref()
            .filter(|set| {
                set.line == index && set.revision == self.buffer.revision() && !set.items.is_empty()
            })
            .map(|set| set.has_quick_fix());
        let has_breakpoint = self.breakpoints.contains(&line_1);
        // Covered (green) or missed (red) by the last coverage run.
        let coverage_hit = self.buffer.path().and_then(|path| {
            cx.try_global::<Coverage>()?.0.get(path)?.get(&line_1).copied()
        });
        let is_debug_line = self.active_debug_line == Some(line_1);
        let mono_family = cx.theme().mono_font_family.clone();
        let bp_view = cx.entity();
        let gutter = h_flex()
            .w(px(GUTTER_WIDTH))
            .h_full()
            .flex_shrink_0()
            .items_center()
            .font_family(mono_family.clone())
            .text_size(px(GUTTER_FONT_SIZE))
            .text_color(gutter_color)
            .child(
                h_flex()
                    .w(px(40.))
                    .h_full()
                    .relative()
                    .items_center()
                    .justify_end()
                    .gap(px(3.))
                    .when_some(
                        self.doc_colors.iter().find(|(l, _)| *l == index).map(|(_, c)| *c),
                        |el, color| {
                            el.child(
                                div()
                                    .absolute()
                                    .left(px(8.))
                                    .top(px((LINE_HEIGHT - 9.) / 2.))
                                    .size(px(9.))
                                    .rounded(px(2.))
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(color),
                            )
                        },
                    )
                    .when_some(coverage_hit, |el, hits| {
                        el.child(
                            div()
                                .absolute()
                                .left(px(2.))
                                .top(px(2.))
                                .bottom(px(2.))
                                .w(px(3.))
                                .rounded(px(1.5))
                                .bg(if hits > 0 {
                                    cx.theme().green.opacity(0.7)
                                } else {
                                    cx.theme().red.opacity(0.7)
                                }),
                        )
                    })
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                        cx.stop_propagation();
                        bp_view.update(cx, |this, cx| {
                            this.toggle_breakpoint(line_1, cx);
                        });
                    })
                    .when(has_breakpoint, |el| {
                        el.child(
                            div()
                                .w(px(7.))
                                .h(px(7.))
                                .rounded(px(4.))
                                .bg(cx.theme().danger),
                        )
                    })
                    .when(is_debug_line && !has_breakpoint, |el| {
                        el.child(
                            div()
                                .text_size(px(9.5))
                                .text_color(cx.theme().warning)
                                .child("\u{25b6}"),
                        )
                    })
                    .when_some(self.test_marks.get(&index).map(|(mark, _)| *mark), |el, mark| {
                        let (icon, color) = match mark {
                            TestMark::Passed => (gpui_kit::assets::IconName::CircleCheck, cx.theme().success),
                            TestMark::Failed => (gpui_kit::assets::IconName::CircleX, cx.theme().danger),
                            TestMark::Running => (gpui_kit::assets::IconName::LoaderCircle, cx.theme().primary),
                            TestMark::Skipped => (
                                gpui_kit::assets::IconName::CircleSlash,
                                cx.theme().muted_foreground,
                            ),
                        };
                        el.child(
                            gpui_kit::component::Icon::new(icon)
                                .with_size(px(10.))
                                .text_color(color),
                        )
                    })
                    .child(div().child(line_1.to_string())),
            )
            .child(
                h_flex()
                    .w(px(GUTTER_WIDTH - 40.))
                    .h_full()
                    .items_center()
                    .justify_center()
                    .gap(px(2.))
                    .when_some(lightbulb.filter(|_| is_active_line), |el, has_fix| {
                        let view = cx.entity();
                        // Amber when a fix is on offer, quiet for refactors only.
                        let color = if has_fix {
                            cx.theme().warning
                        } else {
                            cx.theme().muted_foreground.opacity(0.7)
                        };
                        el.child(
                            div()
                                .cursor_pointer()
                                .child(
                                    gpui_kit::component::Icon::new(
                                        gpui_kit::assets::IconName::Lightbulb,
                                    )
                                    .with_size(px(11.))
                                    .text_color(color),
                                )
                                .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                                    cx.stop_propagation();
                                    view.update(cx, |this, cx| {
                                        window.focus(&this.focus_handle, cx);
                                        this.request_code_actions(cx);
                                    });
                                }),
                        )
                    })
                    .when_some(
                        self.fold_range_at(index),
                        |el, _| {
                            let is_folded = self.folded_starts.contains(&index);
                            let view = cx.entity();
                            el.child(
                                div()
                                    .id(("fold-chevron", index))
                                    .cursor_pointer()
                                    .text_color(if is_folded {
                                        cx.theme().primary
                                    } else {
                                        cx.theme().muted_foreground.opacity(0.75)
                                    })
                                    .child(
                                        gpui_kit::component::Icon::new(if is_folded {
                                            gpui_kit::assets::IconName::ChevronRight
                                        } else {
                                            gpui_kit::assets::IconName::ChevronDown
                                        })
                                        .with_size(px(11.)),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        move |_event, _window, cx| {
                                            cx.stop_propagation();
                                            view.update(cx, |this, cx| {
                                                this.toggle_fold(index, cx);
                                            });
                                        },
                                    ),
                            )
                        },
                    ),
            );

        // A thin bar flush against the window's left edge for a line
        // that differs from `HEAD` — green for a new line, blue for an
        // edited one, and a short amber notch at the top of the line
        // that now sits where something was deleted. On the active line,
        // fall back to a 2px accent bar (`docs/design-spec.md` §3.19).
        let change_marker = line_changes
            .get(&(index as u32))
            .map(|change| {
                let (color, full_height) = match change {
                    LineChange::Added => (cx.theme().success, true),
                    LineChange::Modified => (cx.theme().info, true),
                    LineChange::Removed => (cx.theme().warning, false),
                };
                let mut marker = div().absolute().left_0().top_0().w(px(2.)).bg(color);
                marker = if full_height { marker.bottom_0() } else { marker.h(px(5.)) };
                marker
            })
            .or_else(|| {
                is_active_line.then(|| {
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(2.))
                        .bg(cx.theme().primary)
                })
            });

        let active_right_marker = is_active_line.then(|| {
            div()
                .absolute()
                .right(px(2.))
                .top(px(2.))
                .bottom(px(2.))
                .w(px(3.))
                .rounded(px(1.5))
                .bg(cx.theme().success.opacity(0.75))
        });

        let line_start = self.buffer.line_to_byte(index);
        let display_text = line_text.trim_end_matches(['\n', '\r']);
        let row_group: SharedString = format!("editor-row-{index}").into();
        // A test declaration gets run actions after its text; the blame would
        // sit in the same place, so it yields to them on that line.
        let test_target = self
            .language
            .detect_test_target(self.buffer.path(), display_text);
        let cursor_col = (cursor_line == Some(index)).then(|| {
            let head = self.buffer.selections().primary().head;
            (head - line_start).min(display_text.len())
        });

        let (extra_carets, extra_sels) = self.secondary_line_marks(line_start, display_text.len());
        let selection_bg = cx.theme().selection.opacity(0.35);
        let word_match_bg = cx.theme().blue.opacity(0.16);
        let search_match_bg = cx.theme().warning.opacity(0.22);
        let active_search_bg = cx.theme().primary.opacity(0.38);
        let active_search_border = cx.theme().primary.opacity(0.85);
        let default_fg: Hsla = cx.theme().foreground;
        let is_doc_line = display_text.trim_start().starts_with("///");
        let caret_color = cx.theme().caret;

        // Build `TextRun`s for the single `shape_line` call that both paints
        // the text and computes exact `x_for_index` positions for the cursor,
        // selection, and word-match highlights — guaranteeing 0.0px drift
        // between characters and the caret!
        let options = cx.try_global::<EditorOptions>().copied().unwrap_or_default();
        let runs: Vec<_> = Self::line_runs(line_start, display_text, highlights)
            .into_iter()
            .filter(|(range, _)| !range.is_empty())
            .collect();
        let runs = if options.bracket_colors {
            let depth = self.bracket_depths.get(index).copied().unwrap_or(0);
            brackets::split_bracket_runs(display_text, runs, depth)
        } else {
            runs.into_iter().map(|(range, kind)| (range, kind, None)).collect()
        };
        let text_runs: Vec<TextRun> = runs
            .into_iter()
            .map(|(range, kind, bracket)| {
                let mut run_font = font(mono_family.clone());
                if let Some(k) = kind {
                    if k == HighlightKind::Comment
                        || (is_doc_line && k == HighlightKind::Attribute)
                    {
                        run_font.style = FontStyle::Italic;
                    }
                }
                TextRun {
                    len: range.len(),
                    font: run_font,
                    color: match bracket {
                        Some(level) => rgb(BRACKET_COLORS[level as usize % BRACKET_COLORS.len()]).into(),
                        None => kind.map(|k| highlight_color(k).into()).unwrap_or(default_fg),
                    },
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }
            })
            .collect();

        // Local byte ranges within `display_text` for word-occurrences and matching brackets.
        // Other uses of the symbol under the cursor: the server's ranges when
        // they are for this exact text and cursor, else plain word matches.
        let head_now = self.buffer.selections().primary().head;
        let server_marks = current_word.and(self.doc_highlights.as_ref()).filter(|(rev, head, _)| {
            *rev == self.buffer.revision() && *head == head_now
        });
        let mut local_word_matches: Vec<Range<usize>> = match server_marks {
            Some((_, _, ranges)) => {
                let line_end = line_start + display_text.len();
                ranges
                    .iter()
                    .filter(|r| r.start < line_end && r.end > line_start)
                    .map(|r| (r.start.max(line_start) - line_start)..(r.end.min(line_end) - line_start))
                    .collect()
            }
            None => current_word
                .map(|word| Self::word_occurrences(display_text, word))
                .unwrap_or_default(),
        };
        if let Some((b1, b2)) = bracket_pair {
            let line_end = line_start + display_text.len();
            for b in [b1, b2] {
                if b >= line_start && b < line_end {
                    let col = b - line_start;
                    local_word_matches.push(col..(col + 1));
                }
            }
        }

        let local_selection: Option<Range<usize>> = selection.and_then(|sel| {
            let line_end = line_start + display_text.len();
            (sel.end > line_start && sel.start < line_end).then(|| {
                let start = sel.start.max(line_start) - line_start;
                let end = sel.end.min(line_end) - line_start;
                start..end
            })
        });

        let line_end = line_start + display_text.len();
        let active_search_range = self.search_matches.get(self.active_search_match).cloned();
        // `search_matches` is sorted and non-overlapping: binary-search to this
        // line instead of scanning every match in the file for every row.
        let first_match = self.search_matches.partition_point(|m| m.end <= line_start);
        let local_search_matches: Vec<(Range<usize>, bool)> = self.search_matches[first_match..]
            .iter()
            .take_while(|m| m.start < line_end)
            .map(|m| {
                let is_active_match = active_search_range.as_ref() == Some(m);
                let start = m.start.max(line_start) - line_start;
                let end = m.end.min(line_end) - line_start;
                (start..end, is_active_match)
            })
            .collect();

        let diag_inline_spec: Option<(Hsla, Range<usize>)> =
            diagnostic_msg.map(|(sev, col_range, _)| {
                let diag_color = match *sev {
                    s if s == lsp_types::DiagnosticSeverity::ERROR => cx.theme().danger,
                    s if s == lsp_types::DiagnosticSeverity::WARNING => cx.theme().warning,
                    _ => cx.theme().info,
                };
                (diag_color, col_range.clone())
            });

        // Single unified line canvas: shapes `display_text` ONCE with `text_runs`
        // and uses that exact `ShapedLine` for:
        // 2. Word-occurrence & matching-bracket background pills
        // 3. Selection highlight
        // 4. Token-level diagnostic underline + inline diagnostic badge right after the line's text
        // 5. Painting the syntax-highlighted line (`shaped.paint(...)`)
        // 6. Painting the vertical caret at `shaped.x_for_index(cursor_col)`
        // 7. Mouse click/drag/hover column hit-testing (`shaped.closest_index_for_x`)
        // Guides for this line: columns `k * indent_width` for k ≥ 1 (none
        // at the text's own edge), the caret's block drawn brighter.
        let guide_columns: Vec<(usize, bool)> = if options.indent_guides {
            let width = self.indent_width.max(1);
            let count = self.guide_counts.get(index).copied().unwrap_or(0) as usize;
            (1..count)
                .map(|level| {
                    let active = self.active_guide.is_some_and(|(g, first, last)| {
                        g as usize == level && (first..=last).contains(&index)
                    });
                    (level * width, active)
                })
                .collect()
        } else {
            Vec::new()
        };
        let guide_color = cx.theme().border.opacity(0.45);
        let guide_active_color = cx.theme().muted_foreground.opacity(0.5);
        let line_shared: SharedString = display_text.to_string().into();
        let line_len = line_shared.len();
        let leading_spaces = display_text.as_bytes().iter().take_while(|&&b| b == b' ').count();
        let folded_count = self
            .folded_starts
            .contains(&index)
            .then(|| {
                self.fold_range_at(index)
                    .map(|r| r.end_line.saturating_sub(r.start_line))
            })
            .flatten();
        let closing_label_shared: Option<SharedString> = if let Some(n) = folded_count {
            Some(SharedString::from(format!("\u{22ef} {n} lines folded }}}}")) )
        } else if let Some(lbl) = self
            .closing_labels
            .get(&index)
            .filter(|_| !display_text.contains("//"))
        {
            Some(SharedString::from(lbl.clone()))
        } else if is_active_line && test_target.is_none() && !display_text.trim().is_empty() {
            self.line_blame
                .get(&(index as u32))
                .map(|b| SharedString::from(b.clone()))
        } else {
            None
        };
        let closing_font = {
            let mut f = font(mono_family.clone());
            f.style = FontStyle::Italic;
            f
        };
        let closing_color: Hsla = cx.theme().muted_foreground.opacity(0.42);
        // Inlay hints on this line as `(column, label, pad_left, pad_right)`.
        let line_inlays: Vec<(usize, SharedString, bool, bool)> = if self.inlay_hints_enabled {
            inlay_hints::hints_on_line(&self.inlay_hints, line_start..line_end)
                .map(|(col, h)| (col, SharedString::from(h.label.clone()), h.pad_left, h.pad_right))
                .collect()
        } else {
            Vec::new()
        };
        let inlay_font = font(mono_family.clone());
        let inlay_color: Hsla = cx.theme().muted_foreground.opacity(0.8);
        let inlay_bg: Hsla = cx.theme().muted_foreground.opacity(0.12);
        // Exact x of a column in the text area, for anchoring popups. Shapes
        // the same text and runs as the canvas below (GPUI reuses the layout
        // within a frame), plus any inlay hints in front of the column.
        let has_popup = (is_active_line && (!self.completions.is_empty() || self.code_action_menu.is_some()))
            || self.signature_help.as_ref().is_some_and(|sig| sig.line == index)
            || self.hover_point.is_some_and(|(line, _)| line == index);
        let column_x: Option<Box<dyn Fn(usize) -> Pixels>> = has_popup.then(|| {
            let shaped = window.text_system().shape_line(line_shared.clone(), px(LINE_FONT_SIZE), &text_runs, None);
            let (_, inlays) = shape_inlays(window, &line_inlays, &inlay_font, inlay_color);
            Box::new(move |col: usize| {
                (shaped.x_for_index(col.min(line_len)) + inlays.shift(col, true) - scroll_x)
                    .max(px(0.))
            }) as Box<dyn Fn(usize) -> Pixels>
        });
        let text_area_width = self.text_area_width.get();
        // Keep a `width`-wide popup inside the text area.
        let popup_left = |col: usize, width: f32| -> Pixels {
            let x = column_x.as_ref().map_or(px(0.), |x_at| x_at(col));
            let max = (text_area_width - px(width + 8.)).max(px(0.));
            x.min(max).max(px(0.))
        };
        let width_cell = self.text_area_width.clone();
        let view = cx.entity();
        let line_canvas = canvas(
            move |bounds, window, _cx| {
                let shaped = window.text_system().shape_line(
                    line_shared.clone(),
                    px(LINE_FONT_SIZE),
                    &text_runs,
                    None,
                );
                let (shaped_inlays, inlays) = shape_inlays(window, &line_inlays, &inlay_font, inlay_color);
                let shaped_closing = closing_label_shared.as_ref().and_then(|lbl| {
                    let run = TextRun {
                        len: lbl.len(),
                        font: closing_font.clone(),
                        color: closing_color,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    let full = window.text_system().shape_line(
                        lbl.clone(),
                        px(LINE_FONT_SIZE - 1.),
                        &[run],
                        None,
                    );
                    // Blame on the active line can be longer than the space
                    // left after the code: shorten it with an ellipsis rather
                    // than letting the viewport cut it mid-word.
                    let available = bounds.size.width.to_f64() + scroll_x.to_f64()
                        - (shaped.width + inlays.total()).to_f64()
                        - 20.0;
                    let fitted = fit_label(lbl, full.width.to_f64(), available)?;
                    if fitted == lbl.as_ref() {
                        Some(full)
                    } else {
                        let run = TextRun {
                            len: fitted.len(),
                            font: closing_font.clone(),
                            color: closing_color,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        Some(window.text_system().shape_line(
                            fitted.into(),
                            px(LINE_FONT_SIZE - 1.),
                            &[run],
                            None,
                        ))
                    }
                });
                let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                (hitbox, shaped, shaped_closing, inlays, shaped_inlays)
            },
            move |bounds, (hitbox, shaped, shaped_closing, inlays, shaped_inlays), window, cx| {
                width_cell.set(bounds.size.width);
                // The row itself stays where it is (clicks and the hitbox use
                // `frame_bounds`); the text is painted and hit-tested shifted
                // left by the horizontal pan.
                let frame_bounds = bounds;
                let bounds = Bounds::new(
                    point(bounds.origin.x - scroll_x, bounds.origin.y),
                    bounds.size,
                );
                // Buffer column -> x, past any inlay hints in front of it:
                // `xs` for a character's left edge (caret, range start),
                // `xe` for a range end, which stops before a hint there.
                let xs = |col: usize| shaped.x_for_index(col.min(line_len)) + inlays.shift(col, true);
                let xe = |col: usize| shaped.x_for_index(col.min(line_len)) + inlays.shift(col, false);
                let text_w = shaped.width + inlays.total();
                // 1. Indent guides, at the shaped x of their column inside the
                //    leading whitespace (and at whole cells past a short or
                //    blank line), full height so they join up between lines.
                let cell = (char_width as f32).max(1.);
                for &(column, active) in &guide_columns {
                    let x = if column <= leading_spaces {
                        xs(column)
                    } else {
                        px(column as f32 * cell)
                    };
                    let rect = Bounds::new(
                        point(bounds.origin.x + x, bounds.origin.y),
                        size(px(1.), bounds.size.height),
                    );
                    window.paint_quad(fill(
                        rect,
                        if active { guide_active_color } else { guide_color },
                    ));
                }

                // 2. Word-occurrence, bracket, & search-match highlight pills
                let box_h = px(LINE_FONT_SIZE + 4.);
                let box_y = bounds.origin.y + (bounds.size.height - box_h) * 0.5;
                for range in &local_word_matches {
                    let x1 = xs(range.start);
                    let x2 = xe(range.end);
                    if x2 > x1 {
                        let rect = Bounds::new(
                            point(bounds.origin.x + x1, box_y),
                            size(x2 - x1, box_h),
                        );
                        window.paint_quad(fill(rect, word_match_bg).corner_radii(px(3.)));
                    }
                }
                for (range, is_active_match) in &local_search_matches {
                    let x1 = xs(range.start);
                    let x2 = xe(range.end);
                    if x2 > x1 {
                        let rect = Bounds::new(
                            point(bounds.origin.x + x1, box_y),
                            size(x2 - x1, box_h),
                        );
                        if *is_active_match {
                            window.paint_quad(gpui_kit::quad(
                                rect,
                                px(3.),
                                active_search_bg,
                                Edges::all(px(1.)),
                                active_search_border,
                                BorderStyle::Solid,
                            ));
                        } else {
                            window.paint_quad(fill(rect, search_match_bg).corner_radii(px(3.)));
                        }
                    }
                }

                // 3a. Secondary (multi-cursor) selections
                for sel in &extra_sels {
                    let x1 = xs(sel.start);
                    let x2 = xe(sel.end);
                    if x2 > x1 {
                        let rect = Bounds::new(
                            point(bounds.origin.x + x1, bounds.origin.y + px(1.)),
                            size(x2 - x1, bounds.size.height - px(2.)),
                        );
                        window.paint_quad(fill(rect, selection_bg).corner_radii(px(2.)));
                    }
                }

                // 3. Selection highlight
                if let Some(sel) = &local_selection {
                    let x1 = xs(sel.start);
                    let x2 = xe(sel.end);
                    if x2 > x1 {
                        let rect = Bounds::new(
                            point(bounds.origin.x + x1, bounds.origin.y + px(1.)),
                            size(x2 - x1, bounds.size.height - px(2.)),
                        );
                        window.paint_quad(fill(rect, selection_bg).corner_radii(px(2.)));
                    }
                }

                // 3b. Token-level diagnostic underline & subtle background tint
                if let Some((diag_color, col_range)) = &diag_inline_spec {
                    let start_idx = col_range.start.clamp(leading_spaces, line_len);
                    let end_idx = col_range.end.clamp(start_idx, line_len);
                    let x1 = xs(start_idx);
                    let mut x2 = xe(end_idx);
                    if x2 <= x1 {
                        x2 = (x1 + px(14.)).min(text_w.max(x1 + px(6.)));
                    }
                    let token_bg = Bounds::new(
                        point(bounds.origin.x + x1, box_y),
                        size(x2 - x1, box_h),
                    );
                    window.paint_quad(fill(token_bg, diag_color.opacity(0.10)).corner_radii(px(2.)));
                    let underline_rect = Bounds::new(
                        point(bounds.origin.x + x1, box_y + box_h - px(1.5)),
                        size(x2 - x1, px(1.5)),
                    );
                    window.paint_quad(
                        fill(underline_rect, diag_color.opacity(0.85)).corner_radii(px(1.)),
                    );
                }

                // 4. Paint syntax-highlighted text & optional closing label (`// WidgetName`)
                let text_y = bounds.origin.y + (bounds.size.height - px(LINE_HEIGHT)) * 0.5;
                if shaped_inlays.is_empty() {
                    let _ = shaped.paint(
                        point(bounds.origin.x, text_y),
                        px(LINE_HEIGHT),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                } else {
                    // Paint the one shaped line once per gap between hints,
                    // each copy shifted right and clipped to its segment.
                    let mut cuts: Vec<usize> = shaped_inlays.iter().map(|(col, _, _)| *col).collect();
                    cuts.dedup();
                    cuts.push(line_len);
                    let mut seg_start = 0usize;
                    for (i, &seg_end) in cuts.iter().enumerate() {
                        if seg_end > seg_start {
                            let shift = inlays.shift(seg_start, true);
                            let left = if seg_start == 0 {
                                px(0.)
                            } else {
                                shaped.x_for_index(seg_start) + shift
                            };
                            let right = if i + 1 == cuts.len() {
                                bounds.size.width.max(text_w)
                            } else {
                                shaped.x_for_index(seg_end) + shift
                            };
                            let clip = Bounds::new(
                                point(bounds.origin.x + left, bounds.origin.y),
                                size(right - left, bounds.size.height),
                            );
                            window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
                                let _ = shaped.paint(
                                    point(bounds.origin.x + shift, text_y),
                                    px(LINE_HEIGHT),
                                    TextAlign::Left,
                                    None,
                                    window,
                                    cx,
                                );
                            });
                        }
                        seg_start = seg_end;
                    }
                    for (i, (col, shaped_hint, left_pad)) in shaped_inlays.iter().enumerate() {
                        // Sum by index, not column, so hints sharing a column sit side by side.
                        let before: Pixels = inlays.items[..i].iter().map(|(_, advance)| *advance).sum();
                        let x = bounds.origin.x + shaped.x_for_index(*col) + before + *left_pad;
                        let w = shaped_hint.width + INLAY_INNER * 2.;
                        let box_h = px(LINE_FONT_SIZE + 2.);
                        let pill = Bounds::new(
                            point(x, bounds.origin.y + (bounds.size.height - box_h) * 0.5),
                            size(w, box_h),
                        );
                        window.paint_quad(fill(pill, inlay_bg).corner_radii(px(3.)));
                        let _ = shaped_hint.paint(
                            point(x + INLAY_INNER, text_y),
                            px(LINE_HEIGHT),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                }
                if let Some(closing) = &shaped_closing {
                    let closing_x = text_w + px(12.);
                    let _ = closing.paint(
                        point(bounds.origin.x + closing_x, text_y),
                        px(LINE_HEIGHT),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }

                // 5. Paint caret on top of text at the exact shaped glyph boundary
                if let Some(col) = cursor_col {
                    let x = xs(col);
                    let caret_h = px(LINE_FONT_SIZE + 3.);
                    let y = bounds.origin.y + (bounds.size.height - caret_h) * 0.5;
                    let caret_rect =
                        Bounds::new(point(bounds.origin.x + x, y), size(px(1.5), caret_h));
                    window.paint_quad(fill(caret_rect, caret_color).corner_radii(px(0.75)));
                }

                for col in &extra_carets {
                    let x = xs(*col);
                    let caret_h = px(LINE_FONT_SIZE + 3.);
                    let y = bounds.origin.y + (bounds.size.height - caret_h) * 0.5;
                    let caret_rect =
                        Bounds::new(point(bounds.origin.x + x, y), size(px(1.5), caret_h));
                    window.paint_quad(fill(caret_rect, caret_color).corner_radii(px(0.75)));
                }

                // 6. Mouse interactions using the exact same `shaped` line, gated by `hitbox.is_hovered(window)`
                // so any `.occlude()` modal, popover, or menu above the editor blocks background clicks/hovers.
                let down_shaped = shaped.clone();
                let down_inlays = inlays.clone();
                let down_view = view.clone();
                let down_hitbox = hitbox.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble
                        || !matches!(event.button, MouseButton::Left | MouseButton::Right)
                        || !frame_bounds.contains(&event.position)
                        || !down_hitbox.is_hovered(window)
                    {
                        return;
                    }
                    let is_right_click = event.button == MouseButton::Right;
                    let rel_x = (event.position.x - bounds.origin.x).max(px(0.));
                    let column = down_inlays.column_for_x(
                        rel_x,
                        |i| down_shaped.x_for_index(i),
                        |x| down_shaped.closest_index_for_x(x),
                    );
                    let go_to_definition = event.modifiers.platform || event.modifiers.control;
                    let click_count = event.click_count;
                    down_view.update(cx, |this, cx| {
                        window.focus(&this.focus_handle, cx);
                        this.clear_hover(cx);
                        this.dismiss_completions(cx);
                        if is_right_click {
                            // As in VS Code: right-clicking outside the selection
                            // moves the cursor there, so the menu acts on what was
                            // clicked; inside the selection it leaves it alone.
                            let offset = line_start + column;
                            let inside = this
                                .buffer
                                .selections()
                                .as_slice()
                                .iter()
                                .any(|s| !s.is_empty() && s.range().contains(&offset));
                            if !inside {
                                this.set_cursor(offset, cx);
                            }
                        } else if go_to_definition {
                            this.request_definition(index, column);
                        } else if click_count >= 3 {
                            this.select_line(index, cx);
                        } else if click_count == 2 {
                            this.select_word_on_line(index, column, cx);
                        } else if event.modifiers.shift {
                            this.extend_to(line_start + column, cx);
                        } else {
                            this.set_cursor(line_start + column, cx);
                        }
                    });
                });

                let move_shaped = shaped.clone();
                let move_inlays = inlays.clone();
                let move_view = view.clone();
                let move_hitbox = hitbox.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble
                        || event.pressed_button != Some(MouseButton::Left)
                        || !frame_bounds.contains(&event.position)
                        || !move_hitbox.is_hovered(window)
                    {
                        return;
                    }
                    let rel_x = (event.position.x - bounds.origin.x).max(px(0.));
                    let column = move_inlays.column_for_x(
                        rel_x,
                        |i| move_shaped.x_for_index(i),
                        |x| move_shaped.closest_index_for_x(x),
                    );
                    move_view.update(cx, |this, cx| {
                        // Dragging the horizontal scrollbar pans the code; it
                        // must not also drag a text selection under it.
                        if this.h_drag.is_none() {
                            this.extend_to(line_start + column, cx);
                        }
                    });
                });

                // Horizontal gestures pan the text and are stopped here so the
                // vertical list does not also scroll under them. Vertical
                // gestures fall through to the list as before.
                let scroll_view = view.clone();
                let scroll_hitbox = hitbox.clone();
                window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture
                        || !frame_bounds.contains(&event.position)
                        || !scroll_hitbox.is_hovered(window)
                    {
                        return;
                    }
                    let (dx, dy) = match event.delta {
                        ScrollDelta::Pixels(p) => (p.x.to_f64(), p.y.to_f64()),
                        ScrollDelta::Lines(p) => {
                            (p.x as f64 * 24.0, p.y as f64 * 24.0)
                        }
                    };
                    // Shift turns a horizontal wheel; otherwise only a
                    // mostly-horizontal gesture pans, so a vertical scroll
                    // with a little sideways drift does not move the code.
                    let delta = if event.modifiers.shift && dy != 0.0 {
                        dy
                    } else if dx != 0.0 && dx.abs() > dy.abs() {
                        dx
                    } else {
                        return;
                    };
                    scroll_view.update(cx, |this, cx| {
                        this.clear_hover(cx);
                        this.pan_horizontally(delta);
                        cx.notify();
                    });
                    cx.stop_propagation();
                });

                let hover_shaped = shaped.clone();
                let hover_inlays = inlays.clone();
                let hover_view = view;
                let hover_hitbox = hitbox;
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble
                        || event.pressed_button.is_some()
                        || !frame_bounds.contains(&event.position)
                        || !hover_hitbox.is_hovered(window)
                    {
                        return;
                    }
                    let rel_x = (event.position.x - bounds.origin.x).max(px(0.));
                    let column = hover_inlays.column_for_x(
                        rel_x,
                        |i| hover_shaped.x_for_index(i),
                        |x| hover_shaped.closest_index_for_x(x),
                    );
                    hover_view.update(cx, |this, cx| {
                        this.note_hover_position(index, column, cx);
                    });
                });
            },
        )
        .size_full();

        div()
            .id(("editor-line", index))
            .group(row_group.clone())
            // Lets UI tests find a row; compiled out of normal builds.
            .debug_selector(|| format!("editor-line-{index}"))
            .relative()
            .flex()
            .w_full()
            .h(px(LINE_HEIGHT))
            .flex_shrink_0()
            .font_family(mono_family.clone())
            .text_size(px(LINE_FONT_SIZE))
            // The active line gets a full-width, flat wash of colour —
            // subtle enough to read as a highlight, not a second panel.
            // That's the whole cue; the gutter number brightening to match
            // is enough to say "and here's where", without a second marker
            // competing for the same job.
            .when(is_active_line && !is_debug_line, |el| el.bg(rgb(ACTIVE_LINE_BG)))
            .when(is_debug_line, |el| el.bg(cx.theme().warning.opacity(0.16)))
            .when_some(change_marker, |el, marker| el.child(marker))
            .when_some(active_right_marker, |el, marker| el.child(marker))
            .child(gutter)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .h_full()
                    .overflow_hidden()
                    .child(div().relative().h_full().w_full().child(line_canvas))
                    .when_some(
                        test_target.clone().and_then(|(is_whole_file, test_name)| {
                            self.buffer
                                .path()
                                .map(|p| (p.to_path_buf(), is_whole_file, test_name))
                        }),
                        |el, (file_path, is_whole_file, test_name)| {
                            // Quiet text after the code: "▶ Run" in a muted
                            // tone, the extra launch.json actions only while
                            // the line is hovered or holds the caret. Colour
                            // comes on hover, so the code stays the loudest
                            // thing on the line.
                            let green = cx.theme().green;
                            let muted = cx.theme().muted_foreground;
                            let mut actions: Vec<(String, Vec<String>)> = vec![(
                                if is_whole_file { "Run file" } else { "Run" }.to_string(),
                                Vec::new(),
                            )];
                            for tpl in &self.test_code_lenses {
                                if (is_whole_file && tpl.for_file) || (!is_whole_file && tpl.for_test) {
                                    actions.push((tpl.title.clone(), tpl.args.clone()));
                                }
                            }
                            let view = cx.entity();
                            let lens_left = px(lens_left_after_text(
                                display_columns(display_text),
                                char_width,
                                scroll_x.to_f64(),
                            ) as f32 + 4.);
                            let row_group = row_group.clone();
                            el.child(
                                h_flex()
                                    .debug_selector(|| format!("test-lens-row-{index}"))
                                    .absolute()
                                    .left(lens_left)
                                    .top_0()
                                    .bottom_0()
                                    .items_center()
                                    .gap(px(2.))
                                    .font_family(cx.theme().font_family.clone())
                                    .text_size(px(11.))
                                    .children(actions.into_iter().enumerate().map(
                                        |(btn_ix, (label, extra_args))| {
                                            let view = view.clone();
                                            let path = file_path.clone();
                                            let t_name = test_name.clone();
                                            let lbl = label.clone();
                                            let primary = btn_ix == 0;
                                            h_flex()
                                                .id(("test-lens", index * 16 + btn_ix))
                                                .h(px(17.))
                                                .px(px(5.))
                                                .gap(px(4.))
                                                .items_center()
                                                .rounded(px(4.))
                                                .cursor_pointer()
                                                .text_color(muted.opacity(0.75))
                                                .hover(move |s| {
                                                    s.bg(green.opacity(0.12)).text_color(green)
                                                })
                                                .when(!primary && !is_active_line, |b| {
                                                    b.invisible()
                                                        .group_hover(row_group.clone(), |s| s.visible())
                                                })
                                                .when(primary, |b| {
                                                    b.child(
                                                        gpui_kit::component::Icon::new(
                                                            gpui_kit::assets::IconName::Play,
                                                        )
                                                        .with_size(px(9.)),
                                                    )
                                                })
                                                .when(!primary, |b| {
                                                    b.child(div().text_color(muted.opacity(0.35)).child("\u{b7}"))
                                                })
                                                .child(label)
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    move |_event, _window, cx| {
                                                        cx.stop_propagation();
                                                        let path = path.clone();
                                                        let t_name = t_name.clone();
                                                        let extra_args = extra_args.clone();
                                                        let lbl = lbl.clone();
                                                        view.update(cx, |_this, cx| {
                                                            cx.emit(EditorEvent::RunTest {
                                                                path,
                                                                test_name: t_name,
                                                                extra_args,
                                                                label: lbl,
                                                            });
                                                        });
                                                    },
                                                )
                                        },
                                    )),
                            )
                        },
                    )
                    .when(
                        is_active_line && !self.completions.is_empty(),
                        |el| {
                            let left_offset = popup_left(cursor_col.unwrap_or(0), 340.);
                            let popup = self.render_completion_popup(&mono_family, cx);
                            el.child(
                                deferred(
                                    div()
                                        .occlude()
                                        .absolute()
                                        .top(px(LINE_HEIGHT + 2.))
                                        .left(left_offset)
                                        .w(px(340.))
                                        .child(popup),
                                )
                                .with_priority(20),
                            )
                        },
                    )
                    .when(
                        is_active_line && self.code_action_menu.is_some(),
                        |el| {
                            let left_offset = popup_left(cursor_col.unwrap_or(0), 360.);
                            let menu = self.render_code_action_menu(cx);
                            el.child(
                                deferred(
                                    div()
                                        .debug_selector(|| "code-action-menu".into())
                                        .occlude()
                                        .absolute()
                                        .top(px(LINE_HEIGHT + 2.))
                                        .left(left_offset)
                                        .w(px(360.))
                                        .child(menu),
                                )
                                .with_priority(21),
                            )
                        },
                    )
                    .when_some(
                        self.signature_help.as_ref().filter(|sig| sig.line == index),
                        |el, sig| {
                            let left_offset = popup_left(sig.column, 420.);
                            let card = Self::render_signature_card(sig, &mono_family, cx);
                            el.child(
                                deferred(
                                    div()
                                        .occlude()
                                        .absolute()
                                        .bottom(px(LINE_HEIGHT + 2.))
                                        .left(left_offset)
                                        .child(card),
                                )
                                .with_priority(12),
                            )
                        },
                    )
                    .when_some(
                        self.hover_to_render().filter(|(line, _)| {
                            *line == index && self.completions.is_empty() && window.is_window_active()
                        }),
                        |el, (_, text)| {
                            let hover_col = self
                                .hover_point
                                .filter(|(l, _)| *l == index)
                                .map(|(_, c)| c)
                                .unwrap_or(0);
                            let left_offset = popup_left(hover_col, 420.);
                            let card = Self::render_hover_card(text, &mono_family, cx);
                            el.child(
                                deferred(
                                    div()
                                        .occlude()
                                        .absolute()
                                        .top(px(LINE_HEIGHT + 2.))
                                        .left(left_offset)
                                        .w(px(420.))
                                        .child(card),
                                )
                                .with_priority(10),
                            )
                        },
                    ),
            )
    }

    /// A one-line popover showing the active signature, with the current
    /// parameter in bold accent color.
    fn render_signature_card(
        sig: &lsp_integration::SignatureView,
        mono_family: &SharedString,
        cx: &Context<Self>,
    ) -> Div {
        let (before, active, after) = match &sig.active {
            Some(r) if r.end <= sig.label.len() && sig.label.is_char_boundary(r.start) && sig.label.is_char_boundary(r.end) => (
                sig.label[..r.start].to_string(),
                sig.label[r.clone()].to_string(),
                sig.label[r.end..].to_string(),
            ),
            _ => (sig.label.clone(), String::new(), String::new()),
        };
        h_flex()
            .px_2()
            .py_1()
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(4.))
            .shadow_md()
            .font_family(mono_family.clone())
            .text_size(px(12.))
            .text_color(cx.theme().muted_foreground)
            .child(before)
            .child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .text_color(cx.theme().foreground)
                    .child(active),
            )
            .child(after)
    }

    /// Format an LSP markdown hover payload (` ```dart ... ``` `, `Type: ...`,
    /// `---`, doc prose) into a structured, opaque popover card with syntax-
    /// highlighted code signatures and clean inline code pills.
    fn render_hover_card(
        raw: &str,
        mono_family: &SharedString,
        cx: &Context<Self>,
    ) -> Div {
        let mut code_blocks: Vec<String> = Vec::new();
        let mut sections: Vec<Vec<String>> = vec![Vec::new()];
        let mut in_fence = false;
        let mut current_code = Vec::new();

        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("```") {
                if in_fence {
                    let block = current_code.join("\n");
                    if !block.trim().is_empty() {
                        code_blocks.push(block);
                    }
                    current_code.clear();
                    in_fence = false;
                } else {
                    in_fence = true;
                }
                continue;
            }
            if in_fence {
                current_code.push(line.to_string());
            } else if trimmed == "---" || trimmed == "***" {
                sections.push(Vec::new());
            } else if let Some(last) = sections.last_mut() {
                last.push(line.to_string());
            }
        }

        let fg = cx.theme().popover_foreground;
        let muted = cx.theme().muted_foreground;
        let cyan = cx.theme().cyan;
        let border = cx.theme().border;
        let code_bg = cx.theme().background;
        let ui_family = cx.theme().font_family.clone();

        let mut card = v_flex()
            .w_full()
            .max_h(px(200.))
            .overflow_hidden()
            .p_2p5()
            .gap_1p5()
            .rounded(px(8.))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(border)
            .shadow_xl()
            .font_family(ui_family)
            .text_size(px(11.5))
            .text_color(fg);

        // Render only the primary signature block (first fenced block), capped at 5 lines
        if let Some(first_block) = code_blocks.into_iter().next() {
            let all_sig_lines: Vec<&str> = first_block.lines().collect();
            let capped_sig = if all_sig_lines.len() > 5 {
                let mut head = all_sig_lines[..5].join("\n");
                head.push_str("\n  \u{2026}");
                head
            } else {
                first_block
            };
            // Fence info strings say which grammar the server meant.
            let grammar = if raw.contains("```python") || raw.contains("```py\n") {
                SyntaxLanguage::Python
            } else if raw.contains("```rust") || raw.contains("```rs\n") {
                SyntaxLanguage::Rust
            } else if raw.contains("```tsx") {
                SyntaxLanguage::Tsx
            } else if raw.contains("```typescript") || raw.contains("```ts\n") {
                SyntaxLanguage::TypeScript
            } else if raw.contains("```javascript") || raw.contains("```js\n") || raw.contains("```jsx") {
                SyntaxLanguage::JavaScript
            } else {
                SyntaxLanguage::Dart
            };
            let tree = SyntaxTree::parse_as(grammar, &capped_sig);
            let highlights = tree.highlights(&capped_sig);
            let mut block_el = v_flex()
                .w_full()
                .px_2p5()
                .py_1p5()
                .gap_0p5()
                .rounded(px(6.))
                .bg(code_bg)
                .border_1()
                .border_color(border)
                .font_family(mono_family.clone())
                .text_size(px(11.5));

            let mut byte_offset = 0;
            for code_line in capped_sig.split('\n') {
                let runs = Self::line_runs(byte_offset, code_line, &highlights);
                let mut row = h_flex().w_full().flex_wrap().items_center();
                for (range, kind) in runs {
                    if range.is_empty() {
                        continue;
                    }
                    let slice = code_line[range].to_string();
                    let color: Hsla = kind.map(|k| highlight_color(k).into()).unwrap_or(fg);
                    row = row.child(div().text_color(color).child(slice));
                }
                block_el = block_el.child(row);
                byte_offset += code_line.len() + 1;
            }
            card = card.child(block_el);
        }

        // Collect clean summary lines (skipping Dartdoc `{@template}`, `{@macro}`, and `See also:` tails),
        // capped strictly at 4 lines so the hover card stays compact and glanceable.
        let mut prose_lines: Vec<String> = Vec::new();
        'outer: for sec in sections {
            for raw_l in sec {
                let t = raw_l.trim();
                if t.is_empty()
                    || t.starts_with("{@")
                    || t.starts_with("///")
                {
                    continue;
                }
                if t.starts_with("See also:") || t.starts_with("### ") || t.starts_with("## ") {
                    break 'outer;
                }
                let clean = Self::clean_hover_markdown_line(t);
                if !clean.is_empty() {
                    prose_lines.push(clean);
                    if prose_lines.len() >= 4 {
                        break 'outer;
                    }
                }
            }
        }

        if !prose_lines.is_empty() {
            let mut sec_col = v_flex().w_full().gap_0p5();
            for line in prose_lines {
                let is_meta = line.starts_with("Type:") || line.starts_with("Declared in");
                sec_col = sec_col.child(
                    div()
                        .w_full()
                        .truncate()
                        .text_size(px(if is_meta { 11. } else { 11.5 }))
                        .text_color(if is_meta { muted } else { fg })
                        .child(line),
                );
            }
            let _ = cyan;
            card = card.child(sec_col);
        }

        card
    }

    fn clean_hover_markdown_line(line: &str) -> String {
        let mut out = String::with_capacity(line.len());
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '`' || chars[i] == '*' {
                i += 1;
                continue;
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }

    /// Show the code-action menu under the cursor (empty list shows a
    /// short "nothing here" note rather than silently doing nothing).
    pub(crate) fn open_code_action_menu(&mut self, items: Vec<code_actions::ActionItem>, cx: &mut Context<Self>) {
        self.dismiss_completions(cx);
        self.clear_hover(cx);
        self.code_action_menu = Some(code_actions::CodeActionMenu { items, selected: 0 });
        cx.notify();
    }

    fn apply_code_action_from_menu(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(menu) = self.code_action_menu.take() else {
            return;
        };
        if let Some(item) = menu.items.into_iter().nth(ix) {
            self.apply_code_action(item.action, cx);
        }
        cx.notify();
    }

    fn render_code_action_menu(&self, cx: &mut Context<Self>) -> Div {
        let view = cx.entity();
        let fg = cx.theme().popover_foreground;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let primary = cx.theme().primary;
        let ui_family = cx.theme().font_family.clone();
        let Some(menu) = &self.code_action_menu else {
            return div();
        };

        let mut rows: Vec<AnyElement> = Vec::new();
        if menu.items.is_empty() {
            rows.push(
                div()
                    .px_2()
                    .py_1()
                    .text_size(px(12.))
                    .text_color(muted)
                    .child("No code actions here")
                    .into_any_element(),
            );
        }
        let mut last_group = None;
        for (ix, item) in menu.items.iter().enumerate() {
            if last_group != Some(item.group) {
                last_group = Some(item.group);
                rows.push(
                    div()
                        .px_2()
                        .pt(px(if ix == 0 { 2. } else { 6. }))
                        .pb(px(2.))
                        .text_size(px(10.))
                        .text_color(muted.opacity(0.8))
                        .child(item.group.label().to_uppercase())
                        .into_any_element(),
                );
            }
            let is_sel = ix == menu.selected;
            let icon_color: Hsla = match item.group {
                code_actions::ActionGroup::QuickFix => cx.theme().warning,
                code_actions::ActionGroup::Refactor => cx.theme().info,
                _ => muted,
            };
            let view_click = view.clone();
            let view_hover = view.clone();
            rows.push(
                h_flex()
                    .id(("code-action-item", ix))
                    .relative()
                    .w_full()
                    .h(px(24.))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .rounded(px(5.))
                    .when(is_sel, |el| {
                        el.bg(primary.opacity(0.14)).child(
                            div()
                                .absolute()
                                .left(px(1.5))
                                .top(px(5.))
                                .bottom(px(5.))
                                .w(px(2.))
                                .rounded(px(1.))
                                .bg(primary),
                        )
                    })
                    .cursor_pointer()
                    .on_hover(move |hovered, _window, cx| {
                        if *hovered {
                            view_hover.update(cx, |this, cx| {
                                if let Some(menu) = &mut this.code_action_menu {
                                    if menu.selected != ix {
                                        menu.selected = ix;
                                        cx.notify();
                                    }
                                }
                            });
                        }
                    })
                    .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                        cx.stop_propagation();
                        view_click.update(cx, |this, cx| this.apply_code_action_from_menu(ix, cx));
                    })
                    .child(
                        gpui_kit::component::Icon::new(match item.group {
                            code_actions::ActionGroup::QuickFix => gpui_kit::assets::IconName::Lightbulb,
                            _ => gpui_kit::assets::IconName::ChevronRight,
                        })
                        .with_size(px(11.))
                        .text_color(icon_color),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(12.5))
                            .text_color(fg)
                            .child(item.title.clone()),
                    )
                    .when(item.preferred, |el| {
                        el.child(
                            div()
                                .text_size(px(10.))
                                .text_color(cx.theme().warning.opacity(0.9))
                                .child("preferred"),
                        )
                    })
                    .into_any_element(),
            );
        }

        v_flex()
            .w_full()
            .max_h(px(320.))
            .p_1()
            .rounded(px(8.))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(border)
            .shadow_xl()
            .font_family(ui_family)
            .children(rows)
            .child(
                div()
                    .w_full()
                    .mt_1()
                    .pt_1()
                    .px_2()
                    .border_t_1()
                    .border_color(border.opacity(0.6))
                    .text_size(px(10.))
                    .text_color(muted)
                    .child("\u{2191}\u{2193} select   \u{21b5} apply   esc close"),
            )
    }

    fn render_completion_popup(
        &self,
        mono_family: &SharedString,
        cx: &mut Context<Self>,
    ) -> Div {
        let view = cx.entity();
        let total = self.completions.len();
        let selected = self.completion_selected.min(total.saturating_sub(1));
        let window_size = 8usize;
        let start_ix = if selected >= window_size {
            selected + 1 - window_size
        } else {
            0
        };
        let end_ix = (start_ix + window_size).min(total);

        let fg = cx.theme().popover_foreground;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let primary = cx.theme().primary;

        let rows: Vec<AnyElement> = (start_ix..end_ix)
            .map(|ix| {
                let entry = &self.completions[ix];
                let is_sel = ix == selected;
                let (badge_text, badge_color) = match entry.kind {
                    CompletionKindBadge::Function => ("fn", rgb(0x61afef)),
                    CompletionKindBadge::Property => ("pr", rgb(0xe06c75)),
                    CompletionKindBadge::Type => ("ty", rgb(0xe5c07b)),
                    CompletionKindBadge::Variable => ("vr", rgb(0x56b6c2)),
                    CompletionKindBadge::Keyword => ("kw", rgb(0xc678dd)),
                    CompletionKindBadge::Constant => ("ct", rgb(0xd19a66)),
                };
                let badge_hsla: Hsla = badge_color.into();
                let view_click = view.clone();

                h_flex()
                    .id(("completion-item", ix))
                    .relative()
                    .w_full()
                    .h(px(24.))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .justify_between()
                    .rounded(px(5.))
                    .border_1()
                    .border_color(if is_sel {
                        rgba(0xea7a3c40)
                    } else {
                        rgba(0x00000000)
                    })
                    .when(is_sel, |el| {
                        el.bg(rgba(0xea7a3c1a)).child(
                            div()
                                .absolute()
                                .left(px(1.5))
                                .top(px(4.))
                                .bottom(px(4.))
                                .w(px(2.))
                                .rounded(px(1.))
                                .bg(primary),
                        )
                    })
                    .when(!is_sel, |el| el.hover(|s| s.bg(rgba(0xffffff08))))
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                        cx.stop_propagation();
                        view_click.update(cx, |this, cx| {
                            this.accept_completion(ix, cx);
                        });
                    })
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .w(px(18.))
                                    .h(px(15.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(3.))
                                    .bg(badge_hsla.opacity(0.16))
                                    .font_family(mono_family.clone())
                                    .text_size(px(9.5))
                                    .text_color(badge_hsla)
                                    .child(badge_text),
                            )
                            .child(
                                div()
                                    .font_family(mono_family.clone())
                                    .text_size(px(12.))
                                    .text_color(fg)
                                    . truncate()
                                    .child(entry.label.clone()),
                            ),
                    )
                    .when_some(entry.detail.clone(), |el, detail| {
                        el.child(
                            div()
                                .max_w(px(120.))
                                .truncate()
                                .font_family(mono_family.clone())
                                .text_size(px(10.5))
                                .text_color(muted)
                                .child(detail),
                        )
                    })
                    .into_any_element()
            })
            .collect();

        v_flex()
            .w_full()
            .p_1()
            .gap_0p5()
            .rounded(px(8.))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(border)
            .shadow_xl()
            .children(rows)
            .child(
                h_flex()
                    .w_full()
                    .pt_1()
                    .px_2()
                    .border_t_1()
                    .border_color(border.opacity(0.6))
                    .items_center()
                    .justify_between()
                    .font_family(cx.theme().font_family.clone())
                    .text_size(px(10.))
                    .text_color(muted)
                    .child("\u{2191}\u{2193} select   \u{21e5}/\u{21b5} accept   esc close")
                    .child(format!("{}/{}", selected + 1, total)),
            )
    }
}

/// Extract document symbols `(symbol_name, kind_badge, 0_indexed_line)` from source text.
pub fn extract_document_symbols(text: &str) -> Vec<(String, &'static str, u32)> {
    let mut out = Vec::new();
    const PREFIXES: &[(&str, &str)] = &[
        ("class ", "class"),
        ("abstract class ", "class"),
        ("sealed class ", "class"),
        ("mixin ", "mixin"),
        ("extension ", "ext"),
        ("enum ", "enum"),
        ("typedef ", "type"),
        ("pub struct ", "struct"),
        ("struct ", "struct"),
        ("pub enum ", "enum"),
        ("pub trait ", "trait"),
        ("trait ", "trait"),
        ("impl ", "impl"),
        ("pub fn ", "fn"),
        ("pub(crate) fn ", "fn"),
        ("pub async fn ", "fn"),
        ("async fn ", "fn"),
        ("fn ", "fn"),
        ("func ", "func"),
        ("def ", "def"),
        ("async def ", "def"),
        ("export interface ", "iface"),
        ("interface ", "iface"),
        ("export type ", "type"),
        ("export function ", "fn"),
        ("export async function ", "fn"),
        ("function ", "fn"),
        ("export class ", "class"),
    ];

    for (line_ix, raw_line) in text.lines().enumerate() {
        let trimmed = raw_line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with('#') {
            continue;
        }
        let mut matched = false;
        for (prefix, kind) in PREFIXES {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let sym: String = rest
                    .trim_start()
                    .chars()
                    .take_while(|c| {
                        c.is_alphanumeric() || *c == '_' || *c == '<' || *c == '>' || *c == ' '
                    })
                    .collect();
                let sym = sym
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
                if !sym.is_empty() {
                    out.push((sym.to_string(), *kind, line_ix as u32));
                    matched = true;
                    break;
                }
            }
        }
        if matched {
            continue;
        }
        if let Some(open_paren) = trimmed.find('(')
            && (trimmed.ends_with('{') || trimmed.ends_with(") =>") || trimmed.contains(") async"))
            && !trimmed.starts_with("if ")
            && !trimmed.starts_with("for ")
            && !trimmed.starts_with("while ")
            && !trimmed.starts_with("switch ")
            && !trimmed.starts_with("return ")
        {
            let before_paren = trimmed[..open_paren].trim();
            let mut words = before_paren.split_whitespace().rev();
            if let Some(name_tok) = words.next()
                && words.next().is_some()
            {
                let clean = name_tok.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
                if !clean.is_empty()
                    && clean
                        .bytes()
                        .next()
                        .is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
                {
                    out.push((clean.to_string(), "method", line_ix as u32));
                }
            }
        }
    }
    out
}

fn buffer_edit_to_input_edit(edit: &BufferEdit) -> InputEdit {
    InputEdit {
        start_byte: edit.start_byte,
        old_end_byte: edit.old_end_byte,
        new_end_byte: edit.new_end_byte,
        start_position: Point {
            row: edit.start_position.0,
            column: edit.start_position.1,
        },
        old_end_position: Point {
            row: edit.old_end_position.0,
            column: edit.old_end_position.1,
        },
        new_end_position: Point {
            row: edit.new_end_position.0,
            column: edit.new_end_position.1,
        },
    }
}

/// The smallest [`InputEdit`] that turns `old` into `new`, found by
/// trimming the common prefix and common suffix and treating whatever's
/// left in the middle as replaced. This doesn't know *which* edit
/// happened (a keystroke, an undo, a whole-file open) — it only needs the
/// before/after text, so every caller gets incremental reparsing for
/// free without threading edit details through every mutation path.
fn diff_as_edit(old: &str, new: &str) -> Option<InputEdit> {
    if old == new {
        return None;
    }

    let old_bytes = old.as_bytes();
    let new_bytes = new.as_bytes();

    let common_prefix = old_bytes
        .iter()
        .zip(new_bytes.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let max_suffix = (old_bytes.len() - common_prefix).min(new_bytes.len() - common_prefix);
    let common_suffix = (0..max_suffix)
        .take_while(|&i| old_bytes[old_bytes.len() - 1 - i] == new_bytes[new_bytes.len() - 1 - i])
        .count();

    let start_byte = common_prefix;
    let old_end_byte = old_bytes.len() - common_suffix;
    let new_end_byte = new_bytes.len() - common_suffix;

    Some(InputEdit {
        start_byte,
        old_end_byte,
        new_end_byte,
        start_position: point_at(old, start_byte),
        old_end_position: point_at(old, old_end_byte),
        new_end_position: point_at(new, new_end_byte),
    })
}

/// Whether `b` can be part of an identifier — ASCII alphanumerics and
/// underscore, plus (conservatively) every non-ASCII byte, so a run never
/// gets split in the middle of a multi-byte UTF-8 character.
/// Shape a line's inlay hint labels `(column, label, pad_left, pad_right)`
/// and measure how far each pushes the text after it. Returns each hint's
/// shaped label with its left padding, plus the layout used to shift x.
fn shape_inlays(
    window: &mut Window,
    hints: &[(usize, SharedString, bool, bool)],
    font: &Font,
    color: Hsla,
) -> (Vec<(usize, ShapedLine, Pixels)>, inlay_hints::InlayLayout) {
    let mut shaped = Vec::with_capacity(hints.len());
    let mut items = Vec::with_capacity(hints.len());
    for (col, label, pad_left, pad_right) in hints {
        let run = TextRun {
            len: label.len(),
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let shaped_hint = window.text_system().shape_line(label.clone(), px(LINE_FONT_SIZE - 1.), &[run], None);
        let left = if *pad_left { INLAY_PAD } else { px(0.) };
        let right = if *pad_right { INLAY_PAD } else { px(0.) };
        items.push((*col, left + shaped_hint.width + INLAY_INNER * 2. + right));
        shaped.push((*col, shaped_hint, left));
    }
    (shaped, inlay_hints::InlayLayout { items })
}

/// One character's width in editor cells. The face is monospace, so ASCII is
/// one cell; tabs are the editor's indent step, and full-width characters
/// are two so long CJK lines scroll by the right amount.
fn char_columns(ch: char) -> usize {
    if ch == '\t' {
        return 4;
    }
    if ch.is_ascii() {
        return 1;
    }
    let c = ch as u32;
    let wide = (0x1100..=0x115F).contains(&c)
        || (0x2E80..=0xA4CF).contains(&c)
        || (0xAC00..=0xD7A3).contains(&c)
        || (0xF900..=0xFAFF).contains(&c)
        || (0xFE30..=0xFE4F).contains(&c)
        || (0xFF00..=0xFF60).contains(&c)
        || (0xFFE0..=0xFFE6).contains(&c)
        || (0x1F300..=0x1FAFF).contains(&c);
    if wide { 2 } else { 1 }
}

fn display_columns(text: &str) -> usize {
    if text.is_ascii() && !text.as_bytes().contains(&b'\t') {
        text.len()
    } else {
        text.chars().map(char_columns).sum()
    }
}

/// Left edge of the inline test actions: just after the code, following the
/// horizontal pan, and never before the row's own start.
fn lens_left_after_text(columns: usize, char_width: f64, scroll_x: f64) -> f64 {
    (columns as f64 * char_width - scroll_x).max(0.0) + 8.0
}

/// Shorten `label` to `available` pixels, given the width it takes whole.
/// `None` when not even a few characters plus an ellipsis would fit.
fn fit_label(label: &str, full_width: f64, available: f64) -> Option<String> {
    if full_width <= available {
        return Some(label.to_string());
    }
    let chars = label.chars().count();
    if chars == 0 || full_width <= 0.0 || available <= 0.0 {
        return None;
    }
    let per_char = full_width / chars as f64;
    // Leave one character's worth for the ellipsis itself.
    let keep = ((available / per_char).floor() as usize).saturating_sub(1);
    if keep < 4 {
        return None;
    }
    let mut out: String = label.chars().take(keep).collect();
    out.push('\u{2026}');
    Some(out)
}

/// The pan that keeps `caret_x` visible in a `viewport`-wide column, without
/// moving it further than `max`. A cursor already on screen leaves the pan
/// alone, so scrolling by hand is not undone until the cursor moves.
fn reveal_offset(current: f64, caret_x: f64, viewport: f64, margin: f64, max: f64) -> f64 {
    if viewport <= 0.0 {
        return 0.0;
    }
    let max = max.max(0.0);
    let mut x = current.clamp(0.0, max);
    if caret_x + margin > x + viewport {
        x = caret_x + margin - viewport;
    }
    if caret_x - margin < x {
        x = caret_x - margin;
    }
    x.clamp(0.0, max)
}

/// The editor's list rows: the visible (unfolded) lines in order, plus
/// logical line -> row for scrolling.
fn display_rows(visible: &[usize], line_count: usize) -> (Vec<usize>, Vec<usize>) {
    let mut line_row = vec![0usize; line_count];
    for (row, &line) in visible.iter().enumerate() {
        if let Some(slot) = line_row.get_mut(line) {
            *slot = row;
        }
    }
    let rows = if visible.is_empty() { vec![0] } else { visible.to_vec() };
    (rows, line_row)
}

/// The display column of a byte offset in one line, rounded down to a
/// character boundary. Tabs count as four cells, as [`display_columns`] does.
fn display_column_at_byte(text: &str, byte: usize) -> usize {
    let mut byte = byte.min(text.len());
    while byte > 0 && !text.is_char_boundary(byte) {
        byte -= 1;
    }
    display_columns(&text[..byte])
}

/// The byte offset of a display column in one line. A column inside a wide
/// character or a tab lands on that character's start, and a column past the
/// end lands on the line's end, so a short line clamps without losing the
/// caller's goal column.
fn byte_at_display_column(text: &str, column: usize) -> usize {
    let mut used = 0;
    for (byte, ch) in text.char_indices() {
        let width = char_columns(ch);
        if used + width > column {
            return byte;
        }
        used += width;
    }
    text.len()
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Start of the word before `byte`, skipping punctuation and whitespace.
fn prev_word_boundary(text: &str, byte: usize) -> usize {
    let mut i = byte.min(text.len());
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    while i > 0 {
        let ch = text[..i].chars().next_back().expect("i > 0");
        if is_word_char(ch) {
            break;
        }
        i -= ch.len_utf8();
    }
    while i > 0 {
        let ch = text[..i].chars().next_back().expect("i > 0");
        if !is_word_char(ch) {
            break;
        }
        i -= ch.len_utf8();
    }
    i
}

/// End of the word after `byte`, skipping punctuation and whitespace.
fn next_word_boundary(text: &str, byte: usize) -> usize {
    let mut i = byte.min(text.len());
    while i < text.len() {
        let ch = text[i..].chars().next().expect("i < len");
        if is_word_char(ch) {
            break;
        }
        i += ch.len_utf8();
    }
    while i < text.len() {
        let ch = text[i..].chars().next().expect("i < len");
        if !is_word_char(ch) {
            break;
        }
        i += ch.len_utf8();
    }
    i
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Byte ranges of every non-overlapping occurrence of `query` in `text`.
/// Case-insensitive matching folds ASCII only, so offsets in the folded copy
/// stay identical to `text` (Unicode lowercasing can change byte lengths).
/// `whole_word` rejects matches with a word byte directly on either side.
fn find_matches(text: &str, query: &str, case_sensitive: bool, whole_word: bool) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let (hay, needle) = if case_sensitive {
        (std::borrow::Cow::Borrowed(text), std::borrow::Cow::Borrowed(query))
    } else {
        (
            std::borrow::Cow::Owned(text.to_ascii_lowercase()),
            std::borrow::Cow::Owned(query.to_ascii_lowercase()),
        )
    };
    let bytes = text.as_bytes();
    let mut matches = Vec::new();
    let mut from = 0;
    while let Some(pos) = hay[from..].find(needle.as_ref()) {
        let start = from + pos;
        let end = start + needle.len();
        let bounded = !whole_word
            || ((start == 0 || !is_word_byte(bytes[start - 1]))
                && (end == bytes.len() || !is_word_byte(bytes[end])));
        if bounded && text.is_char_boundary(start) && text.is_char_boundary(end) {
            matches.push(start..end);
            from = end;
        } else {
            // A rejected candidate may still overlap a valid one further on.
            from = start + hay[start..].chars().next().map_or(1, char::len_utf8);
        }
        if from >= hay.len() {
            break;
        }
    }
    matches
}

/// Zero-allocation ASCII case-insensitive prefix check (`needle_lower` must be lowercase).
fn starts_with_ascii_ci(haystack: &str, needle_lower: &str) -> bool {
    haystack.len() >= needle_lower.len()
        && haystack.as_bytes()[..needle_lower.len()]
            .iter()
            .zip(needle_lower.as_bytes())
            .all(|(h, n)| h.to_ascii_lowercase() == *n)
}

/// Zero-allocation ASCII case-insensitive substring check (`needle_lower` must be lowercase).
fn contains_ascii_ci(haystack: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    let h = haystack.as_bytes();
    let n = needle_lower.as_bytes();
    if h.len() < n.len() {
        return false;
    }
    h.windows(n.len())
        .any(|w| w.iter().zip(n).all(|(a, b)| a.to_ascii_lowercase() == *b))
}

/// A quiet, One Dark-derived palette: each kind gets its own hue family
/// (purple/gold/blue/cyan/red/green/orange) so two unrelated kinds never
/// compete for the same colour the way the previous pass's keyword and
/// constant (both orange) or property and variable (both blue) did. This
/// is the same family the `cutoff-studio` reference leans on for its own
/// `--syn-*` tokens — not copied line for line, since that's CSS for a
/// web layout and this is GPUI, but the same idea: keywords read as
/// purple, types/classes as gold, calls as blue, namespaces/attributes
/// as cyan, member access as a warm red, literals as green/orange, and
/// everything structural (operators, punctuation) recedes to a dim
/// blue-gray instead of competing with any of it.
/// Fast single-pass `{ ... }` fold range scanner for non-Dart languages.
/// Returns sorted, deduplicated `FoldRange` entries by `start_line`.
/// The tree-sitter grammar for a buffer, if `core-syntax` has one. Untitled
/// buffers without a language hint are Dart, matching the default language.
fn syntax_language(language_id: &str, path: Option<&Path>) -> Option<SyntaxLanguage> {
    if let Some(ext) = path.and_then(|p| p.extension()).and_then(|e| e.to_str()) {
        match ext.to_ascii_lowercase().as_str() {
            "tsx" => return Some(SyntaxLanguage::Tsx),
            "js" | "jsx" | "mjs" | "cjs" => return Some(SyntaxLanguage::JavaScript),
            "ts" | "mts" | "cts" => return Some(SyntaxLanguage::TypeScript),
            "rs" => return Some(SyntaxLanguage::Rust),
            _ => {}
        }
    }
    SyntaxLanguage::from_language_id(language_id)
        .or_else(|| path.is_none().then_some(SyntaxLanguage::Dart))
}

fn lexical_fold_ranges(source: &str) -> Vec<core_syntax::FoldRange> {
    let bytes = source.as_bytes();
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let mut ranges = Vec::new();
    let mut line = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' {
            line += 1;
            i += 1;
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b == b'"' || b == b'\'' {
            let quote = b;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'\n' {
                    line += 1;
                }
                if bytes[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if b == b'{' {
            stack.push((line, i));
        } else if b == b'}' {
            if let Some((start_line, start_byte)) = stack.pop() {
                if line > start_line {
                    ranges.push(core_syntax::FoldRange {
                        start_line,
                        end_line: line,
                        start_byte,
                        end_byte: i + 1,
                    });
                }
            }
        }
        i += 1;
    }
    ranges.sort_by_key(|r| (r.start_line, std::cmp::Reverse(r.end_line)));
    ranges.dedup_by_key(|r| r.start_line);
    ranges
}

/// Viewport-scoped lexical syntax highlighter for all non-Dart languages
/// (Rust, TypeScript/JS, Python, Go, Swift, Kotlin, C/C++, Java, Ruby, Zig,
/// C#, PHP, Lua, HTML/CSS, JSON, YAML, TOML, Shell, SQL).
fn lexical_highlights_in_range(
    source: &str,
    range: Range<usize>,
    keywords: &[&str],
) -> Vec<Highlight> {
    let start = range.start.min(source.len());
    let end = range.end.min(source.len()).max(start);
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = start;

    while i < end {
        let b = bytes[i];
        // Line comments: `//` or `#` (when at start of token)
        if (b == b'/' && i + 1 < end && bytes[i + 1] == b'/')
            || (b == b'#' && (i == start || bytes[i - 1].is_ascii_whitespace()))
        {
            let c_start = i;
            while i < end && bytes[i] != b'\n' {
                i += 1;
            }
            out.push(Highlight {
                range: c_start..i,
                kind: HighlightKind::Comment,
            });
            continue;
        }
        // Strings: `"..."` or `'...'`
        if b == b'"' || b == b'\'' {
            let quote = b;
            let s_start = i;
            i += 1;
            while i < end && bytes[i] != b'\n' {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(end);
                    continue;
                }
                if bytes[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push(Highlight {
                range: s_start..i,
                kind: HighlightKind::String,
            });
            continue;
        }
        // Attributes / decorators: `@foo` or `#[...]`
        if b == b'@' || (b == b'#' && i + 1 < end && bytes[i + 1] == b'[') {
            let a_start = i;
            i += 1;
            while i < end && (is_word_byte(bytes[i]) || bytes[i] == b'[' || bytes[i] == b']') {
                i += 1;
            }
            out.push(Highlight {
                range: a_start..i,
                kind: HighlightKind::Attribute,
            });
            continue;
        }
        // Numbers
        if b.is_ascii_digit() && (i == start || !is_word_byte(bytes[i - 1])) {
            let n_start = i;
            while i < end
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'_')
            {
                i += 1;
            }
            out.push(Highlight {
                range: n_start..i,
                kind: HighlightKind::Number,
            });
            continue;
        }
        // Identifiers / Keywords / Types / Function calls
        if b.is_ascii_alphabetic() || b == b'_' {
            let w_start = i;
            while i < end && is_word_byte(bytes[i]) {
                i += 1;
            }
            let word = &source[w_start..i];
            let kind = if keywords.contains(&word) {
                Some(HighlightKind::Keyword)
            } else if word.bytes().next().is_some_and(|c| c.is_ascii_uppercase()) {
                Some(HighlightKind::Type)
            } else if i < end && bytes[i] == b'(' {
                Some(HighlightKind::Function)
            } else if w_start > start && bytes[w_start - 1] == b'.' {
                Some(HighlightKind::Property)
            } else {
                None
            };
            if let Some(k) = kind {
                out.push(Highlight {
                    range: w_start..i,
                    kind: k,
                });
            }
            continue;
        }
        i += 1;
    }
    out
}

pub fn highlight_color(kind: HighlightKind) -> Rgba {
    match kind {
        HighlightKind::Keyword => rgb(0xc678dd),
        HighlightKind::Type => rgb(0xe5c07b),
        HighlightKind::BuiltinType => rgb(0x56b6c2),
        HighlightKind::Function => rgb(0x61afef),
        HighlightKind::Attribute => rgb(0x56b6c2),
        HighlightKind::Property => rgb(0xe06c75),
        HighlightKind::String => rgb(0x98c379),
        HighlightKind::Number | HighlightKind::Constant => rgb(0xd19a66),
        HighlightKind::Comment => rgb(0x6c7686),
        HighlightKind::Variable => rgb(0xc8ccd4),
        HighlightKind::Operator | HighlightKind::Punctuation | HighlightKind::Other => rgb(0x9aa3b2),
    }
}

#[cfg(test)]
mod tests {

    #[core::prelude::v1::test]
    fn normalizing_trims_trailing_whitespace_and_keeps_line_endings() {
        assert_eq!(normalize_for_save("a  \nb\t\n", true, false).as_deref(), Some("a\nb\n"));
        assert_eq!(normalize_for_save("a \r\nb \r\n", true, false).as_deref(), Some("a\r\nb\r\n"));
        assert_eq!(normalize_for_save("clean\n", true, true), None, "already clean");
        assert_eq!(normalize_for_save("  indented\n", true, false), None, "leading space is kept");
    }

    #[core::prelude::v1::test]
    fn normalizing_adds_one_final_newline_only_to_non_empty_files() {
        assert_eq!(normalize_for_save("a", false, true).as_deref(), Some("a\n"));
        assert_eq!(normalize_for_save("a\n", false, true), None);
        assert_eq!(normalize_for_save("", false, true), None, "an empty file stays empty");
        assert_eq!(normalize_for_save("a ", true, true).as_deref(), Some("a\n"));
        // Options off: nothing changes.
        assert_eq!(normalize_for_save("a  ", false, false), None);
    }

    #[core::prelude::v1::test]
    fn display_columns_round_trip_tabs_and_unicode() {
        let text = "a\tbé";
        assert_eq!(display_columns(text), 1 + 4 + 1 + 1);
        assert_eq!(display_column_at_byte(text, 2), 5);
        assert_eq!(byte_at_display_column(text, 5), 2);
        assert_eq!(byte_at_display_column(text, 6), 3);
        assert_eq!(byte_at_display_column("ab", 9), 2);
    }

    #[core::prelude::v1::test]
    fn a_short_line_keeps_the_vertical_goal_column() {
        let long = "abcdefghij";
        let goal = display_column_at_byte(long, 8);
        assert_eq!(goal, 8);
        assert_eq!(byte_at_display_column("x", goal), 1);
        assert_eq!(byte_at_display_column(long, goal), 8);
    }

    #[core::prelude::v1::test]
    fn word_boundaries_skip_punctuation_and_whitespace() {
        assert_eq!(next_word_boundary("foo.bar", 0), 3);
        assert_eq!(prev_word_boundary("foo.bar", 7), 4);
        assert_eq!(next_word_boundary("foo bar", 3), 7);
        assert_eq!(prev_word_boundary("foo bar", 7), 4);
        assert_eq!(prev_word_boundary("héllo world", 12), 7);
        assert_eq!(next_word_boundary("héllo world", 0), 6);
    }

    #[core::prelude::v1::test]
    fn word_selection_handles_unicode_identifiers() {
        assert_eq!(EditorView::word_at("héllo world", 1), Some("héllo"));
        assert_eq!(EditorView::word_at("héllo world", 5), Some("héllo"));
        assert_eq!(EditorView::word_at("héllo world", 7), Some("world"));
    }

    #[core::prelude::v1::test]
    fn minimal_replacement_finds_the_changed_middle() {
        assert_eq!(minimal_replacement("same", "same"), None);
        assert_eq!(minimal_replacement("abcXdef", "abcYYdef"), Some((3..4, "YY")));
        assert_eq!(minimal_replacement("a", "a\n"), Some((1..1, "\n")));
        // Never splits a multi-byte character.
        assert_eq!(minimal_replacement("h\u{e9}llo", "h\u{e8}llo"), Some((1..3, "\u{e8}")));
    }

    use super::{
        byte_at_display_column, diff_as_edit, display_column_at_byte, display_columns,
        display_rows, find_matches, fit_label, lens_left_after_text, minimal_replacement,
        next_word_boundary, normalize_for_save, prev_word_boundary, reveal_offset, EditorView,
    };

    #[test]
    fn folded_lines_have_no_row_and_later_lines_move_up() {
        let (rows, line_row) = display_rows(&[0, 1, 4], 5);
        assert_eq!(rows, vec![0, 1, 4]);
        assert_eq!(line_row[4], 2, "lines 2 and 3 are folded away");
    }

    #[test]
    fn no_change_is_no_edit() {
        assert!(diff_as_edit("same", "same").is_none());
    }

    #[test]
    fn pure_insertion_in_the_middle() {
        let edit = diff_as_edit("ab", "aXb").unwrap();
        assert_eq!(edit.start_byte, 1);
        assert_eq!(edit.old_end_byte, 1);
        assert_eq!(edit.new_end_byte, 2);
    }

    #[test]
    fn pure_deletion_in_the_middle() {
        let edit = diff_as_edit("aXb", "ab").unwrap();
        assert_eq!(edit.start_byte, 1);
        assert_eq!(edit.old_end_byte, 2);
        assert_eq!(edit.new_end_byte, 1);
    }

    #[test]
    fn full_replacement_shares_no_prefix_or_suffix() {
        let edit = diff_as_edit("hello", "goodbye").unwrap();
        assert_eq!(edit.start_byte, 0);
        assert_eq!(edit.old_end_byte, "hello".len());
        assert_eq!(edit.new_end_byte, "goodbye".len());
    }

    #[test]
    fn whole_file_replace_still_produces_one_edit() {
        let old = "line one\nline two";
        let new = "totally different file\ncontent here";
        let edit = diff_as_edit(old, new).unwrap();
        assert_eq!(edit.start_byte, 0);
        assert_eq!(edit.old_end_byte, old.len());
        assert_eq!(edit.new_end_byte, new.len());
    }

    #[test]
    fn search_is_case_insensitive_by_default() {
        let text = "Foo foo FOO";
        assert_eq!(find_matches(text, "foo", false, false), vec![0..3, 4..7, 8..11]);
        assert_eq!(find_matches(text, "foo", true, false), vec![4..7]);
    }

    #[test]
    fn whole_word_skips_matches_inside_identifiers() {
        let text = "count counter recount count_x count.";
        assert_eq!(find_matches(text, "count", false, true), vec![0..5, 30..35]);
    }

    #[test]
    fn whole_word_finds_a_match_overlapping_a_rejected_one() {
        // `aa` at 0 is glued to the next `a`; the real word starts at 4.
        assert_eq!(find_matches("aaa aa", "aa", false, true), vec![4..6]);
    }

    #[test]
    fn search_offsets_stay_aligned_after_non_ascii_text() {
        let text = "İstanbul foo";
        let matches = find_matches(text, "FOO", false, false);
        assert_eq!(matches, vec![10..13]);
        assert_eq!(&text[matches[0].clone()], "foo");
    }

    #[test]
    fn revealing_the_cursor_only_moves_the_pan_when_needed() {
        assert_eq!(reveal_offset(0.0, 10.0, 100.0, 5.0, 500.0), 0.0, "already visible");
        assert_eq!(
            reveal_offset(0.0, 120.0, 100.0, 5.0, 500.0),
            25.0,
            "caret past the right edge pulls the pan along"
        );
        assert_eq!(
            reveal_offset(200.0, 10.0, 100.0, 5.0, 500.0),
            5.0,
            "caret before the left edge scrolls back"
        );
        assert_eq!(
            reveal_offset(0.0, 1000.0, 100.0, 5.0, 500.0),
            500.0,
            "never past the content"
        );
    }

    #[test]
    fn a_long_blame_label_is_shortened_with_an_ellipsis() {
        assert_eq!(fit_label("short", 40.0, 100.0).as_deref(), Some("short"));
        let fitted = fit_label("Jeremiah Ogbomo \u{b7} build the feature", 240.0, 120.0)
            .expect("a shortened label");
        assert!(fitted.ends_with('\u{2026}'));
        assert!(fitted.chars().count() < 20, "fitted: {fitted}");
        assert_eq!(fit_label("anything", 100.0, 8.0), None, "no room for a label");
    }

    #[test]
    fn test_actions_sit_after_the_code_and_follow_the_pan() {
        assert_eq!(lens_left_after_text(30, 7.5, 0.0), 233.0);
        assert_eq!(lens_left_after_text(30, 7.5, 100.0), 133.0);
        assert_eq!(
            lens_left_after_text(30, 7.5, 1000.0),
            8.0,
            "scrolled past the end, the actions stop at the row start"
        );
    }

    #[test]
    fn tabs_and_wide_characters_count_as_cells() {
        assert_eq!(display_columns("abc"), 3);
        assert_eq!(display_columns("a\tb"), 6);
        assert_eq!(display_columns("\u{4e2d}\u{6587}"), 4);
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<EditorEvent> for EditorView {}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Only touch the full buffer string or re-run tree-sitter when an edit
        // actually incremented `buffer.revision()`. Hovering, scrolling, or
        // moving the cursor does zero full-buffer work.
        self.sync_buffer_state();
        let options = cx.try_global::<EditorOptions>().copied().unwrap_or_default();
        self.set_inlay_hints_enabled(options.inlay_hints, cx);
        self.maybe_schedule_inlay_hints(cx);
        self.dismiss_stale_signature_help(cx);
        self.maybe_prefetch_code_actions(cx);
        self.maybe_request_highlights(cx);
        self.maybe_request_colors(cx);

        let primary = self.buffer.selections().primary();
        let head = primary.head;
        let cursor_line = Some(self.buffer.byte_to_line(head));
        let selection_range = (!primary.is_empty()).then(|| primary.range());

        let current_word = if selection_range.is_none() && self.search_query.is_empty() {
            let line_idx = self.buffer.byte_to_line(head);
            let line_start = self.buffer.line_to_byte(line_idx);
            let line_text = self.buffer.line(line_idx);
            let display = line_text.trim_end_matches(['\n', '\r']);
            let col = (head - line_start).min(display.len());
            Self::word_at(display, col).map(str::to_string)
        } else {
            None
        };

        let diagnostics_by_line = self.diagnostics_by_line();
        let diagnostic_messages = self.diagnostic_messages_by_line();
        self.active_guide = if options.indent_guides {
            indent_guides::active_guide(&self.guide_counts, self.buffer.byte_to_line(head))
        } else {
            None
        };
        let bracket_pair = match self.bracket_pair_cache {
            Some((cached_head, pair)) if cached_head == head => pair,
            _ => {
                let pair = self.syntax_tree.as_ref().and_then(|tree| {
                    tree.matching_bracket(&self.synced_text, head)
                        .map(|other| (head, other))
                        .or_else(|| {
                            (head > 0).then(|| {
                                tree.matching_bracket(&self.synced_text, head - 1)
                                    .map(|other| (head - 1, other))
                            })?
                        })
                });
                self.bracket_pair_cache = Some((head, pair));
                pair
            }
        };

        // Horizontal scrolling: the text column can be wider than the pane.
        // Measure the monospace cell once, cache the widest line per revision,
        // and pan only the text (the gutter stays put).
        let char_width = {
            let run = TextRun {
                len: 10,
                font: font(cx.theme().mono_font_family.clone()),
                color: cx.theme().foreground,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            (window
                .text_system()
                .shape_line("MMMMMMMMMM".into(), px(LINE_FONT_SIZE), &[run], None)
                .width
                / 10.0)
                .to_f64()
        };
        let head = self.buffer.selections().primary().head;
        if self.content_revision != Some(self.buffer.revision()) {
            let length = self.buffer.len_bytes();
            if length + 64 < self.content_scan_len {
                // A big deletion or undo can remove the widest line: rescan
                // once rather than showing a bar for text that is gone.
                self.recompute_content_cols();
            } else {
                // Ordinary typing only changes the caret's line. Fold its
                // width into the cached maximum instead of walking every line
                // on every keystroke, which stalls big files under key repeat.
                let line = self.buffer.byte_to_line(head);
                let display = self.buffer.line(line);
                let cols = display_columns(display.trim_end_matches(['\n', '\r']));
                self.content_cols = self.content_cols.max(cols);
                self.content_revision = Some(self.buffer.revision());
                self.content_scan_len = length;
            }
        }
        let viewport = self.text_area_width.get().to_f64();
        let content_width = self.content_cols as f64 * char_width + 24.0;
        let max_scroll = (content_width - viewport).max(0.0);
        self.max_scroll_x = px(max_scroll as f32);

        // Follow the cursor only when it moves, so panning by hand sticks
        // until the next edit or cursor movement.
        if self.revealed_head != Some(head) {
            let line = self.buffer.byte_to_line(head);
            let line_start = self.buffer.line_to_byte(line);
            let text = self.buffer.line(line);
            let display = text.trim_end_matches(['\n', '\r']);
            let mut column = (head - line_start).min(display.len());
            while column > 0 && !display.is_char_boundary(column) {
                column -= 1;
            }
            let caret_x = display_columns(&display[..column]) as f64 * char_width;
            let x = reveal_offset(
                self.scroll_x.to_f64(),
                caret_x,
                viewport,
                char_width * 2.0,
                max_scroll,
            );
            self.scroll_x = px(x as f32);
            self.revealed_head = Some(head);
        }
        self.scroll_x = self.scroll_x.clamp(px(0.), self.max_scroll_x);
        let scroll_x = self.scroll_x;

        // One list row per visible code line, all LINE_HEIGHT; folding is the
        // only thing that shifts the line -> row mapping. Diagnostics live in
        // the margin notes, not in the list.
        let visible_lines: Vec<usize> = if self.folded_starts.is_empty() {
            (0..self.buffer.line_count().max(1)).collect()
        } else {
            self.visible_lines.clone()
        };
        let (display_items, display_line_index) =
            display_rows(&visible_lines, self.buffer.line_count());
        self.display_line_index = display_line_index;
        // The note column takes its width out of the text area, so long
        // lines scroll under the code's own edge, never under a note.
        let line_notes = self.line_notes();
        let notes_width = self.margin_notes_width(!line_notes.is_empty());
        let margin_notes =
            self.render_margin_notes(line_notes, notes_width, LIST_TOP_PAD, window, cx);

        // Sticky scroll: the enclosing blocks' opening lines, pinned at the
        // top, drawn exactly like code lines. Clicking one scrolls to it.
        // The top row comes from the scroll offset (rows are LINE_HEIGHT
        // each), so the pinned lines match this frame, not the last one.
        let scroll_y = -self.scroll_handle.0.borrow().base_handle.offset().y;
        let top_row = (scroll_y.to_f64() / LINE_HEIGHT as f64).floor().max(0.) as usize;
        let sticky_top = display_items.get(top_row).copied().unwrap_or(0);
        let sticky_rows: Vec<AnyElement> = self
            .sticky_header_lines(sticky_top)
            .into_iter()
            .map(|line| {
                let start = self.buffer.line_to_byte(line).min(self.synced_text.len());
                let end = if line + 1 < self.buffer.line_count() {
                    self.buffer.line_to_byte(line + 1)
                } else {
                    self.buffer.len_bytes()
                }
                .min(self.synced_text.len())
                .max(start);
                let highlights = if let Some(tree) = &self.syntax_tree {
                    tree.highlights_in_range(&self.synced_text, start..end)
                } else {
                    lexical_highlights_in_range(&self.synced_text, start..end, self.language.keywords())
                };
                let text = self.buffer.line(line);
                let row = self.render_line(
                    line,
                    &text,
                    None,
                    &highlights,
                    None,
                    &diagnostics_by_line,
                    &diagnostic_messages,
                    &self.line_changes,
                    None,
                    None,
                    scroll_x,
                    char_width,
                    window,
                    cx,
                );
                div()
                    .id(("sticky-line", line))
                    .w_full()
                    .h(px(LINE_HEIGHT))
                    .overflow_hidden()
                    .cursor_pointer()
                    .child(row)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.scroll_to_line(line, ScrollStrategy::Top);
                            cx.notify();
                        }),
                    )
                    .into_any_element()
            })
            .collect();
        let sticky_overlay = (!sticky_rows.is_empty()).then(|| {
            v_flex()
                .id("editor-sticky")
                .occlude()
                .absolute()
                .top(px(LIST_TOP_PAD))
                .left_0()
                .right(px(notes_width))
                .bg(cx.theme().background)
                .border_b_1()
                .border_color(cx.theme().border.opacity(0.6))
                .shadow_sm()
                .children(sticky_rows)
        });

        let list = uniform_list(
            "editor-lines",
            display_items.len(),
            cx.processor(move |this, visible_range: Range<usize>, window, cx| {
                if visible_range.is_empty() {
                    return Vec::new();
                }
                this.viewport_lines = visible_range.len();
                let mut first_line = None;
                let mut end_line = 0usize;
                for &line in &display_items[visible_range.clone()] {
                    first_line.get_or_insert(line);
                    end_line = line + 1;
                }
                let (start_line, end_line) = match first_line {
                    Some(line) => (line, end_line.min(this.buffer.line_count())),
                    None => (0, 0),
                };
                let start_byte = this.buffer.line_to_byte(start_line);
                let end_byte = if end_line < this.buffer.line_count() {
                    this.buffer.line_to_byte(end_line)
                } else {
                    this.buffer.len_bytes()
                };

                let clamped_start = start_byte.min(this.synced_text.len());
                let clamped_end = end_byte.min(this.synced_text.len()).max(clamped_start);
                this.closing_labels = if !this.synced_text.is_empty() {
                    this.language.closing_labels_in_range(
                        &this.synced_text,
                        start_line..end_line,
                        clamped_start..clamped_end,
                    )
                } else {
                    std::collections::HashMap::new()
                };
                let highlights = if let Some(tree) = &this.syntax_tree {
                    tree.highlights_in_range(&this.synced_text, clamped_start..clamped_end)
                } else if !this.synced_text.is_empty() {
                    lexical_highlights_in_range(
                        &this.synced_text,
                        clamped_start..clamped_end,
                        this.language.keywords(),
                    )
                } else {
                    Vec::new()
                };

                visible_range
                    .map(|row| {
                        let ix = display_items[row];
                        let line = if this.buffer.is_empty() && ix == 0 {
                            String::new()
                        } else if ix < this.buffer.line_count() {
                            this.buffer.line(ix)
                        } else {
                            String::new()
                        };
                        this.render_line(
                            ix,
                            &line,
                            cursor_line,
                            &highlights,
                            selection_range.clone(),
                            &diagnostics_by_line,
                            &diagnostic_messages,
                            &this.line_changes,
                            current_word.as_deref(),
                            bracket_pair,
                            scroll_x,
                            char_width,
                            window,
                            cx,
                        )
                        .into_any_element()
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.scroll_handle)
        .size_full();

        let show_h_scroll = max_scroll > 0.5 && viewport > 1.0;
        let viewport_f = viewport.max(1.0);
        let thumb_w_f = if content_width > 0.0 {
            (viewport_f * viewport_f / content_width).clamp(24.0, viewport_f)
        } else {
            viewport_f
        };
        let thumb_w = px(thumb_w_f as f32);
        self.h_span = px((viewport_f - thumb_w_f).max(1.0) as f32);
        let thumb_x = px(if max_scroll > 0.0 {
            ((viewport_f - thumb_w_f) * (self.scroll_x.to_f64() / max_scroll)) as f32
        } else {
            0.0
        });
        let track_color = cx.theme().border.opacity(0.16);
        let thumb_color = cx.theme().muted_foreground.opacity(0.5);
        let thumb_hover = cx.theme().muted_foreground.opacity(0.85);
        // A window-level mouse tracker for the thumb drag: moving over the
        // thumb itself does not reach the editor root's hover handler.
        let drag_view = cx.entity();
        let drag_up_view = drag_view.clone();
        let drag_tracker = canvas(
            |_, _, _| (),
            move |_, _, window, _cx| {
                let view = drag_view.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
                    if phase != DispatchPhase::Bubble
                        || event.pressed_button != Some(MouseButton::Left)
                    {
                        return;
                    }
                    view.update(cx, |this, cx| {
                        if let Some(drag) = &this.h_drag {
                            let per_pixel =
                                drag.max_scroll.to_f64() / drag.span.to_f64().max(1.0);
                            let moved = (event.position.x - drag.start_x).to_f64();
                            let next = (drag.start_scroll.to_f64() + moved * per_pixel)
                                .clamp(0.0, drag.max_scroll.to_f64());
                            let next = px(next as f32);
                            if next != this.scroll_x {
                                this.scroll_x = next;
                                cx.notify();
                            }
                        }
                    });
                });
                let view = drag_up_view.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
                    if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                        return;
                    }
                    view.update(cx, |this, cx| {
                        if this.h_drag.take().is_some() {
                            cx.notify();
                        }
                    });
                });
            },
        )
        .absolute()
        .left(px(0.))
        .top(px(0.))
        .w(px(1.))
        .h(px(1.));

        let menu_view = cx.entity();
        let menu_lsp_ready = self.lsp_ready();
        let menu_has_selection = !primary.is_empty();

        div()
            .id("editor-view")
            .key_context("Editor")
            .track_focus(&self.focus_handle)
            .relative()
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                // Scrolling moves the code out from under the pointer: the
                // hover card must not ride along with it.
                this.clear_hover(cx);
                // Same rule as the row-level capture: Shift means horizontal,
                // otherwise only a mostly-horizontal gesture pans. This path
                // handles the empty space below the last line.
                let (dx, dy) = match event.delta {
                    ScrollDelta::Pixels(p) => (p.x.to_f64(), p.y.to_f64()),
                    ScrollDelta::Lines(p) => (p.x as f64 * 24.0, p.y as f64 * 24.0),
                };
                let delta = if event.modifiers.shift && dy != 0.0 {
                    dy
                } else if dx != 0.0 && dx.abs() > dy.abs() {
                    dx
                } else {
                    return;
                };
                if this.max_scroll_x > px(0.) {
                    this.pan_horizontally(delta);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event, _window, cx| {
                    if this.h_drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _event, _window, cx| {
                    if this.h_drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                if !hovered {
                    this.clear_hover(cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event, window, cx| {
                    // A click anywhere in the editor ends a stale thumb drag.
                    this.h_drag = None;
                    window.focus(&this.focus_handle, cx);
                    if this.code_action_menu.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .pt(px(LIST_TOP_PAD))
            .child(drag_tracker)
            .child(div().size_full().pr(px(notes_width)).child(list))
            .when_some(sticky_overlay, |el, overlay| el.child(overlay))
            .children(margin_notes)
            .when(show_h_scroll, |el| {
                el.child(
                    div()
                        .id("editor-h-scrollbar")
                        .debug_selector(|| "editor-h-scrollbar".into())
                        .absolute()
                        .bottom(px(0.))
                        .left(px(GUTTER_WIDTH))
                        .w(px(viewport_f as f32))
                        .h(px(7.))
                        .bg(track_color)
                        .occlude()
                        .child(
                            div()
                                .id("editor-h-thumb")
                                .debug_selector(|| "editor-h-thumb".into())
                                .absolute()
                                .top(px(1.))
                                .bottom(px(1.))
                                .left(thumb_x)
                                .w(thumb_w)
                                .rounded(px(2.5))
                                .bg(thumb_color)
                                .cursor_pointer()
                                .hover(move |s| s.bg(thumb_hover))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                        cx.stop_propagation();
                                        window.focus(&this.focus_handle, cx);
                                        this.h_drag = Some(HorizontalDrag {
                                            start_x: event.position.x,
                                            start_scroll: this.scroll_x,
                                            max_scroll: this.max_scroll_x,
                                            span: this.h_span,
                                        });
                                        cx.notify();
                                    }),
                                ),
                        ),
                )
            })
            .context_menu(move |menu, _window, _cx| {
                editor_context_menu(menu, menu_view.clone(), menu_lsp_ready, menu_has_selection)
            })
    }
}

/// One menu row: muted icon, label at the UI size, and the shortcut on the
/// right. Matches the app's other menus.
fn menu_row(icon: gpui_kit::assets::IconName, label: &'static str, hint: Option<&'static str>) -> PopupMenuItem {
    PopupMenuItem::element(move |_window, cx| {
        let muted = cx.theme().muted_foreground;
        h_flex()
            .w_full()
            .min_w(px(200.))
            .gap(px(8.))
            .items_center()
            .text_size(px(12.5))
            .text_color(cx.theme().foreground)
            .child(gpui_kit::component::Icon::new(icon.clone()).with_size(px(13.)).text_color(muted))
            .child(div().flex_1().child(label))
            .when_some(hint, |el, hint| {
                el.child(div().pl(px(16.)).text_size(px(11.)).text_color(muted.opacity(0.8)).child(hint))
            })
    })
}

/// Right-click menu in the text area, in the order VS Code's editor menu
/// uses. Each entry runs the same command as its keyboard shortcut.
fn editor_context_menu(
    menu: PopupMenu,
    view: Entity<EditorView>,
    lsp_ready: bool,
    has_selection: bool,
) -> PopupMenu {
    use gpui_kit::assets::IconName as I;
    let run = |view: Entity<EditorView>, command: &'static str| {
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            view.update(cx, |this, cx| {
                this.run_editor_command(command, cx);
            });
        }
    };
    menu.item(menu_row(I::ArrowRight, "Go to Definition", Some("\u{2318}click")).disabled(!lsp_ready).on_click({
        let view = view.clone();
        move |_, _, cx| {
            view.update(cx, |this, _cx| {
                let head = this.buffer.selections().primary().head;
                let line = this.buffer.byte_to_line(head);
                this.request_definition(line, head - this.buffer.line_to_byte(line));
            });
        }
    }))
    .item(menu_row(I::Layers, "Go to Implementation", None).disabled(!lsp_ready).on_click(run(view.clone(), "go-to-implementation")))
    .item(menu_row(I::Shapes, "Go to Type Definition", None).disabled(!lsp_ready).on_click(run(view.clone(), "go-to-type-definition")))
    .item(menu_row(I::Eye, "Peek Definition", Some("\u{2318}Y")).disabled(!lsp_ready).on_click(run(view.clone(), "peek-definition")))
    .item(menu_row(I::Search, "Find All References", Some("\u{21e7}\u{2318}Y")).disabled(!lsp_ready).on_click(run(view.clone(), "find-references")))
    .separator()
    .item(menu_row(I::Pencil, "Rename Symbol", Some("F2")).disabled(!lsp_ready).on_click(run(view.clone(), "rename")))
    .item(menu_row(I::WandSparkles, "Refactor\u{2026}", Some("\u{21e7}\u{2318}R")).disabled(!lsp_ready).on_click(run(view.clone(), "refactor")))
    .item(menu_row(I::Lightbulb, "Quick Fix\u{2026}", Some("\u{2318}.")).disabled(!lsp_ready).on_click(run(view.clone(), "code-actions")))
    .item(menu_row(I::TextAlignStart, "Format Document", Some("\u{2325}\u{21e7}F")).disabled(!lsp_ready).on_click(run(view.clone(), "format-doc")))
    .separator()
    .item(menu_row(I::Scissors, "Cut", Some("\u{2318}X")).disabled(!has_selection).on_click(run(view.clone(), "cut")))
    .item(menu_row(I::Copy, "Copy", Some("\u{2318}C")).disabled(!has_selection).on_click(run(view.clone(), "copy")))
    .item(menu_row(I::Clipboard, "Paste", Some("\u{2318}V")).on_click(run(view.clone(), "paste")))
    .separator()
    .item(menu_row(I::MessageSquareCode, "Toggle Comment", Some("\u{2318}/")).on_click(run(view.clone(), "toggle-comment")))
    .item(menu_row(I::CopyPlus, "Copy Line Down", Some("\u{21e7}\u{2325}\u{2193}")).on_click(run(view.clone(), "copy-line-down")))
    .item(menu_row(I::Trash, "Delete Line", Some("\u{21e7}\u{2318}K")).on_click(run(view.clone(), "delete-line")))
    .separator()
    .item(menu_row(I::SquareDashed, "Select All", Some("\u{2318}A")).on_click(run(view, "select-all")))
}
