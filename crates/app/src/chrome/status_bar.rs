use std::path::Path;

use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, tokens};

/// `lib › src › pane_controller.dart` — relative to the project root, matching
/// the reference status bar breadcrumb (`docs/design-spec.md` §3.32).
fn breadcrumb_segments(root: &Path, path: &Path) -> Vec<String> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let segments: Vec<String> = relative
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(str::to_string))
        .collect();
    if segments.is_empty() {
        vec!["untitled".to_string()]
    } else {
        segments
    }
}

impl DartIde {
    pub(crate) fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = self.active_editor().read(cx);
        let buffer = editor.buffer();
        let segments = match buffer.path() {
            Some(path) => breadcrumb_segments(&self.root, &path),
            None => vec!["untitled".to_string()],
        };
        let is_dirty = buffer.is_dirty();
        let (line, column) = editor.cursor_position();
        let (errors, warnings) = editor.diagnostic_summary();
        let total_issues = errors + warnings;
        let muted = cx.theme().muted_foreground;

        let mut breadcrumb = h_flex().items_center().gap(px(tokens::SPACE_2));
        for (ix, seg) in segments.iter().enumerate() {
            if ix > 0 {
                breadcrumb = breadcrumb.child(
                    div()
                        .text_color(muted.opacity(0.65))
                        .child("\u{203A}"),
                );
            }
            breadcrumb = breadcrumb.child(div().text_color(muted).child(seg.clone()));
        }

        h_flex()
            .w_full()
            .h(px(tokens::STATUS_H))
            .flex_shrink_0()
            .px_3()
            .items_center()
            .justify_between()
            .bg(cx.theme().status_bar)
            .border_t_1()
            .border_color(cx.theme().status_bar_border)
            .text_size(px(tokens::UI_SM))
            .text_color(muted)
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .when(is_dirty, |el| {
                        el.child(div().text_color(cx.theme().primary).child("\u{25cf}"))
                    })
                    .child(breadcrumb)
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(
                                Icon::new(gpui_kit::assets::IconName::RotateCw)
                                    .xsmall()
                                    .text_color(muted),
                            )
                            .child(total_issues.to_string()),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(
                                Icon::new(gpui_kit::assets::IconName::Clock)
                                    .xsmall()
                                    .text_color(muted),
                            )
                            .child("WakaTime"),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .when(errors > 0, |el| {
                        el.child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .text_color(cx.theme().danger)
                                .child(Icon::new(IconName::CircleX).small())
                                .child(errors.to_string()),
                        )
                    })
                    .when(warnings > 0, |el| {
                        el.child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .text_color(cx.theme().warning)
                                .child(Icon::new(IconName::TriangleAlert).small())
                                .child(warnings.to_string()),
                        )
                    })
                    .child(
                        Icon::new(gpui_kit::assets::IconName::Bot)
                            .xsmall()
                            .text_color(muted),
                    )
                    .child(
                        Icon::new(gpui_kit::assets::IconName::Radio)
                            .xsmall()
                            .text_color(muted),
                    )
                    .child(
                        Icon::new(gpui_kit::assets::IconName::Feather)
                            .xsmall()
                            .text_color(muted),
                    )
                    .child(format!("{line}:{column}"))
                    .child("LF")
                    .child("UTF-8"),
            )
    }
}

