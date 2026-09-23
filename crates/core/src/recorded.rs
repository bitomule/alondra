//! Recorded reality: real event sequences from the log of 23 Sep, cut with `tools/fixture.py`
//! and stripped of every text, replayed through the reducer.

use serde::Deserialize;

use crate::event::Event;
use crate::model::{Millis, NodeState};
use crate::states;
use crate::view::View;

#[derive(Deserialize)]
struct Recorded {
    seq: u64,
    at: Millis,
    event: Event,
}

pub const AUDITORIA: &str = include_str!("../fixtures/auditoria_jev_alondra.jsonl");
pub const COLMENA: &str = include_str!("../fixtures/servidor_colmena.jsonl");
pub const QUILL: &str = include_str!("../fixtures/quill.jsonl");
pub const CTRL_TRIVIAL: &str = include_str!("../fixtures/ctrl_trivial_2.jsonl");

const AUDITORIA_SID: &str = "bea82247-f71b-4d3e-b815-a082dbea985f";
const COLMENA_SID: &str = "a3934428-f43b-45b1-974d-8cb023b45e23";
const QUILL_SID: &str = "11f98ac0-771f-4725-88f5-7e5176ffff92";
const CTRL_TRIVIAL_SID: &str = "128f3fa5-71f3-4376-8948-6ee1cf147c22";

const MINUTE: Millis = 60_000;

fn records(fixture: &str) -> Vec<Recorded> {
    fixture.lines().map(|l| serde_json::from_str(l).expect("fixture line")).collect()
}

/// The state after every event, plus a sweep every minute in between, as the daemon would
/// record them: an `Event::Clock` only when time alone changed something.
fn replay(fixture: &str, sid: &str) -> (View, Vec<(u64, Millis, NodeState)>) {
    let mut view = View::default();
    let mut states = vec![];
    let mut seq = 0;
    let record = |view: &View, seq: u64, at: Millis, states: &mut Vec<(u64, Millis, NodeState)>| {
        if let Some(n) = view.nodes.get(sid)
            && states.last().is_none_or(|(_, _, s)| *s != n.state)
        {
            states.push((seq, at, n.state));
        }
    };
    let mut last_at = None;
    for r in records(fixture) {
        if let Some(mut t) = last_at {
            while t + MINUTE < r.at {
                t += MINUTE;
                if view.due(t, false) {
                    view.apply(seq, t, &Event::Clock { machine_strained: false });
                    record(&view, seq, t, &mut states);
                }
                assert!(states::limbo(&view, t).is_empty(), "limbo at {t}: {:?}", states::limbo(&view, t));
            }
        }
        seq = r.seq;
        view.apply(r.seq, r.at, &r.event);
        record(&view, r.seq, r.at, &mut states);
        last_at = Some(r.at);
    }
    (view, states)
}

/// The state the node shows at `seq`, sweeps included.
fn at_seq(states: &[(u64, Millis, NodeState)], seq: u64) -> NodeState {
    states.iter().rev().find(|(s, _, _)| *s <= seq).map(|(_, _, st)| *st).expect("node seen by then")
}

/// The states the node went through between two events, sweeps included.
fn between(states: &[(u64, Millis, NodeState)], from: u64, to: u64) -> Vec<NodeState> {
    let mut v = vec![at_seq(states, from)];
    v.extend(states.iter().filter(|(s, _, _)| *s > from && *s < to).map(|(_, _, st)| *st));
    v
}

#[test]
fn every_fixture_ends_every_sweep_without_limbo() {
    for fixture in [AUDITORIA, COLMENA, QUILL, CTRL_TRIVIAL] {
        let (mut view, _) = replay(fixture, "");
        let end = records(fixture).last().unwrap().at;
        for later in [MINUTE, 10 * MINUTE, 60 * MINUTE, 6 * 60 * MINUTE] {
            view.apply(view.seq, end + later, &Event::Clock { machine_strained: false });
            assert!(states::limbo(&view, end + later).is_empty());
        }
    }
}

