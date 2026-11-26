//! This Mac's daemon over HTTP: the user's requests signed with the key this terminal paired.
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::client::availability::ResponseObserver;
use crate::client::{Client, MAX_ATTACHMENT_BYTES, computer_name, config};
use alondra_client_port::transport::{Local, Request};

#[derive(Clone)]
struct Signer {
    device: String,
    key: p256::ecdsa::SigningKey,
}

static LAST_TIMESTAMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Keep timestamps ordered within this process; signing entropy distinguishes separate processes.
fn unique_timestamp() -> u64 {
    use std::sync::atomic::Ordering::SeqCst;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let mut last = LAST_TIMESTAMP.load(SeqCst);
    loop {
        let next = now.max(last + 1);
        match LAST_TIMESTAMP.compare_exchange(last, next, SeqCst, SeqCst) {
            Ok(_) => return next,
            Err(seen) => last = seen,
        }
    }
}

const KEY_FILE: &str = "tui-key.pem";
const DEVICE_FILE: &str = "tui-device";

struct SigningEntropy(ring::rand::SystemRandom);
impl p256::elliptic_curve::rand_core::TryRng for SigningEntropy {
    type Error = std::io::Error;
    fn try_next_u32(&mut self) -> std::io::Result<u32> {
        let mut bytes = [0; 4]; self.try_fill_bytes(&mut bytes)?; Ok(u32::from_le_bytes(bytes))
    }
    fn try_next_u64(&mut self) -> std::io::Result<u64> {
        let mut bytes = [0; 8]; self.try_fill_bytes(&mut bytes)?; Ok(u64::from_le_bytes(bytes))
    }
    fn try_fill_bytes(&mut self, bytes: &mut [u8]) -> std::io::Result<()> {
        use ring::rand::SecureRandom as _;
        self.0.fill(bytes).map_err(|_| std::io::Error::other("system entropy unavailable"))
    }
}
impl p256::elliptic_curve::rand_core::TryCryptoRng for SigningEntropy {}

impl Signer {
    fn sign(&self, method: &str, path: &str, body: &[u8]) -> Result<[(&'static str, String); 3]> {
        self.sign_at(method, path, body, unique_timestamp())
    }

    fn sign_at(&self, method: &str, path: &str, body: &[u8], timestamp: u64) -> Result<[(&'static str, String); 3]> {
        self.sign_with_entropy(method, path, body, timestamp, &mut SigningEntropy(ring::rand::SystemRandom::new()))
    }

    fn sign_with_entropy<R: p256::elliptic_curve::rand_core::TryCryptoRng + ?Sized>(
        &self, method: &str, path: &str, body: &[u8], timestamp: u64, entropy: &mut R,
    ) -> Result<[(&'static str, String); 3]> {
        use base64::Engine as _;
        use p256::ecdsa::signature::RandomizedSigner as _;
        use sha2::Digest as _;
        let timestamp = timestamp.to_string();
        let digest: String = sha2::Sha256::digest(body).iter().map(|b| format!("{b:02x}")).collect();
        let message = format!("{method}\n{path}\n{timestamp}\n{digest}");
        let signature: p256::ecdsa::Signature = self.key.try_sign_with_rng(entropy, message.as_bytes())
            .map_err(|_| anyhow!(tr!("Could not sign the local request", "No se pudo firmar la solicitud local")))?;
        Ok([
            ("x-alondra-device", self.device.clone()),
            ("x-alondra-timestamp", timestamp),
            ("x-alondra-signature", base64::engine::general_purpose::STANDARD.encode(signature.to_der().as_bytes())),
        ])
    }

    fn same_pair(&self, other: &Signer) -> bool {
        self.device == other.device && self.key.verifying_key() == other.key.verifying_key()
    }
}

const PAIRING_LOCK_FILE: &str = "tui-pairing.lock";

static PAIRING: std::sync::LazyLock<std::sync::Arc<Pairing>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(Pairing::at(config().home.clone())));

/// The key this terminal signs the user's answers with, paired with alondrad on first use. Every
/// client of the process shares it, and `tui-pairing.lock` keeps other processes from pairing at
/// the same time, so the home never holds the key of one pairing with the device of another.
struct Pairing {
    home: std::path::PathBuf,
    signer: std::sync::Mutex<Option<Signer>>,
    replaced: std::sync::atomic::AtomicBool,
    /// What the user is told once about a pairing made again on its own.
    notice: std::sync::Mutex<Option<String>>,
}

/// Why alondrad refused a signed request in a way a new pairing mends.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rejection {
    /// It knows the device, with another key.
    WrongSignature,
    /// It no longer has the device: someone unpaired this terminal.
    NotPaired,
}

impl Rejection {
    fn of(status: u16, answer: &[u8]) -> Option<Self> {
        if status != 401 {
            return None;
        }
        match serde_json::from_slice::<Value>(answer).ok()?["error"].as_str()? {
            "wrong signature" => Some(Rejection::WrongSignature),
            "device not paired" => Some(Rejection::NotPaired),
            _ => None,
        }
    }
}

impl Pairing {
    fn at(home: std::path::PathBuf) -> Self {
        Pairing {
            home,
            signer: std::sync::Mutex::new(None),
            replaced: std::sync::atomic::AtomicBool::new(false),
            notice: std::sync::Mutex::new(None),
        }
    }

