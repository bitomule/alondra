//! A task in stages: the role of each execution, the plan approval that hands the author's task
//! to a new implementer, and the tools each role may use.
use super::lifecycle_tests::{assert_aggregate_rules, exec, last, must, node, task, user, world};
use super::*;
use alondra_core::accounts::{AccountKey, Provider};
use alondra_core::model_selection::{ModelKey, ModelProfile, ModelSelection, PendingTaskSelection, TaskSelection};
use proptest::prelude::*;
use serde_json::json;
use std::collections::BTreeMap;

fn stages_command(staged: bool) -> Command {
    let selections = if staged { TaskStages::role_selections(&TaskSelection::automatic(ModelProfile::Implementation)) } else { BTreeMap::new() };
    Command::RecordTaskStages { task_id: "t".into(), staged, reason: "It touches the task state machine".into(), selections }
}
fn admit(s: &mut DomainState, agent: &str, generation: u64) {
    must(s, Actor::System, Command::AdmitExecution { provider: Default::default(), task_id: "t".into(), agent_id: agent.into(),
        conversation: format!("conversation-{generation}"), instance: format!("instance-{generation}"), host: "sdk-host".into(),
        model: None, account: None, leaves_limit: None });
}
fn turn(s: &mut DomainState) {
    let LifecycleRecord::Planned { input } = s.lifecycle_journal[&TaskId::from("t")][0].clone() else { panic!("planned first") };
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::ResourceAvailable, RuntimeObservation::TurnStarted { turn: "turn-1".into(), input },
    ] });
}
/// A task whose first execution, of agent `a`, has its first turn running.
fn running(staged: Option<bool>) -> DomainState {
    let mut s = world();
    task(&mut s, "t", None);
    if let Some(staged) = staged {
        must(&mut s, Actor::System, stages_command(staged));
    }
    admit(&mut s, "a", 1);
    turn(&mut s);
    s
}
fn plan_card() -> Command {
    let body = "# Plan\n\n- [ ] Add the parser\n- [ ] Test it\n";
    let card = CardSpec::new(CardKind::Review, "Review the plan").content(ContentKind::Plan, body);
    Command::RecordDecisionRequest { task_id: "t".into(), generation: 1, epoch: 7, hold: Hold::Plan, tool: Some("plan".into()), input: json!({}), mcp_server: None,
        prompt: DecisionPrompt::Card { body: body.into(), options: Hold::Plan.options_offering(false), card } }
}
fn approve(s: &mut DomainState) {
    must(s, Actor::System, plan_card());
    must(s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: decision_card_id(&"t".into(), 1, 7), option_id: Hold::APPROVE.into(), inputs: BTreeMap::new() });
}
fn pending() -> PendingTaskSelection {
    PendingTaskSelection { account: AccountKey::native(Provider::Claude),
        model: ModelSelection { model: ModelKey { provider: Provider::Claude, model: "sonnet".into() }, reasoning_effort: None, catalog_revision: "catalog".into() },
        reason: "The implementer takes the task over".into() }
}
fn begin(s: &mut DomainState, transition: &str) -> Result<CommandOutcome, DomainError> {
    let expected_selection = s.task_selection_stamp(&"t".into()).unwrap();
    exec(s, Actor::System, Command::BeginStageHandover { task_id: "t".into(), expected_selection, transition: transition.into(), pending: pending() })
}
fn stages(s: &DomainState) -> &TaskStages {
    &s.tasks[&TaskId::from("t")].stages
}
fn roles(s: &DomainState) -> Vec<Option<ExecutionRole>> {
    stages(s).runs.iter().map(|run| run.role).collect()
}

#[test]
fn the_first_execution_of_a_staged_task_is_its_author_and_one_without_stages_has_no_role() {
    let s = running(Some(true));
    assert_eq!(roles(&s), [Some(ExecutionRole::Author)]);
    assert_eq!(stages(&s).reason.as_deref(), Some("It touches the task state machine"));
    for staged in [None, Some(false)] {
        let s = running(staged);
        assert_eq!(roles(&s), [None], "a task without stages runs as a single execution");
        assert_eq!(stages(&s).role(), None);
    }
}

