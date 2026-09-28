use std::sync::OnceLock;
use std::time::Duration;

use alondra_core::config::Config;
use alondra_core::domain_v2::{Command, CommandEnvelope, CommandOutcome, DataEpoch, DomainError, DomainProjection, PROTOCOL_VERSION, Revision};
use anyhow::{Context, Result, anyhow};
use serde::de::DeserializeOwned;
use serde_json::Value;

static CONFIG: OnceLock<Config> = OnceLock::new();

/// The effective configuration; a broken `config.toml` falls back to defaults here and is
/// reported by `alondra doctor`.
pub fn config() -> &'static Config {
    CONFIG.get_or_init(|| Config::load().unwrap_or_else(|_| Config::defaults()))
}

pub struct Client {
    base: String,
    agent: ureq::Agent,
    signer: Option<Signer>,
    service: Option<std::sync::Arc<crate::service_client::Profile>>,
    timeout: Duration,
}

struct Signer {
    device: String,
    key: p256::ecdsa::SigningKey,
}

const KEY_FILE: &str = "tui-key.pem";
const DEVICE_FILE: &str = "tui-device";

impl Signer {
    fn sign(&self, method: &str, path: &str, body: &[u8]) -> [(&'static str, String); 3] {
        use base64::Engine as _;
        use p256::ecdsa::signature::Signer as _;
        use sha2::Digest as _;
        let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis()).to_string();
        let digest: String = sha2::Sha256::digest(body).iter().map(|b| format!("{b:02x}")).collect();
        let message = format!("{method}\n{path}\n{timestamp}\n{digest}");
        let signature: p256::ecdsa::Signature = self.key.sign(message.as_bytes());
        [
            ("x-alondra-device", self.device.clone()),
            ("x-alondra-timestamp", timestamp),
            ("x-alondra-signature", base64::engine::general_purpose::STANDARD.encode(signature.to_der().as_bytes())),
        ]
    }

    /// The key this terminal signs the user's answers with, paired with alondrad on first use.
    fn load_or_pair(unsigned: &Client) -> Result<Signer> {
        use base64::Engine as _;
        use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};
        let home = &config().home;
        let key_path = home.join(KEY_FILE);
        let device_path = home.join(DEVICE_FILE);
        if let (Ok(pem), Ok(device)) = (std::fs::read_to_string(&key_path), std::fs::read_to_string(&device_path)) {
            let key = p256::ecdsa::SigningKey::from_pkcs8_pem(&pem).map_err(|e| anyhow!("{KEY_FILE}: {e}"))?;
            return Ok(Signer { device: device.trim().to_string(), key });
        }
        let mut seed = [0u8; 32];
        std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut seed)?;
        let key = p256::ecdsa::SigningKey::from_slice(&seed).map_err(|e| anyhow!("key: {e}"))?;
        let code = unsigned.post("/v2/pairing", &serde_json::json!({}))?.get("code").and_then(Value::as_str).map(str::to_string).context("no pairing code")?;
        let public = base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_sec1_point(false).as_bytes());
        let host = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|h| !h.is_empty()).unwrap_or_else(|| "Mac".into());
        let paired = unsigned.post("/v2/pair", &serde_json::json!({"code": code, "device_name": format!("Terminal ({host})"), "public_key": public}))?;
        let device = paired.get("device_id").and_then(Value::as_str).context("no device id")?.to_string();
        let pem = key.to_pkcs8_pem(LineEnding::LF).map_err(|e| anyhow!("key: {e}"))?;
        std::fs::create_dir_all(home)?;
        write_private(&key_path, pem.as_bytes())?;
        write_private(&device_path, device.as_bytes())?;
        Ok(Signer { device, key })
    }
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    std::io::Write::write_all(&mut f, bytes)?;
    Ok(())
}

impl Client {
    pub fn new(timeout: Duration) -> Self {
        let c = config();
        let host = if c.listen.is_unspecified() || c.listen.is_loopback() { "127.0.0.1".to_string() } else { c.listen.to_string() };
        let port = c.port;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .build()
            .into();
        Client { base: format!("http://{host}:{port}"), agent, signer: None, service: None, timeout }
    }

    /// A client whose requests are signed as the user: for the TUI and the commands the user
    /// types. Falls back to unsigned (and says so on stderr) when pairing is impossible.
    pub fn for_user(timeout: Duration) -> Self {
        let mut client = Self::new(timeout);
        match Signer::load_or_pair(&client) {
            Ok(signer) => client.signer = Some(signer),
            Err(e) => eprintln!("{}", trf!("alondra: answers will go unsigned ({e})", "alondra: las respuestas irán sin firmar ({e})")),
        }
        client
    }

