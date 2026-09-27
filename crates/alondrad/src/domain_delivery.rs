//! Technical delivery of committed v2 effects. An outbox handoff is not a
//! business decision, user acceptance, or proof that Claude acted on a turn.
use crate::domain_store::OutboxMessage;
use crate::hub::Hub;
use crate::push::Push;
use crate::service::Service;
use alondra_core::Event;
use alondra_core::delivery::State;
use anyhow::Result;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const RETRY_BACKOFF: Duration = Duration::from_secs(120);

pub async fn run(hub: Arc<Hub>, push: Arc<Push>, service: Option<Arc<Service>>) {
    let mut changed = hub.domain.subscribe();
    let mut retry_after: HashMap<i64, Instant> = HashMap::new();
    loop {
        retry_after.retain(|_, at| at.elapsed() < RETRY_BACKOFF);
        let pending = {
            let store = hub.domain.store.lock().expect("domain store poisoned");
            store.pending_messages()
        };
        match pending {
            Ok(messages) => {
                for message in messages.into_iter().take(64) {
                    let id = message.id;
                    if retry_after
                        .get(&id)
                        .is_some_and(|at| at.elapsed() < RETRY_BACKOFF)
                    {
                        continue;
                    }
                    if let Err(error) = deliver_one(&hub, &push, service.as_deref(), message).await
                    {
                        log!("domain v2 outbox dispatch failed: {error:#}");
                    }
                    retry_after.insert(id, Instant::now());
                }
            }
            Err(error) => log!("domain v2 outbox read failed: {error:#}"),
        }
        tokio::select! {
            _ = changed.changed() => {},
            _ = tokio::time::sleep(Duration::from_secs(10)) => {},
        }
    }
}

async fn deliver_one(
    hub: &Hub,
    push: &Push,
    service: Option<&Service>,
    message: OutboxMessage,
) -> Result<()> {
    // Claim is durable before either APNs or a second store is touched. If the
    // process dies after this point, recovery marks the attempt uncertain.
    let claim = {
        let mut store = hub.domain.store.lock().expect("domain store poisoned");
        store.claim_message(message.id)?
    };
    let Some(claim) = claim else { return Ok(()) };
    let outcome = match claim.message.recipient_kind.as_str() {
        "user" => deliver_user(hub, push, service, &claim.message).await,
        "agent" | "orchestrator" => deliver_producer(hub, &claim.message),
        _ => Err((State::Uncertain, "unknown_recipient")),
    };
    let (state, reason) = match outcome {
        Ok(()) => (State::Handed, None),
        Err((state, reason)) => (state, Some(reason)),
    };
    let finished = {
        let mut store = hub.domain.store.lock().expect("domain store poisoned");
        store.finish_message(claim.attempt_id, claim.version, state, reason)?
    };
    anyhow::ensure!(finished, "domain delivery claim changed while finishing");
    Ok(())
}

fn deliver_producer(
    hub: &Hub,
    message: &OutboxMessage,
) -> std::result::Result<(), (State, &'static str)> {
    let destination = match message.recipient_kind.as_str() {
        "agent" => message.agent_id.clone(),
        "orchestrator" => hub.read(|view| view.orchestrator_destination().map(str::to_owned)),
        _ => None,
    };
    let Some(destination) = destination else {
        return Err((State::Retryable, "producer_unavailable"));
    };
    let text = format!(
        "[Alondra domain v2 message {}] {}\nRefresh the v2 Task and respond with a v2 command.",
        message.id, message.message
    );
    match hub.append_with(|view| {
        view.alondra_message(&destination, text, alondra_core::now_millis())
            .map(Event::MessageQueued)
    }) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err((State::Retryable, "producer_unavailable")),
        Err(_) => Err((State::Uncertain, "technical_queue_commit_unknown")),
    }
}

