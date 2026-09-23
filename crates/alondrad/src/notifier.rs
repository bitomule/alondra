use std::sync::Arc;
use std::time::Duration;

use alondra_core::{Event, Message, MessageFrom, NodeState, now_millis};

use crate::hub::Hub;

const DIGEST_EVERY: Duration = Duration::from_secs(90);
const LOW_MEMORY_PERCENT: u32 = 15;
const HIGH_LOAD: f64 = 50.0;

pub async fn run(hub: Arc<Hub>) {
    let (mut last_report, mut last_question) = hub.read(|v| {
        (
            v.reports.keys().next_back().copied().unwrap_or(0),
            v.questions.keys().next_back().copied().unwrap_or(0),
        )
    });
    let mut machine_alarm = false;
    let mut leftovers_told: std::collections::HashSet<(String, i64)> = std::collections::HashSet::new();
    loop {
        tokio::time::sleep(DIGEST_EVERY).await;
        let machine = hub.machine();
        let digest = hub.read(|v| {
            let orchestrator = v.nodes.values().find(|n| n.is_orchestrator)?.session_id.clone();
            let mut lines = vec![];
            for r in v.reports.range(last_report + 1..).map(|(_, r)| r).filter(|r| r.session_id != orchestrator) {
                lines.push(format!("- Report #{} from {}: {}", r.id, r.node_name.clone().unwrap_or_default(), r.title));
            }
            for q in v.questions.range(last_question + 1..).map(|(_, q)| q).filter(|q| q.to_orchestrator && q.status.is_open()) {
                lines.push(format!("- Question #{} for you from {}: {}", q.id, q.node_name.clone().unwrap_or_default(), q.title));
            }
            Some((orchestrator, lines, v.reports.keys().next_back().copied().unwrap_or(0), v.questions.keys().next_back().copied().unwrap_or(0)))
        });
        let low_memory = machine.memory_free_percent.is_some_and(|p| p < LOW_MEMORY_PERCENT);
        let overloaded = machine.load[0] > HIGH_LOAD;
        let Some((orchestrator, mut lines, max_report, max_question)) = digest else { continue };
        let finished: Vec<(String, String, i64)> = hub.read(|v| {
            v.nodes
                .values()
                .filter(|n| n.in_daemon && !n.is_orchestrator && matches!(n.state, NodeState::Done | NodeState::Delivered))
                .map(|n| (n.session_id.clone(), n.display_name().to_string(), n.state_since))
                .collect()
        });
        for (session, name, since) in finished {
            if leftovers_told.contains(&(session.clone(), since)) {
                continue;
            }
            leftovers_told.insert((session.clone(), since));
            if let Some(left) = crate::worktree::leftovers(&session).await {
                lines.push(format!("- {name} looks finished but its worktree has {left}: send it back to commit and push, or ask the user."));
            }
        }
        last_report = max_report;
        last_question = max_question;
        if (low_memory || overloaded) && !machine_alarm {
            lines.push(format!(
                "- Machine at its limit: {}% memory free, load {:.0}. Do not launch new nodes until it goes down.",
                machine.memory_free_percent.unwrap_or(0),
                machine.load[0]
            ));
        }
        machine_alarm = low_memory || overloaded;
        if lines.is_empty() {
            continue;
        }
        let text = format!("[Alondra] Fleet updates:\n{}", lines.join("\n"));
        let _ = hub.append_with(|v| {
            Some(Event::MessageQueued(Message {
                id: v.messages.keys().next_back().copied().unwrap_or(0) + 1,
                session_id: orchestrator.clone(),
                from: MessageFrom::Alondra,
                text: text.clone(),
                answers_question: None,
                created_at: now_millis(),
                handed_at: None,
                delivered_at: None,
            }))
        });
    }
}
