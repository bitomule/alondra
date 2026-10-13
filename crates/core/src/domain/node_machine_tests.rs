//! A node is never stopped and working at once. Every forbidden transition of the node machine is
//! refused, and generated runs of launches, turns (some reported late), cards, deliveries, pauses,
//! resumes, sessions that vanish or close, daemon restarts and listings of the real sessions keep
//! one state per node that the gate, Fleet, the Tasks list and the header all count alike.
use super::*;
use crate::views::{Client, Item, Lang, LiveSnapshot, RenderContext, Transport, View};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestError, TestRng, TestRunner};
use std::collections::{BTreeMap, BTreeSet};

const OPEN: TaskFacts = TaskFacts { open: true, closing: false, held: false, card: false, delivered: false };
const SINCE: Millis = 1_000_000;
const AT: Millis = SINCE + 1;

fn pause(reason: PauseReason) -> Pause {
    Pause { reason, since: SINCE, cut: None, turn_since: None, cleanup: PauseCleanup::Pending, released: false }
}

fn api_error() -> PauseReason {
    PauseReason::ApiError(ApiRetry { error: "server_error".into(), detail: None, attempt: 1, at: SINCE + 30_000, outage: false })
}

fn declared() -> PauseReason {
    PauseReason::Declared { reason: "The build queue is full".into() }
}

fn phases() -> Vec<NodePhase> {
    vec![
        NodePhase::Starting { reviving: false },
        NodePhase::Starting { reviving: true },
        NodePhase::Lost,
        NodePhase::Working,
        NodePhase::WaitingUser { in_turn: true },
        NodePhase::WaitingUser { in_turn: false },
        NodePhase::Paused(pause(PauseReason::Memory)),
        NodePhase::Paused(pause(PauseReason::ByUser)),
        NodePhase::Paused(pause(declared())),
        NodePhase::Paused(pause(api_error())),
        NodePhase::Delivered { in_turn: true },
        NodePhase::Delivered { in_turn: false },
        NodePhase::Stopped,
        NodePhase::Closing,
        NodePhase::Closed,
    ]
}

fn events() -> Vec<NodeEvent> {
    let mut all = vec![
        NodeEvent::TurnStarted, NodeEvent::TurnEnded, NodeEvent::TurnFailed, NodeEvent::SessionLost, NodeEvent::SessionClosed,
        NodeEvent::StartFailed, NodeEvent::RevivalFailed, NodeEvent::RevivalAdmitted, NodeEvent::Paused { pause: pause(PauseReason::Usage) },
        NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done }, NodeEvent::PauseReleased, NodeEvent::Resumed,
    ];
    all.extend([Listed::Working, Listed::Idle, Listed::Blocked, Listed::Done, Listed::Absent].map(|listed| NodeEvent::Listed { listed }));
    all
}

/// The machine's table of refusals, written apart from `next`: the events each phase refuses.
fn forbidden(phase: &NodePhase, event: &NodeEvent) -> Option<&'static str> {
    use NodeEvent as E;
    use NodePhase as P;
    let paused = matches!(phase, P::Paused(_));
    match (phase, event) {
        (P::Closed, _) => Some("node_closed"),
        (P::Paused(Pause { reason: PauseReason::ByUser, .. }), E::TurnStarted) => Some("paused_by_user"),
        (P::Paused(Pause { reason: PauseReason::Memory, .. }), E::TurnStarted) => Some("paused_for_memory"),
        (P::Paused(Pause { reason: PauseReason::Declared { .. }, .. }), E::TurnStarted) => Some("waits_for_machine"),
        (P::Starting { .. }, E::StartFailed | E::RevivalFailed) => None,
        (_, E::StartFailed | E::RevivalFailed) => Some("not_starting"),
        (P::Lost, E::RevivalAdmitted) => None,
        (_, E::RevivalAdmitted) => Some("not_lost"),
        (P::Paused(_), E::Paused { .. }) => Some("already_paused"),
        (P::Starting { .. } | P::Working | P::Delivered { in_turn: true } | P::Stopped, E::Paused { .. }) => None,
        (_, E::Paused { .. }) => Some("not_working"),
        (_, E::PauseCleanup { .. } | E::PauseReleased | E::Resumed) if !paused => Some("not_paused"),
        _ => None,
    }
}

fn refused(phase: NodePhase, event: NodeEvent, at: Millis) -> String {
    match next(&phase, SINCE, &event, &OPEN, at) {
        Ok(moved) => panic!("{phase:?} accepted {event:?} and moved to {moved:?}"),
        Err(error) => error.code,
    }
}

macro_rules! forbid {
    ($($name:ident: $phase:expr, $event:expr => $code:literal;)*) => {$(
        #[test]
        fn $name() {
            assert_eq!(refused($phase, $event, AT), $code);
        }
    )*};
}

