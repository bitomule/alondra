use std::sync::Arc;
use std::time::Duration;
use alondra_core::{Event, NodeState, now_millis, orchestrator::Transition};
use crate::hub::Hub;

const DIGEST_EVERY: Duration = Duration::from_secs(90);
const LOW_MEMORY_PERCENT: u32 = 15;
const HIGH_LOAD: f64 = 50.0;

/// Checkpoints and inbox messages commit together. Restarting the daemon cannot skip a
/// question/report seen immediately before a crash or enqueue that same digest twice.
fn enqueue(hub: &Hub, leftovers: &[(String, i64, String)]) -> anyhow::Result<()> {
    let machine = hub.machine();
    let alarm = (machine.memory_free_percent.is_some_and(|p| p < LOW_MEMORY_PERCENT) || machine.load[0] > HIGH_LOAD)
        .then(|| "machine_overloaded".to_string());
    hub.append_all(|v| {
        let Some(runtime) = &v.orchestrator else { return Ok::<_, ()>(vec![]); };
        let Some(orchestrator) = v.orchestrator_destination() else { return Ok(vec![]); };
        let mut lines = vec![];
        let now = now_millis();
        let watched = |session: &str| v.nodes.get(session).is_some_and(|n| alondra_core::states::watching(v, n, now));
        for r in v.reports.range(runtime.last_report.saturating_add(1)..).map(|(_, r)| r)
            .filter(|r| r.session_id != orchestrator && !watched(&r.session_id)) {
            lines.push(format!("- {} #{} from {}: {}", if r.to_orchestrator { "Handoff" } else { "Report" }, r.id, r.node_name.clone().unwrap_or_default(), r.title));
        }
        for q in v.questions.range(runtime.last_question.saturating_add(1)..).map(|(_, q)| q).filter(|q| q.to_orchestrator && q.status.is_open()) {
            lines.push(format!("- Question #{} for you from {}: {}", q.id, q.node_name.clone().unwrap_or_default(), q.title));
        }
        for (session, since, text) in leftovers {
            let key = format!("[leftovers:{session}:{since}]");
            if !v.messages.values().any(|m| m.session_id == orchestrator && m.text.contains(&key)) {
                lines.push(format!("- {key} {text}"));
            }
        }
        if alarm.is_some() && runtime.machine_alarm != alarm {
            lines.push(format!("- Machine at its limit: {}% memory free, load {:.0}. Do not launch new nodes until it goes down.", machine.memory_free_percent.unwrap_or(0), machine.load[0]));
        }
        let last_report = v.reports.keys().next_back().copied().unwrap_or(runtime.last_report).max(runtime.last_report);
        let last_question = v.questions.keys().next_back().copied().unwrap_or(runtime.last_question).max(runtime.last_question);
        let mut events = vec![];
        if !lines.is_empty() {
            if let Some(message) = v.alondra_message(orchestrator, format!("[Alondra] Fleet updates:\n{}", lines.join("\n")), now) {
                events.push(Event::MessageQueued(message));
            } else { return Ok(vec![]); }
        }
        if last_report != runtime.last_report || last_question != runtime.last_question || alarm != runtime.machine_alarm {
            events.push(Event::Orchestrator(Transition::NotificationsQueued { last_report, last_question, machine_alarm: alarm }));
        }
        Ok(events)
    })?;
    Ok(())
}

