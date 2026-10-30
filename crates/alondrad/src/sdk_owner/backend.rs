//! The SDK backend names the host, the native Claude, each with its SHA256, and the exact
//! environments they get. A release runs the host it installed beside its binaries with the
//! Claude Code the user installed; a configuration file in the home replaces both (a sandbox, the
//! upgrade rehearsal). Outside a sandbox the native Claude runs as the user's own: in their real
//! HOME and with the Claude profile the daemon was set up with (`claude_account::configured_profile`),
//! so it signs in as the user does and as setup and doctor check; Seatbelt confinement stays
//! optional.
use crate::locks::Poisonable as _;
use alondra_core::accounts::AccountId;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

pub(crate) const FILE: &str = "sdk-backend.json";
/// The release's host: one executable, Node and the bundle together, in `libexec/alondra/sdk`.
pub(crate) const HOST: &str = "alondra-host";
/// Setting either selects another credential namespace than the user's default profile, even when
/// it names the default folder: they come only from the profile the daemon was set up with.
const DEFAULT_PROFILE: [&str; 2] = ["CLAUDE_CONFIG_DIR", "CLAUDE_HOME"];
/// The host's socket is `<state root>/<execution UUID>/s`, and a Unix socket path holds 103 bytes.
const SOCKET_PATH_LIMIT: usize = 103;
const EXECUTION_FOLDER: usize = "/00000000-0000-0000-0000-000000000000/s".len();
/// What the daemon's environment carries for Alondra, or inherited from a Claude session it was
/// started in: never the user's Claude's. The SDK sets its own session variables.
const PRIVATE: [&str; 5] = ["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_SSE_PORT", "CLAUDE_AGENT_SDK_VERSION", "CLAUDE_CODE_SDK_READS_SESSION_STATE"];
const VERSION_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SdkBackend {
    #[serde(default, skip_serializing_if = "is_claude")]
    pub(crate) provider: alondra_core::accounts::Provider,
    #[serde(skip)]
    pub(crate) origin: Option<Box<SdkBackend>>,
    pub(crate) node: PathBuf,
    pub(crate) node_sha256: String,
    pub(crate) host: PathBuf,
    pub(crate) host_sha256: String,
    pub(crate) native: PathBuf,
    pub(crate) native_sha256: String,
    pub(crate) host_environment: BTreeMap<String, String>,
    pub(crate) native_environment: BTreeMap<String, String>,
    /// A Seatbelt profile the whole host runs under, Claude included.
    #[serde(default)]
    pub(crate) confinement: Option<PathBuf>,
    /// Short enough for the host's Unix socket: each execution gets a private folder in it.
    pub(crate) state_root: PathBuf,
    pub(crate) io_timeout_seconds: u64,
    /// The Claude Code the user installed, which updates itself: each host start takes it as it is
    /// then.
    #[serde(skip)]
    pub(crate) installed_native: bool,
}

/// The backend a daemon runs: the configuration in `home` when there is one, else the host the
/// release of `daemon` installed with the user's own Claude Code, found on `environment`'s PATH.
pub(crate) fn resolve(home: &Path, sandbox: bool, user_home: &Path, daemon: &Path, environment: &BTreeMap<String, String>) -> Result<SdkBackend> {
    if let Some(backend) = load(home, sandbox, user_home)? {
        return Ok(backend);
    }
    ensure!(!sandbox, "a sandbox daemon runs only the SDK backend its {FILE} names");
    installed(home, user_home, daemon, environment)
}

/// The host beside `daemon`: `<keg>/libexec/alondra/sdk/alondra-host`, through Homebrew's `opt`
/// link, so the next start after an upgrade verifies the new release's host.
pub(crate) fn installed_host(daemon: &Path) -> PathBuf {
    let cli = crate::plugins::cli_beside(daemon);
    let bin = cli.parent().unwrap_or(Path::new("/"));
    bin.parent().unwrap_or(bin).join("libexec/alondra/sdk").join(HOST)
}

