//! The daemon's root: it opens the engine, registers the screens, assembles the server and
//! supervises what runs until it has to stop, relaunch or hand over its hosts.
use alondra_daemon_server::*;
use alondra_store_sqlite::{database_check, domain_database_migration};
use alondra_engine_admission::AdmissionOf as _;
use alondra_engine_sdk::sdk_owner::SdkOwners as _;
use alondra_daemon_engine::live::LiveOf as _;
#[cfg(test)]
use alondra_daemon_engine::test_support;

pub mod assemble;
pub mod wiring;
pub use alondra_daemon_server::push_transport;
pub use alondra_daemon_server::service::scenario_phone;
#[cfg(test)]
mod startup_tests;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use alondra_core::config::Config;
use anyhow::{Context, Result, anyhow};

/// Registers what the root alone sees: the hosts of agents, through the real launcher unless the
/// daemon's seams already gave another, and the screens, which it hands to the server.
pub fn wire(registry: &alondra_engine_ports::Registry) {
    wire_machine(registry);
    wire_providers(registry);
    if registry.port::<dyn alondra_engine_ports::agent_host::AgentHost>().is_none() {
        alondra_agent_runtime::wire(registry, Default::default());
    }
    alondra_views::wire(registry);
    alondra_daemon_server::screens::install(registry);
}

/// Gives the registry the providers the engine runs agents on.
pub fn wire_providers(registry: &alondra_engine_ports::Registry) {
    alondra_provider_claude::wire(registry);
    alondra_provider_codex::wire(registry);
    alondra_provider_antigravity::wire(registry);
}

/// Gives the registry the Mac's git and disks: the server asks for them as it is put together, so
/// they are wired before it.
pub fn wire_machine(registry: &alondra_engine_ports::Registry) {
    alondra_system::wire(registry, alondra_daemon_server::watcher::claude_home);
}

