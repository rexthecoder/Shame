//! The live web preview panel: the system's own WebKit view (via `wry`),
//! embedded in the window under the panel's toolbar. It is a real browser
//! view — native scrolling, typing, selection, video — not a picture of one.
//!
//! The view is a native macOS view laid over GPUI's drawing, so two things
//! need care: its bounds follow the panel on every paint, and it is hidden
//! whenever the panel isn't drawn or a GPUI overlay (the command palette,
//! settings) needs to show over it.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::process::Command;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::Input,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, tokens};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WebViewport {
    Desktop,
    Mobile,
}

impl WebViewport {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Desktop => "Desktop",
            Self::Mobile => "Mobile",
        }
    }
}

/// Width of the page in Mobile mode, in points.
const PHONE_WIDTH: f32 = 390.0;

/// What the web view reports back to the panel.
pub(crate) enum WebEvent {
    Title(String),
    /// Loading started (`true`) or finished, and the URL it's at.
    Load(bool, String),
}

/// Everything the preview holds while open. The `Rc`s are shared with the
/// panel's paint callback, which is where the native view is created,
/// placed, shown and hidden.
#[derive(Default)]
pub(crate) struct WebPreviewState {
    pub view: Rc<RefCell<Option<wry::WebView>>>,
    /// Where to go once the view exists.
    pub pending_url: Rc<RefCell<Option<String>>>,
    pub events: Option<async_channel::Sender<WebEvent>>,
    pub title: String,
    pub loading: bool,
    pub error: Option<String>,
    /// Set by each paint of the panel; a frame without one hides the view.
    pub painted: Rc<Cell<bool>>,
    pub shown: Rc<Cell<bool>>,
    pub bounds: Rc<Cell<(f32, f32, f32, f32)>>,
}

