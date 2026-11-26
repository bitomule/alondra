//! The specs of a plan on its card, and the screens that edit them.
use super::*;
use alondra_domain_model::specs::{self, SpecDirs};
use alondra_domain_model::{Hold, SpecCheck, SpecDraft, SpecLevel, SpecMode, SpecProblem};

const PARTIAL_PATH: &str = "specs/billing/partial.feature";
const PARTIAL: &str = "@spec:billing.partial\nFeature: Partial refunds\n  Any amount up to what is left of the order comes back.\n\n  @id:part\n  Scenario: Part of an order\n    Given a paid order of 30 EUR\n    When 10 EUR are refunded\n    Then the customer receives 10 EUR\n";

/// Task t-1 whose card i-1 is its plan, waiting for the user, with a proposal of one spec.
fn planned(mode: SpecMode) -> DomainState {
    let mut state = fixture();
    state.holds.insert("i-1".into(), Hold::Plan);
    let files: alondra_domain_model::SpecFiles = [(PARTIAL_PATH.to_owned(), Some(PARTIAL.to_owned()))].into();
    let check = SpecCheck { mode, problems: vec![], before: [(PARTIAL_PATH.to_owned(), None)].into(), dirs: SpecDirs { specs: "specs".into(), decisions: Some("docs/decisions".into()), reference: None } };
    state.tasks.get_mut(&"t-1".into()).unwrap().specs.draft = Some(SpecDraft { level: SpecLevel::Light, files, check, revision: 7, edited: false });
    state
}

fn texts(view: &View, section: &str) -> String {
    view.sections.iter().filter(|s| s.id == section).flat_map(|s| &s.items).map(|item| match item {
        Item::Markdown(markdown) => markdown.text.clone(),
        Item::Notice(notice) => notice.text.clone(),
        Item::Actions(bar) => bar.actions.iter().map(|a| a.label.clone()).collect::<Vec<_>>().join(" "),
        Item::Row(row) => format!("{} {}", row.title, row.children.iter().map(|c| c.title.clone()).collect::<Vec<_>>().join(" ")),
        Item::Facts(facts) => facts.items.iter().map(|f| format!("{}: {}", f.label, f.value)).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }).collect::<Vec<_>>().join("\n")
}

#[test]
fn the_plans_card_shows_each_spec_as_its_explanation_and_scenarios_never_raw_gherkin() {
    let state = planned(SpecMode::Committed);
    for (lang, edit) in [(Lang::En, "Edit the specs"), (Lang::Es, "Editar las specs")] {
        let view = render(&state, &LiveSnapshot::default(), Client::Tui, lang, "card:i-1", "");
        let shown = texts(&view, "specs");
        assert!(shown.contains("Partial refunds") && shown.contains("Any amount up to what is left"), "{shown}");
        assert!(shown.contains("Part of an order") && shown.contains("`billing.partial/part`"), "{shown}");
        assert!(!shown.contains("@spec:") && !shown.contains("Feature:"), "no raw Gherkin: {shown}");
        assert!(shown.contains(edit), "{shown}");
        let scenario = view.sections.iter().find(|s| s.id == "specs").unwrap().items.iter()
            .find_map(|item| match item { Item::Markdown(m) if m.text.contains("Part of an order") => Some(m), _ => None }).unwrap();
        assert!(scenario.commentable, "a comment anchors to the scenario's own block");
    }
}

#[test]
fn a_plan_whose_specs_or_settings_cannot_be_read_cannot_be_approved_and_says_why() {
    let mut state = planned(SpecMode::Committed);
    state.tasks.get_mut(&"t-1".into()).unwrap().specs.settings = Some(SpecProblem::new("invalid_settings", "alondra.toml", 3, "unknown key `surprise`"));
    let render_context_block = |lang| {
        let live = LiveSnapshot::default();
        let index = alondra_domain_model::ReadIndex::build(&state);
        let context = RenderContext { domain: &state, index: &index, live: &live, client: Client::Ios, lang, transport: Transport::Loopback, cursor: "", rendered_at: "" };
        crate::specs::plan_block(&state.interactions[&"i-1".into()], &context)
    };
    let why = render_context_block(Lang::Es).expect("the plan cannot be approved");
    assert!(why.contains("alondra.toml:3") && why.starts_with("No se puede leer"), "{why}");
    assert!(render_context_block(Lang::En).is_some_and(|why| why.contains("cannot be read")));
    let detail = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "task:t-1", "");
    assert!(texts(&detail, "specs").contains("alondra.toml no se puede leer"), "the task shows it as its state");
}