#[test]
fn a_restart_changes_nothing() {
    for fixture in [AUDITORIA, COLMENA, QUILL, CTRL_TRIVIAL] {
        let all = records(fixture);
        let mut full = View::default();
        let mut resumed = View::default();
        for (i, r) in all.iter().enumerate() {
            full.apply(r.seq, r.at, &r.event);
            if i == all.len() / 2 {
                let snapshot = serde_json::to_string(&resumed).unwrap();
                resumed = serde_json::from_str(&snapshot).unwrap();
            }
            resumed.apply(r.seq, r.at, &r.event);
        }
        assert_eq!(full, resumed);
    }
}

/// hive/auditoria-jev-alondra showed "working" for hours: its `ask` (question 31) was withdrawn
/// at 10:12 and it had stopped at 10:08, yet "asked" kept mapping to working.
#[test]
fn auditoria_an_ask_that_was_closed_explains_nothing() {
    let (_, states) = replay(AUDITORIA, AUDITORIA_SID);
    assert_eq!(at_seq(&states, 1715), NodeState::Unclear, "stopped at 10:08:49 with nothing declared");
    assert_eq!(at_seq(&states, 1720), NodeState::WaitingAnswer, "asked question 31 at 10:09:03");
    assert!(between(&states, 1823, 4206).iter().all(|s| *s == NodeState::Unclear), "withdrawn at 10:12, idle until 13:46");
}

/// Its final report at 13:46:50 was contradicted two seconds after the turn ended: the Claude
/// daemon wrote that it was blocked on a decision.
#[test]
fn auditoria_a_final_report_the_daemon_contradicts_is_not_done() {
    let (_, states) = replay(AUDITORIA, AUDITORIA_SID);
    assert_eq!(at_seq(&states, 4220), NodeState::Done, "Stop after the final report");
    assert_eq!(at_seq(&states, 4222), NodeState::Unclear, "daemon: blocked, needs a decision");
}

/// At 13:48:01 and 14:09:18 it reported final and its turn went on until 13:48:09 and 14:09:25.
#[test]
fn auditoria_a_final_report_while_the_turn_goes_on_is_working() {
    let (_, states) = replay(AUDITORIA, AUDITORIA_SID);
    assert_eq!(at_seq(&states, 4282), NodeState::Working);
    assert_eq!(at_seq(&states, 4285), NodeState::Done);
    assert_eq!(at_seq(&states, 4695), NodeState::Working);
    assert_eq!(between(&states, 4697, 4772), vec![NodeState::Done]);
}

/// hive/servidor-colmena showed "waiting on ExitPlanMode" long after: the plan's PreToolUse at
/// 12:11:26 was never followed by any hook.
#[test]
fn colmena_a_tool_nobody_closed_stops_counting() {
    let (view, states) = replay(COLMENA, COLMENA_SID);
    assert_eq!(at_seq(&states, 2300), NodeState::WaitingAnswer, "plan 39 open");
    assert_eq!(at_seq(&states, 2312), NodeState::Working, "plan approved; the daemon says active");
    assert_eq!(at_seq(&states, 2532), NodeState::Unclear, "the daemon says idle at 12:42");
    assert!(view.nodes[COLMENA_SID].tool.as_ref().is_none_or(|t| t.name != "ExitPlanMode"));
}

/// ios27/quill declared `wait_for_machine` and then got woken without anyone writing to it;
/// every wake ended its turn again and the old state put it in the sentinel's hands, which
/// nudged it four times in 40 s.
#[test]
fn quill_a_declared_wait_survives_wakes_nobody_sent() {
    let (_, states) = replay(QUILL, QUILL_SID);
    for stop in [3429, 3436, 3442, 3455] {
        assert_eq!(at_seq(&states, stop), NodeState::WaitingMachine, "turn ended at seq {stop}");
    }
}

/// hive/corpus-router/ctrl-trivial~2: dormant, no plugin, never active. Each daemon restart
/// observed it afresh and the sentinel, which kept its memory in RAM, posted the same
/// "Finished" report five times.
#[test]
fn ctrl_trivial_one_report_settles_a_dormant_node_for_good() {
    let (_, states) = replay(CTRL_TRIVIAL, CTRL_TRIVIAL_SID);
    assert_eq!(at_seq(&states, 2930), NodeState::Unclear, "dormant, idle, no plugin: the sentinel's");
    assert!(
        between(&states, 2966, 4439).iter().all(|s| matches!(s, NodeState::Delivered | NodeState::Done)),
        "delivered from the first report on, done once the user read it"
    );
}