    /// Only interactive TUI requests consult its explicitly selected remote destination.
    pub fn for_tui(timeout: Duration) -> Self {
        if let Some(profile) = crate::service_client::active() {
            let mut client = Self::new(timeout.max(Duration::from_secs(15)));
            client.service = Some(profile);
            client
        } else { Self::for_user(timeout) }
    }

    fn headers<B>(&self, mut req: ureq::RequestBuilder<B>, method: &str, path: &str, body: &[u8]) -> ureq::RequestBuilder<B> {
        if let Ok(run) = std::env::var("ALONDRA_ORCHESTRATOR_EXECUTION") { req = req.header("x-alondra-orchestrator-execution", run); }
        if let Ok(sid) = std::env::var("ALONDRA_SESSION_ID").or_else(|_| std::env::var("CLAUDE_CODE_SESSION_ID")) { req = req.header("x-alondra-session", sid); }
        match &self.signer {
            Some(s) => s.sign(method, path, body).into_iter().fold(req, |r, (k, v)| r.header(k, v)),
            None => req,
        }
    }

    /// The raw answer, a refusal included: domain commands read their typed error from it.
    fn exchange(&self, method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Value)> {
        if let Some(service) = &self.service {
            return service.request(method, path, body, self.timeout);
        }
        let url = format!("{}{path}", self.base);
        let not_responding = tr!("alondrad is not responding (is it running?)", "alondrad no responde (¿está arrancado?)");
        let bytes = body.map(serde_json::to_vec).transpose()?.unwrap_or_default();
        let mut resp = match method {
            "POST" => self.headers(self.agent.post(url).header("content-type", "application/json"), method, path, &bytes).send(&bytes[..]),
            "PUT" => self.headers(self.agent.put(url).header("content-type", "application/json"), method, path, &bytes).send(&bytes[..]),
            "DELETE" => self.headers(self.agent.delete(url), method, path, b"").call(),
            _ => self.headers(self.agent.get(url), method, path, b"").call(),
        }
        .context(not_responding)?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
    }

    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let (status, value) = self.exchange(method, path, body)?;
        if status >= 400 {
            return Err(failure(status, &value));
        }
        Ok(value)
    }

    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        Ok(serde_json::from_value(self.call("GET", path, None)?)?)
    }

    pub fn post(&self, path: &str, body: &Value) -> Result<Value> {
        self.call("POST", path, Some(body))
    }

    pub fn put(&self, path: &str, body: &Value) -> Result<Value> {
        self.call("PUT", path, Some(body))
    }

    pub fn delete(&self, path: &str) -> Result<()> {
        self.call("DELETE", path, None).map(|_| ())
    }

    /// Domain producers use their daemon-issued bearer, never a locally paired User key.
    pub fn domain_get(&self, path: &str, bearer: &str) -> Result<Value> {
        let url = format!("http://127.0.0.1:{}{path}", config().port);
        let resp = self.agent.get(url).header("authorization", format!("Bearer {bearer}")).call()?;
        answer(resp)
    }

    pub fn domain_post(&self, path: &str, bearer: &str, body: &Value) -> Result<Value> {
        let bytes = serde_json::to_vec(body)?;
        let url = format!("http://127.0.0.1:{}{path}", config().port);
        let resp = self.agent.post(url)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {bearer}"))
            .send(&bytes[..])?;
        answer(resp)
    }

    /// The whole domain as the daemon projects it: roots, children, actions and their revisions.
    pub fn domain(&self) -> Result<DomainProjection> {
        self.get("/v2/domain")
    }

    /// Waits up to `wait` seconds for the domain to move past `revision`.
    pub fn domain_changed(&self, epoch: &DataEpoch, revision: Revision, wait: u64) -> Result<bool> {
        let changes: Value = self.get(&format!("/v2/domain/changes?data_epoch={epoch}&after={revision}&wait={wait}"))?;
        Ok(changes["changed"].as_bool().unwrap_or(true))
    }

    /// A refusal is the daemon's typed answer; anything else leaves the outcome unknown.
    pub fn command(&self, envelope: &CommandEnvelope) -> Result<CommandOutcome, CommandFailure> {
        let body = serde_json::to_value(envelope).map_err(|e| CommandFailure::Uncertain(e.to_string()))?;
        let (status, value) = self.exchange("POST", "/v2/domain/commands", Some(&body)).map_err(|e| CommandFailure::Uncertain(format!("{e:#}")))?;
        classify(status, value)
    }
}

fn answer(mut resp: ureq::http::Response<ureq::Body>) -> Result<Value> {
    let status = resp.status().as_u16();
    let value = serde_json::from_str(&resp.body_mut().read_to_string().unwrap_or_default()).unwrap_or(Value::Null);
    if status >= 400 {
        return Err(failure(status, &value));
    }
    Ok(value)
}

