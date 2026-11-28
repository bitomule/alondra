//! The task flow, the rules suite of the component `task-flow`: a task without stages proposes and
//! validates its own specs, a task that continues another takes over its work, and what the
//! component's rules may reach.
use super::lifecycle_tests::{exec, must, node, sdk_turn, task, user, world};
use super::spec_tests::{admit, answer, told};
use super::specs_tests::{changes, Mem, REFUNDS, REFUNDS_PATH, SETTINGS};
use super::*;
use crate::spec_card::AMENDMENT_PREFIX;
use crate::specs;
use std::collections::{BTreeMap, BTreeSet};

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
const BASE: &str = "1111111111111111111111111111111111111111";
const PR: &str = "github.com/example/repo#7";
const WORKTREE: &str = "/repo/.claude/worktrees/t";
const FULL: &str = "billing.refunds/full";

fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}
fn the(s: &DomainState, id: &str) -> Task {
    s.tasks[&TaskId::from(id)].clone()
}

// ---- A task without stages proposes and validates its own specs ----

/// What the agent writes of refunds: a full refund now pays 40 EUR.
fn refunds() -> String {
    REFUNDS.replace("receives 30 EUR", "receives 40 EUR")
}
/// Task `t` without stages, its first agent `a` at work.
fn unstaged() -> DomainState {
    let mut s = world();
    task(&mut s, "t", None);
    must(&mut s, Actor::System, Command::RecordTaskStages { task_id: "t".into(), staged: false, reason: "Simple work".into(), selections: BTreeMap::new() });
    admit(&mut s, "a", 1);
    s
}
fn proposal(text: &str) -> Command {
    let entries = [(REFUNDS_PATH, Some(text))];
    let check = specs::check(&Mem::project(), &changes(&entries), &specs::NoImages);
    Command::ProposeSpecs { task_id: "t".into(), level: SpecLevel::Light, files: vec![SpecFile::of(REFUNDS_PATH.into(), Some(text.into()))], check }
}
fn plan_card() -> Command {
    let body = "# Refunds pay 40 EUR\n\n- [ ] Pay 40 EUR on a full refund (billing.refunds/full)\n";
    let card = CardSpec::new(CardKind::Review, "Review the plan").content(ContentKind::Plan, body);
    Command::RecordDecisionRequest { task_id: "t".into(), generation: 1, epoch: 7, hold: Hold::Plan, tool: Some("plan".into()), input: serde_json::json!({}), mcp_server: None,
        prompt: DecisionPrompt::Card { body: body.into(), options: Hold::Plan.options_offering(false), card } }
}
fn plan_id() -> InteractionId {
    decision_card_id(&"t".into(), 1, 7)
}
/// `unstaged`, whose agent proposed the refunds it wrote and presented its plan.
fn proposed() -> DomainState {
    let mut s = unstaged();
    must(&mut s, node("a"), proposal(&refunds()));
    must(&mut s, Actor::System, plan_card());
    s
}
/// `proposed`, with the plan approved and its specs written into the worktree.
fn approved() -> DomainState {
    let mut s = proposed();
    answer(&mut s, plan_id(), Hold::APPROVE, None).unwrap();
    must(&mut s, Actor::System, Command::RecordDecisionSettled { task_id: "t".into(), generation: 1, epoch: 7 });
    let due = the(&s, "t").specs.due();
    must(&mut s, Actor::System, Command::RecordSpecsWritten { task_id: "t".into(), written: due.iter().map(|(path, text)| (path.clone(), specs::fingerprint(text.as_ref()))).collect() });
    s
}
fn item(subject: &str, kind: ValidationKind) -> ValidationItem {
    ValidationItem { subject: subject.into(), kind, inputs: format!("sha256:{subject}-{}", kind.as_str()) }
}
fn ready() -> Command {
    Command::ReadyForValidation { task_id: "t".into(), head: HEAD.into(), items: vec![item(FULL, ValidationKind::Adequate)], scope: vec![] }
}
fn record(verdict: ValidationVerdict) -> Command {
    Command::RecordValidation {
        task_id: "t".into(), subject: FULL.into(), check: ValidationKind::Adequate, verdict,
        reasoning: Some("The test asserts the refunded amount".into()), nearest_wrong: Some("Refunding 30 EUR".into()),
        output: None, evidence: vec![], inputs: format!("sha256:{FULL}-adequate"), listed: None,
    }
}
/// What Alondra read of the worktree's commit before a delivery, with what the engine lists there.
fn reading(owed: Vec<ValidationItem>) -> Command {
    Command::RecordCommitReading { task_id: "t".into(), reading: CommitReading {
        commit: HEAD.into(), worktree: true, specs: true, owed, problems: vec![], ledger: vec![], stray: vec![], since: None, changed: vec![],
        base: Some(BASE.into()), tip: None, pr_base: None, default_branch: Some("main".into()), spec_changes: vec![],
    } }
}
fn present(id: &str) -> Command {
    let targets = vec![DeliveryTarget::Text { text: "Done".into() }];
    Command::PresentResult {
        attachments: vec![], task_id: "t".into(), interaction_id: id.into(), body: "Full refunds pay 40 EUR".into(),
        card: CardSpec::delivery("Review the result", &targets, true), targets, is_final: true, nothing_to_review: false, checklist: vec![],
    }
}
fn runs(s: &DomainState) -> usize {
    the(s, "t").stages.runs.len()
}

