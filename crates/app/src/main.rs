//! A GPUI window with a dock-style split layout: a file tree panel reading
//! the real project directory, and our own `editor-view` (M1) in place of
//! the M0 `Textarea` stand-in.
//!
//! See `NOTES.md` at the repo root for the measured startup time, idle RAM,
//! and binary size from the M0 spike.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

mod agent_composer;
mod agent_ctx;
mod agent_indicator;
mod agent_permission;
mod agent_plan;
mod chrome;
mod diff_view;
mod editor;
mod file_state;
mod git_panel;
mod history;
mod markdown;
mod lsp_health;
mod rails;
mod peek;
mod debug;
mod flutter;
mod palette;
mod prompts;
mod settings_ui;
mod tab_ops;
mod test_runner;
mod workspace;
mod shell;
mod simulator;
mod theme;
#[allow(dead_code)]
mod tokens;

use core_buffer::Buffer;
use core_lsp::lsp_types;
use editor_view::{ConflictChoice, EditorEvent, EditorView};
use gpui_kit::assets::AllAssets;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    h_flex,
    input::{Input, InputState},
    tree::TreeState,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use rails::files::{build_tree_items, render_file_icon};

/// Directory/file names we never want to show in the file tree or scan during Quick Open.
pub(crate) const IGNORED_NAMES: &[&str] = &[
    "target",
    "dist",
    "node_modules",
    ".dart_tool",
    "build",
    "Pods",
    "DerivedData",
    ".gradle",
    ".next",
    "__pycache__",
    ".venv",
    "Library",
    ".Trash",
    ".DS_Store",
];

const MAX_SCAN_DEPTH: usize = 7;
const MAX_FLATTENED_FILES: usize = 12_000;

/// Every file under `dir`, bounded by depth and count for instantaneous startup.
fn flatten_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    flatten_files_bounded(dir, 0, &mut files);
    files
}

fn flatten_files_bounded(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_SCAN_DEPTH || out.len() >= MAX_FLATTENED_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_FLATTENED_FILES {
            return;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == ".git" || IGNORED_NAMES.contains(&name) {
            continue;
        }
        if path.is_dir() {
            flatten_files_bounded(&path, depth + 1, out);
        } else {
            out.push(path);
        }
    }
}

#[derive(Default)]
struct WorkspaceStore {
    current: Option<PathBuf>,
    recents: Vec<PathBuf>,
}

fn workspace_store_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(Path::new(&home).join(".config/shame/workspaces.json"))
}

fn load_workspace_store() -> WorkspaceStore {
    let Some(path) = workspace_store_path() else {
        return WorkspaceStore::default();
    };
    let Ok(bytes) = std::fs::read(path) else {
        return WorkspaceStore::default();
    };
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return WorkspaceStore::default();
    };
    let current = val
        .get("current")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .filter(|p| p.is_dir() && p != Path::new("/"));
    let recents = val
        .get("recents")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(PathBuf::from))
                .filter(|p| p.is_dir() && p != Path::new("/"))
                .collect()
        })
        .unwrap_or_default();
    WorkspaceStore { current, recents }
}

fn save_workspace_store(current: Option<&Path>, recents: &[PathBuf]) {
    let Some(path) = workspace_store_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let recents_strs: Vec<String> = recents
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    let val = serde_json::json!({
        "current": current.map(|p| p.to_string_lossy().to_string()),
        "recents": recents_strs,
    });
    if let Ok(json) = serde_json::to_vec_pretty(&val) {
        let _ = std::fs::write(path, json);
    }
}

/// Resolve the startup workspace state:
/// - If a directory path was explicitly passed on the CLI (`shame /path/to/project`), open it.
/// - Otherwise, restore `store.current` if the user had a folder open in their last session.
/// - If `store.current` is `None` (first launch or after closing a folder), start in the
///   Welcome / No-Folder Empty State (`has_workspace = false`) with `store.recents` ready to click.
fn resolve_initial_workspace() -> (bool, PathBuf, Vec<PathBuf>) {
    let mut store = load_workspace_store();

    if let Some(arg_path) = std::env::args().nth(1).filter(|a| !a.starts_with('-')) {
        let p = std::fs::canonicalize(&arg_path).unwrap_or_else(|_| PathBuf::from(arg_path));
        if p.is_dir() && p != Path::new("/") {
            store.recents.retain(|r| r != &p);
            store.recents.insert(0, p.clone());
            store.recents.truncate(10);
            save_workspace_store(Some(&p), &store.recents);
            let _ = std::env::set_current_dir(&p);
            return (true, p, store.recents);
        }
    }

    if let Some(cur) = store.current.clone() {
        let _ = std::env::set_current_dir(&cur);
        return (true, cur, store.recents);
    }

    let fallback = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    (false, fallback, store.recents)
}

