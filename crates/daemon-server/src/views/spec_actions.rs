//! The user's edits of a task's proposed specs on its plan's card: each one is made on the
//! proposal, read by the spec engine over the task's worktree, and kept only when the engine can
//! read the result; a problem goes back next to the field it is about.
use super::actions::ActionRequest;
use alondra_core::text::Lang;
use alondra_domain_model::specs::{self, engine, DecisionFields, KeptImages as _, SpecEdit, SpecEditError, SpecFile, SpecLevel, StoredImage, MAX_MOCKUP_BYTES};
use alondra_domain_model::specs::engine::PartKind;
use alondra_engine_ports::kept_images::HomeImages;
use std::path::Path;
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

/// The file name a mockup takes from the image uploaded for it, as a tag can cite it.
fn part_file_name(uploaded: &str) -> String {
    uploaded.to_lowercase().replace(char::is_whitespace, "-")
}

/// The mockup the user sent with an edit: the file attached to it, kept as Alondra keeps mockups.
fn sent_mockup(home: &Path, request: &ActionRequest, lang: Lang) -> Result<Option<(String, StoredImage)>, SpecEditError> {
    let Some(id) = request.input.attachments.first() else { return Ok(None) };
    let refused = |en: &str, es: &str| SpecEditError { field: "image".into(), message: if lang == Lang::Es { es } else { en }.into() };
    if request.input.attachments.len() > 1 {
        return Err(refused("Attach one image", "Adjunta una sola imagen"));
    }
    let id = id.strip_prefix("att:").unwrap_or(id).to_owned();
    let stored = alondra_engine_ports::uploads::resolve(home, &[id]).ok().and_then(|found| found.into_iter().next());
    let path = stored.as_ref().and_then(|file| file.stored_path.clone()).ok_or_else(|| refused("The image is no longer available: attach it again", "La imagen ya no está disponible: adjúntala de nuevo"))?;
    let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(u64::MAX);
    if size > MAX_MOCKUP_BYTES as u64 {
        return Err(refused("A mockup is at most 2 MB", "Un mockup ocupa como mucho 2 MB"));
    }
    let name = stored.map(|file| file.name).unwrap_or_default();
    let image = std::fs::read(&path).ok().and_then(|bytes| HomeImages::new(home).keep(&bytes))
        .ok_or_else(|| refused("A mockup is a PNG or a JPEG image", "Un mockup es una imagen PNG o JPEG"))?;
    Ok(Some((name, image)))
}

/// The spec and scenario a part edit is about, from `s<spec handle>` or `c<scenario handle>`.
fn part_target(draft: &specs::SpecDraft, target: &str) -> Option<(String, Option<String>)> {
    match target.split_at_checked(1)? {
        ("s", h) => Some((specs::file_of(draft, h)?.to_owned(), None)),
        ("c", h) => specs::scenario_of(draft, h).map(|(path, id)| (path, Some(id))),
        _ => None,
    }
}

/// The edit of a diagram or mockup: `partadd-<kind>-<target>`, `part-<kind>-<target>-<part>` or
/// `rmpart-<kind>-<target>-<part>`.
fn part_edit(home: &Path, draft: &specs::SpecDraft, op: &str, rest: &str, request: &ActionRequest, lang: Lang) -> Result<SpecEdit, SpecEditError> {
    let unknown = || SpecEditError { field: "name".into(), message: "This part is no longer in the spec: look at it again".into() };
    let mut words = rest.splitn(3, '-');
    let kind = match words.next() {
        Some("diagram") => PartKind::Diagram,
        Some("mockup") => PartKind::Mockup,
        _ => return Err(unknown()),
    };
    let (path, scenario) = words.next().and_then(|target| part_target(draft, target)).ok_or_else(unknown)?;
    let part = match words.next() {
        Some(h) => Some(specs::part_of(draft, &path, scenario.as_deref(), kind, h).ok_or_else(unknown)?),
        None => None,
    };
    if op == "rmpart" {
        return Ok(SpecEdit::RemovePart { path, scenario, kind, part: part.ok_or_else(unknown)? });
    }
    let name = field(request, "name").to_owned();
    Ok(match kind {
        PartKind::Diagram => SpecEdit::PutDiagram { path, scenario, part, name, code: raw(request, "code").to_owned() },
        PartKind::Mockup => {
            let sent = sent_mockup(home, request, lang)?;
            let current = part.as_deref().and_then(|part| specs::part_content(draft, &engine::part_path(&path, part))).and_then(|content| content.image().cloned());
            let uploaded = sent.as_ref().map(|(name, _)| name.clone());
            let image = sent.map(|(_, image)| image).or(current).ok_or_else(|| SpecEditError {
                field: "image".into(), message: if lang == Lang::Es { "Adjunta la imagen del mockup" } else { "Attach the image of the mockup" }.into() })?;
            let name = if name.is_empty() { uploaded.as_deref().map(part_file_name).unwrap_or_default() } else { name };
            SpecEdit::PutMockup { path, scenario, part, name, image }
        }
    })
}

/// The edit an action asks for, from its `extra` and the form's fields.
fn edit_of(home: &Path, state: &DomainState, task: &TaskId, action: &ActionId, request: &ActionRequest, lang: Lang) -> Result<SpecEdit, SpecEditError> {
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
        (op @ ("part" | "partadd" | "rmpart"), rest) => part_edit(home, draft, op, rest, request, lang)?,
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
        SpecEdit::PutDiagram { .. } => "code",
        SpecEdit::PutMockup { .. } => "image",
        SpecEdit::RemovePart { .. } => "name",
    }
}

pub(crate) fn execute(
    domain: &dyn alondra_engine_ports::Runtime, state: &DomainState, action: &ActionId, request: &ActionRequest, user_id: &str, lang: Lang,
    authorize: &(dyn Fn() -> anyhow::Result<()> + Sync),
) -> anyhow::Result<Value> {
    let task_id = TaskId(action.subject.clone());
    let Some(draft) = state.tasks.get(&task_id).and_then(|task| task.specs.draft.clone()) else {
        return Ok(rejected(lang, "path", "this task has no proposed specs any more"));
    };
    if draft.revision != action.revision {
        return Ok(rejected(lang, "path", "the specs changed since you opened them: look at them again"));
    }
    let edit = match edit_of(domain.home(), state, &task_id, action, request, lang) {
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
    let files: Vec<SpecFile> = files.into_iter().map(|(path, content)| SpecFile::of(path, content)).collect();
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
        Err(alondra_engine_ports::journal::StoreError::Domain(error)) => Ok(rejected(lang, field_of(&edit), &error.message)),
        Err(alondra_engine_ports::journal::StoreError::Storage(error)) => Err(error.into()),
    }
}
