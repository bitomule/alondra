use super::*;
use std::{future::Future, sync::{Mutex, atomic::{AtomicBool, Ordering}}, task::{Context as TaskContext, Waker}};
use tokio::sync::oneshot;

struct Listing(Mutex<Option<oneshot::Receiver<String>>>);
impl node_close::Sessions for Listing {
    fn claude<'a>(&'a self, _: Vec<String>) -> node_close::Answer<'a, std::result::Result<node_close::Ran, String>> {
        let listing = self.0.lock().unwrap().take().unwrap();
        Box::pin(async move { Ok(node_close::Ran { success: true, stdout: listing.await.unwrap(), stderr: String::new() }) })
    }
    fn leftovers<'a>(&'a self, _: &'a str) -> node_close::Answer<'a, Option<String>> {
        panic!("initial reconciliation must not close sessions")
    }
}

#[tokio::test]
async fn workers_wait_for_recovery_and_receive_the_configured_admission_gate() {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = Arc::new(node_reconcile::tests::open(&home));
    node_reconcile::tests::launch(&domain, "vanished");
    domain.live.gate.tune(admission::Tuning { spacing: 0, limit: Some(admission::Limit::Fixed(1)) });
    let mut config = Config::defaults();
    config.home = home.to_path_buf();
    config.fast_timing = true;
    let (send, receive) = oneshot::channel();
    let listing = Listing(Mutex::new(Some(receive)));
    let mut preparing = std::pin::pin!(prepare_workers(&config, domain.clone(), None, &listing));
    assert!(preparing.as_mut().poll(&mut TaskContext::from_waker(Waker::noop())).is_pending(),
        "producers must not receive a prepared domain while recovery is pending");
    assert_eq!(domain.state().unwrap().slots().len(), 1);
    assert!(accounts::require_initial_check(&domain).is_err(), "an account check cannot admit a node during recovery");
    send.send("[]".into()).unwrap();
    let PreparedWorkers(ready) = preparing.await;
    assert!(ready.state().unwrap().slots().is_empty());
    assert_eq!(ready.state().unwrap().agents[&"vanished".into()].phase, alondra_core::domain::NodePhase::Lost);
    assert_eq!(ready.live.gate.snapshot(&ready, 0).unwrap().limit, admission::UNPACED);
}

#[tokio::test]
async fn the_listener_is_supervised_while_release_maintenance_waits_for_the_writer() {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = Arc::new(domain_runtime::DomainRuntime::open(&home).unwrap());
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
    let domain = Arc::new(domain_runtime::DomainRuntime::open(&home).unwrap());
    let state = domain_bootstrap::DomainDaemon::over_shared(&home, domain.clone()).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
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
    let daemon = tokio::spawn(async move { serve_domain(&config, state, domain, recovery, None).await });
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
