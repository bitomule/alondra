mod classify;
mod http;
mod hub;
mod machine;
mod store;
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
    let claude_home = std::env::var("CLAUDE_HOME").map(PathBuf::from).unwrap_or_else(|_| home_dir().join(".claude"));
    let port: u16 = std::env::var("ALONDRA_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(alondra_core::DEFAULT_PORT);
    let hold = std::env::var("ALONDRA_HOLD").map(|v| v != "0").unwrap_or(true);

    let hub = Arc::new(hub::Hub::open(alondra_home.clone(), hold)?);
    tokio::spawn(watcher::run(hub.clone(), claude_home));
    tokio::spawn(machine::run(hub.clone()));

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("no puedo escuchar en {addr}"))?;
    eprintln!("alondrad: escuchando en http://{addr}, datos en {}", alondra_home.display());
    axum::serve(listener, http::router(hub)).await?;
    Ok(())
}
