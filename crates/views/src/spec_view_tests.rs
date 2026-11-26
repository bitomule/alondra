//! The specs of a plan on its card, and the screens that edit them.
use super::*;
use alondra_domain_model::specs::{self, SpecDirs};
use alondra_domain_model::{Hold, SpecCheck, SpecDraft, SpecLevel, SpecMode, SpecProblem};

const PARTIAL_PATH: &str = "specs/billing/partial.feature";
const PARTIAL: &str = "@spec:billing.partial\nFeature: Partial refunds\n  Any amount up to what is left of the order comes back.\n\n  @id:part\n  Scenario: Part of an order\n    Given a paid order of 30 EUR\n    When 10 EUR are refunded\n    Then the customer receives 10 EUR\n";

fn dirs() -> SpecDirs {
    SpecDirs { specs: "specs".into(), decisions: Some("docs/decisions".into()), reference: None }
}

fn sheets<'a>(view: &'a View, section: &str) -> Vec<&'a SpecSheet> {
    view.sections.iter().filter(|s| s.id == section).flat_map(|s| &s.items)
        .filter_map(|item| match item { Item::SpecSheet(sheet) => Some(&**sheet), _ => None }).collect()
}

fn scenario<'a>(sheet: &'a SpecSheet, id: &str) -> &'a SheetScenario {
    sheet.all_scenarios().find(|scenario| scenario.id == id).unwrap_or_else(|| panic!("{id} is not in {sheet:?}"))
}

/// Task t-1 whose card i-1 is its plan, waiting for the user, with a proposal of one spec.
fn planned(mode: SpecMode) -> DomainState {
    let mut state = fixture();
    state.holds.insert("i-1".into(), Hold::Plan);
    let files: alondra_domain_model::SpecFiles = [(PARTIAL_PATH.to_owned(), Some(PARTIAL.into()))].into();
    let check = SpecCheck { mode, problems: vec![], before: [(PARTIAL_PATH.to_owned(), None)].into(), dirs: dirs(), ..SpecCheck::default() };
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
        Item::SpecSheet(sheet) => serde_json::to_string(sheet).unwrap(),
        Item::DecisionSheet(sheet) => serde_json::to_string(sheet).unwrap(),
        _ => String::new(),
    }).collect::<Vec<_>>().join("\n")
}

#[test]
fn the_plans_card_shows_each_spec_as_a_sheet_with_its_steps_in_lanes_never_raw_gherkin() {
    let state = planned(SpecMode::Committed);
    for (lang, edit, given, kind) in [(Lang::En, "Edit the specs", "Given", "test"), (Lang::Es, "Editar las specs", "Partiendo de", "test")] {
        let view = render(&state, &LiveSnapshot::default(), Client::Tui, lang, "card:i-1", "");
        let shown = texts(&view, "specs");
        assert!(!shown.contains("@spec:") && !shown.contains("Feature:"), "no raw Gherkin: {shown}");
        assert!(shown.contains(edit), "{shown}");
        let [sheet] = sheets(&view, "specs")[..] else { panic!("{view:?}") };
        assert_eq!((sheet.title.as_str(), sheet.spec.as_str()), ("Partial refunds", "billing.partial"));
        assert_eq!(sheet.explanation, "Any amount up to what is left of the order comes back.");
        assert_eq!(sheet.mark.as_ref().map(|mark| mark.change), Some(SheetChange::New));
        let drawn = scenario(sheet, "billing.partial/part");
        assert_eq!((drawn.kind.as_str(), drawn.lanes[0].label.as_str(), drawn.lanes.len()), (kind, given, 3));
        assert_eq!(drawn.lanes[2].steps[0].text, "the customer receives 10 EUR");
        assert_eq!(drawn.anchor.as_ref().map(|anchor| anchor.quote.as_str()), Some("billing.partial/part · Part of an order"), "a comment anchors to the scenario");
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
fn the_editor_draws_each_spec_as_a_sheet_whose_parts_open_their_forms() {
    let state = planned(SpecMode::Committed);
    let editor = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "task_specs:t-1", "");
    let [sheet] = sheets(&editor, "specs")[..] else { panic!("{editor:?}") };
    let handle = specs::scenario_handle(PARTIAL_PATH, "part");
    let drawn = scenario(sheet, "billing.partial/part");
    assert_eq!(drawn.edit, Some(Link::View { key: format!("task_specs:t-1:scenario:{handle}") }));
    assert_eq!(drawn.add.iter().map(|add| add.title.as_str()).collect::<Vec<_>>(), ["Add a diagram", "Add a mockup"]);
    let Some(Item::Facts(links)) = editor.sections.iter().find(|s| s.id == "documents").and_then(|s| s.items.first()) else { panic!("the terminal lists what it can edit") };
    assert!(links.items.iter().any(|fact| fact.value == "Part of an order" && fact.link == Some(Link::View { key: format!("task_specs:t-1:scenario:{handle}") })));
    let form = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, &format!("task_specs:t-1:scenario:{handle}"), "");
    let Some(Item::Form(form)) = form.sections[0].items.first() else { panic!() };
    let value = |id: &str| form.fields.iter().find(|field| field.id == id).map(|field| field.value.clone()).unwrap();
    assert_eq!(value("title"), "Part of an order");
    assert_eq!(value("steps"), "Given a paid order of 30 EUR\nWhen 10 EUR are refunded\nThen the customer receives 10 EUR");
    assert!(matches!(form.fields.iter().find(|field| field.id == "steps").unwrap().kind, FieldKind::Steps { .. }));
    let action = alondra_view_model::action_id::ActionId::parse(&form.submit.id).unwrap();
    assert_eq!((action.subject.as_str(), action.revision, action.extra.as_deref()), ("t-1", 7, Some(format!("sc-{handle}").as_str())), "the edit names the proposal's revision");
}

