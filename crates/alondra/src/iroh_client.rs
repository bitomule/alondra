//! A paired Mac over its iroh endpoint (`docs/iroh-protocol.md`): one QUIC connection, directly
//! or through the iroh relay, opened with a hello signed by this terminal's pairing key, then one
//! sealed envelope per stream. This terminal has one iroh key for every Mac, kept beside its
//! pairings; what it needs to dial each Mac comes from that Mac's encrypted `/routes`.
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use iroh::endpoint::{Connection, PortmapperConfig, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayConfig, RelayMode, RelayUrl, SecretKey};
use p256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::net::SocketAddr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const ALPN: &[u8] = b"alondra/1";
const KEY_FILE: &str = "iroh-client.key";
const STORE: &str = "direct-iroh.json";
const MAX_REPLY: usize = 8 * 1024 * 1024;
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);

/// What dials one Mac's endpoint, as its `/routes` gave it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dial {
    pub endpoint: String,
    #[serde(default)]
    pub addrs: Vec<String>,
    #[serde(default)]
    pub relay: Option<String>,
}

impl Dial {
    /// The dial `/routes` describes, when that Mac runs an iroh endpoint.
    pub fn from_routes(body: &Value) -> Option<Self> {
        let endpoint = body["iroh_endpoint"].as_str()?.parse::<EndpointId>().ok()?.to_string();
        let addrs = body["iroh_addrs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|addr| addr.parse::<SocketAddr>().is_ok())
            .map(str::to_owned)
            .collect();
        let relay = body["iroh_relay"].as_str().filter(|url| url.parse::<RelayUrl>().is_ok()).map(str::to_owned);
        Some(Self { endpoint, addrs, relay })
    }
}

fn store_path(home: &Path) -> std::path::PathBuf {
    home.join(STORE)
}

pub fn load_dial(home: &Path, mac_id: &str) -> Option<Dial> {
    let bytes = fs::read(store_path(home)).ok()?;
    let mut dials: BTreeMap<String, Dial> = serde_json::from_slice(&bytes).ok()?;
    dials.remove(mac_id)
}

/// Keeps `dial` for `mac_id`, or forgets it with `None`. Callers hold the pairing store's lock.
pub fn save_dial(home: &Path, mac_id: &str, dial: Option<Dial>) -> Result<()> {
    let path = store_path(home);
    let mut dials: BTreeMap<String, Dial> =
        fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    match dial {
        Some(dial) => dials.insert(mac_id.to_owned(), dial),
        None => dials.remove(mac_id),
    };
    write_private(&path, &serde_json::to_vec_pretty(&dials)?)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    Ok(())
}

/// This terminal's iroh key, created on first use; an unreadable or shared file is refused.
pub fn key(home: &Path) -> Result<SecretKey> {
    let path = home.join(KEY_FILE);
    match fs::symlink_metadata(&path) {
        Ok(meta) => {
            if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
                bail!("{} must be a regular owner-only file", path.display());
            }
            let bytes: [u8; 32] = B64
                .decode(fs::read_to_string(&path)?.trim())?
                .try_into()
                .map_err(|_| anyhow!("{} is not an iroh key", path.display()))?;
            Ok(SecretKey::from_bytes(&bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(home)?;
            let mut bytes = [0u8; 32];
            ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
                .map_err(|_| anyhow!("OS randomness failed"))?;
            let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
            file.write_all(B64.encode(bytes).as_bytes())?;
            file.sync_all()?;
            Ok(SecretKey::from_bytes(&bytes))
        }
        Err(error) => Err(error.into()),
    }
}

/// The process's one iroh endpoint and the runtime that drives it.
struct Local {
    runtime: tokio::runtime::Runtime,
    endpoint: Endpoint,
}

static LOCAL: OnceLock<std::result::Result<Arc<Local>, String>> = OnceLock::new();

fn local(home: &Path) -> Result<Arc<Local>> {
    LOCAL
        .get_or_init(|| {
            let start = || -> Result<Local> {
                let key = key(home)?;
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("alondra-iroh")
                    .enable_all()
                    .build()?;
                let endpoint = runtime.block_on(
                    Endpoint::builder(presets::Minimal)
                        .secret_key(key)
                        .relay_mode(RelayMode::Custom(iroh::RelayMap::empty()))
                        .portmapper_config(PortmapperConfig::Disabled)
                        .bind(),
                )?;
                Ok(Local { runtime, endpoint })
            };
            start().map(Arc::new).map_err(|error| format!("{error:#}"))
        })
        .clone()
        .map_err(|error| anyhow!("iroh endpoint unavailable: {error}"))
}

/// How the connection reaches the Mac right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathKind {
    Direct(SocketAddr),
    Relay,
}

/// What the Mac answered on one stream.
#[derive(Debug, PartialEq)]
pub enum Reply {
    /// A sealed response envelope: `response`, or `revoked` (which opens to `device_revoked`).
    Sealed(Vec<u8>),
    Refused { error: String, detail: Option<String> },
}

/// Whether a failed exchange may have reached the Mac.
#[derive(Debug)]
pub struct Failed {
    pub sent: bool,
}

/// One open connection to one Mac, after its hello was accepted.
pub struct Session {
    local: Arc<Local>,
    connection: Connection,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.connection.close(0u32.into(), b"done");
    }
}