fn installed(home: &Path, user_home: &Path, daemon: &Path, environment: &BTreeMap<String, String>) -> Result<SdkBackend> {
    let host = installed_host(daemon);
    ensure!(host.is_file(), "this release's Agent SDK host is missing at {}: reinstall Alondra (`brew reinstall alondra`)", host.display());
    let mut native_environment: BTreeMap<String, String> = environment.iter()
        .filter(|(key, _)| !DEFAULT_PROFILE.contains(&key.as_str()) && !PRIVATE.contains(&key.as_str()) && !key.starts_with("ALONDRA_"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    native_environment.insert("HOME".into(), user_home.display().to_string());
    if let Some(profile) = alondra_core::claude_account::configured_profile(&|key: &str| environment.get(key).cloned()) {
        for key in DEFAULT_PROFILE {
            native_environment.insert(key.into(), profile.display().to_string());
        }
    }
    let state_root = home.join("sdk");
    ensure!(state_root.as_os_str().len() + EXECUTION_FOLDER <= SOCKET_PATH_LIMIT,
        "{} is too long a path for the Agent SDK host's socket: move ALONDRA_HOME to a shorter one", state_root.display());
    Ok(SdkBackend {
        provider: alondra_core::accounts::Provider::Claude, origin: None,
        node: host.clone(), node_sha256: String::new(), host, host_sha256: String::new(),
        native: PathBuf::new(), native_sha256: String::new(),
        host_environment: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
        native_environment,
        confinement: None, state_root, io_timeout_seconds: 120, installed_native: true,
    })
}

fn on_path(path: &str, name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    path.split(':').filter(|folder| Path::new(folder).is_absolute())
        .map(|folder| Path::new(folder).join(name))
        .find(|candidate| std::fs::metadata(candidate).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0))
}

/// The configuration in `home`, checked: `None` without one. `user_home` is the HOME the native
/// Claude of a daemon outside a sandbox runs in, whatever the configuration leaves out.
pub(crate) fn load(home: &Path, sandbox: bool, user_home: &Path) -> Result<Option<SdkBackend>> {
    let path = home.join(FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let mut backend: SdkBackend = serde_json::from_slice(&bytes).with_context(|| format!("reading {}", path.display()))?;
    for (file, sha256) in [(&backend.node, &backend.node_sha256), (&backend.host, &backend.host_sha256), (&backend.native, &backend.native_sha256)] {
        ensure!(file.is_absolute(), "{} must be an absolute path", file.display());
        ensure!(digest(file)? == *sha256, "{} does not have the SHA256 {sha256} the SDK backend names", file.display());
    }
    for profile in DEFAULT_PROFILE {
        ensure!(!backend.native_environment.contains_key(profile),
            "the SDK backend's native environment sets {profile}: Claude Code runs with the user's default profile, which leaves it unset");
    }
    if !sandbox {
        let home = backend.native_environment.entry("HOME".into()).or_insert_with(|| user_home.display().to_string());
        ensure!(Path::new(home) == user_home,
            "the SDK backend's native environment sets HOME to {home}: outside a sandbox Claude Code runs in the user's HOME, {}", user_home.display());
    }
    if let Some(confinement) = &backend.confinement {
        ensure!(confinement.is_absolute(), "the SDK backend's Seatbelt profile must be an absolute path");
    }
    ensure!(backend.state_root.is_absolute(), "the SDK state root must be an absolute path");
    ensure!((1..=300).contains(&backend.io_timeout_seconds), "the SDK host's I/O timeout is 1 to 300 seconds");
    Ok(Some(backend))
}

impl SdkBackend {
    /// The SHA256 of the configuration as it runs: every artifact's digest, both environments, the
    /// confinement and the host's limits. The provider binding is named by it, so executions keep
    /// the configuration they were admitted under and a changed one binds the next ones.
    pub(crate) fn configuration_digest(&self) -> String {
        use sha2::Digest;
        let canonical = serde_json::to_vec(self).expect("the SDK backend serializes");
        sha2::Sha256::digest(canonical).iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// The backend one host starts with. The user's Claude Code is found on the PATH it runs with
    /// and taken as it is now, by its SHA256, and refused unless it is a release the SDK runs; a
    /// configuration file's native Claude is pinned and was checked when it was read.
    pub(crate) fn for_start(&self) -> Result<SdkBackend> {
        if !self.installed_native {
            return Ok(self.clone());
        }
        let mut backend = self.clone();
        backend.host_sha256 = installed_digest(&backend.host)?;
        backend.node_sha256 = backend.host_sha256.clone();
        backend.native = on_path(backend.native_environment.get("PATH").map(String::as_str).unwrap_or_default(), if backend.provider == alondra_core::accounts::Provider::Claude { "claude" } else { "codex" })
            .with_context(|| if backend.provider == alondra_core::accounts::Provider::Claude {
                "Claude Code (`claude`) is not on this daemon's PATH: install it, then run `alondra doctor --fix`"
            } else { "Codex (`codex`) is not on this daemon's PATH: install it, then run `alondra doctor --fix`" })?;
        backend.native_sha256 = digest(&backend.native)?;
        match backend.provider {
            alondra_core::accounts::Provider::Claude => { compatible(&backend.native, &backend.native_sha256)?; }
            alondra_core::accounts::Provider::Codex => { compatible_codex(&backend.native, &backend.native_sha256)?; }
        }
        Ok(backend)
    }
}

impl SdkBackend {
    /// The backend an execution on `account` runs with. The default account runs with the profile
    /// the daemon was set up with, none for Claude's default one; any other runs Claude with its folder inside
    /// `home`, which Alondra made when the user added it. An account `registered` does not name,
    /// or whose folder is missing or a link, is refused: no other folder is ever handed to Claude.
    pub(crate) fn for_account(&self, home: &Path, account: &AccountId, registered: &[AccountId]) -> Result<SdkBackend> {
        if account.is_default() {
            return Ok(self.clone());
        }
        ensure!(registered.contains(account), "the Claude account {account} is not one of the accounts in options.toml");
        let folder = alondra_core::accounts::folder(home, account).context("the account has no folder")?;
        let meta = std::fs::symlink_metadata(&folder).with_context(|| format!("the folder of the Claude account {account} is missing"))?;
        ensure!(meta.is_dir(), "the folder of the Claude account {account} is not a folder Alondra made");
        let mut backend = self.clone();
        for key in DEFAULT_PROFILE {
            backend.native_environment.insert(key.into(), folder.display().to_string());
        }
        Ok(backend)
    }
}

/// A holder that panics leaves it whole, so it is recovered: a memo, read or filled one entry at a
/// time.
static CHECKED: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// Uses the same stable file identity as launch and adoption verification.
fn installed_digest(host: &Path) -> Result<String> {
    digest(host)
}

/// The release `claude --version` names, once per binary: a Claude Code older than the one the
/// pinned SDK needs is refused with what to do.
fn compatible(native: &Path, sha256: &str) -> Result<String> {
    if let Some(version) = CHECKED.lock_or_recover("the checked SDK releases").as_ref().and_then(|known| known.get(sha256).cloned()) {
        return Ok(version);
    }
    let text = version_of(native)?;
    if !alondra_core::claude_code::compatible(&text) {
        bail!("Claude Code {} is older than {}, the release Alondra needs: update it with `claude update`",
            text.trim(), alondra_core::claude_code::MIN_TEXT);
    }
    CHECKED.lock_or_recover("the checked SDK releases").get_or_insert_default().insert(sha256.to_owned(), text.clone());
    Ok(text)
}

fn version_of(native: &Path) -> Result<String> {
    use std::io::Read;
    let mut child = std::process::Command::new(native).arg("--version")
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null())
        .spawn().with_context(|| format!("running {} --version", native.display()))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        let _ = sender.send(text);
    });
    let Ok(text) = receiver.recv_timeout(VERSION_TIMEOUT) else {
        let _ = child.kill();
        let _ = child.wait();
        bail!("Claude Code (`{} --version`) did not answer within {} s", native.display(), VERSION_TIMEOUT.as_secs());
    };
    let status = child.wait()?;
    ensure!(status.success(), "Claude Code (`{} --version`) failed", native.display());
    Ok(text)
}

