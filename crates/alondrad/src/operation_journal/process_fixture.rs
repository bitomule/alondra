use alondra_core::domain::{
    Actor, CardKind, CardSpec, Command, CommandEnvelope, LaunchPlan, PROTOCOL_VERSION,
    QuestionOption, Recipient, Scope,
};
use alondra_core::views::action_id::{ActionId, Verb};
use axum::{body::Body, extract::State, http::Request, middleware::Next, response::Response};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use p256::ecdsa::SigningKey;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::sync::Notify;

struct Harness {
    home: Arc<crate::test_support::Directory>,
    hold_action: AtomicBool,
    hold_ack: AtomicBool,
    release: Notify,
    posts: AtomicU64,
    results: AtomicU64,
    acknowledgments: AtomicU64,
}
fn emit(value: Value) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "ALONDRA_FIXTURE {}",
        serde_json::to_string(&value).unwrap()
    )
    .unwrap();
    out.flush().unwrap();
}
fn effects(home: &Path) -> i64 {
    let c = Connection::open(crate::domain_database_migration::active_path(home)).unwrap();
    c.query_row(
        "SELECT count(*) FROM interactions WHERE id='fixture-question' AND answer IS NOT NULL",
        [],
        |row| row.get(0),
    )
    .unwrap()
}
fn stats(harness: &Harness) -> Value {
    let path = harness.home.join("action-results.sqlite");
    let (records, intervals) = if path.exists() {
        let c = Connection::open(path).unwrap();
        (
            c.query_row("SELECT count(*) FROM action_results", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            c.query_row("SELECT count(*) FROM operation_retired", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        )
    } else {
        (0, 0)
    };
    json!({"event":"stats","effects":effects(&harness.home),"action_posts":harness.posts.load(Ordering::Relaxed),
        "result_gets":harness.results.load(Ordering::Relaxed),"acknowledgments":harness.acknowledgments.load(Ordering::Relaxed),
        "journal_records":records,"retired_intervals":intervals})
}
async fn barrier(
    State(harness): State<Arc<Harness>>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let action = request.method() == "POST" && request.uri().path() == "/actions";
    let command_id = if action {
        let (parts, body) = request.into_parts();
        let bytes = axum::body::to_bytes(body, 256 * 1024).await.unwrap();
        let command = serde_json::from_slice::<Value>(&bytes).unwrap()["command_id"]
            .as_str()
            .unwrap()
            .to_owned();
        request = Request::from_parts(parts, Body::from(bytes));
        Some(command)
    } else {
        None
    };
    let ack = request.method() == "POST" && request.uri().path() == "/operation-journal/ack";
    if action {
        harness.posts.fetch_add(1, Ordering::Relaxed);
    }
    if ack {
        harness.acknowledgments.fetch_add(1, Ordering::Relaxed);
    }
    if request.uri().path().starts_with("/operation-results/") {
        harness.results.fetch_add(1, Ordering::Relaxed);
    }
    if ack && harness.hold_ack.swap(false, Ordering::Relaxed) {
        let released = harness.release.notified();
        tokio::pin!(released);
        released.as_mut().enable();
        emit(json!({"event":"ack_entered","effects":effects(&harness.home)}));
        released.await;
    }
    let response = next.run(request).await;
    if action && harness.hold_action.swap(false, Ordering::Relaxed) {
        let released = harness.release.notified();
        tokio::pin!(released);
        released.as_mut().enable();
        emit(json!({"event":"applied","command_id":command_id,"effects":effects(&harness.home)}));
        released.await;
    }
    response
}
fn seed(domain: &crate::domain_runtime::DomainRuntime, actor: Actor, id: &str, command: Command) {
    let state = domain.state().unwrap();
    domain
        .execute(
            &actor,
            &CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: state.data_epoch.clone(),
                command_id: id.into(),
                expected_revision: state.revision,
                command,
            },
            None,
            || Ok(()),
        )
        .unwrap();
}

#[ignore = "stdin-controlled real server for the separate client process E2E target"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_stack_fixture_server() {
    let home = Arc::new(match std::env::var("ALONDRA_FIXTURE_HOME") {
        Ok(path) => crate::test_support::Directory::for_process_fixture(
            Path::new(&path),
            &std::env::var("ALONDRA_FIXTURE_TOKEN").expect("fixture token required"),
        )
        .expect("parent-owned marked fixture HOME required"),
        Err(std::env::VarError::NotPresent) => crate::test_support::Directory::new(),
        Err(error) => panic!("invalid fixture HOME: {error}"),
    });
    std::fs::create_dir_all(&**home).unwrap();
    let database = crate::domain_database_migration::active_path(&home);
    let fresh = !database.exists();
    assert!(
        !home.join("service.toml").exists() || !fresh,
        "initialized fixture domain cannot disappear"
    );
    let domain = crate::domain_runtime::DomainRuntime::open(&home).unwrap();
    if fresh {
        seed(
            &domain,
            Actor::System,
            "fixture-agent",
            Command::RegisterAgent {
                agent_id: "fixture-agent".into(),
                name: "Fixture agent".into(),
                at: 0,
            },
        );
        seed(
            &domain,
            Actor::User {
                user_id: "seed-user".into(),
            },
            "fixture-request",
            Command::CreateRequest {
                request_id: "fixture-request".into(),
                body: "Fixture request".into(),
                auto_merge: false,
                maintenance: false,
                message: false,
            },
        );
        seed(
            &domain,
            Actor::Orchestrator { generation: 0 },
            "fixture-task",
            Command::CreateTask { plan_items: None, continues: None,
                task_id: "fixture-task".into(),
                request_id: "fixture-request".into(),
                title: "Fixture task".into(),
                agent_id: Some("fixture-agent".into()),
                review_prs: false,
                dependencies: Default::default(),
                stack: None,
                launch: Some(LaunchPlan::new("node", home.to_str().unwrap())),
            },
        );
        seed(
            &domain,
            Actor::Agent {
                agent_id: "fixture-agent".into(),
            },
            "fixture-question",
            Command::AskQuestion {
                attachments: vec![],
                interaction_id: "fixture-question".into(),
                scope: Scope::Task {
                    task_id: "fixture-task".into(),
                },
                recipient: Recipient::User,
                body: "Approve the fixture change?".into(),
                blocking: true,
                options: vec![QuestionOption {
                    id: "yes".into(),
                    label: "Yes".into(),
                    description: None,
                }],
                card: CardSpec::derived(CardKind::Provide, "Answer the question"),
            },
        );
        for id in ["fixture-detail-b", "fixture-detail-c"] {
            seed(&domain, Actor::Agent { agent_id: "fixture-agent".into() }, id,
                Command::AskQuestion {
                    attachments: vec![], interaction_id: id.into(),
                    scope: Scope::Task { task_id: "fixture-task".into() }, recipient: Recipient::User,
                    body: format!("Independent detail {id}"), blocking: false,
                    options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None }],
                    card: CardSpec::derived(CardKind::Provide, "Another question"),
                });
        }
    }
    let current = domain.state().unwrap();
    let epoch = current.data_epoch.0.clone();
    let action = ActionId::new(
        Verb::Ans,
        "fixture-question",
        current.interactions[&"fixture-question".into()].revision,
        None,
    )
    .unwrap()
    .to_string();
    drop(current);
    let identity_path = home.join("process-fixture.json");
    let initial: Value = if fresh {
        let identity = json!({"data_epoch":epoch,"action_id":action});
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&identity_path)
            .unwrap();
        file.write_all(&serde_json::to_vec(&identity).unwrap())
            .unwrap();
        file.sync_all().unwrap();
        identity
    } else {
        serde_json::from_slice(
            &std::fs::read(&identity_path).expect("initial fixture ticket required"),
        )
        .unwrap()
    };
    assert_eq!(
        initial["data_epoch"].as_str(),
        Some(epoch.as_str()),
        "fixture epoch changed"
    );
    let action = initial["action_id"].as_str().unwrap().to_owned();
    if let Ok(relay_url) = std::env::var("ALONDRA_FIXTURE_RELAY_URL") {
        relay_daemon(home, Arc::new(domain), &relay_url, &epoch, &action).await;
        return;
    }
    if std::env::var_os("ALONDRA_FIXTURE_IROH").is_some()
        || std::env::var_os("ALONDRA_FIXTURE_IROH_RELAY").is_some()
    {
        iroh_daemon(home, Arc::new(domain), &epoch, &action).await;
        return;
    }
    drop(domain);
    let state = crate::domain_bootstrap::DomainDaemon::open(&home).unwrap();
    let signing = SigningKey::from_slice(&[41u8; 32]).unwrap();
    let auth = state.auth();
    let device = auth
        .pair(
            &auth.new_pairing_code(),
            "Fixture client",
            &STANDARD.encode(signing.verifying_key().to_sec1_bytes()),
        )
        .unwrap();
    let harness = Arc::new(Harness {
        home: home.clone(),
        hold_action: AtomicBool::new(false),
        hold_ack: AtomicBool::new(false),
        release: Notify::new(),
        posts: AtomicU64::new(0),
        results: AtomicU64::new(0),
        acknowledgments: AtomicU64::new(0),
    });
    let router = crate::domain_bootstrap::router(state.clone())
        .layer(axum::middleware::from_fn_with_state(
            state.auth(),
            crate::auth::guard,
        ))
        .layer(axum::middleware::from_fn_with_state(
            harness.clone(),
            barrier,
        ))
        .layer(axum::Extension(home.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            let _ = stopped.await;
        })
        .await
        .unwrap();
    });
    emit(
        json!({"event":"ready","base_url":base,"data_epoch":epoch,"epoch":epoch,"action_id":action,"option_id":"yes",
        "device_id":device.id,"signing_seed_base64":STANDARD.encode([41u8;32]),"fixture_home":home.to_str().unwrap()}),
    );
    let (commands, mut incoming) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let reader = std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let value: Value = serde_json::from_str(&line.unwrap()).unwrap();
            let stopped = value["command"] == "stop";
            if commands.send(value).is_err() || stopped {
                break;
            }
        }
    });
    while let Some(command) = incoming.recv().await {
        match command["command"].as_str().unwrap() {
            "hold_next_action" => {
                harness.hold_action.store(true, Ordering::Relaxed);
                emit(json!({"event":"armed","kind":"action"}));
            }
            "hold_next_ack" => {
                harness.hold_ack.store(true, Ordering::Relaxed);
                emit(json!({"event":"armed","kind":"ack"}));
            }
            "release" => {
                harness.release.notify_waiters();
                emit(json!({"event":"released"}));
            }
            "stats" => emit(stats(&harness)),
            "stop" => break,
            other => panic!("unexpected fixture command {other}"),
        }
    }
    harness.release.notify_waiters();
    let _ = stop.send(());
    server.await.unwrap();
    reader.join().unwrap();
    emit(json!({"event":"stopped"}));
    drop(state);
    drop(harness);
    drop(home);
}