pub struct Identity<'a> {
    pub mac_id: &'a str,
    pub device_id: &'a str,
    pub signing: &'a SigningKey,
}

/// Connects to the Mac `dial` names and says hello as `identity`, within `CONNECT_TIMEOUT`.
pub fn connect(home: &Path, dial: &Dial, identity: &Identity) -> Result<Session> {
    let local = local(home)?;
    let mac: EndpointId = dial.endpoint.parse().context("invalid Mac iroh endpoint")?;
    let mut addr = EndpointAddr::new(mac);
    for ip in dial.addrs.iter().filter_map(|addr| addr.parse::<SocketAddr>().ok()) {
        addr = addr.with_ip_addr(ip);
    }
    let relay = dial.relay.as_deref().map(str::parse::<RelayUrl>).transpose().context("invalid iroh relay")?;
    if let Some(relay) = &relay {
        addr = addr.with_relay_url(relay.clone());
    }
    let endpoint = local.endpoint.clone();
    let signature = alondra_e2e::sign_iroh_hello(
        identity.signing,
        identity.mac_id,
        identity.device_id,
        endpoint.id().as_bytes(),
        mac.as_bytes(),
    )
    .map_err(|error| anyhow!("sign the iroh hello: {error:?}"))?;
    let hello = serde_json::to_vec(&serde_json::json!({"device_id": identity.device_id, "signature": signature}))?;
    let connection = local.runtime.block_on(async {
        if let Some(relay) = relay {
            endpoint.insert_relay(relay.clone(), Arc::new(RelayConfig::from(relay))).await;
        }
        tokio::time::timeout(CONNECT_TIMEOUT, async {
            let connection = endpoint.connect(addr, ALPN).await?;
            let reply: Value = serde_json::from_slice(&exchange(&connection, &hello).await?)?;
            if reply != serde_json::json!({"ok": true}) {
                bail!("the Mac refused this terminal's iroh hello: {reply}");
            }
            anyhow::Ok(connection)
        })
        .await
        .map_err(|_| anyhow!("no iroh connection within {} s", CONNECT_TIMEOUT.as_secs()))?
    })?;
    Ok(Session { local, connection })
}

async fn exchange(connection: &Connection, body: &[u8]) -> Result<Vec<u8>> {
    let (mut send, mut recv) = connection.open_bi().await?;
    send.write_all(body).await?;
    send.finish()?;
    Ok(recv.read_to_end(MAX_REPLY).await?)
}

impl Session {
    /// Sends one sealed request envelope on its own stream and waits up to `timeout` for the answer.
    pub fn send(&self, envelope: &[u8], timeout: Duration) -> std::result::Result<Reply, Failed> {
        self.local.runtime.block_on(async {
            let (mut send, mut recv) = self
                .connection
                .open_bi()
                .await
                .map_err(|_| Failed { sent: false })?;
            fn lost<E>(_: E) -> Failed {
                Failed { sent: true }
            }
            send.write_all(envelope).await.map_err(lost)?;
            send.finish().map_err(lost)?;
            let bytes = tokio::time::timeout(timeout, recv.read_to_end(MAX_REPLY)).await.map_err(lost)?.map_err(lost)?;
            reply(&bytes).map_err(lost)
        })
    }

