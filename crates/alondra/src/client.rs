use std::sync::OnceLock;
use std::time::Duration;

use alondra_core::config::Config;
use alondra_core::{Machine, Node, Question, Report};
use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

static CONFIG: OnceLock<Config> = OnceLock::new();

/// The effective configuration; a broken `config.toml` falls back to defaults here and is
/// reported by `alondra doctor`.
pub fn config() -> &'static Config {
    CONFIG.get_or_init(|| Config::load().unwrap_or_else(|_| Config::defaults()))
}

#[derive(Deserialize, Clone)]
pub struct FleetNode {
    #[serde(flatten)]
    pub node: Node,
    #[serde(default)]
    pub project: Option<String>,
}

impl std::ops::Deref for FleetNode {
    type Target = Node;
    fn deref(&self) -> &Node {
        &self.node
    }
}

#[derive(Deserialize, Clone, Default)]
pub struct Overview {
    pub tick: u64,
    pub nodes: Vec<FleetNode>,
    pub open_questions: Vec<OpenQuestion>,
    #[serde(default)]
    pub news: Vec<NewsReport>,
    pub machine: Machine,
    #[serde(default)]
    pub usage: Option<alondra_core::usage::Usage>,
    #[serde(default)]
    pub prs: Vec<PrCard>,
}

/// An open question, with the PR card it belongs to when that card is waiting on the user.
#[derive(Deserialize, Clone)]
pub struct OpenQuestion {
    #[serde(flatten)]
    pub question: Question,
    #[serde(default)]
    pub pr_id: Option<u64>,
    /// The report this question is about; it is shown inside the question, not as its own card.
    #[serde(default)]
    pub report: Option<Report>,
    /// Every report the question takes in, oldest first: the one it cites and the node's earlier
    /// reports of the same episode.
    #[serde(default)]
    pub reports: Vec<Report>,
}

impl OpenQuestion {
    /// The reports shown inside the question; an older alondrad sends only `report`.
    pub fn about(&self) -> Vec<&Report> {
        if self.reports.is_empty() { self.report.iter().collect() } else { self.reports.iter().collect() }
    }
}

impl From<Question> for OpenQuestion {
    fn from(question: Question) -> Self {
        OpenQuestion { question, pr_id: None, report: None, reports: vec![] }
    }
}

impl std::ops::Deref for OpenQuestion {
    type Target = Question;
    fn deref(&self) -> &Question {
        &self.question
    }
}

/// An unread report, with the PR card it is shown inside when it links a PR that has one.
#[derive(Deserialize, Clone)]
pub struct NewsReport {
    #[serde(flatten)]
    pub report: Report,
    #[serde(default)]
    pub pr_id: Option<u64>,
    /// The earlier unread reports of the same episode, oldest first; this card carries them.
    #[serde(default)]
    pub earlier: Vec<Report>,
}

impl From<Report> for NewsReport {
    fn from(report: Report) -> Self {
        NewsReport { report, pr_id: None, earlier: vec![] }
    }
}

impl std::ops::Deref for NewsReport {
    type Target = Report;
    fn deref(&self) -> &Report {
        &self.report
    }
}

#[derive(Deserialize, Clone)]
pub struct PrCard {
    #[serde(flatten)]
    pub pr: alondra_core::pr::PullRequest,
    #[serde(default)]
    pub preferred_method: Option<alondra_core::pr::MergeMethod>,
}

impl PrCard {
    /// The methods offered, the repo's preferred one first; all three when the repo was not read.
    pub fn methods(&self) -> Vec<alondra_core::pr::MergeMethod> {
        use alondra_core::pr::MergeMethod;
        let allowed = if self.pr.merge_methods.is_empty() { vec![MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase] } else { self.pr.merge_methods.clone() };
        let preferred = self.preferred_method.filter(|m| allowed.contains(m)).unwrap_or(allowed[0]);
        std::iter::once(preferred).chain(allowed.into_iter().filter(|m| *m != preferred)).collect()
    }

