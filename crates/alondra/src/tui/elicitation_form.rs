//! The form an MCP server asks the user to fill, drawn over its card's buttons: one row per field
//! of the card's `form`, each edited as its kind says, and sent as typed `content` with the option
//! that accepts the form. The values are checked against the form before they leave.

use alondra_core::requested_form::FormProblem;
use alondra_core::views::{
    Action, RequestedChoice, RequestedField, RequestedFieldKind, RequestedForm, TextFormat,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::{Map, Number, Value};

use super::{App, Flow, Key};

pub struct ElicitationForm {
    pub card: String,
    form: RequestedForm,
    values: Map<String, Value>,
    action: Action,
    selected: Key,
    epoch: String,
    cursor: usize,
    /// What is being typed in the text or number row under the cursor.
    editing: Option<String>,
    problem: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Row {
    Field(usize),
    Option(usize, usize),
    Send,
}

enum Pressed {
    Stay,
    Close,
    Send(Map<String, Value>),
}

fn label(field: &RequestedField) -> String {
    field.title.clone().unwrap_or_else(|| field.name.clone())
}

fn choice_label(choice: &RequestedChoice) -> String {
    choice.title.clone().unwrap_or_else(|| choice.value.clone())
}

fn initial(kind: &RequestedFieldKind) -> Option<Value> {
    match kind {
        RequestedFieldKind::Text { default, .. } => default.clone().map(Value::String),
        RequestedFieldKind::Number { default, .. } => default.clone().map(Value::Number),
        RequestedFieldKind::Boolean { default } => Some(Value::Bool(default.unwrap_or(false))),
        RequestedFieldKind::Choice { default, .. } => default.clone().map(Value::String),
        RequestedFieldKind::Choices { default, .. } => Some(Value::Array(default.iter().cloned().map(Value::String).collect())),
    }
}

fn shown(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn limits(min: Option<String>, max: Option<String>) -> Option<String> {
    match (min, max) {
        (Some(min), Some(max)) => Some(trf!("from {min} to {max}", "de {min} a {max}")),
        (Some(min), None) => Some(trf!("at least {min}", "como mínimo {min}")),
        (None, Some(max)) => Some(trf!("at most {max}", "como máximo {max}")),
        (None, None) => None,
    }
}

/// What the field takes, beside its value: its format and its limits.
fn hint(kind: &RequestedFieldKind) -> String {
    let mut parts: Vec<String> = Vec::new();
    match kind {
        RequestedFieldKind::Text { format, min_length, max_length, .. } => {
            parts.extend(format.map(|format| match format {
                TextFormat::Email => tr!("email", "correo").to_owned(),
                TextFormat::Uri => tr!("link", "enlace").to_owned(),
                TextFormat::Date => tr!("date, YYYY-MM-DD", "fecha, AAAA-MM-DD").to_owned(),
                TextFormat::DateTime => tr!("date and time", "fecha y hora").to_owned(),
            }));
            parts.extend(limits(min_length.map(|n| n.to_string()), max_length.map(|n| n.to_string()))
                .map(|range| trf!("{range} characters", "{range} caracteres")));
        }
        RequestedFieldKind::Number { integer, minimum, maximum, .. } => {
            parts.push(if *integer { tr!("whole number", "número entero") } else { tr!("number", "número") }.to_owned());
            parts.extend(limits(minimum.as_ref().map(Number::to_string), maximum.as_ref().map(Number::to_string)));
        }
        RequestedFieldKind::Choice { .. } => parts.push(tr!("⏎ next option", "⏎ siguiente opción").to_owned()),
        RequestedFieldKind::Choices { min_items, max_items, .. } => {
            parts.extend(limits(min_items.map(|n| n.to_string()), max_items.map(|n| n.to_string()))
                .map(|range| trf!("pick {range}", "elige {range}")));
        }
        RequestedFieldKind::Boolean { .. } => {}
    }
    parts.join(" · ")
}

impl ElicitationForm {
    pub fn new(card: String, form: RequestedForm, action: Action, selected: Key, epoch: String) -> Self {
        let values = form.fields.iter()
            .filter_map(|field| Some((field.name.clone(), initial(&field.kind)?)))
            .collect();
        Self { card, form, values, action, selected, epoch, cursor: 0, editing: None, problem: None }
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (index, field) in self.form.fields.iter().enumerate() {
            rows.push(Row::Field(index));
            if let RequestedFieldKind::Choices { options, .. } = &field.kind {
                rows.extend((0..options.len()).map(|option| Row::Option(index, option)));
            }
        }
        rows.push(Row::Send);
        rows
    }

    fn row(&self) -> Row {
        let rows = self.rows();
        rows[self.cursor.min(rows.len() - 1)]
    }

    /// The values the form leaves: an empty list is left out unless the field needs one.
    fn content(&self) -> Map<String, Value> {
        self.values.iter()
            .filter(|(name, value)| {
                !matches!(value, Value::Array(items) if items.is_empty())
                    || self.form.fields.iter().any(|field| &field.name == *name && field.required)
            })
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect()
    }

    fn field_label(&self, name: &str) -> String {
        self.form.fields.iter().find(|field| field.name == name).map_or_else(|| name.to_owned(), label)
    }

    fn problem_text(&self, problem: &FormProblem) -> String {
        match problem {
            FormProblem::TooLarge => tr!("The values are too long to send", "Los valores son demasiado largos para enviarlos").into(),
            FormProblem::UnknownField(name) | FormProblem::WrongType(name) => {
                let field = self.field_label(name);
                trf!("{field} has a value of the wrong kind", "{field} tiene un valor de otro tipo")
            }
            FormProblem::Missing(name) => {
                let field = self.field_label(name);
                trf!("Fill in {field}", "Rellena {field}")
            }
            FormProblem::OutOfRange(name) => {
                let field = self.field_label(name);
                trf!("{field} is outside its limits", "{field} está fuera de sus límites")
            }
            FormProblem::NotAnOption(name) => {
                let field = self.field_label(name);
                trf!("{field} takes only its options", "{field} solo admite sus opciones")
            }
        }
    }

    /// The typed text becomes the field's value, as the field's kind reads it.
    fn commit(&mut self, field: usize, typed: &str) -> bool {
        let field = &self.form.fields[field];
        let typed = typed.trim();
        if typed.is_empty() {
            self.values.remove(&field.name);
            return true;
        }
        let value = match &field.kind {
            RequestedFieldKind::Number { integer: true, .. } => typed.parse::<i64>().ok().map(Number::from),
            RequestedFieldKind::Number { .. } => typed.parse::<f64>().ok().and_then(Number::from_f64),
            _ => {
                self.values.insert(field.name.clone(), Value::String(typed.to_owned()));
                return true;
            }
        };
        match value {
            Some(number) => {
                self.values.insert(field.name.clone(), Value::Number(number));
                true
            }
            None => {
                let name = label(field);
                self.problem = Some(match field.kind {
                    RequestedFieldKind::Number { integer: true, .. } => trf!("{name} takes a whole number", "{name} admite un número entero"),
                    _ => trf!("{name} takes a number", "{name} admite un número"),
                });
                false
            }
        }
    }

    fn press_row(&mut self) -> Pressed {
        match self.row() {
            Row::Send => match self.form.check(&self.content()) {
                Ok(()) => return Pressed::Send(self.content()),
                Err(problem) => self.problem = Some(self.problem_text(&problem)),
            },
            Row::Option(field, option) => {
                let (name, kind) = (&self.form.fields[field].name, &self.form.fields[field].kind);
                if let RequestedFieldKind::Choices { options, .. } = kind {
                    let value = Value::String(options[option].value.clone());
                    let picked = self.values.entry(name.clone()).or_insert_with(|| Value::Array(vec![]));
                    if let Value::Array(items) = picked {
                        match items.iter().position(|item| *item == value) {
                            Some(at) => { items.remove(at); }
                            None => items.push(value),
                        }
                    }
                }
            }
            Row::Field(field) => {
                let (name, kind, required) = (self.form.fields[field].name.clone(), self.form.fields[field].kind.clone(), self.form.fields[field].required);
                match kind {
                    RequestedFieldKind::Boolean { .. } => {
                        let on = self.values.get(&name).and_then(Value::as_bool).unwrap_or(false);
                        self.values.insert(name, Value::Bool(!on));
                    }
                    RequestedFieldKind::Choice { options, .. } => {
                        let mut cycle: Vec<Option<&str>> = options.iter().map(|option| Some(option.value.as_str())).collect();
                        if !required { cycle.insert(0, None); }
                        let now = self.values.get(&name).and_then(Value::as_str);
                        let next = cycle.iter().position(|value| *value == now).map_or(0, |at| (at + 1) % cycle.len());
                        match cycle.get(next).copied().flatten() {
                            Some(value) => { self.values.insert(name, Value::String(value.to_owned())); }
                            None => { self.values.remove(&name); }
                        }
                    }
                    RequestedFieldKind::Text { .. } | RequestedFieldKind::Number { .. } => {
                        self.editing = Some(self.values.get(&name).map(shown).unwrap_or_default());
                    }
                    RequestedFieldKind::Choices { .. } => self.cursor += 1,
                }
            }
        }
        Pressed::Stay
    }

    fn key(&mut self, key: KeyEvent) -> Pressed {
        if let Some(typed) = self.editing.as_mut() {
            match key.code {
                KeyCode::Esc => self.editing = None,
                KeyCode::Backspace => { typed.pop(); }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => typed.push(character),
                KeyCode::Enter => {
                    let typed = typed.clone();
                    if let Row::Field(field) = self.row()
                        && self.commit(field, &typed)
                    {
                        self.editing = None;
                    }
                }
                _ => {}
            }
            return Pressed::Stay;
        }
        self.problem = None;
        let last = self.rows().len() - 1;
        match key.code {
            KeyCode::Esc => return Pressed::Close,
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => self.cursor = (self.cursor + 1).min(last),
            KeyCode::Enter | KeyCode::Char(' ') => return self.press_row(),
            _ => {}
        }
        Pressed::Stay
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(typed) = self.editing.as_mut() {
            typed.push_str(&super::md::one_line(text));
        }
    }

    pub fn lines(&self) -> Vec<Line<'static>> {
        let dim = Style::new().fg(super::draw::DIM);
        let focused = Style::new().fg(Color::Black).bg(super::draw::ACCENT);
        let width = self.form.fields.iter().map(|field| label(field).chars().count() + if field.required { 4 } else { 2 }).max().unwrap_or(0);
        let mut lines = vec![Line::from(Span::styled(
            tr!("The server asks you to fill in:", "El servidor te pide rellenar:"),
            Style::new().add_modifier(Modifier::BOLD),
        ))];
        let at = self.row();
        for row in self.rows() {
            let here = row == at;
            let marker = Span::styled(if here { "▸ " } else { "  " }, Style::new().fg(super::draw::ACCENT));
            match row {
                Row::Field(index) => {
                    let field = &self.form.fields[index];
                    let name = format!("{}{}", label(field), if field.required { " *" } else { "" });
                    let value = self.values.get(&field.name);
                    let text = match (&field.kind, value) {
                        _ if here && self.editing.is_some() => format!("{}▏", self.editing.as_deref().unwrap_or_default()),
                        (RequestedFieldKind::Boolean { .. }, value) => {
                            if value.and_then(Value::as_bool).unwrap_or(false) { tr!("[x] yes", "[x] sí") } else { tr!("[ ] no", "[ ] no") }.to_owned()
                        }
                        (RequestedFieldKind::Choice { options, .. }, Some(Value::String(picked))) => options.iter()
                            .find(|option| &option.value == picked).map_or_else(|| picked.clone(), choice_label),
                        (RequestedFieldKind::Choices { .. }, _) => String::new(),
                        (_, Some(value)) => shown(value),
                        (_, None) => "—".to_owned(),
                    };
                    let mut spans = vec![marker, Span::styled(format!("{name:<width$}"), dim), Span::styled(text, if here { focused } else { Style::new() })];
                    let hint = hint(&field.kind);
                    if !hint.is_empty() {
                        spans.push(Span::styled(format!("  {hint}"), dim));
                    }
                    lines.push(Line::from(spans));
                    if let Some(description) = &field.description {
                        lines.push(Line::from(Span::styled(format!("  {:width$}{description}", ""), dim)));
                    }
                }
                Row::Option(index, option) => {
                    let field = &self.form.fields[index];
                    let RequestedFieldKind::Choices { options, .. } = &field.kind else { continue };
                    let on = self.values.get(&field.name).and_then(Value::as_array)
                        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(options[option].value.as_str())));
                    let text = format!("{} {}", if on { "[x]" } else { "[ ]" }, choice_label(&options[option]));
                    lines.push(Line::from(vec![marker, Span::raw(format!("{:width$}", "")), Span::styled(text, if here { focused } else { Style::new() })]));
                }
                Row::Send => {
                    lines.push(Line::default());
                    let send = format!(" {} ", self.action.label);
                    lines.push(Line::from(vec![
                        marker,
                        Span::styled(send, if here { focused.add_modifier(Modifier::BOLD) } else { Style::new().fg(Color::Black).bg(Color::Green) }),
                        Span::styled(format!("  {}", tr!("* required", "* obligatorio")), dim),
                    ]));
                }
            }
        }
        if let Some(problem) = &self.problem {
            lines.push(Line::from(Span::styled(format!("⚠ {problem}"), Style::new().fg(Color::Red))));
        }
        lines
    }

    pub fn editing(&self) -> bool {
        self.editing.is_some()
    }
}

