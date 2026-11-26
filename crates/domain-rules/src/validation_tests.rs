//! The validation of a task in stages: the implementer says its work is ready, a new execution in
//! the validator's role records what each check found, sends the work back up to three rounds in a
//! row or delivers it, and what comes after the delivery goes back to an implementer.
use super::lifecycle_tests::{assert_aggregate_rules, exec, must, node, user};
use super::spec_tests::{admit, answer, begin, implementing, told};
use super::*;
use crate::spec_card::{SpecCard, CANCEL, CHANGE_SPECS, STRONGER, VALIDATE_AGAIN};
use crate::validation::MAX_VALIDATION_ROUNDS;

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
const HEAD1: &str = "1111111111111111111111111111111111111111";
const HEAD2: &str = "2222222222222222222222222222222222222222";

fn item(subject: &str, kind: ValidationKind) -> ValidationItem {
    ValidationItem { subject: subject.into(), kind, inputs: format!("sha256:{subject}-{}", kind.as_str()) }
}
fn items() -> Vec<ValidationItem> {
    vec![item("billing/unit", ValidationKind::Suite), item("billing.partial/part", ValidationKind::Adequate)]
}
fn ready(items: Vec<ValidationItem>) -> Command {
    Command::ReadyForValidation { task_id: "t".into(), head: HEAD.into(), items, scope: vec!["README.md".into()] }
}
fn record(subject: &str, kind: ValidationKind, verdict: ValidationVerdict) -> Command {
    let judged = kind.judgment() || verdict == ValidationVerdict::Incomplete;
    Command::RecordValidation {
        task_id: "t".into(), subject: subject.into(), check: kind, verdict,
        reasoning: judged.then(|| "The test asserts the refunded amount".into()),
        nearest_wrong: (kind == ValidationKind::Adequate).then(|| "Refunding 10 EUR twice".into()),
        output: (kind == ValidationKind::Suite).then(|| "test result: ok. 12 passed".into()),
        evidence: vec![], inputs: format!("sha256:{subject}-{}", kind.as_str()), listed: None,
    }
}
fn validation(s: &DomainState) -> &TaskValidation {
    &s.tasks[&TaskId::from("t")].validation
}
fn role(s: &DomainState) -> Option<ExecutionRole> {
    s.tasks[&TaskId::from("t")].stages.role()
}
fn handover(s: &DomainState) -> Option<StageHandover> {
    s.tasks[&TaskId::from("t")].stages.handover.clone()
}
/// The change of role the task owes runs: the current execution ends and `agent` takes over.
fn hand_over(s: &mut DomainState, agent: &str, generation: u64) {
    begin(s, &format!("stage-{generation}")).unwrap();
    must(s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: generation - 1, resumes: false, ended_work: vec![], by_alondra: true });
    admit(s, agent, generation);
}
/// `implementing`, with the implementer's work ready and a validator at work on round 1.
fn validating() -> DomainState {
    let mut s = implementing();
    must(&mut s, node("b"), ready(items()));
    hand_over(&mut s, "c", 3);
    s
}
fn present(s: &mut DomainState, agent: &str, id: &str) -> Result<CommandOutcome, DomainError> {
    exec(s, node(agent), present_command(id))
}
fn present_command(id: &str) -> Command {
    Command::PresentResult {
        attachments: vec![], task_id: "t".into(), interaction_id: id.into(), body: "Partial refunds work".into(),
        targets: vec![DeliveryTarget::Text { text: "Done".into() }], is_final: true, nothing_to_review: false,
        checklist: vec![ChecklistMark { item: "1".into(), evidence: vec![Evidence { kind: EvidenceKind::Scenario, value: "billing.partial/part".into(), detail: None }] }],
        card: CardSpec::delivery("Review the result", &[], true),
    }
}
fn send_back(s: &mut DomainState, validator: &str) {
    must(s, node(validator), record("billing/unit", ValidationKind::Suite, ValidationVerdict::Unmet));
    must(s, node(validator), Command::EndValidation { task_id: "t".into(), findings: "The suite fails: a refund of zero is accepted".into() });
}

