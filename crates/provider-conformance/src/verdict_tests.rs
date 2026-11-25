use super::*;
use crate::catalogue::scenario;

const EXPECTED: &[&str] = &["Available", "TurnStarted(T1, input-1)", "TurnEnded(T1, Completed)"];
const DIVERGENT: &[&str] = &["Available", "TurnStarted(T1, input-1)", "TurnEnded(T1, Failed(other))"];
const SCENARIO: Scenario = scenario("demo", &[], (None, None, None), EXPECTED);
const KNOWN: KnownDivergence = KnownDivergence { scenario: "demo", provider: Provider::Codex, cause: "the provider names its own code", actual: DIVERGENT };

fn facts(facts: &[&str]) -> Result<Vec<String>, String> {
    Ok(facts.iter().map(|fact| fact.to_string()).collect())
}

#[test]
fn the_expected_facts_pass_and_any_other_sequence_fails() {
    assert_eq!(judge(&SCENARIO, facts(EXPECTED), None), Verdict::Pass);
    assert!(matches!(judge(&SCENARIO, facts(DIVERGENT), None), Verdict::Failed(_)));
    assert!(matches!(judge(&SCENARIO, facts(&EXPECTED[..2]), None), Verdict::Failed(_)), "a missing fact fails");
    assert!(matches!(judge(&SCENARIO, Err("unreadable".into()), None), Verdict::Failed(_)));
}

#[test]
fn a_known_divergence_holds_only_while_it_happens_exactly() {
    assert_eq!(judge(&SCENARIO, facts(DIVERGENT), Some(&KNOWN)), Verdict::KnownDivergence(KNOWN.cause));
    let Verdict::Failed(reason) = judge(&SCENARIO, facts(EXPECTED), Some(&KNOWN)) else { panic!("a divergence that stopped must fail") };
    assert!(reason.contains("remove its entry"), "{reason}");
    assert!(matches!(judge(&SCENARIO, facts(&DIVERGENT[..2]), Some(&KNOWN)), Verdict::Failed(_)), "a divergence that changed fails");
}

#[test]
fn native_identities_read_in_order_of_first_appearance_and_inputs_keep_theirs() {
    use alondra_agent_application::ObservedFact;
    use alondra_domain::{RuntimeFact, Trigger};
    use alondra_foundation::{TurnId, WorkId};
    let mut canon = canonical::Canon::default();
    let turn = |id: &str| TurnId::new(id).unwrap();
    let started = |id: &str, input: &str| ObservedFact::Runtime(RuntimeFact::TurnStarted { turn: turn(id), trigger: Trigger::Input(InputId::new(input).unwrap()) });
    assert_eq!(canon.fact(&started("codex:1:process:turn:b", "input-7")), "TurnStarted(T1, input-7)");
    assert_eq!(canon.fact(&started("claude-run:a", "input-8")), "TurnStarted(T2, input-8)");
    let work = ObservedFact::Runtime(RuntimeFact::WorkStarted { turn: turn("codex:1:process:turn:b"), work: WorkId::new("claude-task:x").unwrap() });
    assert_eq!(canon.fact(&work), "WorkStarted(T1, W1)");
}

#[test]
fn availability_said_before_any_turn_reads_first_and_said_later_stays() {
    let mut early: Vec<String> = ["InputHeld(input-1)", "Available", "TurnStarted(T1, input-1)"].map(String::from).to_vec();
    canonical::settle_availability(&mut early);
    assert_eq!(early, ["Available", "InputHeld(input-1)", "TurnStarted(T1, input-1)"]);
    let mut late: Vec<String> = ["TurnStarted(T1, input-1)", "Available"].map(String::from).to_vec();
    canonical::settle_availability(&mut late);
    assert_eq!(late, ["TurnStarted(T1, input-1)", "Available"]);
}
