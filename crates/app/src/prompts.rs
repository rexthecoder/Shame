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
