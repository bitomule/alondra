use alondra_engine_admission::AdmissionOf as _;
use super::*;
use std::{future::Future, sync::{Mutex, atomic::{AtomicBool, Ordering}}, task::{Context as TaskContext, Waker}};
use tokio::sync::oneshot;

#[tokio::test]
async fn workers_receive_the_configured_admission_gate() {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = Arc::new(crate::engine::open(&home).unwrap());
    domain.admission().tune(admission::Tuning { spacing: 0, limit: Some(admission::Limit::Fixed(1)) });
    let mut config = Config::defaults();
    config.home = home.to_path_buf();
    config.fast_timing = true;
    let PreparedWorkers(ready) = prepare_workers(&config, domain.clone(), None).await;
    assert_eq!(ready.admission().snapshot(&*ready, 0).unwrap().limit, admission::UNPACED);
}

#[tokio::test]
async fn the_listener_is_supervised_while_release_maintenance_waits_for_the_writer() {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = Arc::new(crate::engine::open(&home).unwrap());
    let state = domain_bootstrap::DomainDaemon::over_shared(&home, domain.clone()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut server = std::pin::pin!(axum::serve(listener,
        domain_bootstrap::router(state).into_make_service_with_connect_info::<SocketAddr>()).into_future());
    let polled = AtomicBool::new(false);
    let serving = std::future::poll_fn(|context| {
        polled.store(true, Ordering::Relaxed);
        server.as_mut().poll(context)
    });
    let (finished, complete) = oneshot::channel();
    let (entered, entering) = std::sync::mpsc::channel();
    let (waiting, waits) = std::sync::mpsc::channel();
    domain.store.on_wait(move || { waiting.send(()).unwrap(); });
    let maintenance = domain.clone();
    let caller = std::thread::current().id();
    let pending = || tokio::spawn(std::future::pending::<Result<()>>());
    let workers = [pending(), pending(), pending(), pending()];
    let abort: Vec<_> = workers.iter().map(|worker| worker.abort_handle()).collect();
    let [outbox, scheduler, transport, github] = workers;
    let mut supervised = std::pin::pin!(supervise_with_maintenance(serving, move || {
        let off_runtime = std::thread::current().id() != caller;
        entered.send(off_runtime).unwrap();
        if off_runtime { maintenance.record_release("test", 0).unwrap(); }
        finished.send(off_runtime).unwrap();
    }, outbox, scheduler, transport, github, None, None, std::future::pending()));
    let served_while_locked = {
        let _writer = domain.store.lock().unwrap();
        assert!(supervised.as_mut().poll(&mut TaskContext::from_waker(Waker::noop())).is_pending());
        if entering.recv().unwrap() { waits.recv().unwrap(); }
        polled.load(Ordering::Relaxed)
    };
    let off_runtime = complete.await.unwrap();
    for worker in abort { worker.abort(); }
    assert!(off_runtime, "maintenance must not acquire the SQL writer on the runtime thread");
    assert!(served_while_locked, "waiting for release maintenance delayed polling the HTTP listener");
}

#[tokio::test]
async fn the_port_answers_while_hosts_are_still_being_taken_over() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = Arc::new(crate::engine::open(&home).unwrap());
    let state = domain_bootstrap::DomainDaemon::over_shared(&home, domain.clone()).unwrap();
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reserved.local_addr().unwrap().port();
    let mut config = Config::defaults();
    config.home = home.to_path_buf();
    config.listen = "127.0.0.1".parse().unwrap();
    config.port = port;
    let (_hold, held) = oneshot::channel::<()>();
    let recovering = domain.clone();
    let recovery = async move {
        let _ = held.await;
        PreparedWorkers(recovering)
    };
    let seams = assemble::Seams { listener: Some(reserved), ..assemble::Seams::over(offline_github()) };
    let daemon = tokio::spawn(async move { assemble::assemble_with(&config, state, domain, recovery, None, &seams).await.map(|_| ()) });
    let connecting = async {
        loop {
            match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                Ok(stream) => break stream,
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            }
        }
    };
    let mut stream = tokio::time::timeout(std::time::Duration::from_secs(60), connecting).await
        .expect("the port never opened while the hosts were being taken over");
    stream.write_all(b"GET /daemon HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await.unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).await.unwrap();
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(!daemon.is_finished());
    daemon.abort();
}

/// Every port has its implementation once the root wired the daemon: the engine's pieces, the
/// orchestrator the server adds, the screens only the root sees and the GitHub of its seams.
#[test]
fn the_daemon_implements_every_port() {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    drop(crate::engine::open(&home).unwrap());
    alondra_daemon_engine::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite(&alondra_domain_rules::Rules));
    let runtime = domain_runtime::DomainRuntime::open_unwired_at(&home, &alondra_engine_ports::journal::active_path(&home)).unwrap();
    crate::wiring::wire_engine(runtime.ports());
    crate::wire(runtime.ports());
    offline_github().wire(runtime.ports());
    let daemon = domain_bootstrap::DomainDaemon::over(&home, runtime).unwrap();
    assert_eq!(daemon.domain().ports().missing(), Vec::<&str>::new(), "a port nothing implements");
}

