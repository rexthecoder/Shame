use std::collections::HashMap;

use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::rails::files::{file_icon_color, icon_for_file};
use crate::{DartIde, MarkdownMode, tokens};

impl DartIde {
    pub(crate) fn render_tabs(
        &self,
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let info = cx.theme().info;
        let muted = cx.theme().muted_foreground;
        let active_fg = cx.theme().tab_active_foreground;
        let inactive_fg = cx.theme().tab_foreground;
        let list_hover = cx.theme().list_hover;
        let primary = cx.theme().primary;
        let active_md_mode = self
            .tabs
            .get(self.active_tab)
            .filter(|t| t.is_markdown())
            .map(|t| t.md_mode);
        let active_is_pubspec = self
            .tabs
            .get(self.active_tab)
            .is_some_and(|t| t.is_pubspec());

        // Count duplicate file basenames so two open `mod.rs` or `lib.rs` tabs
        // automatically show their parent folder chip (Zed-style disambiguation).
        let mut title_counts: HashMap<String, usize> = HashMap::new();
        for tab in &self.tabs {
            *title_counts.entry(tab.title()).or_insert(0) += 1;
        }

        h_flex()
            .w_full()
            .h(px(tokens::TABSTRIP_H))
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .px(px(tokens::SPACE_4))
            .bg(cx.theme().tab_bar)
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .id("tab-scroll-strip")
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_x_scroll()
                    .track_scroll(&self.tab_scroll)
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .children(self.tabs.iter().enumerate().map(|(ix, tab)| {
                                let is_active = ix == self.active_tab;
                                let is_dirty = tab.editor.read(cx).buffer().is_dirty();
                                let is_preview = !tab.is_pinned(cx);
                                let title = tab.title();
                                let disambig_parent = (title_counts.get(&title).copied().unwrap_or(0) > 1)
                                    .then(|| tab.parent_label())
                                    .flatten();

                                h_flex()
                                    .id(("tab", ix))
                                    .h(px(30.))
                                    .flex_shrink_0()
                                    .items_center()
                                    .gap(px(tokens::SPACE_2))
                                    .px(px(tokens::SPACE_3))
                                    .cursor_pointer()
                                    .rounded(px(tokens::RADIUS_PANEL))
                                    .border_1()
                                    .border_color(gpui_kit::transparent_black())
                                    .when(is_active, |el| {
                                        el.bg(cx.theme().tab_active)
                                            .border_color(tokens::accent_dim())
                                    })
                                    .when(!is_active, |el| {
                                        el.hover(move |s| s.bg(list_hover).text_color(active_fg))
                                    })
                                    .text_color(if is_active { active_fg } else { inactive_fg })
                                    .on_click({
                                        let view = view.clone();
                                        move |event, window, cx| {
                                            let double_clicked = event.click_count() >= 2;
                                            view.update(cx, |this, cx| {
                                                this.active_tab = ix;
                                                if double_clicked {
                                                    if let Some(t) = this.tabs.get_mut(ix) {
                                                        t.is_preview = false;
                                                    }
                                                }
                                                if let Some(t) = this.tabs.get(ix) {
                                                    let fh = t.editor.read(cx).focus_handle(cx);
                                                    window.focus(&fh, cx);
                                                }
                                                this.tab_overflow_open = false;
                                                cx.notify();
                                            });
                                        }
                                    })
                                    .child(
                                        icon_for_file(&title)
                                            .text_color(file_icon_color(&title))
                                            .small(),
                                    )
                                    .child(
                                        h_flex()
                                            .items_center()
                                            .gap(px(4.))
                                            .child(
                                                div()
                                                    .text_size(px(tokens::UI))
                                                    .when(is_preview, |s| {
                                                        s.italic().text_color(if is_active {
                                                            active_fg.opacity(0.88)
                                                        } else {
                                                            muted
                                                        })
                                                    })
                                                    .child(title),
                                            )
                                            .when_some(disambig_parent, |el, parent| {
                                                el.child(
                                                    div()
                                                        .text_size(px(tokens::UI_XS))
                                                        .text_color(muted)
                                                        .child(format!("{parent}/")),
                                                )
                                            }),
                                    )
                                    .when(is_dirty, |el| {
                                        el.child(
                                            div()
                                                .text_size(px(8.))
                                                .text_color(info)
                                                .child("\u{25cf}"),
                                        )
                                    })
                                    .child(
                                        div()
                                            .id(("tab-close", ix))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_color(muted)
                                            .hover(move |s| s.text_color(active_fg))
                                            .child(Icon::new(gpui_kit::assets::IconName::X).xsmall())
                                            .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                                                cx.stop_propagation();
                                            })
                                            .on_click({
                                                let view = view.clone();
                                                move |_event, _window, cx| {
                                                    cx.stop_propagation();
                                                    view.update(cx, |this, cx| {
                                                        this.close_tab(ix, cx);
                                                    });
                                                }
                                            }),
                                    )
                            }))
                            .child(
                                Button::new("split-editor")
                                    .ghost()
                                    .small()
                                    .icon(
                                        Icon::new(gpui_kit::assets::IconName::Columns2)
                                            .xsmall()
                                            .text_color(muted),
                                    )
                                    .tooltip("Split editor right"),
                            )
                            .child(
                                Button::new("new-tab")
                                    .ghost()
                                    .small()
                                    .icon(
                                        Icon::new(gpui_kit::assets::IconName::Plus)
                                            .small()
                                            .text_color(muted),
                                    )
                                    .tooltip("New scratch tab (cmd/ctrl+N)")
                                    .on_click(cx.listener(|this, _event, window, cx| {
                                        this.new_scratch_tab(window, cx);
                                    })),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(tokens::SPACE_1))
                    .pl(px(tokens::SPACE_2))
                    .when(self.tabs.len() > 1, |el| {
                        let view = view.clone();
                        let is_open = self.tab_overflow_open;
                        let count = self.tabs.len();
                        el.child(
                            h_flex()
                                .id("tab-overflow-btn")
                                .h(px(24.))
                                .px(px(tokens::SPACE_2))
                                .gap(px(3.))
                                .items_center()
                                .rounded(px(tokens::RADIUS_SM))
                                .cursor_pointer()
                                .when(is_open, |s| {
                                    s.bg(rgba(0xea7a3c22))
                                        .border_1()
                                        .border_color(rgba(0xea7a3c55))
                                        .text_color(active_fg)
                                })
                                .when(!is_open, |s| {
                                    s.bg(cx.theme().background)
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .text_color(muted)
                                        .hover(move |h| h.bg(list_hover).text_color(active_fg))
                                })
                                .child(
                                    div()
                                        .font_family("SF Mono")
                                        .text_size(px(10.5))
                                        .child(count.to_string()),
                                )
                                .child(
                                    Icon::new(gpui_kit::assets::IconName::ChevronDown)
                                        .xsmall()
                                        .text_color(if is_open { primary } else { muted }),
                                )
                                .on_click(move |_event, _window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.tab_overflow_open = !this.tab_overflow_open;
                                        cx.notify();
                                    });
                                }),
                        )
                    })
                    .when_some(active_md_mode, |el, md_mode| {
                        let primary = cx.theme().primary;
                        let mode_pill = |id: &'static str,
                                         label: &'static str,
                                         icon: gpui_kit::assets::IconName,
                                         target: MarkdownMode,
                                         view: &Entity<Self>| {
                            let is_current = md_mode == target;
                            let view = view.clone();
                            h_flex()
                                .id(id)
                                .h(px(24.))
                                .px(px(tokens::SPACE_3))
                                .gap(px(tokens::SPACE_1))
                                .items_center()
                                .rounded(px(tokens::RADIUS_SM))
                                .border_1()
                                .border_color(gpui_kit::transparent_black())
                                .cursor_pointer()
                                .when(is_current, |s| {
                                    s.bg(rgba(0xea7a3c22))
                                        .border_color(rgba(0xea7a3c55))
                                        .text_color(active_fg)
                                })
                                .when(!is_current, |s| {
                                    s.text_color(muted).hover(|h| h.bg(rgba(0xffffff08)))
                                })
                                .child(
                                    Icon::new(icon).xsmall().text_color(if is_current {
                                        primary
                                    } else {
                                        muted
                                    }),
                                )
                                .child(div().text_size(px(tokens::UI_XS)).child(label))
                                .on_click(move |_event, _window, cx| {
                                    view.update(cx, |this, cx| {
                                        if let Some(tab) = this.tabs.get_mut(this.active_tab) {
                                            tab.md_mode = target;
                                            cx.notify();
                                        }
                                    });
                                })
                        };

                        el.child(
                            h_flex()
                                .mr(px(tokens::SPACE_2))
                                .p(px(2.))
                                .gap(px(2.))
                                .items_center()
                                .rounded(px(tokens::RADIUS_PANEL))
                                .bg(cx.theme().background)
                                .border_1()
                                .border_color(cx.theme().border)
                                .child(mode_pill(
                                    "md-mode-code",
                                    "Code",
                                    gpui_kit::assets::IconName::File,
                                    MarkdownMode::Code,
                                    view,
                                ))
                                .child(mode_pill(
                                    "md-mode-split",
                                    "Split",
                                    gpui_kit::assets::IconName::Columns2,
                                    MarkdownMode::Split,
                                    view,
                                ))
                                .child(mode_pill(
                                    "md-mode-preview",
                                    "Preview",
                                    gpui_kit::assets::IconName::Eye,
                                    MarkdownMode::Preview,
                                    view,
                                )),
                        )
                    })
                    .when(active_is_pubspec, |el| {
                        let green = cx.theme().green;
                        el.child(
                            h_flex()
                                .id("pubspec-pub-get")
                                .h(px(24.))
                                .px(px(tokens::SPACE_3))
                                .gap(px(tokens::SPACE_1))
                                .items_center()
                                .rounded(px(tokens::RADIUS_SM))
                                .bg(green.opacity(0.14))
                                .border_1()
                                .border_color(green.opacity(0.35))
                                .cursor_pointer()
                                .hover(move |s| s.bg(green.opacity(0.22)))
                                .text_size(px(tokens::UI_XS))
                                .text_color(active_fg)
                                .child(
                                    Icon::new(gpui_kit::assets::IconName::RotateCw)
                                        .with_size(px(10.5))
                                        .text_color(green),
                                )
                                .child("Pub Get")
                                .on_click(cx.listener(|this, _event, _window, cx| {
                                    this.run_pub_get(cx);
                                })),
                        )
                        .child(
                            h_flex()
                                .id("pubspec-add-pkg")
                                .h(px(24.))
                                .px(px(tokens::SPACE_3))
                                .gap(px(tokens::SPACE_1))
                                .items_center()
                                .rounded(px(tokens::RADIUS_SM))
                                .bg(cx.theme().background)
                                .border_1()
                                .border_color(cx.theme().border)
                                .cursor_pointer()
                                .hover(move |s| s.bg(list_hover).text_color(active_fg))
                                .text_size(px(tokens::UI_XS))
                                .text_color(muted)
                                .child(
                                    Icon::new(gpui_kit::assets::IconName::Plus)
                                        .with_size(px(10.5))
                                        .text_color(info),
                                )
                                .child("Package")
                                .on_click(cx.listener(|this, _event, window, cx| {
                                    this.open_add_package(window, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new("tab-code-actions")
                            .ghost()
                            .small()
                            .icon(
                                Icon::new(gpui_kit::assets::IconName::Lightbulb)
                                    .xsmall()
                                    .text_color(cx.theme().warning),
                            )
                            .tooltip("Code actions (cmd/ctrl+.)")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                if let Some(ed) = this.active_editor().cloned() {
                                    ed.update(cx, |editor, cx| editor.request_code_actions(cx));
                                }
                            })),
                    )
                    .child(
                        Button::new("tab-format")
                            .ghost()
                            .small()
                            .icon(
                                Icon::new(gpui_kit::assets::IconName::Sparkles)
                                    .xsmall()
                                    .text_color(muted),
                            )
                            .tooltip("Format document (shift+cmd/ctrl+F)")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                if let Some(ed) = this.active_editor().cloned() {
                                    ed.update(cx, |editor, _cx| {
                                        editor.request_format();
                                    });
                                }
                            })),
                    ),
            )
    }

    pub(crate) fn render_tab_overflow_popover(
        &self,
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        if !self.tab_overflow_open || self.tabs.is_empty() {
            return None;
        }

        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let info = cx.theme().info;
        let list_hover = cx.theme().list_hover;
        let active_ix = self.active_tab;

        Some(
            v_flex()
                .id("tab-overflow-popover")
                .absolute()
                .top(px(tokens::TABSTRIP_H + 4.))
                .right(px(tokens::SPACE_4))
                .w(px(260.))
                .max_h(px(340.))
                .p(px(tokens::SPACE_2))
                .gap(px(tokens::SPACE_1))
                .bg(cx.theme().popover)
                .border_1()
                .border_color(cx.theme().border)
                .rounded(px(tokens::RADIUS_CARD))
                .shadow_xl()
                .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                    cx.stop_propagation();
                })
                .child(
                    v_flex()
                        .id("tab-overflow-list")
                        .max_h(px(230.))
                        .overflow_y_scroll()
                        .gap(px(2.))
                        .children(self.tabs.iter().enumerate().map(|(ix, tab)| {
                            let is_active = ix == active_ix;
                            let is_dirty = tab.editor.read(cx).buffer().is_dirty();
                            let is_preview = !tab.is_pinned(cx);
                            let title = tab.title();
                            let parent = tab.parent_label();
                            let view_select = view.clone();
                            let view_close = view.clone();

                            h_flex()
                                .id(("overflow-tab-item", ix))
                                .w_full()
                                .h(px(28.))
                                .px(px(tokens::SPACE_3))
                                .items_center()
                                .justify_between()
                                .rounded(px(tokens::RADIUS_SM))
                                .border_1()
                                .border_color(gpui_kit::transparent_black())
                                .cursor_pointer()
                                .when(is_active, |el| {
                                    el.bg(rgba(0xea7a3c18))
                                        .border_color(rgba(0xea7a3c38))
                                })
                                .when(!is_active, |el| el.hover(move |s| s.bg(list_hover)))
                                .on_click(move |_event, window, cx| {
                                    view_select.update(cx, |this, cx| {
                                        this.active_tab = ix;
                                        this.tab_overflow_open = false;
                                        this.tab_scroll.scroll_to_item(ix);
                                        if let Some(t) = this.tabs.get(ix) {
                                            let fh = t.editor.read(cx).focus_handle(cx);
                                            window.focus(&fh, cx);
                                        }
                                        cx.notify();
                                    });
                                })
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap(px(tokens::SPACE_2))
                                        .min_w(px(0.))
                                        .flex_1()
                                        .child(
                                            icon_for_file(&title)
                                                .text_color(file_icon_color(&title))
                                                .xsmall(),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(px(tokens::UI_SM))
                                                .text_color(fg)
                                                .when(is_preview, |s| s.italic())
                                                .child(title),
                                        )
                                        .when_some(parent, |el, dir| {
                                            el.child(
                                                div()
                                                    .text_size(px(tokens::UI_XS))
                                                    .text_color(muted)
                                                    .child(dir),
                                            )
                                        }),
                                )
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap(px(tokens::SPACE_2))
                                        .when(is_dirty, |el| {
                                            el.child(
                                                div()
                                                    .text_size(px(8.))
                                                    .text_color(info)
                                                    .child("\u{25cf}"),
                                            )
                                        })
                                        .child(
                                            div()
                                                .id(("overflow-tab-close", ix))
                                                .text_color(muted)
                                                .hover(move |s| s.text_color(fg))
                                                .child(
                                                    Icon::new(gpui_kit::assets::IconName::X)
                                                        .xsmall(),
                                                )
                                                .on_click(move |_event, _window, cx| {
                                                    cx.stop_propagation();
                                                    view_close.update(cx, |this, cx| {
                                                        this.close_tab(ix, cx);
                                                    });
                                                }),
                                        ),
                                )
                        })),
                )
                .child(div().h(px(1.)).w_full().bg(cx.theme().border))
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap(px(tokens::SPACE_1))
                        .child({
                            let view = view.clone();
                            div()
                                .id("close-other-tabs")
                                .flex_1()
                                .h(px(24.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(tokens::RADIUS_SM))
                                .cursor_pointer()
                                .text_size(px(tokens::UI_XS))
                                .text_color(muted)
                                .hover(move |s| s.bg(list_hover).text_color(fg))
                                .child("Close Others")
                                .on_click(move |_event, _window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.close_other_tabs(active_ix, cx);
                                    });
                                })
                        })
                        .child({
                            let view = view.clone();
                            div()
                                .id("close-clean-tabs")
                                .flex_1()
                                .h(px(24.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(tokens::RADIUS_SM))
                                .cursor_pointer()
                                .text_size(px(tokens::UI_XS))
                                .text_color(muted)
                                .hover(move |s| s.bg(list_hover).text_color(fg))
                                .child("Close Clean")
                                .on_click(move |_event, _window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.close_clean_tabs(cx);
                                    });
                                })
                        })
                        .child({
                            let view = view.clone();
                            div()
                                .id("close-all-tabs")
                                .flex_1()
                                .h(px(24.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(tokens::RADIUS_SM))
                                .cursor_pointer()
                                .text_size(px(tokens::UI_XS))
                                .text_color(muted)
                                .hover(move |s| s.bg(list_hover).text_color(fg))
                                .child("Close All")
                                .on_click(move |_event, _window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.tabs.clear();
                                        this.active_tab = 0;
                                        this.tab_overflow_open = false;
                                        cx.notify();
                                    });
                                })
                        }),
                ),
        )
    }
}