    fn take_notice(&self) -> Option<String> {
        self.notice.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
    }

    fn tell(&self, rejection: Rejection) {
        if rejection == Rejection::NotPaired {
            let host = computer_name();
            *self.notice.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(trf!(
                "This Mac no longer had this terminal among its devices; it paired again as “Terminal ({host})”.",
                "Este Mac ya no tenía este terminal entre sus dispositivos; se ha vuelto a emparejar como «Terminal ({host})»."
            ));
        }
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, Option<Signer>> {
        self.signer.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn current(&self) -> Option<Signer> {
        self.cache().clone()
    }

    fn signer(&self, unsigned: &Client) -> Result<Signer> {
        let mut cache = self.cache();
        if let Some(signer) = cache.as_ref() {
            return Ok(signer.clone());
        }
        let _lock = self.lock()?;
        let signer = match self.stored() {
            Some(stored) => stored,
            None => self.pair(unsigned)?,
        };
        *cache = Some(signer.clone());
        Ok(signer)
    }

    /// alondrad knows the stored device but not with the stored key, as a home paired twice at
    /// once was left, or no longer knows the device at all. Pairs again, once per process, unless
    /// another process already did.
    fn replace_rejected(&self, rejected: &Signer, unsigned: &Client) -> Result<Option<Signer>> {
        let mut cache = self.cache();
        let _lock = self.lock()?;
        let signer = match self.stored() {
            Some(stored) if !stored.same_pair(rejected) => stored,
            _ if self.replaced.swap(true, std::sync::atomic::Ordering::SeqCst) => return Ok(None),
            _ => self.pair(unsigned)?,
        };
        *cache = Some(signer.clone());
        Ok(Some(signer))
    }

    fn lock(&self) -> Result<std::fs::File> {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::create_dir_all(&self.home)?;
        let file = std::fs::OpenOptions::new().write(true).create(true).truncate(false).mode(0o600)
            .open(self.home.join(PAIRING_LOCK_FILE))?;
        file.lock()?;
        Ok(file)
    }

    fn stored(&self) -> Option<Signer> {
        use p256::pkcs8::DecodePrivateKey;
        let device = std::fs::read_to_string(self.home.join(DEVICE_FILE)).ok()?.trim().to_string();
        let key = p256::ecdsa::SigningKey::from_pkcs8_pem(&std::fs::read_to_string(self.home.join(KEY_FILE)).ok()?).ok()?;
        (!device.is_empty()).then_some(Signer { device, key })
    }

    /// The device file goes first and comes back last: a cut in between leaves no device, and
    /// the next start pairs again instead of signing with a key alondrad does not have.
    fn pair(&self, unsigned: &Client) -> Result<Signer> {
        use base64::Engine as _;
        use p256::pkcs8::{EncodePrivateKey, LineEnding};
        let mut seed = [0u8; 32];
        std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut seed)?;
        let key = p256::ecdsa::SigningKey::from_slice(&seed).map_err(|e| anyhow!("key: {e}"))?;
        let code = unsigned.post("/pairing", &serde_json::json!({}))?.get("code").and_then(Value::as_str).map(str::to_string).context("no pairing code")?;
        let public = base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_sec1_point(false).as_bytes());
        let host = computer_name();
        let paired = unsigned.post("/pair", &serde_json::json!({"code": code, "device_name": format!("Terminal ({host})"), "public_key": public}))?;
        let device = paired.get("device_id").and_then(Value::as_str).context("no device id")?.to_string();
        let pem = key.to_pkcs8_pem(LineEnding::LF).map_err(|e| anyhow!("key: {e}"))?;
        let device_path = self.home.join(DEVICE_FILE);
        match std::fs::remove_file(&device_path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        replace_private(&self.home.join(KEY_FILE), pem.as_bytes())?;
        replace_private(&device_path, device.as_bytes())?;
        Ok(Signer { device, key })
    }
}

fn replace_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let name = path.file_name().context("no file name")?.to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.{}", std::process::id()));
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temporary)?;
    std::io::Write::write_all(&mut f, bytes)?;
    f.sync_all()?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

#[derive(Clone)]
pub(crate) struct LocalRoute {
    base: String,
    /// Where a producer's domain calls go: this Mac's daemon on loopback, whatever `base` is.
    domain_base: String,
    agent: ureq::Agent,
    timeout: Duration,
    pairing: Option<Arc<Pairing>>,
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into()
}

impl LocalRoute {
    pub(crate) fn new(timeout: Duration) -> Self {
        let c = config();
        let host = if c.listen.is_unspecified() || c.listen.is_loopback() { "127.0.0.1".to_string() } else { c.listen.to_string() };
        let port = c.port;
        LocalRoute { base: format!("http://{host}:{port}"), domain_base: format!("http://127.0.0.1:{port}"), agent: agent(timeout), timeout, pairing: None }
    }

