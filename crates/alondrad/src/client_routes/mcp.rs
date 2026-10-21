//! The routes of the card that tasks waiting for an MCP server wait on: Connect, where the browser
//! came back to, Reconnect and Go on without it. They reach the daemon on this Mac or inside the
//! relay's end-to-end envelope, like every write. Where the browser came back to carries a
//! one-time code: it is handed to Claude in memory and never kept, logged or echoed back.
use super::{Client, Reply};
use crate::mcp_waits::Opened;
use alondra_core::domain::{Actor, Command, CommandEnvelope, DomainError, InteractionId, MCP_GO_ON_WITHOUT, PROTOCOL_VERSION};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Connect,
    Return,
    Reconnect,
    Without,
}

impl Step {
    pub(crate) fn of(name: &str) -> Option<Self> {
        Some(match name {
            "connect" => Self::Connect,
            "return" => Self::Return,
            "reconnect" => Self::Reconnect,
            "without" => Self::Without,
            _ => return None,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Return {
    address: String,
}

fn invalid() -> Reply {
    Reply::error(DomainError::new("invalid_input", "invalid request body"))
}

impl Client {
    pub(super) async fn mcp_sign_in(
        &self,
        user_id: &str,
        card: &str,
        step: Step,
        body: &Value,
        authorize: &(dyn Fn() -> anyhow::Result<()> + Sync),
    ) -> anyhow::Result<Reply> {
        let card = InteractionId::from(card);
        if step == Step::Return {
            let Ok(back) = serde_json::from_value::<Return>(body.clone()) else { return Ok(invalid()) };
            authorize()?;
            return Ok(match crate::mcp_waits::give_back(&self.domain, &card, back.address).await {
                Ok(()) => Reply::ok(json!({"ok": true})),
                Err(error) => Reply::error(error),
            });
        }
        if serde_json::from_value::<Empty>(body.clone()).is_err() {
            return Ok(invalid());
        }
        authorize()?;
        Ok(match step {
            Step::Connect => match crate::mcp_waits::connect(&self.domain, &card).await {
                Ok(Opened::Browser { server, url, returns_to_claude }) => {
                    Reply::ok(json!({"server": server, "url": url, "returns_to_claude": returns_to_claude}))
                }
                Ok(Opened::Connected { server }) => Reply::ok(json!({"server": server, "connected": true})),
                Err(error) => Reply::error(error),
            },
            Step::Reconnect => match crate::mcp_waits::reconnect(&self.domain, &card) {
                Ok(reconnecting) => Reply::ok(json!({"reconnecting": reconnecting})),
                Err(error) => Reply::error(error),
            },
            Step::Without => self.go_on_without(user_id, &card, authorize)?,
            Step::Return => unreachable!("handled above"),
        })
    }

    fn go_on_without(&self, user_id: &str, card: &InteractionId, authorize: &(dyn Fn() -> anyhow::Result<()> + Sync)) -> anyhow::Result<Reply> {
        let state = self.domain.state()?;
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("mcp-without-{card}").into(),
            expected_revision: state.revision,
            command: Command::AnswerQuestion {
                attachments: vec![],
                interaction_id: card.clone(),
                option_id: MCP_GO_ON_WITHOUT.into(),
                inputs: Default::default(),
            },
        };
        let outcome = self.domain.execute(&Actor::User { user_id: user_id.to_owned() }, &envelope, None, || {
            authorize().map_err(|_| DomainError::new("forbidden", "device approval was revoked").into())
        });
        match outcome {
            Ok(_) => Ok(Reply::ok(json!({"ok": true}))),
            Err(crate::domain_store::StoreError::Domain(error)) => Ok(Reply::error(error)),
            Err(crate::domain_store::StoreError::Storage(error)) => Err(error),
        }
    }
}
