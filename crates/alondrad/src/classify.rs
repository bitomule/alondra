use std::sync::Arc;
use std::time::Duration;

use alondra_core::{Event, NodeState};
use serde_json::Value;
use tokio::process::Command;

use crate::hub::Hub;

const QUESTION: &str = "Es el último mensaje de un agente de programación que acaba de terminar su turno. ¿En qué estado queda, visto desde la persona que lo supervisa?";
const OPTIONS: &str = "esperando_respuesta,entrega_resultado,esperando_maquina,trabajando,no_se_sabe";

pub fn spawn_if_unclear(hub: &Arc<Hub>, session_id: &str) {
    let target = hub.read(|v| {
        v.nodes.get(session_id).and_then(|n| {
            let text = if n.has_plugin {
                n.last_message.clone()
            } else {
                let joined = [n.daemon_detail.as_deref(), n.daemon_needs.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(". ");
                (!joined.trim().is_empty() && joined.trim() != "stopped").then_some(joined)
            };
            (n.state == NodeState::Unclear && n.classified.is_none() && !n.is_orchestrator).then_some((n.turn, text))
        })
    });
    let Some((turn, Some(message))) = target else { return };
    let hub = hub.clone();
    let session_id = session_id.to_string();
    tokio::spawn(async move {
        let Some((state, confidence)) = classify(&message).await else { return };
        let _ = hub.append(Event::Classified { session_id, turn, state, confidence: Some(confidence) });
    });
}

const TAIL_CHARS: usize = 4000;

fn tail(text: &str) -> &str {
    let count = text.chars().count();
    if count <= TAIL_CHARS {
        return text;
    }
    let start = text.char_indices().nth(count - TAIL_CHARS).map_or(0, |(i, _)| i);
    &text[start..]
}

async fn classify(message: &str) -> Option<(NodeState, f64)> {
    let out = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new("jevi")
            .args(["ask", QUESTION, "--options", OPTIONS, "-s", tail(message), "--json", "--soft", "--timeout", "8000"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    let answer = v.get("answers")?.get("answer")?;
    let label = answer.get("label")?.as_str()?;
    let confidence = answer.get("confidence").and_then(Value::as_f64).unwrap_or(0.0);
    let state = match label {
        "esperando_respuesta" => NodeState::WaitingAnswer,
        _ => NodeState::Unclear,
    };
    Some((state, confidence))
}

fn report_question(user: &str) -> String {
    format!(
        "This is a report a coding agent sends to {user}. Classify it. asks = it asks {user} to decide, approve, answer or do \
         something; done = it tells {user} about something finished, a result, a finding or a failure he should know about; \
         progress = the work is still under way and the agent does not need {user} yet, even if it lists the steps done so far."
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportVerdict {
    Asks,
    Progress,
    Fine,
}

pub async fn judge_report(text: &str, user: &str) -> ReportVerdict {
    let out = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new("jevi")
            .args(["ask", &report_question(user), "--options", "asks,done,progress,unsure", "-s", tail(text), "--json", "--soft", "--timeout", "8000"])
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let Ok(Ok(out)) = out else { return ReportVerdict::Fine };
    let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else { return ReportVerdict::Fine };
    match v["answers"]["answer"]["label"].as_str() {
        Some("asks") => ReportVerdict::Asks,
        Some("progress") => ReportVerdict::Progress,
        _ => ReportVerdict::Fine,
    }
}

const EXPECTS_QUESTION: &str = "This is the last message of a coding agent that has just ended its turn. What does it expect from the person supervising it? decide = it asks them to decide or answer something; act = it asks them to do something outside (test on a device, get a key, review something); nothing = it is done and expects nothing from that person.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expects {
    Decide,
    Act,
    Nothing,
    Unsure,
}

pub async fn what_node_expects(text: &str) -> Expects {
    let out = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new("jevi")
            .args(["ask", EXPECTS_QUESTION, "--options", "decide,act,nothing,unsure", "-s", tail(text), "--json", "--soft", "--timeout", "8000"])
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let Ok(Ok(out)) = out else { return Expects::Unsure };
    let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else { return Expects::Unsure };
    match v["answers"]["answer"]["label"].as_str() {
        Some("decide") => Expects::Decide,
        Some("act") => Expects::Act,
        Some("nothing") => Expects::Nothing,
        _ => Expects::Unsure,
    }
}

const SEARCH_CONCURRENCY: usize = 8;

pub async fn search(query: &str, candidates: Vec<(String, String)>) -> Vec<String> {
    let question = format!("¿Este agente encaja con lo que busca la persona: «{query}»?");
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(SEARCH_CONCURRENCY));
    let mut tasks = tokio::task::JoinSet::new();
    for (session, text) in candidates {
        let question = question.clone();
        let semaphore = semaphore.clone();
        tasks.spawn(async move {
            let _permit = semaphore.acquire().await.ok()?;
            let out = tokio::time::timeout(
                Duration::from_secs(10),
                Command::new("jevi").args(["ask", &question, "-s", &text, "--json", "--soft", "--timeout", "8000"]).kill_on_drop(true).output(),
            )
            .await
            .ok()?
            .ok()?;
            let v: Value = serde_json::from_slice(&out.stdout).ok()?;
            (v.get("answers")?.get("answer")?.get("verdict")?.as_str()? == "yes").then_some(session)
        });
    }
    let mut found = vec![];
    while let Some(r) = tasks.join_next().await {
        if let Ok(Some(s)) = r {
            found.push(s);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_the_end_where_the_question_is() {
        let text = format!("{}¿mergeo la PR?", "á".repeat(5000));
        let t = tail(&text);
        assert!(t.ends_with("¿mergeo la PR?"));
        assert_eq!(t.chars().count(), TAIL_CHARS);
        assert_eq!(tail("corto"), "corto");
    }
}
