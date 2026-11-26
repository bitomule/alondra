//! The specs of a task's plan: the section of the plan's card that shows them as specs a person
//! reads (an explanation, then the scenarios), never their raw syntax, and the screens where the
//! user adds, edits and removes them before approving the plan.
use super::{
    action_id::{ActionId, Verb},
    common,
    text::{t, tf},
    Action, ActionOption, ActionBar, BuildError, Confirm, Field, FieldKind, Form, Icon, Input, Item, Markdown, Notice, Presentation, RenderContext, Section, Style, Tone, View,
};
use alondra_domain_model::spec_card;
use alondra_domain_model::specs::{self, SpecDraft, SpecFileKind, SpecLevel, SpecMode};
use alondra_domain_model::{Hold, Interaction, InteractionPayload, Scope, Task, TaskId};
use alondra_domain_model::specs::engine;
use serde_json::json;

pub(super) const EDITOR: &str = "task_specs";

/// The task whose plan this card is, while the card waits for the user.
fn plan_task<'a>(interaction: &Interaction, context: &'a RenderContext<'_>) -> Option<&'a Task> {
    if context.domain.holds.get(&interaction.id) != Some(&Hold::Plan) {
        return None;
    }
    let Scope::Task { task_id } = &interaction.scope else { return None };
    context.domain.tasks.get(task_id)
}

