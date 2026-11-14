//! The notebook mode of an `.ipynb` tab: cells you edit and run against a
//! Jupyter kernel, with their outputs (text, errors, images, tables)
//! underneath.
//!
//! The tab's `EditorView` keeps the notebook JSON and stays the source of
//! truth for saving: every cell edit or new output is written back to it
//! (debounced), so Cmd+S, the dirty dot, Code mode and local history all
//! work as for any file. Kernels run on a Jupyter Server: one Shame starts
//! per workspace (`jupyter server` from the project's environment), or a
//! remote server the user connects to by URL.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, Weak};

use base64::Engine as _;
use core_jupyter::notebook::{self, Cell, CellKind, Notebook, OutputPart, OutputTable};
use core_jupyter::{KernelEvent, KernelSession, LocalServer, ServerInfo};
use editor_view::EditorView;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::data_view::{fmt_count, looks_numeric};
use crate::tokens;

/// The run-count column left of each cell.
const PROMPT_W: f32 = 52.;
/// Width of the cell column, for readable code and output.
const CELL_MAX_W: f32 = 900.;
/// Output lines shown before the rest is folded.
const OUTPUT_MAX_LINES: usize = 400;
/// Columns drawn for a table output before the rest are summarised.
const TABLE_MAX_COLS: usize = 12;
/// Tallest table body before it scrolls inside the cell.
const TABLE_MAX_H: f32 = 340.;
/// Widest column, in characters, used to weigh table columns.
const TABLE_COL_CHARS: usize = 28;
/// Delay before cell edits are written back to the notebook JSON.
const SYNC_DELAY_MS: u64 = 300;

// ---- servers ----------------------------------------------------------------

/// Local servers by workspace root, shared by every notebook in it and
/// stopped when the last one closes.
fn local_servers() -> &'static Mutex<HashMap<PathBuf, Weak<LocalServer>>> {
    static SERVERS: std::sync::OnceLock<Mutex<HashMap<PathBuf, Weak<LocalServer>>>> = std::sync::OnceLock::new();
    SERVERS.get_or_init(Default::default)
}

/// The remote server the user connected to, used by new kernels instead of
/// a local one until cleared.
fn remote_server() -> &'static Mutex<Option<ServerInfo>> {
    static REMOTE: Mutex<Option<ServerInfo>> = Mutex::new(None);
    &REMOTE
}

#[derive(Clone)]
enum ServerHandle {
    Local(Arc<LocalServer>),
    Remote(ServerInfo),
}

impl ServerHandle {
    fn info(&self) -> &ServerInfo {
        match self {
            ServerHandle::Local(s) => &s.info,
            ServerHandle::Remote(i) => i,
        }
    }

    fn label(&self) -> String {
        match self {
            ServerHandle::Local(_) => "local".into(),
            ServerHandle::Remote(i) => i.base_url.split("://").nth(1).unwrap_or(&i.base_url).to_string(),
        }
    }
}

/// Where to run: the workspace (start or reuse its local server) unless a
/// remote server is connected.
fn server_for(root: &Path) -> Result<ServerHandle, String> {
    if let Some(remote) = remote_server().lock().ok().and_then(|r| r.clone()) {
        return Ok(ServerHandle::Remote(remote));
    }
    let mut servers = local_servers().lock().map_err(|_| "server registry poisoned".to_string())?;
    if let Some(existing) = servers.get(root).and_then(Weak::upgrade) {
        return Ok(ServerHandle::Local(existing));
    }
    let env = editor_view::python::detect_env(Some(root));
    let jupyter = editor_view::python::find_tool("jupyter", env.as_ref()).ok_or_else(|| {
        let env = env.map(|e| e.label()).unwrap_or_else(|| "this project".into());
        format!("Jupyter isn't installed in {env}. Run `pip install jupyter`, or connect to a server URL.")
    })?;
    let server = Arc::new(LocalServer::start(&jupyter, root)?);
    servers.insert(root.to_path_buf(), Arc::downgrade(&server));
    Ok(ServerHandle::Local(server))
}

struct Connected {
    server: ServerHandle,
    kernel_id: String,
    kernel_name: String,
    display_name: String,
    session: KernelSession,
    events: async_channel::Receiver<KernelEvent>,
}

/// Start a kernel for a notebook at `path` (blocking: run off the UI thread).
fn connect(root: PathBuf, path: PathBuf, wanted: Option<String>) -> Result<Connected, String> {
    let server = server_for(&root)?;
    let info = server.info().clone();
    let specs = info.kernel_specs()?;
    let spec = wanted
        .as_deref()
        .and_then(|w| specs.iter().find(|s| s.name == w))
        .or_else(|| specs.iter().find(|s| s.language.eq_ignore_ascii_case("python")))
        .or_else(|| specs.first())
        .cloned()
        .ok_or("the server has no kernels installed")?;
    // Local kernels start in the notebook's folder, like Jupyter does.
    let cwd = match &server {
        ServerHandle::Local(_) => {
            path.parent().and_then(|d| d.strip_prefix(&root).ok()).map(|d| d.to_string_lossy().into_owned())
        }
        ServerHandle::Remote(_) => None,
    };
    let kernel_id = info.start_kernel(&spec.name, cwd.as_deref())?;
    let (session, events) = KernelSession::connect(&info, &kernel_id)?;
    Ok(Connected { server, kernel_id, kernel_name: spec.name, display_name: spec.display_name, session, events })
}

enum Kernel {
    Idle,
    Starting,
    Ready { conn: Connected, status: String },
    Failed(String),
}

// ---- the view ----------------------------------------------------------------

struct CellUi {
    /// Our id for the cell, stable across moves; also the request id sent
    /// to the kernel.
    uid: u64,
    editor: Entity<EditorView>,
    /// Markdown cells show rendered text until edited.
    editing: bool,
    running: bool,
    queued: bool,
    /// Show outputs past `OUTPUT_MAX_LINES`.
    show_all_output: bool,
    _observe: Subscription,
}

