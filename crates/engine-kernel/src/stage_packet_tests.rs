//! The packet a new stage of a task starts from names the files of its plan, its record and its
//! specs, and keeps inline only what its role does and the validation that concerns it.
use super::*;
use alondra_core::accounts::{AccountKey, Provider};
use alondra_core::model_selection::{ModelKey, ModelProfile, ModelSelection, PendingTaskSelection, TaskSelection};
use alondra_domain_model::*;
use alondra_domain_rules::Apply as _;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

const FOLDER: &str = "/Users/ana/.alondra/files/tasks/t-refund";
const WORKTREE: &str = "/Users/ana/work/refunds";
const PLAN: &str = "# Refund part of an order\n\n- [ ] Refund any amount up to what is left (billing.partial/part)";
const PARTIAL_PATH: &str = "specs/billing/partial.feature";
const PARTIAL: &str = "@spec:billing.partial\nFeature: Partial refunds\n  Part of an order comes back.\n\n  @id:part\n  Scenario: Part of an order\n    Given a paid order of 30 EUR\n    When 10 EUR are refunded\n    Then the customer receives 10 EUR\n";
const DECISION_PATH: &str = "docs/decisions/refund-what-is-left.md";
const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

fn must(s: &mut DomainState, actor: Actor, command: Command) {
    let shown = format!("{command:?}");
    s.seed_mergeable_for(&command);
    let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision, command };
    if let Err(error) = s.execute(&actor, &envelope) {
        panic!("{shown} was refused: {error}");
    }
}

fn node(agent: &str) -> Actor {
    Actor::Agent { agent_id: agent.into() }
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn task_id(task: &str) -> TaskId {
    task.into()
}

fn admit(s: &mut DomainState, task: &str, agent: &str, generation: u64) {
    must(s, Actor::System, Command::RegisterAgent { agent_id: agent.into(), name: agent.into(), at: 0 });
    must(s, Actor::System, Command::AdmitExecution { provider: Default::default(), task_id: task.into(), agent_id: agent.into(),
        conversation: format!("conversation-{generation}"), instance: format!("instance-{generation}"), host: "sdk-host".into(),
        model: None, account: None, leaves_limit: None });
    if generation > 1 {
        let selection = s.tasks[&task_id(task)].selection.clone();
        must(s, Actor::System, Command::AbortTaskSelection { task_id: task.into(), selection, refused: None });
    }
}

/// A task in stages whose author `a` is at work on it.
fn authoring(task: &str, request: &str) -> DomainState {
    let mut s = DomainState::new("epoch");
    must(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: request.into(), auto_merge: false, maintenance: false, message: false });
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::TitleRequest { request_id: "r".into(), title: "Refunds".into() });
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { priority: Default::default(), task_id: task.into(), request_id: "r".into(),
        title: "Refund part of an order".into(), agent_id: None, review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(LaunchPlan::new("node", WORKTREE)), plan_items: None, continues: None });
    must(&mut s, Actor::System, Command::RecordTaskStages { task_id: task.into(), staged: true, reason: "Large".into(),
        selections: TaskStages::role_selections(&TaskSelection::automatic(ModelProfile::Implementation)) });
    admit(&mut s, task, "a", 1);
    s
}

fn propose(s: &mut DomainState, task: &str, mode: SpecMode, files: &[(&str, &str)]) {
    let files = files.iter().map(|(path, text)| SpecFile::of((*path).into(), Some((*text).into()))).collect();
    must(s, node("a"), Command::ProposeSpecs { task_id: task.into(), level: SpecLevel::Light, files, check: SpecCheck { mode, ..Default::default() } });
}

