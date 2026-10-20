//! A node is never stopped and working at once. Every forbidden transition of the node machine is
//! refused, and generated runs of launches, turns (some reported late), cards, deliveries, pauses,
//! resumes, sessions that vanish or close, daemon restarts and listings of the real sessions keep
//! one state per node that the gate, Fleet, the Tasks list and the header all count alike.
use super::*;
use crate::views::{Client, Item, Lang, LiveSnapshot, RenderContext, Transport, View};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestError, TestRng, TestRunner};
use std::collections::{BTreeMap, BTreeSet};

const OPEN: TaskFacts = TaskFacts { open: true, closing: false, held: false, card: false, delivered: false, awaits: false };
const SINCE: Millis = 1_000_000;
const AT: Millis = SINCE + 1;
const HOUR: Millis = 60 * 60_000;

/// Where a move leaves the node once the domain did its part: a turn that ended with nothing for
/// the user rests on the card that carries its last words.
fn next_phase(phase: &NodePhase, since: Millis, event: &NodeEvent, f: &TaskFacts, at: Millis) -> Result<NodePhase, DomainError> {
    next(phase, since, event, f, at).map(|moved| match moved {
        Settled::At(phase) => phase,
        Settled::Unheard => NodePhase::WaitingUser { in_turn: false },
    })
}

fn unheard(phase: &NodePhase, event: &NodeEvent, f: &TaskFacts, at: Millis) -> bool {
    next(phase, SINCE, event, f, at).unwrap() == Settled::Unheard
}

fn pause(reason: PauseReason) -> Pause {
    Pause { reason, since: SINCE, cut: None, turn_since: None, cleanup: PauseCleanup::Pending, released: false }
}

fn api_error() -> PauseReason {
    PauseReason::ApiError(ApiRetry { error: "server_error".into(), detail: None, attempt: 1, at: SINCE + 30_000, outage: false })
}

fn declared() -> PauseReason {
    PauseReason::Declared { reason: "The build queue is full".into() }
}

fn background() -> Background {
    Background { work: crate::model::BackgroundKind::Shell, label: "./deploy/brew/release.sh 0.2.67".into(), since: SINCE, others: 0, gone: None }
}

fn phases() -> Vec<NodePhase> {
    vec![
        NodePhase::Starting { reviving: false, idle_since: None },
        NodePhase::Starting { reviving: true, idle_since: None },
        NodePhase::Lost,
        NodePhase::Working,
        NodePhase::WaitingBackground(background()),
        NodePhase::WaitingUser { in_turn: true },
        NodePhase::WaitingUser { in_turn: false },
        NodePhase::Paused(pause(PauseReason::Memory)),
        NodePhase::Paused(pause(PauseReason::ByUser)),
        NodePhase::Paused(pause(declared())),
        NodePhase::Paused(pause(api_error())),
        NodePhase::Delivered { in_turn: true },
        NodePhase::Delivered { in_turn: false },
        NodePhase::Resuming { idle_since: None },
        NodePhase::Resuming { idle_since: Some(SINCE) },
        NodePhase::WaitingDependency,
        NodePhase::NoTask,
        NodePhase::Closing,
        NodePhase::Closed,
    ]
}

fn events() -> Vec<NodeEvent> {
    let mut all = vec![
        NodeEvent::TurnStarted, NodeEvent::TurnEnded, NodeEvent::TurnEndedBehind { work: background() }, NodeEvent::BackgroundEnded, NodeEvent::TurnFailed, NodeEvent::SessionLost, NodeEvent::SessionClosed,
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
        (P::Closed, E::SessionLost | E::SessionClosed | E::Listed { listed: Listed::Absent | Listed::Done }) => None,
        (P::Closed, _) => Some("node_closed"),
        (P::Paused(Pause { reason: PauseReason::ByUser, .. }), E::TurnStarted) => Some("paused_by_user"),
        (P::Paused(Pause { reason: PauseReason::Memory, .. }), E::TurnStarted) => Some("paused_for_memory"),
        (P::Paused(Pause { reason: PauseReason::Declared { .. }, .. }), E::TurnStarted) => Some("waits_for_machine"),
        (P::Starting { .. }, E::StartFailed | E::RevivalFailed) => None,
        (_, E::StartFailed | E::RevivalFailed) => Some("not_starting"),
        (P::Lost, E::RevivalAdmitted) => None,
        (_, E::RevivalAdmitted) => Some("not_lost"),
        (P::Paused(_), E::Paused { .. }) => Some("already_paused"),
        (P::Starting { .. } | P::Working | P::WaitingBackground(_) | P::Resuming { .. } | P::Delivered { in_turn: true }, E::Paused { .. }) => None,
        (_, E::Paused { .. }) => Some("not_working"),
        (_, E::PauseCleanup { .. } | E::PauseReleased | E::Resumed) if !paused => Some("not_paused"),
        (P::WaitingBackground(_), E::BackgroundEnded) => None,
        (_, E::BackgroundEnded) => Some("not_waiting_background"),
        _ => None,
    }
}

fn refused(phase: NodePhase, event: NodeEvent, at: Millis) -> String {
    match next(&phase, SINCE, &event, &OPEN, at) {
        Ok(moved) => panic!("{phase:?} accepted {event:?} and moved to {moved:?}"),
        Err(error) => error.code,
    }
}

