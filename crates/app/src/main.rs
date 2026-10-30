//! A GPUI window with a dock-style split layout: a file tree panel reading
//! the real project directory, and our own `editor-view` (M1) in place of
//! the M0 `Textarea` stand-in.
//!
//! See `NOTES.md` at the repo root for the measured startup time, idle RAM,
//! and binary size from the M0 spike.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

mod chrome;
mod diff_view;
mod editor;
mod git_panel;
mod rails;
mod shell;
mod simulator;
mod theme;
#[allow(dead_code)]
mod tokens;

use core_buffer::Buffer;
use core_lsp::lsp_types;
use editor_view::{EditorEvent, EditorView};
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

/// Directory/file names we never want to show in the file tree.
pub(crate) const IGNORED_NAMES: &[&str] = &["target", "node_modules", ".DS_Store"];

/// Every file under `dir`, for the Quick Open palette.
fn flatten_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    let Ok(entries) = std::fs::read_dir(dir) else {
        return files;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == ".git" || IGNORED_NAMES.contains(&name) {
            continue;
        }
        if path.is_dir() {
            files.extend(flatten_files(&path));
        } else {
            files.push(path);
        }
    }

    files
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
}

#[derive(Clone, Copy)]
pub(crate) struct IdeCommand {
    pub(crate) id: &'static str,
    pub(crate) title: &'static str,
    pub(crate) shortcut: &'static str,
}

