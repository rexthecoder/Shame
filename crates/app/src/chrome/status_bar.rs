use std::path::Path;

use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, spinner::Spinner};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, tokens};

/// `lib › src › pane_controller.dart` — relative to the project root, matching
/// the reference status bar breadcrumb (`docs/design-spec.md` §3.32).
fn breadcrumb_segments(root: &Path, path: &Path) -> Vec<String> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(str::to_string))
        .collect()
}

/// The language a file is in, by extension, for the status bar.
fn language_label(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "dart" => "Dart",
        "rs" => "Rust",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" => "JavaScript",
        "json" | "jsonl" => "JSON",
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        "md" => "Markdown",
        "swift" => "Swift",
        "kt" | "kts" => "Kotlin",
        "java" => "Java",
        "gradle" => "Gradle",
        "xml" | "plist" => "XML",
        "html" => "HTML",
        "css" => "CSS",
        "sh" | "zsh" | "bash" => "Shell",
        "py" => "Python",
        "go" => "Go",
        "c" | "h" => "C",
        "cc" | "cpp" | "hpp" => "C++",
        "svg" => "SVG",
        _ => return None,
    })
}

/// Something long-running the IDE started, shown as a status bar pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Process {
    Flutter,
    Debugger,
    Tests,
    Agent,
}

/// Where a process stands: starting/working (spinner), live (green dot),
/// or stopped at a breakpoint (blue dot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Busy,
    Live,
    Paused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunPill {
    pub(crate) process: Process,
    pub(crate) phase: Phase,
    pub(crate) label: String,
}

/// What the IDE has running right now, in a fixed order, from plain facts
/// about each process (kept free of UI state so it can be tested).
pub(crate) struct RunFacts<'a> {
    pub(crate) flutter_running: bool,
    pub(crate) flutter_status: Option<&'a str>,
    pub(crate) device: Option<&'a str>,
    pub(crate) debugger: bool,
    pub(crate) debugger_paused: bool,
    pub(crate) tests_running: bool,
    pub(crate) tests_secs: u64,
    pub(crate) agent_running: bool,
}

pub(crate) fn running_pills(f: &RunFacts) -> Vec<RunPill> {
    let mut out = Vec::new();
    if f.flutter_running {
        let busy = crate::chrome::titlebar::flutter_session_busy(true, f.flutter_status);
        let what = if busy { f.flutter_status.unwrap_or("Starting") } else { "Running" };
        out.push(RunPill {
            process: Process::Flutter,
            phase: if busy { Phase::Busy } else { Phase::Live },
            label: match f.device {
                Some(device) => format!("{what} \u{b7} {device}"),
                None => what.to_string(),
            },
        });
    }
    if f.debugger {
        out.push(RunPill {
            process: Process::Debugger,
            phase: if f.debugger_paused { Phase::Paused } else { Phase::Live },
            label: if f.debugger_paused { "Paused".into() } else { "Debugging".into() },
        });
    }
    if f.tests_running {
        out.push(RunPill {
            process: Process::Tests,
            phase: Phase::Busy,
            label: format!("Tests \u{b7} {}s", f.tests_secs),
        });
    }
    if f.agent_running {
        out.push(RunPill { process: Process::Agent, phase: Phase::Busy, label: "Agent working".into() });
    }
    out
}

/// One clickable status bar item: same height, padding and hover everywhere.
fn status_item(id: &'static str, cx: &App) -> Stateful<Div> {
    let fg = cx.theme().foreground;
    h_flex()
        .id(id)
        .h(px(22.))
        .px(px(7.))
        .gap(px(5.))
        .items_center()
        .rounded(px(tokens::RADIUS_BADGE))
        .cursor_pointer()
        .hover(move |s| s.bg(rgba(0xffffff10)).text_color(fg))
}