#[test]
fn a_part_form_adds_a_diagram_as_mermaid_and_a_mockup_as_an_attached_image() {
    let state = planned(SpecMode::Committed);
    let target = format!("c{}", specs::scenario_handle(PARTIAL_PATH, "part"));
    let form_of = |kind: &str| {
        let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, &format!("task_specs:t-1:part:{kind}:{target}:new"), "");
        let Some(Item::Form(form)) = view.sections[0].items.first().cloned() else { panic!("{view:?}") };
        form
    };
    let diagram = form_of("diagram");
    assert!(matches!(diagram.fields.iter().find(|field| field.id == "code").unwrap().kind, FieldKind::Mermaid { .. }));
    let extra = alondra_view_model::action_id::ActionId::parse(&diagram.submit.id).unwrap().extra.unwrap();
    assert_eq!(extra, format!("partadd-diagram-{target}"));
    let mockup = form_of("mockup");
    assert!(matches!(mockup.fields.iter().find(|field| field.id == "image").unwrap().kind, FieldKind::Attachments) && mockup.submit.attachments);

    let mut older = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, &format!("task_specs:t-1:part:diagram:{target}:new"), "");
    ClientCapabilities::default().adapt_view(&mut older, Lang::En);
    let Some(Item::Form(form)) = older.sections[0].items.first() else { panic!() };
    assert!(matches!(form.fields.iter().find(|field| field.id == "code").unwrap().kind, FieldKind::Multiline { .. }), "an app that does not know the field edits it as text");
}

/// Task t-1 whose proposal changes a spec the project has: one scenario changed, one added, the
/// others as they were, with a diagram and a mockup.
fn amended_plan(unchanged: usize) -> DomainState {
    let mut state = planned(SpecMode::Committed);
    let same: String = (0..unchanged).map(|n| format!("\n  @id:same-{n}\n  Scenario: Stays the same {n}\n    Given an order {n}\n    Then it stays\n")).collect();
    let before = format!("@spec:billing.partial\nFeature: Partial refunds\n{same}\n  @id:part\n  Scenario: Part of an order\n    Given a paid order of 30 EUR\n    Then nothing\n");
    let after = format!("@spec:billing.partial\n@diagram:flow.mmd\nFeature: Partial refunds\n{same}\n  @id:part @mockup:sheet.png\n  Scenario: Part of an order\n    Given a paid order of 30 EUR\n    And a card on file\n    When 10 EUR are refunded\n      | amount | currency |\n      | 10     | EUR      |\n    Then the customer receives 10 EUR\n    But keeps the rest\n");
    let image = alondra_domain_model::StoredImage { file: format!("files/images/{}.png", "a".repeat(64)), fingerprint: format!("sha256:{}", "a".repeat(64)), bytes: 900, width: 1170, height: 2532 };
    let draft = state.tasks.get_mut(&"t-1".into()).unwrap().specs.draft.as_mut().unwrap();
    draft.files = [(PARTIAL_PATH.to_owned(), Some(after.into()))].into();
    draft.check.before = [(PARTIAL_PATH.to_owned(), Some(before.into()))].into();
    draft.check.parts = [("specs/billing/flow.mmd".to_owned(), "flowchart LR\n  A --> B\n".into()), ("specs/billing/sheet.png".to_owned(), alondra_domain_model::SpecContent::Mockup(image))].into();
    state
}

