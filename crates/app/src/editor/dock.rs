use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};

use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, DockTab, tokens};

const MAX_SCROLLBACK_LINES: usize = 2_000;

/// A lone `cd` (optionally with a quoted path); anything chained with a shell
/// operator has to be run by the shell, not by the built-in.
fn is_plain_cd(cmd: &str) -> bool {
    (cmd == "cd" || cmd.starts_with("cd "))
        && !cmd.contains("&&")
        && !cmd.contains("||")
        && !cmd.contains(';')
        && !cmd.contains('|')
}

/// Arguments for `kill` that signal a command's whole process group (the
/// shell and everything it started), not just the shell.
pub(crate) fn kill_args(signal: &str, pid: u32) -> Vec<String> {
    vec![signal.to_string(), "--".to_string(), format!("-{pid}")]
}

/// Send `signal` (`-INT`, `-TERM`, `-KILL`) to the process group led by `pid`.
/// Commands are started as group leaders, so this reaches child processes too.
pub(crate) fn signal_process_group(pid: u32, signal: &str) {
    let _ = Command::new("kill").args(kill_args(signal, pid)).status();
}

/// What ⌃C sends: a polite interrupt first, then a hard kill if the command
/// ignored it (so a stuck process can always be stopped).
pub(crate) fn interrupt_signal(already_interrupted: bool) -> &'static str {
    if already_interrupted { "-KILL" } else { "-INT" }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AnsiColor {
    Default,
    Muted,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AnsiSpan {
    pub(crate) text: String,
    pub(crate) color: AnsiColor,
    pub(crate) bold: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum TerminalLine {
    Command {
        user_host: String,
        dir: String,
        command: String,
    },
    Output {
        spans: Vec<AnsiSpan>,
        file_link: Option<(PathBuf, u32, u32)>,
    },
}

pub(crate) enum TerminalEvent {
    Started { session_id: usize, pid: u32 },
    Line { session_id: usize, raw: String },
    Finished { session_id: usize },
}

pub(crate) struct TerminalSession {
    pub(crate) id: usize,
    pub(crate) shell_name: String,
    pub(crate) user_host: String,
    pub(crate) cwd: PathBuf,
    pub(crate) env_overrides: HashMap<String, String>,
    pub(crate) lines: Vec<TerminalLine>,
    pub(crate) input: String,
    pub(crate) cursor: usize,
    pub(crate) history: Vec<String>,
    pub(crate) history_index: Option<usize>,
    pub(crate) running_pid: Option<u32>,
    pub(crate) running_cmd: Option<String>,
    /// ⌃C was pressed and the command has been sent an interrupt.
    pub(crate) interrupt_sent: bool,
    /// ⌃C was pressed before the process had started; interrupt it as soon as it does.
    pub(crate) interrupt_pending: bool,
    pub(crate) scroll_handle: ScrollHandle,
}

impl TerminalSession {
    pub(crate) fn new(id: usize, cwd: PathBuf, seed_git_log: bool) -> Self {
        let shell_path = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let shell_name = Path::new(&shell_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("bash")
            .to_string();
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "dev".to_string());
        let host = Command::new("hostname")
            .arg("-s")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "mac".to_string());
        let user_host = format!("{user}@{host}");

        let mut session = Self {
            id,
            shell_name,
            user_host,
            cwd,
            env_overrides: HashMap::new(),
            lines: Vec::new(),
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_index: None,
            running_pid: None,
            running_cmd: None,
            interrupt_sent: false,
            interrupt_pending: false,
            scroll_handle: ScrollHandle::new(),
        };

        if seed_git_log {
            session.seed_initial_git_log();
        }
        session
    }

    /// A command is running: the prompt is held until it finishes.
    pub(crate) fn is_busy(&self) -> bool {
        self.running_cmd.is_some()
    }

    pub(crate) fn dir_label(&self) -> String {
        self.cwd
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("/")
            .to_string()
    }

    pub(crate) fn tab_title(&self) -> String {
        format!("{} \u{2014} {}", self.dir_label(), self.shell_name)
    }

    fn seed_initial_git_log(&mut self) {
        let cmd = "git --no-pager log --oneline -6".to_string();
        self.lines.push(TerminalLine::Command {
            user_host: self.user_host.clone(),
            dir: self.dir_label(),
            command: cmd.clone(),
        });
        self.history.push(cmd);

        if let Ok(output) = Command::new("git")
            .current_dir(&self.cwd)
            .args([
                "--no-pager",
                "log",
                "--color=always",
                "--decorate=short",
                "--oneline",
                "-6",
            ])
            .output()
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for raw_line in stdout.lines() {
                    self.push_output_line(raw_line);
                }
            }
        }
    }

    pub(crate) fn reset(&mut self, root_cwd: PathBuf) {
        if let Some(pid) = self.running_pid.take() {
            signal_process_group(pid, "-TERM");
        }
        self.running_cmd = None;
        self.interrupt_sent = false;
        self.interrupt_pending = false;
        self.cwd = root_cwd;
        self.env_overrides.clear();
        self.lines.clear();
        self.input.clear();
        self.cursor = 0;
        self.history_index = None;
        self.seed_initial_git_log();
    }

    pub(crate) fn push_output_line(&mut self, raw: &str) {
        let spans = parse_ansi_line(raw);
        let plain: String = spans.iter().map(|s| s.text.as_str()).collect();
        let file_link = extract_file_link(&plain, &self.cwd);
        self.lines.push(TerminalLine::Output { spans, file_link });
        if self.lines.len() > MAX_SCROLLBACK_LINES {
            let drain = self.lines.len() - MAX_SCROLLBACK_LINES;
            self.lines.drain(0..drain);
        }
    }
}

/// Parse ANSI SGR escape sequences (`\x1b[...m`) into styled [`AnsiSpan`]s while
/// stripping non-SGR control sequences.
pub(crate) fn parse_ansi_line(raw: &str) -> Vec<AnsiSpan> {
    // A progress line rewrites itself with `\r`; the reader splits on `\n`,
    // so keep only the last rewrite instead of gluing every frame together.
    let latest = raw.rsplit('\r').next().unwrap_or("");
    let cleaned = latest.replace(['\r', '\u{0}'], "");
    let mut spans = Vec::new();
    let mut cur_color = AnsiColor::Default;
    let mut cur_bold = false;
    let mut buf = String::new();

    let bytes = cleaned.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                if !buf.is_empty() {
                    spans.push(AnsiSpan {
                        text: std::mem::take(&mut buf),
                        color: cur_color,
                        bold: cur_bold,
                    });
                }
                i += 2;
                let param_start = i;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b';' || bytes[i] == b'?') {
                    i += 1;
                }
                let final_byte = if i < bytes.len() {
                    let b = bytes[i];
                    i += 1;
                    b
                } else {
                    b'm'
                };
                if final_byte == b'm' {
                    let params = &cleaned[param_start..(i - 1)];
                    if params.is_empty() {
                        cur_color = AnsiColor::Default;
                        cur_bold = false;
                    } else {
                        for code_str in params.split(';') {
                            match code_str.parse::<u16>().unwrap_or(0) {
                                0 => {
                                    cur_color = AnsiColor::Default;
                                    cur_bold = false;
                                }
                                1 => cur_bold = true,
                                2 => cur_color = AnsiColor::Muted,
                                22 => cur_bold = false,
                                30 | 90 => cur_color = AnsiColor::Muted,
                                31 | 91 => cur_color = AnsiColor::Red,
                                32 | 92 => cur_color = AnsiColor::Green,
                                33 | 93 => cur_color = AnsiColor::Yellow,
                                34 | 94 => cur_color = AnsiColor::Blue,
                                35 | 95 => cur_color = AnsiColor::Magenta,
                                36 | 96 => cur_color = AnsiColor::Cyan,
                                37 | 97 | 39 => cur_color = AnsiColor::Default,
                                _ => {}
                            }
                        }
                    }
                }
                continue;
            } else if i + 1 < bytes.len() && bytes[i + 1] == b']' {
                // Skip OSC sequence until BEL (\x07) or ST (\x1b\\)
                i += 2;
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
        }
        let ch = cleaned[i..].chars().next().unwrap_or(' ');
        buf.push(ch);
        i += ch.len_utf8();
    }

    if !buf.is_empty() || spans.is_empty() {
        spans.push(AnsiSpan {
            text: buf,
            color: cur_color,
            bold: cur_bold,
        });
    }
    spans
}

