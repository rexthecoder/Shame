//! Modal prompts: rename, code actions, go-to-line, add-package.

use super::*;

impl DartIde {
    pub(crate) fn render_rename_prompt(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> Option<Div> {
        self.rename_target.as_ref()?;

        Some(
            div()
                .occlude()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(84.))
                .bg(rgba(0x00000066))
                .child(
                    v_flex()
                        .w(px(360.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .child(
                            div()
                                .px_3()
                                .py_2()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Rename symbol"),
                        )
                        .child(
                            div()
                                .px_3()
                                .pb_3()
                                .on_key_down({
                                    let view = view.clone();
                                    move |event, window, cx| {
                                        let key = event.keystroke.key.as_str();
                                        if key == "escape" {
                                            view.update(cx, |this, cx| {
                                                this.rename_target = None;
                                                cx.notify();
                                            });
                                        } else if key == "enter" {
                                            view.update(cx, |this, cx| {
                                                this.submit_rename(window, cx)
                                            });
                                        }
                                    }
                                })
                                .child(
                                    h_flex()
                                        .h(px(28.))
                                        .px_2p5()
                                        .items_center()
                                        .rounded(px(tokens::RADIUS_SM))
                                        .bg(cx.theme().background)
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .child(
                                            Input::new(&self.rename_input)
                                                .appearance(false)
                                                .small(),
                                        ),
                                ),
                        ),
                ),
        )
    }

    pub(crate) fn submit_rename(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some((editor, line, column)) = self.rename_target.take() else {
            return;
        };
        let new_name = self.rename_input.read(cx).value().to_string();
        if !new_name.is_empty() {
            editor.update(cx, |editor, _cx| editor.send_rename(line, column, new_name));
        }
        cx.notify();
    }

    pub(crate) fn open_go_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_editor().is_none() {
            return;
        }
        self.go_to_line_open = true;
        self.go_to_line_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find_open = false;
        if let Some(editor) = self.active_editor().cloned() {
            editor.update(cx, |ed, cx| ed.clear_search(cx));
            let fh = editor.read(cx).focus_handle(cx);
            window.focus(&fh, cx);
        }
        cx.notify();
    }

    pub(crate) fn submit_go_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.go_to_line_input.read(cx).value().trim().to_string();
        self.go_to_line_open = false;
        if let Some(editor) = self.active_editor().cloned() {
            let mut parts = raw.split(':');
            if let Some(line_1) = parts.next().and_then(|s| s.trim().parse::<u32>().ok()) {
                let col_1 = parts
                    .next()
                    .and_then(|s| s.trim().parse::<u32>().ok())
                    .unwrap_or(1);
                editor.update(cx, |ed, cx| {
                    let max_line = ed.buffer().line_count().saturating_sub(1) as u32;
                    let line_0 = line_1.saturating_sub(1).min(max_line);
                    let col_0 = col_1.saturating_sub(1);
                    ed.set_cursor_to_position(line_0, col_0, cx);
                });
            }
            let fh = editor.read(cx).focus_handle(cx);
            window.focus(&fh, cx);
        }
        cx.notify();
    }

