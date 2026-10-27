//! A plan or a decision that cites a Markdown file carries the file's content, or it never
//! reaches the user.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

fn exec(s: &mut DomainState, a: Actor, c: Command) -> Result<CommandOutcome, DomainError> {
    let envelope = CommandEnvelope {
        protocol_version: 2,
        data_epoch: s.data_epoch.clone(),
        command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision,
        command: c,
    };
    s.execute(&a, &envelope)
}
fn ok(s: &mut DomainState, a: Actor, c: Command) {
    exec(s, a, c).unwrap();
}
fn refused(s: &mut DomainState, a: Actor, c: Command) -> DomainError {
    let before = s.clone();
    let error = exec(s, a, c).unwrap_err();
    assert_eq!(*s, before, "a rejected command changed state");
    error
}
fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}
fn fixture() -> DomainState {
    let mut s = DomainState::new("epoch");
    ok(&mut s, Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "Node A".into(), at: 0 });
    ok(&mut s, Actor::User { user_id: "u".into() }, Command::CreateRequest { request_id: "r".into(), body: "Build it".into(), auto_merge: false, maintenance: false, message: false });
    ok(&mut s, orchestrator(), Command::CreateTask { plan_items: None,
        task_id: "t".into(), request_id: "r".into(), title: "Build it".into(), agent_id: Some("a".into()),
        review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    s
}
fn plan(id: &str, body: &str) -> Command {
    Command::HoldHook {
        interaction_id: id.into(), task_id: "t".into(), agent_id: "a".into(), hold: Hold::Plan,
        body: body.into(), options: Hold::Plan.options(),
        card: CardSpec::new(CardKind::Review, "Review the plan").content(ContentKind::Plan, body),
    }
}
fn ask_user(body: &str) -> Command {
    let mut card = CardSpec::new(CardKind::Decide, "Aprueba el plan para quitar el buzón del relay y conectar por iroh")
        .why("Son unas 8 tareas que cambian cómo conectan iPhone y Mac con el daemon.");
    card.content = Some(CardContent { kind: ContentKind::Document, summary: None, body: body.into() });
    Command::AskQuestion {
        attachments: vec![], interaction_id: "approve".into(), scope: Scope::Orchestrator { request_id: None },
        recipient: Recipient::User, body: body.into(), blocking: false, card,
        options: vec![
            QuestionOption { id: "approve".into(), label: "Aprobar: crea las tareas".into(), description: Some("Creo las tareas.".into()) },
            QuestionOption { id: "reject".into(), label: "No, desmonta la prueba".into(), description: Some("No se crean tareas.".into()) },
        ],
    }
}
fn files(entries: &[(&str, &str)]) -> impl FnMut(&str) -> Option<String> {
    let files: BTreeMap<String, String> = entries.iter().map(|(path, content)| ((*path).to_owned(), (*content).to_owned())).collect();
    move |path| files.get(path).cloned()
}

/// The orchestrator's card of 14 October, word for word where it pointed at the plan.
const POINTER: &str = "Plan completo: docs/fast-connection-plan.md (PR #975, sin fusionar). Resultados de la prueba: docs/hole-punching-research.md §9.\n\nPASOS (una tarea y una PR cada uno):\n0. Medir el estado actual.\n1. Relay de iroh de producción.";

#[test]
fn local_markdown_paths_are_cited_but_urls_code_and_deliverables_are_not() {
    let text = "Plan completo: docs/fast-connection-plan.md (PR #975). Ver `AGENTS.md` y [guía](./docs/guide.md#arriba), ~/notes/today.md.\n\
        Enlaces: https://github.com/o/r/blob/main/README.md y github.com/o/r/HELP.md, ficheros x.mdx y .md sueltos.\n\
        ```sh\ncat docs/inside-code.md\n```\n\
        - [ ] Escribir los resultados en docs/results.md\n\
        - Repetido: docs/fast-connection-plan.md, al final de frase docs/end.md.";
    assert_eq!(cited_markdown_files(text), ["docs/fast-connection-plan.md", "AGENTS.md", "./docs/guide.md", "~/notes/today.md", "docs/end.md"]);
}

#[test]
fn a_cited_file_is_included_under_its_heading_and_never_adds_points_to_the_plan() {
    let body = "# Paso 1\n\nContexto en docs/plan.md.\n\n- [ ] Montar el relay\n- [ ] Escribir docs/results.md";
    let file = "# Plan de conexión rápida\n\n## Pasos\n\n- [ ] Medir\n- [x] Relay\n\n```\n- [ ] in code\n";
    let included = include_cited_files(body, files(&[("docs/plan.md", file)])).unwrap();
    assert!(included.starts_with(body));
    assert!(included.contains("\n\n---\n\n## docs/plan.md\n\n### Plan de conexión rápida\n\n#### Pasos\n\n- ☐ Medir\n- ☑ Relay\n\n```\n- [ ] in code\n```"), "{included}");
    assert_eq!(checklist_items(&included), checklist_items(body));
    assert!(files_not_included(&included).is_empty());
    assert_eq!(cited_files_problem(&included), None);
    assert_eq!(include_cited_files(&included, files(&[])).unwrap(), included, "an included file is not included twice");
    assert_eq!(without_included_files(&included), body);
    assert_eq!(without_included_files(body), body);
}

#[test]
fn each_included_file_comes_back_apart_with_its_own_headings() {
    let body = "Ver docs/a.md y docs/b.md";
    let included = include_cited_files(body, files(&[
        ("docs/a.md", "# Plan A\n\n## Pasos\n\n```\n## no es un fichero.md\n```"),
        ("docs/b.md", "Solo texto"),
    ])).unwrap();
    assert_eq!(included_files(&included), [
        ("docs/a.md".to_owned(), "# Plan A\n\n## Pasos\n\n```\n## no es un fichero.md\n```".to_owned()),
        ("docs/b.md".to_owned(), "Solo texto".to_owned()),
    ]);
    assert!(included_files(body).is_empty());
}

#[test]
fn citations_inside_an_included_file_are_not_followed() {
    let included = include_cited_files("Ver docs/a.md", files(&[("docs/a.md", "Detalle en docs/b.md")])).unwrap();
    assert_eq!(cited_markdown_files(&included), ["docs/a.md"]);
    assert_eq!(cited_files_problem(&included), None);
}

#[test]
fn a_file_that_cannot_be_read_or_is_too_long_refuses_the_card_naming_it() {
    let unreadable = include_cited_files(POINTER, files(&[])).unwrap_err();
    assert!(unreadable.contains("docs/fast-connection-plan.md") && unreadable.contains("cannot open"), "{unreadable}");
    let long = "x".repeat(CITED_FILE_MAX_BYTES + 1);
    let too_long = include_cited_files("Ver docs/a.md", files(&[("docs/a.md", &long)])).unwrap_err();
    assert!(too_long.contains("docs/a.md") && too_long.contains("too long"), "{too_long}");
    let part = "x".repeat(CITED_FILE_MAX_BYTES);
    let names: Vec<String> = (0..5).map(|n| format!("d{n}.md")).collect();
    let many: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), part.as_str())).collect();
    let together = include_cited_files(&names.join(" "), files(&many)).unwrap_err();
    assert!(together.contains("add up"), "{together}");
}

