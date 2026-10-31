//! Running Flutter: devices, launch configs, daemon, hot reload, tests, pub.

use super::*;

impl DartIde {
    /// Push a plain or ANSI-styled line into the Debug Console (flutter run / pub / test output).
    pub(crate) fn push_flutter_log(&mut self, line: &str) {
        self.push_debug_line(line);
    }

    /// Discover connected Flutter devices (`flutter devices --machine`) and
    /// available macOS iOS Simulators (`xcrun simctl list devices available -j`)
    /// on a background thread so UI startup and frame rendering never block.
    pub(crate) fn refresh_devices(&mut self, cx: &mut Context<Self>) {
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let sims = core_flutter::list_ios_simulators().unwrap_or_default();
            let mut devices = core_flutter::list_devices().unwrap_or_default();
            // Always ensure booted iOS simulators from `xcrun simctl` appear as active targets
            // even if `flutter devices` was slow or unavailable in GUI PATH.
            for sim in sims.iter().filter(|s| s.is_booted()) {
                if !devices
                    .iter()
                    .any(|d| d.id.eq_ignore_ascii_case(&sim.udid) || d.name == sim.name)
                {
                    devices.insert(
                        0,
                        core_flutter::Device {
                            id: sim.udid.clone(),
                            name: sim.name.clone(),
                            target_platform: "ios".to_string(),
                            emulator: true,
                            is_supported: true,
                        },
                    );
                }
            }
            if !devices.iter().any(|d| d.id == "macos") {
                devices.push(core_flutter::Device {
                    id: "macos".to_string(),
                    name: "macOS".to_string(),
                    target_platform: "darwin".to_string(),
                    emulator: false,
                    is_supported: true,
                });
            }
            let _ = tx.send_blocking((devices, sims));
        });

        cx.spawn(async move |weak, cx| {
            let Ok((devices, sims)) = rx.recv().await else {
                return;
            };
            let _ = weak.update(cx, |this, cx| {
                if !devices.is_empty() {
                    this.devices = devices;
                    let current_valid = this.selected_device.as_ref().is_some_and(|sel| {
                        this.devices
                            .iter()
                            .any(|d| d.id.eq_ignore_ascii_case(&sel.id))
                    });
                    if !current_valid {
                        // Prefer a booted iOS Simulator if one is running, otherwise macOS or first device
                        let preferred = this
                            .devices
                            .iter()
                            .find(|d| d.is_ios())
                            .or_else(|| this.devices.iter().find(|d| d.id == "macos"))
                            .or_else(|| this.devices.first())
                            .cloned();
                        this.selected_device = preferred;
                    }
                }
                this.ios_simulators = sims;
                this.refresh_simulator_screenshot(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Reload `.vscode/launch.json` from `self.root`, splitting configurations into
    /// runnable app targets (`self.launch_configs`) and test CodeLens templates
    /// (`self.test_launch_templates`), and updating all open editor buffers.
    pub(crate) fn reload_launch_configs(&mut self, cx: &mut Context<Self>) {
        let prev_name = self
            .selected_launch_config
            .and_then(|ix| self.launch_configs.get(ix))
            .map(|c| c.name.clone());

        if let Some(file) = core_flutter::load_launch_file(&self.root) {
            let mut runnable = Vec::new();
            let mut templates = Vec::new();
            for cfg in file.configurations {
                if cfg.is_test_template() {
                    templates.push(cfg);
                } else {
                    runnable.push(cfg);
                }
            }
            self.launch_configs = runnable;
            self.test_launch_templates = templates;
        } else {
            self.launch_configs.clear();
            self.test_launch_templates.clear();
        }

        self.selected_launch_config = prev_name
            .and_then(|name| self.launch_configs.iter().position(|c| c.name == name))
            .or_else(|| (!self.launch_configs.is_empty()).then_some(0));

        let lenses = self.current_test_code_lenses();
        let editors: Vec<_> = self.tabs.iter().map(|t| t.editor.clone()).collect();
        for ed in editors {
            let lenses = lenses.clone();
            ed.update(cx, |editor, cx| {
                editor.set_test_code_lenses(lenses, cx);
            });
        }
    }

    pub(crate) fn current_test_code_lenses(&self) -> Vec<editor_view::TestCodeLensTemplate> {
        self.test_launch_templates
            .iter()
            .map(|tpl| editor_view::TestCodeLensTemplate {
                title: tpl.code_lens_title().to_string(),
                args: tpl.resolved_cli_args(&self.root),
                for_test: tpl.applies_to_single_test(),
                for_file: tpl.applies_to_test_file(),
            })
            .collect()
    }

    /// Execute `flutter test` for a specific test file (and optional test name +
    /// extra arguments from a `.vscode/launch.json` test template like `--update-goldens`
    /// or `--coverage`) in the nearest package directory containing `pubspec.yaml`.
    pub(crate) fn run_flutter_test(
        &mut self,
        path: PathBuf,
        test_name: Option<String>,
        extra_args: Vec<String>,
        label: String,
        cx: &mut Context<Self>,
    ) {
        // Walk up from `path` to find the nearest package directory containing `pubspec.yaml`
        let mut pkg_dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone());
        let mut cur = Some(pkg_dir.as_path());
        while let Some(dir) = cur {
            if dir.join("pubspec.yaml").is_file() {
                pkg_dir = dir.to_path_buf();
                break;
            }
            cur = dir.parent();
        }

        let rel_test_path = path
            .strip_prefix(&pkg_dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();

        let local_fvm = pkg_dir.join(".fvm/flutter_sdk/bin/flutter");
        let root_fvm = self.root.join(".fvm/flutter_sdk/bin/flutter");
        let flutter_bin = if local_fvm.is_file() {
            local_fvm.to_string_lossy().into_owned()
        } else if root_fvm.is_file() {
            root_fvm.to_string_lossy().into_owned()
        } else {
            "flutter".to_string()
        };

        let mut parts: Vec<String> = Vec::new();
        if pkg_dir != self.root {
            parts.push(format!("cd '{}' &&", pkg_dir.display()));
        }
        parts.push(flutter_bin);
        parts.push("test".to_string());
        for arg in &extra_args {
            if arg.contains(' ') {
                parts.push(format!("'{arg}'"));
            } else {
                parts.push(arg.clone());
            }
        }
        if let Some(name) = &test_name {
            let escaped = name.replace('\'', "'\\''");
            parts.push("--plain-name".to_string());
            parts.push(format!("'{escaped}'"));
        }
        parts.push(format!("'{rel_test_path}'"));

        let cmd = parts.join(" ");
        self.terminal_open = true;
        self.dock_tab = DockTab::Terminal;
        self.push_flutter_log(&format!(
            "\x1b[36m\u{25b6} [{label}] Running test{}{}\x1b[0m",
            test_name
                .as_ref()
                .map(|n| format!(" \"{n}\""))
                .unwrap_or_default(),
            if extra_args.is_empty() {
                String::new()
            } else {
                format!(" ({})", extra_args.join(" "))
            }
        ));
        self.execute_terminal_command(cmd, cx);
    }

    pub(crate) fn open_or_create_launch_json(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let vscode_dir = self.root.join(".vscode");
        let launch_path = vscode_dir.join("launch.json");
        if !launch_path.is_file() {
            let _ = std::fs::create_dir_all(&vscode_dir);
            let starter = r#"{
    "version": "0.2.0",
    "configurations": [
        {
            "name": "Update goldens",
            "type": "dart",
            "request": "launch",
            "args": [
                "--update-goldens"
            ],
            "templateFor": "test",
            "codeLens": {
                "for": [
                    "run-test",
                    "run-test-file"
                ],
                "title": "Update goldens"
            }
        },
        {
            "name": "Coverage",
            "type": "dart",
            "request": "launch",
            "args": [
                "--coverage"
            ],
            "templateFor": "test",
            "codeLens": {
                "for": [
                    "run-test",
                    "run-test-file"
                ],
                "title": "Coverage"
            }
        },
        {
            "name": "Flutter (Debug)",
            "type": "dart",
            "request": "launch"
        }
    ]
}
"#;
            let _ = std::fs::write(&launch_path, starter);
            self.reload_launch_configs(cx);
        }
        self.open_path(launch_path, window, cx);
    }

    /// Start or restart `flutter run --machine -d <device_id>` using the active
    /// `.vscode/launch.json` configuration (if any) and selected device.
    pub(crate) fn start_flutter_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active_launch = self
            .selected_launch_config
            .and_then(|ix| self.launch_configs.get(ix))
            .cloned();

        let project_dir = active_launch
            .as_ref()
            .map(|c| c.resolved_cwd(&self.root))
            .unwrap_or_else(|| self.root.clone());
        let extra_args = active_launch
            .as_ref()
            .map(|c| c.resolved_cli_args(&self.root))
            .unwrap_or_default();
        let extra_env = active_launch
            .as_ref()
            .map(|c| c.resolved_env(&self.root))
            .unwrap_or_default();

        if !project_dir.join("pubspec.yaml").is_file() && active_launch.is_none() {
            self.terminal_open = true;
            window.focus(&self.terminal_focus, cx);
            let cmd = if self.root.join("Cargo.toml").is_file() {
                "cargo check".to_string()
            } else {
                "ls -la".to_string()
            };
            self.execute_terminal_command(cmd, cx);
            return;
        }

        // Stop any existing Flutter daemon first
        self.stop_flutter_run(cx);

        let mut device = self.selected_device.clone().unwrap_or(core_flutter::Device {
            id: "macos".to_string(),
            name: "macOS".to_string(),
            target_platform: "darwin".to_string(),
            emulator: false,
            is_supported: true,
        });
        if let Some(cfg_dev_id) = active_launch
            .as_ref()
            .and_then(|c| c.device_id.as_ref())
            .filter(|s| !s.is_empty())
        {
            if let Some(found) = self.devices.iter().find(|d| &d.id == cfg_dev_id) {
                device = found.clone();
            } else {
                device.id = cfg_dev_id.clone();
                device.name = cfg_dev_id.clone();
            }
        }

        // If targeting an iOS simulator, ensure it is booted and open in Simulator.app
        if device.is_ios() {
            let _ = core_flutter::boot_ios_simulator(&device.id);
        }

        let rel_cwd = project_dir
            .strip_prefix(&self.root)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty());

        self.terminal_open = true;
        self.dock_tab = DockTab::DebugConsole;
        {
            let args_suffix = if extra_args.is_empty() {
                String::new()
            } else {
                format!(" {}", extra_args.join(" "))
            };
            let cwd_prefix = rel_cwd
                .as_ref()
                .map(|d| format!("(cd {d}) "))
                .unwrap_or_default();
            let cmd_str = format!("{cwd_prefix}flutter run -d {}{args_suffix}", device.id);
            self.push_debug_line(&format!("\x1b[90m$ {cmd_str}\x1b[0m"));
        }

        match core_flutter::FlutterDaemon::spawn_with_options(
            &project_dir,
            &self.root,
            &device.id,
            &extra_args,
            &extra_env,
        ) {
            Ok(daemon) => {
                let events = daemon.events.clone();
                let daemon_arc = std::sync::Arc::new(std::sync::Mutex::new(daemon));
                self.flutter_daemon = Some(daemon_arc);
                self.flutter_app_id = None;
                let launch_title = active_launch
                    .as_ref()
                    .map(|c| c.name.as_str())
                    .unwrap_or("Flutter");
                self.flutter_status = Some(format!(
                    "Launching {launch_title} on {}\u{2026}",
                    device.name
                ));
                self.push_flutter_log(&format!(
                    "\x1b[36m\u{25b6} Starting {launch_title} on {} ({}){}...\x1b[0m",
                    device.name,
                    device.id,
                    rel_cwd
                        .as_ref()
                        .map(|d| format!(" in {d}"))
                        .unwrap_or_default(),
                ));
                cx.notify();

                cx.spawn_in(window, async move |weak, cx| {
                    while let Ok(output) = events.recv().await {
                        let keep_going = weak
                            .update_in(cx, |this, window, cx| {
                                this.handle_flutter_daemon_output(output, window, cx);
                            })
                            .is_ok();
                        if !keep_going {
                            break;
                        }
                    }
                    let _ = weak.update(cx, |this, cx| {
                        this.flutter_daemon = None;
                        this.flutter_app_id = None;
                        this.flutter_status = None;
                        cx.notify();
                    });
                })
                .detach();
            }
            Err(err) => {
                self.flutter_status = None;
                self.push_flutter_log(&format!(
                    "\x1b[31mFailed to spawn `flutter run --machine`: {err}\x1b[0m"
                ));
                cx.notify();
            }
        }
    }

    pub(crate) fn handle_flutter_daemon_output(
        &mut self,
        output: core_flutter::DaemonOutput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The reply to "which widget is selected?" is not a hot-reload response.
        if let core_flutter::DaemonOutput::Response { id, result } = &output {
            if self.sim.inspect_request == Some(*id) {
                self.sim.inspect_request = None;
                self.finish_inspect(result.clone(), window, cx);
                return;
            }
        }
        match output {
            core_flutter::DaemonOutput::Event { event, params } => match event.as_str() {
                "app.start" => {
                    if let Some(app_id) = params.get("appId").and_then(|v| v.as_str()) {
                        self.flutter_app_id = Some(app_id.to_string());
                    }
                    self.flutter_status = Some("Starting\u{2026}".to_string());
                    cx.notify();
                }
                "app.progress" => {
                    let finished = params
                        .get("finished")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if !finished {
                        if let Some(msg) = params.get("message").and_then(|v| v.as_str()) {
                            self.flutter_status = Some(msg.to_string());
                            self.push_flutter_log(&format!("\x1b[33m\u{25d0} {msg}\x1b[0m"));
                        }
                    } else if self.flutter_app_id.is_some() {
                        self.flutter_status = Some("Running".to_string());
                    }
                    cx.notify();
                }
                "app.debugPort" => {
                    if let Some(ws) = params.get("wsUri").and_then(|v| v.as_str()) {
                        self.flutter_ws_uri = Some(ws.to_string());
                    }
                }
                "app.started" => {
                    if let Some(app_id) = params.get("appId").and_then(|v| v.as_str()) {
                        self.flutter_app_id = Some(app_id.to_string());
                    }
                    self.flutter_status = Some("Running".to_string());
                    self.push_flutter_log(
                        "\x1b[32m\u{2713} Flutter app running \u{2014} Save (\u{2318}S) or click \u{26a1} to Hot Reload\x1b[0m",
                    );
                    self.refresh_simulator_screenshot(cx);
                    cx.notify();
                }
                "app.log" => {
                    if let Some(log) = params.get("log").and_then(|v| v.as_str()) {
                        for line in log.lines() {
                            self.push_flutter_log(line);
                        }
                        cx.notify();
                    }
                }
                "app.stop" => {
                    self.sim.inspect = false;
                    self.sim.inspected = None;
                    self.flutter_app_id = None;
                    self.flutter_ws_uri = None;
                    self.flutter_daemon = None;
                    self.flutter_status = None;
                    self.push_flutter_log("\x1b[90m\u{25a0} Flutter app stopped.\x1b[0m");
                    cx.notify();
                }
                _ => {}
            },
            core_flutter::DaemonOutput::Response { result, .. } => {
                if let Ok(val) = result {
                    if let Some(msg) = val.get("message").and_then(|v| v.as_str()) {
                        if !msg.is_empty() {
                            self.push_flutter_log(&format!("\x1b[32m\u{26a1} {msg}\x1b[0m"));
                        }
                    }
                }
                if self.flutter_app_id.is_some() {
                    self.flutter_status = Some("Running".to_string());
                }
                self.refresh_simulator_screenshot(cx);
                cx.notify();
            }
            core_flutter::DaemonOutput::Log(line) => {
                if !line.trim().is_empty() {
                    self.push_flutter_log(&line);
                    cx.notify();
                }
            }
        }
    }

    /// Trigger sub-second stateful Hot Reload (`fullRestart: false`) on the active `FlutterDaemon`.
    pub(crate) fn trigger_hot_reload(&mut self, cx: &mut Context<Self>) {
        let (Some(daemon), Some(app_id)) = (self.flutter_daemon.clone(), self.flutter_app_id.clone()) else {
            self.push_flutter_log(
                "\x1b[33mNo running Flutter session \u{2014} click \u{25b6} Run first.\x1b[0m",
            );
            cx.notify();
            return;
        };
        if let Ok(guard) = daemon.lock() {
            if guard.hot_reload(&app_id).is_ok() {
                self.flutter_status = Some("Hot reloading\u{2026}".to_string());
                self.push_flutter_log("\x1b[33m\u{26a1} Hot reloading...\x1b[0m");
                cx.notify();
            }
        }
    }

    /// Trigger a full Hot Restart (`fullRestart: true`) on the active `FlutterDaemon`.
    pub(crate) fn trigger_hot_restart(&mut self, cx: &mut Context<Self>) {
        let (Some(daemon), Some(app_id)) = (self.flutter_daemon.clone(), self.flutter_app_id.clone()) else {
            return;
        };
        if let Ok(guard) = daemon.lock() {
            if guard.hot_restart(&app_id).is_ok() {
                self.flutter_status = Some("Restarting\u{2026}".to_string());
                self.push_flutter_log("\x1b[36m\u{21bb} Hot restarting app...\x1b[0m");
                cx.notify();
            }
        }
    }

    /// Gracefully stop the active `FlutterDaemon` session.
    pub(crate) fn stop_flutter_run(&mut self, cx: &mut Context<Self>) {
        if let Some(daemon) = self.flutter_daemon.take() {
            if let Ok(mut guard) = daemon.lock() {
                if let Some(app_id) = self.flutter_app_id.take() {
                    let _ = guard.stop(&app_id);
                }
                let _ = guard.kill();
            }
            self.flutter_ws_uri = None;
            self.flutter_status = None;
            self.push_flutter_log("\x1b[90m\u{25a0} Stopped Flutter run session.\x1b[0m");
            cx.notify();
        }
    }

    pub(crate) fn open_flutter_devtools(&mut self, cx: &mut Context<Self>) {
        self.terminal_open = true;
        if let Some(ws) = &self.flutter_ws_uri {
            let http = ws
                .replacen("ws://", "http://", 1)
                .replacen("wss://", "https://", 1);
            let _ = std::process::Command::new("open").arg(&http).spawn();
            self.dock_tab = DockTab::DebugConsole;
            self.push_flutter_log(&format!(
                "\x1b[36m\u{2713} Opened Flutter VM Service / DevTools at {http}\x1b[0m"
            ));
        } else {
            self.dock_tab = DockTab::Terminal;
            self.execute_terminal_command("dart devtools".to_string(), cx);
        }
        cx.notify();
    }

    pub(crate) fn toggle_flutter_vm_ext(
        &mut self,
        method: &str,
        label: &str,
        cx: &mut Context<Self>,
    ) {
        let (Some(daemon), Some(app_id)) = (self.flutter_daemon.clone(), self.flutter_app_id.clone())
        else {
            self.terminal_open = true;
            self.dock_tab = DockTab::DebugConsole;
            self.push_flutter_log(&format!(
                "\x1b[33mStart a Flutter session first to toggle {label}.\x1b[0m"
            ));
            cx.notify();
            return;
        };
        let params = match method {
            "ext.flutter.showPerformanceOverlay" => {
                self.perf_overlay_on = !self.perf_overlay_on;
                serde_json::json!({ "enabled": if self.perf_overlay_on { "true" } else { "false" } })
            }
            "ext.flutter.debugPaint" => {
                self.debug_paint_on = !self.debug_paint_on;
                serde_json::json!({ "enabled": if self.debug_paint_on { "true" } else { "false" } })
            }
            "ext.flutter.timeDilation" => {
                self.slow_animations_on = !self.slow_animations_on;
                serde_json::json!({ "timeDilation": if self.slow_animations_on { "5.0" } else { "1.0" } })
            }
            _ => serde_json::json!({}),
        };
        if let Ok(guard) = daemon.lock() {
            let _ = guard.call_service_extension(&app_id, method, params);
            self.push_flutter_log(&format!("\x1b[36m\u{26a1} Toggled {label}\x1b[0m"));
        }
        cx.notify();
    }

    /// Run `flutter pub get` in `self.root` asynchronously and stream output to the terminal.
    pub(crate) fn run_pub_get(&mut self, cx: &mut Context<Self>) {
        if !self.root.join("pubspec.yaml").is_file() {
            return;
        }
        let root = self.root.clone();
        self.flutter_status = Some("Running pub get\u{2026}".to_string());
        self.terminal_open = true;
        self.dock_tab = DockTab::DebugConsole;
        self.push_flutter_log("\x1b[36m\u{21bb} Running `flutter pub get`...\x1b[0m");
        cx.notify();

        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let res = core_flutter::pub_get(&root);
            let _ = tx.send_blocking(res);
        });

        cx.spawn(async move |weak, cx| {
            let Ok(res) = rx.recv().await else { return };
            let _ = weak.update(cx, |this, cx| {
                match res {
                    Ok(out) => {
                        for line in out.stdout.lines().chain(out.stderr.lines()) {
                            if !line.trim().is_empty() {
                                this.push_flutter_log(line);
                            }
                        }
                        if out.success {
                            this.push_flutter_log(
                                "\x1b[32m\u{2713} `flutter pub get` succeeded.\x1b[0m",
                            );
                        } else {
                            this.push_flutter_log(
                                "\x1b[31m\u{2717} `flutter pub get` exited with errors.\x1b[0m",
                            );
                        }
                    }
                    Err(err) => {
                        this.push_flutter_log(&format!(
                            "\x1b[31mFailed to run `flutter pub get`: {err}\x1b[0m"
                        ));
                    }
                }
                this.flutter_status = this.flutter_app_id.as_ref().map(|_| "Running".to_string());
                this.refresh_git(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn render_launch_picker_popover(
        &self,
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        if !self.launch_picker_open {
            return None;
        }

        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let green = cx.theme().green;
        let info = cx.theme().info;
        let primary = cx.theme().primary;
        let list_hover = cx.theme().list_hover;
        let selected_ix = self.selected_launch_config;
        let active_test_path = self
            .active_editor()
            .and_then(|ed| ed.read(cx).buffer().path())
            .map(Path::to_path_buf);

        Some(
            div()
                .id("launch-picker-backdrop")
                .occlude()
                .absolute()
                .inset_0()
                .on_mouse_down(MouseButton::Left, {
                    let view = view.clone();
                    move |_event, _window, cx| {
                        view.update(cx, |this, cx| {
                            this.launch_picker_open = false;
                            cx.notify();
                        });
                    }
                })
                .child(
                    v_flex()
                        .id("launch-picker-card")
                        .absolute()
                        .top(px(tokens::TITLEBAR_H + 4.))
                        .right(px(230.))
                        .w(px(360.))
                        .p(px(tokens::SPACE_2))
                        .gap(px(tokens::SPACE_1))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                            cx.stop_propagation();
                        })
                        .child(
                            h_flex()
                                .px(px(tokens::SPACE_2))
                                .py(px(2.))
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(tokens::UI_XS))
                                        .font_medium()
                                        .text_color(muted)
                                        .child("LAUNCH CONFIGURATIONS (.vscode/launch.json)"),
                                )
                                .child({
                                    let view = view.clone();
                                    h_flex()
                                        .id("reload-launch-btn")
                                        .px_1p5()
                                        .py_0p5()
                                        .gap_1()
                                        .items_center()
                                        .rounded(px(tokens::RADIUS_SM))
                                        .cursor_pointer()
                                        .text_size(px(tokens::UI_XS))
                                        .text_color(info)
                                        .hover(move |s| s.bg(list_hover))
                                        .child(
                                            gpui_kit::component::Icon::new(
                                                gpui_kit::assets::IconName::RotateCw,
                                            )
                                            .with_size(px(10.)),
                                        )
                                        .child("Reload")
                                        .on_click(move |_event, _window, cx| {
                                            view.update(cx, |this, cx| {
                                                this.reload_launch_configs(cx);
                                                cx.notify();
                                            });
                                        })
                                }),
                        )
                        .when(self.launch_configs.is_empty(), |el| {
                            el.child(
                                div()
                                    .px(px(tokens::SPACE_3))
                                    .py(px(6.))
                                    .text_size(px(tokens::UI_SM))
                                    .text_color(muted)
                                    .child(
                                        "No app launch configurations found in .vscode/launch.json",
                                    ),
                            )
                        })
                        .children(self.launch_configs.iter().enumerate().map(|(ix, cfg)| {
                            let is_sel = selected_ix == Some(ix);
                            let name = cfg.name.clone();
                            let rel_cwd = cfg.relative_cwd_label(&self.root);
                            let flavor = cfg.flavor();
                            let view_select = view.clone();
                            let view_run = view.clone();

                            h_flex()
                                .id(("launch-cfg-item", ix))
                                .w_full()
                                .min_h(px(30.))
                                .px(px(tokens::SPACE_3))
                                .py(px(3.))
                                .items_center()
                                .justify_between()
                                .rounded(px(tokens::RADIUS_SM))
                                .border_1()
                                .border_color(if is_sel {
                                    rgba(0xea7a3c44)
                                } else {
                                    rgba(0x00000000)
                                })
                                .when(is_sel, |el| el.bg(rgba(0xea7a3c18)))
                                .when(!is_sel, |el| el.hover(move |s| s.bg(list_hover)))
                                .cursor_pointer()
                                .on_click(move |_event, _window, cx| {
                                    view_select.update(cx, |this, cx| {
                                        this.selected_launch_config = Some(ix);
                                        this.launch_picker_open = false;
                                        cx.notify();
                                    });
                                })
                                .child(
                                    v_flex()
                                        .min_w(px(0.))
                                        .flex_1()
                                        .child(
                                            h_flex()
                                                .items_center()
                                                .gap_1p5()
                                                .child(
                                                    gpui_kit::component::Icon::new(
                                                        gpui_kit::assets::IconName::Play,
                                                    )
                                                    .with_size(px(11.))
                                                    .text_color(if is_sel { primary } else { green }),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(px(tokens::UI_SM))
                                                        .font_medium()
                                                        .text_color(fg)
                                                        .truncate()
                                                        .child(name),
                                                )
                                                .when_some(flavor, |el, flv| {
                                                    el.child(
                                                        div()
                                                            .px_1p5()
                                                            .rounded(px(3.))
                                                            .bg(info.opacity(0.16))
                                                            .font_family("SF Mono")
                                                            .text_size(px(9.5))
                                                            .text_color(info)
                                                            .child(flv),
                                                    )
                                                }),
                                        )
                                        .when_some(rel_cwd, |el, cwd| {
                                            el.child(
                                                div()
                                                    .pl(px(17.))
                                                    .font_family("SF Mono")
                                                    .text_size(px(10.))
                                                    .text_color(muted)
                                                    .truncate()
                                                    .child(cwd),
                                            )
                                        }),
                                )
                                .child(
                                    h_flex()
                                        .id(("launch-cfg-run-now", ix))
                                        .px_2()
                                        .h(px(22.))
                                        .items_center()
                                        .gap_1()
                                        .rounded(px(tokens::RADIUS_SM))
                                        .bg(green.opacity(0.15))
                                        .border_1()
                                        .border_color(green.opacity(0.32))
                                        .text_size(px(10.))
                                        .text_color(green)
                                        .child("\u{25b6} Run")
                                        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                                            cx.stop_propagation();
                                        })
                                        .on_click(move |_event, window, cx| {
                                            cx.stop_propagation();
                                            view_run.update(cx, |this, cx| {
                                                this.selected_launch_config = Some(ix);
                                                this.launch_picker_open = false;
                                                this.start_flutter_run(window, cx);
                                            });
                                        }),
                                )
                        }))
                        .when(!self.test_launch_templates.is_empty(), |el| {
                            el.child(div().h(px(1.)).w_full().my_1().bg(cx.theme().border))
                                .child(
                                    div()
                                        .px(px(tokens::SPACE_2))
                                        .py(px(2.))
                                        .text_size(px(tokens::UI_XS))
                                        .font_medium()
                                        .text_color(muted)
                                        .child("TEST TEMPLATES & CODELENS (templateFor: \"test\")"),
                                )
                                .children(self.test_launch_templates.iter().enumerate().map(
                                    |(ix, tpl)| {
                                        let title = tpl.code_lens_title().to_string();
                                        let args = tpl.resolved_cli_args(&self.root);
                                        let flags = args.join(" ");
                                        let view = view.clone();
                                        let path_opt = active_test_path.clone();

                                        h_flex()
                                            .id(("test-tpl-item", ix))
                                            .w_full()
                                            .h(px(28.))
                                            .px(px(tokens::SPACE_3))
                                            .items_center()
                                            .justify_between()
                                            .rounded(px(tokens::RADIUS_SM))
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(list_hover))
                                            .on_click(move |_event, _window, cx| {
                                                let title = title.clone();
                                                let args = args.clone();
                                                let path = path_opt
                                                    .clone()
                                                    .unwrap_or_else(|| PathBuf::from("test"));
                                                view.update(cx, |this, cx| {
                                                    this.launch_picker_open = false;
                                                    this.run_flutter_test(
                                                        path, None, args, title, cx,
                                                    );
                                                });
                                            })
                                            .child(
                                                h_flex()
                                                    .items_center()
                                                    .gap_2()
                                                    .child(
                                                        div()
                                                            .text_size(px(10.5))
                                                            .text_color(info)
                                                            .child("\u{25b6}"),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(px(tokens::UI_SM))
                                                            .text_color(fg)
                                                            .child(tpl.code_lens_title().to_string()),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .font_family("SF Mono")
                                                    .text_size(px(10.))
                                                    .text_color(muted)
                                                    .child(flags),
                                            )
                                    },
                                ))
                        })
                        .child(div().h(px(1.)).w_full().my_1().bg(cx.theme().border))
                        .child({
                            let view = view.clone();
                            h_flex()
                                .id("open-launch-json-btn")
                                .w_full()
                                .h(px(28.))
                                .px(px(tokens::SPACE_3))
                                .items_center()
                                .justify_between()
                                .rounded(px(tokens::RADIUS_SM))
                                .cursor_pointer()
                                .hover(move |s| s.bg(list_hover))
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            gpui_kit::component::Icon::new(
                                                gpui_kit::assets::IconName::Settings,
                                            )
                                            .with_size(px(12.))
                                            .text_color(primary),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(tokens::UI_SM))
                                                .text_color(fg)
                                                .child("Edit .vscode/launch.json"),
                                        ),
                                )
                                .child(
                                    div()
                                        .font_family("SF Mono")
                                        .text_size(px(10.))
                                        .text_color(muted)
                                        .child("Open"),
                                )
                                .on_click(move |_event, window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.launch_picker_open = false;
                                        this.open_or_create_launch_json(window, cx);
                                    });
                                })
                        }),
                )
        )
    }

    pub(crate) fn render_device_picker_popover(
        &self,
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        if !self.device_picker_open {
            return None;
        }

        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let green = cx.theme().green;
        let primary = cx.theme().primary;
        let list_hover = cx.theme().list_hover;
        let selected_id = self.selected_device.as_ref().map(|d| d.id.clone());
        let selected_cfg_ix = self.selected_launch_config;
        let sim_attached = self.sim.attached;
        let sims_expanded = self.sim_list_expanded;

        // Build active targets: `self.devices` + any booted (or currently selected) iOS simulator
        // from `self.ios_simulators`, so booted iPhones always appear under `TARGET`.
        let mut active_targets: Vec<core_flutter::Device> = self.devices.clone();
        for sim in &self.ios_simulators {
            let is_selected_sim = selected_id
                .as_deref()
                .is_some_and(|id| id.eq_ignore_ascii_case(&sim.udid));
            if (sim.is_booted() || is_selected_sim)
                && !active_targets
                    .iter()
                    .any(|d| d.id.eq_ignore_ascii_case(&sim.udid) || d.name == sim.name)
            {
                active_targets.insert(
                    0,
                    core_flutter::Device {
                        id: sim.udid.clone(),
                        name: sim.name.clone(),
                        target_platform: "ios".to_string(),
                        emulator: true,
                        is_supported: true,
                    },
                );
            }
        }

        // Deduplicate unbooted iOS simulators by model name (keeping the first/latest runtime)
        // and exclude any simulator already shown in `active_targets`.
        let mut seen_names = std::collections::HashSet::new();
        for d in &active_targets {
            seen_names.insert(d.name.clone());
        }
        let extra_sims: Vec<_> = self
            .ios_simulators
            .iter()
            .filter(|sim| {
                !sim.is_booted()
                    && !active_targets
                        .iter()
                        .any(|d| d.id.eq_ignore_ascii_case(&sim.udid))
                    && seen_names.insert(sim.name.clone())
            })
            .take(6)
            .cloned()
            .collect();

        Some(
            div()
                .id("device-picker-backdrop")
                .occlude()
                .absolute()
                .inset_0()
                .on_mouse_down(MouseButton::Left, {
                    let view = view.clone();
                    move |_event, _window, cx| {
                        view.update(cx, |this, cx| {
                            this.device_picker_open = false;
                            cx.notify();
                        });
                    }
                })
                .child(
                    v_flex()
                        .id("device-picker-card")
                        .absolute()
                        .top(px(tokens::TITLEBAR_H + 4.))
                        .right(px(108.))
                        .w(px(250.))
                        .max_h(px(400.))
                        .overflow_y_scroll()
                        .p(px(tokens::SPACE_2))
                        .gap(px(2.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                            cx.stop_propagation();
                        })
                        // Section 1: Active Device Targets (clean rows, checkmark only)
                        .child(
                            h_flex()
                                .px(px(tokens::SPACE_2))
                                .py(px(2.))
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(tokens::UI_XS))
                                        .font_medium()
                                        .text_color(muted)
                                        .child("TARGET"),
                                )
                                .child({
                                    let view = view.clone();
                                    div()
                                        .id("refresh-devices-btn")
                                        .cursor_pointer()
                                        .text_color(muted)
                                        .hover(move |s| s.text_color(fg))
                                        .child(
                                            gpui_kit::component::Icon::new(
                                                gpui_kit::assets::IconName::RotateCw,
                                            )
                                            .with_size(px(10.5)),
                                        )
                                        .on_click(move |_event, _window, cx| {
                                            view.update(cx, |this, cx| {
                                                this.refresh_devices(cx);
                                            });
                                        })
                                }),
                        )
                        .children(active_targets.into_iter().enumerate().map(|(ix, dev)| {
                            let is_sel = selected_id
                                .as_deref()
                                .is_some_and(|id| id.eq_ignore_ascii_case(&dev.id));
                            let icon = if dev.is_ios()
                                || dev.target_platform.contains("android")
                            {
                                gpui_kit::assets::IconName::Smartphone
                            } else if dev.target_platform.contains("web") || dev.id == "chrome" {
                                gpui_kit::assets::IconName::Globe
                            } else {
                                gpui_kit::assets::IconName::Monitor
                            };
                            let dev_clone = dev.clone();
                            let view_sel = view.clone();

                            h_flex()
                                .id(("device-item", ix))
                                .w_full()
                                .h(px(26.))
                                .px(px(tokens::SPACE_3))
                                .items_center()
                                .justify_between()
                                .rounded(px(tokens::RADIUS_SM))
                                .when(is_sel, |el| el.bg(tokens::accent_dim()))
                                .when(!is_sel, |el| el.hover(move |s| s.bg(list_hover)))
                                .cursor_pointer()
                                .on_click(move |_event, _window, cx| {
                                    view_sel.update(cx, |this, cx| {
                                        this.selected_device = Some(dev_clone.clone());
                                        this.device_picker_open = false;
                                        this.refresh_simulator_screenshot(cx);
                                        cx.notify();
                                    });
                                })
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            gpui_kit::component::Icon::new(icon)
                                                .with_size(px(12.5))
                                                .text_color(if is_sel { primary } else { green }),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(tokens::UI_SM))
                                                .text_color(fg)
                                                .child(dev.name.clone()),
                                        ),
                                )
                                .when(is_sel, |el| {
                                    el.child(
                                        gpui_kit::component::Icon::new(
                                            gpui_kit::assets::IconName::Check,
                                        )
                                        .with_size(px(11.5))
                                        .text_color(primary),
                                    )
                                })
                        }))
                        // Collapsible "More iOS Simulators" row (keeps the popover short & clean)
                        .when(!extra_sims.is_empty(), |el| {
                            let view_toggle = view.clone();
                            el.child(
                                h_flex()
                                    .id("toggle-extra-sims")
                                    .w_full()
                                    .h(px(24.))
                                    .px(px(tokens::SPACE_3))
                                    .items_center()
                                    .justify_between()
                                    .rounded(px(tokens::RADIUS_SM))
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(list_hover))
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(muted)
                                    .child(format!("More iOS Simulators ({})", extra_sims.len()))
                                    .child(
                                        gpui_kit::component::Icon::new(if sims_expanded {
                                            gpui_kit::assets::IconName::ChevronDown
                                        } else {
                                            gpui_kit::assets::IconName::ChevronRight
                                        })
                                        .with_size(px(10.5))
                                        .text_color(muted),
                                    )
                                    .on_click(move |_e, _w, cx| {
                                        view_toggle.update(cx, |this, cx| {
                                            this.sim_list_expanded = !this.sim_list_expanded;
                                            cx.notify();
                                        });
                                    }),
                            )
                            .when(sims_expanded, |list| {
                                list.children(extra_sims.into_iter().enumerate().map(
                                    |(ix, sim)| {
                                        let view_sel = view.clone();
                                        let sim_name = sim.name.clone();
                                        let sim_for_click = sim.clone();
                                        h_flex()
                                            .id(("ios-sim-item", ix))
                                            .w_full()
                                            .h(px(25.))
                                            .pl(px(tokens::SPACE_5))
                                            .pr(px(tokens::SPACE_3))
                                            .items_center()
                                            .justify_between()
                                            .rounded(px(tokens::RADIUS_SM))
                                            .cursor_pointer()
                                            .hover(move |s| s.bg(list_hover))
                                            .on_click(move |_event, window, cx| {
                                                let s = sim_for_click.clone();
                                                view_sel.update(cx, |this, cx| {
                                                    this.selected_device =
                                                        Some(core_flutter::Device {
                                                            id: s.udid.clone(),
                                                            name: s.name.clone(),
                                                            target_platform: "ios".to_string(),
                                                            emulator: true,
                                                            is_supported: true,
                                                        });
                                                    this.device_picker_open = false;
                                                    this.attach_simulator(window, cx);
                                                    this.refresh_devices(cx);
                                                });
                                            })
                                            .child(
                                                h_flex()
                                                    .min_w(px(0.))
                                                    .items_center()
                                                    .gap_2()
                                                    .child(
                                                        gpui_kit::component::Icon::new(
                                                            gpui_kit::assets::IconName::Smartphone,
                                                        )
                                                        .with_size(px(12.))
                                                        .text_color(muted),
                                                    )
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .text_size(px(tokens::UI_SM))
                                                            .text_color(fg)
                                                            .child(sim_name),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(tokens::UI_XS))
                                                    .text_color(muted)
                                                    .child("Boot"),
                                            )
                                    },
                                ))
                            })
                        })
                        // Launch Profiles (only listed if `.vscode/launch.json` has profiles)
                        .when(!self.launch_configs.is_empty(), |el| {
                            el.child(div().h(px(1.)).w_full().my_1().bg(cx.theme().border))
                                .child(
                                    div()
                                        .px(px(tokens::SPACE_2))
                                        .py(px(2.))
                                        .text_size(px(tokens::UI_XS))
                                        .font_medium()
                                        .text_color(muted)
                                        .child("PROFILE"),
                                )
                                .children(self.launch_configs.iter().enumerate().map(
                                    |(ix, cfg)| {
                                        let is_sel = selected_cfg_ix == Some(ix);
                                        let name = cfg.name.clone();
                                        let view = view.clone();
                                        h_flex()
                                            .id(("unified-launch-cfg", ix))
                                            .w_full()
                                            .h(px(25.))
                                            .px(px(tokens::SPACE_3))
                                            .items_center()
                                            .justify_between()
                                            .rounded(px(tokens::RADIUS_SM))
                                            .cursor_pointer()
                                            .when(is_sel, |s| s.bg(tokens::accent_dim()))
                                            .when(!is_sel, |s| s.hover(move |h| h.bg(list_hover)))
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_size(px(tokens::UI_SM))
                                                    .text_color(fg)
                                                    .child(name),
                                            )
                                            .when(is_sel, |r| {
                                                r.child(
                                                    gpui_kit::component::Icon::new(
                                                        gpui_kit::assets::IconName::Check,
                                                    )
                                                    .with_size(px(11.5))
                                                    .text_color(primary),
                                                )
                                            })
                                            .on_click(move |_e, _w, cx| {
                                                view.update(cx, |this, cx| {
                                                    this.selected_launch_config = Some(ix);
                                                    this.device_picker_open = false;
                                                    cx.notify();
                                                });
                                            })
                                    },
                                ))
                        })
                        // Quiet footer actions: Simulator panel toggle + Configure launch.json
                        .child(div().h(px(1.)).w_full().my_1().bg(cx.theme().border))
                        .child({
                            let view = view.clone();
                            h_flex()
                                .id("device-picker-attach-sim")
                                .w_full()
                                .h(px(26.))
                                .px(px(tokens::SPACE_3))
                                .items_center()
                                .justify_between()
                                .rounded(px(tokens::RADIUS_SM))
                                .cursor_pointer()
                                .hover(move |s| s.bg(list_hover))
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            gpui_kit::component::Icon::new(
                                                gpui_kit::assets::IconName::Smartphone,
                                            )
                                            .with_size(px(12.))
                                            .text_color(if sim_attached { green } else { muted }),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(tokens::UI_SM))
                                                .text_color(fg)
                                                .child("Simulator in Editor"),
                                        ),
                                )
                                .child(
                                    div()
                                        .font_family("SF Mono")
                                        .text_size(px(tokens::UI_XS))
                                        .text_color(if sim_attached { green } else { muted })
                                        .child(if sim_attached { "On" } else { "Off" }),
                                )
                                .on_click(move |_event, window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.device_picker_open = false;
                                        if this.sim.attached {
                                            this.detach_simulator(cx);
                                        } else {
                                            this.attach_simulator(window, cx);
                                        }
                                    });
                                })
                        })
                        .child({
                            let view = view.clone();
                            h_flex()
                                .id("unified-edit-launch-json")
                                .w_full()
                                .h(px(26.))
                                .px(px(tokens::SPACE_3))
                                .items_center()
                                .gap_2()
                                .rounded(px(tokens::RADIUS_SM))
                                .cursor_pointer()
                                .hover(move |s| s.bg(list_hover))
                                .child(
                                    gpui_kit::component::Icon::new(
                                        gpui_kit::assets::IconName::Settings,
                                    )
                                    .with_size(px(11.5))
                                    .text_color(muted),
                                )
                                .child(
                                    div()
                                        .text_size(px(tokens::UI_SM))
                                        .text_color(muted)
                                        .child("Configure launch.json\u{2026}"),
                                )
                                .on_click(move |_event, window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.device_picker_open = false;
                                        this.open_or_create_launch_json(window, cx);
                                    });
                                })
                        }),
                )
        )
    }
}
