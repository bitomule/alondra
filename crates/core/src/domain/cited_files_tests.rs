//! A card carries the Markdown files its author lists, and a native plan the ones it links to; a
//! path only mentioned in the text is never read and never refuses anything (C-03).
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
    ok(&mut s, orchestrator(), Command::CreateTask { plan_items: None, continues: None,
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

fn listing(mut command: Command, files: &[&str]) -> Command {
    if let Command::AskQuestion { card, .. } | Command::HoldHook { card, .. } = &mut command {
        card.files = files.iter().map(|path| (*path).to_owned()).collect();
    }
    command
}
fn owned(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| (*path).to_owned()).collect()
}

/// The orchestrator's card of 14 October, word for word where it pointed at the plan.
const POINTER: &str = "Plan completo: docs/relay-plan.md (PR #975, sin fusionar). Resultados de la prueba: docs/relay-research.md §9.\n\nPASOS (una tarea y una PR cada uno):\n0. Medir el estado actual.\n1. Relay de iroh de producción.";

#[test]
fn a_mentioned_markdown_path_is_never_read_and_never_refuses_a_card_or_a_plan() {
    let mut s = fixture();
    ok(&mut s, orchestrator(), ask_user(POINTER));
    ok(&mut s, Actor::System, plan("p1", "- [ ] Ship\n\nCreate `docs/new.md` later, after README.md"));
    assert_eq!(s.interactions[&InteractionId::from("p1")].body, "- [ ] Ship\n\nCreate `docs/new.md` later, after README.md");
    assert_eq!(include_linked_files(POINTER, |_| panic!("a mention is never read")).unwrap(), POINTER);
}

#[test]
fn only_explicit_links_to_worktree_markdown_files_are_linked() {
    let plan = "Diseño en [el diseño](docs/design.md) y [guía](./docs/guide.md#arriba \"Guía\"), [otra](<docs/a b.md>).\n\
        Sueltos: docs/plain.md, `[code](docs/code.md)`, ![img](docs/image.md), [web](https://github.com/o/r/blob/main/README.md),\n\
        [fuera](../other/notes.md), [abs](/etc/notes.md), [casa](~/notes.md), [texto](docs/notes.txt), [ancla](#arriba).\n\
        ```sh\n[inside](docs/inside-code.md)\n```\n\
        - [ ] Escribir [los resultados](docs/results.md)\n\
        - Repetido: [diseño](docs/design.md).";
    assert_eq!(linked_markdown_files(plan), ["docs/design.md", "docs/guide.md", "docs/results.md"]);
}

#[test]
fn a_linked_file_is_included_under_its_heading_and_never_adds_points_to_the_plan() {
    let body = "# Paso 1\n\nContexto en [el plan](docs/design.md).\n\n- [ ] Montar el relay\n- [ ] Escribir [los resultados](docs/results.md)";
    let file = "# Plan de conexión rápida\n\n## Pasos\n\n- [ ] Medir\n- [x] Relay\n\n```\n- [ ] in code\n";
    let included = include_linked_files(body, files(&[("docs/design.md", file)])).unwrap();
    assert!(included.starts_with(body));
    assert!(included.ends_with("\n\n---\n\n## docs/design.md\n\n### Plan de conexión rápida\n\n#### Pasos\n\n- ☐ Medir\n- ☑ Relay\n\n```\n- [ ] in code\n```"), "{included}");
    assert_eq!(checklist_items(&included), checklist_items(body));
    assert_eq!(include_linked_files(&included, files(&[])).unwrap(), included, "an included file is not included twice");
    assert_eq!(without_included_files(&included), body);
    assert_eq!(without_included_files(body), body);
}

#[test]
fn listed_files_are_included_in_order_and_come_back_apart_with_their_own_headings() {
    let body = "Lee el plan y la investigación.";
    let included = include_files(body, &owned(&["docs/a.md", "docs/b.md"]), files(&[
        ("docs/a.md", "# Plan A\n\n## Pasos\n\n```\n## no es un fichero.md\n```\n\nDetalle en docs/c.md"),
        ("docs/b.md", "Solo texto"),
    ])).unwrap();
    assert_eq!(included_files(&included), [
        ("docs/a.md".to_owned(), "# Plan A\n\n## Pasos\n\n```\n## no es un fichero.md\n```\n\nDetalle en docs/c.md".to_owned()),
        ("docs/b.md".to_owned(), "Solo texto".to_owned()),
    ]);
    assert!(included_files(body).is_empty());
    assert_eq!(include_files("", &owned(&["docs/b.md"]), files(&[("docs/b.md", "Solo texto")])).unwrap(), "---\n\n## docs/b.md\n\nSolo texto");
}

#[test]
fn a_listed_file_that_is_outside_the_worktree_unreadable_or_too_long_refuses_the_card_naming_it() {
    for outside in ["/etc/notes.md", "~/notes.md", "../notes.md", "docs/notes.txt", "https://x.dev/a.md"] {
        let error = include_files("x", &owned(&[outside]), |_| panic!("never read")).unwrap_err();
        assert!(error.contains(outside) && error.contains("not a Markdown file of your worktree"), "{error}");
    }
    let unreadable = include_files("x", &owned(&["docs/missing.md"]), files(&[])).unwrap_err();
    assert!(unreadable.contains("docs/missing.md cannot be read"), "{unreadable}");
    let long = "x".repeat(CITED_FILE_MAX_BYTES + 1);
    let too_long = include_files("x", &owned(&["docs/a.md"]), files(&[("docs/a.md", &long)])).unwrap_err();
    assert!(too_long.contains("docs/a.md") && too_long.contains("too long"), "{too_long}");
    let linked = include_linked_files("[a](docs/a.md)", files(&[("docs/a.md", &long)])).unwrap_err();
    assert!(linked.contains("too long"), "{linked}");
    let part = "x".repeat(CITED_FILE_MAX_BYTES);
    let names: Vec<String> = (0..5).map(|n| format!("d{n}.md")).collect();
    let many: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), part.as_str())).collect();
    let together = include_files("x", &names, files(&many)).unwrap_err();
    assert!(together.contains("add up"), "{together}");
    assert_eq!(include_linked_files("[nuevo](docs/new.md)", files(&[])).unwrap(), "[nuevo](docs/new.md)", "a link to a file not there yet includes nothing");
}

