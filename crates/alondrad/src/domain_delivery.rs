//! Technical delivery of committed v2 effects. An outbox handoff is not a
//! business decision, user acceptance, or proof that Claude acted on a turn.
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::OutboxMessage;
use crate::hub::Hub;
use crate::push::Push;
use crate::service::Service;
use alondra_core::Event;
use alondra_core::delivery::State;
use anyhow::Result;
use alondra_core::domain_v2::{
    DomainState, InputKind, InteractionId, InteractionPayload, QuestionOption,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
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

/// V2-only dispatcher: all receipts live in the active domain database. User hints are sent
/// here; the orchestrator executor and producer transport own producer messages.
pub async fn run_v2(
    domain: Arc<DomainRuntime>,
    home: std::path::PathBuf,
    push: Arc<Push>,
    service: Option<Arc<Service>>,
) -> Result<()> {
    let mut changed = domain.subscribe();
    let mut retry_after: HashMap<i64, Instant> = HashMap::new();
    loop {
        retry_after.retain(|_, at| at.elapsed() < RETRY_BACKOFF);
        let pending = {
            let mut store = domain.store.lock().expect("domain store poisoned");
            store.discard_expired_orchestrator_messages(
                alondra_core::now_millis()
                    .saturating_sub(crate::domain_store::ORCHESTRATOR_MESSAGE_TTL_MS),
            )?;
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

async fn deliver_one_v2(
    domain: &DomainRuntime,
    home: &Path,
    push: &Push,
    service: Option<&Service>,
    message: OutboxMessage,
) -> Result<()> {
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
fn ready_messages_v2(
    messages: Vec<OutboxMessage>,
    retry_after: &HashMap<i64, Instant>,
) -> Vec<OutboxMessage> {
    let (user, producer): (Vec<_>, Vec<_>) = messages
        .into_iter()
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
    domain: &DomainRuntime,
    home: &Path,
    push: &Push,
    service: Option<&Service>,
    message: &OutboxMessage,
) -> std::result::Result<(), (State, &'static str)> {
    // DomainProjection is the durable user inbox. APNs is only a generic wake hint;
    // the receipt never means a person read or accepted this interaction.
    let state = domain
        .state()
        .map_err(|_| (State::Retryable, "domain_read_failed"))?;
    let payload = user_push_payload(&state, message.interaction_id.as_deref());
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

const PUSH_OPTION_LIMIT: usize = 4;

fn user_push_payload(state: &DomainState, interaction_id: Option<&str>) -> Value {
    let mut alondra = json!({"protocol_version": 2, "data_epoch": state.data_epoch});
    let mut aps = json!({"alert": {"title": "Alondra", "body": "New activity in Alondra"},
        "sound": "default"});
    if let Some(id) = interaction_id {
        alondra["item_id"] = json!(format!("interaction:{id}"));
        alondra["interaction_id"] = json!(id);
        if let Some(interaction) = state.interactions.get(&InteractionId::from(id)) {
            let (kind, category) = match interaction.payload {
                InteractionPayload::Information { .. } => ("information", Some("ALONDRA_READ")),
                InteractionPayload::Question { .. } => ("question", Some("ALONDRA_QUESTION")),
                InteractionPayload::ActionRequest { .. } => {
                    ("action_request", Some("ALONDRA_QUESTION"))
                }
                InteractionPayload::Delivery { .. } => ("delivery", None),
            };
            alondra["kind"] = json!(kind);
            if let Some(category) = category {
                alondra["expected_revision"] = json!(interaction.revision);
                aps["category"] = json!(category);
                aps["mutable-content"] = json!(1);
            }
            if let InteractionPayload::Question { options, .. }
            | InteractionPayload::ActionRequest { options, .. } = &interaction.payload
            {
                let (options, reply_option_id) = push_options(options);
                if !options.is_empty() {
                    alondra["options"] = json!(options);
                }
                if let Some(reply) = reply_option_id {
                    alondra["reply_option_id"] = json!(reply);
                }
            }
        }
    }
    json!({"aps": aps, "alondra": alondra})
}

/// The options the phone can answer from a notification: those with no required input become
/// buttons, and the first with a single text input is answered by typing a reply.
fn push_options(options: &[QuestionOption]) -> (Vec<Value>, Option<String>) {
    let mut pushed = Vec::new();
    let mut reply = None;
    for option in options {
        if pushed.len() == PUSH_OPTION_LIMIT {
            break;
        }
        let mut entry = json!({"id": option.id, "label": option.label});
        match option.inputs.as_slice() {
            [input] if input.kind == InputKind::Text => {
                entry["text_input"] = json!(input.id);
                reply.get_or_insert_with(|| option.id.clone());
            }
            inputs if inputs.iter().all(|input| !input.required) => {}
            _ => continue,
        }
        pushed.push(entry);
    }
    (pushed, reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain_v2::*;
    use std::collections::BTreeSet;

    #[test]
    fn unavailable_old_recipients_do_not_starve_new_messages() {
        let messages: Vec<_> = (1..=65)
            .map(|id| OutboxMessage {
                id,
                recipient_kind: "agent".into(),
                agent_id: Some("absent".into()),
                task_id: None,
                interaction_id: None,
                message: String::new(),
                state: "retryable".into(),
            })
            .collect();
        let backoff: HashMap<_, _> = (1..=64).map(|id| (id, Instant::now())).collect();
        let ready = ready_messages(messages, &backoff);
        assert_eq!(
            ready.iter().map(|message| message.id).collect::<Vec<_>>(),
            vec![65]
        );
    }

    #[tokio::test]
    async fn v2_only_outbox_exits_on_sql_failure() {
        let home = std::env::temp_dir().join(format!(
            "alondra-v2-outbox-failure-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        rusqlite::Connection::open(home.join("domain.db"))
            .unwrap()
            .execute_batch("DROP TABLE domain_outbox")
            .unwrap();
        let push = Arc::new(Push::open(&home).unwrap());
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            run_v2(domain.clone(), home.clone(), push, None),
        )
        .await
        .unwrap();
        assert!(
            result.is_err(),
            "SQL failure must terminate the critical outbox worker"
        );
        drop(domain);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn v2_only_retryable_producers_never_starve_later_user_hints() {
        let mut messages: Vec<_> = (1..=65)
            .map(|id| OutboxMessage {
                id,
                recipient_kind: "agent".into(),
                agent_id: Some("absent".into()),
                task_id: None,
                interaction_id: None,
                message: String::new(),
                state: "retryable".into(),
            })
            .collect();
        messages.push(OutboxMessage {
            id: 66,
            recipient_kind: "user".into(),
            agent_id: None,
            task_id: None,
            interaction_id: None,
            message: String::new(),
            state: "pending".into(),
        });
        let ready = ready_messages_v2(messages, &HashMap::new());
        let ids: Vec<_> = ready.iter().map(|message| message.id).collect();
        assert_eq!(
            ids[0], 66,
            "the user hint is never behind the producer backlog"
        );
        assert_eq!(
            &ids[1..],
            (1..=64).collect::<Vec<_>>().as_slice(),
            "retryable producer messages are retried, not filtered forever"
        );
    }

    fn v2_command(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain
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
    async fn v2_only_outbox_hands_user_hint_and_leaves_producer_pending_without_transport() {
        let home = std::env::temp_dir().join(format!(
            "alondra-v2-only-outbox-{}",
            crate::auth::random_hex(8)
        ));
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        v2_command(
            &domain,
            Actor::System,
            "agent",
            Command::RegisterAgent {
                agent_id: "agent-a".into(),
                name: "A".into(),
            },
        );
        v2_command(
            &domain,
            Actor::User {
                user_id: "u".into(),
            },
            "request",
            Command::CreateRequest {
                request_id: "request-a".into(),
                body: "Request".into(),
                auto_merge: false,
            },
        );
        v2_command(
            &domain,
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
        v2_command(
            &domain,
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
                    description: None,
                    inputs: vec![],
                }],
            },
        );
        let push = Push::open(&home).unwrap();
        let user = domain
            .store
            .lock()
            .unwrap()
            .pending_messages()
            .unwrap()
            .into_iter()
            .find(|message| message.recipient_kind == "user")
            .unwrap();
        deliver_one_v2(&domain, &home, &push, None, user.clone())
            .await
            .unwrap();
        assert!(
            domain
                .store
                .lock()
                .unwrap()
                .delivery_evidence_page(0)
                .unwrap()
                .iter()
                .any(|item| item.message_id == user.id && item.state == "handed")
        );
        assert!(matches!(
            domain.state().unwrap().interactions[&"question-a".into()].payload,
            InteractionPayload::Question { .. }
        ));
        v2_command(
            &domain,
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
        let producer = domain
            .store
            .lock()
            .unwrap()
            .pending_messages()
            .unwrap()
            .into_iter()
            .find(|message| message.recipient_kind == "agent")
            .unwrap();
        deliver_one_v2(&domain, &home, &push, None, producer.clone())
            .await
            .unwrap();
        let evidence = domain
            .store
            .lock()
            .unwrap()
            .delivery_evidence_page(0)
            .unwrap();
        assert!(
            !evidence.iter().any(|item| item.message_id == producer.id),
            "without an Agent transport the producer message is not claimed"
        );
        assert!(
            domain
                .store
                .lock()
                .unwrap()
                .pending_messages()
                .unwrap()
                .iter()
                .any(|message| message.id == producer.id && message.state == "pending")
        );
        // A retry claim has a fresh version. A stale finisher cannot complete it,
        // and restart makes the ambiguous claim uncertain rather than redelivering.
        let claim = domain
            .store
            .lock()
            .unwrap()
            .claim_message(producer.id)
            .unwrap()
            .unwrap();
        assert!(
            !domain
                .store
                .lock()
                .unwrap()
                .finish_message(claim.attempt_id, claim.version + 1, State::Handed, None)
                .unwrap()
        );
        drop(domain);
        let reopened = DomainRuntime::open(&home).unwrap();
        let recovered = reopened
            .store
            .lock()
            .unwrap()
            .delivery_evidence_page(0)
            .unwrap();
        assert!(
            recovered
                .iter()
                .any(|item| item.attempt_id == claim.attempt_id
                    && item.state == "uncertain"
                    && item.reason.as_deref() == Some("daemon_restarted_after_claim"))
        );
        assert!(
            !reopened
                .store
                .lock()
                .unwrap()
                .pending_messages()
                .unwrap()
                .iter()
                .any(|message| message.id == producer.id)
        );
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
                auto_merge: false,
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
                    description: None,
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
        command(
            &hub,
            Actor::Agent {
                agent_id: "agent-a".into(),
            },
            "info-2",
            Command::PublishInformation {
                attachments: vec![],
                interaction_id: "info-b".into(),
                scope: Scope::Task {
                    task_id: "task-a".into(),
                },
                recipient: Recipient::User,
                body: "Another update".into(),
            },
        );
        std::fs::write(home.join("service.toml"), "service unavailable").unwrap();
        let user_message = hub
            .domain
            .store
            .lock()
            .unwrap()
            .pending_messages()
            .unwrap()
            .into_iter()
            .find(|m| m.interaction_id.as_deref() == Some("info-b"))
            .unwrap();
        deliver_one(&hub, &push, None, user_message.clone())
            .await
            .unwrap();
        assert!(
            hub.domain
                .store
                .lock()
                .unwrap()
                .delivery_evidence()
                .unwrap()
                .iter()
                .any(|e| e.message_id == user_message.id && e.state == "retryable")
        );
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn user_push_payload_matches_the_apple_client_fixture() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../../fixtures/client-v2/push-payloads.json"))
                .unwrap();
        let mut state = DomainState::new(fixture["data_epoch"].as_str().unwrap());
        let options = vec![
            QuestionOption {
                id: "yes".into(),
                label: "Yes".into(),
                description: None,
                inputs: vec![],
            },
            QuestionOption {
                id: "changes".into(),
                label: "Request changes".into(),
                description: None,
                inputs: vec![InputSpec {
                    id: "why".into(),
                    kind: InputKind::Text,
                    required: true,
                }],
            },
            QuestionOption {
                id: "scale".into(),
                label: "Scale".into(),
                description: None,
                inputs: vec![InputSpec {
                    id: "count".into(),
                    kind: InputKind::Integer,
                    required: true,
                }],
            },
        ];
        let delivery = |target| InteractionPayload::Delivery {
            targets: vec![target],
            is_final: true,
            acceptance: None,
            feedback: vec![],
        };
        let payloads = [
            (
                "question",
                InteractionPayload::Question {
                    options: options.clone(),
                    answer: None,
                },
            ),
            (
                "action_request",
                InteractionPayload::ActionRequest {
                    options,
                    answer: None,
                },
            ),
            (
                "information",
                InteractionPayload::Information {
                    read_by: BTreeSet::new(),
                },
            ),
            (
                "delivery_text",
                delivery(DeliveryTarget::Text { text: "Done".into() }),
            ),
            (
                "delivery_pull_request",
                delivery(DeliveryTarget::PullRequest {
                    pr_id: "pr-1".into(),
                    head: "abc".into(),
                }),
            ),
        ];
        for (name, payload) in payloads {
            let id = InteractionId::from(format!("i-{name}").as_str());
            state.interactions.insert(
                id.clone(),
                Interaction {
                    attachments: vec![],
                    id,
                    scope: Scope::Task {
                        task_id: "task-a".into(),
                    },
                    author: Author::System,
                    recipient: Recipient::User,
                    body: "Body".into(),
                    blocking: false,
                    subject: None,
                    payload,
                    revision: 1,
                    order: 1,
                    withdrawn: false,
                },
            );
        }
        let cases = fixture["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 5);
        for case in cases {
            let name = case["interaction"].as_str().unwrap();
            assert_eq!(
                user_push_payload(&state, Some(&format!("i-{name}"))),
                case["payload"],
                "{name}"
            );
        }
    }
}