const IDE_COMMANDS: &[IdeCommand] = &[
    IdeCommand { id: "run-flutter", title: "Flutter: Run App on Selected Device", shortcut: "" },
    IdeCommand { id: "hot-reload", title: "Flutter: Hot Reload", shortcut: "\u{2318}S" },
    IdeCommand { id: "hot-restart", title: "Flutter: Hot Restart", shortcut: "" },
    IdeCommand { id: "pub-get", title: "Flutter: Run Pub Get", shortcut: "" },
    IdeCommand { id: "add-package", title: "Flutter: Add Package (pub.dev)\u{2026}", shortcut: "" },
    IdeCommand { id: "debug-dap", title: "Debug: Start Dart DAP Debugger", shortcut: "" },
    IdeCommand { id: "toggle-sim", title: "Simulator: Attach / Detach iOS Simulator", shortcut: "" },
    IdeCommand { id: "open-git", title: "Git: Open Git Panel", shortcut: "\u{2318}\u{21e7}G" },
    IdeCommand { id: "git-stage-all", title: "Git: Stage All Changes", shortcut: "" },
    IdeCommand { id: "git-unstage-all", title: "Git: Unstage All Changes", shortcut: "" },
    IdeCommand { id: "git-stash", title: "Git: Stash Uncommitted Changes", shortcut: "" },
    IdeCommand { id: "git-uncommit", title: "Git: Undo Last Commit (Keep Staged)", shortcut: "" },
    IdeCommand { id: "diff-unified", title: "Diff: Toggle Split / Unified View", shortcut: "" },
    IdeCommand { id: "project-search", title: "Search: Find in Project", shortcut: "\u{2318}\u{21e7}F" },
    IdeCommand { id: "toggle-problems", title: "View: Toggle Problems Panel", shortcut: "\u{2318}\u{21e7}M" },
    IdeCommand { id: "toggle-terminal", title: "View: Toggle Terminal Dock", shortcut: "\u{2318}J" },
    IdeCommand { id: "toggle-sidebar", title: "View: Toggle Left Sidebar", shortcut: "\u{2318}B" },
    IdeCommand { id: "toggle-agent", title: "View: Toggle Agent Rail", shortcut: "" },
    IdeCommand { id: "format-doc", title: "Editor: Format Document", shortcut: "\u{21e7}\u{2318}F" },
    IdeCommand { id: "goto-line", title: "Editor: Go to Line\u{2026}", shortcut: "\u{2318}G" },
    IdeCommand { id: "new-file", title: "File: New Scratch File", shortcut: "\u{2318}N" },
    IdeCommand { id: "open-folder", title: "File: Open Folder\u{2026}", shortcut: "\u{2318}O" },
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
    pub(crate) agent_open: bool,
    pub(crate) agent: rails::agent::AgentState,
    pub(crate) project_search_input: Entity<InputState>,
    pub(crate) project_search_matches: Vec<core_workspace::SearchMatch>,
    pub(crate) dap_client: Option<std::sync::Arc<std::sync::Mutex<core_dap::DapClient>>>,
    pub(crate) dap_breakpoints: HashMap<PathBuf, Vec<u32>>,
    pub(crate) dap_paused_thread: Option<i64>,
    pub(crate) dap_stack_summary: Option<String>,
    pub(crate) dap_variables: Vec<(String, String)>,
    pub(crate) show_hidden: bool,
    pub(crate) tree_state: Entity<TreeState>,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active_tab: usize,
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
    pub(crate) go_to_line_open: bool,
    pub(crate) go_to_line_input: Entity<InputState>,
    pub(crate) rename_target: Option<(Entity<EditorView>, u32, u32)>,
    pub(crate) rename_input: Entity<InputState>,
    pub(crate) code_actions: Option<(Entity<EditorView>, Vec<lsp_types::CodeActionOrCommand>)>,
    pub(crate) terminals: Vec<editor::dock::TerminalSession>,
    pub(crate) active_terminal: usize,
    pub(crate) next_terminal_id: usize,
    pub(crate) terminal_focus: FocusHandle,
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
    pub(crate) flutter_status: Option<String>,
    pub(crate) add_package_open: bool,
    pub(crate) add_package_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl DartIde {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let show_hidden = true;
        let items = build_tree_items(&root, show_hidden);
        let all_files = flatten_files(&root);

        let tree_state = cx.new(|cx| TreeState::new(cx).items(items));
        let palette_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Go to file, or type > for commands\u{2026}")
        });
        let find_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Find in file..."));
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
        let agent = rails::agent::AgentState::new(window, cx);

        // Filled in by the first `refresh_git` (off the UI thread) right after construction.
        let git_branch = None;
        let git_status = HashMap::new();
        let (terminal_tx, terminal_rx) = async_channel::unbounded();
        let initial_terminal = editor::dock::TerminalSession::new(0, root.clone(), true);
        let terminal_focus = cx.focus_handle();

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
            root,
            git_branch,
            git_status,
            sidebar_open: true,
            terminal_open: true,
            terminal_maximized: false,
            dock_tab: DockTab::Terminal,
            debug_lines: Vec::new(),
            debug_scroll: ScrollHandle::new(),
            diff: None,
            diff_seq: 0,
            sidebar_tab: git_panel::SidebarTab::Files,
            git: git_panel::GitState::new(window, cx),
            sim: Default::default(),
            agent_open: false,
            agent,
            project_search_input,
            project_search_matches: Vec::new(),
            dap_client: None,
            dap_breakpoints: HashMap::new(),
            dap_paused_thread: None,
            dap_stack_summary: None,
            dap_variables: Vec::new(),
            show_hidden,
            tree_state,
            tabs: Vec::new(),
            active_tab: 0,
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
            go_to_line_open: false,
            go_to_line_input,
            rename_target: None,
            rename_input,
            code_actions: None,
            terminals: vec![initial_terminal],
            active_terminal: 0,
            next_terminal_id: 1,
            terminal_focus,
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
            flutter_status: None,
            add_package_open: false,
            add_package_input,
            _subscriptions: Vec::new(),
        };

        this.reload_launch_configs(cx);
        this.refresh_devices(cx);
        this.start_git_refresh_loop(cx);
        this.refresh_git(cx);
        // The Commit button depends on whether the message box is empty.
        cx.observe(&this.git.message, |_, _, cx| cx.notify()).detach();
        cx.observe(&this.git.branch_input, |_, _, cx| cx.notify())
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
        this
    }

    pub(crate) fn run_project_search(&mut self, query: String, cx: &mut Context<Self>) {
        if query.is_empty() {
            self.project_search_matches.clear();
            cx.notify();
            return;
        }
        let root = self.root.clone();
        let q = query.clone();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(core_workspace::search_text(&root, &q));
        });
        cx.spawn(async move |weak, cx| {
            let Ok(matches) = rx.recv().await else { return };
            let _ = weak.update(cx, |this, cx| {
                if this.project_search_input.read(cx).value().trim() == query {
                    this.project_search_matches = matches;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Background check: if files were added, renamed, or deleted on disk outside the editor,
    /// refresh `all_files` and `tree_state` automatically without cloning `all_files` on the UI thread.
    pub(crate) fn sync_workspace_files_if_changed(&mut self, cx: &mut Context<Self>) {
        if self.syncing_files {
            return;
        }
        self.syncing_files = true;
        let root = self.root.clone();
        let prev_hash = self.all_files_hash;
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let files = flatten_files(&root);
            let h = hash_paths(&files);
            if h != prev_hash {
                let _ = tx.send_blocking(Some((files, h)));
            } else {
                let _ = tx.send_blocking(None);
            }
        });
        cx.spawn(async move |weak, cx| {
            let res = rx.recv().await;
            let _ = weak.update(cx, |this, cx| {
                this.syncing_files = false;
                if let Ok(Some((files, h))) = res {
                    let items = build_tree_items(&this.root, this.show_hidden);
                    this.all_files = files;
                    this.all_files_hash = h;
                    this.tree_state.update(cx, |state, cx| {
                        state.set_items(items, cx);
                    });
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn palette_is_command_mode(&self, cx: &Context<Self>) -> bool {
        self.palette_input
            .read(cx)
            .value()
            .trim_start()
            .starts_with('>')
    }

    fn palette_command_matches(&self, cx: &Context<Self>) -> Vec<IdeCommand> {
        let raw = self.palette_input.read(cx).value().to_string();
        let query = raw
            .trim_start()
            .strip_prefix('>')
            .unwrap_or(&raw)
            .trim()
            .to_lowercase();
        IDE_COMMANDS
            .iter()
            .filter(|cmd| query.is_empty() || cmd.title.to_lowercase().contains(&query))
            .copied()
            .collect()
    }

    fn palette_matches(&self, cx: &Context<Self>) -> Vec<PathBuf> {
        let query = self.palette_input.read(cx).value().to_lowercase();
        self.all_files
            .iter()
            .filter(|path| {
                query.is_empty() || path.to_string_lossy().to_lowercase().contains(&query)
            })
            .take(20)
            .cloned()
            .collect()
    }

    fn open_palette_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette_is_command_mode(cx) {
            let cmds = self.palette_command_matches(cx);
            if let Some(cmd) = cmds.get(self.palette_selected).copied() {
                self.palette_open = false;
                self.execute_ide_command(cmd.id, window, cx);
            }
            return;
        }
        let matches = self.palette_matches(cx);
        let Some(path) = matches.get(self.palette_selected).cloned() else {
            return;
        };
        self.open_path(path, window, cx);
        self.palette_open = false;
        cx.notify();
    }

    pub(crate) fn execute_ide_command(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match id {
            "run-flutter" => self.start_flutter_run(window, cx),
            "hot-reload" => self.trigger_hot_reload(cx),
            "hot-restart" => self.trigger_hot_restart(cx),
            "pub-get" => self.run_pub_get(cx),
            "add-package" => self.open_add_package(window, cx),
            "debug-dap" => self.start_dap_debug(window, cx),
            "toggle-sim" => {
                if self.sim.attached {
                    self.detach_simulator(cx);
                } else {
                    self.attach_simulator(window, cx);
                }
            }
            "open-git" => {
                self.sidebar_open = true;
                self.set_sidebar_tab(git_panel::SidebarTab::Git, cx);
            }
            "git-stage-all" => self.git_stage_all(window, cx),
            "git-unstage-all" => self.git_unstage_all(window, cx),
            "git-stash" => self.git_stash_save(window, cx),
            "git-uncommit" => self.git_uncommit_last(window, cx),
            "diff-unified" => self.toggle_diff_unified(cx),
            "project-search" => {
                self.sidebar_open = true;
                self.set_sidebar_tab(git_panel::SidebarTab::Search, cx);
                self.project_search_input
                    .update(cx, |s, cx| s.focus(window, cx));
            }
            "toggle-problems" => {
                if self.terminal_open && self.dock_tab == DockTab::Problems {
                    self.terminal_open = false;
                } else {
                    self.terminal_open = true;
                    self.dock_tab = DockTab::Problems;
                }
            }
            "toggle-terminal" => self.terminal_open = !self.terminal_open,
            "toggle-sidebar" => self.sidebar_open = !self.sidebar_open,
            "toggle-agent" => self.agent_open = !self.agent_open,
            "format-doc" => {
                if let Some(ed) = self.active_editor().cloned() {
                    ed.update(cx, |editor, _cx| {
                        editor.request_format();
                    });
                }
            }
            "goto-line" => self.open_go_to_line(window, cx),
            "new-file" => self.new_scratch_tab(window, cx),
            "open-folder" => self.open_folder(window, cx),
            _ => {}
        }
        cx.notify();
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
                EditorEvent::ExternalFileEdits { path, edits } => {
                    this.apply_external_file_edits(path.clone(), edits.clone(), cx);
                }
                EditorEvent::BreakpointsChanged { path, lines } => {
                    if lines.is_empty() {
                        this.dap_breakpoints.remove(path);
                    } else {
                        this.dap_breakpoints.insert(path.clone(), lines.clone());
                    }
                    if let Some(dap) = &this.dap_client {
                        if let Ok(guard) = dap.lock() {
                            let _ = guard.set_breakpoints(&path.to_string_lossy(), lines);
                        }
                    }
                    cx.notify();
                }
                EditorEvent::Saved { path } => {
                    this.refresh_git(cx);
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

    fn render_rename_prompt(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> Option<Div> {
        self.rename_target.as_ref()?;

        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(84.))
                .bg(rgba(0x00000066))
                .child(
                    v_flex()
                        .w(px(360.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .child(
                            div()
                                .px_3()
                                .py_2()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Rename symbol"),
                        )
                        .child(
                            div()
                                .px_3()
                                .pb_3()
                                .on_key_down({
                                    let view = view.clone();
                                    move |event, window, cx| {
                                        let key = event.keystroke.key.as_str();
                                        if key == "escape" {
                                            view.update(cx, |this, cx| {
                                                this.rename_target = None;
                                                cx.notify();
                                            });
                                        } else if key == "enter" {
                                            view.update(cx, |this, cx| {
                                                this.submit_rename(window, cx)
                                            });
                                        }
                                    }
                                })
                                .child(
                                    h_flex()
                                        .h(px(28.))
                                        .px_2p5()
                                        .items_center()
                                        .rounded(px(tokens::RADIUS_SM))
                                        .bg(cx.theme().background)
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .child(
                                            Input::new(&self.rename_input)
                                                .appearance(false)
                                                .small(),
                                        ),
                                ),
                        ),
                ),
        )
    }

    fn render_code_actions(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> Option<Div> {
        let (_, actions) = self.code_actions.as_ref()?;

        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(84.))
                .bg(rgba(0x00000066))
                .on_key_down({
                    let view = view.clone();
                    move |event, _window, cx| {
                        if event.keystroke.key.as_str() == "escape" {
                            view.update(cx, |this, cx| {
                                this.code_actions = None;
                                cx.notify();
                            });
                        }
                    }
                })
                .child(
                    v_flex()
                        .w(px(420.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .children(actions.iter().enumerate().map(|(ix, action)| {
                            let title = match action {
                                lsp_types::CodeActionOrCommand::Command(c) => c.title.clone(),
                                lsp_types::CodeActionOrCommand::CodeAction(a) => a.title.clone(),
                            };
                            div()
                                .id(("code-action", ix))
                                .px_3()
                                .py_2()
                                .text_sm()
                                .cursor_pointer()
                                .hover(|s| s.bg(cx.theme().list_hover))
                                .child(title)
                                .on_click({
                                    let view = view.clone();
                                    move |_event, _window, cx| {
                                        view.update(cx, |this, cx| this.apply_code_action(ix, cx));
                                    }
                                })
                        })),
                ),
        )
    }

    fn apply_code_action(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some((editor, mut actions)) = self.code_actions.take() else {
            return;
        };
        if index < actions.len() {
            let action = actions.remove(index);
            editor.update(cx, |editor, cx| editor.apply_code_action(action, cx));
        }
        cx.notify();
    }

    fn submit_rename(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some((editor, line, column)) = self.rename_target.take() else {
            return;
        };
        let new_name = self.rename_input.read(cx).value().to_string();
        if !new_name.is_empty() {
            editor.update(cx, |editor, _cx| editor.send_rename(line, column, new_name));
        }
        cx.notify();
    }

    pub(crate) fn open_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Folder".into()),
        });

        let view = cx.entity();
        cx.spawn_in(window, async move |_, window| {
            let root = paths.await.ok()?.ok()??.into_iter().next()?;
            window
                .update(|_window, cx| {
                    view.update(cx, |this, cx| this.set_root(root, cx));
                })
                .ok()
        })
        .detach();
    }

    fn set_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        // Stop any running Flutter daemon from the previous project
        self.stop_flutter_run(cx);

        // Close tabs and overlays from the previous project workspace
        self.tabs.clear();
        self.active_tab = 0;
        self.tab_overflow_open = false;
        self.device_picker_open = false;
        self.launch_picker_open = false;
        self.add_package_open = false;
        self.palette_open = false;
        self.find_open = false;
        self.go_to_line_open = false;
        self.rename_target = None;
        self.code_actions = None;

        // Kill any running terminal child processes from the old project and
        // start a fresh terminal session rooted at the new workspace
        for session in &mut self.terminals {
            if let Some(pid) = session.running_pid.take() {
                let _ = std::process::Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .status();
            }
        }
        let id = self.next_terminal_id;
        self.next_terminal_id += 1;
        self.terminals = vec![editor::dock::TerminalSession::new(id, root.clone(), true)];
        self.active_terminal = 0;
        self.debug_lines.clear();
        self.dock_tab = DockTab::Terminal;

        let items = build_tree_items(&root, self.show_hidden);
        self.all_files = flatten_files(&root);
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
        });
        self.root = root;
        // A different folder is a different repository.
        self.git.status = None;
        self.git.commit_types = None;
        self.git_branch = None;
        self.git_status.clear();
        self.diff = None;
        self.refresh_git(cx);
        self.reload_launch_configs(cx);
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let primary_mod = keystroke.modifiers.platform || keystroke.modifiers.control;

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
                self.close_tab(self.active_tab, cx);
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
                    let count = if self.palette_is_command_mode(cx) {
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

    fn render_palette(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> Option<Div> {
        if !self.palette_open {
            return None;
        }

        let is_cmd_mode = self.palette_is_command_mode(cx);
        let cmd_matches = if is_cmd_mode {
            self.palette_command_matches(cx)
        } else {
            Vec::new()
        };
        let matches = if is_cmd_mode {
            Vec::new()
        } else {
            self.palette_matches(cx)
        };
        let total_items = if is_cmd_mode {
            cmd_matches.len()
        } else {
            matches.len()
        };
        let selected = self.palette_selected.min(total_items.saturating_sub(1));

        let rows: Vec<AnyElement> = if is_cmd_mode {
            cmd_matches
                .into_iter()
                .enumerate()
                .map(|(ix, cmd)| {
                    let is_selected = ix == selected;
                    let cmd_id = cmd.id;
                    let view = view.clone();
                    h_flex()
                        .id(("palette-cmd", ix))
                        .relative()
                        .w_full()
                        .h(px(30.))
                        .items_center()
                        .justify_between()
                        .px_3()
                        .rounded(px(tokens::RADIUS_SM))
                        .border_1()
                        .border_color(gpui_kit::transparent_black())
                        .cursor_pointer()
                        .when(is_selected, |el| {
                            el.bg(rgba(0xea7a3c16))
                                .border_color(rgba(0xea7a3c36))
                                .child(
                                    div()
                                        .absolute()
                                        .left(px(2.))
                                        .top(px(6.))
                                        .bottom(px(6.))
                                        .w(px(2.5))
                                        .rounded(px(2.))
                                        .bg(cx.theme().primary),
                                )
                        })
                        .when(!is_selected, |el| el.hover(|s| s.bg(rgba(0xffffff08))))
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .min_w(px(0.))
                                .child(
                                    gpui_kit::component::Icon::new(
                                        gpui_kit::assets::IconName::Sparkles,
                                    )
                                    .with_size(px(12.))
                                    .text_color(cx.theme().primary),
                                )
                                .child(
                                    div()
                                        .text_size(px(tokens::UI))
                                        .text_color(cx.theme().foreground)
                                        .child(cmd.title),
                                ),
                        )
                        .when(!cmd.shortcut.is_empty(), |el| {
                            el.child(
                                div()
                                    .px_1p5()
                                    .py_0p5()
                                    .rounded(px(4.))
                                    .bg(cx.theme().background)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .font_family("SF Mono")
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(cmd.shortcut),
                            )
                        })
                        .on_click(move |_event, window, cx| {
                            view.update(cx, |this, cx| {
                                this.palette_open = false;
                                this.execute_ide_command(cmd_id, window, cx);
                            });
                        })
                        .into_any_element()
                })
                .collect()
        } else {
            matches
                .into_iter()
                .enumerate()
                .map(|(ix, path)| {
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default()
                        .to_string();
                    let directory = path
                        .parent()
                        .and_then(|p| p.strip_prefix(&self.root).ok())
                        .map(|p| p.to_string_lossy().to_string())
                        .filter(|p| !p.is_empty())
                        .unwrap_or_else(|| ".".to_string());
                    let badge = self.git_status.get(&path).copied();
                    let row_path = path.clone();
                    let is_selected = ix == selected;

                    h_flex()
                        .id(("palette-item", ix))
                        .relative()
                        .w_full()
                        .h(px(30.))
                        .items_center()
                        .justify_between()
                        .px_3()
                        .rounded(px(tokens::RADIUS_SM))
                        .border_1()
                        .border_color(gpui_kit::transparent_black())
                        .cursor_pointer()
                        .when(is_selected, |el| {
                            el.bg(rgba(0xea7a3c16))
                                .border_color(rgba(0xea7a3c36))
                                .child(
                                    div()
                                        .absolute()
                                        .left(px(2.))
                                        .top(px(6.))
                                        .bottom(px(6.))
                                        .w(px(2.5))
                                        .rounded(px(2.))
                                        .bg(cx.theme().primary),
                                )
                        })
                        .when(!is_selected, |el| el.hover(|s| s.bg(rgba(0xffffff08))))
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .min_w(px(0.))
                                .child(render_file_icon(&name))
                                .child(
                                    div()
                                        .text_size(px(tokens::UI))
                                        .text_color(cx.theme().foreground)
                                        .child(name),
                                ),
                        )
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_size(px(tokens::UI_SM))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(directory),
                                )
                                .when_some(badge, |el, letter| {
                                    el.child(
                                        div()
                                            .text_size(px(10.))
                                            .font_medium()
                                            .text_color(cx.theme().warning)
                                            .child(letter.to_string()),
                                    )
                                }),
                        )
                        .on_click({
                            let view = view.clone();
                            move |_event, window, cx| {
                                view.update(cx, |this, cx| {
                                    this.open_path(row_path.clone(), window, cx);
                                    this.palette_open = false;
                                    cx.notify();
                                });
                            }
                        })
                        .into_any_element()
                })
                .collect()
        };

        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(84.))
                .bg(rgba(0x00000066))
                .child(
                    v_flex()
                        .w(px(560.))
                        .max_h(px(420.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .overflow_hidden()
                        .child(
                            h_flex()
                                .h(px(38.))
                                .px_3()
                                .gap_2()
                                .items_center()
                                .border_b_1()
                                .border_color(cx.theme().border)
                                .child(
                                    gpui_kit::component::Icon::new(
                                        gpui_kit::assets::IconName::Search,
                                    )
                                    .with_size(px(13.))
                                    .text_color(cx.theme().muted_foreground),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .child(
                                            Input::new(&self.palette_input)
                                                .appearance(false)
                                                .small(),
                                        ),
                                ),
                        )
                        .child(
                            v_flex()
                                .id("palette-list")
                                .p_1p5()
                                .gap(px(tokens::SPACE_1))
                                .overflow_y_scroll()
                                .children(rows),
                        )
                        .child(
                            h_flex()
                                .px_3()
                                .py_1p5()
                                .gap_3()
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .text_size(px(tokens::UI_XS))
                                .text_color(cx.theme().muted_foreground)
                                .child("\u{2191}\u{2193} Navigate")
                                .child("\u{21b5} Execute / Open")
                                .child("> Commands (\u{2318}\u{21e7}P)")
                                .child("Esc Dismiss"),
                        ),
                ),
        )
    }

    pub(crate) fn open_go_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_editor().is_none() {
            return;
        }
        self.go_to_line_open = true;
        self.go_to_line_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find_open = false;
        if let Some(editor) = self.active_editor().cloned() {
            editor.update(cx, |ed, cx| ed.clear_search(cx));
            let fh = editor.read(cx).focus_handle(cx);
            window.focus(&fh, cx);
        }
        cx.notify();
    }

    fn submit_go_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.go_to_line_input.read(cx).value().trim().to_string();
        self.go_to_line_open = false;
        if let Some(editor) = self.active_editor().cloned() {
            let mut parts = raw.split(':');
            if let Some(line_1) = parts.next().and_then(|s| s.trim().parse::<u32>().ok()) {
                let col_1 = parts
                    .next()
                    .and_then(|s| s.trim().parse::<u32>().ok())
                    .unwrap_or(1);
                editor.update(cx, |ed, cx| {
                    let max_line = ed.buffer().line_count().saturating_sub(1) as u32;
                    let line_0 = line_1.saturating_sub(1).min(max_line);
                    let col_0 = col_1.saturating_sub(1);
                    ed.set_cursor_to_position(line_0, col_0, cx);
                });
            }
            let fh = editor.read(cx).focus_handle(cx);
            window.focus(&fh, cx);
        }
        cx.notify();
    }

    fn render_go_to_line_prompt(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> Option<Div> {
        if !self.go_to_line_open {
            return None;
        }
        let line_count = self
            .active_editor()
            .map(|ed| ed.read(cx).buffer().line_count())
            .unwrap_or(1);

        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(84.))
                .bg(rgba(0x00000066))
                .child(
                    v_flex()
                        .w(px(360.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .child(
                            h_flex()
                                .px_3()
                                .py_2()
                                .items_center()
                                .justify_between()
                                .text_size(px(tokens::UI_SM))
                                .text_color(cx.theme().muted_foreground)
                                .child("Go to Line")
                                .child(
                                    div()
                                        .font_family("SF Mono")
                                        .text_size(px(tokens::UI_XS))
                                        .child(format!("1..{line_count}")),
                                ),
                        )
                        .child(
                            div()
                                .px_3()
                                .pb_3()
                                .on_key_down({
                                    let view = view.clone();
                                    move |event, window, cx| {
                                        let key = event.keystroke.key.as_str();
                                        if key == "escape" {
                                            view.update(cx, |this, cx| {
                                                this.go_to_line_open = false;
                                                cx.notify();
                                            });
                                        } else if key == "enter" {
                                            view.update(cx, |this, cx| {
                                                this.submit_go_to_line(window, cx)
                                            });
                                        }
                                    }
                                })
                                .child(
                                    h_flex()
                                        .h(px(28.))
                                        .px_2p5()
                                        .items_center()
                                        .rounded(px(tokens::RADIUS_SM))
                                        .bg(cx.theme().background)
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .child(
                                            Input::new(&self.go_to_line_input)
                                                .appearance(false)
                                                .small(),
                                        ),
                                ),
                        ),
                ),
        )
    }

    pub(crate) fn active_editor(&self) -> Option<&Entity<EditorView>> {
        self.tabs.get(self.active_tab).map(|t| &t.editor)
    }

    pub(crate) fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_path_with_mode(path, false, window, cx);
    }

    pub(crate) fn open_path_with_mode(
        &mut self,
        path: PathBuf,
        as_preview: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diff = None;
        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());

        //Promote any existing tab that has been edited so `is_preview` stays accurate.
        for tab in &mut self.tabs {
            if tab.editor.read(cx).buffer().revision() != tab.opened_revision {
                tab.is_preview = false;
            }
        }

        if let Some(ix) = self.tabs.iter().position(|t| {
            t.path.as_ref().is_some_and(|existing| {
                existing == &canonical
                    || std::fs::canonicalize(existing)
                        .map(|c| c == canonical)
                        .unwrap_or(false)
            })
        }) {
            if !as_preview {
                self.tabs[ix].is_preview = false;
            }
            self.active_tab = ix;
        } else if is_image_path(&canonical) {
            let Some(preview) = load_image_preview(&canonical) else {
                eprintln!("failed to decode image {}", canonical.display());
                return;
            };
            let editor = cx.new(|cx| EditorView::new(cx));
            let new_tab = Tab {
                path: Some(canonical),
                editor,
                md_mode: MarkdownMode::Code,
                image: Some(preview),
                is_preview: as_preview,
                opened_revision: 0,
            };
            if as_preview {
                if let Some(preview_ix) = self.tabs.iter().position(|t| t.is_preview) {
                    self.tabs[preview_ix] = new_tab;
                    self.active_tab = preview_ix;
                } else {
                    self.tabs.push(new_tab);
                    self.active_tab = self.tabs.len() - 1;
                }
            } else {
                self.tabs.push(new_tab);
                self.active_tab = self.tabs.len() - 1;
            }
        } else {
            let buffer = match Buffer::open(&canonical) {
                Ok(buffer) => buffer,
                Err(err) => {
                    eprintln!("failed to open {}: {err}", canonical.display());
                    return;
                }
            };
            let opened_revision = buffer.revision();
            let lenses = self.current_test_code_lenses();
            let saved_bps = self.dap_breakpoints.get(&canonical).cloned();
            let editor = cx.new(|cx| {
                let mut ed = EditorView::from_buffer(buffer, cx);
                ed.set_test_code_lenses(lenses, cx);
                if let Some(bps) = saved_bps {
                    ed.set_breakpoints(bps, cx);
                }
                ed
            });
            self.watch_editor(&editor, window, cx);
            let new_tab = Tab {
                path: Some(canonical),
                editor,
                md_mode: MarkdownMode::Code,
                image: None,
                is_preview: as_preview,
                opened_revision,
            };

            // Zed-style preview slot: if opening in preview mode and an
            // unmodified preview tab already exists, replace it in-place so
            // browsing the file tree doesn't spawn dozens of transient tabs.
            if as_preview {
                if let Some(preview_ix) = self.tabs.iter().position(|t| t.is_preview) {
                    self.tabs[preview_ix] = new_tab;
                    self.active_tab = preview_ix;
                } else {
                    self.tabs.push(new_tab);
                    self.active_tab = self.tabs.len() - 1;
                }
            } else {
                self.tabs.push(new_tab);
                self.active_tab = self.tabs.len() - 1;
            }
        }

        self.tab_overflow_open = false;
        self.tab_scroll.scroll_to_item(self.active_tab);

        if let Some(editor) = self.active_editor() {
            let focus_handle = editor.read(cx).focus_handle(cx);
            window.focus(&focus_handle, cx);
        }
        cx.notify();
    }

    pub(crate) fn new_scratch_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = cx.new(|cx| EditorView::new(cx));
        let opened_revision = editor.read(cx).buffer().revision();
        self.watch_editor(&editor, window, cx);
        self.tabs.push(Tab {
            path: None,
            editor,
            md_mode: MarkdownMode::Code,
            image: None,
            is_preview: false,
            opened_revision,
        });
        self.active_tab = self.tabs.len() - 1;
        self.tab_overflow_open = false;
        self.tab_scroll.scroll_to_item(self.active_tab);

        if let Some(editor) = self.active_editor() {
            let focus_handle = editor.read(cx).focus_handle(cx);
            window.focus(&focus_handle, cx);
        }
        cx.notify();
    }

    pub(crate) fn close_other_tabs(&mut self, keep_ix: usize, cx: &mut Context<Self>) {
        if let Some(keep) = self.tabs.get(keep_ix) {
            let keep_tab = Tab {
                path: keep.path.clone(),
                editor: keep.editor.clone(),
                md_mode: keep.md_mode,
                image: keep.image.clone(),
                is_preview: false,
                opened_revision: keep.opened_revision,
            };
            self.tabs = vec![keep_tab];
            self.active_tab = 0;
            self.tab_overflow_open = false;
            cx.notify();
        }
    }

    pub(crate) fn close_clean_tabs(&mut self, cx: &mut Context<Self>) {
        let active_path = self.tabs.get(self.active_tab).and_then(|t| t.path.clone());
        self.tabs.retain(|t| t.editor.read(cx).buffer().is_dirty());
        if self.tabs.is_empty() {
            self.active_tab = 0;
        } else if let Some(path) = active_path {
            self.active_tab = self
                .tabs
                .iter()
                .position(|t| t.path.as_ref() == Some(&path))
                .unwrap_or(0);
        } else {
            self.active_tab = 0;
        }
        self.tab_overflow_open = false;
        cx.notify();
    }

    pub(crate) fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.tabs.remove(ix);
        if self.tabs.is_empty() {
            self.active_tab = 0;
        } else if self.active_tab > ix {
            self.active_tab -= 1;
        } else {
            self.active_tab = self.active_tab.min(self.tabs.len() - 1);
        }
        self.notify_tab_change(cx);
    }

    fn notify_tab_change(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    /// Push a plain or ANSI-styled line into the Debug Console (flutter run / pub / test output).
    pub(crate) fn push_flutter_log(&mut self, line: &str) {
        self.push_debug_line(line);
    }

    pub(crate) fn push_debug_line(&mut self, raw: &str) {
        let spans = editor::dock::parse_ansi_line(raw);
        let plain: String = spans.iter().map(|s| s.text.as_str()).collect();
        let file_link = editor::dock::extract_file_link(&plain, &self.root);
        self.debug_lines
            .push(editor::dock::TerminalLine::Output { spans, file_link });
        const MAX_DEBUG_LINES: usize = 5_000;
        if self.debug_lines.len() > MAX_DEBUG_LINES {
            let drain = self.debug_lines.len() - MAX_DEBUG_LINES;
            self.debug_lines.drain(0..drain);
        }
        self.debug_scroll
            .scroll_to_item(self.debug_lines.len().saturating_sub(1));
    }

    /// Discover connected Flutter devices (`flutter devices --machine`) and
    /// available macOS iOS Simulators (`xcrun simctl list devices available -j`)
    /// on a background thread so UI startup and frame rendering never block.
    pub(crate) fn refresh_devices(&mut self, cx: &mut Context<Self>) {
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let devices = core_flutter::list_devices().unwrap_or_default();
            let sims = core_flutter::list_ios_simulators().unwrap_or_default();
            let _ = tx.send_blocking((devices, sims));
        });

        cx.spawn(async move |weak, cx| {
            let Ok((devices, sims)) = rx.recv().await else {
                return;
            };
            let _ = weak.update(cx, |this, cx| {
                if !devices.is_empty() {
                    this.devices = devices;
                    let current_valid = this.selected_device.as_ref().is_some_and(|sel| {
                        this.devices.iter().any(|d| d.id == sel.id)
                    });
                    if !current_valid {
                        // Prefer a booted iOS Simulator if one is running, otherwise macOS or first device
                        let preferred = this
                            .devices
                            .iter()
                            .find(|d| d.target_platform.contains("ios") && d.emulator)
                            .or_else(|| this.devices.iter().find(|d| d.id == "macos"))
                            .or_else(|| this.devices.first())
                            .cloned();
                        this.selected_device = preferred;
                    }
                }
                this.ios_simulators = sims;
                this.refresh_simulator_screenshot(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Reload `.vscode/launch.json` from `self.root`, splitting configurations into
    /// runnable app targets (`self.launch_configs`) and test CodeLens templates
    /// (`self.test_launch_templates`), and updating all open editor buffers.
    pub(crate) fn reload_launch_configs(&mut self, cx: &mut Context<Self>) {
        let prev_name = self
            .selected_launch_config
            .and_then(|ix| self.launch_configs.get(ix))
            .map(|c| c.name.clone());

        if let Some(file) = core_flutter::load_launch_file(&self.root) {
            let mut runnable = Vec::new();
            let mut templates = Vec::new();
            for cfg in file.configurations {
                if cfg.is_test_template() {
                    templates.push(cfg);
                } else {
                    runnable.push(cfg);
                }
            }
            self.launch_configs = runnable;
            self.test_launch_templates = templates;
        } else {
            self.launch_configs.clear();
            self.test_launch_templates.clear();
        }

        self.selected_launch_config = prev_name
            .and_then(|name| self.launch_configs.iter().position(|c| c.name == name))
            .or_else(|| (!self.launch_configs.is_empty()).then_some(0));

        let lenses = self.current_test_code_lenses();
        let editors: Vec<_> = self.tabs.iter().map(|t| t.editor.clone()).collect();
        for ed in editors {
            let lenses = lenses.clone();
            ed.update(cx, |editor, cx| {
                editor.set_test_code_lenses(lenses, cx);
            });
        }
    }

    pub(crate) fn current_test_code_lenses(&self) -> Vec<editor_view::TestCodeLensTemplate> {
        self.test_launch_templates
            .iter()
            .map(|tpl| editor_view::TestCodeLensTemplate {
                title: tpl.code_lens_title().to_string(),
                args: tpl.resolved_cli_args(&self.root),
                for_test: tpl.applies_to_single_test(),
                for_file: tpl.applies_to_test_file(),
            })
            .collect()
    }

    /// Execute `flutter test` for a specific test file (and optional test name +
    /// extra arguments from a `.vscode/launch.json` test template like `--update-goldens`
    /// or `--coverage`) in the nearest package directory containing `pubspec.yaml`.
    pub(crate) fn run_flutter_test(
        &mut self,
        path: PathBuf,
        test_name: Option<String>,
        extra_args: Vec<String>,
        label: String,
        cx: &mut Context<Self>,
    ) {
        // Walk up from `path` to find the nearest package directory containing `pubspec.yaml`
        let mut pkg_dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone());
        let mut cur = Some(pkg_dir.as_path());
        while let Some(dir) = cur {
            if dir.join("pubspec.yaml").is_file() {
                pkg_dir = dir.to_path_buf();
                break;
            }
            cur = dir.parent();
        }

        let rel_test_path = path
            .strip_prefix(&pkg_dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();

        let local_fvm = pkg_dir.join(".fvm/flutter_sdk/bin/flutter");
        let root_fvm = self.root.join(".fvm/flutter_sdk/bin/flutter");
        let flutter_bin = if local_fvm.is_file() {
            local_fvm.to_string_lossy().into_owned()
        } else if root_fvm.is_file() {
            root_fvm.to_string_lossy().into_owned()
        } else {
            "flutter".to_string()
        };

        let mut parts: Vec<String> = Vec::new();
        if pkg_dir != self.root {
            parts.push(format!("cd '{}' &&", pkg_dir.display()));
        }
        parts.push(flutter_bin);
        parts.push("test".to_string());
        for arg in &extra_args {
            if arg.contains(' ') {
                parts.push(format!("'{arg}'"));
            } else {
                parts.push(arg.clone());
            }
        }
        if let Some(name) = &test_name {
            let escaped = name.replace('\'', "'\\''");
            parts.push("--plain-name".to_string());
            parts.push(format!("'{escaped}'"));
        }
        parts.push(format!("'{rel_test_path}'"));

        let cmd = parts.join(" ");
        self.terminal_open = true;
        self.dock_tab = DockTab::Terminal;
        self.push_flutter_log(&format!(
            "\x1b[36m\u{25b6} [{label}] Running test{}{}\x1b[0m",
            test_name
                .as_ref()
                .map(|n| format!(" \"{n}\""))
                .unwrap_or_default(),
            if extra_args.is_empty() {
                String::new()
            } else {
                format!(" ({})", extra_args.join(" "))
            }
        ));
        self.execute_terminal_command(cmd, cx);
    }

    pub(crate) fn open_or_create_launch_json(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let vscode_dir = self.root.join(".vscode");
        let launch_path = vscode_dir.join("launch.json");
        if !launch_path.is_file() {
            let _ = std::fs::create_dir_all(&vscode_dir);
            let starter = r#"{
    "version": "0.2.0",
    "configurations": [
        {
            "name": "Update goldens",
            "type": "dart",
            "request": "launch",
            "args": [
                "--update-goldens"
            ],
            "templateFor": "test",
            "codeLens": {
                "for": [
                    "run-test",
                    "run-test-file"
                ],
                "title": "Update goldens"
            }
        },
        {
            "name": "Coverage",
            "type": "dart",
            "request": "launch",
            "args": [
                "--coverage"
            ],
            "templateFor": "test",
            "codeLens": {
                "for": [
                    "run-test",
                    "run-test-file"
                ],
                "title": "Coverage"
            }
        },
        {
            "name": "Flutter (Debug)",
            "type": "dart",
            "request": "launch"
        }
    ]
}
"#;
            let _ = std::fs::write(&launch_path, starter);
            self.reload_launch_configs(cx);
        }
        self.open_path(launch_path, window, cx);
    }

    /// Start or restart `flutter run --machine -d <device_id>` using the active
    /// `.vscode/launch.json` configuration (if any) and selected device.
    pub(crate) fn start_flutter_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active_launch = self
            .selected_launch_config
            .and_then(|ix| self.launch_configs.get(ix))
            .cloned();

        let project_dir = active_launch
            .as_ref()
            .map(|c| c.resolved_cwd(&self.root))
            .unwrap_or_else(|| self.root.clone());
        let extra_args = active_launch
            .as_ref()
            .map(|c| c.resolved_cli_args(&self.root))
            .unwrap_or_default();
        let extra_env = active_launch
            .as_ref()
            .map(|c| c.resolved_env(&self.root))
            .unwrap_or_default();

        if !project_dir.join("pubspec.yaml").is_file() && active_launch.is_none() {
            self.terminal_open = true;
            window.focus(&self.terminal_focus, cx);
            let cmd = if self.root.join("Cargo.toml").is_file() {
                "cargo check".to_string()
            } else {
                "ls -la".to_string()
            };
            self.execute_terminal_command(cmd, cx);
            return;
        }

        // Stop any existing Flutter daemon first
        self.stop_flutter_run(cx);

        let mut device = self.selected_device.clone().unwrap_or(core_flutter::Device {
            id: "macos".to_string(),
            name: "macOS".to_string(),
            target_platform: "darwin".to_string(),
            emulator: false,
            is_supported: true,
        });
        if let Some(cfg_dev_id) = active_launch
            .as_ref()
            .and_then(|c| c.device_id.as_ref())
            .filter(|s| !s.is_empty())
        {
            if let Some(found) = self.devices.iter().find(|d| &d.id == cfg_dev_id) {
                device = found.clone();
            } else {
                device.id = cfg_dev_id.clone();
                device.name = cfg_dev_id.clone();
            }
        }

        // If targeting an iOS simulator, ensure it is booted and open in Simulator.app
        if device.is_ios() {
            let _ = core_flutter::boot_ios_simulator(&device.id);
        }

        let rel_cwd = project_dir
            .strip_prefix(&self.root)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty());

        self.terminal_open = true;
        self.dock_tab = DockTab::DebugConsole;
        {
            let args_suffix = if extra_args.is_empty() {
                String::new()
            } else {
                format!(" {}", extra_args.join(" "))
            };
            let cwd_prefix = rel_cwd
                .as_ref()
                .map(|d| format!("(cd {d}) "))
                .unwrap_or_default();
            let cmd_str = format!("{cwd_prefix}flutter run -d {}{args_suffix}", device.id);
            self.push_debug_line(&format!("\x1b[90m$ {cmd_str}\x1b[0m"));
        }

        match core_flutter::FlutterDaemon::spawn_with_options(
            &project_dir,
            &self.root,
            &device.id,
            &extra_args,
            &extra_env,
        ) {
            Ok(daemon) => {
                let events = daemon.events.clone();
                let daemon_arc = std::sync::Arc::new(std::sync::Mutex::new(daemon));
                self.flutter_daemon = Some(daemon_arc);
                self.flutter_app_id = None;
                let launch_title = active_launch
                    .as_ref()
                    .map(|c| c.name.as_str())
                    .unwrap_or("Flutter");
                self.flutter_status = Some(format!(
                    "Launching {launch_title} on {}\u{2026}",
                    device.name
                ));
                self.push_flutter_log(&format!(
                    "\x1b[36m\u{25b6} Starting {launch_title} on {} ({}){}...\x1b[0m",
                    device.name,
                    device.id,
                    rel_cwd
                        .as_ref()
                        .map(|d| format!(" in {d}"))
                        .unwrap_or_default(),
                ));
                cx.notify();

                cx.spawn_in(window, async move |weak, cx| {
                    while let Ok(output) = events.recv().await {
                        let keep_going = weak
                            .update_in(cx, |this, window, cx| {
                                this.handle_flutter_daemon_output(output, window, cx);
                            })
                            .is_ok();
                        if !keep_going {
                            break;
                        }
                    }
                    let _ = weak.update(cx, |this, cx| {
                        this.flutter_daemon = None;
                        this.flutter_app_id = None;
                        this.flutter_status = None;
                        cx.notify();
                    });
                })
                .detach();
            }
            Err(err) => {
                self.flutter_status = None;
                self.push_flutter_log(&format!(
                    "\x1b[31mFailed to spawn `flutter run --machine`: {err}\x1b[0m"
                ));
                cx.notify();
            }
        }
    }

    fn handle_flutter_daemon_output(
        &mut self,
        output: core_flutter::DaemonOutput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The reply to "which widget is selected?" is not a hot-reload response.
        if let core_flutter::DaemonOutput::Response { id, result } = &output {
            if self.sim.inspect_request == Some(*id) {
                self.sim.inspect_request = None;
                self.finish_inspect(result.clone(), window, cx);
                return;
            }
        }
        match output {
            core_flutter::DaemonOutput::Event { event, params } => match event.as_str() {
                "app.start" => {
                    if let Some(app_id) = params.get("appId").and_then(|v| v.as_str()) {
                        self.flutter_app_id = Some(app_id.to_string());
                    }
                    self.flutter_status = Some("Starting\u{2026}".to_string());
                    cx.notify();
                }
                "app.progress" => {
                    let finished = params
                        .get("finished")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if !finished {
                        if let Some(msg) = params.get("message").and_then(|v| v.as_str()) {
                            self.flutter_status = Some(msg.to_string());
                            self.push_flutter_log(&format!("\x1b[33m\u{25d0} {msg}\x1b[0m"));
                        }
                    } else if self.flutter_app_id.is_some() {
                        self.flutter_status = Some("Running".to_string());
                    }
                    cx.notify();
                }
                "app.started" => {
                    if let Some(app_id) = params.get("appId").and_then(|v| v.as_str()) {
                        self.flutter_app_id = Some(app_id.to_string());
                    }
                    self.flutter_status = Some("Running".to_string());
                    self.push_flutter_log(
                        "\x1b[32m\u{2713} Flutter app running \u{2014} Save (\u{2318}S) or click \u{26a1} to Hot Reload\x1b[0m",
                    );
                    self.refresh_simulator_screenshot(cx);
                    cx.notify();
                }
                "app.log" => {
                    if let Some(log) = params.get("log").and_then(|v| v.as_str()) {
                        for line in log.lines() {
                            self.push_flutter_log(line);
                        }
                        cx.notify();
                    }
                }
                "app.stop" => {
                    self.sim.inspect = false;
                    self.sim.inspected = None;
                    self.flutter_app_id = None;
                    self.flutter_daemon = None;
                    self.flutter_status = None;
                    self.push_flutter_log("\x1b[90m\u{25a0} Flutter app stopped.\x1b[0m");
                    cx.notify();
                }
                _ => {}
            },
            core_flutter::DaemonOutput::Response { result, .. } => {
                if let Ok(val) = result {
                    if let Some(msg) = val.get("message").and_then(|v| v.as_str()) {
                        if !msg.is_empty() {
                            self.push_flutter_log(&format!("\x1b[32m\u{26a1} {msg}\x1b[0m"));
                        }
                    }
                }
                if self.flutter_app_id.is_some() {
                    self.flutter_status = Some("Running".to_string());
                }
                self.refresh_simulator_screenshot(cx);
                cx.notify();
            }
            core_flutter::DaemonOutput::Log(line) => {
                if !line.trim().is_empty() {
                    self.push_flutter_log(&line);
                    cx.notify();
                }
            }
        }
    }

    /// Trigger sub-second stateful Hot Reload (`fullRestart: false`) on the active `FlutterDaemon`.
    pub(crate) fn trigger_hot_reload(&mut self, cx: &mut Context<Self>) {
        let (Some(daemon), Some(app_id)) = (self.flutter_daemon.clone(), self.flutter_app_id.clone()) else {
            self.push_flutter_log(
                "\x1b[33mNo running Flutter session \u{2014} click \u{25b6} Run first.\x1b[0m",
            );
            cx.notify();
            return;
        };
        if let Ok(guard) = daemon.lock() {
            if guard.hot_reload(&app_id).is_ok() {
                self.flutter_status = Some("Hot reloading\u{2026}".to_string());
                self.push_flutter_log("\x1b[33m\u{26a1} Hot reloading...\x1b[0m");
                cx.notify();
            }
        }
    }

    /// Trigger a full Hot Restart (`fullRestart: true`) on the active `FlutterDaemon`.
    pub(crate) fn trigger_hot_restart(&mut self, cx: &mut Context<Self>) {
        let (Some(daemon), Some(app_id)) = (self.flutter_daemon.clone(), self.flutter_app_id.clone()) else {
            return;
        };
        if let Ok(guard) = daemon.lock() {
            if guard.hot_restart(&app_id).is_ok() {
                self.flutter_status = Some("Restarting\u{2026}".to_string());
                self.push_flutter_log("\x1b[36m\u{21bb} Hot restarting app...\x1b[0m");
                cx.notify();
            }
        }
    }

    /// Gracefully stop the active `FlutterDaemon` session.
    pub(crate) fn stop_flutter_run(&mut self, cx: &mut Context<Self>) {
        if let Some(daemon) = self.flutter_daemon.take() {
            if let Ok(mut guard) = daemon.lock() {
                if let Some(app_id) = self.flutter_app_id.take() {
                    let _ = guard.stop(&app_id);
                }
                let _ = guard.kill();
            }
            self.flutter_status = None;
            self.push_flutter_log("\x1b[90m\u{25a0} Stopped Flutter run session.\x1b[0m");
            cx.notify();
        }
    }

    /// Apply cross-file `WorkspaceEdit` changes to an open tab or directly to disk.
    pub(crate) fn apply_external_file_edits(
        &mut self,
        path: PathBuf,
        mut edits: Vec<lsp_types::TextEdit>,
        cx: &mut Context<Self>,
    ) {
        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if let Some(tab) = self.tabs.iter().find(|t| {
            t.path.as_ref().is_some_and(|existing| {
                existing == &canonical
                    || std::fs::canonicalize(existing)
                        .map(|c| c == canonical)
                        .unwrap_or(false)
            })
        }) {
            let editor = tab.editor.clone();
            editor.update(cx, |ed, cx| ed.apply_external_edits(edits, cx));
        } else if let Ok(mut buffer) = Buffer::open(&canonical) {
            edits.sort_by_key(|edit| (edit.range.start.line, edit.range.start.character));
            for edit in edits.into_iter().rev() {
                let start = buffer.line_to_byte(edit.range.start.line as usize)
                    + edit.range.start.character as usize;
                let end = buffer.line_to_byte(edit.range.end.line as usize)
                    + edit.range.end.character as usize;
                buffer.edit(start..end, &edit.new_text);
            }
            let _ = buffer.save();
        }
        self.refresh_git(cx);
        cx.notify();
    }

    /// Launch a `core_dap::DapClient` (`dart debug_adapter`) session for the active Dart file
    /// (or `bin/main.dart` / `lib/main.dart`), registering all gutter breakpoints.
    pub(crate) fn start_dap_debug(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_dap_debug(cx);
        let program = self
            .active_editor()
            .and_then(|ed| ed.read(cx).buffer().path().map(Path::to_path_buf))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("dart"))
            .or_else(|| {
                let bin = self.root.join("bin/main.dart");
                let lib = self.root.join("lib/main.dart");
                if bin.is_file() {
                    Some(bin)
                } else if lib.is_file() {
                    Some(lib)
                } else {
                    None
                }
            });
        let Some(program_path) = program else {
            self.terminal_open = true;
            self.dock_tab = DockTab::DebugConsole;
            self.push_flutter_log(
                "\x1b[33mOpen a `.dart` file to start a DAP debug session.\x1b[0m",
            );
            cx.notify();
            return;
        };

        self.terminal_open = true;
        self.dock_tab = DockTab::DebugConsole;

        let dart_bin = if self.root.join(".fvm/flutter_sdk/bin/dart").is_file() {
            self.root
                .join(".fvm/flutter_sdk/bin/dart")
                .to_string_lossy()
                .into_owned()
        } else {
            "dart".to_string()
        };

        let client = match core_dap::DapClient::spawn(&dart_bin, &["debug_adapter"]) {
            Ok(c) => c,
            Err(err) => {
                self.push_flutter_log(&format!(
                    "\x1b[31mFailed to spawn `dart debug_adapter`: {err}\x1b[0m"
                ));
                cx.notify();
                return;
            }
        };

        let events = client.events.clone();
        let _ = client.request(
            "initialize",
            serde_json::json!({
                "adapterID": "dart",
                "linesStartAt1": true,
                "columnsStartAt1": true,
            }),
        );
        let program_str = program_path.to_string_lossy().to_string();
        let cwd_str = self.root.to_string_lossy().to_string();
        let _ = client.launch(&program_str, &cwd_str);
        for (bp_path, lines) in &self.dap_breakpoints {
            if !lines.is_empty() {
                let _ = client.set_breakpoints(&bp_path.to_string_lossy(), lines);
            }
        }
        let _ = client.configuration_done();

        self.push_flutter_log(&format!(
            "\x1b[36m\u{25b6} Started DAP debugger on {}\x1b[0m",
            program_path
                .strip_prefix(&self.root)
                .unwrap_or(&program_path)
                .display()
        ));
        let client_arc = std::sync::Arc::new(std::sync::Mutex::new(client));
        self.dap_client = Some(client_arc);
        cx.notify();

        cx.spawn_in(window, async move |weak, cx| {
            while let Ok(msg) = events.recv().await {
                let alive = weak
                    .update_in(cx, |this, window, cx| {
                        this.handle_dap_message(msg, window, cx);
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
            let _ = weak.update(cx, |this, cx| {
                this.stop_dap_debug(cx);
            });
        })
        .detach();
    }

    fn handle_dap_message(
        &mut self,
        msg: core_dap::DapMessage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match msg {
            core_dap::DapMessage::Event { event, body } => match event.as_str() {
                "stopped" => {
                    let thread_id = body.get("threadId").and_then(|v| v.as_i64()).unwrap_or(1);
                    let reason = body
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("breakpoint");
                    self.dap_paused_thread = Some(thread_id);
                    self.dap_stack_summary = Some(format!("Paused ({reason})"));
                    if let Some(dap) = &self.dap_client {
                        if let Ok(guard) = dap.lock() {
                            let _ = guard.stack_trace(thread_id);
                        }
                    }
                    cx.notify();
                }
                "continued" => {
                    self.dap_paused_thread = None;
                    self.dap_stack_summary = None;
                    self.dap_variables.clear();
                    self.clear_all_debug_lines(cx);
                    cx.notify();
                }
                "output" => {
                    if let Some(out) = body.get("output").and_then(|v| v.as_str()) {
                        for line in out.lines() {
                            if !line.trim().is_empty() {
                                self.push_flutter_log(line);
                            }
                        }
                        cx.notify();
                    }
                }
                "terminated" | "exited" => {
                    self.stop_dap_debug(cx);
                }
                _ => {}
            },
            core_dap::DapMessage::Response {
                command,
                result: Ok(body),
                ..
            } => match command.as_str() {
                "stackTrace" => {
                    if let Some(frame) = body
                        .get("stackFrames")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.first())
                    {
                        let frame_id = frame.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                        let fn_name = frame.get("name").and_then(|v| v.as_str()).unwrap_or("main");
                        let line_1 = frame.get("line").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
                        let src_path = frame
                            .get("source")
                            .and_then(|s| s.get("path"))
                            .and_then(|v| v.as_str())
                            .map(PathBuf::from);
                        self.dap_stack_summary = Some(format!("Paused in {fn_name}:{line_1}"));
                        if let Some(path) = src_path {
                            self.open_path(path, window, cx);
                            if let Some(ed) = self.active_editor().cloned() {
                                ed.update(cx, |editor, cx| {
                                    editor.set_active_debug_line(Some(line_1), cx);
                                });
                            }
                        }
                        if let Some(dap) = &self.dap_client {
                            if let Ok(guard) = dap.lock() {
                                let _ = guard.scopes(frame_id);
                            }
                        }
                        cx.notify();
                    }
                }
                "scopes" => {
                    if let Some(var_ref) = body
                        .get("scopes")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.first())
                        .and_then(|s| s.get("variablesReference"))
                        .and_then(|v| v.as_i64())
                        .filter(|&r| r > 0)
                    {
                        if let Some(dap) = &self.dap_client {
                            if let Ok(guard) = dap.lock() {
                                let _ = guard.variables(var_ref);
                            }
                        }
                    }
                }
                "variables" => {
                    if let Some(arr) = body.get("variables").and_then(|v| v.as_array()) {
                        self.dap_variables = arr
                            .iter()
                            .take(8)
                            .filter_map(|v| {
                                Some((
                                    v.get("name")?.as_str()?.to_string(),
                                    v.get("value")?.as_str()?.to_string(),
                                ))
                            })
                            .collect();
                        cx.notify();
                    }
                }
                _ => {}
            },
            core_dap::DapMessage::Request { seq, command, .. } => {
                if let Some(dap) = &self.dap_client {
                    if let Ok(guard) = dap.lock() {
                        let _ = guard.respond(seq, &command, serde_json::json!({}));
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn clear_all_debug_lines(&mut self, cx: &mut Context<Self>) {
        let editors: Vec<_> = self.tabs.iter().map(|t| t.editor.clone()).collect();
        for ed in editors {
            ed.update(cx, |editor, cx| editor.set_active_debug_line(None, cx));
        }
    }

    pub(crate) fn stop_dap_debug(&mut self, cx: &mut Context<Self>) {
        if let Some(dap) = self.dap_client.take() {
            if let Ok(mut guard) = dap.lock() {
                let _ = guard.disconnect(true);
                let _ = guard.kill();
            }
            self.dap_paused_thread = None;
            self.dap_stack_summary = None;
            self.dap_variables.clear();
            self.clear_all_debug_lines(cx);
            self.push_flutter_log("\x1b[90m\u{25a0} DAP debug session ended.\x1b[0m");
            cx.notify();
        }
    }

    pub(crate) fn dap_continue(&mut self, cx: &mut Context<Self>) {
        let (Some(dap), Some(tid)) = (&self.dap_client, self.dap_paused_thread) else {
            return;
        };
        if let Ok(guard) = dap.lock() {
            let _ = guard.continue_thread(tid);
        }
        self.dap_paused_thread = None;
        self.dap_variables.clear();
        self.clear_all_debug_lines(cx);
        cx.notify();
    }

    pub(crate) fn dap_step_over(&mut self, cx: &mut Context<Self>) {
        let (Some(dap), Some(tid)) = (&self.dap_client, self.dap_paused_thread) else {
            return;
        };
        if let Ok(guard) = dap.lock() {
            let _ = guard.next(tid);
        }
        cx.notify();
    }

    pub(crate) fn dap_step_in(&mut self, cx: &mut Context<Self>) {
        let (Some(dap), Some(tid)) = (&self.dap_client, self.dap_paused_thread) else {
            return;
        };
        if let Ok(guard) = dap.lock() {
            let _ = guard.step_in(tid);
        }
        cx.notify();
    }

    pub(crate) fn dap_step_out(&mut self, cx: &mut Context<Self>) {
        let (Some(dap), Some(tid)) = (&self.dap_client, self.dap_paused_thread) else {
            return;
        };
        if let Ok(guard) = dap.lock() {
            let _ = guard.step_out(tid);
        }
        cx.notify();
    }

    /// Run `flutter pub get` in `self.root` asynchronously and stream output to the terminal.
    pub(crate) fn run_pub_get(&mut self, cx: &mut Context<Self>) {
        if !self.root.join("pubspec.yaml").is_file() {
            return;
        }
        let root = self.root.clone();
        self.flutter_status = Some("Running pub get\u{2026}".to_string());
        self.terminal_open = true;
        self.dock_tab = DockTab::DebugConsole;
        self.push_flutter_log("\x1b[36m\u{21bb} Running `flutter pub get`...\x1b[0m");
        cx.notify();

        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let res = core_flutter::pub_get(&root);
            let _ = tx.send_blocking(res);
        });

        cx.spawn(async move |weak, cx| {
            let Ok(res) = rx.recv().await else { return };
            let _ = weak.update(cx, |this, cx| {
                match res {
                    Ok(out) => {
                        for line in out.stdout.lines().chain(out.stderr.lines()) {
                            if !line.trim().is_empty() {
                                this.push_flutter_log(line);
                            }
                        }
                        if out.success {
                            this.push_flutter_log(
                                "\x1b[32m\u{2713} `flutter pub get` succeeded.\x1b[0m",
                            );
                        } else {
                            this.push_flutter_log(
                                "\x1b[31m\u{2717} `flutter pub get` exited with errors.\x1b[0m",
                            );
                        }
                    }
                    Err(err) => {
                        this.push_flutter_log(&format!(
                            "\x1b[31mFailed to run `flutter pub get`: {err}\x1b[0m"
                        ));
                    }
                }
                this.flutter_status = this.flutter_app_id.as_ref().map(|_| "Running".to_string());
                this.refresh_git(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn open_add_package(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_package_open = true;
        self.add_package_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn submit_add_package(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.add_package_input.read(cx).value().trim().to_string();
        self.add_package_open = false;
        if raw.is_empty() {
            cx.notify();
            return;
        }
        let (dev, pkg) = if let Some(rest) = raw.strip_prefix("dev:") {
            (true, rest.trim().to_string())
        } else {
            (false, raw)
        };
        let root = self.root.clone();
        self.terminal_open = true;
        self.flutter_status = Some(format!("Adding {pkg}\u{2026}"));
        self.push_flutter_log(&format!(
            "\x1b[36m+ Running `flutter pub add {}{pkg}`...\x1b[0m",
            if dev { "--dev " } else { "" }
        ));
        cx.notify();

        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let res = core_flutter::add_package(&root, &pkg, dev);
            let _ = tx.send_blocking(res);
        });

        cx.spawn(async move |weak, cx| {
            let Ok(res) = rx.recv().await else { return };
            let _ = weak.update(cx, |this, cx| {
                if let Ok(out) = res {
                    for line in out.stdout.lines().chain(out.stderr.lines()) {
                        if !line.trim().is_empty() {
                            this.push_flutter_log(line);
                        }
                    }
                }
                this.flutter_status = this.flutter_app_id.as_ref().map(|_| "Running".to_string());
                this.refresh_git(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn render_add_package_prompt(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> Option<Div> {
        if !self.add_package_open {
            return None;
        }

        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(84.))
                .bg(rgba(0x00000066))
                .child(
                    v_flex()
                        .w(px(400.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .child(
                            h_flex()
                                .px_3()
                                .py_2()
                                .items_center()
                                .justify_between()
                                .text_size(px(tokens::UI_SM))
                                .text_color(cx.theme().muted_foreground)
                                .child("Add Flutter / Dart Package (pub.dev)")
                                .child(
                                    div()
                                        .font_family("SF Mono")
                                        .text_size(px(tokens::UI_XS))
                                        .child("prefix `dev:` for dev_dependencies"),
                                ),
                        )
                        .child(
                            div()
                                .px_3()
                                .pb_3()
                                .on_key_down({
                                    let view = view.clone();
                                    move |event, window, cx| {
                                        let key = event.keystroke.key.as_str();
                                        if key == "escape" {
                                            view.update(cx, |this, cx| {
                                                this.add_package_open = false;
                                                cx.notify();
                                            });
                                        } else if key == "enter" {
                                            view.update(cx, |this, cx| {
                                                this.submit_add_package(window, cx)
                                            });
                                        }
                                    }
                                })
                                .child(
                                    h_flex()
                                        .h(px(28.))
                                        .px_2p5()
                                        .items_center()
                                        .rounded(px(tokens::RADIUS_SM))
                                        .bg(cx.theme().background)
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .child(
                                            Input::new(&self.add_package_input)
                                                .appearance(false)
                                                .small(),
                                        ),
                                ),
                        ),
                ),
        )
    }

    fn render_launch_picker_popover(
        &self,
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        if !self.launch_picker_open {
            return None;
        }

        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let green = cx.theme().green;
        let info = cx.theme().info;
        let primary = cx.theme().primary;
        let list_hover = cx.theme().list_hover;
        let selected_ix = self.selected_launch_config;
        let active_test_path = self
            .active_editor()
            .and_then(|ed| ed.read(cx).buffer().path())
            .map(Path::to_path_buf);

        Some(
            div()
                .id("launch-picker-backdrop")
                .absolute()
                .inset_0()
                .on_mouse_down(MouseButton::Left, {
                    let view = view.clone();
                    move |_event, _window, cx| {
                        view.update(cx, |this, cx| {
                            this.launch_picker_open = false;
                            cx.notify();
                        });
                    }
                })
                .child(
                    v_flex()
                        .id("launch-picker-card")
                        .absolute()
                        .top(px(tokens::TITLEBAR_H + 4.))
                        .right(px(230.))
                        .w(px(360.))
                        .p(px(tokens::SPACE_2))
                        .gap(px(tokens::SPACE_1))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                            cx.stop_propagation();
                        })
                        .child(
                            h_flex()
                                .px(px(tokens::SPACE_2))
                                .py(px(2.))
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(tokens::UI_XS))
                                        .font_medium()
                                        .text_color(muted)
                                        .child("LAUNCH CONFIGURATIONS (.vscode/launch.json)"),
                                )
                                .child({
                                    let view = view.clone();
                                    h_flex()
                                        .id("reload-launch-btn")
                                        .px_1p5()
                                        .py_0p5()
                                        .gap_1()
                                        .items_center()
                                        .rounded(px(tokens::RADIUS_SM))
                                        .cursor_pointer()
                                        .text_size(px(tokens::UI_XS))
                                        .text_color(info)
                                        .hover(move |s| s.bg(list_hover))
                                        .child(
                                            gpui_kit::component::Icon::new(
                                                gpui_kit::assets::IconName::RotateCw,
                                            )
                                            .with_size(px(10.)),
                                        )
                                        .child("Reload")
                                        .on_click(move |_event, _window, cx| {
                                            view.update(cx, |this, cx| {
                                                this.reload_launch_configs(cx);
                                                cx.notify();
                                            });
                                        })
                                }),
                        )
                        .when(self.launch_configs.is_empty(), |el| {
                            el.child(
                                div()
                                    .px(px(tokens::SPACE_3))
                                    .py(px(6.))
                                    .text_size(px(tokens::UI_SM))
                                    .text_color(muted)
                                    .child(
                                        "No app launch configurations found in .vscode/launch.json",
                                    ),
                            )
                        })
                        .children(self.launch_configs.iter().enumerate().map(|(ix, cfg)| {
                            let is_sel = selected_ix == Some(ix);
                            let name = cfg.name.clone();
                            let rel_cwd = cfg.relative_cwd_label(&self.root);
                            let flavor = cfg.flavor();
                            let view_select = view.clone();
                            let view_run = view.clone();

                            h_flex()
                                .id(("launch-cfg-item", ix))
                                .w_full()
                                .min_h(px(30.))
                                .px(px(tokens::SPACE_3))
                                .py(px(3.))
                                .items_center()
                                .justify_between()
                                .rounded(px(tokens::RADIUS_SM))
                                .border_1()
                                .border_color(if is_sel {
                                    rgba(0xea7a3c44)
                                } else {
                                    rgba(0x00000000)
                                })
                                .when(is_sel, |el| el.bg(rgba(0xea7a3c18)))
                                .when(!is_sel, |el| el.hover(move |s| s.bg(list_hover)))
                                .cursor_pointer()
                                .on_click(move |_event, _window, cx| {
                                    view_select.update(cx, |this, cx| {
                                        this.selected_launch_config = Some(ix);
                                        this.launch_picker_open = false;
                                        cx.notify();
                                    });
                                })
                                .child(
                                    v_flex()
                                        .min_w(px(0.))
                                        .flex_1()
                                        .child(
                                            h_flex()
                                                .items_center()
                                                .gap_1p5()
                                                .child(
                                                    gpui_kit::component::Icon::new(
                                                        gpui_kit::assets::IconName::Play,
                                                    )
                                                    .with_size(px(11.))
                                                    .text_color(if is_sel { primary } else { green }),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(px(tokens::UI_SM))
                                                        .font_medium()
                                                        .text_color(fg)
                                                        .truncate()
                                                        .child(name),
                                                )
                                                .when_some(flavor, |el, flv| {
                                                    el.child(
                                                        div()
                                                            .px_1p5()
                                                            .rounded(px(3.))
                                                            .bg(info.opacity(0.16))
                                                            .font_family("SF Mono")
                                                            .text_size(px(9.5))
                                                            .text_color(info)
                                                            .child(flv),
                                                    )
                                                }),
                                        )
                                        .when_some(rel_cwd, |el, cwd| {
                                            el.child(
                                                div()
                                                    .pl(px(17.))
                                                    .font_family("SF Mono")
                                                    .text_size(px(10.))
                                                    .text_color(muted)
                                                    .truncate()
                                                    .child(cwd),
                                            )
                                        }),
                                )
                                .child(
                                    h_flex()
                                        .id(("launch-cfg-run-now", ix))
                                        .px_2()
                                        .h(px(22.))
                                        .items_center()
                                        .gap_1()
                                        .rounded(px(tokens::RADIUS_SM))
                                        .bg(green.opacity(0.15))
                                        .border_1()
                                        .border_color(green.opacity(0.32))
                                        .text_size(px(10.))
                                        .text_color(green)
                                        .child("\u{25b6} Run")
                                        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                                            cx.stop_propagation();
                                        })
                                        .on_click(move |_event, window, cx| {
                                            cx.stop_propagation();
                                            view_run.update(cx, |this, cx| {
                                                this.selected_launch_config = Some(ix);
                                                this.launch_picker_open = false;
                                                this.start_flutter_run(window, cx);
                                            });
                                        }),
                                )
                        }))
                        .when(!self.test_launch_templates.is_empty(), |el| {
                            el.child(div().h(px(1.)).w_full().my_1().bg(cx.theme().border))
                                .child(
                                    div()
                                        .px(px(tokens::SPACE_2))
                                        .py(px(2.))
                                        .text_size(px(tokens::UI_XS))
                                        .font_medium()
                                        .text_color(muted)
                                        .child("TEST TEMPLATES & CODELENS (templateFor: \"test\")"),
                                )
                                .children(self.test_launch_templates.iter().enumerate().map(
                                    |(ix, tpl)| {
                                        let title = tpl.code_lens_title().to_string();
                                        let args = tpl.resolved_cli_args(&self.root);
                                        let flags = args.join(" ");
                                        let view = view.clone();
                                        let path_opt = active_test_path.clone();

                                        h_flex()
                                            .id(("test-tpl-item", ix))
                                            .w_full()
                                            .h(px(28.))
                                            .px(px(tokens::SPACE_3))
                                            .items_center()
                                            .justify_between()
                                            .rounded(px(tokens::RADIUS_SM))
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(list_hover))
                                            .on_click(move |_event, _window, cx| {
                                                let title = title.clone();
                                                let args = args.clone();
                                                let path = path_opt
                                                    .clone()
                                                    .unwrap_or_else(|| PathBuf::from("test"));
                                                view.update(cx, |this, cx| {
                                                    this.launch_picker_open = false;
                                                    this.run_flutter_test(
                                                        path, None, args, title, cx,
                                                    );
                                                });
                                            })
                                            .child(
                                                h_flex()
                                                    .items_center()
                                                    .gap_2()
                                                    .child(
                                                        div()
                                                            .text_size(px(10.5))
                                                            .text_color(info)
                                                            .child("\u{25b6}"),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(px(tokens::UI_SM))
                                                            .text_color(fg)
                                                            .child(tpl.code_lens_title().to_string()),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .font_family("SF Mono")
                                                    .text_size(px(10.))
                                                    .text_color(muted)
                                                    .child(flags),
                                            )
                                    },
                                ))
                        })
                        .child(div().h(px(1.)).w_full().my_1().bg(cx.theme().border))
                        .child({
                            let view = view.clone();
                            h_flex()
                                .id("open-launch-json-btn")
                                .w_full()
                                .h(px(28.))
                                .px(px(tokens::SPACE_3))
                                .items_center()
                                .justify_between()
                                .rounded(px(tokens::RADIUS_SM))
                                .cursor_pointer()
                                .hover(move |s| s.bg(list_hover))
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            gpui_kit::component::Icon::new(
                                                gpui_kit::assets::IconName::Settings,
                                            )
                                            .with_size(px(12.))
                                            .text_color(primary),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(tokens::UI_SM))
                                                .text_color(fg)
                                                .child("Edit .vscode/launch.json"),
                                        ),
                                )
                                .child(
                                    div()
                                        .font_family("SF Mono")
                                        .text_size(px(10.))
                                        .text_color(muted)
                                        .child("Open"),
                                )
                                .on_click(move |_event, window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.launch_picker_open = false;
                                        this.open_or_create_launch_json(window, cx);
                                    });
                                })
                        }),
                )
        )
    }

    fn render_device_picker_popover(
        &self,
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        if !self.device_picker_open {
            return None;
        }

        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let green = cx.theme().green;
        let info = cx.theme().info;
        let primary = cx.theme().primary;
        let list_hover = cx.theme().list_hover;
        let selected_id = self.selected_device.as_ref().map(|d| d.id.clone());
        let selected_cfg_ix = self.selected_launch_config;
        let sim_attached = self.sim.attached;
        let sims_expanded = self.sim_list_expanded;

        // Deduplicate unbooted iOS simulators by model name (keeping the first/latest runtime)
        // and exclude any simulator already shown in `self.devices` (by UDID or name).
        let mut seen_names = std::collections::HashSet::new();
        for d in &self.devices {
            seen_names.insert(d.name.clone());
        }
        let extra_sims: Vec<_> = self
            .ios_simulators
            .iter()
            .filter(|sim| {
                !self.devices.iter().any(|d| d.id == sim.udid)
                    && seen_names.insert(sim.name.clone())
            })
            .take(6)
            .cloned()
            .collect();

        Some(
            div()
                .id("device-picker-backdrop")
                .absolute()
                .inset_0()
                .on_mouse_down(MouseButton::Left, {
                    let view = view.clone();
                    move |_event, _window, cx| {
                        view.update(cx, |this, cx| {
                            this.device_picker_open = false;
                            cx.notify();
                        });
                    }
                })
                .child(
                    v_flex()
                        .id("device-picker-card")
                        .absolute()
                        .top(px(tokens::TITLEBAR_H + 4.))
                        .right(px(108.))
                        .w(px(250.))
                        .max_h(px(400.))
                        .overflow_y_scroll()
                        .p(px(tokens::SPACE_2))
                        .gap(px(2.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                            cx.stop_propagation();
                        })
                        // Section 1: Active Device Targets (clean rows, checkmark only)
                        .child(
                            h_flex()
                                .px(px(tokens::SPACE_2))
                                .py(px(2.))
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(tokens::UI_XS))
                                        .font_medium()
                                        .text_color(muted)
                                        .child("TARGET"),
                                )
                                .child({
                                    let view = view.clone();
                                    div()
                                        .id("refresh-devices-btn")
                                        .cursor_pointer()
                                        .text_color(muted)
                                        .hover(move |s| s.text_color(fg))
                                        .child(
                                            gpui_kit::component::Icon::new(
                                                gpui_kit::assets::IconName::RotateCw,
                                            )
                                            .with_size(px(10.5)),
                                        )
                                        .on_click(move |_event, _window, cx| {
                                            view.update(cx, |this, cx| {
                                                this.refresh_devices(cx);
                                            });
                                        })
                                }),
                        )
                        .children(self.devices.iter().enumerate().map(|(ix, dev)| {
                            let is_sel = selected_id.as_deref() == Some(dev.id.as_str());
                            let icon = if dev.target_platform.contains("ios")
                                || dev.target_platform.contains("android")
                            {
                                gpui_kit::assets::IconName::Smartphone
                            } else if dev.target_platform.contains("web") || dev.id == "chrome" {
                                gpui_kit::assets::IconName::Globe
                            } else {
                                gpui_kit::assets::IconName::Monitor
                            };
                            let dev_clone = dev.clone();
                            let view_sel = view.clone();

                            h_flex()
                                .id(("device-item", ix))
                                .w_full()
                                .h(px(26.))
                                .px(px(tokens::SPACE_3))
                                .items_center()
                                .justify_between()
                                .rounded(px(tokens::RADIUS_SM))
                                .when(is_sel, |el| el.bg(tokens::accent_dim()))
                                .when(!is_sel, |el| el.hover(move |s| s.bg(list_hover)))
                                .cursor_pointer()
                                .on_click(move |_event, _window, cx| {
                                    view_sel.update(cx, |this, cx| {
                                        this.selected_device = Some(dev_clone.clone());
                                        this.device_picker_open = false;
                                        this.refresh_simulator_screenshot(cx);
                                        cx.notify();
                                    });
                                })
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            gpui_kit::component::Icon::new(icon)
                                                .with_size(px(12.5))
                                                .text_color(if is_sel { primary } else { green }),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(tokens::UI_SM))
                                                .text_color(fg)
                                                .child(dev.name.clone()),
                                        ),
                                )
                                .when(is_sel, |el| {
                                    el.child(
                                        gpui_kit::component::Icon::new(
                                            gpui_kit::assets::IconName::Check,
                                        )
                                        .with_size(px(11.5))
                                        .text_color(primary),
                                    )
                                })
                        }))
                        // Collapsible "More iOS Simulators" row (keeps the popover short & clean)
                        .when(!extra_sims.is_empty(), |el| {
                            let view_toggle = view.clone();
                            el.child(
                                h_flex()
                                    .id("toggle-extra-sims")
                                    .w_full()
                                    .h(px(24.))
                                    .px(px(tokens::SPACE_3))
                                    .items_center()
                                    .justify_between()
                                    .rounded(px(tokens::RADIUS_SM))
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(list_hover))
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(muted)
                                    .child(format!("More iOS Simulators ({})", extra_sims.len()))
                                    .child(
                                        gpui_kit::component::Icon::new(if sims_expanded {
                                            gpui_kit::assets::IconName::ChevronDown
                                        } else {
                                            gpui_kit::assets::IconName::ChevronRight
                                        })
                                        .with_size(px(10.5))
                                        .text_color(muted),
                                    )
                                    .on_click(move |_e, _w, cx| {
                                        view_toggle.update(cx, |this, cx| {
                                            this.sim_list_expanded = !this.sim_list_expanded;
                                            cx.notify();
                                        });
                                    }),
                            )
                            .when(sims_expanded, |list| {
                                list.children(extra_sims.into_iter().enumerate().map(
                                    |(ix, sim)| {
                                        let view_sel = view.clone();
                                        let sim_name = sim.name.clone();
                                        let sim_for_click = sim.clone();
                                        h_flex()
                                            .id(("ios-sim-item", ix))
                                            .w_full()
                                            .h(px(25.))
                                            .pl(px(tokens::SPACE_5))
                                            .pr(px(tokens::SPACE_3))
                                            .items_center()
                                            .justify_between()
                                            .rounded(px(tokens::RADIUS_SM))
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(list_hover))
                                            .on_click(move |_event, window, cx| {
                                                let s = sim_for_click.clone();
                                                view_sel.update(cx, |this, cx| {
                                                    this.selected_device =
                                                        Some(core_flutter::Device {
                                                            id: s.udid.clone(),
                                                            name: s.name.clone(),
                                                            target_platform: "ios".to_string(),
                                                            emulator: true,
                                                            is_supported: true,
                                                        });
                                                    this.device_picker_open = false;
                                                    this.attach_simulator(window, cx);
                                                    this.refresh_devices(cx);
                                                });
                                            })
                                            .child(
                                                h_flex()
                                                    .min_w(px(0.))
                                                    .items_center()
                                                    .gap_2()
                                                    .child(
                                                        gpui_kit::component::Icon::new(
                                                            gpui_kit::assets::IconName::Smartphone,
                                                        )
                                                        .with_size(px(12.))
                                                        .text_color(muted),
                                                    )
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .text_size(px(tokens::UI_SM))
                                                            .text_color(fg)
                                                            .child(sim_name),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(tokens::UI_XS))
                                                    .text_color(muted)
                                                    .child("Boot"),
                                            )
                                    },
                                ))
                            })
                        })
                        // Launch Profiles (only listed if `.vscode/launch.json` has profiles)
                        .when(!self.launch_configs.is_empty(), |el| {
                            el.child(div().h(px(1.)).w_full().my_1().bg(cx.theme().border))
                                .child(
                                    div()
                                        .px(px(tokens::SPACE_2))
                                        .py(px(2.))
                                        .text_size(px(tokens::UI_XS))
                                        .font_medium()
                                        .text_color(muted)
                                        .child("PROFILE"),
                                )
                                .children(self.launch_configs.iter().enumerate().map(
                                    |(ix, cfg)| {
                                        let is_sel = selected_cfg_ix == Some(ix);
                                        let name = cfg.name.clone();
                                        let view = view.clone();
                                        h_flex()
                                            .id(("unified-launch-cfg", ix))
                                            .w_full()
                                            .h(px(25.))
                                            .px(px(tokens::SPACE_3))
                                            .items_center()
                                            .justify_between()
                                            .rounded(px(tokens::RADIUS_SM))
                                            .cursor_pointer()
                                            .when(is_sel, |s| s.bg(tokens::accent_dim()))
                                            .when(!is_sel, |s| s.hover(move |h| h.bg(list_hover)))
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_size(px(tokens::UI_SM))
                                                    .text_color(fg)
                                                    .child(name),
                                            )
                                            .when(is_sel, |r| {
                                                r.child(
                                                    gpui_kit::component::Icon::new(
                                                        gpui_kit::assets::IconName::Check,
                                                    )
                                                    .with_size(px(11.5))
                                                    .text_color(primary),
                                                )
                                            })
                                            .on_click(move |_e, _w, cx| {
                                                view.update(cx, |this, cx| {
                                                    this.selected_launch_config = Some(ix);
                                                    this.device_picker_open = false;
                                                    cx.notify();
                                                });
                                            })
                                    },
                                ))
                        })
                        // Quiet footer actions: Simulator panel toggle + Configure launch.json
                        .child(div().h(px(1.)).w_full().my_1().bg(cx.theme().border))
                        .child({
                            let view = view.clone();
                            h_flex()
                                .id("device-picker-attach-sim")
                                .w_full()
                                .h(px(26.))
                                .px(px(tokens::SPACE_3))
                                .items_center()
                                .justify_between()
                                .rounded(px(tokens::RADIUS_SM))
                                .cursor_pointer()
                                .hover(move |s| s.bg(list_hover))
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            gpui_kit::component::Icon::new(
                                                gpui_kit::assets::IconName::Smartphone,
                                            )
                                            .with_size(px(12.))
                                            .text_color(if sim_attached { green } else { muted }),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(tokens::UI_SM))
                                                .text_color(fg)
                                                .child("Simulator in Editor"),
                                        ),
                                )
                                .child(
                                    div()
                                        .font_family("SF Mono")
                                        .text_size(px(tokens::UI_XS))
                                        .text_color(if sim_attached { green } else { muted })
                                        .child(if sim_attached { "On" } else { "Off" }),
                                )
                                .on_click(move |_event, window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.device_picker_open = false;
                                        if this.sim.attached {
                                            this.detach_simulator(cx);
                                        } else {
                                            this.attach_simulator(window, cx);
                                        }
                                    });
                                })
                        })
                        .child({
                            let view = view.clone();
                            h_flex()
                                .id("unified-edit-launch-json")
                                .w_full()
                                .h(px(26.))
                                .px(px(tokens::SPACE_3))
                                .items_center()
                                .gap_2()
                                .rounded(px(tokens::RADIUS_SM))
                                .cursor_pointer()
                                .hover(move |s| s.bg(list_hover))
                                .child(
                                    gpui_kit::component::Icon::new(
                                        gpui_kit::assets::IconName::Settings,
                                    )
                                    .with_size(px(11.5))
                                    .text_color(muted),
                                )
                                .child(
                                    div()
                                        .text_size(px(tokens::UI_SM))
                                        .text_color(muted)
                                        .child("Configure launch.json\u{2026}"),
                                )
                                .on_click(move |_event, window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.device_picker_open = false;
                                        this.open_or_create_launch_json(window, cx);
                                    });
                                })
                        }),
                )
        )
    }
}