fn hash_paths(paths: &[PathBuf]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    paths.len().hash(&mut h);
    for p in paths {
        p.hash(&mut h);
    }
    h.finish()
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileTreeOp {
    NewFile,
    NewFolder,
    Rename(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkdownMode {
    Code,
    Split,
    Preview,
}

/// Which view the bottom dock currently shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DockTab {
    Terminal,
    Problems,
    DebugConsole,
    Tests,
}

actions!(
    shame_menus,
    [
        AboutShame,
        OpenSettings,
        QuitShame,
        MenuOpenFolder,
        MenuCloseFolder,
        MenuNewFile,
        MenuSaveFile,
        MenuCloseTab,
        MenuGoToFile,
        MenuCommandPalette,
        MenuFindInFile,
        MenuFindInProject,
        MenuFormatDoc,
        MenuToggleSidebar,
        MenuToggleTerminal,
        MenuToggleProblems,
        MenuToggleAgent,
        MenuToggleWebPreview,
        MenuSplitEditor,
    ]
);

#[derive(Clone, Copy)]
pub(crate) struct IdeCommand {
    pub(crate) id: &'static str,
    pub(crate) title: &'static str,
    pub(crate) shortcut: &'static str,
    pub(crate) flutter_only: bool,
}

const IDE_COMMANDS: &[IdeCommand] = &[
    IdeCommand { id: "test-all", title: "Test: Run All Tests", shortcut: "", flutter_only: true },
    IdeCommand { id: "test-file", title: "Test: Run Tests in Current File", shortcut: "", flutter_only: true },
    IdeCommand { id: "test-rerun-failed", title: "Test: Re-run Failed Tests", shortcut: "", flutter_only: true },
    IdeCommand { id: "run-flutter", title: "Flutter: Run App on Selected Device", shortcut: "", flutter_only: true },
    IdeCommand { id: "hot-reload", title: "Flutter: Hot Reload", shortcut: "\u{2318}S", flutter_only: true },
    IdeCommand { id: "hot-restart", title: "Flutter: Hot Restart", shortcut: "", flutter_only: true },
    IdeCommand { id: "pub-get", title: "Flutter: Run Pub Get", shortcut: "", flutter_only: true },
    IdeCommand { id: "pub-upgrade", title: "Flutter: Run Pub Upgrade", shortcut: "", flutter_only: true },
    IdeCommand { id: "flutter-clean", title: "Flutter: Clean & Pub Get", shortcut: "", flutter_only: true },
    IdeCommand { id: "build-runner", title: "Flutter: Run build_runner (Codegen)", shortcut: "", flutter_only: true },
    IdeCommand { id: "build-runner-watch", title: "Flutter: Watch build_runner", shortcut: "", flutter_only: true },
    IdeCommand { id: "gen-l10n", title: "Flutter: Generate Localizations (gen-l10n)", shortcut: "", flutter_only: true },
    IdeCommand { id: "add-package", title: "Flutter: Add Package (pub.dev)\u{2026}", shortcut: "", flutter_only: true },
    IdeCommand { id: "open-devtools", title: "Flutter: Open DevTools in Browser", shortcut: "", flutter_only: true },
    IdeCommand { id: "toggle-perf-overlay", title: "Flutter: Toggle Performance Overlay", shortcut: "", flutter_only: true },
    IdeCommand { id: "toggle-debug-paint", title: "Flutter: Toggle Debug Paint (Widget Bounds)", shortcut: "", flutter_only: true },
    IdeCommand { id: "toggle-slow-animations", title: "Flutter: Toggle Slow Animations", shortcut: "", flutter_only: true },
    IdeCommand { id: "flutter-doctor", title: "Flutter: Run Flutter Doctor", shortcut: "", flutter_only: true },
    IdeCommand { id: "debug-dap", title: "Debug: Start Dart DAP Debugger", shortcut: "", flutter_only: true },
    IdeCommand { id: "debug-add-watch", title: "Debug: Add Watch Expression\u{2026}", shortcut: "", flutter_only: true },
    IdeCommand { id: "debug-clear-watches", title: "Debug: Clear Watch Expressions", shortcut: "", flutter_only: true },
    IdeCommand { id: "debug-breakpoint-condition", title: "Debug: Set Breakpoint Condition on Current Line\u{2026}", shortcut: "", flutter_only: true },
    IdeCommand { id: "toggle-sim", title: "Simulator: Attach / Detach iOS Simulator", shortcut: "", flutter_only: true },
    IdeCommand { id: "open-git", title: "Git: Open Git Panel", shortcut: "\u{2318}\u{21e7}G", flutter_only: false },
    IdeCommand { id: "git-switch-branch", title: "Git: Switch / Create Branch\u{2026}", shortcut: "", flutter_only: false },
    IdeCommand { id: "git-stage-all", title: "Git: Stage All Changes", shortcut: "", flutter_only: false },
    IdeCommand { id: "git-unstage-all", title: "Git: Unstage All Changes", shortcut: "", flutter_only: false },
    IdeCommand { id: "git-stash", title: "Git: Stash Uncommitted Changes", shortcut: "", flutter_only: false },
    IdeCommand { id: "git-uncommit", title: "Git: Undo Last Commit (Keep Staged)", shortcut: "", flutter_only: false },
    IdeCommand { id: "diff-unified", title: "Diff: Toggle Split / Unified View", shortcut: "", flutter_only: false },
    IdeCommand { id: "project-search", title: "Search: Find in Project", shortcut: "\u{2318}\u{21e7}F", flutter_only: false },
    IdeCommand { id: "toggle-problems", title: "View: Toggle Problems Panel", shortcut: "\u{2318}\u{21e7}M", flutter_only: false },
    IdeCommand { id: "toggle-terminal", title: "View: Toggle Terminal Dock", shortcut: "\u{2318}J", flutter_only: false },
    IdeCommand { id: "toggle-sidebar", title: "View: Toggle Left Sidebar", shortcut: "\u{2318}B", flutter_only: false },
    IdeCommand { id: "toggle-web-preview", title: "View: Toggle Web Preview Rail", shortcut: "", flutter_only: false },
    IdeCommand { id: "toggle-agent", title: "View: Toggle Agent Rail", shortcut: "", flutter_only: false },
    IdeCommand { id: "format-doc", title: "Editor: Format Document", shortcut: "\u{21e7}\u{2318}F", flutter_only: false },
    IdeCommand { id: "save-all", title: "File: Save All", shortcut: "\u{2325}\u{2318}S", flutter_only: false },
    IdeCommand { id: "save-no-format", title: "File: Save Without Formatting", shortcut: "", flutter_only: false },
    IdeCommand { id: "autosave-cycle", title: "File: Change Auto Save Mode", shortcut: "", flutter_only: false },
    IdeCommand { id: "local-history", title: "File: Local History\u{2026} (Restore Earlier Version)", shortcut: "", flutter_only: false },
    IdeCommand { id: "conflict-current", title: "Git: Accept Current Change (Merge Conflict)", shortcut: "", flutter_only: false },
    IdeCommand { id: "conflict-incoming", title: "Git: Accept Incoming Change (Merge Conflict)", shortcut: "", flutter_only: false },
    IdeCommand { id: "conflict-both", title: "Git: Accept Both Changes (Merge Conflict)", shortcut: "", flutter_only: false },
    IdeCommand { id: "refactor", title: "Editor: Refactor\u{2026} (Wrap with Widget, Extract\u{2026})", shortcut: "\u{21e7}\u{2318}R", flutter_only: false },
    IdeCommand { id: "peek-definition", title: "Editor: Peek Definition", shortcut: "\u{2318}Y", flutter_only: false },
    IdeCommand { id: "organize-imports", title: "Editor: Organize Imports", shortcut: "", flutter_only: false },
    IdeCommand { id: "find-references", title: "Editor: Find References", shortcut: "\u{21e7}\u{2318}Y", flutter_only: false },
    IdeCommand { id: "goto-workspace-symbol", title: "Editor: Go to Symbol in Workspace\u{2026}", shortcut: "\u{2318}T", flutter_only: false },
    IdeCommand { id: "goto-symbol", title: "Editor: Go to Symbol in File\u{2026}", shortcut: "\u{21e7}\u{2318}O", flutter_only: false },
    IdeCommand { id: "goto-line", title: "Editor: Go to Line\u{2026}", shortcut: "\u{2318}G", flutter_only: false },
    IdeCommand { id: "new-file", title: "File: New Scratch File", shortcut: "\u{2318}N", flutter_only: false },
    IdeCommand { id: "open-folder", title: "File: Open Folder\u{2026}", shortcut: "\u{2318}O", flutter_only: false },
    IdeCommand { id: "close-folder", title: "File: Close Folder", shortcut: "", flutter_only: false },
    IdeCommand { id: "open-settings", title: "Preferences: Open Settings", shortcut: "\u{2318},", flutter_only: false },
    IdeCommand { id: "about-shame", title: "Help: About Shame", shortcut: "", flutter_only: false },
];

#[derive(Clone)]
pub(crate) struct ImagePreview {
    pub(crate) render_image: std::sync::Arc<RenderImage>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) byte_size: u64,
    pub(crate) format_label: String,
    /// `None` fits the image to the viewport; `Some(pct)` renders at `pct`% (`25..=400`).
    pub(crate) zoom_percent: Option<u32>,
    /// Toggle between dark and light backdrop for inspecting dark/white transparent assets.
    pub(crate) light_backdrop: bool,
}

pub(crate) fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "ico" | "tiff" | "tif"
            )
        })
}

