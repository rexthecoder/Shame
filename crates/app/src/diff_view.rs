//! The side-by-side diff view shown in the editor area when a file is clicked in the Git panel:
//! old version on the left, new on the right, changed lines tinted, the missing side of an
//! addition / deletion left empty, and long unchanged stretches folded into a clickable gap.
//!
//! The diff itself comes from `core_workspace::file_diff`; syntax colors (Dart only, like the
//! editor) come from `core_syntax`. Both are computed off the UI thread.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;

use core_syntax::{HighlightKind, SyntaxTree};
use core_workspace::{Block, ChangeKind, ChangedFile, DiffCell, FileDiff, RowKind};
use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{DartIde, tokens};

/// Unchanged lines kept visible around every change.
const CONTEXT: usize = 3;
const ROW_H: f32 = 22.;
const GUTTER_W: f32 = 52.;

type Spans = Vec<(Range<usize>, HighlightKind)>;

enum DisplayItem {
    Row(usize),
    /// In unified mode, a `RowKind::Changed` row splits into its removed half and added half.
    UnifiedLeft(usize),
    UnifiedRight(usize),
    /// A folded run of unchanged rows `start..end`.
    Gap {
        start: usize,
        end: usize,
    },
}

pub(crate) struct DiffState {
    pub(crate) file: ChangedFile,
    pub(crate) staged: bool,
    /// `Some(hash)` when viewing a historical commit rather than staged / working-tree changes.
    pub(crate) commit: Option<String>,
    pub(crate) unified: bool,
    pub(crate) full_path: PathBuf,
    diff: FileDiff,
    /// Syntax spans per line (0-based) of the old / new text.
    old_spans: Vec<Spans>,
    new_spans: Vec<Spans>,
    /// Gaps (by their first row) the developer has unfolded.
    expanded: HashSet<usize>,
    display: Vec<DisplayItem>,
    scroll: UniformListScrollHandle,
    /// Runs of changed rows; the unit for hunk staging, navigation and the change map.
    blocks: Vec<Block>,
    /// For each block, the index of its first row in `display`.
    block_display: Vec<usize>,
    /// Which block a row starts, if any.
    block_at_row: HashMap<usize, usize>,
    /// Changed words inside replaced lines: row -> (old-side ranges, new-side ranges).
    intra: HashMap<usize, (Vec<Range<usize>>, Vec<Range<usize>>)>,
    /// The block last navigated to (drives the "3 / 12" counter and the change-map marker).
    current_change: usize,
}

impl DiffState {
    fn rebuild_display(&mut self) {
        let n = self.diff.rows.len();
        let unified = self.unified;
        let mut near_change = vec![false; n];
        for (i, row) in self.diff.rows.iter().enumerate() {
            if row.is_change() {
                for flag in &mut near_change[i.saturating_sub(CONTEXT)..(i + CONTEXT + 1).min(n)] {
                    *flag = true;
                }
            }
        }
        let push_row = |display: &mut Vec<DisplayItem>, ix: usize, rows: &[core_workspace::DiffRow]| {
            if unified && rows[ix].kind == RowKind::Changed {
                display.push(DisplayItem::UnifiedLeft(ix));
                display.push(DisplayItem::UnifiedRight(ix));
            } else {
                display.push(DisplayItem::Row(ix));
            }
        };
        let mut display = Vec::new();
        let mut i = 0;
        while i < n {
            if near_change[i] {
                push_row(&mut display, i, &self.diff.rows);
                i += 1;
                continue;
            }
            let start = i;
            while i < n && !near_change[i] {
                i += 1;
            }
            if self.expanded.contains(&start) {
                for r in start..i {
                    push_row(&mut display, r, &self.diff.rows);
                }
            } else {
                display.push(DisplayItem::Gap { start, end: i });
            }
        }
        self.display = display;
        self.block_display = self
            .blocks
            .iter()
            .map(|b| {
                self.display
                    .iter()
                    .position(|d| {
                        matches!(
                            d,
                            DisplayItem::Row(r) | DisplayItem::UnifiedLeft(r) if *r == b.start
                        )
                    })
                    .unwrap_or(0)
            })
            .collect();
    }
}

/// Syntax spans for each line of `text`, from a Dart parse. Overlaps are dropped so the spans can
/// be handed straight to `StyledText`.
fn line_spans(text: &str) -> Vec<Spans> {
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let mut out: Vec<Spans> = vec![Vec::new(); line_starts.len()];
    let mut highlights = SyntaxTree::parse(text).highlights(text);
    highlights.sort_by_key(|h| (h.range.start, h.range.end));
    let mut cursor = 0;
    for h in highlights {
        let (start, end) = (h.range.start.max(cursor), h.range.end);
        if start >= end || end > text.len() {
            continue;
        }
        cursor = end;
        // Split a multi-line token (block comment, multi-line string) per line.
        let mut line = line_starts.partition_point(|&s| s <= start) - 1;
        let mut pos = start;
        while pos < end && line < line_starts.len() {
            let line_end = line_starts.get(line + 1).map_or(text.len(), |&s| s - 1);
            let seg_end = end.min(line_end);
            if pos < seg_end {
                out[line].push((pos - line_starts[line]..seg_end - line_starts[line], h.kind));
            }
            pos = line_starts.get(line + 1).copied().unwrap_or(end);
            line += 1;
        }
    }
    out
}

impl DartIde {
    /// Open `file`'s diff in the editor area (`staged`: HEAD vs the index, else index vs working tree).
    pub(crate) fn open_git_diff(
        &mut self,
        file: ChangedFile,
        staged: bool,
        cx: &mut Context<Self>,
    ) {
        self.load_git_diff(file, staged, None, cx);
    }