    /// The daemon at `base`, its domain calls included.
    pub(crate) fn at(base: String, timeout: Duration) -> Self {
        LocalRoute { domain_base: base.clone(), base, agent: agent(timeout), timeout, pairing: None }
    }

    /// Signs as the user with the key this terminal paired, pairing now on first use; unsigned,
    /// and said on stderr, when pairing is impossible.
    pub(crate) fn signed(mut self) -> Self {
        match PAIRING.signer(&self.unsigned()) {
            Ok(_) => self.pairing = Some(PAIRING.clone()),
            Err(e) => eprintln!("{}", trf!("alondra: answers will go unsigned ({e})", "alondra: las respuestas irán sin firmar ({e})")),
        }
        self
    }

    fn unsigned(&self) -> Client {
        Client::over(Arc::new(LocalRoute { pairing: None, ..self.clone() }), self.timeout)
    }

    fn headers<B>(&self, mut req: ureq::RequestBuilder<B>, request: &Request<'_>, signer: Option<&Signer>, observed: bool) -> Result<ureq::RequestBuilder<B>> {
        if observed { req = req.config().max_redirects(0).build(); }
        req = req.header("x-alondra-client-version", env!("CARGO_PKG_VERSION"));
        if let Some(operation) = request.operation { req = req.header("x-alondra-mcp-operation", operation); }
        if let Ok(run) = std::env::var("ALONDRA_ORCHESTRATOR_EXECUTION") { req = req.header("x-alondra-orchestrator-execution", run); }
        if let Ok(sid) = std::env::var("ALONDRA_SESSION_ID").or_else(|_| std::env::var("CLAUDE_CODE_SESSION_ID")) { req = req.header("x-alondra-session", sid); }
        match signer {
            Some(s) => Ok(s.sign(request.method, request.path, request.body)?.into_iter().fold(req, |r, (k, v)| r.header(k, v))),
            None => Ok(req),
        }
    }

    fn send_local(&self, request: &Request<'_>, signer: Option<&Signer>, observer: Option<&ResponseObserver>) -> Result<(u16, Vec<u8>)> {
        let (method, path, content_type, body) = (request.method, request.path, request.content_type, request.body);
        let url = format!("{}{path}", self.base);
        let not_responding = tr!("alondrad is not responding (is it running?)", "alondrad no responde (¿está arrancado?)");
        let observed = observer.is_some();
        let mut resp = match method {
            "POST" => self.headers(self.agent.post(url).header("content-type", content_type), request, signer, observed)?.send(body),
            "PUT" => self.headers(self.agent.put(url).header("content-type", content_type), request, signer, observed)?.send(body),
            "DELETE" => self.headers(self.agent.delete(url), &Request { body: b"", ..*request }, signer, observed)?.call(),
            _ => self.headers(self.agent.get(url), &Request { body: b"", ..*request }, signer, observed)?.call(),
        }
        .context(not_responding)?;
        if let Some(observer) = observer { observer.received(); }
        if let Some(version) = resp.headers().get("x-alondra-version").and_then(|v| v.to_str().ok()) {
            crate::client::note_service_version(version);
        }
        let status = resp.status().as_u16();
        Ok((status, resp.body_mut().with_config().limit(MAX_ATTACHMENT_BYTES + 1024 * 1024).read_to_vec()?))
    }
}

impl Local for LocalRoute {
    /// A request to this Mac's alondrad, sent once more after pairing again when alondrad
    /// rejects the stored key or no longer has the device: a 401 is refused before anything runs.
    fn send(&self, request: Request<'_>, observer: Option<&ResponseObserver>) -> Result<(u16, Vec<u8>)> {
        let signer = self.pairing.as_ref().and_then(|pairing| pairing.current());
        let (status, answer) = self.send_local(&request, signer.as_ref(), observer)?;
        if let (Some(pairing), Some(rejected)) = (&self.pairing, &signer)
            && let Some(rejection) = Rejection::of(status, &answer)
            && let Ok(Some(fresh)) = pairing.replace_rejected(rejected, &self.unsigned())
        {
            pairing.tell(rejection);
            return self.send_local(&request, Some(&fresh), observer);
        }
        Ok((status, answer))
    }

    fn domain_get(&self, path: &str, bearer: &str) -> Result<(u16, Vec<u8>)> {
        let url = format!("{}{path}", self.domain_base);
        let mut resp = self.agent.get(url).header("authorization", format!("Bearer {bearer}")).call()?;
        let status = resp.status().as_u16();
        Ok((status, resp.body_mut().read_to_string().unwrap_or_default().into_bytes()))
    }