impl DartIde {
    pub(crate) fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active_editor = self.active_editor().map(|ed| ed.read(cx));
        let active_path = active_editor.and_then(|ed| ed.buffer().path().map(Path::to_path_buf));
        let segments: Vec<String> = active_path
            .as_deref()
            .map(|path| breadcrumb_segments(&self.root, path))
            .unwrap_or_default();
        let language = active_path.as_deref().and_then(language_label);
        let is_dirty = active_editor.is_some_and(|ed| ed.buffer().is_dirty());
        let cursor_pos = active_editor.map(|ed| ed.cursor_position());
        let (errors, warnings) = active_editor
            .map(|ed| ed.diagnostic_summary())
            .unwrap_or((0, 0));
        // Mistakes in `keymap.json` (those entries are skipped, the rest work).
        let keymap_errors: Vec<String> = cx
            .try_global::<editor_view::keymap::Keymap>()
            .map(|k| k.errors.clone())
            .unwrap_or_default();
        let muted = cx.theme().muted_foreground;
        let view = cx.entity();
        // Language servers: say when they are starting or indexing (a big
        // Flutter project takes ~20 s), and keep repainting until they settle,
        // since the server's status changes arrive without a UI event.
        let lsp = editor_view::lsp_activity();
        let lsp_busy = matches!(lsp, editor_view::LspActivity::Starting | editor_view::LspActivity::Analyzing);
        if lsp_busy && !self.git.lsp_poll {
            cx.defer({
                let view = view.clone();
                move |cx| {
                    view.update(cx, |this, cx| {
                        if this.git.lsp_poll {
                            return;
                        }
                        this.git.lsp_poll = true;
                        cx.spawn(async move |weak, cx| {
                            cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
                            let _ = weak.update(cx, |this, cx| {
                                this.git.lsp_poll = false;
                                cx.notify();
                            });
                        })
                        .detach();
                    })
                }
            });
        }
        let lsp_item = match lsp {
            editor_view::LspActivity::Idle => None,
            editor_view::LspActivity::Starting => Some(("Starting language server\u{2026}", cx.theme().warning)),
            editor_view::LspActivity::Analyzing => Some(("Analyzing\u{2026}", cx.theme().warning)),
            editor_view::LspActivity::Ready => Some(("Language server", cx.theme().green)),
            editor_view::LspActivity::Failed => Some(("Language server stopped", cx.theme().danger)),
        };
        let lsp_failed = lsp == editor_view::LspActivity::Failed;

        let device = self.selected_device.as_ref().map(|d| d.name.clone());
        let pills = running_pills(&RunFacts {
            flutter_running: self.flutter_daemon.is_some(),
            flutter_status: self.flutter_status.as_deref(),
            device: device.as_deref(),
            debugger: self.dap_client.is_some(),
            debugger_paused: self.dap_paused_thread.is_some(),
            tests_running: self.test_run.as_ref().is_some_and(|t| t.pid.is_some()),
            tests_secs: self.test_run.as_ref().map_or(0, |t| t.started.elapsed().as_secs()),
            agent_running: self.agent.running_pid.is_some(),
        });
        let running = h_flex().gap(px(tokens::SPACE_1)).items_center().children(
            pills.into_iter().map(|pill| self.render_run_pill(pill, cx)),
        );

        let mut breadcrumb = status_item("status-breadcrumb", cx).on_click({
            let view = view.clone();
            move |_e, window, cx| {
                view.update(cx, |this, cx| this.open_symbol_outline(window, cx));
            }
        });
        for (ix, seg) in segments.iter().enumerate() {
            if ix > 0 {
                breadcrumb = breadcrumb.child(div().text_color(muted.opacity(0.55)).child("\u{203A}"));
            }
            breadcrumb = breadcrumb.child(seg.clone());
        }
        if is_dirty {
            breadcrumb = breadcrumb.child(
                div()
                    .size(px(6.))
                    .rounded(px(3.))
                    .bg(cx.theme().primary),
            );
        }