#[test]
fn specs_kept_in_alondra_say_nothing_can_be_regenerated_from_them() {
    let state = planned(SpecMode::Uncommitted);
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "card:i-1", "");
    assert!(texts(&view, "specs").contains("nothing can be regenerated from them later"));
}

#[test]
fn a_plan_without_specs_has_no_specs_section() {
    let mut state = fixture();
    state.holds.insert("i-1".into(), Hold::Plan);
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "card:i-1", "");
    assert!(view.sections.iter().all(|s| s.id != "specs"));
}

#[test]
fn the_editor_lists_specs_with_their_scenarios_folded_and_opens_their_forms() {
    let state = planned(SpecMode::Committed);
    let editor = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "task_specs:t-1", "");
    let Some(Item::Row(spec)) = editor.sections.iter().find(|s| s.id == "specs").and_then(|s| s.items.first()) else { panic!("{editor:?}") };
    assert_eq!(spec.title, "Partial refunds");
    assert!(spec.folded, "its scenarios start folded under it");
    assert_eq!(spec.children[0].title, "Part of an order");
    let scenario = specs::scenario_handle(PARTIAL_PATH, "part");
    assert_eq!(spec.children[0].link, Some(Link::View { key: format!("task_specs:t-1:scenario:{scenario}") }));
    let Some(Item::Facts(links)) = editor.sections.iter().find(|s| s.id == "documents").and_then(|s| s.items.first()) else { panic!("the terminal lists what it can edit") };
    assert!(links.items.iter().any(|fact| fact.value == "Part of an order" && fact.link == Some(Link::View { key: format!("task_specs:t-1:scenario:{scenario}") })));
    let form = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, &format!("task_specs:t-1:scenario:{scenario}"), "");
    let Some(Item::Form(form)) = form.sections[0].items.first() else { panic!() };
    let value = |id: &str| form.fields.iter().find(|field| field.id == id).map(|field| field.value.clone()).unwrap();
    assert_eq!(value("title"), "Part of an order");
    assert_eq!(value("steps"), "Given a paid order of 30 EUR\nWhen 10 EUR are refunded\nThen the customer receives 10 EUR");
    let action = alondra_view_model::action_id::ActionId::parse(&form.submit.id).unwrap();
    assert_eq!((action.subject.as_str(), action.revision, action.extra.as_deref()), ("t-1", 7, Some(format!("sc-{scenario}").as_str())), "the edit names the proposal's revision");
}

#[test]
fn once_the_plan_is_decided_its_specs_no_longer_change_on_the_editor() {
    let mut state = planned(SpecMode::Committed);
    if let InteractionPayload::Question { answer, .. } = &mut state.interactions.get_mut(&"i-1".into()).unwrap().payload {
        *answer = Some(alondra_domain_model::Answer { attachments: vec![], option_id: "approve".into(), inputs: Default::default(), responder: "u".into(), at_revision: 9, decision: None });
    }
    let editor = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "task_specs:t-1", "");
    assert!(editor.sections.iter().all(|s| s.id != "documents"), "nothing left to open for editing");
    let level = editor.sections.iter().flat_map(|s| &s.items).find_map(|item| match item {
        Item::Actions(bar) => bar.actions.iter().find(|a| a.label == "Change the level").cloned(),
        _ => None,
    }).unwrap();
    assert!(!level.enabled && level.disabled_reason.is_some());
}