pub(crate) struct NotebookView {
    file: Entity<EditorView>,
    root: PathBuf,
    path: PathBuf,
    notebook: Notebook,
    cells: Vec<CellUi>,
    parse_error: Option<String>,
    /// The file buffer revision this view last loaded or wrote.
    synced_revision: Option<u64>,
    sync_pending: bool,
    next_uid: u64,
    selected: Option<u64>,
    kernel: Kernel,
    /// Cells waiting for the kernel to come up or for the one before.
    queue: Vec<u64>,
    images: HashMap<u64, Arc<RenderImage>>,
    /// Outputs already reduced to what the view draws, per cell, so a big
    /// DataFrame's HTML isn't re-read on every frame. Dropped whenever any
    /// cell's outputs change.
    parts: HashMap<u64, Rc<Vec<Vec<OutputPart>>>>,
    url_input: Option<Entity<InputState>>,
    _subs: Vec<Subscription>,
}

impl NotebookView {
    pub(crate) fn new(file: Entity<EditorView>, root: PathBuf, path: PathBuf, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&file, |this, _, cx| {
            this.reload_if_changed(cx);
            cx.notify();
        });
        let mut view = Self {
            file,
            root,
            path,
            notebook: Notebook::empty(),
            cells: Vec::new(),
            parse_error: None,
            synced_revision: None,
            sync_pending: false,
            next_uid: 1,
            selected: None,
            kernel: Kernel::Idle,
            queue: Vec::new(),
            images: HashMap::new(),
            parts: HashMap::new(),
            url_input: None,
            _subs: vec![observe],
        };
        view.reload_if_changed(cx);
        view
    }

    /// Re-read the notebook when the file buffer changed under us (Code
    /// mode edits, reload from disk), not when we wrote it ourselves.
    fn reload_if_changed(&mut self, cx: &mut Context<Self>) {
        let revision = self.file.read(cx).buffer().revision();
        if self.synced_revision == Some(revision) || self.sync_pending {
            return;
        }
        self.synced_revision = Some(revision);
        let text = self.file.read(cx).buffer().text();
        match Notebook::parse(&text) {
            Ok(nb) => {
                self.parse_error = None;
                self.notebook = nb;
                self.cells = (0..self.notebook.cells.len()).map(|i| self.make_cell_ui(i, cx)).collect();
                self.images.clear();
                self.parts.clear();
            }
            Err(e) => self.parse_error = Some(e),
        }
    }

    fn make_cell_ui(&mut self, ix: usize, cx: &mut Context<Self>) -> CellUi {
        let uid = self.next_uid;
        self.next_uid += 1;
        let cell = &self.notebook.cells[ix];
        let source = cell.source.clone();
        let hint = match cell.kind {
            CellKind::Code => "cell.py",
            CellKind::Markdown => "cell.md",
            CellKind::Raw => "cell.txt",
        };
        let editing = cell.kind != CellKind::Markdown || source.trim().is_empty();
        let editor = cx.new(|cx| {
            let mut ed = EditorView::from_buffer(core_buffer::Buffer::from_str(&source), cx);
            ed.set_language_hint(hint, cx);
            ed
        });
        let observe = cx.observe(&editor, move |this, editor, cx| {
            let text = editor.read(cx).buffer().text();
            if let Some(ix) = this.index_of(uid) {
                if this.notebook.cells[ix].source != text {
                    this.notebook.cells[ix].source = text;
                    this.schedule_sync(cx);
                }
            }
        });
        CellUi { uid, editor, editing, running: false, queued: false, show_all_output: false, _observe: observe }
    }

    fn index_of(&self, uid: u64) -> Option<usize> {
        self.cells.iter().position(|c| c.uid == uid)
    }

    /// Write the notebook back to the file buffer after a short pause.
    fn schedule_sync(&mut self, cx: &mut Context<Self>) {
        if self.sync_pending {
            return;
        }
        self.sync_pending = true;
        cx.spawn(async move |weak, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(SYNC_DELAY_MS)).await;
            let _ = weak.update(cx, |this, cx| this.sync_now(cx));
        })
        .detach();
    }

    fn sync_now(&mut self, cx: &mut Context<Self>) {
        self.sync_pending = false;
        let json = self.notebook.to_json();
        if self.file.read(cx).buffer().text() != json {
            self.file.update(cx, |ed, cx| ed.replace_all_text(&json, cx));
        }
        self.synced_revision = Some(self.file.read(cx).buffer().revision());
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        self.sync_now(cx);
        self.file.update(cx, |ed, cx| ed.save_without_format(cx));
    }

    // ---- editing ----

    fn insert_cell(&mut self, after: Option<usize>, kind: CellKind, window: &mut Window, cx: &mut Context<Self>) {
        let at = after.map_or(self.cells.len(), |i| i + 1).min(self.cells.len());
        self.notebook.cells.insert(at, Cell::new(kind, ""));
        let ui = self.make_cell_ui(at, cx);
        let (uid, editor) = (ui.uid, ui.editor.clone());
        self.cells.insert(at, ui);
        self.selected = Some(uid);
        let handle = editor.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.schedule_sync(cx);
        cx.notify();
    }

    fn delete_cell(&mut self, uid: u64, cx: &mut Context<Self>) {
        let Some(ix) = self.index_of(uid) else { return };
        self.cells.remove(ix);
        self.notebook.cells.remove(ix);
        self.parts.remove(&uid);
        self.queue.retain(|q| *q != uid);
        if self.cells.is_empty() {
            self.notebook.cells.push(Cell::new(CellKind::Code, ""));
            let ui = self.make_cell_ui(0, cx);
            self.cells.push(ui);
        }
        self.schedule_sync(cx);
        cx.notify();
    }

    fn move_cell(&mut self, uid: u64, delta: isize, cx: &mut Context<Self>) {
        let Some(ix) = self.index_of(uid) else { return };
        let to = ix as isize + delta;
        if to < 0 || to as usize >= self.cells.len() {
            return;
        }
        self.cells.swap(ix, to as usize);
        self.notebook.cells.swap(ix, to as usize);
        self.schedule_sync(cx);
        cx.notify();
    }

    fn set_kind(&mut self, uid: u64, kind: CellKind, cx: &mut Context<Self>) {
        let Some(ix) = self.index_of(uid) else { return };
        let cell = &mut self.notebook.cells[ix];
        if cell.kind == kind {
            return;
        }
        cell.kind = kind;
        cell.outputs.clear();
        cell.execution_count = None;
        self.parts.clear();
        let hint = if kind == CellKind::Code { "cell.py" } else { "cell.md" };
        self.cells[ix].editor.update(cx, |ed, cx| ed.set_language_hint(hint, cx));
        self.cells[ix].editing = true;
        self.schedule_sync(cx);
        cx.notify();
    }

    fn clear_all_outputs(&mut self, cx: &mut Context<Self>) {
        for cell in &mut self.notebook.cells {
            cell.outputs.clear();
            cell.execution_count = None;
        }
        self.images.clear();
        self.parts.clear();
        self.schedule_sync(cx);
        cx.notify();
    }

    // ---- running ----

    /// Run a cell: markdown just renders; code goes to the kernel (started
    /// on first use).
    fn run_cell(&mut self, uid: u64, cx: &mut Context<Self>) {
        let Some(ix) = self.index_of(uid) else { return };
        if self.notebook.cells[ix].kind != CellKind::Code {
            self.cells[ix].editing = false;
            cx.notify();
            return;
        }
        if self.cells[ix].running || self.cells[ix].queued {
            return;
        }
        self.cells[ix].queued = true;
        self.queue.push(uid);
        match self.kernel {
            Kernel::Ready { .. } => self.pump_queue(cx),
            Kernel::Starting => {}
            Kernel::Idle | Kernel::Failed(_) => self.start_kernel(cx),
        }
        cx.notify();
    }

    fn run_all(&mut self, cx: &mut Context<Self>) {
        let uids: Vec<u64> = self
            .cells
            .iter()
            .zip(&self.notebook.cells)
            .filter(|(_, c)| c.kind == CellKind::Code && !c.source.trim().is_empty())
            .map(|(ui, _)| ui.uid)
            .collect();
        for uid in uids {
            self.run_cell(uid, cx);
        }
    }

    /// Run a cell and move to the next one (Shift+Enter), adding a cell at
    /// the end like Jupyter.
    fn run_and_advance(&mut self, uid: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.run_cell(uid, cx);
        let Some(ix) = self.index_of(uid) else { return };
        if ix + 1 >= self.cells.len() {
            self.insert_cell(Some(ix), CellKind::Code, window, cx);
        } else {
            let next = &self.cells[ix + 1];
            self.selected = Some(next.uid);
            let handle = next.editor.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    /// Send the next queued cell. One at a time, so outputs stay in order
    /// and an error stops the rest (as `stop_on_error` does in Jupyter).
    fn pump_queue(&mut self, cx: &mut Context<Self>) {
        if self.cells.iter().any(|c| c.running) {
            return;
        }
        let Kernel::Ready { conn, .. } = &self.kernel else { return };
        while !self.queue.is_empty() {
            let uid = self.queue.remove(0);
            let Some(ix) = self.index_of(uid) else { continue };
            let code = self.notebook.cells[ix].source.clone();
            self.cells[ix].queued = false;
            if let Err(e) = conn.session.execute(uid.to_string(), code) {
                self.kernel = Kernel::Failed(e);
                break;
            }
            self.cells[ix].running = true;
            self.cells[ix].show_all_output = false;
            self.notebook.cells[ix].outputs.clear();
            self.parts.clear();
            break;
        }
        cx.notify();
    }

    fn start_kernel(&mut self, cx: &mut Context<Self>) {
        self.kernel = Kernel::Starting;
        let (root, path) = (self.root.clone(), self.path.clone());
        let wanted = self.notebook.kernel_name().map(str::to_string);
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(connect(root, path, wanted));
        });
        cx.spawn(async move |weak, cx| {
            let Ok(result) = rx.recv().await else { return };
            let events = result.as_ref().ok().map(|c| c.events.clone());
            let _ = weak.update(cx, |this, cx| {
                match result {
                    Ok(conn) => {
                        let (name, display) = (conn.kernel_name.clone(), conn.display_name.clone());
                        if this.notebook.kernel_name() != Some(name.as_str()) {
                            this.notebook.set_kernelspec(&name, &display, "python");
                        }
                        this.kernel = Kernel::Ready { conn, status: "idle".into() };
                        this.pump_queue(cx);
                    }
                    Err(e) => {
                        this.kernel = Kernel::Failed(e);
                        for cell in &mut this.cells {
                            cell.queued = false;
                        }
                        this.queue.clear();
                    }
                }
                cx.notify();
            });
            let Some(events) = events else { return };
            while let Ok(ev) = events.recv().await {
                let mut batch = vec![ev];
                while let Ok(more) = events.try_recv() {
                    batch.push(more);
                }
                if weak.update(cx, |this, cx| this.handle_kernel_events(batch, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn cell_for(&self, req: &str) -> Option<usize> {
        req.parse().ok().and_then(|uid| self.index_of(uid))
    }

    fn handle_kernel_events(&mut self, events: Vec<KernelEvent>, cx: &mut Context<Self>) {
        let mut changed = false;
        for ev in events {
            match ev {
                KernelEvent::Status(s) => {
                    if let Kernel::Ready { status, .. } = &mut self.kernel {
                        *status = s;
                    }
                }
                KernelEvent::Started { req, count } => {
                    if let Some(ix) = self.cell_for(&req) {
                        self.notebook.cells[ix].execution_count = count;
                        changed = true;
                    }
                }
                KernelEvent::Output { req, output } => {
                    if let Some(ix) = self.cell_for(&req) {
                        self.notebook.cells[ix].push_output(output);
                        changed = true;
                    }
                }
                KernelEvent::Clear { req, .. } => {
                    if let Some(ix) = self.cell_for(&req) {
                        self.notebook.cells[ix].outputs.clear();
                        changed = true;
                    }
                }
                KernelEvent::Done { req, status, count } => {
                    if let Some(ix) = self.cell_for(&req) {
                        self.cells[ix].running = false;
                        if count.is_some() {
                            self.notebook.cells[ix].execution_count = count;
                        }
                        changed = true;
                    }
                    if status == "error" {
                        // Like Jupyter: an error stops the cells queued after it.
                        for uid in std::mem::take(&mut self.queue) {
                            if let Some(ix) = self.index_of(uid) {
                                self.cells[ix].queued = false;
                            }
                        }
                    }
                    self.pump_queue(cx);
                }
                KernelEvent::Disconnected(e) => {
                    self.kernel = Kernel::Failed(format!("Kernel connection lost: {e}"));
                    for cell in &mut self.cells {
                        cell.running = false;
                        cell.queued = false;
                    }
                    self.queue.clear();
                }
            }
        }
        if changed {
            self.parts.clear();
            self.schedule_sync(cx);
        }
        cx.notify();
    }

    fn interrupt(&mut self, cx: &mut Context<Self>) {
        if let Kernel::Ready { conn, .. } = &self.kernel {
            let (info, id) = (conn.server.info().clone(), conn.kernel_id.clone());
            std::thread::spawn(move || {
                let _ = info.interrupt_kernel(&id);
            });
        }
        self.queue.clear();
        for cell in &mut self.cells {
            cell.queued = false;
        }
        cx.notify();
    }

    fn restart(&mut self, cx: &mut Context<Self>) {
        if let Kernel::Ready { conn, .. } = &self.kernel {
            let (info, id) = (conn.server.info().clone(), conn.kernel_id.clone());
            std::thread::spawn(move || {
                let _ = info.restart_kernel(&id);
            });
        }
        self.queue.clear();
        for cell in &mut self.cells {
            cell.queued = false;
            cell.running = false;
        }
        cx.notify();
    }

    fn shutdown_kernel(&mut self) {
        if let Kernel::Ready { conn, .. } = std::mem::replace(&mut self.kernel, Kernel::Idle) {
            let Connected { server, kernel_id, session, .. } = conn;
            drop(session);
            std::thread::spawn(move || {
                let _ = server.info().shutdown_kernel(&kernel_id);
                // The local server stays up until its kernel is gone.
                drop(server);
            });
        }
    }

    fn open_url_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Jupyter server URL, e.g. https://host/lab?token=…"));
        let sub = cx.subscribe_in(&input, window, |this, input, ev: &InputEvent, _window, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                let url = input.read(cx).value().to_string();
                this.connect_url(&url, cx);
            }
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        self._subs.push(sub);
        self.url_input = Some(input);
        cx.notify();
    }

    /// Use a remote Jupyter server for this notebook's kernel (and new ones).
    fn connect_url(&mut self, url: &str, cx: &mut Context<Self>) {
        match ServerInfo::parse(url) {
            Ok(info) => {
                if let Ok(mut r) = remote_server().lock() {
                    *r = Some(info);
                }
                self.url_input = None;
                self.shutdown_kernel();
                self.start_kernel(cx);
            }
            Err(e) => self.kernel = Kernel::Failed(e),
        }
        cx.notify();
    }

    fn use_local_server(&mut self, cx: &mut Context<Self>) {
        if let Ok(mut r) = remote_server().lock() {
            *r = None;
        }
        self.shutdown_kernel();
        cx.notify();
    }

    // ---- rendering ----

    fn image_for(&mut self, base64: &str) -> Option<Arc<RenderImage>> {
        use std::hash::{Hash as _, Hasher as _};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        base64.hash(&mut h);
        let key = h.finish();
        if let Some(img) = self.images.get(&key) {
            return Some(img.clone());
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(base64).ok()?;
        let img = crate::decode_image_preview(&bytes, "PNG".into())?.render_image;
        self.images.insert(key, img.clone());
        Some(img)
    }

    fn toolbar(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (muted, fg, border, green, red, primary) =
            (theme.muted_foreground, theme.foreground, theme.border, theme.green, theme.red, theme.primary);
        let remote = remote_server().lock().ok().and_then(|r| r.clone());
        let (dot, label): (Hsla, String) = match &self.kernel {
            Kernel::Idle => (
                muted,
                match &remote {
                    Some(r) => format!("Not started · {}", ServerHandle::Remote(r.clone()).label()),
                    None => "Kernel starts on first run".into(),
                },
            ),
            Kernel::Starting => (primary, "Starting kernel…".into()),
            Kernel::Ready { conn, status } => (
                if status == "busy" { primary } else { green },
                format!("{} · {} · {}", conn.display_name, status, conn.server.label()),
            ),
            Kernel::Failed(_) => (red, "Kernel unavailable".into()),
        };
        let ready = matches!(self.kernel, Kernel::Ready { .. });

        let bar = h_flex()
            .w_full()
            .h(px(tokens::PANEL_BAR_H))
            .flex_none()
            .px(px(tokens::SPACE_5))
            .gap(px(tokens::SPACE_3))
            .items_center()
            .border_b_1()
            .border_color(border)
            .child(div().size(px(tokens::SPACE_3)).flex_none().rounded(px(tokens::RADIUS_PILL)).bg(dot))
            .child(div().min_w(px(0.)).text_size(px(tokens::UI_SM)).text_color(fg).truncate().child(label))
            .child(div().flex_1());

        if let Some(input) = &self.url_input {
            return bar.child(div().w(px(360.)).child(Input::new(input).small())).child(
                chip("nb-url-cancel", "Cancel", cx).on_click(cx.listener(|this, _, _, cx| {
                    this.url_input = None;
                    cx.notify();
                })),
            );
        }
        bar.child(chip("nb-run-all", "Run all", cx).on_click(cx.listener(|this, _, _, cx| this.run_all(cx))))
            .when(ready, |b| {
                b.child(chip("nb-interrupt", "Interrupt", cx).on_click(cx.listener(|this, _, _, cx| this.interrupt(cx))))
                    .child(chip("nb-restart", "Restart", cx).on_click(cx.listener(|this, _, _, cx| this.restart(cx))))
            })
            .child(chip("nb-clear", "Clear outputs", cx).on_click(cx.listener(|this, _, _, cx| this.clear_all_outputs(cx))))
            .child(match remote {
                Some(_) => chip("nb-local", "Use local", cx).on_click(cx.listener(|this, _, _, cx| this.use_local_server(cx))),
                None => chip("nb-connect", "Connect URL…", cx)
                    .on_click(cx.listener(|this, _, window, cx| this.open_url_input(window, cx))),
            })
    }

    fn render_outputs(&mut self, ix: usize, cx: &mut Context<Self>) -> Option<Div> {
        let uid = self.cells[ix].uid;
        let parts: Rc<Vec<Vec<OutputPart>>> = match self.parts.get(&uid) {
            Some(parts) => parts.clone(),
            None => {
                let parts: Rc<Vec<Vec<OutputPart>>> =
                    Rc::new(self.notebook.cells[ix].outputs.iter().map(notebook::output_parts).collect());
                self.parts.insert(uid, parts.clone());
                parts
            }
        };
        if parts.iter().all(Vec::is_empty) {
            return None;
        }
        let show_all = self.cells[ix].show_all_output;
        let theme = cx.theme();
        let (fg, muted, red, primary) = (theme.foreground, theme.muted_foreground, theme.red, theme.primary);
        let mono = theme.mono_font_family.clone();
        let mut col = v_flex().w_full().gap(px(tokens::SPACE_3)).py(px(tokens::SPACE_3)).font_family(mono);
        let mut shown = 0usize;
        let mut hidden = 0usize;
        let mut text_block = |text: &str, color: Hsla| {
            let all: Vec<&str> = text.trim_end_matches('\n').lines().collect();
            let room = if show_all { usize::MAX } else { OUTPUT_MAX_LINES.saturating_sub(shown) };
            let take = all.len().min(room);
            shown += take;
            hidden += all.len() - take;
            div()
                .w_full()
                .text_size(px(tokens::UI_SM))
                .line_height(px(tokens::CODE_LINE_H - tokens::SPACE_2))
                .text_color(color)
                .child(all[..take].join("\n"))
        };
        let max_img_w = CELL_MAX_W - PROMPT_W;
        for output in parts.iter() {
            for part in output.iter() {
                col = match part {
                    OutputPart::Stream { stderr, text } => col.child(text_block(text, if *stderr { red } else { fg })),
                    OutputPart::Text(text) => col.child(text_block(text, fg)),
                    OutputPart::Table(table) => col.child(render_table(table, uid, cx)),
                    OutputPart::Error { ename, evalue, traceback } => col.child(
                        v_flex()
                            .w_full()
                            .gap(px(tokens::SPACE_2))
                            .p(px(tokens::SPACE_4))
                            .rounded(px(tokens::RADIUS_SM))
                            .bg(red.opacity(0.08))
                            .border_1()
                            .border_color(red.opacity(0.25))
                            .child(
                                div()
                                    .text_size(px(tokens::UI_SM))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(red)
                                    .child(format!("{ename}: {evalue}")),
                            )
                            .child(text_block(traceback, muted)),
                    ),
                    OutputPart::Image { base64, .. } => match self.image_for(base64) {
                        Some(image) => {
                            let size = image.size(0);
                            let (w, h) = (size.width.0 as f32, size.height.0 as f32);
                            // Figures are rendered at 2x for Retina: show at half size,
                            // and never wider than the cell.
                            let shown_w = (w / 2.).min(max_img_w).max(1.);
                            let shown_h = h * shown_w / w.max(1.);
                            col.child(img(ImageSource::Render(image)).w(px(shown_w)).h(px(shown_h)).rounded(px(tokens::RADIUS_SM)))
                        }
                        None => col.child(
                            div().text_size(px(tokens::UI_XS)).text_color(muted).child("[image could not be decoded]"),
                        ),
                    },
                };
            }
        }
        if hidden > 0 {
            col = col.child(
                div()
                    .id(("nb-more-output", uid as usize))
                    .text_size(px(tokens::UI_XS))
                    .text_color(primary)
                    .cursor_pointer()
                    .child(format!("Show {hidden} more lines"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(ix) = this.index_of(uid) {
                            this.cells[ix].show_all_output = true;
                            cx.notify();
                        }
                    })),
            );
        }
        Some(col)
    }

    fn render_cell(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        use gpui_kit::assets::IconName;
        use gpui_kit::component::Icon;

        let uid = self.cells[ix].uid;
        let kind = self.notebook.cells[ix].kind;
        let selected = self.selected == Some(uid);
        let (running, queued, editing) = (self.cells[ix].running, self.cells[ix].queued, self.cells[ix].editing);
        let count = self.notebook.cells[ix].execution_count;
        let source = self.notebook.cells[ix].source.clone();
        let editor = self.cells[ix].editor.clone();
        let outputs = self.render_outputs(ix, cx);
        let theme = cx.theme();
        let (fg, muted, border, primary, raised, bg) =
            (theme.foreground, theme.muted_foreground, theme.border, theme.primary, theme.secondary, theme.background);
        let mono = theme.mono_font_family.clone();

        let prompt = match (kind, running, queued, count) {
            (CellKind::Code, true, _, _) => "[*]".to_string(),
            (CellKind::Code, _, true, _) => "[…]".to_string(),
            (CellKind::Code, _, _, Some(n)) => format!("[{n}]"),
            (CellKind::Code, _, _, None) => "[ ]".to_string(),
            _ => String::new(),
        };

        let group: SharedString = format!("nb-cell-{uid}").into();
        let lines = source.split('\n').count().max(1);
        let editor_h = lines as f32 * tokens::CODE_LINE_H + tokens::SPACE_4;

        let body: AnyElement = if kind == CellKind::Markdown && !editing {
            div()
                .id(("nb-md", uid as usize))
                .w_full()
                .px(px(tokens::SPACE_5))
                .py(px(tokens::SPACE_4))
                .text_size(px(tokens::UI))
                .text_color(fg)
                .child(TextView::markdown(("nb-md-text", uid as usize), source))
                .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                    this.selected = Some(uid);
                    if ev.click_count() >= 2 {
                        if let Some(ix) = this.index_of(uid) {
                            this.cells[ix].editing = true;
                            let handle = this.cells[ix].editor.read(cx).focus_handle(cx);
                            window.focus(&handle, cx);
                        }
                    }
                    cx.notify();
                }))
                .into_any_element()
        } else {
            div()
                .w_full()
                .h(px(editor_h))
                .py(px(tokens::SPACE_2))
                .rounded(px(tokens::RADIUS_SM))
                .bg(tokens::bg_editor())
                .border_1()
                .border_color(if selected { primary.opacity(0.45) } else { border })
                .overflow_hidden()
                .child(editor)
                .into_any_element()
        };

        let action = |id: &'static str, icon: IconName| {
            div()
                .id((id, uid as usize))
                .p(px(tokens::SPACE_2))
                .rounded(px(tokens::RADIUS_BADGE))
                .cursor_pointer()
                .text_color(muted)
                .hover(move |s| s.bg(raised).text_color(fg))
                .child(Icon::new(icon).xsmall())
        };
        let cell_tools = h_flex()
            .absolute()
            .top(px(-tokens::SPACE_5))
            .right(px(tokens::SPACE_3))
            .gap(px(tokens::SPACE_1))
            .px(px(tokens::SPACE_1))
            .rounded(px(tokens::RADIUS_SM))
            .bg(bg)
            .border_1()
            .border_color(border)
            .invisible()
            .group_hover(group.clone(), |s| s.visible())
            .when(selected, |s| s.visible())
            .child(action("nb-up", IconName::ArrowUp).on_click(cx.listener(move |this, _, _, cx| this.move_cell(uid, -1, cx))))
            .child(action("nb-down", IconName::ArrowDown).on_click(cx.listener(move |this, _, _, cx| this.move_cell(uid, 1, cx))))
            .child(
                action("nb-kind", if kind == CellKind::Code { IconName::File } else { IconName::SquareTerminal })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let to = if kind == CellKind::Code { CellKind::Markdown } else { CellKind::Code };
                        this.set_kind(uid, to, cx);
                    })),
            )
            .child(action("nb-del", IconName::Delete).on_click(cx.listener(move |this, _, _, cx| this.delete_cell(uid, cx))));

        let run_button = div()
            .id(("nb-run", uid as usize))
            .p(px(tokens::SPACE_2))
            .rounded(px(tokens::RADIUS_BADGE))
            .cursor_pointer()
            .text_color(if running { primary } else { muted })
            .hover(move |s| s.bg(raised).text_color(primary))
            .child(Icon::new(IconName::Play).xsmall())
            .on_click(cx.listener(move |this, _, _, cx| this.run_cell(uid, cx)));

        let prompt_col = v_flex()
            .w(px(PROMPT_W))
            .flex_none()
            .items_end()
            .pr(px(tokens::SPACE_4))
            .pt(px(tokens::SPACE_3))
            .gap(px(tokens::SPACE_2))
            .font_family(mono)
            .text_size(px(tokens::UI_XS))
            .text_color(if running { primary } else { muted })
            .child(prompt)
            .when(kind == CellKind::Code, |c| {
                c.child(
                    div()
                        .invisible()
                        .group_hover(group.clone(), |s| s.visible())
                        .when(selected || running, |s| s.visible())
                        .child(run_button),
                )
            });

        h_flex()
            .id(("nb-cell", uid as usize))
            .group(group)
            .w_full()
            .max_w(px(CELL_MAX_W))
            .items_start()
            .py(px(tokens::SPACE_3))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    if this.selected != Some(uid) {
                        this.selected = Some(uid);
                        cx.notify();
                    }
                }),
            )
            .capture_key_down(cx.listener(move |this, ev: &KeyDownEvent, window, cx| {
                let k = &ev.keystroke;
                if k.key == "enter" && k.modifiers.shift && !k.modifiers.platform {
                    this.run_and_advance(uid, window, cx);
                    cx.stop_propagation();
                } else if k.key == "enter" && k.modifiers.platform {
                    this.run_cell(uid, cx);
                    cx.stop_propagation();
                }
            }))
            .child(prompt_col)
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .relative()
                    .child(body)
                    .when_some(outputs, |c, o| c.child(o))
                    .child(cell_tools),
            )
            .into_any_element()
    }

    fn add_cell_row(&self, cx: &mut Context<Self>) -> Div {
        h_flex()
            .w_full()
            .max_w(px(CELL_MAX_W))
            .pl(px(PROMPT_W))
            .py(px(tokens::SPACE_4))
            .gap(px(tokens::SPACE_3))
            .child(chip("nb-add-code", "+ Code", cx).on_click(cx.listener(|this, _, window, cx| {
                let last = this.cells.len().checked_sub(1);
                this.insert_cell(last, CellKind::Code, window, cx);
            })))
            .child(chip("nb-add-md", "+ Markdown", cx).on_click(cx.listener(|this, _, window, cx| {
                let last = this.cells.len().checked_sub(1);
                this.insert_cell(last, CellKind::Markdown, window, cx);
            })))
    }
}