        // Errors and warnings always show, muted at zero, so the counts sit
        // in one place instead of appearing and vanishing.
        let problems = status_item("status-problems", cx)
            .gap(px(8.))
            .child(
                h_flex()
                    .gap(px(4.))
                    .items_center()
                    .when(errors > 0, |el| el.text_color(cx.theme().danger))
                    .child(Icon::new(IconName::CircleX).with_size(px(13.)))
                    .child(errors.to_string()),
            )
            .child(
                h_flex()
                    .gap(px(4.))
                    .items_center()
                    .when(warnings > 0, |el| el.text_color(cx.theme().warning))
                    .child(Icon::new(IconName::TriangleAlert).with_size(px(13.)))
                    .child(warnings.to_string()),
            )
            .tooltip(|window, cx| {
                gpui_kit::component::tooltip::Tooltip::new("Problems").build(window, cx)
            })
            .on_click({
                let view = view.clone();
                move |_e, _w, cx| {
                    view.update(cx, |this, cx| {
                        this.terminal_open = true;
                        this.dock_tab = crate::DockTab::Problems;
                        cx.notify();
                    });
                }
            });

        h_flex()
            .w_full()
            .h(px(tokens::STATUS_H))
            .flex_shrink_0()
            .px(px(tokens::SPACE_4))
            .items_center()
            .justify_between()
            .bg(cx.theme().status_bar)
            .border_t_1()
            .border_color(cx.theme().status_bar_border)
            .text_size(px(tokens::UI_SM))
            .text_color(muted)
            .child(
                h_flex()
                    .min_w(px(0.))
                    .gap(px(tokens::SPACE_2))
                    .items_center()
                    .child(problems)
                    .child(running)
                    .when(!segments.is_empty(), |el| el.child(breadcrumb)),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap(px(tokens::SPACE_2))
                    .items_center()
                    .when(!keymap_errors.is_empty(), |el| {
                        let tooltip = keymap_errors.join("\n");
                        let view = view.clone();
                        el.child(
                            status_item("status-keymap-errors", cx)
                                .text_color(cx.theme().warning)
                                .child(Icon::new(IconName::TriangleAlert).with_size(px(13.)))
                                .child("Keymap")
                                .tooltip(move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(tooltip.clone())
                                        .build(window, cx)
                                })
                                .on_click(move |_e, window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.execute_ide_command("open-keymap", window, cx)
                                    });
                                }),
                        )
                    })
                    .when_some(lsp_item, |el, (text, tone)| {
                        el.child(
                            h_flex()
                                .id("status-lsp")
                                .px(px(7.))
                                .gap(px(5.))
                                .items_center()
                                .child(div().size(px(6.)).rounded(px(3.)).bg(tone))
                                .when(lsp_busy || lsp_failed, |el| el.text_color(cx.theme().foreground).child(text))
                                .tooltip(move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(if lsp_failed {
                                        "The language server exited. Settings lists what is installed."
                                    } else if lsp_busy {
                                        text
                                    } else {
                                        "Language server ready"
                                    })
                                    .build(window, cx)
                                })
                                .when(lsp_failed, |el| {
                                    let view = view.clone();
                                    el.cursor_pointer().on_click(move |_e, _w, cx| {
                                        view.update(cx, |this, cx| this.toggle_settings(cx));
                                    })
                                }),
                        )
                    })
                    .when_some(cursor_pos, |el, (line, column)| {
                        let view = view.clone();
                        el.child(
                            status_item("status-goto-line", cx)
                                .child(format!("Ln {line}, Col {column}"))
                                .on_click(move |_event, window, cx| {
                                    view.update(cx, |this, cx| this.open_go_to_line(window, cx));
                                }),
                        )
                        .child(div().px(px(7.)).child("UTF-8"))
                        .child(div().px(px(7.)).child("LF"))
                    })
                    .when_some(language, |el, language| {
                        el.child(div().px(px(7.)).text_color(cx.theme().foreground).child(language))
                    }),
            )
    }
}

