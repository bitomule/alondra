//! The identity of a settings change the journal commits with the options file.
use alondra_domain_rules::{Actor, CommandEnvelope};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionIdentity {
    pub actor: String,
    pub command_id: String,
    pub data_epoch: String,
    pub command_digest: [u8; 32],
    pub previous_digest: Option<[u8; 32]>,
    pub target_digest: [u8; 32],
}

impl ProjectionIdentity {
    pub fn new(actor: &Actor, command: &CommandEnvelope, previous: Option<&str>, target: &str) -> Result<Self> {
        let actor = match actor { Actor::Orchestrator { .. } => serde_json::json!({"kind":"orchestrator"}), _ => serde_json::to_value(actor)? };
        Ok(Self { actor: String::from_utf8(super::canonical::bytes(&actor)?)?, command_id: command.command_id.0.clone(),
            data_epoch: command.data_epoch.0.clone(), command_digest: super::canonical::digest(command)?, previous_digest: previous.map(|text| Sha256::digest(text.as_bytes()).into()), target_digest: Sha256::digest(target.as_bytes()).into() })
    }
    pub fn key(&self) -> Result<Vec<u8>> { Ok(super::canonical::bytes(self)?) }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingProjection {
    pub identity: ProjectionIdentity,
    pub previous: Option<String>,
    pub target: String,
}
