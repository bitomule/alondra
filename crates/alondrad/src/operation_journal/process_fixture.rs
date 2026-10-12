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
            Command::CreateTask {
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
                    inputs: vec![],
                }],
                card: CardSpec::derived(CardKind::Provide, "Answer the question"),
            },
        );
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
                let c = Connection::open(home.join("service-receipts.sqlite")).unwrap();
                let records: i64 = c
                    .query_row(
                        "SELECT count(*) FROM service_receipts WHERE journal_issuer IS NOT NULL",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                let intervals: i64 = c
                    .query_row("SELECT count(*) FROM operation_retired", [], |row| {
                        row.get(0)
                    })
                    .unwrap();
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
