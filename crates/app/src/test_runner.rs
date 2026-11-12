//! The Tests panel: run `flutter test` / `dart test` with the JSON reporter and
//! watch the results arrive as a tree of files, groups and tests, with failures
//! expandable in place, click-to-jump to the source, and re-run of failures.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use core_flutter::{TestCase, TestRun, TestStatus};
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, tokens};

/// What a run covers, so it can be repeated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TestTarget {
    All,
    File(PathBuf),
}

enum TestEvent {
    Started { pid: u32 },
    Line(String),
    Stderr(String),
    Finished,
}

pub(crate) struct TestRunState {
    pub run: TestRun,
    pub target: TestTarget,
    pub package_dir: PathBuf,
    pub pid: Option<u32>,
    pub started: Instant,
    pub elapsed: Option<Duration>,
    pub only_failures: bool,
    /// Failed tests whose error is shown.
    pub expanded: HashSet<i64>,
    /// Runner output that wasn't a test event (build errors and the like).
    pub log: Vec<String>,
    /// Run with `--coverage`; the report is loaded when it finishes.
    pub coverage: bool,
}

/// The nearest directory at or above `from` containing a `pubspec.yaml`.
pub(crate) fn package_dir_for(from: &Path, fallback: &Path) -> PathBuf {
    let mut cur = from.parent().or(Some(from));
    while let Some(dir) = cur {
        if dir.join("pubspec.yaml").is_file() {
            return dir.to_path_buf();
        }
        cur = dir.parent();
    }
    fallback.to_path_buf()
}

/// `flutter` / `dart` for this package: the project's pinned FVM SDK if there
/// is one, otherwise whatever is on `PATH`.
fn tool_binary(package_dir: &Path, root: &Path, flutter: bool) -> String {
    for base in [package_dir, root] {
        let pinned = base.join(".fvm/flutter_sdk/bin").join(if flutter { "flutter" } else { "dart" });
        if pinned.is_file() {
            return pinned.to_string_lossy().into_owned();
        }
    }
    if flutter { "flutter" } else { "dart" }.to_string()
}

/// "12 passed", "2 failed", ... for the summary line.
pub(crate) fn summary_text(counts: core_flutter::Counts) -> String {
    let mut parts = Vec::new();
    if counts.passed > 0 {
        parts.push(format!("{} passed", counts.passed));
    }
    if counts.failed > 0 {
        parts.push(format!("{} failed", counts.failed));
    }
    if counts.skipped > 0 {
        parts.push(format!("{} skipped", counts.skipped));
    }
    if counts.running > 0 {
        parts.push(format!("{} running", counts.running));
    }
    if parts.is_empty() {
        "No tests yet".to_string()
    } else {
        parts.join(" \u{b7} ")
    }
}

