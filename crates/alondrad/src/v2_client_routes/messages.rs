//! A message from the user to an agent or the orchestrator. It is one domain command, so the
//! message, the read mark of the information it replies to and its outbox row commit together.
use super::{Client, Reply};
use crate::domain_store::{StoreError, TransportIdentity};
use alondra_core::domain_v2::{
    Actor, AgentId, AttachmentRef, Command, CommandEnvelope, DomainError, MessageKind, PROTOCOL_VERSION, Recipient,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    text: String,
    #[serde(default)]
    attachments: Vec<AttachmentRef>,
    #[serde(default = "plain")]
    kind: MessageKind,
    #[serde(default)]
    mark_read: bool,
    #[serde(default)]
    interaction_id: Option<String>,
    #[serde(default)]
    pr_id: Option<String>,
}

fn plain() -> MessageKind {
    MessageKind::Message
}

impl Client {
    /// `agent: None` is the orchestrator. The answer names the outbox message and, once an
    /// attempt to hand it over exists, what happened to it.
    pub(super) fn send_message(
        &self,
        user_id: &str,
        agent: Option<&str>,
        body: &Value,
        transport: Option<TransportIdentity<'_>>,
        authorize: &dyn Fn() -> anyhow::Result<()>,
    ) -> anyhow::Result<Reply> {
        let Ok(body) = serde_json::from_value::<Body>(body.clone()) else {
            return Ok(Reply::error(DomainError::new("invalid_input", "invalid message")));
        };
        let state = self.domain.state()?;
        let recipient = match agent {
            Some(id) => Recipient::Agent { agent_id: AgentId(id.to_owned()) },
            None => Recipient::Orchestrator,
        };
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch,
            command_id: format!("message-{}", crate::auth::random_hex(16)).into(),
            expected_revision: 0,
            command: Command::SendMessage {
                message_id: format!("message-{}", crate::auth::random_hex(16)).into(),
                recipient,
                purpose: body.kind,
                text: body.text,
                attachments: body.attachments,
                interaction_id: body.interaction_id.map(Into::into),
                pr_id: body.pr_id.map(Into::into),
                mark_read: body.mark_read,
            },
        };
        let outcome = self.domain.execute(&Actor::User { user_id: user_id.to_owned() }, &envelope, transport, || {
            authorize().map_err(|_| DomainError::new("forbidden", "device approval was revoked").into())
        });
        match outcome {
            Ok(outcome) => {
                let store = self.domain.store.lock().expect("domain store poisoned");
                let Some(&message_id) = store.outbox_ids(outcome.revision)?.first() else {
                    return Ok(Reply::error(DomainError::new("storage_unavailable", "the message has no outbox row")));
                };
                let mut answer = json!({"message_id": message_id});
                if let Some(state) = store.delivery_state(message_id)? {
                    answer["delivery_state"] = json!(state);
                }
                Ok(Reply::ok(answer))
            }
            Err(StoreError::Domain(error)) => Ok(Reply::error(error)),
            Err(StoreError::Storage(error)) => Err(error),
        }
    }
}