/// Runs the daemon, with the edges `seams` gives once its domain is open.
pub fn main(seams: fn() -> assemble::Seams) -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--help" || flag == "-h" => {
            println!(
                "Alondra daemon\n\nUsage: alondrad [OPTIONS]\n\nOptions:\n  -h, --help                       Print help\n  -V, --version                    Print version\n      --preflight                  Check that this binary can start on the configured HOME, without changing it\n      --check-database <path>       Audit a stopped HOME database without changing it\n      --test-service-push <device-id>\n                                   Send an encrypted diagnostic push to one paired device\n\nWithout options, starts the local domain daemon and configured service connector."
            );
            return Ok(());
        }
        [flag] if flag == "--version" || flag == "-V" => {
            println!("alondrad {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        [flag, path] if flag == "--check-database" && !path.is_empty() && !path.starts_with('-') => {
            database_check::run(Path::new(path))?;
            println!("database check ok: {path}");
            return Ok(());
        }
        [flag] if flag == "--preflight" => {
            let checked = Config::load()
                .map_err(|e| anyhow!("config.toml unreadable: {e}"))
                .and_then(|config| preflight::run(&config, &alondra_store_sqlite::Sqlite, wiring::open, wire_machine));
            match checked {
                Ok(summary) => println!("preflight ok: {summary}"),
                Err(e) => {
                    eprintln!(
                        "preflight failed: {}",
                        format!("{e:#}").lines().collect::<Vec<_>>().join(" ")
                    );
                    std::process::exit(1);
                }
            }
            return Ok(());
        }
        _ => (),
    }
    let diagnostic = diagnostic_device(args)?;
    match open_files::raise() {
        Ok((before, after)) if after > before => log!("open files: soft limit raised from {before} to {after}"),
        Ok(_) => {}
        Err(why) => log!("open files: the soft limit could not be raised: {why}"),
    }
    let env = |k: &str| std::env::var(k).ok();
    let label = Config::load()
        .map(|c| c.service_label)
        .unwrap_or_else(|_| Config::defaults().service_label);
    let supervised = relaunch::supervised(&label, env);
    if supervised {
        relaunch::wait_until_free(&alondra_core::config::alondra_home(&env), std::time::Duration::from_millis(500));
    }
    start_guard::begin(alondra_core::config::alondra_home(&env), supervised);
    let error = match run(diagnostic, seams) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    if let Some((record, wait)) = start_guard::failed(&error) {
        log!(
            "start failed {} times in a row ({}); waiting {}s before exiting so launchd does not restart it every 10s",
            record.failures,
            record.reason,
            wait.as_secs()
        );
        let mut watcher = relaunch::watcher(&label);
        let home = alondra_core::config::alondra_home(&env);
        let mut cut_over = start_guard::became_true(|| {
            domain_cutover::read_marker(&home)
                .ok()
                .flatten()
                .is_some_and(|marker| marker.phase == domain_cutover::Phase::Committed)
        });
        if start_guard::wait(wait, relaunch::every().min(std::time::Duration::from_secs(2)), || {
            watcher.as_mut().is_some_and(|w| w.replaced()) || cut_over()
        }) {
            log!("the binary was replaced or HOME was cut over; exiting now so launchd starts it");
        }
    }
    Err(error)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn run(diagnostic: Option<String>, seams: fn() -> assemble::Seams) -> Result<()> {
    let config = Config::load().map_err(|e| anyhow!("config.toml unreadable: {e}"))?;
    if let Some(device) = diagnostic {
        return test_push(&config, &device).await;
    }
    log::rotate_when_full(&config.home);
    relaunch::record_supervision(&config.service_label);
    home_renames::apply(&config.home, domain_orchestrator::RECORDS, &alondra_store_sqlite::Sqlite)?;
    legacy_home::refuse_legacy_home(&config.home)?;
    let (database_migrated, _source_fence) =
        domain_database_migration::migrate_and_fence(&config.home)?;
    if database_migrated {
        log!(
            "activated verified domain database at {}",
            config
                .home
                .join(domain_database_migration::DATABASE)
                .display()
        );
    }
    if let Some(kept) = retire_empty_older_sidecar(&config.home)? {
        log!(
            "set aside an empty domain database from an older build; copy kept in {}",
            kept.display()
        );
    }
    let _home_lock = alondra_core::home_lock::HomeLock::shared(&config.home)?;
    alondra_core::home_lock::ensure_ready(&config.home)?;
    if relaunch::supervised(&config.service_label, |key| std::env::var(key).ok()) && let Some(daemon) = relaunch::launch_path() {
        relaunch::keep_launcher(&daemon, &config.home);
    }
    ensure_domain_protocol(&config.home)?;
    sdk_owner::backend::load(&config.home, config.sandbox, &options::user_home())?;
    let (_writer, domain) = open_as_writer(&config.home, |home| wiring::open(home, &alondra_store_sqlite::Sqlite))?;
    run_domain(&config, domain, seams()).await
}

/// The diagnostic push runs beside the live daemon: it reads the service's devices and sends,
/// and never opens the domain database, whose restart recovery belongs to the daemon alone.
async fn test_push(config: &Config, device: &str) -> Result<()> {
    let _home_lock = alondra_core::home_lock::HomeLock::shared(&config.home)?;
    alondra_core::home_lock::ensure_ready(&config.home)?;
    let auth = Arc::new(auth::Auth::open(&config.home)?);
    let service = service::Service::open(&config.home, config.port, auth)?
        .context("service is not configured; run alondra first")?;
    service.test_push(device).await?;
    println!(
        "Encrypted diagnostic push accepted by APNs for {device}. Delivery and decryption must be confirmed on the device."
    );
    Ok(())
}

/// Opens the domain database as its one writer: opening it marks what a dead daemon left in
/// flight (deliveries, scheduled launches, local actions) as uncertain, which over a live daemon's
/// work would undo it. A second daemon on the same HOME is refused before it opens anything.
fn open_as_writer<R>(home: &Path, open: impl FnOnce(&Path) -> Result<R>) -> Result<(alondra_core::home_lock::HomeLock, R)> {
    let writer = alondra_core::home_lock::HomeLock::daemon(home)?;
    Ok((writer, open(home)?))
}