impl Drop for NotebookView {
    fn drop(&mut self) {
        self.shutdown_kernel();
    }
}

/// A table output drawn as a compact grid: columns weighed by their widest
/// cell, numbers right-aligned, a scrollable body, and the frame's shape.
fn render_table(table: &OutputTable, uid: u64, cx: &App) -> Div {
    let theme = cx.theme();
    let (fg, muted, border, raised) = (theme.foreground, theme.muted_foreground, theme.border, theme.secondary);
    let columns = table.columns();
    let shown_cols = columns.min(TABLE_MAX_COLS);
    let (weights, numeric) = table_layout(table, columns);
    let row_h = tokens::CODE_LINE_H + tokens::SPACE_2;
    let total_rows = table.rows.len() + table.dropped_rows;
    let cell = |c: usize, text: String, strong: bool| {
        let num = numeric.get(c).copied().unwrap_or(false);
        div()
            .flex_grow(weights.get(c).copied().unwrap_or(1.))
            .flex_basis(px(0.))
            .min_w(px(0.))
            .px(px(tokens::SPACE_4))
            .flex()
            .items_center()
            .overflow_hidden()
            .when(c > 0, |d| d.border_l_1().border_color(border.opacity(0.5)))
            .text_size(px(tokens::UI_XS))
            .when(strong, |d| d.font_weight(FontWeight::MEDIUM).text_color(muted))
            .when(!strong, |d| d.text_color(fg))
            .child(div().w_full().min_w(px(0.)).truncate().when(num, |d| d.text_right()).child(text))
    };

    let header = h_flex()
        .w_full()
        .h(px(row_h))
        .flex_none()
        .bg(raised)
        .border_b_1()
        .border_color(border)
        .children((0..shown_cols).map(|c| cell(c, table.headers.get(c).cloned().unwrap_or_default(), true)));

    let last = table.rows.len().saturating_sub(1);
    let body = div()
        .id(("nb-table-rows", uid as usize))
        .w_full()
        // Long frames scroll inside the cell instead of pushing the rest of
        // the notebook down.
        .when(table.rows.len() > 10, |d| d.max_h(px(TABLE_MAX_H)).overflow_y_scroll())
        .children(table.rows.iter().enumerate().map(|(r, row)| {
            h_flex()
                .w_full()
                .min_h(px(row_h))
                .when(r % 2 == 1, |d| d.bg(raised.opacity(0.35)))
                .when(r < last, |d| d.border_b_1().border_color(border.opacity(0.5)))
                .children((0..shown_cols).map(|c| cell(c, row.get(c).cloned().unwrap_or_default(), false)))
        }));

    let mut caption = format!("{} rows × {} columns", fmt_count(total_rows), columns);
    match (table.dropped_rows > 0, shown_cols < columns) {
        (false, false) => {}
        (true, false) => caption.push_str(&format!(" · first {} shown", fmt_count(table.rows.len()))),
        (false, true) => caption.push_str(&format!(" · first {shown_cols} columns")),
        (true, true) => {
            caption.push_str(&format!(" · first {} rows, {shown_cols} columns", fmt_count(table.rows.len())))
        }
    }

    v_flex()
        .w_full()
        .rounded(px(tokens::RADIUS_SM))
        .border_1()
        .border_color(border)
        .overflow_hidden()
        .child(header)
        .child(body)
        .child(
            h_flex()
                .w_full()
                .px(px(tokens::SPACE_4))
                .py(px(tokens::SPACE_2))
                .border_t_1()
                .border_color(border.opacity(0.5))
                .text_size(px(tokens::UI_XS))
                .text_color(muted)
                .child(caption),
        )
}

