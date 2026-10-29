use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::rails::files::{file_icon_color, icon_for_file};
use crate::{DartIde, tokens};

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
            .overflow_hidden()
            .child(
                h_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .items_center()
                    .gap(px(tokens::SPACE_2))
                    .overflow_hidden()
                    .children(self.tabs.iter().enumerate().map(|(ix, tab)| {
                        let is_active = ix == self.active_tab;
                        let is_dirty = tab.editor.read(cx).buffer().is_dirty();
                        let title = tab.title();

                        h_flex()
                            .id(("tab", ix))
                            .h(px(30.))
                            .flex_shrink_0()
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .px(px(tokens::SPACE_3))
                            .cursor_pointer()
                            .rounded(px(tokens::RADIUS_PANEL))
                            .when(is_active, |el| {
                                el.bg(cx.theme().tab_active)
                                    .border_1()
                                    .border_color(tokens::accent_dim())
                            })
                            .when(!is_active, |el| {
                                el.hover(|s| s.bg(cx.theme().list_hover))
                            })
                            .text_color(if is_active { active_fg } else { inactive_fg })
                            .child(
                                icon_for_file(&title)
                                    .text_color(file_icon_color(&title))
                                    .small(),
                            )
                            .child(
                                div()
                                    .id(("tab-label", ix))
                                    .text_size(px(tokens::UI))
                                    .child(title)
                                    .on_click({
                                        let view = view.clone();
                                        move |_event, _window, cx| {
                                            view.update(cx, |this, cx| {
                                                this.active_tab = ix;
                                                cx.notify();
                                            });
                                        }
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
                                    .on_click({
                                        let view = view.clone();
                                        move |_event, _window, cx| {
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
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(tokens::SPACE_1))
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
                                    ed.update(cx, |editor, _cx| editor.request_code_actions());
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
}
