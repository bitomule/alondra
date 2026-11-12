use super::*;

const S: Millis = 1000;
const HOUR: Millis = 60 * 60 * S;

struct Room(bool);
impl World for Room {
    fn waits_for_slot(&self, _: &TaskId) -> bool {
        self.0
    }
}

fn owes(party: Party, owed: Owed, since: Revision) -> Vec<(TaskId, Obligation)> {
    vec![("t-1".into(), Obligation { party, owed, since })]
}

fn hand_input(since: Revision) -> Vec<(TaskId, Obligation)> {
    owes(Party::Alondra, Owed::HandInput, since)
}

#[test]
fn a_step_owed_by_alondra_past_its_deadline_is_reported_once() {
    let mut watch = Watch::default();
    let at = |watch: &mut Watch, seconds| watch.observe(hand_input(1), &Room(false), seconds * S);
    assert_eq!(at(&mut watch, 0), vec![]);
    assert_eq!(at(&mut watch, 119), vec![]);
    assert_eq!(at(&mut watch, 121), vec![Overdue { task: "t-1".into(), party: Party::Alondra, owed: Owed::HandInput, age: 121 * S }]);
    assert_eq!(at(&mut watch, 300), vec![]);
}

#[test]
fn what_the_user_capacity_or_another_task_owes_is_never_overdue() {
    let mut watch = Watch::default();
    for (party, owed) in [(Party::User, Owed::User), (Party::Capacity(Capacity::Gate), Owed::Room(Capacity::Gate)), (Party::Tasks, Owed::Tasks)] {
        assert_eq!(watch.observe(owes(party, owed, 1), &Room(false), 0), vec![]);
        assert_eq!(watch.observe(owes(party, owed, 1), &Room(false), 24 * HOUR), vec![]);
    }
}

#[test]
fn a_running_turn_has_no_deadline() {
    let mut watch = Watch::default();
    let turn = || owes(Party::Provider, Owed::Turn, 1);
    assert_eq!(watch.observe(turn(), &Room(false), 0), vec![]);
    assert_eq!(watch.observe(turn(), &Room(false), 24 * HOUR), vec![]);
}

#[test]
fn an_input_waiting_for_a_slot_is_owed_by_capacity() {
    let mut watch = Watch::default();
    assert_eq!(watch.observe(hand_input(1), &Room(true), 0), vec![]);
    assert_eq!(watch.observe(hand_input(1), &Room(true), HOUR), vec![]);
}

#[test]
fn a_new_obligation_restarts_the_clock() {
    let mut watch = Watch::default();
    assert_eq!(watch.observe(hand_input(1), &Room(false), 0), vec![]);
    assert_eq!(watch.observe(owes(Party::Provider, Owed::Turn, 2), &Room(false), 100 * S), vec![]);
    assert_eq!(watch.observe(hand_input(3), &Room(false), 130 * S), vec![]);
    assert_eq!(watch.observe(hand_input(3), &Room(false), 249 * S), vec![]);
}

#[test]
fn the_hourly_summary_counts_every_finished_step() {
    let mut watch = Watch::default();
    assert_eq!(watch.summary(0), Vec::<String>::new());
    let mut now = 0;
    for (since, lasted) in [(1, 1), (3, 5), (5, 90)] {
        watch.observe(hand_input(since), &Room(false), now);
        now += lasted * S;
        watch.observe(owes(Party::Provider, Owed::Turn, since + 1), &Room(false), now);
    }
    let lines = watch.summary(HOUR);
    let line = lines.iter().find(|line| line.starts_with("obligations HandInput")).expect("a line for HandInput");
    assert!(line.contains("n=3") && line.contains("max=90s") && line.contains("overdue=0"), "{line}");
}

fn every_deadline() -> Watch {
    Watch::enforcing(|owed| matches!(owed, Owed::HandInput | Owed::Wake))
}

fn acted(watch: &mut Watch, obligations: Vec<(TaskId, Obligation)>, now: Millis) -> Vec<(TaskId, Action)> {
    let late = watch.observe(obligations, &Room(false), now);
    watch.enforce(&late, now)
}

#[test]
fn an_input_past_its_deadline_is_offered_again_then_requeued_each_after_a_deadline_of_its_own() {
    let mut watch = every_deadline();
    assert_eq!(acted(&mut watch, hand_input(1), 0), vec![]);
    assert_eq!(acted(&mut watch, hand_input(1), 121 * S), vec![("t-1".into(), Action::OfferAgain)]);
    assert_eq!(acted(&mut watch, hand_input(1), 200 * S), vec![], "the offer again has a deadline of its own");
    assert_eq!(acted(&mut watch, hand_input(1), 242 * S), vec![("t-1".into(), Action::Requeue)]);
}

#[test]
fn a_new_step_starts_the_count_of_attempts_again() {
    let mut watch = every_deadline();
    acted(&mut watch, hand_input(1), 0);
    assert_eq!(acted(&mut watch, hand_input(1), 121 * S), vec![("t-1".into(), Action::OfferAgain)]);
    acted(&mut watch, owes(Party::Provider, Owed::Turn, 2), 130 * S);
    acted(&mut watch, hand_input(3), 140 * S);
    assert_eq!(acted(&mut watch, hand_input(3), 261 * S), vec![("t-1".into(), Action::OfferAgain)], "a turn started between the two");
}

#[test]
fn only_an_enforced_deadline_is_acted_on() {
    let mut watch = Watch::default();
    acted(&mut watch, hand_input(1), 0);
    assert_eq!(acted(&mut watch, hand_input(1), 121 * S), vec![], "handing an input waits for its measure");
    let mut watch = Watch::default();
    acted(&mut watch, owes(Party::Alondra, Owed::Wake, 1), 0);
    assert_eq!(acted(&mut watch, owes(Party::Alondra, Owed::Wake, 1), 121 * S), vec![("t-1".into(), Action::Wake)]);
    assert_eq!(acted(&mut watch, owes(Party::Alondra, Owed::Answer, 2), 0), vec![]);
    assert_eq!(acted(&mut watch, owes(Party::Alondra, Owed::Answer, 2), 24 * HOUR), vec![], "what the orchestrator owes is not acted on here");
}