/// Column grow factors, from the widest cell (bounded), and which columns
/// read as numbers so they can be right-aligned like a DataFrame.
fn table_layout(table: &OutputTable, columns: usize) -> (Vec<f32>, Vec<bool>) {
    let mut widest = vec![0usize; columns];
    let mut numeric = vec![0usize; columns];
    let mut filled = vec![0usize; columns];
    for (c, header) in table.headers.iter().enumerate().take(columns) {
        widest[c] = header.chars().count();
    }
    for row in &table.rows {
        for c in 0..columns {
            let Some(text) = row.get(c).map(String::as_str) else { continue };
            if text.is_empty() {
                continue;
            }
            widest[c] = widest[c].max(text.chars().count());
            filled[c] += 1;
            numeric[c] += usize::from(looks_numeric(text));
        }
    }
    let weights = widest.iter().map(|&w| w.clamp(3, TABLE_COL_CHARS) as f32).collect();
    let numeric = (0..columns).map(|c| filled[c] > 0 && numeric[c] * 2 > filled[c]).collect();
    (weights, numeric)
}

fn chip(id: &'static str, label: &'static str, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let (border, muted, fg, raised) = (theme.border, theme.muted_foreground, theme.foreground, theme.secondary);
    h_flex()
        .id(id)
        .flex_none()
        .h(px(tokens::TREE_ROW_H - tokens::SPACE_3))
        .px(px(tokens::SPACE_4))
        .items_center()
        .rounded(px(tokens::RADIUS_SM))
        .border_1()
        .border_color(border)
        .text_size(px(tokens::UI_XS))
        .text_color(muted)
        .cursor_pointer()
        .hover(move |h| h.bg(raised).text_color(fg))
        .child(label)
}

