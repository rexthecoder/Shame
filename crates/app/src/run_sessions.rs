//! Run sessions: every "run" (a Python file, pytest, a training script)
//! becomes a session in the Runs dock tab with its command, status, exit
//! code, duration and full output, kept after it finishes.
//!
//! Unlike a terminal, the output is plain text the app owns, so a run can be
//! handed to the agent whole ("Ask agent" attaches it) instead of scraping
//! the terminal's screen.

use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::agent_ctx::Attachment;
use crate::{DartIde, DockTab, tokens};

/// Output kept per session; older text is dropped from the front.
const MAX_OUTPUT: usize = 2 * 1024 * 1024;
/// Finished sessions kept; the oldest go first.
const MAX_SESSIONS: usize = 30;
/// Width of the session list.
const LIST_W: f32 = 220.;
/// Output sent to the agent: its tail, where errors usually are.
const AGENT_TAIL: usize = 24 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunStatus {
    Running,
    /// Exit code; `None` when killed by a signal.
    Exited(Option<i32>),
    Stopped,
    FailedToStart(String),
}

impl RunStatus {
    fn ok(&self) -> bool {
        matches!(self, RunStatus::Exited(Some(0)))
    }
}

/// What to run: enough to run it again.
#[derive(Debug, Clone)]
pub(crate) struct RunSpec {
    /// Short name for the list: "model.py", "pytest".
    pub(crate) title: String,
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) env: Vec<(String, String)>,
}

impl RunSpec {
    /// The command as a shell line, for display and for the agent.
    pub(crate) fn command_line(&self) -> String {
        let mut s = crate::python::sh_quote(&self.program.to_string_lossy());
        for a in &self.args {
            s.push(' ');
            s.push_str(&crate::python::sh_quote(a));
        }
        s
    }
}

pub(crate) struct RunSession {
    pub(crate) id: u64,
    pub(crate) spec: RunSpec,
    pub(crate) status: RunStatus,
    pub(crate) started: Instant,
    pub(crate) duration: Option<Duration>,
    pub(crate) output: String,
    child: Option<Arc<Mutex<Child>>>,
}

impl RunSession {
    fn status_line(&self) -> String {
        match &self.status {
            RunStatus::Running => "Running".into(),
            RunStatus::Exited(Some(0)) => "Passed · exit 0".into(),
            RunStatus::Exited(Some(code)) => format!("Failed · exit {code}"),
            RunStatus::Exited(None) => "Killed".into(),
            RunStatus::Stopped => "Stopped".into(),
            RunStatus::FailedToStart(e) => format!("Couldn't start: {e}"),
        }
    }

    /// The run as an agent attachment: command, outcome and the tail of
    /// the output.
    pub(crate) fn attachment(&self) -> Attachment {
        let tail_start = self.output.len().saturating_sub(AGENT_TAIL);
        let tail_start = (tail_start..self.output.len())
            .find(|&i| self.output.is_char_boundary(i))
            .unwrap_or(self.output.len());
        let mut text = String::new();
        if tail_start > 0 {
            text.push_str("[… earlier output omitted …]\n");
        }
        text.push_str(&self.output[tail_start..]);
        Attachment::Run {
            title: self.spec.title.clone(),
            command: self.spec.command_line(),
            cwd: self.spec.cwd.to_string_lossy().into_owned(),
            status: self.status_line(),
            output: text,
        }
    }
}

pub(crate) enum RunEvent {
    Output { id: u64, text: String },
    Exited { id: u64, code: Option<i32> },
}

#[derive(Default)]
pub(crate) struct RunStore {
    pub(crate) sessions: Vec<RunSession>,
    pub(crate) selected: Option<u64>,
    next_id: u64,
    scroll: UniformListScrollHandle,
    /// Keep the output scrolled to its end while it streams.
    follow: bool,
}

impl RunStore {
    pub(crate) fn has_sessions(&self) -> bool {
        !self.sessions.is_empty()
    }

    pub(crate) fn any_running(&self) -> bool {
        self.sessions.iter().any(|s| s.status == RunStatus::Running)
    }

    fn selected(&self) -> Option<&RunSession> {
        let id = self.selected?;
        self.sessions.iter().find(|s| s.id == id)
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut RunSession> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }
}