fn level_text(level: SpecLevel, context: &RenderContext<'_>) -> &'static str {
    match level {
        SpecLevel::None => t!(context.lang, "No new specs: the change does not alter behaviour", "Sin specs nuevas: el cambio no altera el comportamiento"),
        SpecLevel::Light => t!(context.lang, "Light: up to 5 scenarios and low risk", "Ligero: hasta 5 escenarios y riesgo bajo"),
        SpecLevel::Full => t!(context.lang, "Full: a new boundary, or a risk of concurrency, security or stored data", "Completo: frontera nueva, o riesgo de concurrencia, seguridad o datos guardados"),
    }
}

fn status_word(before: Option<&Option<String>>, after: &Option<String>, context: &RenderContext<'_>) -> &'static str {
    match (before.cloned().flatten(), after) {
        (None, Some(_)) => t!(context.lang, "new", "nueva"),
        (Some(_), None) => t!(context.lang, "removed", "eliminada"),
        _ => t!(context.lang, "changed", "cambiada"),
    }
}

/// Why the plan of `task` cannot be approved for its specs, in the reader's language.
pub(super) fn approval_block(task: &Task, context: &RenderContext<'_>) -> Option<String> {
    if let Some(problem) = &task.specs.settings {
        return Some(tf!(context.lang, "The project's alondra.toml cannot be read: {problem}", "No se puede leer el alondra.toml del proyecto: {problem}", problem = problem.located()));
    }
    let draft = task.specs.draft.as_ref()?;
    (!draft.check.clean()).then(|| tf!(context.lang, "Its specs cannot be read: {problems}", "Sus specs no se pueden leer: {problems}", problems = draft.check.summary()))
}

/// The button of a plan's card that opens the editor of its specs, while the plan waits.
pub(super) fn edit_link(interaction: &Interaction, context: &RenderContext<'_>) -> Option<Action> {
    let task = plan_task(interaction, context)?;
    task.specs.draft.as_ref()?;
    matches!(interaction.payload, InteractionPayload::Question { answer: None, .. })
        .then(|| common::view_link(&format!("{EDITOR}:{}", task.id), t!(context.lang, "Edit the specs", "Editar las specs")))
}

/// Why the plan on `interaction` cannot be approved for its specs, if it cannot.
pub(super) fn plan_block(interaction: &Interaction, context: &RenderContext<'_>) -> Option<String> {
    plan_task(interaction, context).and_then(|task| approval_block(task, context))
}

fn commentable(text: String) -> Item {
    Item::Markdown(Markdown { text, truncated: false, more: None, commentable: true })
}

fn plain(text: String) -> Item {
    Item::Markdown(Markdown { text, truncated: false, more: None, commentable: false })
}

fn notice(text: String, tone: Tone) -> Item {
    Item::Notice(Notice { text, tone, action: None })
}

/// The section «Specs» of a plan's card: each spec as its explanation and its scenarios, the
/// boundaries and decisions it changes, and the way to edit them while the plan waits.
pub(super) fn plan_section(interaction: &Interaction, context: &RenderContext<'_>) -> Option<Section> {
    let task = plan_task(interaction, context)?;
    let open = matches!(interaction.payload, InteractionPayload::Question { answer: None, .. });
    let draft = task.specs.draft.as_ref();
    let settings = task.specs.settings.as_ref();
    if draft.is_none() && settings.is_none() {
        return None;
    }
    let mut items = vec![];
    if let Some(block) = approval_block(task, context) {
        items.push(notice(tf!(context.lang, "Fix this before approving the plan. {block}", "Corrige esto antes de aprobar el plan. {block}"), Tone::Warning));
    }
    if let Some(draft) = draft {
        if draft.check.mode == SpecMode::Uncommitted {
            items.push(notice(t!(context.lang,
                "This project keeps its specs in Alondra only, never in the repository: nothing can be regenerated from them later.",
                "Este proyecto guarda sus specs solo en Alondra, nunca en el repositorio: después no se podrá regenerar nada a partir de ellas.").into(), Tone::Info));
        }
        items.push(plain(tf!(context.lang, "**Level:** {level}", "**Nivel:** {level}", level = level_text(draft.level, context))));
        items.extend(draft_items(draft, context));
        if open {
            items.push(Item::Actions(ActionBar { actions: vec![common::view_link(&format!("{EDITOR}:{}", task.id), t!(context.lang, "Edit the specs", "Editar las specs"))] }));
        }
    }
    Some(common::section("specs", Some("Specs".into()), items))
}

/// Every file of a proposal as the card shows it: specs as cards with their scenarios, the rest
/// by what it is.
fn draft_items(draft: &SpecDraft, context: &RenderContext<'_>) -> Vec<Item> {
    let dirs = &draft.check.dirs;
    let mut items = vec![];
    if draft.files.is_empty() {
        items.push(plain(t!(context.lang, "This plan changes no spec.", "Este plan no cambia ninguna spec.").into()));
    }
    let order = |path: &str| match dirs.kind(path) {
        Some(SpecFileKind::Spec) => 0,
        Some(SpecFileKind::Boundary) => 1,
        Some(SpecFileKind::Decision) => 2,
        Some(SpecFileKind::Reference) => 3,
        _ => 4,
    };
    let mut files: Vec<_> = draft.files.iter().collect();
    files.sort_by_key(|(path, _)| (order(path), (*path).clone()));
    for (path, text) in files {
        let status = status_word(draft.check.before.get(path), text, context);
        match (dirs.kind(path), text) {
            (Some(SpecFileKind::Spec), Some(text)) => match engine::read_spec(path, text.as_bytes()) {
                Ok(spec) => {
                    let mut header = format!("### {} · `{}` ({status})", spec.title, spec.id);
                    if !spec.description.is_empty() {
                        header.push_str(&format!("\n\n{}", spec.description.join(" ")));
                    }
                    if !spec.decisions.is_empty() {
                        header.push_str(&tf!(context.lang, "\n\nRests on the decisions {ids}", "\n\nSe apoya en las decisiones {ids}",
                            ids = spec.decisions.iter().map(|id| format!("`{id}`")).collect::<Vec<_>>().join(", ")));
                    }
                    if !spec.background.is_empty() {
                        header.push_str(&tf!(context.lang, "\n\n**Always first:**\\\n{steps}", "\n\n**Siempre antes:**\\\n{steps}", steps = spec_card::steps_text(&spec.background)));
                    }
                    items.push(commentable(header));
                    if spec.scenarios.is_empty() {
                        items.push(plain(t!(context.lang, "No scenario yet.", "Aún sin escenarios.").into()));
                    }
                    items.extend(spec.scenarios.iter().map(|scenario| commentable(spec_card::scenario_text(&spec.id, scenario, context.lang))));
                }
                Err(_) => items.push(plain(format!("### `{path}` ({status})"))),
            },
            (Some(SpecFileKind::Decision), Some(text)) => {
                let fields = specs::decision_fields(text);
                items.push(commentable(tf!(context.lang,
                    "### {title} · `{id}` (decision, {status})\n\n{decision}\n\n**Context:** {why}\n\n**Consequences:** {consequences}",
                    "### {title} · `{id}` (decisión, {status})\n\n{decision}\n\n**Contexto:** {why}\n\n**Consecuencias:** {consequences}",
                    title = fields.title, id = fields.id, decision = fields.decision, why = fields.context, consequences = fields.consequences)));
            }
            (kind, Some(text)) => {
                let what = match kind {
                    Some(SpecFileKind::Boundary) => t!(context.lang, "boundary", "frontera"),
                    Some(SpecFileKind::Reference) => t!(context.lang, "reference", "referencia"),
                    Some(SpecFileKind::Settings) => t!(context.lang, "settings", "ajustes"),
                    _ => t!(context.lang, "file", "archivo"),
                };
                items.push(commentable(format!("### `{path}` ({what}, {status})\n\n{}", fence(text))));
            }
            (_, None) => items.push(commentable(format!("### ~~`{path}`~~ ({status})"))),
        }
    }
    items
}

fn fence(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}\n{}\n{fence}", text.trim_end())
}

use alondra_domain_model::specs::{file_of, handle, scenario_handle, scenario_of};

fn edit_action(task: &TaskId, draft: &SpecDraft, extra: &str, label: &str, style: Style) -> Option<Action> {
    let mut action = common::action(Verb::SpecEdit, &task.0, draft.revision, label, style)?;
    action.id = ActionId::new(Verb::SpecEdit, &task.0, draft.revision, Some(extra.to_owned())).ok()?.to_string();
    Some(action)
}

fn removal(task: &TaskId, draft: &SpecDraft, extra: &str, what: &str, context: &RenderContext<'_>) -> Option<Action> {
    let mut action = edit_action(task, draft, extra, t!(context.lang, "Remove", "Quitar"), Style::Destructive)?;
    action.confirm = Some(Confirm {
        title: tf!(context.lang, "Remove {what} from the plan?", "¿Quitar {what} del plan?"),
        message: t!(context.lang, "It leaves the specs you approve with this plan. You can add it again while the plan waits.",
            "Deja de estar en las specs que apruebas con este plan. Puedes volver a añadirlo mientras el plan espera.").into(),
        button: t!(context.lang, "Remove", "Quitar").into(),
        cancel: None,
    });
    Some(action)
}

/// The task, its proposal, and whether the user can still edit it.
fn editable<'a>(id: &str, context: &'a RenderContext<'_>) -> Result<(&'a Task, &'a SpecDraft, Option<String>), BuildError> {
    let task = context.domain.tasks.get(&TaskId::from(id)).ok_or(BuildError::NotFound)?;
    let draft = task.specs.draft.as_ref().ok_or(BuildError::NotFound)?;
    let closed = context.domain.open_plan_card(&task.id).is_none()
        .then(|| t!(context.lang, "The plan was already decided: its specs no longer change here", "El plan ya se decidió: sus specs ya no cambian aquí").to_owned());
    Ok((task, draft, closed))
}

