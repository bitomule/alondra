#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

#[macro_use]
mod log;
mod allow_always;
mod approval_lease;
mod blocking_work;
mod runtime_mutex;
mod operation_journal;
mod api_failures;
mod auth;
mod classify;
mod claude_access;
mod cleanup;
mod claude_trust;
mod direct;
mod disk;
mod domain_agent_lifecycle;
mod domain_api;
mod domain_authority;
mod domain_bootstrap;
mod domain_content;
mod domain_cutover;
mod domain_database_migration;
mod domain_delivery;
#[cfg(test)]
mod domain_e2e_tests;
mod domain_github;
mod domain_merges;
mod domain_holds;
mod domain_messages;
mod domain_orchestrator;
mod orchestrator_inbox;
mod domain_orchestrator_reads;
mod domain_orchestrator_tools;
mod domain_runtime;
mod domain_schedule_api;
mod domain_schedule_runner;
mod domain_sessions;
mod domain_store;
mod domain_transport;
mod hook_socket;
mod home_renames;
mod hook_text;
mod iroh_endpoint;
mod iroh_terminals;
mod issues;
mod job_state;
mod local_network;
mod local_network_card;
mod reachability;
mod machine;
mod mcp_sign_in;
mod machine_state;
mod machine_watch;
mod fleet_gate;
mod fleet_pause;
mod claude_check;
mod auto_launch;
mod launch_plans;
mod plan_reviews;
mod start_failures;
mod launch_worktree;
mod node_close;
mod node_reconcile;
mod revoked_devices;
mod device_expiry;
mod device_usage;
mod evidence_retention;
mod node_push;
mod node_reports;
mod node_work;
mod task_closer;
mod task_worktrees;
mod workflow_state;
mod node_revival;
mod notices;
mod options;
mod orchestrator_process;
mod orchestrator_sdk;
mod plugins;
mod preflight;
mod process_watch;
mod prs;
mod push;
mod pr_heads;
mod pr_ids;
mod result_files;
mod push_gate;
mod push_retract;
mod relaunch;
mod sdk_handover;
mod sdk_owner;
#[cfg(target_os = "macos")]
mod schedule_clock;
mod schedules;
mod service;
mod settings;
mod service_files;
mod start_guard;
mod updates;
mod usage_pause;
mod uploads;
mod client_routes;
mod views;
mod watcher;
mod worktree;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use alondra_core::config::Config;
use anyhow::{Context, Result, anyhow};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--help" || flag == "-h" => {
            println!(
                "Alondra daemon\n\nUsage: alondrad [OPTIONS]\n\nOptions:\n  -h, --help                       Print help\n  -V, --version                    Print version\n      --preflight                  Check that this binary can start on the configured HOME, without changing it\n      --test-service-push <device-id>\n                                   Send an encrypted diagnostic push to one paired device\n\nWithout options, starts the local domain daemon and configured service connector."
            );
            return Ok(());
        }
        [flag] if flag == "--version" || flag == "-V" => {
            println!("alondrad {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        [flag] if flag == "--preflight" => {
            let checked = Config::load()
                .map_err(|e| anyhow!("config.toml unreadable: {e}"))
                .and_then(|config| preflight::run(&config));
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
    let env = |k: &str| std::env::var(k).ok();
    let label = Config::load()
        .map(|c| c.service_label)
        .unwrap_or_else(|_| Config::defaults().service_label);
    start_guard::begin(
        alondra_core::config::alondra_home(&env),
        relaunch::supervised(&label, env),
    );
    let error = match run(diagnostic) {
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
        let watcher = relaunch::watcher(&label);
        let home = alondra_core::config::alondra_home(&env);
        let mut cut_over = start_guard::became_true(|| {
            domain_cutover::read_marker(&home)
                .ok()
                .flatten()
                .is_some_and(|marker| marker.phase == domain_cutover::Phase::Committed)
        });
        if start_guard::wait(wait, std::time::Duration::from_secs(2), || {
            watcher.as_ref().is_some_and(|w| w.replaced()) || cut_over()
        }) {
            log!("the binary was replaced or HOME was cut over; exiting now so launchd starts it");
        }
    }
    Err(error)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn run(diagnostic: Option<String>) -> Result<()> {
    let config = Config::load().map_err(|e| anyhow!("config.toml unreadable: {e}"))?;
    home_renames::apply(&config.home)?;
    domain_cutover::refuse_legacy_home(&config.home)?;
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
    let domain = domain_runtime::DomainRuntime::open(&config.home)?;
    if let Some(device) = diagnostic {
        // Diagnostic delivery uses the same canonical domain store but does not
        // start the long-running workers or HTTP listener.
        let auth = Arc::new(auth::Auth::open(&config.home)?);
        let service = service::Service::open(&config.home, config.port, auth)?
            .context("service is not configured; run alondra first")?;
        service.test_push(&device).await?;
        println!(
            "Encrypted diagnostic push accepted by APNs for {device}. Delivery and decryption must be confirmed on the device."
        );
        return Ok(());
    }
    run_domain(&config, domain).await
}

fn ensure_domain_protocol(home: &std::path::Path) -> Result<()> {
    match domain_cutover::read_marker(home)? {
        Some(marker) if marker.phase == domain_cutover::Phase::Prepared => {
            anyhow::bail!("domain cutover is prepared but not committed")
        }
        Some(_) => Ok(()),
        None => {
            anyhow::ensure!(
                legacy_sidecar_is_empty(&home.join("domain-v2.db"))?,
                "domain database contains state without a committed cutover marker; refusing to start and preserving the database"
            );
            Ok(())
        }
    }
}

/// Resolves once the launchd job's binary was replaced by one that passed preflight.
async fn replaced(config: &Config) {
    match relaunch::watcher(&config.service_label) {
        Some(watcher) => relaunch::watch(watcher, relaunch::every()).await,
        None => std::future::pending().await,
    }
}

fn relaunch_now() -> ! {
    log!(
        "exiting with {} so launchd starts the new binary",
        relaunch::RELAUNCH
    );
    std::process::exit(relaunch::RELAUNCH)
}

/// What approving a direct pairing request does on this Mac: pin the device's keys in
/// `service.toml`, approve it in the registry, and make the running service accept it.
fn direct_approval(
    home: std::path::PathBuf,
    auth: Arc<auth::Auth>,
    service: Arc<service::Service>,
) -> Box<dyn Fn(&direct::pairing::ApprovedDevice) -> std::result::Result<(), String> + Send + Sync> {
    use alondra_core::service_config::{NewDevice, ServiceConfigLock, add_device};
    Box::new(move |device| {
        let lock = ServiceConfigLock::exclusive(&home).map_err(|error| error.to_string())?;
        add_device(
            &home,
            &lock,
            &NewDevice {
                device_id: &device.device_id,
                signing_key: &device.signing_key,
                agreement_key: &device.agreement_key,
            },
        )
        .map_err(|error| error.to_string())?;
        direct::remember_direct(&home, &device.device_id).map_err(|error| error.to_string())?;
        drop(lock);
        auth.approve_service(&device.device_id, &device.name, &device.signing_key)?;
        service.reload_devices().map_err(|error| format!("{error:#}"))
    })
}

/// Critical v2 workers must not disappear while the local listener remains healthy.
async fn supervise_workers<S>(
    server: S,
    outbox: tokio::task::JoinHandle<Result<()>>,
    scheduler: tokio::task::JoinHandle<Result<()>>,
    transport: tokio::task::JoinHandle<Result<()>>,
    github: tokio::task::JoinHandle<Result<()>>,
    orchestrator: Option<tokio::task::JoinHandle<Result<()>>>,
    connector: Option<tokio::task::JoinHandle<()>>,
) -> Result<()>
where
    S: std::future::Future<Output = std::io::Result<()>>,
{
    tokio::select! {
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

/// Before cutover, the legacy Hub creates an empty v2 sidecar for its API.
/// A markerless database may be reused by legacy only while it contains no
/// commands, entities, receipts, deliveries, or migration metadata.
pub(crate) fn legacy_sidecar_is_empty(path: &Path) -> Result<bool> {
    empty_domain_sidecar(path, false)
}

/// A pre-release build left an empty domain database on an older schema. Legacy
/// startup refuses unknown schemas, so under the exclusive HOME lock it is copied
/// aside and removed; the legacy Hub then creates the current empty one. Anything
/// holding rows, or not recognisable as ours, is left for `ensure_domain_protocol` to refuse.
fn retire_empty_older_sidecar(home: &Path) -> Result<Option<std::path::PathBuf>> {
    let database = home.join("domain-v2.db");
    if !database.exists() || domain_cutover::read_marker(home)?.is_some() {
        return Ok(None);
    }
    let Ok(_lock) = alondra_core::home_lock::HomeLock::recovery(home) else {
        return Ok(None);
    };
    if legacy_sidecar_is_empty(&database).unwrap_or(false)
        || !replaceable_cutover_sidecar_is_empty(&database).unwrap_or(false)
    {
        return Ok(None);
    }
    let kept = home.join(format!("domain-retired-{}", unix_seconds()));
    std::fs::create_dir(&kept)?;
    let files = ["domain-v2.db", "domain-v2.db-wal", "domain-v2.db-shm"];
    for name in files {
        let source = home.join(name);
        if source.exists() {
            std::fs::copy(&source, kept.join(name))?;
        }
    }
    anyhow::ensure!(
        replaceable_cutover_sidecar_is_empty(&kept.join("domain-v2.db"))?,
        "copy of the old domain database does not verify; leaving it in place"
    );
    for name in files {
        match std::fs::remove_file(home.join(name)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => (),
        }
    }
    Ok(Some(kept))
}

pub(crate) fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Cutover may replace a markerless experimental sidecar only after proving it
/// contains no business, receipt, delivery, or migration rows. The final store
/// is installed from the verified legacy backup, not upgraded in place.
pub(crate) fn replaceable_cutover_sidecar_is_empty(path: &Path) -> Result<bool> {
    empty_domain_sidecar(path, true)
}

fn empty_domain_sidecar(path: &Path, allow_older_empty_schema: bool) -> Result<bool> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        metadata.is_file(),
        "markerless domain database must be a regular file"
    );
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let application: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    anyhow::ensure!(
        application == 0x414c4432
            && (version == domain_store::SCHEMA_VERSION || allow_older_empty_schema),
        "markerless domain database has unknown schema"
    );
    let (revision, generation): (i64, i64) = conn.query_row(
        "SELECT revision,orchestrator_generation FROM domain_meta WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if revision != 0 || generation != 0 {
        return Ok(false);
    }
    let mut tables = conn.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name != 'domain_meta'")?;
    let names = tables.query_map([], |row| row.get::<_, String>(0))?;
    for name in names {
        let escaped = name?.replace('"', "\"\"");
        let statement = format!("SELECT EXISTS(SELECT 1 FROM \"{escaped}\" LIMIT 1)");
        let has_row: bool = conn.query_row(&statement, [], |row| row.get(0))?;
        if has_row {
            return Ok(false);
        }
    }
    Ok(true)
}

/// A committed HOME has one SQL authority shared by local and encrypted adapters.
async fn run_domain(config: &Config, domain: domain_runtime::DomainRuntime) -> Result<()> {
    let state = domain_bootstrap::DomainDaemon::over(&config.home, domain)?;
    let auth = state.auth();
    let service = service::Service::open(&config.home, config.port, auth.clone())?.map(|service| {
        Arc::new(
            service
                .with_domain(state.domain())
                .with_client(state.client()),
        )
    });
    if let Some(service) = &service {
        service.recover_interrupted()?;
        state.attach_service(service.clone());
    }
    let actions = views::action_results::recover_interrupted(&config.home)?;
    if actions > 0 {
        log!("the previous daemon stopped in the middle of {actions} local action(s); they are marked interrupted");
    }
    let domain = state.domain();
    domain.live.sandbox.store(config.sandbox, std::sync::atomic::Ordering::Relaxed);
    // Before any SDK execution is taken over: a host that replaces one that ended loads this plugin.
    match plugins::node_source(&watcher::claude_home()).zip(plugins::own_cli()) {
        Some((source, cli)) => match plugins::install_node(&config.home, &source, &cli) {
            Ok(dir) => log!("node plugin {} brought up to date from {}", dir.display(), source.display()),
            Err(error) => log!("could not bring the node plugin up to date from {}: {error:#}", source.display()),
        },
        None => log!("Alondra's node plugin was not found: run `alondra doctor --fix`"),
    }
    let environment: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let daemon = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("alondrad"));
    match sdk_owner::backend::resolve(&config.home, config.sandbox, &options::user_home(), &daemon, &environment) {
        Ok(backend) => {
            log!("the Agent SDK host is {} (configuration {})", backend.host.display(), backend.configuration_digest());
            domain.enable_sdk(backend.clone());
            // Adopt the live host of every task a previous daemon left running, so a restart keeps
            // the same Claude and its session instead of starting over.
            let adopting = domain.clone();
            tokio::task::spawn_blocking(move || sdk_owner::adopt_all(&adopting, &backend)).await.ok();
        }
        Err(error) => log!("no task or orchestrator batch starts: {error:#}"),
    }
    let state_domain = domain.clone();
    let gauges = state.gauges();
    let client = state.client();
    let push = Arc::new(push::Push::open(&config.home)?);
    let addr = SocketAddr::new(config.listen, config.port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("cannot listen on {addr}"))?;
    // The producer transport owns the per-home hook socket before any outbox
    // message can be claimed for a session.
    let hooks = hook_socket::bind(&config.home).await?;
    if let Some(service) = service.clone().filter(|service| service.direct_config().enabled) {
        let direct = Arc::new(direct::Direct::new(
            service.clone(),
            Arc::new(direct::pairing::PairingBook::new(&config.home)),
            config.name.clone(),
            direct_approval(config.home.clone(), auth.clone(), service),
        ));
        state.attach_direct(direct.clone());
        tokio::spawn(direct::run(direct, config.sandbox));
    }
    if let Some(service) = service.clone().filter(|service| service.iroh_config().enabled) {
        tokio::spawn(iroh_endpoint::run(service));
        tokio::spawn(local_network::watch());
        tokio::spawn(local_network_card::run(domain.clone()));
    }
    tokio::spawn(device_expiry::run(state.clone(), config.home.clone(), config.device_expiry_days));
    let app = domain_bootstrap::router(state)
        .layer(axum::middleware::from_fn_with_state(auth, auth::guard))
        .layer(axum::middleware::from_fn(views::stamp_version));
    log!(
        "domain listener on http://{addr}, data in {}",
        config.home.display()
    );
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .into_future();
    tokio::spawn(usage_pause::run(
        domain.clone(),
        usage_pause::PushAlerts { domain: domain.clone(), push: push.clone(), service: service.clone(), client: client.clone() },
    ));
    tokio::spawn(claude_access::run(domain.clone()));
    tokio::spawn(machine_watch::run(
        domain.clone(),
        gauges.clone(),
        usage_pause::PushAlerts { domain: domain.clone(), push: push.clone(), service: service.clone(), client: client.clone() },
    ));
    let outbox = tokio::spawn(domain_delivery::run(
        domain.clone(),
        config.home.clone(),
        push,
        service.clone(),
        client.clone(),
    ));
    if let Some(service) = &service {
        tokio::spawn(push_retract::run(domain.clone(), service.clone()));
    }
    let scheduler = tokio::spawn(domain_schedule_runner::run(
        domain.clone(),
        config.home.clone(),
        config.wake,
    ));
    if config.wake {
        tokio::spawn(auto_launch::run(domain.clone(), config.home.clone()));
    }
    node_reconcile::once(&domain, &node_close::Machine).await;
    let handed = domain.clone();
    tokio::task::spawn_blocking(move || domain_orchestrator::read_handed_notices_in_background(&handed));
    tokio::spawn(node_reconcile::run(domain.clone(), Arc::new(node_close::Machine)));
    tokio::spawn(evidence_retention::run(domain.clone()));
    tokio::spawn(fleet_gate::run(domain.clone(), config.home.clone()));
    let transport = tokio::spawn(domain_transport::run(hooks, domain.clone()));
    tokio::spawn(claude_check::run(plugins::node()));
    if config.fast_timing {
        domain.live.gate.tune(fleet_gate::Tuning { spacing: 1_000, limit: Some(fleet_gate::Limit::Fixed(alondra_core::options::MAX_WORKING_NODES)) });
        domain.live.gate.ignore_mac();
    }
    if config.ignore_mac_load {
        domain.live.gate.ignore_mac();
    }
    let revival = if config.fast_timing { &node_revival::Timing::SANDBOX } else { &node_revival::Timing::LIVE };
    tokio::spawn(node_revival::run(state_domain.clone(), revival));
    tokio::spawn(sdk_handover::run(state_domain.clone()));
    let closer = if config.fast_timing { task_closer::Timing::sandbox(config.close_free_nodes_after) } else { task_closer::Timing::new(config.close_free_nodes_after) };
    tokio::spawn(task_closer::run(state_domain.clone(), client.sessions(), closer));
    let merges_domain = domain.clone();
    let (merger, pr_poll) = if config.fast_timing {
        (domain_merges::Merger::with(std::time::Duration::from_secs(2), std::time::Duration::from_secs(15 * 60)), std::time::Duration::from_secs(2))
    } else {
        (domain_merges::Merger::default(), std::time::Duration::from_secs(60))
    };
    tokio::spawn(domain_merges::run(merges_domain, Arc::new(domain_github::GhCli::new()), merger));
    let github = tokio::spawn(domain_github::run(
        domain,
        Arc::new(domain_github::GhCli::new()),
        pr_poll,
    ));
    tokio::spawn(machine::run(gauges, state_domain.clone()));
    let owners = state_domain.clone();
    tokio::spawn(client.run(Arc::new(client_routes::GhView)));
    let orchestrator = config.wake.then(|| {
        tokio::spawn(domain_orchestrator::run(
            state_domain,
            config.home.clone(),
            watcher::claude_home(),
        ))
    });
    let connector = service
        .filter(|service| service.has_relay())
        .map(|service| tokio::spawn(async move { service.run().await }));
    tokio::spawn(updates::updater(config).run());
    start_guard::served();
    let stopped = tokio::select! {
        stopped = supervise_workers(server, outbox, scheduler, transport, github, orchestrator, connector) => stopped,
        () = replaced(config) => relaunch_now(),
        signal = terminated() => {
            log!("{signal} received: stopping the SDK hosts, then exiting");
            stop_sdk_owners(owners).await;
            std::process::exit(0)
        }
    };
    stopped
}

/// Resolves on the first SIGTERM or SIGINT, naming it.
async fn terminated() -> &'static str {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut interrupt)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else {
        log!("could not listen for SIGTERM and SIGINT; the daemon stops only when killed");
        return std::future::pending().await;
    };
    tokio::select! {
        _ = term.recv() => "SIGTERM",
        _ = interrupt.recv() => "SIGINT",
    }
}

