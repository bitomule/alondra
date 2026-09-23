use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alondra_core::view::derive;
use alondra_core::{Event, Message, MessageFrom, Node, NodeState, Question, QuestionOrigin, QuestionStatus, View, now_millis};

use crate::hub::Hub;

const EVERY: Duration = Duration::from_secs(15);
const TITLE_MAX: usize = 110;
const MARK: &str = "sentinel:";
const GRACE: Duration = Duration::from_secs(180);

pub async fn run(hub: Arc<Hub>) {
    let mut nudged: HashMap<String, (String, Instant)> = HashMap::new();
    loop {
        hub.wait_event(EVERY).await;
        if let Err(e) = sweep(&hub, &mut nudged) {
            eprintln!("alondrad: vigilante: {e}");
        }
    }
}

fn nudge_text(user: &str) -> String {
    format!(
        "[Alondra] Tu turno ha terminado sin decirle nada a {user} por Alondra, así que no sabe si le esperas. \
         Lo que escribes en tu terminal no le llega. Declara tu estado con una herramienta de Alondra: \
         `ask` si necesitas que decida algo (una pregunta por decisión, con opciones), \
         `report` con kind final si has terminado, o `wait_for_machine` si esperas recursos. \
         Si en 3 minutos no lo has hecho, Alondra le pasará tu último mensaje como pregunta."
    )
}

fn sweep(hub: &Hub, nudged: &mut HashMap<String, (String, Instant)>) -> anyhow::Result<()> {
    let stale: Vec<u64> = hub.read(|v| {
        v.questions
            .values()
            .filter(|q| q.status.is_open() && is_sentinel(q))
            .filter(|q| v.nodes.get(&q.session_id).is_none_or(|n| moved_on(n, q)))
            .map(|q| q.id)
            .collect()
    });
    for id in stale {
        hub.append(Event::QuestionClosed { id, status: QuestionStatus::AnsweredInTerminal })?;
    }
    let pending: Vec<(String, String, bool)> =
        hub.read(|v| v.nodes.values().filter(|n| needs_question(v, n)).map(|n| (n.session_id.clone(), key(n), n.has_plugin)).collect());
    nudged.retain(|session, (k, _)| pending.iter().any(|(s, key, _)| s == session && key == k));
    let mut waiting = vec![];
    for (session, key, has_plugin) in pending {
        if !has_plugin {
            waiting.push(session);
            continue;
        }
        match nudged.get(&session) {
            Some((_, at)) if at.elapsed() >= GRACE => waiting.push(session),
            Some(_) => {}
            None => {
                let text = nudge_text(&hub.config.name);
                hub.append_with(|v| {
                    Some(Event::MessageQueued(Message {
                        id: v.messages.keys().next_back().copied().unwrap_or(0) + 1,
                        session_id: session.clone(),
                        from: MessageFrom::Alondra,
                        text,
                        answers_question: None,
                        created_at: now_millis(),
                        handed_at: None,
                        delivered_at: None,
                    }))
                })?;
                nudged.insert(session, (key, Instant::now()));
            }
        }
    }
    for session in waiting {
        hub.append_with(|v| {
            let node = v.nodes.get(&session)?;
            needs_question(v, node).then(|| Event::QuestionAsked(question_for(v, node)))
        })?;
    }
    Ok(())
}

fn is_sentinel(q: &Question) -> bool {
    q.tool_use_id.as_deref().is_some_and(|t| t.starts_with(MARK))
}

fn key(node: &Node) -> String {
    format!("{MARK}{}:{}", node.turn, node.daemon_needs.as_deref().unwrap_or("").trim())
}

fn moved_on(node: &Node, q: &Question) -> bool {
    !node.alive || derive(node, false).0 == NodeState::Working || q.tool_use_id.as_deref() != Some(key(node).as_str())
}

fn needs_question(v: &View, node: &Node) -> bool {
    let waiting = matches!(node.state, NodeState::WaitingAnswer | NodeState::Unclear);
    if !node.alive || !node.in_daemon || node.is_orchestrator || !waiting || node.waiting_permission {
        return false;
    }
    let key = key(node);
    !v.questions
        .values()
        .any(|q| q.session_id == node.session_id && (q.status.is_open() || q.tool_use_id.as_deref() == Some(key.as_str())))
}

