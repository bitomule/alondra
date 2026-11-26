//! No sequence of commands brings a task in stages to a delivery the user sees unless every commit
//! it delivers was read by Alondra and is covered by what a validator recorded at the fingerprint
//! read there: whoever delivers (the implementer, the validator, or Alondra delivering the open
//! pull request when the task is closed), in any order with what moves the pull request, the specs
//! and the rounds, across restarts.
use super::lifecycle_tests::{exec, must, node};
use super::spec_tests::{admit, begin, implementing};
use super::*;
use crate::delivery_validation::{CommitReading, LedgerEntry};
use proptest::prelude::*;

const HEADS: [&str; 3] = ["1111111111111111111111111111111111111111", "2222222222222222222222222222222222222222", "3333333333333333333333333333333333333333"];
const ITEMS: [(&str, ValidationKind, &str); 2] = [("billing/unit", ValidationKind::Suite, "sha256:unit"), ("billing.partial/part", ValidationKind::Adequate, "sha256:part")];
const STALE: &str = "sha256:stale";
const BASE: &str = "0000000000000000000000000000000000000000";

#[derive(Clone, Debug)]
enum Step {
    /// The node of the task says the work is ready for a validator.
    Ready,
    /// The next agent takes over the role the task passes to.
    TakeOver,
    /// The node starts the turn its pending input asks for.
    Start,
    /// The node records a validation: which item, its verdict, at the fingerprint read or not.
    Record { item: usize, met: bool, current: bool },
    /// The node ends the round.
    End,
    /// Alondra records what it read of a commit.
    Read { head: usize, worktree: bool, specs: bool, owed: bool, ledger: Vec<(usize, bool, bool)>, stray: bool, implemented: bool, changed: bool, drift: Option<bool>, based: Option<bool>, tipped: bool, side: bool },
    /// The pull request receives a commit.
    Move { head: usize },
    /// The pull request is pointed at another base branch, its head unchanged.
    Rebase { side: bool },
    /// The node delivers its pull request, or a text.
    PresentPr,
    PresentText,
    /// The node asks for a change of the specs.
    SpecChange,
    /// Alondra closes the task: its open pull request is delivered for the node, held until read.
    Close,
    /// What GitHub reports of a held delivery settles it.
    Settle,
    Restart,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        2 => Just(Step::Ready),
        2 => Just(Step::TakeOver),
        2 => Just(Step::Start),
        4 => (0..ITEMS.len(), any::<bool>(), any::<bool>()).prop_map(|(item, met, current)| Step::Record { item, met, current }),
        1 => Just(Step::End),
        5 => (0..HEADS.len(), any::<bool>(), any::<bool>(), any::<bool>(), prop::collection::vec((0..ITEMS.len(), any::<bool>(), any::<bool>()), 0..3), any::<bool>(), any::<bool>(), any::<bool>(), prop::option::weighted(0.2, any::<bool>()), prop::option::weighted(0.9, prop::bool::weighted(0.85)), (prop::bool::weighted(0.85), prop::bool::weighted(0.3)))
            .prop_map(|(head, worktree, specs, owed, ledger, stray, implemented, changed, drift, based, (tipped, side))| Step::Read { head, worktree, specs, owed, ledger, stray, implemented, changed, drift, based, tipped, side }),
        2 => (0..HEADS.len()).prop_map(|head| Step::Move { head }),
        2 => any::<bool>().prop_map(|side| Step::Rebase { side }),
        3 => Just(Step::PresentPr),
        2 => Just(Step::PresentText),
        1 => Just(Step::SpecChange),
        1 => Just(Step::Close),
        1 => Just(Step::Settle),
        1 => Just(Step::Restart),
    ]
}