pub(crate) fn digest(file: &Path) -> Result<String> {
    alondra_agent_runtime::artifact_digest::digest(file)
        .with_context(|| format!("hashing {}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_host_digest_detects_changes_with_preserved_size_and_mtime() {
        let root = crate::test_support::Directory::new();
        std::fs::create_dir(&root).unwrap();
        let host = root.join("host");
        std::fs::write(&host, b"good").unwrap();
        let expected = installed_digest(&host).unwrap();
        let modified = std::fs::metadata(&host).unwrap().modified().unwrap();
        std::fs::write(&host, b"evil").unwrap();
        std::fs::File::options().write(true).open(&host).unwrap().set_modified(modified).unwrap();
        assert_ne!(installed_digest(&host).unwrap(), expected);
    }

    fn configuration(home: &Path, native_environment: serde_json::Value) -> serde_json::Value {
        let file = home.join("artifact");
        std::fs::write(&file, b"artifact").unwrap();
        let sha256 = digest(&file).unwrap();
        serde_json::json!({
            "node": file, "node_sha256": sha256, "host": file, "host_sha256": sha256, "native": file, "native_sha256": sha256,
            "host_environment": {}, "native_environment": native_environment,
            "state_root": home.join("state"), "io_timeout_seconds": 30,
        })
    }

    #[test]
    fn a_daemon_without_a_configuration_builds_no_sdk_backend() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        assert!(load(&home, false, &home.join("user")).unwrap().is_none(), "no configuration, no SDK backend");
        assert!(load(&home, true, &home.join("user")).unwrap().is_none(), "a sandbox without the configuration has none either");
    }

    #[test]
    fn a_daemon_outside_a_sandbox_runs_claude_in_the_users_home_with_or_without_seatbelt() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        let user = home.join("user");
        std::fs::write(home.join(FILE), configuration(&home, serde_json::json!({"PATH": "/usr/bin"})).to_string()).unwrap();
        let backend = load(&home, false, &user).unwrap().expect("a production daemon admits the configuration");
        assert_eq!(backend.native_environment.get("HOME").map(String::as_str), Some(user.to_str().unwrap()), "Claude runs in the user's real HOME");
        assert!(backend.confinement.is_none(), "Seatbelt is optional");
        let mut confined = configuration(&home, serde_json::json!({"HOME": user}));
        confined["confinement"] = serde_json::json!(home.join("profile.sb"));
        std::fs::write(home.join(FILE), confined.to_string()).unwrap();
        assert!(load(&home, false, &user).unwrap().unwrap().confinement.is_some(), "and allowed");
        std::fs::write(home.join(FILE), configuration(&home, serde_json::json!({"HOME": home.join("synthetic")})).to_string()).unwrap();
        let refused = load(&home, false, &user).unwrap_err().to_string();
        assert!(refused.contains("runs in the user's HOME"), "{refused}");
        assert!(load(&home, true, &user).unwrap().is_some(), "a sandbox daemon keeps its synthetic HOME");
    }

    #[test]
    fn the_configuration_digest_changes_with_what_runs() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        let user = home.join("user");
        std::fs::write(home.join(FILE), configuration(&home, serde_json::json!({})).to_string()).unwrap();
        let first = load(&home, false, &user).unwrap().unwrap();
        assert_eq!(first.configuration_digest(), load(&home, false, &user).unwrap().unwrap().configuration_digest(), "the same configuration, the same digest");
        std::fs::write(home.join(FILE), configuration(&home, serde_json::json!({"PATH": "/bin"})).to_string()).unwrap();
        assert_ne!(first.configuration_digest(), load(&home, false, &user).unwrap().unwrap().configuration_digest(), "another environment, another digest");
    }

    #[test]
    fn codex_account_binding_rejects_symbolic_managed_ancestors_and_keeps_default_native_home(){
        use alondra_core::accounts::{AccountKey,Provider,provider_folder};
        let home=crate::test_support::Directory::new();std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join(FILE),configuration(&home,serde_json::json!({"HOME":home.to_str().unwrap()})).to_string()).unwrap();
        let mut backend=load(&home,true,&home).unwrap().unwrap();backend.provider=Provider::Codex;
        let account=AccountId::parse("work").unwrap();let folder=provider_folder(&home,&AccountKey::new(Provider::Codex,account.clone())).unwrap();std::fs::create_dir_all(&folder).unwrap();
        assert_eq!(backend.for_account_provider(&home,Provider::Codex,&AccountId::default_account(),&[]).unwrap().native_environment,backend.native_environment);
        assert_eq!(backend.for_account_provider(&home,Provider::Codex,&account,std::slice::from_ref(&account)).unwrap().native_environment["CODEX_HOME"],folder.display().to_string());
        for managed in [folder.parent().unwrap().to_path_buf(),folder.parent().unwrap().parent().unwrap().to_path_buf()]{
            let retained=home.join("retained-managed");std::fs::rename(&managed,&retained).unwrap();std::os::unix::fs::symlink(&retained,&managed).unwrap();
            assert!(backend.for_account_provider(&home,Provider::Codex,&account,std::slice::from_ref(&account)).unwrap_err().to_string().contains("symlink"));
            std::fs::remove_file(&managed).unwrap();std::fs::rename(&retained,&managed).unwrap();
        }
    }

    #[test]
    fn a_selected_codex_backend_is_idempotent_and_returns_to_its_claude_launcher() {
        use alondra_core::accounts::Provider;
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        let environment = serde_json::json!({"HOME": home.to_str().unwrap()});
        let original = configuration(&home, environment.clone());
        std::fs::write(home.join(FILE), original.to_string()).unwrap();
        let source = load(&home, true, &home).unwrap().unwrap();
        let mut codex = configuration(&home, environment);
        let native = home.join("codex-native");
        std::fs::write(&native, "must never execute during backend selection").unwrap();
        codex["native"] = serde_json::json!(native);
        codex["native_sha256"] = serde_json::json!(digest(&native).unwrap());
        codex["state_root"] = serde_json::json!(home.join("codex"));
        std::fs::write(home.join("codex-backend.json"), codex.to_string()).unwrap();
        codex["native_environment"]["OPENAI_API_KEY"]=serde_json::json!("fixture-key-must-not-inherit");
        codex["native_environment"]["CODEX_ACCESS_TOKEN"]=serde_json::json!("fixture-external-token-must-not-inherit");
        codex["native_environment"]["OPENAI_IDENTITY_TOKEN_FILE"]=serde_json::json!("/fixture-only-token-source");
        std::fs::write(home.join("codex-backend.json"),codex.to_string()).unwrap();
        let selected = source.for_provider(Provider::Codex, &home).unwrap();
        assert!(!selected.native_environment.contains_key("OPENAI_API_KEY"),"runtime and no-inference probe must use the same native file/OAuth authentication context");
        assert!(!selected.native_environment.contains_key("CODEX_ACCESS_TOKEN"));assert!(!selected.native_environment.contains_key("OPENAI_IDENTITY_TOKEN_FILE"));
        assert_eq!(selected.provider, Provider::Codex);
        let mut contaminated=selected.clone();contaminated.native_environment.insert("OPENAI_API_KEY".into(),"fixture-key-must-not-inherit".into());
        let again = contaminated.for_provider(Provider::Codex, &home).unwrap();
        assert!(!again.native_environment.contains_key("OPENAI_API_KEY"));
        assert_eq!(again.configuration_digest(), selected.configuration_digest());
        assert_eq!(again.native, native);
        let claude = selected.for_provider(Provider::Claude, &home).unwrap();
        assert_eq!(claude.provider, Provider::Claude);
        assert_eq!(claude.configuration_digest(), source.configuration_digest());
        assert_eq!(claude.native, source.native);
        assert_ne!(claude.native, selected.native);
        assert!(serde_json::to_value(&source).unwrap().get("provider").is_none(), "legacy Claude configuration bytes do not acquire a provider field");
    }

    /// A Homebrew install: the keg with the daemon and its host, the `opt` link to it, and a
    /// folder on PATH whose `claude` prints `version`.
    fn release(root: &Path, version: &str) -> (PathBuf, PathBuf) {
        let keg = root.join("Cellar/alondra/0.3.0");
        std::fs::create_dir_all(keg.join("bin")).unwrap();
        std::fs::create_dir_all(keg.join("libexec/alondra/sdk")).unwrap();
        std::fs::write(keg.join("bin/alondrad"), b"daemon").unwrap();
        std::fs::write(keg.join("libexec/alondra/sdk").join(HOST), b"host").unwrap();
        std::fs::create_dir_all(root.join("opt")).unwrap();
        std::os::unix::fs::symlink(&keg, root.join("opt/alondra")).unwrap();
        let tools = root.join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        claude_code(&tools, version);
        (keg.join("bin/alondrad"), tools)
    }

    fn claude_code(tools: &Path, version: &str) {
        use std::os::unix::fs::PermissionsExt;
        let claude = tools.join("claude");
        std::fs::write(&claude, format!("#!/bin/sh\necho '{version} (Claude Code)'\n")).unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn a_release_without_a_configuration_runs_its_own_host_with_the_users_claude_code() {
        let root = crate::test_support::Directory::new();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let user = root.join("user");
        let (daemon, tools) = release(&root, "2.1.300");
        let environment = BTreeMap::from([
            ("PATH".to_owned(), format!("{}:/usr/bin:/bin", tools.display())), ("LANG".to_owned(), "es_ES.UTF-8".to_owned()),
            ("ALONDRA_PORT".to_owned(), "7777".to_owned()), ("CLAUDECODE".to_owned(), "1".to_owned()),
        ]);
        let backend = resolve(&home, false, &user, &daemon, &environment).expect("a release runs the host it installed");
        let host = root.join("opt/alondra/libexec/alondra/sdk").join(HOST);
        assert_eq!((&backend.node, &backend.host), (&host, &host), "one executable, by the link an upgrade moves");
        assert_eq!(backend.state_root, home.join("sdk"));
        assert!(backend.confinement.is_none());
        assert!(backend.host_sha256.is_empty(), "the daemon starts without reading the host");
        let started = backend.for_start().expect("Claude Code 2.1.300 runs the SDK");
        assert_eq!((&started.node_sha256, &started.host_sha256), (&digest(&host).unwrap(), &digest(&host).unwrap()), "a host start reads it");
        assert_eq!(started.native, tools.join("claude"), "the user's own Claude Code, found on the daemon's PATH");
        assert_eq!(started.native_sha256, digest(&tools.join("claude")).unwrap());
        assert_eq!(started.native_environment.get("HOME").map(String::as_str), Some(user.to_str().unwrap()));
        assert_eq!(started.native_environment.get("LANG").map(String::as_str), Some("es_ES.UTF-8"), "the user's environment");
        for private in ["ALONDRA_PORT", "CLAUDE_CONFIG_DIR", "CLAUDE_HOME", "CLAUDECODE"] {
            assert!(!started.native_environment.contains_key(private), "{private} is not the user's Claude's");
        }
        claude_code(&tools, "2.1.301");
        let updated = backend.for_start().unwrap();
        assert_ne!(updated.native_sha256, started.native_sha256, "Claude Code updated itself: the next host takes the new one");
        assert_ne!(updated.configuration_digest(), started.configuration_digest(), "and binds its executions to it");
    }

    // Setup and doctor check the sign-in of the profile in the daemon's environment: nodes run with
    // that same one, also when it names the default folder, whose sign-in is another one.
    #[test]
    fn nodes_run_with_the_claude_profile_the_daemon_was_set_up_with() {
        let root = crate::test_support::Directory::new();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let user = root.join("user");
        let (daemon, tools) = release(&root, "2.1.300");
        let path = ("PATH".to_owned(), format!("{}:/usr/bin:/bin", tools.display()));
        let work = user.join(".claude-work").display().to_string();
        for set_up in [
            BTreeMap::from([path.clone(), ("CLAUDE_CONFIG_DIR".to_owned(), work.clone()), ("CLAUDE_HOME".to_owned(), work.clone())]),
            BTreeMap::from([path.clone(), ("CLAUDE_CONFIG_DIR".to_owned(), work.clone())]),
            BTreeMap::from([path.clone(), ("CLAUDE_HOME".to_owned(), work.clone())]),
        ] {
            let started = resolve(&home, false, &user, &daemon, &set_up).unwrap().for_start().unwrap();
            for key in DEFAULT_PROFILE {
                assert_eq!(started.native_environment.get(key), Some(&work), "{key} with {set_up:?}");
            }
        }
        let default = resolve(&home, false, &user, &daemon, &BTreeMap::from([path])).unwrap().for_start().unwrap();
        for key in DEFAULT_PROFILE {
            assert!(!default.native_environment.contains_key(key), "no profile set up: Claude's default one, without {key}");
        }
        let (account, registered) = (AccountId::parse("other").unwrap(), [AccountId::default_account(), AccountId::parse("other").unwrap()]);
        std::fs::create_dir_all(home.join("accounts/other")).unwrap();
        let explicit = resolve(&home, false, &user, &daemon, &BTreeMap::from([("CLAUDE_CONFIG_DIR".to_owned(), work.clone())])).unwrap();
        let other = explicit.for_account(&home, &account, &registered).unwrap();
        for key in DEFAULT_PROFILE {
            assert_eq!(other.native_environment.get(key).map(String::as_str), home.join("accounts/other").to_str(), "an added account runs only on its folder");
        }
    }

    #[test]
    fn a_claude_code_the_sdk_cannot_run_keeps_hosts_from_starting_and_says_what_to_do() {
        let root = crate::test_support::Directory::new();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let (daemon, tools) = release(&root, "2.1.200");
        let environment = BTreeMap::from([("PATH".to_owned(), format!("{}:/usr/bin:/bin", tools.display()))]);
        let backend = resolve(&home, false, &root.join("user"), &daemon, &environment).expect("the daemon still starts");
        let old = backend.for_start().unwrap_err().to_string();
        assert!(old.contains("2.1.200") && old.contains("claude update"), "{old}");
        let missing = BTreeMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())]);
        let none = resolve(&home, false, &root.join("user"), &daemon, &missing).unwrap().for_start().unwrap_err().to_string();
        assert!(none.contains("not on this daemon's PATH"), "{none}");
        claude_code(&tools, "2.1.300");
        assert!(backend.for_start().is_ok(), "once updated, the next start runs");
    }

    #[test]
    fn a_configuration_file_replaces_the_release_and_a_sandbox_needs_one() {
        let root = crate::test_support::Directory::new();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let (daemon, _) = release(&root, "2.1.300");
        assert!(resolve(&home, true, &home, &daemon, &BTreeMap::new()).unwrap_err().to_string().contains(FILE));
        std::fs::write(home.join(FILE), configuration(&home, serde_json::json!({"HOME": home})).to_string()).unwrap();
        let pinned = resolve(&home, false, &home, &daemon, &BTreeMap::new()).unwrap();
        assert_eq!(pinned.host, home.join("artifact"));
        assert!(!pinned.installed_native);
        assert_eq!(pinned.for_start().unwrap().native, home.join("artifact"), "a pinned native Claude stays as it was checked");
        std::fs::remove_dir_all(root.join("Cellar")).unwrap();
        std::fs::remove_file(home.join(FILE)).unwrap();
        let missing = resolve(&home, false, &home, &daemon, &BTreeMap::new()).unwrap_err().to_string();
        assert!(missing.contains("brew reinstall alondra"), "{missing}");
    }

    // Claude Code keeps the user's sign-in per profile: naming the default folder in either
    // variable selects another credential namespace than the one the user signed in to.
    #[test]
    fn a_native_environment_that_names_a_claude_profile_is_refused() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        for (profile, sandbox) in DEFAULT_PROFILE.into_iter().flat_map(|profile| [(profile, true), (profile, false)]) {
            let backend = configuration(&home, serde_json::json!({"HOME": home.join("home"), profile: home.join("home/.claude")}));
            std::fs::write(home.join(FILE), backend.to_string()).unwrap();
            let refused = load(&home, sandbox, &home.join("home")).unwrap_err().to_string();
            assert!(refused.contains(&format!("sets {profile}")), "{refused}");
        }
    }

    #[test]
    fn an_account_runs_claude_with_its_own_folder_and_only_a_registered_one() {
        let root = crate::test_support::Directory::new();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(FILE), configuration(&home, serde_json::json!({"HOME": root.join("user")})).to_string()).unwrap();
        let backend = load(&home, false, &root.join("user")).unwrap().unwrap();
        let (work, other) = (AccountId::parse("work").unwrap(), AccountId::parse("other").unwrap());
        let registered = [AccountId::default_account(), work.clone()];
        let default = backend.for_account(&home, &AccountId::default_account(), &registered).unwrap();
        assert!(!default.native_environment.contains_key("CLAUDE_CONFIG_DIR"), "the default account is the user's own profile");
        assert_eq!(default.configuration_digest(), backend.configuration_digest());
        let missing = backend.for_account(&home, &work, &registered).unwrap_err().to_string();
        assert!(missing.contains("missing"), "{missing}");
        let folder = home.join("accounts/work");
        std::fs::create_dir_all(&folder).unwrap();
        let on_work = backend.for_account(&home, &work, &registered).unwrap();
        assert_eq!(on_work.native_environment.get("CLAUDE_CONFIG_DIR").map(String::as_str), Some(folder.to_str().unwrap()));
        assert_ne!(on_work.configuration_digest(), backend.configuration_digest(), "each account binds its own executions");
        std::fs::create_dir_all(home.join("accounts/other")).unwrap();
        let unregistered = backend.for_account(&home, &other, &registered).unwrap_err().to_string();
        assert!(unregistered.contains("not one of the accounts"), "{unregistered}");
        std::fs::remove_dir(&folder).unwrap();
        std::os::unix::fs::symlink(root.join("user"), &folder).unwrap();
        let linked = backend.for_account(&home, &work, &registered).unwrap_err().to_string();
        assert!(linked.contains("not a folder Alondra made"), "{linked}");
    }
}

