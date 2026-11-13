//! A task never ends with its pull request open and nobody to answer for it: the user's cancel
//! closes the PR on GitHub, and anyone else's close hands the PR to the user as a delivery in the
//! same command.
use super::lifecycle_tests::{assert_aggregate_rules, exec, must, node, sdk_turn, user, world};
use super::*;
use crate::open_pr_delivery::{OpenPrDelivery, PrReadiness};
use proptest::prelude::*;

const HEAD: &str = "1111111111111111111111111111111111111111";
const PUSHED: &str = "2222222222222222222222222222222222222222";
const PR: &str = "github.com/bitomule/musts#113";

fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}
fn task(s: &DomainState) -> &Task {
    &s.tasks[&TaskId::from("t")]
}
fn pr(s: &DomainState) -> &PullRequest {
    &s.pull_requests[&PrId::from(PR)]
}
fn end_empty(s: &mut DomainState, turn: &str) {
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: turn.into(), failure: None, failed: None });
}
fn start(s: &mut DomainState, turn: &str) {
    let lifecycle = s.lifecycle_of(&"t".into()).unwrap();
    let input = lifecycle.pending_inputs().first().map(|input| input.as_str().to_owned()).expect("the wake to start with");
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: turn.into(), input: input.into() },
    ] });
}
fn open_case(s: &DomainState) -> InteractionId {
    s.interactions.values().find(|i| is_stall_case(i) && s.open(i)).map(|i| i.id.clone()).expect("an open case")
}
fn settle_case(s: &mut DomainState, actor: Actor, outcome: StallOutcome) {
    let case_id = open_case(s);
    must(s, actor, Command::SettleStall { case_id, task_id: "t".into(), outcome });
}
fn status(state: PrState, mergeable: PrMergeable, checks: PrChecks) -> OpenPrStatus {
    OpenPrStatus { state, head: PUSHED.into(), base: "main".into(), mergeable, checks }
}
fn running(names: &[&str]) -> PrChecks {
    PrChecks::Pending { names: names.iter().map(|name| (*name).to_owned()).collect() }
}
fn settle_pr(s: &mut DomainState, status: OpenPrStatus) -> Result<CommandOutcome, DomainError> {
    exec(s, Actor::System, Command::SettleOpenPr { task_id: "t".into(), status })
}
/// The close an older build stored to deliver the PR once GitHub reported it.
fn held(s: &mut DomainState, result: &str) {
    s.tasks.get_mut(&TaskId::from("t")).unwrap().open_pr = Some(OpenPrPending::Deliver { result: Some(result.into()) });
}
fn alondras_delivery(s: &DomainState) -> &Interaction {
    let id = task(s).final_presentation.as_ref().expect("a delivery");
    &s.interactions[id]
}

/// A node working on task `t` that registered its PR and never presented it.
fn registered() -> DomainState {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    must(&mut s, node("a"), Command::RegisterPr {
        task_id: "t".into(), pr_id: PR.into(), host: "github.com".into(), repository: "bitomule/musts".into(), number: 113,
        head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    s
}

#[test]
fn only_alondra_can_adopt_or_request_adoption_and_a_live_task_cannot_be_taken() {
    let mut s = registered();
    for actor in [user(), orchestrator(), node("a")] {
        let adopt = Command::AdoptOpenPr { task_id: "t".into(), pr_id: PR.into(), branch: "feature".into(), status: status(PrState::Open, PrMergeable::Clean, PrChecks::Passed) };
        assert_eq!(exec(&mut s, actor.clone(), adopt).unwrap_err().code, "forbidden");
        assert_eq!(exec(&mut s, actor, Command::RequestPrAdoption { branch: "feature".into(), pr: OpenedPr {
            host: "github.com".into(), repository: "owner/repo".into(), number: 8,
        } }).unwrap_err().code, "forbidden");
    }
    let adopt = Command::AdoptOpenPr { task_id: "t".into(), pr_id: PR.into(), branch: "feature".into(), status: status(PrState::Open, PrMergeable::Clean, PrChecks::Passed) };
    assert_eq!(exec(&mut s, Actor::System, adopt).unwrap_err().code, "task_not_active");
    super::lifecycle_tests::task(&mut s, "new", Some("b"));
    let unknown = OpenedPr { host: "github.com".into(), repository: "owner/repo".into(), number: 8 };
    must(&mut s, Actor::System, Command::RequestPrAdoption { pr: unknown.clone(), branch: "worktree-new".into() });
    assert!(!s.pr_adoptions.contains(&unknown), "the transaction rechecks a branch that acquired a live task");
    assert_eq!(exec(&mut s, node("b"), Command::RegisterPr {
        task_id: "new".into(), pr_id: PR.into(), host: "github.com".into(), repository: "bitomule/musts".into(), number: 113,
        head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    }).unwrap_err().code, "pr_owned_by_other_task");
}

/// The user cancelled `t`, GitHub closed its PR, and GitHub later reopened it: nothing closes it now.
fn cancelled_with_its_pr_reopened() -> DomainState {
    let mut s = registered();
    must(&mut s, user(), Command::CancelTask { task_id: "t".into(), reason: None });
    must(&mut s, Actor::System, Command::ObservePrState { pr_id: PR.into(), head: HEAD.into(), base: "main".into(), state: PrState::Closed, merged_head: None });
    must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: None });
    assert_eq!((task(&s).state, task(&s).open_pr.clone()), (TaskState::Cancelled, None));
    s
}
fn close_cancelled(s: &mut DomainState, actor: Actor, status: OpenPrStatus) -> Result<CommandOutcome, DomainError> {
    exec(s, actor, Command::ClosePrOfCancelledTask { task_id: "t".into(), pr_id: PR.into(), branch: "feature".into(), status })
}

