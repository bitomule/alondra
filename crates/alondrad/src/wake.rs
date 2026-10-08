use std::time::Duration;

use alondra_core::{Event, now_millis, upkeep};

use crate::hub::Hub;

/// What Alondra asks Claude Code to do with a background session, so restarting one can run
/// against a fake.
pub trait Sessions: Sync {
    fn respawn(&self, short: &str) -> impl Future<Output = Result<(), String>> + Send;
}

/// Restarts the nodes whose process ended while they still had work and tells each one to carry
/// on; the orchestrator hears about those that could not be restarted, and about those that
/// kept dying. Nothing is restarted while the machine is strained: that may be why they died.
pub async fn resume_dead(hub: &Hub, sessions: &impl Sessions, now: i64, strained: bool) {
    let user = hub.config.name.clone();
    let spent: Vec<String> = hub.read(|v| v.nodes.values().filter(|n| n.resumed.as_ref().is_some_and(|r| r.streak >= upkeep::MAX_RESUMES)).map(|n| n.session_id.clone()).collect());
    for session_id in spent {
        if let Err(e) = hub.append_with(|v| v.nodes.get(&session_id).and_then(|n| upkeep::resumes_given_up_message(v, n, now)).map(Event::MessageQueued)) {
            log!("could not tell the orchestrator about a node that keeps stopping: {e}");
        }
    }
    if strained {
        return;
    }
    let due: Vec<(String, String)> = hub.read(|v| {
        v.nodes
            .values()
            .filter(|n| !hub.is_closing(&n.session_id) && upkeep::resume_due(v, n, now).is_some())
            .map(|n| (n.session_id.clone(), n.short_id.clone()))
            .collect()
    });
    for (session_id, short) in due {
        let error = sessions.respawn(&short).await.err();
        match &error {
            None => log!("restarted {short}: its process ended while it still had work"),
            Some(e) => log!("could not restart {short}: {e}"),
        }
        let ok = error.is_none();
        if let Err(e) = hub.append(Event::Resumed { session_id: session_id.clone(), ok, error }) {
            log!("could not record the restart of {short}: {e}");
            continue;
        }
        if ok && let Err(e) = hub.append_with(|v| upkeep::resumed_message(v, &session_id, now).map(Event::MessageQueued)) {
            log!("could not tell {short} it was restarted: {e}");
        }
    }
    let failed: Vec<String> = hub.read(|v| v.nodes.values().filter(|n| n.resumed.as_ref().is_some_and(|r| !r.ok)).map(|n| n.session_id.clone()).collect());
    for session_id in failed {
        if let Err(e) = hub.append_with(|v| v.nodes.get(&session_id).and_then(|n| upkeep::resume_failed_message(v, n, &user, now)).map(Event::MessageQueued)) {
            log!("could not tell the orchestrator about a failed restart: {e}");
        }
    }
}

#[cfg(test)]
mod resume_tests {
    use super::*;
    use alondra_core::{DaemonObservation, HookEvent, MessageFrom};

    struct FakeClaude {
        refuses: Option<&'static str>,
        respawned: std::sync::Mutex<Vec<String>>,
    }

    impl FakeClaude {
        fn new(refuses: Option<&'static str>) -> Self {
            FakeClaude { refuses, respawned: Default::default() }
        }

        fn calls(&self) -> Vec<String> {
            self.respawned.lock().unwrap().clone()
        }
    }

    impl Sessions for FakeClaude {
        async fn respawn(&self, short: &str) -> Result<(), String> {
            self.respawned.lock().unwrap().push(short.to_string());
            self.refuses.map_or(Ok(()), |why| Err(why.to_string()))
        }
    }

    fn hub(_tag: &str) -> Hub {
        let directory = crate::test_support::Directory::new();
        let home = directory.to_path_buf();
        let config = alondra_core::config::Config::resolve(home, &Default::default(), |_| None, || Some("Ana".into()));
        Hub::open(config).unwrap().with_test_directory(directory)
    }

    fn seen(session: &str, state: &str, pid: Option<i32>) -> Event {
        let short = session.chars().take(8).collect();
        Event::DaemonSeen(DaemonObservation { session_id: session.into(), short_id: short, name: Some("n".into()), pid, state: Some(state.into()), ..Default::default() })
    }

