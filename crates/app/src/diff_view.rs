//! The side-by-side diff view shown in the editor area when a file is clicked in the Git panel:
//! old version on the left, new on the right, changed lines tinted, the missing side of an
//! addition / deletion left empty, and long unchanged stretches folded into a clickable gap.
//!
//! The diff itself comes from `core_workspace::file_diff`; syntax colors (Dart only, like the
//! editor) come from `core_syntax`. Both are computed off the UI thread.

use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;

use core_syntax::{HighlightKind, SyntaxTree};
use core_workspace::{ChangeKind, ChangedFile, DiffCell, FileDiff, RowKind};
use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
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
    /// A folded run of unchanged rows `start..end`.
    Gap {
        start: usize,
        end: usize,
    },
}

pub(crate) struct DiffState {
    pub(crate) file: ChangedFile,
    pub(crate) staged: bool,
    pub(crate) full_path: PathBuf,
    diff: FileDiff,
    /// Syntax spans per line (0-based) of the old / new text.
    old_spans: Vec<Spans>,
    new_spans: Vec<Spans>,
    /// Gaps (by their first row) the developer has unfolded.
    expanded: HashSet<usize>,
    display: Vec<DisplayItem>,
    scroll: UniformListScrollHandle,
}

impl DiffState {
    fn rebuild_display(&mut self) {
        let n = self.diff.rows.len();
        let mut near_change = vec![false; n];
        for (i, row) in self.diff.rows.iter().enumerate() {
            if row.is_change() {
                for flag in &mut near_change[i.saturating_sub(CONTEXT)..(i + CONTEXT + 1).min(n)] {
                    *flag = true;
                }
            }
        }
        let mut display = Vec::new();
        let mut i = 0;
        while i < n {
            if near_change[i] {
                display.push(DisplayItem::Row(i));
                i += 1;
                continue;
            }
            let start = i;
            while i < n && !near_change[i] {
                i += 1;
            }
            if self.expanded.contains(&start) {
                display.extend((start..i).map(DisplayItem::Row));
            } else {
                display.push(DisplayItem::Gap { start, end: i });
            }
        }
        self.display = display;
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
        let Some(root) = self.git.status.as_ref().map(|s| s.root.clone()) else {
            return;
        };
        self.diff_seq += 1;
        let seq = self.diff_seq;
        let expanded = self
            .diff
            .as_ref()
            .filter(|d| d.file.path == file.path)
            .map(|d| d.expanded.clone())
            .unwrap_or_default();
        let (tx, rx) = async_channel::bounded(1);
        let job_file = file.clone();
        let job_root = root.clone();
        std::thread::spawn(move || {
            let result = core_workspace::file_diff(&job_root, &job_file, staged).map(|diff| {
                let is_dart = job_file.path.ends_with(".dart");
                let (old_spans, new_spans) = if is_dart && !diff.binary {
                    (line_spans(&diff.old_text()), line_spans(&diff.new_text()))
                } else {
                    (Vec::new(), Vec::new())
                };
                (diff, old_spans, new_spans)
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
                    Ok((diff, old_spans, new_spans)) => {
                        let mut state = DiffState {
                            full_path: root.join(&file.path),
                            file,
                            staged,
                            diff,
                            old_spans,
                            new_spans,
                            expanded,
                            display: Vec::new(),
                            scroll: this
                                .diff
                                .as_ref()
                                .map(|d| d.scroll.clone())
                                .unwrap_or_else(UniformListScrollHandle::new),
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
        let (label, tone) = match (state.file.kind, state.staged) {
            (ChangeKind::Untracked, _) => ("Untracked", cx.theme().green),
            (_, true) => ("Staged", cx.theme().info),
            (_, false) => ("Working tree", cx.theme().warning),
        };
        let staged = state.staged;
        let stage_file = state.file.clone();
        let open_path = state.full_path.clone();
        let can_open = state.file.kind != ChangeKind::Deleted;
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
                    .gap(px(tokens::SPACE_2))
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
                    .child(
                        Button::new("diff-stage")
                            .small()
                            .label(if staged { "Unstage" } else { "Stage" })
                            .on_click(cx.listener(move |this, _event, window, cx| {
                                if staged {
                                    this.git_unstage(std::slice::from_ref(&stage_file), window, cx);
                                } else {
                                    this.git_stage(std::slice::from_ref(&stage_file), window, cx);
                                }
                            })),
                    )
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
        div().flex_1().w_full().min_h(px(0.)).child(
            uniform_list(
                "diff-rows",
                state.display.len(),
                cx.processor(|this, range: Range<usize>, _window, cx| {
                    let Some(state) = &this.diff else {
                        return Vec::new();
                    };
                    range.map(|ix| render_diff_item(state, ix, cx)).collect()
                }),
            )
            .track_scroll(&scroll)
            .size_full(),
        )
    }
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

fn render_diff_item(state: &DiffState, ix: usize, cx: &mut Context<DartIde>) -> AnyElement {
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
        DisplayItem::Row(row_ix) => {
            let row = &state.diff.rows[row_ix];
            let removed_side = matches!(row.kind, RowKind::Removed | RowKind::Changed);
            let added_side = matches!(row.kind, RowKind::Added | RowKind::Changed);
            let spans_of = |spans: &[Spans], cell: &Option<DiffCell>| -> Spans {
                cell.as_ref()
                    .and_then(|c| spans.get(c.line as usize - 1))
                    .cloned()
                    .unwrap_or_default()
            };
            h_flex()
                .w_full()
                .h(px(ROW_H))
                .child(diff_half(
                    &row.left,
                    spans_of(&state.old_spans, &row.left),
                    removed_side.then_some(Side::Removed),
                    cx,
                ))
                .child(div().w(px(1.)).h_full().bg(cx.theme().border))
                .child(diff_half(
                    &row.right,
                    spans_of(&state.new_spans, &row.right),
                    added_side.then_some(Side::Added),
                    cx,
                ))
                .into_any_element()
        }
    }
}

#[derive(Clone, Copy)]
enum Side {
    Removed,
    Added,
}

/// One half of a row: line-number gutter plus the (syntax-colored) text.
fn diff_half(
    cell: &Option<DiffCell>,
    spans: Spans,
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
    let text: SharedString = if spans.is_empty() {
        cell.text.replace('\t', "    ")
    } else {
        cell.text.clone()
    }
    .into();
    let default = theme.foreground;
    let styled = StyledText::new(text).with_highlights(spans.into_iter().map(|(range, kind)| {
        (
            range,
            HighlightStyle {
                color: Some(editor_view::highlight_color(kind).into()),
                ..Default::default()
            },
        )
    }));
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
                .child(styled),
        )
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
        }
    }

    fn shape(s: &DiffState) -> Vec<String> {
        s.display
            .iter()
            .map(|d| match d {
                DisplayItem::Row(i) => format!("r{i}"),
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
}