fn guarded(mut action: Action, closed: &Option<String>) -> Action {
    if let Some(reason) = closed {
        action.enabled = false;
        action.disabled_reason = Some(reason.clone());
    }
    action
}

/// The screens of a task's proposed specs: `task_specs:<task>` lists them, and its longer keys
/// open the form of one spec, scenario or decision.
pub(super) fn build(tail: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    let mut parts = tail.splitn(3, ':');
    let id = parts.next().filter(|id| !id.is_empty()).ok_or(BuildError::InvalidKey)?;
    match (parts.next(), parts.next()) {
        (None, _) => editor(id, context),
        (Some("spec"), Some(h)) => spec_form(id, h, context),
        (Some("scenario"), Some(h)) => scenario_form(id, h, context),
        (Some("decision"), Some(h)) => decision_form(id, h, context),
        _ => Err(BuildError::InvalidKey),
    }
}

fn editor(id: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    let (task, draft, closed) = editable(id, context)?;
    let lang = context.lang;
    let key = format!("{EDITOR}:{id}");
    let mut sections = vec![];
    let mut top = vec![];
    if let Some(block) = approval_block(task, context) {
        top.push(notice(block, Tone::Warning));
    }
    if let Some(reason) = &closed {
        top.push(notice(reason.clone(), Tone::Info));
    }
    top.push(plain(tf!(lang, "**Level:** {level}", "**Nivel:** {level}", level = level_text(draft.level, context))));
    let mut bar = vec![];
    if let Some(mut level) = edit_action(&task.id, draft, "level", t!(lang, "Change the level", "Cambiar el nivel"), Style::Secondary) {
        level.presentation = Presentation::Menu;
        level.key = Some("Shift+N".into());
        level.options = SpecLevel::ALL.into_iter().filter(|option| *option != draft.level).map(|option| ActionOption {
            id: option.as_str().into(), label: level_text(option, context).into(), description: None, style: Style::Plain, input: Input::None, key: None,
        }).collect();
        bar.push(guarded(level, &closed));
    }
    let dirs = &draft.check.dirs;
    let mut specs_items = vec![];
    let mut decisions = vec![];
    let mut others = vec![];
    let mut links: Vec<super::Fact> = vec![];
    let mut removals: Vec<ActionOption> = vec![];
    let link = |label: String, value: String, to: String| super::Fact { kind: None, label, value, tone: None, link: Some(super::Link::View { key: to }) };
    let remove = |id: String, label: String| ActionOption { id, label, description: None, style: Style::Destructive, input: Input::None, key: None };
    for (path, text) in &draft.files {
        let h = handle(path);
        match (dirs.kind(path), text) {
            (Some(SpecFileKind::Spec), Some(text)) => {
                let Ok(spec) = engine::read_spec(path, text.as_bytes()) else { continue };
                let mut row = common::row(&format!("spec-{h}"), &format!("{key}:spec:{h}"), Icon::Document, spec.title.clone(), Some(format!("`{}`", spec.id)));
                row.folded = true;
                links.push(link(t!(lang, "Spec", "Spec").into(), spec.title.clone(), format!("{key}:spec:{h}")));
                removals.push(remove(format!("rmspec-{h}"), tf!(lang, "The spec “{title}”", "La spec «{title}»", title = spec.title)));
                row.children = spec.scenarios.iter().map(|scenario| {
                    let sh = scenario_handle(path, &scenario.id);
                    let steps: Vec<String> = scenario.steps.iter().map(|step| format!("{} {}", step.keyword.as_str(), step.text)).collect();
                    let mut child = common::row(&format!("scenario-{sh}"), &format!("{key}:scenario:{sh}"), Icon::Check, scenario.title.clone(), Some(steps.join(" · ")));
                    child.depth = 1;
                    if scenario.judgment.is_some() {
                        child.badges.push(super::Label { text: t!(lang, "judged", "de juicio").into(), tone: Tone::Info, elapsed: None });
                    }
                    child.actions = removal(&task.id, draft, &format!("rmsc-{sh}"), t!(lang, "this scenario", "este escenario"), context).map(|a| guarded(a, &closed)).into_iter().collect();
                    child
                }).collect();
                for scenario in &spec.scenarios {
                    let sh = scenario_handle(path, &scenario.id);
                    links.push(link(tf!(lang, "Scenario of “{spec}”", "Escenario de «{spec}»", spec = spec.title), scenario.title.clone(), format!("{key}:scenario:{sh}")));
                    removals.push(remove(format!("rmsc-{sh}"), tf!(lang, "The scenario “{title}”", "El escenario «{title}»", title = scenario.title)));
                }
                links.push(link(t!(lang, "New", "Nuevo").into(), tf!(lang, "A scenario of “{spec}”", "Un escenario de «{spec}»", spec = spec.title), format!("{key}:scenario:new-{h}")));
                row.actions = [
                    Some(common::view_link(&format!("{key}:scenario:new-{h}"), t!(lang, "Add a scenario", "Añadir un escenario"))),
                    removal(&task.id, draft, &format!("rmspec-{h}"), t!(lang, "this spec", "esta spec"), context),
                ].into_iter().flatten().map(|a| guarded(a, &closed)).collect();
                specs_items.push(Item::Row(row));
            }
            (Some(SpecFileKind::Decision), Some(text)) => {
                let fields = specs::decision_fields(text);
                let mut row = common::row(&format!("decision-{h}"), &format!("{key}:decision:{h}"), Icon::Document, fields.title.clone(), Some(format!("`{}`", fields.id)));
                row.actions = removal(&task.id, draft, &format!("rmdec-{h}"), t!(lang, "this decision", "esta decisión"), context).map(|a| guarded(a, &closed)).into_iter().collect();
                links.push(link(t!(lang, "Decision", "Decisión").into(), fields.title.clone(), format!("{key}:decision:{h}")));
                removals.push(remove(format!("rmdec-{h}"), tf!(lang, "The decision “{title}”", "La decisión «{title}»", title = fields.title)));
                decisions.push(Item::Row(row));
            }
            _ => {
                let mut row = common::row(&format!("file-{h}"), &key, Icon::Attachment, format!("`{path}`"), Some(status_word(draft.check.before.get(path), text, context).into()));
                row.link = None;
                others.push(Item::Row(row));
            }
        }
    }
    links.push(link(t!(lang, "New", "Nueva").into(), t!(lang, "A spec", "Una spec").into(), format!("{key}:spec:new")));
    if dirs.decisions.is_some() {
        links.push(link(t!(lang, "New", "Nueva").into(), t!(lang, "A decision", "Una decisión").into(), format!("{key}:decision:new")));
    }
    if !removals.is_empty()
        && let Some(mut take_out) = edit_action(&task.id, draft, "remove", t!(lang, "Take out of the plan", "Quitar del plan"), Style::Destructive)
    {
        take_out.presentation = Presentation::Menu;
        take_out.key = Some("x".into());
        take_out.description = Some(t!(lang, "What you take out is not approved with the plan; a file the project already has stays as it is",
            "Lo que quitas no se aprueba con el plan; un archivo que el proyecto ya tiene se queda como está").into());
        take_out.options = removals;
        bar.push(guarded(take_out, &closed));
    }
    top.push(Item::Actions(ActionBar { actions: bar }));
    sections.push(common::section("level", Some(t!(lang, "Specs of the plan", "Specs del plan").into()), top));
    if closed.is_none() {
        sections.push(common::section(DOCUMENTS, Some(t!(lang, "Choose what to edit", "Elegir qué editar").into()), vec![Item::Facts(super::Facts { items: links })]));
    }
    sections.push(common::section("specs", Some("Specs".into()), specs_items));
    if dirs.decisions.is_some() && !decisions.is_empty() {
        sections.push(common::section("decisions", Some(t!(lang, "Decisions", "Decisiones").into()), decisions));
    }
    if !others.is_empty() {
        sections.push(common::section("files", Some(t!(lang, "Other files", "Otros archivos").into()), others));
    }
    Ok(common::view(EDITOR, &key, Some(tf!(lang, "Specs of “{title}”", "Specs de «{title}»", title = task.title)), context, sections))
}