fn lifecycle(s: &DomainState) -> alondra_domain::TaskLifecycle {
    crate::replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap()
}
fn task(s: &DomainState) -> &Task {
    &s.tasks[&TaskId::from("t")]
}
fn agent(s: &DomainState) -> Actor {
    node(task(s).agent_id.as_ref().map_or("b", |agent| agent.0.as_str()))
}
fn generation(s: &DomainState) -> u64 {
    lifecycle(s).execution().map_or(2, |execution| execution.generation())
}
fn pr_head(s: &DomainState) -> String {
    s.pull_requests[&PrId::from("pr")].head.clone()
}
fn item(index: usize, current: bool) -> ValidationItem {
    let (subject, kind, inputs) = ITEMS[index];
    ValidationItem { subject: subject.into(), kind, inputs: if current { inputs.into() } else { STALE.into() } }
}
fn present(s: &mut DomainState, id: &str, targets: Vec<DeliveryTarget>) {
    let actor = agent(s);
    let _ = exec(s, actor, Command::PresentResult {
        attachments: vec![], task_id: "t".into(), interaction_id: id.into(), body: "Partial refunds work".into(), targets, is_final: true, nothing_to_review: false,
        checklist: vec![ChecklistMark { item: "1".into(), evidence: vec![Evidence { kind: EvidenceKind::Scenario, value: "billing.partial/part".into(), detail: None }] }],
        card: CardSpec::delivery("Review the result", &[], true),
    });
}

fn run(s: &mut DomainState, step: &Step, n: usize) {
    match step {
        Step::Ready => {
            let items = ITEMS.iter().enumerate().map(|(index, _)| item(index, true)).collect();
            let head = pr_head(s);
            let actor = agent(s);
            let _ = exec(s, actor, Command::ReadyForValidation { task_id: "t".into(), head, items, scope: vec![] });
        }
        Step::TakeOver if task(s).stages.handover.is_some() => {
            let current = generation(s);
            if begin(s, &format!("stage-{n}")).is_ok() {
                let _ = exec(s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: current, resumes: false, ended_work: vec![], by_alondra: true });
                admit(s, &format!("agent-{n}"), current + 1);
            }
        }
        Step::Start if lifecycle(s).turn().is_none() && !lifecycle(s).pending_inputs().is_empty() && lifecycle(s).execution().is_some() => {
            let input = lifecycle(s).pending_inputs()[0].as_str().to_owned();
            let generation = generation(s);
            let _ = exec(s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation, facts: vec![
                RuntimeObservation::TurnStarted { turn: format!("turn-{n}"), input: input.into() },
            ] });
        }
        Step::Record { item: index, met, current } => {
            let it = item(*index, *current);
            let verdict = if *met { ValidationVerdict::Met } else { ValidationVerdict::Unmet };
            let actor = agent(s);
            let _ = exec(s, actor, Command::RecordValidation {
                task_id: "t".into(), subject: it.subject, check: it.kind, verdict, reasoning: Some("Read the test".into()),
                nearest_wrong: Some("Refunding twice".into()), output: None, evidence: vec![], inputs: it.inputs, listed: None,
            });
        }
        Step::End => {
            let actor = agent(s);
            let _ = exec(s, actor, Command::EndValidation { task_id: "t".into(), findings: "A refund of zero is accepted".into() });
        }
        Step::Read { head, worktree, specs, owed, ledger, stray, implemented, changed, drift, based, tipped, side } => {
            let since = task(s).validation.current().map(|round| if *implemented { round.implemented.clone() } else { HEADS[2].to_owned() });
            let reading = CommitReading {
                commit: HEADS[*head].into(), worktree: *worktree, specs: *specs,
                owed: if *owed { vec![item(0, true)] } else { vec![] },
                problems: vec![],
                ledger: ledger.iter().map(|(index, current, met)| {
                    let it = item(*index, *current);
                    LedgerEntry { subject: it.subject, kind: it.kind, inputs: it.inputs, verdict: if *met { ValidationVerdict::Met } else { ValidationVerdict::Unmet } }
                }).collect(),
                stray: if *stray { vec!["specs/ledger/notes.toml".into()] } else { vec![] },
                since, changed: if *changed { vec!["crates/billing/src/lib.rs".into()] } else { vec![] },
                base: based.map(|apart| if apart { BASE.into() } else { HEADS[*head].into() }),
                tip: tipped.then(|| BASE.into()), pr_base: Some(if *side { "side" } else { "main" }.into()), default_branch: Some("main".into()),
                spec_changes: drift.iter().map(|approved| crate::SpecFileChange { path: "alondra.toml".into(), approved: *approved }).collect(),
            };
            let _ = exec(s, Actor::System, Command::RecordCommitReading { task_id: "t".into(), reading });
        }
        Step::Move { head } => {
            let _ = exec(s, Actor::System, Command::ObservePrState { pr_id: "pr".into(), head: HEADS[*head].into(), base: "main".into(), state: PrState::Open, merged_head: None });
        }
        Step::Rebase { side } => {
            let head = pr_head(s);
            let _ = exec(s, Actor::System, Command::ObservePrState { pr_id: "pr".into(), head, base: if *side { "side" } else { "main" }.into(), state: PrState::Open, merged_head: None });
        }
        Step::PresentPr => {
            let head = pr_head(s);
            present(s, &format!("pr-result-{n}"), vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head }]);
        }
        Step::PresentText => present(s, &format!("text-result-{n}"), vec![DeliveryTarget::Text { text: "Done".into() }]),
        Step::SpecChange => {
            let actor = agent(s);
            let _ = exec(s, actor, Command::RequestSpecChange { task_id: "t".into(), reason: "A refund of zero is a case of its own".into() });
        }
        Step::Close => {
            let _ = exec(s, Actor::Orchestrator { generation: 0 }, Command::CancelTask { task_id: "t".into(), reason: None });
        }
        Step::Settle => {
            let status = OpenPrStatus { state: PrState::Open, head: pr_head(s), base: "main".into(), mergeable: PrMergeable::Computing, checks: PrChecks::None };
            let _ = exec(s, Actor::System, Command::SettleOpenPr { task_id: "t".into(), status });
        }
        Step::Restart => *s = serde_json::from_str(&serde_json::to_string(s).unwrap()).unwrap(),
        _ => {}
    }
}