// spec: task-flow.specs-without-stages/told-at-start
#[test]
fn the_agent_of_a_task_without_stages_in_a_project_with_specs_is_told_the_rule_when_it_starts() {
    let mission = "Make full refunds pay 40 EUR";
    let input = unstaged_mission(mission, false, true, Some(SETTINGS.as_bytes()));
    assert!(input.starts_with(mission), "{input}");
    let section = input.split_once("## Specs in this project").map(|(_, section)| section).expect("the section is in the first input");
    for tool in ["propose_specs", "ready_for_validation", "record_validation"] {
        assert!(section.contains(tool), "the section names {tool}: {section}");
    }
    assert_eq!(unstaged_mission(mission, false, false, Some(SETTINGS.as_bytes())), mission, "only the first agent is told: a later one resumes its context");
    assert_eq!(unstaged_mission(mission, true, true, Some(SETTINGS.as_bytes())), mission, "a task in stages is told as its author instead");
}

// spec: task-flow.specs-without-stages/nothing-added-without-specs
#[test]
fn a_project_without_specs_adds_nothing_to_the_mission() {
    let off = "format = 1\n\n[specs]\nenabled = false\n";
    for settings in [Some(off.as_bytes()), None] {
        assert_eq!(unstaged_mission("Make full refunds pay 40 EUR", false, true, settings), "Make full refunds pay 40 EUR");
    }
}

// spec: task-flow.specs-without-stages/proposes-without-stages
#[test]
fn the_agent_of_a_task_without_stages_proposes_specs_and_its_next_plan_carries_them() {
    let mut s = unstaged();
    must(&mut s, node("a"), proposal(&refunds()));
    let draft = the(&s, "t").specs.draft.expect("the task holds the proposal");
    assert_eq!(draft.level, SpecLevel::Light);
    assert_eq!(crate::specs_tests::text(&draft.files, REFUNDS_PATH), Some(refunds().as_str()));
    must(&mut s, Actor::System, plan_card());
    assert_eq!(s.open_plan_card(&"t".into()).map(|card| card.id.clone()), Some(plan_id()), "the specs wait on the plan's card");
}

// spec: task-flow.specs-without-stages/refusal-names-the-way-out
#[test]
fn the_refusal_of_a_task_without_stages_tells_it_to_propose_the_file_and_present_a_plan() {
    let s = unstaged();
    let check = DeliveryCheck::SpecsDiffer { unapproved: vec![REFUNDS_PATH.into()], other: vec![] };
    let refused = check.refusal_to(Deliverer::of(&s.tasks[&TaskId::from("t")])).expect("refused");
    assert_eq!(refused.code, "specs_differ");
    for wanted in [REFUNDS_PATH, "propose_specs", "present a plan"] {
        assert!(refused.message.contains(wanted), "the refusal says «{wanted}»: {}", refused.message);
    }
    assert_eq!(specs_differ_text(&[REFUNDS_PATH.into()], &[], false), refused.message.trim_end(), "the daemon's refusal says the same");
    let staged = check.refusal(Some(ExecutionRole::Implementer)).unwrap();
    assert!(!staged.message.contains("propose_specs"), "an implementer asks for a change instead: {}", staged.message);
}

