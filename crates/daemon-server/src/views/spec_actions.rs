//! The user's edits of a task's proposed specs on its plan's card: each one is made on the
//! proposal, read by the spec engine over the task's worktree, and kept only when the engine can
//! read the result; a problem goes back next to the field it is about.
use super::actions::ActionRequest;
use alondra_core::text::Lang;
use alondra_domain_model::specs::{self, DecisionFields, SpecEdit, SpecEditError, SpecFile, SpecLevel};
use alondra_domain_model::{Actor, Command, CommandEnvelope, DataEpoch, DomainError, DomainState, TaskId};
use alondra_view_model::action_id::ActionId;
use serde_json::{json, Value};

fn field<'a>(request: &'a ActionRequest, name: &str) -> &'a str {
    request.input.fields.get(name).and_then(Value::as_str).map(str::trim).unwrap_or_default()
}

fn raw<'a>(request: &'a ActionRequest, name: &str) -> &'a str {
    request.input.fields.get(name).and_then(Value::as_str).unwrap_or_default()
}

fn rejected(lang: Lang, field: &str, message: &str) -> Value {
    let lead = if lang == Lang::Es { "No se puede guardar" } else { "This cannot be saved" };
    json!({"protocol":alondra_view_model::VIEWS_PROTOCOL,"outcome":"rejected","field":field,"message":format!("{lead}: {message}")})
}

/// The edit an action asks for, from its `extra` and the form's fields.
fn edit_of(state: &DomainState, task: &TaskId, action: &ActionId, request: &ActionRequest) -> Result<SpecEdit, SpecEditError> {
    let unknown = || SpecEditError { field: "path".into(), message: "This spec is no longer in the proposal: look at it again".into() };
    let draft = state.tasks.get(task).and_then(|task| task.specs.draft.as_ref()).ok_or_else(unknown)?;
    let extra = match action.extra.as_deref().unwrap_or_default() {
        "remove" => request.option_id.as_deref().unwrap_or_default(),
        extra => extra,
    };
    let file = |h: &str| specs::file_of(draft, h).map(str::to_owned).ok_or_else(unknown);
    let scenario = |h: &str| specs::scenario_of(draft, h).ok_or_else(unknown);
    let today = specs::date_of(alondra_core::now_millis());
    let decision = || DecisionFields {
        id: field(request, "id").to_owned(), title: field(request, "title").to_owned(), context: field(request, "context").to_owned(),
        decision: field(request, "decision").to_owned(), consequences: field(request, "consequences").to_owned(), confirmation: field(request, "confirmation").to_owned(),
        supersedes: field(request, "supersedes").split(',').map(str::trim).filter(|id| !id.is_empty()).map(str::to_owned).collect(),
    };
    Ok(match extra.split_once('-').map_or((extra, ""), |(op, rest)| (op, rest)) {
        ("level", _) => {
            let level = request.option_id.as_deref().and_then(SpecLevel::parse).ok_or_else(|| SpecEditError { field: "level".into(), message: "Choose a level".into() })?;
            SpecEdit::Level(level)
        }
        ("spec", "new") => SpecEdit::PutSpec { path: None, component: field(request, "component").to_owned(), id: field(request, "id").to_owned(),
            title: field(request, "title").to_owned(), explanation: raw(request, "explanation").to_owned() },
        ("spec", h) => {
            let path = file(h)?;
            SpecEdit::PutSpec { path: Some(path), component: String::new(), id: field(request, "id").to_owned(), title: field(request, "title").to_owned(), explanation: raw(request, "explanation").to_owned() }
        }
        ("rmspec", h) => SpecEdit::RemoveSpec { path: file(h)? },
        ("sc", rest) => {
            let (path, old) = match rest.strip_prefix("new-") {
                Some(spec) => (file(spec)?, None),
                None => {
                    let (path, id) = scenario(rest)?;
                    (path, Some(id))
                }
            };
            SpecEdit::PutScenario { path, scenario: old, id: field(request, "id").to_owned(), title: field(request, "title").to_owned(),
                steps: raw(request, "steps").to_owned(), judged: request.input.fields.get("judged").and_then(Value::as_bool).unwrap_or(false) }
        }
        ("rmsc", h) => {
            let (path, scenario) = scenario(h)?;
            SpecEdit::RemoveScenario { path, scenario }
        }
        ("dec", "new") => SpecEdit::PutDecision { path: None, fields: decision(), date: today },
        ("dec", h) => SpecEdit::PutDecision { path: Some(file(h)?), fields: decision(), date: today },
        ("rmdec", h) => SpecEdit::RemoveDecision { path: file(h)? },
        _ => return Err(SpecEditError { field: "path".into(), message: "This edit is not known: update the app".into() }),
    })
}