/// The section of a screen whose links a client lists to open one: the terminal's «f».
const DOCUMENTS: &str = "documents";

fn field(id: &str, label: &str, kind: FieldKind, value: serde_json::Value, required: bool) -> Field {
    Field { id: id.into(), label: label.into(), kind, value, required, help: None, visible_when: None, reload_on_change: false }
}

fn text_kind() -> FieldKind {
    FieldKind::Text { placeholder: None }
}

fn long_kind(placeholder: Option<String>) -> FieldKind {
    FieldKind::Multiline { placeholder }
}

fn form_view(id: &str, key: &str, title: String, fields: Vec<Field>, submit: Action, context: &RenderContext<'_>) -> View {
    common::view(EDITOR, key, Some(title), context, vec![common::section("form", None, vec![Item::Form(Form { substitution_was_edited: None, id: format!("{EDITOR}-{id}"), fields, submit })])])
}

fn spec_form(id: &str, h: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    let (task, draft, closed) = editable(id, context)?;
    let key = format!("{EDITOR}:{id}:spec:{h}");
    let save = t!(context.lang, "Save the spec", "Guardar la spec");
    if h == "new" {
        let submit = guarded(edit_action(&task.id, draft, "spec-new", save, Style::Primary).ok_or(BuildError::InvalidActionId)?, &closed);
        let mut component = field("component", t!(context.lang, "Component", "Componente"), text_kind(), json!(""), true);
        component.help = Some(tf!(context.lang, "The folder of its boundary under {dir}/", "La carpeta de su frontera dentro de {dir}/", dir = draft.check.dirs.specs));
        let fields = vec![component,
            field("id", t!(context.lang, "Identifier", "Identificador"), text_kind(), json!(""), true),
            field("title", t!(context.lang, "Title", "Título"), text_kind(), json!(""), true),
            field("explanation", t!(context.lang, "Explanation", "Explicación"), long_kind(Some(t!(context.lang, "What it does and why, in plain words", "Qué hace y por qué, en palabras llanas").into())), json!(""), false)];
        return Ok(form_view(id, &key, t!(context.lang, "New spec", "Spec nueva").into(), fields, submit, context));
    }
    let path = file_of(draft, h).ok_or(BuildError::NotFound)?;
    let text = draft.files.get(path).cloned().flatten().ok_or(BuildError::NotFound)?;
    let spec = engine::read_spec(path, text.as_bytes()).map_err(|_| BuildError::NotFound)?;
    let submit = guarded(edit_action(&task.id, draft, &format!("spec-{h}"), save, Style::Primary).ok_or(BuildError::InvalidActionId)?, &closed);
    let fields = vec![
        field("id", t!(context.lang, "Identifier", "Identificador"), text_kind(), json!(spec.id), true),
        field("title", t!(context.lang, "Title", "Título"), text_kind(), json!(spec.title), true),
        field("explanation", t!(context.lang, "Explanation", "Explicación"), long_kind(None), json!(spec.description.join("\n")), false),
    ];
    Ok(form_view(id, &key, spec.title.clone(), fields, submit, context))
}