// spec: task-flow.specs-without-stages/proposes-what-it-wrote
#[test]
fn specs_a_task_without_stages_already_wrote_are_approved_in_the_same_task() {
    let mut s = unstaged();
    let tasks = s.tasks.len();
    let refused = DeliveryCheck::SpecsDiffer { unapproved: vec![REFUNDS_PATH.into()], other: vec![] }.refusal_to(Deliverer::of(&the(&s, "t"))).unwrap();
    assert_eq!(refused.code, "specs_differ");
    must(&mut s, node("a"), proposal(&refunds()));
    must(&mut s, Actor::System, plan_card());
    answer(&mut s, plan_id(), Hold::APPROVE, None).unwrap();
    let approved = the(&s, "t").specs.approved.expect("approved");
    assert_eq!(crate::specs_tests::text(&approved.files, REFUNDS_PATH), Some(refunds().as_str()));
    assert_eq!((the(&s, "t").agent_id, runs(&s), s.tasks.len()), (Some("a".into()), 1, tasks), "the same agent goes on and no task was created");
}

// spec: task-flow.specs-without-stages/same-agent-goes-on
#[test]
fn approving_the_plan_of_a_task_without_stages_keeps_its_agent_and_tells_it_to_commit_the_specs() {
    let mut s = proposed();
    answer(&mut s, plan_id(), Hold::APPROVE, None).unwrap();
    let approved = the(&s, "t").specs.approved.expect("approved");
    assert_eq!(approved.files.keys().cloned().collect::<Vec<_>>(), vec![REFUNDS_PATH.to_owned()], "exactly the proposed files");
    assert_eq!(the(&s, "t").stages.handover, None, "no other agent takes the task");
    must(&mut s, Actor::System, Command::RecordDecisionSettled { task_id: "t".into(), generation: 1, epoch: 7 });
    let due = the(&s, "t").specs.due();
    must(&mut s, Actor::System, Command::RecordSpecsWritten { task_id: "t".into(), written: due.iter().map(|(path, text)| (path.clone(), specs::fingerprint(text.as_ref()))).collect() });
    let said = told(&s);
    assert!(said.last().is_some_and(|text| text.contains("Commit them") && text.contains(REFUNDS_PATH)), "{said:?}");
    assert_eq!(runs(&s), 1);
}

// spec: task-flow.specs-without-stages/validates-itself
#[test]
fn the_agent_of_a_task_without_stages_validates_its_own_work() {
    let mut s = approved();
    must(&mut s, node("a"), ready());
    let round = the(&s, "t").validation.current().cloned().expect("a round");
    assert_eq!((round.head.as_str(), round.state, round.generation), (HEAD, RoundState::Validating, Some(1)), "the round opens with the same agent as its validator");
    assert_eq!(the(&s, "t").stages.handover, None);
    must(&mut s, node("a"), record(ValidationVerdict::Met));
    let recorded = &the(&s, "t").validation.records[0];
    assert_eq!((recorded.subject.as_str(), recorded.round, recorded.generation), (FULL, 1, 1));
    assert_eq!(the(&s, "t").validation.current().unwrap().items[0].state, ValidationState::Met);
}

// spec: task-flow.specs-without-stages/unmet-comes-back-to-itself
#[test]
fn work_a_task_without_stages_finds_unmet_comes_back_to_the_same_agent() {
    let mut s = approved();
    must(&mut s, node("a"), ready());
    must(&mut s, node("a"), record(ValidationVerdict::Unmet));
    must(&mut s, node("a"), Command::EndValidation { task_id: "t".into(), findings: "A full refund still pays 30 EUR".into() });
    assert_eq!(the(&s, "t").validation.current().unwrap().state, RoundState::Failed);
    let said = told(&s);
    assert!(said.last().is_some_and(|text| text.contains(FULL) && text.contains("ready_for_validation")), "{said:?}");
    assert_eq!((the(&s, "t").stages.handover, runs(&s)), (None, 1));
}

// spec: task-flow.specs-without-stages/delivery-waits-for-validation
#[test]
fn the_delivery_of_a_task_without_stages_waits_for_its_own_validation() {
    let mut s = approved();
    must(&mut s, Actor::System, reading(vec![item(FULL, ValidationKind::Adequate)]));
    let refused = exec(&mut s, node("a"), present("result-1")).unwrap_err();
    assert_eq!(refused.code, "validation_required");
    assert!(refused.message.contains("ready_for_validation") && refused.message.contains("yourself"), "{}", refused.message);
    assert_eq!(the(&s, "t").final_presentation, None);
}

// spec: task-flow.specs-without-stages/no-specs-delivered-as-before
#[test]
fn a_task_without_stages_and_without_approved_specs_delivers_as_before() {
    let mut s = unstaged();
    must(&mut s, Actor::System, reading(vec![]));
    must(&mut s, node("a"), present("result-1"));
    assert_eq!(the(&s, "t").final_presentation, Some("result-1".into()));
    assert!(the(&s, "t").validation.rounds.is_empty(), "no validation round");
}

