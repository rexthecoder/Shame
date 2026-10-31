//! Keeping files safe: autosave, noticing changes made outside the editor
//! (by an agent, git or a build), and not losing unsaved work when a tab, the
//! folder or the whole app is closed.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use editor_view::{DiskState, EditorView, SaveOptions};
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, tokens};

/// How long after the last edit a modified file is saved automatically.
pub(crate) const AUTOSAVE_DELAY: Duration = Duration::from_millis(1000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutosaveMode {
    Off,
    /// A second after you stop typing.
    AfterDelay,
    /// When Shame loses focus (you switch to another app).
    OnFocusChange,
}

impl AutosaveMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            AutosaveMode::Off => "Off",
            AutosaveMode::AfterDelay => "After a delay",
            AutosaveMode::OnFocusChange => "When focus leaves Shame",
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            AutosaveMode::Off => "off",
            AutosaveMode::AfterDelay => "delay",
            AutosaveMode::OnFocusChange => "focus",
        }
    }

    pub(crate) fn from_key(key: &str) -> Self {
        match key {
            "delay" => AutosaveMode::AfterDelay,
            "focus" => AutosaveMode::OnFocusChange,
            _ => AutosaveMode::Off,
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            AutosaveMode::Off => AutosaveMode::AfterDelay,
            AutosaveMode::AfterDelay => AutosaveMode::OnFocusChange,
            AutosaveMode::OnFocusChange => AutosaveMode::Off,
        }
    }
}

/// Whether a file should be autosaved right now. `idle` is the time since its
/// last edit (`None` if that moment is still in the future, e.g. a save was
/// just started).
pub(crate) fn autosave_due(
    mode: AutosaveMode,
    dirty: bool,
    has_path: bool,
    idle: Option<Duration>,
    delay: Duration,
) -> bool {
    mode == AutosaveMode::AfterDelay && dirty && has_path && idle.is_some_and(|idle| idle >= delay)
}

/// "main.dart" / "a.dart and b.dart" / "a.dart, b.dart and 2 more".
pub(crate) fn unsaved_summary(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} more", rest.len()),
    }
}

/// Something worth telling you about the file in the active tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FileNotice {
    /// Changed on disk while it has unsaved edits here.
    ChangedOnDisk,
    DeletedOnDisk,
    SaveFailed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AfterUnsaved {
    CloseTab(usize),
    CloseFolder,
    Quit,
}

pub(crate) struct UnsavedPrompt {
    pub tabs: Vec<usize>,
    pub then: AfterUnsaved,
}