/// The user approves `plan`, which the author presented.
fn approve(s: &mut DomainState, task: &str, plan: &str) {
    let card = CardSpec::new(CardKind::Review, "Review the plan").content(ContentKind::Plan, plan);
    must(s, Actor::System, Command::RecordDecisionRequest { task_id: task.into(), generation: 1, epoch: 7, hold: Hold::Plan, tool: Some("plan".into()), input: json!({}),
        mcp_server: None, prompt: DecisionPrompt::Card { body: plan.into(), options: Hold::Plan.options_offering(false), card } });
    must(s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: decision_card_id(&task_id(task), 1, 7), option_id: Hold::APPROVE.into(), inputs: BTreeMap::new() });
    must(s, Actor::System, Command::RecordDecisionSettled { task_id: task.into(), generation: 1, epoch: 7 });
}

/// The change of role the task owes runs: its execution ends and `agent` takes over.
fn hand_over(s: &mut DomainState, task: &str, agent: &str, generation: u64) {
    let expected_selection = s.task_selection_stamp(&task_id(task)).unwrap();
    let pending = PendingTaskSelection { account: AccountKey::native(Provider::Claude),
        model: ModelSelection { model: ModelKey { provider: Provider::Claude, model: "sonnet".into() }, reasoning_effort: None, catalog_revision: "catalog".into() },
        reason: "The next role takes the task over".into() };
    must(s, Actor::System, Command::BeginStageHandover { task_id: task.into(), expected_selection, transition: format!("stage-{generation}"), pending });
    must(s, Actor::System, Command::RecordHostEnded { task_id: task.into(), generation: generation - 1, resumes: false, ended_work: vec![], by_alondra: true });
    admit(s, task, agent, generation);
}

fn written(s: &mut DomainState, task: &str) {
    let due = s.tasks[&task_id(task)].specs.due();
    let written = due.iter().map(|(path, content)| (path.clone(), specs::fingerprint(content.as_ref()))).collect();
    must(s, Actor::System, Command::RecordSpecsWritten { task_id: task.into(), written });
}

fn packet(s: &DomainState, task: &str, role: ExecutionRole) -> String {
    stage_packet(s, &s.tasks[&task_id(task)], role, WORKTREE, Some("refunds"), FOLDER).unwrap()
}

fn file(s: &DomainState, task: &str, path: &str) -> String {
    let files = stage_files(s, &s.tasks[&task_id(task)]).unwrap();
    match files.into_iter().find(|file| file.path == path).unwrap_or_else(|| panic!("no file {path}")).body {
        StageFileBody::Text(text) => text,
        StageFileBody::Spec(content) => content.text().unwrap().to_owned(),
    }
}

// spec: stage-handover.packet/plan-by-path
#[test]
fn the_approved_plan_goes_to_a_file_the_packet_names() {
    let mut s = authoring("t-refund", "Let customers get part of an order refunded");
    approve(&mut s, "t-refund", PLAN);
    assert_eq!(file(&s, "t-refund", PLAN_FILE), PLAN, "the file holds the approved plan exactly");
    let packet = packet(&s, "t-refund", ExecutionRole::Implementer);
    assert!(packet.contains(&format!("{FOLDER}/approved-plan.md: read it there before you start")), "{packet}");
    assert!(!packet.contains("- [ ] Refund any amount up to what is left (billing.partial/part)"), "{packet}");
}

// spec: stage-handover.packet/phase-points-stay-inline
#[test]
fn the_points_of_a_plan_in_phases_that_belong_to_the_task_stay_in_the_packet() {
    let mut s = authoring("t-phase", "Refunds in phases");
    let plan = "# Refunds in phases\n\n## Phase 1\n- [ ] Store refunds\n\n## Phase 2\n- [ ] Refund part of an order\n- [ ] Refund the shipping";
    approve(&mut s, "t-phase", plan);
    s.checklists.get_mut(&task_id("t-phase")).unwrap().items.retain(|item| item.id != "1");
    let packet = packet(&s, "t-phase", ExecutionRole::Implementer);
    assert!(packet.contains("Your part of it is points 2, 3"), "{packet}");
    assert_eq!(file(&s, "t-phase", PLAN_FILE), plan, "the file holds the whole plan");
}

