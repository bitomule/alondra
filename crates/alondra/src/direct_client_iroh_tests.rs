//! `Connection` against a fake Mac: a real iroh endpoint on loopback that behaves as told, and a
//! minimal direct HTTP listener. Both open each envelope with the Mac's keys and record its bytes.
use super::*;
use iroh::endpoint::{PortmapperConfig, presets};
use iroh::{Endpoint, RelayMode};
use std::io::{BufRead as _, BufReader, Read as _};
use std::net::{Ipv4Addr, TcpListener};

pub(crate) const MAC: &str = "mac-fake";
pub(crate) const DEVICE: &str = "client-fake";

pub(crate) fn mac_signing() -> SigningKey {
    SigningKey::from_slice(&[5u8; 32]).unwrap()
}
pub(crate) fn mac_agreement() -> SecretKey {
    SecretKey::from_slice(&[6u8; 32]).unwrap()
}
pub(crate) fn device_signing() -> SigningKey {
    SigningKey::from_slice(&[7u8; 32]).unwrap()
}
pub(crate) fn device_agreement() -> SecretKey {
    SecretKey::from_slice(&[8u8; 32]).unwrap()
}
pub(crate) fn wire(key: &VerifyingKey) -> String {
    B64.encode(key.to_sec1_point(false).as_bytes())
}

/// Opens a request envelope as the Mac and seals the answer it gets.
pub(crate) fn answer(request: &[u8], routes: &Value) -> Vec<u8> {
    let envelope: Envelope = serde_json::from_slice(request).unwrap();
    let plain = envelope
        .open(device_signing().verifying_key(), &mac_agreement(), DEVICE, MAC, MAC, now_ms())
        .unwrap();
    let request: Value = serde_json::from_slice(&plain).unwrap();
    let body = if request["path"] == "/routes" { routes.clone() } else { json!({"path": request["path"]}) };
    let sealed = Envelope::seal_random(
        EnvelopeContext {
            route: MAC,
            sender: MAC,
            recipient: DEVICE,
            request_id: &envelope.request_id,
            kind: "response",
            expires_at_ms: now_ms() + 60_000,
        },
        &serde_json::to_vec(&json!({"status": 200, "body": body})).unwrap(),
        &mac_signing(),
        &device_agreement().public_key(),
    )
    .unwrap();
    serde_json::to_vec(&sealed).unwrap()
}

/// What the direct HTTP listener answers to a request, as the Mac or as whoever sits in its path.
#[derive(Clone, Copy)]
enum Reply {
    Sealed,
    Bare(u16, &'static str),
    /// A response envelope for the request whose ciphertext does not open.
    Tampered,
}

/// The fake Mac's direct HTTP listener; it records every request envelope.
struct Http {
    address: String,
    received: Arc<Mutex<Vec<Vec<u8>>>>,
    routes: Arc<Mutex<Value>>,
    reply: Arc<Mutex<Reply>>,
}

impl Http {
    fn start() -> Self {
        Self::replying(Reply::Sealed)
    }

    fn replying(first: Reply) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let received = Arc::new(Mutex::new(Vec::new()));
        let routes = Arc::new(Mutex::new(json!({"lan": []})));
        let reply = Arc::new(Mutex::new(first));
        let (seen, routes_given, reply_with) = (received.clone(), routes.clone(), reply.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                    if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let (status, reply) = if line.starts_with("GET /direct/hello") {
                    (200, serde_json::to_vec(&json!({"mac_id": MAC, "name": "Fake", "signing_key": wire(mac_signing().verifying_key()),
                        "agreement_key": B64.encode(mac_agreement().public_key().to_sec1_point(false).as_bytes())})).unwrap())
                } else {
                    seen.lock().unwrap().push(body.clone());
                    let current = *reply_with.lock().unwrap();
                    match current {
                        Reply::Sealed => (200, answer(&body, &routes_given.lock().unwrap())),
                        Reply::Bare(status, text) => (status, text.as_bytes().to_vec()),
                        Reply::Tampered => {
                            let mut sealed: Value = serde_json::from_slice(&answer(&body, &Value::Null)).unwrap();
                            sealed["ciphertext"] = json!(B64.encode([0u8; 48]));
                            (200, serde_json::to_vec(&sealed).unwrap())
                        }
                    }
                };
                let head = format!("HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", reply.len());
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&reply);
            }
        });
        Self { address, received, routes, reply }
    }

    fn received(&self) -> Vec<Vec<u8>> {
        self.received.lock().unwrap().clone()
    }
}