forbid! {
    closed_refuses_a_turn_start: NodePhase::Closed, NodeEvent::TurnStarted => "node_closed";
    closed_refuses_a_turn_end: NodePhase::Closed, NodeEvent::TurnEnded => "node_closed";
    closed_refuses_a_failed_turn: NodePhase::Closed, NodeEvent::TurnFailed => "node_closed";
    closed_refuses_a_lost_session: NodePhase::Closed, NodeEvent::SessionLost => "node_closed";
    closed_refuses_closing_again: NodePhase::Closed, NodeEvent::SessionClosed => "node_closed";
    closed_refuses_a_failed_start: NodePhase::Closed, NodeEvent::StartFailed => "node_closed";
    closed_refuses_a_failed_revival: NodePhase::Closed, NodeEvent::RevivalFailed => "node_closed";
    closed_refuses_a_revival: NodePhase::Closed, NodeEvent::RevivalAdmitted => "node_closed";
    closed_refuses_a_pause: NodePhase::Closed, NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "node_closed";
    closed_refuses_a_pause_cleanup: NodePhase::Closed, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "node_closed";
    closed_refuses_a_pause_release: NodePhase::Closed, NodeEvent::PauseReleased => "node_closed";
    closed_refuses_a_resume: NodePhase::Closed, NodeEvent::Resumed => "node_closed";
    closed_refuses_a_listed_working_session: NodePhase::Closed, NodeEvent::Listed { listed: Listed::Working } => "node_closed";
    closed_refuses_a_listed_idle_session: NodePhase::Closed, NodeEvent::Listed { listed: Listed::Idle } => "node_closed";
    closed_refuses_a_listed_blocked_session: NodePhase::Closed, NodeEvent::Listed { listed: Listed::Blocked } => "node_closed";
    closed_refuses_an_absent_session: NodePhase::Closed, NodeEvent::Listed { listed: Listed::Absent } => "node_closed";

    paused_by_the_user_refuses_a_turn: NodePhase::Paused(pause(PauseReason::ByUser)), NodeEvent::TurnStarted => "paused_by_user";
    paused_for_memory_refuses_a_turn_it_starts_by_itself: NodePhase::Paused(pause(PauseReason::Memory)), NodeEvent::TurnStarted => "paused_for_memory";
    waiting_for_the_machine_refuses_a_turn_it_starts_by_itself: NodePhase::Paused(pause(declared())), NodeEvent::TurnStarted => "waits_for_machine";

    working_refuses_a_failed_start: NodePhase::Working, NodeEvent::StartFailed => "not_starting";
    working_refuses_a_failed_revival: NodePhase::Working, NodeEvent::RevivalFailed => "not_starting";
    waiting_in_turn_refuses_a_failed_start: NodePhase::WaitingUser { in_turn: true }, NodeEvent::StartFailed => "not_starting";
    waiting_in_turn_refuses_a_failed_revival: NodePhase::WaitingUser { in_turn: true }, NodeEvent::RevivalFailed => "not_starting";
    waiting_refuses_a_failed_start: NodePhase::WaitingUser { in_turn: false }, NodeEvent::StartFailed => "not_starting";
    waiting_refuses_a_failed_revival: NodePhase::WaitingUser { in_turn: false }, NodeEvent::RevivalFailed => "not_starting";
    paused_refuses_a_failed_start: NodePhase::Paused(pause(PauseReason::Memory)), NodeEvent::StartFailed => "not_starting";
    paused_refuses_a_failed_revival: NodePhase::Paused(pause(PauseReason::Memory)), NodeEvent::RevivalFailed => "not_starting";
    paused_by_the_user_refuses_a_failed_start: NodePhase::Paused(pause(PauseReason::ByUser)), NodeEvent::StartFailed => "not_starting";
    paused_by_the_user_refuses_a_failed_revival: NodePhase::Paused(pause(PauseReason::ByUser)), NodeEvent::RevivalFailed => "not_starting";
    delivered_refuses_a_failed_start: NodePhase::Delivered { in_turn: false }, NodeEvent::StartFailed => "not_starting";
    delivered_refuses_a_failed_revival: NodePhase::Delivered { in_turn: false }, NodeEvent::RevivalFailed => "not_starting";
    stopped_refuses_a_failed_start: NodePhase::Stopped, NodeEvent::StartFailed => "not_starting";
    stopped_refuses_a_failed_revival: NodePhase::Stopped, NodeEvent::RevivalFailed => "not_starting";
    closing_refuses_a_failed_start: NodePhase::Closing, NodeEvent::StartFailed => "not_starting";
    closing_refuses_a_failed_revival: NodePhase::Closing, NodeEvent::RevivalFailed => "not_starting";
    lost_refuses_a_failed_start: NodePhase::Lost, NodeEvent::StartFailed => "not_starting";
    lost_refuses_a_failed_revival: NodePhase::Lost, NodeEvent::RevivalFailed => "not_starting";

    starting_refuses_a_revival: NodePhase::Starting { reviving: false }, NodeEvent::RevivalAdmitted => "not_lost";
    reviving_refuses_a_second_revival: NodePhase::Starting { reviving: true }, NodeEvent::RevivalAdmitted => "not_lost";
    working_refuses_a_revival: NodePhase::Working, NodeEvent::RevivalAdmitted => "not_lost";
    waiting_refuses_a_revival: NodePhase::WaitingUser { in_turn: false }, NodeEvent::RevivalAdmitted => "not_lost";
    paused_refuses_a_revival: NodePhase::Paused(pause(PauseReason::Memory)), NodeEvent::RevivalAdmitted => "not_lost";
    delivered_refuses_a_revival: NodePhase::Delivered { in_turn: false }, NodeEvent::RevivalAdmitted => "not_lost";
    stopped_refuses_a_revival: NodePhase::Stopped, NodeEvent::RevivalAdmitted => "not_lost";
    closing_refuses_a_revival: NodePhase::Closing, NodeEvent::RevivalAdmitted => "not_lost";

    paused_refuses_a_second_pause: NodePhase::Paused(pause(PauseReason::Memory)), NodeEvent::Paused { pause: pause(PauseReason::Usage) } => "already_paused";
    paused_for_an_api_error_refuses_a_second_pause: NodePhase::Paused(pause(api_error())), NodeEvent::Paused { pause: pause(api_error()) } => "already_paused";
    waiting_in_turn_refuses_an_api_error_pause: NodePhase::WaitingUser { in_turn: true }, NodeEvent::Paused { pause: pause(api_error()) } => "not_working";
    paused_by_the_user_refuses_a_second_pause: NodePhase::Paused(pause(PauseReason::ByUser)), NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "already_paused";
    waiting_in_turn_refuses_a_pause: NodePhase::WaitingUser { in_turn: true }, NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "not_working";
    waiting_refuses_a_pause: NodePhase::WaitingUser { in_turn: false }, NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "not_working";
    delivered_refuses_a_pause: NodePhase::Delivered { in_turn: false }, NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "not_working";
    closing_refuses_a_pause: NodePhase::Closing, NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "not_working";
    lost_refuses_a_pause: NodePhase::Lost, NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "not_working";

    starting_refuses_a_pause_cleanup: NodePhase::Starting { reviving: false }, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    starting_refuses_a_pause_release: NodePhase::Starting { reviving: false }, NodeEvent::PauseReleased => "not_paused";
    starting_refuses_a_resume: NodePhase::Starting { reviving: false }, NodeEvent::Resumed => "not_paused";
    reviving_refuses_a_pause_cleanup: NodePhase::Starting { reviving: true }, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    reviving_refuses_a_pause_release: NodePhase::Starting { reviving: true }, NodeEvent::PauseReleased => "not_paused";
    reviving_refuses_a_resume: NodePhase::Starting { reviving: true }, NodeEvent::Resumed => "not_paused";
    lost_refuses_a_pause_cleanup: NodePhase::Lost, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    lost_refuses_a_pause_release: NodePhase::Lost, NodeEvent::PauseReleased => "not_paused";
    lost_refuses_a_resume: NodePhase::Lost, NodeEvent::Resumed => "not_paused";
    working_refuses_a_pause_cleanup: NodePhase::Working, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    working_refuses_a_pause_release: NodePhase::Working, NodeEvent::PauseReleased => "not_paused";
    working_refuses_a_resume: NodePhase::Working, NodeEvent::Resumed => "not_paused";
    waiting_in_turn_refuses_a_pause_cleanup: NodePhase::WaitingUser { in_turn: true }, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    waiting_in_turn_refuses_a_pause_release: NodePhase::WaitingUser { in_turn: true }, NodeEvent::PauseReleased => "not_paused";
    waiting_in_turn_refuses_a_resume: NodePhase::WaitingUser { in_turn: true }, NodeEvent::Resumed => "not_paused";
    waiting_refuses_a_pause_cleanup: NodePhase::WaitingUser { in_turn: false }, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    waiting_refuses_a_pause_release: NodePhase::WaitingUser { in_turn: false }, NodeEvent::PauseReleased => "not_paused";
    waiting_refuses_a_resume: NodePhase::WaitingUser { in_turn: false }, NodeEvent::Resumed => "not_paused";
    delivered_refuses_a_pause_cleanup: NodePhase::Delivered { in_turn: false }, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    delivered_refuses_a_pause_release: NodePhase::Delivered { in_turn: false }, NodeEvent::PauseReleased => "not_paused";
    delivered_refuses_a_resume: NodePhase::Delivered { in_turn: false }, NodeEvent::Resumed => "not_paused";
    stopped_refuses_a_pause_cleanup: NodePhase::Stopped, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    stopped_refuses_a_pause_release: NodePhase::Stopped, NodeEvent::PauseReleased => "not_paused";
    stopped_refuses_a_resume: NodePhase::Stopped, NodeEvent::Resumed => "not_paused";
    closing_refuses_a_pause_cleanup: NodePhase::Closing, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    closing_refuses_a_pause_release: NodePhase::Closing, NodeEvent::PauseReleased => "not_paused";
    closing_refuses_a_resume: NodePhase::Closing, NodeEvent::Resumed => "not_paused";
}

