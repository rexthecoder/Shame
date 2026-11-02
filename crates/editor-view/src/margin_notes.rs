//! Margin notes: errors and warnings shown in a column to the right of the
//! code, each tied to its line by a soft leader curve.
//!
//! The code rows never change height or order for a diagnostic. The column
//! takes its width out of the text area, and each note is placed at its
//! line's height, then pushed down just enough not to overlap the note above
//! (see [`stack_notes`]). When the editor is too narrow for a column, each
//! line gets a small marker at its right edge instead, with the message in a
//! tooltip.

use std::collections::HashMap;

use core_lsp::lsp_types::DiagnosticSeverity;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::*;

use crate::{EditorView, LINE_HEIGHT};

/// The note column, including the gap the leader curve runs through.
pub(crate) const NOTES_WIDTH: f32 = 268.;
/// Room the curve needs between the code's edge and a note.
const CURVE_GAP: f32 = 30.;
const RIGHT_PAD: f32 = 10.;
/// Below this full editor width the code keeps the room and notes collapse
/// to markers.
const MIN_EDITOR_WIDTH: f32 = 640.;
const NOTE_GAP: f32 = 6.;
const TITLE_LINE_H: f32 = 16.;
/// Padding, the meta row and the gap above it.
const NOTE_CHROME_H: f32 = 34.;
/// Rough width of one character of the note's UI font, for line wrapping.
const NOTE_CHAR_W: f32 = 6.4;
const MAX_TITLE_LINES: usize = 3;

/// The most severe diagnostic on one line, as a note shows it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LineNote {
    pub line: usize,
    pub severity: DiagnosticSeverity,
    /// Byte column the diagnostic starts at.
    pub column: usize,
    pub message: String,
    pub source: Option<String>,
    /// Other diagnostics on the same line, folded into this note.
    pub more: usize,
}

/// Lines the title wraps to in a card `card_width` wide, capped.
fn title_lines(message: &str, card_width: f32) -> usize {
    let per_line = ((card_width - 34.) / NOTE_CHAR_W).max(8.) as usize;
    message.chars().count().div_ceil(per_line).clamp(1, MAX_TITLE_LINES)
}

fn note_height(lines: usize) -> f32 {
    NOTE_CHROME_H + lines as f32 * TITLE_LINE_H
}

/// Top of each note, given where each would like to sit (sorted by line) and
/// its height: as close as possible to its line, never overlapping the note
/// above it.
pub(crate) fn stack_notes(wanted: &[(f32, f32)], gap: f32) -> Vec<f32> {
    let mut tops = Vec::with_capacity(wanted.len());
    let mut floor = f32::MIN;
    for &(top, height) in wanted {
        let top = top.max(floor);
        tops.push(top);
        floor = top + height + gap;
    }
    tops
}

impl EditorView {
    /// Errors and warnings, one per line (the most severe), sorted by line.
    pub(crate) fn line_notes(&self) -> Vec<LineNote> {
        let Some(lsp) = &self.lsp else {
            return Vec::new();
        };
        let mut by_line: HashMap<usize, LineNote> = HashMap::new();
        for diagnostic in &lsp.diagnostics {
            let severity = diagnostic.severity.unwrap_or(DiagnosticSeverity::INFORMATION);
            if severity != DiagnosticSeverity::ERROR && severity != DiagnosticSeverity::WARNING {
                continue;
            }
            let message = diagnostic.message.lines().next().unwrap_or("").trim().to_string();
            if message.is_empty() {
                continue;
            }
            let line = diagnostic.range.start.line as usize;
            let column = core_buffer::utf16_to_byte_col(
                &self.buffer.line(line),
                diagnostic.range.start.character,
            );
            let note = LineNote {
                line,
                severity,
                column,
                message,
                source: diagnostic.source.clone(),
                more: 0,
            };
            by_line
                .entry(line)
                .and_modify(|existing| {
                    existing.more += 1;
                    // ERROR < WARNING, so the smaller one is more severe.
                    if severity < existing.severity {
                        let more = existing.more;
                        *existing = LineNote { more, ..note.clone() };
                    }
                })
                .or_insert(note);
        }
        let mut notes: Vec<LineNote> = by_line.into_values().collect();
        notes.sort_by_key(|note| note.line);
        notes
    }

    /// The column's width this frame: all of it when there is something to
    /// show and room for it, none otherwise.
    pub(crate) fn margin_notes_width(&mut self, has_notes: bool) -> f32 {
        let full = self.text_area_width.get().to_f64() as f32 + self.notes_reserved;
        // Before the first paint the width is unknown; assume room.
        let fits = full <= 0. || full >= MIN_EDITOR_WIDTH;
        self.notes_reserved = if has_notes && fits { NOTES_WIDTH } else { 0. };
        self.notes_reserved
    }