/// What the fake Mac's iroh endpoint does with a request stream.
#[derive(Clone, Copy)]
pub(crate) enum Behaviour {
    Answer,
    /// Reads the whole envelope, then drops the connection without answering.
    DropAfterRequest,
    Refuse(&'static str),
}

pub(crate) struct FakeIroh {
    runtime: tokio::runtime::Runtime,
    endpoint: Endpoint,
    received: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl FakeIroh {
    pub(crate) fn start(behaviour: Behaviour) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let endpoint = runtime.block_on(async {
            Endpoint::builder(presets::Minimal)
                .secret_key(iroh::SecretKey::from_bytes(&crate::service_client::random::<32>().unwrap()))
                .alpns(vec![b"alondra/1".to_vec()])
                .relay_mode(RelayMode::Disabled)
                .portmapper_config(PortmapperConfig::Disabled)
                .clear_ip_transports()
                .bind_addr((Ipv4Addr::LOCALHOST, 0))
                .unwrap()
                .bind()
                .await
                .unwrap()
        });
        let received = Arc::new(Mutex::new(Vec::new()));
        let (server, seen) = (endpoint.clone(), received.clone());
        runtime.spawn(async move {
            while let Some(incoming) = server.accept().await {
                let seen = seen.clone();
                let mac_endpoint = server.id();
                tokio::spawn(async move {
                    let Ok(connection) = incoming.await else { return };
                    let (mut send, mut recv) = connection.accept_bi().await.unwrap();
                    let hello: Value = serde_json::from_slice(&recv.read_to_end(4096).await.unwrap()).unwrap();
                    alondra_e2e::verify_iroh_hello(
                        device_signing().verifying_key(),
                        hello["signature"].as_str().unwrap(),
                        MAC,
                        DEVICE,
                        connection.remote_id().as_bytes(),
                        mac_endpoint.as_bytes(),
                    )
                    .expect("the terminal signs its hello with its pairing key, over both endpoints");
                    send.write_all(br#"{"ok":true}"#).await.unwrap();
                    send.finish().unwrap();
                    while let Ok((mut send, mut recv)) = connection.accept_bi().await {
                        let request = recv.read_to_end(256 * 1024).await.unwrap();
                        seen.lock().unwrap().push(request.clone());
                        let reply = match behaviour {
                            Behaviour::Answer => [&b"{\"response\":"[..], &answer(&request, &Value::Null), b"}"].concat(),
                            Behaviour::DropAfterRequest => return connection.close(7u32.into(), b"gone"),
                            Behaviour::Refuse(error) => json!({"error": error}).to_string().into_bytes(),
                        };
                        send.write_all(&reply).await.unwrap();
                        send.finish().unwrap();
                    }
                });
            }
        });
        Self { runtime, endpoint, received }
    }

    pub(crate) fn dial(&self) -> crate::iroh_client::Dial {
        crate::iroh_client::Dial {
            endpoint: self.endpoint.id().to_string(),
            addrs: self.endpoint.bound_sockets().iter().map(ToString::to_string).collect(),
            relay: None,
        }
    }

    pub(crate) fn received(&self) -> Vec<Vec<u8>> {
        self.received.lock().unwrap().clone()
    }
}

impl Drop for FakeIroh {
    fn drop(&mut self) {
        self.runtime.block_on(self.endpoint.close());
    }
}

struct Terminal {
    home: crate::test_support::Directory,
    connection: Connection,
}

fn terminal(http: &Http, dial: Option<crate::iroh_client::Dial>) -> Terminal {
    let home = crate::test_support::Directory::new();
    fs::create_dir_all(&*home).unwrap();
    let mac = DirectMac {
        mac_id: MAC.into(),
        name: "Fake".into(),
        mac_signing_key: wire(mac_signing().verifying_key()),
        mac_agreement_key: B64.encode(mac_agreement().public_key().to_sec1_point(false).as_bytes()),
        device_id: DEVICE.into(),
        signing_key: B64.encode(device_signing().to_bytes()),
        agreement_key: B64.encode(device_agreement().to_bytes()),
        routes: vec![http.address.clone()],
    };
    remember(&home, mac.clone()).unwrap();
    if let Some(dial) = dial {
        crate::iroh_client::save_dial(&home, MAC, Some(dial)).unwrap();
    }
    let connection = Connection::at(&home, mac);
    Terminal { home, connection }
}

fn read(terminal: &Terminal) -> Result<(u16, Value)> {
    terminal.connection.request("GET", "/capabilities", None, Duration::from_secs(5))
}

#[test]
fn a_request_goes_over_iroh_when_the_mac_is_reachable_there() {
    let (http, iroh) = (Http::start(), FakeIroh::start(Behaviour::Answer));
    let terminal = terminal(&http, Some(iroh.dial()));
    assert_eq!(read(&terminal).unwrap(), (200, json!({"path": "/capabilities"})));
    assert_eq!(iroh.received().len(), 1);
    assert!(http.received().is_empty(), "nothing went over HTTP");
    assert_eq!(terminal.connection.route().map(|(_, kind)| kind), Some(RouteKind::Lan));
}

#[test]
fn an_envelope_lost_mid_request_on_iroh_goes_byte_for_byte_over_http() {
    let (http, iroh) = (Http::start(), FakeIroh::start(Behaviour::DropAfterRequest));
    let terminal = terminal(&http, Some(iroh.dial()));
    assert_eq!(read(&terminal).unwrap(), (200, json!({"path": "/capabilities"})));
    let (over_iroh, over_http) = (iroh.received(), http.received());
    assert_eq!(over_http[0], over_iroh[0], "the Mac sees the same envelope twice, and its journal answers it once");
}

#[test]
fn an_unreachable_endpoint_falls_back_to_http_and_learns_the_dial_again() {
    let (http, iroh) = (Http::start(), FakeIroh::start(Behaviour::Answer));
    let stale = crate::iroh_client::Dial { addrs: vec!["127.0.0.1:9".into()], ..iroh.dial() };
    let terminal = terminal(&http, Some(stale));
    *http.routes.lock().unwrap() = json!({"lan": [], "iroh_endpoint": iroh.dial().endpoint, "iroh_addrs": iroh.dial().addrs});
    let started = Instant::now();
    assert_eq!(read(&terminal).unwrap(), (200, json!({"path": "/capabilities"})));
    assert!(started.elapsed() < crate::iroh_client::CONNECT_TIMEOUT + Duration::from_secs(3));
    assert_eq!(http.received().len(), 2, "the read, then the routes that tell how to dial the Mac now");
    assert_eq!(crate::iroh_client::load_dial(&terminal.home, MAC), Some(iroh.dial()));
    assert_eq!(read(&terminal).unwrap(), (200, json!({"path": "/capabilities"})));
    assert_eq!((iroh.received().len(), http.received().len()), (1, 2), "the next read goes over iroh");
}

#[test]
fn a_refusal_on_iroh_reads_as_it_does_over_http() {
    let http = Http::start();
    let iroh = FakeIroh::start(Behaviour::Refuse("unauthorized"));
    let terminal = terminal(&http, Some(iroh.dial()));
    let refused = read(&terminal).unwrap_err();
    assert_eq!(refused.downcast_ref::<Answered>().map(|answered| answered.status), Some(401));
    let conflict = FakeIroh::start(Behaviour::Refuse("idempotency_conflict"));
    let terminal = self::terminal(&http, Some(conflict.dial()));
    assert_eq!(read(&terminal).unwrap(), (409, json!({"error": "idempotency_conflict"})));
    assert!(http.received().is_empty(), "a refusal is the Mac's answer: it is not sent again over HTTP");
}

#[test]
fn forgetting_a_mac_forgets_how_to_dial_it() {
    let http = Http::start();
    let iroh = FakeIroh::start(Behaviour::Answer);
    let terminal = terminal(&http, Some(iroh.dial()));
    assert_eq!(forget(&terminal.home, MAC).unwrap(), [MAC]);
    assert_eq!(crate::iroh_client::load_dial(&terminal.home, MAC), None);
}

#[test]
fn only_a_sealed_answer_speaks_for_the_mac_over_http() {
    for reply in [
        Reply::Bare(400, r#"{"error":"rejected","detail":"forged refusal"}"#),
        Reply::Bare(401, r#"{"error":"unauthorized"}"#),
        Reply::Bare(409, r#"{"error":"idempotency_conflict"}"#),
        Reply::Bare(503, r#"{"error":"busy"}"#),
        Reply::Tampered,
    ] {
        let http = Http::replying(reply);
        let terminal = terminal(&http, None);
        let failure = read(&terminal).expect_err("nothing the Mac sealed came back");
        assert_eq!(crate::client::delivery(&failure), crate::client::Delivery::Unanswered, "{failure:#}");
        assert!(!format!("{failure:#}").contains("--repair"), "{failure:#}");
        assert_eq!(terminal.connection.route(), None, "the route that gave it is not kept");
        assert_eq!(http.received().len(), 1);
    }
    let http = Http::start();
    let terminal = terminal(&http, None);
    assert_eq!(read(&terminal).unwrap(), (200, json!({"path": "/capabilities"})));
}

#[test]
fn a_bare_status_from_a_route_the_mac_already_answered_on_is_no_answer_either() {
    for status in [409, 400, 421] {
        let http = Http::start();
        let terminal = terminal(&http, None);
        assert_eq!(read(&terminal).unwrap(), (200, json!({"path": "/capabilities"})));
        assert!(terminal.connection.route().is_some());
        *http.reply.lock().unwrap() = Reply::Bare(status, "{}");
        let failure = read(&terminal).expect_err("a bare status is not the Mac's answer");
        assert_eq!(crate::client::delivery(&failure), crate::client::Delivery::Unanswered, "{status}: {failure:#}");
        assert_eq!(terminal.connection.route(), None, "{status} rules the route out");
    }
}