async fn relay_daemon(
    home: Arc<crate::test_support::Directory>,
    domain: Arc<crate::domain_runtime::DomainRuntime>,
    relay_url: &str,
    epoch: &str,
    action: &str,
) {
    let service =
        crate::service::Service::process_fixture_service(home.clone(), domain.clone(), relay_url)
            .await;
    let runner = tokio::spawn(service.clone().run());
    emit(
        json!({"event":"ready","data_epoch":epoch,"action_id":action,"option_id":"yes","fixture_home":home.to_str().unwrap()}),
    );
    let (commands, mut incoming) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let reader = std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let value: Value = serde_json::from_str(&line.unwrap()).unwrap();
            let stopped = value["command"] == "stop";
            if commands.send(value).is_err() || stopped {
                break;
            }
        }
    });
    while let Some(command) = incoming.recv().await {
        match command["command"].as_str().unwrap() {
            "stats" => {
                let (records, intervals) = service_receipts(&home);
                emit(
                    json!({"event":"stats","effects":effects(&home),"journal_records":records,"retired_intervals":intervals}),
                );
            }
            "stop" => break,
            other => panic!("unexpected daemon fixture command {other}"),
        }
    }
    runner.abort();
    let _ = runner.await;
    reader.join().unwrap();
    drop(service);
    drop(domain);
    emit(json!({"event":"stopped"}));
    drop(home);
}