/// Why `delivery` should never have reached the user, judged on its own from what Alondra read and
/// what the validators recorded: `None` when it is covered.
fn uncovered(s: &DomainState, delivery: &InteractionId, validator: bool) -> Option<String> {
    let t = task(s);
    let InteractionPayload::Delivery { targets, .. } = &s.interactions[delivery].payload else { return Some("not a delivery".into()) };
    let mut commits: Vec<String> = targets.iter().filter_map(|target| match target {
        DeliveryTarget::PullRequest { head, .. } => Some(head.clone()),
        _ => None,
    }).collect();
    if commits.is_empty() {
        commits.extend(t.validation.readings.iter().rev().find(|reading| reading.worktree).map(|reading| reading.commit.clone()));
    }
    let mut gated = false;
    for commit in &commits {
        let of_pr = targets.iter().any(|target| matches!(target, DeliveryTarget::PullRequest { head, .. } if head == commit));
        let Some(reading) = t.validation.readings.iter().rev().find(|reading| reading.commit == *commit && reading.worktree != of_pr) else { return Some(format!("{commit} was never read")) };
        if reading.problems.is_empty() && (reading.base.is_none() || (of_pr && reading.tip.is_none())) {
            return Some(format!("{commit} was never compared with its base as GitHub reports it"));
        }
        if of_pr && reading.problems.is_empty() {
            let base = &s.pull_requests[&PrId::from("pr")].base;
            if reading.pr_base.as_ref() != Some(base) || base != "main" {
                return Some(format!("{commit} was read for base {:?} and delivered to {base}, not to the default branch", reading.pr_base));
            }
        }
        if of_pr && reading.problems.is_empty() && reading.base.as_deref() == Some(commit.as_str()) {
            return Some(format!("{commit} was compared with itself"));
        }
        if !reading.spec_changes.is_empty() {
            return Some(format!("{commit} holds spec files the user did not approve, specs turned off included"));
        }
        if !reading.specs {
            continue;
        }
        gated = true;
        if !reading.owed.is_empty() || !reading.problems.is_empty() || !reading.stray.is_empty() {
            return Some(format!("{commit} still owes validations"));
        }
        for entry in &reading.ledger {
            let recorded = t.validation.records.iter().any(|record| record.subject == entry.subject && record.kind == entry.kind && record.inputs == entry.inputs && record.verdict == entry.verdict);
            if !recorded {
                return Some(format!("{} at {} is in the ledger of {commit} and no validator recorded it", entry.subject, entry.inputs));
            }
        }
        if validator && let Some(round) = t.validation.current() && (reading.since.as_deref() != Some(round.implemented.as_str()) || !reading.changed.is_empty()) {
            return Some(format!("{commit} carries changes after the implementer's commit"));
        }
    }
    if gated && t.specs.amendments.iter().any(|a| matches!(a.state, AmendmentState::Requested | AmendmentState::Proposed)) {
        return Some("a change of the specs is open".into());
    }
    None
}