#[test]
fn a_cancelled_tasks_open_pr_is_closed_with_the_cancels_reason() {
    let mut s = cancelled_with_its_pr_reopened();
    close_cancelled(&mut s, Actor::System, status(PrState::Open, PrMergeable::Clean, PrChecks::Passed)).unwrap();
    assert_eq!(task(&s).open_pr, Some(OpenPrPending::Close { comment: format!("Cancelled in Alondra: {}", task(&s).title) }));
    assert_eq!((pr(&s).state, pr(&s).head.as_str()), (PrState::Open, PUSHED), "Alondra holds the PR as GitHub reports it");
    assert_eq!(task(&s).state, TaskState::Cancelled);
    let settled = s.clone();
    close_cancelled(&mut s, Actor::System, status(PrState::Open, PrMergeable::Clean, PrChecks::Passed)).unwrap();
    assert_eq!(s, settled, "asking twice changes nothing");
    must(&mut s, Actor::System, Command::ObservePrState { pr_id: PR.into(), head: PUSHED.into(), base: "main".into(), state: PrState::Closed, merged_head: None });
    assert_eq!(task(&s).open_pr, None, "GitHub reporting it closed ends the pending close");
    assert_aggregate_rules(&s);
}

#[test]
fn only_alondra_closes_the_pr_of_a_cancelled_task_and_only_while_github_reports_it_open() {
    let mut s = cancelled_with_its_pr_reopened();
    for actor in [user(), orchestrator(), node("a")] {
        assert_eq!(close_cancelled(&mut s, actor, status(PrState::Open, PrMergeable::Clean, PrChecks::Passed)).unwrap_err().code, "forbidden");
    }
    for state in [PrState::Closed, PrState::Merged] {
        let before = s.clone();
        assert_eq!(close_cancelled(&mut s, Actor::System, status(state, PrMergeable::Clean, PrChecks::Passed)).unwrap_err().code, "open_pr_close");
        assert_eq!(s, before);
    }
    let mut running = registered();
    assert_eq!(close_cancelled(&mut running, Actor::System, status(PrState::Open, PrMergeable::Clean, PrChecks::Passed)).unwrap_err().code, "open_pr_close",
        "a task still working keeps its PR");
    let before = s.clone();
    let command = Command::ClosePrOfCancelledTask { task_id: "t".into(), pr_id: "github.com/owner/repo#1".into(), branch: "feature".into(),
        status: status(PrState::Open, PrMergeable::Clean, PrChecks::Passed) };
    assert_eq!(exec(&mut s, Actor::System, command).unwrap_err().code, "open_pr_close");
    assert_eq!(s, before);
    let mut ownership = registered();
    super::lifecycle_tests::task(&mut ownership, "other", Some("b"));
    must(&mut ownership, user(), Command::CancelTask { task_id: "other".into(), reason: None });
    must(&mut ownership, Actor::System, Command::RecordNodeEnd { task_id: "other".into(), end: NodeEnd::Closed, agent_id: None });
    assert_eq!(ownership.tasks[&TaskId::from("other")].state, TaskState::Cancelled);
    let before = ownership.clone();
    let command = Command::ClosePrOfCancelledTask { task_id: "other".into(), pr_id: PR.into(), branch: "feature".into(),
        status: status(PrState::Open, PrMergeable::Clean, PrChecks::Passed) };
    assert_eq!(exec(&mut ownership, Actor::System, command).unwrap_err().code, "open_pr_close");
    assert_eq!(ownership, before, "a cancelled task cannot close another task's PR");
}

