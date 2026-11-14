//! Keeping the Claude Code the user installed at the release Alondra needs. The install is found
//! by where the `claude` the nodes run really lives, updated by its own updater, and trusted only
//! as far as `claude --version` says afterwards.
use crate::claude_code;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const UPDATE_TIMEOUT: Duration = Duration::from_secs(300);
const VERSION_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq)]
pub enum Install {
    /// `~/.local/share/claude/versions/X`: the updater installs next to the running release.
    Native,
    /// The updater replaces the binary in place.
    Npm,
    Cask { name: String, brew: PathBuf },
    /// A launcher of the user's own, or an install Alondra does not know.
    Other,
}

/// What kind of install `real`, the file the launcher of `claude` ends up at, belongs to.
pub fn detect(real: &Path) -> Install {
    let text = real.to_string_lossy();
    if text.contains("/.local/share/claude/versions/") {
        return Install::Native;
    }
    if text.contains("/node_modules/@anthropic-ai/claude-code") {
        return Install::Npm;
    }
    if let Some((prefix, rest)) = text.split_once("/Caskroom/")
        && let Some(name) = rest.split('/').next().filter(|name| name.starts_with("claude-code"))
    {
        return Install::Cask { name: name.to_owned(), brew: Path::new(prefix).join("bin/brew") };
    }
    Install::Other
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Current(String),
    Updated { from: String, to: String },
    /// A running process holds the binary the update would replace.
    Waiting,
    Failed(Failure),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Failure {
    NoAnswer,
    NotUpdatable(PathBuf),
    Refused(String),
    TimedOut,
    StillBelow { version: String, output: String },
}

impl Failure {
    pub fn english(&self) -> String {
        match self {
            Failure::NoAnswer => "Claude Code did not answer `claude --version`".into(),
            Failure::NotUpdatable(path) => format!(
                "{} is a launcher of your own or an install Alondra does not know: update it where you installed it", path.display()),
            Failure::Refused(output) => format!("the Claude Code updater refused: {}", claude_code::tail(output)),
            Failure::TimedOut => "the update did not finish within 5 minutes and the previous Claude Code is untouched".into(),
            Failure::StillBelow { version, output } => format!(
                "the update finished but Claude Code is still {version}: the release channel it follows has no {} yet, or a launcher of your own keeps the old version ({})",
                claude_code::MIN_TEXT, claude_code::tail(output)),
        }
    }
}

impl Outcome {
    pub fn english(&self) -> String {
        match self {
            Outcome::Current(version) => format!("Claude Code {version} is current"),
            Outcome::Updated { from, to } => format!("Claude Code updated from {from} to {to}"),
            Outcome::Waiting => "a Claude session runs the Claude Code the update would replace: Alondra updates it as soon as none does".into(),
            Outcome::Failed(failure) => failure.english(),
        }
    }
}

/// How the `claude` the nodes run is reached: the same program and environment a node starts with.
pub trait Host {
    fn claude(&self, args: &[&str], timeout: Duration) -> Option<(bool, String)>;
    /// Where the PATH finds that `claude`.
    fn launcher(&self) -> Option<PathBuf>;
    fn brew(&self, brew: &Path, args: &[&str], timeout: Duration) -> Option<(bool, String)>;
    /// Whether some process runs `binary`; when that cannot be told, it is in use.
    fn in_use(&self, binary: &Path) -> bool;
}

fn release(text: &str) -> String {
    text.split_whitespace().next().unwrap_or_default().to_owned()
}

/// Brings Claude Code to the release Alondra needs, when it is below it.
pub fn update(host: &impl Host) -> Outcome {
    let Some((true, text)) = host.claude(&["--version"], VERSION_TIMEOUT) else { return Outcome::Failed(Failure::NoAnswer) };
    let from = release(&text);
    if claude_code::compatible(&from) {
        return Outcome::Current(from);
    }
    let Some(launcher) = host.launcher() else { return Outcome::Failed(Failure::NoAnswer) };
    let real = std::fs::canonicalize(&launcher).unwrap_or_else(|_| launcher.clone());
    let install = detect(&real);
    if matches!(install, Install::Npm | Install::Cask { .. }) && host.in_use(&real) {
        return Outcome::Waiting;
    }
    let ran = match &install {
        Install::Native | Install::Npm => host.claude(&["update"], UPDATE_TIMEOUT),
        Install::Cask { name, brew } => host.brew(brew, &["upgrade", "--cask", name], UPDATE_TIMEOUT),
        Install::Other => return Outcome::Failed(Failure::NotUpdatable(launcher)),
    };
    let output = match ran {
        None => return Outcome::Failed(Failure::TimedOut),
        Some((false, output)) => return Outcome::Failed(Failure::Refused(output)),
        Some((true, output)) => output,
    };
    match host.claude(&["--version"], VERSION_TIMEOUT) {
        Some((true, text)) if claude_code::compatible(&text) => Outcome::Updated { from, to: release(&text) },
        Some((true, text)) => Outcome::Failed(Failure::StillBelow { version: release(&text), output }),
        _ => Outcome::Failed(Failure::NoAnswer),
    }
}

pub fn find_on_path(path: &str, name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    path.split(':').filter(|folder| Path::new(folder).is_absolute())
        .map(|folder| Path::new(folder).join(name))
        .find(|candidate| std::fs::metadata(candidate).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0))
}

