//! The "Table" / "Preview" mode of a data-file tab: CSV/TSV as an editable
//! grid, JSONL as a table or, for chat datasets, as readable conversations.
//!
//! The tab's `EditorView` stays the source of truth. The grid re-parses when
//! the buffer's revision changes, and a cell edit replaces that cell's raw
//! byte range in the buffer, so Code mode, undo and save all keep working
//! and "Write CSV" is just a save.

use std::collections::HashSet;
use std::rc::Rc;

use editor_view::EditorView;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::data_file::{self, ChatTurn, CsvTable, DataKind, JsonlFile};
use crate::tokens;

/// Row height in the grid.
const ROW_H: f32 = tokens::CODE_LINE_H + tokens::SPACE_2;
/// The row-number column.
const GUTTER_W: f32 = 52.;
const COL_MIN: f32 = 64.;
const COL_MAX: f32 = 320.;
/// Approximate advance of one mono character at `UI_SM`.
const CHAR_W: f32 = 7.;
/// Width of the conversation column in chat previews, for comfortable lines.
const CHAT_MAX_W: f32 = 760.;
/// Role label column in a conversation turn.
const ROLE_W: f32 = 72.;
/// Lines of a turn shown before "Show more".
const TURN_CLAMP: usize = 6;
/// The record inspector beside a JSONL table.
const INSPECTOR_W: f32 = 440.;
/// Lines of one field shown in the inspector before "Show all".
const FIELD_CLAMP: usize = 40;

#[derive(Clone)]
enum Grid {
    Csv { table: Rc<CsvTable>, header: bool },
    Jsonl { file: Rc<JsonlFile>, columns: Rc<Vec<String>> },
}

impl Grid {
    fn row_count(&self) -> usize {
        match self {
            Grid::Csv { table, header } => table.rows.len().saturating_sub(usize::from(*header)),
            Grid::Jsonl { file, .. } => file.records.len(),
        }
    }

    fn column_count(&self) -> usize {
        match self {
            Grid::Csv { table, .. } => table.column_count(),
            Grid::Jsonl { columns, .. } => columns.len().max(1),
        }
    }

    fn header(&self, col: usize) -> String {
        match self {
            Grid::Csv { table, header: true } => {
                table.rows.first().and_then(|r| r.get(col)).map(|c| c.value.clone()).unwrap_or_default()
            }
            Grid::Csv { .. } => column_letter(col),
            Grid::Jsonl { columns, .. } => columns.get(col).cloned().unwrap_or_else(|| "value".into()),
        }
    }

    /// Display text of a body cell (`row` excludes the header).
    fn cell(&self, row: usize, col: usize) -> String {
        match self {
            Grid::Csv { table, header } => table
                .rows
                .get(row + usize::from(*header))
                .and_then(|r| r.get(col))
                .map(|c| c.value.replace('\n', " ⏎ "))
                .unwrap_or_default(),
            Grid::Jsonl { file, columns } => match file.records.get(row).map(|r| &r.value) {
                Some(Ok(serde_json::Value::Object(o))) => {
                    columns.get(col).and_then(|k| o.get(k)).map(data_file::compact).unwrap_or_default()
                }
                Some(Ok(other)) if col == 0 => data_file::compact(other),
                Some(Err(e)) if col == 0 => format!("invalid JSON: {e}"),
                _ => String::new(),
            },
        }
    }
}

