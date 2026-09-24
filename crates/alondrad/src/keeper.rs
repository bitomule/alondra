use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use alondra_core::{Event, keeper, now_millis};
use serde_json::Value;

use crate::hub::Hub;

const EVERY: Duration = Duration::from_secs(30);
const MARKETPLACE: &str = "alondra";

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
    let ok = match orchestrator_plugin(claude_home) {
        Some(plugin) => launch(&plugin, &hub.config.name).await,
        None => {
            log!("keeper: no orchestrator is running and the Alondra marketplace is not registered, so there is no plugin to launch one with");
            false
        }
    };
    hub.append(Event::OrchestratorLaunched { ok })?;
    Ok(())
}

/// The orchestrator plugin ships next to the node plugin, in the repo registered as the Alondra marketplace.
fn orchestrator_plugin(claude_home: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(claude_home.join("plugins/known_marketplaces.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let entry = v.get(MARKETPLACE)?;
    let root = entry.get("installLocation").or_else(|| entry.pointer("/source/path")).and_then(Value::as_str)?;
    let plugin = Path::new(root).join("plugin-orchestrator");
    plugin.join(".claude-plugin/plugin.json").is_file().then_some(plugin)
}

async fn launch(plugin: &Path, user: &str) -> bool {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let mut cmd = tokio::process::Command::new("claude");
    cmd.current_dir(&home)
        .args(["--bg", "--name", "orchestrator", "--plugin-dir"])
        .arg(plugin)
        .args(["--permission-mode", "auto", "--"])
        .arg(keeper::mission(user))
        .kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(60), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => {
            log!("keeper: no orchestrator was running; launched one with {}", plugin.display());
            true
        }
        Ok(Ok(out)) => {
            log!("keeper: launching an orchestrator failed: {}", String::from_utf8_lossy(&out.stderr).trim());
            false
        }
        Ok(Err(e)) => {
            log!("keeper: launching an orchestrator failed: {e}");
            false
        }
        Err(_) => {
            log!("keeper: claude --bg did not respond within 60 s");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plugin_is_found_through_the_registered_marketplace() {
        let root = std::env::temp_dir().join(format!("alondra-keeper-{}", std::process::id()));
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join("plugin-orchestrator/.claude-plugin")).unwrap();
        std::fs::create_dir_all(root.join("claude/plugins")).unwrap();
        let marketplaces = root.join("claude/plugins/known_marketplaces.json");

        assert_eq!(orchestrator_plugin(&root.join("claude")), None, "no marketplace file");
        std::fs::write(&marketplaces, format!(r#"{{"alondra": {{"source": {{"source": "directory", "path": "{}"}}}}}}"#, repo.display())).unwrap();
        assert_eq!(orchestrator_plugin(&root.join("claude")), None, "no plugin manifest");
        std::fs::write(repo.join("plugin-orchestrator/.claude-plugin/plugin.json"), "{}").unwrap();
        assert_eq!(orchestrator_plugin(&root.join("claude")), Some(repo.join("plugin-orchestrator")));

        std::fs::remove_dir_all(root).unwrap();
    }
}