fn refused_with(phase: NodePhase, event: NodeEvent, f: &TaskFacts) -> String {
    match next(&phase, SINCE, &event, f, AT) {
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

/// David's rule, 15 October: closing a node whose session is already over is the outcome sought,
/// not an error, whoever saw it first.
#[test]
fn closing_a_closed_node_again_keeps_it_closed() {
    for event in [NodeEvent::SessionLost, NodeEvent::SessionClosed, NodeEvent::Listed { listed: Listed::Absent }, NodeEvent::Listed { listed: Listed::Done }] {
        for facts in [OPEN, TaskFacts::default(), TaskFacts { closing: true, ..OPEN }] {
            assert_eq!(next_phase(&NodePhase::Closed, SINCE, &event, &facts, AT).unwrap(), NodePhase::Closed, "{event:?}");
        }
    }
}

forbid! {
    closed_refuses_a_turn_start: NodePhase::Closed, NodeEvent::TurnStarted => "node_closed";
    closed_refuses_a_turn_end: NodePhase::Closed, NodeEvent::TurnEnded => "node_closed";
    closed_refuses_a_failed_turn: NodePhase::Closed, NodeEvent::TurnFailed => "node_closed";
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
    no_task_refuses_a_failed_start: NodePhase::NoTask, NodeEvent::StartFailed => "not_starting";
    no_task_refuses_a_failed_revival: NodePhase::NoTask, NodeEvent::RevivalFailed => "not_starting";
    closing_refuses_a_failed_start: NodePhase::Closing, NodeEvent::StartFailed => "not_starting";
    closing_refuses_a_failed_revival: NodePhase::Closing, NodeEvent::RevivalFailed => "not_starting";
    lost_refuses_a_failed_start: NodePhase::Lost, NodeEvent::StartFailed => "not_starting";
    lost_refuses_a_failed_revival: NodePhase::Lost, NodeEvent::RevivalFailed => "not_starting";

    starting_refuses_a_revival: NodePhase::Starting { reviving: false, idle_since: None }, NodeEvent::RevivalAdmitted => "not_lost";
    reviving_refuses_a_second_revival: NodePhase::Starting { reviving: true, idle_since: None }, NodeEvent::RevivalAdmitted => "not_lost";
    working_refuses_a_revival: NodePhase::Working, NodeEvent::RevivalAdmitted => "not_lost";
    waiting_refuses_a_revival: NodePhase::WaitingUser { in_turn: false }, NodeEvent::RevivalAdmitted => "not_lost";
    paused_refuses_a_revival: NodePhase::Paused(pause(PauseReason::Memory)), NodeEvent::RevivalAdmitted => "not_lost";
    delivered_refuses_a_revival: NodePhase::Delivered { in_turn: false }, NodeEvent::RevivalAdmitted => "not_lost";
    no_task_refuses_a_revival: NodePhase::NoTask, NodeEvent::RevivalAdmitted => "not_lost";
    closing_refuses_a_revival: NodePhase::Closing, NodeEvent::RevivalAdmitted => "not_lost";

    paused_refuses_a_second_pause: NodePhase::Paused(pause(PauseReason::Memory)), NodeEvent::Paused { pause: pause(PauseReason::Usage) } => "already_paused";
    paused_for_an_api_error_refuses_a_second_pause: NodePhase::Paused(pause(api_error())), NodeEvent::Paused { pause: pause(api_error()) } => "already_paused";
    waiting_in_turn_refuses_an_api_error_pause: NodePhase::WaitingUser { in_turn: true }, NodeEvent::Paused { pause: pause(api_error()) } => "not_working";
    paused_by_the_user_refuses_a_second_pause: NodePhase::Paused(pause(PauseReason::ByUser)), NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "already_paused";
    waiting_in_turn_refuses_a_pause: NodePhase::WaitingUser { in_turn: true }, NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "not_working";
    waiting_refuses_a_pause: NodePhase::WaitingUser { in_turn: false }, NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "not_working";
    delivered_refuses_a_pause: NodePhase::Delivered { in_turn: false }, NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "not_working";
    no_task_refuses_a_pause: NodePhase::NoTask, NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "not_working";
    waiting_for_a_task_refuses_a_pause: NodePhase::WaitingDependency, NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "not_working";
    closing_refuses_a_pause: NodePhase::Closing, NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "not_working";
    lost_refuses_a_pause: NodePhase::Lost, NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "not_working";

    starting_refuses_a_pause_cleanup: NodePhase::Starting { reviving: false, idle_since: None }, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    starting_refuses_a_pause_release: NodePhase::Starting { reviving: false, idle_since: None }, NodeEvent::PauseReleased => "not_paused";
    starting_refuses_a_resume: NodePhase::Starting { reviving: false, idle_since: None }, NodeEvent::Resumed => "not_paused";
    reviving_refuses_a_pause_cleanup: NodePhase::Starting { reviving: true, idle_since: None }, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    reviving_refuses_a_pause_release: NodePhase::Starting { reviving: true, idle_since: None }, NodeEvent::PauseReleased => "not_paused";
    reviving_refuses_a_resume: NodePhase::Starting { reviving: true, idle_since: None }, NodeEvent::Resumed => "not_paused";
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
    no_task_refuses_a_pause_cleanup: NodePhase::NoTask, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    no_task_refuses_a_pause_release: NodePhase::NoTask, NodeEvent::PauseReleased => "not_paused";
    no_task_refuses_a_resume: NodePhase::NoTask, NodeEvent::Resumed => "not_paused";
    closing_refuses_a_pause_cleanup: NodePhase::Closing, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    closing_refuses_a_pause_release: NodePhase::Closing, NodeEvent::PauseReleased => "not_paused";
    closing_refuses_a_resume: NodePhase::Closing, NodeEvent::Resumed => "not_paused";

    waiting_background_refuses_a_failed_start: NodePhase::WaitingBackground(background()), NodeEvent::StartFailed => "not_starting";
    waiting_background_refuses_a_revival: NodePhase::WaitingBackground(background()), NodeEvent::RevivalAdmitted => "not_lost";
    waiting_background_refuses_a_pause_cleanup: NodePhase::WaitingBackground(background()), NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    waiting_background_refuses_a_resume: NodePhase::WaitingBackground(background()), NodeEvent::Resumed => "not_paused";
    working_refuses_an_end_of_background_work: NodePhase::Working, NodeEvent::BackgroundEnded => "not_waiting_background";
    no_task_refuses_an_end_of_background_work: NodePhase::NoTask, NodeEvent::BackgroundEnded => "not_waiting_background";
    paused_refuses_an_end_of_background_work: NodePhase::Paused(pause(PauseReason::ByUser)), NodeEvent::BackgroundEnded => "not_waiting_background";
    closed_refuses_an_end_of_background_work: NodePhase::Closed, NodeEvent::BackgroundEnded => "node_closed";
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
    a_late_turn_start_never_moves_a_resting_node_without_a_task: NodePhase::NoTask, NodeEvent::TurnStarted;
    a_late_turn_end_never_moves_a_working_node: NodePhase::Working, NodeEvent::TurnEnded;
    a_late_turn_end_behind_background_work_never_moves_a_working_node: NodePhase::Working, NodeEvent::TurnEndedBehind { work: background() };
    a_late_failed_turn_never_moves_a_working_node: NodePhase::Working, NodeEvent::TurnFailed;
    a_late_lost_session_never_moves_a_working_node: NodePhase::Working, NodeEvent::SessionLost;
    a_late_lost_session_never_moves_a_revived_node: NodePhase::Starting { reviving: false, idle_since: None }, NodeEvent::SessionLost;
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
    assert_eq!(next_phase(&paused, SINCE, &NodeEvent::TurnEnded, &OPEN, AT).unwrap(), paused, "the turn the pause cut ends");
    assert_eq!(next_phase(&paused, SINCE, &NodeEvent::Listed { listed: Listed::Working }, &OPEN, AT).unwrap(), paused,
        "a listing has no turn times: it never undoes a pause");
    assert_eq!(next_phase(&paused, SINCE, &NodeEvent::TurnStarted, &OPEN, AT).unwrap_err().code, "waits_for_machine",
        "a hook of its own turn is no resume");
    let resumed = next_phase(&paused, SINCE, &NodeEvent::Resumed, &OPEN, AT).unwrap();
    assert_eq!(resumed, NodePhase::Starting { reviving: false, idle_since: None });
    assert_eq!(next_phase(&resumed, SINCE, &NodeEvent::TurnStarted, &OPEN, AT).unwrap(), NodePhase::Working);
}

#[test]
fn a_turn_an_api_error_ended_waits_without_a_slot_and_its_resume_brings_it_back_to_work() {
    let paused = next_phase(&NodePhase::Working, SINCE, &NodeEvent::Paused { pause: pause(api_error()) }, &OPEN, AT).unwrap();
    assert_eq!(paused, NodePhase::Paused(pause(api_error())));
    assert!(!paused.holds_slot());
    assert_eq!(next_phase(&paused, SINCE, &NodeEvent::TurnFailed, &OPEN, AT).unwrap(), paused, "the failed turn's end leaves the pause");
    assert_eq!(next_phase(&paused, SINCE, &NodeEvent::Listed { listed: Listed::Blocked }, &OPEN, AT).unwrap(), paused,
        "Claude Code lists the failed session as blocked: that never undoes the pause");
    let resumed = next_phase(&paused, SINCE, &NodeEvent::Resumed, &OPEN, AT).unwrap();
    assert_eq!(resumed, NodePhase::Starting { reviving: false, idle_since: None });
    assert_eq!(next_phase(&resumed, SINCE, &NodeEvent::TurnStarted, &OPEN, AT).unwrap(), NodePhase::Working);
}

#[test]
fn a_turn_an_api_error_ended_for_good_rests_on_its_card_and_never_counts_as_work() {
    let card = TaskFacts { card: true, ..OPEN };
    assert_eq!(next_phase(&NodePhase::Working, SINCE, &NodeEvent::TurnFailed, &card, AT).unwrap(), NodePhase::WaitingUser { in_turn: false });
    assert!(unheard(&NodePhase::Working, &NodeEvent::TurnFailed, &OPEN, AT), "with no card its last words go to the user");
    let closing = TaskFacts { closing: true, ..OPEN };
    assert_eq!(next_phase(&NodePhase::Closing, SINCE, &NodeEvent::TurnFailed, &closing, AT).unwrap(), NodePhase::Closing);
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
    assert_eq!(next_phase(&NodePhase::WaitingUser { in_turn: false }, SINCE, &NodeEvent::TurnStarted, &card, AT).unwrap(), NodePhase::WaitingUser { in_turn: true });
    assert_eq!(settle(NodePhase::WaitingUser { in_turn: true }, &OPEN), NodePhase::Working, "the answer lets its turn go on");
    assert_eq!(settle(NodePhase::WaitingUser { in_turn: false }, &OPEN), NodePhase::Resuming { idle_since: None },
        "the answer is on its way to the node and starts its next turn");
}

/// t-stale-delivery-retract delivered at 11:53, its node went on with final checks and the gate
/// gave its slot to t-release-0259: Fleet showed 7 of 6 working.
#[test]
fn a_node_that_delivers_keeps_its_slot_until_its_turn_ends() {
    let delivered = TaskFacts { delivered: true, ..OPEN };
    let delivering = settle(NodePhase::Working, &delivered);
    assert_eq!(delivering, NodePhase::Delivered { in_turn: true });
    assert!(delivering.holds_slot(), "its final checks are real work");
    let rested = next_phase(&delivering, SINCE, &NodeEvent::TurnEnded, &delivered, AT).unwrap();
    assert_eq!(rested, NodePhase::Delivered { in_turn: false });
    assert!(!rested.holds_slot());
    let back = next_phase(&rested, SINCE, &NodeEvent::TurnStarted, &delivered, AT).unwrap();
    assert!(back.holds_slot(), "a turn it starts after delivering is work too");
    assert_eq!(next_phase(&delivering, SINCE, &NodeEvent::Listed { listed: Listed::Idle }, &delivered, AT).unwrap(), rested,
        "an idle listing frees the slot when the turn end was missed");
}

/// 14 October: t-holepunch-iroh-prototype ended its turn writing to David in its terminal, with no
/// card, and showed «Sin actividad» everywhere. A turn that ends on an open task with nothing for
/// the user is never a phase of its own: its last words go to the user in a card and it waits on
/// that card, holding no slot.
#[test]
fn a_turn_that_ends_with_nothing_for_the_user_rests_on_a_card_with_its_last_words() {
    for event in [NodeEvent::TurnEnded, NodeEvent::TurnFailed] {
        assert!(unheard(&NodePhase::Working, &event, &OPEN, AT), "{event:?}");
    }
    let rested = next_phase(&NodePhase::Working, SINCE, &NodeEvent::TurnEnded, &OPEN, AT).unwrap();
    assert_eq!(rested, NodePhase::WaitingUser { in_turn: false });
    assert!(!rested.holds_slot());
    let card = TaskFacts { card: true, ..OPEN };
    assert_eq!(next_phase(&rested, SINCE, &NodeEvent::Listed { listed: Listed::Blocked }, &card, AT).unwrap(), rested,
        "Claude Code lists it blocked while it waits on that card");
    assert_eq!(next(&NodePhase::Working, SINCE, &NodeEvent::TurnEnded, &TaskFacts { awaits: true, ..OPEN }, AT).unwrap(),
        Settled::At(NodePhase::WaitingDependency), "a task that waits for another task waits for it, not for the user");
    assert_eq!(next(&NodePhase::Working, SINCE, &NodeEvent::TurnEnded, &TaskFacts::default(), AT).unwrap(),
        Settled::At(NodePhase::NoTask), "a node without an open task has nobody to tell");
}

/// 13 October: 1d6d53c5 ended its turn asking for a repro in its own terminal and its turn end
/// never reached Alondra. Claude Code listed it blocked for 8.5 hours while the node machine said
/// Working: it held a slot, counted in "1 node working", and nothing asked the user anything.
/// Blocked is never work: a wait Alondra can answer is a held card, and one it cannot puts the
/// node's last words in a card for the user.
#[test]
fn a_node_listed_blocked_is_never_at_work() {
    let blocked = NodeEvent::Listed { listed: Listed::Blocked };
    for phase in [NodePhase::Working, NodePhase::Starting { reviving: true, idle_since: None }, NodePhase::Starting { reviving: false, idle_since: Some(AT) }, NodePhase::Lost] {
        assert!(unheard(&phase, &blocked, &OPEN, AT + 1), "{phase:?}");
    }
    let card = TaskFacts { card: true, ..OPEN };
    assert_eq!(next_phase(&NodePhase::WaitingUser { in_turn: true }, SINCE, &blocked, &card, AT).unwrap(), NodePhase::WaitingUser { in_turn: true },
        "a held permission or question already waits on its card");
    let delivered = TaskFacts { delivered: true, ..OPEN };
    assert_eq!(next_phase(&NodePhase::Delivered { in_turn: true }, SINCE, &blocked, &delivered, AT).unwrap(), NodePhase::Delivered { in_turn: false });
    let starting = NodePhase::Starting { reviving: false, idle_since: None };
    let seen = NodePhase::Starting { reviving: false, idle_since: Some(AT) };
    assert_eq!(next_phase(&starting, SINCE, &blocked, &OPEN, AT).unwrap(), seen, "a resumed start is listed as it was a moment more");
    assert_eq!(next_phase(&seen, SINCE, &blocked, &OPEN, AT).unwrap(), seen, "the same listing again moves nothing");
}

/// 13 October: t-gate-count-mismatch delivered and Claude Code listed its session done, which read
/// as gone, so a node waiting on the user's review showed as brought back. Done is a turn that
/// ended: with its task open the node rests, without one it has finished.
#[test]
fn a_session_listed_done_rests_while_its_task_needs_it_and_closes_once_it_does_not() {
    let done = NodeEvent::Listed { listed: Listed::Done };
    let delivered = TaskFacts { delivered: true, ..OPEN };
    assert!(unheard(&NodePhase::Working, &done, &OPEN, AT));
    let awaits = TaskFacts { awaits: true, ..OPEN };
    assert_eq!(next_phase(&NodePhase::WaitingDependency, SINCE, &done, &awaits, AT).unwrap(), NodePhase::WaitingDependency);
    assert_eq!(next_phase(&NodePhase::Delivered { in_turn: true }, SINCE, &done, &delivered, AT).unwrap(), NodePhase::Delivered { in_turn: false });
    let over = TaskFacts { open: false, closing: true, ..OPEN };
    assert_eq!(next_phase(&NodePhase::Closing, SINCE, &done, &over, AT).unwrap(), NodePhase::Closed);
    assert_eq!(next_phase(&NodePhase::Working, SINCE, &done, &TaskFacts::default(), AT).unwrap(), NodePhase::Closed);
}

/// 16 and 17 October: the node of «mapa del backend» waited for other tasks, its session ended,
/// and the gate let it back while they were still open, four times.
#[test]
fn a_node_whose_task_waits_for_other_tasks_is_let_back_only_once_they_are_done() {
    let awaits = TaskFacts { awaits: true, ..OPEN };
    assert_eq!(next_phase(&NodePhase::WaitingDependency, SINCE, &NodeEvent::SessionLost, &awaits, AT).unwrap(), NodePhase::Lost);
    assert_eq!(refused_with(NodePhase::Lost, NodeEvent::RevivalAdmitted, &awaits), "waits_dependency");
    assert_eq!(next_phase(&NodePhase::Lost, SINCE, &NodeEvent::RevivalAdmitted, &OPEN, AT).unwrap(),
        NodePhase::Starting { reviving: true, idle_since: None }, "once they are done");
}

/// 17 October, 08:37: a minute after its turn ended, Claude Code still listed the session of a
/// node waiting for other tasks as working; the node read as at work, its turn as stalled, and it
/// was restarted.
#[test]
fn a_listing_that_says_working_never_puts_a_node_waiting_for_other_tasks_back_to_work() {
    let awaits = TaskFacts { awaits: true, ..OPEN };
    let working = NodeEvent::Listed { listed: Listed::Working };
    assert_eq!(next_phase(&NodePhase::WaitingDependency, SINCE, &working, &awaits, AT).unwrap(), NodePhase::WaitingDependency);
    assert_eq!(next_phase(&NodePhase::WaitingDependency, SINCE, &NodeEvent::TurnStarted, &awaits, AT).unwrap(), NodePhase::Working,
        "the start of its own turn does");
}

/// 13 October: quill's session ended and its restart waited at the fleet gate for memory, every
/// 30 s, while its task said «Reconectando». A lost node waits for the gate holding no slot; only
/// once the gate lets it back is it restarted, and only then does it hold one.
#[test]
fn a_lost_session_waits_for_the_gate_and_is_restarted_only_once_admitted() {
    let lost = next_phase(&NodePhase::Working, SINCE, &NodeEvent::SessionLost, &OPEN, AT).unwrap();
    assert_eq!(lost, NodePhase::Lost);
    assert!(!lost.holds_slot() && lost.coming_back());
    let admitted = next_phase(&lost, SINCE, &NodeEvent::RevivalAdmitted, &OPEN, AT).unwrap();
    assert_eq!(admitted, NodePhase::Starting { reviving: true, idle_since: None });
    assert!(admitted.holds_slot() && admitted.coming_back(), "the gate counted its start");
    assert_eq!(next_phase(&admitted, SINCE, &NodeEvent::SessionLost, &OPEN, AT).unwrap(), NodePhase::Lost,
        "a restart that did not connect back leaves it lost, waiting for the gate again");
    assert_eq!(next_phase(&admitted, SINCE, &NodeEvent::TurnStarted, &OPEN, AT).unwrap(), NodePhase::Working, "it came back");
    assert_eq!(next_phase(&lost, SINCE, &NodeEvent::Listed { listed: Listed::Working }, &OPEN, AT).unwrap(), NodePhase::Working,
        "a session that came back by itself works again");
    let over = TaskFacts { open: false, closing: true, ..OPEN };
    assert_eq!(next_phase(&NodePhase::Working, SINCE, &NodeEvent::SessionLost, &over, AT).unwrap(), NodePhase::Closed,
        "with no open task nothing brings it back");
}

/// A node being restarted whose session two listings in a row find absent is lost: it waits for
/// the gate again instead of holding a slot. The first may predate the restart.
#[test]
fn a_restart_two_listings_find_absent_is_lost_again() {
    let reviving = NodePhase::Starting { reviving: true, idle_since: None };
    let absent = NodeEvent::Listed { listed: Listed::Absent };
    let seen = next_phase(&reviving, SINCE, &absent, &OPEN, AT).unwrap();
    assert_eq!(seen, NodePhase::Starting { reviving: true, idle_since: Some(AT) }, "a restart in progress is not listed yet");
    assert_eq!(next_phase(&seen, SINCE, &absent, &OPEN, AT).unwrap(), seen, "the same listing again moves nothing");
    assert_eq!(next_phase(&seen, SINCE, &absent, &OPEN, AT + 1).unwrap(), NodePhase::Lost);
    assert_eq!(next_phase(&seen, SINCE, &absent, &TaskFacts { open: false, closing: true, ..OPEN }, AT + 1).unwrap(), NodePhase::Closed,
        "with its task over, a session that is gone has finished");
    assert_eq!(next_phase(&reviving, SINCE, &absent, &OPEN, SINCE - 1).unwrap(), reviving, "a listing asked for before the restart says nothing of it");
}

#[test]
fn a_session_two_listings_miss_frees_its_slot() {
    let starting = NodePhase::Starting { reviving: false, idle_since: None };
    let absent = NodeEvent::Listed { listed: Listed::Absent };
    let seen = next_phase(&starting, SINCE, &absent, &OPEN, AT).unwrap();
    assert_eq!(seen, NodePhase::Starting { reviving: false, idle_since: Some(AT) }, "a start in progress is not listed yet");
    assert!(seen.holds_slot());
    let lost = next_phase(&seen, SINCE, &absent, &OPEN, AT + 1).unwrap();
    assert_eq!(lost, NodePhase::Lost);
    assert!(!lost.holds_slot());
    assert_eq!(next_phase(&lost, SINCE, &absent, &OPEN, SINCE + 24 * 60 * 60_000).unwrap(), lost, "a lost node waits for the gate");
    assert_eq!(next_phase(&NodePhase::Working, SINCE, &absent, &TaskFacts::default(), AT).unwrap(), NodePhase::Closed,
        "a node without an open task whose session is gone is closed");
}

/// 13 October, the release of 0.2.67: the node ran the release script in the background and ended
/// its turn to wait for it; Claude Code woke it with the result twelve minutes later. It waits for
/// its own command the whole time, holding its slot, whatever the listing of sessions says.
#[test]
fn a_node_waiting_for_its_own_command_keeps_its_slot_until_the_command_wakes_it() {
    let behind = NodeEvent::TurnEndedBehind { work: background() };
    let waiting = next_phase(&NodePhase::Working, SINCE, &behind, &OPEN, AT).unwrap();
    assert_eq!(waiting, NodePhase::WaitingBackground(background()));
    assert!(waiting.holds_slot(), "its command uses the Mac and it goes on without the gate");
    for listed in [Listed::Idle, Listed::Done, Listed::Working] {
        assert_eq!(next_phase(&waiting, SINCE, &NodeEvent::Listed { listed }, &OPEN, SINCE + 12 * 60_000).unwrap(), waiting,
            "a listing has no turns: {listed:?} leaves it waiting");
    }
    assert_eq!(next_phase(&waiting, SINCE, &NodeEvent::TurnStarted, &OPEN, AT).unwrap(), NodePhase::Working, "the command's end wakes it");
}

#[test]
fn a_node_whose_command_ended_without_waking_it_rests_and_frees_its_slot() {
    let waiting = NodePhase::WaitingBackground(background());
    let gone = next_phase(&waiting, SINCE, &NodeEvent::BackgroundEnded, &OPEN, AT).unwrap();
    assert!(gone.holds_slot(), "its command's notification may still wake it");
    assert!(unheard(&gone, &NodeEvent::Listed { listed: Listed::Idle }, &OPEN, AT + 1), "nothing woke it: with nothing for the user its last words go to them");
    assert_eq!(next_phase(&waiting, SINCE, &NodeEvent::Listed { listed: Listed::Absent }, &OPEN, AT).unwrap(), NodePhase::Lost,
        "a session that is gone is lost, command or not");
    assert_eq!(next_phase(&waiting, SINCE, &NodeEvent::SessionLost, &OPEN, AT).unwrap(), NodePhase::Lost);
}

#[test]
fn a_node_waiting_for_its_command_yields_to_its_card_its_result_and_its_task_s_end() {
    let waiting = NodePhase::WaitingBackground(background());
    assert_eq!(settle(waiting.clone(), &TaskFacts { card: true, ..OPEN }), NodePhase::WaitingUser { in_turn: false });
    assert_eq!(settle(waiting.clone(), &TaskFacts { delivered: true, ..OPEN }), NodePhase::Delivered { in_turn: false });
    assert_eq!(settle(waiting.clone(), &TaskFacts { closing: true, ..OPEN }), NodePhase::Closing);
    assert_eq!(settle(waiting.clone(), &OPEN), waiting);
    let paused = next_phase(&waiting, SINCE, &NodeEvent::Paused { pause: pause(PauseReason::ByUser) }, &OPEN, AT).unwrap();
    assert!(matches!(paused, NodePhase::Paused(_)) && !paused.holds_slot(), "the user may pause it");
    assert_eq!(next_phase(&paused, SINCE, &NodeEvent::TurnEndedBehind { work: background() }, &OPEN, AT).unwrap(), paused,
        "a turn the pause cut ends behind its work and stays paused");
}

/// 15 October, 06:21: t-fastconn-6-reachable-mac ended its turn with «Esperando el resto.» behind
/// its ablation script and a Monitor on its results, and Claude Code's model listed it blocked
/// «waiting for complete user instructions». A node waiting for its own work is at work whatever
/// Claude Code guesses: it keeps its slot and no card goes to the user; a card of its own still
/// takes it out of the wait.
#[test]
fn a_node_behind_its_own_work_that_claude_code_lists_blocked_keeps_waiting_for_it() {
    let waiting = NodePhase::WaitingBackground(background());
    for listed in [Listed::Blocked, Listed::Idle, Listed::Done, Listed::Working] {
        assert_eq!(next(&waiting, SINCE, &NodeEvent::Listed { listed }, &OPEN, SINCE + 3 * HOUR).unwrap(), Settled::At(waiting.clone()), "{listed:?}");
    }
    assert!(waiting.holds_slot());
    let card = TaskFacts { card: true, ..OPEN };
    assert_eq!(next_phase(&waiting, SINCE, &NodeEvent::Listed { listed: Listed::Blocked }, &card, AT).unwrap(), NodePhase::WaitingUser { in_turn: false },
        "with a card of its own it waits on that one");
}

/// David's rule, 15 October: a node waiting for work of its own never rests on the card with its
/// last words. Only a turn of its own that ends (`TurnEnded`, `TurnFailed`), or that work seen gone
/// (`BackgroundEnded`) and then a later listing that finds the node without work, may put it there;
/// no listing, pause, loss, revival or change of its task's facts does, at any time.
#[test]
fn a_node_waiting_for_its_own_work_rests_on_its_last_words_only_once_that_work_or_a_turn_ends() {
    let waiting = NodePhase::WaitingBackground(background());
    let may_end_the_wait = |event: &NodeEvent| matches!(event, NodeEvent::TurnEnded | NodeEvent::TurnFailed);
    for bits in 0u8..64 {
        let bit = |n: u8| bits & (1 << n) != 0;
        let f = TaskFacts { open: bit(0), closing: bit(1), held: bit(2), card: bit(3), delivered: bit(4), awaits: bit(5) };
        for event in events().iter().filter(|event| !may_end_the_wait(event)) {
            for at in [AT, SINCE + 2 * HOUR, SINCE + 30 * HOUR] {
                if let Ok(moved) = next(&waiting, SINCE, event, &f, at) {
                    assert_ne!(moved, Settled::Unheard, "{event:?} with {f:?} at {at}");
                }
            }
        }
    }
    let gone = next_phase(&waiting, SINCE, &NodeEvent::BackgroundEnded, &OPEN, AT).unwrap();
    assert_eq!(gone, NodePhase::WaitingBackground(Background { gone: Some(AT), ..background() }), "its work's notification is on its way");
    assert_eq!(next_phase(&gone, SINCE, &NodeEvent::BackgroundEnded, &OPEN, AT + 1).unwrap(), gone, "seen gone again moves nothing");
    let idle = NodeEvent::Listed { listed: Listed::Idle };
    assert_eq!(next_phase(&gone, SINCE, &idle, &OPEN, AT).unwrap(), gone, "a listing asked for before it was gone says nothing of it");
    assert_eq!(next_phase(&gone, SINCE, &NodeEvent::TurnStarted, &OPEN, AT + 1).unwrap(), NodePhase::Working, "the notification woke it");
    assert!(unheard(&gone, &idle, &OPEN, AT + 1), "its work over and nothing woke it: its last words go to the user");
}

/// The user answers a node's card while it rests: the answer is on its way and the node works
/// again, holding its slot. A later listing that still finds it idle after the first that did ends
/// the wait as a turn end; the same listing twice moves nothing.
#[test]
fn an_answered_node_works_until_its_answer_starts_a_turn_or_two_listings_find_it_idle() {
    let resuming = settle(NodePhase::WaitingUser { in_turn: false }, &OPEN);
    assert_eq!(resuming, NodePhase::Resuming { idle_since: None });
    assert!(resuming.holds_slot());
    assert_eq!(settle(NodePhase::Delivered { in_turn: false }, &OPEN), resuming, "a result sent back for changes goes back to it too");
    let idle = NodeEvent::Listed { listed: Listed::Idle };
    let seen = next_phase(&resuming, SINCE, &idle, &OPEN, AT).unwrap();
    assert_eq!(seen, NodePhase::Resuming { idle_since: Some(AT) }, "the first idle listing may come before the answer reached it");
    assert_eq!(next_phase(&seen, SINCE, &idle, &OPEN, AT).unwrap(), seen, "the same listing again moves nothing");
    assert!(unheard(&seen, &idle, &OPEN, AT + 1));
    assert_eq!(next_phase(&seen, SINCE, &NodeEvent::TurnStarted, &OPEN, AT).unwrap(), NodePhase::Working);
    assert_eq!(settle(resuming.clone(), &TaskFacts { card: true, ..OPEN }), NodePhase::WaitingUser { in_turn: false });
    assert_eq!(settle(resuming, &TaskFacts { awaits: true, ..OPEN }), NodePhase::WaitingDependency);
}

#[derive(Clone, Debug)]
enum Step {
    Launch,
    Turn { pick: usize, start: bool, late: bool },
    Background { pick: usize, ended: bool },
    ApiFailure { pick: usize, retried: bool },
    Ask(usize),
    Answer(usize),
    Say(usize),
    Blocked(usize),
    Deliver(usize),
    Accept(usize),
    Reply(usize),
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
        3 => (0usize..8, any::<bool>()).prop_map(|(pick, ended)| Step::Background { pick, ended }),
        2 => (0usize..8, any::<bool>()).prop_map(|(pick, retried)| Step::ApiFailure { pick, retried }),
        2 => (0usize..8).prop_map(Step::Ask),
        2 => (0usize..8).prop_map(Step::Answer),
        2 => (0usize..8).prop_map(Step::Say),
        2 => (0usize..8).prop_map(Step::Blocked),
        2 => (0usize..8).prop_map(Step::Deliver),
        1 => (0usize..8).prop_map(Step::Accept),
        2 => (0usize..8).prop_map(Step::Reply),
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
    /// How far the clock moves at each step, in turn; empty, it moves as the step says.
    gaps: Vec<Millis>,
    moves: usize,
}

impl World {
    fn new() -> World {
        World::timed(vec![])
    }

    fn timed(gaps: Vec<Millis>) -> World {
        let mut w = World { s: DomainState::new("epoch"), now: SINCE, real: BTreeMap::new(), nodes: 0, last: None, gaps, moves: 0 };
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
            assert!(match agent.phase {
                NodePhase::Closed | NodePhase::Paused(_) | NodePhase::Lost => true,
                NodePhase::Starting { idle_since, .. } => idle_since == Some(self.now),
                _ => false,
            }, "{id:?} has no session yet is {:?}", agent.phase);
        }
    }

    /// A delivery the user replied to is away only while its node attends to the reply: once the
    /// node ends a turn, pauses, loses its session or closes, it is back in front of the user.
    fn replied_and_away(&self, agent: &AgentId) -> bool {
        self.open_task(agent).and_then(|task| self.s.task_handed_back(&task)) == Some(Instruction::Reply)
    }

    fn advance(&mut self, by: Millis) {
        self.now += if self.gaps.is_empty() { by } else { self.gaps[self.moves % self.gaps.len()] };
        self.moves += 1;
    }

    /// What every node is and what waits on the user, without a single time in it.
    fn shape(&self) -> (Vec<(AgentId, &'static str, bool)>, BTreeSet<(AgentId, InteractionId)>) {
        let seen = |phase: &NodePhase| match phase {
            NodePhase::Starting { idle_since, .. } | NodePhase::Resuming { idle_since } => idle_since.is_some(),
            NodePhase::WaitingBackground(work) => work.gone.is_some(),
            _ => false,
        };
        (self.s.agents.values().map(|a| (a.id.clone(), a.phase.key(), seen(&a.phase))).collect(), self.unheard_cards())
    }

    fn apply(&mut self, step: &Step) {
        self.advance(1_000);
        if !matches!(step, Step::Replay) {
            self.last = None;
        }
        let ending = match step {
            Step::Turn { pick, start: false, late: false } => self.pick(*pick, |a, _| a.phase.live()),
            Step::Pause { pick, .. } => self.pick(*pick, |a, _| a.phase.holds_slot()),
            Step::Lost(n) | Step::UserCloses(n) => self.pick(*n, |a, _| a.phase.live()),
            _ => None,
        };
        let phase_before = ending.as_ref().and_then(|agent| self.s.node_phase(agent).cloned());
        let behind: Vec<AgentId> = self.s.agents.values().filter(|a| matches!(a.phase, NodePhase::WaitingBackground(Background { gone: None, .. }))).map(|a| a.id.clone()).collect();
        let heard_before = self.unheard_cards();
        self.apply_step(step);
        self.no_card_while_behind_own_work(step, &behind, &heard_before);
        if let Some(agent) = ending
            && self.s.node_phase(&agent).cloned() != phase_before
        {
            assert!(!self.replied_and_away(&agent), "{step:?} ended {agent:?}'s attention, yet its replied delivery stays away");
        }
    }

    fn apply_step(&mut self, step: &Step) {
        match step.clone() {
            Step::Launch => {
                self.nodes += 1;
                let (agent, task): (AgentId, TaskId) = (format!("node-{}", self.nodes).into(), format!("t-{}", self.nodes).into());
                self.must(Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None,
                    task_id: task.clone(), request_id: "r".into(), title: format!("Task {}", self.nodes), agent_id: None, review_prs: false,
                    dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
                });
                self.must(Actor::System, Command::RegisterAgent { agent_id: agent.clone(), name: format!("node {}", self.nodes), at: self.now });
                assert_eq!(self.s.node_phase(&agent), Some(&NodePhase::Starting { reviving: false, idle_since: None }));
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
            Step::Background { pick, ended: false } => {
                let Some(agent) = self.pick(pick, |a, _| a.phase.live()) else { return };
                let work = Background { work: crate::model::BackgroundKind::Shell, label: "make test".into(), since: self.now, others: 0, gone: None };
                if self.event(&agent, NodeEvent::TurnEndedBehind { work }, self.now) && self.real.contains_key(&agent) {
                    self.real.insert(agent, Listed::Idle);
                }
            }
            Step::Background { pick, ended: true } => {
                let Some(agent) = self.pick(pick, |a, _| matches!(a.phase, NodePhase::WaitingBackground(_))) else { return };
                assert!(self.event(&agent, NodeEvent::BackgroundEnded, self.now));
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
                        QuestionOption { id: "a".into(), label: "Atlas".into(), description: None },
                        QuestionOption { id: "b".into(), label: "Borealis".into(), description: None },
                    ],
                    card: CardSpec::new(CardKind::Decide, "Choose the release name").why("The name goes on the release"),
                });
            }
            Step::Answer(n) => {
                let Some(agent) = self.pick(n, |a, t| t.is_some_and(|t| a.phase.live() && !t.state.terminal())) else { return };
                let task = self.open_task(&agent).unwrap();
                let Some(card) = self.s.open_user_card(&task) else { return };
                let option = match &card.payload {
                    InteractionPayload::Question { options, .. } | InteractionPayload::ActionRequest { options, .. } => options[0].id.clone(),
                    _ => unreachable!("open_user_card is a question or a request"),
                };
                let card = card.id.clone();
                self.must(user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card, option_id: option, inputs: BTreeMap::new() });
            }
            Step::Say(n) => {
                let Some(agent) = self.pick(n, |a, _| a.phase.live()) else { return };
                let said = format!("Said at {}", self.now);
                self.must(Actor::System, Command::RecordLastWords { agent_id: agent.clone(), text: said.clone() });
                assert_eq!(self.s.last_words.get(&agent), Some(&said));
            }
            Step::Blocked(n) => {
                let Some(agent) = self.pick(n, |a, _| self.real.contains_key(&a.id)) else { return };
                self.real.insert(agent, Listed::Blocked);
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
            Step::Reply(n) => {
                let Some(agent) = self.pick(n, |_, t| t.is_some_and(|t| t.state == TaskState::ForYourReview)) else { return };
                let task = self.open_task(&agent).unwrap();
                let presentation = self.s.tasks[&task].final_presentation.clone().unwrap();
                self.must(user(), Command::SubmitFeedback { attachments: vec![], presentation_id: presentation.clone(),
                    targets: vec![DeliveryTarget::Text { text: "Done".into() }], body: "Why this way?".into(), instruction: Instruction::Reply });
                let attends = self.s.node_attends(&self.s.interactions[&presentation]);
                assert_eq!(self.replied_and_away(&agent), attends, "a reply takes the delivery away exactly when its node can act on it");
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
                assert_eq!(self.s.node_phase(&agent), Some(&NodePhase::Starting { reviving: true, idle_since: None }));
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
                let Some(agent) = self.pick(n, |_, t| t.is_some_and(|t| !t.over())) else { return };
                let task = self.open_task(&agent).unwrap();
                self.must(user(), Command::CancelTask { task_id: task });
            }
            Step::Tick(by) => self.advance(by),
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
        let index = crate::domain::ReadIndex::build(&self.s);
        crate::views::build(key, RenderContext {
            domain: &self.s, index: &index, live: &LiveSnapshot::default(), client: Client::Mac, lang: Lang::En,
            transport: Transport::Loopback, cursor: "", rendered_at: "",
        }).unwrap()
    }

    fn unheard_cards(&self) -> BTreeSet<(AgentId, InteractionId)> {
        self.s.interactions.values().filter(|card| card.id.0.starts_with(UNHEARD_PREFIX) && self.s.open(card))
            .filter_map(|card| match &card.author { Author::Agent { agent_id } => Some((agent_id.clone(), card.id.clone())), _ => None })
            .collect()
    }

    /// David's rule, 15 October: a node waiting for work of its own is at work. No step opens the
    /// card with its last words while that work is not seen gone, unless a turn of its own ended.
    fn no_card_while_behind_own_work(&self, step: &Step, behind: &[AgentId], before: &BTreeSet<(AgentId, InteractionId)>) {
        for (agent, card) in self.unheard_cards().difference(before) {
            let ended_its_wait = self.last.as_ref().is_some_and(|(moved, event, _)| moved == agent
                && matches!(event, NodeEvent::TurnEnded | NodeEvent::TurnFailed));
            assert!(!behind.contains(agent) || ended_its_wait, "{step:?} opened {card:?} while {agent:?} waited for its own work");
        }
    }

    /// David's rule, 14 October: a node whose task is open is at work, or waits on a card in front
    /// of the user. Waiting for the machine, for its session to come back or for another task are
    /// facts of their own; resting with nothing for anyone is no state at all.
    fn no_node_rests_unheard(&self) {
        for agent in self.s.agents.values() {
            let Some(task) = self.s.node_task(&agent.id).filter(|t| !t.state.terminal() && !t.archived && t.state != TaskState::Closing) else {
                continue;
            };
            let facts = self.s.task_facts(&agent.id);
            let asks = self.s.open_user_card(&task.id).is_some() || facts.held;
            match &agent.phase {
                NodePhase::Starting { .. } | NodePhase::Working | NodePhase::WaitingBackground(_) | NodePhase::Resuming { .. }
                | NodePhase::Delivered { in_turn: true } | NodePhase::Paused(_) | NodePhase::Lost | NodePhase::Closing | NodePhase::Closed => {}
                NodePhase::WaitingUser { .. } => assert!(asks, "{} waits on the user with no card in front of them", agent.id.0),
                NodePhase::Delivered { in_turn: false } => assert!(facts.delivered, "{} rests delivered with no result in front of the user", agent.id.0),
                NodePhase::WaitingDependency => assert_eq!(task.state, TaskState::WaitingDependency, "{} waits for a task its own does not wait for", agent.id.0),
                NodePhase::NoTask => panic!("{} rests as if it had no task while {} is open", agent.id.0, task.id.0),
            }
        }
    }

    /// After every step: one valid state per node, and the gate, Fleet, the Tasks list and the
    /// header count the same nodes at work.
    fn check(&self) {
        self.s.validate_nodes().unwrap();
        self.no_node_rests_unheard();
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
            .filter(|a| !matches!(a.phase, NodePhase::Delivered { .. }) && !(a.phase.coming_back() && self.s.task_facts(&a.id).delivered))
            .filter_map(|a| self.s.node_task(&a.id).filter(|t| !t.state.terminal()).map(|t| t.id.0.clone())).collect();
        let at_work = ["Working", "Reconnecting", "Making changes", "Resolving conflicts", "Answering you"];
        let listed = badged(&tasks, &at_work);
        assert_eq!(listed, task_slots, "the Tasks list says at work of the tasks whose node holds a slot");
        let paused: BTreeSet<String> = self.s.paused_nodes().keys()
            .filter_map(|agent| self.s.node_task(agent).filter(|t| !t.state.terminal()).map(|t| t.id.0.clone())).collect();
        for view in [&tasks, &self.render("sidebar"), &fleet] {
            let working = badged(view, &at_work);
            assert!(working.is_disjoint(&paused), "{} says a task whose node is paused is at work: {:?}", view.view, working.intersection(&paused));
        }

        let header = fleet.status.counts.iter().find(|count| count.id == "working").map(|count| count.value as usize);
        assert_eq!(header, Some(slots.len()), "the header counts the same nodes");

        let index = ReadIndex::build(&self.s);
        for task in for_you(&tasks) {
            assert!(index.task(&TaskId(task.clone())).is_some_and(|read| read.waits_for_user), "«For you» lists {task} only with a card waiting for the user");
            let detail = self.render(&format!("task:{task}"));
            assert!(detail.sections.iter().any(|section| section.id == "waiting" && !section.items.is_empty()),
                "the detail of {task} shows the card it waits on");
        }
        let asking: BTreeSet<String> = index.waiting().iter().filter(|item| item.kind != "information")
            .filter_map(|item| item.agent_id.as_ref()).filter(|agent| self.s.agents.contains_key(*agent))
            .map(|agent| agent.0.clone()).collect();
        let claims = badged(&fleet, &["Waiting for your review", "Waiting for an answer", "Your turn"]);
        assert!(claims.is_subset(&asking), "Fleet says {claims:?} wait for the user, only {asking:?} have a card");
        let waiting = fleet.status.counts.iter().find(|count| count.id == "waiting").map(|count| count.value as usize);
        assert_eq!(waiting, Some(asking.len()), "the header counts the nodes whose card waits");
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
    TaskFacts { open: !t.state.terminal() && !t.archived, closing, held, card: asked || s.open_user_card(&t.id).is_some(), delivered,
        awaits: t.state == TaskState::WaitingDependency }
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

/// The tasks the Tasks list puts under «For you».
fn for_you(view: &View) -> Vec<String> {
    fn tasks(rows: &[crate::views::Row], out: &mut Vec<String>) {
        for row in rows {
            if row.link.is_some() && row.id.starts_with("t-") {
                out.push(row.id.clone());
            }
            tasks(&row.children, out);
        }
    }
    let mut out = vec![];
    for item in view.sections.iter().flat_map(|section| &section.items) {
        if let Item::Row(block) = item && block.id == "block:for-you" {
            tasks(&block.children, &mut out);
        }
    }
    out
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

/// 14 October, t-holepunch-iroh-prototype, replayed: the node starts a listener of its own in the
/// background, ends its turn writing to David with no card, its mod says a turn started, and
/// Claude Code lists it blocked. Its words end in a card for David, in his language, and the
/// node waits on it; no screen says «Sin actividad».
#[test]
fn the_14_october_incident_ends_with_the_node_s_words_on_a_card_for_the_user() {
    let mut w = World::new();
    w.apply(&Step::Launch);
    let (agent, task): (AgentId, TaskId) = ("node-1".into(), "t-1".into());
    assert!(w.event(&agent, NodeEvent::TurnStarted, w.now));
    let said = "Both runs arrived but are no good: the fault was in my setup. Repeat the one on mobile data.";
    w.must(Actor::System, Command::RecordLastWords { agent_id: agent.clone(), text: said.into() });
    let listener = Background { work: crate::model::BackgroundKind::Shell, label: "listen-at-home.sh".into(), since: w.now, others: 0, gone: None };
    assert!(w.event(&agent, NodeEvent::TurnEndedBehind { work: listener }, w.now));
    assert!(matches!(w.s.node_phase(&agent), Some(NodePhase::WaitingBackground(_))));
    w.now += 60_000;
    assert!(w.event(&agent, NodeEvent::TurnStarted, w.now), "its mod says a turn started");
    w.real.insert(agent.clone(), Listed::Blocked);
    w.now += 60_000;
    w.reconcile();
    assert_eq!(w.s.node_phase(&agent), Some(&NodePhase::WaitingUser { in_turn: false }));
    let card = w.s.open_user_card(&task).expect("its words wait for the user").clone();
    assert!(card.id.0.starts_with(UNHEARD_PREFIX));
    assert_eq!((card.body.as_str(), card.card.why.as_str()), (said, said), "its words are on the card itself, not behind a tap");
    assert!(card.card.content.is_none(), "words that fit on the card need no detail");
    w.check();

    let index = crate::domain::ReadIndex::build(&w.s);
    let render = |key: &str| serde_json::to_string(&crate::views::build(key, RenderContext {
        domain: &w.s, index: &index, live: &LiveSnapshot::default(), client: Client::Ios, lang: Lang::Es,
        transport: Transport::Loopback, cursor: "", rendered_at: "",
    }).unwrap()).unwrap();
    let shown = render(&format!("card:{}", card.id));
    for wanted in ["«Task 1» se ha parado y te escribe", "Repeat the one on mobile data", "Que siga"] {
        assert!(shown.contains(wanted), "{wanted}: {shown}");
    }
    for key in ["tasks", "fleet", "task:t-1", "node:node-1"] {
        let text = render(key);
        assert!(!text.contains("Sin actividad") && !text.contains("Detenido"), "{key}: {text}");
        assert!(!text.contains("stopped and wrote to you"), "{key} says the card in English: {text}");
    }
    let tasks = render("tasks");
    assert!(tasks.contains(&format!("«{said}»")), "the task's row says what its node wrote: {tasks}");

    w.must(user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.id.clone(), option_id: CARRY_ON.into(), inputs: BTreeMap::new() });
    assert_eq!(w.s.node_phase(&agent), Some(&NodePhase::Resuming { idle_since: None }), "the answer is on its way to it");
}

/// Words longer than a card's why keep their start on the card and the whole text in its detail.
#[test]
fn long_last_words_keep_their_start_on_the_card_and_the_whole_in_its_detail() {
    let mut w = World::new();
    w.apply(&Step::Launch);
    let agent: AgentId = "node-1".into();
    assert!(w.event(&agent, NodeEvent::TurnStarted, w.now));
    let said = "Both runs arrived. ".repeat(20);
    w.must(Actor::System, Command::RecordLastWords { agent_id: agent.clone(), text: said.clone() });
    assert!(w.event(&agent, NodeEvent::TurnEnded, w.now));
    let card = w.s.open_user_card(&"t-1".into()).expect("its words wait for the user").clone();
    assert!(card.card.why.chars().count() <= CardSpec::WHY_MAX && said.starts_with(card.card.why.trim_end_matches('…')), "{}", card.card.why);
    assert_eq!(card.card.content.map(|content| content.body), Some(said.trim().to_owned()));
}

/// David's rule, 15 October: a node's state moves only on facts, never on how long it has been.
/// The same history of facts on a clock that moves a millisecond a step and on one that moves up
/// to two days leaves every node in the same state, step by step, with the same cards.
#[test]
fn the_same_facts_on_any_clock_move_every_node_alike() {
    let mut runner = TestRunner::new_with_rng(Config { cases: 256, failure_persistence: None, ..Config::default() },
        TestRng::deterministic_rng(RngAlgorithm::ChaCha));
    let history = (prop::collection::vec(step(), 1..60), prop::collection::vec(1 as Millis..48 * HOUR, 1..16));
    let result = runner.run(&history, |(steps, gaps)| {
        let (mut fast, mut slow) = (World::timed(vec![1]), World::timed(gaps));
        for step in &steps {
            fast.apply(step);
            slow.apply(step);
            prop_assert_eq!(fast.shape(), slow.shape(), "after {:?}", step);
        }
        Ok(())
    });
    if let Err(TestError::Fail(reason, history)) = result {
        panic!("{reason}\nhistory: {history:#?}");
    }
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