    pub(crate) fn render_go_to_line_prompt(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> Option<Div> {
        if !self.go_to_line_open {
            return None;
        }
        let line_count = self
            .active_editor()
            .map(|ed| ed.read(cx).buffer().line_count())
            .unwrap_or(1);

        Some(
            div()
                .occlude()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(84.))
                .bg(rgba(0x00000066))
                .child(
                    v_flex()
                        .w(px(360.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .child(
                            h_flex()
                                .px_3()
                                .py_2()
                                .items_center()
                                .justify_between()
                                .text_size(px(tokens::UI_SM))
                                .text_color(cx.theme().muted_foreground)
                                .child("Go to Line")
                                .child(
                                    div()
                                        .font_family("SF Mono")
                                        .text_size(px(tokens::UI_XS))
                                        .child(format!("1..{line_count}")),
                                ),
                        )
                        .child(
                            div()
                                .px_3()
                                .pb_3()
                                .on_key_down({
                                    let view = view.clone();
                                    move |event, window, cx| {
                                        let key = event.keystroke.key.as_str();
                                        if key == "escape" {
                                            view.update(cx, |this, cx| {
                                                this.go_to_line_open = false;
                                                cx.notify();
                                            });
                                        } else if key == "enter" {
                                            view.update(cx, |this, cx| {
                                                this.submit_go_to_line(window, cx)
                                            });
                                        }
                                    }
                                })
                                .child(
                                    h_flex()
                                        .h(px(28.))
                                        .px_2p5()
                                        .items_center()
                                        .rounded(px(tokens::RADIUS_SM))
                                        .bg(cx.theme().background)
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .child(
                                            Input::new(&self.go_to_line_input)
                                                .appearance(false)
                                                .small(),
                                        ),
                                ),
                        ),
                ),
        )
    }

    pub(crate) fn open_add_package(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_project_open = false;
        self.add_package_open = true;
        self.add_package_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn submit_add_package(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.add_package_input.read(cx).value().trim().to_string();
        self.add_package_open = false;
        if raw.is_empty() {
            cx.notify();
            return;
        }
        let (dev, pkg) = if let Some(rest) = raw.strip_prefix("dev:") {
            (true, rest.trim().to_string())
        } else {
            (false, raw)
        };
        let root = self.root.clone();
        self.terminal_open = true;
        self.flutter_status = Some(format!("Adding {pkg}\u{2026}"));
        self.push_flutter_log(&format!(
            "\x1b[36m+ Running `flutter pub add {}{pkg}`...\x1b[0m",
            if dev { "--dev " } else { "" }
        ));
        cx.notify();

        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let res = core_flutter::add_package(&root, &pkg, dev);
            let _ = tx.send_blocking(res);
        });

        cx.spawn(async move |weak, cx| {
            let Ok(res) = rx.recv().await else { return };
            let _ = weak.update(cx, |this, cx| {
                if let Ok(out) = res {
                    for line in out.stdout.lines().chain(out.stderr.lines()) {
                        if !line.trim().is_empty() {
                            this.push_flutter_log(line);
                        }
                    }
                }
                this.flutter_status = this.flutter_app_id.as_ref().map(|_| "Running".to_string());
                this.refresh_git(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn render_add_package_prompt(&self, view: &Entity<Self>, cx: &mut Context<Self>) -> Option<Div> {
        if !self.add_package_open {
            return None;
        }

        Some(
            div()
                .occlude()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(84.))
                .bg(rgba(0x00000066))
                .child(
                    v_flex()
                        .w(px(400.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .child(
                            h_flex()
                                .px_3()
                                .py_2()
                                .items_center()
                                .justify_between()
                                .text_size(px(tokens::UI_SM))
                                .text_color(cx.theme().muted_foreground)
                                .child("Add Flutter / Dart Package (pub.dev)")
                                .child(
                                    div()
                                        .font_family("SF Mono")
                                        .text_size(px(tokens::UI_XS))
                                        .child("prefix `dev:` for dev_dependencies"),
                                ),
                        )
                        .child(
                            div()
                                .px_3()
                                .pb_3()
                                .on_key_down({
                                    let view = view.clone();
                                    move |event, window, cx| {
                                        let key = event.keystroke.key.as_str();
                                        if key == "escape" {
                                            view.update(cx, |this, cx| {
                                                this.add_package_open = false;
                                                cx.notify();
                                            });
                                        } else if key == "enter" {
                                            view.update(cx, |this, cx| {
                                                this.submit_add_package(window, cx)
                                            });
                                        }
                                    }
                                })
                                .child(
                                    h_flex()
                                        .h(px(28.))
                                        .px_2p5()
                                        .items_center()
                                        .rounded(px(tokens::RADIUS_SM))
                                        .bg(cx.theme().background)
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .child(
                                            Input::new(&self.add_package_input)
                                                .appearance(false)
                                                .small(),
                                        ),
                                ),
                        ),
                ),
        )
    }
}

/// Where a new project goes by default: `~/Projects` when it exists, else the
/// home directory.
fn default_project_location() -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let projects = home.join("Projects");
    if projects.is_dir() { projects } else { home }
}

/// Expand a leading `~` and resolve a relative path against the current
/// directory, so the modal accepts the paths people actually type.
fn expand_project_location(raw: &str) -> std::path::PathBuf {
    let raw = raw.trim();
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let path = if raw == "~" {
        home.unwrap_or_else(|| std::path::PathBuf::from("."))
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.map(|h| h.join(rest))
            .unwrap_or_else(|| std::path::PathBuf::from(rest))
    } else {
        std::path::PathBuf::from(raw)
    };
    std::path::absolute(&path).unwrap_or(path)
}

/// Flutter's package-name rule: lowercase, digits and underscores, starting
/// with a letter.
fn validate_project_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        Some(_) => return Err("Start the name with a lowercase letter.".into()),
        None => return Err("Give the project a name.".into()),
    }
    if let Some(bad) = chars.find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')) {
        return Err(format!(
            "`{bad}` isn't allowed. Use lowercase letters, digits and underscores."
        ));
    }
    Ok(())
}

const PROJECT_TEMPLATES: &[(&str, &str)] = &[
    ("App", "app"),
    ("Package", "package"),
    ("Plugin", "plugin"),
    ("Module", "module"),
];

impl DartIde {
    pub(crate) fn open_new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_package_open = false;
        self.new_project_open = true;
        self.new_project_error = None;
        self.new_project_template = 0;
        let location = if self.has_workspace {
            self.root
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| self.root.clone())
        } else {
            default_project_location()
        }
        .to_string_lossy()
        .to_string();
        self.new_project_name.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        self.new_project_location.update(cx, |state, cx| {
            state.set_value(location, window, cx);
        });
        self.new_project_org.update(cx, |state, cx| {
            state.set_value("com.example", window, cx);
        });
        cx.notify();
    }

    pub(crate) fn submit_new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.new_project_name.read(cx).value().trim().to_string();
        let location = self.new_project_location.read(cx).value().trim().to_string();
        let org = self.new_project_org.read(cx).value().trim().to_string();
        if let Err(message) = validate_project_name(&name) {
            self.new_project_error = Some(message);
            cx.notify();
            return;
        }
        if location.is_empty() {
            self.new_project_error = Some("Choose where the project should live.".into());
            cx.notify();
            return;
        }
        let parent = expand_project_location(&location);
        let target = parent.join(&name);
        if target.exists() {
            self.new_project_error = Some(format!(
                "{} already exists. Pick another name or location.",
                target.display()
            ));
            cx.notify();
            return;
        }
        let template = PROJECT_TEMPLATES
            .get(self.new_project_template)
            .map(|(_, id)| *id)
            .unwrap_or("app");
        let org = if org.is_empty() { "com.example".to_string() } else { org };
        self.new_project_open = false;
        self.new_project_error = None;
        self.terminal_open = true;
        self.dock_tab = DockTab::DebugConsole;
        self.flutter_status = Some(format!("Creating {name}\u{2026}"));
        self.push_flutter_log(&format!(
            "\x1b[36m\u{2726} Creating Flutter project `{name}` ({template}) in {}...\x1b[0m",
            parent.display()
        ));
        cx.notify();

        let (tx, rx) = async_channel::bounded(1);
        let parent_for_thread = parent.clone();
        let name_for_thread = name.clone();
        std::thread::spawn(move || {
            let res = core_flutter::create_project(
                &parent_for_thread,
                &name_for_thread,
                Some(&org),
                Some(template),
            );
            let _ = tx.send_blocking(res);
        });

        let target_for_task = target.clone();
        let name_for_task = name.clone();
        cx.spawn_in(window, async move |weak, cx| {
            let Ok(res) = rx.recv().await else { return };
            let _ = weak.update_in(cx, |this, window, cx| {
                match res {
                    Ok(out) => {
                        for line in out.stdout.lines().chain(out.stderr.lines()) {
                            if !line.trim().is_empty() {
                                this.push_flutter_log(line);
                            }
                        }
                        if out.success {
                            this.set_root(target_for_task.clone(), cx);
                            this.push_flutter_log(&format!(
                                "\x1b[32m\u{2713} Created `{}`.\x1b[0m",
                                name_for_task
                            ));
                            let main = target_for_task.join("lib/main.dart");
                            if main.is_file() {
                                this.open_path(main, window, cx);
                            }
                        } else {
                            this.flutter_status = None;
                            this.new_project_open = true;
                            this.new_project_error =
                                Some("`flutter create` failed. See the Debug Console.".into());
                            this.push_flutter_log(
                                "\x1b[31m\u{2717} `flutter create` failed.\x1b[0m",
                            );
                        }
                    }
                    Err(err) => {
                        this.flutter_status = None;
                        this.new_project_open = true;
                        this.new_project_error = Some(format!("Couldn't run `flutter create`: {err}"));
                        this.push_flutter_log(&format!(
                            "\x1b[31mFailed to run `flutter create`: {err}\x1b[0m"
                        ));
                    }
                }
                this.flutter_status = None;
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn render_new_project_prompt(
        &self,
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if !self.new_project_open {
            return None;
        }
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let border = cx.theme().border;
        let field_bg = cx.theme().background;
        let field = |label: &'static str, hint: Option<&'static str>, input: &Entity<InputState>| {
            v_flex()
                .w_full()
                .gap(px(4.))
                .child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(tokens::UI_XS))
                                .text_color(muted)
                                .child(label),
                        )
                        .when_some(hint, |el, hint| {
                            el.child(
                                div()
                                    .text_size(px(tokens::UI_XS))
                                    .text_color(muted.opacity(0.65))
                                    .child(hint),
                            )
                        }),
                )
                .child(
                    h_flex()
                        .w_full()
                        .h(px(32.))
                        .px(px(9.))
                        .items_center()
                        .rounded(px(tokens::RADIUS_SM))
                        .bg(field_bg)
                        .border_1()
                        .border_color(border)
                        .child(Input::new(input).appearance(false).small().w_full()),
                )
        };
        let template_pill = |ix: usize, label: &'static str| {
            let selected = self.new_project_template == ix;
            h_flex()
                .id(("new-project-template", ix))
                .flex_1()
                .h(px(28.))
                .justify_center()
                .items_center()
                .rounded(px(tokens::RADIUS_SM))
                .cursor_pointer()
                .when(selected, |el| {
                    el.bg(cx.theme().primary.opacity(0.16))
                        .border_1()
                        .border_color(cx.theme().primary.opacity(0.55))
                        .text_color(fg)
                })
                .when(!selected, |el| {
                    el.bg(field_bg)
                        .border_1()
                        .border_color(border)
                        .text_color(muted)
                })
                .text_size(px(tokens::UI_SM))
                .child(label)
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    this.new_project_template = ix;
                    cx.notify();
                }))
        };
        let close = |view: &Entity<Self>| {
            h_flex()
                .id("new-project-close")
                .size(px(24.))
                .items_center()
                .justify_center()
                .rounded(px(tokens::RADIUS_SM))
                .cursor_pointer()
                .text_size(px(14.))
                .text_color(muted)
                .hover(|s| s.bg(rgba(0xffffff0c)).text_color(fg))
                .child("\u{00d7}")
                .on_click({
                    let view = view.clone();
                    move |_event, _window, cx| {
                        view.update(cx, |this, cx| {
                            this.new_project_open = false;
                            this.new_project_error = None;
                            cx.notify();
                        });
                    }
                })
        };