fn scenario_form(id: &str, h: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    let (task, draft, closed) = editable(id, context)?;
    let key = format!("{EDITOR}:{id}:scenario:{h}");
    let save = t!(context.lang, "Save the scenario", "Guardar el escenario");
    let steps_help = t!(context.lang, "One step per line: Given, When, Then, And or But. Tables between | and an Examples: table work as in Gherkin.",
        "Un paso por línea: Given, When, Then, And o But. Las tablas entre | y una tabla Examples: funcionan como en Gherkin.");
    let (extra, values, title) = match h.strip_prefix("new-") {
        Some(spec) => {
            file_of(draft, spec).ok_or(BuildError::NotFound)?;
            (format!("sc-new-{spec}"), ("".to_owned(), "".to_owned(), "".to_owned(), false), t!(context.lang, "New scenario", "Escenario nuevo").to_owned())
        }
        None => {
            let (path, scenario_id) = scenario_of(draft, h).ok_or(BuildError::NotFound)?;
            let text = draft.files.get(&path).cloned().flatten().ok_or(BuildError::NotFound)?;
            let spec = engine::read_spec(&path, text.as_bytes()).map_err(|_| BuildError::NotFound)?;
            let scenario = spec.scenarios.iter().find(|scenario| scenario.id == scenario_id).ok_or(BuildError::NotFound)?;
            (format!("sc-{h}"), (scenario.id.clone(), scenario.title.clone(), specs::scenario_body(scenario), scenario.judgment.is_some()), scenario.title.clone())
        }
    };
    let submit = guarded(edit_action(&task.id, draft, &extra, save, Style::Primary).ok_or(BuildError::InvalidActionId)?, &closed);
    let mut steps = field("steps", t!(context.lang, "Steps", "Pasos"), long_kind(Some("Given …\nWhen …\nThen …".into())), json!(values.2), true);
    steps.help = Some(steps_help.into());
    let mut judged = field("judged", t!(context.lang, "Judged", "De juicio"), FieldKind::Toggle, json!(values.3), false);
    judged.help = Some(t!(context.lang, "An agent checks it by running it or on captures, instead of a test", "Lo comprueba un agente ejecutándolo o sobre capturas, en vez de un test").into());
    let fields = vec![
        field("id", t!(context.lang, "Identifier", "Identificador"), text_kind(), json!(values.0), true),
        field("title", t!(context.lang, "Title", "Título"), text_kind(), json!(values.1), true),
        steps, judged,
    ];
    Ok(form_view(id, &key, title, fields, submit, context))
}

