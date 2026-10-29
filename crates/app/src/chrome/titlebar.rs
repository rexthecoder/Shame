use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, tokens};

fn icon_button(
    id: &'static str,
    icon: gpui_kit::assets::IconName,
    color: Option<Hsla>,
    tooltip: &'static str,
) -> Button {
    let mut ic = Icon::new(icon);
    if let Some(c) = color {
        ic = ic.text_color(c);
    }
    Button::new(id).ghost().small().icon(ic).tooltip(tooltip)
}

impl DartIde {
    pub(crate) fn render_titlebar(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let traffic_light_inset = if window.is_fullscreen() { px(12.) } else { px(78.) };

        let root_name = self
            .root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("/")
            .to_string();

        let active_file_name = self
            .active_editor()
            .and_then(|ed| ed.read(cx).buffer().path())
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .map(str::to_string);

        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let green = cx.theme().green;
        let info = cx.theme().info;
        let has_changes = !self.git_status.is_empty();

        h_flex()
            .w_full()
            .h(px(tokens::TITLEBAR_H))
            .flex_shrink_0()
            .pl(traffic_light_inset)
            .pr_2()
            .items_center()
            .bg(cx.theme().title_bar)
            .border_b_1()
            .border_color(cx.theme().title_bar_border)
            .child(
                h_flex()
                    .flex_1()
                    .items_center()
                    .gap_1()
                    .child(icon_button(
                        "menu",
                        gpui_kit::assets::IconName::Menu,
                        Some(muted),
                        "Application menu",
                    ))
                    .child(
                        Button::new("layout-sidebar")
                            .ghost()
                            .small()
                            .icon(
                                Icon::new(if self.sidebar_open {
                                    gpui_kit::assets::IconName::PanelLeftClose
                                } else {
                                    gpui_kit::assets::IconName::PanelLeftOpen
                                })
                                .text_color(if self.sidebar_open { fg } else { muted }),
                            )
                            .tooltip("Toggle sidebar (cmd/ctrl+B)")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.sidebar_open = !this.sidebar_open;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("layout-panel")
                            .ghost()
                            .small()
                            .icon(
                                Icon::new(gpui_kit::assets::IconName::PanelBottom)
                                    .text_color(if self.terminal_open { fg } else { muted }),
                            )
                            .tooltip("Toggle terminal dock (cmd/ctrl+J)")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.terminal_open = !this.terminal_open;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("layout-columns")
                            .ghost()
                            .small()
                            .icon(
                                Icon::new(gpui_kit::assets::IconName::Columns2)
                                    .text_color(if self.agent_open { fg } else { muted }),
                            )
                            .tooltip("Toggle agent rail")
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.agent_open = !this.agent_open;
                                cx.notify();
                            })),
                    )
                    .child(icon_button(
                        "add-person",
                        gpui_kit::assets::IconName::UserPlus,
                        Some(muted),
                        "Invite collaborator",
                    )),
            )
            .child(
                h_flex()
                    .id("titlebar-project-pill")
                    .flex_shrink_0()
                    .h(px(26.))
                    .px(px(tokens::SPACE_4))
                    .rounded(px(tokens::RADIUS_PILL))
                    .bg(rgba(0xffffff05))
                    .border_1()
                    .border_color(cx.theme().border.opacity(0.6))
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .hover(|s| s.bg(rgba(0xffffff0c)))
                    .child(
                        div()
                            .text_size(px(tokens::UI))
                            .font_medium()
                            .text_color(fg)
                            .child(root_name),
                    )
                    .when_some(self.git_branch.clone(), |el, branch| {
                        let ahead = core_workspace::ahead_behind(&self.root)
                            .map(|(ahead, _)| ahead)
                            .unwrap_or(0);
                        el.child(
                            h_flex()
                                .items_center()
                                .gap_1()
                                .text_size(px(tokens::UI_SM))
                                .child(
                                    Icon::new(gpui_kit::assets::IconName::GitBranch)
                                        .small()
                                        .text_color(green),
                                )
                                .child(div().text_color(fg).child(branch))
                                .when(has_changes, |el| {
                                    el.child(
                                        div()
                                            .text_size(px(8.))
                                            .text_color(cx.theme().primary)
                                            .child("\u{25cf}"),
                                    )
                                })
                                .when(ahead > 0, |el| {
                                    el.child(
                                        div()
                                            .text_color(info)
                                            .child(format!("\u{2191}{ahead}")),
                                    )
                                }),
                        )
                    })
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
                    .flex_1()
                    .justify_end()
                    .items_center()
                    .gap_1()
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .px(px(tokens::SPACE_2))
                            .text_size(px(tokens::UI_SM))
                            .text_color(muted)
                            .child(Icon::new(gpui_kit::assets::IconName::Monitor).small())
                            .child("macOS"),
                    )
                    .when_some(active_file_name, |el, file| {
                        el.child(
                            div()
                                .h(px(24.))
                                .px(px(tokens::SPACE_3))
                                .flex()
                                .items_center()
                                .gap(px(tokens::SPACE_2))
                                .rounded(px(tokens::RADIUS_PILL))
                                .bg(cx.theme().secondary)
                                .border_1()
                                .border_color(cx.theme().border)
                                .text_size(px(tokens::UI_SM))
                                .text_color(fg)
                                .child(
                                    Icon::new(gpui_kit::assets::IconName::Feather)
                                        .xsmall()
                                        .text_color(info),
                                )
                                .child(file),
                        )
                    })
                    .child(
                        h_flex()
                            .h(px(28.))
                            .px(px(tokens::SPACE_1))
                            .items_center()
                            .gap(px(tokens::SPACE_1))
                            .rounded(px(tokens::RADIUS_PANEL))
                            .bg(rgba(0xffffff05))
                            .border_1()
                            .border_color(cx.theme().border.opacity(0.6))
                            .child(
                                icon_button(
                                    "run",
                                    gpui_kit::assets::IconName::Play,
                                    Some(green),
                                    "Run project",
                                )
                                .on_click(cx.listener(|this, _event, window, cx| {
                                    this.terminal_open = true;
                                    window.focus(&this.terminal_focus, cx);
                                    let cmd = if this.root.join("pubspec.yaml").is_file() {
                                        "flutter run".to_string()
                                    } else if this.root.join("Cargo.toml").is_file() {
                                        "cargo check".to_string()
                                    } else {
                                        "ls -la".to_string()
                                    };
                                    this.execute_terminal_command(cmd, cx);
                                })),
                            )
                            .child(icon_button(
                                "debug",
                                gpui_kit::assets::IconName::Bug,
                                Some(muted),
                                "Debug project",
                            ))
                            .child(div().w(px(1.)).h(px(12.)).bg(cx.theme().border))
                            .child(icon_button(
                                "fast",
                                gpui_kit::assets::IconName::Zap,
                                Some(green),
                                "Hot reload",
                            )),
                    )
                    .child(icon_button(
                        "agent-wand",
                        gpui_kit::assets::IconName::WandSparkles,
                        Some(info),
                        "Inline assistant",
                    ))
                    .child(
                        Button::new("quick-open")
                            .ghost()
                            .small()
                            .icon(
                                Icon::new(gpui_kit::assets::IconName::Search).text_color(muted),
                            )
                            .tooltip("Go to file (cmd/ctrl+P)")
                            .on_click(cx.listener(|this, _event, window, cx| {
                                this.palette_open = true;
                                this.palette_selected = 0;
                                this.palette_input
                                    .update(cx, |state, cx| state.focus(window, cx));
                                cx.notify();
                            })),
                    )
                    .child(icon_button(
                        "settings",
                        gpui_kit::assets::IconName::Settings,
                        Some(muted),
                        "Settings",
                    )),
            )
    }
}