fn with_pr() -> DomainState {
    let mut s = implementing();
    must(&mut s, node("b"), Command::RegisterPr { task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "ana/app".into(),
        number: 1, head: HEADS[0].into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash] });
    s
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn no_sequence_delivers_a_task_in_stages_without_a_validators_verdict_at_the_current_fingerprint(steps in prop::collection::vec(step(), 1..60)) {
        let mut s = with_pr();
        let mut delivered_base: Option<(InteractionId, String)> = None;
        for (n, step) in steps.iter().enumerate() {
            let before = task(&s).final_presentation.clone();
            let validator = task(&s).stages.role() == Some(ExecutionRole::Validator);
            run(&mut s, step, n);
            let after = task(&s).final_presentation.clone();
            if after != before && let Some(delivery) = &after && lifecycle(&s).phase() == alondra_domain::TaskPhase::Delivered {
                let system = s.interactions[delivery].author == Author::System;
                prop_assert_eq!(uncovered(&s, delivery, validator && !system), None, "after {:?}, in {:?}", step, steps);
                delivered_base = Some((delivery.clone(), s.pull_requests[&PrId::from("pr")].base.clone()));
            }
            if let (Some((delivery, base)), Some(standing)) = (&delivered_base, &task(&s).final_presentation)
                && delivery == standing && !s.interactions[delivery].withdrawn && lifecycle(&s).phase() == alondra_domain::TaskPhase::Delivered
            {
                prop_assert_eq!(&s.pull_requests[&PrId::from("pr")].base, base, "a delivery stood after its pull request changed base, in {:?}", steps);
            }
        }
    }
}

#[test]
fn a_hand_written_ledger_never_passes_however_the_task_is_delivered() {
    let mut s = with_pr();
    let reading = CommitReading { commit: HEADS[0].into(), worktree: false, specs: true, owed: vec![], problems: vec![],
        ledger: vec![LedgerEntry { subject: "billing/unit".into(), kind: ValidationKind::Suite, inputs: "sha256:unit".into(), verdict: ValidationVerdict::Met }],
        stray: vec![], since: None, changed: vec![], base: Some(BASE.into()), tip: Some(BASE.into()), pr_base: Some("main".into()), default_branch: Some("main".into()), spec_changes: vec![] };
    must(&mut s, Actor::System, Command::RecordCommitReading { task_id: "t".into(), reading });
    present(&mut s, "r1", vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEADS[0].into() }]);
    assert!(task(&s).final_presentation.is_none(), "the implementer's delivery is refused");
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::CancelTask { task_id: "t".into(), reason: None });
    assert!(task(&s).final_presentation.is_none(), "Alondra does not deliver it for the node either");
    let round = task(&s).validation.current().expect("a validator checks what nobody recorded");
    assert_eq!((round.head.as_str(), round.items.len()), (HEADS[0], 1));
    assert_eq!(task(&s).stages.handover.as_ref().map(|h| h.to), Some(ExecutionRole::Validator));
}