/// `A`, `B`, … `Z`, `AA`: spreadsheet column names for header-less CSV.
fn column_letter(mut col: usize) -> String {
    let mut s = String::new();
    loop {
        s.insert(0, (b'A' + (col % 26) as u8) as char);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    s
}

fn looks_numeric(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.parse::<f64>().is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortDir {
    Asc,
    Desc,
}

struct Editing {
    row: usize,
    col: usize,
    input: Entity<InputState>,
    _sub: Subscription,
}

pub(crate) struct DataView {
    editor: Entity<EditorView>,
    kind: DataKind,
    parsed_revision: Option<u64>,
    grid: Grid,
    col_widths: Rc<Vec<f32>>,
    numeric_cols: Rc<Vec<bool>>,
    /// JSONL whose records read as conversations: one entry per record.
    chat: Option<Rc<Vec<Option<Vec<ChatTurn>>>>>,
    /// Show a chat dataset as raw columns instead.
    chat_as_table: bool,
    csv_header: bool,
    filter_query: String,
    filter_input: Option<(Entity<InputState>, Subscription)>,
    sort: Option<(usize, SortDir)>,
    /// Underlying `grid` row indices after filtering and sorting, or `None` when unfiltered & unsorted.
    visible_rows: Option<Rc<Vec<usize>>>,
    table_scroll: UniformListScrollHandle,
    hscroll: ScrollHandle,
    chat_list: ListState,
    expanded: Rc<std::cell::RefCell<HashSet<usize>>>,
    selected: Option<(usize, usize)>,
    /// Show the selected JSONL record in full beside the table.
    inspect: bool,
    /// Inspector fields expanded past `FIELD_CLAMP`, by key.
    expanded_fields: HashSet<String>,
    inspector_scroll: ScrollHandle,
    editing: Option<Editing>,
    _observe: Subscription,
}

impl DataView {
    pub(crate) fn new(editor: Entity<EditorView>, kind: DataKind, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&editor, |_, _, cx| cx.notify());
        let mut view = Self {
            editor,
            kind,
            parsed_revision: None,
            grid: Grid::Csv { table: Rc::default(), header: true },
            col_widths: Rc::default(),
            numeric_cols: Rc::default(),
            chat: None,
            chat_as_table: false,
            csv_header: true,
            filter_query: String::new(),
            filter_input: None,
            sort: None,
            visible_rows: None,
            table_scroll: UniformListScrollHandle::new(),
            hscroll: ScrollHandle::new(),
            chat_list: ListState::new(0, ListAlignment::Top, px(400.)),
            expanded: Rc::default(),
            selected: None,
            inspect: true,
            expanded_fields: HashSet::new(),
            inspector_scroll: ScrollHandle::new(),
            editing: None,
            _observe: observe,
        };
        view.reparse_if_needed(cx);
        view
    }

    pub(crate) fn kind(&self) -> DataKind {
        self.kind
    }

    fn reparse_if_needed(&mut self, cx: &mut Context<Self>) {
        let editor = self.editor.read(cx);
        let revision = editor.buffer().revision();
        if self.parsed_revision == Some(revision) {
            return;
        }
        self.parsed_revision = Some(revision);
        let text = editor.buffer().text();
        match self.kind {
            DataKind::Csv(delim) => {
                let table = Rc::new(data_file::parse_csv(&text, delim));
                self.grid = Grid::Csv { table, header: self.csv_header };
                self.chat = None;
            }
            DataKind::Jsonl => {
                let file = data_file::parse_jsonl(&text);
                let chat = data_file::is_chat_dataset(&file).then(|| {
                    Rc::new(
                        file.records
                            .iter()
                            .map(|r| r.value.as_ref().ok().and_then(data_file::chat_turns))
                            .collect::<Vec<_>>(),
                    )
                });
                if let Some(chat) = &chat {
                    if self.chat_list.item_count() != chat.len() {
                        self.chat_list.reset(chat.len());
                    } else {
                        self.chat_list.remeasure();
                    }
                }
                self.chat = chat;
                let columns = Rc::new(data_file::jsonl_columns(&file));
                self.grid = Grid::Jsonl { file: Rc::new(file), columns };
            }
        }
        self.measure_columns();
        self.rebuild_visible_rows();
        if matches!(self.grid, Grid::Jsonl { .. }) {
            let rows = self.grid.row_count();
            match self.selected {
                Some((r, _)) if r < rows => {}
                _ => self.selected = (rows > 0).then_some((0, 0)),
            }
        }
    }

    fn ensure_filter_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some((input, _)) = &self.filter_input {
            return input.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter rows\u{2026}"));
        let sub = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                let q = input.read(cx).value().to_string();
                this.set_filter(q, cx);
            }
        });
        self.filter_input = Some((input.clone(), sub));
        input
    }

    fn set_filter(&mut self, query: String, cx: &mut Context<Self>) {
        if self.filter_query == query {
            return;
        }
        self.filter_query = query;
        self.rebuild_visible_rows();
        cx.notify();
    }

    fn clear_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((input, _)) = &self.filter_input {
            input.update(cx, |s, cx| s.set_value("", window, cx));
        }
        self.set_filter(String::new(), cx);
    }

    fn toggle_sort(&mut self, col: usize, cx: &mut Context<Self>) {
        self.sort = match self.sort {
            Some((c, SortDir::Asc)) if c == col => Some((col, SortDir::Desc)),
            Some((c, SortDir::Desc)) if c == col => None,
            _ => Some((col, SortDir::Asc)),
        };
        self.rebuild_visible_rows();
        cx.notify();
    }

    fn visible_row_count(&self) -> usize {
        self.visible_rows.as_ref().map_or_else(|| self.grid.row_count(), |v| v.len())
    }

    fn rebuild_visible_rows(&mut self) {
        let total = self.grid.row_count();
        let needle = self.filter_query.trim().to_ascii_lowercase();
        let sort = self.sort.filter(|&(c, _)| c < self.grid.column_count());
        if needle.is_empty() && sort.is_none() {
            self.visible_rows = None;
            if let Some(chat) = &self.chat {
                if self.chat_list.item_count() != chat.len() {
                    self.chat_list.reset(chat.len());
                }
            }
            return;
        }
        let cols = self.grid.column_count();
        let mut rows: Vec<usize> = if needle.is_empty() {
            (0..total).collect()
        } else if let (Some(chat), false) = (&self.chat, self.chat_as_table) {
            (0..total)
                .filter(|&r| {
                    chat.get(r).and_then(Option::as_ref).is_some_and(|turns| {
                        turns.iter().any(|t| {
                            t.role.to_ascii_lowercase().contains(&needle)
                                || t.content.to_ascii_lowercase().contains(&needle)
                        })
                    })
                })
                .collect()
        } else {
            (0..total)
                .filter(|&r| {
                    (0..cols).any(|c| self.grid.cell(r, c).to_ascii_lowercase().contains(&needle))
                })
                .collect()
        };

        if let Some((col, dir)) = sort {
            let numeric = self.numeric_cols.get(col).copied().unwrap_or(false);
            let mut keyed: Vec<(usize, String)> = rows
                .into_iter()
                .map(|r| {
                    let v = self.grid.cell(r, col);
                    (r, v)
                })
                .collect();
            keyed.sort_by(|(_, a), (_, b)| {
                let a_empty = a.trim().is_empty();
                let b_empty = b.trim().is_empty();
                match (a_empty, b_empty) {
                    (true, true) => return std::cmp::Ordering::Equal,
                    (true, false) => return std::cmp::Ordering::Greater,
                    (false, true) => return std::cmp::Ordering::Less,
                    (false, false) => {}
                }
                let ord = if numeric {
                    match (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
                        (Ok(na), Ok(nb)) => na.total_cmp(&nb),
                        _ => a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()),
                    }
                } else {
                    a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase())
                };
                match dir {
                    SortDir::Asc => ord,
                    SortDir::Desc => ord.reverse(),
                }
            });
            rows = keyed.into_iter().map(|(r, _)| r).collect();
        }

        if self.chat.is_some() {
            self.chat_list.reset(rows.len());
        }
        self.visible_rows = Some(Rc::new(rows));
    }

    fn measure_columns(&mut self) {
        let cols = self.grid.column_count();
        let sample = self.grid.row_count().min(200);
        let mut widths = Vec::with_capacity(cols);
        let mut numeric = Vec::with_capacity(cols);
        for c in 0..cols {
            let mut chars = self.grid.header(c).chars().count();
            let mut nums = 0;
            let mut filled = 0;
            for r in 0..sample {
                let v = self.grid.cell(r, c);
                chars = chars.max(v.chars().count());
                if !v.trim().is_empty() {
                    filled += 1;
                    nums += usize::from(looks_numeric(&v));
                }
            }
            widths.push((chars as f32 * CHAR_W + tokens::SPACE_6).clamp(COL_MIN, COL_MAX));
            numeric.push(filled > 0 && nums == filled);
        }
        self.col_widths = Rc::new(widths);
        self.numeric_cols = Rc::new(numeric);
    }

    fn set_csv_header(&mut self, header: bool, cx: &mut Context<Self>) {
        self.csv_header = header;
        self.editing = None;
        self.selected = None;
        self.parsed_revision = None;
        self.reparse_if_needed(cx);
        cx.notify();
    }

    fn start_edit(&mut self, row: usize, col: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Grid::Csv { table, header } = &self.grid else { return };
        let value = table
            .rows
            .get(row + usize::from(*header))
            .and_then(|r| r.get(col))
            .map(|c| c.value.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(value, window, cx);
            state
        });
        let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } | InputEvent::Blur => this.commit_edit(window, cx),
            _ => {}
        });
        input.update(cx, |state, cx| state.focus(window, cx));
        self.selected = Some((row, col));
        self.editing = Some(Editing { row, col, input, _sub: sub });
        cx.notify();
    }

    /// Write the edited value into the buffer: replace the cell's raw range,
    /// or pad a short row out to the edited column.
    fn commit_edit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else { return };
        cx.notify();
        let DataKind::Csv(delim) = self.kind else { return };
        let Grid::Csv { table, header } = &self.grid else { return };
        let value = editing.input.read(cx).value().to_string();
        let Some(row) = table.rows.get(editing.row + usize::from(*header)) else { return };
        let (range, text) = match row.get(editing.col) {
            Some(cell) if cell.value == value => return,
            Some(cell) => (cell.raw.clone(), data_file::quote_field(&value, delim)),
            None => {
                let Some(last) = row.last() else { return };
                let missing = editing.col + 1 - row.len();
                let sep = (delim as char).to_string();
                let text = format!("{}{}", sep.repeat(missing), data_file::quote_field(&value, delim));
                (last.raw.end..last.raw.end, text)
            }
        };
        self.editor.update(cx, |editor, cx| editor.replace_range(range, &text, cx));
    }

    fn write_file(&mut self, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| editor.save_without_format(cx));
        cx.notify();
    }

    fn toolbar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let dirty = self.editor.read(cx).buffer().is_dirty();
        let truncated = match &self.grid {
            Grid::Csv { table, .. } => table.truncated,
            Grid::Jsonl { file, .. } => file.truncated,
        };
        let total = self.grid.row_count();
        let shown = self.visible_row_count();
        let filtered = !self.filter_query.trim().is_empty();
        let mut summary = match (&self.chat, self.chat_as_table) {
            (Some(chat), false) => {
                let turns: usize = chat.iter().flatten().map(Vec::len).sum();
                if filtered {
                    format!("{} of {} conversations · {} turns", fmt_count(shown), fmt_count(chat.len()), fmt_count(turns))
                } else {
                    format!("{} conversations · {} turns", fmt_count(chat.len()), fmt_count(turns))
                }
            }
            _ => {
                if filtered {
                    format!("{} of {} rows · {} columns", fmt_count(shown), fmt_count(total), self.grid.column_count())
                } else {
                    format!("{} rows · {} columns", fmt_count(total), self.grid.column_count())
                }
            }
        };
        if truncated {
            summary.push_str(&format!(" · first {} shown", fmt_count(data_file::MAX_ROWS)));
        }

        let filter_input = self.ensure_filter_input(window, cx);
        let mut bar = h_flex()
            .w_full()
            .h(px(tokens::PANEL_BAR_H))
            .flex_none()
            .px(px(tokens::SPACE_5))
            .gap(px(tokens::SPACE_3))
            .items_center()
            .border_b_1()
            .border_color(border)
            .child(div().text_size(px(tokens::UI_SM)).text_color(muted).child(summary))
            .child(div().flex_1())
            .child(
                div()
                    .w(px(180.))
                    .child(Input::new(&filter_input).small().cleanable(true)),
            );

        if self.sort.is_some() {
            bar = bar.child(chip("data-clear-sort", "Clear sort", true, cx).on_click(cx.listener(
                |this, _, _, cx| {
                    this.sort = None;
                    this.rebuild_visible_rows();
                    cx.notify();
                },
            )));
        }

        if matches!(self.kind, DataKind::Csv(_)) {
            let header = self.csv_header;
            bar = bar.child(
                chip("data-header", "Header row", header, cx)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_csv_header(!header, cx))),
            );
            let label = if self.kind == DataKind::Csv(b'\t') { "Write TSV" } else { "Write CSV" };
            bar = bar.child(
                chip("data-write", label, dirty, cx)
                    .when(!dirty, |el| el.opacity(0.5))
                    .on_click(cx.listener(|this, _, _, cx| this.write_file(cx))),
            );
        }
        if self.kind == DataKind::Jsonl && (self.chat.is_none() || self.chat_as_table) {
            let inspect = self.inspect;
            bar = bar.child(chip("data-inspect", "Details", inspect, cx).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.inspect = !inspect;
                    cx.notify();
                },
            )));
        }
        if self.chat.is_some() {
            let as_table = self.chat_as_table;
            bar = bar.child(chip("data-raw", "Columns", as_table, cx).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.chat_as_table = !as_table;
                    this.rebuild_visible_rows();
                    cx.notify();
                },
            )));
        }
        bar
    }

    fn render_grid(&self, cx: &mut Context<Self>) -> AnyElement {
        let grid = self.grid.clone();
        if grid.row_count() == 0 && matches!(grid, Grid::Csv { .. }) && grid.column_count() == 0 {
            return empty_state("Empty file", "Switch to Code to start typing.", cx).into_any_element();
        }
        let border = cx.theme().border;
        let mono = cx.theme().mono_font_family.clone();
        let widths = self.col_widths.clone();
        let total_w: f32 = GUTTER_W + widths.iter().sum::<f32>();
        let numeric = self.numeric_cols.clone();

        let sort = self.sort;
        let header = h_flex()
            .h(px(ROW_H))
            .flex_none()
            .w(px(total_w))
            .border_b_1()
            .border_color(border)
            .bg(cx.theme().secondary)
            .child(div().w(px(GUTTER_W)).flex_none())
            .children((0..grid.column_count()).map(|c| {
                let num = numeric.get(c).copied().unwrap_or(false);
                let arrow = match sort {
                    Some((sc, SortDir::Asc)) if sc == c => Some(" \u{2191}"),
                    Some((sc, SortDir::Desc)) if sc == c => Some(" \u{2193}"),
                    _ => None,
                };
                div()
                    .id(("data-col", c))
                    .w(px(widths.get(c).copied().unwrap_or(COL_MIN)))
                    .flex_none()
                    .h_full()
                    .px(px(tokens::SPACE_4))
                    .flex()
                    .items_center()
                    .gap(px(tokens::SPACE_1))
                    .when(num, |d| d.justify_end())
                    .border_l_1()
                    .border_color(border)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if arrow.is_some() { cx.theme().primary } else { cx.theme().foreground })
                    .cursor_pointer()
                    .hover(|s| s.bg(tokens::line_active()))
                    .child(div().truncate().child(grid.header(c)))
                    .when_some(arrow, |d, a| d.child(div().flex_none().child(a)))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_sort(c, cx)))
            }));

        let editable = matches!(grid, Grid::Csv { .. });
        let selected = self.selected;
        let editing_cell = self.editing.as_ref().map(|e| (e.row, e.col, e.input.clone()));
        let visible = self.visible_rows.clone();
        let row_count = visible.as_ref().map_or_else(|| grid.row_count(), |v| v.len());
        let rows = uniform_list(
            "data-rows",
            row_count,
            cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                let fg = cx.theme().foreground;
                let muted = cx.theme().muted_foreground;
                let border = cx.theme().border;
                let accent = cx.theme().primary;
                range
                    .map(|display_r| {
                        let r = visible
                            .as_ref()
                            .and_then(|v| v.get(display_r).copied())
                            .unwrap_or(display_r);
                        let cells: Vec<AnyElement> = (0..grid.column_count())
                            .map(|c| {
                                let w = widths.get(c).copied().unwrap_or(COL_MIN);
                                let base = div()
                                    .id(("data-cell", r * 4096 + c))
                                    .w(px(w))
                                    .flex_none()
                                    .h_full()
                                    .border_l_1()
                                    .border_color(border);
                                if let Some((er, ec, input)) = &editing_cell {
                                    if *er == r && *ec == c {
                                        return base
                                            .border_1()
                                            .border_color(accent)
                                            .child(Input::new(input).appearance(false).small().h_full())
                                            .into_any_element();
                                    }
                                }
                                let num = numeric.get(c).copied().unwrap_or(false);
                                base.px(px(tokens::SPACE_4))
                                    .flex()
                                    .items_center()
                                    .when(num, |d| d.justify_end())
                                    .text_color(fg)
                                    .when(selected == Some((r, c)), |d| {
                                        d.bg(tokens::accent_dim()).border_1().border_color(accent.opacity(0.6))
                                    })
                                    .child(div().truncate().child(grid.cell(r, c)))
                                    .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                                        if editable && ev.click_count() >= 2 {
                                            this.start_edit(r, c, window, cx);
                                        } else {
                                            this.selected = Some((r, c));
                                            cx.notify();
                                        }
                                    }))
                                    .into_any_element()
                            })
                            .collect();
                        let row_selected = !editable && selected.map(|s| s.0) == Some(r);
                        h_flex()
                            .h(px(ROW_H))
                            .w(px(total_w))
                            .border_b_1()
                            .border_color(border.opacity(0.5))
                            .when(row_selected, |d| d.bg(tokens::accent_dim()))
                            .hover(|s| s.bg(tokens::line_active()))
                            .child(
                                div()
                                    .w(px(GUTTER_W))
                                    .flex_none()
                                    .pr(px(tokens::SPACE_4))
                                    .text_right()
                                    .text_color(muted)
                                    .child((r + 1).to_string()),
                            )
                            .children(cells)
                            .into_any_element()
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.table_scroll)
        .flex_1()
        .w(px(total_w));

        div()
            .id("data-hscroll")
            .flex_1()
            .min_h(px(0.))
            .w_full()
            .overflow_x_scroll()
            .track_scroll(&self.hscroll)
            .font_family(mono)
            .text_size(px(tokens::UI_SM))
            .child(v_flex().h_full().w(px(total_w)).child(header).child(rows))
            .into_any_element()
    }

    fn render_chat(&self, chat: Rc<Vec<Option<Vec<ChatTurn>>>>, cx: &mut Context<Self>) -> AnyElement {
        let Grid::Jsonl { file, .. } = &self.grid else {
            return div().into_any_element();
        };
        let lines: Rc<Vec<usize>> = Rc::new(file.records.iter().map(|r| r.line).collect());
        let errors: Rc<Vec<Option<String>>> =
            Rc::new(file.records.iter().map(|r| r.value.as_ref().err().cloned()).collect());
        let expanded = self.expanded.clone();
        let list_state = self.chat_list.clone();
        let view = cx.entity().downgrade();
        let visible = self.visible_rows.clone();
        list(self.chat_list.clone(), move |display_ix, _window, cx| {
            let ix = visible
                .as_ref()
                .and_then(|v| v.get(display_ix).copied())
                .unwrap_or(display_ix);
            let muted = cx.theme().muted_foreground;
            let is_open = expanded.borrow().contains(&ix);
            let mut card = v_flex()
                .w_full()
                .max_w(px(CHAT_MAX_W))
                .p(px(tokens::SPACE_5))
                .gap(px(tokens::SPACE_4))
                .rounded(px(tokens::RADIUS_CARD))
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().secondary)
                .child(
                    h_flex()
                        .gap(px(tokens::SPACE_3))
                        .text_size(px(tokens::UI_XS))
                        .text_color(muted)
                        .child(format!("#{}", ix + 1))
                        .child("·")
                        .child(format!("line {}", lines.get(ix).copied().unwrap_or(0) + 1)),
                );
            match (chat.get(ix).and_then(Option::as_ref), errors.get(ix).and_then(Option::as_ref)) {
                (Some(turns), _) => {
                    let mut clamped = false;
                    for turn in turns {
                        let long = turn.content.lines().count() > TURN_CLAMP || turn.content.len() > 600;
                        clamped |= long && !is_open;
                        card = card.child(render_turn(turn, long && !is_open, cx));
                    }
                    if clamped || is_open {
                        let expanded = expanded.clone();
                        let state = list_state.clone();
                        let view = view.clone();
                        card = card.child(
                            div()
                                .id(("chat-more", ix))
                                .pl(px(ROLE_W + tokens::SPACE_5))
                                .text_size(px(tokens::UI_XS))
                                .text_color(cx.theme().primary)
                                .cursor_pointer()
                                .child(if is_open { "Show less" } else { "Show more" })
                                .on_click(move |_, _, cx| {
                                    {
                                        let mut set = expanded.borrow_mut();
                                        if !set.remove(&ix) {
                                            set.insert(ix);
                                        }
                                    }
                                    state.remeasure_items(ix..ix + 1);
                                    if let Some(view) = view.upgrade() {
                                        view.update(cx, |_, cx| cx.notify());
                                    }
                                }),
                        );
                    }
                }
                (None, Some(err)) => {
                    card = card.child(
                        div()
                            .text_size(px(tokens::UI_SM))
                            .text_color(cx.theme().danger)
                            .child(format!("Invalid JSON: {err}")),
                    );
                }
                (None, None) => {
                    card = card.child(
                        div()
                            .text_size(px(tokens::UI_SM))
                            .text_color(muted)
                            .child("Not a conversation. Switch to Columns to see its fields."),
                    );
                }
            }
            h_flex()
                .w_full()
                .justify_center()
                .px(px(tokens::SPACE_6))
                .pt(px(if ix == 0 { tokens::SPACE_6 } else { tokens::SPACE_4 }))
                .child(card)
                .into_any_element()
        })
        .flex_1()
        .w_full()
        .into_any_element()
    }
}