/// Remove ANSI escape sequences and resolve carriage returns the way a
/// terminal would for progress bars: `\r` rewinds to the line start.
pub(crate) fn clean_output(raw: &str, out: &mut String) {
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                // CSI: ESC [ ... final byte in @..~ ; OSC: ESC ] ... BEL/ST.
                match chars.peek() {
                    Some('[') => {
                        chars.next();
                        for c in chars.by_ref() {
                            if ('@'..='~').contains(&c) {
                                break;
                            }
                        }
                    }
                    Some(']') => {
                        chars.next();
                        while let Some(c) = chars.next() {
                            if c == '\u{7}' {
                                break;
                            }
                            if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                                chars.next();
                                break;
                            }
                        }
                    }
                    _ => {
                        chars.next();
                    }
                }
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\r' => {
                // Overwrite the current line (tqdm-style progress).
                let line_start = out.rfind('\n').map_or(0, |i| i + 1);
                out.truncate(line_start);
            }
            c => out.push(c),
        }
    }
}

impl DartIde {
    /// Start `spec` as a new run session and show it in the Runs tab.
    pub(crate) fn start_run(&mut self, spec: RunSpec, cx: &mut Context<Self>) {
        let id = self.runs.next_id;
        self.runs.next_id += 1;
        let mut session = RunSession {
            id,
            spec: spec.clone(),
            status: RunStatus::Running,
            started: Instant::now(),
            duration: None,
            output: String::new(),
            child: None,
        };

        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Output as it happens, plain, without pagers.
            .env("PYTHONUNBUFFERED", "1")
            .env("NO_COLOR", "1")
            .env("TERM", "dumb");
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        // Its own process group, so Stop also ends what it started.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            cmd.process_group(0);
        }