fn service_receipts(home: &Path) -> (i64, i64) {
    let c = Connection::open(home.join("service-receipts.sqlite")).unwrap();
    let records = c
        .query_row(
            "SELECT count(*) FROM service_receipts WHERE journal_issuer IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let intervals = c
        .query_row("SELECT count(*) FROM operation_retired", [], |row| row.get(0))
        .unwrap();
    (records, intervals)
}

const PHONE_SIGNING: [u8; 32] = [33u8; 32];
const PHONE_AGREEMENT: [u8; 32] = [34u8; 32];
const MAC_SIGNING: [u8; 32] = [1u8; 32];
const MAC_AGREEMENT: [u8; 32] = [2u8; 32];

/// The Mac paired with one phone, `phone-1`, reached only over its iroh endpoint on loopback. The
/// keys come from the fixed seeds above, which the ready line hands to the client.
fn iroh_service(
    home: &Path,
    domain: Arc<crate::domain_runtime::DomainRuntime>,
) -> Arc<crate::service::Service> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let signing = SigningKey::from_slice(&PHONE_SIGNING).unwrap();
    let agreement = p256::SecretKey::from_slice(&PHONE_AGREEMENT).unwrap();
    if !home.join("service.toml").exists() {
        for (name, bytes) in [("signing.key", MAC_SIGNING), ("agreement.key", MAC_AGREEMENT)] {
            let path = home.join(name);
            std::fs::write(&path, URL_SAFE_NO_PAD.encode(bytes)).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let approved = json!([{"id":"phone-1","name":"Fixture iPhone","created_at":0,"last_seen":null,
            "public_key":STANDARD.encode(signing.verifying_key().to_sec1_bytes())}]);
        std::fs::write(home.join("devices.json"), serde_json::to_vec(&approved).unwrap()).unwrap();
        std::fs::write(
            home.join("service.toml"),
            format!(
                "mac_id = \"mac-1\"\nsigning_key_path = \"signing.key\"\nagreement_key_path = \"agreement.key\"\n\
                 [[devices]]\ndevice_id = \"phone-1\"\nsigning_key = \"{}\"\nagreement_key = \"{}\"\n",
                URL_SAFE_NO_PAD.encode(signing.verifying_key().to_sec1_bytes()),
                URL_SAFE_NO_PAD.encode(agreement.public_key().to_sec1_bytes())
            ),
        )
        .unwrap();
    }
    let auth = Arc::new(crate::auth::Auth::open(home).unwrap());
    let client = crate::client_routes::Client::open(domain.clone(), home, Default::default());
    let service = crate::service::Service::open(home, 0, auth)
        .unwrap()
        .expect("fixture service configured")
        .with_domain(domain)
        .with_client(client);
    Arc::new(service)
}

async fn bind_iroh(service: &crate::service::Service, port: u16) -> iroh::Endpoint {
    if let Ok(relay) = std::env::var("ALONDRA_FIXTURE_IROH_RELAY") {
        let relay: iroh::RelayUrl = relay.parse().expect("valid fixture iroh relay URL required");
        let transport = iroh::endpoint::QuicTransportConfig::builder()
            .max_concurrent_bidi_streams(iroh::endpoint::VarInt::from_u32(crate::iroh_endpoint::MAX_STREAMS))
            .max_concurrent_uni_streams(iroh::endpoint::VarInt::from_u32(0))
            .build();
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(service.iroh_key().clone())
            .alpns(vec![crate::iroh_endpoint::ALPN.to_vec()])
            .clear_ip_transports()
            .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::empty()))
            .transport_config(transport)
            .bind()
            .await
            .unwrap();
        endpoint.insert_relay(relay.clone(), Arc::new(iroh::RelayConfig::from(relay))).await;
        return endpoint;
    }
    let config = crate::iroh_endpoint::IrohConfig {
        relay: "off".into(),
        listen: Some(std::net::Ipv4Addr::LOCALHOST.into()),
        port,
        portmapper: false,
        ..Default::default()
    };
    crate::iroh_endpoint::bind(service.iroh_key().clone(), &config)
        .await
        .unwrap()
}

