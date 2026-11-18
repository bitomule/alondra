//! Alondra's plugins, which the sessions it launches load with `--plugin-dir`: `plugin` into every
//! one, `plugin-orchestrator` on top for its orchestrator. Where they are installed is the Mac's.
use std::path::{Path, PathBuf};

pub const NODE: &str = "plugin";
pub const ORCHESTRATOR: &str = "plugin-orchestrator";

pub trait Plugins: Send + Sync {
    /// The node plugin this daemon installed, else the copy in `home` an earlier start left.
    fn node(&self, home: &Path) -> Option<PathBuf>;
    /// The orchestrator plugin installed in `home`.
    fn orchestrator(&self, home: &Path) -> Option<PathBuf>;
    /// The plugin `name` of the marketplace registered in the Claude folder `claude_home`.
    fn registered(&self, claude_home: &Path, name: &str) -> Option<PathBuf>;
}

/// Alondra's launch settings, with extra permissions when given.
pub fn launch_settings_with(permissions: Option<serde_json::Value>) -> serde_json::Value {
    let mut settings: serde_json::Value = serde_json::from_str(&alondra_core::launch_settings()).expect("launch settings are JSON");
    if let Some(permissions) = permissions {
        settings["permissions"] = permissions;
    }
    settings
}