// spec: stage-handover.packet/record-by-path
#[test]
fn the_tasks_record_goes_to_a_file_the_packet_names_without_the_conversation() {
    let mut s = authoring("t-refund", "Let customers get part of an order refunded");
    approve(&mut s, "t-refund", PLAN);
    let journal = s.lifecycle_journal.get_mut(&task_id("t-refund")).unwrap();
    journal.push(LifecycleRecord::Told { input: "cents".into(), text: Some("Use cents, not floats".into()), answers: None });
    journal.push(LifecycleRecord::Answered { card: "card-destination".into(), input: "destination".into(), text: Some("Refund to the original card".into()) });
    let record = file(&s, "t-refund", RECORD_FILE);
    for said in ["Let customers get part of an order refunded", "Use cents, not floats", "Refund to the original card"] {
        assert!(record.contains(said), "{said}: {record}");
    }
    assert!(!record.contains("I will start with the refund model"), "the conversation is not part of the record");
    assert!(!record.contains("- [ ] Refund any amount"), "the plan has a file of its own: {record}");
    let packet = packet(&s, "t-refund", ExecutionRole::Implementer);
    assert!(packet.contains(&format!("{FOLDER}/task-records.md: search it with grep and read only the part you need")), "{packet}");
    for absent in ["Use cents, not floats", "I will start with the refund model", "Refund to the original card"] {
        assert!(!packet.contains(absent), "{absent}: {packet}");
    }
}

// spec: stage-handover.packet/specs-by-path
#[test]
fn approved_specs_written_into_the_worktree_go_by_their_paths() {
    let mut s = authoring("t-refund", "Let customers get part of an order refunded");
    propose(&mut s, "t-refund", SpecMode::Committed, &[(PARTIAL_PATH, PARTIAL), (DECISION_PATH, "# Refund what is left\n\nAccepted.\n")]);
    approve(&mut s, "t-refund", PLAN);
    written(&mut s, "t-refund");
    let packet = packet(&s, "t-refund", ExecutionRole::Implementer);
    for wanted in [PARTIAL_PATH, DECISION_PATH, "Read them in the worktree", "the spec wins", "request_spec_change"] {
        assert!(packet.contains(wanted), "{wanted}: {packet}");
    }
    assert!(!packet.contains("Part of an order"), "{packet}");
    assert!(stage_files(&s, &s.tasks[&task_id("t-refund")]).unwrap().iter().all(|file| !file.path.starts_with("specs/")), "specs in the worktree are not copied");
}

// spec: stage-handover.packet/uncommitted-specs-in-task-folder
#[test]
fn specs_a_project_keeps_out_of_the_repository_go_to_the_tasks_folder() {
    let mut s = authoring("t-refund", "Let customers get part of an order refunded");
    propose(&mut s, "t-refund", SpecMode::Uncommitted, &[(PARTIAL_PATH, PARTIAL)]);
    approve(&mut s, "t-refund", PLAN);
    assert_eq!(file(&s, "t-refund", "specs/specs/billing/partial.feature"), PARTIAL);
    let packet = packet(&s, "t-refund", ExecutionRole::Implementer);
    assert!(packet.contains(&format!("{FOLDER}/specs/specs/billing/partial.feature")), "{packet}");
    assert!(packet.contains("do not commit them"), "{packet}");
    assert!(!packet.contains("Part of an order"), "{packet}");
}

