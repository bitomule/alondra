//! What the user does to a pull request that the domain does not keep.
use super::{Client, Reply};
use alondra_core::domain_v2::DomainError;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Nothing {}

impl Client {
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
}