    /// Open `file`'s diff from a specific commit `hash` in the editor area.
    pub(crate) fn open_git_commit_diff(
        &mut self,
        commit: String,
        file: ChangedFile,
        cx: &mut Context<Self>,
    ) {
        self.load_git_diff(file, false, Some(commit), cx);
    }

    fn load_git_diff(
        &mut self,
        file: ChangedFile,
        staged: bool,
        commit: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.git.status.as_ref().map(|s| s.root.clone()) else {
            return;
        };
        self.diff_seq += 1;
        let seq = self.diff_seq;
        let expanded = self
            .diff
            .as_ref()
            .filter(|d| d.file.path == file.path && d.commit == commit)
            .map(|d| d.expanded.clone())
            .unwrap_or_default();
        let (tx, rx) = async_channel::bounded(1);
        let job_file = file.clone();
        let job_root = root.clone();
        let job_commit = commit.clone();
        std::thread::spawn(move || {
            let raw = match &job_commit {
                Some(hash) => core_workspace::commit_file_diff(&job_root, hash, &job_file),
                None => core_workspace::file_diff(&job_root, &job_file, staged),
            };
            let result = raw.map(|diff| {
                let is_dart = job_file.path.ends_with(".dart");
                let (old_spans, new_spans) = if is_dart && !diff.binary {
                    (line_spans(&diff.old_text()), line_spans(&diff.new_text()))
                } else {
                    (Vec::new(), Vec::new())
                };
                let intra: HashMap<usize, _> = diff
                    .rows
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.kind == RowKind::Changed)
                    .filter_map(|(i, r)| {
                        let (old, new) = core_workspace::intraline(
                            &r.left.as_ref()?.text,
                            &r.right.as_ref()?.text,
                        );
                        (!old.is_empty() || !new.is_empty()).then_some((i, (old, new)))
                    })
                    .collect();
                (diff, old_spans, new_spans, intra)
            });
            let _ = tx.send_blocking(result);
        });
        cx.spawn(async move |weak, cx| {
            let Ok(result) = rx.recv().await else { return };
            let _ = weak.update(cx, |this, cx| {
                if this.diff_seq != seq {
                    return; // a newer request superseded this one
                }
                match result {
                    Ok((diff, old_spans, new_spans, intra)) => {
                        let same_file = this
                            .diff
                            .as_ref()
                            .is_some_and(|d| d.file.path == file.path && d.commit == commit);
                        let blocks = core_workspace::blocks(&diff.rows);
                        let block_at_row = blocks
                            .iter()
                            .enumerate()
                            .map(|(i, b)| (b.start, i))
                            .collect();
                        let current_change = this
                            .diff
                            .as_ref()
                            .filter(|_| same_file)
                            .map_or(0, |d| d.current_change)
                            .min(blocks.len().saturating_sub(1));
                        let mut state = DiffState {
                            full_path: root.join(&file.path),
                            file,
                            staged,
                            commit,
                            unified: this.git.unified_diff,
                            diff,
                            old_spans,
                            new_spans,
                            expanded,
                            display: Vec::new(),
                            // Keep the scroll position across refreshes of the same file only.
                            scroll: this
                                .diff
                                .as_ref()
                                .filter(|_| same_file)
                                .map(|d| d.scroll.clone())
                                .unwrap_or_else(UniformListScrollHandle::new),
                            blocks,
                            block_display: Vec::new(),
                            block_at_row,
                            intra,
                            current_change,
                        };
                        state.rebuild_display();
                        this.diff = Some(state);
                    }
                    Err(text) => {
                        this.git.notice = Some(crate::git_panel::GitNotice {
                            text,
                            is_error: true,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn toggle_diff_unified(&mut self, cx: &mut Context<Self>) {
        self.git.unified_diff = !self.git.unified_diff;
        if let Some(diff) = self.diff.as_mut() {
            diff.unified = self.git.unified_diff;
            diff.rebuild_display();
        }
        cx.notify();
    }

    pub(crate) fn close_diff(&mut self, cx: &mut Context<Self>) {
        if self.diff.take().is_some() {
            self.diff_seq += 1;
            cx.notify();
        }
    }

    /// After the repository changed: re-read the open diff, following the file between the
    /// Staged and Changes lists, or closing the view once the file has no changes left.
    pub(crate) fn refresh_open_diff(&mut self, cx: &mut Context<Self>) {
        let Some(open) = &self.diff else { return };
        if open.commit.is_some() {
            return;
        }
        let Some(status) = &self.git.status else {
            return self.close_diff(cx);
        };
        let path = open.file.path.clone();
        let find = |list: &[ChangedFile]| list.iter().find(|f| f.path == path).cloned();
        let (same, other) = if open.staged {
            (find(&status.staged), find(&status.unstaged))
        } else {
            (find(&status.unstaged), find(&status.staged))
        };
        let staged = open.staged;
        match (same, other) {
            (Some(file), _) => self.open_git_diff(file, staged, cx),
            (None, Some(file)) => self.open_git_diff(file, !staged, cx),
            (None, None) => self.close_diff(cx),
        }
    }

    /// Scroll to the next / previous change block.
    pub(crate) fn diff_goto_change(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(state) = self.diff.as_mut() else {
            return;
        };
        if state.blocks.is_empty() {
            return;
        }
        let n = state.blocks.len() as isize;
        let next = (state.current_change as isize + delta).rem_euclid(n) as usize;
        self.diff_goto_block(next, cx);
    }

    pub(crate) fn diff_goto_block(&mut self, block: usize, cx: &mut Context<Self>) {
        let Some(state) = self.diff.as_mut() else {
            return;
        };
        let Some(&display_ix) = state.block_display.get(block) else {
            return;
        };
        state.current_change = block;
        // Leave a little context above the change.
        state
            .scroll
            .scroll_to_item(display_ix.saturating_sub(2), ScrollStrategy::Top);
        cx.notify();
    }

    /// Every file shown in the active list (either the expanded commit's files, or Staged + Changes).
    fn diff_file_list(&self) -> Vec<(ChangedFile, bool)> {
        if self.diff.as_ref().is_some_and(|d| d.commit.is_some()) {
            return self
                .git
                .expanded_commit_files
                .iter()
                .map(|f| (f.clone(), false))
                .collect();
        }
        let Some(status) = &self.git.status else {
            return Vec::new();
        };
        status
            .staged
            .iter()
            .map(|f| (f.clone(), true))
            .chain(status.unstaged.iter().map(|f| (f.clone(), false)))
            .collect()
    }

    /// Step to the next / previous changed file without going back to the panel.
    pub(crate) fn diff_goto_file(&mut self, delta: isize, cx: &mut Context<Self>) {
        let list = self.diff_file_list();
        let Some(open) = &self.diff else { return };
        let commit = open.commit.clone();
        let Some(here) = list
            .iter()
            .position(|(f, s)| f.path == open.file.path && *s == open.staged)
        else {
            return;
        };
        let (file, staged) =
            list[(here as isize + delta).rem_euclid(list.len() as isize) as usize].clone();
        if let Some(hash) = commit {
            self.open_git_commit_diff(hash, file, cx);
        } else {
            self.open_git_diff(file, staged, cx);
        }
    }

    /// Mark the open file reviewed (or not) and, when marking, move on to the next unreviewed one.
    pub(crate) fn diff_toggle_reviewed(&mut self, cx: &mut Context<Self>) {
        let Some(open) = &self.diff else { return };
        let key = (open.file.path.clone(), open.staged);
        if !self.git.reviewed.remove(&key) {
            self.git.reviewed.insert(key.clone());
            let list = self.diff_file_list();
            let here = list
                .iter()
                .position(|(f, s)| (f.path.clone(), *s) == key)
                .unwrap_or(0);
            let next = (1..list.len())
                .map(|step| &list[(here + step) % list.len()])
                .find(|(f, s)| !self.git.reviewed.contains(&(f.path.clone(), *s)))
                .cloned();
            if let Some((file, staged)) = next {
                self.open_git_diff(file, staged, cx);
            }
        }
        cx.notify();
    }

    /// Stage / unstage / revert one change block of the open diff.
    pub(crate) fn diff_hunk_action(
        &mut self,
        action: HunkAction,
        block: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = &self.diff else { return };
        let (Some(&block_range), false) = (
            state.blocks.get(block),
            state.file.kind == ChangeKind::Untracked,
        ) else {
            return;
        };
        if action == HunkAction::Revert {
            // Reverting cannot be undone: the first click arms it, the second runs it.
            let key = format!("hunk:{}:{block}", state.file.path);
            if self.git.confirm_discard.as_deref() != Some(key.as_str()) {
                self.arm_confirm(key, cx);
                return;
            }
        }
        let (file, rows) = (state.file.clone(), state.diff.rows.clone());
        let (label, done) = match action {
            HunkAction::Stage => ("Staging hunk\u{2026}", "Staged hunk"),
            HunkAction::Unstage => ("Unstaging hunk\u{2026}", "Unstaged hunk"),
            HunkAction::Revert => ("Reverting hunk\u{2026}", "Reverted hunk"),
        };
        self.git_run(window, Some(label), false, cx, move |root, _| {
            match action {
                HunkAction::Stage => core_workspace::stage_hunk(root, &file, &rows, block_range),
                HunkAction::Unstage => {
                    core_workspace::unstage_hunk(root, &file, &rows, block_range)
                }
                HunkAction::Revert => core_workspace::revert_hunk(root, &file, &rows, block_range),
            }
            .map(|_| done.to_string())
        });
    }

    pub(crate) fn render_diff_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(state) = &self.diff else {
            return div().into_any_element();
        };
        let muted = cx.theme().muted_foreground;
        v_flex()
            .size_full()
            .min_h(px(0.))
            .bg(cx.theme().background)
            .child(self.render_diff_header(state, cx))
            .child(if state.diff.binary {
                diff_message("Binary file not shown", muted)
            } else if state.diff.rows.is_empty() {
                diff_message("No differences", muted)
            } else {
                self.render_diff_columns(state, cx)
            })
            .into_any_element()
    }

    fn render_diff_header(&self, state: &DiffState, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let path = std::path::Path::new(&state.file.path);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&state.file.path)
            .to_string();
        let dir = path
            .parent()
            .and_then(|p| p.to_str())
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        let (label, tone) = if let Some(hash) = &state.commit {
            (format!("Commit {hash}"), cx.theme().magenta)
        } else {
            match (state.file.kind, state.staged) {
                (ChangeKind::Untracked, _) => ("Untracked".to_string(), cx.theme().green),
                (_, true) => ("Staged".to_string(), cx.theme().info),
                (_, false) => ("Working tree".to_string(), cx.theme().warning),
            }
        };
        let is_commit_diff = state.commit.is_some();
        let staged = state.staged;
        let stage_file = state.file.clone();
        let open_path = state.full_path.clone();
        let can_open = state.file.kind != ChangeKind::Deleted;
        let files = self.diff_file_list();
        let file_pos = files
            .iter()
            .position(|(f, s)| f.path == state.file.path && *s == staged);
        let reviewed = self
            .git
            .reviewed
            .contains(&(state.file.path.clone(), staged));
        let change_count = state.blocks.len();
        let unified = state.unified;

        // A little "‹ 3 / 12 ›" stepper.
        let stepper = |id: &'static str,
                       text: String,
                       tip: &'static str,
                       enabled: bool,
                       step: fn(&mut DartIde, isize, &mut Context<DartIde>),
                       cx: &mut Context<Self>| {
            h_flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(1.))
                .child(
                    Button::new((id, 0usize))
                        .ghost()
                        .xsmall()
                        .disabled(!enabled)
                        .icon(
                            Icon::new(IconName::ChevronLeft)
                                .with_size(px(12.))
                                .text_color(muted),
                        )
                        .tooltip(format!("Previous {tip}"))
                        .on_click(cx.listener(move |this, _e, _w, cx| step(this, -1, cx))),
                )
                .child(
                    div()
                        .min_w(px(34.))
                        .text_center()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_size(px(tokens::UI_XS))
                        .text_color(muted)
                        .child(text),
                )
                .child(
                    Button::new((id, 1usize))
                        .ghost()
                        .xsmall()
                        .disabled(!enabled)
                        .icon(
                            Icon::new(IconName::ChevronRight)
                                .with_size(px(12.))
                                .text_color(muted),
                        )
                        .tooltip(format!("Next {tip}"))
                        .on_click(cx.listener(move |this, _e, _w, cx| step(this, 1, cx))),
                )
        };

        h_flex()
            .w_full()
            .h(px(tokens::TABSTRIP_H))
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .gap(px(tokens::SPACE_4))
            .px(px(tokens::SPACE_5))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .min_w(px(0.))
                    .items_center()
                    .gap(px(tokens::SPACE_3))
                    .child(crate::rails::files::render_file_icon(&name))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(tokens::UI))
                            .font_medium()
                            .text_color(cx.theme().foreground)
                            .child(name),
                    )
                    .when_some(dir, |el, dir| {
                        el.child(
                            div()
                                .truncate()
                                .text_size(px(tokens::UI_SM))
                                .text_color(muted)
                                .child(dir),
                        )
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .px(px(6.))
                            .rounded(px(tokens::RADIUS_PILL))
                            .bg(tone.opacity(0.15))
                            .text_size(px(tokens::UI_XS))
                            .text_color(tone)
                            .child(label),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap(px(tokens::SPACE_3))
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(tokens::UI_SM))
                            .child(
                                div()
                                    .text_color(cx.theme().green)
                                    .child(format!("+{}", state.diff.added)),
                            )
                            .child(
                                div()
                                    .text_color(cx.theme().red)
                                    .child(format!("\u{2212}{}", state.diff.removed)),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(tokens::SPACE_3))
                    .child(stepper(
                        "diff-change-nav",
                        if change_count == 0 {
                            "\u{2013}".to_string()
                        } else {
                            format!("{}/{}", state.current_change + 1, change_count)
                        },
                        "change",
                        change_count > 1,
                        DartIde::diff_goto_change,
                        cx,
                    ))
                    .child(div().w(px(1.)).h(px(14.)).bg(cx.theme().border))
                    .child(stepper(
                        "diff-file-nav",
                        file_pos.map_or("\u{2013}".to_string(), |i| {
                            format!("{}/{}", i + 1, files.len())
                        }),
                        "file",
                        files.len() > 1,
                        DartIde::diff_goto_file,
                        cx,
                    ))
                    .child(
                        Button::new("diff-toggle-unified")
                            .ghost()
                            .small()
                            .icon(
                                Icon::new(IconName::Columns2)
                                    .with_size(px(12.))
                                    .text_color(if !unified {
                                        cx.theme().primary
                                    } else {
                                        muted
                                    }),
                            )
                            .label(if unified { "Unified" } else { "Split" })
                            .tooltip("Toggle between Split (side-by-side) and Unified diff views")
                            .on_click(
                                cx.listener(|this, _e, _w, cx| this.toggle_diff_unified(cx)),
                            ),
                    )
                    .when(!is_commit_diff, |el| {
                        el.child(
                            Button::new("diff-reviewed")
                                .ghost()
                                .small()
                                .icon(
                                    Icon::new(IconName::Check)
                                        .with_size(px(13.))
                                        .text_color(if reviewed {
                                            cx.theme().green
                                        } else {
                                            muted
                                        }),
                                )
                                .label(if reviewed {
                                    "Reviewed"
                                } else {
                                    "Mark reviewed"
                                })
                                .tooltip("Mark this file reviewed and move to the next one")
                                .on_click(cx.listener(|this, _e, _w, cx| {
                                    this.diff_toggle_reviewed(cx)
                                })),
                        )
                    })
                    .when(can_open, |el| {
                        el.child(
                            Button::new("diff-open-file")
                                .ghost()
                                .small()
                                .label("Open file")
                                .on_click(cx.listener(move |this, _event, window, cx| {
                                    this.open_path(open_path.clone(), window, cx);
                                })),
                        )
                    })
                    .when(!is_commit_diff, |el| {
                        el.child(
                            Button::new("diff-stage")
                                .small()
                                .label(if staged { "Unstage" } else { "Stage" })
                                .on_click(cx.listener(move |this, _event, window, cx| {
                                    if staged {
                                        this.git_unstage(
                                            std::slice::from_ref(&stage_file),
                                            window,
                                            cx,
                                        );
                                    } else {
                                        this.git_stage(
                                            std::slice::from_ref(&stage_file),
                                            window,
                                            cx,
                                        );
                                    }
                                })),
                        )
                    })
                    .child(
                        Button::new("diff-close")
                            .ghost()
                            .small()
                            .icon(Icon::new(IconName::X).xsmall().text_color(muted))
                            .tooltip("Close diff")
                            .on_click(cx.listener(|this, _event, _window, cx| this.close_diff(cx))),
                    ),
            )
    }

    fn render_diff_columns(&self, state: &DiffState, cx: &mut Context<Self>) -> Div {
        let scroll = state.scroll.clone();
        let total = state.display.len().max(1) as f32;
        div()
            .relative()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .child(
                uniform_list(
                    "diff-rows",
                    state.display.len(),
                    cx.processor(|this, range: Range<usize>, window, cx| {
                        let Some(state) = &this.diff else {
                            return Vec::new();
                        };
                        let armed = this.git.confirm_discard.clone();
                        range
                            .map(|ix| render_diff_item(state, ix, armed.as_deref(), window, cx))
                            .collect()
                    }),
                )
                .track_scroll(&scroll)
                .size_full(),
            )
            // The change map: one mark per change block, positioned along the whole file.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .w(px(10.))
                    .bg(cx.theme().background.opacity(0.6))
                    .border_l_1()
                    .border_color(cx.theme().border)
                    .children(state.blocks.iter().enumerate().map(|(i, block)| {
                        let row = &state.diff.rows[block.start];
                        let color = match row.kind {
                            RowKind::Added => cx.theme().green,
                            RowKind::Removed => cx.theme().red,
                            _ => cx.theme().warning,
                        };
                        let current = i == state.current_change;
                        let at = state.block_display.get(i).copied().unwrap_or(0) as f32 / total;
                        div()
                            .id(("diff-map-mark", i))
                            .absolute()
                            .left(px(1.))
                            .right(px(1.))
                            .top(relative(at))
                            .h(px(if current { 7. } else { 5. }))
                            .rounded(px(1.))
                            .bg(color.opacity(if current { 1.0 } else { 0.75 }))
                            .cursor_pointer()
                            .on_click(
                                cx.listener(move |this, _e, _w, cx| this.diff_goto_block(i, cx)),
                            )
                    })),
            )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HunkAction {
    Stage,
    Unstage,
    Revert,
}

fn diff_message(text: &'static str, color: Hsla) -> Div {
    div()
        .flex_1()
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(tokens::UI))
        .text_color(color)
        .child(text)
}

fn render_diff_item(
    state: &DiffState,
    ix: usize,
    armed: Option<&str>,
    _window: &mut Window,
    cx: &mut Context<DartIde>,
) -> AnyElement {
    match state.display[ix] {
        DisplayItem::Gap { start, end } => {
            let muted = cx.theme().muted_foreground;
            let hidden = end - start;
            h_flex()
                .id(("diff-gap", ix))
                .w_full()
                .h(px(ROW_H))
                .items_center()
                .justify_center()
                .gap(px(tokens::SPACE_3))
                .bg(cx.theme().list_hover.opacity(0.5))
                .border_y_1()
                .border_color(cx.theme().border)
                .cursor_pointer()
                .hover(|s| s.bg(cx.theme().list_hover))
                .text_size(px(tokens::UI_XS))
                .text_color(muted)
                .child(Icon::new(IconName::ChevronsUpDown).xsmall())
                .child(format!(
                    "{hidden} unchanged line{}",
                    if hidden == 1 { "" } else { "s" }
                ))
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    if let Some(diff) = this.diff.as_mut() {
                        diff.expanded.insert(start);
                        diff.rebuild_display();
                        cx.notify();
                    }
                }))
                .into_any_element()
        }
        DisplayItem::UnifiedLeft(row_ix) => {
            let row = &state.diff.rows[row_ix];
            let spans = row
                .left
                .as_ref()
                .and_then(|c| state.old_spans.get(c.line as usize - 1))
                .cloned()
                .unwrap_or_default();
            let (old_marks, _) = state.intra.get(&row_ix).cloned().unwrap_or_default();
            let chips = state
                .block_at_row
                .get(&row_ix)
                .copied()
                .filter(|_| state.commit.is_none() && state.file.kind != ChangeKind::Untracked)
                .map(|block| hunk_chips(state, block, armed, cx));
            diff_unified_row(
                row.left.as_ref().map(|c| c.line),
                None,
                row.left.as_ref().map_or("", |c| c.text.as_str()),
                spans,
                old_marks,
                Some(Side::Removed),
                chips,
                cx,
            )
            .into_any_element()
        }
        DisplayItem::UnifiedRight(row_ix) => {
            let row = &state.diff.rows[row_ix];
            let spans = row
                .right
                .as_ref()
                .and_then(|c| state.new_spans.get(c.line as usize - 1))
                .cloned()
                .unwrap_or_default();
            let (_, new_marks) = state.intra.get(&row_ix).cloned().unwrap_or_default();
            diff_unified_row(
                None,
                row.right.as_ref().map(|c| c.line),
                row.right.as_ref().map_or("", |c| c.text.as_str()),
                spans,
                new_marks,
                Some(Side::Added),
                None,
                cx,
            )
            .into_any_element()
        }
        DisplayItem::Row(row_ix) => {
            let row = &state.diff.rows[row_ix];
            let spans_of = |spans: &[Spans], cell: &Option<DiffCell>| -> Spans {
                cell.as_ref()
                    .and_then(|c| spans.get(c.line as usize - 1))
                    .cloned()
                    .unwrap_or_default()
            };
            let chips = state
                .block_at_row
                .get(&row_ix)
                .copied()
                .filter(|_| state.commit.is_none() && state.file.kind != ChangeKind::Untracked)
                .map(|block| hunk_chips(state, block, armed, cx));
            if state.unified {
                let (tint, spans, text) = match row.kind {
                    RowKind::Removed => (
                        Some(Side::Removed),
                        spans_of(&state.old_spans, &row.left),
                        row.left.as_ref().map_or("", |c| c.text.as_str()),
                    ),
                    RowKind::Added => (
                        Some(Side::Added),
                        spans_of(&state.new_spans, &row.right),
                        row.right.as_ref().map_or("", |c| c.text.as_str()),
                    ),
                    _ => (
                        None,
                        spans_of(&state.new_spans, &row.right),
                        row.right.as_ref().map_or("", |c| c.text.as_str()),
                    ),
                };
                return diff_unified_row(
                    row.left.as_ref().map(|c| c.line),
                    row.right.as_ref().map(|c| c.line),
                    text,
                    spans,
                    Vec::new(),
                    tint,
                    chips,
                    cx,
                )
                .into_any_element();
            }
            let removed_side = matches!(row.kind, RowKind::Removed | RowKind::Changed);
            let added_side = matches!(row.kind, RowKind::Added | RowKind::Changed);
            let (old_marks, new_marks) = state.intra.get(&row_ix).cloned().unwrap_or_default();
            h_flex()
                .w_full()
                .h(px(ROW_H))
                .child(diff_half(
                    &row.left,
                    spans_of(&state.old_spans, &row.left),
                    old_marks,
                    removed_side.then_some(Side::Removed),
                    cx,
                ))
                .child(div().w(px(1.)).h_full().bg(cx.theme().border))
                .child(
                    diff_half(
                        &row.right,
                        spans_of(&state.new_spans, &row.right),
                        new_marks,
                        added_side.then_some(Side::Added),
                        cx,
                    )
                    .when_some(chips, |el, chips| el.child(chips)),
                )
                .into_any_element()
        }
    }
}