pub(crate) fn load_image_preview(path: &Path) -> Option<ImagePreview> {
    let bytes = std::fs::read(path).ok()?;
    let byte_size = bytes.len() as u64;
    let format_label = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("IMG")
        .to_ascii_uppercase();
    let decoded = image::load_from_memory(&bytes).ok()?;
    let (width, height) = (decoded.width(), decoded.height());
    let mut rgba = decoded.into_rgba8();
    // GPUI renders BGRA channel order in `RenderImage`.
    for px in rgba.pixels_mut() {
        px.0.swap(0, 2);
    }
    let render_image = std::sync::Arc::new(RenderImage::new(vec![image::Frame::new(rgba)]));
    Some(ImagePreview {
        render_image,
        width,
        height,
        byte_size,
        format_label,
        zoom_percent: None,
        light_backdrop: false,
    })
}

pub(crate) fn format_byte_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub(crate) struct Tab {
    /// `None` for the initial scratch buffer; every file opened from the
    /// tree gets a path, which also doubles as the tab's dedup key.
    pub(crate) path: Option<PathBuf>,
    pub(crate) editor: Entity<EditorView>,
    pub(crate) md_mode: MarkdownMode,
    pub(crate) image: Option<ImagePreview>,
    /// Zed/VS Code-style preview tab (`true` when opened via single-click in
    /// the file tree, promoted to `false` on double-click or buffer edit).
    pub(crate) is_preview: bool,
    /// Baseline buffer revision when opened, used to auto-pin a preview tab as
    /// soon as the user types or modifies the buffer.
    pub(crate) opened_revision: u64,
}

impl Tab {
    pub(crate) fn title(&self) -> String {
        match &self.path {
            Some(path) => path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("untitled")
                .to_string(),
            None => "untitled".to_string(),
        }
    }

    pub(crate) fn parent_label(&self) -> Option<String> {
        self.path
            .as_ref()?
            .parent()?
            .file_name()?
            .to_str()
            .map(str::to_string)
    }

    pub(crate) fn is_pinned(&self, cx: &App) -> bool {
        !self.is_preview || self.editor.read(cx).buffer().revision() != self.opened_revision
    }

    pub(crate) fn is_markdown(&self) -> bool {
        self.path
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "md" | "markdown"))
    }

    pub(crate) fn is_pubspec(&self) -> bool {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("pubspec.yaml"))
    }
}

