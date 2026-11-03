//! Private capabilities for domain writers. Public session/run identifiers are not credentials.
//! Tokens stay in the child environment; only hashes are retained by this registry.
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Principal {
    Agent {
        session_id: String,
        task_binding: Option<String>,
        provider_agent_id: Option<String>,
    },
    Orchestrator {
        generation: u64,
        execution_id: String,
    },
}

/// Deliberately has no Debug/Serialize implementation: never put bearer credentials in logs.
pub struct Capability(String);
impl Capability {
    pub fn expose_for_child_environment(&self) -> &str {
        &self.0
    }
}

#[derive(Default)]
pub struct Authority {
    epoch: String,
    entries: BTreeMap<[u8; 32], Principal>,
    /// What each live orchestrator execution was let change, in order. Recorded under the same
    /// lock that revokes it, so what `end_execution` returns is all it will ever change.
    changes: BTreeMap<String, Vec<String>>,
}
impl Authority {
    pub fn new(epoch: String) -> Self {
        Self {
            epoch,
            entries: BTreeMap::new(),
            changes: BTreeMap::new(),
        }
    }

    /// Rebuild the registry from persisted digests. Entries of another epoch
    /// were fenced by that epoch change and are dropped here.
    pub fn restore(epoch: String, entries: impl IntoIterator<Item = ([u8; 32], Principal)>) -> Self {
        let mut authority = Self::new(epoch);
        authority.entries.extend(entries);
        authority
    }