macro_rules! stale {
    ($($name:ident: $phase:expr, $event:expr;)*) => {$(
        #[test]
        fn $name() {
            assert_eq!(refused($phase, $event, SINCE - 1), "stale");
        }
    )*};
}

stale! {
    a_late_turn_start_never_moves_a_paused_node: NodePhase::Paused(pause(PauseReason::Memory)), NodeEvent::TurnStarted;
    a_late_turn_start_never_moves_a_waiting_node: NodePhase::WaitingUser { in_turn: false }, NodeEvent::TurnStarted;
    a_late_turn_start_never_moves_a_delivered_node: NodePhase::Delivered { in_turn: false }, NodeEvent::TurnStarted;
    a_late_turn_start_never_moves_a_stopped_node: NodePhase::Stopped, NodeEvent::TurnStarted;
    a_late_turn_end_never_moves_a_working_node: NodePhase::Working, NodeEvent::TurnEnded;
    a_late_failed_turn_never_moves_a_working_node: NodePhase::Working, NodeEvent::TurnFailed;
    a_late_lost_session_never_moves_a_working_node: NodePhase::Working, NodeEvent::SessionLost;
    a_late_lost_session_never_moves_a_revived_node: NodePhase::Starting { reviving: false }, NodeEvent::SessionLost;
}

/// The refusal table covers every pair: what it does not forbid, the machine accepts.
#[test]
fn every_transition_the_table_allows_is_accepted_and_every_other_refused() {
    for phase in phases() {
        for event in events() {
            let moved = next(&phase, SINCE, &event, &OPEN, AT);
            match forbidden(&phase, &event) {
                Some(code) => assert_eq!(moved.err().map(|e| e.code).as_deref(), Some(code), "{phase:?} + {event:?}"),
                None => assert!(moved.is_ok(), "{phase:?} + {event:?}: {moved:?}"),
            }
        }
    }
}

/// 13 October: a node called `wait_for_machine` and the hook of that very call reported a turn a
/// second later, which read as the node going on by itself: the pause was undone at once and the
/// node counted as working again. Only the gate ends a declared wait, and it sees a new turn itself.
#[test]
fn a_node_waiting_for_the_machine_holds_no_slot_and_only_the_gate_brings_it_back() {
    let paused = NodePhase::Paused(pause(declared()));
    assert!(!paused.holds_slot());
    assert_eq!(next(&paused, SINCE, &NodeEvent::TurnEnded, &OPEN, AT).unwrap(), paused, "the turn the pause cut ends");
    assert_eq!(next(&paused, SINCE, &NodeEvent::Listed { listed: Listed::Working }, &OPEN, AT).unwrap(), paused,
        "a listing has no turn times: it never undoes a pause");
    assert_eq!(next(&paused, SINCE, &NodeEvent::TurnStarted, &OPEN, AT).unwrap_err().code, "waits_for_machine",
        "a hook of its own turn is no resume");
    let resumed = next(&paused, SINCE, &NodeEvent::Resumed, &OPEN, AT).unwrap();
    assert_eq!(resumed, NodePhase::Starting { reviving: false });
    assert_eq!(next(&resumed, SINCE, &NodeEvent::TurnStarted, &OPEN, AT).unwrap(), NodePhase::Working);
}

#[test]
fn a_turn_an_api_error_ended_waits_without_a_slot_and_its_resume_brings_it_back_to_work() {
    let paused = next(&NodePhase::Working, SINCE, &NodeEvent::Paused { pause: pause(api_error()) }, &OPEN, AT).unwrap();
    assert_eq!(paused, NodePhase::Paused(pause(api_error())));
    assert!(!paused.holds_slot());
    assert_eq!(next(&paused, SINCE, &NodeEvent::TurnFailed, &OPEN, AT).unwrap(), paused, "the failed turn's end leaves the pause");
    assert_eq!(next(&paused, SINCE, &NodeEvent::Listed { listed: Listed::Blocked }, &OPEN, AT).unwrap(), paused,
        "Claude Code lists the failed session as blocked: that never undoes the pause");
    let resumed = next(&paused, SINCE, &NodeEvent::Resumed, &OPEN, AT).unwrap();
    assert_eq!(resumed, NodePhase::Starting { reviving: false });
    assert_eq!(next(&resumed, SINCE, &NodeEvent::TurnStarted, &OPEN, AT).unwrap(), NodePhase::Working);
}

