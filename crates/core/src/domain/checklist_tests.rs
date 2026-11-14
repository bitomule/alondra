//! The approved checklist stays owed through PR review, merge and session recovery; only a
//! final delivery with evidence for every point can complete the task.
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
    ok(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
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

const RELEASE_PLAN: &str = "- [ ] Implement the release\n- [ ] Test the release\n- [ ] Prepare the pull request\n- [ ] Publish the release\n- [ ] Install the release\n- [ ] Report the installed version\n";

fn release_pr() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::PullRequest { pr_id: "release-pr".into(), head: "abcdef0".into() }]
}

fn release_task() -> DomainState {
    let mut s = fixture();
    approved(&mut s, "release-plan", RELEASE_PLAN);
    ok(&mut s, agent(), Command::StartTask { task_id: "t".into() });
    ok(&mut s, agent(), Command::RegisterPr {
        task_id: "t".into(), pr_id: "release-pr".into(), host: "github.com".into(), repository: "acme/build-tool".into(), number: 49,
        head: "abcdef0".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    s
}

fn release_delivery() -> Command {
    let mut ready = deliver("release-review", true, (1..=3).map(|n| mark(&n.to_string(), vec![commit("abcdef0")])).collect(), vec![]);
    if let Command::PresentResult { targets, .. } = &mut ready { *targets = release_pr(); }
    ready
}

fn release_ready_for_review() -> DomainState {
    let mut s = release_task();
    ok(&mut s, agent(), Command::DeclarePostMergeWork {
        task_id: "t".into(), reason: Some("Publish, install and report the release after merge".into()), waits_for: vec![],
    });
    ok(&mut s, agent(), release_delivery());
    s
}

#[test]
fn a_release_pr_is_reviewable_before_its_publication_installation_and_report() {
    let mut s = release_ready_for_review();
    assert_eq!(s.checklists[&"t".into()].items.len(), 6);
    assert_eq!(s.checklist_marks[&"release-review".into()].iter().map(|m| m.item.as_str()).collect::<Vec<_>>(), ["1", "2", "3"]);
    let index = ReadIndex::build(&s);
    assert_eq!(index.cards_of(&"release-review".into()).len(), 1);
    let item = index.item(&s, &s.interactions[&"release-review".into()]);
    assert!(item.available_actions.iter().any(|a| a.kind == "authorize_merge" && a.enabled));
    ok(&mut s, user(), Command::AcceptDelivery { presentation_id: "release-review".into(), targets: release_pr() });
    ok(&mut s, Actor::System, Command::ObservePrState {
        pr_id: "release-pr".into(), head: "abcdef0".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("abcdef0".into()),
    });
    assert!(!s.tasks[&"t".into()].over());
    assert!(s.post_merge_owed(&s.tasks[&"t".into()]));
    assert_eq!(s.checklist_marks[&"release-review".into()].len(), 3, "the merge proves none of the remaining steps");
    assert!(ReadIndex::build(&s).cards_of(&"release-review".into()).is_empty());
    assert_eq!(refused(&mut s, agent(), deliver("still-pending", true, vec![mark("1", vec![commit("abcdef0")])], vec![])).0, "checklist_incomplete");
    assert_eq!(refused(&mut s, agent(), Command::DeclarePostMergeWork { task_id: "t".into(), reason: None, waits_for: vec![] }).0, "checklist_incomplete");
    assert_eq!(refused(&mut s, agent(), Command::ConfirmCleanup { task_id: "t".into(), discard: vec![] }).0, "checklist_incomplete");
    let proof = (1..=6).map(|n| mark(&n.to_string(), vec![Evidence {
        kind: EvidenceKind::Command, value: format!("verified release step {n}"), detail: None,
    }])).collect();
    ok(&mut s, agent(), deliver("installed-release", true, proof, vec![]));
    ok(&mut s, user(), Command::AcceptDelivery { presentation_id: "installed-release".into(), targets: vec![DeliveryTarget::Text { text: "Done".into() }] });
    ok(&mut s, agent(), Command::DeclarePostMergeWork { task_id: "t".into(), reason: None, waits_for: vec![] });
    ok(&mut s, agent(), Command::ConfirmCleanup { task_id: "t".into(), discard: vec![] });
    ok(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: Some("a".into()) });
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
}

#[test]
fn deferring_checklist_points_requires_an_open_pr_and_declared_work_without_weakening_evidence() {
    let mut s = release_task();
    assert_eq!(refused(&mut s, agent(), release_delivery()).0, "checklist_incomplete");
    ok(&mut s, agent(), Command::DeclarePostMergeWork {
        task_id: "t".into(), reason: Some("Publish after merge".into()), waits_for: vec![],
    });
    let mut unproved = release_delivery();
    if let Command::PresentResult { checklist, .. } = &mut unproved { checklist[0].evidence.clear(); }
    assert_eq!(refused(&mut s, agent(), unproved).0, "checklist_without_evidence");
    ok(&mut s, Actor::System, Command::ObservePrState {
        pr_id: "release-pr".into(), head: "abcdef0".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("abcdef0".into()),
    });
    assert_eq!(refused(&mut s, agent(), release_delivery()).0, "checklist_incomplete");
}

#[test]
fn an_unproved_release_remains_recoverable_when_its_session_disappears() {
    let mut s = release_ready_for_review();
    ok(&mut s, user(), Command::AcceptDelivery { presentation_id: "release-review".into(), targets: release_pr() });
    ok(&mut s, Actor::System, Command::ObservePrState {
        pr_id: "release-pr".into(), head: "abcdef0".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("abcdef0".into()),
    });
    ok(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Gone, agent_id: Some("a".into()) });
    assert!(!s.tasks[&"t".into()].over(), "a real session end cannot prove publication");
    assert!(!s.landed(&s.tasks[&"t".into()]), "dependent tasks must still wait for publication");
    assert_eq!(s.tasks[&"t".into()].wrap_up.node, None, "the task can acquire a working node again");
    assert_eq!(s.checklist_marks[&"release-review".into()].len(), 3);
    assert_eq!(refused(&mut s, Actor::System, Command::CompleteTask { task_id: "t".into(), presentation_id: "release-review".into() }).0, "checklist_incomplete");
    ok(&mut s, Actor::System, Command::RegisterAgent { agent_id: "b".into(), name: "Replacement node".into(), at: 0 });
    ok(&mut s, Actor::Orchestrator { generation: 0 }, Command::AssignTask { task_id: "t".into(), agent_id: "b".into(), model: None });
    assert_eq!(s.tasks[&"t".into()].agent_id, Some("b".into()));
    assert!(!s.node_closing(&s.tasks[&"t".into()]));
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Running);
}

