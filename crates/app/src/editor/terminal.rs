//! The dock's terminal: the user's `$SHELL` in a real PTY, emulated by
//! `alacritty_terminal` (VT parser, grid, scrollback, selection) and painted
//! cell by cell with GPUI. Interactive programs (vim, htop, less, password
//! prompts, progress bars) work because bytes go straight to the PTY.

use std::borrow::Cow;
use std::cell::Cell as StdCell;
use std::collections::HashMap;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Processor, Rgb};
use gpui_kit::*;

use super::dock::{TerminalEvent, extract_file_link};

const SCROLLBACK_LINES: usize = 10_000;

/// A grid size, in cells, for `Term::new` / `Term::resize`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GridSize {
    pub(crate) cols: usize,
    pub(crate) rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// Receives the emulator's events on the PTY thread. Replies the program asked
/// for go straight back to the PTY; everything the UI needs goes on the channel.
#[derive(Clone)]
pub(crate) struct Listener {
    session_id: usize,
    tx: async_channel::Sender<TerminalEvent>,
    /// A redraw is already queued: a flood of output is one redraw, not thousands.
    wake_pending: Arc<AtomicBool>,
    pty: Arc<OnceLock<EventLoopSender>>,
    window_size: Arc<Mutex<WindowSize>>,
}

impl Listener {
    fn write(&self, bytes: Vec<u8>) {
        if let Some(pty) = self.pty.get()
            && !bytes.is_empty()
        {
            let _ = pty.send(Msg::Input(Cow::Owned(bytes)));
        }
    }
}

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        let session_id = self.session_id;
        let ui = match event {
            TermEvent::Wakeup | TermEvent::MouseCursorDirty | TermEvent::CursorBlinkingChange => {
                if self.wake_pending.swap(true, Ordering::AcqRel) {
                    return;
                }
                TerminalEvent::Wakeup { session_id }
            }
            TermEvent::PtyWrite(text) => return self.write(text.into_bytes()),
            TermEvent::TextAreaSizeRequest(format) => {
                let size = *self.window_size.lock().unwrap_or_else(|e| e.into_inner());
                return self.write(format(size).into_bytes());
            }
            TermEvent::Title(title) => TerminalEvent::Title { session_id, title: Some(title) },
            TermEvent::ResetTitle => TerminalEvent::Title { session_id, title: None },
            TermEvent::ClipboardStore(_, text) => TerminalEvent::Clipboard { text },
            TermEvent::ChildExit(_) | TermEvent::Exit => TerminalEvent::Exited { session_id },
            _ => return,
        };
        let _ = self.tx.try_send(ui);
    }
}

/// The emulator plus the way to reach its PTY. Shared with the painter and the
/// PTY thread; cheap to clone.
#[derive(Clone)]
pub(crate) struct TermHandle {
    pub(crate) term: Arc<FairMutex<Term<Listener>>>,
    pty: Option<EventLoopSender>,
    window_size: Arc<Mutex<WindowSize>>,
}

impl TermHandle {
    pub(crate) fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        if let Some(pty) = &self.pty
            && !bytes.is_empty()
        {
            let _ = pty.send(Msg::Input(bytes));
        }
    }

    /// Fit the grid (and the program behind the PTY) to `size` cells.
    pub(crate) fn resize(&self, size: GridSize, cell: Size<Pixels>) {
        let mut term = self.term.lock();
        if term.columns() == size.cols && term.screen_lines() == size.rows {
            return;
        }
        term.resize(size);
        drop(term);
        let window_size = WindowSize {
            num_lines: size.rows as u16,
            num_cols: size.cols as u16,
            cell_width: f32::from(cell.width) as u16,
            cell_height: f32::from(cell.height) as u16,
        };
        *self.window_size.lock().unwrap_or_else(|e| e.into_inner()) = window_size;
        if let Some(pty) = &self.pty {
            let _ = pty.send(Msg::Resize(window_size));
        }
    }

    /// The last `max_lines` lines of scrollback and screen as plain text.
    pub(crate) fn recent_text(&self, max_lines: usize) -> String {
        recent_text(&self.term.lock(), max_lines)
    }
}

/// Where the grid was last laid out, to map the mouse onto cells.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GridLayout {
    pub(crate) origin: Point<Pixels>,
    pub(crate) cell: Size<Pixels>,
    pub(crate) cols: usize,
    pub(crate) rows: usize,
}

impl GridLayout {
    /// The viewport cell under `position` (clamped to the grid) and which half of it.
    pub(crate) fn cell_at(&self, position: Point<Pixels>) -> (usize, usize, Side) {
        let x = f32::from(position.x - self.origin.x).max(0.);
        let y = f32::from(position.y - self.origin.y).max(0.);
        let cell_w = f32::from(self.cell.width).max(1.);
        let cell_h = f32::from(self.cell.height).max(1.);
        let col = ((x / cell_w) as usize).min(self.cols.saturating_sub(1));
        let row = ((y / cell_h) as usize).min(self.rows.saturating_sub(1));
        let side = if x - col as f32 * cell_w < cell_w / 2. { Side::Left } else { Side::Right };
        (row, col, side)
    }
}

pub(crate) struct TerminalSession {
    pub(crate) id: usize,
    pub(crate) shell_name: String,
    /// Where the shell started; [`Self::live_cwd`] follows `cd`.
    pub(crate) cwd: PathBuf,
    /// The emulator. Named `lines` because the agent's context reader takes
    /// "the terminal's lines" from here.
    pub(crate) lines: TermHandle,
    shell_pid: Option<u32>,
    pty_fd: Option<RawFd>,
    /// Set by the program with an OSC title sequence.
    pub(crate) title: Option<String>,
    pub(crate) exited: bool,
    /// Whether a program other than the shell held the PTY at the last redraw.
    pub(crate) was_busy: bool,
    wake_pending: Arc<AtomicBool>,
    pub(crate) layout: Rc<StdCell<Option<GridLayout>>>,
    /// The left button is down and dragging extends the selection.
    pub(crate) selecting: bool,
    /// A button press was reported to the program (mouse mode) and not released yet.
    pub(crate) mouse_reported: bool,
    /// Sub-line scroll-wheel travel not applied yet.
    pub(crate) scroll_remainder: f32,
}