#[test]
fn a_turn_an_api_error_ended_for_good_rests_on_its_card_and_never_counts_as_work() {
    let card = TaskFacts { card: true, ..OPEN };
    assert_eq!(next(&NodePhase::Working, SINCE, &NodeEvent::TurnFailed, &card, AT).unwrap(), NodePhase::WaitingUser { in_turn: false });
    assert_eq!(next(&NodePhase::Working, SINCE, &NodeEvent::TurnFailed, &OPEN, AT).unwrap(), NodePhase::Stopped);
    let closing = TaskFacts { closing: true, ..OPEN };
    assert_eq!(next(&NodePhase::Closing, SINCE, &NodeEvent::TurnFailed, &closing, AT).unwrap(), NodePhase::Closing);
}

#[test]
fn an_api_error_is_retried_a_growing_wait_apart_and_asks_the_user_only_when_others_prove_the_api_works() {
    let retry = |attempt, health| after_api_failure("server_error", Some("Connection lost mid-response"), attempt, health, AT);
    let waits: Vec<_> = (1..=3).map(|attempt| match retry(attempt, ApiHealth::Proven) {
        AfterApiFailure::Retry(r) => { assert!(!r.outage); r.at - AT }
        other => panic!("attempt {attempt}: {other:?}"),
    }).collect();
    assert_eq!(waits, API_RETRIES.to_vec());
    assert_eq!(retry(4, ApiHealth::Proven), AfterApiFailure::Ask);
    assert!(matches!(retry(4, ApiHealth::Unknown), AfterApiFailure::Retry(ApiRetry { outage: true, .. })), "no proof the API works: it keeps waiting");
    assert!(matches!(retry(1, ApiHealth::Down), AfterApiFailure::Retry(ApiRetry { outage: true, .. })));
    assert!(matches!(retry(9, ApiHealth::Down), AfterApiFailure::Retry(ApiRetry { outage: true, .. })), "an outage never raises a card");
    assert_eq!(after_api_failure("rate_limit", None, 1, ApiHealth::Proven, AT), AfterApiFailure::WaitUsage);
    assert_eq!(after_api_failure("authentication_failed", None, 1, ApiHealth::Proven, AT), AfterApiFailure::Ask);
    assert_eq!(classify("overloaded"), ApiErrorKind::Transient);
    assert_eq!(classify("an_error_added_later"), ApiErrorKind::Transient);
}

#[test]
fn an_open_card_takes_the_node_out_of_the_slots_even_inside_a_turn() {
    let card = TaskFacts { card: true, ..OPEN };
    assert_eq!(next(&NodePhase::Stopped, SINCE, &NodeEvent::TurnStarted, &card, AT).unwrap(), NodePhase::WaitingUser { in_turn: true });
    assert_eq!(settle(NodePhase::WaitingUser { in_turn: true }, &OPEN), NodePhase::Working, "the answer lets its turn go on");
    assert_eq!(settle(NodePhase::WaitingUser { in_turn: false }, &OPEN), NodePhase::Stopped);
}

/// t-stale-delivery-retract delivered at 11:53, its node went on with final checks and the gate
/// gave its slot to t-release-0259: Fleet showed 7 of 6 working.
#[test]
fn a_node_that_delivers_keeps_its_slot_until_its_turn_ends() {
    let delivered = TaskFacts { delivered: true, ..OPEN };
    let delivering = settle(NodePhase::Working, &delivered);
    assert_eq!(delivering, NodePhase::Delivered { in_turn: true });
    assert!(delivering.holds_slot(), "its final checks are real work");
    let rested = next(&delivering, SINCE, &NodeEvent::TurnEnded, &delivered, AT).unwrap();
    assert_eq!(rested, NodePhase::Delivered { in_turn: false });
    assert!(!rested.holds_slot());
    let back = next(&rested, SINCE, &NodeEvent::TurnStarted, &delivered, AT).unwrap();
    assert!(back.holds_slot(), "a turn it starts after delivering is work too");
    assert_eq!(next(&delivering, SINCE, &NodeEvent::Listed { listed: Listed::Idle }, &delivered, AT).unwrap(), rested,
        "an idle listing frees the slot when the turn end was missed");
}

/// t-gate-count-mismatch ended its turn with "budget exhausted (1% left)" and Claude Code listed
/// it blocked, waiting on the user: it is stopped, not at work, and Alondra must send it back.
#[test]
fn a_stopped_node_listed_blocked_stays_stopped_and_holds_no_slot() {
    let blocked = NodeEvent::Listed { listed: Listed::Blocked };
    let stopped = next(&NodePhase::Working, SINCE, &NodeEvent::TurnEnded, &OPEN, AT).unwrap();
    assert_eq!(stopped, NodePhase::Stopped);
    assert_eq!(next(&stopped, SINCE, &blocked, &OPEN, AT).unwrap(), NodePhase::Stopped);
    assert!(!NodePhase::Stopped.holds_slot());
}

/// 13 October: 1d6d53c5 ended its turn asking for a repro in its own terminal and its turn end
/// never reached Alondra. Claude Code listed it blocked for 8.5 hours while the node machine said
/// Working: it held a slot, counted in "1 node working", and nothing asked the user anything.
/// Blocked is never work: a wait Alondra can answer is a held card, and one it cannot rests the
/// node, which Alondra sends back to ask through a card.
#[test]
fn a_node_listed_blocked_is_never_at_work() {
    let blocked = NodeEvent::Listed { listed: Listed::Blocked };
    let late = SINCE + START_GRACE;
    for phase in [NodePhase::Working, NodePhase::Starting { reviving: true }, NodePhase::Starting { reviving: false }, NodePhase::Lost] {
        let moved = next(&phase, SINCE, &blocked, &OPEN, late).unwrap();
        assert_eq!(moved, NodePhase::Stopped, "{phase:?}");
        assert!(!moved.holds_slot());
    }
    let card = TaskFacts { card: true, ..OPEN };
    assert_eq!(next(&NodePhase::WaitingUser { in_turn: true }, SINCE, &blocked, &card, AT).unwrap(), NodePhase::WaitingUser { in_turn: true },
        "a held permission or question already waits on its card");
    let delivered = TaskFacts { delivered: true, ..OPEN };
    assert_eq!(next(&NodePhase::Delivered { in_turn: true }, SINCE, &blocked, &delivered, AT).unwrap(), NodePhase::Delivered { in_turn: false });
    let starting = NodePhase::Starting { reviving: false };
    assert_eq!(next(&starting, SINCE, &blocked, &OPEN, AT).unwrap(), starting, "a resumed start is listed as it was a moment more");
}

