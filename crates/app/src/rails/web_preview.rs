use std::path::Path;
use std::process::Command;
use std::sync::Arc;

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

use crate::{DartIde, ImagePreview, load_image_preview, tokens};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WebViewport {
    Desktop,
    Mobile,
}

impl WebViewport {
    pub(crate) fn dimensions(self) -> (u32, u32) {
        match self {
            Self::Desktop => (1280, 860),
            Self::Mobile => (390, 844),
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Desktop => "Desktop",
            Self::Mobile => "Mobile",
        }
    }
}

fn find_chrome_binary() -> Option<&'static str> {
    const CANDIDATES: &[&str] = &[
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Vivaldi.app/Contents/MacOS/Vivaldi",
    ];
    CANDIDATES.iter().copied().find(|p| Path::new(p).is_file())
}

impl DartIde {
    pub(crate) fn toggle_web_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.web_preview_open = !self.web_preview_open;
        if self.web_preview_open {
            // If an HTML file is active and URL is still the default, preview that HTML file
            if let Some(html_path) = self
                .active_editor()
                .and_then(|ed| ed.read(cx).buffer().path().map(Path::to_path_buf))
                .filter(|p| {
                    matches!(
                        p.extension().and_then(|e| e.to_str()),
                        Some("html" | "htm")
                    )
                })
            {
                let file_url = format!("file://{}", html_path.display());
                self.web_preview_url
                    .update(cx, |s, cx| s.set_value(file_url, window, cx));
            }
            self.refresh_web_preview(cx);
        }
        cx.notify();
    }

    pub(crate) fn refresh_web_preview(&mut self, cx: &mut Context<Self>) {
        let raw_url = self.web_preview_url.read(cx).value().trim().to_string();
        if raw_url.is_empty() {
            return;
        }
        let url = if raw_url.starts_with("http://")
            || raw_url.starts_with("https://")
            || raw_url.starts_with("file://")
        {
            raw_url
        } else if raw_url.starts_with(':') {
            format!("http://localhost{raw_url}")
        } else if raw_url.starts_with("localhost") || raw_url.starts_with("127.0.0.1") {
            format!("http://{raw_url}")
        } else {
            let candidate = self.root.join(&raw_url);
            if candidate.is_file() {
                format!("file://{}", candidate.display())
            } else {
                format!("https://{raw_url}")
            }
        };

        self.web_preview_loading = true;
        self.web_preview_error = None;
        let (w, h) = self.web_preview_viewport.dimensions();
        cx.notify();

        let (tx, rx) = async_channel::bounded::<Result<ImagePreview, String>>(1);
        std::thread::spawn(move || {
            let Some(chrome) = find_chrome_binary() else {
                let _ = tx.send_blocking(Err(
                    "No Chromium/Chrome browser found for headless snapshot.".to_string(),
                ));
                return;
            };
            let out_path =
                std::env::temp_dir().join(format!("shame-web-preview-{}.png", std::process::id()));
            let _ = std::fs::remove_file(&out_path);
            let status = Command::new(chrome)
                .args([
                    "--headless=new",
                    "--disable-gpu",
                    "--hide-scrollbars",
                    "--no-first-run",
                    "--no-default-browser-check",
                    &format!("--window-size={w},{h}"),
                    &format!("--screenshot={}", out_path.display()),
                    &url,
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();

            match status {
                Ok(s) if s.success() && out_path.is_file() => {
                    if let Some(preview) = load_image_preview(&out_path) {
                        let _ = tx.send_blocking(Ok(preview));
                    } else {
                        let _ = tx.send_blocking(Err("Failed to decode rendered page.".into()));
                    }
                }
                _ => {
                    let _ = tx.send_blocking(Err(format!("Could not reach {url}")));
                }
            }
        });

        cx.spawn(async move |weak, cx| {
            let Ok(res) = rx.recv().await else {
                return;
            };
            let _ = weak.update(cx, |this, cx| {
                this.web_preview_loading = false;
                match res {
                    Ok(img) => {
                        this.web_preview_image = Some(Arc::new(img));
                        this.web_preview_error = None;
                    }
                    Err(e) => {
                        this.web_preview_error = Some(e);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn render_web_preview_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let green = cx.theme().green;
        let border = cx.theme().border;
        let card_bg = cx.theme().muted;
        let list_hover = cx.theme().list_hover;
        let viewport = self.web_preview_viewport;
        let is_loading = self.web_preview_loading;

        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .border_1()
            .border_color(border)
            .rounded(px(tokens::RADIUS_CARD))
            .overflow_hidden()
            // Top bar: Web Preview title + Viewport toggle + Open in Browser + Close
            .child(
                h_flex()
                    .w_full()
                    .h(px(tokens::TABSTRIP_H))
                    .flex_shrink_0()
                    .px(px(tokens::SPACE_3))
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .child(Icon::new(IconName::Globe).small().text_color(primary))
                            .child(
                                div()
                                    .text_size(px(tokens::UI_SM))
                                    .font_medium()
                                    .text_color(fg)
                                    .child("Web Preview"),
                            )
                            .when(is_loading, |el| {
                                el.child(div().size(px(6.)).rounded(px(3.)).bg(green))
                            }),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(tokens::SPACE_1))
                            .child(
                                h_flex()
                                    .id("web-viewport-toggle")
                                    .h(px(22.))
                                    .px(px(6.))
                                    .items_center()
                                    .gap(px(4.))
                                    .rounded(px(tokens::RADIUS_SM))
                                    .bg(cx.theme().background)
                                    .border_1()
                                    .border_color(border)
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(list_hover))
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(fg)
                                    .child(
                                        Icon::new(match viewport {
                                            WebViewport::Desktop => IconName::Monitor,
                                            WebViewport::Mobile => IconName::Smartphone,
                                        })
                                        .with_size(px(11.))
                                        .text_color(primary),
                                    )
                                    .child(viewport.label())
                                    .on_click(cx.listener(|this, _e, _w, cx| {
                                        this.web_preview_viewport = match this.web_preview_viewport
                                        {
                                            WebViewport::Desktop => WebViewport::Mobile,
                                            WebViewport::Mobile => WebViewport::Desktop,
                                        };
                                        this.refresh_web_preview(cx);
                                    })),
                            )
                            .child(
                                Button::new("web-open-external")
                                    .ghost()
                                    .xsmall()
                                    .icon(
                                        Icon::new(IconName::ExternalLink)
                                            .with_size(px(11.5))
                                            .text_color(muted),
                                    )
                                    .tooltip("Open URL in default browser")
                                    .on_click(cx.listener(|this, _e, _w, cx| {
                                        let u =
                                            this.web_preview_url.read(cx).value().trim().to_string();
                                        if !u.is_empty() {
                                            let _ = Command::new("open").arg(&u).spawn();
                                        }
                                    })),
                            )
                            .child(
                                Button::new("close-web-preview")
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(IconName::X).xsmall().text_color(muted))
                                    .tooltip("Close Web Preview")
                                    .on_click(cx.listener(|this, _e, _w, cx| {
                                        this.web_preview_open = false;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            // URL Bar + Reload button + quick port chips
            .child(
                v_flex()
                    .w_full()
                    .p(px(tokens::SPACE_3))
                    .gap(px(tokens::SPACE_2))
                    .border_b_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .w_full()
                            .h(px(28.))
                            .px(px(tokens::SPACE_2))
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .rounded(px(tokens::RADIUS_SM))
                            .bg(cx.theme().input)
                            .border_1()
                            .border_color(border)
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _w, cx| {
                                if event.keystroke.key.as_str() == "enter" {
                                    this.refresh_web_preview(cx);
                                    cx.stop_propagation();
                                }
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .child(
                                        Input::new(&self.web_preview_url)
                                            .appearance(false)
                                            .xsmall(),
                                    ),
                            )
                            .child(
                                div()
                                    .id("web-reload-btn")
                                    .cursor_pointer()
                                    .text_color(muted)
                                    .hover(move |s| s.text_color(fg))
                                    .child(Icon::new(IconName::RotateCw).with_size(px(11.5)))
                                    .on_click(cx.listener(|this, _e, _w, cx| {
                                        this.refresh_web_preview(cx);
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap(px(4.))
                            .children(
                                [
                                    (":3000", "http://localhost:3000"),
                                    (":5173", "http://localhost:5173"),
                                    (":8080", "http://localhost:8080"),
                                ]
                                .into_iter()
                                .enumerate()
                                .map(|(ix, (lbl, target_url))| {
                                    div()
                                        .id(("web-port-chip", ix))
                                        .px(px(6.))
                                        .py(px(2.))
                                        .rounded(px(4.))
                                        .bg(cx.theme().background)
                                        .border_1()
                                        .border_color(border)
                                        .cursor_pointer()
                                        .hover(move |s| s.bg(list_hover).text_color(fg))
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_size(px(9.5))
                                        .text_color(muted)
                                        .child(lbl)
                                        .on_click(cx.listener(move |this, _e, window, cx| {
                                            this.web_preview_url.update(cx, |s, cx| {
                                                s.set_value(target_url, window, cx)
                                            });
                                            this.refresh_web_preview(cx);
                                        }))
                                }),
                            ),
                    ),
            )
            // Viewport canvas
            .child(
                v_flex()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .p(px(tokens::SPACE_3))
                    .items_center()
                    .justify_center()
                    .bg(cx.theme().background)
                    .child(if let Some(img) = &self.web_preview_image {
                        let aspect = img.width as f32 / (img.height.max(1) as f32);
                        let target_w = match viewport {
                            WebViewport::Desktop => 320.0,
                            WebViewport::Mobile => 240.0,
                        };
                        let target_h = (target_w / aspect).min(520.0);
                        div()
                            .w(px(target_w))
                            .h(px(target_h))
                            .rounded(px(tokens::RADIUS_CARD))
                            .border_1()
                            .border_color(border)
                            .overflow_hidden()
                            .bg(gpui_kit::white())
                            .child(
                                gpui_kit::img(img.render_image.clone())
                                    .w_full()
                                    .h_full()
                                    .object_fit(ObjectFit::Contain),
                            )
                            .into_any_element()
                    } else {
                        v_flex()
                            .w_full()
                            .p(px(tokens::SPACE_5))
                            .gap(px(tokens::SPACE_2))
                            .items_center()
                            .rounded(px(tokens::RADIUS_CARD))
                            .bg(card_bg)
                            .border_1()
                            .border_color(border)
                            .child(Icon::new(IconName::Globe).text_color(muted))
                            .child(
                                div()
                                    .text_size(px(tokens::UI_SM))
                                    .text_color(fg)
                                    .child(if is_loading {
                                        "Rendering page preview\u{2026}".to_string()
                                    } else {
                                        self.web_preview_error.clone().unwrap_or_else(|| {
                                            "Enter a localhost URL or open an HTML file".to_string()
                                        })
                                    }),
                            )
                            .into_any_element()
                    }),
            )
    }
}
