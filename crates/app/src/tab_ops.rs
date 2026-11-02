//! Opening, closing and splitting editor tabs.

use super::*;

impl DartIde {
    pub(crate) fn active_editor(&self) -> Option<&Entity<EditorView>> {
        self.tabs.get(self.active_tab).map(|t| &t.editor)
    }

    pub(crate) fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_path_with_mode(path, false, window, cx);
    }

    pub(crate) fn open_path_with_mode(
        &mut self,
        path: PathBuf,
        as_preview: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diff = None;
        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());

        //Promote any existing tab that has been edited so `is_preview` stays accurate.
        for tab in &mut self.tabs {
            if tab.editor.read(cx).buffer().revision() != tab.opened_revision {
                tab.is_preview = false;
            }
        }

        if let Some(ix) = self.tabs.iter().position(|t| {
            t.path.as_ref().is_some_and(|existing| {
                existing == &canonical
                    || std::fs::canonicalize(existing)
                        .map(|c| c == canonical)
                        .unwrap_or(false)
            })
        }) {
            if !as_preview {
                self.tabs[ix].is_preview = false;
            }
            self.active_tab = ix;
        } else if is_image_path(&canonical) {
            let Some(preview) = load_image_preview(&canonical) else {
                eprintln!("failed to decode image {}", canonical.display());
                return;
            };
            let editor = cx.new(|cx| EditorView::new(cx));
            let new_tab = Tab {
                path: Some(canonical),
                editor,
                md_mode: MarkdownMode::Code,
                image: Some(preview),
                is_preview: as_preview,
                opened_revision: 0,
            };
            if as_preview {
                if let Some(preview_ix) = self.tabs.iter().position(|t| t.is_preview) {
                    self.tabs[preview_ix] = new_tab;
                    self.active_tab = preview_ix;
                } else {
                    self.tabs.push(new_tab);
                    self.active_tab = self.tabs.len() - 1;
                }
            } else {
                self.tabs.push(new_tab);
                self.active_tab = self.tabs.len() - 1;
            }
        } else {
            let buffer = match Buffer::open(&canonical) {
                Ok(buffer) => buffer,
                Err(err) => {
                    eprintln!("failed to open {}: {err}", canonical.display());
                    return;
                }
            };
            let opened_revision = buffer.revision();
            let lenses = self.current_test_code_lenses();
            let saved_bps = self.dap_breakpoints.get(&canonical).cloned();
            let editor = cx.new(|cx| {
                let mut ed = EditorView::from_buffer(buffer, cx);
                ed.set_test_code_lenses(lenses, cx);
                if let Some(bps) = saved_bps {
                    ed.set_breakpoints(bps, cx);
                }
                ed
            });
            self.watch_editor(&editor, window, cx);
            let new_tab = Tab {
                path: Some(canonical),
                editor,
                md_mode: MarkdownMode::Code,
                image: None,
                is_preview: as_preview,
                opened_revision,
            };

            // Zed-style preview slot: if opening in preview mode and an
            // unmodified preview tab already exists, replace it in-place so
            // browsing the file tree doesn't spawn dozens of transient tabs.
            if as_preview {
                if let Some(preview_ix) = self.tabs.iter().position(|t| t.is_preview) {
                    self.tabs[preview_ix] = new_tab;
                    self.active_tab = preview_ix;
                } else {
                    self.tabs.push(new_tab);
                    self.active_tab = self.tabs.len() - 1;
                }
            } else {
                self.tabs.push(new_tab);
                self.active_tab = self.tabs.len() - 1;
            }
        }

        self.tab_overflow_open = false;
        self.tab_scroll.scroll_to_item(self.active_tab);

        // Opening a file feeds the search index's frecency ranking, so the
        // files used most come first in Quick Open.
        if let Some(index) = &self.search_index
            && let Some(path) = self.tabs.get(self.active_tab).and_then(|t| t.path.as_ref())
        {
            index.note_opened(path);
        }

        if let Some(editor) = self.active_editor() {
            let focus_handle = editor.read(cx).focus_handle(cx);
            window.focus(&focus_handle, cx);
        }
        self.save_session_state();
        cx.notify();
    }

    pub(crate) fn new_scratch_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = cx.new(|cx| EditorView::new(cx));
        let opened_revision = editor.read(cx).buffer().revision();
        self.watch_editor(&editor, window, cx);
        self.tabs.push(Tab {
            path: None,
            editor,
            md_mode: MarkdownMode::Code,
            image: None,
            is_preview: false,
            opened_revision,
        });
        self.active_tab = self.tabs.len() - 1;
        self.tab_overflow_open = false;
        self.tab_scroll.scroll_to_item(self.active_tab);

        if let Some(editor) = self.active_editor() {
            let focus_handle = editor.read(cx).focus_handle(cx);
            window.focus(&focus_handle, cx);
        }
        self.save_session_state();
        cx.notify();
    }

    pub(crate) fn close_other_tabs(&mut self, keep_ix: usize, cx: &mut Context<Self>) {
        if let Some(keep) = self.tabs.get(keep_ix) {
            let keep_tab = Tab {
                path: keep.path.clone(),
                editor: keep.editor.clone(),
                md_mode: keep.md_mode,
                image: keep.image.clone(),
                is_preview: false,
                opened_revision: keep.opened_revision,
            };
            self.tabs = vec![keep_tab];
            self.active_tab = 0;
            self.tab_overflow_open = false;
            cx.notify();
        }
    }

    pub(crate) fn close_clean_tabs(&mut self, cx: &mut Context<Self>) {
        let active_path = self.tabs.get(self.active_tab).and_then(|t| t.path.clone());
        self.tabs.retain(|t| t.editor.read(cx).buffer().is_dirty());
        if self.tabs.is_empty() {
            self.active_tab = 0;
        } else if let Some(path) = active_path {
            self.active_tab = self
                .tabs
                .iter()
                .position(|t| t.path.as_ref() == Some(&path))
                .unwrap_or(0);
        } else {
            self.active_tab = 0;
        }
        self.tab_overflow_open = false;
        cx.notify();
    }

    pub(crate) fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.tabs.remove(ix);
        if self.tabs.is_empty() {
            self.active_tab = 0;
            self.split_tab = None;
        } else if self.active_tab > ix {
            self.active_tab -= 1;
        } else {
            self.active_tab = self.active_tab.min(self.tabs.len() - 1);
        }
        if let Some(sp) = self.split_tab {
            if sp == ix || self.tabs.is_empty() {
                self.split_tab = None;
            } else if sp > ix {
                self.split_tab = Some(sp - 1);
            }
        }
        self.notify_tab_change(cx);
    }

    pub(crate) fn toggle_split_editor(&mut self, cx: &mut Context<Self>) {
        if self.split_tab.is_some() {
            self.split_tab = None;
        } else if !self.tabs.is_empty() {
            let right_ix = if self.tabs.len() > 1 {
                (self.active_tab + 1) % self.tabs.len()
            } else {
                self.active_tab
            };
            self.split_tab = Some(right_ix);
        }
        cx.notify();
    }

    pub(crate) fn notify_tab_change(&mut self, cx: &mut Context<Self>) {
        self.save_session_state();
        cx.notify();
    }

    /// Apply cross-file `WorkspaceEdit` changes to an open tab or directly to disk.
    pub(crate) fn apply_external_file_edits(
        &mut self,
        path: PathBuf,
        mut edits: Vec<lsp_types::TextEdit>,
        cx: &mut Context<Self>,
    ) {
        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if let Some(tab) = self.tabs.iter().find(|t| {
            t.path.as_ref().is_some_and(|existing| {
                existing == &canonical
                    || std::fs::canonicalize(existing)
                        .map(|c| c == canonical)
                        .unwrap_or(false)
            })
        }) {
            let editor = tab.editor.clone();
            editor.update(cx, |ed, cx| ed.apply_external_edits(edits, cx));
        } else if let Ok(mut buffer) = Buffer::open(&canonical) {
            edits.sort_by_key(|edit| (edit.range.start.line, edit.range.start.character));
            for edit in edits.into_iter().rev() {
                let start = buffer.offset_from_lsp(edit.range.start.line, edit.range.start.character);
                let end = buffer.offset_from_lsp(edit.range.end.line, edit.range.end.character);
                buffer.edit(start.min(end)..end, &edit.new_text);
            }
            let _ = buffer.save();
        }
        self.refresh_git(cx);
        cx.notify();
    }
}