impl SdkBackend {
    /// Selects a provider before checking its native executable. A Codex-only start never probes Claude.
    pub(crate) fn for_provider(&self, provider: alondra_core::accounts::Provider, home: &Path) -> Result<Self> {
        if self.provider == provider {
            let mut selected=self.clone();
            if provider==alondra_core::accounts::Provider::Codex {remove_codex_auth_environment(&mut selected.native_environment);}
            return Ok(selected);
        }
        if let Some(origin) = &self.origin { return origin.for_provider(provider, home); }
        ensure!(self.provider == alondra_core::accounts::Provider::Claude, "the selected backend has no original launcher for the requested provider");
        let path = home.join("codex-backend.json");
        match std::fs::read(&path) {
            Ok(bytes) => {
                let mut backend: Self = serde_json::from_slice(&bytes).context("reading the Codex backend")?;
                let declared: serde_json::Value = serde_json::from_slice(&bytes)?;
                ensure!(declared.get("provider").is_none() || backend.provider == provider, "the Codex backend declares another provider");
                backend.provider = provider;
                remove_codex_auth_environment(&mut backend.native_environment);
                backend.origin = Some(Box::new(self.clone()));
                if let Some(port) = self.native_environment.get("ALONDRA_PORT") { backend.native_environment.insert("ALONDRA_PORT".into(), port.clone()); }
                for (file, expected) in [(&backend.node, &backend.node_sha256), (&backend.host, &backend.host_sha256), (&backend.native, &backend.native_sha256)] {
                    ensure!(file.is_absolute() && digest(file)? == *expected, "the Codex backend artifact does not match its manifest");
                }
                ensure!(backend.state_root.is_absolute() && (1..=300).contains(&backend.io_timeout_seconds), "the Codex backend limits are invalid");
                ensure!(backend.state_root.as_os_str().len() + EXECUTION_FOLDER <= SOCKET_PATH_LIMIT, "the Codex state root is too long for its Unix socket");
                ensure!(backend.native_environment.get("HOME") == self.native_environment.get("HOME"), "the Codex backend must use this installation's native HOME");
                return Ok(backend);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("reading the Codex backend"),
        }
        ensure!(self.installed_native, "a sandbox Codex execution requires an explicit codex-backend.json; it never uses the machine's Codex");
        let host = self.host.parent().and_then(Path::parent).context("the installed runtime has no provider asset directory")?.join("codex/alondra-codex-host");
        let sha256 = installed_digest(&host).context("the Codex host is missing: reinstall Alondra")?;
        let mut native_environment = self.native_environment.clone();
        remove_codex_auth_environment(&mut native_environment);
        native_environment.retain(|key, _| key!="OPENAI_API_KEY" && !key.starts_with("CLAUDE") && (!key.starts_with("ALONDRA_") || key == "ALONDRA_PORT"));
        let native = on_path(native_environment.get("PATH").map(String::as_str).unwrap_or_default(), "codex")
            .context("Codex (`codex`) is not on this daemon's PATH: install Codex, then run `alondra doctor --fix`")?;
        let native_sha256 = digest(&native)?;
        compatible_codex(&native, &native_sha256)?;
        let state_root = home.join("codex");
        ensure!(state_root.as_os_str().len() + EXECUTION_FOLDER <= SOCKET_PATH_LIMIT, "the Codex state root is too long for its Unix socket");
        Ok(Self {
            provider, origin: Some(Box::new(self.clone())),
            node: host.clone(), node_sha256: sha256.clone(), host, host_sha256: sha256,
            native, native_sha256, host_environment: self.host_environment.clone(), native_environment,
            confinement: self.confinement.clone(), state_root, io_timeout_seconds: self.io_timeout_seconds,
            installed_native: true,
        })
    }

    pub(crate) fn for_account_provider(&self, home: &Path, provider: alondra_core::accounts::Provider, account: &AccountId, registered: &[AccountId]) -> Result<Self> {
        if provider == alondra_core::accounts::Provider::Claude { return self.for_account(home, account, registered); }
        if account.is_default() { return Ok(self.clone()); }
        ensure!(registered.contains(account), "the Codex account is not registered in options.toml");
        let key = alondra_core::accounts::AccountKey::new(provider, account.clone());
        let folder = alondra_core::accounts::provider_folder(home, &key).context("the managed provider account has no folder")?;
        let mut ancestor = home.to_path_buf();
        for component in folder.strip_prefix(home).context("the Codex account escaped its managed home")?.components() {
            ensure!(matches!(component,std::path::Component::Normal(_)),"the Codex account path is not managed");
            ancestor.push(component);
            let metadata=std::fs::symlink_metadata(&ancestor).context("a managed Codex account directory is missing")?;
            ensure!(metadata.is_dir() && !metadata.is_symlink(),"a managed Codex account directory is a symlink or is not owned by Alondra");
        }
        let mut backend = self.clone();
        backend.native_environment.insert("CODEX_HOME".into(), folder.display().to_string());
        Ok(backend)
    }
}

fn is_claude(provider: &alondra_core::accounts::Provider) -> bool { *provider == alondra_core::accounts::Provider::Claude }
fn compatible_codex(native: &Path, sha256: &str) -> Result<String> {
    let key = format!("codex:{sha256}");
    if let Some(known) = CHECKED.lock_or_recover("the checked SDK releases").as_ref().and_then(|known| known.get(&key).cloned()) { return Ok(known); }
    let release = version_of(native)?;
    ensure!(alondra_core::codex_account::supported_cli_version(&release), "Codex {} is not supported by this Alondra release; Codex 0.163.0 is required", release.trim());
    CHECKED.lock_or_recover("the checked SDK releases").get_or_insert_default().insert(key, release.clone());
    Ok(release)
}

fn remove_codex_auth_environment(environment:&mut std::collections::BTreeMap<String,String>){
    for key in ["OPENAI_API_KEY","CODEX_API_KEY","CODEX_ACCESS_TOKEN","OPENAI_FEDERATION_RULE_ID","OPENAI_IDENTITY_TOKEN_FILE","OPENAI_WORKLOAD_IDENTITY_CONTEXT"]{environment.remove(key);}
}
