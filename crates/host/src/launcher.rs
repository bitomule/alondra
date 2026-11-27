//! The launcher launchd runs for alondrad. macOS keeps a launchd agent's Local Network permission
//! for the signing identifier and the exact path of the program it ran; Homebrew puts every
//! release in a new Cellar directory, so the launcher is copied to a fixed path under
//! `ALONDRA_HOME` and only replaced when its own version changes.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
    version_by(launcher, &run_version)
}

/// Runs the launcher with `--version`; `None` when it cannot be started.
fn run_version(launcher: &Path) -> Option<Output> {
    Command::new(launcher).arg("--version").output().ok()
}

/// What `ask` says the launcher answered to `--version`, held to the launcher's own name. Tests
/// hand it an `ask` that reads a file, so they wait on no program whose time they do not control.
fn version_by(launcher: &Path, ask: &dyn Fn(&Path) -> Option<Output>) -> Option<String> {
    let out = ask(launcher).filter(|out| out.status.success())?;
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
    install_by(bundled, installed, &run_version)
}

fn install_by(bundled: &Path, installed: &Path, ask: &dyn Fn(&Path) -> Option<Output>) -> std::io::Result<Installed> {
    let Some(wanted) = version_by(bundled, ask) else { return Ok(Installed::NotBundled) };
    if version_by(installed, ask).as_deref() == Some(wanted.as_str()) {
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
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::ExitStatus;

    /// A launcher that is a plain file holding what it would print: nothing here starts a program.
    fn launcher(dir: &Path, name: &str, prints: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, prints).unwrap();
        path
    }

    fn exits(code: i32, stdout: Vec<u8>) -> Output {
        Output { status: ExitStatus::from_raw(code << 8), stdout, stderr: Vec::new() }
    }

    /// Prints the file's text and exits 0; a missing file answers nothing, as a launcher that
    /// cannot be started does.
    fn prints_its_file(path: &Path) -> Option<Output> {
        std::fs::read(path).ok().map(|text| exits(0, text))
    }

    fn installs(bundled: &Path, installed: &Path) -> Installed {
        install_by(bundled, installed, &prints_its_file).unwrap()
    }

    #[test]
    fn the_launcher_is_copied_once_and_kept_while_its_version_does_not_change() {
        let root = crate::test_support::Directory::new();
        let installed = installed_path(&root.join("home"));
        let first = launcher(&root.join("Cellar/1/bin"), NAME, &format!("{NAME} 1\n"));
        assert_eq!(installs(&first, &installed), Installed::Copied);
        assert_eq!(version_by(&installed, &prints_its_file).as_deref(), Some("alondrad-launcher 1"));
        let before = std::fs::metadata(&installed).unwrap().modified().unwrap();

        let same = launcher(&root.join("Cellar/2/bin"), NAME, &format!("{NAME} 1\n"));
        assert_eq!(installs(&same, &installed), Installed::Current);
        assert_eq!(std::fs::metadata(&installed).unwrap().modified().unwrap(), before);

        let newer = launcher(&root.join("Cellar/3/bin"), NAME, &format!("{NAME} 2\n"));
        assert_eq!(installs(&newer, &installed), Installed::Copied);
        assert_eq!(version_by(&installed, &prints_its_file).as_deref(), Some("alondrad-launcher 2"));
        assert_eq!(std::fs::read_dir(installed.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn a_build_without_a_launcher_leaves_the_fixed_path_alone() {
        let root = crate::test_support::Directory::new();
        let installed = installed_path(&root.join("home"));
        assert_eq!(installs(&root.join("bin").join(NAME), &installed), Installed::NotBundled);
        assert!(!installed.exists());
    }

    #[test]
    fn only_a_successful_answer_that_starts_with_the_launchers_name_is_a_version() {
        let impostor = Path::new("/Users/ana/bin/launcher");
        let says = |status: i32, text: &str| version_by(impostor, &|_| Some(exits(status, text.as_bytes().to_vec())));
        assert_eq!(says(0, "alondrad-launcher 7\n").as_deref(), Some("alondrad-launcher 7"));
        assert_eq!(says(0, "something-else 1\n"), None);
        assert_eq!(says(1, "alondrad-launcher 7\n"), None);
        assert_eq!(version_by(impostor, &|_| None), None);
    }

    #[test]
    fn a_launcher_that_cannot_be_started_has_no_version() {
        let root = crate::test_support::Directory::new();
        assert_eq!(version(&root.join("missing").join(NAME)), None);
    }

    #[test]
    fn the_bundled_launcher_sits_beside_the_daemon() {
        assert_eq!(bundled_path(Path::new("/opt/homebrew/opt/alondra/bin/alondrad")), Path::new("/opt/homebrew/opt/alondra/bin/alondrad-launcher"));
        assert_eq!(installed_path(Path::new("/Users/ana/.alondra")), Path::new("/Users/ana/.alondra/bin/alondrad-launcher"));
    }
}