#[test]
fn the_domain_refuses_a_card_that_lists_a_file_it_does_not_include() {
    let mut s = fixture();
    let error = refused(&mut s, orchestrator(), listing(ask_user(POINTER), &["docs/relay-plan.md"]));
    assert_eq!((error.code.as_str(), error.subject.as_deref()), ("invalid_input", Some("card.files")));
    assert!(error.message.contains("docs/relay-plan.md"), "{}", error.message);
    let error = refused(&mut s, orchestrator(), listing(ask_user("Elige"), &["/etc/plan.md"]));
    assert_eq!(error.subject.as_deref(), Some("card.files"));
    let whole = include_files(POINTER, &owned(&["docs/relay-plan.md"]), files(&[("docs/relay-plan.md", "# Plan\n\nLos 8 pasos.")])).unwrap();
    ok(&mut s, orchestrator(), listing(ask_user(&whole), &["docs/relay-plan.md"]));
    let linked = include_linked_files("- [ ] Hacerlo según [el plan](docs/design.md)", files(&[("docs/design.md", "# Plan")])).unwrap();
    ok(&mut s, Actor::System, plan("p2", &linked));
    assert_eq!(s.interactions[&InteractionId::from("p2")].body, linked);
}

#[test]
fn listed_files_problem_checks_content_and_plan() {
    let content = include_files("", &owned(&["docs/a.md"]), files(&[("docs/a.md", "A")])).unwrap();
    let plan = include_files("- [ ] x", &owned(&["docs/b.md"]), files(&[("docs/b.md", "B")])).unwrap();
    assert_eq!(listed_files_problem(&owned(&["docs/a.md", "docs/b.md"]), &[&content, &plan]), None);
    assert!(listed_files_problem(&owned(&["docs/c.md"]), &[&content, &plan]).is_some_and(|fix| fix.contains("docs/c.md")));
    assert_eq!(listed_files_problem(&[], &["## docs/x.md"]), None);
}