impl TerminalSession {
    /// The user's `$SHELL` at `cwd`.
    pub(crate) fn new(id: usize, cwd: PathBuf, tx: async_channel::Sender<TerminalEvent>) -> Self {
        let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
        Self::with_shell(id, cwd, tx, shell)
    }

    fn with_shell(id: usize, cwd: PathBuf, tx: async_channel::Sender<TerminalEvent>, shell: String) -> Self {
        let shell_name = Path::new(&shell).file_name().and_then(|s| s.to_str()).unwrap_or("sh").to_string();
        let window_size = WindowSize { num_lines: 24, num_cols: 80, cell_width: 8, cell_height: 17 };
        let wake_pending = Arc::new(AtomicBool::new(false));
        let listener = Listener {
            session_id: id,
            tx,
            wake_pending: wake_pending.clone(),
            pty: Arc::new(OnceLock::new()),
            window_size: Arc::new(Mutex::new(window_size)),
        };
        let config = Config { scrolling_history: SCROLLBACK_LINES, ..Config::default() };
        let size = GridSize { cols: window_size.num_cols as usize, rows: window_size.num_lines as usize };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));

        let mut env = HashMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        env.insert("TERM_PROGRAM".to_string(), "shame".to_string());
        env.insert("TERM_PROGRAM_VERSION".to_string(), env!("CARGO_PKG_VERSION").to_string());
        // Apps launched from the Dock get no locale; without one zsh mangles UTF-8.
        if std::env::var("LANG").map_or(true, |l| l.is_empty()) {
            env.insert("LANG".to_string(), "en_US.UTF-8".to_string());
        }
        let options = tty::Options {
            // A login shell, so PATH and friends come from the user's profile
            // even when the IDE was started from Finder.
            shell: Some(tty::Shell::new(shell.clone(), vec!["-l".to_string()])),
            working_directory: Some(cwd.clone()),
            drain_on_exit: false,
            env,
            ..tty::Options::default()
        };

        let mut session = Self {
            id,
            shell_name,
            cwd,
            lines: TermHandle { term: term.clone(), pty: None, window_size: listener.window_size.clone() },
            shell_pid: None,
            pty_fd: None,
            title: None,
            exited: false,
            was_busy: false,
            wake_pending,
            layout: Rc::new(StdCell::new(None)),
            selecting: false,
            mouse_reported: false,
            scroll_remainder: 0.,
        };

        let spawned = tty::new(&options, window_size, id as u64).and_then(|pty| {
            let pid = pty.child().id();
            let fd = std::os::fd::AsRawFd::as_raw_fd(pty.file());
            let event_loop = EventLoop::new(term.clone(), listener.clone(), pty, false, false)?;
            Ok((pid, fd, event_loop))
        });
        match spawned {
            Ok((pid, fd, event_loop)) => {
                let sender = event_loop.channel();
                let _ = listener.pty.set(sender.clone());
                session.lines.pty = Some(sender);
                session.shell_pid = Some(pid);
                session.pty_fd = Some(fd);
                event_loop.spawn();
            }
            Err(err) => {
                session.exited = true;
                let message = format!("\x1b[31mCould not start {shell}: {err}\x1b[0m\r\n");
                let mut parser: Processor = Processor::new();
                parser.advance(&mut *term.lock(), message.as_bytes());
            }
        }
        session
    }

    /// The UI has drawn what the last wakeup announced.
    pub(crate) fn clear_wakeup(&self) {
        self.wake_pending.store(false, Ordering::Release);
    }

    /// Bytes typed by the user: jump back to the live screen and send them.
    pub(crate) fn input(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        self.lines.term.lock().scroll_display(Scroll::Bottom);
        self.lines.write(bytes);
    }

    /// Type `command` at the prompt and press Enter.
    pub(crate) fn run_command(&self, command: &str) {
        self.input(format!("{command}\r").into_bytes());
    }

    pub(crate) fn paste(&self, text: &str) {
        let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
        self.input(paste_bytes(text, bracketed));
    }

    pub(crate) fn mode(&self) -> TermMode {
        *self.lines.term.lock().mode()
    }

    /// The process group in the foreground of the PTY (the shell when idle).
    fn foreground_pid(&self) -> Option<i32> {
        let fd = self.pty_fd.filter(|_| !self.exited)?;
        // SAFETY: `fd` is the PTY master, open for as long as the PTY thread runs.
        let pgrp = unsafe { libc::tcgetpgrp(fd) };
        (pgrp > 0).then_some(pgrp)
    }

    /// Something other than the shell is running in the foreground.
    pub(crate) fn is_busy(&self) -> bool {
        match (self.foreground_pid(), self.shell_pid) {
            (Some(fg), Some(shell)) => fg as u32 != shell,
            _ => false,
        }
    }

    /// The working directory of the foreground program (follows `cd`).
    pub(crate) fn live_cwd(&self) -> PathBuf {
        self.foreground_pid()
            .or(self.shell_pid.map(|p| p as i32))
            .and_then(process_cwd)
            .unwrap_or_else(|| self.cwd.clone())
    }

    pub(crate) fn dir_label(&self) -> String {
        let cwd = self.live_cwd();
        cwd.file_name().and_then(|n| n.to_str()).unwrap_or("/").to_string()
    }

    pub(crate) fn tab_title(&self) -> String {
        match &self.title {
            Some(title) if !title.trim().is_empty() => title.clone(),
            _ => format!("{} \u{2014} {}", self.dir_label(), self.shell_name),
        }
    }

    pub(crate) fn selected_text(&self) -> Option<String> {
        self.lines.term.lock().selection_to_string().filter(|s| !s.is_empty())
    }

    pub(crate) fn select_all(&mut self) {
        select_all(&mut self.lines.term.lock());
        self.selecting = false;
    }

    /// Start a selection at a viewport cell: a plain click starts a drag, a
    /// double click takes the word, a triple click the line.
    pub(crate) fn begin_selection(&mut self, row: usize, col: usize, side: Side, clicks: usize) {
        let mut term = self.lines.term.lock();
        let point = viewport_point(&term, row, col);
        begin_selection(&mut term, point, side, clicks);
        self.selecting = clicks < 2;
    }

    pub(crate) fn extend_selection(&mut self, row: usize, col: usize, side: Side) {
        let mut term = self.lines.term.lock();
        let point = viewport_point(&term, row, col);
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    /// Mouse released: a click without a drag selects nothing.
    pub(crate) fn end_selection(&mut self) {
        self.selecting = false;
        let mut term = self.lines.term.lock();
        if term.selection.as_ref().is_some_and(Selection::is_empty) {
            term.selection = None;
        }
    }

    /// ⌘K: drop the scrollback and the screen; an idle shell redraws its prompt.
    pub(crate) fn clear(&self) {
        let mut term = self.lines.term.lock();
        term.selection = None;
        term.grid_mut().clear_history();
        drop(term);
        if !self.is_busy() {
            // Ctrl+L: the shell clears the screen and reprints the prompt.
            self.lines.write(&b"\x0c"[..]);
        }
    }

    /// Scroll through the scrollback by `lines` (positive is up, into history).
    pub(crate) fn scroll_lines(&self, lines: i32) {
        if lines != 0 {
            self.lines.term.lock().scroll_display(Scroll::Delta(lines));
        }
    }

    pub(crate) fn scroll_page(&self, up: bool) {
        self.lines.term.lock().scroll_display(if up { Scroll::PageUp } else { Scroll::PageDown });
    }

    /// The text of viewport row `row` with any soft-wrapped neighbours joined
    /// in, and the column of the row's first cell within that text.
    pub(crate) fn logical_line_at(&self, row: usize) -> (String, usize) {
        let term = self.lines.term.lock();
        let point = viewport_point(&term, row, 0);
        logical_line(&term, point.line)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        // The PTY thread exits and hangs up on the shell.
        if let Some(pty) = &self.lines.pty {
            let _ = pty.send(Msg::Shutdown);
        }
    }
}

