//! The "the agent is working" indicator: a spinner glyph, a shimmering label
//! that says what it is doing ("Reading main.dart…"), the time so far, and how
//! to stop it. It is its own view with its own redraw loop, so animating it
//! never redraws the rest of the IDE.

use std::time::{Duration, Instant};

use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::*;

use crate::agent_ctx::{format_elapsed, shimmer_alpha, spinner_glyph};
use crate::tokens;

/// How often the indicator redraws while active.
const FRAME: Duration = Duration::from_millis(90);

pub(crate) struct ThinkingIndicator {
    started: Instant,
    label: String,
    active: bool,
    _ticker: Task<()>,
}

impl ThinkingIndicator {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let ticker = cx.spawn(async move |weak, cx| {
            loop {
                cx.background_executor().timer(FRAME).await;
                let alive = weak
                    .update(cx, |this, cx| {
                        if this.active {
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        });
        Self { started: Instant::now(), label: "Thinking".to_string(), active: false, _ticker: ticker }
    }

    /// A new run begins: restart the clock.
    pub(crate) fn start(&mut self, label: &str, cx: &mut Context<Self>) {
        self.started = Instant::now();
        self.label = label.to_string();
        self.active = true;
        cx.notify();
    }

    /// What the agent is doing now ("Editing main.dart"); the clock keeps running.
    pub(crate) fn set_label(&mut self, label: &str, cx: &mut Context<Self>) {
        if self.label != label {
            self.label = label.to_string();
            cx.notify();
        }
    }

    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        self.active = false;
        cx.notify();
    }
}

impl Render for ThinkingIndicator {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let primary = cx.theme().primary;
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let elapsed_ms = self.started.elapsed().as_millis() as u64;
        let label = format!("{}\u{2026}", self.label);

        h_flex()
            .w_full()
            .h(px(24.))
            .px(px(tokens::SPACE_2))
            .gap(px(8.))
            .items_center()
            .text_size(px(tokens::UI_SM))
            .child(
                div()
                    .w(px(14.))
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .text_color(primary)
                    .child(spinner_glyph(elapsed_ms)),
            )
            // One character at a time, so a brightness wave can travel along the label.
            .child(
                h_flex().children(label.chars().enumerate().map(|(ix, ch)| {
                    div()
                        .text_color(fg.opacity(shimmer_alpha(ix, elapsed_ms)))
                        .child(if ch == ' ' { "\u{a0}".to_string() } else { ch.to_string() })
                })),
            )
            .child(
                div()
                    .text_size(px(tokens::UI_XS))
                    .text_color(muted)
                    .child(format!("({} \u{b7} esc to stop)", format_elapsed(elapsed_ms))),
            )
    }
}