/// The request inside a sealed envelope, or `Null` for anything else, such as the hello.
fn opened_request(service: &crate::service::Service, bytes: &[u8]) -> Value {
    let Ok(envelope) = serde_json::from_slice::<alondra_e2e::Envelope>(bytes) else {
        return Value::Null;
    };
    let phone = SigningKey::from_slice(&PHONE_SIGNING).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    envelope
        .open(phone.verifying_key(), service.agreement_secret(), &envelope.sender, "mac-1", "mac-1", now)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

/// The daemon's iroh endpoint as `iroh_endpoint::serve` runs it: a hello proven with a paired
/// key, then one envelope per stream answered by `iroh_endpoint::answer`. Around the answer the
/// fixture counts what arrived and can hold an applied action's answer before it leaves.
async fn serve_iroh(endpoint: iroh::Endpoint, service: Arc<crate::service::Service>, harness: Arc<Harness>) {
    while let Some(incoming) = endpoint.accept().await {
        let (service, harness) = (service.clone(), harness.clone());
        tokio::spawn(async move {
            let Ok(connection) = incoming.await else { return };
            let Some(device_id) = iroh_hello(&service, &connection).await else {
                return connection.close(
                    iroh::endpoint::VarInt::from_u32(crate::iroh_endpoint::close::UNAUTHORIZED),
                    b"unauthorized",
                );
            };
            while let Ok((send, recv)) = connection.accept_bi().await {
                tokio::spawn(iroh_stream(service.clone(), harness.clone(), device_id.clone(), send, recv));
            }
        });
    }
}

async fn iroh_hello(service: &crate::service::Service, connection: &iroh::endpoint::Connection) -> Option<String> {
    let (mut send, mut recv) = connection.accept_bi().await.ok()?;
    let hello: Value = serde_json::from_slice(&recv.read_to_end(4096).await.ok()?).ok()?;
    let device_id = hello["device_id"].as_str()?.to_owned();
    let proven = service.iroh_hello_proven(
        &device_id,
        hello["signature"].as_str()?,
        connection.remote_id().as_bytes(),
    );
    let reply = if proven { json!({"ok": true}) } else { json!({"error": "unauthorized"}) };
    if send.write_all(reply.to_string().as_bytes()).await.is_ok() {
        let _ = send.finish();
        let _ = send.stopped().await;
    }
    proven.then_some(device_id)
}

async fn iroh_stream(
    service: Arc<crate::service::Service>,
    harness: Arc<Harness>,
    device_id: String,
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
) {
    let Ok(bytes) = recv.read_to_end(crate::iroh_endpoint::MAX_REQUEST).await else { return };
    let received = std::time::SystemTime::now();
    let request = opened_request(&service, &bytes);
    timing("received", &request, received);
    let (method, path) = (request["method"].as_str().unwrap_or_default(), request["path"].as_str().unwrap_or_default());
    let action = method == "POST" && path == "/actions";
    if action {
        harness.posts.fetch_add(1, Ordering::Relaxed);
    }
    if path.starts_with("/operation-results/") {
        harness.results.fetch_add(1, Ordering::Relaxed);
    }
    if method == "POST" && path == "/operation-journal/ack" {
        harness.acknowledgments.fetch_add(1, Ordering::Relaxed);
    }
    let reply = crate::iroh_endpoint::answer(&service, &device_id, &bytes).await;
    timing("committed", &request, std::time::SystemTime::now());
    if action && harness.hold_action.swap(false, Ordering::Relaxed) {
        let released = harness.release.notified();
        tokio::pin!(released);
        released.as_mut().enable();
        emit(json!({"event":"applied","command_id":request["body"]["command_id"],"effects":effects(&harness.home)}));
        released.await;
    }
    if send.write_all(&reply).await.is_ok() {
        let _ = send.finish();
    }
}

fn timing(stage: &str, request: &Value, at: std::time::SystemTime) {
    if std::env::var_os("ALONDRA_FIXTURE_MEASURE").is_none() {
        return;
    }
    let us = at.duration_since(std::time::UNIX_EPOCH).unwrap().as_micros() as u64;
    emit(json!({"event":"timing","stage":stage,"command_id":request["body"]["command_id"],
        "us":us,"method":request["method"],"path":request["path"]}));
}

/// One life of the fixture's iroh endpoint, on a runtime of its own. iroh frees the UDP port only
/// after every task holding the endpoint is gone, and netwatch then closes the socket on a blocking
/// thread. Dropping the runtime drops those tasks and joins its blocking threads, so once `close`
/// returns the port can be bound again.
struct IrohLife {
    runtime: tokio::runtime::Runtime,
    endpoint: iroh::Endpoint,
}

impl IrohLife {
    async fn open(service: &Arc<crate::service::Service>, harness: &Arc<Harness>, port: u16) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let bound = service.clone();
        let endpoint = runtime.spawn(async move { bind_iroh(&bound, port).await }).await.unwrap();
        runtime.spawn(serve_iroh(endpoint.clone(), service.clone(), harness.clone()));
        Self { runtime, endpoint }
    }

    async fn close(self) {
        let Self { runtime, endpoint } = self;
        runtime.spawn(async move { endpoint.close().await }).await.unwrap();
        tokio::task::spawn_blocking(move || drop(runtime)).await.unwrap();
    }
}