/// One turn: a role label column and the message text.
fn render_turn(turn: &ChatTurn, clamp: bool, cx: &App) -> Div {
    let theme = cx.theme();
    let role_color = match turn.role.as_str() {
        "user" => theme.info,
        "assistant" => theme.primary,
        "system" => theme.muted_foreground,
        _ => theme.warning,
    };
    let empty = turn.content.trim().is_empty();
    let content: SharedString = if empty { "(empty)".into() } else { turn.content.clone().into() };
    h_flex()
        .items_start()
        .gap(px(tokens::SPACE_5))
        .child(
            div()
                .w(px(ROLE_W))
                .flex_none()
                .pt(px(tokens::SPACE_1))
                .text_size(px(tokens::UI_XS))
                .font_weight(FontWeight::MEDIUM)
                .text_color(role_color)
                .child(turn.role.to_uppercase()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(tokens::UI))
                .line_height(px(tokens::CODE_LINE_H - tokens::SPACE_1))
                .text_color(if empty { theme.muted_foreground } else { theme.foreground })
                .when(clamp, |d| d.line_clamp(TURN_CLAMP))
                .child(content),
        )
}

fn chip(id: &'static str, label: &'static str, active: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let (primary, border, fg, muted, raised) =
        (theme.primary, theme.border, theme.foreground, theme.muted_foreground, theme.secondary);
    h_flex()
        .id(id)
        .h(px(tokens::TREE_ROW_H - tokens::SPACE_3))
        .px(px(tokens::SPACE_4))
        .items_center()
        .rounded(px(tokens::RADIUS_SM))
        .border_1()
        .text_size(px(tokens::UI_XS))
        .cursor_pointer()
        .when(active, |s| s.bg(tokens::accent_dim()).border_color(primary.opacity(0.4)).text_color(fg))
        .when(!active, |s| s.border_color(border).text_color(muted).hover(|h| h.bg(raised)))
        .child(label)
}

