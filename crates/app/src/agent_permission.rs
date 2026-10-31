//! Letting you approve what the agent wants to do. Claude Code, started with
//! `--input-format stream-json --permission-prompt-tool stdio`, stops at each
//! action that needs permission and sends a `control_request`; we show it as a
//! card (what it wants to change, with a diff or the command), and your answer
//! goes back as a `control_response`. Formats below were captured from a real run.

use std::path::Path;

use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, h_flex,
    input::{Input, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::{DartIde, tokens};

/// One thing the agent is asking permission to do.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PermissionRequest {
    pub request_id: String,
    pub tool_name: String,
    pub input: Value,
    /// Settings changes the CLI offers with an allow (e.g. "accept edits for the session").
    pub suggestions: Vec<Value>,
}

/// Parse a `control_request` line's JSON. `None` unless it is a tool-use request.
pub(crate) fn parse_permission_request(event: &Value) -> Option<PermissionRequest> {
    if event.get("type")?.as_str()? != "control_request" {
        return None;
    }
    let request = event.get("request")?;
    if request.get("subtype")?.as_str()? != "can_use_tool" {
        return None;
    }
    Some(PermissionRequest {
        request_id: event.get("request_id")?.as_str()?.to_string(),
        tool_name: request.get("tool_name")?.as_str()?.to_string(),
        input: request.get("input").cloned().unwrap_or(Value::Null),
        suggestions: request
            .get("permission_suggestions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    })
}

/// Your answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Allow,
    /// Allow, and apply the CLI's suggested setting so it doesn't ask again.
    AllowAlways,
    Deny(String),
}

/// The `control_response` line that answers `request`.
pub(crate) fn control_response(request: &PermissionRequest, decision: &Decision) -> String {
    let body = match decision {
        Decision::Allow => serde_json::json!({"behavior": "allow", "updatedInput": request.input}),
        Decision::AllowAlways => serde_json::json!({
            "behavior": "allow",
            "updatedInput": request.input,
            "updatedPermissions": request.suggestions,
        }),
        Decision::Deny(message) => serde_json::json!({"behavior": "deny", "message": message}),
    };
    serde_json::json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": request.request_id, "response": body}
    })
    .to_string()
}

/// A user message in the CLI's streaming-input format.
pub(crate) fn user_message_line(text: &str) -> String {
    serde_json::json!({"type": "user", "message": {"role": "user", "content": text}}).to_string()
}

/// Whether a stdout line is the turn's final `result` event (after which the
/// CLI should be told there is no more input).
pub(crate) fn is_result_line(line: &str) -> bool {
    serde_json::from_str::<Value>(line.trim())
        .ok()
        .and_then(|v| v.get("type").and_then(Value::as_str).map(|t| t == "result"))
        .unwrap_or(false)
}

/// The label for the "don't ask again" button, from what the CLI offered.
pub(crate) fn always_label(suggestions: &[Value]) -> Option<&'static str> {
    let accept_edits = suggestions.iter().any(|s| {
        s.get("type").and_then(Value::as_str) == Some("setMode")
            && s.get("mode").and_then(Value::as_str) == Some("acceptEdits")
    });
    match (accept_edits, suggestions.is_empty()) {
        (true, _) => Some("Allow edits this session"),
        (false, false) => Some("Always allow"),
        (false, true) => None,
    }
}

/// The detail shown under the title of a permission card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Detail {
    /// The text being replaced and what replaces it.
    Diff { old: String, new: String },
    /// A command or file contents.
    Code(String),
    /// `name: value` pairs, for tools we don't have a special view of.
    Fields(Vec<(String, String)>),
    Nothing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PermissionSummary {
    pub title: String,
    pub detail: Detail,
}

/// Lines shown from a long detail before it is cut.
const MAX_DETAIL_LINES: usize = 14;

fn cut_lines(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= MAX_DETAIL_LINES {
        return text.trim_end().to_string();
    }
    format!("{}\n\u{2026} {} more lines", lines[..MAX_DETAIL_LINES].join("\n"), lines.len() - MAX_DETAIL_LINES)
}

fn short_path(path: &str, root: &Path) -> String {
    Path::new(path).strip_prefix(root).unwrap_or(Path::new(path)).to_string_lossy().to_string()
}