#[test]
fn the_implementer_hands_its_ready_work_to_a_new_validator_that_owes_every_listed_check() {
    let mut s = implementing();
    assert_eq!(role(&s), Some(ExecutionRole::Implementer));
    assert_eq!(exec(&mut s, node("b"), ready(vec![])).unwrap_err().code, "nothing_to_validate", "nothing listed: it delivers as today");
    let unread = Command::ReadyForValidation { task_id: "t".into(), head: String::new(), items: items(), scope: vec![] };
    assert_eq!(exec(&mut s, node("b"), unread).unwrap_err().code, "worktree_unreadable");
    must(&mut s, node("b"), ready(items()));
    let owed = handover(&s).expect("a validator takes over");
    assert_eq!((owed.to, owed.cause), (ExecutionRole::Validator, HandoverCause::ReadyForValidation { round: 1 }));
    assert!(told(&s).last().unwrap().contains("as its validator"));
    assert_eq!(present(&mut s, "b", "early").unwrap_err().code, "validation_open", "the implementer never delivers its own validated work");
    hand_over(&mut s, "c", 3);
    assert_eq!(role(&s), Some(ExecutionRole::Validator));
    let round = validation(&s).current().unwrap();
    assert_eq!((round.state, round.generation, round.head.as_str()), (RoundState::Validating, Some(3), HEAD));
    assert!(round.items.iter().all(|item| item.state == ValidationState::Running));
    assert_eq!(round.scope, vec!["README.md".to_owned()]);
    assert_aggregate_rules(&s);
}

#[test]
fn only_the_validator_records_a_validation_and_each_judgment_carries_its_reasoning() {
    let mut s = implementing();
    let by_implementer = exec(&mut s, node("b"), record("billing/unit", ValidationKind::Suite, ValidationVerdict::Met)).unwrap_err();
    assert_eq!(by_implementer.code, "role_forbids_tool", "the implementer cannot record its own validation");
    let mut s = validating();
    let mut bare = record("billing.partial/part", ValidationKind::Adequate, ValidationVerdict::Met);
    if let Command::RecordValidation { reasoning, .. } = &mut bare { *reasoning = None; }
    assert_eq!(exec(&mut s, node("c"), bare).unwrap_err().code, "invalid_input");
    let mut unstamped = record("billing/unit", ValidationKind::Suite, ValidationVerdict::Met);
    if let Command::RecordValidation { inputs, .. } = &mut unstamped { inputs.clear(); }
    assert_eq!(exec(&mut s, node("c"), unstamped).unwrap_err().code, "unknown_subject");
    must(&mut s, node("c"), record("billing/unit", ValidationKind::Suite, ValidationVerdict::Met));
    let kept = validation(&s).record("billing/unit", ValidationKind::Suite, "sha256:billing/unit-suite").unwrap();
    assert_eq!((kept.verdict, kept.generation, kept.round), (ValidationVerdict::Met, 3, 1));
    assert_eq!(validation(&s).current().unwrap().items[0].state, ValidationState::Met);
    assert_aggregate_rules(&s);
}

#[test]
fn the_validator_delivers_only_once_every_validation_of_its_round_is_met() {
    let mut s = validating();
    let mut unknown = present_command("r0");
    if let Command::PresentResult { checklist, .. } = &mut unknown { checklist[0].evidence[0].value = "billing.partial/nope".into(); }
    assert_eq!(exec(&mut s, node("c"), unknown).unwrap_err().code, "evidence_unknown_scenario", "a point cites a scenario of the task's specs");
    assert_eq!(present(&mut s, "c", "r1").unwrap_err().code, "validation_pending");
    must(&mut s, node("c"), record("billing/unit", ValidationKind::Suite, ValidationVerdict::Met));
    must(&mut s, node("c"), record("billing.partial/part", ValidationKind::Adequate, ValidationVerdict::Unmet));
    assert_eq!(present(&mut s, "c", "r1").unwrap_err().code, "validation_unmet");
    must(&mut s, node("c"), record("billing.partial/part", ValidationKind::Adequate, ValidationVerdict::Met));
    assert_eq!(exec(&mut s, node("c"), Command::EndValidation { task_id: "t".into(), findings: "All good".into() }).unwrap_err().code, "nothing_failed");
    present(&mut s, "c", "r1").unwrap();
    assert_eq!(validation(&s).current().unwrap().state, RoundState::Delivered);
    assert_aggregate_rules(&s);
}