/// 13 October: t-gate-count-mismatch delivered and Claude Code listed its session done, which read
/// as gone, so a node waiting on the user's review showed as brought back. Done is a turn that
/// ended: with its task open the node rests, without one it has finished.
#[test]
fn a_session_listed_done_rests_while_its_task_needs_it_and_closes_once_it_does_not() {
    let done = NodeEvent::Listed { listed: Listed::Done };
    let delivered = TaskFacts { delivered: true, ..OPEN };
    assert_eq!(next(&NodePhase::Working, SINCE, &done, &OPEN, SINCE + START_GRACE).unwrap(), NodePhase::Stopped);
    assert_eq!(next(&NodePhase::Stopped, SINCE, &done, &OPEN, AT).unwrap(), NodePhase::Stopped);
    assert_eq!(next(&NodePhase::Delivered { in_turn: true }, SINCE, &done, &delivered, AT).unwrap(), NodePhase::Delivered { in_turn: false });
    let over = TaskFacts { open: false, closing: true, ..OPEN };
    assert_eq!(next(&NodePhase::Closing, SINCE, &done, &over, AT).unwrap(), NodePhase::Closed);
    assert_eq!(next(&NodePhase::Working, SINCE, &done, &TaskFacts::default(), AT).unwrap(), NodePhase::Closed);
}

/// 13 October: quill's session ended and its restart waited at the fleet gate for memory, every
/// 30 s, while its task said «Reconectando». A lost node waits for the gate holding no slot; only
/// once the gate lets it back is it restarted, and only then does it hold one.
#[test]
fn a_lost_session_waits_for_the_gate_and_is_restarted_only_once_admitted() {
    let lost = next(&NodePhase::Working, SINCE, &NodeEvent::SessionLost, &OPEN, AT).unwrap();
    assert_eq!(lost, NodePhase::Lost);
    assert!(!lost.holds_slot() && lost.coming_back());
    let admitted = next(&lost, SINCE, &NodeEvent::RevivalAdmitted, &OPEN, AT).unwrap();
    assert_eq!(admitted, NodePhase::Starting { reviving: true });
    assert!(admitted.holds_slot() && admitted.coming_back(), "the gate counted its start");
    assert_eq!(next(&admitted, SINCE, &NodeEvent::SessionLost, &OPEN, AT).unwrap(), NodePhase::Lost,
        "a restart that did not connect back leaves it lost, waiting for the gate again");
    assert_eq!(next(&admitted, SINCE, &NodeEvent::TurnStarted, &OPEN, AT).unwrap(), NodePhase::Working, "it came back");
    assert_eq!(next(&lost, SINCE, &NodeEvent::Listed { listed: Listed::Working }, &OPEN, AT).unwrap(), NodePhase::Working,
        "a session that came back by itself works again");
    let over = TaskFacts { open: false, closing: true, ..OPEN };
    assert_eq!(next(&NodePhase::Working, SINCE, &NodeEvent::SessionLost, &over, AT).unwrap(), NodePhase::Closed,
        "with no open task nothing brings it back");
}

/// A node the previous daemon saved as being restarted, whose session is still absent once a
/// start has had its time, is lost: it waits for the gate again instead of holding a slot.
#[test]
fn a_restart_absent_past_the_start_grace_is_lost_again() {
    let reviving = NodePhase::Starting { reviving: true };
    let absent = NodeEvent::Listed { listed: Listed::Absent };
    assert_eq!(next(&reviving, SINCE, &absent, &OPEN, SINCE + 1).unwrap(), reviving, "a restart in progress is not listed yet");
    assert_eq!(next(&reviving, SINCE, &absent, &OPEN, SINCE + START_GRACE).unwrap(), NodePhase::Lost);
    assert_eq!(next(&reviving, SINCE, &absent, &TaskFacts { open: false, closing: true, ..OPEN }, SINCE + START_GRACE).unwrap(), NodePhase::Closed,
        "with its task over, a session that is gone has finished");
}

#[test]
fn a_session_missing_from_the_listing_frees_its_slot_once_its_start_had_time() {
    let starting = NodePhase::Starting { reviving: false };
    let absent = NodeEvent::Listed { listed: Listed::Absent };
    assert_eq!(next(&starting, SINCE, &absent, &OPEN, SINCE + 1).unwrap(), starting, "a start in progress is not listed yet");
    let lost = next(&starting, SINCE, &absent, &OPEN, SINCE + START_GRACE).unwrap();
    assert_eq!(lost, NodePhase::Lost);
    assert!(!lost.holds_slot());
    assert_eq!(next(&lost, SINCE, &absent, &OPEN, SINCE + 24 * 60 * 60_000).unwrap(), lost, "a lost node waits for the gate");
    assert_eq!(next(&NodePhase::Working, SINCE, &absent, &TaskFacts::default(), AT).unwrap(), NodePhase::Closed,
        "a node without an open task whose session is gone is closed");
}

