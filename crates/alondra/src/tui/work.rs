//! The forms of "New" and of the schedules: one row per field the server sends ("Label: value"),
//! each opening a short list of its values or a line to type. Which fields exist, their labels
//! and their values all come from the server's form; nothing here knows what a project, an
//! option or a repeat is.

use std::collections::BTreeMap;

use alondra_core::issues::Issue;
use alondra_core::views::{ChoiceItem, Field, FieldKind, Item, View};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};

#[derive(Default)]
pub struct OptionsForm {
    /// The view the fields come from, without its project.
    pub view: String,
    pub fields: Vec<Field>,
    pub values: BTreeMap<String, Value>,
    retained_labels: BTreeMap<String, String>,
    pub submit: Option<Submit>,
    pub replacement: Option<Submit>,
    /// The row under the cursor, or `None` while typing the text.
    pub focus: Option<usize>,
    pub error: Option<String>,
    pub loaded: bool,
    pub popup: Option<Popup>,
}

#[derive(Clone)]
pub struct Submit {
    pub action_id: String,
    pub label: String,
    pub data_epoch: String,
    pub attachments: bool,
    pub enabled: bool,
    pub disabled_reason: Option<String>,
}

pub enum Popup {
    Values { field: usize, sel: usize },
    Filter { field: usize, query: String, sel: usize },
    Issues { field: usize, picker: IssuePicker },
    Text { field: usize, input: String },
}

/// The open issues, filtered as the user types; the list comes once from alondrad's cache.
#[derive(Default)]
pub struct IssuePicker {
    pub all: Vec<Issue>,
    pub query: String,
    pub sel: usize,
    pub loading: bool,
    pub error: Option<String>,
}

impl IssuePicker {
    pub fn visible(&self) -> Vec<&Issue> {
        self.all.iter().filter(|i| i.matches(&self.query)).collect()
    }

    /// What was typed when it is an issue link or `owner/repo#number` that is not listed.
    pub fn typed_reference(&self) -> Option<String> {
        let (repo, number) = alondra_core::issues::parse_ref(&self.query)?;
        Some(format!("{repo}#{number}"))
    }
}

pub enum FormKey {
    Handled,
    /// The field that reloads the form changed: fetch the form for this value.
    Reload(String),
    /// Fetch the open issues; `true` asks alondrad to search GitHub again.
    LoadIssues(bool),
    NotMine,
}

pub fn field_visible(field: &Field, values: &BTreeMap<String, Value>) -> bool {
    let Some(condition) = &field.visible_when else {
        return true;
    };
    let actual = values.get(&condition.field);
    condition.equals.as_ref().is_some_and(|expected| actual == Some(expected))
        || actual.is_some_and(|value| condition.any_of.contains(value))
}

fn is_primary(field: &Field) -> bool {
    matches!(field.kind, FieldKind::Multiline { .. })
}

fn is_row(field: &Field) -> bool {
    !matches!(field.kind, FieldKind::Attachments)
}