#[test]
fn a_failed_round_goes_back_to_the_implementer_and_the_third_in_a_row_asks_the_user() {
    let mut s = validating();
    send_back(&mut s, "c");
    let back = handover(&s).unwrap();
    assert_eq!((back.to, back.cause), (ExecutionRole::Implementer, HandoverCause::ValidationFailed { round: 1 }));
    assert_eq!(validation(&s).failed, 1);
    for (round, (implementer, validator)) in [(2u32, (4u64, 5u64)), (3, (6, 7))] {
        hand_over(&mut s, &format!("i{round}"), implementer);
        must(&mut s, node(&format!("i{round}")), ready(items()));
        hand_over(&mut s, &format!("v{round}"), validator);
        send_back(&mut s, &format!("v{round}"));
    }
    assert_eq!(validation(&s).failed, MAX_VALIDATION_ROUNDS);
    assert!(handover(&s).is_none(), "Alondra does not send it back a fourth time by itself");
    let card = validation(&s).exhausted.clone().expect("the user decides");
    let raised = &s.interactions[&card];
    assert_eq!(SpecCard::from_body(&raised.body), Some(SpecCard::ValidationExhausted { task: "t".into(), round: 3, specs: true }));
    let InteractionPayload::Question { options, .. } = &raised.payload else { panic!("a decision") };
    assert_eq!(options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), [STRONGER, CHANGE_SPECS, CANCEL]);
    assert!(raised.card.content.as_ref().unwrap().body.contains("billing/unit"), "the card names what still fails");
    answer(&mut s, card, STRONGER, Some("Use the strong model")).unwrap();
    let stages = &s.tasks[&TaskId::from("t")].stages;
    assert_eq!(stages.selections[&ExecutionRole::Implementer], stages.selections[&ExecutionRole::Author]);
    assert_eq!(handover(&s).unwrap().cause, HandoverCause::ValidationDecided { round: 3 });
    assert_eq!((validation(&s).failed, validation(&s).exhausted.clone()), (0, None));
    assert_aggregate_rules(&s);
}

#[test]
fn after_three_failed_rounds_the_user_may_change_the_specs_instead() {
    let mut s = validating();
    s.tasks.get_mut(&TaskId::from("t")).unwrap().validation.failed = MAX_VALIDATION_ROUNDS - 1;
    send_back(&mut s, "c");
    let card = validation(&s).exhausted.clone().unwrap();
    answer(&mut s, card, CHANGE_SPECS, None).unwrap();
    let asked = s.tasks[&TaskId::from("t")].specs.requested().cloned().expect("an author drafts the change");
    assert!(asked.reason.contains("billing/unit"));
    assert_eq!(handover(&s).unwrap().cause, HandoverCause::SpecChangeRequested { amendment: asked.id });
    assert_aggregate_rules(&s);
}

#[test]
fn what_could_not_run_goes_to_one_new_validator_then_to_the_user() {
    let mut s = validating();
    must(&mut s, node("c"), record("billing/unit", ValidationKind::Suite, ValidationVerdict::Incomplete));
    must(&mut s, node("c"), Command::EndValidation { task_id: "t".into(), findings: "bazelisk is not installed".into() });
    let retry = handover(&s).unwrap();
    assert_eq!((retry.to, retry.cause.clone()), (ExecutionRole::Validator, HandoverCause::ValidatorRetry { round: 1 }));
    assert!(retry.owed_by(Some(ExecutionRole::Validator)), "a validator owes a change to a new validator");
    assert_eq!(validation(&s).failed, 0, "incomplete is never a failure");
    hand_over(&mut s, "d", 4);
    let round = validation(&s).current().unwrap();
    assert_eq!((round.state, round.generation), (RoundState::Validating, Some(4)));
    assert!(round.items.iter().all(|item| item.state == ValidationState::Running), "what could not run is owed again");
    must(&mut s, node("d"), Command::EndValidation { task_id: "t".into(), findings: "bazelisk is still missing".into() });
    let card = validation(&s).exhausted.clone().expect("the stuck task asks the user");
    assert_eq!(SpecCard::from_body(&s.interactions[&card].body), Some(SpecCard::ValidationStuck { task: "t".into(), round: 1 }));
    answer(&mut s, card, VALIDATE_AGAIN, None).unwrap();
    assert_eq!(handover(&s).unwrap().cause, HandoverCause::ValidatorRetry { round: 1 });
    assert_aggregate_rules(&s);
}