/// What to tell you about a request, in plain words.
pub(crate) fn summarize_permission(request: &PermissionRequest, root: &Path) -> PermissionSummary {
    let input = &request.input;
    let text = |key: &str| input.get(key).and_then(Value::as_str).unwrap_or("");
    match request.tool_name.as_str() {
        "Edit" => PermissionSummary {
            title: format!("Edit {}", short_path(text("file_path"), root)),
            detail: Detail::Diff { old: cut_lines(text("old_string")), new: cut_lines(text("new_string")) },
        },
        "Write" => PermissionSummary {
            title: format!("Write {}", short_path(text("file_path"), root)),
            detail: Detail::Code(cut_lines(text("content"))),
        },
        "MultiEdit" => {
            let n = input.get("edits").and_then(Value::as_array).map_or(0, Vec::len);
            PermissionSummary {
                title: format!("Edit {} ({n} changes)", short_path(text("file_path"), root)),
                detail: Detail::Nothing,
            }
        }
        "Bash" => PermissionSummary {
            title: input
                .get("description")
                .and_then(Value::as_str)
                .filter(|d| !d.is_empty())
                .map_or("Run a command".to_string(), |d| format!("Run a command: {d}")),
            detail: Detail::Code(cut_lines(text("command"))),
        },
        "WebFetch" => PermissionSummary { title: format!("Fetch {}", text("url")), detail: Detail::Nothing },
        "WebSearch" => PermissionSummary {
            title: format!("Search the web: {}", text("query")),
            detail: Detail::Nothing,
        },
        "Task" | "Agent" => PermissionSummary {
            title: format!(
                "Start a helper agent{}",
                Some(text("description")).filter(|d| !d.is_empty()).map_or(String::new(), |d| format!(": {d}"))
            ),
            detail: Detail::Nothing,
        },
        "TodoWrite" => PermissionSummary { title: "Update the task list".to_string(), detail: Detail::Nothing },
        "NotebookEdit" => PermissionSummary {
            title: format!("Edit notebook {}", short_path(text("notebook_path"), root)),
            detail: Detail::Nothing,
        },
        other => PermissionSummary {
            title: format!("Use {other}"),
            detail: fields_detail(input),
        },
    }
}

/// An input object as readable `name: value` rows (long values cut), not JSON.
fn fields_detail(input: &Value) -> Detail {
    let Some(object) = input.as_object().filter(|o| !o.is_empty()) else {
        return Detail::Nothing;
    };
    let fields = object
        .iter()
        .take(8)
        .map(|(key, value)| {
            let text = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let shown = if flat.chars().count() > 160 {
                format!("{}\u{2026}", flat.chars().take(160).collect::<String>())
            } else {
                flat
            };
            (key.replace('_', " "), shown)
        })
        .collect();
    Detail::Fields(fields)
}

// ------------------------------------------------------------------ questions

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QuestionOption {
    pub label: String,
    pub description: String,
}

/// One multiple-choice question from the agent's `AskUserQuestion`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Question {
    pub question: String,
    /// A short tag such as "Focus Areas".
    pub header: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
}