/// "Stage hunk" / "Revert" (or "Unstage hunk") chips at the right edge of a block's first row.
/// Revert asks twice: the first click arms it (`armed` names the armed action), the second runs it.
fn hunk_chips(
    state: &DiffState,
    block: usize,
    armed: Option<&str>,
    cx: &mut Context<DartIde>,
) -> Div {
    let revert_armed = armed == Some(format!("hunk:{}:{block}", state.file.path).as_str());
    let chip = |id: &'static str,
                label: &'static str,
                danger: bool,
                action: HunkAction,
                cx: &mut Context<DartIde>| {
        let theme = cx.theme();
        let (red, fg, border, bg, muted, font) = (
            theme.red,
            theme.foreground,
            theme.border,
            theme.background,
            theme.muted_foreground,
            theme.font_family.clone(),
        );
        div()
            .id((id, block))
            .px(px(7.))
            .h(px(18.))
            .flex()
            .items_center()
            .rounded(px(4.))
            .border_1()
            .border_color(if danger { red.opacity(0.5) } else { border })
            .bg(if danger { red.opacity(0.18) } else { bg })
            .cursor_pointer()
            .text_size(px(tokens::UI_XS))
            .font_family(font)
            .text_color(if danger { red } else { muted })
            .hover(move |s| s.text_color(fg))
            .child(label)
            .on_click(cx.listener(move |this, _e, window, cx| {
                this.diff_hunk_action(action, block, window, cx)
            }))
    };
    h_flex()
        .absolute()
        .top(px(2.))
        .right(px(16.))
        .gap(px(4.))
        .when(state.staged, |el| {
            el.child(chip(
                "hunk-unstage",
                "Unstage hunk",
                false,
                HunkAction::Unstage,
                cx,
            ))
        })
        .when(!state.staged, |el| {
            el.child(chip(
                "hunk-stage",
                "Stage hunk",
                false,
                HunkAction::Stage,
                cx,
            ))
            .child(chip(
                "hunk-revert",
                if revert_armed { "Revert?" } else { "Revert" },
                revert_armed,
                HunkAction::Revert,
                cx,
            ))
        })
}