#[test]
fn a_cancelled_tasks_pr_is_not_closed_if_its_branch_acquired_a_live_task_since_the_sweep() {
    let mut s = registered();
    let branch = "worktree-new";
    assert!(!s.branch_has_live_task(branch));
    let github = status(PrState::Open, PrMergeable::Clean, PrChecks::Passed);
    super::lifecycle_tests::task(&mut s, "new", Some("b"));
    must(&mut s, user(), Command::CancelTask { task_id: "t".into(), reason: None });
    must(&mut s, Actor::System, Command::ObservePrState { pr_id: PR.into(), head: HEAD.into(), base: "main".into(), state: PrState::Closed, merged_head: None });
    must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: None });
    assert_eq!((task(&s).state, task(&s).open_pr.clone()), (TaskState::Cancelled, None));
    assert!(s.branch_has_live_task(branch));
    let before = s.clone();
    exec(&mut s, Actor::System, Command::ClosePrOfCancelledTask {
        task_id: "t".into(), pr_id: PR.into(), branch: branch.into(), status: github,
    }).unwrap();
    assert_eq!(s, before, "the transaction rechecks the branch before scheduling a close");
}

/// The stops of t-musts-validate-ignored-parent: its node registered musts #113 and its turns
/// kept ending without presenting it, so the stop after its wakes became a case for the orchestrator.
fn stalled_with_open_pr() -> DomainState {
    let mut s = registered();
    end_empty(&mut s, "turn-1");
    for n in 2..=alondra_domain::EMPTY_TURN_WAKES + 1 {
        let turn = format!("turn-{n}");
        start(&mut s, &turn);
        end_empty(&mut s, &turn);
    }
    s
}

/// The sequence the command audit recorded for musts #113 and #114 (revisions 196057 and
/// 196122): record_turn_boundary, bind_orchestrator_session, settle_stall with close. It used to
/// end in closing and then cancelled, with the PR left open on GitHub.
#[test]
fn the_recorded_settle_stall_of_a_task_with_its_pr_open_delivers_the_pr_instead_of_cancelling() {
    let mut s = stalled_with_open_pr();
    must(&mut s, Actor::System, Command::BindOrchestratorSession { session_id: "orchestrator-session".into() });
    let result = "The fix and its test are in PR #113, green.";
    let case = open_case(&s);
    settle_case(&mut s, orchestrator(), StallOutcome::Close { result: result.into() });
    assert!(!s.open(&s.interactions[&case]), "the delivery ends the case");
    assert!(exec(&mut s, orchestrator(), Command::SettleStall { case_id: case.clone(), task_id: "t".into(),
        outcome: StallOutcome::Close { result: result.into() } }).is_err(), "nobody settles the case again");
    let delivery = alondras_delivery(&s);
    assert_eq!((delivery.author.clone(), delivery.system_card.clone(), delivery.card.kind),
        (Author::System, Some(SystemCard::OpenPrDelivery), CardKind::Review));
    assert!(matches!(&delivery.payload, InteractionPayload::Delivery { targets, is_final: true, .. }
        if *targets == vec![DeliveryTarget::PullRequest { pr_id: PR.into(), head: HEAD.into() }]));
    let card = OpenPrDelivery::from_body(&delivery.body).expect("the card's body");
    assert_eq!((card.number, card.readiness, card.result.as_deref()), (113, PrReadiness::Unconfirmed, Some(result)));
    assert_eq!(task(&s).open_pr, None);
    assert_eq!(task(&s).state, TaskState::ForYourReview);

    let presentation = task(&s).final_presentation.clone().unwrap();
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: presentation,
        targets: vec![DeliveryTarget::PullRequest { pr_id: PR.into(), head: HEAD.into() }] });
    assert!(s.merge_authorizations.values().any(|a| a.pr_id == PrId::from(PR) && a.head == HEAD && a.valid),
        "accepting it lets Alondra merge it");
    assert_aggregate_rules(&s);
}

