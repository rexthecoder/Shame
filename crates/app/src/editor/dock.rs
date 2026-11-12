use std::path::{Path, PathBuf};

use alacritty_terminal::term::TermMode;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub(crate) use super::terminal::TerminalSession;
use super::terminal::{
    GridLayout, GridSize, TerminalPalette, cell_size, file_link_at, key_to_bytes, mouse_report, paint_grid,
};
use crate::{DartIde, DockTab, tokens};

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

/// A line of the Debug Console.
#[derive(Clone, Debug)]
pub(crate) enum TerminalLine {
    Output {
        spans: Vec<AnsiSpan>,
        file_link: Option<(PathBuf, u32, u32)>,
    },
}

/// What a terminal's PTY thread tells the UI.
pub(crate) enum TerminalEvent {
    /// New output to draw (coalesced: one per redraw).
    Wakeup { session_id: usize },
    /// The program set (or reset) the window title.
    Title { session_id: usize, title: Option<String> },
    /// The program copied text with OSC 52.
    Clipboard { text: String },
    /// The shell exited.
    Exited { session_id: usize },
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

impl DartIde {
    pub(crate) fn active_terminal(&self) -> Option<&TerminalSession> {
        self.terminals.get(self.active_terminal)
    }

    pub(crate) fn active_terminal_mut(&mut self) -> Option<&mut TerminalSession> {
        self.terminals.get_mut(self.active_terminal)
    }

    /// A new shell at `cwd`, with the next free session id.
    pub(crate) fn spawn_terminal(&mut self, cwd: PathBuf) -> TerminalSession {
        let id = self.next_terminal_id;
        self.next_terminal_id += 1;
        TerminalSession::new(id, cwd, self.terminal_tx.clone())
    }

    pub(crate) fn new_terminal_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self
            .active_terminal()
            .map(TerminalSession::live_cwd)
            .unwrap_or_else(|| self.root.clone());
        let session = self.spawn_terminal(cwd);
        self.terminals.push(session);
        self.active_terminal = self.terminals.len() - 1;
        self.terminal_open = true;
        window.focus(&self.terminal_focus, cx);
        cx.notify();
    }

