//! Alondra keeps an orchestrator alive: when none is running or dormant, alondrad launches one
//! and says so once it registers. Every decision here is derived from stored events, so a
//! restart neither launches twice nor forgets to announce.

use crate::model::{Millis, Node, Report, ReportKind};
use crate::view::View;

const MINUTE: Millis = 60 * 1000;
/// After alondrad starts, an existing orchestrator gets this long to show up before it counts as missing.
pub const STARTUP_GRACE_MS: Millis = 3 * MINUTE;
/// A launch that did not produce an orchestrator is retried after this long.
pub const RELAUNCH_AFTER_MS: Millis = 15 * MINUTE;
pub const ANNOUNCE_PREFIX: &str = "Alondra launched an orchestrator";

/// An orchestrator counts while it has a process or the Claude daemon still keeps its session,
/// since a message wakes a dormant one.
pub fn has_orchestrator(v: &View) -> bool {
    v.nodes.values().any(|n| n.is_orchestrator && (n.alive || n.in_daemon))
}

pub fn should_launch(v: &View, now: Millis, started_at: Millis) -> bool {
    !has_orchestrator(v) && now - started_at >= STARTUP_GRACE_MS && v.orchestrator_launched_at.is_none_or(|at| now - at >= RELAUNCH_AFTER_MS)
}

/// The orchestrator alondrad launched, once it has registered and before it has been announced.
pub fn to_announce(v: &View) -> Option<&Node> {
    let launched = v.orchestrator_launched_at?;
    let announced = v.reports.values().any(|r| r.created_at >= launched && r.title.starts_with(ANNOUNCE_PREFIX));
    if announced {
        return None;
    }
    v.nodes.values().find(|n| n.is_orchestrator && n.alive && n.last_seen >= launched)
}

pub fn announcement(v: &View, orchestrator: &Node, at: Millis) -> Report {
    Report {
        id: v.reports.keys().next_back().copied().unwrap_or(0) + 1,
        session_id: orchestrator.session_id.clone(),
        node_name: orchestrator.name.clone().or_else(|| Some(orchestrator.short_id.clone())),
        title: format!("{ANNOUNCE_PREFIX}: none was running"),
        body: Some(format!(
            "No orchestrator was running or dormant, so Alondra launched `{}` with the orchestrator plugin. Nodes' stalled turns, reports and questions for the orchestrator go to it from now on.",
            orchestrator.display_name()
        )),
        links: vec![],
        attachments: vec![],
        kind: ReportKind::Notice,
        created_at: at,
        read: false,
        mission: None,
    }
}

pub fn mission(user: &str) -> String {
    format!(
        "You are {user}'s orchestrator with Alondra. Alondra launched you because no orchestrator was running. \
         Load the alondra-orchestrator skill and follow it. Call `fleet` first; then send {user} one `report` \
         (kind notice) with only what needs their attention, most important first. Do not close nodes or merge \
         anything without asking {user} with `ask`."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{DaemonObservation, Event, HookEvent};

    const T0: Millis = 10_000_000;

    fn orchestrator(v: &mut View, seq: u64, at: Millis) {
        v.apply(seq, at, &Event::Hook(HookEvent { session_id: "o".into(), event: "SessionStart".into(), ..Default::default() }));
        v.apply(seq + 1, at, &Event::MarkedOrchestrator { session_id: "o".into() });
    }

    #[test]
    fn launches_only_when_none_is_running_and_not_twice_in_a_row() {
        let started = T0;
        let min = MINUTE;
        let mut dormant = View::default();
        orchestrator(&mut dormant, 1, T0);
        dormant.apply(3, T0, &Event::DaemonSeen(DaemonObservation { session_id: "o".into(), short_id: "o".into(), ..Default::default() }));
        dormant.apply(4, T0, &Event::ProcessExited { session_id: "o".into(), pid: 1 });
        let mut running = View::default();
        orchestrator(&mut running, 1, T0);
        let mut launched = View::default();
        launched.apply(1, T0 + 5 * min, &Event::OrchestratorLaunched { ok: true });

        let none = View::default();
        let cases: Vec<(&str, &View, Millis, bool)> = vec![
            ("none, right after alondrad started", &none, T0 + min, false),
            ("none, after the grace", &none, T0 + 4 * min, true),
            ("one running", &running, T0 + 60 * min, false),
            ("a dormant one the daemon keeps", &dormant, T0 + 60 * min, false),
            ("launched 5 min ago and not registered yet", &launched, T0 + 10 * min, false),
            ("launched 16 min ago and still none", &launched, T0 + 21 * min, true),
        ];
        for (what, v, now, launch) in cases {
            assert_eq!(should_launch(v, now, started), launch, "{what}");
        }
    }

    #[test]
    fn the_launched_orchestrator_is_announced_once_it_registers_and_only_once() {
        let mut v = View::default();
        v.apply(1, T0, &Event::OrchestratorLaunched { ok: true });
        assert!(to_announce(&v).is_none(), "not registered yet");
        orchestrator(&mut v, 2, T0 + 30_000);
        let o = to_announce(&v).expect("registered").clone();
        let r = announcement(&v, &o, T0 + 31_000);
        assert!(r.title.starts_with(ANNOUNCE_PREFIX));
        v.apply(4, T0 + 31_000, &Event::ReportPosted(r));
        assert!(to_announce(&v).is_none());
    }

    #[test]
    fn an_orchestrator_that_was_already_there_is_not_announced() {
        let mut v = View::default();
        orchestrator(&mut v, 1, T0);
        assert!(to_announce(&v).is_none());
    }
}
