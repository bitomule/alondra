//! Alondra's plugins live in the repo registered as the Alondra marketplace. Alondra loads them
//! into the sessions it launches with `--plugin-dir`, and only there: `plugin` into every one,
//! `plugin-orchestrator` on top for its orchestrator.

use std::path::{Path, PathBuf};

use serde_json::Value;

const MARKETPLACE: &str = "alondra";
pub const NODE: &str = "plugin";
pub const ORCHESTRATOR: &str = "plugin-orchestrator";

pub fn dir(claude_home: &Path, name: &str) -> Option<PathBuf> {
    let text = std::fs::read_to_string(claude_home.join("plugins/known_marketplaces.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let entry = v.get(MARKETPLACE)?;
    let root = entry.get("installLocation").or_else(|| entry.pointer("/source/path")).and_then(Value::as_str)?;
    let plugin = Path::new(root).join(name);
    plugin.join(".claude-plugin/plugin.json").is_file().then_some(plugin)
}

/// The flags that load Alondra's plugins into a session and mark it as Alondra's.
pub fn launch_flags(plugins: &[&Path]) -> Vec<String> {
    let mut out = vec![];
    for p in plugins {
        out.extend(["--plugin-dir".to_string(), p.display().to_string()]);
    }
    let bin = plugins.first().map(|p| p.join("bin/alondra"));
    out.extend(["--settings".to_string(), alondra_core::launch_settings(bin.as_deref())]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plugins_are_found_through_the_registered_marketplace() {
        let root = std::env::temp_dir().join(format!("alondra-plugins-{}", std::process::id()));
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join("plugin-orchestrator/.claude-plugin")).unwrap();
        std::fs::create_dir_all(repo.join("plugin/.claude-plugin")).unwrap();
        std::fs::create_dir_all(root.join("claude/plugins")).unwrap();
        let marketplaces = root.join("claude/plugins/known_marketplaces.json");

        assert_eq!(dir(&root.join("claude"), ORCHESTRATOR), None, "no marketplace file");
        std::fs::write(&marketplaces, format!(r#"{{"alondra": {{"source": {{"source": "directory", "path": "{}"}}}}}}"#, repo.display())).unwrap();
        assert_eq!(dir(&root.join("claude"), ORCHESTRATOR), None, "no plugin manifest");
        std::fs::write(repo.join("plugin-orchestrator/.claude-plugin/plugin.json"), "{}").unwrap();
        std::fs::write(repo.join("plugin/.claude-plugin/plugin.json"), "{}").unwrap();
        assert_eq!(dir(&root.join("claude"), ORCHESTRATOR), Some(repo.join("plugin-orchestrator")));
        assert_eq!(dir(&root.join("claude"), NODE), Some(repo.join("plugin")));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_launch_loads_the_plugins_turns_the_global_ones_off_and_carries_the_mark() {
        let flags = launch_flags(&[Path::new("/r/plugin"), Path::new("/r/plugin-orchestrator")]);
        assert_eq!(flags[..5], ["--plugin-dir", "/r/plugin", "--plugin-dir", "/r/plugin-orchestrator", "--settings"]);
        let settings: Value = serde_json::from_str(&flags[5]).unwrap();
        assert_eq!(settings["env"][alondra_core::LAUNCHED_ENV], "1");
        assert_eq!(settings["enabledPlugins"]["alondra@alondra"], false);
        assert_eq!(settings["enabledPlugins"]["alondra-orchestrator@alondra"], false);
        assert_eq!(settings["statusLine"]["command"], "'/r/plugin/bin/alondra' statusline", "the status line reports the context");
    }
}
