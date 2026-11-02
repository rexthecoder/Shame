pub mod agent;
pub mod files;
pub mod git;
pub mod simulator;
pub mod web_preview;

use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex};
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