    fn domain_call(&self, request: Request<'_>, bearer: &str) -> Result<(u16, Option<String>, Vec<u8>)> {
        let url = format!("{}{}", self.domain_base, request.path);
        let mut builder = ureq::http::Request::builder()
            .method(request.method)
            .uri(url)
            .header("content-type", request.content_type)
            .header("authorization", format!("Bearer {bearer}"))
            .header("x-alondra-client-version", env!("CARGO_PKG_VERSION"));
        if let Some(operation) = request.operation {
            builder = builder.header("x-alondra-mcp-operation", operation);
        }
        let mut response = self.agent.run(builder.body(request.body.to_vec())?)?;
        let status = response.status().as_u16();
        let daemon = response.headers().get("x-alondra-version").and_then(|v| v.to_str().ok()).map(str::to_owned);
        Ok((status, daemon, response.body_mut().read_to_string().unwrap_or_default().into_bytes()))
    }

    fn bounded(&self, timeout: Duration) -> Arc<dyn Local> {
        Arc::new(LocalRoute { agent: agent(timeout), timeout, ..self.clone() })
    }
}

/// Said once when this terminal paired again on its own.
pub(crate) fn notice() -> Option<String> {
    PAIRING.take_notice()
}

/// Whether a request that failed with `error` never left this machine.
pub(crate) fn never_left(error: &ureq::Error) -> bool {
    use std::io::ErrorKind;
    match error {
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed | ureq::Error::BadUri(_) => true,
        ureq::Error::Timeout(timeout) => matches!(timeout, ureq::Timeout::Resolve | ureq::Timeout::Connect),
        ureq::Error::Io(io) => matches!(
            io.kind(),
            ErrorKind::ConnectionRefused | ErrorKind::NotFound | ErrorKind::AddrNotAvailable | ErrorKind::PermissionDenied
        ),
        _ => false,
    }
}

#[cfg(test)]
mod signing_tests {
    use super::*;
    use p256::ecdsa::signature::Verifier as _;
    use sha2::Digest as _;

    fn signer() -> Signer {
        Signer { device: "dev".into(), key: p256::ecdsa::SigningKey::from_slice(&[7u8; 32]).unwrap() }
    }

    #[test]
    fn identical_requests_signed_back_to_back_never_repeat() {
        use base64::Engine as _;
        let signer = signer();
        let verifying = *signer.key.verifying_key();
        let mut timestamps = std::collections::HashSet::new();
        let mut signatures = std::collections::HashSet::new();
        for _ in 0..200 {
            let headers = signer.sign("GET", "/views/home", b"").unwrap();
            let timestamp = headers[1].1.clone();
            let signature = headers[2].1.clone();
            let der = base64::engine::general_purpose::STANDARD.decode(&signature).unwrap();
            let message = format!("GET\n/views/home\n{timestamp}\n{}", sha2::Sha256::digest(b"").iter().map(|b| format!("{b:02x}")).collect::<String>());
            verifying.verify(message.as_bytes(), &p256::ecdsa::Signature::from_der(&der).unwrap()).unwrap();
            assert!(timestamps.insert(timestamp));
            assert!(signatures.insert(signature));
        }
    }

    #[test]
    fn independent_signers_at_the_same_timestamp_do_not_repeat_signatures() {
        use base64::Engine as _;
        let first = signer();
        let second = signer();
        let message = format!("GET\n/views/home\n1000\n{}", sha2::Sha256::digest(b"").iter()
            .map(|byte| format!("{byte:02x}")).collect::<String>());
        let mut signatures = std::collections::HashSet::new();
        for source in [&first, &second].into_iter().cycle().take(32) {
            let headers = source.sign_at("GET", "/views/home", b"", 1000).unwrap();
            let encoded = &headers[2].1;
            let der = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
            source.key.verifying_key().verify(message.as_bytes(),
                &p256::ecdsa::Signature::from_der(&der).unwrap()).unwrap();
            assert!(signatures.insert(encoded.clone()), "independent requests must not look like captured replays");
        }
    }

    #[test]
    fn entropy_failure_never_produces_unsigned_or_deterministic_fallback_headers() {
        struct FailedEntropy;
        impl p256::elliptic_curve::rand_core::TryRng for FailedEntropy {
            type Error = std::io::Error;
            fn try_next_u32(&mut self) -> std::io::Result<u32> { Err(std::io::Error::other("synthetic")) }
            fn try_next_u64(&mut self) -> std::io::Result<u64> { Err(std::io::Error::other("synthetic")) }
            fn try_fill_bytes(&mut self, _: &mut [u8]) -> std::io::Result<()> { Err(std::io::Error::other("synthetic")) }
        }
        impl p256::elliptic_curve::rand_core::TryCryptoRng for FailedEntropy {}
        assert!(signer().sign_with_entropy("GET", "/views/home", b"", 1000, &mut FailedEntropy).is_err());
    }

    #[test]
    fn timestamps_increase_across_threads() {
        let handles: Vec<_> = (0..4).map(|_| std::thread::spawn(|| (0..500).map(|_| unique_timestamp()).collect::<Vec<_>>())).collect();
        let mut all: Vec<u64> = handles.into_iter().flat_map(|h| h.join().unwrap()).collect();
        let total = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), total);
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    use crate::client::{Delivery, delivery};
    use alondra_domain_model::AttachmentRef;
    use alondra_view_model::VIEWS_PROTOCOL;
    use std::io::{Read, Write};