    /// The numbered actions of the card: merging in each method, or, while the branch conflicts
    /// with its base, only asking its node to resolve that.
    pub fn actions(&self) -> Vec<PrAction> {
        if self.pr.conflicts() {
            return vec![PrAction::ResolveConflicts];
        }
        self.methods().into_iter().map(PrAction::Merge).collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrAction {
    Merge(alondra_core::pr::MergeMethod),
    ResolveConflicts,
}

pub struct Client {
    base: String,
    agent: ureq::Agent,
    signer: Option<Signer>,
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
        let code = unsigned.post("/v1/pairing", &serde_json::json!({}))?.get("code").and_then(Value::as_str).map(str::to_string).context("no pairing code")?;
        let public = base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_sec1_point(false).as_bytes());
        let host = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|h| !h.is_empty()).unwrap_or_else(|| "Mac".into());
        let paired = unsigned.post("/v1/pair", &serde_json::json!({"code": code, "device_name": format!("Terminal ({host})"), "public_key": public}))?;
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
        Client { base: format!("http://{host}:{port}"), agent, signer: None }
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

    fn signed(&self, req: ureq::RequestBuilder<ureq::typestate::WithoutBody>, method: &str, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        match &self.signer {
            Some(s) => s.sign(method, path, b"").into_iter().fold(req, |r, (k, v)| r.header(k, v)),
            None => req,
        }
    }

    fn check(mut resp: ureq::http::Response<ureq::Body>) -> Result<(u16, String)> {
        let status = resp.status().as_u16();
        let body = resp.body_mut().read_to_string().unwrap_or_default();
        if status >= 400 {
            let parsed = serde_json::from_str::<Value>(&body).ok();
            let reasons: Vec<String> = parsed
                .as_ref()
                .and_then(|v| v.get("reasons"))
                .and_then(Value::as_array)
                .map(|rs| {
                    rs.iter()
                        .map(|r| crate::i18n::close_reason(r.get("kind").and_then(Value::as_str).unwrap_or_default(), r.get("detail").and_then(Value::as_str)))
                        .collect()
                })
                .unwrap_or_default();
            if !reasons.is_empty() {
                let text = match reasons.as_slice() {
                    [one] => one.clone(),
                    many => many.iter().map(|r| format!("- {r}")).collect::<Vec<_>>().join("\n"),
                };
                return Err(anyhow!(text));
            }
            let msg = parsed.as_ref().and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string)).unwrap_or(body);
            return Err(anyhow!(trf!("alondrad answered: {msg}", "alondrad respondió: {msg}")));
        }
        Ok((status, body))
    }

    pub fn get_text(&self, path: &str) -> Result<(u16, String)> {
        let resp = self.signed(self.agent.get(format!("{}{path}", self.base)), "GET", path).call().context(tr!("alondrad is not responding (is it running?)", "alondrad no responde (¿está arrancado?)"))?;
        Self::check(resp)
    }

    /// The raw body, for files that may not be text (images, PDFs).
    pub fn get_bytes(&self, path: &str) -> Result<Vec<u8>> {
        let mut resp = self.signed(self.agent.get(format!("{}{path}", self.base)), "GET", path).call().context(tr!("alondrad is not responding (is it running?)", "alondrad no responde (¿está arrancado?)"))?;
        if resp.status().as_u16() >= 400 {
            return Self::check(resp).map(|(_, body)| body.into_bytes());
        }
        Ok(resp.body_mut().with_config().limit(64 * 1024 * 1024).read_to_vec()?)
    }

    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let (_, body) = self.get_text(path)?;
        Ok(serde_json::from_str(&body)?)
    }

    pub fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let bytes = serde_json::to_vec(body)?;
        let mut req = self.agent.post(format!("{}{path}", self.base)).header("content-type", "application/json");
        if let Some(s) = &self.signer {
            for (k, v) in s.sign("POST", path, &bytes) {
                req = req.header(k, v);
            }
        }
        let resp = req.send(&bytes[..]).context(tr!("alondrad is not responding (is it running?)", "alondrad no responde (¿está arrancado?)"))?;
        let (_, text) = Self::check(resp)?;
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    pub fn delete(&self, path: &str) -> Result<()> {
        let resp = self.signed(self.agent.delete(format!("{}{path}", self.base)), "DELETE", path).call().context(tr!("alondrad is not responding (is it running?)", "alondrad no responde (¿está arrancado?)"))?;
        Self::check(resp).map(|_| ())
    }

    pub fn overview(&self) -> Result<Overview> {
        self.get("/v1/overview")
    }
}
