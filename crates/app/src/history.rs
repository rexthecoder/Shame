//! Local History: restore an earlier saved version of the active file.
//! Snapshots are taken on every save (see `EditorEvent::Saved` handling).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use core_workspace::Snapshot;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, EditorView, tokens};

/// How many snapshots are kept per file.
pub(crate) const KEEP_SNAPSHOTS: usize = 50;

/// Where snapshots live: `~/.shame/history`.
pub(crate) fn history_base() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".shame").join("history"))
}

/// "just now", "5m ago", "3h ago", "2d ago".
pub(crate) fn time_ago(taken_at_ms: u128, now_ms: u128) -> String {
    let secs = now_ms.saturating_sub(taken_at_ms) / 1000;
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

impl DartIde {
    /// Record a snapshot of `path` off the UI thread.
    pub(crate) fn record_history_snapshot(&self, path: PathBuf) {
        let Some(base) = history_base() else { return };
        std::thread::spawn(move || {
            let _ = core_workspace::record_snapshot(&base, &path, KEEP_SNAPSHOTS);
        });
    }

    pub(crate) fn open_local_history(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor().cloned() else { return };
        let Some(path) = editor.read(cx).buffer().path().map(|p| p.to_path_buf()) else { return };
        let Some(base) = history_base() else { return };
        let snapshots = core_workspace::list_snapshots(&base, &path);
        self.history = Some((editor, snapshots));
        cx.notify();
    }

    pub(crate) fn render_local_history(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> Option<Div> {
        let (_, snapshots) = self.history.as_ref()?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);

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
                                this.history = None;
                                cx.notify();
                            });
                        }
                    }
                })
                .child(
                    v_flex()
                        .w(px(420.))
                        .max_h(px(420.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .child(
                            h_flex()
                                .px_3()
                                .py_2()
                                .justify_between()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Local history \u{2014} pick a version to restore")
                                .child(
                                    div()
                                        .id("history-close")
                                        .cursor_pointer()
                                        .child("Close")
                                        .on_click({
                                            let view = view.clone();
                                            move |_event, _window, cx| {
                                                view.update(cx, |this, cx| {
                                                    this.history = None;
                                                    cx.notify();
                                                });
                                            }
                                        }),
                                ),
                        )
                        .when(snapshots.is_empty(), |el| {
                            el.child(
                                div()
                                    .px_3()
                                    .pb_3()
                                    .text_sm()
                                    .child("No saved versions yet. Save the file to start a history."),
                            )
                        })
                        .child(
                            v_flex()
                                .id("history-list")
                                .overflow_y_scroll()
                                .children(snapshots.iter().enumerate().map(|(ix, snap)| {
                                    div()
                                        .id(("history-item", ix))
                                        .px_3()
                                        .py_2()
                                        .text_sm()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(cx.theme().list_hover))
                                        .child(time_ago(snap.taken_at_ms, now))
                                        .on_click({
                                            let view = view.clone();
                                            move |_event, _window, cx| {
                                                view.update(cx, |this, cx| this.restore_snapshot(ix, cx));
                                            }
                                        })
                                })),
                        ),
                ),
        )
    }

    fn restore_snapshot(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some((editor, snapshots)) = self.history.take() else { return };
        let Some(Snapshot { path, .. }) = snapshots.get(index) else { return };
        if let Ok(text) = std::fs::read_to_string(path) {
            // One undoable edit, so ⌘Z brings the current version back.
            editor.update(cx, |editor: &mut EditorView, cx| editor.replace_all_text(&text, cx));
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::time_ago;

    #[test]
    fn formats_relative_times() {
        assert_eq!(time_ago(1_000, 1_000), "just now");
        assert_eq!(time_ago(0, 5 * 60_000), "5m ago");
        assert_eq!(time_ago(0, 3 * 3_600_000), "3h ago");
        assert_eq!(time_ago(0, 2 * 86_400_000), "2d ago");
    }
}