#[test]
fn a_close_an_older_build_held_for_github_is_delivered_as_github_reports_the_pr() {
    let mut s = stalled_with_open_pr();
    held(&mut s, "Done");
    settle_pr(&mut s, status(PrState::Open, PrMergeable::Clean, PrChecks::Passed)).unwrap();
    let card = OpenPrDelivery::from_body(&alondras_delivery(&s).body).unwrap();
    assert_eq!((card.readiness, card.result.as_deref()), (PrReadiness::Ready, Some("Done")));
    assert_eq!((task(&s).state, task(&s).open_pr.clone()), (TaskState::ForYourReview, None));
    assert_eq!(pr(&s).head, PUSHED, "the delivery is of the head GitHub reported");
    assert_aggregate_rules(&s);
}

#[test]
fn a_pr_with_conflicts_or_failing_checks_reaches_the_user_saying_what_keeps_it_from_merging() {
    for (mergeable, checks, readiness) in [
        (PrMergeable::Conflicting, PrChecks::Failed { names: vec!["test".into()] }, PrReadiness::Conflicting { base: "main".into() }),
        (PrMergeable::Clean, PrChecks::Failed { names: vec!["test".into()] }, PrReadiness::ChecksFailed { names: vec!["test".into()] }),
    ] {
        let mut s = stalled_with_open_pr();
        held(&mut s, "Unfinished");
        settle_pr(&mut s, status(PrState::Open, mergeable, checks)).unwrap();
        assert_eq!(OpenPrDelivery::from_body(&alondras_delivery(&s).body).unwrap().readiness, readiness);
        assert_eq!(task(&s).state, TaskState::ForYourReview);
    }
}

#[test]
fn the_pr_is_delivered_at_once_whatever_its_checks_and_merge_state_still_do() {
    for (mergeable, checks, readiness) in [
        (PrMergeable::Clean, running(&["e2e"]), PrReadiness::ChecksRunning { names: vec!["e2e".into()] }),
        (PrMergeable::Computing, running(&["e2e"]), PrReadiness::ChecksRunning { names: vec!["e2e".into()] }),
        (PrMergeable::Computing, PrChecks::Passed, PrReadiness::Unconfirmed),
    ] {
        let mut s = stalled_with_open_pr();
        held(&mut s, "Done");
        settle_pr(&mut s, status(PrState::Open, mergeable, checks)).unwrap();
        assert_eq!(OpenPrDelivery::from_body(&alondras_delivery(&s).body).unwrap().readiness, readiness);
        assert_eq!((task(&s).state, task(&s).open_pr.clone()), (TaskState::ForYourReview, None));
        assert_aggregate_rules(&s);
    }
}

#[test]
fn a_pr_closed_on_github_lets_the_task_be_cancelled_with_its_result() {
    let mut s = stalled_with_open_pr();
    held(&mut s, "Superseded");
    settle_pr(&mut s, status(PrState::Closed, PrMergeable::Clean, PrChecks::None)).unwrap();
    assert_eq!(pr(&s).state, PrState::Closed);
    assert_eq!(task(&s).ending, Some(TaskEnding::Cancelling));
    assert_eq!(task(&s).stall_closing, Some(StallClosing::Settled { result: "Superseded".into() }));
    assert_eq!(task(&s).open_pr, None);
}

#[test]
fn a_pr_merged_on_github_finishes_the_task_instead_of_cancelling_it() {
    let mut s = stalled_with_open_pr();
    held(&mut s, "Done");
    settle_pr(&mut s, status(PrState::Merged, PrMergeable::Clean, PrChecks::Passed)).unwrap();
    assert_eq!(pr(&s).state, PrState::Merged);
    assert_eq!(alondras_delivery(&s).card.kind, CardKind::Know);
    assert_eq!(task(&s).state, TaskState::Closing, "the merge settles the delivery and the task closes as done");
    assert_ne!(task(&s).ending, Some(TaskEnding::Cancelling));
    assert!(s.finished(task(&s)) && task(&s).wrap_up.asked_at.is_some(), "the node is asked to clean up and close");
}

#[test]
fn alondra_leaving_the_case_unsettled_also_delivers_the_open_pr() {
    let mut s = stalled_with_open_pr();
    settle_case(&mut s, Actor::System, StallOutcome::Unsettled);
    assert_eq!((task(&s).state, task(&s).open_pr.clone()), (TaskState::ForYourReview, None));
    assert_eq!(alondras_delivery(&s).system_card, Some(SystemCard::OpenPrDelivery));
}

