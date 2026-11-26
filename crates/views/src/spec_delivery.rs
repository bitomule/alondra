//! The section «Specs» of a delivery: each scenario the delivered work is about with the engine's
//! state of its validations, the test that declares it, the suite that ran it and its result, and
//! what the validator judged, always labelled a judgment; then the files the branch changed outside
//! the task's components and the reference prose it changed. It shows what Alondra read of the
//! delivered commit when the delivery passed the gate, never what an agent says.
use super::{
    common,
    spec_sheet::{lanes, line, picture, spec_sheet, Source, Version},
    text::{t, tf},
    Icon, Item, Markdown, Notice, RenderContext, Section, SheetAnchor, SheetPart, SheetPartKind, SheetResult, SheetScenario, SheetUnchanged, SheetVerdict, SpecSheet, Tone,
};
use alondra_domain_model::spec_card;
use alondra_domain_model::specs::{engine, StoredImage};
use alondra_domain_model::{clip, DeliveredScenario, DeliveredSpec, DeliveredValidation, DeliveredVerdict, Interaction, InteractionPayload, Scope, Task, ValidationKind, ValidationState, ValidationVerdict};

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

/// The result of a scenario inside its sheet: its state, each check, the tests that declare it and
/// what the validator judged.
fn result(scenario: &DeliveredScenario, delivered: &DeliveredValidation, validating: bool, context: &RenderContext<'_>) -> SheetResult {
    let lang = context.lang;
    let unavailable = scenario.judged.as_deref() == Some("capture")
        && scenario.verdicts.iter().any(|verdict| verdict.kind == ValidationKind::Judged && verdict.captures.iter().all(|capture| capture.image.is_none()));
    let state = if validating {
        line(Icon::History, Tone::Info, t!(lang, "Validating now", "Validando ahora"), None)
    } else if !all_valid(scenario) {
        line(Icon::Warning, Tone::Warning, t!(lang, "Not validated", "Sin validar"), None)
    } else if unavailable {
        line(Icon::Notice, Tone::Warning, t!(lang, "Evidence not available", "Evidencia no disponible"), None)
    } else {
        line(Icon::Check, Tone::Success, t!(lang, "Validated", "Validado"), None)
    };
    let checks = scenario.states.iter().map(|(kind, state)| {
        let (icon, tone) = if state == "valid" { (Icon::Check, Tone::Success) } else { (Icon::Warning, Tone::Warning) };
        line(icon, tone, format!("{}: {}", check_word(kind, context), state_word(state, context)), None)
    }).collect();
    let tests = scenario.tests.iter().map(|test| {
        let name = test.name.as_deref().map(|name| format!(" · {name}")).unwrap_or_default();
        let ran = test.suites.iter().map(|suite| {
            let shown = delivered.suites.iter().find(|s| s.id == *suite);
            let result = shown.and_then(|s| s.verdict).map(|v| verdict_word(v, context)).unwrap_or_else(|| shown.map_or(t!(lang, "not run", "sin ejecutar"), |s| state_word(&s.state, context)));
            tf!(lang, "suite {suite}: {result}", "suite {suite}: {result}")
        }).collect::<Vec<_>>().join(" · ");
        line(Icon::Document, Tone::Neutral, format!("Test · {}:{}{name}", test.path, test.line), (!ran.is_empty()).then_some(ran))
    }).collect();
    let verdicts = scenario.verdicts.iter().map(|verdict| verdict_of(&scenario.id, verdict, context)).collect();
    SheetResult { state, checks, tests, verdicts }
}

