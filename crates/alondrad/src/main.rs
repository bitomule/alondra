mod auth;
mod classify;
mod http;
mod hub;
mod legacy;
mod machine;
mod notifier;
mod push;
mod store;
mod wake;
mod watcher;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let alondra_home = std::env::var("ALONDRA_HOME").map(PathBuf::from).unwrap_or_else(|_| home_dir().join(".alondra"));
    let claude_home = watcher::claude_home();
    let port: u16 = std::env::var("ALONDRA_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(alondra_core::DEFAULT_PORT);
    let hold = std::env::var("ALONDRA_HOLD").map(|v| v != "0").unwrap_or(true);

    let hub = Arc::new(hub::Hub::open(alondra_home.clone(), hold)?);
    tokio::spawn(watcher::run(hub.clone(), claude_home.clone()));
    tokio::spawn(legacy::run(hub.clone(), claude_home));
    tokio::spawn(machine::run(hub.clone()));
    tokio::spawn(wake::run(hub.clone()));
    tokio::spawn(notifier::run(hub.clone()));

    let auth = Arc::new(auth::Auth::open(&alondra_home)?);
    let push = Arc::new(push::Push::open(&alondra_home)?);
    tokio::spawn(push::run(hub.clone(), push.clone()));
    let ip: std::net::IpAddr = std::env::var("ALONDRA_LISTEN").ok().and_then(|v| v.parse().ok()).unwrap_or([127, 0, 0, 1].into());
    let addr = SocketAddr::new(ip, port);
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("no puedo escuchar en {addr}"))?;
    eprintln!("alondrad: escuchando en http://{addr}, datos en {}", alondra_home.display());
    let app = http::router(hub, auth.clone(), push).layer(axum::middleware::from_fn_with_state(auth, auth::guard));
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}