#[test]
fn an_unread_pull_request_is_held_and_delivered_once_alondra_read_it_covered() {
    let mut s = with_pr();
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::CancelTask { task_id: "t".into(), reason: None });
    assert!(matches!(task(&s).open_pr, Some(OpenPrPending::Deliver { .. })), "held until Alondra reads the commit");
    assert!(task(&s).final_presentation.is_none());
    let reading = CommitReading { commit: HEADS[0].into(), worktree: false, specs: true, owed: vec![], problems: vec![], ledger: vec![], stray: vec![], since: None, changed: vec![], base: Some(BASE.into()), tip: Some(BASE.into()), pr_base: Some("main".into()), default_branch: Some("main".into()), spec_changes: vec![] };
    must(&mut s, Actor::System, Command::RecordCommitReading { task_id: "t".into(), reading });
    let status = OpenPrStatus { state: PrState::Open, head: HEADS[0].into(), base: "main".into(), mergeable: PrMergeable::Computing, checks: PrChecks::None };
    must(&mut s, Actor::System, Command::SettleOpenPr { task_id: "t".into(), status });
    assert!(task(&s).final_presentation.is_some(), "nothing was owed on that commit");
    assert_eq!(lifecycle(&s).phase(), alondra_domain::TaskPhase::Delivered);
}

#[test]
fn the_lifecycle_records_no_delivery_the_guard_refuses_whatever_path_asks_for_it() {
    let mut s = implementing();
    let progress = Command::PresentResult {
        attachments: vec![], task_id: "t".into(), interaction_id: "progress".into(), body: "Half of it works".into(),
        targets: vec![DeliveryTarget::Text { text: "Halfway".into() }], is_final: false, nothing_to_review: false, checklist: vec![],
        card: CardSpec::delivery("Read this update", &[], false),
    };
    must(&mut s, node("b"), progress);
    let reading = CommitReading { commit: HEADS[0].into(), worktree: true, specs: true, owed: vec![item(0, true)], problems: vec![], ledger: vec![], stray: vec![], since: None, changed: vec![], base: Some(BASE.into()), tip: Some(BASE.into()), pr_base: Some("main".into()), default_branch: Some("main".into()), spec_changes: vec![] };
    must(&mut s, Actor::System, Command::RecordCommitReading { task_id: "t".into(), reading });
    let refused = s.decide(&mut Lifecycles::new(), &"t".into(), LifecycleRecord::Presented { delivery: "progress".into() }).unwrap_err();
    assert_eq!(refused.code, "validation_required");
    assert_ne!(lifecycle(&s).phase(), alondra_domain::TaskPhase::Delivered);
}

#[test]
fn a_branch_that_turns_specs_off_is_never_delivered_by_the_node_nor_for_it() {
    let mut s = with_pr();
    let reading = CommitReading { commit: HEADS[0].into(), worktree: false, specs: false, owed: vec![], problems: vec![], ledger: vec![], stray: vec![], since: None, changed: vec![], base: Some(BASE.into()), tip: Some(BASE.into()), pr_base: Some("main".into()), default_branch: Some("main".into()),
        spec_changes: vec![crate::SpecFileChange { path: "alondra.toml".into(), approved: false }] };
    must(&mut s, Actor::System, Command::RecordCommitReading { task_id: "t".into(), reading });
    present(&mut s, "r1", vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEADS[0].into() }]);
    assert!(task(&s).final_presentation.is_none(), "the node's delivery is refused");
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::CancelTask { task_id: "t".into(), reason: None });
    assert!(task(&s).final_presentation.is_none(), "Alondra does not deliver it for the node either");
    assert_eq!(task(&s).stages.handover.as_ref().map(|h| h.to), Some(ExecutionRole::Implementer));
}

#[test]
fn a_held_commit_alondra_cannot_read_goes_back_to_the_implementer_instead_of_waiting_forever() {
    let mut s = with_pr();
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::CancelTask { task_id: "t".into(), reason: None });
    assert!(matches!(task(&s).open_pr, Some(OpenPrPending::Deliver { .. })), "held until Alondra reads the commit");
    let reading = CommitReading { commit: HEADS[0].into(), worktree: false, specs: true, owed: vec![],
        problems: vec![format!("Alondra could not read commit {}: the task has no worktree left to read it in", HEADS[0])],
        ledger: vec![], stray: vec![], since: None, changed: vec![], base: None, tip: None, pr_base: Some("main".into()), default_branch: Some("main".into()), spec_changes: vec![] };
    must(&mut s, Actor::System, Command::RecordCommitReading { task_id: "t".into(), reading });
    let status = OpenPrStatus { state: PrState::Open, head: HEADS[0].into(), base: "main".into(), mergeable: PrMergeable::Computing, checks: PrChecks::None };
    must(&mut s, Actor::System, Command::SettleOpenPr { task_id: "t".into(), status });
    assert!(task(&s).final_presentation.is_none(), "nothing unread reaches the user");
    assert!(task(&s).open_pr.is_none(), "the delivery is no longer held");
    assert_eq!(task(&s).stages.handover.as_ref().map(|h| h.to), Some(ExecutionRole::Implementer));
}