// spec: task-flow.specs-without-stages/change-drafted-by-itself
#[test]
fn a_change_to_the_approved_specs_of_a_task_without_stages_is_drafted_by_its_own_agent() {
    let mut s = approved();
    must(&mut s, node("a"), Command::RequestSpecChange { task_id: "t".into(), reason: "A full refund also returns the shipping".into() });
    let said = told(&s);
    assert!(said.last().is_some_and(|text| text.contains("propose_amendment")), "{said:?}");
    assert_eq!(the(&s, "t").stages.handover, None);
    let text = refunds().replace("receives 40 EUR", "receives 45 EUR");
    let entries = [(REFUNDS_PATH, Some(text.as_str()))];
    let check = specs::check(&Mem::project(), &changes(&entries), &specs::NoImages);
    must(&mut s, node("a"), Command::ProposeAmendment { task_id: "t".into(), amendment_id: "a1".into(), files: vec![SpecFile::of(REFUNDS_PATH.into(), Some(text.as_str().into()))], check });
    let card = InteractionId(format!("{AMENDMENT_PREFIX}t-a1"));
    assert!(s.interactions.get(&card).is_some_and(|card| s.open(card)), "the user decides it on the change's card");
    assert_eq!((the(&s, "t").stages.handover, runs(&s)), (None, 1));
}

// spec: task-flow.specs-without-stages/old-tasks-still-read
#[test]
fn a_task_stored_before_this_change_reads_as_a_task_without_stages() {
    let s = unstaged();
    let mut stored = serde_json::to_value(the(&s, "t")).unwrap();
    for field in ["stages", "specs", "validation"] {
        stored.as_object_mut().unwrap().remove(field);
    }
    let read: Task = serde_json::from_value(stored).expect("a task stored by 0.3.72 still reads");
    assert!(!read.stages.staged && read.specs.draft.is_none() && read.specs.approved.is_none() && read.validation.rounds.is_empty());
    assert!(!read.validates_itself() && !read.gated());
}

// ---- A task that continues another takes over its work ----

/// Task `t`, at work in its worktree on branch `worktree-t`, with its open pull request #7.
fn with_pr() -> DomainState {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, Actor::System, Command::RecordTaskWorktree { task_id: "t".into(), path: WORKTREE.into(), origin: WorktreeOrigin::Task, branch: Some("worktree-t".into()) });
    must(&mut s, node("a"), Command::RegisterPr {
        task_id: "t".into(), pr_id: PR.into(), host: "github.com".into(), repository: "example/repo".into(), number: 7,
        head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    s
}
fn continuation(request: &str) -> Command {
    Command::CreateTask { priority: Default::default(),
        task_id: "t2".into(), request_id: request.into(), title: "Task t2".into(), agent_id: None, review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", WORKTREE)), plan_items: None, continues: Some("t".into()),
    }
}
fn owner(s: &DomainState) -> TaskId {
    s.pull_requests[&PrId::from(PR)].task_id.clone()
}
/// `with_pr`, continued by `t2`.
fn continued() -> DomainState {
    let mut s = with_pr();
    must(&mut s, orchestrator(), continuation("r"));
    s
}
fn cancel(s: &mut DomainState, actor: Actor) -> CommandOutcome {
    let outcome = must(s, actor, Command::CancelTask { task_id: "t".into(), reason: None });
    must(s, Actor::System, Command::ReleaseTaskWorktree { task_id: "t".into(), path: WORKTREE.into(), end: WorktreeEnd::HandedOver });
    must(s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: None });
    outcome
}
/// `continued`, as version 0.3.72 left it: the continuation holds the worktree, and the pull
/// request still belongs to the original.
fn continued_by_an_older_version() -> DomainState {
    let mut s = continued();
    let pr = PrId::from(PR);
    s.pull_requests.get_mut(&pr).unwrap().task_id = "t".into();
    s.tasks.get_mut(&TaskId::from("t")).unwrap().pr_id = Some(pr.clone());
    s.tasks.get_mut(&TaskId::from("t2")).unwrap().pr_id = None;
    s.pr_task_history.remove(&pr);
    s
}