impl DartIde {
    pub(crate) fn toggle_web_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.web_preview_open = !self.web_preview_open;
        if self.web_preview_open {
            // Previewing an HTML file that's open: show that file.
            if let Some(html_path) = self
                .active_editor()
                .and_then(|ed| ed.read(cx).buffer().path().map(Path::to_path_buf))
                .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("html" | "htm")))
            {
                let file_url = format!("file://{}", html_path.display());
                self.web_preview_url.update(cx, |s, cx| s.set_value(file_url, window, cx));
            }
            self.web_preview_go(window, cx);
        } else {
            self.stop_web_preview(cx);
        }
        cx.notify();
    }

    /// Remove the web view (dropping it takes it out of the window).
    pub(crate) fn stop_web_preview(&mut self, cx: &mut Context<Self>) {
        self.web.view.borrow_mut().take();
        self.web = WebPreviewState::default();
        cx.notify();
    }

    /// Called every frame from the window's render: if this frame doesn't
    /// paint the panel (it was closed, hidden, or covered by an overlay),
    /// hide the native view before it shows through.
    pub(crate) fn web_preview_frame_check(&mut self, window: &mut Window) {
        if self.web.view.borrow().is_none() {
            return;
        }
        self.web.painted.set(false);
        let (view, painted, shown) =
            (self.web.view.clone(), self.web.painted.clone(), self.web.shown.clone());
        window.on_next_frame(move |_, _| {
            if !painted.get() && shown.get() {
                if let Some(view) = view.borrow().as_ref() {
                    let _ = view.set_visible(false);
                }
                shown.set(false);
            }
        });
    }

    /// Start listening to the web view's title and load events.
    fn ensure_web_events(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.web.events.is_some() {
            return;
        }
        let (tx, rx) = async_channel::unbounded::<WebEvent>();
        self.web.events = Some(tx);
        cx.spawn_in(window, async move |weak, cx| {
            while let Ok(event) = rx.recv().await {
                let alive = weak.update_in(cx, |this, window, cx| {
                    match event {
                        WebEvent::Title(title) => this.web.title = title,
                        WebEvent::Load(loading, url) => {
                            this.web.loading = loading;
                            this.web.error = None;
                            // Don't fight someone typing in the URL bar.
                            if !this.web_preview_url.read(cx).focus_handle(cx).is_focused(window)
                                && !url.is_empty()
                                && url != "about:blank"
                            {
                                this.web_preview_url.update(cx, |s, cx| s.set_value(url, window, cx));
                            }
                        }
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Load what's in the URL bar.
    pub(crate) fn web_preview_go(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.web_preview_url.read(cx).value().trim().to_string();
        if raw.is_empty() {
            return;
        }
        let url = crate::browser::normalize_url(&raw, &self.root);
        self.ensure_web_events(window, cx);
        self.web.error = None;
        match self.web.view.borrow().as_ref() {
            Some(view) => {
                if let Err(err) = view.load_url(&url) {
                    self.web.error = Some(format!("Couldn't load {url}: {err}"));
                }
            }
            // The panel's next paint creates the view and loads this.
            None => *self.web.pending_url.borrow_mut() = Some(url),
        }
        cx.notify();
    }

    pub(crate) fn web_preview_reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reloaded = self.web.view.borrow().as_ref().map(|view| view.reload().is_ok());
        if reloaded != Some(true) {
            self.web_preview_go(window, cx);
        }
    }

    fn web_preview_script(&self, script: &str) {
        if let Some(view) = self.web.view.borrow().as_ref() {
            let _ = view.evaluate_script(script);
        }
    }

    /// After a save: reload a previewed local file (dev servers reload
    /// themselves).
    pub(crate) fn web_preview_after_save(&mut self, cx: &mut Context<Self>) {
        let url = self.web_preview_url.read(cx).value().to_string();
        if url.starts_with("file://")
            && let Some(view) = self.web.view.borrow().as_ref()
        {
            let _ = view.reload();
        }
    }

    pub(crate) fn render_web_preview_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let list_hover = cx.theme().list_hover;
        let viewport = self.web_preview_viewport;
        let mobile = viewport == WebViewport::Mobile;
        let title = if self.web.title.is_empty() || self.web.title.starts_with("about:") {
            "Preview".to_string()
        } else {
            self.web.title.clone()
        };

        let nav_button = |id: &'static str, icon: IconName, tip: &'static str| {
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(Icon::new(icon).with_size(px(13.)).text_color(muted))
                .tooltip(tip)
        };

        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .border_1()
            .border_color(border)
            .rounded(px(tokens::RADIUS_CARD))
            .overflow_hidden()
            // Title bar: page title, viewport, inspect, open externally, close.
            .child(
                h_flex()
                    .w_full()
                    .h(px(tokens::TABSTRIP_H))
                    .flex_shrink_0()
                    .px(px(tokens::SPACE_4))
                    .gap(px(tokens::SPACE_2))
                    .items_center()
                    .border_b_1()
                    .border_color(border)
                    .child(Icon::new(IconName::Globe).with_size(px(13.)).text_color(muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(tokens::UI_SM))
                            .font_medium()
                            .text_color(fg)
                            .child(title),
                    )
                    .child(
                        h_flex()
                            .id("web-viewport-toggle")
                            .h(px(22.))
                            .px(px(7.))
                            .items_center()
                            .gap(px(5.))
                            .rounded(px(tokens::RADIUS_SM))
                            .cursor_pointer()
                            .hover(move |s| s.bg(list_hover))
                            .text_size(px(tokens::UI_XS))
                            .text_color(muted)
                            .child(
                                Icon::new(if mobile { IconName::Smartphone } else { IconName::Monitor })
                                    .with_size(px(12.)),
                            )
                            .child(viewport.label())
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.web_preview_viewport = match this.web_preview_viewport {
                                    WebViewport::Desktop => WebViewport::Mobile,
                                    WebViewport::Mobile => WebViewport::Desktop,
                                };
                                cx.notify();
                            })),
                    )
                    .child(
                        nav_button("web-inspect", IconName::SquareTerminal, "Inspect (Web Inspector)")
                            .on_click(cx.listener(|this, _e, _w, _cx| {
                                if let Some(view) = this.web.view.borrow().as_ref() {
                                    view.open_devtools();
                                }
                            })),
                    )
                    .child(
                        nav_button("web-open-external", IconName::ExternalLink, "Open in your browser")
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                let raw = this.web_preview_url.read(cx).value().to_string();
                                let url = crate::browser::normalize_url(&raw, &this.root);
                                let _ = Command::new("open").arg(&url).spawn();
                            })),
                    )
                    .child(
                        Button::new("close-web-preview")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::X).xsmall().text_color(muted))
                            .tooltip("Close preview")
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.web_preview_open = false;
                                this.stop_web_preview(cx);
                            })),
                    ),
            )
            // Navigation: back, forward, reload, address.
            .child(
                h_flex()
                    .w_full()
                    .h(px(tokens::PANEL_BAR_H + 4.))
                    .flex_shrink_0()
                    .px(px(tokens::SPACE_3))
                    .gap(px(2.))
                    .items_center()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        nav_button("web-back", IconName::ArrowLeft, "Back")
                            .on_click(cx.listener(|this, _e, _w, _cx| this.web_preview_script("history.back()"))),
                    )
                    .child(
                        nav_button("web-forward", IconName::ArrowRight, "Forward").on_click(
                            cx.listener(|this, _e, _w, _cx| this.web_preview_script("history.forward()")),
                        ),
                    )
                    .child(
                        nav_button("web-reload", IconName::RotateCw, "Reload")
                            .on_click(cx.listener(|this, _e, window, cx| this.web_preview_reload(window, cx))),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w(px(0.))
                            .h(px(26.))
                            .ml(px(4.))
                            .px(px(tokens::SPACE_3))
                            .items_center()
                            .rounded(px(tokens::RADIUS_PILL))
                            .bg(cx.theme().background)
                            .border_1()
                            .border_color(border)
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                                if event.keystroke.key.as_str() == "enter" {
                                    this.web_preview_go(window, cx);
                                    cx.stop_propagation();
                                }
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .child(Input::new(&self.web_preview_url).appearance(false).xsmall()),
                            ),
                    ),
            )
            // A thin bar while the page loads.
            .child(
                div()
                    .w_full()
                    .h(px(2.))
                    .flex_shrink_0()
                    .when(self.web.loading, |el| el.bg(cx.theme().primary.opacity(0.7))),
            )
            .child(self.render_web_page(cx))
    }

    /// The area the native web view is placed over, plus a message when
    /// there's nothing loaded.
    fn render_web_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let mobile = self.web_preview_viewport == WebViewport::Mobile;
        // GPUI overlays can't draw over a native view; hide it under them.
        let covered = self.palette_open || self.settings_open || self.about_open;
        let state = (
            self.web.view.clone(),
            self.web.pending_url.clone(),
            self.web.events.clone(),
            self.web.painted.clone(),
            self.web.shown.clone(),
            self.web.bounds.clone(),
        );
        let has_view = self.web.view.borrow().is_some() || self.web.pending_url.borrow().is_some();

        div()
            .id("web-page")
            .relative()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .overflow_hidden()
            .bg(cx.theme().background)
            .when(!has_view || self.web.error.is_some(), |el| {
                el.child(
                    v_flex()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .gap(px(tokens::SPACE_3))
                        .p(px(tokens::SPACE_6))
                        .child(
                            Icon::new(if self.web.error.is_some() { IconName::TriangleAlert } else { IconName::Globe })
                                .with_size(px(20.))
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(
                            div()
                                .text_center()
                                .text_size(px(tokens::UI_SM))
                                .text_color(cx.theme().foreground)
                                .child(self.web.error.clone().unwrap_or_else(|| {
                                    "Enter a URL, a :port, or a project HTML file.".to_string()
                                })),
                        )
                        .child(h_flex().gap(px(6.)).children(
                            [":3000", ":5173", ":8080"].into_iter().enumerate().map(|(ix, port)| {
                                div()
                                    .id(("web-port-chip", ix))
                                    .px(px(8.))
                                    .py(px(3.))
                                    .rounded(px(tokens::RADIUS_PILL))
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .cursor_pointer()
                                    .hover(|s| s.bg(cx.theme().list_hover))
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(port)
                                    .on_click(cx.listener(move |this, _e, window, cx| {
                                        this.web_preview_url.update(cx, |s, cx| s.set_value(port, window, cx));
                                        this.web_preview_go(window, cx);
                                    }))
                            }),
                        )),
                )
            })
            .child(
                canvas(
                    move |bounds, _, _| bounds,
                    move |bounds, _, window, _cx| {
                        let (view, pending, events, painted, shown, last_bounds) = &state;
                        painted.set(true);
                        // Desktop fills the area; Mobile is a phone-width
                        // column in the middle.
                        let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
                        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                        let width = if mobile { w.min(PHONE_WIDTH) } else { w };
                        let rect = (x + (w - width) / 2.0, y, width, h);
                        let wry_rect = wry::Rect {
                            position: wry::dpi::LogicalPosition::new(rect.0 as f64, rect.1 as f64).into(),
                            size: wry::dpi::LogicalSize::new(rect.2 as f64, rect.3 as f64).into(),
                        };

                        if view.borrow().is_none() {
                            let Some(url) = pending.borrow_mut().take() else { return };
                            let (title_tx, load_tx) = (events.clone(), events.clone());
                            let built = wry::WebViewBuilder::new()
                                .with_url(url)
                                .with_bounds(wry_rect)
                                .with_devtools(true)
                                .with_back_forward_navigation_gestures(true)
                                .with_document_title_changed_handler(move |title| {
                                    if let Some(tx) = &title_tx {
                                        let _ = tx.try_send(WebEvent::Title(title));
                                    }
                                })
                                .with_on_page_load_handler(move |event, url| {
                                    if let Some(tx) = &load_tx {
                                        let started = matches!(event, wry::PageLoadEvent::Started);
                                        let _ = tx.try_send(WebEvent::Load(started, url));
                                    }
                                })
                                .build_as_child(&*window);
                            match built {
                                Ok(created) => {
                                    *view.borrow_mut() = Some(created);
                                    shown.set(true);
                                    last_bounds.set(rect);
                                }
                                Err(err) => {
                                    if let Some(tx) = events {
                                        let _ = tx.try_send(WebEvent::Title(format!("Preview failed: {err}")));
                                    }
                                }
                            }
                            return;
                        }

                        let guard = view.borrow();
                        let Some(view) = guard.as_ref() else { return };
                        if covered {
                            if shown.replace(false) {
                                let _ = view.set_visible(false);
                            }
                            return;
                        }
                        if last_bounds.get() != rect {
                            last_bounds.set(rect);
                            let _ = view.set_bounds(wry_rect);
                        }
                        if !shown.replace(true) {
                            let _ = view.set_visible(true);
                        }
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .into_any_element()
    }
}