fn verdict_of(id: &str, verdict: &DeliveredVerdict, context: &RenderContext<'_>) -> SheetVerdict {
    let lang = context.lang;
    let what = spec_card::kind_name(verdict.kind, lang);
    let (icon, tone) = match verdict.verdict {
        ValidationVerdict::Met => (Icon::Question, Tone::Info),
        _ => (Icon::Warning, Tone::Warning),
    };
    let nearest = verdict.nearest_wrong.as_deref().filter(|_| verdict.kind == ValidationKind::Adequate);
    let evidence = if verdict.captures.is_empty() {
        verdict.evidence.iter().map(|path| capture(id, path, None, context)).collect()
    } else {
        verdict.captures.iter().map(|kept| capture(id, &kept.path, kept.image.as_ref(), context)).collect()
    };
    SheetVerdict {
        line: line(icon, tone, tf!(lang, "Judgment · {what}: {result}", "Juicio · {what}: {result}", result = verdict_word(verdict.verdict, context)), None),
        reasoning: verdict.reasoning.as_deref().map(|reasoning| clip(reasoning, 1200)),
        nearest_label: nearest.map(|_| t!(lang, "Nearest wrong behaviour these tests still accept", "Lo más cercano a un error que estos tests aún aceptan").to_owned()),
        nearest_wrong: nearest.map(|nearest| clip(nearest, 1200)),
        evidence,
    }
}

fn capture(scenario: &str, path: &str, image: Option<&StoredImage>, context: &RenderContext<'_>) -> SheetPart {
    let lang = context.lang;
    let title = tf!(lang, "Evidence · {path}", "Evidencia · {path}");
    let id = format!("evidence:{scenario}:{path}");
    SheetPart {
        kind: SheetPartKind::Mockup,
        name: path.to_owned(),
        alt: tf!(lang, "Capture {path} the validator judged for {scenario}", "Captura {path} que el validador juzgó para {scenario}"),
        mark: None,
        code: None,
        picture: image.map(picture),
        unavailable: image.is_none().then(|| t!(lang, "Evidence not available: Alondra keeps no image of it.", "Evidencia no disponible: Alondra no guarda su imagen.").to_owned()),
        anchor: Some(SheetAnchor { id: id.clone(), quote: format!("{scenario} · {path}") }),
        edit: None,
        title,
        id,
    }
}

/// A scenario of a delivery recorded before Alondra kept its spec files: its title and steps.
fn plain_scenario(scenario: &DeliveredScenario, context: &RenderContext<'_>) -> SheetScenario {
    let steps: Vec<engine::Step> = scenario.steps.iter().map(|step| {
        let (word, text) = step.split_once(' ').unwrap_or((step, ""));
        let keyword = [engine::Keyword::Given, engine::Keyword::When, engine::Keyword::Then, engine::Keyword::And, engine::Keyword::But]
            .into_iter().find(|keyword| keyword.as_str() == word).unwrap_or(engine::Keyword::And);
        engine::Step { keyword, text: text.to_owned(), doc: None, table: vec![] }
    }).collect();
    SheetScenario {
        id: scenario.id.clone(),
        title: scenario.title.clone(),
        kind: if scenario.judged.is_some() { t!(context.lang, "judgment", "juicio").into() } else { "test".into() },
        mark: None,
        collapsed: false,
        lanes: lanes(&steps, context.lang),
        examples: None,
        parts: vec![],
        result: None,
        anchor: Some(SheetAnchor { id: format!("scenario:{}", scenario.id), quote: format!("{} · {}", scenario.id, scenario.title) }),
        edit: None,
        add: vec![],
    }
}

