use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use alondra_core::{Event, keeper, now_millis};

use crate::hub::Hub;
use crate::plugins;

const EVERY: Duration = Duration::from_secs(30);

pub async fn run(hub: Arc<Hub>, claude_home: PathBuf) {
    let started_at = now_millis();
    loop {
        tokio::time::sleep(EVERY).await;
        if let Err(e) = tick(&hub, &claude_home, started_at).await {
            log!("keeper: {e}");
        }
    }
}

async fn tick(hub: &Hub, claude_home: &Path, started_at: i64) -> anyhow::Result<()> {
    hub.append_with(|v| keeper::to_announce(v).map(|o| Event::ReportPosted(keeper::announcement(v, o, now_millis()))))?;
    if !hub.read(|v| keeper::should_launch(v, now_millis(), started_at)) {
        return Ok(());
    }
    let found = plugins::dir(claude_home, plugins::NODE).zip(plugins::dir(claude_home, plugins::ORCHESTRATOR));
    let _launching = hub.launching();
    let (ok, session) = match found {
        Some((node, orchestrator)) => match launch(&node, &orchestrator, &hub.config.name).await {
            Some(printed) => (true, alondra_core::sessions::launched_short(&printed)),
            None => (false, None),
        },
        None => {
            log!("keeper: no orchestrator is running and the Alondra marketplace is not registered, so there is no plugin to launch one with");
            (false, None)
        }
    };
    hub.append(Event::OrchestratorLaunched { ok, session: session.clone() })?;
    if let Some(short) = session {
        hub.admit_launched(&short)?;
    }
    Ok(())
}

/// The orchestrator gets the node plugin, for the hooks every Alondra session has, and the
/// orchestrator plugin for its tools. What `claude --bg` printed, or None when it failed.
async fn launch(node: &Path, orchestrator: &Path, user: &str) -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let mut cmd = tokio::process::Command::new("claude");
    cmd.current_dir(&home)
        .args(["--bg", "--name", "orchestrator"])
        .args(plugins::launch_flags(&[node, orchestrator]))
        .args(["--permission-mode", "auto", "--"])
        .arg(keeper::mission(user))
        .kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(60), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => {
            log!("keeper: no orchestrator was running; launched one with {}", orchestrator.display());
            Some(String::from_utf8_lossy(&out.stdout).to_string())
        }
        Ok(Ok(out)) => {
            log!("keeper: launching an orchestrator failed: {}", String::from_utf8_lossy(&out.stderr).trim());
            None
        }
        Ok(Err(e)) => {
            log!("keeper: launching an orchestrator failed: {e}");
            None
        }
        Err(_) => {
            log!("keeper: claude --bg did not respond within 60 s");
            None
        }
    }
}
