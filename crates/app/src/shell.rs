use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    h_flex,
    resizable::{h_resizable, resizable_panel},
    text::markdown,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::rails::files::{file_icon_color, icon_for_file};
use crate::{DartIde, MarkdownMode, tokens};

fn keycap(label: &'static str, cx: &App) -> Div {
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
        .child(label)
}

impl DartIde {
    fn render_empty_editor(
        &self,
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;

        let quick_files: Vec<_> = self
            .all_files
            .iter()
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| matches!(ext, "dart" | "rs" | "toml" | "md"))
            })
            .take(4)
            .cloned()
            .collect();

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(tokens::SPACE_5))
            .child(
                v_flex()
                    .w(px(280.))
                    .p(px(tokens::SPACE_3))
                    .gap(px(tokens::SPACE_1))
                    .rounded(px(tokens::RADIUS_CARD))
                    .bg(cx.theme().sidebar)
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .id("empty-go-file")
                            .w_full()
                            .h(px(30.))
                            .px(px(tokens::SPACE_4))
                            .rounded(px(tokens::RADIUS_SM))
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(0xffffff08)).text_color(fg))
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(tokens::SPACE_3))
                                    .text_size(px(tokens::UI))
                                    .text_color(muted)
                                    .child(
                                        Icon::new(gpui_kit::assets::IconName::Search)
                                            .xsmall()
                                            .text_color(cx.theme().primary),
                                    )
                                    .child("Go to File\u{2026}"),
                            )
                            .child(keycap("\u{2318}P", cx))
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.palette_open = true;
                                this.palette_selected = 0;
                                this.palette_input
                                    .update(cx, |state, cx| state.focus(window, cx));
                                cx.notify();
                            })),
                    )
                    .child(
                        h_flex()
                            .id("empty-new-file")
                            .w_full()
                            .h(px(30.))
                            .px(px(tokens::SPACE_4))
                            .rounded(px(tokens::RADIUS_SM))
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(0xffffff08)).text_color(fg))
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(tokens::SPACE_3))
                                    .text_size(px(tokens::UI))
                                    .text_color(muted)
                                    .child(
                                        Icon::new(gpui_kit::assets::IconName::FilePlus)
                                            .xsmall()
                                            .text_color(muted),
                                    )
                                    .child("New Scratch File"),
                            )
                            .child(keycap("\u{2318}N", cx))
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.new_scratch_tab(window, cx);
                            })),
                    )
                    .child(
                        h_flex()
                            .id("empty-open-folder")
                            .w_full()
                            .h(px(30.))
                            .px(px(tokens::SPACE_4))
                            .rounded(px(tokens::RADIUS_SM))
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(0xffffff08)).text_color(fg))
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(tokens::SPACE_3))
                                    .text_size(px(tokens::UI))
                                    .text_color(muted)
                                    .child(
                                        Icon::new(gpui_kit::assets::IconName::FolderPlus)
                                            .xsmall()
                                            .text_color(muted),
                                    )
                                    .child("Open Folder\u{2026}"),
                            )
                            .child(keycap("\u{2318}O", cx))
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.open_folder(window, cx);
                            })),
                    )
                    .child(
                        h_flex()
                            .id("empty-toggle-files")
                            .w_full()
                            .h(px(30.))
                            .px(px(tokens::SPACE_4))
                            .rounded(px(tokens::RADIUS_SM))
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgba(0xffffff08)).text_color(fg))
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap(px(tokens::SPACE_3))
                                    .text_size(px(tokens::UI))
                                    .text_color(muted)
                                    .child(
                                        Icon::new(gpui_kit::assets::IconName::PanelLeft)
                                            .xsmall()
                                            .text_color(muted),
                                    )
                                    .child("Toggle Files Panel"),
                            )
                            .child(keycap("\u{2318}B", cx))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.sidebar_open = !this.sidebar_open;
                                cx.notify();
                            })),
                    ),
            )
            .when(!quick_files.is_empty(), |el| {
                el.child(
                    h_flex()
                        .items_center()
                        .gap(px(tokens::SPACE_2))
                        .children(quick_files.into_iter().enumerate().map(|(ix, path)| {
                            let name = path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or_default()
                                .to_string();
                            let view = view.clone();
                            h_flex()
                                .id(("quick-file", ix))
                                .h(px(26.))
                                .px(px(tokens::SPACE_3))
                                .gap(px(tokens::SPACE_2))
                                .items_center()
                                .rounded(px(tokens::RADIUS_PILL))
                                .bg(cx.theme().sidebar)
                                .border_1()
                                .border_color(cx.theme().border)
                                .cursor_pointer()
                                .hover(move |s| {
                                    s.bg(rgba(0xea7a3c14)).border_color(rgba(0xea7a3c40))
                                })
                                .child(
                                    icon_for_file(&name)
                                        .text_color(file_icon_color(&name))
                                        .xsmall(),
                                )
                                .child(
                                    div()
                                        .text_size(px(tokens::UI_SM))
                                        .text_color(fg)
                                        .child(name),
                                )
                                .on_click(move |_event, window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.open_path(path.clone(), window, cx);
                                    });
                                })
                        })),
                )
            })
    }

    pub(crate) fn render_shell(
        &self,
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active_tab = self.tabs.get(self.active_tab);
        let active_editor = active_tab.map(|t| t.editor.clone());
        let active_md_mode = active_tab
            .filter(|t| t.is_markdown())
            .map(|t| t.md_mode)
            .unwrap_or(MarkdownMode::Code);
        let has_tabs = !self.tabs.is_empty();

        let render_md_pane = |editor: &Entity<editor_view::EditorView>, cx: &App| {
            let source: SharedString = editor.read(cx).buffer().text().into();
            div()
                .flex_1()
                .h_full()
                .min_w(px(0.))
                .min_h(px(0.))
                .bg(cx.theme().background)
                .p(px(tokens::SPACE_6))
                .child(markdown(source).scrollable(true).selectable(true))
        };

        let editor_body = match (active_editor, active_md_mode) {
            (Some(editor), MarkdownMode::Code) => editor.into_any_element(),
            (Some(editor), MarkdownMode::Preview) => render_md_pane(&editor, cx).into_any_element(),
            (Some(editor), MarkdownMode::Split) => {
                let preview = render_md_pane(&editor, cx);
                h_flex()
                    .size_full()
                    .min_h(px(0.))
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .min_w(px(0.))
                            .min_h(px(0.))
                            .border_r_1()
                            .border_color(cx.theme().border)
                            .child(editor),
                    )
                    .child(preview)
                    .into_any_element()
            }
            (None, _) => self.render_empty_editor(view, cx).into_any_element(),
        };

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
            .child(div().flex_1().w_full().min_h(px(0.)).child(editor_body))
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

        let split_id = match (self.sidebar_open, self.agent_open) {
            (true, true) => "shell-split-both",
            (true, false) => "shell-split-left",
            (false, true) => "shell-split-right",
            (false, false) => "shell-split-center",
        };
        let mut split = h_resizable(split_id);

        if self.sidebar_open {
            split = split.child(
                resizable_panel()
                    .size(px(tokens::RAIL_LEFT))
                    .size_range(px(tokens::RAIL_LEFT_MIN)..px(tokens::RAIL_LEFT_MAX))
                    .fixed()
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
                    .fixed()
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
