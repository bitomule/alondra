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
        None => bundled_source(NODE).or_else(|| dir(claude_home, NODE)),
    }
}

pub fn orchestrator_source(claude_home: &Path) -> Option<PathBuf> {
    let explicit = std::env::var_os("ALONDRA_ORCHESTRATOR_SOURCE").map(PathBuf::from);
    let beside_node = std::env::var_os("ALONDRA_PLUGIN_SOURCE").and_then(|source| PathBuf::from(source).parent().map(|parent| parent.join(ORCHESTRATOR)));
    explicit.or(beside_node).filter(|source| source.join(".claude-plugin/plugin.json").is_file())
        .or_else(|| bundled_source(ORCHESTRATOR)).or_else(|| dir(claude_home, ORCHESTRATOR))
}

fn bundled_source(name: &str) -> Option<PathBuf> {
    bundled_source_for(&std::env::current_exe().ok()?, name)
}

fn bundled_source_for(executable: &Path, name: &str) -> Option<PathBuf> {
    if !matches!(name, NODE | ORCHESTRATOR) { return None; }
    let prefix = executable.parent()?.parent()?;
    let source = prefix.join("libexec/alondra").join(name);
    source.join(".claude-plugin/plugin.json").is_file().then_some(source)
}

static NODE_PLUGIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// The node plugin launched sessions load with `--plugin-dir`, once `install_node` placed it.
pub fn node() -> Option<PathBuf> {
    NODE_PLUGIN.get().cloned().or_else(|| cfg!(any(test, feature = "test-hooks")).then(|| PathBuf::from("/synthetic/plugin")))
}

/// The node plugin this daemon installed, else the copy in `home` an earlier start left.
pub fn node_in(home: &Path) -> Option<PathBuf> {
    node().or_else(|| Some(home.join(NODE)).filter(|plugin| plugin.is_dir()))
}

pub fn orchestrator_in(home: &Path) -> Option<PathBuf> {
    Some(home.join(ORCHESTRATOR)).filter(|plugin| plugin.join(".claude-plugin/plugin.json").is_file())
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

pub fn install_orchestrator(home: &Path, source: &Path, alondra: &Path) -> anyhow::Result<PathBuf> {
    let target = home.join(ORCHESTRATOR);
    sync(source, &target, &wrapper(home, alondra))?;
    Ok(target)
}

/// The CLI next to this daemon, by the path that survives an upgrade: Homebrew's `opt` link
/// instead of the versioned keg.
pub fn own_cli() -> Option<PathBuf> {
    Some(cli_beside(&std::env::current_exe().ok()?))
}

pub fn cli_beside(daemon: &Path) -> PathBuf {
    let cli = daemon.with_file_name("alondra");
    let rack = cli.ancestors().nth(3);
    let cellar = cli.ancestors().nth(4);
    match (rack.and_then(Path::file_name), cellar.and_then(Path::file_name), cli.ancestors().nth(5)) {
        (Some(rack), Some(cellar), Some(prefix)) if rack == "alondra" && cellar == "Cellar" => prefix.join("opt/alondra/bin/alondra"),
        _ => cli,
    }
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

/// Alondra's launch settings, with extra permissions when given.
pub fn launch_settings_with(permissions: Option<serde_json::Value>) -> serde_json::Value {
    let mut settings: serde_json::Value = serde_json::from_str(&alondra_core::launch_settings()).expect("launch settings are JSON");
    if let Some(permissions) = permissions {
        settings["permissions"] = permissions;
    }
    settings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_role_assets_are_available_without_a_claude_marketplace() {
        let scratch = crate::test_support::Directory::new();
        let source = scratch.join("prefix/libexec/alondra/plugin-orchestrator");
        std::fs::create_dir_all(source.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(source.join("skills/alondra-orchestrator")).unwrap();
        std::fs::write(source.join(".claude-plugin/plugin.json"), "{}").unwrap();
        std::fs::write(source.join("skills/alondra-orchestrator/SKILL.md"), "Role instructions").unwrap();
        let executable = scratch.join("prefix/bin/alondrad");
        assert_eq!(bundled_source_for(&executable, ORCHESTRATOR), Some(source.clone()));
        assert_eq!(bundled_source_for(&executable, "../secrets"), None);
        let home = scratch.join("home");
        assert_eq!(orchestrator_in(&home), None);
        let owned = install_orchestrator(&home, &source, &scratch.join("prefix/bin/alondra")).unwrap();
        assert_eq!(orchestrator_in(&home), Some(owned.clone()));
        assert_eq!(std::fs::read_to_string(owned.join("skills/alondra-orchestrator/SKILL.md")).unwrap(), "Role instructions");
        assert!(!home.join(".claude").exists());
    }

    #[test]
    fn the_plugins_are_found_through_the_registered_marketplace() {
        let scratch_directory = crate::test_support::Directory::new();
        let root = scratch_directory.parent().unwrap().join(format!("alondra-plugins-{}", std::process::id()));
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
    fn nodes_run_the_cli_beside_the_daemon_by_the_path_an_upgrade_keeps() {
        assert_eq!(cli_beside(Path::new("/opt/homebrew/Cellar/alondra/0.2.23/bin/alondrad")),
            Path::new("/opt/homebrew/opt/alondra/bin/alondra"));
        assert_eq!(cli_beside(Path::new("/tmp/alondra-sandbox/bin/alondrad")), Path::new("/tmp/alondra-sandbox/bin/alondra"),
            "a sandbox near the root of the disk");
        assert_eq!(cli_beside(Path::new("/Users/me/.local/bin/alondrad")), Path::new("/Users/me/.local/bin/alondra"));
    }

    #[test]
    fn an_upgrade_updates_the_daemons_plugin_copy_in_place_so_running_sessions_keep_their_mod() {
        let scratch_directory = crate::test_support::Directory::new();
        use std::os::unix::fs::MetadataExt;
        let root = scratch_directory.parent().unwrap().join(format!("alondra-plugin-copy-{}", crate::auth::random_hex(8)));
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
}
