//! The Flutter panel: a rail beside the editor for everything Flutter in
//! this project. Like Xcode's device window, it pairs a searchable list (the project,
//! the running app, devices, iOS Simulators) with the selection's details
//! and actions, stacked to fit the rail. Every action runs a command that
//! already exists in the palette, so the panel adds no new behaviour,
//! only a place to find it.

use std::collections::HashSet;
use std::path::Path;

use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, tokens};

/// What is selected in the panel's list.
#[derive(Clone, PartialEq, Eq, Default)]
pub(crate) enum PanelItem {
    #[default]
    Project,
    /// A device `flutter devices` reported, by id.
    Device(String),
    /// An iOS Simulator, by UDID (booted or not).
    Simulator(String),
    /// The running app's widget tree.
    Inspector,
    /// DevTools pages for the running app.
    DevTools,
}

/// What `flutter --version --machine` said, loaded once when the panel opens.
#[derive(Clone)]
pub(crate) enum SdkInfo {
    Loading,
    Found(core_flutter::FlutterSdk),
    Missing(String),
}

pub(crate) struct FlutterPanelState {
    pub(crate) open: bool,
    pub(crate) selected: PanelItem,
    pub(crate) search: Entity<InputState>,
    pub(crate) sdk: Option<SdkInfo>,
    /// The running app's widget tree, once fetched.
    pub(crate) tree: Option<core_flutter::WidgetNode>,
    /// The daemon request for the tree, while its reply is awaited.
    pub(crate) tree_request: Option<i64>,
    pub(crate) tree_error: Option<String>,
    /// Tree nodes the developer folded, by their index path ("0/2/1").
    pub(crate) collapsed: HashSet<String>,
    /// The daemon request that starts the DevTools server.
    pub(crate) devtools_request: Option<i64>,
    /// `http://host:port` of the DevTools server, once it is up.
    pub(crate) devtools_base: Option<String>,
    /// The DevTools page to open as soon as the server answers.
    pub(crate) devtools_page: Option<&'static str>,
}

impl FlutterPanelState {
    pub(crate) fn new(window: &mut Window, cx: &mut App) -> Self {
        Self {
            open: false,
            selected: PanelItem::Project,
            search: cx.new(|cx| InputState::new(window, cx).placeholder("Search")),
            sdk: None,
            tree: None,
            tree_request: None,
            tree_error: None,
            collapsed: HashSet::new(),
            devtools_request: None,
            devtools_base: None,
            devtools_page: None,
        }
    }
}

/// `name:` and `description:` from a pubspec, and how many packages sit
/// directly under `dependencies:`, without a YAML parser.
fn pubspec_summary(root: &Path) -> (Option<String>, Option<String>, usize) {
    let Ok(text) = std::fs::read_to_string(root.join("pubspec.yaml")) else {
        return (None, None, 0);
    };
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
            .filter(|v| !v.is_empty())
    };
    let mut in_deps = false;
    let mut deps = 0;
    for line in text.lines() {
        if !line.starts_with(' ') && !line.trim().is_empty() {
            in_deps = line.trim_end() == "dependencies:";
            continue;
        }
        if in_deps && line.starts_with("  ") && !line.starts_with("   ") && line.contains(':') {
            deps += 1;
        }
    }
    (field("name:"), field("description:"), deps)
}

/// "iOS 18.5" -> "18.5"; anything else unchanged.
fn runtime_version(runtime: &str) -> String {
    runtime.rsplit(' ').next().unwrap_or(runtime).to_string()
}

fn platform_label(platform: &str) -> (&'static str, IconName) {
    if platform.contains("ios") {
        ("iPhone", IconName::Smartphone)
    } else if platform.contains("android") {
        ("Android", IconName::Smartphone)
    } else if platform.contains("web") {
        ("Web", IconName::Globe)
    } else {
        ("Desktop", IconName::Monitor)
    }
}

