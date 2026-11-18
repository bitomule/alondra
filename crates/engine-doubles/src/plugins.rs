//! Alondra's plugins on a test's Mac: the copy in the test's home when it made one, else a path no
//! session of the test reads; the registered marketplace is the one the test wrote.
use alondra_engine_ports::Registry;
use alondra_engine_ports::plugins::{NODE, ORCHESTRATOR, Plugins as Port};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Plugins;

pub fn wire(registry: &Registry) {
    registry.provide::<dyn Port>(Arc::new(Plugins));
}

impl Port for Plugins {
    fn node(&self, home: &Path) -> Option<PathBuf> {
        Some(home.join(NODE)).filter(|plugin| plugin.is_dir()).or_else(|| Some(PathBuf::from("/synthetic/plugin")))
    }
    fn orchestrator(&self, home: &Path) -> Option<PathBuf> {
        Some(home.join(ORCHESTRATOR)).filter(|plugin| plugin.join(".claude-plugin/plugin.json").is_file())
    }
    fn registered(&self, claude_home: &Path, name: &str) -> Option<PathBuf> {
        let text = std::fs::read_to_string(claude_home.join("plugins/known_marketplaces.json")).ok()?;
        let marketplaces: serde_json::Value = serde_json::from_str(&text).ok()?;
        let entry = marketplaces.get("alondra")?;
        let root = entry.get("installLocation").or_else(|| entry.pointer("/source/path")).and_then(serde_json::Value::as_str)?;
        let plugin = Path::new(root).join(name);
        plugin.join(".claude-plugin/plugin.json").is_file().then_some(plugin)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_doubled_plugins_keep_the_contract_the_macs_keep() {
        crate::contract::plugins(&super::Plugins);
    }
}
