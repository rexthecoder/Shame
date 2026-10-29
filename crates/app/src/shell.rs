use gpui_kit::component::{
    ActiveTheme as _,
    resizable::{h_resizable, resizable_panel},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, tokens};

impl DartIde {
    pub(crate) fn render_shell(
        &self,
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let editor_card = v_flex()
            .flex_1()
            .h_full()
            .min_h(px(0.))
            .bg(cx.theme().background)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(tokens::RADIUS_CARD))
            .overflow_hidden()
            .child(self.render_tabs(view, cx))
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .child(self.active_editor().clone()),
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

