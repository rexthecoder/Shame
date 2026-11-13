//! One look for every right-click and dropdown menu: a compact row with a
//! muted icon, the label at the UI size, and the shortcut (when there is a
//! real one) right-aligned. Destructive rows are red.

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, menu::PopupMenuItem};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::tokens;

/// A menu row. `hint` is a shortcut to show, only when it really works.
pub(crate) fn row(icon: IconName, label: impl Into<SharedString>, hint: Option<&'static str>) -> PopupMenuItem {
    build(icon, label.into(), hint, false)
}

/// A destructive row (delete, discard), in red.
pub(crate) fn danger(icon: IconName, label: impl Into<SharedString>) -> PopupMenuItem {
    build(icon, label.into(), None, true)
}

fn build(icon: IconName, label: SharedString, hint: Option<&'static str>, danger: bool) -> PopupMenuItem {
    PopupMenuItem::element(move |_window, cx| {
        let muted = cx.theme().muted_foreground;
        let tone = if danger { cx.theme().red } else { cx.theme().foreground };
        h_flex()
            .w_full()
            .min_w(px(180.))
            .gap(px(tokens::SPACE_4))
            .items_center()
            .text_size(px(tokens::UI))
            .text_color(tone)
            .child(
                Icon::new(icon.clone())
                    .with_size(px(13.))
                    .text_color(if danger { tone } else { muted }),
            )
            .child(div().flex_1().child(label.clone()))
            .when_some(hint, |el, hint| {
                el.child(
                    div()
                        .pl(px(tokens::SPACE_6))
                        .text_size(px(tokens::UI_XS))
                        .text_color(muted.opacity(0.8))
                        .child(hint),
                )
            })
    })
}
