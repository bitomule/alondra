#[macro_use]
mod log;
mod auth;
mod approval_lease;
mod domain_cutover;
mod domain_agent_lifecycle;
mod domain_authority;
mod domain_content;
mod domain_delivery;
mod domain_github;
mod domain_migration;
mod domain_orchestrator;
mod domain_runtime;
mod domain_schedule_runner;
mod domain_store;
mod domain_transport;
mod domain_messages;
mod domain_sessions;
mod domain_orchestrator_tools;
mod domain_orchestrator_reads;
#[cfg(test)]
mod domain_e2e_tests;
mod domain_api;
mod domain_reads;
mod domain_schedule_api;
mod domain_bootstrap;
mod v2_client_routes;
mod classify;
mod http;
mod hook_socket;
mod delivery;
mod issues;
mod keeper;
mod managed_orchestrator;
mod orchestrator_process;
mod hub;
mod legacy;
mod machine;
mod markdown;
mod menus;
mod notifier;
mod options;
mod orchestrate;
mod plugins;
mod preflight;
mod prs;
mod schedules;
mod service;
mod service_files;
mod service_overview;
mod push;
mod relaunch;
mod sentinel;
mod start_guard;
mod store;
mod updates;
mod uploads;
mod usage;
mod wake;
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
            println!("Alondra daemon\n\nUsage: alondrad [OPTIONS]\n\nOptions:\n  -h, --help                       Print help\n  -V, --version                    Print version\n      --preflight                  Check that this binary can start on the configured HOME, without changing it\n      --test-service-push <device-id>\n                                   Send an encrypted diagnostic push to one paired device\n      --inspect-domain-migration <source-home>\n      --rehearse-domain-migration <source-home> <new-stage-home>\n      --cutover-domain-v2 <new-backup-dir> <legacy-alondrad> <legacy-alondra>\n                                   Cut this HOME over to domain v2: waits for the daemon to release HOME, backs it up with the legacy binaries, imports the work in flight and commits\n      --rollback-domain-v2 <backup-dir>\n                                   Restore the legacy HOME from a cutover backup\n      --rehearse-domain-cutover <live-home> <new-work-dir> <legacy-daemon> <report-prefix>\n\nWithout options, starts the local daemon and configured service connector.");
            return Ok(());
        }
        [flag] if flag == "--version" || flag == "-V" => {
            println!("alondrad {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        [flag] if flag == "--preflight" => {
            let checked = Config::load().map_err(|e| anyhow!("config.toml unreadable: {e}"))
                .and_then(|config| preflight::run(&config));
            match checked {
                Ok(summary) => println!("preflight ok: {summary}"),
                Err(e) => {
                    eprintln!("preflight failed: {}", format!("{e:#}").lines().collect::<Vec<_>>().join(" "));
                    std::process::exit(1);
                }
            }
            return Ok(());
        }
        [flag, source] if flag == "--inspect-domain-migration" => {
            let view = domain_migration::load_legacy_readonly(&std::path::Path::new(source).join("alondra.db"))?;
            println!("{}", serde_json::to_string_pretty(&domain_migration::inventory(&view))?);
            return Ok(());
        }
        [flag, source, target] if flag == "--rehearse-domain-migration" => {
            let result = domain_migration::rehearse(std::path::Path::new(source), std::path::Path::new(target))?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }
        [flag, backup, legacy_daemon, legacy_cli] if flag == "--cutover-domain-v2" => {
            domain_cutover::cutover_cli(Path::new(backup), &[Path::new(legacy_daemon), Path::new(legacy_cli)])?;
            return Ok(());
        }
        [flag, backup] if flag == "--rollback-domain-v2" => {
            domain_cutover::rollback_cli(Path::new(backup))?;
            return Ok(());
        }
        [flag, live, work, legacy, report] if flag == "--rehearse-domain-cutover" => {
            let passed = domain_migration::rehearsal::run_and_write(
                Path::new(live), Path::new(work), Path::new(legacy), Path::new(report))?;
            if !passed { std::process::exit(1); }
            return Ok(());
        }
        _ => (),
    }
    let diagnostic = diagnostic_device(args)?;
    let env = |k: &str| std::env::var(k).ok();
    let label = Config::load().map(|c| c.service_label).unwrap_or_else(|_| Config::defaults().service_label);
    start_guard::begin(alondra_core::config::alondra_home(&env), relaunch::supervised(&label, env));
    let error = match run(diagnostic) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    if let Some((record, wait)) = start_guard::failed(&error) {
        log!("start failed {} times in a row ({}); waiting {}s before exiting so launchd does not restart it every 10s",
            record.failures, record.reason, wait.as_secs());
        let watcher = relaunch::watcher(&label);
        let home = alondra_core::config::alondra_home(&env);
        let committed = || domain_cutover::read_marker(&home).ok().flatten()
            .is_some_and(|marker| marker.phase == domain_cutover::Phase::Committed);
        if start_guard::wait(wait, std::time::Duration::from_secs(2),
            || watcher.as_ref().is_some_and(|w| w.replaced()) || committed()) {
            log!("the binary was replaced or HOME was cut over; exiting now so launchd starts it");
        }
    }
    Err(error)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn run(diagnostic: Option<String>) -> Result<()> {
    let config = Config::load().map_err(|e| anyhow!("config.toml unreadable: {e}"))?;
    domain_cutover::refuse_legacy_home(&config.home)?;
    if let Some(kept) = retire_empty_older_sidecar(&config.home)? {
        log!("set aside an empty domain database from an older build; copy kept in {}", kept.display());
    }
    let _home_lock = alondra_core::home_lock::HomeLock::shared(&config.home)?;
    alondra_core::home_lock::ensure_ready(&config.home)?;
    // Select durable protocol mode before opening the legacy Hub or starting
    // any watcher, delivery worker, schedule, hook or v1 HTTP route.
    match protocol_mode(&config.home)? {
        ProtocolMode::Legacy => (),
        ProtocolMode::Prepared => anyhow::bail!("domain cutover is prepared but not committed"),
        ProtocolMode::Committed(epoch) => {
            anyhow::ensure!(diagnostic.is_none(), "legacy diagnostic push is unavailable after domain cutover");
            return run_v2_only(&config, &epoch).await;
        }
    }
    if let Some(device) = diagnostic {
        let auth = Arc::new(auth::Auth::open(&config.home)?);
        let service = service::Service::open(&config.home, config.port, auth)?
            .context("service is not configured; run alondra setup first")?;
        service.test_push(&device).await?;
        println!("Encrypted diagnostic push accepted by APNs for {device}. Delivery and decryption must be confirmed on the device.");
        return Ok(());
    }
    let claude_home = watcher::claude_home();
    let hold = std::env::var("ALONDRA_HOLD").map(|v| v != "0").unwrap_or(true);
    let addr = SocketAddr::new(config.listen, config.port);
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("cannot listen on {addr}"))?;

    // Own the per-home socket before recovery/migration can change durable delivery state.
    std::fs::create_dir_all(&config.home)?;
    let hooks = hook_socket::bind(&config.home).await?;
    let hub = Arc::new(hub::Hub::open(config.clone(), hold)?);
    let (drain, draining) = tokio::sync::watch::channel(false);
    let hooks = tokio::spawn(hook_socket::serve(hooks, hub.clone(), draining));
    tokio::spawn(watcher::run(hub.clone(), claude_home.clone()));
    if config.legacy_bridge {
        tokio::spawn(legacy::run(hub.clone(), claude_home.clone()));
    }
    tokio::spawn(machine::run(hub.clone()));
    if config.wake {
        tokio::spawn(wake::run(hub.clone()));
        tokio::spawn(keeper::run(hub.clone(), claude_home.clone()));
        tokio::spawn(menus::run(hub.clone()));
        if config.resume_dead_nodes {
            tokio::spawn(wake::resume_loop(hub.clone()));
        }
    }
    tokio::spawn(notifier::run(hub.clone()));
    tokio::spawn(retention(hub.clone(), config.retention_hours));
    tokio::spawn(updates::updater(&config).run());
    tokio::spawn(sentinel::run(hub.clone()));
    tokio::spawn(prs::run(hub.clone()));
    tokio::spawn(orchestrate::run(hub.clone()));
    if config.wake {
        tokio::spawn(schedules::run(hub.clone()));
    }

    let auth = Arc::new(auth::Auth::open(&config.home)?);
    let service = match service::Service::open(&config.home, config.port, auth.clone()) {
        Ok(Some(service)) => Some(Arc::new(service.with_hub(hub.clone()))),
        Ok(None) => None,
        Err(error) => {
            log!("service connector disabled: {error:#}");
            None
        }
    };
    if let Some(service) = service.clone() {
        tokio::spawn(async move { service.run().await });
    }
    let push = Arc::new(push::Push::open(&config.home)?);
    tokio::spawn(domain_delivery::run(hub.clone(), push.clone(), service.clone()));
    tokio::spawn(push::run(hub.clone(), push.clone(), service));
    log!("listening on http://{addr}, data in {}, for {} (legacy bridge {}, wake nodes {})",
        config.home.display(),
        config.name,
        if config.legacy_bridge { "yes" } else { "no" },
        if config.wake { "yes" } else { "no" },
    );
    let app = http::router(hub.clone(), auth.clone(), push).layer(axum::middleware::from_fn_with_state(auth, auth::guard));
    let server = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).into_future();
    start_guard::served();
    tokio::select! {
        served = server => served?,
        () = replaced(&config) => {
            drain.send_replace(true);
            let _ = hooks.await;
            hub.read_settled(|_| ());
            relaunch_now();
        }
    }
    Ok(())
}