/// Task t-1 whose final delivery i-3 a validator delivered, with what Alondra read of its specs.
fn validated() -> DomainState {
    use alondra_domain_model::{DeclaringTest, DeliveredScenario, DeliveredSpec, DeliveredSuite, DeliveredValidation, DeliveredVerdict, ValidationKind, ValidationVerdict};
    let mut state = fixture();
    let scenario = |id: &str, title: &str, adequate: &str| DeliveredScenario {
        id: format!("billing.partial/{id}"), title: title.into(), steps: vec!["Given a paid order of 30 EUR".into(), "When 10 EUR are refunded".into()],
        states: vec![("passes".into(), "valid".into()), ("adequate".into(), adequate.into())],
        tests: vec![DeclaringTest { path: "src/billing/tests/refunds.rs".into(), line: 1, name: Some("refunds_part_of_an_order".into()), suites: vec!["billing/unit".into()] }],
        judged: None,
        verdicts: vec![DeliveredVerdict { kind: ValidationKind::Adequate, verdict: ValidationVerdict::Met, reasoning: Some("The test checks what comes back and what is left".into()),
            nearest_wrong: Some("Refunding without lowering what is left".into()), evidence: vec![] }],
    };
    let delivered = DeliveredValidation {
        delivery: "i-3".into(), head: "0123456789abcdef0123456789abcdef01234567".into(),
        specs: vec![DeliveredSpec { id: "billing.partial".into(), path: PARTIAL_PATH.into(), title: "Partial refunds".into(),
            scenarios: vec![scenario("part", "Part of an order", "valid"), scenario("more", "More than what is left", "stale")] }],
        suites: vec![DeliveredSuite { id: "billing/unit".into(), state: "valid".into(), verdict: Some(ValidationVerdict::Met), output: Some("test result: ok. 3 passed".into()) }],
        outside: vec!["README.md".into()],
        sections: vec![],
    };
    let task = state.tasks.get_mut(&"t-1".into()).unwrap();
    task.validation.delivered.push(delivered);
    state.checklists.insert("t-1".into(), alondra_domain_model::Checklist { plan: "i-1".into(), items: vec![alondra_domain_model::ChecklistItem { id: "1".into(), text: "Refund part of an order".into() }] });
    state.checklist_marks.insert("i-3".into(), vec![alondra_domain_model::ChecklistMark { item: "1".into(),
        evidence: vec![alondra_domain_model::Evidence { kind: alondra_domain_model::EvidenceKind::Scenario, value: "billing.partial/part".into(), detail: None }] }]);
    state
}

#[test]
fn a_validated_delivery_shows_each_scenario_with_its_test_its_suite_and_the_judgment_labelled_as_one() {
    let state = validated();
    for (lang, count, judgment, outside, test) in [
        (Lang::En, "1 of 2 scenarios validated", "Judgment · its test checks it · judgment: met", "1 file changed outside the components", "Test · src/billing/tests/refunds.rs:1 · refunds_part_of_an_order"),
        (Lang::Es, "1 de 2 escenarios validados", "Juicio · su test lo comprueba · juicio: se cumple", "1 archivo cambiado fuera de los componentes", "Test · src/billing/tests/refunds.rs:1 · refunds_part_of_an_order"),
    ] {
        let view = render(&state, &LiveSnapshot::default(), Client::Tui, lang, "card:i-3", "");
        let shown = texts(&view, "specs");
        for wanted in [count, "Part of an order", judgment, outside, test, "README.md", "billing/unit"] {
            assert!(shown.contains(wanted), "{lang:?} lacks «{wanted}»: {shown}");
        }
        let order: Vec<&str> = view.sections.iter().map(|s| s.id.as_str()).collect();
        assert!(order.iter().position(|s| *s == "specs") < order.iter().position(|s| *s == "plan"), "specs come right after the card: {order:?}");
        let plan = texts(&view, "plan");
        assert!(plan.contains("billing.partial/part"), "{plan}");
    }
    let specs = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-3", "");
    let rows: Vec<&Row> = specs.sections.iter().filter(|s| s.id == "specs").flat_map(|s| &s.items).filter_map(|item| match item { Item::Row(row) => Some(row), _ => None }).collect();
    let stale = rows.iter().find(|row| row.title == "More than what is left").unwrap();
    assert!(stale.subtitle.as_deref().is_some_and(|s| s.starts_with("Not validated") && s.contains("out of date")), "a state is said in words, never only by colour or icon: {:?}", stale.subtitle);
}

#[test]
fn a_delivery_says_when_its_validation_runs_or_its_evidence_is_not_available() {
    use alondra_domain_model::{RoundState, ValidationRound};
    let mut state = fixture();
    let round = |state| ValidationRound { number: 1, head: "0123456789abcdef0123456789abcdef01234567".into(), items: vec![], scope: vec![], state, generation: Some(2), findings: None, retried: false };
    state.tasks.get_mut(&"t-1".into()).unwrap().validation.rounds.push(round(RoundState::Validating));
    let running = texts(&render(&state, &LiveSnapshot::default(), Client::Tui, Lang::Es, "card:i-3", ""), "specs");
    assert!(running.contains("Un validador está comprobando"), "{running}");
    state.tasks.get_mut(&"t-1".into()).unwrap().validation.rounds = vec![round(RoundState::Delivered)];
    let gone = texts(&render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "card:i-3", ""), "specs");
    assert!(gone.contains("Evidence not available"), "{gone}");
    let plain = fixture();
    assert!(render(&plain, &LiveSnapshot::default(), Client::Tui, Lang::En, "card:i-3", "").sections.iter().all(|s| s.id != "specs"), "a task without specs shows nothing new");
}