fn viewport_point<T>(term: &Term<T>, row: usize, col: usize) -> GridPoint {
    let offset = term.grid().display_offset() as i32;
    let col = col.min(term.columns().saturating_sub(1));
    GridPoint::new(Line(row as i32 - offset), Column(col))
}

pub(crate) fn selection_type(clicks: usize) -> SelectionType {
    match clicks {
        2 => SelectionType::Semantic,
        n if n >= 3 => SelectionType::Lines,
        _ => SelectionType::Simple,
    }
}

pub(crate) fn begin_selection<T>(term: &mut Term<T>, point: GridPoint, side: Side, clicks: usize) {
    term.selection = Some(Selection::new(selection_type(clicks), point, side));
}

pub(crate) fn select_all<T>(term: &mut Term<T>) {
    let start = GridPoint::new(term.topmost_line(), Column(0));
    let mut selection = Selection::new(SelectionType::Lines, start, Side::Left);
    selection.update(GridPoint::new(term.bottommost_line(), term.last_column()), Side::Right);
    term.selection = Some(selection);
}

/// The last `max_lines` non-blank-trailing lines of scrollback and screen.
pub(crate) fn recent_text<T>(term: &Term<T>, max_lines: usize) -> String {
    let start = GridPoint::new(term.topmost_line(), Column(0));
    let end = GridPoint::new(term.bottommost_line(), term.last_column());
    let text = term.bounds_to_string(start, end);
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let end = lines.iter().rposition(|l| !l.is_empty()).map_or(0, |ix| ix + 1);
    lines[end.saturating_sub(max_lines)..end].join("\n")
}

/// The logical line (soft wraps joined) containing grid line `line`, and the
/// column offset of `line`'s first cell within it.
fn logical_line<T>(term: &Term<T>, line: Line) -> (String, usize) {
    let grid = term.grid();
    let cols = term.columns();
    let wraps = |l: Line| grid[l][term.last_column()].flags.contains(Flags::WRAPLINE);
    let mut start = line;
    while start > term.topmost_line() && wraps(start - 1) {
        start = start - 1;
    }
    let mut end = line;
    while end < term.bottommost_line() && wraps(end) {
        end = end + 1;
    }
    let text = term.bounds_to_string(GridPoint::new(start, Column(0)), GridPoint::new(end, term.last_column()));
    let offset = (line.0 - start.0) as usize * cols;
    (text.trim_end_matches('\n').to_string(), offset)
}

/// A `path:line[:col]` under character `col` of `text`, resolved against `cwd`.
pub(crate) fn file_link_at(text: &str, col: usize, cwd: &Path) -> Option<(PathBuf, u32, u32)> {
    let chars: Vec<char> = text.chars().collect();
    if chars.get(col).is_none_or(|c| c.is_whitespace()) {
        return None;
    }
    let from = chars[..col].iter().rposition(|c| c.is_whitespace()).map_or(0, |ix| ix + 1);
    let to = chars[col..].iter().position(|c| c.is_whitespace()).map_or(chars.len(), |ix| col + ix);
    let token: String = chars[from..to].iter().collect();
    extract_file_link(&token, cwd)
}