#[derive(Clone, Debug)]
enum Step {
    Launch,
    Turn { pick: usize, start: bool, late: bool },
    ApiFailure { pick: usize, retried: bool },
    Ask(usize),
    Answer(usize),
    Deliver(usize),
    Accept(usize),
    Pause { pick: usize, by_user: bool },
    Resume(usize),
    Vanish(usize),
    Lost(usize),
    Revive { pick: usize, back: bool },
    UserCloses(usize),
    Cancel(usize),
    Tick(i64),
    Reconcile,
    Restart,
    Replay,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        3 => Just(Step::Launch),
        6 => (0usize..8, any::<bool>(), prop::bool::weighted(0.25)).prop_map(|(pick, start, late)| Step::Turn { pick, start, late }),
        2 => (0usize..8, any::<bool>()).prop_map(|(pick, retried)| Step::ApiFailure { pick, retried }),
        2 => (0usize..8).prop_map(Step::Ask),
        2 => (0usize..8).prop_map(Step::Answer),
        2 => (0usize..8).prop_map(Step::Deliver),
        1 => (0usize..8).prop_map(Step::Accept),
        2 => (0usize..8, prop::bool::weighted(0.3)).prop_map(|(pick, by_user)| Step::Pause { pick, by_user }),
        2 => (0usize..8).prop_map(Step::Resume),
        2 => (0usize..8).prop_map(Step::Vanish),
        1 => (0usize..8).prop_map(Step::Lost),
        2 => (0usize..8, any::<bool>()).prop_map(|(pick, back)| Step::Revive { pick, back }),
        1 => (0usize..8).prop_map(Step::UserCloses),
        1 => (0usize..8).prop_map(Step::Cancel),
        2 => (1i64..20).prop_map(|minutes| Step::Tick(minutes * 60_000)),
        2 => Just(Step::Reconcile),
        1 => Just(Step::Restart),
        2 => Just(Step::Replay),
    ]
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

struct World {
    s: DomainState,
    now: Millis,
    /// The sessions Claude Code really runs, as `claude agents` would list them.
    real: BTreeMap<AgentId, Listed>,
    nodes: usize,
    last: Option<(AgentId, NodeEvent, Millis)>,
}

