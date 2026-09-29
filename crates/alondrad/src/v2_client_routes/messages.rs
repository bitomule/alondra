//! A message from the user to an agent or the orchestrator. It is one domain command, so the
//! message, the read mark of the information it replies to and its outbox row commit together.
use super::{Client, Reply};
use crate::domain_store::{StoreError, TransportIdentity};
use alondra_core::domain_v2::{
    Actor, AgentId, AttachmentRef, Command, CommandEnvelope, DomainError, MessageKind, PROTOCOL_VERSION, PrState, Recipient,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Nothing {}

fn plain() -> MessageKind {
    MessageKind::Message
}

struct Message {
    recipient: Recipient,
    purpose: MessageKind,
    text: String,
    attachments: Vec<AttachmentRef>,
    interaction_id: Option<String>,
    pr_id: Option<String>,
    mark_read: bool,
}

type Authorize<'a> = &'a (dyn Fn() -> anyhow::Result<()> + Sync);

impl Client {
    /// `agent: None` is the orchestrator. The answer names the outbox message and, once an
    /// attempt to hand it over exists, what happened to it.
    pub(super) fn send_message(
        &self,
        user_id: &str,
        agent: Option<&str>,
        body: &Value,
        transport: Option<TransportIdentity<'_>>,
        authorize: Authorize<'_>,
    ) -> anyhow::Result<Reply> {
        let Ok(body) = serde_json::from_value::<Body>(body.clone()) else {
            return Ok(Reply::error(DomainError::new("invalid_input", "invalid message")));
        };
        let recipient = match agent {
            Some(id) => Recipient::Agent { agent_id: AgentId(id.to_owned()) },
            None => Recipient::Orchestrator,
        };
        let message = Message {
            recipient,
            purpose: body.kind,
            text: body.text,
            attachments: body.attachments,
            interaction_id: body.interaction_id,
            pr_id: body.pr_id,
            mark_read: body.mark_read,
        };
        Ok(match self.deliver(user_id, message, transport, authorize)? {
            Ok(message_id) => Reply::ok(self.message_answer(message_id)?),
            Err(error) => Reply::error(error),
        })
    }

    /// Asks the agent that owns a pull request to resolve its conflicts with its base.
    pub(super) fn resolve_conflicts(
        &self,
        user_id: &str,
        pr_id: &str,
        body: &Value,
        transport: Option<TransportIdentity<'_>>,
        authorize: Authorize<'_>,
    ) -> anyhow::Result<Reply> {
        if serde_json::from_value::<Nothing>(body.clone()).is_err() {
            return Ok(Reply::error(DomainError::new("invalid_input", "invalid request")));
        }
        let state = self.domain.state()?;
        let Some(pr) = state.pull_requests.get(&pr_id.into()) else {
            return Ok(Reply::error(DomainError::new("not_found", "PR does not exist")));
        };
        let url = format!("https://{}/{}/pull/{}", pr.host, pr.repository, pr.number);
        if pr.state != PrState::Open {
            return Ok(Reply::error(DomainError::new("pr_not_open", format!("{url} is no longer open"))));
        }
        let Some(agent) = state.tasks.get(&pr.task_id).and_then(|task| task.agent_id.clone()) else {
            return Ok(Reply::error(DomainError::new("not_assigned", "the PR's task has no agent to ask")));
        };
        let text = format!(
            "The user asks you to resolve the conflicts of {url} with {base}: bring {base} into the branch, resolve them, \
             run the tests and push. The PR card goes back to the user once GitHub can merge it.",
            base = pr.base
        );
        let message = Message {
            recipient: Recipient::Agent { agent_id: agent },
            purpose: MessageKind::Message,
            text,
            attachments: vec![],
            interaction_id: None,
            pr_id: None,
            mark_read: false,
        };
        Ok(match self.deliver(user_id, message, transport, authorize)? {
            Ok(_) => Reply::ok(json!({})),
            Err(error) => Reply::error(error),
        })
    }

    /// The user sets a pull request aside: a preference of theirs that the domain does not keep.
    pub(super) fn set_aside(&self, pr_id: &str, body: &Value) -> anyhow::Result<Reply> {
        if serde_json::from_value::<Nothing>(body.clone()).is_err() {
            return Ok(Reply::error(DomainError::new("invalid_input", "invalid request")));
        }
        if !self.domain.state()?.pull_requests.contains_key(&pr_id.into()) {
            return Ok(Reply::error(DomainError::new("not_found", "PR does not exist")));
        }
        self.sidecar.set_aside(pr_id, true)?;
        Ok(Reply::ok(json!({})))
    }

    /// The outbox message the command produced, or the domain's refusal.
    fn deliver(
        &self,
        user_id: &str,
        message: Message,
        transport: Option<TransportIdentity<'_>>,
        authorize: Authorize<'_>,
    ) -> anyhow::Result<Result<i64, DomainError>> {
        let state = self.domain.state()?;
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch,
            command_id: format!("message-{}", crate::auth::random_hex(16)).into(),
            expected_revision: 0,
            command: Command::SendMessage {
                message_id: format!("message-{}", crate::auth::random_hex(16)).into(),
                recipient: message.recipient,
                purpose: message.purpose,
                text: message.text,
                attachments: message.attachments,
                interaction_id: message.interaction_id.map(Into::into),
                pr_id: message.pr_id.map(Into::into),
                mark_read: message.mark_read,
            },
        };
        let outcome = self.domain.execute(&Actor::User { user_id: user_id.to_owned() }, &envelope, transport, || {
            authorize().map_err(|_| DomainError::new("forbidden", "device approval was revoked").into())
        });
        match outcome {
            Ok(outcome) => {
                let store = self.domain.store.lock().expect("domain store poisoned");
                match store.outbox_ids(outcome.revision)?.first() {
                    Some(&id) => Ok(Ok(id)),
                    None => Ok(Err(DomainError::new("storage_unavailable", "the message has no outbox row"))),
                }
            }
            Err(StoreError::Domain(error)) => Ok(Err(error)),
            Err(StoreError::Storage(error)) => Err(error),
        }
    }

    fn message_answer(&self, message_id: i64) -> anyhow::Result<Value> {
        let mut answer = json!({"message_id": message_id});
        let store = self.domain.store.lock().expect("domain store poisoned");
        if let Some(state) = store.delivery_state(message_id)? {
            answer["delivery_state"] = json!(state);
        }
        Ok(answer)
    }
}