#[test]
fn a_delivery_does_not_survive_its_pull_request_changing_base() {
    let mut s = implementing();
    must(&mut s, node("b"), Command::RegisterPr { task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "ana/app".into(),
        number: 1, head: HEADS[0].into(), base: "side".into(), allowed_methods: vec![MergeMethod::Squash] });
    let reading = |pr_base: &str| CommitReading { commit: HEADS[0].into(), worktree: false, specs: false, owed: vec![], problems: vec![], ledger: vec![], stray: vec![], since: None, changed: vec![],
        base: Some(HEADS[1].into()), tip: Some(HEADS[1].into()), pr_base: Some(pr_base.into()), default_branch: Some("main".into()), spec_changes: vec![] };
    must(&mut s, Actor::System, Command::RecordCommitReading { task_id: "t".into(), reading: reading("side") });
    present(&mut s, "r1", vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEADS[0].into() }]);
    assert!(task(&s).final_presentation.is_none(), "a pull request against a branch other than the default one is refused");
    must(&mut s, Actor::System, Command::ObservePrState { pr_id: "pr".into(), head: HEADS[0].into(), base: "main".into(), state: PrState::Open, merged_head: None });
    must(&mut s, Actor::System, Command::RecordCommitReading { task_id: "t".into(), reading: reading("main") });
    let input = lifecycle(&s).pending_inputs()[0].as_str().to_owned();
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 2, facts: vec![RuntimeObservation::TurnStarted { turn: "turn-b".into(), input: input.into() }] });
    present(&mut s, "r2", vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEADS[0].into() }]);
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 2, turn: "turn-b".into(), failure: None, failed: None });
    assert_eq!(task(&s).final_presentation.as_ref().map(|id| id.0.as_str()), Some("r2"));
    must(&mut s, Actor::System, Command::ObservePrState { pr_id: "pr".into(), head: HEADS[0].into(), base: "side".into(), state: PrState::Open, merged_head: None });
    assert!(s.interactions[&InteractionId::from("r2")].withdrawn, "changing the base withdraws the delivery like a new head");
    assert_ne!(lifecycle(&s).phase(), alondra_domain::TaskPhase::Delivered);
}

#[test]
fn a_reading_made_for_another_base_branch_does_not_cover_the_pull_request() {
    let mut s = with_pr();
    let reading = CommitReading { commit: HEADS[0].into(), worktree: false, specs: false, owed: vec![], problems: vec![], ledger: vec![], stray: vec![], since: None, changed: vec![],
        base: Some(BASE.into()), tip: Some(BASE.into()), pr_base: Some("side".into()), default_branch: Some("main".into()), spec_changes: vec![] };
    must(&mut s, Actor::System, Command::RecordCommitReading { task_id: "t".into(), reading: reading.clone() });
    let actor = agent(&s);
    let delivery = |id: &str| Command::PresentResult {
        attachments: vec![], task_id: "t".into(), interaction_id: id.into(), body: "Partial refunds work".into(),
        targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEADS[0].into() }], is_final: true, nothing_to_review: false,
        checklist: vec![ChecklistMark { item: "1".into(), evidence: vec![Evidence { kind: EvidenceKind::Scenario, value: "billing.partial/part".into(), detail: None }] }],
        card: CardSpec::delivery("Review the result", &[], true),
    };
    assert_eq!(exec(&mut s, actor.clone(), delivery("r1")).unwrap_err().code, "validation_unread", "read against side, delivered to main");
    must(&mut s, Actor::System, Command::RecordCommitReading { task_id: "t".into(), reading: CommitReading { pr_base: Some("main".into()), ..reading } });
    exec(&mut s, actor, delivery("r2")).expect("read against main, delivered to main");
}
