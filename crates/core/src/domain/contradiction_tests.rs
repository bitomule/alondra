//! A node is stuck when its lifecycle and what its provider reported disagree. The lifecycle
//! records what it could not apply, and recovers by facts alone: it ends the open turn and wakes
//! the node, then gives it a host that resumes its session, then hands the orchestrator the case.
//! Progress between two contradictions starts the count again. Nothing reaches the user.
use super::lifecycle_tests::{assert_aggregate_rules, last, must, refused_unchanged, sdk_turn, world};
use super::*;
use alondra_domain::{Contradiction, Recovery};
use proptest::prelude::*;

fn lifecycle(s: &DomainState) -> std::sync::Arc<alondra_domain::TaskLifecycle> {
    s.lifecycle_of(&"t".into()).unwrap()
}
fn unapplied(s: &mut DomainState, evidence: UnappliedEvidence) -> CommandOutcome {
    must(s, Actor::System, Command::RecordUnappliedEvidence { task_id: "t".into(), generation: 1, evidence })
}
fn start(s: &mut DomainState, turn: &str) {
    let input = lifecycle(s).pending_inputs().first().map(|input| input.as_str().to_owned()).expect("an input that starts the turn");
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::TurnStarted { turn: turn.into(), input },
    ] });
}
fn end(s: &mut DomainState, turn: &str) {
    must(s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: turn.into(), failure: None, failed: None });
}
fn contradictions(s: &DomainState) -> Vec<(ContradictionRecord, RecoveryRecord)> {
    s.lifecycle_journal[&TaskId::from("t")].iter().filter_map(|record| match record {
        LifecycleRecord::Contradicted { contradiction, recovery } => Some((*contradiction, *recovery)),
        _ => None,
    }).collect()
}
fn user_cards(s: &DomainState) -> Vec<InteractionId> {
    s.interactions.values().filter(|i| i.recipient == Recipient::User).map(|i| i.id.clone()).collect()
}
fn open_case(s: &DomainState) -> Option<InteractionId> {
    s.interactions.values().find(|i| is_stall_case(i) && s.open(i)).map(|i| i.id.clone())
}
fn working(s: &DomainState) -> bool {
    lifecycle(s).activity() == alondra_domain::ActivityView::Working
}

#[test]
fn a_turn_end_the_lifecycle_could_not_apply_ends_its_open_turn_and_wakes_the_node() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    assert!(working(&s));
    let outcome = unapplied(&mut s, UnappliedEvidence::TurnEnd);
    assert_eq!(contradictions(&s), [(ContradictionRecord::TurnEndUnapplied, RecoveryRecord::CloseTurn)]);
    let LifecycleRecord::TurnEnded { wake: Some(wake), text: Some(text), .. } = last(&s, "t") else { panic!("the turn ends and wakes the node: {:?}", last(&s, "t")) };
    assert!(text.contains("could not read how your last turn ended"), "{text}");
    assert_eq!(lifecycle(&s).pending_inputs().iter().map(|input| input.as_str()).collect::<Vec<_>>(), [wake.as_str()]);
    assert_eq!(lifecycle(&s).recoveries(), 1);
    assert!(!outcome.effects.iter().any(|effect| matches!(effect, DomainEffect::NotifyUser { .. })));
    assert!(user_cards(&s).is_empty());
    assert_aggregate_rules(&s);
}

#[test]
fn a_second_contradiction_with_no_progress_gives_the_node_a_host_that_resumes_its_session() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    unapplied(&mut s, UnappliedEvidence::TurnEnd);
    start(&mut s, "turn-2");
    unapplied(&mut s, UnappliedEvidence::TurnEnd);
    assert_eq!(contradictions(&s).last(), Some(&(ContradictionRecord::TurnEndUnapplied, RecoveryRecord::ReplaceHost)));
    assert!(lifecycle(&s).replaces_host(), "the owner replaces the host");
    assert!(working(&s), "the turn stays open until the host ends");
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: true, ended_work: vec![], by_alondra: true });
    assert!(!lifecycle(&s).replaces_host(), "the host's end settles the replacement");
    assert!(!lifecycle(&s).pending_inputs().is_empty(), "the resumed session gets what it goes on with");
    assert!(user_cards(&s).is_empty());
    assert_aggregate_rules(&s);
}

#[test]
fn the_third_contradiction_with_no_progress_is_the_orchestrators_case_never_a_user_card() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    unapplied(&mut s, UnappliedEvidence::TurnEnd);
    start(&mut s, "turn-2");
    unapplied(&mut s, UnappliedEvidence::TurnEnd);
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: true, ended_work: vec![], by_alondra: true });
    start(&mut s, "turn-3");
    unapplied(&mut s, UnappliedEvidence::TurnEnd);
    assert_eq!(contradictions(&s).last(), Some(&(ContradictionRecord::TurnEndUnapplied, RecoveryRecord::OpenCase)));
    assert!(matches!(last(&s, "t"), LifecycleRecord::TurnEnded { wake: None, stalled: true, .. }), "{:?}", last(&s, "t"));
    let case = open_case(&s).expect("the orchestrator gets the case");
    assert_eq!(lifecycle(&s).card_owner(&case), Some(alondra_domain::Owner::Orchestrator));
    assert!(s.interactions[&case].body.contains("could not apply"), "{}", s.interactions[&case].body);
    assert!(user_cards(&s).is_empty());
    assert_aggregate_rules(&s);
}