    fn died(hub: &Hub, session: &str, pid: i32, after: &str) {
        hub.append(seen(session, "working", Some(pid))).unwrap();
        hub.append(Event::Hook(HookEvent { session_id: session.into(), event: "UserPromptSubmit".into(), ..Default::default() })).unwrap();
        let id = hub.read(|v| v.next_report_id());
        hub.append(Event::ReportPosted(alondra_core::Report {
            id, session_id: session.into(), node_name: None, title: "Halfway".into(), body: None, links: vec![], attachments: vec![], kind: alondra_core::ReportKind::Notice,
            created_at: now_millis(), read: false, to_orchestrator: false, mission: None, looks_like_question: false, task: None,
        }))
        .unwrap();
        hub.append(Event::ProcessExited { session_id: session.into(), pid }).unwrap();
        hub.append(seen(session, after, None)).unwrap();
    }

    fn alondra_said(hub: &Hub, session: &str, prefix: &str) -> usize {
        hub.read(|v| v.messages.values().filter(|m| m.session_id == session && m.from == MessageFrom::Alondra && m.text.starts_with(prefix)).count())
    }

    const MIN: i64 = 60_000;

    #[tokio::test]
    async fn a_node_that_died_with_work_is_respawned_once_and_told_to_carry_on() {
        let hub = hub("ok");
        died(&hub, "deadnode-1", 7, "working");
        died(&hub, "idlenode-1", 8, "done");
        died(&hub, "stopnode-1", 9, "stopped");
        let claude = FakeClaude::new(None);
        resume_dead(&hub, &claude, now_millis() + 2 * MIN, false).await;
        assert_eq!(claude.calls(), vec!["deadnode"], "only the one that died while working: not the one the daemon let go, nor the one someone stopped");
        assert_eq!(hub.read(|v| v.nodes["deadnode-1"].resumed.as_ref().map(|r| r.ok)), Some(true));
        assert_eq!(alondra_said(&hub, "deadnode-1", upkeep::RESUMED_PREFIX), 1);

        resume_dead(&hub, &claude, now_millis() + 3 * 3600 * 1000, false).await;
        assert_eq!(claude.calls().len(), 1, "once per exit");

        tokio::time::sleep(Duration::from_millis(5)).await;
        hub.append(seen("deadnode-1", "working", Some(9))).unwrap();
        hub.append(Event::ProcessExited { session_id: "deadnode-1".into(), pid: 9 }).unwrap();
        hub.append(seen("deadnode-1", "failed", None)).unwrap();
        resume_dead(&hub, &claude, now_millis() + 2 * MIN, false).await;
        assert_eq!(claude.calls().len(), 1, "died again within the backoff");
        resume_dead(&hub, &claude, now_millis() + upkeep::RESUME_BACKOFF_MS + MIN, false).await;
        assert_eq!(claude.calls().len(), 2, "past the backoff it is restarted again");
    }

    /// A few milliseconds after the last one, so each exit has its own time: restarts are one per exit.
    async fn died_again(hub: &Hub, session: &str, pid: i32) {
        tokio::time::sleep(Duration::from_millis(5)).await;
        hub.append(seen(session, "working", Some(pid))).unwrap();
        hub.append(Event::ProcessExited { session_id: session.into(), pid }).unwrap();
        hub.append(seen(session, "failed", None)).unwrap();
    }

    fn with_orchestrator(tag: &str) -> Hub {
        let hub = hub(tag);
        hub.append(Event::MarkedOrchestrator { session_id: "orchestr-1".into() }).unwrap();
        hub.append(Event::Hook(HookEvent { session_id: "orchestr-1".into(), event: "SessionStart".into(), ..Default::default() })).unwrap();
        hub
    }

