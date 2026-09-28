//! Technical delivery of committed v2 effects. An outbox handoff is not a
//! business decision, user acceptance, or proof that Claude acted on a turn.
use crate::domain_store::OutboxMessage;
use crate::hub::Hub;
use crate::domain_runtime::DomainRuntime;
use std::path::Path;
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
                for message in ready_messages(messages, &retry_after) {
                    let id = message.id;
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

/// V2-only dispatcher: all receipts live in domain-v2.db. Producer messages
/// stay pending until a v2 Agent transport exists; nothing claims them, so
/// nothing is marked retryable and then filtered out.
pub async fn run_v2(domain: Arc<DomainRuntime>, home: std::path::PathBuf,
    push: Arc<Push>, service: Option<Arc<Service>>) -> Result<()> {
    let mut changed = domain.subscribe();
    let mut retry_after: HashMap<i64, Instant> = HashMap::new();
    loop {
        retry_after.retain(|_, at| at.elapsed() < RETRY_BACKOFF);
        let pending = {
            let store = domain.store.lock().expect("domain store poisoned");
            store.pending_messages()?
        };
        // SQLite errors stop this worker. The daemon supervisor then fails the
        // committed process instead of silently serving a dead outbox.
        for message in ready_messages_v2(pending, &retry_after) {
            let id = message.id;
            deliver_one_v2(&domain, &home, &push, service.as_deref(), message).await?;
            retry_after.insert(id, Instant::now());
        }
        tokio::select! {
            _ = changed.changed() => {},
            _ = tokio::time::sleep(Duration::from_secs(10)) => {},
        }
    }
}

async fn deliver_one_v2(domain: &DomainRuntime, home: &Path, push: &Push,
    service: Option<&Service>, message: OutboxMessage) -> Result<()> {
    if matches!(message.recipient_kind.as_str(), "agent" | "orchestrator") {
        return Ok(());
    }
    let claim = {
        let mut store = domain.store.lock().expect("domain store poisoned");
        store.claim_message(message.id)?
    };
    let Some(claim) = claim else { return Ok(()) };
    let outcome = match claim.message.recipient_kind.as_str() {
        "user" => deliver_user_runtime(domain, home, push, service, &claim.message).await,
        _ => Err((State::Uncertain, "unknown_recipient")),
    };
    let (state, reason) = match outcome {
        Ok(()) => (State::Handed, None),
        Err((state, reason)) => (state, Some(reason)),
    };
    let finished = {
        let mut store = domain.store.lock().expect("domain store poisoned");
        store.finish_message(claim.attempt_id, claim.version, state, reason)?
    };
    anyhow::ensure!(finished, "domain delivery claim changed while finishing");
    Ok(())
}

/// User hints and producer messages are limited separately, so a backlog of
/// one kind never hides the other and a retryable producer message is retried.
fn ready_messages_v2(messages: Vec<OutboxMessage>, retry_after: &HashMap<i64, Instant>) -> Vec<OutboxMessage> {
    let (user, producer): (Vec<_>, Vec<_>) = messages.into_iter()
        .partition(|message| message.recipient_kind == "user");
    let mut ready = ready_messages(user, retry_after);
    ready.extend(ready_messages(producer, retry_after));
    ready
}

fn ready_messages(
    messages: Vec<OutboxMessage>,
    retry_after: &HashMap<i64, Instant>,
) -> Vec<OutboxMessage> {
    // Apply backoff before limiting a pass; otherwise 64 unavailable old
    // recipients permanently hide all newer User and Agent messages.
    messages
        .into_iter()
        .filter(|message| {
            !retry_after
                .get(&message.id)
                .is_some_and(|at| at.elapsed() < RETRY_BACKOFF)
        })
        .take(64)
        .collect()
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
    deliver_user_runtime(&hub.domain, &hub.home, push, service, message).await
}

async fn deliver_user_runtime(
    domain: &DomainRuntime, home: &Path, push: &Push,
    service: Option<&Service>, message: &OutboxMessage,
) -> std::result::Result<(), (State, &'static str)> {
    // DomainProjection is the durable user inbox. APNs is only a generic wake hint;
    // the receipt never means a person read or accepted this interaction.
    let state = domain.state()
        .map_err(|_| (State::Retryable, "domain_read_failed"))?;
    let item_id = message.interaction_id.as_ref().map(|id| format!("interaction:{id}"));
    let payload = json!({"aps":{"alert":{"title":"Alondra","body":"Hay actividad nueva en Alondra"},
        "sound":"default"},"alondra":{"protocol_version":2,"data_epoch":state.data_epoch,
        "item_id":item_id}});
    if let Some(service) = service {
        let (attempted, accepted) = service
            .broadcast_push_report(&payload, true)
            .await
            .map_err(|_| (State::Uncertain, "encrypted_push_unknown"))?;
        if attempted != accepted {
            return Err((State::Uncertain, "encrypted_push_partial_or_unknown"));
        }
    } else if home.join("service.toml").exists() {
        return Err((State::Retryable, "encrypted_service_unavailable"));
    } else if push.enabled() {
        let (attempted, accepted) = push.broadcast_report(&payload, true).await;
        if attempted != accepted {
            return Err((State::Uncertain, "push_partial_or_unknown"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain_v2::*;
    use std::collections::BTreeSet;

    #[test]
    fn unavailable_old_recipients_do_not_starve_new_messages() {
        let messages: Vec<_> = (1..=65).map(|id| OutboxMessage {
            id, recipient_kind: "agent".into(), agent_id: Some("absent".into()),
            task_id: None, interaction_id: None, message: String::new(), state: "retryable".into(),
        }).collect();
        let backoff: HashMap<_, _> = (1..=64).map(|id| (id, Instant::now())).collect();
        let ready = ready_messages(messages, &backoff);
        assert_eq!(ready.iter().map(|message| message.id).collect::<Vec<_>>(), vec![65]);
    }

    #[tokio::test]
    async fn v2_only_outbox_exits_on_sql_failure() {
        let home = std::env::temp_dir().join(format!("alondra-v2-outbox-failure-{}", crate::auth::random_hex(8)));
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        rusqlite::Connection::open(home.join("domain-v2.db")).unwrap()
            .execute_batch("DROP TABLE domain_outbox").unwrap();
        let push = Arc::new(Push::open(&home).unwrap());
        let result = tokio::time::timeout(Duration::from_secs(2),
            run_v2(domain.clone(), home.clone(), push, None)).await.unwrap();
        assert!(result.is_err(), "SQL failure must terminate the critical outbox worker");
        drop(domain);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn v2_only_retryable_producers_never_starve_later_user_hints() {
        let mut messages: Vec<_> = (1..=65).map(|id| OutboxMessage {
            id, recipient_kind: "agent".into(), agent_id: Some("absent".into()),
            task_id: None, interaction_id: None, message: String::new(), state: "retryable".into(),
        }).collect();
        messages.push(OutboxMessage { id: 66, recipient_kind: "user".into(),
            agent_id: None, task_id: None, interaction_id: None, message: String::new(), state: "pending".into() });
        let ready = ready_messages_v2(messages, &HashMap::new());
        let ids: Vec<_> = ready.iter().map(|message| message.id).collect();
        assert_eq!(ids[0], 66, "the user hint is never behind the producer backlog");
        assert_eq!(&ids[1..], (1..=64).collect::<Vec<_>>().as_slice(),
            "retryable producer messages are retried, not filtered forever");
    }

    fn v2_command(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: 2, data_epoch: state.data_epoch,
            command_id: id.into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    #[tokio::test]
    async fn v2_only_outbox_hands_user_hint_and_leaves_producer_pending_without_transport() {
        let home = std::env::temp_dir().join(format!("alondra-v2-only-outbox-{}", crate::auth::random_hex(8)));
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        v2_command(&domain, Actor::System, "agent", Command::RegisterAgent {
            agent_id: "agent-a".into(), name: "A".into(),
        });
        v2_command(&domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest {
            request_id: "request-a".into(), body: "Request".into(),
        });
        v2_command(&domain, Actor::User { user_id: "u".into() }, "task", Command::CreateTask {
            task_id: "task-a".into(), request_id: "request-a".into(), title: "Task".into(),
            agent_id: Some("agent-a".into()), review_prs: false,
            dependencies: BTreeSet::new(), stack: None,
        });
        v2_command(&domain, Actor::Agent { agent_id: "agent-a".into() }, "question", Command::AskQuestion {
            attachments: vec![], interaction_id: "question-a".into(),
            scope: Scope::Task { task_id: "task-a".into() }, recipient: Recipient::User,
            body: "Choose".into(), blocking: true,
            options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), inputs: vec![] }],
        });
        let push = Push::open(&home).unwrap();
        let user = domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .find(|message| message.recipient_kind == "user").unwrap();
        deliver_one_v2(&domain, &home, &push, None, user.clone()).await.unwrap();
        assert!(domain.store.lock().unwrap().delivery_evidence_page(0).unwrap().iter()
            .any(|item| item.message_id == user.id && item.state == "handed"));
        assert!(matches!(domain.state().unwrap().interactions[&"question-a".into()].payload,
            InteractionPayload::Question { .. }));
        v2_command(&domain, Actor::User { user_id: "u".into() }, "answer", Command::AnswerQuestion {
            attachments: vec![], interaction_id: "question-a".into(),
            option_id: "yes".into(), inputs: Default::default(),
        });
        let producer = domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .find(|message| message.recipient_kind == "agent").unwrap();
        deliver_one_v2(&domain, &home, &push, None, producer.clone()).await.unwrap();
        let evidence = domain.store.lock().unwrap().delivery_evidence_page(0).unwrap();
        assert!(!evidence.iter().any(|item| item.message_id == producer.id),
            "without an Agent transport the producer message is not claimed");
        assert!(domain.store.lock().unwrap().pending_messages().unwrap().iter()
            .any(|message| message.id == producer.id && message.state == "pending"));
        // A retry claim has a fresh version. A stale finisher cannot complete it,
        // and restart makes the ambiguous claim uncertain rather than redelivering.
        let claim = domain.store.lock().unwrap().claim_message(producer.id).unwrap().unwrap();
        assert!(!domain.store.lock().unwrap().finish_message(claim.attempt_id,
            claim.version + 1, State::Handed, None).unwrap());
        drop(domain);
        let reopened = DomainRuntime::open(&home).unwrap();
        let recovered = reopened.store.lock().unwrap().delivery_evidence_page(0).unwrap();
        assert!(recovered.iter().any(|item| item.attempt_id == claim.attempt_id
            && item.state == "uncertain" && item.reason.as_deref() == Some("daemon_restarted_after_claim")));
        assert!(!reopened.store.lock().unwrap().pending_messages().unwrap().iter()
            .any(|message| message.id == producer.id));
        drop(reopened);
        std::fs::remove_dir_all(home).unwrap();
    }

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
        command(&hub, Actor::Agent { agent_id: "agent-a".into() }, "info-2",
            Command::PublishInformation { attachments: vec![], interaction_id: "info-b".into(),
                scope: Scope::Task { task_id: "task-a".into() }, recipient: Recipient::User,
                body: "Another update".into() });
        std::fs::write(home.join("service.toml"), "service unavailable").unwrap();
        let user_message = hub.domain.store.lock().unwrap().pending_messages().unwrap()
            .into_iter().find(|m| m.interaction_id.as_deref() == Some("info-b")).unwrap();
        deliver_one(&hub, &push, None, user_message.clone()).await.unwrap();
        assert!(hub.domain.store.lock().unwrap().delivery_evidence().unwrap().iter()
            .any(|e| e.message_id == user_message.id && e.state == "retryable"));
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }
}