#[test]
fn the_orchestrators_cancel_of_a_task_with_its_pr_open_delivers_the_pr() {
    let mut s = registered();
    must(&mut s, orchestrator(), Command::CancelTask { task_id: "t".into(), reason: None });
    assert_eq!((task(&s).state, task(&s).open_pr.clone()), (TaskState::ForYourReview, None));
    assert_eq!(OpenPrDelivery::from_body(&alondras_delivery(&s).body).unwrap().readiness, PrReadiness::Unconfirmed);
}

/// The orchestrator closed a running task three times while its PR was open, and
/// each close answered Recorded with the task still running: none of them may leave it running.
#[test]
fn closing_a_running_task_with_its_pr_open_delivers_the_pr_in_the_same_command_and_refuses_a_repeat() {
    let mut s = registered();
    must(&mut s, orchestrator(), Command::CancelTask { task_id: "t".into(), reason: None });
    assert_eq!((task(&s).state, task(&s).open_pr.clone()), (TaskState::ForYourReview, None));
    let delivery = alondras_delivery(&s);
    assert_eq!((delivery.author.clone(), delivery.system_card.clone(), delivery.card.kind),
        (Author::System, Some(SystemCard::OpenPrDelivery), CardKind::Review));
    assert!(matches!(&delivery.payload, InteractionPayload::Delivery { targets, is_final: true, .. }
        if *targets == vec![DeliveryTarget::PullRequest { pr_id: PR.into(), head: HEAD.into() }]));
    assert_eq!(OpenPrDelivery::from_body(&delivery.body).unwrap().readiness, PrReadiness::Unconfirmed,
        "nothing waits for GitHub: what keeps it from merging is read when the user merges it");
    let delivered = s.clone();
    for _ in 0..2 {
        let refused = exec(&mut s, orchestrator(), Command::CancelTask { task_id: "t".into(), reason: None }).unwrap_err();
        assert_eq!(refused.code, "open_pr_delivered", "{}", refused.message);
        assert_eq!(s, delivered, "a repeated close is refused and changes nothing");
    }
    assert_aggregate_rules(&s);
}

#[test]
fn the_users_cancel_closes_the_pr_with_the_reason_until_github_reports_it_closed() {
    let mut s = registered();
    must(&mut s, user(), Command::CancelTask { task_id: "t".into(), reason: None });
    assert_eq!(task(&s).ending, Some(TaskEnding::Cancelling));
    assert_eq!(task(&s).open_pr, Some(OpenPrPending::Close { comment: format!("Cancelled in Alondra: {}", task(&s).title) }));
    must(&mut s, Actor::System, Command::ObservePrState { pr_id: PR.into(), head: HEAD.into(), base: "main".into(),
        state: PrState::Closed, merged_head: None });
    assert_eq!(task(&s).open_pr, None);
}