#[test]
fn approving_the_authors_plan_is_recorded_ends_its_turn_and_owes_the_implementer() {
    let mut s = running(Some(true));
    approve(&mut s);
    let LifecycleRecord::Decided { decision, .. } = last(&s, "t") else { panic!("the user's answer decides the plan") };
    assert_eq!(decision, DecisionRecord::HandedOver, "the author is told to end its turn, not to build the plan");
    let approval = &stages(&s).approvals[0];
    assert_eq!((approval.generation, approval.role), (1, Some(ExecutionRole::Author)));
    let handover = stages(&s).handover.clone().expect("the task owes its implementer");
    assert_eq!((handover.to, handover.transition, handover.refused), (ExecutionRole::Implementer, None, false));
    assert_eq!(stages(&s).next_role(), Some(ExecutionRole::Implementer));
    assert!(s.lifecycle_journal[&TaskId::from("t")].iter().any(|record| matches!(record, LifecycleRecord::Told { text: Some(text), .. } if *text == stage_input(ExecutionRole::Implementer))),
        "the implementer's input is queued with the approval, so the author's turn ends on it instead of being woken");
    assert_aggregate_rules(&s);
}

#[test]
fn approving_a_plan_of_a_task_without_stages_is_recorded_and_lets_its_node_build_it() {
    let mut s = running(None);
    approve(&mut s);
    let LifecycleRecord::Decided { decision, .. } = last(&s, "t") else { panic!("the user's answer decides the plan") };
    assert_eq!(decision, DecisionRecord::Allow);
    assert_eq!(stages(&s).approvals.len(), 1, "every approval is recorded, with or without stages");
    assert_eq!(stages(&s).handover, None);
}

#[test]
fn the_implementer_takes_the_task_over_from_a_new_execution_with_its_own_selection() {
    let mut s = running(Some(true));
    approve(&mut s);
    must(&mut s, Actor::System, Command::RecordDecisionSettled { task_id: "t".into(), generation: 1, epoch: 7 });
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    begin(&mut s, "stage-1").unwrap();
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: false, ended_work: vec![], by_alondra: true });
    let task = &s.tasks[&TaskId::from("t")];
    assert_eq!(task.selection.as_ref().map(|selection| selection.profile), Some(ModelProfile::Implementation), "the implementer's selection, kept apart from the task's");
    assert_eq!(task.pending_selection, Some(pending()));
    assert_eq!(stages(&s).handover.as_ref().and_then(|handover| handover.transition.as_deref()), Some("stage-1"));
    assert_eq!(begin(&mut s, "stage-2").unwrap_err().code, "conflict", "one change of role at a time");
    admit(&mut s, "b", 2);
    assert_eq!(roles(&s), [Some(ExecutionRole::Author), Some(ExecutionRole::Implementer)]);
    assert_eq!(stages(&s).handover, None, "the change of role ends with the implementer's admission");
    assert_eq!(s.tasks[&TaskId::from("t")].agent_id, Some(AgentId::from("b")));
    assert_aggregate_rules(&s);
}

#[test]
fn a_change_of_role_that_keeps_failing_waits_for_the_user_who_can_try_again() {
    let mut s = running(Some(true));
    approve(&mut s);
    for attempt in 1..=MAX_STAGE_ATTEMPTS {
        let id = format!("stage-{attempt}");
        let expected_selection = s.task_selection_stamp(&"t".into()).unwrap();
        must(&mut s, Actor::System, Command::BeginStageHandover { task_id: "t".into(), expected_selection, transition: id.clone(), pending: pending() });
        must(&mut s, Actor::System, Command::AbortTaskSelection { task_id: "t".into(), selection: None, refused: None });
        must(&mut s, Actor::System, Command::StageHandoverFailed { task_id: "t".into(), transition: Some(id), refused: false });
    }
    let handover = stages(&s).handover.clone().unwrap();
    assert!(handover.refused && !handover.due(), "Alondra stops trying by itself after {MAX_STAGE_ATTEMPTS} tries");
    assert_eq!(begin(&mut s, "stage-4").unwrap_err().code, "conflict");
    must(&mut s, Actor::System, Command::RaiseStartProblem { task_id: "t".into(), problem: StartProblem::StageStartRefused { role: ExecutionRole::Implementer, error: "No account can start it".into() } });
    let card = s.open_start_problem(&"t".into()).expect("the user is asked").id.clone();
    must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card, option_id: START_RETRY.into(), inputs: BTreeMap::new() });
    assert!(stages(&s).handover.as_ref().is_some_and(StageHandover::due), "trying again makes the change due once more");
}

#[test]
fn each_role_uses_only_its_own_tools() {
    let mut s = running(Some(true));
    let refused = |s: &mut DomainState, actor: Actor, command: Command| exec(s, actor, command).unwrap_err();
    let post_merge = || Command::DeclarePostMergeWork { task_id: "t".into(), reason: Some("Publish the release".into()), waits_for: vec![] };
    let error = refused(&mut s, node("a"), post_merge());
    assert_eq!((error.code.as_str(), error.subject.as_deref()), ("role_forbids_tool", Some("post_merge_work")));
    assert!(error.message.contains("author"), "{error:?}");
    let verdict = Command::RecordVerdict { task_id: "t".into(), presentation_id: "p".into(), verdict: Verdict::Approved { summary: "Fine".into(), open_issues: false, serious_findings: 0 } };
    assert_eq!(refused(&mut s, node("a"), verdict.clone()).code, "role_forbids_tool");
    let mut plain = running(None);
    assert_ne!(refused(&mut plain, node("a"), verdict).code, "role_forbids_tool", "a task without stages keeps every tool");
}

