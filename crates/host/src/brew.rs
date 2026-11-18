//! The Homebrew install Alondra runs from, and the CLI beside its daemon.
use std::path::{Path, PathBuf};

/// The Homebrew prefix of a binary Homebrew installed: `<prefix>/Cellar/alondra/<version>/bin/…`,
/// whatever link (`<prefix>/opt/alondra/bin`, `<prefix>/bin`) it was reached through.
pub fn brew_prefix(binary: &Path) -> Option<PathBuf> {
    let real = binary.canonicalize().ok()?;
    let parts: Vec<_> = real.components().collect();
    let cellar = parts.windows(2).position(|w| w[0].as_os_str() == "Cellar" && w[1].as_os_str() == "alondra")?;
    Some(parts[..cellar].iter().collect())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodes_run_the_cli_beside_the_daemon_by_the_path_an_upgrade_keeps() {
        assert_eq!(cli_beside(Path::new("/opt/homebrew/Cellar/alondra/0.2.23/bin/alondrad")),
            Path::new("/opt/homebrew/opt/alondra/bin/alondra"));
        assert_eq!(cli_beside(Path::new("/tmp/alondra-sandbox/bin/alondrad")), Path::new("/tmp/alondra-sandbox/bin/alondra"),
            "a sandbox near the root of the disk");
        assert_eq!(cli_beside(Path::new("/Users/me/.local/bin/alondrad")), Path::new("/Users/me/.local/bin/alondra"));
    }

    #[test]
    fn a_homebrew_binary_is_recognised_through_its_links() {
        let scratch_directory = crate::test_support::Directory::new();
        let root = scratch_directory.parent().unwrap().join(format!("alondra-brew-{}-{}", std::process::id(), alondra_foundation::clock::now_millis()));
        let keg = root.join("Cellar/alondra/0.1.12/bin");
        std::fs::create_dir_all(&keg).unwrap();
        std::fs::write(keg.join("alondra"), "").unwrap();
        std::fs::create_dir_all(root.join("opt")).unwrap();
        std::os::unix::fs::symlink("../Cellar/alondra/0.1.12", root.join("opt/alondra")).unwrap();
        let prefix = root.canonicalize().unwrap();
        assert_eq!(brew_prefix(&root.join("opt/alondra/bin/alondra")), Some(prefix.clone()));
        assert_eq!(brew_prefix(&keg.join("alondra")), Some(prefix));
        let dev = root.join("bin");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join("alondra"), "").unwrap();
        assert_eq!(brew_prefix(&dev.join("alondra")), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