fn with_a_second_pr(s: &mut DomainState) {
    must(s, node("a"), Command::RegisterPr {
        task_id: "t".into(), pr_id: "github.com/acme/site#6".into(), host: "github.com".into(), repository: "acme/site".into(), number: 6,
        head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
}

#[test]
fn a_task_closed_with_two_prs_open_delivers_both_and_a_cancel_closes_both() {
    let mut delivered = registered();
    with_a_second_pr(&mut delivered);
    must(&mut delivered, orchestrator(), Command::CancelTask { task_id: "t".into(), reason: None });
    let InteractionPayload::Delivery { targets, .. } = &alondras_delivery(&delivered).payload else { panic!("a delivery") };
    assert_eq!(targets.len(), 2, "{targets:?}");

    let mut cancelled = registered();
    with_a_second_pr(&mut cancelled);
    must(&mut cancelled, user(), Command::CancelTask { task_id: "t".into(), reason: None });
    let closed = |pr: &str, head: &str| Command::ObservePrState { pr_id: pr.into(), head: head.into(), base: "main".into(), state: PrState::Closed, merged_head: None };
    must(&mut cancelled, Actor::System, closed(PR, HEAD));
    assert!(task(&cancelled).open_pr.is_some(), "the other PR is still to close");
    must(&mut cancelled, Actor::System, closed("github.com/acme/site#6", HEAD));
    assert!(task(&cancelled).open_pr.is_none());
}

#[test]
fn cancelling_on_a_start_problem_closes_the_pr_like_any_other_cancel_of_the_user() {
    for problem in [StartProblem::ApiFailed { error: "server_error".into(), detail: None, attempts: 4 }, StartProblem::NodeClosed] {
        let mut s = registered();
        must(&mut s, Actor::System, Command::RaiseStartProblem { task_id: "t".into(), problem });
        let card = s.open_start_problem(&"t".into()).map(|i| i.id.clone()).expect("the card");
        must(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card, option_id: START_CANCEL.into(), inputs: Default::default() });
        assert_eq!(task(&s).ending, Some(TaskEnding::Cancelling));
        assert_eq!(task(&s).open_pr, Some(OpenPrPending::Close { comment: format!("Cancelled in Alondra: {}", task(&s).title) }));
        assert_aggregate_rules(&s);
    }
}

#[test]
fn only_alondra_settles_an_open_pr_and_only_one_it_holds() {
    let mut s = registered();
    let refused = settle_pr(&mut s, status(PrState::Open, PrMergeable::Clean, PrChecks::Passed)).unwrap_err();
    assert_eq!(refused.code, "open_pr_settled");
    must(&mut s, orchestrator(), Command::CancelTask { task_id: "t".into(), reason: None });
    for actor in [user(), orchestrator(), node("a")] {
        let refused = exec(&mut s, actor, Command::SettleOpenPr { task_id: "t".into(),
            status: status(PrState::Open, PrMergeable::Clean, PrChecks::Passed) }).unwrap_err();
        assert_eq!(refused.code, "forbidden");
    }
}

#[derive(Clone, Copy, Debug)]
enum Closer {
    SettleClose,
    SettleUnsettled,
    OrchestratorCancel,
    UserCancel,
}

proptest! {
    /// Whoever closes a task with its PR open, the task is never cancelled with its PR open and
    /// nothing left to close it: the user's cancel closes the PR, anyone else's delivers it.
    #[test]
    fn no_close_leaves_a_cancelled_task_with_its_pr_open(
        closer in prop_oneof![Just(Closer::SettleClose), Just(Closer::SettleUnsettled), Just(Closer::OrchestratorCancel), Just(Closer::UserCancel)],
        state in prop_oneof![Just(PrState::Open), Just(PrState::Closed), Just(PrState::Merged)],
        mergeable in prop_oneof![Just(PrMergeable::Clean), Just(PrMergeable::Conflicting), Just(PrMergeable::Computing), Just(PrMergeable::Other)],
        checks in prop_oneof![Just(PrChecks::None), Just(running(&["lint"])), Just(PrChecks::Passed), Just(PrChecks::Failed { names: vec!["test".into()] })],
    ) {
        let mut s = stalled_with_open_pr();
        let case = open_case(&s);
        let settle = |outcome| Command::SettleStall { case_id: case.clone(), task_id: "t".into(), outcome };
        let _ = match closer {
            Closer::SettleClose => exec(&mut s, orchestrator(), settle(StallOutcome::Close { result: "Done".into() })),
            Closer::SettleUnsettled => exec(&mut s, Actor::System, settle(StallOutcome::Unsettled)),
            Closer::OrchestratorCancel => exec(&mut s, orchestrator(), Command::CancelTask { task_id: "t".into(), reason: None }),
            Closer::UserCancel => exec(&mut s, user(), Command::CancelTask { task_id: "t".into(), reason: None }),
        }.unwrap();
        if matches!(task(&s).open_pr, Some(OpenPrPending::Deliver { .. })) {
            settle_pr(&mut s, OpenPrStatus { state, head: PUSHED.into(), base: "main".into(), mergeable, checks }).unwrap();
        }
        let t = task(&s);
        let cancelled = t.cancelled() || t.ending == Some(TaskEnding::Cancelling);
        prop_assert!(!(cancelled && pr(&s).state == PrState::Open && !matches!(t.open_pr, Some(OpenPrPending::Close { .. }))),
            "cancelled with its PR open and nothing to close it: {:?} {:?}", t.state, t.open_pr);
        prop_assert!(matches!(closer, Closer::UserCancel) || !cancelled || pr(&s).state == PrState::Closed,
            "only the user, or a PR closed on GitHub, cancels it: {:?}", t.state);
        prop_assert!(matches!(closer, Closer::UserCancel) || t.state == TaskState::ForYourReview,
            "anyone else's close delivers the PR in the same command: {:?} {:?}", t.state, t.open_pr);
        assert_aggregate_rules(&s);
    }
}