/// Each piece's ports run on a runtime that is not the kernel's: none is written against the
/// concrete runtime, so none reaches for it.
#[test]
fn the_ports_run_on_a_runtime_that_is_not_the_kernels() {
    use alondra_engine_ports::admission::{AdmissionEngine, AdmissionGate};
    use alondra_engine_ports::hosts::SdkHosts;
    use alondra_engine_ports::orchestrator::OrchestratorEngine;
    use alondra_engine_ports::sdk::SdkEngine;
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    drop(crate::engine::open(&home).unwrap());
    alondra_daemon_engine::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite(&alondra_domain_rules::Rules));
    let runtime = domain_runtime::DomainRuntime::open_unwired_at(&home, &alondra_engine_ports::journal::active_path(&home)).unwrap();
    crate::wiring::wire_engine(runtime.ports());
    crate::wire(runtime.ports());
    let daemon = domain_bootstrap::DomainDaemon::over(&home, runtime).unwrap();
    let domain = daemon.domain();
    let ports = domain.ports();
    let forwarded = alondra_engine_doubles::runtime::Forwarded(domain.clone());

    assert!(!ports.port::<dyn AdmissionGate>().unwrap().paused_by_user(&forwarded, "s"), "admission");
    assert!(!ports.port::<dyn SdkHosts>().unwrap().owns(&forwarded, &"a".into()), "sessions");
    let engine = ports.port::<dyn AdmissionEngine>().unwrap();
    assert!(!engine.is_launching(&forwarded, &"t".into()), "engine");
    assert!(!engine.usage_paused(&forwarded), "engine");
    assert!(!ports.port::<dyn SdkEngine>().unwrap().waits_for_session(&forwarded), "engine");
    assert!(ports.port::<dyn OrchestratorEngine>().unwrap().usage_pause(&forwarded).is_none(), "engine");
}

/// The server's wiring, the root's and its seams' GitHub, on an empty registry, leave no port to
/// nothing.
#[test]
fn the_daemons_wiring_alone_implements_every_port() {
    let registry = alondra_engine_ports::Registry::default();
    crate::wiring::wire_engine(&registry);
    crate::wire(&registry);
    offline_github().wire(&registry);
    assert_eq!(registry.missing(), Vec::<&str>::new(), "a port the daemon's wiring leaves to nothing");
}

#[path = "../../engine-sdk/src/engine_assembly.rs"]
mod engine_assembly;

/// The tests of the crates below the root open the engine with a copy of the root's observers:
/// every subsystem hears of each commit there as in the daemon, in the same order.
#[test]
fn the_engines_tests_hear_of_commits_as_the_daemon_does() {
    let names = |observers: Vec<Box<dyn domain_runtime::CommitObserver>>| observers.iter().map(|observer| observer.name()).collect::<Vec<_>>();
    assert_eq!(names(engine_assembly::observers()), names(crate::wiring::observers()));
}

/// `alondrad` started by hand (or a diagnostic) next to the live daemon opened the domain database
/// and turned the push the daemon was sending into an uncertain delivery; the daemon's own finish
/// then failed and stopped it.
#[test]
fn a_second_daemon_is_refused_before_its_restart_recovery_touches_the_live_daemons_deliveries() {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let (_writer, live) = open_as_writer(&home, crate::engine::open).unwrap();
    let epoch = live.state().unwrap().data_epoch.clone();
    live.execute_fixture(&alondra_domain_model::Actor::User { user_id: "u".into() }, alondra_domain_model::CommandEnvelope {
        protocol_version: alondra_domain_model::PROTOCOL_VERSION, data_epoch: epoch, command_id: "request".into(), expected_revision: 0,
        command: alondra_domain_model::Command::CreateRequest { request_id: "r".into(), body: "Fix the login".into(), auto_merge: false, maintenance: false, message: false },
    }).unwrap();
    let message = live.store.lock().unwrap().pending_messages().unwrap()[0].id;
    let claim = live.store.lock().unwrap().claim_message(message).unwrap().unwrap();
    let second = open_as_writer(&home, crate::engine::open);
    assert!(second.as_ref().is_err_and(|error| error.to_string().contains("another Alondra daemon")), "{:?}", second.err());
    assert!(live.store.lock().unwrap().finish_message(claim.attempt_id, claim.version, alondra_core::delivery::State::Handed, None).unwrap(),
        "the live daemon's delivery is still its own");
}

/// The seams' GitHub of a test: one that nothing reaches.
fn offline_github() -> assemble::GithubSeams {
    assemble::GithubSeams::of(Arc::new(alondra_engine_doubles::github::Offline))
}
