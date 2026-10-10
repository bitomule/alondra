//! A task's scope is the checklist of the plan the user approved: its final delivery marks every
//! point done with evidence, or the domain refuses it.
use super::*;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestError, TestRng, TestRunner};
use std::collections::{BTreeMap, BTreeSet};

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}
fn agent() -> Actor {
    Actor::Agent { agent_id: "a".into() }
}
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
/// The rejection's code and message; the state must not move.
fn refused(s: &mut DomainState, a: Actor, c: Command) -> (String, String) {
    let before = s.clone();
    let error = exec(s, a, c).unwrap_err();
    assert_eq!(*s, before, "a rejected command changed state");
    (error.code, error.message)
}
fn fixture() -> DomainState {
    let mut s = DomainState::new("epoch");
    ok(&mut s, Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "Node A".into(), at: 0 });
    ok(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Build it".into(), auto_merge: false, maintenance: false, message: false });
    ok(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask {
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
        card: CardSpec::new(CardKind::Review, "Build it").content(ContentKind::Plan, body),
    }
}
fn answer(id: &str, option: &str) -> Command {
    let inputs = if option == Hold::REQUEST_CHANGES {
        BTreeMap::from([(Hold::NOTE.to_owned(), InputValue::Text("Split it".into()))])
    } else {
        BTreeMap::new()
    };
    Command::AnswerQuestion { attachments: vec![], interaction_id: id.into(), option_id: option.into(), inputs }
}
fn approved(s: &mut DomainState, id: &str, body: &str) {
    ok(s, Actor::System, plan(id, body));
    ok(s, user(), answer(id, Hold::APPROVE));
}
fn commit(value: &str) -> Evidence {
    Evidence { kind: EvidenceKind::Commit, value: value.into(), detail: None }
}
fn mark(item: &str, evidence: Vec<Evidence>) -> ChecklistMark {
    ChecklistMark { item: item.into(), evidence }
}
fn deliver(id: &str, is_final: bool, checklist: Vec<ChecklistMark>, attachments: Vec<AttachmentRef>) -> Command {
    let targets = vec![DeliveryTarget::Text { text: "Done".into() }];
    Command::PresentResult {
        checklist, attachments, task_id: "t".into(), interaction_id: id.into(), body: "Done".into(),
        card: CardSpec::delivery("Done", &targets, is_final), targets, is_final, nothing_to_review: false,
    }
}
fn texts(s: &DomainState) -> Vec<String> {
    s.checklists.get(&"t".into()).map(|c| c.items.iter().map(|i| i.text.clone()).collect()).unwrap_or_default()
}
const PLAN: &str = "# Build it\n\n- [ ] Parse the plan\n- [ ] Refuse an incomplete delivery\n- [ ] Show it to the user\n";

#[test]
fn a_plan_without_a_checklist_never_reaches_the_user() {
    let mut s = fixture();
    let (code, message) = refused(&mut s, Actor::System, plan("p", "# Build it\n\n1. Parse it\n2. Ship it"));
    assert_eq!(code, "plan_without_checklist");
    assert!(message.contains("`- [ ] …`"), "{message}");
    let long = (0..=CHECKLIST_MAX_ITEMS).map(|i| format!("- [ ] Point {i}\n")).collect::<String>();
    assert_eq!(refused(&mut s, Actor::System, plan("p", &long)).0, "plan_checklist_too_long");
    let wordy = format!("- [ ] {}", "x".repeat(CHECKLIST_MAX_TEXT + 1));
    assert_eq!(refused(&mut s, Actor::System, plan("p", &wordy)).0, "plan_checklist_too_long");
}

#[test]
fn approving_a_plan_makes_its_points_the_task_s_scope_and_a_later_approved_plan_replaces_them() {
    let mut s = fixture();
    ok(&mut s, Actor::System, plan("p1", PLAN));
    assert!(texts(&s).is_empty(), "a plan waiting for the user is not the scope yet");
    ok(&mut s, user(), answer("p1", Hold::REQUEST_CHANGES));
    assert!(texts(&s).is_empty(), "a plan sent back is not the scope");
    approved(&mut s, "p2", PLAN);
    assert_eq!(texts(&s), ["Parse the plan", "Refuse an incomplete delivery", "Show it to the user"]);
    assert_eq!(s.checklists[&"t".into()].plan, "p2".into());
    approved(&mut s, "p3", "- [ ] Parse the plan\n- [ ] Ship it\n");
    assert_eq!(texts(&s), ["Parse the plan", "Ship it"]);
    ok(&mut s, Actor::System, plan("p4", "- [ ] Something else\n"));
    ok(&mut s, user(), answer("p4", Hold::REQUEST_CHANGES));
    assert_eq!(texts(&s), ["Parse the plan", "Ship it"], "a revised plan sent back leaves the approved scope as it was");
}

#[test]
fn a_final_delivery_that_leaves_points_undone_is_refused_naming_them() {
    let mut s = fixture();
    approved(&mut s, "p", PLAN);
    let (code, message) = refused(&mut s, agent(), deliver("d", true, vec![], vec![]));
    assert_eq!(code, "checklist_incomplete");
    assert!(message.contains("1 “Parse the plan”, 2 “Refuse an incomplete delivery”, 3 “Show it to the user”"), "{message}");
    assert!(message.contains("ExitPlanMode"), "it says how to change the scope: {message}");
    let (code, message) = refused(&mut s, agent(), deliver("d", true, vec![mark("1", vec![commit("3f2a1c9")]), mark("3", vec![commit("3f2a1c9")])], vec![]));
    assert_eq!(code, "checklist_incomplete");
    assert!(message.contains("3 points and these are not marked done: 2 “Refuse an incomplete delivery”."), "{message}");
}

#[test]
fn a_final_delivery_that_proves_every_point_is_accepted_and_keeps_its_marks() {
    let mut s = fixture();
    approved(&mut s, "p", PLAN);
    let shot = AttachmentRef { id: "att-1".into(), name: "delivery.png".into(), content_digest: "a".repeat(64), content_reference: "files/delivery.png".into(), bytes: 1 };
    let marks = vec![
        mark("2", vec![Evidence { kind: EvidenceKind::Test, value: "bazel test //crates/core:alondra_core_test".into(), detail: Some("713 passed".into()) }]),
        mark("1", vec![commit("3f2a1c9")]),
        mark("3", vec![Evidence { kind: EvidenceKind::Screenshot, value: "att-1".into(), detail: None }]),
    ];
    ok(&mut s, agent(), deliver("d", true, marks.clone(), vec![shot]));
    assert_eq!(s.checklist_marks[&"d".into()], marks);
    assert_eq!(s.tasks[&"t".into()].final_presentation, Some("d".into()));
}

#[test]
fn every_mark_names_a_point_of_the_plan_once_with_evidence_that_can_be_checked() {
    let mut s = fixture();
    approved(&mut s, "p", "- [ ] Parse the plan\n");
    let refused_with = |s: &mut DomainState, marks: Vec<ChecklistMark>| refused(s, agent(), deliver("d", true, marks, vec![]));
    let (code, message) = refused_with(&mut s, vec![mark("1", vec![commit("3f2a1c9")]), mark("7", vec![commit("3f2a1c9")])]);
    assert_eq!((code.as_str(), message.as_str()), ("checklist_unknown_item", "checklist: point 7 is not in the approved plan (points 1–1)"));
    assert_eq!(refused_with(&mut s, vec![mark("1", vec![commit("3f2a1c9")]), mark("1", vec![commit("3f2a1c9")])]).0, "checklist_unknown_item");
    assert_eq!(refused_with(&mut s, vec![mark("1", vec![])]).0, "checklist_without_evidence");
    assert_eq!(refused_with(&mut s, vec![mark("1", vec![commit("  ")])]).0, "checklist_without_evidence");
    assert_eq!(refused_with(&mut s, vec![mark("1", vec![commit("main")])]).0, "invalid_input");
    assert_eq!(refused_with(&mut s, vec![mark("1", vec![commit("3f2a1c9"); EVIDENCE_MAX_PER_ITEM + 1])]).0, "invalid_input");
    let shot = Evidence { kind: EvidenceKind::Screenshot, value: "att-9".into(), detail: None };
    assert_eq!(refused_with(&mut s, vec![mark("1", vec![shot])]).0, "evidence_unknown_attachment");
    let pr = Evidence { kind: EvidenceKind::Pr, value: "pr-1".into(), detail: None };
    assert_eq!(refused_with(&mut s, vec![mark("1", vec![pr])]).0, "evidence_unknown_pr");
}

#[test]
fn a_task_without_an_approved_plan_delivers_as_before_and_carries_no_checklist() {
    let mut s = fixture();
    let (code, message) = refused(&mut s, agent(), deliver("d", true, vec![mark("1", vec![commit("3f2a1c9")])], vec![]));
    assert_eq!((code.as_str(), message.as_str()), ("checklist_without_plan", "checklist: this task has no approved plan: leave checklist out"));
    ok(&mut s, agent(), deliver("d", true, vec![], vec![]));
    assert!(s.checklist_marks.is_empty());
}

#[test]
fn only_the_final_delivery_marks_points_done() {
    let mut s = fixture();
    approved(&mut s, "p", PLAN);
    ok(&mut s, agent(), deliver("progress", false, vec![], vec![]));
    assert_eq!(refused(&mut s, agent(), deliver("progress-2", false, vec![mark("1", vec![commit("3f2a1c9")])], vec![])).0, "checklist_not_final");
}

#[derive(Clone, Debug)]
enum Step {
    SendPlan(usize),
    Answer { approve: bool },
    Deliver(Vec<(usize, bool)>),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        2 => (0usize..4).prop_map(Step::SendPlan),
        2 => any::<bool>().prop_map(|approve| Step::Answer { approve }),
        3 => prop::collection::vec((0usize..4, prop::bool::weighted(0.85)), 0..5).prop_map(Step::Deliver),
    ]
}