#[test]
fn a_changed_spec_marks_each_change_joins_and_and_but_to_their_lane_and_draws_its_parts() {
    let state = amended_plan(2);
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::Es, "card:i-1", "");
    let [sheet] = sheets(&view, "specs")[..] else { panic!("{view:?}") };
    assert_eq!(sheet.mark.as_ref().map(|mark| mark.label.as_str()), Some("cambiada"));
    let drawn = scenario(sheet, "billing.partial/part");
    assert_eq!(drawn.mark.as_ref().map(|mark| mark.change), Some(SheetChange::Changed));
    assert_eq!(drawn.lanes.iter().map(|lane| (lane.label.as_str(), lane.steps.len())).collect::<Vec<_>>(), [("Partiendo de", 2), ("Cuando", 1), ("Entonces", 2)]);
    assert_eq!(drawn.lanes[1].steps[0].table[1], ["10", "EUR"]);
    let diagram = &sheet.parts[0];
    assert_eq!((diagram.title.as_str(), diagram.code.as_deref(), diagram.mark.as_ref().map(|mark| mark.change)), ("Diagrama «Flow»", Some("flowchart LR\n  A --> B\n"), Some(SheetChange::New)));
    let mockup = &drawn.parts[0];
    assert_eq!(mockup.picture.as_ref().map(|picture| (picture.width, picture.height)), Some((1170, 2532)));
    assert!(mockup.picture.as_ref().unwrap().url.starts_with("/files?path=files/images/"));
    assert_eq!(mockup.alt, "Mockup «Sheet» del escenario «Part of an order»");
    assert_eq!(mockup.mark.as_ref().map(|mark| mark.change), Some(SheetChange::New));
    assert_eq!(mockup.anchor.as_ref().map(|anchor| anchor.quote.as_str()), Some("billing.partial/part · Mockup «Sheet»"), "a comment anchors to the mockup");
    assert!(diagram.anchor.is_some(), "a comment anchors to the diagram");
    assert_eq!(sheet.unchanged.as_ref().map(|folded| (folded.label.as_str(), folded.scenarios.len())), Some(("2 escenarios sin cambios", 2)));
}

#[test]
fn a_spec_with_40_scenarios_folds_the_unchanged_ones_into_one_line_and_closes_many_changes() {
    let view = render(&amended_plan(39), &LiveSnapshot::default(), Client::Tui, Lang::En, "card:i-1", "");
    let [sheet] = sheets(&view, "specs")[..] else { panic!("{view:?}") };
    assert_eq!(sheet.scenarios.len(), 1);
    assert_eq!(sheet.unchanged.as_ref().map(|folded| folded.label.as_str()), Some("39 unchanged scenarios"));
    assert_eq!(sheet.all_scenarios().count(), 40);

    let mut new = amended_plan(39);
    new.tasks.get_mut(&"t-1".into()).unwrap().specs.draft.as_mut().unwrap().check.before = [(PARTIAL_PATH.to_owned(), None)].into();
    let view = render(&new, &LiveSnapshot::default(), Client::Tui, Lang::En, "card:i-1", "");
    let [sheet] = sheets(&view, "specs")[..] else { panic!() };
    assert_eq!(sheet.scenarios.len(), 40);
    assert!(sheet.scenarios.iter().all(|scenario| scenario.collapsed), "with many, each is one line until opened");
}

#[test]
fn a_proposal_with_no_spec_says_so_and_a_spec_with_no_scenario_too() {
    let mut state = planned(SpecMode::Committed);
    state.tasks.get_mut(&"t-1".into()).unwrap().specs.draft.as_mut().unwrap().files.clear();
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::Es, "card:i-1", "");
    assert!(texts(&view, "specs").contains("Este plan no cambia ninguna spec."));
    let mut empty = planned(SpecMode::Committed);
    empty.tasks.get_mut(&"t-1".into()).unwrap().specs.draft.as_mut().unwrap().files = [(PARTIAL_PATH.to_owned(), Some("@spec:billing.partial\nFeature: Partial refunds\n".into()))].into();
    let view = render(&empty, &LiveSnapshot::default(), Client::Tui, Lang::En, "card:i-1", "");
    let [sheet] = sheets(&view, "specs")[..] else { panic!("{view:?}") };
    assert_eq!(sheet.empty.as_deref(), Some("No scenario yet."));
}