impl World {
    fn new() -> World {
        let mut w = World { s: DomainState::new("epoch"), now: SINCE, real: BTreeMap::new(), nodes: 0, last: None };
        w.must(user(), Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        w.must(Actor::Orchestrator { generation: 0 }, Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
        w
    }

    fn exec(&mut self, actor: Actor, command: Command) -> Result<CommandOutcome, DomainError> {
        self.s.execute(&actor, &CommandEnvelope {
            protocol_version: 2, data_epoch: self.s.data_epoch.clone(), command_id: format!("c-{}", self.s.revision).into(),
            expected_revision: self.s.revision, command,
        })
    }

    fn must(&mut self, actor: Actor, command: Command) {
        let shown = format!("{command:?}");
        if let Err(error) = self.exec(actor, command) {
            panic!("{shown} was refused: {error}");
        }
    }

    fn event(&mut self, agent: &AgentId, event: NodeEvent, at: Millis) -> bool {
        let accepted = self.exec(Actor::System, Command::MoveNode { agent_id: agent.clone(), event: event.clone(), at }).is_ok();
        if accepted {
            self.last = Some((agent.clone(), event, at));
        }
        accepted
    }

    fn pick(&self, n: usize, keep: impl Fn(&Agent, Option<&Task>) -> bool) -> Option<AgentId> {
        let found: Vec<AgentId> = self.s.agents.values().filter(|a| keep(a, self.s.node_task(&a.id))).map(|a| a.id.clone()).collect();
        (!found.is_empty()).then(|| found[n % found.len()].clone())
    }

    fn open_task(&self, agent: &AgentId) -> Option<TaskId> {
        self.s.node_task(agent).filter(|t| !t.state.terminal()).map(|t| t.id.clone())
    }

    fn listing(&self) -> BTreeMap<AgentId, Listed> {
        self.s.agents.keys().map(|id| (id.clone(), self.real.get(id).copied().unwrap_or(Listed::Absent))).collect()
    }

    fn reconcile(&mut self) {
        let listed = self.listing();
        self.must(Actor::System, Command::ReconcileNodes { listed: listed.clone(), at: self.now });
        let again = self.exec(Actor::System, Command::ReconcileNodes { listed, at: self.now }).unwrap();
        assert!(!again.changed, "the same listing twice moves nothing the second time");
        for (id, agent) in &self.s.agents {
            if self.real.contains_key(id) {
                continue;
            }
            let young = self.now - agent.since < START_GRACE;
            assert!(match agent.phase {
                NodePhase::Closed | NodePhase::Paused(_) | NodePhase::Lost => true,
                NodePhase::Starting { .. } => young,
                _ => false,
            }, "{id:?} has no session yet is {:?}", agent.phase);
        }
    }

    fn apply(&mut self, step: &Step) {
        self.now += 1_000;
        if !matches!(step, Step::Replay) {
            self.last = None;
        }
        match step.clone() {
            Step::Launch => {
                self.nodes += 1;
                let (agent, task): (AgentId, TaskId) = (format!("node-{}", self.nodes).into(), format!("t-{}", self.nodes).into());
                self.must(Actor::Orchestrator { generation: 0 }, Command::CreateTask {
                    task_id: task.clone(), request_id: "r".into(), title: format!("Task {}", self.nodes), agent_id: None, review_prs: false,
                    dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
                });
                self.must(Actor::System, Command::RegisterAgent { agent_id: agent.clone(), name: format!("node {}", self.nodes), at: self.now });
                assert_eq!(self.s.node_phase(&agent), Some(&NodePhase::Starting { reviving: false }));
                self.must(Actor::System, Command::AssignTask { task_id: task.clone(), agent_id: agent.clone(), model: None });
                self.must(Actor::Agent { agent_id: agent.clone() }, Command::StartTask { task_id: task });
                self.real.insert(agent, Listed::Idle);
            }
            Step::Turn { pick, start, late } => {
                let Some(agent) = self.pick(pick, |a, _| a.phase.live()) else { return };
                let at = if late { self.s.agents[&agent].since - 1 } else { self.now };
                let event = if start { NodeEvent::TurnStarted } else { NodeEvent::TurnEnded };
                let before = self.s.agents[&agent].clone();
                let accepted = self.event(&agent, event, at);
                if late {
                    assert!(!accepted || self.s.agents[&agent] == before, "a late report moved {agent:?}");
                }
                if accepted && !late && self.real.contains_key(&agent) {
                    self.real.insert(agent, if start { Listed::Working } else { Listed::Idle });
                }
            }
            Step::ApiFailure { pick, retried } => {
                let Some(agent) = self.pick(pick, |a, _| a.phase.live()) else { return };
                let retry = ApiRetry { error: "overloaded".into(), detail: None, attempt: 1, at: self.now + API_RETRIES[0], outage: false };
                let pause = Pause { reason: PauseReason::ApiError(retry), since: self.now, cut: None, turn_since: None, cleanup: PauseCleanup::Done, released: false };
                if !(retried && self.event(&agent, NodeEvent::Paused { pause }, self.now)) {
                    self.event(&agent, NodeEvent::TurnFailed, self.now);
                }
                if self.real.contains_key(&agent) {
                    self.real.insert(agent, Listed::Blocked);
                }
            }
            Step::Ask(n) => {
                let Some(agent) = self.pick(n, |a, t| a.phase.live() && t.is_some_and(|t| t.state == TaskState::Running)) else { return };
                let task = self.open_task(&agent).unwrap();
                let _ = self.exec(Actor::Agent { agent_id: agent }, Command::AskQuestion {
                    attachments: vec![], interaction_id: format!("q-{}", self.s.revision).into(), scope: Scope::Task { task_id: task },
                    recipient: Recipient::User, body: "Which name?".into(), blocking: false,
                    options: vec![
                        QuestionOption { id: "a".into(), label: "Atlas".into(), description: None, inputs: vec![] },
                        QuestionOption { id: "b".into(), label: "Borealis".into(), description: None, inputs: vec![] },
                    ],
                    card: CardSpec::new(CardKind::Decide, "Choose the release name").why("The name goes on the release"),
                });
            }
            Step::Answer(n) => {
                let Some(agent) = self.pick(n, |a, t| t.is_some_and(|t| a.phase.live() && !t.state.terminal())) else { return };
                let task = self.open_task(&agent).unwrap();
                let Some(card) = self.s.open_user_card(&task).map(|i| i.id.clone()) else { return };
                self.must(user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card, option_id: "a".into(), inputs: BTreeMap::new() });
            }
            Step::Deliver(n) => {
                let Some(agent) = self.pick(n, |a, t| a.phase.live() && t.is_some_and(|t| t.state == TaskState::Running)) else { return };
                let task = self.open_task(&agent).unwrap();
                let _ = self.exec(Actor::Agent { agent_id: agent }, Command::PresentResult {
                    checklist: Vec::new(),
                    attachments: vec![], task_id: task.clone(), interaction_id: format!("final-{task}-{}", self.s.revision).into(),
                    body: "Done".into(), targets: vec![DeliveryTarget::Text { text: "Done".into() }], is_final: true,
                    nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], true),
                });
            }
            Step::Accept(n) => {
                let Some(agent) = self.pick(n, |_, t| t.is_some_and(|t| t.state == TaskState::ForYourReview)) else { return };
                let task = self.open_task(&agent).unwrap();
                let presentation = self.s.tasks[&task].final_presentation.clone().unwrap();
                self.must(user(), Command::AcceptDelivery { presentation_id: presentation, targets: vec![DeliveryTarget::Text { text: "Done".into() }] });
                if self.s.tasks[&task].state == TaskState::Closing {
                    self.must(Actor::System, Command::RecordNodeEnd { task_id: task, end: NodeEnd::Closed, agent_id: Some(agent.clone()) });
                }
                if self.s.node_phase(&agent).is_some_and(NodePhase::live) {
                    self.event(&agent, NodeEvent::SessionClosed, self.now);
                }
                self.real.remove(&agent);
            }
            Step::Pause { pick, by_user } => {
                let Some(agent) = self.pick(pick, |a, _| a.phase.holds_slot()) else { return };
                let reason = if by_user { PauseReason::ByUser } else { PauseReason::Memory };
                let pause = Pause { reason, since: self.now, cut: None, turn_since: None, cleanup: PauseCleanup::Pending, released: false };
                assert!(self.event(&agent, NodeEvent::Paused { pause }, self.now), "a node at work can always be paused");
                if self.real.contains_key(&agent) {
                    self.real.insert(agent, Listed::Idle);
                }
            }
            Step::Resume(n) => {
                let Some(agent) = self.pick(n, |a, _| matches!(a.phase, NodePhase::Paused(_))) else { return };
                assert!(self.event(&agent, NodeEvent::Resumed, self.now));
            }
            Step::Vanish(n) => {
                let Some(agent) = self.pick(n, |a, _| self.real.contains_key(&a.id)) else { return };
                self.real.remove(&agent);
            }
            Step::Lost(n) => {
                let Some(agent) = self.pick(n, |a, _| a.phase.live()) else { return };
                self.real.remove(&agent);
                self.event(&agent, NodeEvent::SessionLost, self.now);
            }
            Step::Revive { pick, back } => {
                let Some(agent) = self.pick(pick, |a, _| a.phase == NodePhase::Lost) else { return };
                assert!(self.event(&agent, NodeEvent::RevivalAdmitted, self.now), "the gate always lets a lost node back");
                assert_eq!(self.s.node_phase(&agent), Some(&NodePhase::Starting { reviving: true }));
                if back {
                    self.real.insert(agent, Listed::Idle);
                } else {
                    self.event(&agent, NodeEvent::SessionLost, self.now);
                }
            }
            Step::UserCloses(n) => {
                let Some(agent) = self.pick(n, |a, _| a.phase.live()) else { return };
                if let Some(task) = self.open_task(&agent) {
                    let _ = self.exec(Actor::System, Command::ReleaseTask { task_id: task, reason: "node closed by the user".into() });
                }
                self.real.remove(&agent);
                assert!(self.event(&agent, NodeEvent::SessionClosed, self.now));
            }
            Step::Cancel(n) => {
                let Some(agent) = self.pick(n, |_, t| t.is_some_and(|t| !t.state.terminal())) else { return };
                let task = self.open_task(&agent).unwrap();
                self.must(user(), Command::CancelTask { task_id: task });
            }
            Step::Tick(by) => self.now += by,
            Step::Reconcile => self.reconcile(),
            Step::Restart => {
                let saved = serde_json::to_string(&self.s).unwrap();
                self.s = serde_json::from_str(&saved).unwrap();
                self.reconcile();
            }
            Step::Replay => {
                let Some((agent, event, at)) = self.last.clone() else { return };
                let before = self.s.clone();
                let _ = self.exec(Actor::System, Command::MoveNode { agent_id: agent, event: event.clone(), at });
                assert_eq!(self.s.agents, before.agents, "{event:?} delivered twice moves nothing the second time");
            }
        }
    }

    fn render(&self, key: &str) -> View {
        let projection = self.s.projection();
        crate::views::build(key, RenderContext {
            domain: &self.s, projection: &projection, live: &LiveSnapshot::default(), client: Client::Mac, lang: Lang::En,
            transport: Transport::Loopback, cursor: "", rendered_at: "",
        }).unwrap()
    }

    /// After every step: one valid state per node, and the gate, Fleet, the Tasks list and the
    /// header count the same nodes at work.
    fn check(&self) {
        self.s.validate_nodes().unwrap();
        for agent in self.s.agents.keys() {
            assert_eq!(self.s.task_facts(agent), facts_node_by_node(&self.s, agent), "the one-pass facts of {} match", agent.0);
        }
        let slots: BTreeSet<String> = self.s.slots().iter().map(|a| a.id.0.clone()).collect();
        assert!(self.s.paused_nodes().keys().all(|id| !slots.contains(&id.0)), "a paused node holds a slot");

        let fleet = self.render("fleet");
        let at_work = ["Working", "Starting", "Reconnecting", "Merging", "Stalled", "Failed", "Waiting for your review", "Delivering results"];
        let delivered: BTreeSet<String> = self.s.agents.values().filter(|a| a.phase == NodePhase::Delivered { in_turn: false })
            .map(|a| a.id.0.clone()).collect();
        let in_fleet: BTreeSet<String> = badged(&fleet, &at_work).difference(&delivered).cloned().collect();
        assert_eq!(in_fleet, slots, "Fleet shows at work exactly the nodes that hold a slot");
        let open_task = |agent: &AgentId| self.s.tasks.values().any(|t| t.agent_id.as_ref() == Some(agent) && !t.archived && !t.state.terminal());
        let live: BTreeSet<String> = self.s.agents.values()
            .filter(|a| a.phase.live() && a.phase != NodePhase::Lost && open_task(&a.id))
            .map(|a| a.id.0.clone()).collect();
        assert_eq!(rows(&fleet).into_iter().filter(|id| self.s.agents.contains_key(&AgentId(id.clone()))).collect::<BTreeSet<_>>(), live,
            "Fleet lists every node with a session on an open task, and no other");

        let tasks = self.render("tasks");
        let task_slots: BTreeSet<String> = self.s.slots().iter()
            .filter_map(|a| self.s.node_task(&a.id).filter(|t| !t.state.terminal()).map(|t| t.id.0.clone())).collect();
        let task_slots: BTreeSet<String> = self.s.slots().iter()
            .filter(|a| !matches!(a.phase, NodePhase::Delivered { .. }) && !(a.phase.coming_back() && self.s.task_facts(&a.id).delivered))
            .filter_map(|a| self.s.node_task(&a.id).filter(|t| !t.state.terminal()).map(|t| t.id.0.clone())).collect();
        let listed = badged(&tasks, &["Working", "Reconnecting", "Making changes", "Resolving conflicts", "Answering you"]);
        assert_eq!(listed, task_slots, "the Tasks list says at work of the tasks whose node holds a slot");

        let header = fleet.status.counts.iter().find(|count| count.id == "working").map(|count| count.value as usize);
        assert_eq!(header, Some(slots.len()), "the header counts the same nodes");
    }
}