impl DartIde {
    /// A running process: its state, what it's doing, and a stop button.
    /// Clicking the pill opens where its output lives.
    fn render_run_pill(&self, pill: RunPill, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        let (id, stop_id, tooltip): (&'static str, &'static str, &'static str) = match pill.process {
            Process::Flutter => ("status-run-flutter", "status-stop-flutter", "Stop app"),
            Process::Debugger => ("status-run-debug", "status-stop-debug", "Stop debugging"),
            Process::Tests => ("status-run-tests", "status-stop-tests", "Stop tests"),
            Process::Agent => ("status-run-agent", "status-stop-agent", "Stop agent"),
        };
        let indicator: AnyElement = match pill.phase {
            Phase::Busy => Spinner::new().xsmall().color(cx.theme().warning).into_any_element(),
            Phase::Live | Phase::Paused => {
                let color = if pill.phase == Phase::Live { cx.theme().green } else { cx.theme().info };
                div()
                    .size(px(10.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .bg(color.opacity(0.25))
                    .child(div().size(px(6.)).rounded(px(3.)).bg(color))
                    .into_any_element()
            }
        };
        let process = pill.process;
        let stop_view = view.clone();
        status_item(id, cx)
            .text_color(cx.theme().foreground)
            .child(indicator)
            .child(pill.label)
            .child(
                div()
                    .id(stop_id)
                    .ml(px(2.))
                    .p(px(2.))
                    .rounded(px(tokens::RADIUS_BADGE))
                    .text_color(cx.theme().muted_foreground)
                    .hover(|s| s.bg(rgba(0xffffff14)).text_color(cx.theme().danger))
                    .child(Icon::new(IconName::Square).with_size(px(9.)))
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tooltip).build(window, cx)
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_e, _w, cx| {
                        cx.stop_propagation();
                        stop_view.update(cx, |this, cx| match process {
                            Process::Flutter => this.stop_flutter_run(cx),
                            Process::Debugger => this.stop_dap_debug(cx),
                            Process::Tests => this.stop_test_run(cx),
                            Process::Agent => this.stop_agent_run(cx),
                        });
                    }),
            )
            .on_click(move |_e, _w, cx| {
                view.update(cx, |this, cx| {
                    match process {
                        Process::Flutter | Process::Debugger => {
                            this.terminal_open = true;
                            this.dock_tab = crate::DockTab::DebugConsole;
                        }
                        Process::Tests => {
                            this.terminal_open = true;
                            this.dock_tab = crate::DockTab::Tests;
                        }
                        Process::Agent => this.set_agent_open(true),
                    }
                    cx.notify();
                });
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{Phase, Process, RunFacts, running_pills};
    use super::language_label;
    use std::path::Path;

    #[core::prelude::v1::test]
    fn files_are_named_by_their_language() {
        assert_eq!(language_label(Path::new("lib/main.dart")), Some("Dart"));
        assert_eq!(language_label(Path::new("Cargo.TOML")), Some("TOML"));
        assert_eq!(language_label(Path::new("Makefile")), None);
    }

    fn idle() -> RunFacts<'static> {
        RunFacts {
            flutter_running: false,
            flutter_status: None,
            device: None,
            debugger: false,
            debugger_paused: false,
            tests_running: false,
            tests_secs: 0,
            agent_running: false,
        }
    }

    #[core::prelude::v1::test]
    fn nothing_running_shows_nothing() {
        assert!(running_pills(&idle()).is_empty());
    }

    #[core::prelude::v1::test]
    fn flutter_is_busy_until_the_cli_says_running() {
        let building = running_pills(&RunFacts {
            flutter_running: true,
            flutter_status: Some("Building"),
            device: Some("iPhone 16"),
            ..idle()
        });
        assert_eq!(building[0].phase, Phase::Busy);
        assert_eq!(building[0].label, "Building \u{b7} iPhone 16");
        let live = running_pills(&RunFacts {
            flutter_running: true,
            flutter_status: Some("Running"),
            device: Some("iPhone 16"),
            ..idle()
        });
        assert_eq!(live[0].phase, Phase::Live);
        assert_eq!(live[0].label, "Running \u{b7} iPhone 16");
    }

    #[core::prelude::v1::test]
    fn every_running_process_gets_a_pill_in_order() {
        let pills = running_pills(&RunFacts {
            flutter_running: true,
            flutter_status: Some("Running"),
            debugger: true,
            debugger_paused: true,
            tests_running: true,
            tests_secs: 7,
            agent_running: true,
            ..idle()
        });
        let order: Vec<Process> = pills.iter().map(|p| p.process).collect();
        assert_eq!(order, [Process::Flutter, Process::Debugger, Process::Tests, Process::Agent]);
        assert_eq!(pills[1].phase, Phase::Paused);
        assert_eq!(pills[2].label, "Tests \u{b7} 7s");
    }
}