fn empty_state(title: &'static str, body: &'static str, cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(tokens::SPACE_3))
        .child(div().text_size(px(tokens::UI_LG)).text_color(theme.foreground).child(title))
        .child(div().text_size(px(tokens::UI_SM)).text_color(theme.muted_foreground).child(body))
}

fn fmt_count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

impl DataView {
    /// The selected JSONL record, every field in full.
    fn render_inspector(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Grid::Jsonl { file, .. } = &self.grid else { return None };
        let (row, _) = self.selected?;
        let record = file.records.get(row)?;
        let theme = cx.theme();
        let (fg, muted, border, primary, red, green) =
            (theme.foreground, theme.muted_foreground, theme.border, theme.primary, theme.red, theme.green);
        let mono = theme.mono_font_family.clone();

        let header = h_flex()
            .h(px(tokens::PANEL_BAR_H))
            .flex_none()
            .px(px(tokens::SPACE_5))
            .gap(px(tokens::SPACE_3))
            .items_center()
            .border_b_1()
            .border_color(border)
            .child(div().text_size(px(tokens::UI_SM)).font_weight(FontWeight::MEDIUM).text_color(fg).child(format!("Record {}", row + 1)))
            .child(div().text_size(px(tokens::UI_XS)).text_color(muted).child(format!("line {}", record.line + 1)));

        let mut fields = v_flex().w_full().p(px(tokens::SPACE_5)).gap(px(tokens::SPACE_5));
        let value = match &record.value {
            Err(e) => {
                fields = fields.child(div().text_size(px(tokens::UI_SM)).text_color(red).child(format!("Invalid JSON: {e}")));
                None
            }
            Ok(v) => Some(v.clone()),
        };
        let entries: Vec<(String, serde_json::Value)> = match value {
            Some(serde_json::Value::Object(o)) => o.into_iter().collect(),
            Some(other) => vec![("value".into(), other)],
            None => Vec::new(),
        };
        for (key, v) in entries {
            let (text, color, kind) = match &v {
                serde_json::Value::String(s) => (s.clone(), fg, format!("text · {} chars", s.chars().count())),
                serde_json::Value::Number(n) => (n.to_string(), primary, "number".into()),
                serde_json::Value::Bool(b) => (b.to_string(), if *b { green } else { red }, "bool".into()),
                serde_json::Value::Null => ("null".into(), muted, "null".into()),
                serde_json::Value::Array(a) => (
                    serde_json::to_string_pretty(&v).unwrap_or_default(),
                    fg,
                    format!("list · {} items", a.len()),
                ),
                serde_json::Value::Object(o) => (
                    serde_json::to_string_pretty(&v).unwrap_or_default(),
                    fg,
                    format!("object · {} keys", o.len()),
                ),
            };
            let lines: Vec<&str> = text.lines().collect();
            let open = self.expanded_fields.contains(&key);
            let shown = if open { lines.len() } else { lines.len().min(FIELD_CLAMP) };
            let more = lines.len() - shown;
            let body = if text.is_empty() { "(empty)".to_string() } else { lines[..shown].join("\n") };
            let key_for_click = key.clone();
            fields = fields.child(
                v_flex()
                    .w_full()
                    .gap(px(tokens::SPACE_2))
                    .child(
                        h_flex()
                            .gap(px(tokens::SPACE_3))
                            .items_baseline()
                            .child(div().font_family(mono.clone()).text_size(px(tokens::UI_SM)).text_color(fg).font_weight(FontWeight::MEDIUM).child(key.clone()))
                            .child(div().text_size(px(tokens::UI_XS)).text_color(muted).child(kind)),
                    )
                    .child(
                        div()
                            .w_full()
                            .px(px(tokens::SPACE_4))
                            .py(px(tokens::SPACE_3))
                            .rounded(px(tokens::RADIUS_SM))
                            .bg(tokens::bg_editor())
                            .border_1()
                            .border_color(border)
                            .font_family(mono.clone())
                            .text_size(px(tokens::UI_SM))
                            .line_height(px(tokens::CODE_LINE_H - tokens::SPACE_2))
                            .text_color(if text.is_empty() { muted } else { color })
                            .child(body),
                    )
                    .when(more > 0 || open && lines.len() > FIELD_CLAMP, |c| {
                        c.child(
                            div()
                                .id(SharedString::from(format!("field-more-{key}")))
                                .text_size(px(tokens::UI_XS))
                                .text_color(primary)
                                .cursor_pointer()
                                .child(if open { "Show less".to_string() } else { format!("Show all ({more} more lines)") })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.expanded_fields.remove(&key_for_click) {
                                        this.expanded_fields.insert(key_for_click.clone());
                                    }
                                    cx.notify();
                                })),
                        )
                    }),
            );
        }
        Some(
            v_flex()
                .w(px(INSPECTOR_W))
                .flex_none()
                .h_full()
                .border_l_1()
                .border_color(border)
                .child(header)
                .child(
                    div()
                        .id("data-inspector")
                        .flex_1()
                        .min_h(px(0.))
                        .overflow_y_scroll()
                        .track_scroll(&self.inspector_scroll)
                        .child(fields),
                )
                .into_any_element(),
        )
    }
}

