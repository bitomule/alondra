//! The section «Specs» of a delivery: each scenario the delivered work is about with the engine's
//! state of its validations, the test that declares it, the suite that ran it and its result, and
//! what the validator judged, always labelled a judgment; then the files the branch changed outside
//! the task's components and the reference prose it changed. It shows what Alondra read of the
//! delivered commit when the delivery passed the gate, never what an agent says.
use super::{
    common,
    text::{t, tf},
    Icon, Item, Markdown, Notice, RenderContext, Row, Section, Tone,
};
use alondra_domain_model::spec_card;
use alondra_domain_model::{clip, DeliveredScenario, DeliveredValidation, DeliveredVerdict, Interaction, InteractionPayload, Scope, Task, ValidationKind, ValidationVerdict};

fn row(id: String, icon: Icon, title: String, subtitle: Option<String>, depth: u8) -> Row {
    let mut row = common::row(&id, "", icon, title, subtitle);
    row.link = None;
    row.depth = depth;
    row
}

fn plain(text: String) -> Item {
    Item::Markdown(Markdown { text, truncated: false, more: None, commentable: false })
}

fn notice(text: String, tone: Tone) -> Item {
    Item::Notice(Notice { text, tone, action: None })
}

/// An engine state in plain words.
pub(super) fn state_word(state: &str, context: &RenderContext<'_>) -> &'static str {
    let lang = context.lang;
    match state {
        "valid" => t!(lang, "valid", "válido"),
        "missing" => t!(lang, "never validated", "sin validar"),
        "stale" => t!(lang, "out of date", "caducado"),
        "unmet" => t!(lang, "not met", "no se cumple"),
        "failed" => t!(lang, "failed", "falló"),
        "uncovered" => t!(lang, "no test declares it", "ningún test lo declara"),
        _ => t!(lang, "unknown", "desconocido"),
    }
}

fn verdict_word(verdict: ValidationVerdict, context: &RenderContext<'_>) -> &'static str {
    match verdict {
        ValidationVerdict::Met => t!(context.lang, "met", "se cumple"),
        ValidationVerdict::Unmet => t!(context.lang, "not met", "no se cumple"),
        ValidationVerdict::Incomplete => t!(context.lang, "could not run", "no se pudo ejecutar"),
    }
}

fn check_word(kind: &str, context: &RenderContext<'_>) -> &'static str {
    match kind {
        "passes" => t!(context.lang, "passes", "pasa"),
        "adequate" => t!(context.lang, "its test checks it", "su test lo comprueba"),
        _ => t!(context.lang, "judgment", "juicio"),
    }
}

/// The task of a final delivery whose card this is.
fn delivery_task<'a>(interaction: &Interaction, context: &'a RenderContext<'_>) -> Option<&'a Task> {
    if !matches!(interaction.payload, InteractionPayload::Delivery { is_final: true, .. }) {
        return None;
    }
    let Scope::Task { task_id } = &interaction.scope else { return None };
    context.domain.tasks.get(task_id)
}

/// Whether every validation of a scenario is valid on the delivered commit.
fn all_valid(scenario: &DeliveredScenario) -> bool {
    scenario.states.iter().all(|(_, state)| state == "valid")
}

fn scenario_row(scenario: &DeliveredScenario, delivered: &DeliveredValidation, context: &RenderContext<'_>) -> Item {
    let lang = context.lang;
    let states = scenario.states.iter().map(|(kind, state)| format!("{}: {}", check_word(kind, context), state_word(state, context))).collect::<Vec<_>>().join(" · ");
    let kind = if scenario.judged.is_some() { t!(lang, "judgment", "de juicio") } else { t!(lang, "tested", "con test") };
    let (icon, status) = if all_valid(scenario) { (Icon::Check, t!(lang, "Validated", "Validado")) } else { (Icon::Warning, t!(lang, "Not validated", "Sin validar")) };
    let mut parent = row(format!("spec-{}", scenario.id), icon, scenario.title.clone(),
        Some(format!("{status} · {kind} · `{}` · {states}", scenario.id)), 0);
    let mut children = vec![];
    if !scenario.steps.is_empty() {
        children.push(row(format!("spec-{}-steps", scenario.id), Icon::Document, scenario.steps.join(" · "), None, 1));
    }
    for (n, test) in scenario.tests.iter().enumerate() {
        let name = test.name.as_deref().map(|name| format!(" · {name}")).unwrap_or_default();
        let ran = test.suites.iter().map(|suite| {
            let shown = delivered.suites.iter().find(|s| s.id == *suite);
            let result = shown.and_then(|s| s.verdict).map(|v| verdict_word(v, context)).unwrap_or_else(|| shown.map_or(t!(lang, "not run", "sin ejecutar"), |s| state_word(&s.state, context)));
            tf!(lang, "suite {suite}: {result}", "suite {suite}: {result}")
        }).collect::<Vec<_>>().join(" · ");
        children.push(row(format!("spec-{}-test-{n}", scenario.id), Icon::Check,
            format!("Test · {}:{}{name}", test.path, test.line), (!ran.is_empty()).then_some(ran), 1));
    }
    for verdict in &scenario.verdicts {
        children.extend(verdict_rows(&scenario.id, verdict, context));
    }
    parent.children = children;
    Item::Row(parent)
}

fn verdict_rows(id: &str, verdict: &DeliveredVerdict, context: &RenderContext<'_>) -> Vec<Row> {
    let lang = context.lang;
    let what = spec_card::kind_name(verdict.kind, lang);
    let mut rows = vec![row(format!("spec-{id}-{}", verdict.kind.as_str()), Icon::Question,
        tf!(lang, "Judgment · {what}: {result}", "Juicio · {what}: {result}", result = verdict_word(verdict.verdict, context)),
        verdict.reasoning.as_deref().map(|reasoning| clip(reasoning, 600)), 1)];
    if let Some(nearest) = verdict.nearest_wrong.as_deref().filter(|_| verdict.kind == ValidationKind::Adequate) {
        rows.push(row(format!("spec-{id}-nearest"), Icon::Notice,
            t!(lang, "Nearest wrong behaviour these tests still accept", "Lo más cercano a un error que estos tests aún aceptan").into(), Some(clip(nearest, 600)), 2));
    }
    rows.extend(verdict.evidence.iter().enumerate().map(|(n, path)| row(format!("spec-{id}-evidence-{n}"), Icon::Attachment,
        tf!(lang, "Evidence · {path}", "Evidencia · {path}"), None, 2)));
    rows
}

/// What the delivery shows of its specs, or why it shows nothing; none for a task without specs.
pub(super) fn delivery_section(interaction: &Interaction, context: &RenderContext<'_>) -> Option<Section> {
    let lang = context.lang;
    let task = delivery_task(interaction, context)?;
    let mut items = vec![];
    if let Some(problem) = &task.specs.settings {
        items.push(notice(tf!(lang, "The project's alondra.toml cannot be read: {problem}", "No se puede leer el alondra.toml del proyecto: {problem}", problem = problem.located()), Tone::Warning));
    }
    match task.validation.delivered(&interaction.id) {
        Some(delivered) => items.extend(delivered_items(delivered, context)),
        None if task.validation.open_round().is_some() => items.push(notice(
            t!(lang, "A validator is checking this work against its specs.", "Un validador está comprobando este trabajo contra sus specs.").into(), Tone::Info)),
        None if !task.validation.rounds.is_empty() => items.push(notice(
            t!(lang, "Evidence not available: Alondra did not keep what this delivery showed of its specs.", "Evidencia no disponible: Alondra no guardó lo que esta entrega mostraba de sus specs.").into(), Tone::Info)),
        None if task.specs.approved.is_some() => items.push(plain(t!(lang, "This change touches no spec.", "Este cambio no toca ninguna spec.").into())),
        None => {}
    }
    (!items.is_empty()).then(|| common::section("specs", Some("Specs".into()), items))
}

fn delivered_items(delivered: &DeliveredValidation, context: &RenderContext<'_>) -> Vec<Item> {
    let lang = context.lang;
    let mut items = vec![];
    let scenarios: usize = delivered.specs.iter().map(|spec| spec.scenarios.len()).sum();
    if scenarios == 0 {
        items.push(plain(t!(lang, "This change touches no spec.", "Este cambio no toca ninguna spec.").into()));
    }
    let valid = delivered.specs.iter().flat_map(|spec| &spec.scenarios).filter(|scenario| all_valid(scenario)).count();
    if scenarios > 0 {
        items.push(plain(tf!(lang, "**{valid} of {scenarios} scenarios validated** on commit `{head}`. A judgment is a validator's verdict, never a fact a test showed.",
            "**{valid} de {scenarios} escenarios validados** en el commit `{head}`. Un juicio es el veredicto de un validador, nunca un hecho que mostró un test.",
            head = &delivered.head[..delivered.head.len().min(7)])));
    }
    for spec in &delivered.specs {
        items.push(plain(format!("**{}** · `{}`", spec.title, spec.id)));
        items.extend(spec.scenarios.iter().map(|scenario| scenario_row(scenario, delivered, context)));
    }
    if !delivered.suites.is_empty() {
        items.push(plain(format!("**{}**", t!(lang, "Suites", "Suites"))));
        items.extend(delivered.suites.iter().map(|suite| {
            let result = suite.verdict.map(|v| verdict_word(v, context)).unwrap_or_else(|| state_word(&suite.state, context));
            let mut shown = row(format!("suite-{}", suite.id), if suite.state == "valid" { Icon::Check } else { Icon::Warning },
                tf!(lang, "{id}: {result}", "{id}: {result}", id = suite.id), None, 0);
            if let Some(output) = &suite.output {
                shown.children = vec![row(format!("suite-{}-output", suite.id), Icon::Document, clip(output.lines().last().unwrap_or_default(), 200), None, 1)];
            }
            Item::Row(shown)
        }));
    }
    if !delivered.outside.is_empty() {
        let said = match delivered.outside.len() {
            1 => t!(lang, "1 file changed outside the components of the task's specs: no spec states what it does.",
                "1 archivo cambiado fuera de los componentes de las specs de la tarea: ninguna spec dice qué hace.").to_owned(),
            n => tf!(lang, "{n} files changed outside the components of the task's specs: no spec states what they do.",
                "{n} archivos cambiados fuera de los componentes de las specs de la tarea: ninguna spec dice qué hacen."),
        };
        items.push(notice(said, Tone::Warning));
        const SHOWN: usize = 20;
        items.extend(delivered.outside.iter().take(SHOWN).enumerate().map(|(n, path)| Item::Row(row(format!("outside-{n}"), Icon::Document, path.clone(), None, 0))));
        if delivered.outside.len() > SHOWN {
            items.push(plain(tf!(lang, "and {n} more", "y {n} más", n = delivered.outside.len() - SHOWN)));
        }
    }
    for section in &delivered.sections {
        items.push(plain(tf!(lang, "**Reference changed** · `{path}`", "**Referencia cambiada** · `{path}`", path = section.path)));
        items.push(plain(clip(&section.text, 4000)));
    }
    items
}

/// The subtitle of a scenario cited as evidence of a plan's point: how its delivery validated it.
pub(super) fn evidence_state(interaction: &Interaction, scenario: &str, context: &RenderContext<'_>) -> Option<String> {
    let task = delivery_task(interaction, context)?;
    let delivered = task.validation.delivered(&interaction.id)?;
    let found = delivered.specs.iter().flat_map(|spec| &spec.scenarios).find(|s| s.id == scenario)?;
    let states = found.states.iter().map(|(kind, state)| format!("{}: {}", check_word(kind, context), state_word(state, context))).collect::<Vec<_>>().join(" · ");
    let test = found.tests.first().map(|test| format!(" · {}:{}", test.path, test.line)).unwrap_or_default();
    Some(format!("{}{states}{test}", if all_valid(found) { "✓ " } else { "" }))
}