    /// A node whose process kept dying was restarted every 15 min for good, and each restart
    /// queued one more "your process stopped" message for it.
    #[tokio::test]
    async fn a_node_that_keeps_dying_is_restarted_three_times_then_left_to_the_orchestrator() {
        let hub = with_orchestrator("streak");
        died(&hub, "deadnode-1", 7, "working");
        let claude = FakeClaude::new(None);
        let mut now = now_millis() + 2 * MIN;
        resume_dead(&hub, &claude, now, false).await;
        for pid in [8, 9, 10] {
            died_again(&hub, "deadnode-1", pid).await;
            now += upkeep::RESUME_BACKOFF_MS + MIN;
            resume_dead(&hub, &claude, now, false).await;
        }
        assert_eq!(claude.calls().len(), 3, "three restarts in a row, then no more");
        assert_eq!(alondra_said(&hub, "deadnode-1", upkeep::RESUMED_PREFIX), 1, "one carry-on message while it has not reached the node");
        assert_eq!(alondra_said(&hub, "orchestr-1", upkeep::RESUMES_GIVEN_UP_PREFIX), 1);
        resume_dead(&hub, &claude, now + upkeep::RESUME_BACKOFF_MS, false).await;
        assert_eq!((claude.calls().len(), alondra_said(&hub, "orchestr-1", upkeep::RESUMES_GIVEN_UP_PREFIX)), (3, 1), "told once");

        let turns = with_orchestrator("streak-reset");
        died(&turns, "deadnode-1", 7, "working");
        let claude = FakeClaude::new(None);
        let mut now = now_millis() + 2 * MIN;
        resume_dead(&turns, &claude, now, false).await;
        for pid in [8, 9, 10] {
            turns.append(Event::Hook(HookEvent { session_id: "deadnode-1".into(), event: "Stop".into(), ..Default::default() })).unwrap();
            died_again(&turns, "deadnode-1", pid).await;
            now += upkeep::RESUME_BACKOFF_MS + MIN;
            resume_dead(&turns, &claude, now, false).await;
        }
        assert_eq!(claude.calls().len(), 4, "a turn done between deaths starts the count again");
        for pid in [11, 12, 13] {
            died_again(&turns, "deadnode-1", pid).await;
            now += upkeep::RESUME_BACKOFF_MS + MIN;
            resume_dead(&turns, &claude, now, false).await;
        }
        assert_eq!(claude.calls().len(), 6, "three in a row from the last turn done, not from the first restart");
    }

    /// Restarting nodes while the machine is short of memory adds to what may have killed them.
    #[tokio::test]
    async fn nothing_is_restarted_while_the_machine_is_strained() {
        let hub = hub("strained");
        died(&hub, "deadnode-1", 7, "working");
        let claude = FakeClaude::new(None);
        resume_dead(&hub, &claude, now_millis() + 2 * MIN, true).await;
        assert!(claude.calls().is_empty());
        resume_dead(&hub, &claude, now_millis() + 2 * MIN, false).await;
        assert_eq!(claude.calls(), vec!["deadnode"], "once it has room again");
    }

    #[tokio::test]
    async fn a_node_alondra_is_closing_is_not_brought_back() {
        let hub = hub("closing");
        died(&hub, "deadnode-1", 7, "working");
        let claude = FakeClaude::new(None);
        let closing = hub.closing("deadnode-1");
        resume_dead(&hub, &claude, now_millis() + 2 * MIN, false).await;
        assert!(claude.calls().is_empty(), "claude rm stopped it on the way out");
        drop(closing);
        resume_dead(&hub, &claude, now_millis() + 2 * MIN, false).await;
        assert_eq!(claude.calls(), vec!["deadnode"], "a close that did not go through leaves it to be restarted");
    }

    #[tokio::test]
    async fn a_failed_respawn_is_not_retried_for_that_exit_and_reaches_the_orchestrator_once() {
        let hub = hub("fails");
        hub.append(Event::MarkedOrchestrator { session_id: "orchestr-1".into() }).unwrap();
        hub.append(Event::Hook(HookEvent { session_id: "orchestr-1".into(), event: "SessionStart".into(), ..Default::default() })).unwrap();
        died(&hub, "deadnode-1", 7, "working");
        let claude = FakeClaude::new(Some("No job matching deadnode"));
        resume_dead(&hub, &claude, now_millis() + 2 * MIN, false).await;
        resume_dead(&hub, &claude, now_millis() + 3 * MIN, false).await;
        assert_eq!(claude.calls(), vec!["deadnode"]);
        assert_eq!(hub.read(|v| v.nodes["deadnode-1"].resumed.clone().and_then(|r| r.error)).as_deref(), Some("No job matching deadnode"));
        assert_eq!(alondra_said(&hub, "orchestr-1", upkeep::RESUME_FAILED_PREFIX), 1);
        assert_eq!(alondra_said(&hub, "deadnode-1", upkeep::RESUMED_PREFIX), 0, "nothing to tell a node that is still down");
    }
}

