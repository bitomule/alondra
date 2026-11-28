//! The task flow, the views suite of the component `task-flow`: what the cards of a task say of
//! who proposed and who validated its specs.
use super::*;
use alondra_domain_model::specs::SpecDirs;
use alondra_domain_model::{DeliveredScenario, DeliveredSpec, DeliveredValidation, Hold, SpecCheck, SpecDraft, SpecLevel, SpecMode};

const REFUNDS_PATH: &str = "specs/billing/refunds.feature";
const REFUNDS: &str = "@spec:billing.refunds\nFeature: Refunds return the money\n\n  @id:full\n  Scenario: Full refund\n    Given a paid order of 30 EUR\n    When the order is refunded\n    Then the customer receives 40 EUR\n";

fn shown(view: &View, section: &str) -> String {
    view.sections.iter().filter(|s| s.id == section).flat_map(|s| &s.items).map(|item| match item {
        Item::Markdown(markdown) => markdown.text.clone(),
        Item::Notice(notice) => notice.text.clone(),
        Item::SpecSheet(sheet) => serde_json::to_string(sheet).unwrap(),
        _ => String::new(),
    }).collect::<Vec<_>>().join("\n")
}

/// Task t-1, without stages unless `staged`, whose final delivery i-3 passed its validation.
fn delivered(staged: bool) -> DomainState {
    let mut state = fixture();
    let task = state.tasks.get_mut(&"t-1".into()).unwrap();
    task.stages.staged = staged;
    task.validation.delivered.push(DeliveredValidation {
        delivery: "i-3".into(), head: "0123456789abcdef0123456789abcdef01234567".into(),
        specs: vec![DeliveredSpec { id: "billing.refunds".into(), path: REFUNDS_PATH.into(), title: "Refunds return the money".into(),
            scenarios: vec![DeliveredScenario { id: "billing.refunds/full".into(), title: "Full refund".into(), steps: vec![], states: vec![("passes".into(), "valid".into())],
                tests: vec![], judged: None, verdicts: vec![] }],
            source: None, base: None, parts: Default::default(), base_parts: Default::default(), decisions: Default::default() }],
        suites: vec![], outside: vec![], sections: vec![],
    });
    state
}

// spec: task-flow.specs-without-stages/delivery-says-same-agent
#[test]
fn the_delivery_of_a_task_without_stages_says_the_agent_that_did_the_work_validated_it() {
    let state = delivered(false);
    for (lang, said) in [(Lang::En, "Validated by the same agent that did the work"), (Lang::Es, "Validado por el mismo agente que hizo el trabajo")] {
        let specs = shown(&render(&state, &LiveSnapshot::default(), Client::Tui, lang, "card:i-3", ""), "specs");
        assert!(specs.contains(said), "{lang:?} lacks «{said}»: {specs}");
        assert!(!specs.contains("separate agent") && !specs.contains("otro agente"), "{specs}");
    }
}

// spec: task-flow.specs-without-stages/delivery-says-separate-agent
#[test]
fn the_delivery_of_a_task_in_stages_says_a_separate_agent_validated_it() {
    let state = delivered(true);
    for (lang, said) in [(Lang::En, "Validated by a separate agent"), (Lang::Es, "Validado por otro agente")] {
        let specs = shown(&render(&state, &LiveSnapshot::default(), Client::Tui, lang, "card:i-3", ""), "specs");
        assert!(specs.contains(said), "{lang:?} lacks «{said}»: {specs}");
        assert!(!specs.contains("same agent") && !specs.contains("mismo agente"), "{specs}");
    }
}

// spec: task-flow.specs-without-stages/proposes-without-stages
#[test]
fn the_plan_of_a_task_without_stages_shows_the_specs_its_agent_proposed() {
    let mut state = fixture();
    state.holds.insert("i-1".into(), Hold::Plan);
    let dirs = SpecDirs { specs: "specs".into(), decisions: None, reference: None };
    let check = SpecCheck { mode: SpecMode::Committed, problems: vec![], before: [(REFUNDS_PATH.to_owned(), None)].into(), dirs, ..SpecCheck::default() };
    let task = state.tasks.get_mut(&"t-1".into()).unwrap();
    assert!(!task.stages.staged);
    task.specs.draft = Some(SpecDraft { level: SpecLevel::Light, files: [(REFUNDS_PATH.to_owned(), Some(REFUNDS.into()))].into(), check, revision: 7, edited: false });
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "card:i-1", "");
    let section = view.sections.iter().find(|s| s.id == "specs").expect("the plan's card has a section of its specs");
    assert_eq!(section.title.as_deref(), Some("Specs"));
    let sheets: Vec<&SpecSheet> = section.items.iter().filter_map(|item| match item { Item::SpecSheet(sheet) => Some(&**sheet), _ => None }).collect();
    let [sheet] = sheets[..] else { panic!("{view:?}") };
    assert_eq!((sheet.spec.as_str(), sheet.title.as_str()), ("billing.refunds", "Refunds return the money"), "the sheet of {REFUNDS_PATH}");
}