// spec: task-flow.continuation/takes-open-prs
#[test]
fn a_continuation_takes_the_open_pull_requests_of_its_original_with_their_history() {
    let s = continued();
    assert_eq!(owner(&s), TaskId::from("t2"));
    assert_eq!(the(&s, "t2").pr_id, Some(PR.into()));
    assert_eq!(the(&s, "t").pr_id, None);
    let inherited = &the(&s, "t2").worktrees[WORKTREE];
    assert_eq!((inherited.origin, inherited.branch.as_deref()), (WorktreeOrigin::Inherited, Some("worktree-t")));
    assert_eq!(s.pr_task_history[&PrId::from(PR)], BTreeSet::from([TaskId::from("t"), TaskId::from("t2")]));
}

// spec: task-flow.continuation/refused-moves-nothing
#[test]
fn a_refused_continuation_moves_nothing() {
    let mut s = with_pr();
    must(&mut s, user(), Command::CreateRequest { request_id: "h".into(), body: "Other".into(), auto_merge: false, maintenance: false, message: false });
    must(&mut s, orchestrator(), Command::TitleRequest { request_id: "h".into(), title: "Other".into() });
    let before = s.clone();
    assert_eq!(exec(&mut s, orchestrator(), continuation("h")).unwrap_err().code, "continuation_request");
    assert_eq!(owner(&s), TaskId::from("t"));
    assert_eq!(s, before);
}

// spec: task-flow.continuation/delivery-open-is-settled-first
#[test]
fn a_continuation_waits_until_the_user_settles_the_delivery_of_its_original() {
    let mut s = with_pr();
    let targets = vec![DeliveryTarget::PullRequest { pr_id: PR.into(), head: HEAD.into() }];
    must(&mut s, node("a"), Command::PresentResult {
        attachments: vec![], task_id: "t".into(), interaction_id: "result-t".into(), body: "Done".into(),
        card: CardSpec::delivery("Review the result", &targets, true), targets, is_final: true, nothing_to_review: false, checklist: vec![],
    });
    let refused = exec(&mut s, orchestrator(), continuation("r")).unwrap_err();
    assert_eq!(refused.code, "delivery_open");
    assert_eq!(owner(&s), TaskId::from("t"));
}

// spec: task-flow.continuation/cancel-by-orchestrator
#[test]
fn the_orchestrator_cancels_the_original_and_no_card_is_raised() {
    let mut s = continued();
    let outcome = cancel(&mut s, orchestrator());
    assert_eq!(the(&s, "t").state, TaskState::Cancelled);
    assert!(outcome.created_ids.is_empty(), "no card is raised: {:?}", outcome.created_ids);
    assert_eq!((owner(&s), s.pull_requests[&PrId::from(PR)].state), (TaskId::from("t2"), PrState::Open));
}

// spec: task-flow.continuation/cancel-by-user
#[test]
fn the_user_cancels_the_original_and_its_pull_request_stays_open_with_the_continuation() {
    let mut s = continued();
    cancel(&mut s, user());
    assert_eq!(the(&s, "t").state, TaskState::Cancelled);
    assert_eq!((owner(&s), s.pull_requests[&PrId::from(PR)].state), (TaskId::from("t2"), PrState::Open));
    assert!(s.tasks.values().all(|task| !matches!(task.open_pr, Some(OpenPrPending::Close { .. }))), "nothing asks to close it on GitHub");
}

// spec: task-flow.continuation/never-starts-again
#[test]
fn the_original_never_gets_a_new_agent_while_its_continuation_is_not_finished() {
    let mut s = continued();
    must(&mut s, node("a"), present("result-t"));
    cancel(&mut s, user());
    let launched = exec(&mut s, orchestrator(), Command::AssignTask { task_id: "t".into(), agent_id: "b".into(), model: None }).unwrap_err();
    let changes = exec(&mut s, user(), Command::SubmitFeedback {
        presentation_id: "result-t".into(), targets: vec![], body: "Do it again".into(), instruction: Instruction::Changes, attachments: vec![], merge_failure: None,
    }).unwrap_err();
    for refused in [launched, changes] {
        assert_eq!(refused.code, "task_continued");
        assert!(refused.message.contains("t2"), "it names the task that continues it: {}", refused.message);
    }
    assert_ne!(the(&s, "t").agent_id, Some("b".into()), "no new agent takes it");
    assert!(s.lifecycle_of(&"t".into()).unwrap().execution().is_none(), "nothing runs it");
}