#[test]
fn the_domain_keeps_a_plan_with_its_cited_file_and_refuses_one_that_only_points_at_it() {
    let mut s = fixture();
    let pointer = "Plan completo en docs/plan.md.\n\n- [ ] Hacerlo";
    let error = refused(&mut s, Actor::System, plan("p1", pointer));
    assert_eq!((error.code.as_str(), error.subject.as_deref()), ("cited_file_not_included", Some("plan")));
    assert!(error.message.contains("docs/plan.md"), "{}", error.message);
    let whole = include_cited_files(pointer, files(&[("docs/plan.md", "# Plan\n\nTodo el detalle.")])).unwrap();
    ok(&mut s, Actor::System, plan("p2", &whole));
    assert_eq!(s.interactions[&InteractionId::from("p2")].body, whole);
    ok(&mut s, Actor::System, plan("p3", "# Paso\n\n- [ ] Escribir los resultados en docs/results.md"));
}

#[test]
fn the_orchestrators_plan_card_of_14_october_never_reaches_the_user() {
    let mut s = fixture();
    let error = refused(&mut s, orchestrator(), ask_user(POINTER));
    assert_eq!((error.code.as_str(), error.subject.as_deref()), ("cited_file_not_included", Some("card.content.body")));
    assert!(error.message.contains("docs/fast-connection-plan.md, docs/hole-punching-research.md"), "{}", error.message);
    let whole = include_cited_files(POINTER, files(&[
        ("docs/fast-connection-plan.md", "# Plan\n\nLos 8 pasos con sus medidas."),
        ("docs/hole-punching-research.md", "# Investigación\n\n## 9. Resultados"),
    ])).unwrap();
    ok(&mut s, orchestrator(), ask_user(&whole));
}
