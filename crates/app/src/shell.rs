use gpui_kit::component::{
    ActiveTheme as _,
    h_flex,
    resizable::{h_resizable, resizable_panel},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, tokens};

fn shortcut_row(label: &'static str, keys: &'static str, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    h_flex()
        .w(px(220.))
        .items_center()
        .justify_between()
        .text_size(px(tokens::UI_SM))
        .child(div().text_color(muted).child(label))
        .child(
            div()
                .px(px(tokens::SPACE_3))
                .py(px(tokens::SPACE_1))
                .rounded(px(tokens::RADIUS_SM))
                .bg(cx.theme().secondary)
                .border_1()
                .border_color(cx.theme().border)
                .font_family("SF Mono")
                .text_size(px(tokens::UI_XS))
                .text_color(cx.theme().foreground)
                .child(keys),
        )
}

impl DartIde {
    fn render_empty_editor(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(tokens::SPACE_4))
            .child(shortcut_row("Go to File", "\u{2318}P", cx))
            .child(shortcut_row("New File", "\u{2318}N", cx))
            .child(shortcut_row("Open Folder", "\u{2318}O", cx))
            .child(shortcut_row("Toggle Files", "\u{2318}B", cx))
            .child(shortcut_row("Toggle Terminal", "\u{2318}J", cx))
    }

    pub(crate) fn render_shell(
        &self,
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active_editor = self.active_editor().cloned();
        let has_tabs = !self.tabs.is_empty();

        let editor_card = v_flex()
            .flex_1()
            .h_full()
            .min_h(px(0.))
            .bg(cx.theme().background)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(tokens::RADIUS_CARD))
            .overflow_hidden()
            .when(has_tabs, |el| el.child(self.render_tabs(view, cx)))
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .child(match active_editor {
                        Some(editor) => editor.into_any_element(),
                        None => self.render_empty_editor(cx).into_any_element(),
                    }),
            )
            .into_any_element();

        let editor_column = v_flex()
            .flex_1()
            .w_full()
            .h(relative(1.))
            .min_h(px(0.))
            .px(px(tokens::GUTTER / 2.))
            .gap(px(tokens::GUTTER))
            .child(editor_card)
            .when(self.terminal_open, |el| el.child(self.render_terminal(cx)))
            .into_any_element();

        let mut split = h_resizable("shell-split");

        if self.sidebar_open {
            split = split.child(
                resizable_panel()
                    .size(px(tokens::RAIL_LEFT))
                    .size_range(px(tokens::RAIL_LEFT_MIN)..px(tokens::RAIL_LEFT_MAX))
                    .pr(px(tokens::GUTTER / 2.))
                    .child(self.render_files_rail(view, cx)),
            );
        }

        split = split.child(resizable_panel().child(editor_column));

        if self.agent_open {
            split = split.child(
                resizable_panel()
                    .size(px(tokens::RAIL_RIGHT))
                    .size_range(px(tokens::RAIL_RIGHT_MIN)..px(tokens::RAIL_RIGHT_MAX))
                    .pl(px(tokens::GUTTER / 2.))
                    .child(self.render_agent_rail(cx)),
            );
        }

        div()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .p(px(tokens::GUTTER))
            .child(split)
    }
}