#[test]
fn a_plan_from_the_author_after_its_approval_is_denied_without_a_card() {
    let mut s = running(Some(true));
    approve(&mut s);
    must(&mut s, Actor::System, Command::RecordDecisionSettled { task_id: "t".into(), generation: 1, epoch: 7 });
    let Command::RecordDecisionRequest { prompt, .. } = plan_card() else { unreachable!() };
    let outcome = must(&mut s, Actor::System, Command::RecordDecisionRequest { task_id: "t".into(), generation: 1, epoch: 8, hold: Hold::Plan, tool: Some("plan".into()), input: json!({}), mcp_server: None, prompt });
    assert!(outcome.created_ids.is_empty(), "no card for the user");
    assert_eq!(last(&s, "t"), LifecycleRecord::DecidedByPolicy { epoch: 8, decision: DecisionRecord::Deny { message: HANDED_OVER_PLAN.into() } });
}

#[test]
fn stages_are_chosen_once_before_the_first_node_starts() {
    let mut s = running(None);
    assert_eq!(exec(&mut s, Actor::System, stages_command(true)).unwrap_err().code, "conflict");
    let mut s = world();
    task(&mut s, "t", None);
    let partial = Command::RecordTaskStages { task_id: "t".into(), staged: true, reason: "Large".into(), selections: BTreeMap::new() };
    assert_eq!(exec(&mut s, Actor::System, partial).unwrap_err().code, "invalid_input", "a staged task has a selection for every role");
    assert_eq!(exec(&mut s, user(), stages_command(true)).unwrap_err().code, "forbidden");
}

#[test]
fn tasks_and_executions_stored_before_stages_read_as_a_single_execution() {
    let stored: TaskStages = serde_json::from_value(json!({"runs": [{"generation": 1, "agent": "a"}]})).unwrap();
    assert_eq!((stored.staged, stored.role(), stored.next_role(), stored.runs[0].role), (false, None, None, None));
    let s = running(None);
    let mut old = serde_json::to_value(&s.tasks[&TaskId::from("t")]).unwrap();
    old.as_object_mut().unwrap().remove("stages");
    let task: Task = serde_json::from_value(old).unwrap();
    assert!(task.stages.is_default());
}

#[derive(Clone, Debug)]
enum Step {
    Admit,
    Approve,
    Fail,
}

proptest! {
    /// Whatever happens in whatever order, a staged task plays its roles forward, owes at most one
    /// change of role, never owes the role it already plays once that role is admitted, and keeps
    /// a bounded record.
    #[test]
    fn stages_only_move_forward(steps in proptest::collection::vec(prop_oneof![Just(Step::Admit), Just(Step::Approve), Just(Step::Fail)], 0..80)) {
        let mut stages = TaskStages { staged: true, ..TaskStages::default() };
        let mut generation = 0;
        for step in steps {
            match step {
                Step::Admit => {
                    generation += 1;
                    let role = stages.next_role();
                    stages.admitted(TaskRun { generation, agent: AgentId::from(format!("agent-{generation}").as_str()), role, provider: Provider::Claude, account: None, model: None });
                    prop_assert!(stages.handover.is_none());
                }
                Step::Approve if stages.role() == Some(ExecutionRole::Author) && stages.handover.is_none() => {
                    stages.handover = Some(StageHandover { to: ExecutionRole::Implementer, cause: HandoverCause::PlanApproved { card: "card".into() }, transition: Some("stage".into()), attempts: 0, refused: false });
                }
                Step::Approve => {}
                Step::Fail => if let Some(handover) = stages.handover.as_mut() {
                    handover.attempts = handover.attempts.saturating_add(1);
                    handover.refused = handover.attempts >= MAX_STAGE_ATTEMPTS;
                },
            }
            let played: Vec<_> = stages.runs.iter().map(|run| run.role).collect();
            prop_assert!(played.windows(2).all(|pair| pair[0] <= pair[1]), "roles go back: {played:?}");
            prop_assert!(stages.runs.len() <= MAX_TASK_RUNS);
            if let Some(handover) = &stages.handover {
                prop_assert_ne!(Some(handover.to), stages.role());
            }
        }
    }
}