fn question_for(v: &View, node: &Node) -> Question {
    let needs = node.daemon_needs.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let message = node.last_message.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let title = needs.or(message.and_then(last_question)).map_or_else(
        || "El nodo espera algo de ti y no ha dicho qué".to_string(),
        |t| cap(t.lines().next().unwrap_or(t)),
    );
    Question {
        id: v.questions.keys().next_back().copied().unwrap_or(0) + 1,
        session_id: node.session_id.clone(),
        node_name: node.name.clone(),
        title,
        detail: message.or(needs).map(str::to_string),
        options: vec![],
        links: vec![],
        attachments: vec![],
        blocking: true,
        origin: QuestionOrigin::Classifier,
        tool_use_id: Some(key(node)),
        to_orchestrator: false,
        node_context: Some("El nodo lo pidió en su terminal, no con una pregunta de Alondra".into()),
        status: QuestionStatus::Open,
        answer: None,
        asked_at: now_millis(),
    }
}

fn last_question(message: &str) -> Option<&str> {
    message
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.ends_with('?'))
        .or_else(|| message.lines().rev().map(str::trim).find(|l| !l.is_empty()))
}

fn cap(text: &str) -> String {
    let text = text.trim().trim_start_matches(['-', '*', ' ']);
    if text.chars().count() <= TITLE_MAX {
        return text.to_string();
    }
    let cut: String = text.chars().take(TITLE_MAX).collect();
    format!("{}…", cut.rsplit_once(' ').map_or(cut.as_str(), |(a, _)| a))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::HookEvent;

    fn waiting_view() -> View {
        let mut v = View::default();
        let hook = |event: &str, last: Option<&str>| {
            Event::Hook(HookEvent {
                session_id: "s".into(),
                event: event.into(),
                last_message: last.map(str::to_string),
                ..Default::default()
            })
        };
        v.apply(0, 500, &Event::DaemonSeen(alondra_core::DaemonObservation {
            session_id: "s".into(),
            short_id: "s".into(),
            pid: Some(42),
            ..Default::default()
        }));
        v.apply(1, 1000, &hook("UserPromptSubmit", None));
        v.apply(2, 2000, &hook("Stop", Some("He preparado la release.\n¿Publico la 4.5.0 ya o mergeo antes la #256?")));
        let turn = v.nodes["s"].turn;
        v.apply(3, 3000, &Event::Classified { session_id: "s".into(), turn, state: NodeState::WaitingAnswer, confidence: None });
        v
    }

    #[test]
    fn a_waiting_node_without_a_question_gets_one_with_its_real_question() {
        let v = waiting_view();
        let node = &v.nodes["s"];
        assert_eq!(node.state, NodeState::WaitingAnswer);
        assert!(needs_question(&v, node));
        let q = question_for(&v, node);
        assert_eq!(q.title, "¿Publico la 4.5.0 ya o mergeo antes la #256?");
        assert!(is_sentinel(&q));
    }

    #[test]
    fn it_is_created_once_and_not_again_after_being_answered() {
        let mut v = waiting_view();
        let q = question_for(&v, &v.nodes["s"]);
        v.apply(4, 4000, &Event::QuestionAsked(q.clone()));
        assert!(!needs_question(&v, &v.nodes["s"]));
        v.apply(5, 5000, &Event::QuestionClosed { id: q.id, status: QuestionStatus::Delivered });
        assert!(!needs_question(&v, &v.nodes["s"]));
    }

    #[test]
    fn it_is_withdrawn_when_the_node_carries_on_without_the_answer() {
        let mut v = waiting_view();
        let q = question_for(&v, &v.nodes["s"]);
        v.apply(4, 4000, &Event::QuestionAsked(q.clone()));
        assert!(!moved_on(&v.nodes["s"], &q));
        v.apply(5, 5000, &Event::Hook(HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), ..Default::default() }));
        assert!(moved_on(&v.nodes["s"], &q));
    }
}