#[derive(Clone, Copy)]
enum Side {
    Removed,
    Added,
}

/// Combine syntax colors and "changed word" backgrounds into non-overlapping styled ranges.
fn merge_styles(
    len: usize,
    spans: &[(Range<usize>, HighlightKind)],
    marks: &[Range<usize>],
    mark_bg: Hsla,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut cuts: BTreeSet<usize> = BTreeSet::from([0, len]);
    for r in spans.iter().map(|(r, _)| r).chain(marks) {
        cuts.insert(r.start.min(len));
        cuts.insert(r.end.min(len));
    }
    let cuts: Vec<usize> = cuts.into_iter().collect();
    cuts.windows(2)
        .filter_map(|w| {
            let (a, b) = (w[0], w[1]);
            let color = spans
                .iter()
                .find(|(r, _)| r.start <= a && b <= r.end)
                .map(|(_, k)| editor_view::highlight_color(*k).into());
            let background_color = marks
                .iter()
                .any(|m| m.start <= a && b <= m.end)
                .then_some(mark_bg);
            (color.is_some() || background_color.is_some()).then(|| {
                (
                    a..b,
                    HighlightStyle {
                        color,
                        background_color,
                        ..Default::default()
                    },
                )
            })
        })
        .collect()
}

/// One half of a row: line-number gutter plus the (syntax-colored) text.
fn diff_half(
    cell: &Option<DiffCell>,
    spans: Spans,
    marks: Vec<Range<usize>>,
    tint: Option<Side>,
    cx: &mut Context<DartIde>,
) -> Div {
    let theme = cx.theme();
    let (bg, bar) = match tint {
        Some(Side::Removed) => (Some(rgba(tokens::LINE_DEL).into()), Some(theme.red)),
        Some(Side::Added) => (Some(rgba(tokens::LINE_ADD).into()), Some(theme.green)),
        // Nothing on this side of the row: a quiet, slightly darker field.
        None if cell.is_none() => (Some(rgba(0xffffff07).into()), None),
        None => (None, None),
    };
    let half = div()
        .relative()
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .overflow_hidden()
        .flex()
        .items_center()
        .when_some(bg, |el: Div, bg: Hsla| el.bg(bg))
        .when_some(bar, |el, bar| {
            el.child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(3.))
                    .bg(bar),
            )
        });
    let Some(cell) = cell else { return half };
    let gutter_color = if bar.is_some() {
        theme.foreground.opacity(0.8)
    } else {
        theme.muted_foreground.opacity(0.7)
    };
    let plain = spans.is_empty() && marks.is_empty();
    let text: SharedString = if plain {
        cell.text.replace('\t', "    ")
    } else {
        cell.text.clone()
    }
    .into();
    let mark_bg = match tint {
        Some(Side::Removed) => theme.red.opacity(0.35),
        _ => theme.green.opacity(0.35),
    };
    let styles = merge_styles(text.len(), &spans, &marks, mark_bg);
    let default = theme.foreground;
    half.font_family(theme.mono_font_family.clone())
        .text_size(px(tokens::MONO))
        .child(
            div()
                .flex_shrink_0()
                .w(px(GUTTER_W))
                .pr(px(tokens::SPACE_5))
                .text_right()
                .text_color(gutter_color)
                .child(cell.line.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .whitespace_nowrap()
                .text_color(default)
                .child(StyledText::new(text).with_highlights(styles)),
        )
}