#[test]
fn the_pr_review_shows_unproved_points_as_pending_instead_of_completed() {
    use crate::views::{Client, Lang, LiveSnapshot, RenderContext, Transport};
    let s = release_ready_for_review();
    let index = ReadIndex::build(&s);
    for (lang, subtitle, count) in [(Lang::En, "Pending after merge", "3 of 6 done"), (Lang::Es, "Pendiente tras el merge", "3 de 6 hechos")] {
        let view = crate::views::build("card:release-review", RenderContext {
            domain: &s, index: &index, live: &LiveSnapshot::default(), client: Client::Mac, lang,
            transport: Transport::Loopback, cursor: "", rendered_at: "",
        }).unwrap();
        let json = serde_json::to_value(view).unwrap();
        let plan = json["sections"].as_array().unwrap().iter().find(|section| section["id"] == "plan").unwrap();
        assert!(plan["title"].as_str().unwrap().contains(count));
        let rows = plan["items"].as_array().unwrap();
        assert_eq!(rows.len(), 6);
        for number in 4..=6 {
            let pending = rows.iter().find(|row| row["id"] == format!("plan-pending-{number}")).unwrap();
            assert_eq!(pending["icon"], "schedule");
            assert_eq!(pending["subtitle"], subtitle);
        }
    }
}

#[test]
fn ending_post_merge_on_a_card_cannot_block_the_delivery_of_remaining_proof() {
    for sdk in [false, true] {
        let mut s = if sdk {
            let mut state = super::lifecycle_tests::world();
            super::lifecycle_tests::sdk_turn(&mut state, "t", "a");
            approved(&mut state, "release-plan", RELEASE_PLAN);
            ok(&mut state, agent(), Command::RegisterPr {
                task_id: "t".into(), pr_id: "release-pr".into(), host: "github.com".into(), repository: "acme/build-tool".into(), number: 49,
                head: "abcdef0".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
            });
            ok(&mut state, agent(), Command::DeclarePostMergeWork { task_id: "t".into(), reason: Some("Publish after merge".into()), waits_for: vec![] });
            ok(&mut state, agent(), release_delivery());
            state
        } else { release_ready_for_review() };
        ok(&mut s, user(), Command::AcceptDelivery { presentation_id: "release-review".into(), targets: release_pr() });
        ok(&mut s, Actor::System, Command::ObservePrState {
            pr_id: "release-pr".into(), head: "abcdef0".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("abcdef0".into()),
        });
        let (card, close, carry_on) = if sdk {
            ok(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: false, ended_work: vec![], by_alondra: false });
            let card = s.interactions.keys().find(|id| id.0.starts_with(SESSION_ENDED_PREFIX)).unwrap().clone();
            (card, CLOSE_TASK, CARRY_ON)
        } else {
            ok(&mut s, Actor::System, Command::RaiseProblem {
                attachments: vec![], interaction_id: format!("{POST_MERGE_PROBLEM_PREFIX}t-proof").into(), scope: Scope::Task { task_id: "t".into() }, body: "Work is pending".into(),
                options: vec![QuestionOption { id: POST_MERGE_CLOSE.into(), label: "Close the node".into(), description: None },
                    QuestionOption { id: POST_MERGE_LEAVE.into(), label: "Leave it".into(), description: None }],
                card: CardSpec { system: Some(SystemCard::PostMergeProblem), ..CardSpec::new(CardKind::Problem, "Review the pending work")
                    .why("Publication remains pending.").content(ContentKind::Diagnosis, "Work is pending") },
            });
            (format!("{POST_MERGE_PROBLEM_PREFIX}t-proof").into(), POST_MERGE_CLOSE, POST_MERGE_LEAVE)
        };
        let answer = |option: &str| Command::AnswerQuestion { attachments: vec![], interaction_id: card.clone(), option_id: option.into(), inputs: Default::default() };
        assert_eq!(refused(&mut s, user(), answer(close)).0, "checklist_incomplete");
        assert!(!s.node_closing(&s.tasks[&"t".into()]));
        ok(&mut s, user(), answer(carry_on));
        let proof = (1..=6).map(|n| mark(&n.to_string(), vec![commit("abcdef0")])).collect();
        ok(&mut s, agent(), deliver("proved-release", true, proof, vec![]));
        assert!(s.unproved_checklist(&s.tasks[&"t".into()]).is_empty());
    }
}

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
