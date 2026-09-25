#[macro_use]
mod log;
mod auth;
mod classify;
mod http;
mod hook_socket;
mod issues;
mod keeper;
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

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let config = Config::load().map_err(|e| anyhow!("config.toml unreadable: {e}"))?;
    let claude_home = watcher::claude_home();
    let hold = std::env::var("ALONDRA_HOLD").map(|v| v != "0").unwrap_or(true);
    let addr = SocketAddr::new(config.listen, config.port);
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("cannot listen on {addr}"))?;

    let hub = Arc::new(hub::Hub::open(config.clone(), hold)?);
    let hooks = hook_socket::bind(&config.home).await?;
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
    let push = Arc::new(push::Push::open(&config.home)?);
    tokio::spawn(push::run(hub.clone(), push.clone()));
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