/// One full-width unified diff row: old line column, new line column, `+/-` marker, and styled text.
#[allow(clippy::too_many_arguments)]
fn diff_unified_row(
    old_line: Option<u32>,
    new_line: Option<u32>,
    raw_text: &str,
    spans: Spans,
    marks: Vec<Range<usize>>,
    tint: Option<Side>,
    chips: Option<Div>,
    cx: &mut Context<DartIde>,
) -> Div {
    let theme = cx.theme();
    let (bg, bar, sign, sign_color) = match tint {
        Some(Side::Removed) => (
            Some(rgba(tokens::LINE_DEL).into()),
            Some(theme.red),
            "\u{2212}",
            theme.red,
        ),
        Some(Side::Added) => (
            Some(rgba(tokens::LINE_ADD).into()),
            Some(theme.green),
            "+",
            theme.green,
        ),
        None => (None, None, " ", theme.muted_foreground),
    };
    let gutter_color = if bar.is_some() {
        theme.foreground.opacity(0.8)
    } else {
        theme.muted_foreground.opacity(0.7)
    };
    let plain = spans.is_empty() && marks.is_empty();
    let text: SharedString = if plain {
        raw_text.replace('\t', "    ")
    } else {
        raw_text.to_string()
    }
    .into();
    let mark_bg = match tint {
        Some(Side::Removed) => theme.red.opacity(0.35),
        _ => theme.green.opacity(0.35),
    };
    let styles = merge_styles(text.len(), &spans, &marks, mark_bg);
    h_flex()
        .relative()
        .w_full()
        .h(px(ROW_H))
        .overflow_hidden()
        .items_center()
        .font_family(theme.mono_font_family.clone())
        .text_size(px(tokens::MONO))
        .when_some(bg, |el: Div, bg: Hsla| el.bg(bg))
        .when_some(bar, |el, bar| {
            el.child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(3.))
                    .bg(bar),
            )
        })
        .child(
            div()
                .flex_shrink_0()
                .w(px(40.))
                .pr(px(tokens::SPACE_2))
                .text_right()
                .text_color(gutter_color)
                .child(old_line.map_or(String::new(), |n| n.to_string())),
        )
        .child(
            div()
                .flex_shrink_0()
                .w(px(40.))
                .pr(px(tokens::SPACE_3))
                .text_right()
                .text_color(gutter_color)
                .child(new_line.map_or(String::new(), |n| n.to_string())),
        )
        .child(
            div()
                .flex_shrink_0()
                .w(px(16.))
                .text_center()
                .text_color(sign_color)
                .child(sign),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .whitespace_nowrap()
                .text_color(theme.foreground)
                .child(StyledText::new(text).with_highlights(styles)),
        )
        .when_some(chips, |el, chips| el.child(chips))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_workspace::DiffRow;

    fn row(kind: RowKind, n: u32) -> DiffRow {
        let cell = || {
            Some(DiffCell {
                line: n,
                text: format!("line {n}"),
            })
        };
        match kind {
            RowKind::Context => DiffRow {
                left: cell(),
                right: cell(),
                kind,
            },
            RowKind::Added => DiffRow {
                left: None,
                right: cell(),
                kind,
            },
            _ => DiffRow {
                left: cell(),
                right: None,
                kind,
            },
        }
    }

    fn state(rows: Vec<DiffRow>) -> DiffState {
        DiffState {
            file: ChangedFile {
                path: "a.dart".into(),
                orig_path: None,
                kind: ChangeKind::Modified,
            },
            staged: false,
            commit: None,
            unified: false,
            full_path: PathBuf::from("/a.dart"),
            diff: FileDiff {
                rows,
                ..Default::default()
            },
            old_spans: Vec::new(),
            new_spans: Vec::new(),
            expanded: HashSet::new(),
            display: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            blocks: Vec::new(),
            block_display: Vec::new(),
            block_at_row: HashMap::new(),
            intra: HashMap::new(),
            current_change: 0,
        }
    }

    fn shape(s: &DiffState) -> Vec<String> {
        s.display
            .iter()
            .map(|d| match d {
                DisplayItem::Row(i) => format!("r{i}"),
                DisplayItem::UnifiedLeft(i) => format!("u-{i}"),
                DisplayItem::UnifiedRight(i) => format!("u+{i}"),
                DisplayItem::Gap { start, end } => format!("gap{start}..{end}"),
            })
            .collect()
    }

    // `gpui_kit::*` brings a `test` attribute of its own into scope; ask for the standard one.
    #[core::prelude::v1::test]
    fn long_unchanged_runs_fold_with_three_lines_of_context() {
        // 20 unchanged rows, one change, 20 unchanged rows.
        let mut rows: Vec<DiffRow> = (1..=20).map(|n| row(RowKind::Context, n)).collect();
        rows.push(row(RowKind::Added, 21));
        rows.extend((22..=41).map(|n| row(RowKind::Context, n)));
        let mut s = state(rows);
        s.rebuild_display();
        assert_eq!(
            shape(&s),
            [
                "gap0..17",
                "r17",
                "r18",
                "r19",
                "r20",
                "r21",
                "r22",
                "r23",
                "gap24..41"
            ]
        );
        // Unfolding the first gap shows all of its rows.
        s.expanded.insert(0);
        s.rebuild_display();
        assert_eq!(shape(&s)[0], "r0");
        assert_eq!(shape(&s).iter().filter(|x| x.starts_with("gap")).count(), 1);
    }

    // `gpui_kit::*` brings a `test` attribute of its own into scope; ask for the standard one.
    #[core::prelude::v1::test]
    fn nearby_changes_share_context_and_a_clean_file_has_no_rows() {
        let mut rows = vec![row(RowKind::Removed, 1)];
        rows.extend((2..=5).map(|n| row(RowKind::Context, n)));
        rows.push(row(RowKind::Added, 6));
        let mut s = state(rows);
        s.rebuild_display();
        assert!(s.display.iter().all(|d| matches!(d, DisplayItem::Row(_))));
        let mut empty = state(Vec::new());
        empty.rebuild_display();
        assert!(empty.display.is_empty());
    }

    // `gpui_kit::*` brings a `test` attribute of its own into scope; ask for the standard one.
    #[core::prelude::v1::test]
    fn syntax_spans_are_split_per_line_and_never_overlap() {
        let text = "/* a\nb */\nclass A {}\n";
        let spans = line_spans(text);
        assert_eq!(spans.len(), 4); // three lines plus the empty one after the last newline
        // The block comment covers the first two lines, each clipped to its own line.
        assert!(
            spans[0]
                .iter()
                .any(|(r, k)| *k == HighlightKind::Comment && r.start == 0 && r.end == 4)
        );
        assert!(
            spans[1]
                .iter()
                .any(|(r, k)| *k == HighlightKind::Comment && r.start == 0 && r.end == 4)
        );
        for line in &spans {
            let mut last = 0;
            for (range, _) in line {
                assert!(range.start >= last, "overlapping spans: {line:?}");
                last = range.end;
            }
        }
    }

    #[core::prelude::v1::test]
    fn word_marks_and_syntax_colors_merge_into_disjoint_ranges() {
        let spans = vec![
            (0..3, HighlightKind::Keyword),
            (4..9, HighlightKind::String),
        ];
        let marks = vec![2..6];
        let merged = merge_styles(12, &spans, &marks, rgba(0xff0000ff).into());
        let ranges: Vec<_> = merged.iter().map(|(r, _)| r.clone()).collect();
        assert_eq!(ranges, vec![0..2, 2..3, 3..4, 4..6, 6..9]);
        let has = |i: usize| {
            (
                merged[i].1.color.is_some(),
                merged[i].1.background_color.is_some(),
            )
        };
        assert_eq!(has(0), (true, false)); // keyword only
        assert_eq!(has(1), (true, true)); // keyword and changed word
        assert_eq!(has(2), (false, true)); // changed word only
        assert_eq!(has(3), (true, true)); // string and changed word
        assert_eq!(has(4), (true, false)); // string only
        // Disjoint and in order, as StyledText requires.
        assert!(ranges.windows(2).all(|w| w[0].end <= w[1].start));
        assert!(merge_styles(5, &[], &[], rgba(0).into()).is_empty());
    }

    #[core::prelude::v1::test]
    fn blocks_drive_navigation_positions_in_the_display_list() {
        let mut rows: Vec<DiffRow> = (1..=30).map(|n| row(RowKind::Context, n)).collect();
        rows[5] = row(RowKind::Added, 6);
        rows[25] = row(RowKind::Removed, 26);
        let mut s = state(rows);
        s.blocks = core_workspace::blocks(&s.diff.rows);
        s.rebuild_display();
        assert_eq!(s.blocks.len(), 2);
        // Each block's first row is on screen (never folded away), and in order.
        assert_eq!(s.block_display.len(), 2);
        assert!(matches!(s.display[s.block_display[0]], DisplayItem::Row(5)));
        assert!(matches!(
            s.display[s.block_display[1]],
            DisplayItem::Row(25)
        ));
        assert!(s.block_display[0] < s.block_display[1]);
    }

    #[core::prelude::v1::test]
    fn unified_mode_splits_changed_rows_into_left_and_right_items() {
        let rows = vec![
            row(RowKind::Context, 1),
            DiffRow {
                left: Some(DiffCell {
                    line: 2,
                    text: "old".into(),
                }),
                right: Some(DiffCell {
                    line: 2,
                    text: "new".into(),
                }),
                kind: RowKind::Changed,
            },
            row(RowKind::Context, 3),
        ];
        let mut s = state(rows);
        s.blocks = core_workspace::blocks(&s.diff.rows);
        s.unified = true;
        s.rebuild_display();
        assert_eq!(shape(&s), ["r0", "u-1", "u+1", "r2"]);
        assert_eq!(s.block_display, [1]);
    }
}