#[test]
fn a_turn_the_provider_ended_between_two_contradictions_starts_the_count_again() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    unapplied(&mut s, UnappliedEvidence::TurnEnd);
    start(&mut s, "turn-2");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkStarted { turn: "turn-2".into(), work: "tests".into() },
    ] });
    assert_eq!(lifecycle(&s).recoveries(), 0, "work of the node's own is progress");
    must(&mut s, Actor::System, Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
        RuntimeObservation::WorkEnded { work: "tests".into(), continuation: "tests-done".into() },
        RuntimeObservation::WorkContinued { continuation: "tests-done".into() },
    ] });
    end(&mut s, "turn-2");
    start(&mut s, "turn-3");
    unapplied(&mut s, UnappliedEvidence::TurnEnd);
    assert_eq!(contradictions(&s).last(), Some(&(ContradictionRecord::TurnEndUnapplied, RecoveryRecord::CloseTurn)));
}

#[test]
fn evidence_that_contradicts_nothing_is_recorded_and_changes_nothing_else() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    for evidence in [UnappliedEvidence::Transport, UnappliedEvidence::Unreadable] {
        unapplied(&mut s, evidence);
        assert_eq!(last(&s, "t"), LifecycleRecord::EvidenceUnapplied { evidence });
        assert!(working(&s));
    }
    end(&mut s, "turn-1");
    let before = lifecycle(&s).activity();
    unapplied(&mut s, UnappliedEvidence::TurnEnd);
    assert_eq!(last(&s, "t"), LifecycleRecord::EvidenceUnapplied { evidence: UnappliedEvidence::TurnEnd }, "no turn is open to contradict");
    assert_eq!(lifecycle(&s).activity(), before);
    assert!(contradictions(&s).is_empty());
    assert_eq!(lifecycle(&s).recoveries(), 0);
    assert!(user_cards(&s).is_empty());
    assert_aggregate_rules(&s);
}

#[test]
fn only_alondra_records_unapplied_evidence() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let command = Command::RecordUnappliedEvidence { task_id: "t".into(), generation: 1, evidence: UnappliedEvidence::TurnEnd };
    assert_eq!(refused_unchanged(&mut s, Actor::Agent { agent_id: "a".into() }, command.clone()), "forbidden");
    let stale = Command::RecordUnappliedEvidence { task_id: "t".into(), generation: 2, evidence: UnappliedEvidence::TurnEnd };
    assert_ne!(refused_unchanged(&mut s, Actor::System, stale), "", "another execution's evidence is refused");
}

#[test]
fn the_aggregate_takes_only_the_recovery_its_count_gives() {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    let lifecycle = lifecycle(&s);
    assert_eq!(lifecycle.contradiction(alondra_domain::Evidence::TurnEnd), Some(Contradiction::TurnEndUnapplied));
    assert_eq!(lifecycle.recovery(Contradiction::TurnEndUnapplied), Recovery::CloseTurn);
    let wrong = lifecycle.advanced(lifecycle.revision(), alondra_domain::Event::Contradicted { contradiction: Contradiction::TurnEndUnapplied, recovery: Recovery::OpenCase });
    assert_eq!(wrong.err(), Some(alondra_domain::Refusal::CorrelationMismatch));
    let lost = lifecycle.advanced(lifecycle.revision(), alondra_domain::Event::Contradicted { contradiction: Contradiction::HostLost, recovery: Recovery::CloseTurn });
    assert_eq!(lost.err(), Some(alondra_domain::Refusal::InvalidState), "a lost host has no turn end to record");
    let replaced = lifecycle.advanced(lifecycle.revision(), alondra_domain::Event::Contradicted { contradiction: Contradiction::HostLost, recovery: Recovery::ReplaceHost }).unwrap();
    assert!(replaced.replaces_host());
}

#[test]
fn new_records_read_back_as_written() {
    for record in [
        LifecycleRecord::EvidenceUnapplied { evidence: UnappliedEvidence::TurnEnd },
        LifecycleRecord::EvidenceUnapplied { evidence: UnappliedEvidence::Transport },
        LifecycleRecord::Contradicted { contradiction: ContradictionRecord::TurnEndUnapplied, recovery: RecoveryRecord::ReplaceHost },
        LifecycleRecord::Contradicted { contradiction: ContradictionRecord::HostLost, recovery: RecoveryRecord::OpenCase },
    ] {
        let written = serde_json::to_string(&record).unwrap();
        assert_eq!(serde_json::from_str::<LifecycleRecord>(&written).unwrap(), record, "{written}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    /// However many contradictions come, a turn the provider ended between each two of them keeps
    /// every one at its first recovery: no new host, no case.
    #[test]
    fn progress_between_contradictions_never_opens_a_case(rounds in 1usize..8) {
        let mut s = world();
        sdk_turn(&mut s, "t", "a");
        for round in 0..rounds {
            unapplied(&mut s, UnappliedEvidence::TurnEnd);
            prop_assert_eq!(contradictions(&s).last().map(|(_, recovery)| *recovery), Some(RecoveryRecord::CloseTurn));
            let woken = format!("woken-{round}");
            start(&mut s, &woken);
            end(&mut s, &woken);
            let next = format!("next-{round}");
            must(&mut s, Actor::System, Command::TellNode { task_id: "t".into(), text: "Go on with the next step".into(), from_user: true, answers: None });
            start(&mut s, &next);
        }
        prop_assert!(open_case(&s).is_none());
        prop_assert!(!lifecycle(&s).replaces_host());
        prop_assert!(user_cards(&s).is_empty());
    }
}