fn decision_form(id: &str, h: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    let (task, draft, closed) = editable(id, context)?;
    let key = format!("{EDITOR}:{id}:decision:{h}");
    let save = t!(context.lang, "Save the decision", "Guardar la decisión");
    let (extra, fields_of, title) = if h == "new" {
        ("dec-new".to_owned(), specs::DecisionFields::default(), t!(context.lang, "New decision", "Decisión nueva").to_owned())
    } else {
        let path = file_of(draft, h).ok_or(BuildError::NotFound)?;
        let text = draft.files.get(path).cloned().flatten().ok_or(BuildError::NotFound)?;
        let fields = specs::decision_fields(&text);
        let title = fields.title.clone();
        (format!("dec-{h}"), fields, title)
    };
    let submit = guarded(edit_action(&task.id, draft, &extra, save, Style::Primary).ok_or(BuildError::InvalidActionId)?, &closed);
    let fields = vec![
        field("id", t!(context.lang, "Identifier", "Identificador"), text_kind(), json!(fields_of.id), true),
        field("title", t!(context.lang, "Title", "Título"), text_kind(), json!(fields_of.title), true),
        field("context", t!(context.lang, "Context", "Contexto"), long_kind(None), json!(fields_of.context), true),
        field("decision", t!(context.lang, "Decision", "Decisión"), long_kind(None), json!(fields_of.decision), true),
        field("consequences", t!(context.lang, "Consequences", "Consecuencias"), long_kind(None), json!(fields_of.consequences), true),
        field("confirmation", t!(context.lang, "How it is checked", "Cómo se comprueba"), long_kind(None), json!(fields_of.confirmation), true),
        field("supersedes", t!(context.lang, "Replaces decisions", "Sustituye a las decisiones"), text_kind(), json!(fields_of.supersedes.join(", ")), false),
    ];
    Ok(form_view(id, &key, title, fields, submit, context))
}