impl App {
    /// Accepting a card whose server sent a form opens the form instead of answering at once.
    pub(super) fn open_elicitation_form(&mut self, card: &alondra_core::views::Card, action: &Action, selected: &Key, epoch: &str) -> bool {
        let accepts = action.enabled && action.option_id.as_deref() == Some(alondra_core::domain::Hold::ACCEPT);
        let Some(form) = card.form.as_ref().filter(|form| accepts && !form.fields.is_empty()) else { return false };
        self.elicitation = Some(ElicitationForm::new(card.id.clone(), form.clone(), action.clone(), selected.clone(), epoch.to_owned()));
        true
    }

    pub(super) fn elicitation_key(&mut self, key: KeyEvent) -> Flow {
        let Some(form) = self.elicitation.as_mut() else { return Flow::Go };
        match form.key(key) {
            Pressed::Stay => {}
            Pressed::Close => self.elicitation = None,
            Pressed::Send(content) => {
                let Some(form) = self.elicitation.take() else { return Flow::Go };
                let acting = super::Acting {
                    origin: form.selected.clone(),
                    stamp: form.action.stamp.clone(),
                    hides: form.action.resolves_card,
                    label: form.action.label.clone(),
                };
                let request = crate::client::Client::action_body(
                    &form.epoch,
                    &form.action.id,
                    form.action.option_id.as_deref(),
                    serde_json::json!({ "content": content }),
                );
                self.submit_view_request(request, Some(acting));
            }
        }
        Flow::Go
    }
}