impl DartIde {
    /// Indices of open tabs with unsaved edits to a file on disk.
    pub(crate) fn unsaved_tabs(&self, cx: &App) -> Vec<usize> {
        self.tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                let buffer = t.editor.read(cx).buffer();
                buffer.is_dirty() && buffer.path().is_some()
            })
            .map(|(ix, _)| ix)
            .collect()
    }

    fn unsaved_names(&self, tabs: &[usize]) -> Vec<String> {
        tabs.iter().filter_map(|&ix| self.tabs.get(ix)).map(|t| t.title()).collect()
    }

    /// Close a tab, asking first if it has unsaved edits.
    pub(crate) fn request_close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.unsaved_tabs(cx).contains(&ix) {
            self.unsaved_prompt = Some(UnsavedPrompt { tabs: vec![ix], then: AfterUnsaved::CloseTab(ix) });
            cx.notify();
        } else {
            self.close_tab(ix, cx);
        }
    }

    pub(crate) fn request_close_workspace(&mut self, cx: &mut Context<Self>) {
        self.request_after_unsaved(AfterUnsaved::CloseFolder, cx);
    }

    pub(crate) fn request_quit(&mut self, cx: &mut Context<Self>) {
        self.request_after_unsaved(AfterUnsaved::Quit, cx);
    }

    fn request_after_unsaved(&mut self, then: AfterUnsaved, cx: &mut Context<Self>) {
        let tabs = self.unsaved_tabs(cx);
        if tabs.is_empty() {
            self.proceed_after_unsaved(then, cx);
        } else {
            self.unsaved_prompt = Some(UnsavedPrompt { tabs, then });
            cx.notify();
        }
    }

    fn proceed_after_unsaved(&mut self, then: AfterUnsaved, cx: &mut Context<Self>) {
        match then {
            AfterUnsaved::CloseTab(ix) => self.close_tab(ix, cx),
            AfterUnsaved::CloseFolder => self.close_workspace(cx),
            AfterUnsaved::Quit => cx.quit(),
        }
    }

    fn unsaved_save(&mut self, cx: &mut Context<Self>) {
        let Some(prompt) = self.unsaved_prompt.take() else { return };
        for &ix in &prompt.tabs {
            if let Some(tab) = self.tabs.get(ix) {
                tab.editor.update(cx, |editor, cx| editor.save_without_format(cx));
            }
        }
        // If a write failed the file is still dirty: stay, with the error banner.
        let failed = prompt
            .tabs
            .iter()
            .any(|&ix| self.tabs.get(ix).is_some_and(|t| t.editor.read(cx).buffer().is_dirty()));
        if failed {
            cx.notify();
            return;
        }
        self.proceed_after_unsaved(prompt.then, cx);
    }

    fn unsaved_discard(&mut self, cx: &mut Context<Self>) {
        if let Some(prompt) = self.unsaved_prompt.take() {
            self.proceed_after_unsaved(prompt.then, cx);
        }
    }

    pub(crate) fn save_all(&mut self, cx: &mut Context<Self>) {
        for ix in self.unsaved_tabs(cx) {
            if let Some(tab) = self.tabs.get(ix) {
                tab.editor.update(cx, |editor, cx| editor.save(cx));
            }
        }
    }

    // ------------------------------------------------------------ settings

    /// Publish the save options to the editors.
    pub(crate) fn apply_save_options(&self, cx: &mut Context<Self>) {
        cx.set_global(self.save_options);
    }

    pub(crate) fn set_autosave(&mut self, mode: AutosaveMode, cx: &mut Context<Self>) {
        self.autosave = mode;
        self.save_session_state();
        cx.notify();
    }

    pub(crate) fn update_save_options(
        &mut self,
        change: impl FnOnce(&mut SaveOptions),
        cx: &mut Context<Self>,
    ) {
        change(&mut self.save_options);
        self.apply_save_options(cx);
        self.save_session_state();
        cx.notify();
    }

    // ------------------------------------------------------------ polling

    /// Twice a second: autosave quiet files, and every couple of seconds look
    /// for files that changed on disk.
    pub(crate) fn tick_files(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        self.disk_tick = self.disk_tick.wrapping_add(1);

        let mut due: Vec<Entity<EditorView>> = Vec::new();
        for tab in &self.tabs {
            let id = tab.editor.entity_id();
            let (rev, dirty, path) = {
                let buffer = tab.editor.read(cx).buffer();
                (buffer.revision(), buffer.is_dirty(), buffer.path().map(PathBuf::from))
            };
            let mark = self.edit_marks.entry(id).or_insert((rev, now));
            if mark.0 != rev {
                *mark = (rev, now);
            }
            // Never autosave over a version that changed on disk meanwhile.
            let conflicted = path
                .as_ref()
                .is_some_and(|p| matches!(
                    self.file_notices.get(p),
                    Some(FileNotice::ChangedOnDisk | FileNotice::DeletedOnDisk)
                ));
            if !conflicted
                && autosave_due(self.autosave, dirty, path.is_some(), now.checked_duration_since(mark.1), AUTOSAVE_DELAY)
            {
                due.push(tab.editor.clone());
            }
        }
        for editor in due {
            // Hold off retrying while a save (which may wait for the formatter) is under way.
            self.edit_marks
                .insert(editor.entity_id(), (editor.read(cx).buffer().revision(), now + Duration::from_secs(3)));
            editor.update(cx, |editor, cx| editor.save(cx));
        }

        if self.disk_tick % 4 == 0 {
            self.check_disk_changes(cx);
        }
    }

    /// Reload clean tabs whose file changed on disk; flag dirty ones.
    pub(crate) fn check_disk_changes(&mut self, cx: &mut Context<Self>) {
        let editors: Vec<Entity<EditorView>> = self.tabs.iter().map(|t| t.editor.clone()).collect();
        let mut changed = false;
        for editor in editors {
            let (path, dirty, state) = {
                let ed = editor.read(cx);
                (ed.buffer().path().map(PathBuf::from), ed.buffer().is_dirty(), ed.disk_state())
            };
            let Some(path) = path else { continue };
            match (state, dirty) {
                (DiskState::Unchanged, _) => {
                    // The conflict went away (saved, reloaded, or the file is back).
                    if matches!(
                        self.file_notices.get(&path),
                        Some(FileNotice::ChangedOnDisk | FileNotice::DeletedOnDisk)
                    ) {
                        self.file_notices.remove(&path);
                        changed = true;
                    }
                }
                (DiskState::Modified, false) => {
                    // Nothing of yours to lose: just show what is on disk now.
                    let ok = editor.update(cx, |ed, cx| ed.reload_from_disk(cx).is_ok());
                    if ok {
                        self.file_notices.remove(&path);
                        changed = true;
                    }
                }
                (DiskState::Modified, true) => {
                    if self.file_notices.get(&path) != Some(&FileNotice::ChangedOnDisk) {
                        self.file_notices.insert(path, FileNotice::ChangedOnDisk);
                        changed = true;
                    }
                }
                (DiskState::Deleted, _) => {
                    if self.file_notices.get(&path) != Some(&FileNotice::DeletedOnDisk) {
                        self.file_notices.insert(path, FileNotice::DeletedOnDisk);
                        changed = true;
                    }
                }
            }
        }
        if changed {
            cx.notify();
        }
    }

    // ------------------------------------------------------------ rendering

    /// The banner above the editor for the active tab's file, if it needs attention.
    pub(crate) fn render_file_notice(&self, cx: &mut Context<Self>) -> Option<Div> {
        let tab = self.tabs.get(self.active_tab)?;
        let path = tab.path.clone()?;
        let notice = self.file_notices.get(&path)?.clone();
        let name = tab.title();
        let fg = cx.theme().foreground;
        let warning = cx.theme().warning;
        let red = cx.theme().red;

        let button = |id: &'static str, label: &'static str, primary: bool| {
            div()
                .id(id)
                .h(px(22.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(tokens::RADIUS_SM))
                .cursor_pointer()
                .text_size(px(tokens::UI_XS))
                .when(primary, |b| b.bg(warning.opacity(0.28)).text_color(fg))
                .when(!primary, |b| b.text_color(cx.theme().muted_foreground).hover(move |h| h.text_color(fg)))
                .child(label)
        };

        let (message, tint, actions): (String, Hsla, Vec<AnyElement>) = match notice {
            FileNotice::ChangedOnDisk => (
                format!("{name} changed on disk, and you have unsaved edits."),
                warning,
                vec![
                    button("notice-keep", "Keep mine", false)
                        .on_click({
                            let path = path.clone();
                            cx.listener(move |this, _e, _w, cx| {
                                if let Some(tab) = this.tabs.iter().find(|t| t.path.as_ref() == Some(&path)) {
                                    tab.editor.update(cx, |ed, cx| ed.keep_my_version(cx));
                                }
                                this.file_notices.remove(&path);
                                cx.notify();
                            })
                        })
                        .into_any_element(),
                    button("notice-reload", "Reload from disk", true)
                        .on_click({
                            let path = path.clone();
                            cx.listener(move |this, _e, _w, cx| {
                                if let Some(tab) = this.tabs.iter().find(|t| t.path.as_ref() == Some(&path)) {
                                    tab.editor.update(cx, |ed, cx| {
                                        let _ = ed.reload_from_disk(cx);
                                    });
                                }
                                this.file_notices.remove(&path);
                                cx.notify();
                            })
                        })
                        .into_any_element(),
                ],
            ),
            FileNotice::DeletedOnDisk => (
                format!("{name} was deleted on disk."),
                warning,
                vec![
                    button("notice-restore", "Save to restore", true)
                        .on_click({
                            let path = path.clone();
                            cx.listener(move |this, _e, _w, cx| {
                                if let Some(tab) = this.tabs.iter().find(|t| t.path.as_ref() == Some(&path)) {
                                    tab.editor.update(cx, |ed, cx| ed.save_without_format(cx));
                                }
                                this.file_notices.remove(&path);
                                cx.notify();
                            })
                        })
                        .into_any_element(),
                ],
            ),
            FileNotice::SaveFailed(error) => (
                format!("Couldn't save {name}: {error}"),
                red,
                vec![
                    button("notice-dismiss", "Dismiss", false)
                        .on_click({
                            let path = path.clone();
                            cx.listener(move |this, _e, _w, cx| {
                                this.file_notices.remove(&path);
                                cx.notify();
                            })
                        })
                        .into_any_element(),
                    button("notice-retry", "Retry", true)
                        .on_click({
                            let path = path.clone();
                            cx.listener(move |this, _e, _w, cx| {
                                this.file_notices.remove(&path);
                                if let Some(tab) = this.tabs.iter().find(|t| t.path.as_ref() == Some(&path)) {
                                    tab.editor.update(cx, |ed, cx| ed.save_without_format(cx));
                                }
                                cx.notify();
                            })
                        })
                        .into_any_element(),
                ],
            ),
        };

        Some(
            h_flex()
                .w_full()
                .px(px(tokens::SPACE_4))
                .py(px(5.))
                .gap(px(tokens::SPACE_3))
                .items_center()
                .justify_between()
                .flex_shrink_0()
                .bg(tint.opacity(0.12))
                .border_b_1()
                .border_color(tint.opacity(0.35))
                .child(
                    div()
                        .min_w(px(0.))
                        .truncate()
                        .text_size(px(tokens::UI_SM))
                        .text_color(fg)
                        .child(message),
                )
                .child(h_flex().gap(px(4.)).flex_shrink_0().children(actions)),
        )
    }

    /// "Do you want to save the changes?" before closing something with unsaved edits.
    pub(crate) fn render_unsaved_prompt(&self, cx: &mut Context<Self>) -> Option<Div> {
        let prompt = self.unsaved_prompt.as_ref()?;
        let names = self.unsaved_names(&prompt.tabs);
        let many = names.len() > 1;
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let title = if many {
            format!("Save changes to {} files?", names.len())
        } else {
            format!("Save changes to {}?", names.first().cloned().unwrap_or_default())
        };
        let detail = if many {
            format!("{} have unsaved changes. They will be lost if you don\u{2019}t save them.", unsaved_summary(&names))
        } else {
            "Your changes will be lost if you don\u{2019}t save them.".to_string()
        };

        let button = |id: &'static str, label: &'static str, primary: bool| {
            div()
                .id(id)
                .h(px(28.))
                .px(px(14.))
                .flex()
                .items_center()
                .rounded(px(tokens::RADIUS_SM))
                .cursor_pointer()
                .text_size(px(tokens::UI_SM))
                .border_1()
                .border_color(border)
                .when(primary, |b| {
                    b.bg(cx.theme().primary).text_color(cx.theme().primary_foreground).border_color(cx.theme().primary)
                })
                .when(!primary, |b| b.text_color(fg).hover(|h| h.bg(cx.theme().list_hover)))
                .child(label)
        };

        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x00000099))
                .child(
                    v_flex()
                        .w(px(420.))
                        .p(px(tokens::SPACE_5))
                        .gap(px(tokens::SPACE_3))
                        .rounded(px(tokens::RADIUS_CARD))
                        .bg(cx.theme().popover)
                        .border_1()
                        .border_color(border)
                        .shadow_xl()
                        .child(div().text_size(px(tokens::UI)).font_medium().text_color(fg).child(title))
                        .child(div().text_size(px(tokens::UI_SM)).text_color(muted).child(detail))
                        .child(
                            h_flex()
                                .w_full()
                                .justify_end()
                                .gap(px(tokens::SPACE_2))
                                .pt(px(tokens::SPACE_2))
                                .child(
                                    button("unsaved-discard", "Don\u{2019}t Save", false)
                                        .on_click(cx.listener(|this, _e, _w, cx| this.unsaved_discard(cx))),
                                )
                                .child(
                                    button("unsaved-cancel", "Cancel", false).on_click(cx.listener(
                                        |this, _e, _w, cx| {
                                            this.unsaved_prompt = None;
                                            cx.notify();
                                        },
                                    )),
                                )
                                .child(
                                    button("unsaved-save", if many { "Save All" } else { "Save" }, true)
                                        .on_click(cx.listener(|this, _e, _w, cx| this.unsaved_save(cx))),
                                ),
                        ),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{AUTOSAVE_DELAY, AutosaveMode, autosave_due, unsaved_summary};
    use std::time::Duration;

    #[core::prelude::v1::test]
    fn autosave_waits_for_quiet_and_only_when_enabled() {
        let due = |mode, dirty, has_path, idle: Option<u64>| {
            autosave_due(mode, dirty, has_path, idle.map(Duration::from_millis), AUTOSAVE_DELAY)
        };
        assert!(due(AutosaveMode::AfterDelay, true, true, Some(1200)));
        assert!(!due(AutosaveMode::AfterDelay, true, true, Some(300)), "still typing");
        assert!(!due(AutosaveMode::AfterDelay, true, true, None), "a save just started");
        assert!(!due(AutosaveMode::AfterDelay, false, true, Some(5000)), "nothing to save");
        assert!(!due(AutosaveMode::AfterDelay, true, false, Some(5000)), "scratch buffers have no file");
        assert!(!due(AutosaveMode::Off, true, true, Some(5000)));
        assert!(!due(AutosaveMode::OnFocusChange, true, true, Some(5000)), "that one saves on blur instead");
    }

    #[core::prelude::v1::test]
    fn autosave_modes_cycle_and_round_trip_through_their_key() {
        let mut mode = AutosaveMode::Off;
        let mut seen = Vec::new();
        for _ in 0..3 {
            mode = mode.next();
            seen.push(mode);
        }
        assert_eq!(seen, vec![AutosaveMode::AfterDelay, AutosaveMode::OnFocusChange, AutosaveMode::Off]);
        for m in [AutosaveMode::Off, AutosaveMode::AfterDelay, AutosaveMode::OnFocusChange] {
            assert_eq!(AutosaveMode::from_key(m.key()), m);
        }
        assert_eq!(AutosaveMode::from_key("nonsense"), AutosaveMode::Off);
    }

    #[core::prelude::v1::test]
    fn summarizes_unsaved_files_briefly() {
        let n = |names: &[&str]| unsaved_summary(&names.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(n(&[]), "");
        assert_eq!(n(&["a.dart"]), "a.dart");
        assert_eq!(n(&["a.dart", "b.dart"]), "a.dart and b.dart");
        assert_eq!(n(&["a", "b", "c", "d"]), "a, b and 2 more");
    }
}