/// Resolves once the launchd job's binary was replaced by one that passed preflight.
async fn replaced(watcher: Option<relaunch::Watcher>) {
    match watcher {
        Some(watcher) => relaunch::watch(watcher, relaunch::every()).await,
        None => std::future::pending().await,
    }
}

fn relaunch_now() -> ! {
    exit::now(relaunch::RELAUNCH, || log!(
        "exiting with {} so launchd starts the new binary",
        relaunch::RELAUNCH
    ))
}


/// Critical v2 workers must not disappear while the local listener remains healthy.
/// The loops that decide whether a node may start: the daemon does not serve without them, so the
/// first to end stops it and launchd starts it again whole.
async fn gates(
    machine_watch: tokio::task::JoinHandle<()>,
    gate: tokio::task::JoinHandle<()>,
) -> (&'static str, std::result::Result<(), tokio::task::JoinError>) {
    tokio::select! {
        ended = machine_watch => ("machine watch", ended),
        ended = gate => ("admission gate", ended),
    }
}

#[allow(clippy::too_many_arguments)]
async fn supervise_workers<S, G>(
    server: S,
    outbox: tokio::task::JoinHandle<Result<()>>,
    scheduler: tokio::task::JoinHandle<Result<()>>,
    transport: tokio::task::JoinHandle<Result<()>>,
    github: tokio::task::JoinHandle<Result<()>>,
    orchestrator: Option<tokio::task::JoinHandle<Result<()>>>,
    connector: Option<tokio::task::JoinHandle<()>>,
    gates: G,
) -> Result<()>
where
    S: std::future::Future<Output = std::io::Result<()>>,
    G: std::future::Future<Output = (&'static str, std::result::Result<(), tokio::task::JoinError>)>,
{
    tokio::select! {
        (gate, ended) = gates => {
            match ended {
                Ok(()) => anyhow::bail!("{gate} stopped unexpectedly"),
                Err(error) => anyhow::bail!("{gate} panicked: {error}"),
            }
        },
        result = github => {
            match result {
                Ok(Ok(())) => anyhow::bail!("GitHub observer stopped unexpectedly"),
                Ok(Err(error)) => anyhow::bail!("GitHub observer failed: {error:#}"),
                Err(error) => anyhow::bail!("GitHub observer panicked: {error}"),
            }
        },
        result = transport => {
            match result {
                Ok(Ok(())) => anyhow::bail!("producer transport stopped unexpectedly"),
                Ok(Err(error)) => anyhow::bail!("producer transport failed: {error:#}"),
                Err(error) => anyhow::bail!("producer transport panicked: {error}"),
            }
        },
        result = server => {
            result?;
            anyhow::bail!("domain listener stopped unexpectedly")
        },
        result = outbox => {
            match result {
                Ok(Ok(())) => anyhow::bail!("outbox worker stopped unexpectedly"),
                Ok(Err(error)) => anyhow::bail!("outbox worker failed: {error:#}"),
                Err(error) => anyhow::bail!("outbox worker panicked: {error}"),
            }
        },
        result = scheduler => {
            match result {
                Ok(Ok(())) => anyhow::bail!("Schedule worker stopped unexpectedly"),
                Ok(Err(error)) => anyhow::bail!("Schedule worker failed: {error:#}"),
                Err(error) => anyhow::bail!("Schedule worker panicked: {error}"),
            }
        },
        result = async {
            match orchestrator {
                Some(task) => Some(task.await),
                None => std::future::pending().await,
            }
        } => {
            match result.expect("orchestrator future has task") {
                Ok(Ok(())) => anyhow::bail!("orchestrator executor stopped unexpectedly"),
                Ok(Err(error)) => anyhow::bail!("orchestrator executor failed: {error:#}"),
                Err(error) => anyhow::bail!("orchestrator executor panicked: {error}"),
            }
        },
        result = async {
            match connector {
                Some(task) => Some(task.await),
                None => std::future::pending().await,
            }
        } => {
            match result.expect("connector future has task") {
                Ok(()) => anyhow::bail!("encrypted service connector stopped unexpectedly"),
                Err(error) => anyhow::bail!("encrypted service connector failed: {error}"),
            }
        },
    }
}






/// A committed HOME has one SQL authority shared by local and encrypted adapters.
async fn run_domain(config: &Config, domain: domain_runtime::DomainRuntime, seams: assemble::Seams) -> Result<()> {
    // Taken before the port opens: whoever sees this daemon answer may replace its binary at once.
    let upgrades = relaunch::watcher(&config.service_label);
    let signals = Signals::listen();
    wire_machine(domain.ports());
    let state = domain_bootstrap::DomainDaemon::over(&config.home, domain)?;
    if config.fast_timing {
        use domain_api::DomainHost as _;
        state.delivery_reviews().limit(delivery_review::Limits::SANDBOX);
    }
    let assembled = assemble::assemble(config, state, seams).await?;
    supervise(assembled, upgrades, signals).await
}

/// Only a recovered domain may activate the workers that claim deliveries, launch or resume nodes.
struct PreparedWorkers(Arc<domain_runtime::DomainRuntime>);

async fn prepare_workers(
    config: &Config,
    domain: Arc<domain_runtime::DomainRuntime>,
    backend: Option<sdk_owner::backend::SdkBackend>,
) -> PreparedWorkers {
    accounts::begin_initial_check(&*domain);
    domain.live().sandbox.store(config.sandbox, std::sync::atomic::Ordering::Relaxed);
    if let Some(backend) = backend {
        domain.enable_sdk(backend.clone());
        let adopting: std::sync::Arc<dyn alondra_engine_ports::Runtime> = domain.clone();
        alondra_core::clock::spawn_blocking(move || {
            accounts::check_if_crossed(&*adopting);
            sdk_owner::adopt_all(&adopting, &backend);
        }).await.ok();
    }
    if config.fast_timing {
        domain.admission().tune(admission::Tuning { spacing: 1_000, limit: Some(admission::Limit::Fixed(admission::UNPACED)) });
        domain.admission().ignore_mac();
        domain.machine_watch().ignore_peaks();
    }
    if config.ignore_mac_load {
        domain.admission().ignore_mac();
    }
    PreparedWorkers(domain)
}

/// Watches the assembled daemon until it has to stop, relaunch or hand over its hosts.
async fn supervise(assembled: assemble::Assembled, upgrades: Option<relaunch::Watcher>, signals: Signals) -> Result<()> {
    let Signals { terminate, interrupt, handover } = signals;
    let assemble::Assembled { domain: owners, server, outbox, scheduler, transport, github, orchestrator, connector, machine_watch, gate, .. } = assembled;
    start_guard::served();
    let maintenance = owners.clone();
    let stopped = tokio::select! {
        stopped = supervise_with_maintenance(server, move || {
            if let Err(error) = maintenance.record_release(env!("CARGO_PKG_VERSION"), alondra_core::now_millis()) {
                log!("could not record this release's first start: {error:#}");
            }
        }, outbox, scheduler, transport, github, orchestrator, connector, gates(machine_watch, gate)) => stopped,
        () = replaced(upgrades) => {
            domain_orchestrator::note_shutdown(&owners, owners.home());
            relaunch_now()
        },
        () = handed_over(handover) => {
            domain_orchestrator::note_shutdown(&owners, owners.home());
            exit::now(relaunch::RELAUNCH, || log!("leaving the SDK hosts to the daemon launchd started"))
        },
        signal = terminated(terminate, interrupt) => {
            log!("{signal} received: stopping the SDK hosts, then exiting");
            domain_orchestrator::note_shutdown(&owners, owners.home());
            stop_sdk_owners(owners).await;
            exit::now(0, || ())
        }
    };
    stopped
}

/// Maintenance may wait for the SQL writer; the supervised listener must keep being polled.
#[allow(clippy::too_many_arguments)]
async fn supervise_with_maintenance<S, G>(
    server: S,
    maintenance: impl FnOnce() + Send + 'static,
    outbox: tokio::task::JoinHandle<Result<()>>,
    scheduler: tokio::task::JoinHandle<Result<()>>,
    transport: tokio::task::JoinHandle<Result<()>>,
    github: tokio::task::JoinHandle<Result<()>>,
    orchestrator: Option<tokio::task::JoinHandle<Result<()>>>,
    connector: Option<tokio::task::JoinHandle<()>>,
    gates: G,
) -> Result<()>
where
    S: std::future::Future<Output = std::io::Result<()>>,
    G: std::future::Future<Output = (&'static str, std::result::Result<(), tokio::task::JoinError>)>,
{
    alondra_core::clock::spawn_blocking(maintenance);
    supervise_workers(server, outbox, scheduler, transport, github, orchestrator, connector, gates).await
}

/// The signals the daemon answers to, listened for before its port opens: a signal sent to a daemon
/// that already answers must never meet the default action, which ends the process by that signal
/// instead of the exit it owes its successor.
struct Signals {
    terminate: Option<tokio::signal::unix::Signal>,
    interrupt: Option<tokio::signal::unix::Signal>,
    handover: Option<tokio::signal::unix::Signal>,
}

impl Signals {
    fn listen() -> Self {
        use tokio::signal::unix::{SignalKind, signal};
        let on = |kind| signal(kind).ok();
        Self {
            terminate: on(SignalKind::terminate()),
            interrupt: on(SignalKind::interrupt()),
            handover: on(SignalKind::user_defined1()),
        }
    }
}

/// Resolves on the first SIGTERM or SIGINT, naming it.
async fn terminated(term: Option<tokio::signal::unix::Signal>, interrupt: Option<tokio::signal::unix::Signal>) -> &'static str {
    let (Some(mut term), Some(mut interrupt)) = (term, interrupt) else {
        log!("could not listen for SIGTERM and SIGINT; the daemon stops only when killed");
        return std::future::pending().await;
    };
    tokio::select! {
        _ = term.recv() => "SIGTERM",
        _ = interrupt.recv() => "SIGINT",
    }
}

/// `alondra doctor --fix` sends SIGUSR1 to a daemon started by hand once launchd holds its job.
async fn handed_over(handover: Option<tokio::signal::unix::Signal>) {
    match handover {
        Some(mut handover) => { handover.recv().await; }
        None => std::future::pending().await,
    }
}

/// A daemon that is stopped stops its SDK hosts and waits for each to exit. One that relaunches
/// into a new binary, or exits on an error, leaves them running for the daemon that comes back to
/// adopt.
async fn stop_sdk_owners(domain: Arc<domain_runtime::DomainRuntime>) {
    if let Err(error) = alondra_core::clock::spawn_blocking_unwatched(move || domain.stop_sdk_owners()).await {
        log!("stopping the SDK hosts failed: {error}");
    }
}

#[cfg(test)]
mod worker_supervision_tests {
    use super::*;

    #[tokio::test]
    async fn a_handover_signal_sent_before_anyone_waits_for_it_is_not_lost() {
        let signals = Signals::listen();
        assert_eq!(unsafe { libc::raise(libc::SIGUSR1) }, 0);
        handed_over(signals.handover).await;
    }

    #[tokio::test]
    async fn the_daemon_stops_when_the_machine_watch_or_the_gate_ends() {
        let pending = || tokio::spawn(std::future::pending::<Result<()>>());
        let supervise = |gates| supervise_workers(std::future::pending::<std::io::Result<()>>(), pending(), pending(), pending(), pending(), None, None, gates);
        let watch_panicked = supervise(gates(tokio::spawn(async { panic!("machine watch poisoned") }), tokio::spawn(std::future::pending()))).await.unwrap_err();
        assert!(watch_panicked.to_string().contains("machine watch panicked"), "{watch_panicked}");
        let gate_ended = supervise(gates(tokio::spawn(std::future::pending()), tokio::spawn(async {}))).await.unwrap_err();
        assert!(gate_ended.to_string().contains("admission gate stopped unexpectedly"), "{gate_ended}");
    }

    #[tokio::test]
    async fn unexpected_connector_or_outbox_exit_stops_listener() {
        let server = || std::future::pending::<std::io::Result<()>>();
        let pending = || tokio::spawn(std::future::pending::<Result<()>>());
        let exit = supervise_workers(
            server(),
            pending(),
            pending(),
            pending(),
            pending(),
            None,
            Some(tokio::spawn(async {})),
            std::future::pending(),
        )
        .await
        .unwrap_err();
        assert!(exit.to_string().contains("connector stopped unexpectedly"));
        let panic = supervise_workers(
            server(),
            pending(),
            pending(),
            pending(),
            pending(),
            None,
            Some(tokio::spawn(async { panic!("synthetic failure") })),
            std::future::pending(),
        )
        .await
        .unwrap_err();
        assert!(panic.to_string().contains("connector failed"));
        let stopped_outbox = supervise_workers(
            server(),
            tokio::spawn(async { Ok(()) }),
            pending(),
            pending(),
            pending(),
            None,
            None,
            std::future::pending(),
        )
        .await
        .unwrap_err();
        assert!(
            stopped_outbox
                .to_string()
                .contains("outbox worker stopped unexpectedly")
        );
        let failed_outbox = supervise_workers(
            server(),
            tokio::spawn(async { anyhow::bail!("synthetic SQL failure") }),
            pending(),
            pending(),
            pending(),
            None,
            None,
            std::future::pending(),
        )
        .await
        .unwrap_err();
        assert!(failed_outbox.to_string().contains("synthetic SQL failure"));
        let failed_scheduler = supervise_workers(
            server(),
            pending(),
            tokio::spawn(async { anyhow::bail!("synthetic Schedule failure") }),
            pending(),
            pending(),
            None,
            None,
            std::future::pending(),
        )
        .await
        .unwrap_err();
        assert!(
            failed_scheduler
                .to_string()
                .contains("synthetic Schedule failure")
        );
        let failed_transport = supervise_workers(
            server(),
            pending(),
            pending(),
            tokio::spawn(async { anyhow::bail!("synthetic hook failure") }),
            pending(),
            None,
            None,
            std::future::pending(),
        )
        .await
        .unwrap_err();
        assert!(
            failed_transport
                .to_string()
                .contains("synthetic hook failure")
        );
        let failed_github = supervise_workers(
            server(),
            pending(),
            pending(),
            pending(),
            tokio::spawn(async { anyhow::bail!("synthetic GitHub failure") }),
            None,
            None,
            std::future::pending(),
        )
        .await
        .unwrap_err();
        assert!(
            failed_github
                .to_string()
                .contains("synthetic GitHub failure")
        );
    }
}

fn diagnostic_device(args: Vec<String>) -> Result<Option<String>> {
    match args.as_slice() {
        [] => Ok(None),
        [flag, device]
            if flag == "--test-service-push" && !device.is_empty() && !device.starts_with('-') =>
        {
            Ok(Some(device.clone()))
        }
        _ => {
            anyhow::bail!("usage: alondrad [--help | --version | --preflight | --check-database <path> | --test-service-push <device-id>]")
        }
    }
}

#[cfg(test)]
mod argument_tests {
    use super::*;

    #[test]
    fn diagnostic_requires_exact_explicit_target() {
        assert_eq!(diagnostic_device(vec![]).unwrap(), None);
        assert_eq!(
            diagnostic_device(vec!["--test-service-push".into(), "phone-1".into()]).unwrap(),
            Some("phone-1".into())
        );
        for args in [
            vec!["--test-service-push"],
            vec!["--test-service-push", ""],
            vec!["--test-service-push", "--all"],
            vec!["--test-service-push", "phone-1", "extra"],
            vec!["--unknown"],
        ] {
            assert!(diagnostic_device(args.into_iter().map(String::from).collect()).is_err());
        }
    }

    #[test]
    fn empty_new_home_selects_domain_startup() {
        let scratch_directory = crate::test_support::Directory::new();
        let home =
            scratch_directory.parent().unwrap().join(format!("alondra-domain-start-{}", alondra_foundation::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        legacy_home::refuse_legacy_home(&home).unwrap();
        ensure_domain_protocol(&home).unwrap();
        let _domain = crate::engine::open(&home).unwrap();
        domain_bootstrap::DomainDaemon::open(&home, &alondra_store_sqlite::Sqlite).unwrap();
        assert!(home.join("domain.db").is_file());
        assert!(!home.join("alondra.db").exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn legacy_only_home_fails_closed_without_creating_domain_database() {
        let scratch_directory = crate::test_support::Directory::new();
        let home =
            scratch_directory.parent().unwrap().join(format!("alondra-domain-legacy-{}", alondra_foundation::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("alondra.db"), b"preserve legacy bytes").unwrap();
        let error = legacy_home::refuse_legacy_home(&home).unwrap_err();
        assert!(error.to_string().contains("legacy data"));
        assert!(!home.join("domain.db").exists());
        assert_eq!(
            std::fs::read(home.join("alondra.db")).unwrap(),
            b"preserve legacy bytes"
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn markerless_domain_rows_and_prepared_cutover_refuse_startup() {
        let scratch_directory = crate::test_support::Directory::new();
        use std::os::unix::fs::PermissionsExt;
        let home =
            scratch_directory.parent().unwrap().join(format!("alondra-domain-guard-{}", alondra_foundation::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        let legacy_domain = home.join("domain-v2.db");
        drop(alondra_store_sqlite::DomainStore::open(&legacy_domain).unwrap());
        let conn = rusqlite::Connection::open(&legacy_domain).unwrap();
        conn.execute("UPDATE domain_meta SET revision=1 WHERE singleton=1", [])
            .unwrap();
        drop(conn);
        assert!(ensure_domain_protocol(&home).is_err());
        std::fs::remove_file(&legacy_domain).unwrap();

        let marker = domain_cutover::Marker {
            format: 1,
            phase: domain_cutover::Phase::Prepared,
            backup_manifest_sha256: "a".repeat(64),
            source_seq: 42,
            data_epoch: "b".repeat(64),
        };
        let marker_path = home.join(domain_cutover::MARKER);
        std::fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
        std::fs::set_permissions(&marker_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            ensure_domain_protocol(&home)
                .unwrap_err()
                .to_string()
                .contains("prepared")
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[cfg(test)]
mod engine {
    /// Opens every runtime the engine's fixtures open on this thread as the daemon does, with the gate
    /// of a test.
    pub fn wired() {
        alondra_foundation::durability::relax();
        alondra_daemon_engine::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite);
        alondra_daemon_engine::wiring::open_with(open_at);
    }

    /// The runtime as the daemon opens it, with the doubles of the Mac and of GitHub and a gate that is unpaced and
    /// launches on the disk the machine watch measured.
    pub fn open_at(home: &std::path::Path, database: &std::path::Path, files: &dyn alondra_engine_ports::journal::JournalFiles) -> anyhow::Result<alondra_engine_sdk::domain_runtime::DomainRuntime> {
        let runtime = crate::wiring::open_at(home, database, files)?;
        alondra_engine_doubles::machine::wire(runtime.ports(), alondra_engine_ports::watcher::claude_home);
        alondra_engine_doubles::github::wire(runtime.ports());
        alondra_agent_runtime::wire(runtime.ports(), Default::default());
        alondra_engine_admission::for_tests(&runtime);
        Ok(runtime)
    }

    /// `open_at` at the home's active database; the engine's fixtures this thread opens open the same.
    pub fn open(home: &std::path::Path) -> anyhow::Result<alondra_engine_sdk::domain_runtime::DomainRuntime> {
        wired();
        open_at(home, &alondra_engine_ports::journal::active_path(home), &alondra_store_sqlite::Sqlite)
    }
}