/// The output of `command`, or `None` when it did not run or did not finish in `timeout`.
pub fn run(mut command: Command, timeout: Duration) -> Option<(bool, String)> {
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()?;
    let readers: Vec<_> = [child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>)]
        .into_iter().flatten()
        .map(|mut stream| std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stream.read_to_end(&mut bytes);
            String::from_utf8_lossy(&bytes).into_owned()
        }))
        .collect();
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let text: String = readers.into_iter().filter_map(|reader| reader.join().ok()).collect();
    Some((status.success(), text))
}

/// Whether `ps` lists `binary` as the program of a process.
pub fn listed(ps: &str, binary: &Path) -> bool {
    ps.lines().any(|line| Path::new(line.trim()) == binary)
}

pub fn processes_run(binary: &Path) -> bool {
    let mut command = Command::new("/bin/ps");
    command.args(["-axo", "comm="]);
    match run(command, Duration::from_secs(10)) {
        Some((true, ps)) => listed(&ps, binary),
        _ => true,
    }
}

/// The real thing: the `claude` found on a PATH, run with the environment given (cleared to it) or
/// the caller's own.
pub struct ProcessHost {
    pub launcher: PathBuf,
    pub environment: Option<Vec<(String, String)>>,
}

impl ProcessHost {
    fn command(&self, program: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(program);
        command.args(args);
        if let Some(environment) = &self.environment {
            command.env_clear().envs(environment.iter().map(|(key, value)| (key, value)));
        }
        command
    }
}

impl Host for ProcessHost {
    fn claude(&self, args: &[&str], timeout: Duration) -> Option<(bool, String)> {
        run(self.command(&self.launcher, args), timeout)
    }

    fn launcher(&self) -> Option<PathBuf> {
        Some(self.launcher.clone())
    }

    fn brew(&self, brew: &Path, args: &[&str], timeout: Duration) -> Option<(bool, String)> {
        run(self.command(brew, args), timeout)
    }

