use std::collections::HashSet;
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
    let mut unclear_told: HashSet<(String, i64)> = HashSet::new();
    let mut machine_alarm = false;
    loop {
        tokio::time::sleep(DIGEST_EVERY).await;
        let machine = hub.machine();
        let digest = hub.read(|v| {
            let orchestrator = v.nodes.values().find(|n| n.is_orchestrator)?.session_id.clone();
            let mut lines = vec![];
            for r in v.reports.range(last_report + 1..).map(|(_, r)| r).filter(|r| r.session_id != orchestrator) {
                lines.push(format!("- Informe #{} de {}: {}", r.id, r.node_name.clone().unwrap_or_default(), r.title));
            }
            for q in v.questions.range(last_question + 1..).map(|(_, q)| q).filter(|q| q.to_orchestrator && q.status.is_open()) {
                lines.push(format!("- Pregunta #{} para ti de {}: {}", q.id, q.node_name.clone().unwrap_or_default(), q.title));
            }
            for n in v.nodes.values().filter(|n| n.state == NodeState::Unclear && !n.is_orchestrator && n.in_daemon) {
                if unclear_told.contains(&(n.session_id.clone(), n.state_since)) {
                    continue;
                }
                let said: String = n
                    .last_message
                    .clone()
                    .or(n.daemon_detail.clone())
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect();
                lines.push(format!("- {} terminó sin decir qué quiere: «{said}»", n.display_name()));
            }
            Some((orchestrator, lines, v.reports.keys().next_back().copied().unwrap_or(0), v.questions.keys().next_back().copied().unwrap_or(0)))
        });
        let low_memory = machine.memory_free_percent.is_some_and(|p| p < LOW_MEMORY_PERCENT);
        let overloaded = machine.load[0] > HIGH_LOAD;
        let Some((orchestrator, mut lines, max_report, max_question)) = digest else { continue };
        last_report = max_report;
        last_question = max_question;
        hub.read(|v| {
            for n in v.nodes.values().filter(|n| n.state == NodeState::Unclear) {
                unclear_told.insert((n.session_id.clone(), n.state_since));
            }
        });
        if (low_memory || overloaded) && !machine_alarm {
            lines.push(format!(
                "- Máquina al límite: memoria {}% libre, carga {:.0}. No lances nodos nuevos hasta que baje.",
                machine.memory_free_percent.unwrap_or(0),
                machine.load[0]
            ));
        }
        machine_alarm = low_memory || overloaded;
        if lines.is_empty() {
            continue;
        }
        let text = format!("[Alondra] Novedades de la flota:\n{}", lines.join("\n"));
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