/// What the task's detail says of its specs: whether the settings can be read, the proposal, what
/// is approved and written, and each change or decision that waits for the user.
pub(super) fn task_facts(task: &Task, context: &RenderContext<'_>) -> Vec<super::Fact> {
    use alondra_domain_model::AmendmentState;
    let lang = context.lang;
    let specs = &task.specs;
    let fact = |label: &str, value: String, tone: Option<Tone>| super::Fact { kind: None, label: label.into(), value, tone, link: None };
    let mut facts = vec![];
    if let Some(problem) = &specs.settings {
        facts.push(fact(t!(lang, "Settings", "Ajustes"), tf!(lang, "alondra.toml cannot be read: {problem}. No plan is approved nor result delivered until it is fixed.",
            "alondra.toml no se puede leer: {problem}. No se aprueba ningún plan ni se entrega ningún resultado hasta corregirlo.", problem = problem.located()), Some(Tone::Warning)));
    }
    if let Some(draft) = &specs.draft {
        facts.push(fact(t!(lang, "Proposed", "Propuestas"), tf!(lang, "{n} files with the plan · {level}", "{n} archivos con el plan · {level}", n = draft.files.len(), level = level_text(draft.level, context)), None));
    }
    if let Some(approved) = &specs.approved {
        let state = match (approved.mode, approved.unwritten.is_empty()) {
            (SpecMode::Uncommitted, _) => t!(lang, "kept in Alondra, never in the repository", "guardadas en Alondra, nunca en el repositorio"),
            (_, true) => t!(lang, "written in the worktree", "escritas en el worktree"),
            (_, false) => t!(lang, "written once no agent works", "se escriben cuando ningún agente trabaje"),
        };
        facts.push(fact(t!(lang, "Approved", "Aprobadas"), tf!(lang, "{n} files · {state}", "{n} archivos · {state}", n = approved.files.len()), None));
    }
    for amendment in &specs.amendments {
        let state = match amendment.state {
            AmendmentState::Requested => t!(lang, "asked for, the author drafts it", "pedido, lo redacta el autor"),
            AmendmentState::Proposed => t!(lang, "waits for you", "te espera"),
            AmendmentState::Approved => t!(lang, "approved", "aprobado"),
            AmendmentState::Rejected => t!(lang, "rejected", "rechazado"),
        };
        let tone = (amendment.state == AmendmentState::Proposed).then_some(Tone::YourTurn);
        facts.push(fact(t!(lang, "Change", "Cambio"), format!("{} · {state}", alondra_domain_model::clip(&amendment.reason, 120)), tone));
    }
    for proposed in &specs.proposed_decisions {
        facts.push(fact(t!(lang, "Decision", "Decisión"), tf!(lang, "{title} · waits for you", "{title} · te espera", title = proposed.title), Some(Tone::YourTurn)));
    }
    for answer in &specs.decisions {
        let verdict = match answer.verdict {
            alondra_domain_model::DecisionVerdict::Accepted => t!(lang, "accepted", "aceptada"),
            alondra_domain_model::DecisionVerdict::Rejected => t!(lang, "rejected", "rechazada"),
        };
        let words = answer.words.as_deref().map(|words| format!(" · «{words}»")).unwrap_or_default();
        facts.push(fact(t!(lang, "Decision", "Decisión"), format!("`{}` · {verdict}{words}", answer.id), None));
    }
    facts
}