fn delivered_sheet(spec: &DeliveredSpec, delivered: &DeliveredValidation, task: &Task, context: &RenderContext<'_>) -> SpecSheet {
    let lang = context.lang;
    let after = |path: &str| spec.parts.get(path).cloned();
    let before = |path: &str| spec.base_parts.get(path).cloned();
    let built = spec.source.as_deref().and_then(|source| spec_sheet(&Source {
        path: &spec.path,
        after: Some(Version { text: source, parts: &after }),
        before: spec.base.as_deref().map(|text| Version { text, parts: &before }),
        titles: &spec.decisions,
    }, None, lang).ok());
    let mut sheet = built.unwrap_or_else(|| SpecSheet {
        id: format!("spec:{}", spec.id),
        spec: spec.id.clone(),
        title: spec.title.clone(),
        mark: None,
        explanation: String::new(),
        decisions: None,
        parts: vec![],
        background: None,
        scenarios: spec.scenarios.iter().map(|scenario| plain_scenario(scenario, context)).collect(),
        unchanged: None,
        empty: None,
        anchor: Some(SheetAnchor { id: format!("spec:{}", spec.id), quote: format!("{} · {}", spec.id, spec.title) }),
        edit: None,
        add: vec![],
        actions: vec![],
    });
    let running = |subject: &str| task.validation.open_round()
        .is_some_and(|round| round.items.iter().any(|item| item.subject == subject && item.state == ValidationState::Running));
    let fill = |drawn: &mut SheetScenario| {
        let found = spec.scenarios.iter().find(|scenario| scenario.id == drawn.id);
        drawn.result = found.map(|found| result(found, delivered, running(&found.id), context));
        drawn.collapsed = drawn.result.as_ref().is_some_and(|result| result.state.tone == Tone::Success);
    };
    sheet.scenarios.iter_mut().for_each(fill);
    if let Some(mut folded) = sheet.unchanged.take() {
        folded.scenarios.iter_mut().for_each(fill);
        let (quiet, loud): (Vec<_>, Vec<_>) = folded.scenarios.into_iter().partition(|scenario| scenario.collapsed);
        sheet.scenarios.extend(loud);
        sheet.unchanged = (!quiet.is_empty()).then(|| SheetUnchanged {
            label: match quiet.len() {
                1 => t!(lang, "1 unchanged scenario, validated", "1 escenario sin cambios, validado").into(),
                n => tf!(lang, "{n} unchanged scenarios, validated", "{n} escenarios sin cambios, validados"),
            },
            scenarios: quiet,
        });
    }
    sheet
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
        Some(delivered) => items.extend(delivered_items(delivered, task, context)),
        None if task.validation.open_round().is_some() => items.push(notice(
            t!(lang, "A validator is checking this work against its specs.", "Un validador está comprobando este trabajo contra sus specs.").into(), Tone::Info)),
        None if !task.validation.rounds.is_empty() => items.push(notice(
            t!(lang, "Evidence not available: Alondra did not keep what this delivery showed of its specs.", "Evidencia no disponible: Alondra no guardó lo que esta entrega mostraba de sus specs.").into(), Tone::Info)),
        None if task.specs.approved.is_some() => items.push(plain(t!(lang, "This change touches no spec.", "Este cambio no toca ninguna spec.").into())),
        None => {}
    }
    (!items.is_empty()).then(|| common::section("specs", Some("Specs".into()), items))
}

fn delivered_items(delivered: &DeliveredValidation, task: &Task, context: &RenderContext<'_>) -> Vec<Item> {
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
    items.extend(delivered.specs.iter().map(|spec| Item::SpecSheet(Box::new(delivered_sheet(spec, delivered, task, context)))));
    if !delivered.suites.is_empty() {
        let suites = delivered.suites.iter().map(|suite| {
            let result = suite.verdict.map(|v| verdict_word(v, context)).unwrap_or_else(|| state_word(&suite.state, context));
            let mark = if suite.state == "valid" { "✓" } else { "!" };
            let last = suite.output.as_deref().and_then(|output| output.lines().last()).map(|line| format!(" · `{}`", clip(line, 200).replace('`', "'"))).unwrap_or_default();
            tf!(lang, "- {mark} `{id}`: {result}{last}", "- {mark} `{id}`: {result}{last}", id = suite.id)
        }).collect::<Vec<_>>();
        items.push(plain(format!("**{}**\n\n{}", t!(lang, "Suites", "Suites"), suites.join("\n"))));
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
        let mut files = delivered.outside.iter().take(SHOWN).map(|path| format!("- `{path}`")).collect::<Vec<_>>();
        if delivered.outside.len() > SHOWN {
            files.push(tf!(lang, "- and {n} more", "- y {n} más", n = delivered.outside.len() - SHOWN));
        }
        items.push(plain(files.join("\n")));
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
