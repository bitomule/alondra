use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alondra_core::{Question, QuestionOrigin, Report, ReportKind, now_millis};
use anyhow::{Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::hub::Hub;

const JWT_LIFETIME_MS: i64 = 50 * 60 * 1000;
const POLL: Duration = Duration::from_secs(3);

#[derive(Deserialize)]
struct ApnsConfig {
    key_path: PathBuf,
    key_id: String,
    team_id: String,
    topic: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Registration {
    pub token: String,
    pub environment: String,
    #[serde(default)]
    pub quiet_start: Option<String>,
    #[serde(default)]
    pub quiet_end: Option<String>,
    pub updated_at: i64,
}

pub struct Push {
    path: PathBuf,
    registrations: Mutex<HashMap<String, Registration>>,
    apns: Option<Apns>,
}

struct Apns {
    key: SigningKey,
    key_id: String,
    team_id: String,
    topic: String,
    client: reqwest::Client,
    jwt: Mutex<Option<(String, i64)>>,
}

impl Push {
    pub fn open(home: &Path) -> Result<Self> {
        let path = home.join("push.json");
        let registrations = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let apns = match std::fs::read_to_string(home.join("apns.json")) {
            Ok(text) => Some(Apns::load(serde_json::from_str(&text).context("apns.json ilegible")?)?),
            Err(_) => None,
        };
        if apns.is_none() {
            eprintln!("alondrad: no ~/.alondra/apns.json, notifications are not sent");
        }
        Ok(Push { path, registrations: Mutex::new(registrations), apns })
    }

    pub fn register(&self, device: &str, reg: Registration) -> Result<()> {
        let mut regs = self.registrations.lock().expect("push lock poisoned");
        regs.retain(|d, r| d == device || r.token != reg.token);
        regs.insert(device.to_string(), reg);
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&*regs)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn enabled(&self) -> bool {
        self.apns.is_some()
    }

    fn registrations(&self) -> Vec<Registration> {
        self.registrations.lock().expect("push lock poisoned").values().cloned().collect()
    }

    fn forget_token(&self, token: &str) {
        let mut regs = self.registrations.lock().expect("push lock poisoned");
        regs.retain(|_, r| r.token != token);
    }

    pub async fn check(&self) -> Result<(u16, String)> {
        let apns = self.apns.as_ref().context("APNs not configured")?;
        apns.send(&"0".repeat(64), "sandbox", &json!({"aps": {"alert": "check"}}), 10).await
    }

    pub async fn broadcast(&self, payload: &Value, with_sound: bool) {
        let Some(apns) = &self.apns else { return };
        for reg in self.registrations() {
            let mut body = payload.clone();
            let quiet = in_quiet_hours(reg.quiet_start.as_deref(), reg.quiet_end.as_deref(), local_minutes());
            let level = if quiet { "passive" } else { "active" };
            body["aps"]["interruption-level"] = json!(level);
            if (quiet || !with_sound)
                && let Some(aps) = body["aps"].as_object_mut()
            {
                aps.remove("sound");
            }
            let priority = if quiet { 5 } else { 10 };
            match apns.send(&reg.token, &reg.environment, &body, priority).await {
                Ok((200, _)) => {}
                Ok((410, _)) | Ok((400, _)) => {
                    eprintln!("alondrad: APNs rejects a token, forgetting it");
                    self.forget_token(&reg.token);
                }
                Ok((status, text)) => eprintln!("alondrad: APNs {status}: {text}"),
                Err(e) => eprintln!("alondrad: APNs does not respond: {e}"),
            }
        }
    }
}

impl Apns {
    fn load(cfg: ApnsConfig) -> Result<Self> {
        let pem = std::fs::read_to_string(&cfg.key_path).with_context(|| format!("cannot read {}", cfg.key_path.display()))?;
        let key = SigningKey::from_pkcs8_pem(&pem).map_err(|e| anyhow::anyhow!("invalid .p8 key: {e}"))?;
        let client = reqwest::Client::builder().http2_prior_knowledge().timeout(Duration::from_secs(15)).build()?;
        Ok(Apns { key, key_id: cfg.key_id, team_id: cfg.team_id, topic: cfg.topic, client, jwt: Mutex::new(None) })
    }

    fn jwt(&self) -> String {
        let mut cached = self.jwt.lock().expect("jwt lock poisoned");
        if let Some((token, at)) = cached.as_ref()
            && now_millis() - at < JWT_LIFETIME_MS
        {
            return token.clone();
        }
        let header = B64URL.encode(json!({"alg": "ES256", "kid": self.key_id}).to_string());
        let claims = B64URL.encode(json!({"iss": self.team_id, "iat": now_millis() / 1000}).to_string());
        let unsigned = format!("{header}.{claims}");
        let signature: Signature = self.key.sign(unsigned.as_bytes());
        let token = format!("{unsigned}.{}", B64URL.encode(signature.to_bytes()));
        *cached = Some((token.clone(), now_millis()));
        token
    }

    async fn send(&self, token: &str, environment: &str, body: &Value, priority: u8) -> Result<(u16, String)> {
        let host = if environment == "production" { "api.push.apple.com" } else { "api.sandbox.push.apple.com" };
        let resp = self
            .client
            .post(format!("https://{host}/3/device/{token}"))
            .bearer_auth(self.jwt())
            .header("apns-topic", &self.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", priority.to_string())
            .json(body)
            .send()
            .await?;
        let status = resp.status().as_u16();
        Ok((status, resp.text().await.unwrap_or_default()))
    }
}

fn local_minutes() -> u32 {
    let t: libc::time_t = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    (tm.tm_hour * 60 + tm.tm_min) as u32
}

fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.split_once(':')?;
    Some(h.parse::<u32>().ok()? * 60 + m.parse::<u32>().ok()?)
}

pub fn in_quiet_hours(start: Option<&str>, end: Option<&str>, now: u32) -> bool {
    let (Some(s), Some(e)) = (start.and_then(parse_hhmm), end.and_then(parse_hhmm)) else { return false };
    if s <= e { now >= s && now < e } else { now >= s || now < e }
}

fn question_payload(q: &Question, badge: usize) -> Value {
    let category = match q.origin {
        QuestionOrigin::Permission => "ALONDRA_PERMISSION",
        QuestionOrigin::Plan => "ALONDRA_PLAN",
        _ => "ALONDRA_QUESTION",
    };
    let origin = match q.origin {
        QuestionOrigin::Ask => "ask",
        QuestionOrigin::AskUserQuestion => "ask_user_question",
        QuestionOrigin::Permission => "permission",
        QuestionOrigin::Plan => "plan",
        QuestionOrigin::Classifier => "classifier",
    };
    let who = q.node_name.clone().unwrap_or_else(|| "a node".into());
    let options = q.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>().join(" · ");
    let body = if options.is_empty() { who.clone() } else { format!("{who} · {options}") };
    json!({
        "aps": {"alert": {"title": q.title, "body": body}, "sound": "default", "category": category,
                "thread-id": q.session_id, "badge": badge, "mutable-content": 1},
        "alondra": {"kind": "question", "id": q.id, "origin": origin,
                    "options": q.options.iter().map(|o| o.label.clone()).collect::<Vec<_>>()}
    })
}

fn report_payload(r: &Report, badge: usize) -> Value {
    let kind = r.kind.label();
    let who = r.node_name.clone().unwrap_or_else(|| "a node".into());
    json!({
        "aps": {"alert": {"title": r.title, "body": format!("{kind} · {who}")}, "category": "ALONDRA_READ",
                "thread-id": r.session_id, "badge": badge},
        "alondra": {"kind": "report", "id": r.id, "report_kind": kind_key(r.kind)}
    })
}

fn kind_key(k: ReportKind) -> &'static str {
    match k {
        ReportKind::Notice => "notice",
        ReportKind::Document => "document",
        ReportKind::Progress => "progress",
        ReportKind::Final => "final",
    }
}

pub async fn run(hub: Arc<Hub>, push: Arc<Push>) {
    if !push.enabled() {
        return;
    }
    let (mut last_q, mut last_r) = hub.read(|v| {
        (v.questions.keys().next_back().copied().unwrap_or(0), v.reports.keys().next_back().copied().unwrap_or(0))
    });
    loop {
        hub.wait_event(POLL).await;
        let (questions, reports, badge, max_q, max_r) = hub.read(|v| {
            let pending = v.questions.values().filter(|q| q.status.is_open() && !q.to_orchestrator).count()
                + v.reports.values().filter(|r| !r.read && r.kind != ReportKind::Progress).count();
            let awake = |session: &str| v.nodes.get(session).is_none_or(|n| n.alive);
            let qs: Vec<Question> = v
                .questions
                .range(last_q + 1..)
                .map(|(_, q)| q.clone())
                .filter(|q| q.status.is_open() && !q.to_orchestrator && awake(&q.session_id))
                .collect();
            let rs: Vec<Report> =
                v.reports.range(last_r + 1..).map(|(_, r)| r.clone()).filter(|r| r.kind != ReportKind::Progress && awake(&r.session_id)).collect();
            (qs, rs, pending, v.questions.keys().next_back().copied().unwrap_or(0), v.reports.keys().next_back().copied().unwrap_or(0))
        });
        last_q = last_q.max(max_q);
        last_r = last_r.max(max_r);
        for q in questions {
            push.broadcast(&question_payload(&q, badge), true).await;
        }
        for r in reports {
            push.broadcast(&report_payload(&r, badge), false).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_hours_across_midnight() {
        assert!(in_quiet_hours(Some("23:00"), Some("08:00"), 23 * 60 + 30));
        assert!(in_quiet_hours(Some("23:00"), Some("08:00"), 3 * 60));
        assert!(!in_quiet_hours(Some("23:00"), Some("08:00"), 12 * 60));
        assert!(in_quiet_hours(Some("13:00"), Some("15:00"), 14 * 60));
        assert!(!in_quiet_hours(None, Some("08:00"), 3 * 60));
    }
}