fn fixture_iroh_address(endpoint: &iroh::Endpoint) -> std::net::SocketAddr {
    endpoint.bound_sockets().first().copied().unwrap_or_else(|| ([127, 0, 0, 1], 0).into())
}

fn stdin_commands() -> (tokio::sync::mpsc::UnboundedReceiver<Value>, std::thread::JoinHandle<()>) {
    let (commands, incoming) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let reader = std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let value: Value = serde_json::from_str(&line.unwrap()).unwrap();
            let stopped = value["command"] == "stop";
            if commands.send(value).is_err() || stopped {
                break;
            }
        }
    });
    (incoming, reader)
}

/// `ALONDRA_FIXTURE_IROH_PORT` binds the endpoint on the port an earlier run of this HOME used, so
/// a restarted daemon is found where the phone last reached it.
async fn iroh_daemon(
    home: Arc<crate::test_support::Directory>,
    domain: Arc<crate::domain_runtime::DomainRuntime>,
    epoch: &str,
    action: &str,
) {
    let service = iroh_service(&home, domain.clone());
    let port = std::env::var("ALONDRA_FIXTURE_IROH_PORT").map_or(0, |port| port.parse().unwrap());
    let harness = Arc::new(Harness {
        home: home.clone(),
        hold_action: AtomicBool::new(false),
        hold_ack: AtomicBool::new(false),
        release: Notify::new(),
        posts: AtomicU64::new(0),
        results: AtomicU64::new(0),
        acknowledgments: AtomicU64::new(0),
    });
    let first = IrohLife::open(&service, &harness, port).await;
    let address = fixture_iroh_address(&first.endpoint);
    let mut endpoint = Some(first);
    // While the endpoint is closed the fixture holds its port, so no other process takes it before
    // the endpoint opens there again.
    let mut held = None;
    let relay = std::env::var("ALONDRA_FIXTURE_IROH_RELAY").ok();
    emit(json!({"event":"ready","data_epoch":epoch,"action_id":action,"option_id":"yes",
        "fixture_home":home.to_str().unwrap(),"mac_id":"mac-1","device_id":"phone-1",
        "signing_seed_base64":STANDARD.encode(PHONE_SIGNING),"agreement_seed_base64":STANDARD.encode(PHONE_AGREEMENT),
        "mac_signing_seed_base64":STANDARD.encode(MAC_SIGNING),"mac_agreement_seed_base64":STANDARD.encode(MAC_AGREEMENT),
        "iroh_endpoint":service.iroh_key().public().to_string(),"iroh_address":address.to_string(),"iroh_relay":relay}));
    let (mut incoming, reader) = stdin_commands();
    while let Some(command) = incoming.recv().await {
        match command["command"].as_str().unwrap() {
            "hold_next_action" => {
                harness.hold_action.store(true, Ordering::Relaxed);
                emit(json!({"event":"armed","kind":"action"}));
            }
            "release" => {
                harness.release.notify_waiters();
                emit(json!({"event":"released"}));
            }
            "close_endpoint" => {
                if let Some(closed) = endpoint.take() {
                    closed.close().await;
                }
                harness.release.notify_waiters();
                held = Some(std::net::UdpSocket::bind(address).expect("the closed endpoint left its port free"));
                emit(json!({"event":"endpoint_closed"}));
            }
            "open_endpoint" => {
                drop(held.take());
                let opened = IrohLife::open(&service, &harness, address.port()).await;
                emit(json!({"event":"endpoint_open","iroh_address":fixture_iroh_address(&opened.endpoint).to_string(),"iroh_relay":relay}));
                endpoint = Some(opened);
            }
            "stats" => {
                let (records, intervals) = service_receipts(&home);
                emit(json!({"event":"stats","effects":effects(&home),"action_posts":harness.posts.load(Ordering::Relaxed),
                    "result_gets":harness.results.load(Ordering::Relaxed),
                    "acknowledgments":harness.acknowledgments.load(Ordering::Relaxed),
                    "journal_records":records,"retired_intervals":intervals}));
            }
            "stop" => break,
            other => panic!("unexpected iroh fixture command {other}"),
        }
    }
    harness.release.notify_waiters();
    if let Some(endpoint) = endpoint.take() {
        endpoint.close().await;
    }
    reader.join().unwrap();
    drop(service);
    drop(domain);
    emit(json!({"event":"stopped"}));
    drop(harness);
    drop(home);
}