impl Render for DartIde {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();

        v_flex()
            .size_full()
            .relative()
            .key_context("DartIde")
            .on_key_down(cx.listener(Self::on_key_down))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_titlebar(window, cx))
            .child(self.render_shell(&view, window, cx))
            .child(self.render_status_bar(cx))
            .when_some(self.render_launch_picker_popover(&view, cx), |el, pop| {
                el.child(pop)
            })
            .when_some(self.render_device_picker_popover(&view, cx), |el, pop| {
                el.child(pop)
            })
            .when_some(self.render_palette(&view, cx), |el, palette| el.child(palette))
            .when_some(self.render_go_to_line_prompt(&view, cx), |el, prompt| {
                el.child(prompt)
            })
            .when_some(self.render_add_package_prompt(&view, cx), |el, prompt| {
                el.child(prompt)
            })
            .when_some(self.render_rename_prompt(&view, cx), |el, prompt| {
                el.child(prompt)
            })
            .when_some(self.render_code_actions(&view, cx), |el, picker| {
                el.child(picker)
            })
    }
}

fn main() {
    let app = gpui_kit::application().with_assets(AllAssets);

    app.run(move |cx| {
        gpui_kit::init(cx);
        gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
        theme::apply(cx);

        let bounds = Bounds::centered(None, size(px(1600.), px(1000.)), cx);

        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: Some(point(px(12.), px(12.))),
            }),
            ..Default::default()
        };

        gpui_kit::open_window(window_options, cx, |window, cx| {
            cx.new(|cx| DartIde::new(window, cx))
        })
        .expect("Failed to open window");
    });
}