impl DartIde {
    /// Run tests: everything in the package, one file, or only the named
    /// tests (a regular expression, used to re-run failures).
    pub(crate) fn start_test_run(
        &mut self,
        target: TestTarget,
        plain_name: Option<String>,
        name_regex: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.test_run.as_ref().is_some_and(|t| t.pid.is_some()) {
            self.stop_test_run(cx);
        }
        let anchor = match &target {
            TestTarget::File(path) => path.clone(),
            TestTarget::All => self.root.clone(),
        };
        let package_dir = package_dir_for(&anchor, &self.root);
        let flutter = std::fs::read_to_string(package_dir.join("pubspec.yaml"))
            .map(|text| core_flutter::is_flutter_package(&text))
            .unwrap_or(true);
        let rel_file = match &target {
            TestTarget::File(path) => path.strip_prefix(&package_dir).ok().map(|p| p.to_string_lossy().into_owned()),
            TestTarget::All => None,
        };
        let program = tool_binary(&package_dir, &self.root, flutter);
        let mut args = core_flutter::test_args(flutter, rel_file.as_deref(), plain_name.as_deref(), name_regex.as_deref());
        // `flutter test --coverage` writes coverage/lcov.info in the package.
        let with_coverage = std::mem::take(&mut self.coverage_requested) && flutter;
        if with_coverage {
            args.insert(1, "--coverage".into());
        }

        self.terminal_open = true;
        self.dock_tab = crate::DockTab::Tests;
        self.test_run = Some(TestRunState {
            run: TestRun::new(),
            target,
            package_dir: package_dir.clone(),
            pid: None,
            started: Instant::now(),
            elapsed: None,
            only_failures: false,
            expanded: HashSet::new(),
            log: Vec::new(),
            coverage: with_coverage,
        });
        cx.notify();

        let (tx, rx) = async_channel::unbounded::<TestEvent>();
        std::thread::spawn(move || {
            let home = std::env::var("HOME").unwrap_or_default();
            let path = std::env::var("PATH").unwrap_or_default();
            let mut child = match Command::new(&program)
                .args(&args)
                .current_dir(&package_dir)
                .env(
                    "PATH",
                    format!("{home}/.pub-cache/bin:{home}/fvm/default/bin:/opt/homebrew/bin:/usr/local/bin:{path}"),
                )
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            {
                Ok(child) => child,
                Err(err) => {
                    let _ = tx.send_blocking(TestEvent::Stderr(format!("Could not start {program}: {err}")));
                    let _ = tx.send_blocking(TestEvent::Finished);
                    return;
                }
            };
            let _ = tx.send_blocking(TestEvent::Started { pid: child.id() });
            let stderr = child.stderr.take().map(|stderr| {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        let _ = tx.send_blocking(TestEvent::Stderr(line));
                    }
                })
            });
            if let Some(stdout) = child.stdout.take() {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    let _ = tx.send_blocking(TestEvent::Line(line));
                }
            }
            if let Some(handle) = stderr {
                let _ = handle.join();
            }
            let _ = child.wait();
            let _ = tx.send_blocking(TestEvent::Finished);
        });

        cx.spawn(async move |weak, cx| {
            while let Ok(event) = rx.recv().await {
                let alive = weak
                    .update(cx, |this, cx| {
                        this.handle_test_event(event, cx);
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// Run every test with coverage; the editors show it when the run ends.
    pub(crate) fn start_coverage_run(&mut self, cx: &mut Context<Self>) {
        self.coverage_requested = true;
        self.start_test_run(TestTarget::All, None, None, cx);
    }

    /// Read `coverage/lcov.info` under `package_dir` and show it in every editor.
    pub(crate) fn load_coverage(&mut self, package_dir: &std::path::Path, cx: &mut Context<Self>) {
        let file = package_dir.join("coverage/lcov.info");
        let Ok(text) = std::fs::read_to_string(&file) else {
            self.show_toast("No coverage report was written (coverage/lcov.info).", true, cx);
            return;
        };
        let coverage = core_flutter::parse_lcov(&text, package_dir);
        let (hit, all) = core_flutter::coverage_totals(&coverage);
        let pct = if all == 0 { 0.0 } else { hit as f64 * 100.0 / all as f64 };
        cx.set_global(editor_view::Coverage(coverage));
        self.coverage_shown = true;
        self.refresh_editors(cx);
        self.show_toast(format!("Coverage {pct:.1}% \u{b7} {hit} of {all} lines run by tests"), false, cx);
    }

    /// Hide the coverage bars.
    pub(crate) fn clear_coverage(&mut self, cx: &mut Context<Self>) {
        cx.set_global(editor_view::Coverage::default());
        self.coverage_shown = false;
        self.refresh_editors(cx);
    }

    fn refresh_editors(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            tab.editor.update(cx, |_, cx| cx.notify());
        }
        cx.notify();
    }

    fn handle_test_event(&mut self, event: TestEvent, cx: &mut Context<Self>) {
        let mut coverage_dir = None;
        let Some(state) = self.test_run.as_mut() else { return };
        match event {
            TestEvent::Started { pid } => state.pid = Some(pid),
            TestEvent::Line(line) => {
                if !state.run.apply_line(&line) && !line.trim_start().starts_with('{') {
                    state.log.push(line);
                }
                // Open the first failure so the reason is on screen straight away.
                if state.expanded.is_empty() {
                    if let Some(first) = state.run.failed_tests().first() {
                        state.expanded.insert(first.id);
                    }
                }
            }
            TestEvent::Stderr(line) => state.log.push(line),
            TestEvent::Finished => {
                state.pid = None;
                state.elapsed = Some(state.started.elapsed());
                if state.coverage {
                    coverage_dir = Some(state.package_dir.clone());
                }
            }
        }
        if state.log.len() > 400 {
            state.log.drain(..state.log.len() - 400);
        }
        if let Some(dir) = coverage_dir {
            self.load_coverage(&dir, cx);
        }
        self.sync_test_marks(cx);
        cx.notify();
    }

    /// Show the last run's result next to each test declaration in the open
    /// editors: a gutter mark, and a margin note with the failure.
    pub(crate) fn sync_test_marks(&self, cx: &mut Context<Self>) {
        let mut by_file: HashMap<PathBuf, BTreeMap<usize, (editor_view::TestMark, Option<String>)>> =
            HashMap::new();
        if let Some(state) = &self.test_run {
            for test in state.run.tests.values() {
                if test.line == 0 {
                    continue;
                }
                let path = self.test_path(state, test);
                let path = std::fs::canonicalize(&path).unwrap_or(path);
                let mark = match test.status {
                    TestStatus::Running => editor_view::TestMark::Running,
                    TestStatus::Passed => editor_view::TestMark::Passed,
                    TestStatus::Failed | TestStatus::Errored => editor_view::TestMark::Failed,
                    TestStatus::Skipped => editor_view::TestMark::Skipped,
                };
                let error = test.failure_summary();
                let slot = by_file.entry(path).or_default();
                let line = test.line as usize - 1;
                // Several tests on one line (a loop): the worst one shows.
                let keep = match slot.get(&line) {
                    Some((existing, _)) => mark_rank(mark) > mark_rank(*existing),
                    None => true,
                };
                if keep {
                    slot.insert(line, (mark, error));
                }
            }
        }
        for tab in &self.tabs {
            let Some(path) = &tab.path else { continue };
            let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            let marks: Vec<_> = by_file
                .get(&path)
                .map(|lines| {
                    lines
                        .iter()
                        .map(|(line, (mark, error))| (*line, *mark, error.clone()))
                        .collect()
                })
                .unwrap_or_default();
            tab.editor.update(cx, |editor, cx| editor.set_test_marks(marks, cx));
        }
    }

    pub(crate) fn stop_test_run(&mut self, cx: &mut Context<Self>) {
        if let Some(pid) = self.test_run.as_mut().and_then(|t| t.pid.take()) {
            let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).status();
            if let Some(state) = self.test_run.as_mut() {
                state.elapsed = Some(state.started.elapsed());
            }
            cx.notify();
        }
    }

    /// Run only the tests that failed last time.
    pub(crate) fn rerun_failed_tests(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &self.test_run else { return };
        let names: Vec<&str> = state.run.failed_tests().iter().map(|t| t.name.as_str()).collect();
        if names.is_empty() {
            return;
        }
        let regex = core_flutter::name_filter_regex(&names);
        let target = state.target.clone();
        self.start_test_run(target, None, Some(regex), cx);
    }

    /// Run the tests in the file open in the editor.
    pub(crate) fn run_tests_in_active_file(&mut self, cx: &mut Context<Self>) {
        let path = self
            .active_editor()
            .and_then(|ed| ed.read(cx).buffer().path().map(Path::to_path_buf));
        match path {
            Some(path) => self.start_test_run(TestTarget::File(path), None, None, cx),
            None => self.start_test_run(TestTarget::All, None, None, cx),
        }
    }

    fn test_path(&self, state: &TestRunState, test: &TestCase) -> PathBuf {
        if test.path.is_absolute() { test.path.clone() } else { state.package_dir.join(&test.path) }
    }

    fn open_test_source(&mut self, path: PathBuf, line: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.open_path(path, window, cx);
        if let Some(editor) = self.active_editor().cloned() {
            editor.update(cx, |editor, cx| editor.set_cursor_to_position(line.saturating_sub(1), 0, cx));
        }
    }

    /// The Tests dock tab's label with its running tally.
    pub(crate) fn tests_tab_badge(&self) -> Option<(usize, bool)> {
        let state = self.test_run.as_ref()?;
        let counts = state.run.counts();
        if counts.failed > 0 {
            Some((counts.failed, true))
        } else if counts.total() > 0 {
            Some((counts.passed, false))
        } else {
            None
        }
    }

    pub(crate) fn render_tests_panel(&self, cx: &mut Context<Self>) -> Div {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let green = cx.theme().green;
        let red = cx.theme().red;
        let border = cx.theme().border;
        let list_hover = cx.theme().list_hover;
        let mono = cx.theme().mono_font_family.clone();

        let toolbar_button = |id: &'static str, label: String, enabled: bool| {
            h_flex()
                .id(id)
                .h(px(24.))
                .px(px(10.))
                .items_center()
                .rounded(px(tokens::RADIUS_SM))
                .border_1()
                .border_color(border)
                .text_size(px(tokens::UI_XS))
                .when(enabled, |b| b.cursor_pointer().text_color(fg).hover(move |h| h.bg(list_hover)))
                .when(!enabled, |b| b.text_color(muted.opacity(0.5)))
                .child(label)
        };

        let Some(state) = self.test_run.as_ref() else {
            return v_flex()
                .flex_1()
                .w_full()
                .items_center()
                .justify_center()
                .gap(px(tokens::SPACE_3))
                .child(
                    div()
                        .text_size(px(tokens::UI_SM))
                        .text_color(muted)
                        .child("Run your tests and watch them pass or fail here."),
                )
                .child(
                    h_flex()
                        .gap(px(tokens::SPACE_2))
                        .child(
                            toolbar_button("tests-run-all", "Run all tests".to_string(), true)
                                .on_click(cx.listener(|this, _e, _w, cx| {
                                    this.start_test_run(TestTarget::All, None, None, cx);
                                })),
                        )
                        .child(
                            toolbar_button("tests-run-file", "Run tests in this file".to_string(), true)
                                .on_click(cx.listener(|this, _e, _w, cx| this.run_tests_in_active_file(cx))),
                        ),
                );
        };

        let counts = state.run.counts();
        let running = state.pid.is_some();
        let failed_count = counts.failed;
        let elapsed = state.elapsed.unwrap_or_else(|| state.started.elapsed());
        let only_failures = state.only_failures;
        let status_color = if failed_count > 0 {
            red
        } else if running {
            muted
        } else if counts.total() > 0 {
            green
        } else {
            muted
        };

        let toolbar = h_flex()
            .w_full()
            .flex_shrink_0()
            .h(px(36.))
            .px(px(tokens::SPACE_4))
            .gap(px(tokens::SPACE_2))
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(border)
            .child(
                h_flex()
                    .gap(px(tokens::SPACE_2))
                    .items_center()
                    .child(if running {
                        toolbar_button("tests-stop", "Stop".to_string(), true)
                            .on_click(cx.listener(|this, _e, _w, cx| this.stop_test_run(cx)))
                    } else {
                        toolbar_button("tests-run-all", "Run all".to_string(), true).on_click(
                            cx.listener(|this, _e, _w, cx| this.start_test_run(TestTarget::All, None, None, cx)),
                        )
                    })
                    .child(
                        toolbar_button("tests-run-file", "This file".to_string(), !running)
                            .on_click(cx.listener(|this, _e, _w, cx| this.run_tests_in_active_file(cx))),
                    )
                    .child(
                        toolbar_button(
                            "tests-rerun-failed",
                            format!("Re-run failed ({failed_count})"),
                            !running && failed_count > 0,
                        )
                        .on_click(cx.listener(|this, _e, _w, cx| this.rerun_failed_tests(cx))),
                    )
                    .child(
                        toolbar_button("tests-failures-only", "Failures only".to_string(), true)
                            .when(only_failures, |b| b.bg(tokens::accent_dim()))
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                if let Some(state) = this.test_run.as_mut() {
                                    state.only_failures = !state.only_failures;
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap(px(tokens::SPACE_3))
                    .items_center()
                    .text_size(px(tokens::UI_SM))
                    .child(div().text_color(status_color).child(summary_text(counts)))
                    .child(div().text_color(muted).child(if running {
                        format!("running\u{2026} {:.1}s", elapsed.as_secs_f32())
                    } else {
                        format!("{:.1}s", elapsed.as_secs_f32())
                    })),
            );

        let mut rows: Vec<AnyElement> = Vec::new();
        for suite in state.run.outline() {
            let suite_tests: Vec<&TestCase> = suite
                .groups
                .iter()
                .flat_map(|g| g.tests.iter())
                .filter_map(|id| state.run.tests.get(id))
                .collect();
            if only_failures && !suite_tests.iter().any(|t| t.status.is_bad()) {
                continue;
            }
            let suite_path = if suite.path.is_absolute() { suite.path.clone() } else { state.package_dir.join(&suite.path) };
            let shown = suite_path.strip_prefix(&state.package_dir).unwrap_or(&suite_path).display().to_string();
            let bad = suite_tests.iter().filter(|t| t.status.is_bad()).count();
            let open_path = suite_path.clone();
            rows.push(
                h_flex()
                    .id(("tests-suite", rows.len()))
                    .w_full()
                    .h(px(26.))
                    .px(px(tokens::SPACE_4))
                    .gap(px(tokens::SPACE_2))
                    .items_center()
                    .cursor_pointer()
                    .text_size(px(tokens::UI_SM))
                    .font_medium()
                    .text_color(fg)
                    .hover(move |s| s.bg(list_hover))
                    .child(crate::rails::files::render_file_icon(&shown))
                    .child(div().truncate().child(shown))
                    .when(bad > 0, |r| r.child(div().text_color(red).text_size(px(tokens::UI_XS)).child(format!("{bad} failed"))))
                    .on_click(cx.listener(move |this, _e, window, cx| {
                        this.open_path(open_path.clone(), window, cx);
                    }))
                    .into_any_element(),
            );
            for group in &suite.groups {
                let tests: Vec<&TestCase> = group
                    .tests
                    .iter()
                    .filter_map(|id| state.run.tests.get(id))
                    .filter(|t| !only_failures || t.status.is_bad())
                    .collect();
                if tests.is_empty() {
                    continue;
                }
                if !group.name.is_empty() {
                    rows.push(
                        div()
                            .w_full()
                            .h(px(22.))
                            .pl(px(tokens::SPACE_4 + 22.))
                            .flex()
                            .items_center()
                            .text_size(px(tokens::UI_XS))
                            .text_color(muted)
                            .child(group.name.clone())
                            .into_any_element(),
                    );
                }
                let indent = if group.name.is_empty() { 22. } else { 38. };
                for test in tests {
                    let id = test.id;
                    let bad = test.status.is_bad();
                    let expanded = state.expanded.contains(&id);
                    let (glyph, color) = match test.status {
                        TestStatus::Passed => ("\u{2713}", green),
                        TestStatus::Failed | TestStatus::Errored => ("\u{2715}", red),
                        TestStatus::Skipped => ("\u{2013}", muted),
                        TestStatus::Running => ("\u{25cf}", cx.theme().warning),
                    };
                    let path = self.test_path(state, test);
                    let line = test.line;
                    rows.push(
                        h_flex()
                            .id(("tests-case", id as usize))
                            .w_full()
                            .h(px(24.))
                            .pl(px(tokens::SPACE_4 + indent))
                            .pr(px(tokens::SPACE_4))
                            .gap(px(tokens::SPACE_2))
                            .items_center()
                            .cursor_pointer()
                            .text_size(px(tokens::UI_SM))
                            .text_color(fg)
                            .hover(move |s| s.bg(list_hover))
                            .child(div().w(px(12.)).flex_shrink_0().text_color(color).child(glyph))
                            .child(div().truncate().child(test.short_name().to_string()))
                            .when(bad, |r| {
                                r.child(
                                    div()
                                        .text_size(px(tokens::UI_XS))
                                        .text_color(muted)
                                        .child(if expanded { "hide" } else { "show error" }),
                                )
                            })
                            .on_click(cx.listener(move |this, _e, window, cx| {
                                if bad {
                                    if let Some(state) = this.test_run.as_mut() {
                                        if !state.expanded.remove(&id) {
                                            state.expanded.insert(id);
                                        }
                                    }
                                    cx.notify();
                                }
                                if line > 0 {
                                    this.open_test_source(path.clone(), line, window, cx);
                                }
                            }))
                            .into_any_element(),
                    );
                    if bad && expanded {
                        let mut detail = test.error.clone().unwrap_or_else(|| "The test failed.".to_string());
                        if let Some(stack) = &test.stack {
                            detail.push_str("\n\n");
                            detail.push_str(stack);
                        }
                        if !test.output.is_empty() {
                            detail.push_str("\n\nOutput:\n");
                            detail.push_str(&test.output.join("\n"));
                        }
                        rows.push(
                            div()
                                .w_full()
                                .ml(px(tokens::SPACE_4 + indent + 20.))
                                .mr(px(tokens::SPACE_4))
                                .my(px(2.))
                                .p(px(tokens::SPACE_3))
                                .rounded(px(tokens::RADIUS_SM))
                                .bg(red.opacity(0.08))
                                .border_1()
                                .border_color(red.opacity(0.25))
                                .font_family(mono.clone())
                                .text_size(px(tokens::UI_XS))
                                .text_color(fg)
                                .child(detail)
                                .into_any_element(),
                        );
                    }
                }
            }
        }

        // Nothing parsed yet, or the runner failed before any test (e.g. a compile error).
        let nothing = rows.is_empty();
        let log_text = state.log.iter().rev().take(40).rev().cloned().collect::<Vec<_>>().join("\n");

        v_flex()
            .size_full()
            .min_h(px(0.))
            .child(toolbar)
            .child(
                v_flex()
                    .id("tests-list")
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .py(px(tokens::SPACE_2))
                    .children(rows)
                    .when(nothing && !log_text.is_empty(), {
                        let log_text = log_text.clone();
                        move |list| {
                        list.child(
                            div()
                                .mx(px(tokens::SPACE_4))
                                .font_family(mono.clone())
                                .text_size(px(tokens::UI_XS))
                                .text_color(muted)
                                .child(log_text),
                        )
                        }
                    })
                    .when(nothing && log_text.is_empty() && running, |list| {
                        list.child(
                            div()
                                .px(px(tokens::SPACE_4))
                                .text_size(px(tokens::UI_SM))
                                .text_color(muted)
                                .child("Starting the test runner\u{2026}"),
                        )
                    })
                    .when(nothing && log_text.is_empty() && !running && only_failures && counts.total() > 0, |list| {
                        list.child(
                            div()
                                .px(px(tokens::SPACE_4))
                                .text_size(px(tokens::UI_SM))
                                .text_color(green)
                                .child("No failures."),
                        )
                    }),
            )
    }
}

/// Which result wins when several tests share a line.
fn mark_rank(mark: editor_view::TestMark) -> u8 {
    match mark {
        editor_view::TestMark::Skipped => 0,
        editor_view::TestMark::Passed => 1,
        editor_view::TestMark::Running => 2,
        editor_view::TestMark::Failed => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::{package_dir_for, summary_text};
    use core_flutter::Counts;

    #[core::prelude::v1::test]
    fn the_summary_lists_only_what_happened() {
        assert_eq!(summary_text(Counts::default()), "No tests yet");
        assert_eq!(summary_text(Counts { passed: 12, failed: 2, skipped: 1, running: 0 }), "12 passed \u{b7} 2 failed \u{b7} 1 skipped");
        assert_eq!(summary_text(Counts { passed: 3, failed: 0, skipped: 0, running: 2 }), "3 passed \u{b7} 2 running");
    }

    #[core::prelude::v1::test]
    fn the_package_dir_is_the_nearest_pubspec_above_the_file() {
        let root = std::env::temp_dir().join(format!("test-runner-pkg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("app/test")).unwrap();
        std::fs::write(root.join("app/pubspec.yaml"), "name: app\n").unwrap();
        assert_eq!(package_dir_for(&root.join("app/test/a_test.dart"), &root), root.join("app"));
        // No pubspec anywhere above: fall back to the workspace root.
        let lone = root.join("elsewhere/b_test.dart");
        assert_eq!(package_dir_for(&lone, &root), root);
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// `lib/a/b.dart` ↔ `test/a/b_test.dart`, relative to the package. `None`
/// when the file is neither a library file nor a test.
pub(crate) fn test_counterpart(rel: &std::path::Path) -> Option<std::path::PathBuf> {
    let rel = rel.to_str()?;
    if let Some(rest) = rel.strip_prefix("test/") {
        let source = rest.strip_suffix("_test.dart")?;
        return Some(std::path::PathBuf::from(format!("lib/{source}.dart")));
    }
    let rest = rel.strip_prefix("lib/")?;
    let stem = rest.strip_suffix(".dart")?;
    Some(std::path::PathBuf::from(format!("test/{stem}_test.dart")))
}

impl DartIde {
    /// Jump between a Dart file and its test. A missing test is created from
    /// a small template first, so the shortcut always lands somewhere useful.
    pub(crate) fn go_to_test_or_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self
            .active_editor()
            .and_then(|ed| ed.read(cx).buffer().path().map(std::path::Path::to_path_buf))
        else {
            return;
        };
        let package_dir = package_dir_for(&path, &self.root);
        let Some(rel) = path.strip_prefix(&package_dir).ok().map(std::path::Path::to_path_buf) else {
            return;
        };
        let Some(target_rel) = test_counterpart(&rel) else {
            self.show_toast("Only files under lib/ or test/ have a test counterpart.", true, cx);
            return;
        };
        let target = package_dir.join(&target_rel);
        if !target.exists() {
            if target_rel.starts_with("lib") {
                self.show_toast(format!("No source file at {}", target_rel.display()), true, cx);
                return;
            }
            let package = std::fs::read_to_string(package_dir.join("pubspec.yaml"))
                .ok()
                .and_then(|t| {
                    t.lines()
                        .find_map(|l| l.strip_prefix("name:"))
                        .map(|n| n.trim().to_string())
                })
                .unwrap_or_else(|| "app".into());
            let flutter = std::fs::read_to_string(package_dir.join("pubspec.yaml"))
                .map(|text| core_flutter::is_flutter_package(&text))
                .unwrap_or(true);
            let import_rel = rel.strip_prefix("lib").unwrap_or(&rel).to_string_lossy().trim_start_matches('/').to_string();
            let name = rel.file_stem().and_then(|s| s.to_str()).unwrap_or("it");
            let test_pkg = if flutter { "flutter_test/flutter_test.dart" } else { "test/test.dart" };
            let body = format!(
                "import 'package:{test_pkg}';\nimport 'package:{package}/{import_rel}';\n\nvoid main() {{\n  group('{name}', () {{\n    test('works', () {{\n    }});\n  }});\n}}\n"
            );
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(err) = std::fs::write(&target, body) {
                self.show_toast(format!("Couldn't create {}: {err}", target_rel.display()), true, cx);
                return;
            }
            self.show_toast(format!("Created {}", target_rel.display()), false, cx);
        }
        self.open_path(target, window, cx);
    }
}

#[cfg(test)]
mod counterpart_tests {
    use super::test_counterpart;
    use std::path::{Path, PathBuf};

    #[core::prelude::v1::test]
    fn library_files_and_tests_point_at_each_other() {
        assert_eq!(
            test_counterpart(Path::new("lib/src/auth/login.dart")),
            Some(PathBuf::from("test/src/auth/login_test.dart"))
        );
        assert_eq!(
            test_counterpart(Path::new("test/src/auth/login_test.dart")),
            Some(PathBuf::from("lib/src/auth/login.dart"))
        );
        assert_eq!(test_counterpart(Path::new("bin/main.dart")), None);
        assert_eq!(test_counterpart(Path::new("test/helpers.dart")), None);
    }
}
