//! One app-wide notice, bottom-right above the status bar, for results that
//! must be seen whichever panel is on screen: a file move that failed, the
//! imports a rename updated. (The Git panel keeps its own notice line for
//! git operations, which happen with that panel open.)

use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, tokens};

/// How long a success stays up. Errors stay until dismissed or replaced.
const SUCCESS_SECS: u64 = 4;

pub(crate) struct Toast {
    pub(crate) text: String,
    pub(crate) is_error: bool,
    /// Which `show_toast` call this is, so an older timer can't hide a newer toast.
    seq: u64,
}

impl DartIde {
    /// Show `text` app-wide. A success hides itself after a few seconds; an
    /// error stays until dismissed or replaced by the next toast.
    pub(crate) fn show_toast(&mut self, text: impl Into<String>, is_error: bool, cx: &mut Context<Self>) {
        self.toast_seq += 1;
        let seq = self.toast_seq;
        self.toast = Some(Toast { text: text.into(), is_error, seq });
        if !is_error {
            cx.spawn(async move |weak, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(SUCCESS_SECS))
                    .await;
                let _ = weak.update(cx, |this, cx| {
                    if this.toast.as_ref().is_some_and(|t| t.seq == seq) {
                        this.toast = None;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    pub(crate) fn render_toast(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let toast = self.toast.as_ref()?;
        let accent = if toast.is_error { cx.theme().danger } else { cx.theme().green };
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        Some(
            div()
                .absolute()
                .right(px(tokens::SPACE_6))
                .bottom(px(tokens::STATUS_H + tokens::SPACE_5))
                .child(
                    h_flex()
                        .id("app-toast")
                        .max_w(px(420.))
                        .gap(px(tokens::SPACE_4))
                        .items_center()
                        .pl(px(tokens::SPACE_5))
                        .pr(px(tokens::SPACE_3))
                        .py(px(tokens::SPACE_4))
                        .rounded(px(tokens::RADIUS_PANEL))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(accent.opacity(0.45))
                        .shadow_lg()
                        .text_size(px(tokens::UI))
                        .text_color(fg)
                        .child(
                            Icon::new(if toast.is_error { IconName::CircleX } else { IconName::CircleCheck })
                                .with_size(px(14.))
                                .text_color(accent),
                        )
                        .child(div().flex_1().min_w(px(0.)).child(toast.text.clone()))
                        .child(
                            div()
                                .id("app-toast-dismiss")
                                .flex_shrink_0()
                                .p(px(tokens::SPACE_1))
                                .rounded(px(tokens::RADIUS_BADGE))
                                .cursor_pointer()
                                .text_color(muted)
                                .hover(move |s| s.bg(rgba(0xffffff10)).text_color(fg))
                                .child(Icon::new(gpui_kit::assets::IconName::X).with_size(px(11.)))
                                .on_click(cx.listener(|this, _e, _w, cx| {
                                    this.toast = None;
                                    cx.notify();
                                })),
                        )
                        .when(toast.is_error, |el| el.border_color(accent.opacity(0.6))),
                )
                .into_any_element(),
        )
    }
}