/// A daemon that is stopped stops its SDK hosts and waits for each to exit. One that relaunches
/// into a new binary, or exits on an error, leaves them running for the daemon that comes back to
/// adopt.
async fn stop_sdk_owners(domain: Arc<domain_runtime::DomainRuntime>) {
    if let Err(error) = tokio::task::spawn_blocking(move || domain.stop_sdk_owners()).await {
        log!("stopping the SDK hosts failed: {error}");
    }
}

#[cfg(test)]
mod worker_supervision_tests {
    use super::*;

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
            anyhow::bail!("usage: alondrad [--help | --version | --test-service-push <device-id>]")
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
            scratch_directory.parent().unwrap().join(format!("alondra-domain-start-{}", auth::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        domain_cutover::refuse_legacy_home(&home).unwrap();
        ensure_domain_protocol(&home).unwrap();
        let _domain = domain_runtime::DomainRuntime::open(&home).unwrap();
        domain_bootstrap::DomainDaemon::open(&home).unwrap();
        assert!(home.join("domain.db").is_file());
        assert!(!home.join("alondra.db").exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn legacy_only_home_fails_closed_without_creating_domain_database() {
        let scratch_directory = crate::test_support::Directory::new();
        let home =
            scratch_directory.parent().unwrap().join(format!("alondra-domain-legacy-{}", auth::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("alondra.db"), b"preserve legacy bytes").unwrap();
        let error = domain_cutover::refuse_legacy_home(&home).unwrap_err();
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
            scratch_directory.parent().unwrap().join(format!("alondra-domain-guard-{}", auth::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        let legacy_domain = home.join("domain-v2.db");
        drop(domain_store::DomainStore::open(&legacy_domain).unwrap());
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