/// Detect `path/to/file.rs:line[:col]` tokens inside a terminal output line so
/// compiler diagnostics (`--> crates/app/src/main.rs:42:5`) are one-click links.
pub(crate) fn extract_file_link(plain: &str, cwd: &Path) -> Option<(PathBuf, u32, u32)> {
    for raw_token in plain.split_whitespace() {
        let token = raw_token.trim_matches(|c: char| {
            matches!(c, '(' | ')' | '[' | ']' | ',' | '\'' | '"' | ':')
        });
        let mut parts = token.split(':');
        let path_part = parts.next()?;
        let line_part = parts.next();
        let Some(line_str) = line_part else {
            continue;
        };
        let Ok(line_1) = line_str.parse::<u32>() else {
            continue;
        };
        if line_1 == 0 {
            continue;
        }
        let col_1 = parts
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(1)
            .max(1);
        let candidate = if Path::new(path_part).is_absolute() {
            PathBuf::from(path_part)
        } else {
            cwd.join(path_part)
        };
        if candidate.is_file() {
            return Some((candidate, line_1 - 1, col_1 - 1));
        }
    }
    None
}

fn prev_char_boundary(s: &str, byte_idx: usize) -> usize {
    let mut idx = byte_idx.min(s.len());
    while idx > 0 {
        idx -= 1;
        if s.is_char_boundary(idx) {
            return idx;
        }
    }
    0
}