/// What the rules must do, told apart from how they do it: the scope is the last approved plan's
/// points, and a final delivery stands only when it proves each of them once.
#[derive(Default)]
struct Model {
    scope: Option<usize>,
    waiting: Option<(String, usize)>,
    sent: usize,
    delivered: usize,
}

impl Model {
    fn delivery_stands(&self, marks: &[(usize, bool)]) -> bool {
        let Some(points) = self.scope else { return marks.is_empty() };
        let mut seen = BTreeSet::new();
        marks.iter().all(|(point, proven)| *point < points && *proven && seen.insert(*point)) && seen.len() == points
    }
}

fn run(steps: &[Step]) -> Result<(), String> {
    let mut s = fixture();
    let mut model = Model::default();
    for step in steps {
        match step {
            Step::SendPlan(points) => {
                model.sent += 1;
                let id = format!("p{}", model.sent);
                let body = (1..=*points).map(|n| format!("- [ ] Point {n} of {id}\n")).collect::<String>();
                let outcome = exec(&mut s, Actor::System, plan(&id, &format!("# Plan\n\n{body}")));
                match (outcome, *points) {
                    (Err(error), 0) if error.code == "plan_without_checklist" => {}
                    (Ok(_), n) if n > 0 => model.waiting = Some((id, n)),
                    (outcome, n) => return Err(format!("a plan with {n} points gave {outcome:?}")),
                }
            }
            Step::Answer { approve } => {
                let Some((id, points)) = model.waiting.take() else { continue };
                exec(&mut s, user(), answer(&id, if *approve { Hold::APPROVE } else { Hold::REQUEST_CHANGES })).map_err(|e| e.message)?;
                if *approve {
                    model.scope = Some(points);
                }
            }
            Step::Deliver(picks) => {
                model.delivered += 1;
                let marks = picks.iter()
                    .map(|(point, proven)| mark(&(point + 1).to_string(), vec![commit(if *proven { "3f2a1c9" } else { " " })]))
                    .collect();
                let before = s.clone();
                let outcome = exec(&mut s, agent(), deliver(&format!("d{}", model.delivered), true, marks, vec![]));
                match outcome {
                    Err(error) if model.waiting.is_some() && error.code == "blocking_open" => {}
                    Err(_) if !model.delivery_stands(picks) => {}
                    Ok(_) if model.waiting.is_none() && model.delivery_stands(picks) => {}
                    outcome => return Err(format!("delivering {picks:?} against a scope of {:?} gave {outcome:?}", model.scope)),
                }
                if s.revision == before.revision && s != before {
                    return Err("a refused delivery changed state".into());
                }
            }
        }
        let scope = s.checklists.get(&"t".into()).map(|c| c.items.len());
        if scope != model.scope {
            return Err(format!("the task's scope has {scope:?} points, the last approved plan {:?}", model.scope));
        }
        s.validate().map_err(|e| e.message)?;
    }
    Ok(())
}

#[test]
fn the_scope_is_always_the_last_approved_plan_and_a_final_delivery_stands_only_when_it_proves_every_point() {
    let config = Config { cases: 256, failure_persistence: None, ..Config::default() };
    let mut runner = TestRunner::new_with_rng(config, TestRng::deterministic_rng(RngAlgorithm::ChaCha));
    let result = runner.run(&prop::collection::vec(step(), 1..30), |steps| run(&steps).map_err(TestCaseError::fail));
    if let Err(TestError::Fail(reason, steps)) = result {
        panic!("{reason}\nsmallest run: {steps:?}");
    }
    result.unwrap();
}