    fn lang_name(lang: alondra_client_port::i18n::Lang) -> &'static str {
        match lang { alondra_client_port::i18n::Lang::En => "en", alondra_client_port::i18n::Lang::Es => "es" }
    }

    fn at(base: String) -> Client {
        crate::install();
        Client::over(Arc::new(LocalRoute::at(base, Duration::from_secs(5))), Duration::from_secs(5))
    }
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A synthetic daemon that answers scripted paths, one request per connection.
    fn peer(script: Vec<(&'static str, Value)>) -> (Client, Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(vec![]));
        let log = seen.clone();
        let server = std::thread::spawn(move || {
            for (expected, body) in script {
                let (mut stream, _) = listener.accept().unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0; 4096];
                    let n = stream.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&chunk[..n]);
                    if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&bytes);
                let path = request.split_whitespace().nth(1).unwrap().to_owned();
                log.lock().unwrap().push(path.clone());
                assert_eq!(path, expected);
                let body = serde_json::to_vec(&body).unwrap();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        (at(base), seen, server)
    }

    /// A daemon that takes the request and then answers with `reply`, or never answers.
    fn one_shot(reply: Option<&'static str>) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut chunk = [0; 4096];
            let _ = stream.read(&mut chunk);
            match reply {
                Some(reply) => stream.write_all(reply.as_bytes()).unwrap(),
                None => std::thread::sleep(Duration::from_millis(800)),
            }
        });
        (base, server)
    }

    fn post_to(base: String, timeout: Duration) -> anyhow::Error {
        crate::install();
        let client = Client::over(Arc::new(LocalRoute::at(base, timeout)), timeout);
        client.post("/actions", &serde_json::json!({})).unwrap_err()
    }

    #[test]
    fn a_project_name_with_a_space_and_an_accent_is_encoded_once_in_the_query() {
        let lang = lang_name(alondra_client_port::i18n::lang());
        let path: &'static str = format!("/views/new_request?protocol={VIEWS_PROTOCOL}&client=tui&lang={lang}&project=Mi+proyecto+ni%C3%B1o").leak();
        let (client, seen, server) = peer(vec![(path, Value::Null)]);
        let _ = client.views(&format!("new_request?project={}", alondra_client_port::routes::urlencode("Mi proyecto niño")), None);
        let served = server.join();
        assert_eq!(*seen.lock().unwrap(), [path]);
        served.unwrap();
    }

    #[test]
    fn a_view_key_with_options_asks_for_its_path_with_the_options_in_the_query() {
        let lang = lang_name(alondra_client_port::i18n::lang());
        let section = format!("/views/tasks?protocol={VIEWS_PROTOCOL}&client=tui&lang={lang}&section=finished&after=0");
        let card = format!("/views/card%3Aturn-ended-t-1?protocol={VIEWS_PROTOCOL}&client=tui&lang={lang}");
        let project = format!("/views/new_request?protocol={VIEWS_PROTOCOL}&client=tui&lang={lang}&project=bitomule%2Falondra");
        let expected: [&'static str; 3] = [section.leak(), card.leak(), project.leak()];
        let (client, seen, server) = peer(expected.iter().map(|path| (*path, Value::Null)).collect());
        let _ = client.views("tasks?section=finished&after=0", None);
        let _ = client.views("card:turn-ended-t-1", None);
        let _ = client.views(&format!("new_request?project={}", alondra_client_port::routes::urlencode("bitomule/alondra")), None);
        let served = server.join();
        assert_eq!(*seen.lock().unwrap(), expected);
        served.unwrap();
    }

    #[test]
    fn a_card_attachment_is_read_from_files_by_the_reference_the_daemon_stored() {
        let shot = AttachmentRef { id: "att-1".into(), name: "shot.png".into(), content_digest: "a".repeat(64), content_reference: "files/uploads/u-1/shot.png".into(), bytes: 4 };
        let (client, seen, server) = peer(vec![("/files?path=files%2Fuploads%2Fu-1%2Fshot.png", Value::from("png"))]);
        let bytes = client.content(&shot.content_reference).unwrap();
        server.join().unwrap();
        assert_eq!(*seen.lock().unwrap(), ["/files?path=files%2Fuploads%2Fu-1%2Fshot.png"]);
        assert_eq!(bytes, br#""png""#);
    }

    #[test]
    fn a_daemon_that_is_not_listening_got_nothing() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let error = post_to(format!("http://127.0.0.1:{port}"), Duration::from_secs(2));
        assert_eq!(delivery(&error), Delivery::Unsent, "{error:#}");
    }

    #[test]
    fn a_daemon_that_took_the_request_and_never_answered_may_have_run_it() {
        let (base, server) = one_shot(None);
        let error = post_to(base, Duration::from_millis(300));
        assert_eq!(delivery(&error), Delivery::Unanswered, "{error:#}");
        server.join().unwrap();
    }

    #[test]
    fn a_daemon_error_is_its_own_answer() {
        let (base, server) = one_shot(Some(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: 21\r\nConnection: close\r\n\r\n{\"error\":\"disk full\"}",
        ));
        let error = post_to(base, Duration::from_secs(2));
        assert_eq!(delivery(&error), Delivery::Answered(Some("disk full".into())), "{error:#}");
        server.join().unwrap();
    }
}

#[cfg(test)]
mod pairing_tests {
    use super::*;
    use p256::ecdsa::signature::Verifier as _;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Condvar, Mutex};

    #[derive(Default)]
    struct Seen {
        pairings: usize,
        pairs: usize,
        devices: Vec<(String, String)>,
        entered: usize,
        released: bool,
    }

    /// alondrad's pairing and signature check over TCP. The first `/pairing` is answered only
    /// once `callers` have entered or a second `/pairing` arrives, so racing pairings meet.
    struct Daemon {
        base: String,
        state: Arc<(Mutex<Seen>, Condvar)>,
        server: Option<std::thread::JoinHandle<()>>,
        stop: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Daemon {
        fn start(callers: usize, reject_every_signature: bool) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let state = Arc::new((Mutex::new(Seen::default()), Condvar::new()));
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (shared, stopped) = (state.clone(), stop.clone());
            let server = std::thread::spawn(move || {
                let mut handlers = vec![];
                for stream in listener.incoming() {
                    if stopped.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    let state = shared.clone();
                    handlers.push(std::thread::spawn(move || serve(stream.unwrap(), &state, callers, reject_every_signature)));
                }
                for handler in handlers {
                    handler.join().unwrap();
                }
            });
            Daemon { base, state, server: Some(server), stop }
        }

        fn enter(&self) {
            let (seen, changed) = &*self.state;
            seen.lock().unwrap().entered += 1;
            changed.notify_all();
        }

        fn add_device(&self, device: &str, key: &p256::ecdsa::SigningKey) {
            self.state.0.lock().unwrap().devices.push((device.into(), public(key)));
        }

        fn forget_devices(&self) {
            self.state.0.lock().unwrap().devices.clear();
        }

        fn counts(&self) -> (usize, usize) {
            let seen = self.state.0.lock().unwrap();
            (seen.pairings, seen.pairs)
        }

        fn key_of(&self, device: &str) -> Option<String> {
            self.state.0.lock().unwrap().devices.iter().find(|(id, _)| id == device).map(|(_, key)| key.clone())
        }

        fn client(&self, pairing: &Arc<Pairing>) -> Client {
            let route = LocalRoute::at(self.base.clone(), Duration::from_secs(5));
            pairing.signer(&route.unsigned()).unwrap();
            Client::over(Arc::new(LocalRoute { pairing: Some(pairing.clone()), ..route }), Duration::from_secs(5))
        }
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = TcpStream::connect(self.base.trim_start_matches("http://"));
            if let Some(server) = self.server.take() {
                server.join().unwrap();
            }
        }
    }

    fn public(key: &p256::ecdsa::SigningKey) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_sec1_point(false).as_bytes())
    }

    fn serve(stream: TcpStream, state: &(Mutex<Seen>, Condvar), callers: usize, reject_every_signature: bool) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let (method, path) = (parts.next().unwrap().to_owned(), parts.next().unwrap().to_owned());
        let mut headers = std::collections::HashMap::new();
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            let Some((name, value)) = header.trim_end().split_once(": ") else { break };
            headers.insert(name.to_ascii_lowercase(), value.to_owned());
        }
        let mut body = vec![0; headers.get("content-length").map_or(0, |n| n.parse().unwrap())];
        reader.read_exact(&mut body).unwrap();
        let (seen, changed) = state;
        let (status, reply) = match path.as_str() {
            "/pairing" => {
                let mut seen = seen.lock().unwrap();
                seen.pairings += 1;
                changed.notify_all();
                if seen.pairings == 1 {
                    seen = changed.wait_while(seen, |s| !s.released && s.entered < callers && s.pairings < 2).unwrap();
                }
                seen.released = true;
                changed.notify_all();
                (200, serde_json::json!({"code": format!("code-{}", seen.pairings)}))
            }
            "/pair" => {
                let request: Value = serde_json::from_slice(&body).unwrap();
                let mut seen = seen.lock().unwrap();
                seen.pairs += 1;
                let device = format!("device-{}", seen.pairs);
                seen.devices.push((device.clone(), request["public_key"].as_str().unwrap().to_owned()));
                (200, serde_json::json!({"device_id": device}))
            }
            _ => match verify(&seen.lock().unwrap().devices, &headers, &method, &path, &body) {
                Err(reason) => (401, serde_json::json!({"error": reason})),
                Ok(()) if reject_every_signature => (401, serde_json::json!({"error": "wrong signature"})),
                Ok(()) => (200, serde_json::json!({"ok": true})),
            },
        };
        let reply = serde_json::to_vec(&reply).unwrap();
        let mut stream = stream;
        write!(stream, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", reply.len()).unwrap();
        stream.write_all(&reply).unwrap();
    }

    fn verify(devices: &[(String, String)], headers: &std::collections::HashMap<String, String>, method: &str, path: &str, body: &[u8]) -> Result<(), &'static str> {
        use base64::Engine as _;
        use sha2::Digest as _;
        let device = headers.get("x-alondra-device").ok_or("missing X-Alondra-Device")?;
        let key = devices.iter().find(|(id, _)| id == device).map(|(_, key)| key).ok_or("device not paired")?;
        let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(&base64::engine::general_purpose::STANDARD.decode(key).unwrap()).unwrap();
        let digest: String = sha2::Sha256::digest(body).iter().map(|b| format!("{b:02x}")).collect();
        let message = format!("{method}\n{path}\n{}\n{digest}", headers["x-alondra-timestamp"]);
        let der = base64::engine::general_purpose::STANDARD.decode(&headers["x-alondra-signature"]).unwrap();
        key.verify(message.as_bytes(), &p256::ecdsa::Signature::from_der(&der).unwrap()).map_err(|_| "wrong signature")
    }

    fn write_pair(home: &std::path::Path, device: &str, key: &p256::ecdsa::SigningKey) {
        use p256::pkcs8::{EncodePrivateKey, LineEnding};
        std::fs::write(home.join(KEY_FILE), key.to_pkcs8_pem(LineEnding::LF).unwrap().as_bytes()).unwrap();
        std::fs::write(home.join(DEVICE_FILE), device).unwrap();
    }

    fn new_home() -> crate::test_support::Directory {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    fn key(byte: u8) -> p256::ecdsa::SigningKey {
        p256::ecdsa::SigningKey::from_slice(&[byte; 32]).unwrap()
    }

    /// The pair on disk is one alondrad has: its device carries the stored key.
    fn assert_stored_pair_matches(daemon: &Daemon, pairing: &Pairing) -> Signer {
        let stored = pairing.stored().expect("a stored pair");
        assert_eq!(daemon.key_of(&stored.device), Some(public(&stored.key)), "key and device come from different pairings");
        stored
    }

    fn first_run(daemon: &Arc<Daemon>, pairings: Vec<Arc<Pairing>>) -> Vec<Signer> {
        let callers: Vec<_> = pairings.into_iter().map(|pairing| {
            let daemon = daemon.clone();
            std::thread::spawn(move || {
                daemon.enter();
                pairing.signer(&LocalRoute::at(daemon.base.clone(), Duration::from_secs(5)).unsigned()).unwrap()
            })
        }).collect();
        callers.into_iter().map(|caller| caller.join().unwrap()).collect()
    }

    #[test]
    fn threads_of_one_process_on_a_new_home_pair_once() {
        let home = new_home();
        let daemon = Arc::new(Daemon::start(6, false));
        let pairing = Arc::new(Pairing::at(home.to_path_buf()));
        let signers = first_run(&daemon, vec![pairing.clone(); 6]);
        assert_eq!(daemon.counts(), (1, 1));
        let stored = assert_stored_pair_matches(&daemon, &pairing);
        assert!(signers.iter().all(|signer| signer.same_pair(&stored)));
    }

    #[test]
    fn a_process_waiting_on_the_lock_reuses_the_pair_the_first_one_made() {
        let home = new_home();
        let daemon = Arc::new(Daemon::start(3, false));
        let processes: Vec<_> = (0..3).map(|_| Arc::new(Pairing::at(home.to_path_buf()))).collect();
        let signers = first_run(&daemon, processes.clone());
        assert_eq!(daemon.counts(), (1, 1));
        let stored = assert_stored_pair_matches(&daemon, &processes[0]);
        assert!(signers.iter().all(|signer| signer.same_pair(&stored)));
    }

    #[test]
    fn a_key_left_without_its_device_is_paired_again_whole() {
        let home = new_home();
        let daemon = Daemon::start(1, false);
        std::fs::write(home.join(KEY_FILE), b"left by a cut pairing").unwrap();
        let pairing = Arc::new(Pairing::at(home.to_path_buf()));
        daemon.enter();
        daemon.client(&pairing);
        assert_eq!(daemon.counts(), (1, 1));
        assert_stored_pair_matches(&daemon, &pairing);
    }

    #[test]
    fn a_cut_between_the_two_files_leaves_no_device_to_sign_with() {
        let home = new_home();
        let daemon = Daemon::start(1, false);
        std::fs::write(home.join(DEVICE_FILE), "device-stale").unwrap();
        std::fs::create_dir(home.join(format!(".{DEVICE_FILE}.{}", std::process::id()))).unwrap();
        let pairing = Pairing::at(home.to_path_buf());
        daemon.enter();
        assert!(pairing.signer(&LocalRoute::at(daemon.base.clone(), Duration::from_secs(5)).unsigned()).is_err());
        assert!(pairing.stored().is_none(), "a new key was left beside the device of another pairing");
    }

    #[test]
    fn a_stored_key_alondrad_rejects_is_paired_again_once_and_the_request_goes_through() {
        let home = new_home();
        let daemon = Daemon::start(1, false);
        daemon.add_device("device-mixed", &key(1));
        write_pair(&home, "device-mixed", &key(2));
        daemon.enter();
        let pairing = Arc::new(Pairing::at(home.to_path_buf()));
        let client = daemon.client(&pairing);
        assert_eq!(client.get::<Value>("/views/home").unwrap()["ok"], true);
        assert_eq!(daemon.counts(), (1, 1));
        assert_stored_pair_matches(&daemon, &pairing);
        assert_eq!(client.get::<Value>("/views/home").unwrap()["ok"], true);
        assert_eq!(daemon.counts(), (1, 1));
    }

    #[test]
    fn a_process_pairs_again_at_most_once() {
        let home = new_home();
        let daemon = Daemon::start(1, true);
        daemon.add_device("device-mixed", &key(1));
        write_pair(&home, "device-mixed", &key(1));
        daemon.enter();
        let pairing = Arc::new(Pairing::at(home.to_path_buf()));
        let client = daemon.client(&pairing);
        assert!(client.get::<Value>("/views/home").is_err());
        assert!(client.get::<Value>("/views/home").is_err());
        assert_eq!(daemon.counts(), (1, 1));
    }

    #[test]
    fn a_terminal_whose_device_was_removed_pairs_again_once_and_says_so() {
        let home = new_home();
        let daemon = Daemon::start(1, false);
        write_pair(&home, "device-removed", &key(1));
        daemon.enter();
        let pairing = Arc::new(Pairing::at(home.to_path_buf()));
        let client = daemon.client(&pairing);
        assert_eq!(client.get::<Value>("/views/home").unwrap()["ok"], true);
        assert_eq!(daemon.counts(), (1, 1));
        let stored = assert_stored_pair_matches(&daemon, &pairing);
        assert_ne!(stored.device, "device-removed");
        assert!(pairing.take_notice().is_some_and(|notice| notice.contains("Terminal (")));
        assert!(pairing.take_notice().is_none(), "it is said once");

        daemon.forget_devices();
        assert!(format!("{:#}", client.get::<Value>("/views/home").unwrap_err()).contains("device not paired"));
        assert_eq!(daemon.counts(), (1, 1), "a process pairs again at most once");
    }
}