/// The questions in an `AskUserQuestion` input (empty if it has none).
pub(crate) fn parse_questions(input: &Value) -> Vec<Question> {
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    input
        .get("questions")
        .and_then(Value::as_array)
        .map(|qs| {
            qs.iter()
                .filter_map(|q| {
                    let question = text(q, "question");
                    let options: Vec<QuestionOption> = q
                        .get("options")
                        .and_then(Value::as_array)
                        .map(|os| {
                            os.iter()
                                .map(|o| QuestionOption { label: text(o, "label"), description: text(o, "description") })
                                .filter(|o| !o.label.is_empty())
                                .collect()
                        })
                        .unwrap_or_default();
                    (!question.is_empty() && !options.is_empty()).then(|| Question {
                        question,
                        header: text(q, "header"),
                        options,
                        multi_select: q.get("multiSelect").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What has been chosen for one question: option indexes, and optionally your own answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Choice {
    pub selected: Vec<usize>,
    /// "Other…" is chosen, with this text (may still be empty).
    pub other: Option<String>,
}

impl Choice {
    /// Pick option `ix`: replaces the choice for a single-answer question,
    /// toggles it for a multi-select one. Choosing an option clears "Other" for
    /// single-answer questions.
    pub(crate) fn pick(&mut self, ix: usize, multi: bool) {
        if multi {
            match self.selected.iter().position(|&i| i == ix) {
                Some(pos) => {
                    self.selected.remove(pos);
                }
                None => self.selected.push(ix),
            }
        } else {
            self.selected = vec![ix];
            self.other = None;
        }
    }

    /// Toggle "Other…". For single-answer questions it replaces the options.
    pub(crate) fn toggle_other(&mut self, multi: bool) {
        if self.other.is_some() {
            self.other = None;
        } else {
            self.other = Some(String::new());
            if !multi {
                self.selected.clear();
            }
        }
    }
}

/// The answer to one question as the CLI wants it: the chosen labels (in the
/// order the options were listed) and your own text, joined with ", ".
/// `None` while nothing is chosen.
pub(crate) fn answer_text(question: &Question, choice: &Choice) -> Option<String> {
    let mut picked: Vec<&str> = {
        let mut ixs = choice.selected.clone();
        ixs.sort_unstable();
        ixs.iter().filter_map(|&i| question.options.get(i)).map(|o| o.label.as_str()).collect()
    };
    let other = choice.other.as_deref().map(str::trim).filter(|t| !t.is_empty());
    if let Some(text) = other {
        picked.push(text);
    }
    (!picked.is_empty()).then(|| picked.join(", "))
}

/// Every question answered? Returns the `question -> answer` map, else `None`.
pub(crate) fn all_answers(questions: &[Question], choices: &[Choice]) -> Option<serde_json::Map<String, Value>> {
    let mut map = serde_json::Map::new();
    for (question, choice) in questions.iter().zip(choices) {
        map.insert(question.question.clone(), Value::String(answer_text(question, choice)?));
    }
    (map.len() == questions.len()).then_some(map)
}

/// The `control_response` that answers an `AskUserQuestion` request.
pub(crate) fn question_response(request: &PermissionRequest, answers: serde_json::Map<String, Value>) -> String {
    let mut input = request.input.clone();
    if let Some(object) = input.as_object_mut() {
        object.insert("answers".to_string(), Value::Object(answers));
    }
    serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request.request_id,
            "response": {"behavior": "allow", "updatedInput": input}
        }
    })
    .to_string()
}

/// Where a permission card stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PermissionState {
    Pending,
    Allowed,
    AllowedAlways,
    Denied,
    /// The run ended or was stopped before you answered.
    Cancelled,
}

impl DartIde {
    /// Answer a pending permission card and let the agent carry on.
    pub(crate) fn answer_permission(&mut self, item_ix: usize, decision: Decision, cx: &mut Context<Self>) {
        let Some(crate::rails::agent::AgentItem::Permission { request, state, .. }) =
            self.agent.items.get_mut(item_ix)
        else {
            return;
        };
        if *state != PermissionState::Pending {
            return;
        }
        let line = control_response(request, &decision);
        *state = match decision {
            Decision::Allow => PermissionState::Allowed,
            Decision::AllowAlways => PermissionState::AllowedAlways,
            Decision::Deny(_) => PermissionState::Denied,
        };
        if let Some(tx) = &self.agent.control_tx {
            let _ = tx.send_blocking(crate::rails::agent::AgentInput::Line(line));
        }
        self.agent.indicator.update(cx, |i, cx| i.set_label("Working", cx));
        cx.notify();
    }

    // ------------------------------------------------------------ questions

    fn question_state_mut(&mut self, item_ix: usize) -> Option<(&mut Vec<Choice>, &Vec<Question>, &mut PermissionState)> {
        match self.agent.items.get_mut(item_ix) {
            Some(crate::rails::agent::AgentItem::Question { questions, choices, state, .. }) => {
                Some((choices, &*questions, state))
            }
            _ => None,
        }
    }

    fn pick_question_option(&mut self, item_ix: usize, q: usize, opt: usize, cx: &mut Context<Self>) {
        if let Some((choices, questions, state)) = self.question_state_mut(item_ix) {
            if *state == PermissionState::Pending {
                if let (Some(choice), Some(question)) = (choices.get_mut(q), questions.get(q)) {
                    choice.pick(opt, question.multi_select);
                }
            }
        }
        cx.notify();
    }

    fn toggle_question_other(&mut self, item_ix: usize, q: usize, window: &mut Window, cx: &mut Context<Self>) {
        let mut turned_on = false;
        if let Some((choices, questions, state)) = self.question_state_mut(item_ix) {
            if *state == PermissionState::Pending {
                if let (Some(choice), Some(question)) = (choices.get_mut(q), questions.get(q)) {
                    choice.toggle_other(question.multi_select);
                    turned_on = choice.other.is_some();
                }
            }
        }
        if turned_on {
            let key = (item_ix, q);
            let input = self
                .agent
                .question_inputs
                .entry(key)
                .or_insert_with(|| cx.new(|cx| InputState::new(window, cx).placeholder("Type your own answer\u{2026}")))
                .clone();
            input.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    /// The choices with whatever has been typed into the "Other" boxes filled in.
    fn question_choices_with_text(&self, item_ix: usize, choices: &[Choice], cx: &App) -> Vec<Choice> {
        choices
            .iter()
            .enumerate()
            .map(|(q, choice)| {
                let mut choice = choice.clone();
                if choice.other.is_some() {
                    if let Some(input) = self.agent.question_inputs.get(&(item_ix, q)) {
                        choice.other = Some(input.read(cx).value().to_string());
                    }
                }
                choice
            })
            .collect()
    }

    pub(crate) fn submit_question(&mut self, item_ix: usize, cx: &mut Context<Self>) {
        let Some(crate::rails::agent::AgentItem::Question { request, questions, choices, state }) =
            self.agent.items.get(item_ix).cloned()
        else {
            return;
        };
        if state != PermissionState::Pending {
            return;
        }
        let filled = self.question_choices_with_text(item_ix, &choices, cx);
        let Some(answers) = all_answers(&questions, &filled) else { return };
        let line = question_response(&request, answers);
        if let Some(crate::rails::agent::AgentItem::Question { choices, state, .. }) = self.agent.items.get_mut(item_ix) {
            *choices = filled;
            *state = PermissionState::Allowed;
        }
        if let Some(tx) = &self.agent.control_tx {
            let _ = tx.send_blocking(crate::rails::agent::AgentInput::Line(line));
        }
        self.agent.indicator.update(cx, |i, cx| i.set_label("Working", cx));
        cx.notify();
    }

    /// Decline to answer; the agent carries on without it.
    pub(crate) fn skip_question(&mut self, item_ix: usize, cx: &mut Context<Self>) {
        let Some(crate::rails::agent::AgentItem::Question { request, state, .. }) = self.agent.items.get_mut(item_ix)
        else {
            return;
        };
        if *state != PermissionState::Pending {
            return;
        }
        let line = control_response(
            request,
            &Decision::Deny("The user chose not to answer. Carry on without it, or ask something different.".to_string()),
        );
        *state = PermissionState::Denied;
        if let Some(tx) = &self.agent.control_tx {
            let _ = tx.send_blocking(crate::rails::agent::AgentInput::Line(line));
        }
        self.agent.indicator.update(cx, |i, cx| i.set_label("Working", cx));
        cx.notify();
    }

    pub(crate) fn render_question_card(
        &self,
        item_ix: usize,
        questions: &[Question],
        choices: &[Choice],
        state: PermissionState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let border = cx.theme().border;
        let list_hover = cx.theme().list_hover;
        let pending = state == PermissionState::Pending;
        let filled = self.question_choices_with_text(item_ix, choices, cx);
        let can_submit = pending && all_answers(questions, &filled).is_some();

        let (status, status_color) = match state {
            PermissionState::Pending => ("Claude has a question", cx.theme().warning),
            PermissionState::Allowed | PermissionState::AllowedAlways => ("Answered", cx.theme().green),
            PermissionState::Denied => ("Skipped", muted),
            PermissionState::Cancelled => ("Not answered", muted),
        };

        let button = |id: &'static str, label: &'static str, primary_style: bool, enabled: bool| {
            div()
                .id(id)
                .h(px(28.))
                .px(px(12.))
                .flex()
                .items_center()
                .rounded(px(tokens::RADIUS_SM))
                .text_size(px(tokens::UI_SM))
                .border_1()
                .when(primary_style && enabled, |b| {
                    b.cursor_pointer().bg(primary).border_color(primary).text_color(cx.theme().primary_foreground)
                })
                .when(primary_style && !enabled, |b| b.border_color(border).text_color(muted.opacity(0.5)))
                .when(!primary_style, |b| {
                    b.cursor_pointer().border_color(border).text_color(fg).hover(move |h| h.bg(list_hover))
                })
                .child(label)
        };

        v_flex()
            .w_full()
            .gap(px(tokens::SPACE_3))
            .p(px(tokens::SPACE_4))
            .rounded(px(tokens::RADIUS_CARD))
            .bg(cx.theme().muted)
            .border_1()
            .border_color(if pending { primary.opacity(0.55) } else { border })
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(tokens::UI_XS)).text_color(status_color).font_medium().child(status)),
            )
            .children(questions.iter().enumerate().map(|(q, question)| {
                let choice = filled.get(q).cloned().unwrap_or_default();
                let answered = answer_text(question, &choice);
                let other_on = choice.other.is_some();
                let multi = question.multi_select;
                v_flex()
                    .w_full()
                    .gap(px(tokens::SPACE_2))
                    .child(
                        h_flex()
                            .gap(px(tokens::SPACE_2))
                            .items_center()
                            .when(!question.header.is_empty(), |row| {
                                row.child(
                                    div()
                                        .px(px(6.))
                                        .py(px(1.))
                                        .rounded(px(4.))
                                        .bg(tokens::accent_dim())
                                        .text_size(px(tokens::UI_XS))
                                        .text_color(primary)
                                        .child(question.header.clone()),
                                )
                            })
                            .when(multi && pending, |row| {
                                row.child(div().text_size(px(tokens::UI_XS)).text_color(muted).child("choose any"))
                            }),
                    )
                    .child(div().text_size(px(tokens::UI)).font_medium().text_color(fg).child(question.question.clone()))
                    .when(!pending, |col| {
                        col.child(
                            div()
                                .text_size(px(tokens::UI_SM))
                                .text_color(muted)
                                .child(answered.clone().unwrap_or_else(|| "\u{2014}".to_string())),
                        )
                    })
                    .when(pending, |col| {
                        col.children(question.options.iter().enumerate().map(|(opt, option)| {
                            let on = choice.selected.contains(&opt);
                            h_flex()
                                .id(("q-opt", item_ix * 1000 + q * 100 + opt))
                                .w_full()
                                .gap(px(tokens::SPACE_3))
                                .items_start()
                                .px(px(tokens::SPACE_3))
                                .py(px(6.))
                                .rounded(px(tokens::RADIUS_SM))
                                .border_1()
                                .cursor_pointer()
                                .when(on, |r| r.bg(tokens::accent_dim()).border_color(primary))
                                .when(!on, |r| r.border_color(border).hover(move |h| h.bg(list_hover)))
                                .child(
                                    div()
                                        .w(px(14.))
                                        .flex_shrink_0()
                                        .text_color(if on { primary } else { muted })
                                        .child(match (multi, on) {
                                            (false, true) => "\u{25c9}",
                                            (false, false) => "\u{25cb}",
                                            (true, true) => "\u{2611}",
                                            (true, false) => "\u{2610}",
                                        }),
                                )
                                .child(
                                    v_flex()
                                        .min_w(px(0.))
                                        .gap(px(1.))
                                        .child(div().text_size(px(tokens::UI_SM)).font_medium().text_color(fg).child(option.label.clone()))
                                        .when(!option.description.is_empty(), |c| {
                                            c.child(
                                                div()
                                                    .text_size(px(tokens::UI_XS))
                                                    .text_color(muted)
                                                    .child(option.description.clone()),
                                            )
                                        }),
                                )
                                .on_click(cx.listener(move |this, _e, _w, cx| this.pick_question_option(item_ix, q, opt, cx)))
                        }))
                        .child(
                            v_flex()
                                .w_full()
                                .gap(px(tokens::SPACE_2))
                                .child(
                                    h_flex()
                                        .id(("q-other", item_ix * 1000 + q * 100))
                                        .w_full()
                                        .gap(px(tokens::SPACE_3))
                                        .px(px(tokens::SPACE_3))
                                        .py(px(6.))
                                        .rounded(px(tokens::RADIUS_SM))
                                        .border_1()
                                        .cursor_pointer()
                                        .when(other_on, |r| r.bg(tokens::accent_dim()).border_color(primary))
                                        .when(!other_on, |r| r.border_color(border).hover(move |h| h.bg(list_hover)))
                                        .child(
                                            div().w(px(14.)).flex_shrink_0().text_color(if other_on { primary } else { muted }).child(
                                                match (multi, other_on) {
                                                    (false, true) => "\u{25c9}",
                                                    (false, false) => "\u{25cb}",
                                                    (true, true) => "\u{2611}",
                                                    (true, false) => "\u{2610}",
                                                },
                                            ),
                                        )
                                        .child(div().text_size(px(tokens::UI_SM)).text_color(fg).child("Other\u{2026}"))
                                        .on_click(cx.listener(move |this, _e, window, cx| {
                                            this.toggle_question_other(item_ix, q, window, cx)
                                        })),
                                )
                                .when_some(
                                    self.agent.question_inputs.get(&(item_ix, q)).filter(|_| other_on),
                                    |col, input| {
                                        col.child(
                                            h_flex()
                                                .w_full()
                                                .h(px(30.))
                                                .px(px(tokens::SPACE_3))
                                                .items_center()
                                                .rounded(px(tokens::RADIUS_SM))
                                                .bg(cx.theme().background)
                                                .border_1()
                                                .border_color(border)
                                                .child(Input::new(input).appearance(false).small()),
                                        )
                                    },
                                ),
                        )
                    })
            }))
            .when(pending, |card| {
                card.child(
                    h_flex()
                        .w_full()
                        .gap(px(tokens::SPACE_2))
                        .child(button("question-submit", "Submit answer", true, can_submit).on_click(cx.listener(
                            move |this, _e, _w, cx| this.submit_question(item_ix, cx),
                        )))
                        .child(button("question-skip", "Skip", false, true).on_click(cx.listener(
                            move |this, _e, _w, cx| this.skip_question(item_ix, cx),
                        ))),
                )
            })
            .into_any_element()
    }

    pub(crate) fn render_permission_card(
        &self,
        item_ix: usize,
        request: &PermissionRequest,
        summary: &PermissionSummary,
        state: PermissionState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let green = cx.theme().green;
        let red = cx.theme().red;
        let warning = cx.theme().warning;
        let border = cx.theme().border;
        let list_hover = cx.theme().list_hover;
        let mono = cx.theme().mono_font_family.clone();
        let pending = state == PermissionState::Pending;
        let always = always_label(&request.suggestions);

        let (status, status_color) = match state {
            PermissionState::Pending => ("Needs your approval", warning),
            PermissionState::Allowed => ("Allowed", green),
            PermissionState::AllowedAlways => ("Allowed for this session", green),
            PermissionState::Denied => ("Denied", red),
            PermissionState::Cancelled => ("Not answered", muted),
        };

        let code_block = |text: String, color: Option<Hsla>| {
            div()
                .w_full()
                .px(px(tokens::SPACE_3))
                .py(px(tokens::SPACE_2))
                .rounded(px(tokens::RADIUS_SM))
                .bg(cx.theme().background)
                .border_1()
                .border_color(border)
                .font_family(mono.clone())
                .text_size(px(tokens::UI_XS))
                .text_color(color.unwrap_or(fg))
                .child(text)
        };

        let button = |id: &'static str, label: String, primary: bool, danger: bool| {
            div()
                .id(id)
                .h(px(28.))
                .px(px(12.))
                .flex()
                .items_center()
                .rounded(px(tokens::RADIUS_SM))
                .cursor_pointer()
                .text_size(px(tokens::UI_SM))
                .border_1()
                .when(primary, |b| {
                    b.bg(cx.theme().primary).border_color(cx.theme().primary).text_color(cx.theme().primary_foreground)
                })
                .when(!primary, |b| {
                    b.border_color(border)
                        .text_color(if danger { red } else { fg })
                        .hover(move |h| h.bg(list_hover))
                })
                .child(label)
        };

        v_flex()
            .w_full()
            .gap(px(tokens::SPACE_3))
            .p(px(tokens::SPACE_4))
            .rounded(px(tokens::RADIUS_CARD))
            .bg(cx.theme().muted)
            .border_1()
            .border_color(if pending { warning.opacity(0.6) } else { border })
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap(px(tokens::SPACE_2))
                    .child(
                        div()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(tokens::UI))
                            .font_medium()
                            .text_color(fg)
                            .child(summary.title.clone()),
                    )
                    .child(div().flex_shrink_0().text_size(px(tokens::UI_XS)).text_color(status_color).child(status)),
            )
            .when(pending || state == PermissionState::Cancelled, |card| {
                match &summary.detail {
                    Detail::Nothing => card,
                    Detail::Code(code) => card.child(code_block(code.clone(), None)),
                    Detail::Fields(fields) => card.child(
                        v_flex().w_full().gap(px(2.)).children(fields.iter().map(|(name, value)| {
                            h_flex()
                                .w_full()
                                .gap(px(tokens::SPACE_2))
                                .items_start()
                                .text_size(px(tokens::UI_XS))
                                .child(div().w(px(92.)).flex_shrink_0().text_color(muted).child(name.clone()))
                                .child(div().min_w(px(0.)).flex_1().text_color(fg).child(value.clone()))
                        })),
                    ),
                    Detail::Diff { old, new } => card
                        .when(!old.is_empty(), |c| {
                            c.child(code_block(
                                old.lines().map(|l| format!("\u{2212} {l}")).collect::<Vec<_>>().join("\n"),
                                Some(red),
                            ))
                        })
                        .when(!new.is_empty(), |c| {
                            c.child(code_block(
                                new.lines().map(|l| format!("+ {l}")).collect::<Vec<_>>().join("\n"),
                                Some(green),
                            ))
                        }),
                }
            })
            .when(pending, |card| {
                card.child(
                    h_flex()
                        .w_full()
                        .flex_wrap()
                        .gap(px(tokens::SPACE_2))
                        .child(button("perm-allow", "Allow".to_string(), true, false).on_click(
                            cx.listener(move |this, _e, _w, cx| this.answer_permission(item_ix, Decision::Allow, cx)),
                        ))
                        .when_some(always, |row, label| {
                            row.child(button("perm-allow-always", label.to_string(), false, false).on_click(
                                cx.listener(move |this, _e, _w, cx| {
                                    this.answer_permission(item_ix, Decision::AllowAlways, cx)
                                }),
                            ))
                        })
                        .child(button("perm-deny", "Deny".to_string(), false, true).on_click(cx.listener(
                            move |this, _e, _w, cx| {
                                this.answer_permission(
                                    item_ix,
                                    Decision::Deny("The user denied this action. Do not retry it; ask what they would like instead.".to_string()),
                                    cx,
                                )
                            },
                        ))),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Captured from a real `claude -p --input-format stream-json --permission-prompt-tool stdio` run.
    const REAL_REQUEST: &str = r#"{"type": "control_request", "request_id": "968fe05b-16d3-433f-b911-ccb056900d19", "request": {"subtype": "can_use_tool", "tool_name": "Write", "display_name": "Write", "input": {"file_path": "/proj/hello.txt", "content": "hi"}, "description": "hello.txt", "permission_suggestions": [{"type": "setMode", "mode": "acceptEdits", "destination": "session"}], "tool_use_id": "toolu_01CPavq8Q1McPSzt1E3dPoL1"}}"#;

    fn request() -> PermissionRequest {
        parse_permission_request(&serde_json::from_str(REAL_REQUEST).unwrap()).unwrap()
    }

    #[core::prelude::v1::test]
    fn parses_a_real_permission_request() {
        let r = request();
        assert_eq!(r.request_id, "968fe05b-16d3-433f-b911-ccb056900d19");
        assert_eq!(r.tool_name, "Write");
        assert_eq!(r.input["content"], "hi");
        assert_eq!(always_label(&r.suggestions), Some("Allow edits this session"));
        // Other control traffic, and ordinary events, are not permission requests.
        assert!(parse_permission_request(&serde_json::json!({"type": "control_request", "request_id": "x", "request": {"subtype": "interrupt"}})).is_none());
        assert!(parse_permission_request(&serde_json::json!({"type": "assistant"})).is_none());
    }

    #[core::prelude::v1::test]
    fn answers_are_the_exact_shape_the_cli_accepted() {
        let r = request();
        let allow: Value = serde_json::from_str(&control_response(&r, &Decision::Allow)).unwrap();
        assert_eq!(allow["type"], "control_response");
        assert_eq!(allow["response"]["subtype"], "success");
        assert_eq!(allow["response"]["request_id"], "968fe05b-16d3-433f-b911-ccb056900d19");
        assert_eq!(allow["response"]["response"]["behavior"], "allow");
        assert_eq!(allow["response"]["response"]["updatedInput"]["file_path"], "/proj/hello.txt");
        assert!(allow["response"]["response"].get("updatedPermissions").is_none());

        let always: Value = serde_json::from_str(&control_response(&r, &Decision::AllowAlways)).unwrap();
        assert_eq!(always["response"]["response"]["updatedPermissions"][0]["mode"], "acceptEdits");

        let deny: Value = serde_json::from_str(&control_response(&r, &Decision::Deny("No.".into()))).unwrap();
        assert_eq!(deny["response"]["response"], serde_json::json!({"behavior": "deny", "message": "No."}));
    }

    #[core::prelude::v1::test]
    fn messages_and_the_end_of_a_turn() {
        let line: Value = serde_json::from_str(&user_message_line("Fix \"it\"\nnow")).unwrap();
        assert_eq!(line["message"]["content"], "Fix \"it\"\nnow", "quotes and newlines survive");
        assert!(is_result_line(r#"{"type":"result","result":"Done."}"#));
        assert!(!is_result_line(r#"{"type":"assistant"}"#));
        assert!(!is_result_line("not json"));
    }

    #[core::prelude::v1::test]
    fn summaries_say_what_will_happen() {
        let root = PathBuf::from("/proj");
        let req = |tool: &str, input: Value| PermissionRequest {
            request_id: "r".into(),
            tool_name: tool.into(),
            input,
            suggestions: vec![],
        };
        let edit = summarize_permission(&req("Edit", serde_json::json!({"file_path": "/proj/lib/a.dart", "old_string": "a\nb", "new_string": "c"})), &root);
        assert_eq!(edit.title, "Edit lib/a.dart");
        assert_eq!(edit.detail, Detail::Diff { old: "a\nb".into(), new: "c".into() });

        let write = summarize_permission(&request(), &PathBuf::from("/proj"));
        assert_eq!(write.title, "Write hello.txt");
        assert_eq!(write.detail, Detail::Code("hi".into()));

        let bash = summarize_permission(&req("Bash", serde_json::json!({"command": "cargo test", "description": "Run the tests"})), &root);
        assert_eq!(bash.title, "Run a command: Run the tests");
        assert_eq!(bash.detail, Detail::Code("cargo test".into()));

        let other = summarize_permission(&req("Mystery", serde_json::json!({"x": 1})), &root);
        assert_eq!(other.title, "Use Mystery");

        // A long command is cut, not dumped.
        let long = (0..40).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let big = summarize_permission(&req("Bash", serde_json::json!({"command": long})), &root);
        let Detail::Code(shown) = big.detail else { panic!("expected code") };
        assert!(shown.ends_with("more lines") && shown.lines().count() == MAX_DETAIL_LINES + 1, "{shown}");
    }

    /// Captured from a real `AskUserQuestion` request.
    const REAL_QUESTION: &str = r#"{"questions":[{"question":"Which color do you prefer?","header":"Color","options":[{"label":"Red","description":"Choose red."},{"label":"Blue","description":"Choose blue."}],"multiSelect":false}]}"#;

    fn questions() -> Vec<Question> {
        parse_questions(&serde_json::from_str(REAL_QUESTION).unwrap())
    }

    #[core::prelude::v1::test]
    fn parses_the_real_question_into_options() {
        let q = questions();
        assert_eq!(q.len(), 1);
        assert_eq!((q[0].header.as_str(), q[0].question.as_str()), ("Color", "Which color do you prefer?"));
        assert_eq!(q[0].options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), ["Red", "Blue"]);
        assert_eq!(q[0].options[1].description, "Choose blue.");
        assert!(!q[0].multi_select);
        // No usable questions: nothing to ask.
        assert!(parse_questions(&serde_json::json!({"questions": [{"question": "", "options": []}]})).is_empty());
        assert!(parse_questions(&serde_json::json!({})).is_empty());
    }

    #[core::prelude::v1::test]
    fn choices_pick_toggle_and_combine_into_answers() {
        let q = questions();
        let mut single = Choice::default();
        assert_eq!(answer_text(&q[0], &single), None, "nothing chosen yet");
        single.pick(0, false);
        single.pick(1, false);
        assert_eq!(answer_text(&q[0], &single).as_deref(), Some("Blue"), "single choice replaces");

        // "Other" replaces the options for a single-answer question, and carries your text.
        single.toggle_other(false);
        assert!(single.selected.is_empty());
        assert_eq!(answer_text(&q[0], &single), None, "an empty Other is not an answer");
        single.other = Some("  Green  ".into());
        assert_eq!(answer_text(&q[0], &single).as_deref(), Some("Green"));
        single.pick(0, false);
        assert_eq!(single.other, None, "choosing an option clears Other");

        // Multi-select toggles, and answers come out in the order the options are listed.
        let mut multi = Choice::default();
        multi.pick(1, true);
        multi.pick(0, true);
        assert_eq!(answer_text(&q[0], &multi).as_deref(), Some("Red, Blue"));
        multi.pick(1, true);
        assert_eq!(answer_text(&q[0], &multi).as_deref(), Some("Red"));
        multi.toggle_other(true);
        multi.other = Some("Teal".into());
        assert_eq!(answer_text(&q[0], &multi).as_deref(), Some("Red, Teal"), "Other adds to the options");
    }

    #[core::prelude::v1::test]
    fn the_answer_goes_back_the_way_the_cli_accepted_it() {
        let q = questions();
        let request = PermissionRequest {
            request_id: "req-1".into(),
            tool_name: "AskUserQuestion".into(),
            input: serde_json::from_str(REAL_QUESTION).unwrap(),
            suggestions: vec![],
        };
        // Not every question answered: nothing to send.
        assert!(all_answers(&q, &[Choice::default()]).is_none());

        let choice = Choice { selected: vec![1], other: None };
        let answers = all_answers(&q, &[choice]).unwrap();
        let line: Value = serde_json::from_str(&question_response(&request, answers)).unwrap();
        let response = &line["response"]["response"];
        assert_eq!(line["response"]["request_id"], "req-1");
        assert_eq!(response["behavior"], "allow");
        assert_eq!(response["updatedInput"]["answers"]["Which color do you prefer?"], "Blue");
        assert_eq!(response["updatedInput"]["questions"][0]["header"], "Color", "the questions are echoed back");
    }

    #[core::prelude::v1::test]
    fn unknown_tools_show_readable_fields_not_json() {
        let root = PathBuf::from("/proj");
        let request = PermissionRequest {
            request_id: "r".into(),
            tool_name: "Mystery".into(),
            input: serde_json::json!({"target_file": "a.dart", "options": {"deep": true}, "note": "x".repeat(300)}),
            suggestions: vec![],
        };
        let summary = summarize_permission(&request, &root);
        let Detail::Fields(fields) = summary.detail else { panic!("expected fields") };
        let names: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains(&"target file"), "underscores become spaces: {names:?}");
        assert!(fields.iter().all(|(_, v)| !v.starts_with('{') || v.contains("deep")));
        assert!(fields.iter().any(|(_, v)| v.ends_with('\u{2026}')), "long values are cut");

        let search = |tool: &str, input: Value| summarize_permission(&PermissionRequest { request_id: "r".into(), tool_name: tool.into(), input, suggestions: vec![] }, &root).title;
        assert_eq!(search("WebSearch", serde_json::json!({"query": "flutter slivers"})), "Search the web: flutter slivers");
        assert_eq!(search("Task", serde_json::json!({"description": "find usages"})), "Start a helper agent: find usages");
        assert_eq!(search("TodoWrite", serde_json::json!({})), "Update the task list");
    }
}