    /// Jump the caret to where a note's diagnostic starts.
    fn go_to_note(&mut self, line: usize, column: usize, window: &mut Window, cx: &mut Context<Self>) {
        let line_start = self.buffer.line_to_byte(line);
        let line_len = self.buffer.line(line).trim_end_matches(['\n', '\r']).len();
        self.set_cursor(line_start + column.min(line_len), cx);
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Where a line's row is centred, in editor pixels, or `None` when the
    /// line is folded away.
    fn note_row_center(&self, line: usize, top_pad: f32, scroll_y: f32) -> Option<f32> {
        let row = *self.display_line_index.get(line)?;
        let shown =
            self.folded_starts.is_empty() || self.visible_lines.binary_search(&line).is_ok();
        shown.then(|| top_pad + row as f32 * LINE_HEIGHT + scroll_y + LINE_HEIGHT / 2.)
    }

    /// The note column (or, when narrow, the edge markers), laid over the
    /// editor's right side. `top_pad` is the list's top padding.
    pub(crate) fn render_margin_notes(
        &self,
        notes: Vec<LineNote>,
        width: f32,
        top_pad: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if notes.is_empty() {
            return None;
        }
        let scroll_y = self.scroll_handle.0.borrow().base_handle.offset().y.to_f64() as f32;
        let viewport_h = window.viewport_size().height.to_f64() as f32;
        let on_screen = |top: f32, bottom: f32| bottom > -40. && top < viewport_h + 40.;
        let color_of = |severity: DiagnosticSeverity, cx: &App| {
            if severity == DiagnosticSeverity::ERROR {
                cx.theme().danger
            } else {
                cx.theme().warning
            }
        };

        if width <= 0. {
            return Some(self.render_note_markers(&notes, top_pad, scroll_y, viewport_h, cx));
        }

        let cursor_line = self
            .buffer
            .byte_to_line(self.buffer.selections().primary().head);
        let card_w = width - CURVE_GAP - RIGHT_PAD;
        let placed: Vec<(LineNote, f32, f32, usize)> = notes
            .into_iter()
            .filter_map(|note| {
                let center = self.note_row_center(note.line, top_pad, scroll_y)?;
                let lines = title_lines(&note.message, card_w);
                Some((note, center, note_height(lines), lines))
            })
            .collect();
        // Each note's first line sits level with its code line.
        let title_center = 8. + TITLE_LINE_H / 2.;
        let wanted: Vec<(f32, f32)> = placed
            .iter()
            .map(|(_, center, height, _)| (center - title_center, *height))
            .collect();
        let tops = stack_notes(&wanted, NOTE_GAP);

        let mut curves: Vec<(f32, f32, Hsla)> = Vec::new();
        let mut cards = Vec::new();
        let surface = cx.theme().popover;
        let border = cx.theme().border;
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let chip = cx.theme().secondary;
        let chip_hover = cx.theme().secondary_hover;
        for ((note, center, height, lines), top) in placed.into_iter().zip(tops) {
            if !on_screen(top.min(center), (top + height).max(center)) {
                continue;
            }
            let color = color_of(note.severity, cx);
            curves.push((center, top + title_center, color));
            let active = note.line == cursor_line;
            let (line, column) = (note.line, note.column);
            let meta = match (&note.source, note.more) {
                (Some(source), 0) => format!("{source} \u{b7} line {}", line + 1),
                (Some(source), more) => format!("{source} \u{b7} line {} \u{b7} +{more}", line + 1),
                (None, 0) => format!("Line {}", line + 1),
                (None, more) => format!("Line {} \u{b7} +{more}", line + 1),
            };
            cards.push(
                v_flex()
                    .id(("margin-note", line))
                    .debug_selector(move || format!("margin-note-{line}"))
                    .absolute()
                    .top(px(top))
                    .left(px(CURVE_GAP))
                    .w(px(card_w))
                    .h(px(height))
                    .px(px(10.))
                    .py(px(8.))
                    .gap(px(4.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(if active { color.opacity(0.55) } else { border })
                    .bg(surface)
                    .shadow_md()
                    .cursor_pointer()
                    .hover(move |card| card.border_color(color.opacity(0.4)))
                    .child(
                        h_flex()
                            .items_start()
                            .gap(px(7.))
                            .child(
                                div()
                                    .mt(px(4.5))
                                    .size(px(7.))
                                    .flex_shrink_0()
                                    .rounded(px(3.5))
                                    .bg(color),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .h(px(lines as f32 * TITLE_LINE_H))
                                    .overflow_hidden()
                                    .text_size(px(12.))
                                    .line_height(px(TITLE_LINE_H))
                                    .text_color(fg)
                                    .child(note.message),
                            ),
                    )
                    .child(
                        h_flex()
                            .pl(px(14.))
                            .h(px(18.))
                            .items_center()
                            .justify_between()
                            .gap(px(6.))
                            .text_size(px(11.))
                            .child(div().min_w(px(0.)).truncate().text_color(muted).child(meta))
                            .child(
                                div()
                                    .id(("margin-note-fix", line))
                                    .flex_shrink_0()
                                    .px(px(7.))
                                    .h(px(18.))
                                    .flex()
                                    .items_center()
                                    .rounded(px(5.))
                                    .bg(chip)
                                    .text_color(fg)
                                    .hover(move |b| b.bg(chip_hover))
                                    .child("Fix")
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                    .on_click(cx.listener(move |this, _e, window, cx| {
                                        cx.stop_propagation();
                                        this.go_to_note(line, column, window, cx);
                                        this.request_code_actions(cx);
                                    })),
                            ),
                    )
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _e, window, cx| {
                        this.go_to_note(line, column, window, cx);
                    }))
                    .into_any_element(),
            );
        }

        Some(
            div()
                .id("margin-notes")
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(width))
                .overflow_hidden()
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            let origin = bounds.origin;
                            let mid = origin.x + px(CURVE_GAP / 2.);
                            for &(from_y, to_y, color) in &curves {
                                let start = point(origin.x + px(2.), origin.y + px(from_y));
                                let end = point(origin.x + px(CURVE_GAP), origin.y + px(to_y));
                                let mut path = PathBuilder::stroke(px(1.2));
                                path.move_to(start);
                                path.cubic_bezier_to(end, point(mid, start.y), point(mid, end.y));
                                if let Ok(path) = path.build() {
                                    window.paint_path(path, color.opacity(0.45));
                                }
                                window.paint_quad(
                                    fill(Bounds::centered_at(start, size(px(5.), px(5.))), color)
                                        .corner_radii(px(2.5)),
                                );
                            }
                        },
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
                )
                .children(cards)
                .into_any_element(),
        )
    }

    /// Narrow editors: a dot at the right edge of each line with a note,
    /// message in the tooltip, click to jump.
    fn render_note_markers(
        &self,
        notes: &[LineNote],
        top_pad: f32,
        scroll_y: f32,
        viewport_h: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut markers = Vec::new();
        for note in notes {
            let Some(center) = self.note_row_center(note.line, top_pad, scroll_y) else {
                continue;
            };
            if center < -LINE_HEIGHT || center > viewport_h + LINE_HEIGHT {
                continue;
            }
            let color = if note.severity == DiagnosticSeverity::ERROR {
                cx.theme().danger
            } else {
                cx.theme().warning
            };
            let (line, column) = (note.line, note.column);
            let tip = note.message.clone();
            markers.push(
                div()
                    .id(("margin-marker", line))
                    .absolute()
                    .top(px(center - 7.))
                    .right(px(6.))
                    .size(px(14.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(7.))
                    .cursor_pointer()
                    .hover(move |m| m.bg(color.opacity(0.18)))
                    .child(div().size(px(7.)).rounded(px(3.5)).bg(color))
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _e, window, cx| {
                        this.go_to_note(line, column, window, cx);
                    }))
                    .into_any_element(),
            );
        }
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(26.))
            .children(markers)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{note_height, stack_notes, title_lines};

    #[test]
    fn notes_sit_by_their_line_and_never_overlap() {
        // Far apart: each keeps its own spot.
        assert_eq!(stack_notes(&[(10., 50.), (200., 50.)], 6.), vec![10., 200.]);
        // Neighbouring lines: the second is pushed just below the first.
        assert_eq!(stack_notes(&[(10., 50.), (32., 50.)], 6.), vec![10., 66.]);
        // A push carries on down the stack.
        assert_eq!(stack_notes(&[(0., 40.), (10., 40.), (20., 40.)], 4.), vec![0., 44., 88.]);
    }

    #[test]
    fn long_messages_wrap_up_to_three_lines() {
        assert_eq!(title_lines("Expected to find '}'.", 220.), 1);
        assert_eq!(title_lines(&"word ".repeat(30), 220.), 3);
        assert!(note_height(2) > note_height(1));
    }
}