fn display(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

impl OptionsForm {
    pub fn new(view: &str) -> Self {
        Self {
            view: view.to_owned(),
            ..Self::default()
        }
    }

    /// The field whose text is the composer's: the first long one.
    pub fn primary(&self) -> Option<&Field> {
        self.fields.iter().find(|field| is_primary(field))
    }

    pub fn attachments_field(&self) -> Option<&Field> {
        self.fields
            .iter()
            .find(|field| matches!(field.kind, FieldKind::Attachments))
    }

    /// Indexes of the fields shown as rows: the one that reloads the form first, then pickers,
    /// then the rest in the server's order.
    fn rows(&self) -> Vec<usize> {
        let rank = |field: &Field| match field.kind {
            _ if field.reload_on_change => 0,
            FieldKind::Picker { .. } => 1,
            _ => 2,
        };
        let primary = self.fields.iter().position(is_primary);
        let mut rows: Vec<usize> = self
            .fields
            .iter()
            .enumerate()
            .filter(|(index, field)| {
                Some(*index) != primary && is_row(field) && field_visible(field, &self.values)
            })
            .map(|(index, _)| index)
            .collect();
        rows.sort_by_key(|index| rank(&self.fields[*index]));
        rows
    }

    fn reloading_field(&self) -> Option<usize> {
        self.fields
            .iter()
            .position(|field| field.reload_on_change && matches!(field.kind, FieldKind::Choice { .. }))
    }

    pub fn loaded(&mut self, view: &View) {
        let Some(form) = view.sections.iter().flat_map(|s| &s.items).find_map(|item| match item {
            Item::Form(form) => Some(form),
            _ => None,
        }) else {
            self.submit = None;
            self.error = Some(view.title.clone().unwrap_or_default());
            self.loaded = true;
            return;
        };
        if matches!(form.id.as_str(), "model_profile" | "task_selection") && let Some(original) = &mut self.submit {
            if original.data_epoch != view.data_epoch {
                original.enabled = false; original.disabled_reason = Some(tr!("The Mac's data changed. Close this form and open it again.", "Los datos del Mac cambiaron. Cierra este formulario y ábrelo de nuevo.").into());
                self.error = original.disabled_reason.clone(); self.replacement = None; return;
            }
            if original.action_id != form.submit.id {
                self.replacement = Some(Submit { action_id: form.submit.id.clone(), label: form.submit.label.clone(), data_epoch: view.data_epoch.clone(), attachments: form.submit.attachments, enabled: form.submit.enabled, disabled_reason: form.submit.disabled_reason.clone() });
                self.error = Some(tr!("The selection changed. Ctrl+R keeps this draft against the current version; Esc cancels.", "La selección cambió. Ctrl+R conserva este borrador con la versión actual; Esc cancela.").into()); return;
            }
        }
        for id in ["model", "account", "reasoning_effort"] {
            let label = self.fields.iter().find(|field| field.id == id).and_then(|field| {
                let FieldKind::Choice { items } = &field.kind else { return None; };
                let item = items.iter().find(|item| self.values.get(id).and_then(Value::as_str) == Some(item.id.as_str()))?;
                Some(item.group.as_ref().map_or_else(|| item.label.clone(), |group| format!("{group} · {}", item.label)))
            });
            if let Some(label) = label { self.retained_labels.insert(id.into(), label); }
        }
        let previous = std::mem::take(&mut self.values);
        self.values = form
            .fields
            .iter()
            .filter_map(|field| {
                let kept = previous.get(&field.id).filter(|value| keeps(&field.kind, value)
                    || model_field(&field.id) && value.is_string());
                let value = kept.unwrap_or(&field.value);
                (!value.is_null()).then(|| (field.id.clone(), value.clone()))
            })
            .collect();
        if modern_intent(&previous) {
            for (id, value) in previous.iter().filter(|(id, _)| model_field(id)) { self.values.entry(id.clone()).or_insert_with(|| value.clone()); }
        } else if !form.fields.iter().any(|field| field.id == "model") {
            if let Some(field) = form.fields.iter().find(|field| field.id == "account") && self.values.get("account").and_then(Value::as_str) == Some("auto") { self.values.insert("account".into(), field.value.clone()); }
        }
        if let Some(value) = previous.get("effort").filter(|value| value.as_str().is_some_and(|value| !value.is_empty() && value != "auto")) { self.values.entry("effort".into()).or_insert_with(|| value.clone()); }
        if let Some(edited) = form.substitution_was_edited { self.values.entry("substitution_was_edited".into()).or_insert(json!(edited)); }
        if let Some(value) = previous.get("substitution_was_edited") { self.values.insert("substitution_was_edited".into(), value.clone()); }
        let retained: Vec<_> = self.fields.iter().filter(|field| model_field(&field.id) && !form.fields.iter().any(|new| new.id == field.id)).cloned().collect();
        self.fields = form.fields.clone();
        if form.id == "model_profile" { self.fields.extend(retained); }
        self.submit = Some(Submit {
            action_id: form.submit.id.clone(),
            label: form.submit.label.clone(),
            data_epoch: view.data_epoch.clone(),
            attachments: form.submit.attachments,
            enabled: form.submit.enabled,
            disabled_reason: form.submit.disabled_reason.clone(),
        });
        self.error = None;
        self.loaded = true;
        if let Some(at) = self.focus {
            self.focus = Some(at.min(self.rows().len().saturating_sub(1)));
        }
    }

    /// The value of the field that reloads the form, to ask for its fields.
    pub fn reload_value(&self) -> Option<String> {
        let field = &self.fields[self.reloading_field()?];
        Some(self.values.get(&field.id).map(display).unwrap_or_default())
    }

    pub fn failed(&mut self, error: String) {
        self.error = Some(error);
        self.loaded = true;
    }

    pub fn issues_loaded(&mut self, result: Result<(Vec<Issue>, Option<String>), String>) {
        let Some(Popup::Issues { picker, .. }) = self.popup.as_mut() else {
            return;
        };
        picker.loading = false;
        match result {
            Ok((all, error)) => {
                picker.all = all;
                picker.error = error;
                picker.sel = picker.sel.min(picker.visible().len().saturating_sub(1));
            }
            Err(e) => picker.error = Some(e),
        }
    }

    pub fn choices(&self, field: usize) -> &[ChoiceItem] {
        match &self.fields[field].kind {
            FieldKind::Choice { items } => items,
            _ => &[],
        }
    }

    /// The choices a typed filter leaves, in the server's order.
    pub fn filtered(&self, field: usize, query: &str) -> Vec<&ChoiceItem> {
        let query = query.trim().to_lowercase();
        self.choices(field)
            .iter()
            .filter(|item| query.is_empty() || [item.label.as_str(), item.group.as_deref().unwrap_or_default(), item.description.as_deref().unwrap_or_default()].join(" ").to_lowercase().contains(&query))
            .collect()
    }

    fn set(&mut self, field: usize, value: Value) {
        let id = self.fields[field].id.clone();
        let changed = self.values.get(&id) != Some(&value);
        if changed && id == "model" {
            if self.fields.iter().any(|field| field.id == "reasoning_effort") { self.values.insert("reasoning_effort".into(), json!("auto")); }
            if self.values.get("substitution_was_edited") != Some(&json!(true)) && self.fields.iter().any(|field| field.id == "allow_model_substitution") {
                self.values.insert("allow_model_substitution".into(), json!(value.as_str() == Some("auto")));
                self.values.insert("substitution_was_edited".into(), json!(false));
            }
        }
        if changed && let Some(position) = id.strip_prefix("candidate_") { self.values.insert(format!("reasoning_effort_{position}"), json!("auto")); }
        if changed && id == "account" && self.fields.iter().any(|field| field.id == "pin_account") {
            self.values.insert("pin_account".into(), json!(value.as_str() != Some("auto")));
            self.values.insert("account_was_edited".into(), json!(true));
        }
        if changed && id == "pin_account" && value == json!(true) && self.values.get("account").and_then(Value::as_str).is_some_and(|account| account != "auto") { self.values.insert("account_was_edited".into(), json!(true)); }
        if id == "allow_model_substitution" { self.values.insert("substitution_was_edited".into(), json!(true)); }
        if value.is_null() {
            self.values.remove(&id);
        } else {
            self.values.insert(id, value);
        }
        let rows = self.rows().len();
        if let Some(at) = self.focus {
            self.focus = Some(at.min(rows.saturating_sub(1)));
        }
    }

    fn value_label(&self, field: &Field) -> String {
        let value = self.values.get(&field.id).cloned().unwrap_or(Value::Null);
        match &field.kind {
            FieldKind::Choice { items } => items
                .iter()
                .find(|item| value.as_str() == Some(item.id.as_str()))
                .map(|item| self.choice_label(item))
                .unwrap_or_else(|| {
                    let old = self.retained_labels.get(&field.id).cloned().unwrap_or_else(|| display(&value));
                    if matches!(field.id.as_str(), "model" | "account" | "reasoning_effort") { trf!("{old} · unavailable in the current catalog", "{old} · no disponible en el catálogo actual") }
                    else { old }
                }),
            FieldKind::Toggle => match value.as_bool() {
                Some(true) => tr!("yes", "sí").into(),
                _ => tr!("no", "no").into(),
            },
            FieldKind::Lines { .. } => display(&value).lines().collect::<Vec<_>>().join(", "),
            _ => display(&value),
        }
    }

    /// `(label, value, focused)` for each row.
    pub fn rows_shown(&self) -> Vec<(String, String, bool)> {
        let mut rows: Vec<_> = self.rows()
            .into_iter()
            .enumerate()
            .map(|(at, index)| {
                let field = &self.fields[index];
                let shown = self.value_label(field);
                let shown = if shown.is_empty() { tr!("none", "ninguno").to_owned() } else { shown };
                (field.label.clone(), shown, self.focus == Some(at))
            })
            .collect();
        for (id, label) in [("model", tr!("Model", "Modelo")), ("account", tr!("Account", "Cuenta")), ("reasoning_effort", tr!("Reasoning effort", "Esfuerzo de razonamiento"))] {
            if !self.fields.iter().any(|field| field.id == id) && let Some(value) = self.values.get(id).and_then(Value::as_str).filter(|value| !value.is_empty() && *value != "auto") {
                let name = self.retained_labels.get(id).map(String::as_str).unwrap_or(value);
                rows.push((label.into(), format!("{name} · {}", tr!("Unavailable", "No disponible")), false));
            }
        }
        if !self.fields.iter().any(|field| field.id == "effort") && self.fields.iter().any(|field| field.id == "model")
            && self.values.get("effort").and_then(Value::as_str).is_some_and(|value| !value.is_empty() && value != "auto") {
            rows.push((tr!("Previous preset", "Ajuste anterior").into(), tr!("Review Model and Reasoning effort; the previous preset does not choose a native model", "Revisa Modelo y Esfuerzo de razonamiento; el ajuste anterior no elige un modelo nativo").into(), false));
        }
        rows
    }

    pub fn popup_title(&self) -> String {
        match &self.popup {
            Some(
                Popup::Values { field, .. }
                | Popup::Filter { field, .. }
                | Popup::Issues { field, .. }
                | Popup::Text { field, .. },
            ) => self.fields[*field].label.clone(),
            None => String::new(),
        }
    }

    fn open(&mut self, at: usize) -> FormKey {
        let Some(&field) = self.rows().get(at) else {
            return FormKey::Handled;
        };
        let current = self.values.get(&self.fields[field].id).cloned().unwrap_or(Value::Null);
        match self.fields[field].kind.clone() {
            FieldKind::Choice { items } => {
                if self.fields[field].reload_on_change || matches!(self.fields[field].id.as_str(), "model" | "account") {
                    self.popup = Some(Popup::Filter { field, query: String::new(), sel: 0 });
                } else {
                    let sel = items
                        .iter()
                        .position(|item| current.as_str() == Some(item.id.as_str()))
                        .unwrap_or(0);
                    self.popup = Some(Popup::Values { field, sel });
                }
            }
            FieldKind::Picker { .. } => {
                self.popup = Some(Popup::Issues {
                    field,
                    picker: IssuePicker { loading: true, ..Default::default() },
                });
                return FormKey::LoadIssues(false);
            }
            FieldKind::Toggle => {
                let on = current.as_bool().unwrap_or(false);
                self.set(field, json!(!on));
            }
            FieldKind::Attachments | FieldKind::Unsupported => {}
            FieldKind::Text { .. }
            | FieldKind::Multiline { .. }
            | FieldKind::Date
            | FieldKind::Time
            | FieldKind::Stepper { .. }
            | FieldKind::Lines { .. } => {
                let input = match self.fields[field].kind {
                    FieldKind::Lines { .. } => self.value_label(&self.fields[field]),
                    _ => display(&current),
                };
                self.popup = Some(Popup::Text { field, input });
            }
        }
        FormKey::Handled
    }

    pub fn choice_reason(&self, item: &ChoiceItem) -> Option<String> {
        item.disabled_reason.clone().or_else(|| item.available_when.as_ref().and_then(|condition| {
            let actual = self.values.get(&condition.field);
            let matches = condition.equals.as_ref().is_some_and(|expected| actual == Some(expected)) || actual.is_some_and(|value| condition.any_of.contains(value));
            (!matches).then(|| tr!("Unavailable for the selected model", "No disponible para el modelo elegido").into())
        }))
    }

    pub fn choice_label(&self, item: &ChoiceItem) -> String {
        let label = item.group.as_ref().map_or_else(|| item.label.clone(), |group| format!("{group} · {}", item.label));
        self.choice_reason(item).map_or_else(|| label.clone(), |reason| format!("{label} · {reason}"))
    }

    fn pick_choice(&mut self, field: usize, id: String) -> FormKey {
        let Some(item) = self.choices(field).iter().find(|item| item.id == id) else { return FormKey::Handled };
        if let Some(reason) = self.choice_reason(item) { self.error = Some(reason); return FormKey::Handled; }
        let changed = self.values.get(&self.fields[field].id).and_then(Value::as_str) != Some(id.as_str());
        self.set(field, json!(id.clone()));
        if changed && self.fields[field].reload_on_change {
            FormKey::Reload(id)
        } else {
            FormKey::Handled
        }
    }

    fn commit_text(&mut self, field: usize, input: &str) {
        let input = input.trim();
        let value = match &self.fields[field].kind {
            _ if input.is_empty() => Value::Null,
            FieldKind::Stepper { .. } => input.parse::<i64>().map_or_else(|_| json!(input), |n| json!(n)),
            FieldKind::Lines { .. } => {
                json!(input.split(',').map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n"))
            }
            _ => json!(input),
        };
        self.set(field, value);
    }

    fn popup_key(&mut self, k: KeyEvent) -> FormKey {
        let Some(popup) = self.popup.as_mut() else {
            return FormKey::NotMine;
        };
        if k.code == KeyCode::Esc {
            self.popup = None;
            return FormKey::Handled;
        }
        match popup {
            Popup::Values { field, sel } => {
                let field = *field;
                let n = self.choices(field).len();
                let Some(Popup::Values { sel, .. }) = self.popup.as_mut() else {
                    return FormKey::Handled;
                };
                match k.code {
                    KeyCode::Up | KeyCode::Char('k') => *sel = sel.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => *sel = (*sel + 1).min(n.saturating_sub(1)),
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        let pick = *sel;
                        self.popup = None;
                        if let Some(item) = self.choices(field).get(pick).cloned() {
                            return self.pick_choice(field, item.id);
                        }
                    }
                    _ => {}
                }
            }
            Popup::Filter { field, query, sel } => match k.code {
                KeyCode::Up => *sel = sel.saturating_sub(1),
                KeyCode::Down => *sel += 1,
                KeyCode::Backspace => {
                    query.pop();
                    *sel = 0;
                }
                KeyCode::Char(c) => {
                    query.push(c);
                    *sel = 0;
                }
                KeyCode::Enter => {
                    let (field, query, sel) = (*field, query.clone(), *sel);
                    self.popup = None;
                    let items = self.filtered(field, &query);
                    let Some(item) = items.get(sel.min(items.len().saturating_sub(1))).map(|i| (*i).clone()) else {
                        return FormKey::Handled;
                    };
                    return self.pick_choice(field, item.id);
                }
                _ => {}
            },
            Popup::Issues { field, picker } => match k.code {
                KeyCode::Char('r') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    picker.loading = true;
                    return FormKey::LoadIssues(true);
                }
                KeyCode::Up => picker.sel = picker.sel.saturating_sub(1),
                KeyCode::Down => {
                    picker.sel = (picker.sel + 1).min(picker.visible().len().saturating_sub(1))
                }
                KeyCode::Backspace => {
                    picker.query.pop();
                    picker.sel = 0;
                }
                KeyCode::Char(c) => {
                    picker.query.push(c);
                    picker.sel = 0;
                }
                KeyCode::Enter => {
                    let field = *field;
                    let reference = match picker.visible().get(picker.sel) {
                        Some(issue) => issue.reference(),
                        None => match picker.typed_reference() {
                            Some(reference) => reference,
                            None => return FormKey::Handled,
                        },
                    };
                    self.popup = None;
                    self.set(field, json!(reference.clone()));
                    return self.project_of(&reference);
                }
                _ => {}
            },
            Popup::Text { field, input } => match k.code {
                KeyCode::Enter => {
                    let (field, input) = (*field, input.clone());
                    self.popup = None;
                    self.commit_text(field, &input);
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) => input.push(c),
                _ => {}
            },
        }
        FormKey::Handled
    }

    /// Choosing an issue picks the project named like its repository, when there is one.
    fn project_of(&mut self, reference: &str) -> FormKey {
        let Some(field) = self.reloading_field() else {
            return FormKey::Handled;
        };
        let Some(name) = reference.split_once('#').and_then(|(repo, _)| repo.split_once('/')).map(|(_, name)| name) else {
            return FormKey::Handled;
        };
        let Some(item) = self
            .choices(field)
            .iter()
            .find(|item| !item.id.is_empty() && item.id.eq_ignore_ascii_case(name))
            .cloned()
        else {
            return FormKey::Handled;
        };
        self.pick_choice(field, item.id)
    }

    pub fn key(&mut self, k: KeyEvent) -> FormKey {
        if k.code == KeyCode::Char('r') && k.modifiers.contains(KeyModifiers::CONTROL) && self.replacement.is_some() {
            self.submit = self.replacement.take(); self.error = None; return FormKey::Handled;
        }
        if self.popup.is_some() {
            return self.popup_key(k);
        }
        let Some(at) = self.focus else {
            if k.code == KeyCode::Tab && self.loaded && !self.rows().is_empty() {
                self.focus = Some(0);
                return FormKey::Handled;
            }
            return FormKey::NotMine;
        };
        let last = self.rows().len().saturating_sub(1);
        match k.code {
            KeyCode::Tab | KeyCode::Esc | KeyCode::BackTab => self.focus = None,
            KeyCode::Up | KeyCode::Char('k') => self.focus = Some(at.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => self.focus = Some((at + 1).min(last)),
            KeyCode::Enter | KeyCode::Char(' ') => return self.open(at),
            KeyCode::Char('p') => {
                if let Some(at) = self.reloading_field().and_then(|f| self.rows().iter().position(|r| *r == f)) {
                    self.focus = Some(at);
                    return self.open(at);
                }
            }
            KeyCode::Char('i') => {
                let picker = self.rows().iter().position(|r| matches!(self.fields[*r].kind, FieldKind::Picker { .. }));
                if let Some(at) = picker {
                    self.focus = Some(at);
                    return self.open(at);
                }
            }
            KeyCode::Char('x') => {
                if let Some(&field) = self.rows().get(at)
                    && matches!(self.fields[field].kind, FieldKind::Picker { .. })
                {
                    self.set(field, Value::Null);
                }
            }
            _ => {}
        }
        FormKey::Handled
    }

    /// The `fields` of the submit: the typed text, the attachments and every visible row.
    pub fn intent_error(&self) -> Option<String> {
        if self.replacement.is_some() { return self.error.clone(); }
        let submit = self.submit.as_ref()?;
        if !submit.enabled { return Some(submit.disabled_reason.clone().unwrap_or_else(|| tr!("This action is unavailable", "Esta acción no está disponible").into())); }
        if modern_intent(&self.values) && (!self.fields.iter().any(|field| field.id == "model") || submit.action_id.starts_with("new_request:") || submit.action_id.starts_with("sched_save:")) {
            return Some(tr!("The connected daemon cannot accept this model selection. Your choices are retained.", "El daemon conectado no admite esta selección de modelo. Tus elecciones se conservan.").into());
        }
        None
    }

    pub fn submitted(&self, text: &str, attachments: &[String]) -> Value {
        let mut out = serde_json::Map::new();
        for field in &self.fields {
            if !field_visible(field, &self.values) {
                continue;
            }
            let value = match &field.kind {
                FieldKind::Multiline { .. } if self.primary().is_some_and(|p| p.id == field.id) => json!(text),
                FieldKind::Attachments if attachments.is_empty() => continue,
                FieldKind::Attachments => json!(attachments),
                _ => match self.values.get(&field.id) {
                    Some(value) => value.clone(),
                    None => continue,
                },
            };
            out.insert(field.id.clone(), value);
        }
        if self.fields.iter().any(|field| field.id == "allow_model_substitution") {
            if let Some(edited) = self.values.get("substitution_was_edited") { out.insert("substitution_was_edited".into(), edited.clone()); }
        }
        Value::Object(out)
    }
}

fn model_field(id: &str) -> bool { matches!(id, "model" | "account" | "reasoning_effort" | "allow_model_substitution" | "substitution_was_edited" | "account_was_edited" | "pin_account") || id.starts_with("candidate_") || id.starts_with("reasoning_effort_") }

fn modern_intent(values: &BTreeMap<String, Value>) -> bool {
    ["model", "reasoning_effort"].iter().any(|id| values.get(*id).and_then(Value::as_str).is_some_and(|value| !value.is_empty() && value != "auto"))
        || values.get("account").and_then(Value::as_str).is_some_and(|value| value.contains('/'))
        || values.get("substitution_was_edited") == Some(&json!(true))
}

fn keeps(kind: &FieldKind, value: &Value) -> bool {
    match kind {
        FieldKind::Choice { items } => items.iter().any(|item| value.as_str() == Some(item.id.as_str())),
        FieldKind::Multiline { .. } | FieldKind::Attachments => false,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn fixture(name: &str) -> View {
        serde_json::from_value(crate::tui::parity::view_fixture(name)).unwrap()
    }

    pub(crate) fn new_request() -> OptionsForm {
        let mut form = OptionsForm::new("new_request");
        form.loaded(&fixture("new-request-ios.json"));
        form
    }

    pub(crate) fn schedule() -> OptionsForm {
        let mut form = OptionsForm::new("schedule:new");
        form.loaded(&fixture("schedule-new-ios.json"));
        form
    }

    fn keys(form: &mut OptionsForm, codes: &[KeyCode]) {
        for code in codes {
            form.key(key(*code));
        }
    }

    #[test]
    fn the_new_request_rows_are_the_servers_fields_project_and_issue_first() {
        i18n::set_for_this_thread(i18n::Lang::En);
        let form = new_request();
        let labels: Vec<_> = form.rows_shown().into_iter().map(|(l, v, _)| (l, v)).collect();
        assert_eq!(
            labels,
            [
                ("Project", "Alondra picks the project"),
                ("Issue", "none"),
                ("PRs", "Review each one"),
                ("Effort", "Auto"),
                ("Account", "Automatic"),
            ]
            .map(|(l, v)| (l.to_owned(), v.to_owned()))
        );
        assert_eq!(form.primary().map(|f| f.id.as_str()), Some("text"));
    }

    #[test]
    fn a_choice_row_opens_its_values_and_enter_sets_one() {
        let mut form = new_request();
        keys(&mut form, &[KeyCode::Tab, KeyCode::Down, KeyCode::Down, KeyCode::Enter, KeyCode::Down, KeyCode::Enter]);
        assert!(form.popup.is_none());
        assert_eq!(form.values["pr"], json!("auto_merge"));
        assert!(matches!(form.key(key(KeyCode::Down)), FormKey::Handled));
    }

    #[test]
    fn the_reloading_choice_filters_as_you_type_and_asks_for_its_fields() {
        let mut form = new_request();
        keys(&mut form, &[KeyCode::Tab, KeyCode::Enter]);
        assert!(matches!(form.popup, Some(Popup::Filter { .. })));
        for c in "garden".chars() {
            form.key(key(KeyCode::Char(c)));
        }
        match form.key(key(KeyCode::Enter)) {
            FormKey::Reload(project) => assert_eq!(project, "Demo Garden"),
            _ => panic!("picking another project reloads the form"),
        }
        assert_eq!(form.values["project"], json!("Demo Garden"));
    }

    #[test]
    fn picking_the_project_already_chosen_does_not_reload() {
        let mut form = new_request();
        keys(&mut form, &[KeyCode::Tab, KeyCode::Enter]);
        assert!(matches!(form.key(key(KeyCode::Enter)), FormKey::Handled));
    }

    #[test]
    fn the_issue_row_loads_issues_and_picking_one_picks_its_project() {
        let mut form = new_request();
        keys(&mut form, &[KeyCode::Tab, KeyCode::Down]);
        assert!(matches!(form.key(key(KeyCode::Enter)), FormKey::LoadIssues(false)));
        let issue = |repo: &str| Issue {
            repo: repo.into(), number: 7, title: "Crash".into(), url: String::new(),
            author: None, association: None, labels: vec![], updated_at: None, comments: 0,
        };
        form.issues_loaded(Ok((vec![issue("me/demo garden")], None)));
        match form.key(key(KeyCode::Enter)) {
            FormKey::Reload(project) => assert_eq!(project, "Demo Garden"),
            _ => panic!("an issue of a known project picks it"),
        }
        assert_eq!(form.values["issue"], json!("me/demo garden#7"));
        keys(&mut form, &[KeyCode::Char('x')]);
        assert!(!form.values.contains_key("issue"));
    }

    #[test]
    fn a_pasted_issue_link_is_accepted_when_it_is_not_listed() {
        let mut form = new_request();
        keys(&mut form, &[KeyCode::Tab, KeyCode::Char('i')]);
        form.issues_loaded(Ok((vec![], None)));
        for c in "https://github.com/me/app/issues/9".chars() {
            form.key(key(KeyCode::Char(c)));
        }
        form.key(key(KeyCode::Enter));
        assert_eq!(form.values["issue"], json!("me/app#9"));
    }

    #[test]
    fn rows_follow_the_visibility_rules_of_the_schedule_form() {
        let mut form = schedule();
        let shown = |form: &OptionsForm| form.rows_shown().into_iter().map(|(l, _, _)| l).collect::<Vec<_>>();
        assert!(shown(&form).contains(&"Time".to_owned()) || !shown(&form).is_empty());
        assert!(!shown(&form).iter().any(|l| l == "Weekday"));
        form.set(form.fields.iter().position(|f| f.id == "repeat").unwrap(), json!("weekly"));
        assert!(shown(&form).iter().any(|l| l == "Weekday"));
    }

    #[test]
    fn text_toggle_and_stepper_rows_edit_in_place() {
        let mut form = schedule();
        let repeat = form.fields.iter().position(|f| f.id == "repeat").unwrap();
        form.set(repeat, json!("every_days"));
        let at = |form: &OptionsForm, id: &str| {
            let field = form.fields.iter().position(|f| f.id == id).unwrap();
            form.rows().iter().position(|r| *r == field).unwrap()
        };
        form.focus = Some(at(&form, "interval"));
        keys(&mut form, &[KeyCode::Enter, KeyCode::Backspace, KeyCode::Char('3'), KeyCode::Enter]);
        assert_eq!(form.values["interval"], json!(3));
        form.focus = Some(at(&form, "active"));
        keys(&mut form, &[KeyCode::Enter]);
        assert_eq!(form.values["active"], json!(false));
    }

    #[test]
    fn submitted_carries_the_text_the_attachments_and_only_visible_rows() {
        let mut form = new_request();
        let sent = form.submitted("Fix the crash", &["att-1".into()]);
        assert_eq!(
            sent,
            json!({
                "text": "Fix the crash", "attachments": ["att-1"], "project": "", "issue": "",
                "pr": "review", "effort": "auto", "account": "auto"
            })
        );
        assert!(form.submitted("Fix the crash", &[]).get("attachments").is_none());
        let schedule = schedule();
        let sent = schedule.submitted("Check the beta", &[]);
        assert!(sent.get("weekday").is_none() && sent["time"] == json!("09:00"));
        form.set(form.fields.len() - 1, json!("me/app#3"));
        assert_eq!(form.submitted("x", &[])["issue"], json!("me/app#3"));
    }

    #[test]
    fn a_reload_keeps_what_was_chosen_while_it_is_still_offered() {
        let mut form = new_request();
        let pr = form.fields.iter().position(|f| f.id == "pr").unwrap();
        form.set(pr, json!("auto_merge"));
        form.loaded(&fixture("new-request-ios.json"));
        assert_eq!(form.values["pr"], json!("auto_merge"));
        let effort = form.fields.iter().position(|f| f.id == "effort").unwrap();
        form.set(effort, json!("gone"));
        form.loaded(&fixture("new-request-ios.json"));
        assert_eq!(form.values["effort"], json!("auto"));
    }
}
