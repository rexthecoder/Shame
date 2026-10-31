//! The Widgets sidebar tab: the running Flutter app's widget tree. Click a
//! widget to jump to the code that created it (and highlight it in the app);
//! tap a widget in the app's inspect mode and it is revealed here.

use std::collections::HashSet;

use core_flutter::{WidgetLocation, WidgetNode, flatten_widget_tree};
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, tokens};

#[derive(Default)]
pub(crate) struct WidgetTreeState {
    pub tree: Option<WidgetNode>,
    pub expanded: HashSet<String>,
    pub selected: Option<String>,
    /// The daemon request whose reply is the tree we're waiting for.
    pub request: Option<i64>,
    pub error: Option<String>,
}

/// How many levels a freshly loaded tree opens.
const INITIAL_DEPTH: usize = 4;

impl DartIde {
    /// Fetch the tree from the running app. Does nothing if no app is running.
    pub(crate) fn refresh_widget_tree(&mut self, cx: &mut Context<Self>) {
        let (Some(daemon), Some(app_id)) = (self.flutter_daemon.clone(), self.flutter_app_id.clone()) else {
            return;
        };
        if let Ok(Ok(id)) = daemon.lock().map(|d| d.inspector_request_tree(&app_id, &self.root)) {
            self.widgets.request = Some(id);
            self.widgets.error = None;
        } else {
            self.widgets.error = Some("Could not reach the Flutter inspector.".to_string());
        }
        cx.notify();
    }

    /// Refresh when the Widgets tab is what the user is looking at (after a hot reload).
    pub(crate) fn refresh_widget_tree_if_visible(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_open && self.sidebar_tab == crate::git_panel::SidebarTab::Widgets {
            self.refresh_widget_tree(cx);
        }
    }

    /// The app answered with its widget tree.
    pub(crate) fn finish_widget_tree(
        &mut self,
        reply: Result<serde_json::Value, serde_json::Value>,
        cx: &mut Context<Self>,
    ) {
        match reply.ok().and_then(|v| core_flutter::parse_widget_tree(&v)) {
            Some(tree) => {
                // Keep what was open if the shape is still the same; open the top levels on first load.
                if self.widgets.tree.is_none() || self.widgets.expanded.is_empty() {
                    self.widgets.expanded = core_flutter::default_expanded(&tree, INITIAL_DEPTH);
                }
                self.widgets.selected = self
                    .widgets
                    .selected
                    .take()
                    .filter(|key| core_flutter::node_at(&tree, key).is_some());
                self.widgets.tree = Some(tree);
                self.widgets.error = None;
            }
            None => {
                self.widgets.error = Some(
                    "No widget tree yet. Run a debug build, then press Refresh.".to_string(),
                );
            }
        }
        cx.notify();
    }

    /// A widget was tapped in the app: open the path to it and select it.
    pub(crate) fn reveal_widget(&mut self, widget: &WidgetLocation, cx: &mut Context<Self>) {
        let Some(tree) = &self.widgets.tree else { return };
        if let Some(trail) = core_flutter::find_by_location(tree, &widget.path, widget.line, widget.column) {
            self.widgets.selected = trail.last().cloned();
            // Open every ancestor so the row is visible.
            for key in trail.iter().take(trail.len().saturating_sub(1)) {
                self.widgets.expanded.insert(key.clone());
            }
            cx.notify();
        }
    }

