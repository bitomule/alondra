//! The launcher launchd runs for alondrad. macOS keeps a launchd agent's Local Network permission
//! for the signing identifier and the exact path of the program it ran; Homebrew puts every
//! release in a new Cellar directory, so the launcher is copied to a fixed path under
//! `ALONDRA_HOME` and only replaced when its own version changes.
use std::path::{Path, PathBuf};
use std::process::Command;

pub const NAME: &str = "alondrad-launcher";

/// Where launchd runs it from: the same path for every release.
pub fn installed_path(alondra_home: &Path) -> PathBuf {
    alondra_home.join("bin").join(NAME)
}

/// The one a release ships, beside its alondrad.
pub fn bundled_path(daemon: &Path) -> PathBuf {
    daemon.with_file_name(NAME)
}

/// What `--version` prints, or `None` for a missing or broken launcher.
pub fn version(launcher: &Path) -> Option<String> {
    let out = Command::new(launcher).arg("--version").output().ok().filter(|out| out.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    text.starts_with(NAME).then_some(text)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Installed {
    /// The fixed path already holds this version.
    Current,
    /// The fixed path now holds the bundled launcher.
    Copied,
    /// This build ships no launcher.
    NotBundled,
}

/// Puts the bundled launcher at `installed` unless the same version is already there. The copy
/// lands beside it and is renamed over it, so a running launcher keeps its file.
pub fn install(bundled: &Path, installed: &Path) -> std::io::Result<Installed> {
    let Some(wanted) = version(bundled) else { return Ok(Installed::NotBundled) };
    if version(installed).as_deref() == Some(wanted.as_str()) {
        return Ok(Installed::Current);
    }
    let dir = installed.parent().ok_or_else(|| std::io::Error::other("the launcher path has no folder"))?;
    std::fs::create_dir_all(dir)?;
    let staged = dir.join(format!(".{NAME}.{}", std::process::id()));
    let copied = std::fs::copy(bundled, &staged).and_then(|_| std::fs::rename(&staged, installed));
    if copied.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    copied.map(|()| Installed::Copied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn launcher(dir: &Path, name: &str, version: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\necho '{NAME} {version}'\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn the_launcher_is_copied_once_and_kept_while_its_version_does_not_change() {
        let root = crate::test_support::Directory::new();
        let installed = installed_path(&root.join("home"));
        let first = launcher(&root.join("Cellar/1/bin"), NAME, "1");
        assert_eq!(install(&first, &installed).unwrap(), Installed::Copied);
        assert_eq!(version(&installed).as_deref(), Some("alondrad-launcher 1"));
        let before = std::fs::metadata(&installed).unwrap().modified().unwrap();

        let same = launcher(&root.join("Cellar/2/bin"), NAME, "1");
        assert_eq!(install(&same, &installed).unwrap(), Installed::Current);
        assert_eq!(std::fs::metadata(&installed).unwrap().modified().unwrap(), before);

        let newer = launcher(&root.join("Cellar/3/bin"), NAME, "2");
        assert_eq!(install(&newer, &installed).unwrap(), Installed::Copied);
        assert_eq!(version(&installed).as_deref(), Some("alondrad-launcher 2"));
        assert_eq!(std::fs::read_dir(installed.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn a_build_without_a_launcher_leaves_the_fixed_path_alone() {
        let root = crate::test_support::Directory::new();
        let installed = installed_path(&root.join("home"));
        assert_eq!(install(&root.join("bin").join(NAME), &installed).unwrap(), Installed::NotBundled);
        assert!(!installed.exists());
        let impostor = launcher(&root.join("bin"), "other", "1");
        std::fs::write(&impostor, "#!/bin/sh\necho something-else 1\n").unwrap();
        assert_eq!(version(&impostor), None);
    }

    #[test]
    fn the_bundled_launcher_sits_beside_the_daemon() {
        assert_eq!(bundled_path(Path::new("/opt/homebrew/opt/alondra/bin/alondrad")), Path::new("/opt/homebrew/opt/alondra/bin/alondrad-launcher"));
        assert_eq!(installed_path(Path::new("/Users/ana/.alondra")), Path::new("/Users/ana/.alondra/bin/alondrad-launcher"));
    }
}
