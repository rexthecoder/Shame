pub mod agent;
pub mod files;
pub mod git;
pub mod simulator;
pub mod web_preview;

use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::tokens;

/// The small rounded count next to a section title or file group.
pub(crate) fn count_pill(count: usize, cx: &App) -> Div {
    div()
        .flex_shrink_0()
        .px(px(5.))
        .rounded(px(tokens::RADIUS_PILL))
        .bg(cx.theme().list_hover)
        .text_size(px(tokens::UI_XS))
        .text_color(cx.theme().muted_foreground)
        .child(count.to_string())
}

/// A collapsible section header for the side panels: chevron, sentence-case
/// title and count, in one inset and size everywhere. The caller adds the
/// click handler, and trailing actions go next to it in [`section_row`].
pub(crate) fn section_label(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    count: Option<usize>,
    collapsed: bool,
    cx: &App,
) -> Stateful<Div> {
    let fg = cx.theme().foreground;
    h_flex()
        .id(id)
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .items_center()
        .gap(px(tokens::SPACE_2))
        .px(px(tokens::SPACE_2))
        .cursor_pointer()
        .text_size(px(tokens::UI_XS))
        .text_color(cx.theme().muted_foreground)
        .font_medium()
        .hover(move |s| s.text_color(fg))
        .child(
            Icon::new(if collapsed {
                gpui_kit::assets::IconName::ChevronRight
            } else {
                gpui_kit::assets::IconName::ChevronDown
            })
            .xsmall(),
        )
        .child(title.into())
        .when_some(count, |el, count| el.child(count_pill(count, cx)))
}

/// The row a [`section_label`] sits in, with room for trailing actions.
pub(crate) fn section_row() -> Div {
    h_flex()
        .w_full()
        .h(px(tokens::TREE_ROW_H))
        .flex_shrink_0()
        .items_center()
        .justify_between()
        .pl(px(tokens::SPACE_3))
        .pr(px(tokens::SPACE_2))
}

/// A quiet, Fleet-style state header: glyph, heading, one sentence. No card
/// chrome -- rail surfaces are already framed, and an empty state should read
/// as calm, not as another panel. Callers add padding, actions and lists.
pub(crate) fn quiet_state(
    glyph: gpui_kit::assets::IconName,
    tone: Hsla,
    title: impl Into<SharedString>,
    body: impl Into<SharedString>,
    cx: &App,
) -> Div {
    let title: SharedString = title.into();
    let body: SharedString = body.into();
    v_flex()
        .w_full()
        .gap(px(tokens::SPACE_4))
        .child(Icon::new(glyph).with_size(px(20.)).text_color(tone))
        .child(
            v_flex()
                .w_full()
                .gap(px(tokens::SPACE_1))
                .child(
                    div()
                        .text_size(px(tokens::UI_LG))
                        .font_semibold()
                        .text_color(cx.theme().foreground)
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(tokens::UI_SM))
                        .text_color(cx.theme().muted_foreground)
                        .child(body),
                ),
        )
}

/// A flat action row for empty states: a glyph in a small keycap, then its
/// label. The accent marks the one action that moves the state forward.
pub(crate) fn rail_action(
    id: impl Into<ElementId>,
    icon: gpui_kit::assets::IconName,
    label: impl Into<SharedString>,
    accent: bool,
    cx: &App,
) -> Stateful<Div> {
    let label: SharedString = label.into();
    let border = cx.theme().border;
    let hover = cx.theme().list_hover;
    let tone = if accent {
        cx.theme().primary
    } else {
        cx.theme().muted_foreground
    };
    h_flex()
        .id(id)
        .w_full()
        .h(px(tokens::TREE_ROW_H))
        .gap(px(tokens::SPACE_3))
        .items_center()
        .rounded(px(tokens::RADIUS_SM))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(
            div()
                .size(px(18.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(tokens::RADIUS_SM))
                .bg(cx.theme().background)
                .border_1()
                .border_color(border)
                .child(Icon::new(icon).with_size(px(11.)).text_color(tone)),
        )
        .child(
            div()
                .text_size(px(tokens::UI_SM))
                .text_color(cx.theme().foreground)
                .child(label),
        )
}