/// Why alondrad said no, in the user's language where the reason is a domain error it names.
fn failure(status: u16, body: &Value) -> anyhow::Error {
    if let Ok(error) = serde_json::from_value::<DomainError>(body.clone()) {
        return anyhow!(refusal(&error));
    }
    let msg = body.get("error").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| body.to_string());
    anyhow!(trf!("alondrad answered {status}: {msg}", "alondrad respondió {status}: {msg}"))
}

/// How a domain command ended when it did not succeed.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandFailure {
    /// The daemon refused it: nothing was written.
    Rejected(DomainError),
    /// The request left but no answer says whether it was applied.
    Uncertain(String),
}

fn classify(status: u16, value: Value) -> Result<CommandOutcome, CommandFailure> {
    if status < 300 {
        return serde_json::from_value(value).map_err(|e| CommandFailure::Uncertain(e.to_string()));
    }
    match serde_json::from_value::<DomainError>(value) {
        Ok(error) if status < 500 || error.code == "storage_unavailable" => Err(CommandFailure::Rejected(error)),
        Ok(error) => Err(CommandFailure::Uncertain(error.to_string())),
        Err(_) => Err(CommandFailure::Uncertain(trf!("alondrad answered {status}", "alondrad respondió {status}"))),
    }
}

/// A fresh identifier for a command or for an entity the command creates.
pub fn new_id(prefix: &str) -> String {
    let bytes = crate::service_client::random::<16>().unwrap_or_default();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}-{hex}")
}

pub fn envelope(epoch: &DataEpoch, expected_revision: Revision, command: Command) -> CommandEnvelope {
    CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: epoch.clone(), command_id: new_id("tui").into(), expected_revision, command }
}

/// What a refusal means to the user; the code decides, the daemon's English message is the fallback.
pub fn refusal(error: &DomainError) -> String {
    match error.code.as_str() {
        "stale_revision" | "stale_epoch" => tr!("It changed since you saw it: check it and try again", "Ha cambiado desde que lo viste: revísalo y vuelve a intentarlo").into(),
        "not_found" => tr!("Alondra no longer has it", "Alondra ya no lo tiene").into(),
        "task_not_active" => tr!("The task is no longer active", "La tarea ya no está activa").into(),
        "incompatible_protocol" => tr!("This alondrad does not accept this domain request", "Este alondrad no acepta esta petición del dominio").into(),
        _ => {
            let (code, why) = (&error.code, &error.message);
            trf!("{why} ({code})", "{why} ({code})")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Lang, set_for_this_thread};
    use serde_json::json;

    #[test]
    fn a_refusal_is_typed_and_a_lost_answer_is_uncertain() {
        let stale = classify(409, json!({"code": "stale_revision", "message": "Refresh", "current_revision": 7, "subject": null}));
        assert!(matches!(stale, Err(CommandFailure::Rejected(e)) if e.code == "stale_revision" && e.current_revision == Some(7)));
        assert!(matches!(classify(502, json!({"error": "bad gateway"})), Err(CommandFailure::Uncertain(_))));
        assert!(matches!(classify(500, json!({"code": "storage_unavailable", "message": "x", "current_revision": null, "subject": null})), Err(CommandFailure::Rejected(_))));
        let ok = classify(200, json!({"revision": 8, "changed": true, "effects": [], "created_ids": []}));
        assert!(matches!(ok, Ok(o) if o.revision == 8));
    }

    #[test]
    fn a_refusal_reads_in_the_users_language() {
        set_for_this_thread(Lang::Es);
        let said = |code: &str| refusal(&DomainError::new(code, "english detail"));
        assert_eq!(said("stale_revision"), "Ha cambiado desde que lo viste: revísalo y vuelve a intentarlo");
        assert_eq!(said("not_found"), "Alondra ya no lo tiene");
        assert_eq!(said("something_new"), "english detail (something_new)");
        assert_eq!(failure(409, &json!({"code": "not_found", "message": "gone", "current_revision": null, "subject": null})).to_string(), "Alondra ya no lo tiene");
    }

    #[test]
    fn every_command_gets_its_own_identifier() {
        let epoch = DataEpoch::from("e");
        let a = envelope(&epoch, 3, Command::MarkRead { interaction_id: "i".into() });
        let b = envelope(&epoch, 3, Command::MarkRead { interaction_id: "i".into() });
        assert_ne!(a.command_id, b.command_id);
        assert_eq!((a.protocol_version, a.expected_revision), (PROTOCOL_VERSION, 3));
    }
}