        Some(
            div()
                .occlude()
                .absolute()
                .inset_0()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(84.))
                .bg(rgba(0x00000066))
                .child(
                    v_flex()
                        .w(px(480.))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(border)
                        .rounded(px(tokens::RADIUS_CARD))
                        .shadow_xl()
                        .on_key_down({
                            let view = view.clone();
                            move |event, window, cx| {
                                match event.keystroke.key.as_str() {
                                    "escape" => view.update(cx, |this, cx| {
                                        this.new_project_open = false;
                                        this.new_project_error = None;
                                        cx.notify();
                                    }),
                                    "enter" => view.update(cx, |this, cx| {
                                        this.submit_new_project(window, cx)
                                    }),
                                    _ => {}
                                }
                            }
                        })
                        .child(
                            h_flex()
                                .w_full()
                                .px(px(16.))
                                .py(px(12.))
                                .items_center()
                                .justify_between()
                                .border_b_1()
                                .border_color(border)
                                .child(
                                    v_flex()
                                        .gap(px(2.))
                                        .child(
                                            div()
                                                .text_size(px(tokens::UI))
                                                .font_medium()
                                                .text_color(fg)
                                                .child("New Flutter Project"),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(tokens::UI_SM))
                                                .text_color(muted)
                                                .child("Create a project, then open it here."),
                                        ),
                                )
                                .child(close(view)),
                        )
                        .child(
                            v_flex()
                                .w_full()
                                .px(px(16.))
                                .py(px(14.))
                                .gap(px(12.))
                                .child(field("Project name", None, &self.new_project_name))
                                .child(field(
                                    "Location",
                                    Some("A new folder is created inside this directory."),
                                    &self.new_project_location,
                                ))
                                .child(field(
                                    "Organization",
                                    Some("Used for the bundle identifier."),
                                    &self.new_project_org,
                                ))
                                .child(
                                    v_flex()
                                        .w_full()
                                        .gap(px(6.))
                                        .child(
                                            div()
                                                .text_size(px(tokens::UI_XS))
                                                .text_color(muted)
                                                .child("Template"),
                                        )
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .gap(px(4.))
                                                .children(PROJECT_TEMPLATES.iter().enumerate().map(
                                                    |(ix, (label, _))| template_pill(ix, label),
                                                )),
                                        ),
                                )
                                .when_some(self.new_project_error.clone(), |el, error| {
                                    el.child(
                                        h_flex()
                                            .w_full()
                                            .px(px(10.))
                                            .py(px(8.))
                                            .rounded(px(tokens::RADIUS_SM))
                                            .bg(cx.theme().danger.opacity(0.12))
                                            .text_size(px(tokens::UI_SM))
                                            .text_color(cx.theme().danger)
                                            .child(error),
                                    )
                                }),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .px(px(16.))
                                .py(px(10.))
                                .items_center()
                                .justify_between()
                                .border_t_1()
                                .border_color(border)
                                .child(
                                    div()
                                        .text_size(px(tokens::UI_XS))
                                        .text_color(muted.opacity(0.7))
                                        .child("Runs flutter create in the Debug Console"),
                                )
                                .child(
                                    h_flex()
                                        .gap(px(8.))
                                        .child(
                                            h_flex()
                                                .id("new-project-cancel")
                                                .h(px(28.))
                                                .px(px(12.))
                                                .items_center()
                                                .rounded(px(tokens::RADIUS_SM))
                                                .cursor_pointer()
                                                .border_1()
                                                .border_color(border)
                                                .text_size(px(tokens::UI_SM))
                                                .text_color(muted)
                                                .hover(|s| s.text_color(fg).bg(rgba(0xffffff08)))
                                                .child("Cancel")
                                                .on_click(cx.listener(|this, _event, _window, cx| {
                                                    this.new_project_open = false;
                                                    this.new_project_error = None;
                                                    cx.notify();
                                                })),
                                        )
                                        .child(
                                            h_flex()
                                                .id("new-project-create")
                                                .h(px(28.))
                                                .px(px(14.))
                                                .items_center()
                                                .rounded(px(tokens::RADIUS_SM))
                                                .cursor_pointer()
                                                .bg(cx.theme().primary)
                                                .text_size(px(tokens::UI_SM))
                                                .font_medium()
                                                .text_color(cx.theme().primary_foreground)
                                                .hover(|s| s.bg(cx.theme().primary.opacity(0.88)))
                                                .child("Create")
                                                .on_click(cx.listener(|this, _event, window, cx| {
                                                    this.submit_new_project(window, cx);
                                                })),
                                        ),
                                ),
                        ),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{expand_project_location, validate_project_name};

    #[core::prelude::v1::test]
    fn project_names_follow_flutters_package_rule() {
        for good in ["app", "my_app", "app2", "a_b_c"] {
            assert!(validate_project_name(good).is_ok(), "{good} should be valid");
        }
        for bad in ["", "MyApp", "my-app", "2app", "my app", "app!"] {
            assert!(validate_project_name(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[core::prelude::v1::test]
    fn project_locations_expand_a_leading_tilde() {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let expanded = expand_project_location("~/demo");
        if let Some(home) = home {
            assert_eq!(expanded, home.join("demo"));
        }
        assert!(expand_project_location("relative_demo").is_absolute());
    }
}