pub struct DartIde {
    pub(crate) has_workspace: bool,
    pub(crate) recent_workspaces: Vec<PathBuf>,
    pub(crate) root: PathBuf,
    pub(crate) git_branch: Option<String>,
    /// Cached `git status` for the tree's row badges.
    pub(crate) git_status: HashMap<PathBuf, char>,
    pub(crate) sidebar_open: bool,
    pub(crate) terminal_open: bool,
    pub(crate) terminal_maximized: bool,
    pub(crate) dock_tab: DockTab,
    pub(crate) debug_lines: Vec<editor::dock::TerminalLine>,
    pub(crate) debug_scroll: ScrollHandle,
    pub(crate) diff: Option<diff_view::DiffState>,
    pub(crate) diff_seq: u64,
    pub(crate) sidebar_tab: git_panel::SidebarTab,
    pub(crate) git: git_panel::GitState,
    pub(crate) sim: crate::simulator::SimulatorState,
    pub(crate) web_preview_open: bool,
    pub(crate) web_preview_url: Entity<InputState>,
    pub(crate) web_preview_image: Option<std::sync::Arc<ImagePreview>>,
    pub(crate) web_preview_viewport: rails::web_preview::WebViewport,
    pub(crate) web_preview_loading: bool,
    pub(crate) web_preview_error: Option<String>,
    pub(crate) agent_open: bool,
    pub(crate) agent: rails::agent::AgentState,
    pub(crate) project_search_input: Entity<InputState>,
    pub(crate) project_search_matches: Vec<core_workspace::SearchMatch>,
    pub(crate) project_replace_input: Entity<InputState>,
    pub(crate) project_replace_status: Option<String>,
    pub(crate) dap_client: Option<std::sync::Arc<std::sync::Mutex<core_dap::DapClient>>>,
    pub(crate) dap_breakpoints: HashMap<PathBuf, Vec<u32>>,
    pub(crate) dap_paused_thread: Option<i64>,
    pub(crate) dap_stack_summary: Option<String>,
    pub(crate) dap_variables: Vec<debug::DapVar>,
    pub(crate) dap_children: HashMap<i64, Vec<debug::DapVar>>,
    pub(crate) dap_expanded: std::collections::HashSet<i64>,
    /// In-flight child `variables` requests: request seq -> parent reference.
    pub(crate) dap_var_pending: HashMap<i64, i64>,
    /// Conditional-breakpoint expressions, keyed by file then 1-indexed line.
    pub(crate) dap_conditions: HashMap<PathBuf, std::collections::BTreeMap<u32, String>>,
    pub(crate) dap_watches: Vec<String>,
    pub(crate) dap_watch_values: HashMap<String, String>,
    pub(crate) dap_frame_id: Option<i64>,
    /// In-flight `evaluate` requests: request seq -> watch expression.
    pub(crate) dap_eval_pending: HashMap<i64, String>,
    pub(crate) debug_prompt: Option<debug::DebugPrompt>,
    pub(crate) debug_prompt_input: Entity<InputState>,
    pub(crate) show_hidden: bool,
    pub(crate) file_tree_op: Option<FileTreeOp>,
    pub(crate) file_tree_input: Entity<InputState>,
    pub(crate) tree_state: Entity<TreeState>,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active_tab: usize,
    pub(crate) split_tab: Option<usize>,
    pub(crate) tab_scroll: ScrollHandle,
    pub(crate) tab_overflow_open: bool,
    pub(crate) all_files: Vec<PathBuf>,
    pub(crate) all_files_hash: u64,
    pub(crate) syncing_files: bool,
    pub(crate) palette_open: bool,
    pub(crate) palette_input: Entity<InputState>,
    pub(crate) palette_selected: usize,
    pub(crate) find_open: bool,
    pub(crate) find_input: Entity<InputState>,
    pub(crate) replace_input: Entity<InputState>,
    pub(crate) go_to_line_open: bool,
    pub(crate) go_to_line_input: Entity<InputState>,
    pub(crate) rename_target: Option<(Entity<EditorView>, u32, u32)>,
    pub(crate) rename_input: Entity<InputState>,
    pub(crate) code_actions: Option<(Entity<EditorView>, Vec<lsp_types::CodeActionOrCommand>)>,
    pub(crate) peek: Option<peek::PeekState>,
    pub(crate) peek_focus: FocusHandle,
    pub(crate) workspace_symbols: Vec<core_workspace::WorkspaceSymbol>,
    pub(crate) symbols_indexing: bool,
    pub(crate) history: Option<(Entity<EditorView>, Vec<core_workspace::Snapshot>)>,
    pub(crate) autosave: file_state::AutosaveMode,
    pub(crate) test_run: Option<test_runner::TestRunState>,
    pub(crate) save_options: editor_view::SaveOptions,
    /// Per editor: the revision last seen and when it last changed (autosave timing).
    pub(crate) edit_marks: HashMap<EntityId, (u64, std::time::Instant)>,
    /// Things worth telling the user about each file (changed on disk, save failed).
    pub(crate) file_notices: HashMap<PathBuf, file_state::FileNotice>,
    pub(crate) unsaved_prompt: Option<file_state::UnsavedPrompt>,
    pub(crate) disk_tick: u32,
    pub(crate) terminals: Vec<editor::dock::TerminalSession>,
    pub(crate) active_terminal: usize,
    pub(crate) next_terminal_id: usize,
    pub(crate) terminal_focus: FocusHandle,
    /// Focus target for the window itself. Menu-bar actions (About, Settings,
    /// Open Folder, ...) only dispatch to views on the focus path, so without
    /// this the menus are dead until an editor happens to take focus.
    pub(crate) root_focus: FocusHandle,
    pub(crate) terminal_tx: async_channel::Sender<editor::dock::TerminalEvent>,
    /// Flutter devices, `.vscode/launch.json` configurations, running daemon, and Attached Simulator state.
    pub(crate) devices: Vec<core_flutter::Device>,
    pub(crate) ios_simulators: Vec<core_flutter::IosSimulator>,
    pub(crate) selected_device: Option<core_flutter::Device>,
    pub(crate) device_picker_open: bool,
    pub(crate) sim_list_expanded: bool,
    pub(crate) launch_configs: Vec<core_flutter::LaunchConfig>,
    pub(crate) test_launch_templates: Vec<core_flutter::LaunchConfig>,
    pub(crate) selected_launch_config: Option<usize>,
    pub(crate) launch_picker_open: bool,
    pub(crate) flutter_daemon: Option<std::sync::Arc<std::sync::Mutex<core_flutter::FlutterDaemon>>>,
    pub(crate) flutter_app_id: Option<String>,
    pub(crate) flutter_ws_uri: Option<String>,
    pub(crate) flutter_status: Option<String>,
    pub(crate) perf_overlay_on: bool,
    pub(crate) debug_paint_on: bool,
    pub(crate) slow_animations_on: bool,
    pub(crate) settings_open: bool,
    pub(crate) settings_tab: usize,
    /// Language servers as resolved the last time Settings was opened, so the
    /// panel never touches the filesystem mid-render.
    pub(crate) lsp_servers: Vec<crate::lsp_health::LspServerStatus>,
    pub(crate) settings_search: Entity<InputState>,
    pub(crate) about_open: bool,
    pub(crate) light_theme: bool,
    pub(crate) add_package_open: bool,
    pub(crate) add_package_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl DartIde {
    /// Open or close Settings, re-checking the installed language servers on
    /// the way in so the list reflects what exists right now.
    pub(crate) fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_open = !self.settings_open;
        if self.settings_open {
            self.refresh_lsp_health();
        }
        cx.notify();
    }