    /// Close a tab; dropping the session hangs up on its shell.
    pub(crate) fn close_terminal_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.terminals.len() {
            return;
        }
        self.terminals.remove(ix);
        if self.terminals.is_empty() {
            let session = self.spawn_terminal(self.root.clone());
            self.terminals.push(session);
            self.active_terminal = 0;
            self.terminal_open = false;
        } else if self.active_terminal > ix {
            self.active_terminal -= 1;
        } else {
            self.active_terminal = self.active_terminal.min(self.terminals.len() - 1);
        }
        cx.notify();
    }

    /// Replace a tab's shell with a fresh one at the project root.
    pub(crate) fn reset_terminal(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.terminals.len() {
            self.terminals[ix] = self.spawn_terminal(self.root.clone());
            cx.notify();
        }
    }

    pub(crate) fn handle_terminal_event(&mut self, event: TerminalEvent, cx: &mut Context<Self>) {
        match event {
            TerminalEvent::Wakeup { session_id } => {
                let Some(session) = self.terminals.iter_mut().find(|t| t.id == session_id) else {
                    return;
                };
                session.clear_wakeup();
                // A command finished (the shell is back in the foreground): files may have changed.
                let busy = session.is_busy();
                let finished = std::mem::replace(&mut session.was_busy, busy) && !busy;
                if finished {
                    self.refresh_git(cx);
                }
                cx.notify();
            }
            TerminalEvent::Title { session_id, title } => {
                if let Some(session) = self.terminals.iter_mut().find(|t| t.id == session_id) {
                    session.title = title;
                    cx.notify();
                }
            }
            TerminalEvent::Clipboard { text } => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            TerminalEvent::Exited { session_id } => {
                if let Some(ix) = self.terminals.iter().position(|t| t.id == session_id) {
                    self.terminals[ix].exited = true;
                    self.close_terminal_tab(ix, cx);
                    self.refresh_git(cx);
                }
            }
        }
    }

    /// Type `raw_cmd` at the active shell's prompt and run it. If a program is
    /// already running there, the command gets a tab of its own.
    pub(crate) fn execute_terminal_command(&mut self, raw_cmd: String, cx: &mut Context<Self>) {
        if self.active_terminal().is_none_or(|s| s.is_busy() || s.exited) {
            let cwd = self
                .active_terminal()
                .map(TerminalSession::live_cwd)
                .unwrap_or_else(|| self.root.clone());
            let session = self.spawn_terminal(cwd);
            self.terminals.push(session);
            self.active_terminal = self.terminals.len() - 1;
        }
        if let Some(session) = self.active_terminal() {
            session.run_command(raw_cmd.trim());
        }
        cx.notify();
    }

    fn on_terminal_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let key = keystroke.key.as_str();
        let modifiers = &keystroke.modifiers;

        // The Debug Console & Problems tabs are read-only: don't send keystrokes to the hidden shell.
        if self.dock_tab != DockTab::Terminal {
            if modifiers.platform && key == "k" && self.dock_tab == DockTab::DebugConsole {
                self.debug_lines.clear();
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }

        // Let global IDE shortcuts (`⌘P`, `⌘B`, `⌘J`, `⌘O`, `⌘N`, `⌘F`, `⌘L`, `⌘G`) bubble up
        if modifiers.platform && matches!(key, "p" | "b" | "j" | "o" | "n" | "f" | "l" | "g") {
            return;
        }

        cx.stop_propagation();

        // `⌘T` opens a new terminal tab when focused in the terminal
        if modifiers.platform && key == "t" {
            self.new_terminal_tab(window, cx);
            return;
        }

        let capslock = window.capslock().on;
        let Some(session) = self.active_terminal_mut() else {
            return;
        };
        let mode = session.mode();
        match key {
            // `⌘K` clears the scrollback
            "k" if modifiers.platform => session.clear(),
            // `⌘C` copies the selection (`Ctrl+C` interrupts, via the PTY)
            "c" if modifiers.platform => {
                if let Some(text) = session.selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                return;
            }
            "a" if modifiers.platform => session.select_all(),
            "v" if modifiers.platform => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    session.paste(&text);
                }
            }
            // Shift+PageUp / PageDown page through the scrollback (full-screen
            // programs get the keys themselves).
            "pageup" | "pagedown"
                if modifiers.shift && !modifiers.control && !mode.contains(TermMode::ALT_SCREEN) =>
            {
                session.scroll_page(key == "pageup");
            }
            _ => match key_to_bytes(keystroke, mode, capslock) {
                Some(bytes) => session.input(bytes),
                None => return,
            },
        }
        cx.notify();
    }

    fn on_terminal_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.terminal_focus, cx);
        let Some(session) = self.active_terminal_mut() else {
            return;
        };
        let Some(layout) = session.layout.get() else {
            return;
        };
        let (row, col, side) = layout.cell_at(event.position);

        // `⌘`-click opens a `path:line:col` in the editor.
        if event.modifiers.platform {
            let (text, offset) = session.logical_line_at(row);
            if let Some((path, line_0, col_0)) = file_link_at(&text, offset + col, &session.live_cwd()) {
                self.open_path(path, window, cx);
                if let Some(ed) = self.active_editor().cloned() {
                    ed.update(cx, |editor, cx| editor.set_cursor_to_position(line_0, col_0, cx));
                }
                return;
            }
        }

        // A program that asked for the mouse gets it; Shift still selects.
        let mode = session.mode();
        if mode.intersects(TermMode::MOUSE_MODE) && !event.modifiers.shift {
            if let Some(bytes) = mouse_report(0, true, false, (col, row), &event.modifiers, mode) {
                session.lines.write(bytes);
            }
            session.mouse_reported = true;
            return;
        }

        session.begin_selection(row, col, side, event.click_count);
        cx.notify();
    }

    fn on_terminal_mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let Some(session) = self.active_terminal_mut() else {
            return;
        };
        let Some(layout) = session.layout.get() else {
            return;
        };
        let (row, col, side) = layout.cell_at(event.position);
        if session.mouse_reported {
            let mode = session.mode();
            if mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
                && let Some(bytes) = mouse_report(0, true, true, (col, row), &event.modifiers, mode)
            {
                session.lines.write(bytes);
            }
            return;
        }
        if session.selecting {
            session.extend_selection(row, col, side);
            cx.notify();
        }
    }

    fn on_terminal_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.active_terminal_mut() else {
            return;
        };
        if std::mem::take(&mut session.mouse_reported) {
            if let Some(layout) = session.layout.get() {
                let (row, col, _) = layout.cell_at(event.position);
                let mode = session.mode();
                if let Some(bytes) = mouse_report(0, false, false, (col, row), &event.modifiers, mode) {
                    session.lines.write(bytes);
                }
            }
            return;
        }
        if session.selecting {
            session.end_selection();
            cx.notify();
        }
    }

    fn on_terminal_scroll(&mut self, event: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.active_terminal_mut() else {
            return;
        };
        let Some(layout) = session.layout.get() else {
            return;
        };
        let travel = match event.delta {
            ScrollDelta::Lines(delta) => delta.y,
            ScrollDelta::Pixels(delta) => f32::from(delta.y) / f32::from(layout.cell.height).max(1.),
        };
        session.scroll_remainder += travel;
        let lines = session.scroll_remainder.trunc() as i32;
        session.scroll_remainder -= lines as f32;
        if lines == 0 {
            return;
        }
        let mode = session.mode();
        let count = lines.unsigned_abs() as usize;
        if mode.intersects(TermMode::MOUSE_MODE) && !event.modifiers.shift {
            let (row, col, _) = layout.cell_at(event.position);
            let button = if lines > 0 { 64 } else { 65 };
            if let Some(report) = mouse_report(button, true, false, (col, row), &event.modifiers, mode) {
                session.lines.write(report.repeat(count));
            }
        } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            // Full-screen programs without mouse support (less, man) scroll with the arrow keys.
            let arrow: &[u8] = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            session.lines.write(arrow.repeat(count));
        } else {
            session.scroll_lines(lines);
        }
        cx.notify();
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
        let selection_bg = cx.theme().selection.opacity(0.45);
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
                let is_running = session.is_busy();
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
            // One header per file (its name, folder and counts), then its
            // problems: errors before warnings, each in line order.
            let mut by_file: Vec<(std::path::PathBuf, Vec<core_lsp::lsp_types::Diagnostic>)> = Vec::new();
            for (path, diag) in all_problems {
                match by_file.iter_mut().find(|(p, _)| *p == path) {
                    Some((_, list)) => list.push(diag),
                    None => by_file.push((path, vec![diag])),
                }
            }
            let is_error = |d: &core_lsp::lsp_types::Diagnostic| {
                d.severity == Some(core_lsp::lsp_types::DiagnosticSeverity::ERROR)
            };
            for (_, list) in &mut by_file {
                list.sort_by_key(|d| (!is_error(d), d.range.start.line, d.range.start.character));
            }
            // Files with errors first.
            by_file.sort_by_key(|(_, list)| !list.iter().any(is_error));
            let root = self.root.clone();
            let problem_row = |ix: usize,
                               path: std::path::PathBuf,
                               diag: core_lsp::lsp_types::Diagnostic|
             -> AnyElement {
                    let is_err = diag.severity
                        == Some(core_lsp::lsp_types::DiagnosticSeverity::ERROR);
                    let sev_color = if is_err { red } else { yellow };
                    let sev_icon = if is_err {
                        gpui_kit::assets::IconName::CircleX
                    } else {
                        gpui_kit::assets::IconName::TriangleAlert
                    };
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
                        .pl(px(tokens::SPACE_7))
                        .pr(px(tokens::SPACE_4))
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
            };
            let mut rows: Vec<AnyElement> = Vec::new();
            let mut ix = 0usize;
            for (fix, (path, diags)) in by_file.into_iter().enumerate() {
                let errs = diags.iter().filter(|d| is_error(d)).count();
                let warns = diags.len() - errs;
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                let dir = path
                    .parent()
                    .map(|p| p.strip_prefix(&root).unwrap_or(p).to_string_lossy().to_string())
                    .unwrap_or_default();
                rows.push(
                    h_flex()
                        .w_full()
                        .h(px(24.))
                        .mt(px(if fix == 0 { 0. } else { tokens::SPACE_2 }))
                        .px(px(tokens::SPACE_4))
                        .items_center()
                        .gap(px(tokens::SPACE_2))
                        .child(crate::rails::files::render_file_icon(&name))
                        .child(div().text_size(px(tokens::UI_SM)).text_color(fg).child(name))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .text_size(px(tokens::UI_XS))
                                .text_color(muted)
                                .child(dir),
                        )
                        .when(errs > 0, |el| {
                            el.child(div().text_size(px(tokens::UI_XS)).text_color(red).child(format!("{errs} error{}", if errs == 1 { "" } else { "s" })))
                        })
                        .when(warns > 0, |el| {
                            el.child(div().text_size(px(tokens::UI_XS)).text_color(yellow).child(format!("{warns} warning{}", if warns == 1 { "" } else { "s" })))
                        })
                        .into_any_element(),
                );
                for diag in diags {
                    let path = path.clone();
                    rows.push({
                        let row_ix = ix;
                        ix += 1;
                        problem_row(row_ix, path, diag)
                    });
                }
            }

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
                    let TerminalLine::Output { spans, file_link } = line;
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
            let brighten = |c: Hsla| Hsla { l: (c.l + 0.08).min(0.92), ..c };
            let black = cx.theme().muted;
            let white = fg.opacity(0.85);
            let palette = TerminalPalette {
                ansi: [
                    black,
                    red,
                    green,
                    yellow,
                    blue,
                    magenta,
                    cyan,
                    white,
                    muted,
                    brighten(red),
                    brighten(green),
                    brighten(yellow),
                    brighten(blue),
                    brighten(magenta),
                    brighten(cyan),
                    fg,
                ],
                foreground: fg,
                background: cx.theme().background,
                cursor: primary,
                selection: selection_bg,
            };
            let handle = session.lines.clone();
            let paint_handle = handle.clone();
            let layout_slot = session.layout.clone();
            let font = gpui_kit::font("SF Mono");
            let paint_font = font.clone();
            let font_size = px(tokens::MONO);
            // Measure the cells, fit the PTY to the space, then paint the grid.
            let grid = canvas(
                move |bounds, window, _cx| {
                    let cell = cell_size(&font, font_size, window);
                    let cols = (f32::from(bounds.size.width) / f32::from(cell.width)).floor().max(2.) as usize;
                    let rows = (f32::from(bounds.size.height) / f32::from(cell.height)).floor().max(1.) as usize;
                    let layout = GridLayout { origin: bounds.origin, cell, cols, rows };
                    layout_slot.set(Some(layout));
                    handle.resize(GridSize { cols, rows }, cell);
                    layout
                },
                move |_bounds, layout, window, cx| {
                    paint_grid(&paint_handle, layout, &palette, &paint_font, font_size, is_focused, window, cx);
                },
            )
            .size_full();

            div()
                .id("terminal-scrollback")
                .flex_1()
                .w_full()
                .min_h(px(0.))
                .px(px(tokens::SPACE_4))
                .py(px(tokens::SPACE_3))
                .bg(cx.theme().background)
                .cursor_text()
                .on_mouse_down(MouseButton::Left, cx.listener(Self::on_terminal_mouse_down))
                .on_mouse_move(cx.listener(Self::on_terminal_mouse_move))
                .on_mouse_up(MouseButton::Left, cx.listener(Self::on_terminal_mouse_up))
                .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_terminal_mouse_up))
                .on_scroll_wheel(cx.listener(Self::on_terminal_scroll))
                .child(grid)
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
                                        let ix = this.active_terminal;
                                        this.reset_terminal(ix, cx);
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
                                        } else if let Some(session) = this.active_terminal() {
                                            session.clear();
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
    use super::{AnsiColor, parse_ansi_line};

    #[core::prelude::v1::test]
    fn carriage_returns_keep_only_the_latest_update() {
        // Flutter test progress rewrites the same line with `\r`; gluing the
        // rewrites together is what made the console look scrambled.
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