// spec: task-flow.continuation/running-original-cannot-take-it-back
#[test]
fn a_running_original_cannot_register_the_pull_request_again() {
    let mut s = continued();
    let refused = exec(&mut s, node("a"), Command::RegisterPr {
        task_id: "t".into(), pr_id: PR.into(), host: "github.com".into(), repository: "example/repo".into(), number: 7,
        head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    }).unwrap_err();
    assert_eq!(refused.code, "pr_owned_by_other_task");
    assert_eq!(owner(&s), TaskId::from("t2"));
}

// spec: task-flow.continuation/sweep-sees-the-continuation
#[test]
fn the_sweep_counts_the_continuation_as_the_owner_of_the_branch_it_inherited() {
    let mut s = continued();
    cancel(&mut s, user());
    assert!(s.branch_has_live_task("worktree-t"));
    let status = OpenPrStatus { state: PrState::Open, head: HEAD.into(), base: "main".into(), mergeable: PrMergeable::Clean, checks: PrChecks::Passed };
    let before = s.clone();
    must(&mut s, Actor::System, Command::AdoptOpenPr { task_id: "t".into(), pr_id: PR.into(), branch: "worktree-t".into(), status });
    assert_eq!(s.tasks, before.tasks, "the pull request is neither adopted nor closed");
    assert_eq!((owner(&s), s.pull_requests[&PrId::from(PR)].state), (TaskId::from("t2"), PrState::Open));
}

// spec: task-flow.continuation/older-continuation-moves-on-cancel
#[test]
fn a_continuation_an_older_version_recorded_takes_the_pull_request_when_the_original_is_cancelled() {
    for actor in [orchestrator(), user()] {
        let mut s = continued_by_an_older_version();
        assert_eq!(owner(&s), TaskId::from("t"));
        let outcome = cancel(&mut s, actor.clone());
        assert_eq!((owner(&s), s.pull_requests[&PrId::from(PR)].state), (TaskId::from("t2"), PrState::Open), "{actor:?}");
        assert_eq!(the(&s, "t").state, TaskState::Cancelled, "{actor:?}");
        assert!(outcome.created_ids.is_empty(), "{actor:?} raised {:?}", outcome.created_ids);
        assert!(the(&s, "t").open_pr.is_none(), "{actor:?}");
    }
}

// ---- What the rules may reach ----

const BOUNDARY: &str = include_str!("../../../specs/task-flow/boundary.toml");
/// Each file the component owns, by its path in the repository.
const OWNED: [(&str, &str); 8] = [
    ("crates/domain-rules/src/rules/spec_rules.rs", include_str!("rules/spec_rules.rs")),
    ("crates/domain-rules/src/rules/validation_rules.rs", include_str!("rules/validation_rules.rs")),
    ("crates/domain-rules/src/rules/continuation_rules.rs", include_str!("rules/continuation_rules.rs")),
    ("crates/domain-rules/src/stage_rules.rs", include_str!("stage_rules.rs")),
    ("crates/domain-model/src/stages.rs", include_str!("../../domain-model/src/stages.rs")),
    ("crates/domain-model/src/delivery_validation.rs", include_str!("../../domain-model/src/delivery_validation.rs")),
    ("crates/domain-model/src/checks/open_pr.rs", include_str!("../../domain-model/src/checks/open_pr.rs")),
    ("crates/domain-model/src/checks/spec_rules.rs", include_str!("../../domain-model/src/checks/spec_rules.rs")),
];
fn boundary() -> toml::Table {
    BOUNDARY.parse().expect("the boundary of task-flow reads")
}
fn strings(table: &toml::Table, key: &str) -> Vec<String> {
    table[key].as_array().unwrap().iter().map(|value| value.as_str().unwrap().to_owned()).collect()
}

// spec: task-flow.boundary/rules-reach-nothing-outside
#[test]
fn the_rules_of_the_task_flow_never_reach_the_disk_the_network_a_program_or_a_thread() {
    let owns: BTreeSet<String> = strings(&boundary(), "owns").into_iter().collect();
    let scanned: BTreeSet<String> = OWNED.iter().map(|(path, _)| (*path).to_owned()).collect();
    assert_eq!(scanned, owns, "every file the component owns is searched");
    for (path, source) in OWNED {
        for reached in ["std::fs", "std::process", "std::net", "std::thread"] {
            assert!(!source.contains(reached), "{path} uses {reached}");
        }
    }
}

// spec: task-flow.boundary/depends-only-on-spec-engine
#[test]
fn the_only_component_the_task_flow_depends_on_is_the_spec_engine() {
    assert_eq!(strings(&boundary(), "depends_on"), vec!["spec-engine".to_owned()]);
}