/// Resolves once the launchd job's binary was replaced by one that passed preflight.
async fn replaced(config: &Config) {
    match relaunch::watcher(&config.service_label) {
        Some(watcher) => relaunch::watch(watcher, relaunch::every()).await,
        None => std::future::pending().await,
    }
}

fn relaunch_now() -> ! {
    log!("exiting with {} so launchd starts the new binary", relaunch::RELAUNCH);
    std::process::exit(relaunch::RELAUNCH)
}

enum ProtocolMode {
    Legacy,
    Prepared,
    Committed(String),
}

fn protocol_mode(home: &Path) -> Result<ProtocolMode> {
    match domain_cutover::read_marker(home)? {
        Some(marker) if marker.phase == domain_cutover::Phase::Prepared => Ok(ProtocolMode::Prepared),
        Some(marker) => Ok(ProtocolMode::Committed(marker.data_epoch)),
        None => {
            anyhow::ensure!(
                legacy_sidecar_is_empty(&home.join("domain-v2.db"))?,
                "domain database contains v2 state without cutover marker; refusing legacy startup"
            );
            Ok(ProtocolMode::Legacy)
        }
    }
}

/// Critical v2 workers must not disappear while the local listener remains healthy.
async fn supervise_v2_workers<S>(server: S, outbox: tokio::task::JoinHandle<Result<()>>,
    scheduler: tokio::task::JoinHandle<Result<()>>,
    transport: tokio::task::JoinHandle<Result<()>>,
    github: tokio::task::JoinHandle<Result<()>>,
    connector: Option<tokio::task::JoinHandle<()>>) -> Result<()>