    fn in_use(&self, binary: &Path) -> bool {
        processes_run(binary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    /// A fake `claude` in the layout of a native install under a temporary HOME. It reads its
    /// release from `$HOME/release`, and `claude update` runs `how`.
    struct Fixture {
        home: tempfile::TempDir,
    }

    impl Fixture {
        fn new(release: &str, how: &str) -> Self {
            let home = tempfile::tempdir().unwrap();
            let binary = home.path().join(".local/share/claude/versions/fake");
            std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
            std::fs::create_dir_all(home.path().join(".local/bin")).unwrap();
            std::fs::write(home.path().join("release"), release).unwrap();
            let script = format!("#!/bin/sh\ncase \"$1\" in\n--version) echo \"$(cat \"$HOME/release\") (Claude Code)\";;\nupdate) {how};;\nesac\n");
            std::fs::write(&binary, script).unwrap();
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
            symlink(&binary, home.path().join(".local/bin/claude")).unwrap();
            Self { home }
        }

        fn launcher(&self) -> PathBuf {
            self.home.path().join(".local/bin/claude")
        }

        fn host(&self) -> ProcessHost {
            ProcessHost {
                launcher: self.launcher(),
                environment: Some(vec![("HOME".into(), self.home.path().display().to_string()), ("PATH".into(), "/usr/bin:/bin".into())]),
            }
        }
    }

    #[test]
    fn a_native_install_below_the_minimum_is_brought_to_it() {
        let fixture = Fixture::new("2.1.291", "echo 2.1.322 > \"$HOME/release\"; echo installed");
        let host = fixture.host();
        assert_eq!(update(&host), Outcome::Updated { from: "2.1.291".into(), to: "2.1.322".into() });
        assert_eq!(update(&host), Outcome::Current("2.1.322".into()));
    }

    #[test]
    fn a_release_at_the_minimum_is_left_alone() {
        let fixture = Fixture::new("2.1.321", "echo must not run >&2; exit 9");
        assert_eq!(update(&fixture.host()), Outcome::Current("2.1.321".into()));
    }

    #[test]
    fn a_refusing_updater_says_why_in_its_own_words() {
        let fixture = Fixture::new("2.1.291", "echo 'Updates are disabled by your organization (DISABLE_UPDATES)' >&2; exit 1");
        let Outcome::Failed(failure) = update(&fixture.host()) else { panic!("refusal expected") };
        assert!(failure.english().contains("DISABLE_UPDATES"), "{}", failure.english());
    }

    #[test]
    fn an_update_that_leaves_the_release_below_says_so_instead_of_trusting_the_exit_code() {
        let fixture = Fixture::new("2.1.291", "echo 'Claude Code is up to date (2.1.291)'");
        let Outcome::Failed(Failure::StillBelow { version, .. }) = update(&fixture.host()) else { panic!("still below expected") };
        assert_eq!(version, "2.1.291");
    }

    #[test]
    fn a_command_that_never_finishes_is_cut_at_its_timeout() {
        let started = Instant::now();
        let mut sleep = Command::new("/bin/sleep");
        sleep.arg("30");
        assert_eq!(run(sleep, Duration::from_millis(300)), None);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_launcher_of_the_users_own_is_not_touched() {
        let home = tempfile::tempdir().unwrap();
        let launcher = std::fs::canonicalize(home.path()).unwrap().join("claude");
        std::fs::write(&launcher, "#!/bin/sh\necho '2.1.291 (Claude Code)'\n").unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        let host = ProcessHost { launcher: launcher.clone(), environment: None };
        assert_eq!(update(&host), Outcome::Failed(Failure::NotUpdatable(launcher)));
    }

    struct Scripted {
        running: bool,
        calls: std::cell::RefCell<Vec<String>>,
        versions: std::cell::RefCell<Vec<&'static str>>,
        launcher: PathBuf,
    }

    impl Host for Scripted {
        fn claude(&self, args: &[&str], _: Duration) -> Option<(bool, String)> {
            self.calls.borrow_mut().push(format!("claude {}", args.join(" ")));
            match args {
                ["--version"] => Some((true, format!("{} (Claude Code)", self.versions.borrow_mut().remove(0)))),
                _ => Some((true, "done".into())),
            }
        }
        fn launcher(&self) -> Option<PathBuf> {
            Some(self.launcher.clone())
        }
        fn brew(&self, brew: &Path, args: &[&str], _: Duration) -> Option<(bool, String)> {
            self.calls.borrow_mut().push(format!("{} {}", brew.display(), args.join(" ")));
            Some((true, "done".into()))
        }
        fn in_use(&self, _: &Path) -> bool {
            self.running
        }
    }

    fn scripted(launcher: &Path, running: bool, versions: Vec<&'static str>) -> Scripted {
        Scripted { running, calls: Default::default(), versions: versions.into(), launcher: launcher.to_path_buf() }
    }

    #[test]
    fn an_in_place_install_waits_while_a_process_runs_it_and_updates_once_none_does() {
        let home = tempfile::tempdir().unwrap();
        let binary = home.path().join("lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, "").unwrap();
        let launcher = home.path().join("claude");
        symlink(&binary, &launcher).unwrap();

        let busy = scripted(&launcher, true, vec!["2.1.291"]);
        assert_eq!(update(&busy), Outcome::Waiting);
        assert_eq!(busy.calls.borrow().as_slice(), ["claude --version"], "nothing was replaced");

        let idle = scripted(&launcher, false, vec!["2.1.291", "2.1.322"]);
        assert!(matches!(update(&idle), Outcome::Updated { .. }));
        assert_eq!(idle.calls.borrow().as_slice(), ["claude --version", "claude update", "claude --version"]);
    }

    #[test]
    fn a_homebrew_cask_is_upgraded_with_brew_and_the_cask_it_is() {
        let home = tempfile::tempdir().unwrap();
        let binary = home.path().join("opt/homebrew/Caskroom/claude-code@latest/2.1.291/claude");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, "").unwrap();
        let launcher = home.path().join("claude");
        symlink(&binary, &launcher).unwrap();
        let host = scripted(&launcher, false, vec!["2.1.291", "2.1.322"]);
        assert!(matches!(update(&host), Outcome::Updated { .. }));
        let prefix = std::fs::canonicalize(home.path()).unwrap().join("opt/homebrew");
        assert_eq!(host.calls.borrow()[1], format!("{}/bin/brew upgrade --cask claude-code@latest", prefix.display()));
    }

    #[test]
    fn the_install_is_told_by_where_the_launcher_ends_up() {
        assert_eq!(detect(Path::new("/Users/ana/.local/share/claude/versions/2.1.291")), Install::Native);
        assert_eq!(detect(Path::new("/opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe")), Install::Npm);
        assert_eq!(detect(Path::new("/usr/local/Caskroom/claude-code/2.1.291/claude")),
            Install::Cask { name: "claude-code".into(), brew: PathBuf::from("/usr/local/bin/brew") });
        assert_eq!(detect(Path::new("/usr/local/Caskroom/other/1/claude")), Install::Other);
        assert_eq!(detect(Path::new("/Users/ana/bin/claude")), Install::Other);
    }

    #[test]
    fn a_process_listing_names_a_binary_only_by_its_whole_path() {
        let ps = "/sbin/launchd\n/Users/ana/.local/share/claude/versions/2.1.291\n  /usr/bin/ssh\n";
        assert!(listed(ps, Path::new("/Users/ana/.local/share/claude/versions/2.1.291")));
        assert!(!listed(ps, Path::new("/Users/ana/.local/share/claude/versions/2.1.29")));
    }
}
