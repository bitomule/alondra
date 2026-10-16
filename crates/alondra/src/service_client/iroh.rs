//! An account-paired Mac over its iroh endpoint, with the envelope the relay mailbox would carry.
//! The Mac's endpoint comes from the account's Mac directory, and this terminal binds its own
//! endpoint at the relay so the iroh relay lets it in. When iroh gives no answer, the caller sends
//! the same envelope through the mailbox and the Mac's journal runs it once.
use super::{Profile, RELAY, http, now_ms, remaining};
use crate::iroh_client::{self, Dial, Identity, Reply, Session};
use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// The relay binds an iroh endpoint to a device only with this proof from the endpoint's key.
const IROH_NODE_PROOF_DOMAIN: &[u8] = b"alondra-iroh-node-v1\0";

/// This process's iroh connection to each account-paired Mac, by home and Mac ID.
enum Link {
    Open(Arc<Session>),
    Down,
}

static LINKS: Mutex<BTreeMap<(PathBuf, String), Link>> = Mutex::new(BTreeMap::new());

pub(super) enum OverIroh {
    Answered(Result<(u16, Value)>),
    /// Not sent, or lost after it left: the mailbox carries the same envelope.
    Unanswered,
}

impl Profile {
    /// Sends `envelope` over iroh. `learn` says how to dial the Mac when this terminal does not
    /// know yet, or when the way it knew failed.
    pub(super) fn over_iroh(
        &self,
        home: &Path,
        envelope: &[u8],
        request_id: &str,
        timeout: Duration,
        learn: impl FnOnce() -> Option<Dial>,
    ) -> OverIroh {
        let Some(session) = self.iroh_session(home, learn) else { return OverIroh::Unanswered };
        match session.send(envelope, timeout + Duration::from_secs(5)) {
            Ok(Reply::Sealed(sealed)) => OverIroh::Answered(
                serde_json::from_slice(&sealed)
                    .map_err(anyhow::Error::from)
                    .and_then(|response| self.open_response(response, request_id, now_ms()?)),
            ),
            Ok(Reply::Refused { error, detail }) => match error.as_str() {
                "idempotency_conflict" => OverIroh::Answered(Ok((409, json!({"error": "idempotency_conflict"})))),
                "wrong_mac" => {
                    self.set_link(home, Some(Link::Down));
                    OverIroh::Unanswered
                }
                "unauthorized" => OverIroh::Answered(Err(anyhow!(
                    "{} did not accept this terminal; run alondra mac connect {} --repair",
                    self.mac_id,
                    self.mac_id
                ))),
                _ => OverIroh::Answered(Err(anyhow!("{} refused the request: {}", self.mac_id, detail.unwrap_or(error)))),
            },
            Err(_) => {
                self.set_link(home, None);
                OverIroh::Unanswered
            }
        }
    }

    fn set_link(&self, home: &Path, link: Option<Link>) {
        let mut links = LINKS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = (home.to_path_buf(), self.mac_id.clone());
        match link {
            Some(link) => links.insert(key, link),
            None => links.remove(&key),
        };
    }

    fn iroh_session(&self, home: &Path, learn: impl FnOnce() -> Option<Dial>) -> Option<Arc<Session>> {
        let key = (home.to_path_buf(), self.mac_id.clone());
        match LINKS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&key) {
            Some(Link::Open(session)) if !session.is_closed() => return Some(session.clone()),
            Some(Link::Down) => return None,
            _ => {}
        }
        let signing = self.signing().ok()?;
        let identity = Identity { mac_id: &self.mac_id, device_id: &self.device_id, signing: &signing };
        let connect = |dial: &Dial| iroh_client::connect(home, dial, &identity).ok();
        let session = iroh_client::load_dial(home, &self.mac_id).as_ref().and_then(connect).or_else(|| {
            let learned = learn()?;
            let session = connect(&learned)?;
            crate::direct_client::keep_dial(home, &self.mac_id, Some(learned)).ok()?;
            Some(session)
        });
        let Some(session) = session.map(Arc::new) else {
            self.set_link(home, Some(Link::Down));
            return None;
        };
        self.set_link(home, Some(Link::Open(session.clone())));
        Some(session)
    }

    /// How to dial the Mac the account's directory names, after binding this terminal's own
    /// endpoint at the relay; `bind` sends that binding and returns the relay's status.
    pub(super) fn learned_dial(
        &self,
        home: &Path,
        endpoint: Option<&str>,
        relay: Option<&str>,
        bind: impl FnOnce(&[u8]) -> Result<u16>,
    ) -> Option<Dial> {
        let dial = Dial::from_routes(&json!({"iroh_endpoint": endpoint?, "iroh_relay": relay}))?;
        let key = iroh_client::key(home).ok()?;
        let proof = key.sign(&[IROH_NODE_PROOF_DOMAIN, self.device_id.as_bytes()].concat());
        let body = serde_json::to_vec(&json!({
            "iroh_node_id": key.public().to_string(),
            "proof": B64.encode(proof.to_bytes()),
        }))
        .ok()?;
        // Unbound, the iroh relay keeps this terminal out, yet a direct path can still answer.
        let _ = bind(&body);
        Some(dial)
    }

    /// The Mac's iroh endpoint from the account's Mac directory, and the relay's iroh relay.
    pub(super) fn learn_iroh(&self, home: &Path, deadline: Instant) -> Option<Dial> {
        let (endpoint, relay) = self.iroh_directory(deadline).ok()?;
        self.learned_dial(home, endpoint.as_deref(), relay.as_deref(), |body| {
            Ok(self.relay_call("PUT", "/v1/iroh-node", body, deadline)?.0)
        })
    }

    fn iroh_directory(&self, deadline: Instant) -> Result<(Option<String>, Option<String>)> {
        let token = crate::account::token_for_service()?;
        let agent = http(remaining(deadline)?.min(Duration::from_secs(15)));
        let mut response = agent
            .get(format!("{RELAY}/v1/macs"))
            .header("authorization", format!("Bearer {}", token.access_token))
            .call()?;
        let directory: Value = response.body_mut().with_config().limit(128 * 1024).read_json()?;
        let endpoint = directory["macs"]
            .as_array()
            .context("invalid Mac directory")?
            .iter()
            .find(|mac| mac["mac_id"] == self.mac_id.as_str())
            .and_then(|mac| mac["iroh_endpoint"].as_str())
            .map(str::to_owned);
        let mut response = agent.get(format!("{RELAY}/v1/bootstrap")).call()?;
        let boot: Value = response.body_mut().with_config().limit(4096).read_json()?;
        Ok((endpoint, boot["iroh_relay"].as_str().map(str::to_owned)))
    }
}

#[cfg(test)]
#[path = "iroh_tests.rs"]
mod tests;