fn next_char_boundary(s: &str, byte_idx: usize) -> usize {
    let mut idx = byte_idx.min(s.len());
    if idx >= s.len() {
        return s.len();
    }
    idx += 1;
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

impl DartIde {
    pub(crate) fn active_terminal(&self) -> Option<&TerminalSession> {
        self.terminals.get(self.active_terminal)
    }

    pub(crate) fn active_terminal_mut(&mut self) -> Option<&mut TerminalSession> {
        self.terminals.get_mut(self.active_terminal)
    }

    pub(crate) fn new_terminal_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self
            .active_terminal()
            .map(|t| t.cwd.clone())
            .unwrap_or_else(|| self.root.clone());
        let id = self.next_terminal_id;
        self.next_terminal_id += 1;
        self.terminals.push(TerminalSession::new(id, cwd, false));
        self.active_terminal = self.terminals.len() - 1;
        self.terminal_open = true;
        window.focus(&self.terminal_focus, cx);
        cx.notify();
    }

    pub(crate) fn close_terminal_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.terminals.len() {
            return;
        }
        if let Some(pid) = self.terminals[ix].running_pid {
            signal_process_group(pid, "-TERM");
        }
        self.terminals.remove(ix);
        if self.terminals.is_empty() {
            let id = self.next_terminal_id;
            self.next_terminal_id += 1;
            self.terminals
                .push(TerminalSession::new(id, self.root.clone(), false));
            self.active_terminal = 0;
            self.terminal_open = false;
        } else if self.active_terminal > ix {
            self.active_terminal -= 1;
        } else {
            self.active_terminal = self.active_terminal.min(self.terminals.len() - 1);
        }
        cx.notify();
    }

    pub(crate) fn handle_terminal_event(&mut self, event: TerminalEvent, cx: &mut Context<Self>) {
        match event {
            TerminalEvent::Started { session_id, pid } => {
                if let Some(session) = self.terminals.iter_mut().find(|t| t.id == session_id) {
                    session.running_pid = Some(pid);
                    // ⌃C arrived before the process existed: deliver it now.
                    if std::mem::take(&mut session.interrupt_pending) {
                        signal_process_group(pid, interrupt_signal(false));
                        session.interrupt_sent = true;
                    }
                    cx.notify();
                }
            }
            TerminalEvent::Line { session_id, raw } => {
                if let Some(session) = self.terminals.iter_mut().find(|t| t.id == session_id) {
                    session.push_output_line(&raw);
                    session.scroll_handle.scroll_to_item(session.lines.len());
                    cx.notify();
                }
            }
            TerminalEvent::Finished { session_id } => {
                if let Some(session) = self.terminals.iter_mut().find(|t| t.id == session_id) {
                    session.running_pid = None;
                    session.running_cmd = None;
                    session.interrupt_sent = false;
                    session.interrupt_pending = false;
                    session.scroll_handle.scroll_to_item(session.lines.len());
                }
                self.refresh_git(cx);
                cx.notify();
            }
        }
    }

    pub(crate) fn execute_terminal_command(
        &mut self,
        raw_cmd: String,
        cx: &mut Context<Self>,
    ) {
        let tx = self.terminal_tx.clone();
        // Something else (a menu command, a test run) wants the terminal while a command
        // is still running here: give it its own tab instead of mixing the output.
        if self.active_terminal().is_some_and(|s| s.is_busy()) {
            let id = self.next_terminal_id;
            self.next_terminal_id += 1;
            let cwd = self
                .active_terminal()
                .map(|s| s.cwd.clone())
                .unwrap_or_else(|| self.root.clone());
            self.terminals.push(TerminalSession::new(id, cwd, false));
            self.active_terminal = self.terminals.len() - 1;
        }
        let Some(session) = self.active_terminal_mut() else {
            return;
        };
        let cmd = raw_cmd.trim().to_string();
        let user_host = session.user_host.clone();
        let dir_label = session.dir_label();
        session.lines.push(TerminalLine::Command {
            user_host,
            dir: dir_label,
            command: raw_cmd.clone(),
        });

        if !cmd.is_empty() {
            if session.history.last() != Some(&cmd) {
                session.history.push(cmd.clone());
            }
        }
        session.history_index = None;

        if cmd.is_empty() {
            session.scroll_handle.scroll_to_item(session.lines.len());
            cx.notify();
            return;
        }

        // Built-in: `clear` / `cls`
        if matches!(cmd.as_str(), "clear" | "cls") {
            session.lines.clear();
            cx.notify();
            return;
        }

        // Built-in: `reset` (kills any hung state, restores root cwd & env)
        if cmd == "reset" {
            let root = self.root.clone();
            if let Some(session) = self.active_terminal_mut() {
                session.reset(root);
            }
            cx.notify();
            return;
        }

        // Built-in: `exit`
        if cmd == "exit" {
            let ix = self.active_terminal;
            self.close_terminal_tab(ix, cx);
            return;
        }

        // Built-in: a plain `cd [dir]`. Compound commands (`cd x && …`) go to
        // the shell; otherwise the whole tail became the path and failed with
        // "no such file or directory" (that is what broke Update goldens).
        if is_plain_cd(&cmd) {
            let arg = cmd
                .strip_prefix("cd")
                .unwrap_or("")
                .trim()
                .trim_matches(['"', '\'']);
            let target = if arg.is_empty() || arg == "~" {
                std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| session.cwd.clone())
            } else if let Some(rest) = arg.strip_prefix("~/") {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(rest))
                    .unwrap_or_else(|_| session.cwd.join(arg))
            } else {
                session.cwd.join(arg)
            };

            match std::fs::canonicalize(&target) {
                Ok(canon) if canon.is_dir() => {
                    session.cwd = canon;
                }
                _ => {
                    session.push_output_line(&format!(
                        "\x1b[31mcd: no such file or directory: {arg}\x1b[0m"
                    ));
                }
            }
            session.scroll_handle.scroll_to_item(session.lines.len());
            cx.notify();
            return;
        }

        // Built-in: `export KEY=VALUE`
        if let Some(rest) = cmd.strip_prefix("export ") {
            if let Some((k, v)) = rest.trim().split_once('=') {
                let key = k.trim().to_string();
                let val = v.trim().trim_matches(['"', '\'']).to_string();
                if !key.is_empty() {
                    session.env_overrides.insert(key, val);
                    session.scroll_handle.scroll_to_item(session.lines.len());
                    cx.notify();
                    return;
                }
            }
        }

        let session_id = session.id;
        let cwd = session.cwd.clone();
        let envs = session.env_overrides.clone();
        session.running_cmd = Some(cmd.clone());
        session.interrupt_sent = false;
        session.interrupt_pending = false;
        session.scroll_handle.scroll_to_item(session.lines.len());
        cx.notify();

        std::thread::spawn(move || {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
            let mut command = Command::new(shell);
            command
                .arg("-c")
                .arg(&cmd)
                .current_dir(&cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .env("CLICOLOR_FORCE", "1")
                .env("FORCE_COLOR", "1")
                .env("CARGO_TERM_COLOR", "always")
                .env("GIT_PAGER", "cat")
                .env("PAGER", "cat")
                .env("TERM", "xterm-256color")
                .envs(&envs)
                // Its own process group, so ⌃C can stop the shell *and* what it started.
                .process_group(0);

            let mut child = match command.spawn() {
                Ok(c) => c,
                Err(err) => {
                    let _ = tx.send_blocking(TerminalEvent::Line {
                        session_id,
                        raw: format!("\x1b[31m{err}\x1b[0m"),
                    });
                    let _ = tx.send_blocking(TerminalEvent::Finished { session_id });
                    return;
                }
            };

            let _ = tx.send_blocking(TerminalEvent::Started {
                session_id,
                pid: child.id(),
            });

            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            let tx_out = tx.clone();
            let out_handle = stdout.map(|out| {
                std::thread::spawn(move || {
                    let reader = BufReader::new(out);
                    for line in reader.lines().map_while(Result::ok) {
                        let _ = tx_out.send_blocking(TerminalEvent::Line {
                            session_id,
                            raw: line,
                        });
                    }
                })
            });

            let tx_err = tx.clone();
            let err_handle = stderr.map(|err| {
                std::thread::spawn(move || {
                    let reader = BufReader::new(err);
                    for line in reader.lines().map_while(Result::ok) {
                        let _ = tx_err.send_blocking(TerminalEvent::Line {
                            session_id,
                            raw: line,
                        });
                    }
                })
            });

            if let Some(h) = out_handle {
                let _ = h.join();
            }
            if let Some(h) = err_handle {
                let _ = h.join();
            }
            let _ = child.wait();
            let _ = tx.send_blocking(TerminalEvent::Finished { session_id });
        });
    }

    fn on_terminal_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keystroke = &event.keystroke;
        let key = keystroke.key.as_str();
        let modifiers = &keystroke.modifiers;
        let primary_mod = modifiers.platform || modifiers.control;

        // The Debug Console & Problems tabs are read-only: don't swallow keystrokes into the hidden shell prompt.
        if self.dock_tab != DockTab::Terminal {
            if modifiers.platform && key == "k" && self.dock_tab == DockTab::DebugConsole {
                self.debug_lines.clear();
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }

        // Let global IDE shortcuts (`⌘P`, `⌘B`, `⌘J`, `⌘O`, `⌘N`, `⌘F`) bubble up
        if modifiers.platform && matches!(key, "p" | "b" | "j" | "o" | "n" | "f" | "l" | "g") {
            return;
        }

        cx.stop_propagation();

        // `⌘K` or `Ctrl+L` clears terminal scrollback
        if (modifiers.platform && key == "k") || (modifiers.control && key == "l") {
            if let Some(session) = self.active_terminal_mut() {
                session.lines.clear();
                cx.notify();
            }
            return;
        }

        // `⌘T` opens a new terminal tab when focused in the terminal
        if modifiers.platform && key == "t" {
            self.new_terminal_tab(window, cx);
            return;
        }

        // `Ctrl+C` interrupts the running command (a second press kills it outright), or
        // cancels the line being typed.
        if modifiers.control && key == "c" {
            if let Some(session) = self.active_terminal_mut() {
                if session.is_busy() {
                    match session.running_pid {
                        Some(pid) => {
                            signal_process_group(pid, interrupt_signal(session.interrupt_sent));
                            session.interrupt_sent = true;
                            session.push_output_line("^C");
                        }
                        None => session.interrupt_pending = true,
                    }
                } else {
                    let user_host = session.user_host.clone();
                    let dir = session.dir_label();
                    let aborted = format!("{}^C", session.input);
                    session.lines.push(TerminalLine::Command {
                        user_host,
                        dir,
                        command: aborted,
                    });
                    session.input.clear();
                    session.cursor = 0;
                    session.history_index = None;
                }
                session.scroll_handle.scroll_to_item(session.lines.len());
                cx.notify();
            }
            return;
        }

        // The prompt is held while a command runs: nothing below (typing, paste,
        // Enter, history) applies until it finishes or is interrupted.
        if self.active_terminal().is_some_and(|s| s.is_busy()) {
            return;
        }

        // `⌘V` pastes from clipboard
        if primary_mod && key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                let clean = text.replace(['\r', '\n'], " ");
                if let Some(session) = self.active_terminal_mut() {
                    let idx = session.cursor.min(session.input.len());
                    session.input.insert_str(idx, &clean);
                    session.cursor = idx + clean.len();
                    cx.notify();
                }
            }
            return;
        }

        // `Ctrl+U` clears current input line
        if modifiers.control && key == "u" {
            if let Some(session) = self.active_terminal_mut() {
                session.input.clear();
                session.cursor = 0;
                cx.notify();
            }
            return;
        }

        match key {
            "enter" => {
                let raw_cmd = if let Some(session) = self.active_terminal_mut() {
                    let taken = std::mem::take(&mut session.input);
                    session.cursor = 0;
                    taken
                } else {
                    return;
                };
                self.execute_terminal_command(raw_cmd, cx);
            }
            "backspace" => {
                if let Some(session) = self.active_terminal_mut() {
                    if session.cursor > 0 {
                        if modifiers.alt {
                            let bytes = session.input.as_bytes();
                            let mut start = session.cursor;
                            while start > 0 && bytes[start - 1].is_ascii_whitespace() {
                                start -= 1;
                            }
                            while start > 0 && !bytes[start - 1].is_ascii_whitespace() {
                                start -= 1;
                            }
                            session.input.replace_range(start..session.cursor, "");
                            session.cursor = start;
                        } else {
                            let prev = prev_char_boundary(&session.input, session.cursor);
                            session.input.replace_range(prev..session.cursor, "");
                            session.cursor = prev;
                        }
                        cx.notify();
                    }
                }
            }
            "delete" => {
                if let Some(session) = self.active_terminal_mut() {
                    if session.cursor < session.input.len() {
                        let next = next_char_boundary(&session.input, session.cursor);
                        session.input.replace_range(session.cursor..next, "");
                        cx.notify();
                    }
                }
            }
            "left" => {
                if let Some(session) = self.active_terminal_mut() {
                    session.cursor = prev_char_boundary(&session.input, session.cursor);
                    cx.notify();
                }
            }
            "right" => {
                if let Some(session) = self.active_terminal_mut() {
                    session.cursor = next_char_boundary(&session.input, session.cursor);
                    cx.notify();
                }
            }
            "home" => {
                if let Some(session) = self.active_terminal_mut() {
                    session.cursor = 0;
                    cx.notify();
                }
            }
            "end" => {
                if let Some(session) = self.active_terminal_mut() {
                    session.cursor = session.input.len();
                    cx.notify();
                }
            }
            "up" => {
                if let Some(session) = self.active_terminal_mut() {
                    if !session.history.is_empty() {
                        let next_ix = match session.history_index {
                            Some(ix) => ix.saturating_sub(1),
                            None => session.history.len() - 1,
                        };
                        session.history_index = Some(next_ix);
                        session.input = session.history[next_ix].clone();
                        session.cursor = session.input.len();
                        cx.notify();
                    }
                }
            }
            "down" => {
                if let Some(session) = self.active_terminal_mut() {
                    if let Some(ix) = session.history_index {
                        if ix + 1 < session.history.len() {
                            session.history_index = Some(ix + 1);
                            session.input = session.history[ix + 1].clone();
                            session.cursor = session.input.len();
                        } else {
                            session.history_index = None;
                            session.input.clear();
                            session.cursor = 0;
                        }
                        cx.notify();
                    }
                }
            }
            "tab" => {
                if let Some(session) = self.active_terminal_mut() {
                    let prefix = &session.input[..session.cursor];
                    if let Some(last_word) = prefix.split_whitespace().last() {
                        let (dir_part, file_prefix) = match last_word.rfind('/') {
                            Some(slash) => (&last_word[..=slash], &last_word[(slash + 1)..]),
                            None => ("", last_word),
                        };
                        let search_dir = if dir_part.is_empty() {
                            session.cwd.clone()
                        } else {
                            session.cwd.join(dir_part)
                        };
                        if let Ok(entries) = std::fs::read_dir(search_dir) {
                            let mut matches: Vec<String> = entries
                                .flatten()
                                .filter_map(|e| {
                                    let name = e.file_name().to_str()?.to_string();
                                    if name.starts_with(file_prefix) {
                                        let suffix = if e.path().is_dir() { "/" } else { "" };
                                        Some(format!("{dir_part}{name}{suffix}"))
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            matches.sort();
                            if let Some(first) = matches.first() {
                                let word_start = session.cursor - last_word.len();
                                session
                                    .input
                                    .replace_range(word_start..session.cursor, first);
                                session.cursor = word_start + first.len();
                                cx.notify();
                            }
                        }
                    }
                }
            }
            "space" => {
                if let Some(session) = self.active_terminal_mut() {
                    let idx = session.cursor.min(session.input.len());
                    session.input.insert(idx, ' ');
                    session.cursor = idx + 1;
                    cx.notify();
                }
            }
            _ => {
                if modifiers.control && key == "a" {
                    if let Some(session) = self.active_terminal_mut() {
                        session.cursor = 0;
                        cx.notify();
                    }
                    return;
                }
                if modifiers.control && key == "e" {
                    if let Some(session) = self.active_terminal_mut() {
                        session.cursor = session.input.len();
                        cx.notify();
                    }
                    return;
                }
                if !modifiers.platform && !modifiers.control {
                    let text_to_insert = keystroke
                        .key_char
                        .as_deref()
                        .or_else(|| (key.chars().count() == 1).then_some(key));
                    if let Some(ch_str) = text_to_insert {
                        if let Some(session) = self.active_terminal_mut() {
                            let idx = session.cursor.min(session.input.len());
                            session.input.insert_str(idx, ch_str);
                            session.cursor = idx + ch_str.len();
                            session.scroll_handle.scroll_to_item(session.lines.len());
                            cx.notify();
                        }
                    }
                }
            }
        }
    }

    /// Bottom dock of the center column (`docs/design-spec.md` §3.20–3.22).
    pub(crate) fn render_terminal(
        &self,
        view: &Entity<Self>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let green = cx.theme().success;
        let blue = cx.theme().info;
        let yellow = cx.theme().warning;
        let red = cx.theme().danger;
        let magenta = cx.theme().magenta;
        let cyan = cx.theme().cyan;
        let primary = cx.theme().primary;
        let is_focused = self.terminal_focus.is_focused(window);

        let active_ix = self.active_terminal.min(self.terminals.len().saturating_sub(1));

        // Render tab pills in the dock header
        let tabs: Vec<AnyElement> = self
            .terminals
            .iter()
            .enumerate()
            .map(|(ix, session)| {
                let is_active = ix == active_ix && self.dock_tab == DockTab::Terminal;
                let title = session.tab_title();
                let is_running = session.running_pid.is_some();
                let view_select = view.clone();
                let view_close = view.clone();

                h_flex()
                    .id(("terminal-tab", ix))
                    .items_center()
                    .gap(px(tokens::SPACE_2))
                    .px(px(tokens::SPACE_3))
                    .h(px(28.))
                    .rounded(px(tokens::RADIUS_PANEL))
                    .border_1()
                    .border_color(if is_active {
                        tokens::accent_dim()
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .when(is_active, |el| {
                        el.bg(cx.theme().tab_active).text_color(fg)
                    })
                    .when(!is_active, |el| {
                        el.text_color(muted)
                            .hover(move |s| s.bg(rgba(0xffffff08)).text_color(fg))
                    })
                    .text_size(px(tokens::UI_SM))
                    .cursor_pointer()
                    .on_click(move |_event, window, cx| {
                        view_select.update(cx, |this, cx| {
                            this.active_terminal = ix;
                            this.dock_tab = DockTab::Terminal;
                            window.focus(&this.terminal_focus, cx);
                            cx.notify();
                        });
                    })
                    .child(
                        div()
                            .font_family("SF Mono")
                            .text_size(px(tokens::UI_SM))
                            .text_color(if is_running { green } else { primary })
                            .child(">_"),
                    )
                    .child(title)
                    .child(
                        div()
                            .id(("close-terminal-tab", ix))
                            .cursor_pointer()
                            .text_color(muted)
                            .hover(move |s| s.text_color(fg))
                            .child(Icon::new(gpui_kit::assets::IconName::X).xsmall())
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(move |_event, _window, cx| {
                                view_close.update(cx, |this, cx| {
                                    this.close_terminal_tab(ix, cx);
                                });
                            }),
                    )
                    .into_any_element()
            })
            .collect();

        let debug_active = self.dock_tab == DockTab::DebugConsole;
        let problems_active = self.dock_tab == DockTab::Problems;
        let tests_active = self.dock_tab == DockTab::Tests;
        let debug_count_err = self
            .debug_lines
            .iter()
            .filter(|l| matches!(l, TerminalLine::Output { spans, .. } if spans.iter().any(|s| s.color == AnsiColor::Red)))
            .count();

        let mut all_problems = Vec::new();
        for tab in &self.tabs {
            let Some(path) = tab.path.clone() else {
                continue;
            };
            for diag in tab.editor.read(cx).diagnostics() {
                all_problems.push((path.clone(), diag.clone()));
            }
        }
        let total_problems = all_problems.len();

        let problems_pill = {
            let view_select = view.clone();
            h_flex()
                .id("problems-dock-tab")
                .items_center()
                .gap(px(tokens::SPACE_2))
                .px(px(tokens::SPACE_3))
                .h(px(28.))
                .rounded(px(tokens::RADIUS_PANEL))
                .border_1()
                .border_color(if problems_active {
                    cx.theme().border
                } else {
                    gpui_kit::transparent_black()
                })
                .when(problems_active, |el| el.bg(cx.theme().tab_active).text_color(fg))
                .when(!problems_active, |el| {
                    el.text_color(muted)
                        .hover(move |s| s.bg(rgba(0xffffff08)).text_color(fg))
                })
                .text_size(px(tokens::UI_SM))
                .cursor_pointer()
                .on_click(move |_event, _window, cx| {
                    view_select.update(cx, |this, cx| {
                        this.dock_tab = DockTab::Problems;
                        cx.notify();
                    });
                })
                .child(
                    Icon::new(gpui_kit::assets::IconName::TriangleAlert)
                        .xsmall()
                        .text_color(if total_problems > 0 { yellow } else { muted }),
                )
                .child("Problems")
                .when(total_problems > 0, |el| {
                    el.child(
                        div()
                            .px(px(5.))
                            .rounded(px(8.))
                            .bg(yellow.opacity(0.18))
                            .text_size(px(9.5))
                            .text_color(yellow)
                            .child(total_problems.to_string()),
                    )
                })
        };

        let debug_pill = {
            let view_select = view.clone();
            h_flex()
                .id("debug-console-tab")
                .items_center()
                .gap(px(tokens::SPACE_2))
                .px(px(tokens::SPACE_3))
                .h(px(28.))
                .rounded(px(tokens::RADIUS_PANEL))
                .border_1()
                .border_color(if debug_active {
                    tokens::accent_dim()
                } else {
                    gpui_kit::transparent_black()
                })
                .when(debug_active, |el| el.bg(cx.theme().tab_active).text_color(fg))
                .when(!debug_active, |el| {
                    el.text_color(muted)
                        .hover(move |s| s.bg(rgba(0xffffff08)).text_color(fg))
                })
                .text_size(px(tokens::UI_SM))
                .cursor_pointer()
                .on_click(move |_event, _window, cx| {
                    view_select.update(cx, |this, cx| {
                        this.dock_tab = DockTab::DebugConsole;
                        cx.notify();
                    });
                })
                .child(
                    Icon::new(gpui_kit::assets::IconName::Bug)
                        .xsmall()
                        .text_color(if self.flutter_daemon.is_some() { green } else { primary }),
                )
                .child("Debug Console")
                .when(debug_count_err > 0, |el| {
                    el.child(
                        div()
                            .px(px(5.))
                            .rounded(px(8.))
                            .bg(red.opacity(0.18))
                            .text_size(px(9.5))
                            .text_color(red)
                            .child(debug_count_err.to_string()),
                    )
                })
        };

        let tests_pill = {
            let view_select = view.clone();
            let badge = self.tests_tab_badge();
            h_flex()
                .id("tests-dock-tab")
                .items_center()
                .gap(px(tokens::SPACE_2))
                .px(px(tokens::SPACE_3))
                .h(px(28.))
                .rounded(px(tokens::RADIUS_PANEL))
                .border_1()
                .border_color(if tests_active { cx.theme().border } else { gpui_kit::transparent_black() })
                .when(tests_active, |el| el.bg(cx.theme().tab_active).text_color(fg))
                .when(!tests_active, |el| {
                    el.text_color(muted).hover(move |s| s.bg(rgba(0xffffff08)).text_color(fg))
                })
                .text_size(px(tokens::UI_SM))
                .cursor_pointer()
                .on_click(move |_event, _window, cx| {
                    view_select.update(cx, |this, cx| {
                        this.dock_tab = DockTab::Tests;
                        cx.notify();
                    });
                })
                .child(Icon::new(gpui_kit::assets::IconName::Check).xsmall().text_color(muted))
                .child("Tests")
                .when_some(badge, |el, (count, failing)| {
                    let tone = if failing { red } else { green };
                    el.child(
                        div()
                            .px(px(5.))
                            .rounded(px(8.))
                            .bg(tone.opacity(0.18))
                            .text_size(px(9.5))
                            .text_color(tone)
                            .child(count.to_string()),
                    )
                })
        };

        const MAX_RENDERED_DOCK_LINES: usize = 350;

        let body_element = if problems_active {
            let rows: Vec<AnyElement> = all_problems
                .into_iter()
                .enumerate()
                .map(|(ix, (path, diag))| {
                    let is_err = diag.severity
                        == Some(core_lsp::lsp_types::DiagnosticSeverity::ERROR);
                    let sev_color = if is_err { red } else { yellow };
                    let sev_icon = if is_err {
                        gpui_kit::assets::IconName::CircleX
                    } else {
                        gpui_kit::assets::IconName::TriangleAlert
                    };
                    let file_name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();
                    let line_0 = diag.range.start.line;
                    let col_0 = diag.range.start.character;
                    let msg = diag
                        .message
                        .lines()
                        .next()
                        .unwrap_or(&diag.message)
                        .trim()
                        .to_string();
                    let view = view.clone();

                    h_flex()
                        .id(("problem-row", ix))
                        .w_full()
                        .h(px(26.))
                        .px(px(tokens::SPACE_4))
                        .items_center()
                        .justify_between()
                        .gap(px(tokens::SPACE_3))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgba(0xffffff08)))
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w(px(0.))
                                .items_center()
                                .gap(px(tokens::SPACE_2))
                                .child(Icon::new(sev_icon).xsmall().text_color(sev_color))
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(tokens::UI_SM))
                                        .text_color(fg)
                                        .child(msg),
                                ),
                        )
                        .child(
                            h_flex()
                                .flex_shrink_0()
                                .items_center()
                                .gap(px(tokens::SPACE_2))
                                .font_family("SF Mono")
                                .text_size(px(tokens::UI_XS))
                                .child(div().text_color(muted).child(file_name))
                                .child(
                                    div()
                                        .text_color(primary)
                                        .child(format!("{}:{}", line_0 + 1, col_0 + 1)),
                                ),
                        )
                        .on_click(move |_e, window, cx| {
                            view.update(cx, |this, cx| {
                                this.open_path(path.clone(), window, cx);
                                if let Some(ed) = this.active_editor().cloned() {
                                    ed.update(cx, |editor, cx| {
                                        editor.set_cursor_to_position(line_0, col_0, cx);
                                    });
                                }
                            });
                        })
                        .into_any_element()
                })
                .collect();

            v_flex()
                .id("problems-scrollback")
                .flex_1()
                .w_full()
                .min_h(px(0.))
                .py(px(tokens::SPACE_2))
                .bg(cx.theme().background)
                .overflow_y_scroll()
                .when(rows.is_empty(), |el| {
                    el.child(
                        div()
                            .px(px(tokens::SPACE_5))
                            .py(px(tokens::SPACE_3))
                            .text_size(px(tokens::UI_SM))
                            .text_color(muted)
                            .child("No problems detected in open files."),
                    )
                })
                .children(rows)
                .into_any_element()
        } else if tests_active {
            self.render_tests_panel(cx).into_any_element()
        } else if debug_active {
            let running = self.flutter_daemon.is_some() || self.dap_client.is_some();
            let dap_paused = self.dap_paused_thread.is_some();
            let status_text = self
                .flutter_status
                .clone()
                .or_else(|| {
                    self.dap_client.as_ref().map(|_| {
                        if dap_paused {
                            self.dap_stack_summary
                                .clone()
                                .unwrap_or_else(|| "Paused at breakpoint".to_string())
                        } else {
                            "Debugging (DAP)".to_string()
                        }
                    })
                })
                .unwrap_or_else(|| "No debug session".to_string());
            let dot = if dap_paused {
                yellow
            } else if running {
                green
            } else {
                muted
            };

            let skip_dbg = self
                .debug_lines
                .len()
                .saturating_sub(MAX_RENDERED_DOCK_LINES);
            let rows: Vec<AnyElement> = self
                .debug_lines
                .iter()
                .enumerate()
                .skip(skip_dbg)
                .filter_map(|(line_ix, line)| {
                    let TerminalLine::Output { spans, file_link } = line else {
                        return None;
                    };
                    let tint = spans
                        .iter()
                        .find(|s| s.color != AnsiColor::Default)
                        .map(|s| s.color);
                    let (bar, wash) = match tint {
                        Some(AnsiColor::Red) => (Some(red), Some(red.opacity(0.07))),
                        Some(AnsiColor::Yellow) => (Some(yellow), Some(yellow.opacity(0.05))),
                        _ => (None, None),
                    };
                    let children: Vec<AnyElement> = spans
                        .iter()
                        .map(|span| {
                            let color = match span.color {
                                AnsiColor::Default => fg,
                                AnsiColor::Muted => muted,
                                AnsiColor::Red => red,
                                AnsiColor::Green => green,
                                AnsiColor::Yellow => yellow,
                                AnsiColor::Blue => blue,
                                AnsiColor::Magenta => magenta,
                                AnsiColor::Cyan => cyan,
                            };
                            div()
                                .text_color(color)
                                .when(span.bold, |el| el.font_semibold())
                                .child(span.text.clone())
                                .into_any_element()
                        })
                        .collect();
                    let row = h_flex()
                        .id(("debug-line", line_ix))
                        .w_full()
                        .min_h(px(20.))
                        .items_start()
                        .gap(px(8.))
                        .px(px(tokens::SPACE_4))
                        .border_l_2()
                        .border_color(bar.unwrap_or_else(gpui_kit::transparent_black))
                        .when_some(wash, |el, w| el.bg(w))
                        .font_family("SF Mono")
                        .text_size(px(tokens::MONO))
                        .child(
                            div()
                                .w(px(28.))
                                .flex_shrink_0()
                                .text_color(muted.opacity(0.5))
                                .text_size(px(9.5))
                                .pt(px(3.))
                                .child(format!("{:>3}", line_ix + 1)),
                        )
                        .child(h_flex().flex_wrap().flex_1().min_w(px(0.)).children(children));
                    if let Some((path, l0, c0)) = file_link.clone() {
                        let view = view.clone();
                        Some(
                            row.cursor_pointer()
                                .hover(|s| s.bg(rgba(0xffffff08)))
                                .on_click(move |_e, window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.open_path(path.clone(), window, cx);
                                        if let Some(ed) = this.active_editor().cloned() {
                                            ed.update(cx, |editor, cx| {
                                                editor.set_cursor_to_position(l0, c0, cx);
                                            });
                                        }
                                    });
                                })
                                .into_any_element(),
                        )
                    } else {
                        Some(row.into_any_element())
                    }
                })
                .collect();

            v_flex()
                .flex_1()
                .w_full()
                .min_h(px(0.))
                .bg(cx.theme().background)
                .child(
                    h_flex()
                        .w_full()
                        .h(px(30.))
                        .flex_shrink_0()
                        .px(px(tokens::SPACE_4))
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            h_flex()
                                .items_center()
                                .gap(px(tokens::SPACE_2))
                                .child(div().size(px(7.)).rounded(px(4.)).bg(dot))
                                .child(
                                    div()
                                        .text_size(px(tokens::UI_SM))
                                        .text_color(fg)
                                        .child(status_text),
                                ),
                        )
                        .child(
                            h_flex()
                                .items_center()
                                .gap(px(2.))
                                .child(
                                    Button::new("dbg-start-dap")
                                        .ghost()
                                        .xsmall()
                                        .icon(
                                            Icon::new(gpui_kit::assets::IconName::Bug)
                                                .xsmall()
                                                .text_color(if self.dap_client.is_some() {
                                                    primary
                                                } else {
                                                    green
                                                }),
                                        )
                                        .tooltip("Start Dart DAP Debugger on active file")
                                        .on_click(cx.listener(|this, _e, window, cx| {
                                            this.start_dap_debug(window, cx);
                                        })),
                                )
                                .when(dap_paused, |row| {
                                    row.child(
                                        Button::new("dap-continue")
                                            .ghost()
                                            .xsmall()
                                            .icon(
                                                Icon::new(gpui_kit::assets::IconName::Play)
                                                    .xsmall()
                                                    .text_color(green),
                                            )
                                            .tooltip("Continue (F5)")
                                            .on_click(cx.listener(|this, _e, _w, cx| {
                                                this.dap_continue(cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("dap-step-over")
                                            .ghost()
                                            .xsmall()
                                            .icon(
                                                Icon::new(gpui_kit::assets::IconName::ArrowRight)
                                                    .xsmall()
                                                    .text_color(fg),
                                            )
                                            .tooltip("Step Over (F10)")
                                            .on_click(cx.listener(|this, _e, _w, cx| {
                                                this.dap_step_over(cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("dap-step-in")
                                            .ghost()
                                            .xsmall()
                                            .icon(
                                                Icon::new(gpui_kit::assets::IconName::ArrowDown)
                                                    .xsmall()
                                                    .text_color(fg),
                                            )
                                            .tooltip("Step Into (F11)")
                                            .on_click(cx.listener(|this, _e, _w, cx| {
                                                this.dap_step_in(cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("dap-step-out")
                                            .ghost()
                                            .xsmall()
                                            .icon(
                                                Icon::new(gpui_kit::assets::IconName::ArrowUp)
                                                    .xsmall()
                                                    .text_color(fg),
                                            )
                                            .tooltip("Step Out (Shift+F11)")
                                            .on_click(cx.listener(|this, _e, _w, cx| {
                                                this.dap_step_out(cx);
                                            })),
                                    )
                                })
                                .child(
                                    Button::new("dbg-reload")
                                        .ghost()
                                        .xsmall()
                                        .icon(
                                            Icon::new(gpui_kit::assets::IconName::Zap)
                                                .xsmall()
                                                .text_color(yellow),
                                        )
                                        .tooltip("Hot Reload")
                                        .on_click(cx.listener(|this, _e, _w, cx| {
                                            this.trigger_hot_reload(cx);
                                        })),
                                )
                                .child(
                                    Button::new("dbg-restart")
                                        .ghost()
                                        .xsmall()
                                        .icon(
                                            Icon::new(gpui_kit::assets::IconName::RotateCw)
                                                .xsmall()
                                                .text_color(blue),
                                        )
                                        .tooltip("Hot Restart")
                                        .on_click(cx.listener(|this, _e, _w, cx| {
                                            this.trigger_hot_restart(cx);
                                        })),
                                )
                                .child(
                                    Button::new("dbg-stop")
                                        .ghost()
                                        .xsmall()
                                        .icon(
                                            Icon::new(gpui_kit::assets::IconName::X)
                                                .xsmall()
                                                .text_color(if running { red } else { muted }),
                                        )
                                        .tooltip("Stop")
                                        .on_click(cx.listener(|this, _e, _w, cx| {
                                            this.stop_flutter_run(cx);
                                            this.stop_dap_debug(cx);
                                        })),
                                ),
                        ),
                )
                .when(!self.dap_variables.is_empty(), |el| {
                    el.child(
                        h_flex()
                            .w_full()
                            .px(px(tokens::SPACE_4))
                            .py(px(tokens::SPACE_1))
                            .gap(px(tokens::SPACE_3))
                            .flex_wrap()
                            .items_center()
                            .bg(cx.theme().tab_bar)
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .font_family("SF Mono")
                            .text_size(px(tokens::UI_XS))
                            .child(div().text_color(muted).child("Locals:"))
                            .children(
                                crate::debug::flatten_variables(
                                    &self.dap_variables,
                                    &self.dap_children,
                                    &self.dap_expanded,
                                )
                                .into_iter()
                                .enumerate()
                                .map(|(ix, var)| {
                                    let expandable = var.var_ref > 0;
                                    let marker = if !expandable {
                                        "  "
                                    } else if self.dap_expanded.contains(&var.var_ref) {
                                        "\u{25be} "
                                    } else {
                                        "\u{25b8} "
                                    };
                                    let var_ref = var.var_ref;
                                    h_flex()
                                        .id(("dap-var", ix))
                                        .gap(px(3.))
                                        .ml(px(var.depth as f32 * 10.))
                                        .when(expandable, |el| {
                                            el.cursor_pointer().on_click(cx.listener(
                                                move |this, _e, _w, cx| {
                                                    this.toggle_dap_variable(var_ref, cx);
                                                },
                                            ))
                                        })
                                        .child(div().text_color(muted).child(marker))
                                        .child(div().text_color(cyan).child(format!("{} =", var.name)))
                                        .child(div().text_color(yellow).child(var.value.clone()))
                                }),
                            ),
                    )
                })
                .when(!self.dap_watches.is_empty(), |el| {
                    el.child(
                        h_flex()
                            .w_full()
                            .px(px(tokens::SPACE_4))
                            .py(px(tokens::SPACE_1))
                            .gap(px(tokens::SPACE_3))
                            .flex_wrap()
                            .items_center()
                            .bg(cx.theme().tab_bar)
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .font_family("SF Mono")
                            .text_size(px(tokens::UI_XS))
                            .child(div().text_color(muted).child("Watch:"))
                            .children(self.dap_watches.iter().map(|expr| {
                                let value = self
                                    .dap_watch_values
                                    .get(expr)
                                    .cloned()
                                    .unwrap_or_else(|| "\u{2026}".to_string());
                                h_flex()
                                    .gap(px(3.))
                                    .child(div().text_color(cyan).child(format!("{expr} =")))
                                    .child(div().text_color(yellow).child(value))
                            })),
                    )
                })
                .child(
                    v_flex()
                        .id("debug-scrollback")
                        .flex_1()
                        .w_full()
                        .min_h(px(0.))
                        .py(px(tokens::SPACE_2))
                        .overflow_y_scroll()
                        .track_scroll(&self.debug_scroll)
                        .when(rows.is_empty(), |el| {
                            el.child(
                                div()
                                    .px(px(tokens::SPACE_5))
                                    .py(px(tokens::SPACE_3))
                                    .text_size(px(tokens::UI_SM))
                                    .text_color(muted)
                                    .child("Run a launch configuration to see debug output here."),
                            )
                        })
                        .children(rows),
                )
                .into_any_element()
        } else if let Some(session) = self.terminals.get(active_ix) {
            let skip_term = session.lines.len().saturating_sub(MAX_RENDERED_DOCK_LINES);
            let line_elements: Vec<AnyElement> = session
                .lines
                .iter()
                .enumerate()
                .skip(skip_term)
                .map(|(line_ix, line)| match line {
                    TerminalLine::Command {
                        user_host,
                        dir,
                        command,
                    } => {
                        // One styled run instead of a row of child divs: a
                        // long command then wraps at word boundaries instead
                        // of running off the right edge.
                        let mut text = String::new();
                        let mut styles: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
                        for (part, color) in [
                            (user_host.as_str(), green),
                            (":", muted),
                            (dir.as_str(), blue),
                            ("$ ", fg),
                            (command.as_str(), fg),
                        ] {
                            if part.is_empty() {
                                continue;
                            }
                            let start = text.len();
                            text.push_str(part);
                            styles.push((
                                start..text.len(),
                                HighlightStyle {
                                    color: Some(color),
                                    ..Default::default()
                                },
                            ));
                        }
                        div()
                            .w_full()
                            .min_w(px(0.))
                            .min_h(px(20.))
                            .flex_shrink_0()
                            .py(px(2.))
                            .whitespace_normal()
                            .font_family("SF Mono")
                            .text_size(px(tokens::MONO))
                            .text_color(fg)
                            .child(StyledText::new(text).with_highlights(styles))
                            .into_any_element()
                    }
                    TerminalLine::Output { spans, file_link } => {
                        let mut text = String::new();
                        let mut styles: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
                        for span in spans {
                            if span.text.is_empty() {
                                continue;
                            }
                            let color = match span.color {
                                AnsiColor::Default => fg,
                                AnsiColor::Muted => muted,
                                AnsiColor::Red => red,
                                AnsiColor::Green => green,
                                AnsiColor::Yellow => yellow,
                                AnsiColor::Blue => blue,
                                AnsiColor::Magenta => magenta,
                                AnsiColor::Cyan => cyan,
                            };
                            let start = text.len();
                            text.push_str(&span.text);
                            styles.push((
                                start..text.len(),
                                HighlightStyle {
                                    color: Some(color),
                                    font_weight: span
                                        .bold
                                        .then_some(gpui_kit::FontWeight::SEMIBOLD),
                                    ..Default::default()
                                },
                            ));
                        }

                        let row = div()
                            .id(("term-line", line_ix))
                            .w_full()
                            .min_w(px(0.))
                            .min_h(px(20.))
                            .flex_shrink_0()
                            .py(px(2.))
                            .whitespace_normal()
                            .font_family("SF Mono")
                            .text_size(px(tokens::MONO))
                            .text_color(fg)
                            .child(StyledText::new(text).with_highlights(styles));

                        if let Some((path, line_0, col_0)) = file_link.clone() {
                            let view = view.clone();
                            row.cursor_pointer()
                                .rounded(px(3.))
                                .hover(|s| s.bg(rgba(0xffffff08)))
                                .on_click(move |_event, window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.open_path(path.clone(), window, cx);
                                        if let Some(ed) = this.active_editor().cloned() {
                                            ed.update(cx, |editor, cx| {
                                                editor.set_cursor_to_position(line_0, col_0, cx);
                                            });
                                        }
                                    });
                                })
                                .into_any_element()
                        } else {
                            row.into_any_element()
                        }
                    }
                })
                .collect();

            // Active prompt line at the bottom of the scrollback
            let cursor_idx = session.cursor.min(session.input.len());
            let before_cursor = session.input[..cursor_idx].to_string();
            let after_cursor = session.input[cursor_idx..].to_string();

            let prompt_row = h_flex()
                .w_full()
                .min_h(px(20.))
                .items_center()
                .font_family("SF Mono")
                .text_size(px(tokens::MONO))
                .child(div().text_color(green).child(session.user_host.clone()))
                .child(div().text_color(muted).child(":"))
                .child(div().text_color(blue).child(session.dir_label()))
                .child(div().text_color(fg).child("$ "))
                .when(!before_cursor.is_empty(), |el| {
                    el.child(div().text_color(fg).child(before_cursor))
                })
                .child(
                    div()
                        .w(px(7.5))
                        .h(px(15.))
                        .border_1()
                        .border_color(primary)
                        .when(is_focused, |el| el.bg(primary))
                        .when(!is_focused, |el| el.bg(rgba(0xea7a3c18))),
                )
                .when(!after_cursor.is_empty(), |el| {
                    el.child(div().text_color(fg).child(after_cursor))
                });

            // While a command runs there is no prompt: just a quiet note on how to stop it.
            let running_row = h_flex()
                .w_full()
                .min_h(px(20.))
                .items_center()
                .gap(px(tokens::SPACE_2))
                .font_family("SF Mono")
                .text_size(px(tokens::UI_XS))
                .text_color(muted)
                .child(div().size(px(6.)).rounded(px(3.)).bg(yellow))
                .child(if session.interrupt_sent {
                    "Stopping\u{2026} press \u{2303}C again to force quit"
                } else {
                    "Running\u{2026}  \u{2303}C to stop"
                });
            let busy = session.is_busy();

            v_flex()
                .id("terminal-scrollback")
                .flex_1()
                .w_full()
                .min_h(px(0.))
                .px(px(tokens::SPACE_5))
                .py(px(tokens::SPACE_4))
                .bg(cx.theme().background)
                .overflow_y_scroll()
                .track_scroll(&session.scroll_handle)
                .children(line_elements)
                .child(if busy { running_row.into_any_element() } else { prompt_row.into_any_element() })
                .into_any_element()
        } else {
            div().flex_1().into_any_element()
        };

        v_flex()
            .id("terminal-dock")
            .key_context("Terminal")
            .track_focus(&self.terminal_focus)
            .on_key_down(cx.listener(Self::on_terminal_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event, window, cx| {
                    window.focus(&this.terminal_focus, cx);
                    cx.notify();
                }),
            )
            .size_full()
            .bg(cx.theme().sidebar)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(tokens::RADIUS_CARD))
            .overflow_hidden()
            .child(
                h_flex()
                    .w_full()
                    .h(px(tokens::TABSTRIP_H))
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .px(px(tokens::SPACE_4))
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .children(tabs)
                            .child(problems_pill)
                            .when(
                                self.is_flutter_or_dart_workspace()
                                    || self.flutter_daemon.is_some()
                                    || self.dap_client.is_some()
                                    || !self.debug_lines.is_empty(),
                                |r| r.child(debug_pill),
                            )
                            .when(
                                self.is_flutter_or_dart_workspace() || self.test_run.is_some(),
                                |r| r.child(tests_pill),
                            )
                            .child(
                                Button::new("new-terminal-tab")
                                    .ghost()
                                    .small()
                                    .icon(Icon::new(IconName::Plus).text_color(muted))
                                    .tooltip("New terminal tab (cmd+T)")
                                    .on_click(cx.listener(|this, _event, window, cx| {
                                        this.new_terminal_tab(window, cx);
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(tokens::SPACE_1))
                            .child(
                                Button::new("reset-terminal")
                                    .ghost()
                                    .xsmall()
                                    .icon(
                                        Icon::new(gpui_kit::assets::IconName::RotateCcw)
                                            .xsmall()
                                            .text_color(muted),
                                    )
                                    .tooltip("Reset terminal session")
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        let root = this.root.clone();
                                        if let Some(session) = this.active_terminal_mut() {
                                            session.reset(root);
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(
                                Button::new("clear-terminal")
                                    .ghost()
                                    .xsmall()
                                    .icon(
                                        Icon::new(gpui_kit::assets::IconName::Trash)
                                            .xsmall()
                                            .text_color(muted),
                                    )
                                    .tooltip("Clear (cmd+K)")
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        if this.dock_tab == DockTab::DebugConsole {
                                            this.debug_lines.clear();
                                        } else if let Some(session) = this.active_terminal_mut() {
                                            session.lines.clear();
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("maximize-terminal")
                                    .ghost()
                                    .xsmall()
                                    .icon(
                                        Icon::new(if self.terminal_maximized {
                                            gpui_kit::assets::IconName::Minimize2
                                        } else {
                                            gpui_kit::assets::IconName::Maximize2
                                        })
                                        .xsmall()
                                        .text_color(muted),
                                    )
                                    .tooltip(if self.terminal_maximized {
                                        "Restore terminal size"
                                    } else {
                                        "Maximize terminal"
                                    })
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.terminal_maximized = !this.terminal_maximized;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(body_element)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AnsiColor, interrupt_signal, is_plain_cd, kill_args, parse_ansi_line,
        signal_process_group,
    };
    use std::os::unix::process::CommandExt as _;
    use std::process::Command;
    use std::time::{Duration, Instant};

    #[core::prelude::v1::test]
    fn the_first_interrupt_is_polite_and_the_second_is_a_hard_kill() {
        assert_eq!(interrupt_signal(false), "-INT");
        assert_eq!(interrupt_signal(true), "-KILL");
        assert_eq!(kill_args("-INT", 4242), ["-INT", "--", "-4242"], "a negative pid means the whole group");
    }

    /// The real thing: a shell that starts two long sleeps is stopped completely,
    /// quickly, by signalling its process group.
    #[core::prelude::v1::test]
    fn interrupting_the_group_stops_the_shell_and_what_it_started() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30 & sleep 30 & wait"])
            .process_group(0)
            .spawn()
            .expect("sh should start");
        std::thread::sleep(Duration::from_millis(200));
        let started = Instant::now();
        signal_process_group(child.id(), "-INT");
        let status = child.wait().expect("the shell should exit");
        assert!(!status.success(), "it was interrupted, not finished");
        assert!(started.elapsed() < Duration::from_secs(5), "it must stop promptly, not after 30s");
    }

    #[core::prelude::v1::test]
    fn only_a_lone_cd_uses_the_builtin() {
        assert!(is_plain_cd("cd"));
        assert!(is_plain_cd("cd /tmp"));
        assert!(is_plain_cd("cd '/tmp/with space'"));
        assert!(!is_plain_cd("cd /tmp && ls"), "a chained command belongs to the shell");
        assert!(!is_plain_cd("cd /tmp; ls"));
        assert!(!is_plain_cd("cd /tmp || exit"));
        assert!(!is_plain_cd("cd /tmp | wc -l"));
        assert!(!is_plain_cd("cdx"), "not a cd at all");
    }

    #[core::prelude::v1::test]
    fn carriage_returns_keep_only_the_latest_update() {
        // Flutter test progress rewrites the same line with `\r`; gluing the
        // rewrites together is what made the terminal look scrambled.
        let spans = parse_ansi_line("00:00 +0: loading a.dart\r00:01 +0: loading b.dart");
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "00:01 +0: loading b.dart");
    }

    #[core::prelude::v1::test]
    fn parses_git_log_ansi_colors_and_resets() {
        let raw = "\x1b[33mce029d1\x1b[m\x1b[33m (\x1b[m\x1b[1;36mHEAD -> \x1b[m\x1b[1;32mdevelop\x1b[m\x1b[33m)\x1b[m Add changelog";
        let spans = parse_ansi_line(raw);
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "ce029d1 (HEAD -> develop) Add changelog");
        assert_eq!(spans[0].color, AnsiColor::Yellow);
        assert!(spans.iter().any(|s| s.color == AnsiColor::Cyan && s.bold));
        assert!(spans.iter().any(|s| s.color == AnsiColor::Green && s.bold));
    }
}