#[test]
fn a_lost_validator_keeps_what_it_recorded_and_owes_the_rest_once_taken_over() {
    let mut s = validating();
    must(&mut s, node("c"), record("billing/unit", ValidationKind::Suite, ValidationVerdict::Met));
    must(&mut s, Actor::System, Command::RecordHostLost { task_id: "t".into(), reason: "The host stopped".into(), quiet: false, signed_out: false });
    let round = validation(&s).current().unwrap();
    assert_eq!(round.state, RoundState::Interrupted);
    assert_eq!(round.items.iter().map(|item| item.state).collect::<Vec<_>>(), [ValidationState::Met, ValidationState::Incomplete]);
    assert!(validation(&s).record("billing/unit", ValidationKind::Suite, "sha256:billing/unit-suite").is_some());
    assert_eq!(exec(&mut s, node("c"), record("billing.partial/part", ValidationKind::Adequate, ValidationVerdict::Met)).unwrap_err().code, "no_validation");
    let card = s.interactions.keys().find(|id| id.0.starts_with(HOST_LOST_PREFIX)).expect("the lost host's own card").clone();
    answer(&mut s, card, RETAKE, None).unwrap();
    let round = validation(&s).current().unwrap();
    assert_eq!((round.state, round.generation), (RoundState::Validating, Some(3)), "the validator taken over again carries the round on");
    assert_eq!(round.items[1].state, ValidationState::Running);
    assert_aggregate_rules(&s);
}

#[test]
fn changes_the_user_asks_of_a_validated_delivery_go_to_an_implementer() {
    let mut s = validating();
    must(&mut s, node("c"), record("billing/unit", ValidationKind::Suite, ValidationVerdict::Met));
    must(&mut s, node("c"), record("billing.partial/part", ValidationKind::Adequate, ValidationVerdict::Met));
    present(&mut s, "c", "r1").unwrap();
    must(&mut s, user(), Command::SubmitFeedback { merge_failure: None, attachments: vec![], presentation_id: "r1".into(),
        targets: vec![DeliveryTarget::Text { text: "Done".into() }], body: "Name the refund in the receipt".into(), instruction: Instruction::Changes });
    let back = handover(&s).expect("an implementer does what the user asked");
    assert_eq!((back.to, back.cause), (ExecutionRole::Implementer, HandoverCause::DeliveryReturned));
    assert_aggregate_rules(&s);
}

#[test]
fn a_validation_survives_a_round_trip_through_its_stored_form() {
    let mut s = validating();
    must(&mut s, node("c"), record("billing/unit", ValidationKind::Suite, ValidationVerdict::Met));
    let kept = validation(&s).clone();
    let stored: TaskValidation = serde_json::from_str(&serde_json::to_string(&kept).unwrap()).unwrap();
    assert_eq!(stored, kept);
    let old: Task = serde_json::from_value({
        let mut task = serde_json::to_value(&s.tasks[&TaskId::from("t")]).unwrap();
        task.as_object_mut().unwrap().remove("validation");
        task
    }).unwrap();
    assert!(old.validation.is_default(), "a task stored before validation existed reads as never validated");
}