/// What a paste sends: bracketed so the shell doesn't run it line by line,
/// or with newlines as Enter when the program didn't ask for brackets.
pub(crate) fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        // A pasted end marker would let the rest run as typed input.
        let clean = text.replace('\x1b', "");
        format!("\x1b[200~{clean}\x1b[201~").into_bytes()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// The xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
fn modifier_param(m: &Modifiers) -> u8 {
    1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.control as u8
}

/// The bytes a key press sends to the program, or `None` when it isn't the
/// terminal's to handle (⌘ shortcuts, bare modifiers).
pub(crate) fn key_to_bytes(keystroke: &Keystroke, mode: TermMode, capslock: bool) -> Option<Vec<u8>> {
    let m = &keystroke.modifiers;
    let key = keystroke.key.as_str();
    if m.platform {
        // macOS line editing: ⌘← / ⌘→ to the start / end, ⌘⌫ kills the line.
        return match key {
            "left" => Some(vec![0x01]),
            "right" => Some(vec![0x05]),
            "backspace" => Some(vec![0x15]),
            _ => None,
        };
    }
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let modified = m.shift || m.alt || m.control;
    let esc = |rest: &[u8]| [&b"\x1b"[..], rest].concat();
    // Cursor keys: `CSI 1;m X` with modifiers, `SS3 X` in application mode.
    let cursor = |letter: char| -> Vec<u8> {
        if modified {
            format!("\x1b[1;{}{letter}", modifier_param(m)).into_bytes()
        } else if app_cursor {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[{letter}").into_bytes()
        }
    };
    // Editing keys: `CSI n ~`, `CSI n;m ~` with modifiers.
    let tilde = |n: u8| -> Vec<u8> {
        if modified {
            format!("\x1b[{n};{}~", modifier_param(m)).into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let function_key = key.strip_prefix('f').and_then(|n| n.parse::<usize>().ok()).filter(|n| (1..=12).contains(n));
    let bytes = match key {
        "enter" if m.alt => esc(b"\r"),
        "enter" => b"\r".to_vec(),
        "tab" if m.shift => b"\x1b[Z".to_vec(),
        "tab" => b"\t".to_vec(),
        "escape" => b"\x1b".to_vec(),
        "backspace" if m.control => vec![0x08],
        "backspace" if m.alt => esc(b"\x7f"),
        "backspace" => vec![0x7f],
        "space" if m.control => vec![0x00],
        "space" if m.alt => esc(b" "),
        "space" => b" ".to_vec(),
        // ⌥← / ⌥→ jump words, as readline and zsh expect on macOS.
        "left" if m.alt && !m.shift && !m.control => esc(b"b"),
        "right" if m.alt && !m.shift && !m.control => esc(b"f"),
        "up" => cursor('A'),
        "down" => cursor('B'),
        "right" => cursor('C'),
        "left" => cursor('D'),
        "home" => cursor('H'),
        "end" => cursor('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        _ if function_key.is_some_and(|n| n <= 4) => {
            let letter = ['P', 'Q', 'R', 'S'][function_key? - 1];
            if modified {
                format!("\x1b[1;{}{letter}", modifier_param(m)).into_bytes()
            } else {
                format!("\x1bO{letter}").into_bytes()
            }
        }
        _ if function_key.is_some() => tilde([15, 17, 18, 19, 20, 21, 23, 24][function_key? - 5]),
        _ if m.control => {
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            let byte = match c.to_ascii_lowercase() {
                c @ 'a'..='z' => c as u8 & 0x1f,
                '@' | '2' | ' ' => 0x00,
                '[' | '3' => 0x1b,
                '\\' | '4' => 0x1c,
                ']' | '5' => 0x1d,
                '^' | '6' => 0x1e,
                '_' | '-' | '/' | '7' => 0x1f,
                '?' | '8' => 0x7f,
                _ => return None,
            };
            if m.alt { esc(&[byte]) } else { vec![byte] }
        }
        _ => {
            // Option types the character macOS composes (ß, é, …); only keys
            // with no such character fall back to Meta (an ESC prefix).
            let typed = keystroke
                .key_char
                .clone()
                .or_else(|| (key.chars().count() == 1).then(|| key.to_string()))?;
            // GPUI builds `key_char` from the key and Shift/Option only, so
            // Caps Lock would otherwise type lowercase.
            let typed = if capslock && !m.shift { typed.to_uppercase() } else { typed };
            if m.alt && keystroke.key_char.is_none() { esc(typed.as_bytes()) } else { typed.into_bytes() }
        }
    };
    Some(bytes)
}

/// A mouse report for a program that turned mouse mode on: `button` is 0/1/2
/// for left/middle/right, 64/65 for wheel up/down; `cell` is (col, row).
pub(crate) fn mouse_report(
    button: u8,
    pressed: bool,
    motion: bool,
    cell: (usize, usize),
    m: &Modifiers,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let mods = 4 * m.shift as u8 + 8 * m.alt as u8 + 16 * m.control as u8;
    let motion = if motion { 32 } else { 0 };
    let (col, row) = (cell.0 + 1, cell.1 + 1);
    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if pressed { 'M' } else { 'm' };
        return Some(format!("\x1b[<{};{col};{row}{end}", button + mods + motion).into_bytes());
    }
    // Legacy X10 encoding: one byte per value, and a release can't say which button.
    let code = if pressed { button } else { 3 } + mods + motion;
    if col + 32 > 255 || row + 32 > 255 {
        return None;
    }
    Some(vec![0x1b, b'[', b'M', 32 + code, 32 + col as u8, 32 + row as u8])
}

/// Theme colours for the 16 ANSI colours and the terminal's own.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TerminalPalette {
    pub(crate) ansi: [Hsla; 16],
    pub(crate) foreground: Hsla,
    pub(crate) background: Hsla,
    pub(crate) cursor: Hsla,
    pub(crate) selection: Hsla,
}

impl TerminalPalette {
    fn named(&self, color: NamedColor) -> Hsla {
        use NamedColor::*;
        let dim = |c: Hsla| c.opacity(0.7);
        match color {
            Foreground | BrightForeground => self.foreground,
            Background => self.background,
            Cursor => self.cursor,
            DimForeground => dim(self.foreground),
            DimBlack => dim(self.ansi[0]),
            DimRed => dim(self.ansi[1]),
            DimGreen => dim(self.ansi[2]),
            DimYellow => dim(self.ansi[3]),
            DimBlue => dim(self.ansi[4]),
            DimMagenta => dim(self.ansi[5]),
            DimCyan => dim(self.ansi[6]),
            DimWhite => dim(self.ansi[7]),
            ansi => self.ansi[ansi as usize % 16],
        }
    }

    /// Resolve a cell colour: colours the program redefined (OSC 4) win, then
    /// the theme for the 16 named ones, then the xterm 256-colour table.
    pub(crate) fn resolve(&self, color: Color, overrides: &Colors) -> Hsla {
        match color {
            Color::Spec(rgb) => rgb_to_hsla(rgb),
            Color::Named(named) => overrides[named as usize].map_or_else(|| self.named(named), rgb_to_hsla),
            Color::Indexed(ix) => overrides[ix as usize].map_or_else(
                || if ix < 16 { self.ansi[ix as usize] } else { rgb_to_hsla(xterm_256(ix)) },
                rgb_to_hsla,
            ),
        }
    }
}

pub(crate) fn rgb_to_hsla(rgb: Rgb) -> Hsla {
    Rgba { r: rgb.r as f32 / 255., g: rgb.g as f32 / 255., b: rgb.b as f32 / 255., a: 1. }.into()
}

/// Colours 16–255 of the xterm palette: a 6×6×6 cube, then 24 greys.
pub(crate) fn xterm_256(ix: u8) -> Rgb {
    match ix {
        16..=231 => {
            let i = ix - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Rgb { r: level(i / 36), g: level(i / 6 % 6), b: level(i % 6) }
        }
        232..=255 => {
            let v = 8 + (ix - 232) * 10;
            Rgb { r: v, g: v, b: v }
        }
        // The first 16 come from the theme; this is only a fallback.
        _ => Rgb { r: 0xc0, g: 0xc0, b: 0xc0 },
    }
}

/// The grid's cell size for `font` at `font_size`.
pub(crate) fn cell_size(font: &Font, font_size: Pixels, window: &Window) -> Size<Pixels> {
    let text = window.text_system();
    let font_id = text.resolve_font(font);
    let width = text.advance(font_id, font_size, 'm').map(|s| s.width).unwrap_or(font_size * 0.6);
    size(width, (font_size * 1.45).round())
}

/// Cells painted as one shaped line. A wide glyph gets a segment of its own
/// so the cells after it stay on the grid.
struct Segment {
    row: usize,
    col: usize,
    cells: usize,
    wide: bool,
    text: String,
    runs: Vec<TextRun>,
}

/// Paint the visible grid: backgrounds, selection, cursor, then text.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_grid(
    handle: &TermHandle,
    layout: GridLayout,
    palette: &TerminalPalette,
    font: &Font,
    font_size: Pixels,
    focused: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let cell_w = layout.cell.width;
    let line_h = layout.cell.height;
    let at = |row: usize, col: usize| {
        point(layout.origin.x + cell_w * col as f32, layout.origin.y + line_h * row as f32)
    };
    let mut segments: Vec<Segment> = Vec::new();
    let mut quads: Vec<(Bounds<Pixels>, Hsla)> = Vec::new();

    let term = handle.term.lock();
    let content = term.renderable_content();
    let offset = content.display_offset as i32;
    let selection = content.selection;
    let colors = content.colors;
    let cursor = content.cursor;
    let cursor_row = cursor.point.line.0 + offset;
    let show_cursor = cursor.shape != CursorShape::Hidden && (0..layout.rows as i32).contains(&cursor_row);
    let block_cursor = show_cursor && focused && cursor.shape == CursorShape::Block;

    for indexed in content.display_iter {
        let row = (indexed.point.line.0 + offset) as usize;
        let col = indexed.point.column.0;
        if row >= layout.rows || col >= layout.cols {
            continue;
        }
        let cell = indexed.cell;
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let wide = cell.flags.contains(Flags::WIDE_CHAR);
        let cells = if wide { 2 } else { 1 };
        let cell_bounds = Bounds::new(at(row, col), size(cell_w * cells as f32, line_h));

        let mut fg = palette.resolve(cell.fg, colors);
        let mut bg = palette.resolve(cell.bg, colors);
        let inverse = cell.flags.contains(Flags::INVERSE);
        if inverse {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.flags.contains(Flags::DIM) {
            fg = fg.opacity(0.7);
        }
        if cell.flags.contains(Flags::HIDDEN) {
            fg = bg;
        }
        if inverse || cell.bg != Color::Named(NamedColor::Background) {
            quads.push((cell_bounds, bg));
        }
        if selection.is_some_and(|s| s.contains(indexed.point)) {
            quads.push((cell_bounds, palette.selection));
        }
        if block_cursor && row as i32 == cursor_row && col == cursor.point.column.0 {
            quads.push((cell_bounds, palette.cursor));
            fg = palette.background;
        }

        let mut text = String::new();
        text.push(if cell.c == '\t' || cell.c == '\0' { ' ' } else { cell.c });
        if let Some(zero_width) = cell.zerowidth() {
            text.extend(zero_width);
        }
        let mut run_font = font.clone();
        if cell.flags.contains(Flags::BOLD) {
            run_font.weight = FontWeight::BOLD;
        }
        if cell.flags.contains(Flags::ITALIC) {
            run_font.style = FontStyle::Italic;
        }
        let underline = cell.flags.intersects(Flags::ALL_UNDERLINES).then_some(UnderlineStyle {
            thickness: px(1.),
            color: Some(fg),
            wavy: cell.flags.contains(Flags::UNDERCURL),
        });
        let strikethrough = cell
            .flags
            .contains(Flags::STRIKEOUT)
            .then_some(StrikethroughStyle { thickness: px(1.), color: Some(fg) });
        let run = TextRun { len: text.len(), font: run_font, color: fg, background_color: None, underline, strikethrough };

        match segments.last_mut() {
            Some(segment) if segment.row == row && !segment.wide && !wide && segment.col + segment.cells == col => {
                segment.text.push_str(&text);
                segment.cells += 1;
                match segment.runs.last_mut() {
                    Some(last)
                        if last.font == run.font
                            && last.color == run.color
                            && last.underline == run.underline
                            && last.strikethrough == run.strikethrough =>
                    {
                        last.len += run.len;
                    }
                    _ => segment.runs.push(run),
                }
            }
            _ => segments.push(Segment { row, col, cells, wide, text, runs: vec![run] }),
        }
    }
    drop(term);

    for (bounds, color) in quads {
        window.paint_quad(fill(bounds, color));
    }
    let text_system = window.text_system().clone();
    for segment in segments {
        if segment.text.trim().is_empty() {
            continue;
        }
        let shaped = text_system.shape_line(segment.text.into(), font_size, &segment.runs, Some(cell_w));
        let _ = shaped.paint(at(segment.row, segment.col), line_h, TextAlign::Left, None, window, cx);
    }

    if show_cursor && !block_cursor {
        let origin = at(cursor_row as usize, cursor.point.column.0);
        let quad = match cursor.shape {
            CursorShape::Beam => fill(Bounds::new(origin, size(px(2.), line_h)), palette.cursor),
            CursorShape::Underline => fill(
                Bounds::new(point(origin.x, origin.y + line_h - px(2.)), size(cell_w, px(2.))),
                palette.cursor,
            ),
            _ => outline(Bounds::new(origin, size(cell_w, line_h)), palette.cursor, BorderStyle::Solid),
        };
        window.paint_quad(quad);
    }
}

/// The working directory of process `pid`.
#[cfg(target_os = "macos")]
fn process_cwd(pid: i32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    // SAFETY: a plain-old-data out parameter, sized for the call.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as i32;
    let written = unsafe {
        libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut info).cast(), size)
    };
    if written != size {
        return None;
    }
    // SAFETY: the kernel NUL-terminates the path inside the buffer.
    let path = unsafe { std::ffi::CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast()) };
    let path = Path::new(std::ffi::OsStr::from_bytes(path.to_bytes()));
    (!path.as_os_str().is_empty()).then(|| path.to_path_buf())
}