/// `task_facts` as it was read before the one-pass index: a scan of every card and task per node.
fn facts_node_by_node(s: &DomainState, agent: &AgentId) -> TaskFacts {
    let held = s.holds.keys().any(|id| s.interactions.get(id).is_some_and(|i| {
        matches!(&i.author, Author::Agent { agent_id } if agent_id == agent)
            && !i.withdrawn && matches!(i.payload, InteractionPayload::Question { answer: None, .. })
    }));
    let asked = s.interactions.values().any(|i| matches!(&i.author, Author::Agent { agent_id } if agent_id == agent)
        && i.recipient == Recipient::User && s.open(i)
        && matches!(i.payload, InteractionPayload::Question { .. } | InteractionPayload::ActionRequest { .. }));
    let Some(t) = s.node_task(agent) else {
        return TaskFacts { held, card: asked, ..TaskFacts::default() };
    };
    let closing = t.state.terminal() || t.state == TaskState::Closing || t.wrap_up.asked_at.is_some();
    let delivered = t.final_presentation.as_ref().and_then(|p| s.interactions.get(p))
        .is_some_and(|p| s.open(p) && s.handed_back(p).is_none())
        || matches!(t.state, TaskState::ForYourReview | TaskState::AwaitingBlocker);
    TaskFacts { open: !t.state.terminal() && !t.archived, closing, held, card: asked || s.open_user_card(&t.id).is_some(), delivered }
}

fn rows(view: &View) -> Vec<String> {
    fn walk(items: &[Item], out: &mut Vec<(String, Option<String>)>) {
        for item in items {
            if let Item::Row(row) = item {
                out.push((row.id.clone(), row.badges.first().map(|badge| badge.text.clone())));
                children(&row.children, out);
            }
        }
    }
    fn children(rows: &[crate::views::Row], out: &mut Vec<(String, Option<String>)>) {
        for row in rows {
            out.push((row.id.clone(), row.badges.first().map(|badge| badge.text.clone())));
            children(&row.children, out);
        }
    }
    let mut out = vec![];
    for section in &view.sections {
        walk(&section.items, &mut out);
    }
    out.into_iter().map(|(id, _)| id).collect()
}

fn badged(view: &View, states: &[&str]) -> BTreeSet<String> {
    fn walk(items: &[Item], states: &[&str], out: &mut BTreeSet<String>) {
        for item in items {
            if let Item::Row(row) = item {
                row_and_children(row, states, out);
            }
        }
    }
    fn row_and_children(row: &crate::views::Row, states: &[&str], out: &mut BTreeSet<String>) {
        if row.badges.first().is_some_and(|badge| states.contains(&badge.text.as_str())) {
            out.insert(row.id.clone());
        }
        for child in &row.children {
            row_and_children(child, states, out);
        }
    }
    let mut out = BTreeSet::new();
    for section in &view.sections {
        walk(&section.items, states, &mut out);
    }
    out
}

#[test]
fn generated_runs_keep_one_state_per_node_that_every_view_counts_alike() {
    let mut runner = TestRunner::new_with_rng(Config { cases: 256, failure_persistence: None, ..Config::default() },
        TestRng::deterministic_rng(RngAlgorithm::ChaCha));
    let result = runner.run(&prop::collection::vec(step(), 1..60), |steps| {
        let mut world = World::new();
        for step in &steps {
            world.apply(step);
            world.check();
        }
        Ok(())
    });
    if let Err(TestError::Fail(reason, steps)) = result {
        panic!("{reason}\nsteps: {steps:#?}");
    }
}