impl Render for NotebookView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let toolbar = self.toolbar(cx);
        let error_banner = match &self.kernel {
            Kernel::Failed(e) => Some(e.clone()),
            _ => None,
        };
        let body: AnyElement = if let Some(err) = self.parse_error.clone() {
            let theme = cx.theme();
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(tokens::SPACE_3))
                .child(div().text_size(px(tokens::UI_LG)).text_color(theme.foreground).child("Can't read this notebook"))
                .child(div().text_size(px(tokens::UI_SM)).text_color(theme.muted_foreground).child(err))
                .child(
                    div()
                        .text_size(px(tokens::UI_XS))
                        .text_color(theme.muted_foreground)
                        .child("Fix it in Code mode; this view updates as you type."),
                )
                .into_any_element()
        } else {
            let cells: Vec<AnyElement> = (0..self.cells.len()).map(|ix| self.render_cell(ix, cx)).collect();
            v_flex()
                .id("nb-scroll")
                .flex_1()
                .min_h(px(0.))
                .w_full()
                .overflow_y_scroll()
                .items_center()
                .px(px(tokens::SPACE_6))
                .pt(px(tokens::SPACE_6))
                .pb(px(tokens::SPACE_7 * 4.))
                .children(cells)
                .child(self.add_cell_row(cx))
                .into_any_element()
        };
        let red = cx.theme().red;
        v_flex()
            .size_full()
            .min_w(px(0.))
            .min_h(px(0.))
            .bg(cx.theme().background)
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                let k = &ev.keystroke;
                if k.key == "s" && k.modifiers.platform && !k.modifiers.shift {
                    this.save(cx);
                    cx.stop_propagation();
                } else if k.key == "escape" && this.url_input.is_some() {
                    this.url_input = None;
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .child(toolbar)
            .when_some(error_banner, |el, err| {
                el.child(
                    div()
                        .w_full()
                        .px(px(tokens::SPACE_5))
                        .py(px(tokens::SPACE_3))
                        .bg(red.opacity(0.08))
                        .border_b_1()
                        .border_color(red.opacity(0.25))
                        .text_size(px(tokens::UI_SM))
                        .text_color(red)
                        .child(err),
                )
            })
            .child(body)
    }
}