        match cmd.spawn() {
            Err(e) => {
                session.status = RunStatus::FailedToStart(e.to_string());
                session.duration = Some(Duration::ZERO);
            }
            Ok(mut child) => {
                let (tx, rx) = async_channel::unbounded::<RunEvent>();
                let mut readers = Vec::new();
                for stream in [
                    child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
                    child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
                ]
                .into_iter()
                .flatten()
                {
                    let tx = tx.clone();
                    readers.push(std::thread::spawn(move || {
                        let mut stream = stream;
                        let mut buf = [0u8; 8192];
                        let mut pending: Vec<u8> = Vec::new();
                        loop {
                            match stream.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    pending.extend_from_slice(&buf[..n]);
                                    // Send only whole UTF-8 characters.
                                    let valid = match std::str::from_utf8(&pending) {
                                        Ok(_) => pending.len(),
                                        Err(e) => e.valid_up_to(),
                                    };
                                    if valid > 0 {
                                        let text = String::from_utf8_lossy(&pending[..valid]).into_owned();
                                        pending.drain(..valid);
                                        let _ = tx.send_blocking(RunEvent::Output { id, text });
                                    }
                                }
                            }
                        }
                        if !pending.is_empty() {
                            let text = String::from_utf8_lossy(&pending).into_owned();
                            let _ = tx.send_blocking(RunEvent::Output { id, text });
                        }
                    }));
                }
                let child = Arc::new(Mutex::new(child));
                session.child = Some(child.clone());
                std::thread::spawn(move || {
                    for r in readers {
                        let _ = r.join();
                    }
                    let code = loop {
                        let polled = child.lock().map(|mut c| c.try_wait());
                        match polled {
                            Ok(Ok(Some(status))) => break status.code(),
                            Ok(Ok(None)) => std::thread::sleep(Duration::from_millis(50)),
                            _ => break None,
                        }
                    };
                    let _ = tx.send_blocking(RunEvent::Exited { id, code });
                });
                cx.spawn(async move |weak, cx| {
                    while let Ok(event) = rx.recv().await {
                        // Batch whatever else is already queued into one update.
                        let mut batch = vec![event];
                        while let Ok(more) = rx.try_recv() {
                            batch.push(more);
                        }
                        if weak.update(cx, |this, cx| this.handle_run_events(batch, cx)).is_err() {
                            break;
                        }
                    }
                })
                .detach();
            }
        }

        self.runs.sessions.push(session);
        // Drop the oldest finished sessions past the cap.
        while self.runs.sessions.len() > MAX_SESSIONS {
            match self.runs.sessions.iter().position(|s| s.status != RunStatus::Running) {
                Some(ix) => {
                    self.runs.sessions.remove(ix);
                }
                None => break,
            }
        }
        self.runs.selected = Some(id);
        self.runs.follow = true;
        self.terminal_open = true;
        self.dock_tab = DockTab::Runs;
        cx.notify();
    }

    fn handle_run_events(&mut self, events: Vec<RunEvent>, cx: &mut Context<Self>) {
        for event in events {
            match event {
                RunEvent::Output { id, text } => {
                    if let Some(s) = self.runs.get_mut(id) {
                        clean_output(&text, &mut s.output);
                        if s.output.len() > MAX_OUTPUT {
                            let mut cut = s.output.len() - MAX_OUTPUT;
                            while !s.output.is_char_boundary(cut) {
                                cut += 1;
                            }
                            s.output.drain(..cut);
                        }
                    }
                }
                RunEvent::Exited { id, code } => {
                    if let Some(s) = self.runs.get_mut(id) {
                        s.child = None;
                        s.duration = Some(s.started.elapsed());
                        if s.status == RunStatus::Running {
                            s.status = RunStatus::Exited(code);
                        }
                    }
                }
            }
        }
        if self.runs.follow {
            if let Some(s) = self.runs.selected() {
                let lines = s.output.lines().count();
                self.runs.scroll.scroll_to_item(lines.saturating_sub(1), ScrollStrategy::Bottom);
            }
        }
        cx.notify();
    }

    fn stop_run(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(s) = self.runs.get_mut(id) else { return };
        if let Some(child) = &s.child {
            if let Ok(child) = child.lock() {
                // The whole process group: workers a script started too.
                #[cfg(unix)]
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGTERM);
                }
            }
        }
        s.status = RunStatus::Stopped;
        cx.notify();
    }

    fn rerun(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(spec) = self.runs.sessions.iter().find(|s| s.id == id).map(|s| s.spec.clone()) else {
            return;
        };
        self.save_dirty_python_buffers(cx);
        self.start_run(spec, cx);
    }

    fn ask_agent_about_run(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.runs.sessions.iter().find(|s| s.id == id) else { return };
        let question = match &session.status {
            RunStatus::Running => "This is still running. What is it doing so far?",
            s if s.ok() => "This run finished. Summarise the output and anything worth acting on.",
            _ => "This run failed. Why, and how do I fix it?",
        };
        let attachment = session.attachment();
        self.set_agent_open(true);
        self.agent.input.update(cx, |state, cx| state.set_value("", window, cx));
        self.insert_mention(attachment, window, cx);
        self.agent.input.update(cx, |state, cx| {
            state.insert(question, window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    fn clear_finished_runs(&mut self, cx: &mut Context<Self>) {
        self.runs.sessions.retain(|s| s.status == RunStatus::Running);
        if self.runs.selected().is_none() {
            self.runs.selected = self.runs.sessions.last().map(|s| s.id);
        }
        cx.notify();
    }

    /// The Runs tab pill's badge: running count, else the last run's outcome.
    pub(crate) fn runs_tab_badge(&self) -> Option<(String, bool)> {
        let running = self.runs.sessions.iter().filter(|s| s.status == RunStatus::Running).count();
        if running > 0 {
            return Some((running.to_string(), false));
        }
        let last = self.runs.sessions.last()?;
        (!last.status.ok()).then(|| ("!".to_string(), true))
    }

    pub(crate) fn render_runs_panel(&self, cx: &mut Context<Self>) -> Div {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let hover = cx.theme().list_hover;
        let (green, red, primary) = (cx.theme().green, cx.theme().red, cx.theme().primary);
        let mono = cx.theme().mono_font_family.clone();

        if self.runs.sessions.is_empty() {
            return v_flex()
                .flex_1()
                .w_full()
                .items_center()
                .justify_center()
                .gap(px(tokens::SPACE_2))
                .child(div().text_size(px(tokens::UI_SM)).text_color(fg).child("No runs yet"))
                .child(
                    div()
                        .text_size(px(tokens::UI_XS))
                        .text_color(muted)
                        .child("Run a Python file or pytest from the command palette; each run stays here."),
                );
        }

        let status_dot = |status: &RunStatus| {
            let color = match status {
                RunStatus::Running => primary,
                s if s.ok() => green,
                RunStatus::Stopped => muted,
                _ => red,
            };
            div().size(px(tokens::SPACE_3)).flex_none().rounded(px(tokens::RADIUS_PILL)).bg(color)
        };

        let list = v_flex()
            .id("runs-list")
            .w(px(LIST_W))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .border_r_1()
            .border_color(border)
            .py(px(tokens::SPACE_2))
            .children(self.runs.sessions.iter().rev().map(|s| {
                let id = s.id;
                let selected = self.runs.selected == Some(id);
                let time = match s.duration {
                    Some(d) => fmt_duration(d),
                    None => fmt_duration(s.started.elapsed()),
                };
                h_flex()
                    .id(("run-row", id as usize))
                    .mx(px(tokens::SPACE_2))
                    .h(px(tokens::TREE_ROW_H))
                    .px(px(tokens::SPACE_4))
                    .gap(px(tokens::SPACE_3))
                    .items_center()
                    .rounded(px(tokens::RADIUS_SM))
                    .cursor_pointer()
                    .when(selected, |r| r.bg(tokens::accent_dim()))
                    .when(!selected, |r| r.hover(move |h| h.bg(hover)))
                    .child(status_dot(&s.status))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(tokens::UI_SM))
                            .text_color(if selected { fg } else { muted })
                            .child(s.spec.title.clone()),
                    )
                    .child(div().text_size(px(tokens::UI_XS)).text_color(muted).child(time))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.runs.selected = Some(id);
                        this.runs.follow = true;
                        cx.notify();
                    }))
            }));

        let Some(session) = self.runs.selected() else {
            return h_flex().flex_1().w_full().min_h(px(0.)).child(list);
        };
        let id = session.id;
        let running = session.status == RunStatus::Running;

        let action = |key: &'static str, label: &'static str| {
            h_flex()
                .id(key)
                .h(px(tokens::TREE_ROW_H - tokens::SPACE_3))
                .px(px(tokens::SPACE_4))
                .items_center()
                .rounded(px(tokens::RADIUS_SM))
                .border_1()
                .border_color(border)
                .text_size(px(tokens::UI_XS))
                .text_color(fg)
                .cursor_pointer()
                .hover(move |h| h.bg(hover))
                .child(label)
        };

        let header = h_flex()
            .h(px(tokens::PANEL_BAR_H))
            .flex_none()
            .px(px(tokens::SPACE_5))
            .gap(px(tokens::SPACE_3))
            .items_center()
            .border_b_1()
            .border_color(border)
            .child(status_dot(&session.status))
            .child(
                div()
                    .flex_none()
                    .text_size(px(tokens::UI_SM))
                    .text_color(fg)
                    .child(session.status_line()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .font_family(mono.clone())
                    .text_size(px(tokens::UI_XS))
                    .text_color(muted)
                    .child(session.spec.command_line()),
            )
            .child(if running {
                action("run-stop", "Stop").on_click(cx.listener(move |this, _, _, cx| this.stop_run(id, cx)))
            } else {
                action("run-again", "Run again").on_click(cx.listener(move |this, _, _, cx| this.rerun(id, cx)))
            })
            .child(
                action("run-ask", "Ask agent")
                    .on_click(cx.listener(move |this, _, window, cx| this.ask_agent_about_run(id, window, cx))),
            )
            .when(self.runs.sessions.iter().any(|s| s.status != RunStatus::Running), |h| {
                h.child(action("run-clear", "Clear").on_click(cx.listener(|this, _, _, cx| this.clear_finished_runs(cx))))
            });

        let lines: std::rc::Rc<Vec<String>> =
            std::rc::Rc::new(session.output.lines().map(str::to_string).collect());
        let body: AnyElement = if lines.is_empty() {
            div()
                .flex_1()
                .p(px(tokens::SPACE_5))
                .text_size(px(tokens::UI_SM))
                .text_color(muted)
                .child(if running { "Waiting for output…" } else { "No output." })
                .into_any_element()
        } else {
            let count = lines.len();
            uniform_list("run-output", count, move |range: std::ops::Range<usize>, _window, cx| {
                let fg = cx.theme().foreground;
                let red = cx.theme().red;
                range
                    .map(|ix| {
                        let line = &lines[ix];
                        // Tracebacks and pytest failures stand out.
                        let is_err = line.starts_with("Traceback")
                            || line.starts_with("E ")
                            || line.contains("Error:")
                            || line.starts_with("FAILED");
                        div()
                            .h(px(tokens::CODE_LINE_H - tokens::SPACE_2))
                            .px(px(tokens::SPACE_5))
                            .whitespace_nowrap()
                            .text_color(if is_err { red } else { fg })
                            .child(if line.is_empty() { " ".to_string() } else { line.clone() })
                    })
                    .collect()
            })
            .track_scroll(&self.runs.scroll)
            .flex_1()
            .w_full()
            .py(px(tokens::SPACE_3))
            .font_family(mono)
            .text_size(px(tokens::UI_XS))
            .into_any_element()
        };

        h_flex()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .child(list)
            .child(v_flex().flex_1().min_w(px(0.)).h_full().child(header).child(body))
    }
}

fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{:.1}s", d.as_secs_f32())
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::{clean_output, fmt_duration};
    use std::time::Duration;

    #[test]
    fn strips_ansi_and_resolves_progress_bars() {
        let mut out = String::new();
        clean_output("\u{1b}[32mok\u{1b}[0m\r\n", &mut out);
        clean_output("epoch 1  10%\repoch 1  50%\repoch 1 100%\n", &mut out);
        clean_output("\u{1b}]0;title\u{7}done\n", &mut out);
        assert_eq!(out, "ok\nepoch 1 100%\ndone\n");
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(fmt_duration(Duration::from_millis(2340)), "2.3s");
        assert_eq!(fmt_duration(Duration::from_secs(125)), "2m 05s");
        assert_eq!(fmt_duration(Duration::from_secs(3700)), "1h 01m");
    }
}
