//! The daemon's input adapter: the HTTP routes, the iroh and direct transports, the pushes and the
//! screens' cache the clients read. `alondrad` assembles it with the engine and the screens.
// The engine's fixtures take its `Directory`, so alondrad's tests share that one.
#[cfg(test)]
pub use alondra_daemon_engine::test_support;

#[macro_use]
extern crate alondra_daemon_support;

pub use alondra_github_gh::{pr_viewed, prs};

pub use alondra_engine_ports::plugins;
pub use alondra_daemon_support::{
    approval_lease,
    auth,
    blocking_work,
    card_language,
    classify,
    device_usage,
    domain_authority,
    domain_cutover,
    exit,
    files_retention,
    job_state,
    local_network,
    locks,
    log,
    open_files,
    pr_heads,
    pr_ids,
    push,
    relaunch,
    reports,
    result_files,
    revoked_devices,
    scoped_reads,
    service_files,
    start_guard,
    updates,
    uploads,
    watcher,
    workflow_state,
};
#[cfg(test)]
pub use alondra_engine_ports::disk;
#[cfg(test)]
pub use alondra_store_sqlite::domain_database_migration;
pub use alondra_daemon_engine::{
    accounts,
    allow_always,
    claude_access,
    cleanup,
    domain_github,
    domain_holds,
    domain_merges,
    domain_messages,
    domain_orchestrator_reads,
    domain_orchestrator_tools,
    domain_runtime,
    domain_store,
    domain_transport,
    home_renames,
    idle_compaction,
    issues,
    launch_plans,
    local_network_card,
    machine,
    mcp_probe,
    mcp_remove,
    mcp_waits,
    node_close,
    node_push,
    node_reports,
    node_work,
    obligation_watch,
    options,
    pr_images,
    retention,
    sdk_handover,
    sdk_live,
    task_context,
    task_worktrees,
    usage_pause,
    usage_probe,
};
pub use alondra_engine_sdk::{account_move, auto_launch, bg_handover, domain_schedule_api, domain_schedule_runner, idle_release, provider_transition, sdk_owner, task_closer};
pub use alondra_engine_admission::{admission, machine_watch, pauses};
pub use alondra_engine_orchestrator::domain_orchestrator;
#[cfg(test)]
pub use alondra_daemon_engine::{domain_agent_lifecycle, machine_state};

pub mod operation_journal;
pub mod direct;
pub mod claude_check;
pub mod domain_api;
pub mod iroh_terminals;
pub mod domain_bootstrap;
pub mod delivery_review;
pub mod domain_delivery;
#[cfg(test)]
mod domain_e2e_tests;
pub mod iroh_endpoint;
pub mod reachability;
pub mod device_expiry;
pub mod notices;
pub mod preflight;
pub mod public_content;
pub mod push_gate;
pub mod push_transport;
pub mod activity_push;
pub mod push_alerts;
pub mod push_retract;
pub mod service;
pub use service::scenario_phone;
pub mod settings;
pub mod client_routes;
pub mod views;
#[cfg(test)]
mod engine_tests;

pub mod screens;

use std::path::Path;
use std::sync::Arc;
use anyhow::Result;

pub fn ensure_domain_protocol(home: &std::path::Path) -> Result<()> {
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

/// What approving a direct pairing request does on this Mac: pin the device's keys in
/// `service.toml`, approve it in the registry, and make the running service accept it.
pub fn direct_approval(
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

/// Before cutover, the legacy Hub creates an empty v2 sidecar for its API.
/// A markerless database may be reused by legacy only while it contains no
/// commands, entities, receipts, deliveries, or migration metadata.
pub fn legacy_sidecar_is_empty(path: &Path) -> Result<bool> {
    empty_domain_sidecar(path, false)
}

/// A pre-release build left an empty domain database on an older schema. Legacy
/// startup refuses unknown schemas, so under the exclusive HOME lock it is copied
/// aside and removed; the legacy Hub then creates the current empty one. Anything
/// holding rows, or not recognisable as ours, is left for `ensure_domain_protocol` to refuse.
pub fn retire_empty_older_sidecar(home: &Path) -> Result<Option<std::path::PathBuf>> {
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

pub fn unix_seconds() -> u64 {
    alondra_core::clock::system_time()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Cutover may replace a markerless experimental sidecar only after proving it
/// contains no business, receipt, delivery, or migration rows. The final store
/// is installed from the verified legacy backup, not upgraded in place.
pub fn replaceable_cutover_sidecar_is_empty(path: &Path) -> Result<bool> {
    empty_domain_sidecar(path, true)
}

pub fn empty_domain_sidecar(path: &Path, allow_older_empty_schema: bool) -> Result<bool> {
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