impl DartIde {
    pub(crate) fn toggle_flutter_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flutter_panel.open = !self.flutter_panel.open;
        if self.flutter_panel.open {
            self.refresh_devices(cx);
            self.flutter_panel.search.update(cx, |s, cx| s.focus(window, cx));
            if self.flutter_panel.sdk.is_none() {
                self.load_flutter_sdk_info(cx);
            }
        }
        cx.notify();
    }

    /// Ask the project's Flutter (an `.fvm` pin first, then `PATH`) for its
    /// version, off the UI thread.
    fn load_flutter_sdk_info(&mut self, cx: &mut Context<Self>) {
        self.flutter_panel.sdk = Some(SdkInfo::Loading);
        let pinned = self.root.join(".fvm/flutter_sdk/bin/flutter");
        cx.spawn(async move |weak, cx| {
            let info = cx
                .background_executor()
                .spawn(async move {
                    let bin = if pinned.is_file() {
                        Some(pinned)
                    } else {
                        crate::lsp_health::find_executable("flutter")
                    };
                    let Some(bin) = bin else {
                        return SdkInfo::Missing("Flutter isn't on PATH".into());
                    };
                    match std::process::Command::new(&bin).args(["--version", "--machine"]).output() {
                        Ok(out) if out.status.success() => {
                            let text = String::from_utf8_lossy(&out.stdout);
                            // Warnings can precede the JSON object.
                            let json = text.find('{').map(|i| &text[i..]).unwrap_or(&text);
                            match serde_json::from_str::<core_flutter::FlutterSdk>(json) {
                                Ok(sdk) => SdkInfo::Found(sdk),
                                Err(e) => SdkInfo::Missing(format!("Couldn't read the version: {e}")),
                            }
                        }
                        Ok(out) => SdkInfo::Missing(String::from_utf8_lossy(&out.stderr).trim().to_string()),
                        Err(e) => SdkInfo::Missing(e.to_string()),
                    }
                })
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.flutter_panel.sdk = Some(info);
                cx.notify();
            });
        })
        .detach();
    }

    /// Run a palette command from the panel. The panel stays open (its
    /// status strip shows what the command is doing); only the developer closes it.
    fn panel_run(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.execute_ide_command(id, window, cx);
        cx.notify();
    }

    /// The panel as a rail beside the editor, so the code stays in view:
    /// the list on top, the selection's details under it, and the session's
    /// status at the foot.
    pub(crate) fn render_flutter_rail(&self, cx: &mut Context<Self>) -> Div {
        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(px(tokens::RADIUS_CARD))
            .overflow_hidden()
            .child(self.render_flutter_panel_list(cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_h(px(0.))
                    .bg(cx.theme().background)
                    .child(self.render_flutter_panel_detail(cx)),
            )
            .child(self.render_panel_status(cx))
    }

    fn render_flutter_panel_list(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let query = self.flutter_panel.search.read(cx).value().trim().to_lowercase();
        let matches = |name: &str| query.is_empty() || name.to_lowercase().contains(&query);
        let (pub_name, _, _) = pubspec_summary(&self.root);
        let project_name = pub_name.unwrap_or_else(|| {
            self.root.file_name().and_then(|n| n.to_str()).unwrap_or("Project").to_string()
        });
        let sdk_version = match &self.flutter_panel.sdk {
            Some(SdkInfo::Found(sdk)) => sdk.flutter_version.clone(),
            _ => String::new(),
        };
        let target = self.selected_device.as_ref().map(|d| d.id.clone());

        let heading = |label: &'static str| {
            div()
                .px(px(tokens::SPACE_4))
                .pt(px(tokens::SPACE_5))
                .pb(px(tokens::SPACE_2))
                .text_size(px(tokens::UI_SM))
                .text_color(muted)
                .child(label)
        };

        let mut list = v_flex().w_full().gap(px(2.));
        if matches(&project_name) {
            list = list.child(heading("Project")).child(self.panel_row(
                PanelItem::Project,
                IconName::Folder,
                project_name.clone(),
                "Flutter project".into(),
                sdk_version,
                false,
                cx,
            ));
        }

        // The running app's tools.
        let running = self.can_inspect();
        let app_sub = |text: &'static str| if running { text.to_string() } else { "App not running".to_string() };
        if matches("widget inspector tree") || matches("devtools") {
            list = list
                .child(heading("Running app"))
                .child(self.panel_row(
                    PanelItem::Inspector,
                    IconName::Inspector,
                    "Widget Inspector".into(),
                    app_sub("Widget tree"),
                    String::new(),
                    false,
                    cx,
                ))
                .child(self.panel_row(
                    PanelItem::DevTools,
                    IconName::LayoutDashboard,
                    "DevTools".into(),
                    app_sub("Performance, memory, network"),
                    String::new(),
                    false,
                    cx,
                ));
        }

        // Connected targets, minus iOS Simulators (they are listed below,
        // booted or not, so they are not shown twice).
        let sim_ids: Vec<&str> = self.ios_simulators.iter().map(|s| s.udid.as_str()).collect();
        let devices: Vec<_> = self
            .devices
            .iter()
            .filter(|d| !sim_ids.contains(&d.id.as_str()) && matches(&d.name))
            .cloned()
            .collect();
        if !devices.is_empty() {
            list = list.child(heading("Devices"));
            for d in devices {
                let (kind, icon) = platform_label(&d.target_platform);
                let subtitle = if d.emulator { format!("{kind} emulator") } else { kind.to_string() };
                let is_target = target.as_deref() == Some(d.id.as_str());
                list = list.child(self.panel_row(
                    PanelItem::Device(d.id.clone()),
                    icon,
                    d.name.clone(),
                    subtitle,
                    String::new(),
                    is_target,
                    cx,
                ));
            }
        }

        let mut sims: Vec<_> = self.ios_simulators.iter().filter(|s| matches(&s.name)).cloned().collect();
        // Booted first, then newest runtime.
        sims.sort_by(|a, b| b.is_booted().cmp(&a.is_booted()).then(b.runtime.cmp(&a.runtime)));
        if !sims.is_empty() {
            list = list.child(heading("iOS Simulators"));
            for s in sims {
                let is_target = target.as_deref() == Some(s.udid.as_str());
                list = list.child(self.panel_row(
                    PanelItem::Simulator(s.udid.clone()),
                    IconName::Smartphone,
                    s.name.clone(),
                    if s.is_booted() { "Booted".into() } else { "Simulator".into() },
                    runtime_version(&s.runtime),
                    is_target,
                    cx,
                ));
            }
        }

        v_flex()
            .w_full()
            .flex_shrink_0()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .h(px(44.))
                    .px(px(tokens::SPACE_5))
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(tokens::UI_LG))
                            .font_medium()
                            .text_color(cx.theme().foreground)
                            .child("Flutter"),
                    )
                    .child(
                        h_flex()
                            .gap(px(2.))
                            .child(
                        Button::new("flutter-panel-close")
                            .ghost()
                            .small()
                            .icon(Icon::new(IconName::X).with_size(px(13.)).text_color(muted))
                            .tooltip("Close the Flutter panel (\u{2318}\u{21e7}D)")
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.flutter_panel.open = false;
                                cx.notify();
                            })),
                            )
                            .child(
                        Button::new("flutter-panel-refresh")
                            .ghost()
                            .small()
                            .icon(Icon::new(IconName::RefreshCw).with_size(px(13.)).text_color(muted))
                            .tooltip("Refresh devices and SDK")
                            .on_click(cx.listener(|this, _e, _w, cx| {
                                this.refresh_devices(cx);
                                this.load_flutter_sdk_info(cx);
                            })),
                            ),
                    ),
            )
            .child(
                div()
                    .px(px(tokens::SPACE_4))
                    .child(Input::new(&self.flutter_panel.search).small().cleanable(true)),
            )
            .child(
                div()
                    .id("flutter-panel-list")
                    .max_h(px(260.))
                    .overflow_y_scroll()
                    .px(px(tokens::SPACE_3))
                    .pb(px(tokens::SPACE_4))
                    .child(list),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn panel_row(
        &self,
        item: PanelItem,
        icon: IconName,
        title: String,
        subtitle: String,
        trailing: String,
        is_target: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selected = self.flutter_panel.selected == item;
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let id: SharedString = match &item {
            PanelItem::Project => "panel-row-project".into(),
            PanelItem::Device(id) => format!("panel-row-dev-{id}").into(),
            PanelItem::Simulator(id) => format!("panel-row-sim-{id}").into(),
            PanelItem::Inspector => "panel-row-inspector".into(),
            PanelItem::DevTools => "panel-row-devtools".into(),
        };
        h_flex()
            .id(id)
            .w_full()
            .h(px(48.))
            .px(px(tokens::SPACE_3))
            .gap(px(tokens::SPACE_4))
            .items_center()
            .rounded(px(tokens::RADIUS_PANEL))
            .cursor_pointer()
            .when(selected, |el| el.bg(rgba(0xffffff0f)))
            .when(!selected, |el| el.hover(|s| s.bg(rgba(0xffffff08))))
            .child(
                div()
                    .size(px(32.))
                    .flex_shrink_0()
                    .rounded(px(tokens::RADIUS_PILL))
                    .bg(rgba(0xffffff0a))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(icon).with_size(px(15.)).text_color(if is_target {
                        cx.theme().primary
                    } else {
                        muted
                    })),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .child(div().truncate().text_size(px(tokens::UI)).font_medium().text_color(fg).child(title))
                    .child(div().truncate().text_size(px(tokens::UI_XS)).text_color(muted).child(subtitle)),
            )
            .when(is_target, |el| {
                el.child(div().size(px(6.)).rounded(px(3.)).bg(cx.theme().green))
            })
            .when(!trailing.is_empty(), |el| {
                el.child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(tokens::UI_SM))
                        .text_color(muted)
                        .child(trailing),
                )
            })
            .on_click(cx.listener(move |this, _e, _w, cx| {
                if item == PanelItem::Inspector && this.flutter_panel.tree.is_none() {
                    this.request_widget_tree(cx);
                }
                this.flutter_panel.selected = item.clone();
                cx.notify();
            }))
    }

    fn render_flutter_panel_detail(&self, cx: &mut Context<Self>) -> Div {
        match self.flutter_panel.selected.clone() {
            PanelItem::Project => self.render_panel_project(cx),
            PanelItem::Inspector => self.render_panel_inspector(cx),
            PanelItem::DevTools => self.render_panel_devtools(cx),
            PanelItem::Device(id) => match self.devices.iter().find(|d| d.id == id).cloned() {
                Some(d) => {
                    let (kind, icon) = platform_label(&d.target_platform);
                    self.render_panel_device(d, kind.to_string(), icon, None, cx)
                }
                None => self.render_panel_project(cx),
            },
            PanelItem::Simulator(udid) => {
                match self.ios_simulators.iter().find(|s| s.udid == udid).cloned() {
                    Some(s) => {
                        let device = core_flutter::Device {
                            id: s.udid.clone(),
                            name: s.name.clone(),
                            target_platform: "ios".into(),
                            emulator: true,
                            is_supported: true,
                        };
                        let booted = s.is_booted();
                        self.render_panel_device(device, s.runtime.clone(), IconName::Smartphone, Some(booted), cx)
                    }
                    None => self.render_panel_project(cx),
                }
            }
        }
    }

    /// The right-hand header: title and subtitle, then a toolbar.
    fn panel_header(title: String, subtitle: String, tools: Div, cx: &App) -> Div {
        v_flex()
            .w_full()
            .flex_shrink_0()
            .px(px(tokens::SPACE_5))
            .py(px(tokens::SPACE_4))
            .gap(px(tokens::SPACE_3))
            .border_b_1()
            .border_color(cx.theme().border.opacity(0.6))
            .child(
                v_flex()
                    .min_w(px(0.))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(tokens::UI_LG))
                            .font_medium()
                            .text_color(cx.theme().foreground)
                            .child(title),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(tokens::UI_SM))
                            .text_color(cx.theme().muted_foreground)
                            .child(subtitle),
                    ),
            )
            .child(tools)
    }

    /// A group of icon buttons in one rounded capsule, like Xcode's toolbar.
    fn tool_capsule(cx: &App) -> Div {
        h_flex()
            .h(px(32.))
            .px(px(tokens::SPACE_2))
            .gap(px(2.))
            .items_center()
            .rounded(px(tokens::RADIUS_PILL))
            .border_1()
            .border_color(cx.theme().border)
            .bg(rgba(0xffffff06))
    }

    fn render_panel_device(
        &self,
        device: core_flutter::Device,
        subtitle: String,
        icon: IconName,
        booted: Option<bool>,
        cx: &mut Context<Self>,
    ) -> Div {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let is_target = self.selected_device.as_ref().is_some_and(|d| d.id == device.id);
        let running_here = self.flutter_daemon.is_some() && is_target;
        let is_sim = booted.is_some();
        let available = booted.unwrap_or(true);

        let tool = |id: &'static str, icon: IconName, tip: &'static str, enabled: bool| {
            Button::new(id)
                .ghost()
                .small()
                .disabled(!enabled)
                .icon(Icon::new(icon).with_size(px(14.)).text_color(muted))
                .tooltip(tip)
        };
        let tools = h_flex()
            .gap(px(tokens::SPACE_3))
            .child(
                Self::tool_capsule(cx)
                    .child(
                        tool("panel-reload", IconName::Zap, "Hot reload", running_here)
                            .on_click(cx.listener(|this, _e, _w, cx| this.trigger_hot_reload(cx))),
                    )
                    .child(
                        tool("panel-restart", IconName::RotateCw, "Hot restart", running_here)
                            .on_click(cx.listener(|this, _e, _w, cx| this.trigger_hot_restart(cx))),
                    )
                    .child(
                        tool("panel-stop", IconName::Square, "Stop the app", running_here)
                            .on_click(cx.listener(|this, _e, _w, cx| this.stop_flutter_run(cx))),
                    ),
            )
            .child(
                Self::tool_capsule(cx)
                    .child(
                        tool("panel-perf", IconName::Scale, "Performance overlay", running_here)
                            .on_click(cx.listener(|this, _e, w, cx| this.panel_run("toggle-perf-overlay", w, cx))),
                    )
                    .child(
                        tool("panel-paint", IconName::Palette, "Debug paint", running_here)
                            .on_click(cx.listener(|this, _e, w, cx| this.panel_run("toggle-debug-paint", w, cx))),
                    )
                    .child(
                        tool("panel-devtools", IconName::ExternalLink, "Open DevTools", running_here)
                            .on_click(cx.listener(|this, _e, w, cx| this.panel_run("open-devtools", w, cx))),
                    ),
            );

        let status = if running_here {
            "Your app is running here.".to_string()
        } else if !available {
            "Shut down. Running here boots it first.".to_string()
        } else if is_target {
            "Selected as the run target.".to_string()
        } else {
            "Ready to run your app.".to_string()
        };

        let dev_for_target = device.clone();
        let dev_for_run = device.clone();
        let primary_label = if running_here {
            "Running"
        } else if is_sim && !available {
            "Boot & run here"
        } else {
            "Run here"
        };

        // The device itself, drawn: a phone outline for phones, a large
        // glyph for desktop and web targets.
        let art = if matches!(icon, IconName::Smartphone) {
            div()
                .w(px(110.))
                .h(px(220.))
                .rounded(px(24.))
                .border(px(4.))
                .border_color(rgba(0xd9dce3ff))
                .bg(if available { rgba(0x4a90d9ff) } else { rgba(0x2a2d33ff) })
                .shadow_lg()
                .relative()
                .child(
                    div()
                        .absolute()
                        .top(px(7.))
                        .left(px(36.))
                        .w(px(30.))
                        .h(px(9.))
                        .rounded(px(tokens::RADIUS_PILL))
                        .bg(rgba(0x0b0b0dff)),
                )
                .when(running_here, |el| {
                    el.child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(IconName::Play).with_size(px(36.)).text_color(rgba(0xffffffcc))),
                    )
                })
        } else {
            div()
                .size(px(120.))
                .rounded(px(tokens::RADIUS_CARD + 6.))
                .bg(rgba(0xffffff08))
                .border_1()
                .border_color(cx.theme().border)
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(icon).with_size(px(48.)).text_color(muted))
        };

        v_flex()
            .flex_1()
            .h_full()
            .min_w(px(0.))
            .child(Self::panel_header(device.name.clone(), subtitle, tools, cx))
            .child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap(px(tokens::SPACE_6))
                    .child(art)
                    .child(
                        v_flex()
                            .items_center()
                            .gap(px(tokens::SPACE_2))
                            .child(div().text_size(px(20.)).font_medium().text_color(fg).child(device.name.clone()))
                            .child(div().text_size(px(tokens::UI)).text_color(muted).child(status)),
                    )
                    .child(
                        h_flex()
                            .gap(px(tokens::SPACE_3))
                            .when(!is_target, |el| {
                                el.child(
                                    Button::new("panel-set-target")
                                        .small()
                                        .ghost()
                                        .label("Use as target")
                                        .on_click(cx.listener(move |this, _e, window, cx| {
                                            this.selected_device = Some(dev_for_target.clone());
                                            if dev_for_target.is_ios() {
                                                this.attach_simulator(window, cx);
                                            }
                                            cx.notify();
                                        })),
                                )
                            })
                            .child(
                                Button::new("panel-run-here")
                                    .small()
                                    .primary()
                                    .disabled(running_here)
                                    .icon(Icon::new(IconName::Play).with_size(px(12.)))
                                    .label(primary_label)
                                    .on_click(cx.listener(move |this, _e, window, cx| {
                                        this.selected_device = Some(dev_for_run.clone());
                                        if dev_for_run.is_ios() {
                                            this.attach_simulator(window, cx);
                                        }
                                        this.start_flutter_run(window, cx);
                                    })),
                            ),
                    ),
            )
    }

    fn render_panel_project(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let border = cx.theme().border;
        let primary = cx.theme().primary;
        let (name, description, deps) = pubspec_summary(&self.root);
        let name = name.unwrap_or_else(|| {
            self.root.file_name().and_then(|n| n.to_str()).unwrap_or("Project").to_string()
        });

        let fact = |label: &'static str, value: String| {
            v_flex()
                .flex_basis(px(120.))
                .flex_grow(1.)
                .min_w(px(0.))
                .p(px(tokens::SPACE_4))
                .gap(px(tokens::SPACE_1))
                .rounded(px(tokens::RADIUS_PANEL))
                .bg(rgba(0xffffff06))
                .border_1()
                .border_color(border.opacity(0.7))
                .child(div().text_size(px(tokens::UI_XS)).text_color(muted).child(label))
                .child(div().truncate().text_size(px(tokens::UI)).font_medium().text_color(fg).child(value))
        };
        let (version, channel, dart) = match &self.flutter_panel.sdk {
            Some(SdkInfo::Found(sdk)) => (
                sdk.flutter_version.clone(),
                sdk.channel.clone(),
                sdk.dart_sdk_version.split(' ').next().unwrap_or("").to_string(),
            ),
            Some(SdkInfo::Missing(_)) => ("Not found".into(), "\u{2013}".into(), "\u{2013}".into()),
            _ => ("Checking\u{2026}".into(), "\u{2026}".into(), "\u{2026}".into()),
        };
        let sdk_problem = match &self.flutter_panel.sdk {
            Some(SdkInfo::Missing(why)) => Some(why.clone()),
            _ => None,
        };
        let pinned = self.root.join(".fvm/flutter_sdk").exists();

        // Each tile runs an existing palette command.
        let tile = |id: &'static str, icon: IconName, title: &'static str, hint: &'static str| {
            h_flex()
                .id(id)
                .flex_basis(px(140.)).flex_grow(1.)
                .h(px(56.))
                .px(px(tokens::SPACE_4))
                .gap(px(tokens::SPACE_3))
                .items_center()
                .rounded(px(tokens::RADIUS_PANEL))
                .border_1()
                .border_color(border.opacity(0.7))
                .cursor_pointer()
                .hover(|s| s.bg(rgba(0xffffff08)).border_color(rgba(0xea7a3c66)))
                .child(Icon::new(icon).with_size(px(15.)).text_color(primary))
                .child(
                    v_flex()
                        .min_w(px(0.))
                        .child(div().text_size(px(tokens::UI_SM)).font_medium().text_color(fg).child(title))
                        .child(div().truncate().text_size(px(tokens::UI_XS)).text_color(muted).child(hint)),
                )
                .on_click(cx.listener(move |this, _e, w, cx| this.panel_run(id, w, cx)))
        };
        let packages = vec![
            tile("pub-get", IconName::ArrowDown, "Get packages", "flutter pub get"),
            tile("pub-upgrade", IconName::ArrowUp, "Upgrade", "flutter pub upgrade"),
            tile("add-package", IconName::Plus, "Add package", "Search pub.dev"),
            tile("flutter-clean", IconName::Trash, "Clean", "clean, then pub get"),
        ];
        let codegen = vec![
            tile("build-runner", IconName::WandSparkles, "build_runner", "Generate once"),
            tile("build-runner-watch", IconName::Eye, "Watch", "Regenerate on save"),
            tile("gen-l10n", IconName::Globe, "Localizations", "flutter gen-l10n"),
        ];
        let run = vec![
            tile("run-flutter", IconName::Play, "Run app", "On the selected target"),
            tile("test-all", IconName::FlaskConical, "Run all tests", "flutter test"),
            tile("test-rerun-failed", IconName::RotateCcw, "Re-run failed", "Only what failed"),
            tile("test-coverage", IconName::Scale, "Coverage", "Tests + covered lines"),
            tile("debug-dap", IconName::Bug, "Debug", "Start the debugger"),
        ];
        let group = |label: &'static str, tiles: Vec<Stateful<Div>>| {
            v_flex()
                .gap(px(tokens::SPACE_3))
                .child(div().text_size(px(tokens::UI_SM)).text_color(muted).child(label))
                .child(h_flex().flex_wrap().gap(px(tokens::SPACE_3)).children(tiles))
        };

        let tools = h_flex().child(
            Self::tool_capsule(cx)
                .child(
                    Button::new("panel-doctor")
                        .ghost()
                        .small()
                        .icon(Icon::new(IconName::CircleCheck).with_size(px(14.)).text_color(muted))
                        .tooltip("Run flutter doctor")
                        .on_click(cx.listener(|this, _e, w, cx| this.panel_run("flutter-doctor", w, cx))),
                )
                .child(
                    Button::new("panel-new-project")
                        .ghost()
                        .small()
                        .icon(Icon::new(IconName::Plus).with_size(px(14.)).text_color(muted))
                        .tooltip("New Flutter project")
                        .on_click(cx.listener(|this, _e, w, cx| this.panel_run("new-flutter-project", w, cx))),
                ),
        );

        v_flex()
            .flex_1()
            .h_full()
            .min_w(px(0.))
            .child(Self::panel_header(
                name,
                description.unwrap_or_else(|| format!("{deps} dependencies")),
                tools,
                cx,
            ))
            .child(
                v_flex()
                    .id("flutter-panel-project")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .p(px(tokens::SPACE_6))
                    .gap(px(tokens::SPACE_6))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap(px(tokens::SPACE_3))
                            .child(fact(if pinned { "Flutter (fvm)" } else { "Flutter" }, version))
                            .child(fact("Channel", channel))
                            .child(fact("Dart", dart))
                            .child(fact("Dependencies", deps.to_string())),
                    )
                    .when_some(sdk_problem, |el, why| {
                        el.child(
                            h_flex()
                                .gap(px(tokens::SPACE_2))
                                .text_size(px(tokens::UI_SM))
                                .text_color(cx.theme().warning)
                                .child(Icon::new(IconName::TriangleAlert).with_size(px(13.)))
                                .child(div().truncate().child(why)),
                        )
                    })
                    .child(group("Packages", packages))
                    .child(group("Code generation", codegen))
                    .child(group("Run & test", run)),
            )
    }

    /// The strip along the bottom: what the Flutter session is doing right
    /// now (building, running, stopped), so the panel can stay open through
    /// a build. Stop and the debug console sit at its right.
    fn render_panel_status(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let running = self.flutter_daemon.is_some();
        let busy = crate::chrome::titlebar::flutter_session_busy(running, self.flutter_status.as_deref());
        let device = self.selected_device.as_ref().map(|d| d.name.clone()).unwrap_or_default();
        let (tone, text) = if busy {
            (cx.theme().warning, self.flutter_status.clone().unwrap_or_else(|| "Building\u{2026}".into()))
        } else if running {
            (cx.theme().green, format!("Running on {device}"))
        } else {
            (muted.opacity(0.6), format!("Not running \u{b7} target: {device}"))
        };
        h_flex()
            .w_full()
            .h(px(34.))
            .flex_shrink_0()
            .px(px(tokens::SPACE_5))
            .gap(px(tokens::SPACE_3))
            .items_center()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .text_size(px(tokens::UI_SM))
            .child(div().size(px(7.)).rounded(px(4.)).bg(tone))
            .child(div().flex_1().min_w(px(0.)).truncate().text_color(if busy || running { cx.theme().foreground } else { muted }).child(text))
            .child(
                Button::new("panel-status-console")
                    .ghost()
                    .xsmall()
                    .label("Console")
                    .tooltip("Show the debug console")
                    .on_click(cx.listener(|this, _e, _w, cx| {
                        this.terminal_open = true;
                        this.dock_tab = crate::DockTab::DebugConsole;
                        cx.notify();
                    })),
            )
            .when(running, |el| {
                el.child(
                    Button::new("panel-status-stop")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(IconName::Square).with_size(px(11.)).text_color(cx.theme().red))
                        .label("Stop")
                        .on_click(cx.listener(|this, _e, _w, cx| this.stop_flutter_run(cx))),
                )
            })
            .when(!running, |el| {
                el.child(
                    Button::new("panel-status-run")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(IconName::Play).with_size(px(11.)).text_color(cx.theme().green))
                        .label("Run")
                        .on_click(cx.listener(|this, _e, window, cx| this.start_flutter_run(window, cx))),
                )
            })
    }

    /// Ask the running app for its widget tree (the reply lands in
    /// [`Self::handle_panel_response`]).
    pub(crate) fn request_widget_tree(&mut self, cx: &mut Context<Self>) {
        let (Some(daemon), Some(app_id)) = (self.flutter_daemon.clone(), self.flutter_app_id.clone()) else {
            return;
        };
        self.flutter_panel.tree_error = None;
        match daemon.lock().map(|d| d.inspector_request_tree(&app_id, &self.root)) {
            Ok(Ok(id)) => self.flutter_panel.tree_request = Some(id),
            _ => self.flutter_panel.tree_error = Some("Couldn't reach the app's inspector.".into()),
        }
        cx.notify();
    }

    /// A daemon reply the panel asked for. Returns whether it was one.
    pub(crate) fn handle_panel_response(
        &mut self,
        id: i64,
        result: &Result<serde_json::Value, serde_json::Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.flutter_panel.tree_request == Some(id) {
            self.flutter_panel.tree_request = None;
            match result.as_ref().ok().and_then(core_flutter::parse_widget_tree) {
                Some(tree) => {
                    self.flutter_panel.tree = Some(tree);
                    self.flutter_panel.tree_error = None;
                }
                None => {
                    self.flutter_panel.tree_error = Some(
                        "The app returned no widget tree. Is it running in debug mode?".into(),
                    )
                }
            }
            cx.notify();
            return true;
        }
        if self.flutter_panel.devtools_request == Some(id) {
            self.flutter_panel.devtools_request = None;
            let base = result.as_ref().ok().and_then(|v| {
                let host = v.get("host")?.as_str()?;
                let port = v.get("port")?.as_u64()?;
                Some(format!("http://{host}:{port}"))
            });
            match (base, self.flutter_panel.devtools_page.take()) {
                (Some(base), page) => {
                    self.flutter_panel.devtools_base = Some(base);
                    if let Some(page) = page {
                        self.open_devtools_page(page, window, cx);
                    }
                }
                // This Flutter can't serve DevTools through the daemon:
                // fall back to the browser.
                (None, _) => self.open_flutter_devtools(cx),
            }
            cx.notify();
            return true;
        }
        false
    }

    /// Open one DevTools page for the running app in the web preview rail,
    /// starting the DevTools server first if needed.
    fn open_devtools_page(&mut self, page: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.flutter_ws_uri.clone() else {
            return;
        };
        let Some(base) = self.flutter_panel.devtools_base.clone() else {
            let Some(daemon) = self.flutter_daemon.clone() else {
                return;
            };
            self.flutter_panel.devtools_page = Some(page);
            if let Ok(Ok(id)) = daemon.lock().map(|d| d.devtools_serve()) {
                self.flutter_panel.devtools_request = Some(id);
            } else {
                self.open_flutter_devtools(cx);
            }
            cx.notify();
            return;
        };
        let url = format!("{base}/{page}?uri={}", encode_query(&ws));
        self.web_preview_url.update(cx, |s, cx| s.set_value(url, window, cx));
        self.web_preview_open = true;
        self.web_preview_go(window, cx);
        cx.notify();
    }

    /// Open the file a widget was created in, at its line.
    fn open_widget_source(&mut self, path: std::path::PathBuf, line: u32, column: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.open_path(path, window, cx);
        if let Some(ed) = self.active_editor().cloned() {
            ed.update(cx, |editor, cx| {
                editor.set_cursor_to_position(line.saturating_sub(1), column.saturating_sub(1), cx)
            });
        }
        cx.notify();
    }

    /// An empty state for the running-app views: what is missing and a way to fix it.
    fn panel_not_running(&self, what: &'static str, cx: &mut Context<Self>) -> Div {
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(px(tokens::SPACE_4))
            .child(Icon::new(IconName::Play).with_size(px(36.)).text_color(cx.theme().muted_foreground))
            .child(
                div()
                    .text_size(px(tokens::UI))
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Run your app to see its {what}.")),
            )
            .child(
                Button::new(("panel-run-for", what.len()))
                    .small()
                    .primary()
                    .icon(Icon::new(IconName::Play).with_size(px(12.)))
                    .label("Run app")
                    .on_click(cx.listener(|this, _e, window, cx| {
                        this.start_flutter_run(window, cx);
                    })),
            )
    }

    fn render_panel_inspector(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let running = self.can_inspect();
        let selecting = self.sim.inspect;
        let loading = self.flutter_panel.tree_request.is_some();
        let count = self.flutter_panel.tree.as_ref().map_or(0, |t| t.count().saturating_sub(1));

        let tools = h_flex().child(
            Self::tool_capsule(cx)
                .child(
                    Button::new("panel-tree-refresh")
                        .ghost()
                        .small()
                        .disabled(!running || loading)
                        .icon(Icon::new(IconName::RefreshCw).with_size(px(14.)).text_color(muted))
                        .tooltip("Reload the widget tree")
                        .on_click(cx.listener(|this, _e, _w, cx| this.request_widget_tree(cx))),
                )
                .child(
                    Button::new("panel-tree-select")
                        .ghost()
                        .small()
                        .disabled(!running)
                        .icon(
                            Icon::new(IconName::MousePointerClick)
                                .with_size(px(14.))
                                .text_color(if selecting { cx.theme().primary } else { muted }),
                        )
                        .tooltip(if selecting {
                            "Stop selecting widgets on the device"
                        } else {
                            "Select a widget by tapping it on the device"
                        })
                        .on_click(cx.listener(|this, _e, _w, cx| this.toggle_inspect(cx))),
                )
                .child(
                    Button::new("panel-tree-fold")
                        .ghost()
                        .small()
                        .disabled(self.flutter_panel.tree.is_none())
                        .icon(Icon::new(IconName::ChevronsUpDown).with_size(px(14.)).text_color(muted))
                        .tooltip(if self.flutter_panel.collapsed.is_empty() {
                            "Collapse all"
                        } else {
                            "Expand all"
                        })
                        .on_click(cx.listener(|this, _e, _w, cx| {
                            if this.flutter_panel.collapsed.is_empty() {
                                // Fold every node that has children, below the top level.
                                let mut keys = Vec::new();
                                if let Some(tree) = &this.flutter_panel.tree {
                                    for (ix, child) in tree.children.iter().enumerate() {
                                        collect_folds(child, ix.to_string(), &mut keys);
                                    }
                                }
                                this.flutter_panel.collapsed = keys.into_iter().collect();
                            } else {
                                this.flutter_panel.collapsed.clear();
                            }
                            cx.notify();
                        })),
                ),
        );

        let subtitle = if !running {
            "App not running".to_string()
        } else if loading {
            "Loading\u{2026}".to_string()
        } else if selecting {
            "Tap a widget on the device to jump to its code".to_string()
        } else if count > 0 {
            format!("{count} widgets from your code")
        } else {
            "Widget tree".to_string()
        };

        let body = if !running {
            self.panel_not_running("widget tree", cx)
        } else if let Some(err) = self.flutter_panel.tree_error.clone() {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(tokens::SPACE_2))
                .text_color(cx.theme().warning)
                .child(Icon::new(IconName::TriangleAlert).with_size(px(20.)))
                .child(div().text_size(px(tokens::UI)).child(err))
        } else if let Some(tree) = &self.flutter_panel.tree {
            let inspected = self.sim.inspected.clone();
            let mut rows: Vec<AnyElement> = Vec::new();
            for (ix, child) in tree.children.iter().enumerate() {
                self.tree_rows(child, ix.to_string(), 0, inspected.as_ref(), &mut rows, cx);
            }
            v_flex()
                .flex_1()
                .min_h(px(0.))
                .child(
                    div()
                        .id("panel-tree")
                        .flex_1()
                        .min_h(px(0.))
                        .overflow_y_scroll()
                        .py(px(tokens::SPACE_3))
                        .px(px(tokens::SPACE_4))
                        .font_family(cx.theme().mono_font_family.clone())
                        .children(rows),
                )
        } else {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_size(px(tokens::UI))
                .text_color(muted)
                .child("Loading the widget tree\u{2026}")
        };
        let _ = fg;

        v_flex()
            .flex_1()
            .h_full()
            .min_w(px(0.))
            .child(Self::panel_header("Widget Inspector".into(), subtitle, tools, cx))
            .child(body)
    }

    /// One row per visible tree node: indent, fold chevron, widget name, and
    /// where it is written (for the project's own widgets).
    fn tree_rows(
        &self,
        node: &core_flutter::WidgetNode,
        key: String,
        depth: usize,
        inspected: Option<&core_flutter::WidgetLocation>,
        rows: &mut Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let folded = self.flutter_panel.collapsed.contains(&key);
        let has_children = !node.children.is_empty();
        let is_selected = match (inspected, &node.location) {
            (Some(sel), Some((path, line, _))) => &sel.path == path && sel.line == *line,
            _ => false,
        };
        let where_ = node.location.as_ref().filter(|_| node.local).map(|(path, line, _)| {
            format!(
                "{}:{line}",
                path.file_name().and_then(|n| n.to_str()).unwrap_or("")
            )
        });
        let location = node.location.clone();
        let fold_key = key.clone();
        rows.push(
            h_flex()
                .id(SharedString::from(format!("tree-{key}")))
                .w_full()
                .h(px(24.))
                .pl(px(depth as f32 * 14. + 4.))
                .pr(px(tokens::SPACE_3))
                .gap(px(tokens::SPACE_2))
                .items_center()
                .rounded(px(tokens::RADIUS_SM))
                .cursor_pointer()
                .when(is_selected, |el| el.bg(tokens::accent_dim()))
                .when(!is_selected, |el| el.hover(|s| s.bg(rgba(0xffffff08))))
                .child(
                    div()
                        .id(SharedString::from(format!("tree-fold-{key}")))
                        .size(px(14.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(has_children, |el| {
                            el.child(
                                Icon::new(if folded { IconName::ChevronRight } else { IconName::ChevronDown })
                                    .with_size(px(11.))
                                    .text_color(muted),
                            )
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _e, _w, cx| {
                                cx.stop_propagation();
                                if !this.flutter_panel.collapsed.remove(&fold_key) {
                                    this.flutter_panel.collapsed.insert(fold_key.clone());
                                }
                                cx.notify();
                            }))
                        }),
                )
                .child(
                    div()
                        .text_size(px(tokens::UI_SM))
                        .text_color(if node.local { fg } else { muted })
                        .when(is_selected, |el| el.text_color(cx.theme().primary))
                        .child(node.name.clone()),
                )
                .child(div().flex_1())
                .when_some(where_, |el, w| {
                    el.child(div().text_size(px(tokens::UI_XS)).text_color(muted.opacity(0.7)).child(w))
                })
                .on_click(cx.listener(move |this, _e, window, cx| {
                    if let Some((path, line, col)) = location.clone() {
                        this.open_widget_source(path, line, col, window, cx);
                    }
                }))
                .into_any_element(),
        );
        if folded {
            return;
        }
        for (ix, child) in node.children.iter().enumerate() {
            self.tree_rows(child, format!("{key}/{ix}"), depth + 1, inspected, rows, cx);
        }
    }

    fn render_panel_devtools(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let border = cx.theme().border;
        let primary = cx.theme().primary;
        let running = self.can_inspect() && self.flutter_ws_uri.is_some();
        let starting = self.flutter_panel.devtools_request.is_some();

        let tools = h_flex().child(
            Self::tool_capsule(cx).child(
                Button::new("panel-devtools-browser")
                    .ghost()
                    .small()
                    .disabled(!running)
                    .icon(Icon::new(IconName::ExternalLink).with_size(px(14.)).text_color(muted))
                    .tooltip("Open DevTools in your browser")
                    .on_click(cx.listener(|this, _e, w, cx| this.panel_run("open-devtools", w, cx))),
            ),
        );
        let subtitle = if !running {
            "App not running".to_string()
        } else if starting {
            "Starting DevTools\u{2026}".to_string()
        } else {
            "Opens beside your code, in the preview rail".to_string()
        };

        // DevTools' own pages, by their URL path.
        const PAGES: &[(&str, IconName, &str, &str)] = &[
            ("inspector", IconName::Inspector, "Inspector", "Layout explorer and properties"),
            ("performance", IconName::Zap, "Performance", "Frame times and jank"),
            ("cpu-profiler", IconName::Cpu, "CPU profiler", "Where time is spent"),
            ("memory", IconName::MemoryStick, "Memory", "Heap and leaks"),
            ("network", IconName::Network, "Network", "HTTP requests"),
            ("logging", IconName::FileText, "Logging", "Structured app logs"),
            ("debugger", IconName::Bug, "Debugger", "Breakpoints and stepping"),
        ];
        let tiles: Vec<Stateful<Div>> = PAGES
            .iter()
            .map(|&(page, icon, title, hint)| {
                h_flex()
                    .id(page)
                    .flex_basis(px(140.)).flex_grow(1.)
                    .h(px(64.))
                    .px(px(tokens::SPACE_4))
                    .gap(px(tokens::SPACE_3))
                    .items_center()
                    .rounded(px(tokens::RADIUS_PANEL))
                    .border_1()
                    .border_color(border.opacity(0.7))
                    .when(!running, |el| el.opacity(0.45))
                    .when(running, |el| {
                        el.cursor_pointer()
                            .hover(|s| s.bg(rgba(0xffffff08)).border_color(rgba(0xea7a3c66)))
                            .on_click(cx.listener(move |this, _e, w, cx| this.open_devtools_page(page, w, cx)))
                    })
                    .child(Icon::new(icon).with_size(px(16.)).text_color(primary))
                    .child(
                        v_flex()
                            .min_w(px(0.))
                            .child(div().text_size(px(tokens::UI_SM)).font_medium().text_color(fg).child(title))
                            .child(div().truncate().text_size(px(tokens::UI_XS)).text_color(muted).child(hint)),
                    )
            })
            .collect();

        v_flex()
            .flex_1()
            .h_full()
            .min_w(px(0.))
            .child(Self::panel_header("DevTools".into(), subtitle, tools, cx))
            .child(if running {
                v_flex()
                    .flex_1()
                    .p(px(tokens::SPACE_5))
                    .gap(px(tokens::SPACE_3))
                    .child(h_flex().flex_wrap().gap(px(tokens::SPACE_3)).children(tiles))
            } else {
                self.panel_not_running("DevTools", cx)
            })
    }
}

/// Every node with children under `node`, as fold keys.
fn collect_folds(node: &core_flutter::WidgetNode, key: String, out: &mut Vec<String>) {
    if node.children.is_empty() {
        return;
    }
    for (ix, child) in node.children.iter().enumerate() {
        collect_folds(child, format!("{key}/{ix}"), out);
    }
    out.push(key);
}

/// Percent-encode a value for a URL query string.
fn encode_query(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn the_vm_service_uri_is_encoded_for_devtools() {
        assert_eq!(encode_query("ws://127.0.0.1:5000/ab=/ws"), "ws%3A%2F%2F127.0.0.1%3A5000%2Fab%3D%2Fws");
    }

    #[core::prelude::v1::test]
    fn simulator_runtimes_read_as_versions() {
        assert_eq!(runtime_version("iOS 18.5"), "18.5");
        assert_eq!(runtime_version("27.2"), "27.2");
    }
}