where S: std::future::Future<Output = std::io::Result<()>> {
    tokio::select! {
        result = github => {
            match result {
                Ok(Ok(())) => anyhow::bail!("v2 GitHub observer stopped unexpectedly"),
                Ok(Err(error)) => anyhow::bail!("v2 GitHub observer failed: {error:#}"),
                Err(error) => anyhow::bail!("v2 GitHub observer panicked: {error}"),
            }
        },
        result = transport => {
            match result {
                Ok(Ok(())) => anyhow::bail!("v2 producer transport stopped unexpectedly"),
                Ok(Err(error)) => anyhow::bail!("v2 producer transport failed: {error:#}"),
                Err(error) => anyhow::bail!("v2 producer transport panicked: {error}"),
            }
        },
        result = server => {
            result?;
            anyhow::bail!("v2-only domain listener stopped unexpectedly")
        },
        result = outbox => {
            match result {
                Ok(Ok(())) => anyhow::bail!("v2 outbox worker stopped unexpectedly"),
                Ok(Err(error)) => anyhow::bail!("v2 outbox worker failed: {error:#}"),
                Err(error) => anyhow::bail!("v2 outbox worker panicked: {error}"),
            }
        },
        result = scheduler => {
            match result {
                Ok(Ok(())) => anyhow::bail!("v2 Schedule worker stopped unexpectedly"),
                Ok(Err(error)) => anyhow::bail!("v2 Schedule worker failed: {error:#}"),
                Err(error) => anyhow::bail!("v2 Schedule worker panicked: {error}"),
            }
        },
        result = async {
            match connector {
                Some(task) => Some(task.await),
                None => std::future::pending().await,
            }
        } => {
            match result.expect("connector future has task") {
                Ok(()) => anyhow::bail!("v2 encrypted service connector stopped unexpectedly"),
                Err(error) => anyhow::bail!("v2 encrypted service connector failed: {error}"),
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
/// holding rows, or not recognisable as ours, is left for `protocol_mode` to refuse.
fn retire_empty_older_sidecar(home: &Path) -> Result<Option<std::path::PathBuf>> {
    let database = home.join("domain-v2.db");
    if !database.exists() || domain_cutover::read_marker(home)?.is_some() { return Ok(None); }
    let Ok(_lock) = alondra_core::home_lock::HomeLock::recovery(home) else { return Ok(None) };
    if legacy_sidecar_is_empty(&database).unwrap_or(false)
        || !replaceable_cutover_sidecar_is_empty(&database).unwrap_or(false) {
        return Ok(None);
    }
    let kept = home.join(format!("domain-v2-retired-{}", unix_seconds()));
    std::fs::create_dir(&kept)?;
    let files = ["domain-v2.db", "domain-v2.db-wal", "domain-v2.db-shm"];
    for name in files {
        let source = home.join(name);
        if source.exists() { std::fs::copy(&source, kept.join(name))?; }
    }
    anyhow::ensure!(replaceable_cutover_sidecar_is_empty(&kept.join("domain-v2.db"))?,
        "copy of the old domain database does not verify; leaving it in place");
    for name in files {
        match std::fs::remove_file(home.join(name)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => (),
        }
    }
    Ok(Some(kept))
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
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
    anyhow::ensure!(metadata.is_file(), "markerless domain database must be a regular file");
    let conn = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let application: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    anyhow::ensure!(
        application == 0x414c4432
            && (version == domain_store::SCHEMA_VERSION
                || (allow_older_empty_schema && version >= 3 && version < domain_store::SCHEMA_VERSION)),
        "markerless domain database has unknown schema");
    let (revision, generation): (i64, i64) = conn.query_row(
        "SELECT revision,orchestrator_generation FROM domain_meta WHERE singleton=1",
        [], |row| Ok((row.get(0)?, row.get(1)?)))?;
    if revision != 0 || generation != 0 { return Ok(false); }
    let mut tables = conn.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name != 'domain_meta'")?;
    let names = tables.query_map([], |row| row.get::<_, String>(0))?;
    for name in names {
        let escaped = name?.replace('"', "\"\"");
        let statement = format!("SELECT EXISTS(SELECT 1 FROM \"{escaped}\" LIMIT 1)");
        let has_row: bool = conn.query_row(&statement, [], |row| row.get(0))?;
        if has_row { return Ok(false); }
    }
    Ok(true)
}

/// A committed HOME has one SQL authority shared by local and encrypted adapters.
async fn run_v2_only(config: &Config, expected_epoch: &str) -> Result<()> {
    let state = domain_bootstrap::V2Only::open(&config.home, expected_epoch)?;
    let auth = state.auth();
    let service = service::Service::open(&config.home, config.port, auth.clone())?
        .map(|service| Arc::new(service.with_domain(state.domain()).with_client(state.client())));
    let domain = state.domain();
    let state_domain = domain.clone();
    let gauges = state.gauges();
    let client = state.client();
    let push = Arc::new(push::Push::open(&config.home)?);
    let addr = SocketAddr::new(config.listen, config.port);
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("cannot listen on {addr}"))?;
    // The producer transport owns the per-home hook socket before any outbox
    // message can be claimed for a session.
    let hooks = hook_socket::bind(&config.home).await?;
    let app = domain_bootstrap::router(state)
        .layer(axum::middleware::from_fn_with_state(auth, auth::guard));
    log!("v2-only domain listener on http://{addr}, data in {}", config.home.display());
    let server = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).into_future();
    let outbox = tokio::spawn(domain_delivery::run_v2(
        domain.clone(), config.home.clone(), push, service.clone()));
    let scheduler = tokio::spawn(domain_schedule_runner::run(domain.clone(), config.home.clone(), config.wake));
    let transport = tokio::spawn(domain_transport::run(hooks, domain.clone(), config.home.clone()));
    let github = tokio::spawn(domain_github::run(domain, Arc::new(domain_github::GhCli::new())));
    tokio::spawn(machine::run_v2(gauges));
    tokio::spawn(client.run(Arc::new(v2_client_routes::GhView)));
    if config.wake {
        tokio::spawn(domain_orchestrator::run(state_domain.clone(), config.home.clone(), watcher::claude_home()));
    }
    let connector = service.map(|service| tokio::spawn(async move { service.run().await }));
    tokio::spawn(updates::updater(config).run());
    start_guard::served();
    tokio::select! {
        stopped = supervise_v2_workers(server, outbox, scheduler, transport, github, connector) => stopped,
        () = replaced(config) => relaunch_now(),
    }
}

#[cfg(test)]
mod v2_worker_supervision_tests {
    use super::*;

    #[tokio::test]
    async fn unexpected_connector_or_outbox_exit_stops_listener() {
        let server = || std::future::pending::<std::io::Result<()>>();
        let pending = || tokio::spawn(std::future::pending::<Result<()>>());
        let exit = supervise_v2_workers(server(), pending(), pending(), pending(), pending(),
            Some(tokio::spawn(async {}))).await.unwrap_err();
        assert!(exit.to_string().contains("connector stopped unexpectedly"));
        let panic = supervise_v2_workers(server(), pending(), pending(), pending(), pending(),
            Some(tokio::spawn(async { panic!("synthetic failure") }))).await.unwrap_err();
        assert!(panic.to_string().contains("connector failed"));
        let stopped_outbox = supervise_v2_workers(server(), tokio::spawn(async { Ok(()) }),
            pending(), pending(), pending(), None).await.unwrap_err();
        assert!(stopped_outbox.to_string().contains("outbox worker stopped unexpectedly"));
        let failed_outbox = supervise_v2_workers(server(),
            tokio::spawn(async { anyhow::bail!("synthetic SQL failure") }),
            pending(), pending(), pending(), None).await.unwrap_err();
        assert!(failed_outbox.to_string().contains("synthetic SQL failure"));
        let failed_scheduler = supervise_v2_workers(server(), pending(),
            tokio::spawn(async { anyhow::bail!("synthetic Schedule failure") }), pending(), pending(), None).await.unwrap_err();
        assert!(failed_scheduler.to_string().contains("synthetic Schedule failure"));
        let failed_transport = supervise_v2_workers(server(), pending(), pending(),
            tokio::spawn(async { anyhow::bail!("synthetic hook failure") }), pending(), None).await.unwrap_err();
        assert!(failed_transport.to_string().contains("synthetic hook failure"));
        let failed_github = supervise_v2_workers(server(), pending(), pending(), pending(),
            tokio::spawn(async { anyhow::bail!("synthetic GitHub failure") }), None).await.unwrap_err();
        assert!(failed_github.to_string().contains("synthetic GitHub failure"));
    }

}

fn diagnostic_device(args: Vec<String>) -> Result<Option<String>> {
    match args.as_slice() {
        [] => Ok(None),
        [flag, device] if flag == "--test-service-push" && !device.is_empty() && !device.starts_with('-') => Ok(Some(device.clone())),
        _ => anyhow::bail!("usage: alondrad [--help | --version | --test-service-push <device-id>]"),
    }
}

#[cfg(test)]
mod argument_tests {
    use super::*;

    #[test]
    fn diagnostic_requires_exact_explicit_target() {
        assert_eq!(diagnostic_device(vec![]).unwrap(), None);
        assert_eq!(diagnostic_device(vec!["--test-service-push".into(), "phone-1".into()]).unwrap(), Some("phone-1".into()));
        for args in [vec!["--test-service-push"], vec!["--test-service-push", ""], vec!["--test-service-push", "--all"], vec!["--test-service-push", "phone-1", "extra"], vec!["--unknown"]] {
            assert!(diagnostic_device(args.into_iter().map(String::from).collect()).is_err());
        }
    }

    #[test]
    fn staged_database_without_marker_never_falls_back_to_legacy() {
        let home = std::env::temp_dir().join(format!("alondra-mode-{}", auth::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        assert!(matches!(protocol_mode(&home).unwrap(), ProtocolMode::Legacy));
        std::fs::write(home.join("domain-v2.db"), b"stage left behind").unwrap();
        assert!(protocol_mode(&home).is_err());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn empty_legacy_sidecar_survives_two_boots_but_v2_rows_require_marker() {
        let home = std::env::temp_dir().join(format!("alondra-mode-sidecar-{}", auth::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        let config = Config::resolve(home.clone(), &Default::default(), |_| None, || Some("Test".into()));
        drop(hub::Hub::open(config.clone(), false).unwrap());
        assert!(matches!(protocol_mode(&home).unwrap(), ProtocolMode::Legacy));
        drop(hub::Hub::open(config, false).unwrap());
        assert!(matches!(protocol_mode(&home).unwrap(), ProtocolMode::Legacy));
        let conn = rusqlite::Connection::open(home.join("domain-v2.db")).unwrap();
        conn.execute("UPDATE domain_meta SET revision=1 WHERE singleton=1", []).unwrap();
        assert!(protocol_mode(&home).is_err());
        std::fs::remove_dir_all(home).unwrap();
    }

    fn older_empty_sidecar(home: &Path) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(home.join("domain-v2.db")).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA application_id=1095517234; PRAGMA user_version=3;
            CREATE TABLE domain_meta(singleton INTEGER PRIMARY KEY,revision INTEGER NOT NULL,
                orchestrator_generation INTEGER NOT NULL);
            INSERT INTO domain_meta VALUES(1,0,0);
            CREATE TABLE command_receipts(id TEXT PRIMARY KEY);").unwrap();
        conn
    }

    #[test]
    fn an_empty_sidecar_from_an_older_build_is_kept_aside_and_legacy_starts() {
        let home = std::env::temp_dir().join(format!("alondra-retire-{}", auth::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        drop(older_empty_sidecar(&home));
        assert!(protocol_mode(&home).is_err());
        let kept = retire_empty_older_sidecar(&home).unwrap().unwrap();
        assert!(!home.join("domain-v2.db").exists());
        assert!(replaceable_cutover_sidecar_is_empty(&kept.join("domain-v2.db")).unwrap());
        assert!(matches!(protocol_mode(&home).unwrap(), ProtocolMode::Legacy));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn an_older_sidecar_with_rows_is_never_moved_and_startup_refuses() {
        let home = std::env::temp_dir().join(format!("alondra-retire-rows-{}", auth::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        let conn = older_empty_sidecar(&home);
        conn.execute("INSERT INTO command_receipts(id) VALUES('existing')", []).unwrap();
        drop(conn);
        assert!(retire_empty_older_sidecar(&home).unwrap().is_none());
        assert!(home.join("domain-v2.db").exists());
        assert!(protocol_mode(&home).is_err());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_busy_home_leaves_the_sidecar_alone() {
        let home = std::env::temp_dir().join(format!("alondra-retire-busy-{}", auth::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        drop(older_empty_sidecar(&home));
        let _running = alondra_core::home_lock::HomeLock::shared(&home).unwrap();
        assert!(retire_empty_older_sidecar(&home).unwrap().is_none());
        assert!(home.join("domain-v2.db").exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn cutover_replaces_only_an_empty_older_domain_sidecar() {
        let home = std::env::temp_dir().join(format!("alondra-empty-sidecar-{}", auth::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        let path = home.join("domain-v2.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA application_id=1095517234; PRAGMA user_version=3;
            CREATE TABLE domain_meta(singleton INTEGER PRIMARY KEY,revision INTEGER NOT NULL,
                orchestrator_generation INTEGER NOT NULL);
            INSERT INTO domain_meta VALUES(1,0,0);
            CREATE TABLE command_receipts(id TEXT PRIMARY KEY);").unwrap();
        assert!(legacy_sidecar_is_empty(&path).is_err());
        assert!(replaceable_cutover_sidecar_is_empty(&path).unwrap());
        conn.execute("INSERT INTO command_receipts(id) VALUES('existing')", []).unwrap();
        assert!(!replaceable_cutover_sidecar_is_empty(&path).unwrap());
        conn.execute("DELETE FROM command_receipts", []).unwrap();
        conn.execute_batch("PRAGMA application_id=0").unwrap();
        assert!(replaceable_cutover_sidecar_is_empty(&path).is_err());
        drop(conn);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn prepared_and_committed_markers_select_only_v2_paths() {
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!("alondra-mode-marker-{}", auth::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        let path = home.join(domain_cutover::MARKER);
        let mut marker = domain_cutover::Marker {
            format: 1,
            phase: domain_cutover::Phase::Prepared,
            backup_manifest_sha256: "a".repeat(64),
            source_seq: 42,
            data_epoch: "b".repeat(64),
        };
        std::fs::write(&path, serde_json::to_vec(&marker).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(protocol_mode(&home).unwrap(), ProtocolMode::Prepared));
        marker.phase = domain_cutover::Phase::Committed;
        std::fs::write(&path, serde_json::to_vec(&marker).unwrap()).unwrap();
        assert!(matches!(protocol_mode(&home).unwrap(), ProtocolMode::Committed(epoch) if epoch == marker.data_epoch));
        assert!(!home.join("alondra.db").exists(), "mode selection must not construct the legacy store");
        std::fs::remove_dir_all(home).unwrap();
    }
}

async fn retention(hub: Arc<hub::Hub>, retention_hours: f64) {
    let every_hours = std::env::var("ALONDRA_COMPACT_EVERY_HOURS").ok().and_then(|v| v.parse::<f64>().ok()).unwrap_or(6.0);
    let keep = std::time::Duration::from_secs_f64(retention_hours * 3600.0);
    let every = std::time::Duration::from_secs_f64(every_hours * 3600.0);
    loop {
        tokio::time::sleep(every).await;
        let compacting = hub.clone();
        match tokio::task::spawn_blocking(move || compacting.compact(keep)).await {
            Ok(Ok(deleted)) => log!("compacted, {deleted} old events deleted"),
            Ok(Err(e)) => log!("could not compact: {e}"),
            Err(e) => log!("compaction crashed: {e}"),
        }
        let referenced = hub.read(|v| v.stored_files());
        let home = hub.home.clone();
        match tokio::task::spawn_blocking(move || uploads::sweep(&home, &referenced, uploads::UNSENT_GRACE)).await {
            Ok(0) => {}
            Ok(n) => log!("{n} attachment folders nothing refers to any more deleted"),
            Err(e) => log!("attachment sweep crashed: {e}"),
        }
    }
}