/// The notebook view in a headless GPUI window.
#[cfg(test)]
mod ui_tests {
    use super::{CellKind, NotebookView, OutputTable, table_layout};
    use core_jupyter::notebook::OutputPart;
    use editor_view::EditorView;
    use gpui_kit::{AppContext as _, Entity, VisualTestContext};
    use std::time::Duration;

    const NB: &str = r##"{"cells": [
      {"cell_type": "markdown", "metadata": {}, "source": ["# Title"]},
      {"cell_type": "code", "execution_count": null, "metadata": {}, "outputs": [], "source": ["x = 6 * 7\n", "print(x)"]}
    ], "metadata": {}, "nbformat": 4, "nbformat_minor": 5}"##;

    fn open<'a>(cx: &'a mut ::gpui::TestAppContext, name: &str) -> (Entity<NotebookView>, Entity<EditorView>, &'a mut VisualTestContext) {
        cx.update(gpui_kit::init);
        let dir = std::env::temp_dir().join(format!("shame-nb-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.ipynb");
        std::fs::write(&path, NB).unwrap();
        let mut file_slot = None;
        let (view, cx) = cx.add_window_view(|_window, cx| {
            let file = cx.new(|cx| EditorView::from_buffer(core_buffer::Buffer::open(&path).unwrap(), cx));
            file_slot = Some(file.clone());
            NotebookView::new(file, dir.clone(), path.clone(), cx)
        });
        cx.run_until_parked();
        (view, file_slot.unwrap(), cx)
    }

    #[::gpui::test]
    fn loads_cells_and_writes_edits_back_to_the_file(cx: &mut ::gpui::TestAppContext) {
        let (view, file, cx) = open(cx, "edit");
        cx.update(|_, cx| {
            let v = view.read(cx);
            assert_eq!(v.cells.len(), 2);
            assert_eq!(v.notebook.cells[0].kind, CellKind::Markdown);
            assert!(!v.cells[0].editing, "markdown starts rendered");
            assert_eq!(v.notebook.cells[1].source, "x = 6 * 7\nprint(x)");
        });
        // Type into the code cell's editor: the notebook JSON follows.
        let cell_editor = cx.update(|_, cx| view.read(cx).cells[1].editor.clone());
        cx.update(|_, cx| cell_editor.update(cx, |ed, cx| ed.replace_all_text("y = 1", cx)));
        cx.executor().advance_clock(Duration::from_millis(400));
        cx.run_until_parked();
        let json = cx.update(|_, cx| file.read(cx).buffer().text());
        assert!(json.contains("\"y = 1\""), "{json}");
        assert!(cx.update(|_, cx| file.read(cx).buffer().is_dirty()));
        // Our own write doesn't rebuild the cells (the editor keeps its state).
        let same = cx.update(|_, cx| view.read(cx).cells[1].editor == cell_editor);
        assert!(same);

        // An edit made in Code mode reloads the cells.
        cx.update(|_, cx| {
            let text = file.read(cx).buffer().text().replace("# Title", "# Renamed");
            file.update(cx, |ed, cx| ed.replace_all_text(&text, cx));
        });
        cx.run_until_parked();
        cx.update(|_, cx| assert_eq!(view.read(cx).notebook.cells[0].source, "# Renamed"));
    }

    /// Kernel events land on the right cell and in the saved JSON. (The
    /// real server round trip is covered by core-jupyter's
    /// `runs_code_against_a_real_jupyter_server`; GPUI's test scheduler
    /// doesn't allow the real socket threads here.)
    #[::gpui::test]
    fn kernel_events_fill_in_outputs_and_counts(cx: &mut ::gpui::TestAppContext) {
        use core_jupyter::KernelEvent;
        let (view, file, cx) = open(cx, "events");
        let uid = cx.update(|_, cx| view.read(cx).cells[1].uid);
        let req = uid.to_string();
        cx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.cells[1].running = true;
                v.handle_kernel_events(
                    vec![
                        KernelEvent::Status("busy".into()),
                        KernelEvent::Started { req: req.clone(), count: Some(1) },
                        KernelEvent::Output {
                            req: req.clone(),
                            output: serde_json::json!({"output_type": "stream", "name": "stdout", "text": "42\n"}),
                        },
                        KernelEvent::Output {
                            req: "999".into(),
                            output: serde_json::json!({"output_type": "stream", "name": "stdout", "text": "stray\n"}),
                        },
                        KernelEvent::Done { req: req.clone(), status: "ok".into(), count: Some(1) },
                    ],
                    cx,
                );
            })
        });
        cx.executor().advance_clock(Duration::from_millis(400));
        cx.run_until_parked();
        cx.update(|_, cx| {
            let v = view.read(cx);
            assert!(!v.cells[1].running);
            assert_eq!(v.notebook.cells[1].execution_count, Some(1));
            assert_eq!(v.notebook.cells[1].outputs.len(), 1);
            let json = file.read(cx).buffer().text();
            assert!(json.contains("\"execution_count\": 1"), "{json}");
            assert!(json.contains("\"42\\n\""), "{json}");
            assert!(!json.contains("stray"));
        });
    }

    /// A DataFrame's HTML repr reaches the view as a table, and its parse is
    /// cached until the output changes.
    #[::gpui::test]
    fn dataframe_outputs_are_parsed_once_as_tables(cx: &mut ::gpui::TestAppContext) {
        use core_jupyter::KernelEvent;
        let (view, _file, cx) = open(cx, "table");
        let uid = cx.update(|_, cx| view.read(cx).cells[1].uid);
        let req = uid.to_string();
        let html = "<table><tr><th></th><th>name</th><th>score</th></tr>\
                    <tr><th>0</th><td>Ada</td><td>9.5</td></tr></table>";
        cx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.handle_kernel_events(
                    vec![KernelEvent::Output {
                        req: req.clone(),
                        output: serde_json::json!({"output_type": "display_data", "data": {
                            "text/plain": "   name  score\n0  Ada     9.5",
                            "text/html": html,
                        }}),
                    }],
                    cx,
                );
                assert!(v.parts.is_empty(), "a new output drops the parsed parts");
                let outputs = v.render_outputs(1, cx).expect("the table renders");
                drop(outputs);
                let parts = v.parts.get(&uid).expect("parsed on first render");
                assert!(matches!(&parts[0][0], OutputPart::Table(t) if t.rows[0][1] == "Ada"));

                // Rendering again reuses the cache rather than re-parsing.
                let cached = parts.clone();
                drop(v.render_outputs(1, cx));
                assert!(std::rc::Rc::ptr_eq(&cached, &v.parts[&uid]));
            });
        });
    }

    #[test]
    fn table_columns_weigh_by_content_and_numbers_align_right() {
        let table = OutputTable {
            headers: vec!["city".into(), "visits".into(), "note".into()],
            rows: vec![
                vec!["Nairobi".into(), "12".into(), "first".into()],
                vec!["Kyiv".into(), "3".into(), "second".into()],
                vec!["Oslo".into(), "128".into(), "third".into()],
            ],
            dropped_rows: 0,
        };
        let (weights, numeric) = table_layout(&table, 3);
        assert_eq!(numeric, [false, true, false]);
        assert!(weights[0] > weights[1], "the seven-letter city column is widest: {weights:?}");
        assert!(weights.iter().all(|w| *w >= 3.), "{weights:?}");
    }
}