// spec: stage-handover.packet/failed-validations-whole
#[test]
fn every_failed_validation_reaches_the_implementer_whole() {
    let mut s = authoring("t-refund", "Let customers get part of an order refunded");
    propose(&mut s, "t-refund", SpecMode::Uncommitted, &[(PARTIAL_PATH, PARTIAL)]);
    approve(&mut s, "t-refund", PLAN);
    hand_over(&mut s, "t-refund", "b", 2);
    let items = |round: u32| vec![
        ValidationItem { subject: "billing/unit".into(), kind: ValidationKind::Suite, inputs: format!("sha256:unit-{round}") },
        ValidationItem { subject: "billing.partial/part".into(), kind: ValidationKind::Adequate, inputs: format!("sha256:part-{round}") },
    ];
    let record = |round: u32, subject: &str, kind: ValidationKind, verdict: ValidationVerdict, reasoning: &str| Command::RecordValidation {
        task_id: "t-refund".into(), subject: subject.into(), check: kind, verdict, reasoning: Some(reasoning.into()), nearest_wrong: None, output: None,
        evidence: vec![], inputs: items(round).into_iter().find(|item| item.subject == subject).unwrap().inputs, listed: None,
    };
    let mut generation = 2;
    for round in 1..=2 {
        let implementer = assigned(&s, "t-refund");
        must(&mut s, implementer, Command::ReadyForValidation { task_id: "t-refund".into(), head: HEAD.into(), items: items(round), scope: vec![] });
        generation += 1;
        hand_over(&mut s, "t-refund", &format!("v{generation}"), generation);
        let validator = node(&format!("v{generation}"));
        must(&mut s, validator.clone(), record(round, "billing.partial/part", ValidationKind::Adequate, ValidationVerdict::Unmet, "The test refunds 30 EUR, not 10 EUR"));
        must(&mut s, validator.clone(), record(round, "billing/unit", ValidationKind::Suite, ValidationVerdict::Incomplete, "The suite stopped at its time limit"));
        must(&mut s, validator, Command::EndValidation { task_id: "t-refund".into(), findings: "Two tests are skipped".into() });
        generation += 1;
        hand_over(&mut s, "t-refund", &format!("i{generation}"), generation);
    }
    let packet = packet(&s, "t-refund", ExecutionRole::Implementer);
    assert!(packet.contains("round 2"), "{packet}");
    for wanted in ["billing.partial/part", "The test refunds 30 EUR, not 10 EUR", "billing/unit", "The suite stopped at its time limit", "Two tests are skipped"] {
        assert!(packet.contains(wanted), "{wanted}: {packet}");
    }
}

/// The agent that works on `task` now.
fn assigned(s: &DomainState, task: &str) -> Actor {
    Actor::Agent { agent_id: s.tasks[&task_id(task)].agent_id.clone().expect("an agent works on the task") }
}

// ---- What the stage handover may reach ----

const BOUNDARY: &str = include_str!("../../../specs/stage-handover/boundary.toml");
/// Each file the component owns, by its path in the repository.
const OWNED: [(&str, &str); 2] = [
    ("crates/engine-kernel/src/stage_packet.rs", include_str!("stage_packet.rs")),
    ("crates/store/src/context_handoff.rs", include_str!("../../store/src/context_handoff.rs")),
];

/// The quoted entries of the list `key` of the boundary, empty when it has none.
fn listed(key: &str) -> Vec<String> {
    let Some(start) = BOUNDARY.lines().position(|line| line.trim_start().starts_with(&format!("{key} ="))) else { return vec![] };
    let list: String = BOUNDARY.lines().skip(start).scan(false, |closed, line| {
        if *closed { return None }
        *closed = line.contains(']');
        Some(line.to_owned())
    }).collect::<Vec<_>>().join("\n");
    list.split('"').skip(1).step_by(2).map(str::to_owned).collect()
}

// spec: stage-handover.boundary/reaches-nothing-outside
#[test]
fn the_packet_and_its_budget_never_reach_the_disk_the_network_a_program_or_a_thread() {
    let owns: BTreeSet<String> = listed("owns").into_iter().collect();
    let scanned: BTreeSet<String> = OWNED.iter().map(|(path, _)| (*path).to_owned()).collect();
    assert_eq!(scanned, owns, "every file the component owns is searched");
    for (path, source) in OWNED {
        for reached in ["std::fs", "std::process", "std::net", "std::thread"] {
            assert!(!source.contains(reached), "{path} uses {reached}");
        }
    }
}

// spec: stage-handover.boundary/depends-on-no-component
#[test]
fn the_stage_handover_depends_on_no_other_component() {
    assert!(listed("owns").len() == 2, "the boundary reads");
    assert_eq!(listed("depends_on"), Vec::<String>::new());
}