impl Render for DataView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.reparse_if_needed(cx);
        let body = match (&self.chat, self.chat_as_table) {
            (Some(chat), false) => self.render_chat(chat.clone(), cx),
            _ => {
                let grid = self.render_grid(cx);
                let inspector = self.inspect.then(|| self.render_inspector(cx)).flatten();
                match inspector {
                    Some(inspector) => h_flex()
                        .flex_1()
                        .min_h(px(0.))
                        .w_full()
                        .child(div().flex_1().min_w(px(0.)).h_full().flex().flex_col().child(grid))
                        .child(inspector)
                        .into_any_element(),
                    None => grid,
                }
            }
        };
        v_flex()
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    if this.editing.take().is_some() {
                        cx.stop_propagation();
                        cx.notify();
                    } else if !this.filter_query.is_empty() {
                        this.clear_filter(window, cx);
                        cx.stop_propagation();
                    }
                }
            }))
            .size_full()
            .min_w(px(0.))
            .min_h(px(0.))
            .bg(cx.theme().background)
            .child(self.toolbar(window, cx))
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::{column_letter, fmt_count, looks_numeric};

    #[test]
    fn column_letters_counts_and_numbers() {
        assert_eq!(column_letter(0), "A");
        assert_eq!(column_letter(25), "Z");
        assert_eq!(column_letter(26), "AA");
        assert_eq!(fmt_count(1_234_567), "1,234,567");
        assert_eq!(fmt_count(12), "12");
        assert!(looks_numeric(" 3.5 "));
        assert!(!looks_numeric("abc"));
    }
}