#[test]
fn a_spec_that_cannot_be_read_says_the_file_and_the_line() {
    let mut state = planned(SpecMode::Committed);
    state.tasks.get_mut(&"t-1".into()).unwrap().specs.draft.as_mut().unwrap().files = [(PARTIAL_PATH.to_owned(), Some("@spec:billing.partial\nFeature: Partial refunds\n\nRule: nope\n".into()))].into();
    for (lang, wanted) in [(Lang::En, "specs/billing/partial.feature, line 4:"), (Lang::Es, "specs/billing/partial.feature, línea 4:")] {
        let view = render(&state, &LiveSnapshot::default(), Client::Tui, lang, "card:i-1", "");
        assert!(view.sections.iter().flat_map(|s| &s.items).any(|item| matches!(item, Item::Notice(notice) if notice.tone == Tone::Critical && notice.text.starts_with(wanted))), "{}", texts(&view, "specs"));
    }
}

#[test]
fn the_card_of_a_change_to_the_specs_draws_what_changes_as_sheets_and_why_it_is_asked() {
    use alondra_domain_model::spec_card::SpecCard;
    use alondra_domain_model::{AmendmentState, SpecAmendment, SystemCard};
    let mut state = amended_plan(0);
    let task = state.tasks.get_mut(&"t-1".into()).unwrap();
    let draft = task.specs.draft.take().unwrap();
    task.specs.amendments.push(SpecAmendment { id: "a1".into(), reason: "Refund in two steps".into(), state: AmendmentState::Proposed, files: draft.files, before: draft.check.before.clone(), check: Some(Box::new(draft.check)), card: Some("i-1".into()), words: None });
    state.holds.remove(&"i-1".into());
    let card = state.interactions.get_mut(&"i-1".into()).unwrap();
    card.author = alondra_domain_model::Author::System;
    card.system_card = Some(SystemCard::SpecChange);
    card.body = SpecCard::Amendment { task: "t-1".into(), id: "a1".into() }.to_body();
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "card:i-1", "");
    assert!(texts(&view, "content").contains("Refund in two steps"));
    assert!(!texts(&view, "content").contains("Partial refunds"), "the spec is drawn once, as its sheet");
    let [sheet] = sheets(&view, "specs")[..] else { panic!("{view:?}") };
    assert_eq!(sheet.mark.as_ref().map(|mark| mark.change), Some(SheetChange::Changed));
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
            nearest_wrong: Some("Refunding without lowering what is left".into()), evidence: vec![], captures: vec![] }],
    };
    let delivered = DeliveredValidation {
        delivery: "i-3".into(), head: "0123456789abcdef0123456789abcdef01234567".into(),
        specs: vec![DeliveredSpec { id: "billing.partial".into(), path: PARTIAL_PATH.into(), title: "Partial refunds".into(),
            scenarios: vec![scenario("part", "Part of an order", "valid"), scenario("more", "More than what is left", "stale")], source: None, base: None, parts: Default::default(), base_parts: Default::default(), decisions: Default::default() }],
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
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-3", "");
    let [sheet] = sheets(&view, "specs")[..] else { panic!("{view:?}") };
    let stale = scenario(sheet, "billing.partial/more").result.as_ref().unwrap();
    assert_eq!((stale.state.text.as_str(), stale.state.icon), ("Not validated", Icon::Warning), "a state is said in words and an icon, never only by colour");
    assert!(stale.checks.iter().any(|check| check.text.contains("out of date")));
}