#[cfg(test)]
mod operation_tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn mcp_operation_header_is_scoped_to_one_client_clone() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut present = vec![];
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut bytes = vec![];
                while !bytes.ends_with(b"\r\n\r\n") { let mut byte = [0]; stream.read_exact(&mut byte).unwrap(); bytes.push(byte[0]); }
                present.push(String::from_utf8(bytes).unwrap().to_ascii_lowercase().contains("x-alondra-mcp-operation: abcd0123\r\n"));
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").unwrap();
            }
            present
        });
        let client = Client::over(Arc::new(LocalRoute::at(base, Duration::from_secs(5))), Duration::from_secs(5));
        let scoped = client.with_mcp_operation("abcd0123");
        assert_eq!(client.operation_id(), None); assert_eq!(scoped.operation_id(), Some("abcd0123"));
        let _: Value = client.get("/echo").unwrap(); let _: Value = scoped.get("/echo").unwrap(); let _: Value = client.get("/echo").unwrap();
        assert_eq!(server.join().unwrap(), [false, true, false]);
    }

    #[test]
    fn a_native_plan_mode_post_carries_its_scoped_mcp_operation_identity() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut observed = vec![];
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut bytes = vec![];
                while !bytes.ends_with(b"\r\n\r\n") {
                    let mut byte = [0]; stream.read_exact(&mut byte).unwrap(); bytes.push(byte[0]);
                }
                let headers = String::from_utf8(bytes).unwrap().to_ascii_lowercase();
                assert!(headers.starts_with("post /agents/plan-mode http/1.1\r\n"));
                assert!(headers.contains("authorization: bearer fixture-capability\r\n"));
                let length = headers.lines().find_map(|line| line.strip_prefix("content-length: ")).unwrap().parse::<usize>().unwrap();
                let mut body = vec![0; length]; stream.read_exact(&mut body).unwrap();
                assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), serde_json::json!({"task_id":"fixture-task"}));
                let present = headers.contains("x-alondra-mcp-operation: abcd0123\r\n");
                observed.push(present);
                let (status, body) = if present {
                    ("200 OK", r#"{"pending":true}"#)
                } else {
                    ("400 Bad Request", r#"{"code":"incompatible_protocol","message":"A native operation identity is required"}"#)
                };
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            observed
        });
        let client = Client::over(Arc::new(LocalRoute::at(base, Duration::from_secs(5))), Duration::from_secs(5));
        let scoped = client.with_mcp_operation("abcd0123");
        let body = serde_json::json!({"task_id":"fixture-task"});
        let first = client.domain_post("/agents/plan-mode", "fixture-capability", &body);
        let native = scoped.domain_post("/agents/plan-mode", "fixture-capability", &body);
        let last = client.domain_post("/agents/plan-mode", "fixture-capability", &body);
        assert_eq!(server.join().unwrap(), [false, true, false]);
        assert!(first.is_err());
        assert_eq!(native.unwrap()["pending"], true);
        assert!(last.is_err());
    }
}