/// The views rendered in a headless GPUI window.
#[cfg(test)]
mod ui_tests {
    use super::{DataKind, DataView};
    use core_buffer::Buffer;
    use editor_view::EditorView;
    use gpui_kit::{AppContext as _, Entity, VisualTestContext};

    fn open<'a>(
        cx: &'a mut ::gpui::TestAppContext,
        text: &str,
        kind: DataKind,
    ) -> (Entity<DataView>, &'a mut VisualTestContext) {
        cx.update(gpui_kit::init);
        let text = text.to_string();
        let (view, cx) = cx.add_window_view(move |_window, cx| {
            let editor = cx.new(|cx| EditorView::from_buffer(Buffer::from_str(&text), cx));
            DataView::new(editor, kind, cx)
        });
        cx.run_until_parked();
        (view, cx)
    }

    fn buffer_text(view: &Entity<DataView>, cx: &mut VisualTestContext) -> String {
        cx.update(|_, cx| view.read(cx).editor.read(cx).buffer().text())
    }

    #[::gpui::test]
    fn editing_a_csv_cell_rewrites_only_that_cell(cx: &mut ::gpui::TestAppContext) {
        let (view, cx) = open(cx, "name,score\nada,1\nbob,2\n", DataKind::Csv(b','));
        cx.update(|_, cx| {
            let v = view.read(cx);
            assert_eq!(v.grid.row_count(), 2);
            assert_eq!(v.grid.column_count(), 2);
            assert_eq!(v.grid.header(1), "score");
            assert_eq!(v.numeric_cols.as_slice(), &[false, true]);
        });
        cx.update(|window, cx| {
            view.update(cx, |v, cx| v.start_edit(1, 0, window, cx));
            let input = view.read(cx).editing.as_ref().unwrap().input.clone();
            input.update(cx, |s, cx| s.set_value("bob, jr", window, cx));
            view.update(cx, |v, cx| v.commit_edit(window, cx));
        });
        cx.run_until_parked();
        assert_eq!(buffer_text(&view, cx), "name,score\nada,1\n\"bob, jr\",2\n");
        cx.update(|_, cx| assert_eq!(view.read(cx).grid.cell(1, 0), "bob, jr"));
    }

    #[::gpui::test]
    fn editing_past_a_short_row_pads_it(cx: &mut ::gpui::TestAppContext) {
        let (view, cx) = open(cx, "a,b,c\n1\n", DataKind::Csv(b','));
        cx.update(|window, cx| {
            view.update(cx, |v, cx| v.start_edit(0, 2, window, cx));
            let input = view.read(cx).editing.as_ref().unwrap().input.clone();
            input.update(cx, |s, cx| s.set_value("z", window, cx));
            view.update(cx, |v, cx| v.commit_edit(window, cx));
        });
        assert_eq!(buffer_text(&view, cx), "a,b,c\n1,,z\n");
    }

    #[::gpui::test]
    fn plain_jsonl_selects_the_first_record_for_the_inspector(cx: &mut ::gpui::TestAppContext) {
        // The shape of the harness's attempts.jsonl: long text, nested maps, nulls.
        let src = "{\"task\":\"tree2graph_concrete\",\"n\":1,\"parse_error\":null,\"verified\":{\"p1\":\"holds\"},\"reply\":\"line one\\nline two\"}\n";
        let (view, cx) = open(cx, src, DataKind::Jsonl);
        cx.update(|_, cx| {
            let v = view.read(cx);
            assert!(v.chat.is_none());
            assert_eq!(v.selected, Some((0, 0)));
            assert!(v.inspect);
            let task = (0..v.grid.column_count()).find(|&c| v.grid.header(c) == "task").unwrap();
            assert_eq!(v.grid.cell(0, task), "tree2graph_concrete");
        });
        // Expanding a field and hiding the inspector both re-render cleanly.
        cx.update(|_, cx| view.update(cx, |v, cx| {
            v.expanded_fields.insert("reply".into());
            cx.notify();
        }));
        cx.run_until_parked();
        cx.update(|_, cx| view.update(cx, |v, cx| {
            v.inspect = false;
            cx.notify();
        }));
        cx.run_until_parked();
    }

    #[::gpui::test]
    fn chat_jsonl_renders_as_conversations(cx: &mut ::gpui::TestAppContext) {
        let src = "{\"messages\":[{\"role\":\"user\",\"content\":\"Hi\"},{\"role\":\"assistant\",\"content\":\"Hello\"}]}\nnot json\n";
        let (view, cx) = open(cx, src, DataKind::Jsonl);
        cx.update(|_, cx| {
            let v = view.read(cx);
            let chat = v.chat.as_ref().expect("a chat dataset");
            assert_eq!(chat.len(), 2);
            assert_eq!(chat[0].as_ref().unwrap()[1].content, "Hello");
            assert!(chat[1].is_none());
        });
        // Switching to columns renders the raw grid.
        cx.update(|_, cx| view.update(cx, |v, cx| {
            v.chat_as_table = true;
            v.rebuild_visible_rows();
            cx.notify();
        }));
        cx.run_until_parked();
        cx.update(|_, cx| assert_eq!(view.read(cx).grid.header(0), "messages"));
    }

    #[::gpui::test]
    fn filtering_and_sorting_preserve_underlying_row_indices(cx: &mut ::gpui::TestAppContext) {
        let (view, cx) = open(cx, "name,score\nada,10\nbob,2\ncara,\ndave,25\n", DataKind::Csv(b','));
        // Numeric sort ascending on column 1 ("score"): bob (2), ada (10), dave (25), cara (empty last).
        cx.update(|_, cx| view.update(cx, |v, cx| v.toggle_sort(1, cx)));
        cx.update(|_, cx| {
            let v = view.read(cx);
            assert_eq!(v.visible_rows.as_deref(), Some(&vec![1, 0, 3, 2]));
        });
        // Descending: dave (25), ada (10), bob (2), cara (empty last).
        cx.update(|_, cx| view.update(cx, |v, cx| v.toggle_sort(1, cx)));
        cx.update(|_, cx| {
            let v = view.read(cx);
            assert_eq!(v.visible_rows.as_deref(), Some(&vec![3, 0, 1, 2]));
        });
        // Filter for "a" (matches ada, cara, dave) while sorted desc: dave (3), ada (0), cara (2).
        cx.update(|_, cx| view.update(cx, |v, cx| v.set_filter("a".into(), cx)));
        cx.update(|_, cx| {
            let v = view.read(cx);
            assert_eq!(v.visible_rows.as_deref(), Some(&vec![3, 0, 2]));
        });
        // Third click clears sort; filter remains.
        cx.update(|_, cx| view.update(cx, |v, cx| v.toggle_sort(1, cx)));
        cx.update(|_, cx| {
            let v = view.read(cx);
            assert_eq!(v.visible_rows.as_deref(), Some(&vec![0, 2, 3]));
        });
    }
}