/// A validator delivered round 1 in pull request `pr`, and the pull request then received `head2`.
fn delivered_then_moved() -> DomainState {
    let mut s = implementing();
    must(&mut s, node("b"), Command::RegisterPr { task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
        number: 1, head: HEAD1.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash] });
    must(&mut s, node("b"), ready(items()));
    hand_over(&mut s, "c", 3);
    let input = crate::replay(&"t".into(), &s.lifecycle_journal[&TaskId::from("t")]).unwrap().pending_inputs()[0].as_str().to_owned();
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 3, facts: vec![RuntimeObservation::TurnStarted { turn: "turn-v".into(), input }] });
    must(&mut s, node("c"), record("billing/unit", ValidationKind::Suite, ValidationVerdict::Met));
    must(&mut s, node("c"), record("billing.partial/part", ValidationKind::Adequate, ValidationVerdict::Met));
    let ledger = items().into_iter().map(|item| crate::LedgerEntry { subject: item.subject, kind: item.kind, inputs: item.inputs, verdict: ValidationVerdict::Met }).collect();
    must(&mut s, Actor::System, Command::RecordCommitReading { task_id: "t".into(), reading: crate::CommitReading {
        commit: HEAD1.into(), worktree: false, specs: true, owed: vec![], problems: vec![], ledger, stray: vec![], since: Some(HEAD.into()), changed: vec![] } });
    let mut delivery = present_command("r1");
    if let Command::PresentResult { targets, .. } = &mut delivery { *targets = vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEAD1.into() }]; }
    must(&mut s, node("c"), delivery);
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 3, turn: "turn-v".into(), failure: None, failed: None });
    assert_eq!(validation(&s).current().unwrap().state, RoundState::Delivered);
    must(&mut s, Actor::System, Command::ObservePrState { pr_id: "pr".into(), head: HEAD2.into(), base: "main".into(), state: PrState::Open, merged_head: None });
    s
}

#[test]
fn a_commit_after_the_validated_delivery_withdraws_it_and_the_same_validator_validates_that_commit() {
    let s = delivered_then_moved();
    let round = validation(&s).current().unwrap();
    assert_eq!((round.state, round.head.as_str()), (RoundState::Validating, HEAD2), "the round validates the commit the pull request received");
    assert_eq!(round.implemented, HEAD, "the implementer's commit stays the one the delivery is compared with");
    assert!(s.interactions[&InteractionId::from("r1")].withdrawn, "the validated delivery is withdrawn");
    assert_eq!(role(&s), Some(ExecutionRole::Validator));
    assert!(handover(&s).is_none(), "the same validator carries on");
    assert_aggregate_rules(&s);
}

#[test]
fn after_a_commit_the_round_owes_what_the_engine_lists_there_and_a_failure_goes_to_the_implementer() {
    let mut s = delivered_then_moved();
    let changed = ValidationItem { inputs: "sha256:part-after-head2".into(), ..item("billing.partial/part", ValidationKind::Adequate) };
    let mut unmet = record("billing.partial/part", ValidationKind::Adequate, ValidationVerdict::Unmet);
    if let Command::RecordValidation { inputs, listed, .. } = &mut unmet {
        *inputs = changed.inputs.clone();
        *listed = Some(vec![changed.clone()]);
    }
    must(&mut s, node("c"), unmet);
    let round = validation(&s).current().unwrap();
    let items: Vec<(&str, &str, ValidationState)> = round.items.iter().map(|item| (item.subject.as_str(), item.inputs.as_str(), item.state)).collect();
    assert_eq!(items, [("billing.partial/part", "sha256:part-after-head2", ValidationState::Unmet)], "only what the engine lists on the new commit is owed, at its new fingerprint");
    must(&mut s, node("c"), Command::EndValidation { task_id: "t".into(), findings: "A refund of zero is accepted again".into() });
    assert_eq!(handover(&s).map(|h| h.to), Some(ExecutionRole::Implementer));
    assert_eq!(validation(&s).failed, 1);
    assert_aggregate_rules(&s);
}

#[test]
fn code_a_commit_brought_after_the_delivery_goes_to_the_implementer_even_with_every_check_met() {
    let mut s = delivered_then_moved();
    let mut nothing = record("billing/unit", ValidationKind::Suite, ValidationVerdict::Met);
    if let Command::RecordValidation { listed, .. } = &mut nothing { *listed = Some(vec![]); }
    must(&mut s, node("c"), nothing);
    must(&mut s, node("c"), Command::EndValidation { task_id: "t".into(), findings: "head2 changed crates/billing/src/lib.rs".into() });
    assert_eq!(handover(&s).map(|h| h.to), Some(ExecutionRole::Implementer));
    assert_eq!(validation(&s).failed, 0, "a commit someone else made is not a failed round");
    assert_aggregate_rules(&s);
}
