#[macro_use]
mod log;
mod auth;
mod approval_lease;
mod domain_authority;
mod domain_content;
mod domain_delivery;
mod domain_migration;
mod domain_runtime;
mod domain_store;
mod domain_api;
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
mod prs;
mod schedules;
mod service;
mod service_files;
mod service_overview;
mod push;
mod sentinel;
mod store;
mod uploads;
mod usage;
mod wake;
mod watcher;
mod worktree;

use std::net::SocketAddr;
use std::sync::Arc;

use alondra_core::config::Config;
use anyhow::{Context, Result, anyhow};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--help" || flag == "-h" => {
            println!("Alondra daemon\n\nUsage: alondrad [OPTIONS]\n\nOptions:\n  -h, --help                       Print help\n  -V, --version                    Print version\n      --test-service-push <device-id>\n                                   Send an encrypted diagnostic push to one paired device\n      --inspect-domain-migration <source-home>\n      --rehearse-domain-migration <source-home> <new-stage-home>\n      --backup-domain-offline <source-home> <new-backup> <legacy-daemon>\n      --prepare-domain-cutover <source-home> <new-backup> <new-stage-home> <legacy-daemon>\n      --restore-domain-rehearsal <backup> <manifest-sha256> <new-home>\n\nThe backup and preparation commands require the daemon to be stopped and leave the live HOME unchanged.\nWithout options, starts the local daemon and configured service connector.");
            return Ok(());
        }
        [flag] if flag == "--version" || flag == "-V" => {
            println!("alondrad {}", env!("CARGO_PKG_VERSION"));
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
        [flag, source, target, old_binary] if flag == "--backup-domain-offline" => {
            let destination = std::path::Path::new(target);
            let result = domain_migration::backup_offline(std::path::Path::new(source), destination, std::path::Path::new(old_binary))?;
            println!("Verified private backup: {} files; legacy seq {}; manifest SHA256 {}",
                result.files.len(), result.source_seq, domain_migration::backup_manifest_digest(destination)?);
            return Ok(());
        }
        [flag, source, backup, stage, old_binary] if flag == "--prepare-domain-cutover" => {
            let result = domain_migration::prepare_cutover_offline(
                std::path::Path::new(source), std::path::Path::new(backup),
                std::path::Path::new(stage), std::path::Path::new(old_binary))?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }
        [flag, backup, digest, target] if flag == "--restore-domain-rehearsal" => {
            let result = domain_migration::restore_rehearsal(std::path::Path::new(backup), digest, std::path::Path::new(target))?;
            println!("Verified rollback rehearsal: legacy seq {}", result.source_seq);
            return Ok(());
        }
        _ => (),
    }
    run(diagnostic_device(args)?)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn run(diagnostic: Option<String>) -> Result<()> {
    let config = Config::load().map_err(|e| anyhow!("config.toml unreadable: {e}"))?;
    let _home_lock = alondra_core::home_lock::HomeLock::shared(&config.home)?;
    alondra_core::home_lock::ensure_ready(&config.home)?;
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
    tokio::spawn(hook_socket::serve(hooks, hub.clone()));
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
    let app = http::router(hub, auth.clone(), push).layer(axum::middleware::from_fn_with_state(auth, auth::guard));
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
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