async fn deliver_user(
    hub: &Hub,
    push: &Push,
    service: Option<&Service>,
    message: &OutboxMessage,
) -> std::result::Result<(), (State, &'static str)> {
    // DomainProjection is the durable user inbox. APNs is only a generic wake hint;
    // the receipt never means a person read or accepted this interaction.
    let state = hub
        .domain
        .state()
        .map_err(|_| (State::Retryable, "domain_read_failed"))?;
    let item_id = message.interaction_id.as_ref().map(|id| format!("interaction:{id}"));
    let payload = json!({"aps":{"alert":{"title":"Alondra","body":"Hay actividad nueva en Alondra"},
        "sound":"default"},"alondra":{"protocol_version":2,"data_epoch":state.data_epoch,
        "item_id":item_id}});
    if let Some(service) = service {
        service
            .broadcast_push(&payload, true)
            .await
            .map_err(|_| (State::Uncertain, "encrypted_push_unknown"))?;
    } else if push.enabled() && !hub.home.join("service.toml").exists() {
        push.broadcast(&payload, true).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain_v2::*;
    use std::collections::BTreeSet;

    fn command(hub: &Hub, actor: Actor, id: &str, command: Command) {
        let state = hub.domain.state().unwrap();
        hub.domain
            .execute(
                &actor,
                &CommandEnvelope {
                    protocol_version: 2,
                    data_epoch: state.data_epoch,
                    command_id: id.into(),
                    expected_revision: state.revision,
                    command,
                },
                None,
                || Ok(()),
            )
            .unwrap();
    }

    #[tokio::test]
    async fn user_hint_is_handed_without_claiming_a_read_or_acceptance() {
        let home = std::env::temp_dir().join(format!(
            "alondra-v2-delivery-{}",
            crate::auth::random_hex(16)
        ));
        let config = alondra_core::config::Config::resolve(
            home.clone(),
            &Default::default(),
            |_| None,
            || Some("Fixture".into()),
        );
        let hub = Hub::open(config, false).unwrap();
        command(
            &hub,
            Actor::System,
            "agent",
            Command::RegisterAgent {
                agent_id: "agent-a".into(),
                name: "A".into(),
            },
        );
        command(
            &hub,
            Actor::User {
                user_id: "u".into(),
            },
            "request",
            Command::CreateRequest {
                request_id: "request-a".into(),
                body: "Request".into(),
            },
        );
        command(
            &hub,
            Actor::User {
                user_id: "u".into(),
            },
            "task",
            Command::CreateTask {
                task_id: "task-a".into(),
                request_id: "request-a".into(),
                title: "Task".into(),
                agent_id: Some("agent-a".into()),
                review_prs: false,
                dependencies: BTreeSet::new(),
                stack: None,
            },
        );
        command(
            &hub,
            Actor::Agent {
                agent_id: "agent-a".into(),
            },
            "info",
            Command::PublishInformation {
                attachments: vec![],
                interaction_id: "info-a".into(),
                scope: Scope::Task {
                    task_id: "task-a".into(),
                },
                recipient: Recipient::User,
                body: "Information".into(),
            },
        );
        let message = hub
            .domain
            .store
            .lock()
            .unwrap()
            .pending_messages()
            .unwrap()
            .into_iter()
            .find(|m| m.recipient_kind == "user")
            .unwrap();
        let push = Push::open(&home).unwrap();
        deliver_one(&hub, &push, None, message.clone())
            .await
            .unwrap();
        let evidence = hub
            .domain
            .store
            .lock()
            .unwrap()
            .delivery_evidence()
            .unwrap();
        assert!(
            evidence
                .iter()
                .any(|item| item.message_id == message.id && item.state == "handed")
        );
        let state = hub.domain.state().unwrap();
        assert!(matches!(state.interactions[&"info-a".into()].payload,
            InteractionPayload::Information { ref read_by } if read_by.is_empty()));
        command(
            &hub,
            Actor::Agent {
                agent_id: "agent-a".into(),
            },
            "question",
            Command::AskQuestion {
                attachments: vec![],
                interaction_id: "question-a".into(),
                scope: Scope::Task {
                    task_id: "task-a".into(),
                },
                recipient: Recipient::User,
                body: "Choose".into(),
                blocking: true,
                options: vec![QuestionOption {
                    id: "yes".into(),
                    label: "Yes".into(),
                    inputs: vec![],
                }],
            },
        );
        command(
            &hub,
            Actor::User {
                user_id: "u".into(),
            },
            "answer",
            Command::AnswerQuestion {
                attachments: vec![],
                interaction_id: "question-a".into(),
                option_id: "yes".into(),
                inputs: Default::default(),
            },
        );
        let agent_message = hub
            .domain
            .store
            .lock()
            .unwrap()
            .pending_messages()
            .unwrap()
            .into_iter()
            .find(|m| m.recipient_kind == "agent")
            .unwrap();
        deliver_one(&hub, &push, None, agent_message.clone())
            .await
            .unwrap();
        assert!(
            hub.domain
                .store
                .lock()
                .unwrap()
                .pending_messages()
                .unwrap()
                .iter()
                .any(|m| m.id == agent_message.id && m.state == "retryable")
        );
        assert!(
            hub.domain
                .store
                .lock()
                .unwrap()
                .delivery_evidence()
                .unwrap()
                .iter()
                .any(|e| e.message_id == agent_message.id && e.state == "retryable")
        );
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }
}