/// The field a problem the engine found is shown next to, for the edit that caused it.
fn field_of(edit: &SpecEdit) -> &'static str {
    match edit {
        SpecEdit::Level(_) => "level",
        SpecEdit::PutSpec { .. } | SpecEdit::RemoveSpec { .. } => "explanation",
        SpecEdit::PutScenario { .. } | SpecEdit::RemoveScenario { .. } => "steps",
        SpecEdit::PutDecision { .. } | SpecEdit::RemoveDecision { .. } => "decision",
    }
}

pub(crate) fn execute(
    domain: &crate::domain_runtime::DomainRuntime, state: &DomainState, action: &ActionId, request: &ActionRequest, user_id: &str, lang: Lang,
    authorize: &(dyn Fn() -> anyhow::Result<()> + Sync),
) -> anyhow::Result<Value> {
    let task_id = TaskId(action.subject.clone());
    let Some(draft) = state.tasks.get(&task_id).and_then(|task| task.specs.draft.clone()) else {
        return Ok(rejected(lang, "path", "this task has no proposed specs any more"));
    };
    if draft.revision != action.revision {
        return Ok(rejected(lang, "path", "the specs changed since you opened them: look at them again"));
    }
    let edit = match edit_of(state, &task_id, action, request) {
        Ok(edit) => edit,
        Err(error) => return Ok(rejected(lang, &error.field, &error.message)),
    };
    let (level, files) = match specs::edited(&draft, &edit) {
        Ok(edited) => edited,
        Err(error) => return Ok(rejected(lang, &error.field, &error.message)),
    };
    let check = match crate::spec_checks::read(domain, state, &task_id, &files) {
        Ok(check) => check,
        Err(error) => return Ok(rejected(lang, field_of(&edit), &error.message)),
    };
    if !check.clean() && check.mode != specs::SpecMode::Off {
        return Ok(rejected(lang, field_of(&edit), &check.summary()));
    }
    let files: Vec<SpecFile> = files.into_iter().map(|(path, text)| SpecFile { path, text }).collect();
    let envelope = CommandEnvelope {
        protocol_version: 2, data_epoch: DataEpoch(request.data_epoch.clone()), command_id: request.command_id.clone().into(), expected_revision: state.revision,
        command: Command::ReviseSpecs { task_id, expected: draft.revision, level, files, check },
    };
    let actor = Actor::User { user_id: user_id.to_owned() };
    match domain.execute(&actor, &envelope, None, || authorize().map_err(|_| DomainError::new("forbidden", "device approval was revoked").into())) {
        Ok(outcome) => {
            let stamp = if lang == Lang::Es { "Specs actualizadas" } else { "Specs updated" };
            Ok(json!({"protocol":alondra_view_model::VIEWS_PROTOCOL,"outcome":"applied","stamp":stamp,"revision":outcome.revision}))
        }
        Err(crate::domain_store::StoreError::Domain(error)) => Ok(rejected(lang, field_of(&edit), &error.message)),
        Err(crate::domain_store::StoreError::Storage(error)) => Err(error.into()),
    }
}
