//! Technical delivery of committed v2 effects. An outbox handoff is not a
//! business decision, user acceptance, or proof that Claude acted on a turn.
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::OutboxMessage;
use crate::hub::Hub;
use crate::push::Push;
use crate::service::Service;
use alondra_core::Event;
use alondra_core::delivery::State;
use alondra_core::views::{self, Lang};
use anyhow::Result;
use alondra_core::domain_v2::{DomainState, InteractionId, InteractionPayload};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

const RETRY_BACKOFF: Duration = Duration::from_secs(120);

/// V2-only dispatcher: all receipts live in the active domain database. User hints are sent
/// here; the orchestrator executor and producer transport own producer messages.
pub async fn run_v2(
    domain: Arc<DomainRuntime>,
    home: std::path::PathBuf,
    push: Arc<Push>,
    service: Option<Arc<Service>>,
    client: Arc<crate::v2_client_routes::Client>,
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
            deliver_one_v2(&domain, &home, &push, service.as_deref(), &client, message).await?;
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
    client: &crate::v2_client_routes::Client,
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
        "user" => deliver_user_runtime(domain, home, push, service, Some(client), &claim.message).await,
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
    let state = hub
        .domain
        .state()
        .map_err(|_| (State::Retryable, "domain_unavailable"))?;
    let text = crate::domain_transport::render(&state, message);
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
    deliver_user_runtime(&hub.domain, &hub.home, push, service, None, message).await
}