pub async fn run(hub: Arc<Hub>) {
    loop {
        tokio::time::sleep(DIGEST_EVERY).await;
        let finished: Vec<(String, String, i64)> = hub.read(|v| v.nodes.values()
            .filter(|n| n.in_daemon && !n.is_orchestrator && matches!(n.state, NodeState::Done | NodeState::Delivered))
            .map(|n| (n.session_id.clone(), n.display_name().to_string(), n.state_since)).collect());
        let mut leftovers = vec![];
        for (session, name, since) in finished {
            if let Some(left) = crate::worktree::leftovers(&session).await {
                leftovers.push((session, since, format!("{name} looks finished but its worktree has {left}: send it back to commit and push, or ask the user.")));
            }
        }
        if let Err(error) = enqueue(&hub, &leftovers) { log!("could not enqueue orchestrator digest: {error}"); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durable_digest_survives_restart_without_losing_or_repeating_reports() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!("alondra-notifier-{}-{}", std::process::id(), now_millis()));
        let config = alondra_core::config::Config::resolve(home.clone(), &Default::default(), |_|None, ||Some("Ana".into()));
        let hub = Hub::open(config.clone()).unwrap();
        hub.append(Event::Orchestrator(Transition::Enabled {session_id:None})).unwrap();
        hub.append(Event::ReportPosted(alondra_core::Report { id:1, session_id:"worker".into(), node_name:Some("worker".into()), title:"Result delivered".into(), body:None, links:vec![], attachments:vec![], kind:alondra_core::ReportKind::Final, created_at:now_millis(), read:false, to_orchestrator:true, mission:None, looks_like_question:false, task:None })).unwrap();
        enqueue(&hub, &[]).unwrap();
        assert_eq!(hub.read(|v|v.pending_messages(alondra_core::orchestrator::NODE_ID).count()),1);
        drop(hub);
        let reopened = Hub::open(config).unwrap();
        let seq = reopened.read(|v|v.seq);
        enqueue(&reopened, &[]).unwrap();
        assert_eq!(reopened.read(|v|v.seq),seq);
        assert_eq!(reopened.read(|v|v.orchestrator.as_ref().unwrap().last_report),1);
        drop(reopened); std::fs::remove_dir_all(home).unwrap();
    }
    /// On 27-sep the migrated orchestrator started from report 0 and got #1-#265 again as news.
    #[test]
    fn enabling_the_orchestrator_does_not_resend_reports_it_already_had() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(crate::orchestrator_process::uuid().unwrap());
        let config = alondra_core::config::Config::resolve(home.clone(), &Default::default(), |_| None, || Some("Test".into()));
        let hub = Hub::open(config).unwrap();
        let report = |id, title: &str| alondra_core::Report { id, session_id: "worker".into(), node_name: Some("worker".into()), title: title.into(), body: None, links: vec![], attachments: vec![], kind: alondra_core::ReportKind::Final, created_at: 1, read: false, to_orchestrator: false, mission: None, looks_like_question: false, task: None };
        for title in ["Old one", "Old two"] {
            hub.append_with(|v| Some(Event::ReportPosted(report(v.next_report_id(), title)))).unwrap();
        }
        hub.append(Event::Orchestrator(Transition::Enabled { session_id: None })).unwrap();
        enqueue(&hub, &[]).unwrap();
        assert_eq!(hub.read(|v| v.pending_messages(alondra_core::orchestrator::NODE_ID).count()), 0);
        hub.append_with(|v| Some(Event::ReportPosted(report(v.next_report_id(), "New one")))).unwrap();
        enqueue(&hub, &[]).unwrap();
        let digests: Vec<String> = hub.read(|v| v.pending_messages(alondra_core::orchestrator::NODE_ID).map(|m| m.text.clone()).collect());
        assert!(digests.len() == 1 && digests[0].contains("#3") && !digests[0].contains("Old"), "{digests:?}");
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn report_ids_and_handoffs_advance_after_retention_and_snapshot_restart() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(crate::orchestrator_process::uuid().unwrap());
        let config = alondra_core::config::Config::resolve(home.clone(), &Default::default(), |_| None, || Some("Test".into()));
        let hub = Hub::open(config.clone()).unwrap();
        hub.append(Event::Orchestrator(Transition::Enabled { session_id: None })).unwrap();
        let report = |id, title: &str| alondra_core::Report { id, session_id: "worker".into(), node_name: Some("worker".into()), title: title.into(), body: None, links: vec![], attachments: vec![], kind: alondra_core::ReportKind::Final, created_at: 1, read: false, to_orchestrator: true, mission: None, looks_like_question: false, task: None };
        hub.append_with(|v| Some(Event::ReportPosted(report(v.next_report_id(), "First handoff")))).unwrap();
        enqueue(&hub, &[]).unwrap();
        hub.append(Event::ReportRead { id: 1 }).unwrap();
        hub.compact(Duration::ZERO).unwrap();
        assert!(hub.read(|v| v.reports.is_empty()));
        drop(hub);
        let hub = Hub::open(config).unwrap();
        assert_eq!(hub.read(|v| v.next_report_id()), 2);
        hub.append_with(|v| Some(Event::ReportPosted(report(v.next_report_id(), "Second handoff")))).unwrap();
        enqueue(&hub, &[]).unwrap();
        hub.read(|v| {
            assert_eq!(v.orchestrator.as_ref().unwrap().last_report, 2);
            assert_eq!(v.pending_messages(alondra_core::orchestrator::NODE_ID).filter(|m| m.text.contains("Second handoff")).count(), 1);
        });
        let seq = hub.read(|v| v.seq);
        enqueue(&hub, &[]).unwrap();
        assert_eq!(hub.read(|v| v.seq), seq);
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }
}