    /// Digest and principal of every live capability; bearer tokens are never retained.
    pub fn entries(&self) -> impl Iterator<Item = (&[u8; 32], &Principal)> {
        self.entries.iter()
    }

    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    pub fn issue(&mut self, principal: Principal) -> Result<Capability, &'static str> {
        match &principal {
            Principal::Agent {
                session_id,
                task_binding,
                provider_agent_id,
            } => {
                if session_id.is_empty()
                    || task_binding.as_ref().is_some_and(|s| s.is_empty())
                    || provider_agent_id.as_ref().is_some_and(|s| s.is_empty())
                    || (provider_agent_id.is_some() && task_binding.is_none())
                {
                    return Err("invalid agent capability binding");
                }
            }
            Principal::Orchestrator { execution_id, .. } if execution_id.is_empty() => {
                return Err("invalid execution binding");
            }
            _ => (),
        }
        if self.epoch.is_empty() {
            return Err("data epoch is required");
        }
        if self.entries.len() >= 4096 {
            return Err("capability capacity reached");
        }
        let mut bytes = [0u8; 32];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| "secure entropy unavailable")?;
        let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let digest = Sha256::digest(token.as_bytes()).into();
        self.entries.insert(digest, principal);
        Ok(Capability(token))
    }

    /// The caller additionally checks live assignment/generation under its command lock.
    /// An invalid bearer never falls back to the loopback user's authority.
    pub fn resolve(&self, epoch: &str, bearer: &str) -> Result<Principal, &'static str> {
        if epoch != self.epoch {
            return Err("stale data epoch");
        }
        if bearer.len() != 64
            || !bearer
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid domain capability");
        }
        let digest: [u8; 32] = Sha256::digest(bearer.as_bytes()).into();
        self.entries
            .get(&digest)
            .cloned()
            .ok_or("invalid or revoked domain capability")
    }

    /// Drops a capability this registry issued and never handed out.
    pub fn forget(&mut self, capability: &Capability) {
        let digest: [u8; 32] = Sha256::digest(capability.0.as_bytes()).into();
        self.entries.remove(&digest);
    }

    pub fn revoke_agent(&mut self, session_id: &str) {
        self.entries.retain(|_, principal| !matches!(principal, Principal::Agent { session_id: id, .. } if id == session_id));
    }

    pub fn revoke_execution(&mut self, execution_id: &str) {
        self.entries.retain(|_, principal| !matches!(principal, Principal::Orchestrator { execution_id: id, .. } if id == execution_id));
        self.changes.remove(execution_id);
    }

    /// Notes that `principal`, if it is an orchestrator execution, was let change `what`.
    pub fn record_change(&mut self, principal: &Principal, what: &str) {
        if let Principal::Orchestrator { execution_id, .. } = principal {
            self.changes.entry(execution_id.clone()).or_default().push(what.to_owned());
        }
    }

    /// Revokes the execution and returns what it was let change: nothing it does after this can.
    pub fn end_execution(&mut self, execution_id: &str) -> Vec<String> {
        let changes = self.changes.remove(execution_id).unwrap_or_default();
        self.revoke_execution(execution_id);
        changes
    }

    pub fn change_epoch(&mut self, epoch: String) {
        self.entries.clear();
        self.changes.clear();
        self.epoch = epoch;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn agent(id: &str) -> Principal {
        Principal::Agent {
            session_id: id.into(),
            task_binding: None,
            provider_agent_id: None,
        }
    }
    #[test]
    fn public_identifiers_are_not_capabilities_and_revoke_is_scoped() {
        let mut authority = Authority::new("epoch".into());
        let first = authority.issue(agent("session-one")).unwrap();
        let second = authority.issue(agent("session-two")).unwrap();
        assert!(authority.resolve("epoch", "session-one").is_err());
        assert!(authority.resolve("epoch", &"0".repeat(64)).is_err());
        assert_eq!(
            authority
                .resolve("epoch", first.expose_for_child_environment())
                .unwrap(),
            agent("session-one")
        );
        authority.revoke_agent("session-one");
        assert!(
            authority
                .resolve("epoch", first.expose_for_child_environment())
                .is_err()
        );
        assert_eq!(
            authority
                .resolve("epoch", second.expose_for_child_environment())
                .unwrap(),
            agent("session-two")
        );
    }
    #[test]
    fn an_execution_ends_with_what_it_was_let_change_and_nothing_after() {
        let mut authority = Authority::new("epoch".into());
        let run = |id: &str| Principal::Orchestrator { generation: 0, execution_id: id.into() };
        authority.issue(run("run-1")).unwrap();
        authority.record_change(&run("run-1"), "create_task");
        authority.record_change(&agent("session"), "ask_question");
        authority.record_change(&run("run-2"), "launch");
        authority.record_change(&run("run-1"), "send");
        assert_eq!(authority.end_execution("run-1"), ["create_task", "send"]);
        assert!(authority.end_execution("run-1").is_empty(), "taken once");
        authority.revoke_execution("run-2");
        assert!(authority.end_execution("run-2").is_empty(), "binding an execution again starts from nothing");
    }
    #[test]
    fn epoch_restart_and_execution_revocation_fence_old_tokens() {
        let mut authority = Authority::new("epoch".into());
        let principal = Principal::Orchestrator {
            generation: 3,
            execution_id: "public-run".into(),
        };
        let token = authority.issue(principal.clone()).unwrap();
        assert_eq!(
            authority
                .resolve("epoch", token.expose_for_child_environment())
                .unwrap(),
            principal
        );
        assert!(
            Authority::new("epoch".into())
                .resolve("epoch", token.expose_for_child_environment())
                .is_err()
        );
        authority.revoke_execution("public-run");
        assert!(
            authority
                .resolve("epoch", token.expose_for_child_environment())
                .is_err()
        );
        let token = authority.issue(agent("session")).unwrap();
        authority.change_epoch("new-epoch".into());
        assert!(
            authority
                .resolve("epoch", token.expose_for_child_environment())
                .is_err()
        );
        assert!(
            authority
                .resolve("new-epoch", token.expose_for_child_environment())
                .is_err()
        );
    }
    #[test]
    fn provider_child_requires_explicit_task_binding() {
        let mut authority = Authority::new("epoch".into());
        assert!(
            authority
                .issue(Principal::Agent {
                    session_id: "parent".into(),
                    task_binding: None,
                    provider_agent_id: Some("child".into())
                })
                .is_err()
        );
        let principal = Principal::Agent {
            session_id: "parent".into(),
            task_binding: Some("task-one".into()),
            provider_agent_id: Some("child".into()),
        };
        let token = authority.issue(principal.clone()).unwrap();
        assert_eq!(
            authority
                .resolve("epoch", token.expose_for_child_environment())
                .unwrap(),
            principal
        );
    }
}