    pub fn is_closed(&self) -> bool {
        self.connection.close_reason().is_some()
    }

    /// The path iroh uses for this connection now.
    pub fn path(&self) -> Option<PathKind> {
        let paths = self.connection.paths();
        let selected = paths.iter().find(|path| path.is_selected())?;
        match selected.remote_addr() {
            iroh::TransportAddr::Ip(addr) => Some(PathKind::Direct(*addr)),
            _ => Some(PathKind::Relay),
        }
    }
}

/// Reads the Mac's answer: a sealed envelope to open, or why the Mac refused the request.
pub fn reply(bytes: &[u8]) -> Result<Reply> {
    let mut answer: Value = serde_json::from_slice(bytes).context("the Mac's iroh answer is not JSON")?;
    let sealed = answer.get_mut("response").map(Value::take);
    let sealed = sealed.or_else(|| answer.get_mut("revoked").map(Value::take));
    match (sealed, answer["error"].as_str()) {
        (Some(sealed), None) if sealed.is_object() => Ok(Reply::Sealed(serde_json::to_vec(&sealed)?)),
        (None, Some(error)) => {
            Ok(Reply::Refused { error: error.to_owned(), detail: answer["detail"].as_str().map(str::to_owned) })
        }
        _ => bail!("the Mac's iroh answer has an unknown shape"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_reply_keeps_the_sealed_bytes_or_names_the_refusal() {
        assert_eq!(reply(br#"{"response":{"kind":"response","x":[1, 2]}}"#).unwrap(), Reply::Sealed(br#"{"kind":"response","x":[1,2]}"#.to_vec()));
        assert_eq!(reply(br#"{"revoked":{"kind":"response"}}"#).unwrap(), Reply::Sealed(br#"{"kind":"response"}"#.to_vec()));
        assert_eq!(
            reply(br#"{"error":"rejected","detail":"no"}"#).unwrap(),
            Reply::Refused { error: "rejected".into(), detail: Some("no".into()) }
        );
        assert!(reply(br#"{"something":"else"}"#).is_err());
        assert!(reply(b"not json").is_err());
    }

    #[test]
    fn routes_without_a_valid_endpoint_give_no_dial_and_bad_addresses_are_dropped() {
        let endpoint = SecretKey::from_bytes(&[3u8; 32]).public().to_string();
        assert_eq!(Dial::from_routes(&json!({"lan": ["192.168.1.2"]})), None);
        assert_eq!(Dial::from_routes(&json!({"iroh_endpoint": "nope"})), None);
        let body = json!({"iroh_endpoint": endpoint, "iroh_addrs": ["192.168.1.2:5000", "garbage", "[fd12::1]:5000"],
            "iroh_relay": "https://iroh.example.com/"});
        assert_eq!(
            Dial::from_routes(&body),
            Some(Dial {
                endpoint,
                addrs: vec!["192.168.1.2:5000".into(), "[fd12::1]:5000".into()],
                relay: Some("https://iroh.example.com/".into())
            })
        );
    }

    #[test]
    fn the_key_is_created_owner_only_once_and_a_readable_one_is_refused() {
        let home = crate::test_support::Directory::new();
        fs::create_dir_all(&*home).unwrap();
        let first = key(&home).unwrap();
        assert_eq!(key(&home).unwrap().public(), first.public());
        let path = home.join(KEY_FILE);
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(key(&home).is_err());
    }

    #[test]
    fn a_dial_is_kept_per_mac_and_can_be_forgotten() {
        let home = crate::test_support::Directory::new();
        fs::create_dir_all(&*home).unwrap();
        let dial = |n: u8| Dial { endpoint: SecretKey::from_bytes(&[n; 32]).public().to_string(), addrs: vec![], relay: None };
        save_dial(&home, "mac-a", Some(dial(1))).unwrap();
        save_dial(&home, "mac-b", Some(dial(2))).unwrap();
        save_dial(&home, "mac-a", None).unwrap();
        assert_eq!(load_dial(&home, "mac-a"), None);
        assert_eq!(load_dial(&home, "mac-b"), Some(dial(2)));
        assert_eq!(fs::metadata(store_path(&home)).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