#[cfg(target_os = "linux")]
fn process_cwd(pid: i32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_cwd(_pid: i32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;

    fn key(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke { modifiers, key: key.into(), key_char: key_char.map(Into::into) }
    }

    fn bytes(keystroke: Keystroke) -> Option<Vec<u8>> {
        key_to_bytes(&keystroke, TermMode::default(), false)
    }

    fn term_with(text: &str, cols: usize, rows: usize) -> Term<VoidListener> {
        let mut term = Term::new(Config::default(), &GridSize { cols, rows }, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, text.as_bytes());
        term
    }

    fn at(line: i32, col: usize) -> GridPoint {
        GridPoint::new(Line(line), Column(col))
    }

    #[core::prelude::v1::test]
    fn plain_keys_and_caps_lock() {
        let none = Modifiers::default();
        let shift = Modifiers { shift: true, ..Modifiers::default() };
        assert_eq!(bytes(key("a", Some("a"), none)), Some(b"a".to_vec()));
        assert_eq!(key_to_bytes(&key("a", Some("a"), none), TermMode::default(), true), Some(b"A".to_vec()));
        // Shift with Caps Lock on still types the shifted character.
        assert_eq!(key_to_bytes(&key("a", Some("A"), shift), TermMode::default(), true), Some(b"A".to_vec()));
        assert_eq!(bytes(key("enter", None, none)), Some(b"\r".to_vec()));
        assert_eq!(bytes(key("tab", None, shift)), Some(b"\x1b[Z".to_vec()));
        assert_eq!(bytes(key("backspace", None, none)), Some(vec![0x7f]));
        assert_eq!(bytes(key("escape", None, none)), Some(vec![0x1b]));
        assert_eq!(bytes(key("space", None, none)), Some(b" ".to_vec()));
        // Option composes characters macOS-style rather than acting as Meta.
        let alt = Modifiers { alt: true, ..Modifiers::default() };
        assert_eq!(bytes(key("s", Some("ß"), alt)), Some("ß".as_bytes().to_vec()));
        assert_eq!(bytes(key("s", None, alt)), Some(b"\x1bs".to_vec()));
    }

    #[core::prelude::v1::test]
    fn control_combos_become_control_bytes() {
        let ctrl = Modifiers { control: true, ..Modifiers::default() };
        assert_eq!(bytes(key("c", None, ctrl)), Some(vec![0x03]));
        assert_eq!(bytes(key("d", None, ctrl)), Some(vec![0x04]));
        assert_eq!(bytes(key("l", None, ctrl)), Some(vec![0x0c]));
        assert_eq!(bytes(key("[", None, ctrl)), Some(vec![0x1b]));
        assert_eq!(bytes(key("space", None, ctrl)), Some(vec![0x00]));
        let ctrl_alt = Modifiers { control: true, alt: true, ..Modifiers::default() };
        assert_eq!(bytes(key("b", None, ctrl_alt)), Some(vec![0x1b, 0x02]));
    }

    #[core::prelude::v1::test]
    fn cursor_and_function_keys() {
        let none = Modifiers::default();
        assert_eq!(bytes(key("up", None, none)), Some(b"\x1b[A".to_vec()));
        assert_eq!(
            key_to_bytes(&key("up", None, none), TermMode::APP_CURSOR, false),
            Some(b"\x1bOA".to_vec()),
            "vim and less put the cursor keys in application mode"
        );
        let shift = Modifiers { shift: true, ..Modifiers::default() };
        assert_eq!(bytes(key("right", None, shift)), Some(b"\x1b[1;2C".to_vec()));
        let ctrl = Modifiers { control: true, ..Modifiers::default() };
        assert_eq!(bytes(key("left", None, ctrl)), Some(b"\x1b[1;5D".to_vec()));
        let alt = Modifiers { alt: true, ..Modifiers::default() };
        assert_eq!(bytes(key("left", None, alt)), Some(b"\x1bb".to_vec()), "⌥← moves a word back");
        assert_eq!(bytes(key("delete", None, none)), Some(b"\x1b[3~".to_vec()));
        assert_eq!(bytes(key("pageup", None, none)), Some(b"\x1b[5~".to_vec()));
        assert_eq!(bytes(key("f1", None, none)), Some(b"\x1bOP".to_vec()));
        assert_eq!(bytes(key("f5", None, none)), Some(b"\x1b[15~".to_vec()));
        assert_eq!(bytes(key("f12", None, none)), Some(b"\x1b[24~".to_vec()));
    }

    #[core::prelude::v1::test]
    fn command_shortcuts_stay_with_the_ide() {
        let cmd = Modifiers { platform: true, ..Modifiers::default() };
        assert_eq!(bytes(key("s", Some("s"), cmd)), None);
        assert_eq!(bytes(key("left", None, cmd)), Some(vec![0x01]), "⌘← goes to the start of the line");
        assert_eq!(bytes(key("backspace", None, cmd)), Some(vec![0x15]));
        assert_eq!(bytes(key("shift", None, Modifiers::default())), None, "a bare modifier types nothing");
    }

    #[core::prelude::v1::test]
    fn pastes_are_bracketed_when_asked() {
        assert_eq!(paste_bytes("echo a\necho b", true), b"\x1b[200~echo a\necho b\x1b[201~".to_vec());
        assert_eq!(paste_bytes("x\x1b[201~rm -rf /", true), b"\x1b[200~x[201~rm -rf /\x1b[201~".to_vec());
        assert_eq!(paste_bytes("a\r\nb\nc", false), b"a\rb\rc".to_vec());
    }

    #[core::prelude::v1::test]
    fn mouse_reports_in_sgr_and_legacy_encodings() {
        let none = Modifiers::default();
        assert_eq!(mouse_report(0, true, false, (4, 2), &none, TermMode::SGR_MOUSE), Some(b"\x1b[<0;5;3M".to_vec()));
        assert_eq!(mouse_report(0, false, false, (4, 2), &none, TermMode::SGR_MOUSE), Some(b"\x1b[<0;5;3m".to_vec()));
        assert_eq!(mouse_report(64, true, false, (0, 0), &none, TermMode::SGR_MOUSE), Some(b"\x1b[<64;1;1M".to_vec()));
        assert_eq!(mouse_report(0, true, false, (0, 0), &none, TermMode::empty()), Some(vec![0x1b, b'[', b'M', 32, 33, 33]));
        assert_eq!(mouse_report(0, false, false, (0, 0), &none, TermMode::empty()), Some(vec![0x1b, b'[', b'M', 35, 33, 33]));
        assert_eq!(mouse_report(0, true, false, (300, 0), &none, TermMode::empty()), None, "too far right for one byte");
    }

    fn palette() -> TerminalPalette {
        let grey = |l: f32| Hsla { h: 0., s: 0., l, a: 1. };
        let mut ansi = [grey(0.5); 16];
        ansi[1] = Hsla { h: 0., s: 0.8, l: 0.5, a: 1. };
        TerminalPalette { ansi, foreground: grey(0.9), background: grey(0.1), cursor: grey(0.7), selection: grey(0.3) }
    }

    #[core::prelude::v1::test]
    fn colors_map_from_theme_256_table_and_truecolor() {
        let palette = palette();
        let defaults = Colors::default();
        assert_eq!(palette.resolve(Color::Named(NamedColor::Red), &defaults), palette.ansi[1]);
        assert_eq!(palette.resolve(Color::Indexed(1), &defaults), palette.ansi[1], "indexed 0-15 are the theme's");
        assert_eq!(palette.resolve(Color::Named(NamedColor::Foreground), &defaults), palette.foreground);
        assert_eq!(palette.resolve(Color::Named(NamedColor::Background), &defaults), palette.background);
        assert_eq!(palette.resolve(Color::Named(NamedColor::DimRed), &defaults), palette.ansi[1].opacity(0.7));
        let red = Rgb { r: 255, g: 0, b: 0 };
        assert_eq!(palette.resolve(Color::Spec(red), &defaults), rgb_to_hsla(red));
        assert_eq!(palette.resolve(Color::Indexed(196), &defaults), rgb_to_hsla(red));

        assert_eq!(xterm_256(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(xterm_256(21), Rgb { r: 0, g: 0, b: 255 });
        assert_eq!(xterm_256(231), Rgb { r: 255, g: 255, b: 255 });
        assert_eq!(xterm_256(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(xterm_256(255), Rgb { r: 238, g: 238, b: 238 });
    }

    #[core::prelude::v1::test]
    fn a_drag_copies_across_lines_in_either_direction() {
        let mut term = term_with("hello world\r\nsecond line\r\nthird", 20, 3);
        // Dragged upwards: from "thi|rd" back to "hello |world".
        begin_selection(&mut term, at(2, 3), Side::Left, 1);
        term.selection.as_mut().unwrap().update(at(0, 6), Side::Left);
        assert_eq!(term.selection_to_string().as_deref(), Some("world\nsecond line\nthi"));
    }

    #[core::prelude::v1::test]
    fn clicks_select_nothing_a_word_or_a_line() {
        let mut term = term_with("cargo build --release", 30, 2);
        begin_selection(&mut term, at(0, 8), Side::Left, 1);
        assert!(term.selection.as_ref().is_some_and(Selection::is_empty), "a click alone selects nothing");
        begin_selection(&mut term, at(0, 8), Side::Left, 2);
        assert_eq!(term.selection_to_string().as_deref(), Some("build"));
        begin_selection(&mut term, at(0, 8), Side::Left, 3);
        assert_eq!(term.selection_to_string().as_deref(), Some("cargo build --release\n"));
        select_all(&mut term);
        assert_eq!(term.selection_to_string().as_deref().map(str::trim_end), Some("cargo build --release"));
    }

    #[core::prelude::v1::test]
    fn select_all_reaches_into_the_scrollback() {
        let mut term = term_with("one\r\ntwo\r\nthree\r\nfour", 10, 2);
        select_all(&mut term);
        assert_eq!(term.selection_to_string().as_deref().map(str::trim_end), Some("one\ntwo\nthree\nfour"));
        assert_eq!(recent_text(&term, 2), "three\nfour");
        assert_eq!(recent_text(&term, 10), "one\ntwo\nthree\nfour");
    }

    #[core::prelude::v1::test]
    fn colors_and_cursor_moves_land_in_the_grid() {
        // Red "ok", then overwrite the start of the line after a carriage return.
        let term = term_with("\x1b[31mok\x1b[0m done\rOK", 20, 2);
        assert_eq!(recent_text(&term, 5), "OK done");
        assert_eq!(term.grid()[Line(0)][Column(3)].c, 'd');
        assert_eq!(term.grid()[Line(0)][Column(0)].fg, Color::Named(NamedColor::Foreground));
        let term = term_with("\x1b[31mok", 20, 2);
        assert_eq!(term.grid()[Line(0)][Column(0)].fg, Color::Named(NamedColor::Red));
    }

    #[core::prelude::v1::test]
    fn soft_wrapped_lines_are_read_back_whole() {
        let term = term_with("abcdefghij", 4, 3);
        assert_eq!(logical_line(&term, Line(1)), ("abcdefghij".to_string(), 4));
    }

    #[core::prelude::v1::test]
    fn finds_the_file_link_under_the_mouse() {
        let dir = std::env::temp_dir().join(format!("dart-ide-term-link-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib/main.dart"), "void main() {}\n").unwrap();
        let line = "error: lib/main.dart:12:5: Expected ';'";
        let col = line.find("main.dart").unwrap();
        assert_eq!(file_link_at(line, col, &dir), Some((dir.join("lib/main.dart"), 11, 4)));
        assert_eq!(file_link_at(line, 0, &dir), None, "not on the link");
        assert_eq!(file_link_at(line, 6, &dir), None, "on whitespace");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real thing: a shell in a PTY runs a command, its output reaches the
    /// grid, a long-running program shows as busy, and resizing reaches the program.
    #[core::prelude::v1::test]
    fn a_shell_in_a_pty_runs_commands() {
        let wait_for = |what: &str, check: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !check() {
                assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        let (tx, rx) = async_channel::unbounded();
        let session = TerminalSession::with_shell(7, std::env::temp_dir(), tx, "/bin/sh".into());
        assert!(!session.exited, "the shell should start");

        session.run_command("printf 'pty-%s\\n' ok");
        wait_for("the command's output", &|| session.lines.recent_text(50).contains("pty-ok"));
        assert!(matches!(rx.try_recv(), Ok(TerminalEvent::Wakeup { session_id: 7 })));

        session.lines.resize(GridSize { cols: 50, rows: 7 }, size(px(8.), px(16.)));
        session.run_command("stty size; sleep 3");
        wait_for("stty to see the new size", &|| session.lines.recent_text(50).contains("7 50"));
        wait_for("sleep to take the foreground", &|| session.is_busy());
        session.input(&b"\x03"[..]);
        wait_for("Ctrl+C to stop it", &|| !session.is_busy());
    }

    #[core::prelude::v1::test]
    fn maps_the_mouse_to_cells() {
        let layout = GridLayout { origin: point(px(10.), px(20.)), cell: size(px(8.), px(16.)), cols: 80, rows: 24 };
        assert_eq!(layout.cell_at(point(px(10.), px(20.))), (0, 0, Side::Left));
        assert_eq!(layout.cell_at(point(px(10. + 8. * 3. + 6.), px(20. + 16. * 2. + 1.))), (2, 3, Side::Right));
        assert_eq!(layout.cell_at(point(px(5000.), px(5000.))), (23, 79, Side::Right), "clamped to the grid");
        assert_eq!(layout.cell_at(point(px(0.), px(0.))), (0, 0, Side::Left));
    }
}