/// A delivery whose spec files Alondra kept, with a judgment on a capture.
fn judged_on_a_capture(image: Option<alondra_domain_model::StoredImage>) -> DomainState {
    use alondra_domain_model::{DeliveredCapture, ValidationKind, ValidationVerdict};
    let mut state = validated();
    let delivered = &mut state.tasks.get_mut(&"t-1".into()).unwrap().validation.delivered[0];
    let spec = &mut delivered.specs[0];
    spec.source = Some("@spec:billing.partial\nFeature: Partial refunds\n\n  @id:part @judged:capture\n  Scenario: Part of an order\n    Then the sheet shows what is left\n".into());
    spec.scenarios.truncate(1);
    let scenario = &mut spec.scenarios[0];
    scenario.judged = Some("capture".into());
    scenario.states = vec![("judged".into(), "valid".into())];
    scenario.verdicts = vec![alondra_domain_model::DeliveredVerdict { kind: ValidationKind::Judged, verdict: ValidationVerdict::Met, reasoning: Some("The sheet shows 20 EUR".into()), nearest_wrong: None,
        evidence: vec!["docs/captures/sheet.png".into()], captures: image.into_iter().map(|image| DeliveredCapture { path: "docs/captures/sheet.png".into(), image: Some(image) }).collect() }];
    state
}

#[test]
fn a_delivery_shows_the_kept_captures_and_says_when_the_evidence_is_not_available() {
    let image = alondra_domain_model::StoredImage { file: format!("files/images/{}.png", "b".repeat(64)), fingerprint: format!("sha256:{}", "b".repeat(64)), bytes: 900, width: 390, height: 844 };
    for (state, expected, unavailable) in [(judged_on_a_capture(Some(image)), "Validated", false), (judged_on_a_capture(None), "Evidence not available", true)] {
        let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "card:i-3", "");
        let [sheet] = sheets(&view, "specs")[..] else { panic!("{view:?}") };
        let result = scenario(sheet, "billing.partial/part").result.as_ref().unwrap();
        assert_eq!(result.state.text, expected);
        let evidence = &result.verdicts[0].evidence[0];
        assert_eq!(evidence.picture.is_none(), unavailable);
        assert_eq!(evidence.unavailable.is_some(), unavailable);
        assert!(result.verdicts[0].line.text.starts_with("Judgment · "), "a verdict is always labelled a judgment");
    }
}

#[test]
fn a_scenario_a_new_round_validates_says_it_is_being_validated() {
    use alondra_domain_model::{RoundItem, RoundState, ValidationKind, ValidationRound, ValidationState};
    let mut state = judged_on_a_capture(None);
    let commit = "0123456789abcdef0123456789abcdef01234567".to_owned();
    state.tasks.get_mut(&"t-1".into()).unwrap().validation.rounds.push(ValidationRound { number: 2, head: commit.clone(), implemented: commit,
        items: vec![RoundItem { subject: "billing.partial/part".into(), kind: ValidationKind::Judged, inputs: "sha256:0".into(), state: ValidationState::Running, reason: None }],
        scope: vec![], state: RoundState::Validating, generation: Some(3), findings: None, retried: false });
    for (lang, wanted) in [(Lang::En, "Validating now"), (Lang::Es, "Validando ahora")] {
        let view = render(&state, &LiveSnapshot::default(), Client::Tui, lang, "card:i-3", "");
        let [sheet] = sheets(&view, "specs")[..] else { panic!() };
        assert_eq!(scenario(sheet, "billing.partial/part").result.as_ref().unwrap().state.text, wanted);
    }
}

#[test]
fn a_delivery_says_when_its_validation_runs_or_its_evidence_is_not_available() {
    use alondra_domain_model::{RoundState, ValidationRound};
    let mut state = fixture();
    let round = |state| ValidationRound { number: 1, head: "0123456789abcdef0123456789abcdef01234567".into(), implemented: "0123456789abcdef0123456789abcdef01234567".into(), items: vec![], scope: vec![], state, generation: Some(2), findings: None, retried: false };
    state.tasks.get_mut(&"t-1".into()).unwrap().validation.rounds.push(round(RoundState::Validating));
    let running = texts(&render(&state, &LiveSnapshot::default(), Client::Tui, Lang::Es, "card:i-3", ""), "specs");
    assert!(running.contains("Un validador está comprobando"), "{running}");
    state.tasks.get_mut(&"t-1".into()).unwrap().validation.rounds = vec![round(RoundState::Delivered)];
    let gone = texts(&render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "card:i-3", ""), "specs");
    assert!(gone.contains("Evidence not available"), "{gone}");
    let plain = fixture();
    assert!(render(&plain, &LiveSnapshot::default(), Client::Tui, Lang::En, "card:i-3", "").sections.iter().all(|s| s.id != "specs"), "a task without specs shows nothing new");
}
