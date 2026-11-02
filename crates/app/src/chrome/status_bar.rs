use std::path::Path;

use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::git_panel::SidebarTab;
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
        let branch = self.git.status.as_ref().map(|status| {
            let name = status
                .branch
                .clone()
                .or_else(|| status.head.as_ref().map(|h| format!("detached @ {h}")))
                .unwrap_or_else(|| "no commits".to_string());
            (name, status.ahead, status.behind)
        });
        let muted = cx.theme().muted_foreground;
        let view = cx.entity();

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
                    .when_some(branch, |el, (name, ahead, behind)| {
                        let view = view.clone();
                        el.child(
                            status_item("status-branch", cx)
                                .child(
                                    Icon::new(gpui_kit::assets::IconName::GitBranch)
                                        .with_size(px(13.)),
                                )
                                .child(name)
                                .when(behind > 0, |el| el.child(format!("\u{2193}{behind}")))
                                .when(ahead > 0, |el| el.child(format!("\u{2191}{ahead}")))
                                .on_click(move |_e, _w, cx| {
                                    view.update(cx, |this, cx| {
                                        this.set_sidebar_tab(SidebarTab::Git, cx)
                                    });
                                }),
                        )
                    })
                    .child(problems)
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

#[cfg(test)]
mod tests {
    use super::language_label;
    use std::path::Path;

    #[core::prelude::v1::test]
    fn files_are_named_by_their_language() {
        assert_eq!(language_label(Path::new("lib/main.dart")), Some("Dart"));
        assert_eq!(language_label(Path::new("Cargo.TOML")), Some("TOML"));
        assert_eq!(language_label(Path::new("Makefile")), None);
    }
}
