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

/// Where the node plugin comes from: `ALONDRA_PLUGIN_SOURCE` when set (a sandbox serving a
/// checkout), else the registered marketplace.
pub fn node_source(claude_home: &Path) -> Option<PathBuf> {
    match std::env::var_os("ALONDRA_PLUGIN_SOURCE") {
        Some(dir) => Some(PathBuf::from(dir)).filter(|dir| dir.join(".claude-plugin/plugin.json").is_file()),
        None => dir(claude_home, NODE),
    }
}

static NODE_PLUGIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// The node plugin launched sessions load with `--plugin-dir`, once `install_node` placed it.
pub fn node() -> Option<PathBuf> {
    NODE_PLUGIN.get().cloned()
}

/// Claude Code loads a mod from the folder a session was started with and drops it for good when
/// that folder is replaced, as `brew upgrade` replaces its keg. So sessions load a copy this
/// daemon owns, `<home>/plugin`, which every start brings up to date file by file: a running
/// session sees its mod change and reloads it. Its `bin/alondra` runs this daemon's own CLI
/// against this daemon's home.
pub fn install_node(home: &Path, source: &Path, alondra: &Path) -> anyhow::Result<PathBuf> {
    let target = home.join(NODE);
    sync(source, &target, &wrapper(home, alondra))?;
    let _ = NODE_PLUGIN.set(target.clone());
    Ok(target)
}

/// The CLI next to this daemon, by the path that survives an upgrade: Homebrew's `opt` link
/// instead of the versioned keg.
pub fn own_cli() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let cli = exe.with_file_name("alondra");
    let keg = cli.parent()?.parent()?;
    let rack = keg.parent()?;
    let cellar = rack.parent()?;
    if cellar.file_name()? == "Cellar" && rack.file_name()? == "alondra" {
        return Some(cellar.parent()?.join("opt/alondra/bin/alondra"));
    }
    Some(cli)
}

fn quoted(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

fn wrapper(home: &Path, alondra: &Path) -> String {
    format!("#!/bin/sh\nALONDRA_HOME={} exec {} \"$@\"\n", quoted(home), quoted(alondra))
}

/// What Claude Code writes beside a mod it loads: never copied, never removed.
fn generated(relative: &Path) -> bool {
    relative.starts_with(".claude-plugin/types") || relative == Path::new("tsconfig.json") || relative == Path::new(".gitignore")
}

fn files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let relative = path.strip_prefix(root).expect("walked under root").to_path_buf();
        if generated(&relative) || relative.starts_with("bin") {
            continue;
        }
        if path.is_dir() {
            files(root, &path, out)?;
        } else {
            out.push(relative);
        }
    }
    Ok(())
}

/// Writes only what changed, each file by rename so no session reads half of one, and never
/// replaces the folder itself.
fn put(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if std::fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    std::fs::create_dir_all(path.parent().expect("a file has a folder"))?;
    let next = path.with_file_name(format!(".{}.next", path.file_name().expect("a file has a name").to_string_lossy()));
    std::fs::write(&next, bytes)?;
    std::fs::set_permissions(&next, std::fs::Permissions::from_mode(mode))?;
    std::fs::rename(&next, path)
}

fn sync(source: &Path, target: &Path, wrapper: &str) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut wanted = vec![];
    files(source, source, &mut wanted)?;
    for relative in &wanted {
        let from = source.join(relative);
        let mode = std::fs::metadata(&from)?.permissions().mode() & 0o777;
        put(&target.join(relative), &std::fs::read(&from)?, mode)?;
    }
    put(&target.join("bin/alondra"), wrapper.as_bytes(), 0o755)?;
    let mut present = vec![];
    if target.is_dir() {
        files(target, target, &mut present)?;
    }
    for stale in present.iter().filter(|relative| !wanted.contains(relative)) {
        std::fs::remove_file(target.join(stale))?;
    }
    Ok(())
}

/// The flags that load Alondra's plugins into a session and mark it as Alondra's.
pub fn launch_flags(plugins: &[&Path]) -> Vec<String> {
    let mut out = vec![];
    for p in plugins {
        out.extend(["--plugin-dir".to_string(), p.display().to_string()]);
    }
    out.extend(["--settings".to_string(), alondra_core::launch_settings()]);
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
    fn an_upgrade_updates_the_daemons_plugin_copy_in_place_so_running_sessions_keep_their_mod() {
        use std::os::unix::fs::MetadataExt;
        let root = std::env::temp_dir().join(format!("alondra-plugin-copy-{}", crate::auth::random_hex(8)));
        let (old, new, home) = (root.join("Cellar/0.2.22/plugin"), root.join("Cellar/0.2.23/plugin"), root.join("home"));
        for (keg, mod_text) in [(&old, "old mod"), (&new, "new mod")] {
            std::fs::create_dir_all(keg.join(".claude-plugin")).unwrap();
            std::fs::create_dir_all(keg.join("hooks")).unwrap();
            std::fs::create_dir_all(keg.join("bin")).unwrap();
            std::fs::write(keg.join(".claude-plugin/plugin.json"), "{}").unwrap();
            std::fs::write(keg.join("hooks/register.ts"), mod_text).unwrap();
            std::fs::write(keg.join("bin/alondra"), "#!/bin/sh\nexec brew-alondra\n").unwrap();
        }
        std::fs::write(old.join("hooks/retired.ts"), "gone in the new release").unwrap();
        let cli = Path::new("/opt/homebrew/opt/alondra/bin/alondra");

        let copy = install_node(&home, &old, cli).unwrap();
        assert_eq!(copy, home.join("plugin"));
        assert_eq!(std::fs::read_to_string(copy.join("hooks/register.ts")).unwrap(), "old mod");
        let wrapper = std::fs::read_to_string(copy.join("bin/alondra")).unwrap();
        assert_eq!(wrapper, format!("#!/bin/sh\nALONDRA_HOME='{}' exec '/opt/homebrew/opt/alondra/bin/alondra' \"$@\"\n", home.display()));
        std::fs::create_dir_all(copy.join(".claude-plugin/types")).unwrap();
        std::fs::write(copy.join(".claude-plugin/types/index.d.ts"), "written by Claude Code").unwrap();
        let folder = std::fs::metadata(&copy).unwrap().ino();
        let manifest = std::fs::metadata(copy.join(".claude-plugin/plugin.json")).unwrap().ino();

        install_node(&home, &new, cli).unwrap();
        std::fs::remove_dir_all(&old).unwrap();
        assert_eq!(std::fs::metadata(&copy).unwrap().ino(), folder, "the folder sessions loaded is never replaced");
        assert_eq!(std::fs::read_to_string(copy.join("hooks/register.ts")).unwrap(), "new mod", "the mod changes in place");
        assert_eq!(std::fs::metadata(copy.join(".claude-plugin/plugin.json")).unwrap().ino(), manifest, "an unchanged file is left alone");
        assert!(!copy.join("hooks/retired.ts").exists(), "a file the release dropped goes");
        assert!(copy.join(".claude-plugin/types/index.d.ts").exists(), "what Claude Code writes beside the mod stays");
        assert_eq!(std::fs::read_to_string(copy.join("bin/alondra")).unwrap(), wrapper);
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
        assert!(settings.get("statusLine").is_none(), "the session keeps the user's own status line");
    }
}