    /// Re-resolve every configured language server binary.
    pub(crate) fn refresh_lsp_health(&mut self) {
        let root = self.has_workspace.then(|| self.root.clone());
        self.lsp_servers = crate::lsp_health::scan(root.as_deref());
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (has_workspace, root, recent_workspaces) = resolve_initial_workspace();
        let show_hidden = true;
        let items = if has_workspace {
            build_tree_items(&root, show_hidden)
        } else {
            Vec::new()
        };
        let all_files = if has_workspace {
            flatten_files(&root)
        } else {
            Vec::new()
        };

        let tree_state = cx.new(|cx| TreeState::new(cx).items(items));
        let palette_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Go to file, or type > for commands\u{2026}")
        });
        let find_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Find in file\u{2026}"));
        let replace_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Replace with\u{2026}"));
        let go_to_line_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Line[:Column] (e.g. 42 or 42:8)...")
        });
        let rename_input = cx.new(|cx| InputState::new(window, cx).placeholder("New name..."));
        let add_package_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Package name (e.g. provider, http, dev:build_runner)...")
        });
        let project_search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search across files...")
        });
        let debug_prompt_input = cx.new(|cx| InputState::new(window, cx));
        let project_replace_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Replace with...")
        });
        let file_tree_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("path/to/file.dart\u{2026}")
        });
        let web_preview_url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("http://localhost:3000 or file://\u{2026}")
                .default_value("http://localhost:3000")
        });
        let settings_search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search settings\u{2026}")
        });
        let agent = rails::agent::AgentState::new(window, cx);

        // Filled in by the first `refresh_git` (off the UI thread) right after construction.
        let git_branch = None;
        let git_status = HashMap::new();
        let (terminal_tx, terminal_rx) = async_channel::unbounded();
        let initial_terminal =
            editor::dock::TerminalSession::new(0, root.clone(), has_workspace);
        let terminal_focus = cx.focus_handle();
        let root_focus = cx.focus_handle();

        cx.spawn(async move |weak, cx| {
            while let Ok(evt) = terminal_rx.recv().await {
                if weak
                    .update(cx, |this, cx| this.handle_terminal_event(evt, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let default_device = core_flutter::Device {
            id: "macos".to_string(),
            name: "macOS".to_string(),
            target_platform: "darwin".to_string(),
            emulator: false,
            is_supported: true,
        };

        let mut this = Self {
            has_workspace,
            recent_workspaces,
            root,
            git_branch,
            git_status,
            sidebar_open: has_workspace,
            terminal_open: has_workspace,
            terminal_maximized: false,
            dock_tab: DockTab::Terminal,
            debug_lines: Vec::new(),
            debug_scroll: ScrollHandle::new(),
            diff: None,
            diff_seq: 0,
            sidebar_tab: git_panel::SidebarTab::Files,
            git: git_panel::GitState::new(window, cx),
            sim: Default::default(),
            web_preview_open: false,
            web_preview_url,
            web_preview_image: None,
            web_preview_viewport: rails::web_preview::WebViewport::Desktop,
            web_preview_loading: false,
            web_preview_error: None,
            agent_open: false,
            agent,
            project_search_input,
            project_search_matches: Vec::new(),
            project_replace_input,
            project_replace_status: None,
            dap_client: None,
            dap_breakpoints: HashMap::new(),
            dap_paused_thread: None,
            dap_stack_summary: None,
            dap_variables: Vec::new(),
            dap_children: HashMap::new(),
            dap_expanded: std::collections::HashSet::new(),
            dap_var_pending: HashMap::new(),
            dap_conditions: HashMap::new(),
            dap_watches: Vec::new(),
            dap_watch_values: HashMap::new(),
            dap_frame_id: None,
            dap_eval_pending: HashMap::new(),
            debug_prompt: None,
            debug_prompt_input,
            show_hidden,
            file_tree_op: None,
            file_tree_input,
            tree_state,
            tabs: Vec::new(),
            active_tab: 0,
            split_tab: None,
            tab_scroll: ScrollHandle::new(),
            tab_overflow_open: false,
            all_files_hash: hash_paths(&all_files),
            all_files,
            syncing_files: false,
            palette_open: false,
            palette_input,
            palette_selected: 0,
            find_open: false,
            find_input,
            replace_input,
            go_to_line_open: false,
            go_to_line_input,
            rename_target: None,
            rename_input,
            code_actions: None,
            peek: None,
            peek_focus: cx.focus_handle(),
            workspace_symbols: Vec::new(),
            symbols_indexing: false,
            history: None,
            autosave: file_state::AutosaveMode::Off,
            test_run: None,
            save_options: editor_view::SaveOptions::default(),
            edit_marks: HashMap::new(),
            file_notices: HashMap::new(),
            unsaved_prompt: None,
            disk_tick: 0,
            terminals: vec![initial_terminal],
            active_terminal: 0,
            next_terminal_id: 1,
            terminal_focus,
            root_focus,
            terminal_tx,
            devices: vec![default_device.clone()],
            ios_simulators: Vec::new(),
            selected_device: Some(default_device),
            device_picker_open: false,
            sim_list_expanded: false,
            launch_configs: Vec::new(),
            test_launch_templates: Vec::new(),
            selected_launch_config: None,
            launch_picker_open: false,
            flutter_daemon: None,
            flutter_app_id: None,
            flutter_ws_uri: None,
            flutter_status: None,
            perf_overlay_on: false,
            debug_paint_on: false,
            slow_animations_on: false,
            settings_open: false,
            settings_tab: 0,
            lsp_servers: Vec::new(),
            settings_search,
            about_open: false,
            light_theme: false,
            add_package_open: false,
            add_package_input,
            _subscriptions: Vec::new(),
        };

        if this.has_workspace {
            this.restore_session_state(window, cx);
            this.reload_launch_configs(cx);
            if this.is_flutter_or_dart_workspace() {
                this.refresh_devices(cx);
            }
            this.refresh_git(cx);
        } else {
            this.restore_preferences_only(window, cx);
        }
        this.apply_save_options(cx);
        this.start_git_refresh_loop(cx);
        // The Commit button depends on whether the message box is empty.
        cx.observe(&this.git.message, |_, _, cx| cx.notify()).detach();
        cx.observe(&this.git.branch_input, |_, _, cx| cx.notify())
            .detach();
        cx.observe(&this.settings_search, |_, _, cx| cx.notify())
            .detach();

        let palette_changes = cx.subscribe(
            &this.palette_input,
            |this, _, event: &gpui_kit::component::input::InputEvent, cx| {
                if matches!(event, gpui_kit::component::input::InputEvent::Change) {
                    this.palette_selected = 0;
                    cx.notify();
                }
            },
        );
        let find_changes = cx.subscribe(
            &this.find_input,
            |this, state, event: &gpui_kit::component::input::InputEvent, cx| {
                if matches!(event, gpui_kit::component::input::InputEvent::Change) {
                    let query = state.read(cx).value().to_string();
                    if let Some(editor) = this.active_editor().cloned() {
                        editor.update(cx, |ed, cx| ed.set_search_query(&query, cx));
                    }
                    cx.notify();
                }
            },
        );
        this._subscriptions.push(palette_changes);
        this._subscriptions.push(find_changes);
        let search_changes = cx.subscribe(
            &this.project_search_input,
            |this, state, event: &gpui_kit::component::input::InputEvent, cx| {
                if matches!(event, gpui_kit::component::input::InputEvent::Change) {
                    let query = state.read(cx).value().trim().to_string();
                    this.run_project_search(query, cx);
                }
            },
        );
        this._subscriptions.push(search_changes);
        // Agent composer: typing `@name` opens a live file picker; picking one turns
        // the text into an inline pill. (Searching the picker resets its highlight below.)
        let mention_changes = cx.subscribe_in(
            &this.agent.input,
            window,
            |this, _state, event: &gpui_kit::component::input::InputEvent, _window, cx| {
                if matches!(event, gpui_kit::component::input::InputEvent::Change) {
                    this.update_mention_state(cx);
                }
            },
        );
        this._subscriptions.push(mention_changes);
        for search in [this.agent.attach_search.clone(), this.agent.model_search.clone()] {
            let sub = cx.subscribe(
                &search,
                |this, _state, event: &gpui_kit::component::input::InputEvent, cx| {
                    if matches!(event, gpui_kit::component::input::InputEvent::Change) {
                        this.agent.attach_selected = 0;
                        this.agent.model_selected = 0;
                        cx.notify();
                    }
                },
            );
            this._subscriptions.push(sub);
        }

        // Files: autosave and outside-change checks, twice a second.
        cx.spawn(async move |weak, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
                if weak.update(cx, |this, cx| this.tick_files(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        // Leaving Shame can autosave; coming back re-checks what changed meanwhile.
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.check_disk_changes(cx);
            } else if this.autosave == file_state::AutosaveMode::OnFocusChange {
                this.save_all(cx);
            }
        });
        this._subscriptions.push(activation);
        // The red close button must not throw away unsaved work either.
        let this_view = cx.entity();
        window.on_window_should_close(cx, move |_window, cx| {
            this_view.update(cx, |this, cx| {
                let tabs = this.unsaved_tabs(cx);
                if tabs.is_empty() {
                    true
                } else {
                    this.unsaved_prompt = Some(file_state::UnsavedPrompt {
                        tabs,
                        then: file_state::AfterUnsaved::Quit,
                    });
                    cx.notify();
                    false
                }
            })
        });
        this
    }

    fn watch_editor(
        &mut self,
        editor: &Entity<EditorView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let subscription =
            cx.subscribe_in(editor, window, |this, editor, event, window, cx| match event {
                EditorEvent::OpenAt { path, line, column } => {
                    let (line, column) = (*line, *column);
                    this.open_path(path.clone(), window, cx);
                    if let Some(target) = this.active_editor().cloned() {
                        target.update(cx, |editor, cx| {
                            editor.set_cursor_to_position(line, column, cx)
                        });
                    }
                }
                EditorEvent::RequestRename { line, column } => {
                    this.rename_target = Some((editor.clone(), *line, *column));
                    this.rename_input.update(cx, |state, cx| {
                        state.set_value("", window, cx);
                        state.focus(window, cx);
                    });
                    cx.notify();
                }
                EditorEvent::ShowCodeActions { actions } => {
                    this.code_actions = Some((editor.clone(), actions.clone()));
                    cx.notify();
                }
                EditorEvent::ShowPeek { title, locations } => {
                    this.open_peek(title.clone(), locations, window, cx);
                }
                EditorEvent::ExternalFileEdits { path, edits } => {
                    this.apply_external_file_edits(path.clone(), edits.clone(), cx);
                }
                EditorEvent::BreakpointsChanged { path, lines } => {
                    if lines.is_empty() {
                        this.dap_breakpoints.remove(path);
                    } else {
                        this.dap_breakpoints.insert(path.clone(), lines.clone());
                    }
                    this.send_breakpoints(path);
                    cx.notify();
                }
                EditorEvent::SaveFailed { path, message } => {
                    this.file_notices.insert(
                        path.clone(),
                        file_state::FileNotice::SaveFailed(message.clone()),
                    );
                    cx.notify();
                }
                EditorEvent::Saved { path } => {
                    this.file_notices.remove(path);
                    this.record_history_snapshot(path.clone());
                    this.refresh_git(cx);
                    if this.web_preview_open {
                        this.refresh_web_preview(cx);
                    }
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if name.eq_ignore_ascii_case("launch.json") {
                        this.reload_launch_configs(cx);
                    } else if ext == "dart" && this.flutter_app_id.is_some() {
                        this.trigger_hot_reload(cx);
                    } else if name.eq_ignore_ascii_case("pubspec.yaml") {
                        this.run_pub_get(cx);
                    }
                    cx.notify();
                }
                EditorEvent::RunTest {
                    path,
                    test_name,
                    extra_args,
                    label,
                } => {
                    this.run_flutter_test(
                        path.clone(),
                        test_name.clone(),
                        extra_args.clone(),
                        label.clone(),
                        cx,
                    );
                }
            });
        self._subscriptions.push(subscription);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let primary_mod = keystroke.modifiers.platform || keystroke.modifiers.control;

        if primary_mod && keystroke.key.as_str() == "," {
            self.toggle_settings(cx);
            return;
        }

        if primary_mod && keystroke.modifiers.shift && keystroke.key.as_str() == "o" {
            self.open_symbol_outline(window, cx);
            return;
        }

        // Reviewing a diff: ⌥↓/⌥↑ step through changes, ⌥→/⌥← through files,
        // ⌥↩ marks the file reviewed.
        if self.diff.is_some() && keystroke.modifiers.alt && !primary_mod {
            let handled = match keystroke.key.as_str() {
                "down" => Some(self.diff_goto_change(1, cx)),
                "up" => Some(self.diff_goto_change(-1, cx)),
                "right" => Some(self.diff_goto_file(1, cx)),
                "left" => Some(self.diff_goto_file(-1, cx)),
                "enter" => Some(self.diff_toggle_reviewed(cx)),
                _ => None,
            };
            if handled.is_some() {
                return;
            }
        }

        // ⌥⌘S: save every file with unsaved changes.
        if primary_mod && keystroke.modifiers.alt && keystroke.key.as_str() == "s" {
            self.save_all(cx);
            return;
        }

        // ⌘L: send the selection (or the whole file) to the agent chat.
        if primary_mod && !keystroke.modifiers.shift && keystroke.key.as_str() == "l" {
            self.attach_editor_context(window, cx);
            return;
        }

        if primary_mod && !keystroke.modifiers.shift && keystroke.key.as_str() == "t" {
            self.open_workspace_symbols(window, cx);
            return;
        }

        if primary_mod && keystroke.key.as_str() == "o" {
            self.open_folder(window, cx);
            return;
        }

        if primary_mod && keystroke.key.as_str() == "n" {
            self.new_scratch_tab(window, cx);
            return;
        }

        if primary_mod && keystroke.key.as_str() == "w" {
            if !self.tabs.is_empty() {
                self.request_close_tab(self.active_tab, cx);
            }
            return;
        }

        if primary_mod && keystroke.key.as_str() == "p" {
            self.palette_open = true;
            self.palette_selected = 0;
            let shift = keystroke.modifiers.shift;
            self.palette_input.update(cx, |state, cx| {
                if shift {
                    state.set_value("> ", window, cx);
                } else if state.value().trim_start().starts_with('>') {
                    state.set_value("", window, cx);
                }
                state.focus(window, cx);
            });
            cx.notify();
            return;
        }

        if primary_mod && keystroke.modifiers.shift && keystroke.key.as_str() == "m" {
            if self.terminal_open && self.dock_tab == DockTab::Problems {
                self.terminal_open = false;
            } else {
                self.terminal_open = true;
                self.dock_tab = DockTab::Problems;
            }
            cx.notify();
            return;
        }

        if primary_mod && keystroke.modifiers.shift && keystroke.key.as_str() == "f" {
            self.sidebar_open = true;
            self.set_sidebar_tab(git_panel::SidebarTab::Search, cx);
            self.project_search_input
                .update(cx, |state, cx| state.focus(window, cx));
            cx.notify();
            return;
        }

        if primary_mod && !keystroke.modifiers.shift && keystroke.key.as_str() == "f" {
            if self.active_editor().is_some() {
                self.find_open = true;
                let query = self.find_input.read(cx).value().to_string();
                if let Some(editor) = self.active_editor().cloned() {
                    if !query.is_empty() {
                        editor.update(cx, |ed, cx| ed.set_search_query(&query, cx));
                    }
                }
                self.find_input
                    .update(cx, |state, cx| state.focus(window, cx));
                cx.notify();
            }
            return;
        }

        if primary_mod && keystroke.modifiers.shift && keystroke.key.as_str() == "g" {
            if self.sidebar_open && self.sidebar_tab == git_panel::SidebarTab::Git {
                self.sidebar_open = false;
            } else {
                self.sidebar_open = true;
                self.set_sidebar_tab(git_panel::SidebarTab::Git, cx);
            }
            cx.notify();
            return;
        }

        if primary_mod && matches!(keystroke.key.as_str(), "l" | "g") {
            if self.active_editor().is_some() {
                self.open_go_to_line(window, cx);
            }
            return;
        }

        if primary_mod && keystroke.key.as_str() == "b" {
            self.sidebar_open = !self.sidebar_open;
            cx.notify();
            return;
        }

        if primary_mod && keystroke.key.as_str() == "j" {
            self.terminal_open = !self.terminal_open;
            cx.notify();
            return;
        }

        if primary_mod && keystroke.modifiers.shift && keystroke.key.as_str() == "v" {
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                if tab.is_markdown() {
                    tab.md_mode = match tab.md_mode {
                        MarkdownMode::Code => MarkdownMode::Split,
                        MarkdownMode::Split => MarkdownMode::Preview,
                        MarkdownMode::Preview => MarkdownMode::Code,
                    };
                    cx.notify();
                    return;
                }
            }
        }

        if self.palette_open {
            match keystroke.key.as_str() {
                "escape" => {
                    self.palette_open = false;
                    cx.notify();
                }
                "up" => {
                    self.palette_selected = self.palette_selected.saturating_sub(1);
                    cx.notify();
                }
                "down" => {
                    let count = if self.palette_is_workspace_symbol_mode(cx) {
                        self.palette_workspace_symbol_matches(cx).len()
                    } else if self.palette_is_symbol_mode(cx) {
                        self.palette_symbol_matches(cx).len()
                    } else if self.palette_is_command_mode(cx) {
                        self.palette_command_matches(cx).len()
                    } else {
                        self.palette_matches(cx).len()
                    };
                    let last = count.saturating_sub(1);
                    self.palette_selected = (self.palette_selected + 1).min(last);
                    cx.notify();
                }
                "enter" => self.open_palette_selection(window, cx),
                _ => {}
            }
            return;
        }

        if keystroke.key.as_str() == "escape" && self.diff.is_some() {
            self.close_diff(cx);
        }
    }
}

impl Render for DartIde {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();

        // Whatever held focus can disappear (last tab closed, folder closed, a
        // modal dismissed); fall back to the window so menu actions and
        // shortcuts keep working with no editor open.
        if window.focused(cx).is_none() {
            window.focus(&self.root_focus, cx);
        }

        v_flex()
            .size_full()
            .relative()
            .key_context("DartIde")
            .track_focus(&self.root_focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(|this, _: &AboutShame, _w, cx| {
                this.about_open = true;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, _w, cx| {
                this.toggle_settings(cx);
            }))
            .on_action(cx.listener(|this, _: &QuitShame, _w, cx| {
                this.request_quit(cx);
            }))
            .on_action(cx.listener(|this, _: &MenuOpenFolder, window, cx| {
                this.open_folder(window, cx);
            }))
            .on_action(cx.listener(|this, _: &MenuCloseFolder, _w, cx| {
                this.request_close_workspace(cx);
            }))
            .on_action(cx.listener(|this, _: &MenuNewFile, window, cx| {
                this.new_scratch_tab(window, cx);
            }))
            .on_action(cx.listener(|this, _: &MenuSaveFile, _w, cx| {
                if let Some(ed) = this.active_editor().cloned() {
                    ed.update(cx, |editor, cx| editor.save(cx));
                }
            }))
            .on_action(cx.listener(|this, _: &MenuCloseTab, _w, cx| {
                if !this.tabs.is_empty() {
                    this.request_close_tab(this.active_tab, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &MenuGoToFile, window, cx| {
                this.palette_open = true;
                this.palette_selected = 0;
                this.palette_input
                    .update(cx, |s, cx| {
                        s.set_value("", window, cx);
                        s.focus(window, cx);
                    });
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &MenuCommandPalette, window, cx| {
                this.palette_open = true;
                this.palette_selected = 0;
                this.palette_input
                    .update(cx, |s, cx| {
                        s.set_value("> ", window, cx);
                        s.focus(window, cx);
                    });
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &MenuFindInFile, window, cx| {
                this.find_open = true;
                this.find_input.update(cx, |s, cx| s.focus(window, cx));
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &MenuFindInProject, window, cx| {
                this.execute_ide_command("project-search", window, cx);
            }))
            .on_action(cx.listener(|this, _: &MenuFormatDoc, window, cx| {
                this.execute_ide_command("format-doc", window, cx);
            }))
            .on_action(cx.listener(|this, _: &MenuToggleSidebar, _w, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &MenuToggleTerminal, _w, cx| {
                this.terminal_open = !this.terminal_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &MenuToggleProblems, window, cx| {
                this.execute_ide_command("toggle-problems", window, cx);
            }))
            .on_action(cx.listener(|this, _: &MenuToggleAgent, _w, cx| {
                this.agent_open = !this.agent_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &MenuToggleWebPreview, window, cx| {
                this.toggle_web_preview(window, cx);
            }))
            .on_action(cx.listener(|this, _: &MenuSplitEditor, _w, cx| {
                this.toggle_split_editor(cx);
            }))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_titlebar(window, cx))
            .child(self.render_shell(&view, window, cx))
            .child(self.render_status_bar(cx))
            .when_some(self.render_branch_switcher_modal(&view, cx), |el, modal| {
                el.child(deferred(modal).with_priority(100))
            })
            .when_some(self.render_settings_modal(&view, cx), |el, modal| {
                el.child(deferred(modal).with_priority(100))
            })
            .when_some(self.render_about_modal(&view, cx), |el, modal| {
                el.child(deferred(modal).with_priority(100))
            })
            .when_some(self.render_launch_picker_popover(&view, cx), |el, pop| {
                el.child(deferred(pop).with_priority(100))
            })
            .when_some(self.render_device_picker_popover(&view, cx), |el, pop| {
                el.child(deferred(pop).with_priority(100))
            })
            .when_some(self.render_palette(&view, cx), |el, palette| {
                el.child(deferred(palette).with_priority(100))
            })
            .when_some(self.render_go_to_line_prompt(&view, cx), |el, prompt| {
                el.child(deferred(prompt).with_priority(100))
            })
            .when_some(self.render_add_package_prompt(&view, cx), |el, prompt| {
                el.child(deferred(prompt).with_priority(100))
            })
            .when_some(self.render_rename_prompt(&view, cx), |el, prompt| {
                el.child(deferred(prompt).with_priority(100))
            })
            .when_some(self.render_code_actions(&view, cx), |el, picker| {
                el.child(deferred(picker).with_priority(100))
            })
            .when_some(self.render_peek(cx), |el, peek| {
                el.child(deferred(peek).with_priority(100))
            })
            .when_some(self.render_unsaved_prompt(cx), |el, prompt| {
                el.child(deferred(prompt).with_priority(120))
            })
            .when_some(self.render_local_history(&view, cx), |el, picker| {
                el.child(deferred(picker).with_priority(100))
            })
            .when_some(self.render_debug_prompt(&view, cx), |el, picker| {
                el.child(deferred(picker).with_priority(100))
            })
    }
}

fn main() {
    let app = gpui_kit::application().with_assets(AllAssets);

    app.run(move |cx| {
        gpui_kit::init(cx);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
        theme::apply(cx);

        cx.bind_keys([
            KeyBinding::new("cmd-,", OpenSettings, None),
            KeyBinding::new("cmd-q", QuitShame, None),
            KeyBinding::new("cmd-o", MenuOpenFolder, None),
            KeyBinding::new("cmd-n", MenuNewFile, None),
            KeyBinding::new("cmd-s", MenuSaveFile, None),
            KeyBinding::new("cmd-w", MenuCloseTab, None),
        ]);

        cx.set_menus(vec![
            Menu {
                name: "Shame".into(),
                items: vec![
                    MenuItem::action("About Shame", AboutShame),
                    MenuItem::separator(),
                    MenuItem::action("Settings\u{2026}", OpenSettings),
                    MenuItem::separator(),
                    MenuItem::os_submenu("Services", SystemMenuType::Services),
                    MenuItem::separator(),
                    MenuItem::action("Quit Shame", QuitShame),
                ],
                disabled: false,
            },
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("New Scratch File", MenuNewFile),
                    MenuItem::action("Open Folder\u{2026}", MenuOpenFolder),
                    MenuItem::separator(),
                    MenuItem::action("Save", MenuSaveFile),
                    MenuItem::action("Close Tab", MenuCloseTab),
                    MenuItem::action("Close Folder", MenuCloseFolder),
                ],
                disabled: false,
            },
            Menu {
                name: "Edit".into(),
                items: vec![
                    MenuItem::action("Find in File\u{2026}", MenuFindInFile),
                    MenuItem::action("Find in Project\u{2026}", MenuFindInProject),
                    MenuItem::separator(),
                    MenuItem::action("Format Document", MenuFormatDoc),
                ],
                disabled: false,
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Command Palette\u{2026}", MenuCommandPalette),
                    MenuItem::action("Go to File\u{2026}", MenuGoToFile),
                    MenuItem::separator(),
                    MenuItem::action("Toggle Left Sidebar", MenuToggleSidebar),
                    MenuItem::action("Toggle Terminal Dock", MenuToggleTerminal),
                    MenuItem::action("Toggle Problems Panel", MenuToggleProblems),
                    MenuItem::action("Toggle AI Agent Rail", MenuToggleAgent),
                    MenuItem::action("Toggle Web Preview Rail", MenuToggleWebPreview),
                    MenuItem::separator(),
                    MenuItem::action("Split Editor Right", MenuSplitEditor),
                ],
                disabled: false,
            },
            Menu {
                name: "Help".into(),
                items: vec![MenuItem::action("About Shame", AboutShame)],
                disabled: false,
            },
        ]);

        cx.set_dock_menu(vec![
            MenuItem::action("Open Folder\u{2026}", MenuOpenFolder),
            MenuItem::action("New Scratch File", MenuNewFile),
            MenuItem::separator(),
            MenuItem::action("Settings\u{2026}", OpenSettings),
            MenuItem::action("About Shame", AboutShame),
        ]);

        let bounds = Bounds::centered(None, size(px(1600.), px(1000.)), cx);

        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(SharedString::from("Shame")),
                appears_transparent: true,
                traffic_light_position: Some(point(px(12.), px(12.))),
            }),
            ..Default::default()
        };

        gpui_kit::open_window(window_options, cx, |window, cx| {
            cx.new(|cx| DartIde::new(window, cx))
        })
        .expect("Failed to open window");
        cx.activate(true);
    });
}
