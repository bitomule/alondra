//! The user, as the iPhone acts for them: requests, server-driven views and their actions.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{
    phone::Phone,
    sandbox::{Sandbox, Target, hex, random},
};

pub struct User<'a> {
    pub phone: Phone<'a>,
    pub target: Target,
    /// Whether it talks to the Mac over its direct listener instead of the relay.
    pub direct: bool,
}

impl<'a> User<'a> {
    /// A phone paired with Mac `mac` with the account (reached over iroh), or directly.
    pub fn paired(sandbox: &'a Sandbox, name: &str, mac: usize, direct: bool) -> Result<Self> {
        let phone = Phone::open(sandbox, name)?;
        let target = sandbox.mac(mac)?;
        if direct {
            if phone.direct_link(&target).is_err() {
                phone.pair_direct(&target)?;
            }
        } else if phone.link(&target).is_err() {
            phone.pair(&target)?;
        }
        Ok(Self { phone, target, direct })
    }

    /// The Mac's answer: its status and body.
    pub fn send(&self, method: &str, path: &str, body: Option<Value>) -> Result<(u16, Value)> {
        let reply = if self.direct {
            self.phone.request_direct(&self.target, method, path, body, None)?
        } else {
            self.phone.request(&self.target, method, path, body, None)?
        };
        let status = reply.response["status"].as_u64().unwrap_or(0) as u16;
        Ok((status, reply.response["body"].clone()))
    }

    fn ok(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let (status, body) = self.send(method, path, body)?;
        if !(200..300).contains(&status) {
            bail!("{method} {path} answered {status}: {body}");
        }
        Ok(body)
    }

    pub fn request(&self, request_id: &str, text: &str) -> Result<Value> {
        self.ok("POST", "/requests", Some(json!({"request_id": request_id, "text": text})))
    }

    pub fn view(&self, key: &str) -> Result<Value> {
        self.view_on(key, "ios")
    }

    /// The view `key` as the app on `client` reads it.
    pub fn view_on(&self, key: &str, client: &str) -> Result<Value> {
        self.ok("GET", &format!("/views/{key}?protocol=1&client={client}&lang=en"), None)
    }

    /// Presses an action of a view, as the app does: with the data epoch the view carried.
    pub fn press(&self, action_id: &str, option_id: Option<&str>, text: Option<&str>) -> Result<Value> {
        let epoch = self.view("home")?["data_epoch"].as_str().context("home has no data_epoch")?.to_owned();
        let mut input = json!({});
        if let Some(text) = text {
            input["text"] = json!(text);
        }
        let body = self.ok("POST", "/actions", Some(json!({
            "protocol": 1, "command_id": uuid()?, "data_epoch": epoch, "client": "ios", "lang": "en",
            "action_id": action_id, "option_id": option_id, "input": input,
        })))?;
        if body["outcome"] != "applied" {
            bail!("{action_id} was not applied: {body}");
        }
        Ok(body)
    }

    /// Presses an action and returns the Mac's answer whatever it is: for an action that must be
    /// refused.
    pub fn try_press(&self, action_id: &str, option_id: Option<&str>) -> Result<(u16, Value)> {
        let epoch = self.view("home")?["data_epoch"].as_str().context("home has no data_epoch")?.to_owned();
        self.send("POST", "/actions", Some(json!({
            "protocol": 1, "command_id": uuid()?, "data_epoch": epoch, "client": "ios", "lang": "en",
            "action_id": action_id, "option_id": option_id, "input": {},
        })))
    }

    /// The ids of the cards waiting in Inbox.
    pub fn inbox(&self) -> Result<Vec<String>> {
        let home = self.view("home")?;
        Ok(home["sections"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|section| section["card_ids"].as_array().cloned().unwrap_or_default())
            .filter_map(|id| id.as_str().map(str::to_owned))
            .collect())
    }

    /// The enabled action of `view` whose id starts with `prefix`.
    pub fn action(&self, view: &Value, prefix: &str) -> Option<Value> {
        let mut found = vec![];
        collect(view, &mut |item| {
            item.get("id").and_then(Value::as_str).is_some_and(|id| id.starts_with(prefix))
                && item.get("label").is_some()
                && item.get("enabled") != Some(&Value::Bool(false))
        }, &mut found);
        found.into_iter().next()
    }

    /// The enabled action of `view` labelled `label`.
    pub fn action_labeled(&self, view: &Value, label: &str) -> Option<Value> {
        let mut found = vec![];
        collect(view, &mut |item| {
            item.get("label").and_then(Value::as_str) == Some(label)
                && item.get("id").and_then(Value::as_str).is_some()
                && item.get("enabled") != Some(&Value::Bool(false))
        }, &mut found);
        found.into_iter().next()
    }

    /// A task's state as its view's header reads it, without the fact after " · ".
    pub fn task_state(&self, task: &str) -> Result<String> {
        let view = self.view(&format!("task:{task}"))?;
        let mut facts = vec![];
        collect(&view, &mut |item| item.get("label").and_then(Value::as_str) == Some("State") && item.get("value").is_some(), &mut facts);
        let value = facts.first().and_then(|fact| fact["value"].as_str()).unwrap_or_default();
        Ok(value.split(" · ").next().unwrap_or_default().to_owned())
    }
}

/// Every object in `value` that `wanted` picks, depth first.
pub fn collect(value: &Value, wanted: &mut dyn FnMut(&Value) -> bool, out: &mut Vec<Value>) {
    match value {
        Value::Object(map) => {
            if wanted(value) {
                out.push(value.clone());
            }
            for child in map.values() {
                collect(child, wanted, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect(item, wanted, out)),
        _ => {}
    }
}

/// Whether any string in `value` contains `text`.
pub fn mentions(value: &Value, text: &str) -> bool {
    match value {
        Value::String(s) => s.contains(text),
        Value::Object(map) => map.values().any(|child| mentions(child, text)),
        Value::Array(items) => items.iter().any(|child| mentions(child, text)),
        _ => false,
    }
}

pub fn uuid() -> Result<String> {
    let h = hex(&random::<16>()?);
    Ok(format!("{}-{}-4{}-8{}-{}", &h[0..8], &h[8..12], &h[13..16], &h[17..20], &h[20..32]))
}
