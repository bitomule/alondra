use std::time::Duration;

use alondra_core::{Machine, Message, Node, Question};
use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Deserialize, Clone, Default)]
pub struct Overview {
    pub tick: u64,
    pub nodes: Vec<Node>,
    pub open_questions: Vec<Question>,
    #[serde(default)]
    pub news: Vec<Node>,
    #[serde(default)]
    pub orchestrator_messages: Vec<Message>,
    pub machine: Machine,
}

pub struct Client {
    base: String,
    agent: ureq::Agent,
}

impl Client {
    pub fn new(timeout: Duration) -> Self {
        let port = std::env::var("ALONDRA_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(alondra_core::DEFAULT_PORT);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .build()
            .into();
        Client { base: format!("http://127.0.0.1:{port}"), agent }
    }

    fn check(mut resp: ureq::http::Response<ureq::Body>) -> Result<(u16, String)> {
        let status = resp.status().as_u16();
        let body = resp.body_mut().read_to_string().unwrap_or_default();
        if status >= 400 {
            let msg = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
                .unwrap_or(body);
            return Err(anyhow!("alondrad respondió {status}: {msg}"));
        }
        Ok((status, body))
    }

    pub fn get_text(&self, path: &str) -> Result<(u16, String)> {
        let resp = self.agent.get(format!("{}{path}", self.base)).call().context("alondrad no responde (¿está arrancado?)")?;
        Self::check(resp)
    }

    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let (_, body) = self.get_text(path)?;
        Ok(serde_json::from_str(&body)?)
    }

    pub fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let resp = self
            .agent
            .post(format!("{}{path}", self.base))
            .send_json(body)
            .context("alondrad no responde (¿está arrancado?)")?;
        let (_, text) = Self::check(resp)?;
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    pub fn overview(&self) -> Result<Overview> {
        self.get("/v1/overview")
    }
}