    fn select_widget_row(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node) = self.widgets.tree.as_ref().and_then(|t| core_flutter::node_at(t, &key)).cloned() else {
            return;
        };
        self.widgets.selected = Some(key);
        // Highlight it in the running app.
        if let (Some(value_id), Some(daemon), Some(app_id)) =
            (node.value_id.as_deref(), self.flutter_daemon.clone(), self.flutter_app_id.clone())
        {
            if let Ok(d) = daemon.lock() {
                let _ = d.inspector_select(&app_id, value_id);
            }
        }
        // Jump to where it is written (only your own widgets have a location worth opening).
        if let Some(location) = node.location.filter(|l| l.local) {
            self.open_source_location(location.path, location.line, location.column, window, cx);
        }
        cx.notify();
    }

    fn toggle_widget_row(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.widgets.expanded.remove(key) {
            self.widgets.expanded.insert(key.to_string());
        }
        cx.notify();
    }

    pub(crate) fn render_widgets_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let list_hover = cx.theme().list_hover;
        let running = self.can_inspect();

        let button = |id: &'static str, label: &'static str, enabled: bool| {
            div()
                .id(id)
                .h(px(24.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(tokens::RADIUS_SM))
                .border_1()
                .border_color(border)
                .text_size(px(tokens::UI_XS))
                .when(enabled, |b| b.cursor_pointer().text_color(fg).hover(move |h| h.bg(list_hover)))
                .when(!enabled, |b| b.text_color(muted.opacity(0.5)))
                .child(label)
        };

        let rows = self
            .widgets
            .tree
            .as_ref()
            .map(|tree| flatten_widget_tree(tree, &self.widgets.expanded))
            .unwrap_or_default();
        let selected = self.widgets.selected.clone();

        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .border_1()
            .border_color(border)
            .rounded(px(tokens::RADIUS_CARD))
            .overflow_hidden()
            .child(self.render_sidebar_tabs(cx))
            .child(
                h_flex()
                    .w_full()
                    .flex_shrink_0()
                    .px(px(tokens::SPACE_4))
                    .py(px(tokens::SPACE_2))
                    .gap(px(tokens::SPACE_2))
                    .items_center()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        button("widgets-refresh", "Refresh", running)
                            .on_click(cx.listener(|this, _e, _w, cx| this.refresh_widget_tree(cx))),
                    )
                    .child(
                        button("widgets-collapse", "Collapse all", self.widgets.tree.is_some())
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.widgets.expanded.clear();
                                if let Some(tree) = &this.widgets.tree {
                                    // Keep the root open so the tree isn't a single line.
                                    let _ = tree;
                                    this.widgets.expanded.insert("0".to_string());
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(
                v_flex()
                    .id("widgets-tree")
                    .flex_1()
                    .w_full()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .py(px(tokens::SPACE_2))
                    .when(!running && self.widgets.tree.is_none(), |list| {
                        list.child(
                            div()
                                .p(px(tokens::SPACE_5))
                                .text_size(px(tokens::UI_SM))
                                .text_color(muted)
                                .child("Run your app (Flutter: Run App) to see its widget tree here."),
                        )
                    })
                    .when_some(self.widgets.error.clone(), |list, error| {
                        list.child(
                            div()
                                .px(px(tokens::SPACE_4))
                                .py(px(tokens::SPACE_2))
                                .text_size(px(tokens::UI_SM))
                                .text_color(muted)
                                .child(error),
                        )
                    })
                    .children(rows.into_iter().enumerate().map(|(ix, row)| {
                        let is_selected = selected.as_deref() == Some(row.key.as_str());
                        let key_toggle = row.key.clone();
                        let key_select = row.key.clone();
                        let local = row.location.as_ref().is_some_and(|l| l.local);
                        h_flex()
                            .id(("widget-row", ix))
                            .w_full()
                            .h(px(24.))
                            .pl(px(tokens::SPACE_3 + row.depth as f32 * 14.))
                            .pr(px(tokens::SPACE_3))
                            .gap(px(2.))
                            .items_center()
                            .cursor_pointer()
                            .when(is_selected, |r| r.bg(tokens::accent_dim()))
                            .when(!is_selected, |r| r.hover(move |h| h.bg(list_hover)))
                            .child(
                                div()
                                    .id(("widget-toggle", ix))
                                    .size(px(16.))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(muted)
                                    .when(row.has_children, |t| {
                                        t.child(
                                            Icon::new(if row.expanded {
                                                IconName::ChevronDown
                                            } else {
                                                IconName::ChevronRight
                                            })
                                            .xsmall(),
                                        )
                                        .on_click(cx.listener(move |this, _e, _w, cx| {
                                            cx.stop_propagation();
                                            this.toggle_widget_row(&key_toggle, cx);
                                        }))
                                    }),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(tokens::UI_SM))
                                    .text_color(if local { fg } else { muted })
                                    .child(row.name.clone()),
                            )
                            .when(row.stateful, |r| {
                                r.child(
                                    div()
                                        .flex_shrink_0()
                                        .px(px(4.))
                                        .rounded(px(3.))
                                        .bg(cx.theme().info.opacity(0.16))
                                        .text_size(px(9.))
                                        .text_color(cx.theme().info)
                                        .child("stateful"),
                                )
                            })
                            .on_click(cx.listener(move |this, _e, window, cx| {
                                this.select_widget_row(key_select.clone(), window, cx);
                            }))
                    })),
            )
    }
}
