use std::sync::Arc;
use std::time::Duration;

use alondra_core::{Event, NodeState};
use serde_json::Value;
use tokio::process::Command;

use crate::hub::Hub;

const QUESTION: &str = "Es el último mensaje de un agente de programación que acaba de terminar su turno. ¿En qué estado queda, visto desde la persona que lo supervisa?";
const OPTIONS: &str = "esperando_respuesta,entrega_resultado,esperando_maquina,trabajando,ninguna";
const MIN_CONFIDENCE: f64 = 0.6;

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
            (n.state == NodeState::Unclear && n.classified.is_none()).then_some((n.turn, text))
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

async fn classify(message: &str) -> Option<(NodeState, f64)> {
    let out = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new("jevi")
            .args(["ask", QUESTION, "--options", OPTIONS, "-s", message, "--json", "--soft", "--timeout", "8000", "--max-chars", "4000"])
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
        _ if confidence < MIN_CONFIDENCE => NodeState::Unclear,
        "esperando_respuesta" => NodeState::WaitingAnswer,
        "entrega_resultado" => NodeState::Delivered,
        "esperando_maquina" => NodeState::WaitingMachine,
        "trabajando" => NodeState::Working,
        _ => NodeState::Unclear,
    };
    Some((state, confidence))
}

const ASKS_QUESTION: &str = "Es un informe que un agente de programación manda a David para que lo lea. ¿Le pide a David que decida, apruebe, conteste o haga algo?";
const ASKS_MIN_CONFIDENCE: f64 = 0.8;

pub async fn report_asks_for_action(text: &str) -> bool {
    let out = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new("jevi")
            .args(["ask", ASKS_QUESTION, "--options", "pide_algo,solo_leer", "-s", text, "--json", "--soft", "--timeout", "8000", "--max-chars", "4000"])
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let Ok(Ok(out)) = out else { return false };
    let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else { return false };
    let answer = &v["answers"]["answer"];
    answer["label"].as_str() == Some("pide_algo") && answer["confidence"].as_f64().unwrap_or(0.0) >= ASKS_MIN_CONFIDENCE
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