async fn deliver_user_runtime(
    domain: &DomainRuntime,
    home: &Path,
    push: &Push,
    service: Option<&Service>,
    client: Option<&crate::v2_client_routes::Client>,
    message: &OutboxMessage,
) -> std::result::Result<(), (State, &'static str)> {
    // DomainProjection is the durable user inbox. APNs is only a generic wake hint;
    // the receipt never means a person read or accepted this interaction.
    let state = domain
        .state()
        .map_err(|_| (State::Retryable, "domain_read_failed"))?;
    let payload = user_push_payload(&state, message.interaction_id.as_deref(), Lang::En);
    if let Some(service) = service {
        let interaction_id = message.interaction_id.clone();
        let (attempted, accepted) = service
            .broadcast_push_report_for_device(true, |device| {
                user_push_payload(&state, interaction_id.as_deref(), client
                    .map_or(Lang::En, |client| client.language_for_device(device)))
            })
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

fn user_push_payload(state: &DomainState, interaction_id: Option<&str>, lang: Lang) -> Value {
    let mut alondra = json!({"protocol": views::VIEWS_PROTOCOL, "data_epoch": state.data_epoch,
        "actions": []});
    let mut aps = json!({"alert": {"title": "Alondra"}, "sound": "default", "mutable-content": 1});
    if let Some(id) = interaction_id {
        alondra["card"] = json!(id);
        if let Some(interaction) = state.interactions.get(&InteractionId::from(id)) {
            let kind = match interaction.payload {
                InteractionPayload::Information { .. } => "information",
                InteractionPayload::Question { .. } => "question",
                InteractionPayload::ActionRequest { .. } => "action_request",
                InteractionPayload::Delivery { .. } => "delivery",
            };
            alondra["kind"] = json!(kind);
            let content = views::push_content(state, id, lang).unwrap_or_else(|error| {
                log!("could not render push content for card {id}: {error:?}");
                views::PushContent { alert: None, actions: vec![] }
            });
            if let Some(alert) = content.alert {
                aps["alert"]["title"] = json!(alert.title);
                if let Some(body) = alert.body {
                    aps["alert"]["body"] = json!(body);
                }
            }
            let actions = content.actions;
            if !actions.is_empty() {
                alondra["actions"] = json!(actions);
                // The extension replaces this bootstrap category with a per-card
                // category after authenticating the encrypted payload.
                aps["category"] = json!("ALONDRA_PUSH");
            }
        }
    }
    json!({"aps": aps, "alondra": alondra})
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
        let client = crate::v2_client_routes::Client::open(domain.clone(), &home, Default::default());
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            run_v2(domain.clone(), home.clone(), push, None, client),
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
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        let client = crate::v2_client_routes::Client::open(
            domain.clone(),
            &home,
            Default::default(),
        );
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
        deliver_one_v2(&domain, &home, &push, None, &client, user.clone())
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
        deliver_one_v2(&domain, &home, &push, None, &client, producer.clone())
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
        let hub = Hub::open(config).unwrap();
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
            serde_json::from_str(include_str!("../../../fixtures/views/push-payloads.json"))
                .unwrap();
        let state = push_fixture_state(fixture["data_epoch"].as_str().unwrap());
        let cases = fixture["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 6);
        for case in cases {
            let id = case["id"].as_str().unwrap();
            let name = case["interaction"].as_str().unwrap();
            assert_eq!(
                user_push_payload(&state, Some(id), Lang::En),
                case["payload"],
                "{name}"
            );
        }
        let english = user_push_payload(&state, Some(push_fixture_id("question")), Lang::En);
        let spanish = user_push_payload(&state, Some(push_fixture_id("question")), Lang::Es);
        assert!(english["alondra"].get("actions_es").is_none());
        assert_eq!(english["alondra"]["actions"][2]["label"], "Need more context");
        assert_eq!(spanish["alondra"]["actions"][2]["label"], "Necesito más contexto");
    }

    const ENCRYPTED_PUSH_FIXTURE: &str = "fixtures/push/encrypted.json";

    /// The Apple notification service extension decrypts this fixture in
    /// NotificationDecoderTests. Regenerate it after a push format change with
    /// `ALONDRA_WRITE_PUSH_FIXTURE=1 bazelisk run //crates/alondrad:alondrad_test -- encrypted_push_fixture`.
    #[test]
    fn encrypted_push_fixture_is_what_the_daemon_sends() {
        use p256::{SecretKey, ecdsa::SigningKey, elliptic_curve::sec1::ToSec1Point};
        let mac_signing = SigningKey::from_slice(&[0x51; 32]).unwrap();
        let phone_agreement = SecretKey::from_slice(&[0x52; 32]).unwrap();
        let (mac_id, device_id) = ("mac-fixture", "phone-fixture");
        let binding = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, [7u8; 32]);
        let state = push_fixture_state("fixture-epoch");
        let payload = user_push_payload(&state, Some(push_fixture_id("question")), Lang::Es);
        let body: Value = serde_json::from_slice(
            &crate::service::seal_push(mac_id, &mac_signing, device_id, &phone_agreement.public_key(),
                &binding, &payload, false).unwrap(),
        ).unwrap();
        let encode = |bytes: &[u8]| base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes);
        let point = mac_signing.verifying_key().to_sec1_point(false);
        let generated = json!({
            "mac_id": mac_id,
            "device_id": device_id,
            "mac_signing_public_key": encode(&point.as_bytes()[1..]),
            "device_agreement_private_key": encode(&phone_agreement.to_bytes()),
            "payload": payload,
            "apns": {
                "aps": {"alert": {"title": "Alondra", "body": "Nueva actividad"}, "mutable-content": 1},
                "alondra": {"envelope": body["envelope"], "push_binding": binding},
            },
        });
        if std::env::var_os("ALONDRA_WRITE_PUSH_FIXTURE").is_some() {
            let root = std::env::var("BUILD_WORKSPACE_DIRECTORY").expect("run through bazelisk run");
            let mut text = serde_json::to_string_pretty(&generated).unwrap();
            text.push('\n');
            std::fs::write(Path::new(&root).join(ENCRYPTED_PUSH_FIXTURE), text).unwrap();
            return;
        }
        let fixture: Value =
            serde_json::from_str(include_str!("../../../fixtures/push/encrypted.json")).unwrap();
        let stale = "push format changed: regenerate fixtures/push/encrypted.json";
        for key in ["mac_id", "device_id", "mac_signing_public_key", "device_agreement_private_key", "payload"] {
            assert_eq!(fixture[key], generated[key], "{stale} ({key})");
        }
        let mut outer = fixture["apns"].clone();
        let envelope: alondra_e2e::EnvelopeV1 =
            serde_json::from_value(outer["alondra"]["envelope"].take()).unwrap();
        let mut expected_outer = generated["apns"].clone();
        expected_outer["alondra"]["envelope"].take();
        assert_eq!(outer, expected_outer, "{stale} (APNs wrapper)");
        let current: alondra_e2e::EnvelopeV1 =
            serde_json::from_value(body["envelope"].clone()).unwrap();
        assert_eq!(
            (&envelope.v, &envelope.route, &envelope.sender, &envelope.recipient, &envelope.kind),
            (&current.v, &current.route, &current.sender, &current.recipient, &current.kind),
            "{stale} (envelope context)"
        );
        let open = |envelope: &alondra_e2e::EnvelopeV1| -> Value {
            let plaintext = envelope
                .open(mac_signing.verifying_key(), &phone_agreement, mac_id, device_id, mac_id,
                    envelope.expires_at_ms)
                .unwrap();
            serde_json::from_slice(&plaintext).unwrap()
        };
        assert_eq!(open(&envelope), open(&current), "{stale} (plaintext)");
    }

    fn push_fixture_state(data_epoch: &str) -> DomainState {
        let mut state = DomainState::new(data_epoch);
        state.tasks.insert("task-a".into(), Task {
            id: "task-a".into(), request_id: "request-a".into(), agent_id: None,
            title: "Split the parser".into(), state: TaskState::NeedsYou, revision: 1,
            started: true, review_prs: false, final_presentation: None, pr_id: None,
            dependencies: BTreeSet::new(), stack: None, documents: Default::default(), archived: false,
        });
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
            let id = InteractionId::from(push_fixture_id(name));
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
                    body: format!("Merge the {name} parser?\n\nThe lexer split is ready and its tests pass."),
                    blocking: false,
                    subject: None,
                    payload,
                    revision: 1,
                    order: 1,
                    withdrawn: false,
                },
            );
        }
        let held = InteractionId::from(push_fixture_id("permission"));
        state.interactions.insert(held.clone(), Interaction {
            attachments: vec![], id: held.clone(), scope: Scope::Task { task_id: "task-a".into() },
            author: Author::System, recipient: Recipient::User,
            body: "Bash: Extract shared waiting-items helper in core views\n\n```\npython3 - <<'EOF'\nimport re\nEOF\n```".into(),
            blocking: true, subject: None,
            payload: InteractionPayload::Question { options: Hold::Permission.options(), answer: None },
            revision: 1, order: 1, withdrawn: false,
        });
        state.holds.insert(held, Hold::Permission);
        state
    }

    fn push_fixture_id(name: &str) -> &'static str {
        match name {
            "question" => "c92d0a11-7c47-4c47-b42b-36b8f22f5120",
            "action_request" => "a019b231-43d1-4aa9-a0c3-5713a8ac2150",
            "information" => "f0ab22dc-2a7a-4c90-b178-d5a91a6338d8",
            "delivery_text" => "hf-weekly-2026-10-01",
            "delivery_pull_request" => "hf-weekly-2026-10-02",
            "permission" => "hold-permission-toolu_01JWScmn532U7ZW6ruo7BjpS",
            _ => panic!("unknown fixture interaction {name}"),
        }
    }

    #[test]
    fn push_payload_fits_1500_bytes_with_four_spanish_options() {
        let mut state = DomainState::new("epoch-1");
        let id = InteractionId::from("i-spanish");
        let options = ["Mantener cookies", "Abrir en Safari", "Usar otra cuenta", "Cancelar"]
            .into_iter()
            .enumerate()
            .map(|(index, label)| QuestionOption {
                id: format!("option-{index}"), label: label.into(), description: None, inputs: vec![],
            })
            .collect();
        state.interactions.insert(id.clone(), Interaction {
            attachments: vec![], id, scope: Scope::Task { task_id: "task-a".into() },
            author: Author::System, recipient: Recipient::User, body: "¿Qué debería hacer?".into(),
            blocking: true, subject: None,
            payload: InteractionPayload::Question { options, answer: None },
            revision: 117, order: 1, withdrawn: false,
        });

        let payload = user_push_payload(&state, Some("i-spanish"), Lang::En);
        let actions = payload["alondra"]["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 4);
        assert_eq!(actions[0]["label"], "Mantener cookies");
        let plaintext = serde_json::to_vec(&json!({
            "push_binding": "A".repeat(43), "payload": payload, "with_sound": true,
        })).unwrap();
        assert!(plaintext.len() <= 1_500, "encrypted push plaintext is {} bytes", plaintext.len());
    }

    #[test]
    fn held_cards_push_allow_deny_or_approve_review_buttons() {
        for (hold, expected) in [
            (Hold::Permission, json!([{"option": "allow", "label": "Allow"}, {"option": "deny", "label": "Deny"}])),
            (Hold::Plan, json!([{"option": "approve", "label": "Approve"}, {"label": "Review", "open": true}])),
        ] {
            let mut state = DomainState::new("epoch-1");
            let id = InteractionId::from("hold-1");
            state.interactions.insert(id.clone(), Interaction {
                attachments: vec![], id: id.clone(), scope: Scope::Task { task_id: "task-a".into() },
                author: Author::System, recipient: Recipient::User, body: "Bash: ls".into(),
                blocking: true, subject: None,
                payload: InteractionPayload::Question { options: hold.options(), answer: None },
                revision: 3, order: 1, withdrawn: false,
            });
            state.holds.insert(id, hold);
            let payload = user_push_payload(&state, Some("hold-1"), Lang::En);
            assert_eq!(payload["alondra"]["kind"], "question", "the app decodes held cards as questions");
            assert_eq!(payload["aps"]["category"], "ALONDRA_PUSH");
            let shown: Vec<Value> = payload["alondra"]["actions"].as_array().unwrap().iter().map(|action| {
                let mut action = action.clone();
                let object = action.as_object_mut().unwrap();
                assert!(object.remove("id").unwrap().as_str().unwrap().ends_with("hold-1@3")
                    || object.get("open").is_some());
                action
            }).collect();
            assert_eq!(Value::from(shown), expected, "{hold:?}");
            if hold == Hold::Permission {
                assert_eq!(payload["aps"]["alert"], json!({"title": "A node needs permission", "body": "Bash: ls"}));
            }
        }
    }
}
