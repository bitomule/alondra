//! A Codex account on this Mac: the private folder Alondra makes for a named account, and the
//! `codex app-server` it asks for the sign-in, the usage, the models and a retained thread.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use alondra_core::accounts::{AccountKey, Provider, provider_folder};
use alondra_core::model_selection::{AccountModelCatalog, CatalogModel};

use alondra_core::codex_account::{ProbeReport, parse_models};

const OUTPUT_LIMIT: u64 = 2 * 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

pub fn prepare(home: &Path, key: &AccountKey) -> Result<Option<PathBuf>, String> {
    if key.provider != Provider::Codex { return Err("The account is not a Codex account".into()); }
    let Some(folder) = provider_folder(home, key) else { return Ok(None) };
    let managed = home.join(alondra_core::accounts::MANAGED_PROVIDER_FOLDER);
    for directory in [managed.clone(), managed.join("codex"), folder.clone()] {
        match directory.symlink_metadata() {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => return Err("The Codex account folder must be a real directory".into()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if directory.parent() == Some(home) { fs::create_dir_all(home).map_err(|error| error.to_string())?; }
                fs::DirBuilder::new().mode(0o700).create(&directory).map_err(|error| error.to_string())?;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).map_err(|error| error.to_string())?;
    match OpenOptions::new().write(true).create_new(true).mode(0o600).open(folder.join("config.toml")) {
        Ok(mut file) => file.write_all(b"cli_auth_credentials_store = \"file\"\n").map_err(|error| error.to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if !folder.join("config.toml").symlink_metadata().is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink()) {
                return Err("The Codex account configuration must be a real file".into());
            }
        }
        Err(error) => return Err(error.to_string()),
    }
    Ok(Some(folder))
}

pub(crate) struct Probe {
    child: Child,
    input: Option<ChildStdin>,
    output: Receiver<Result<Value, String>>,
    reader: Option<std::thread::JoinHandle<()>>,
    until: Instant,
    sequence: u64,
    fixture_diagnostics: bool,
    notifications: std::collections::VecDeque<Value>,
    pub(crate) cancelled: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.input.take();
        unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL); }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() { let _ = reader.join(); }
    }
}

impl Probe {
    pub(crate) fn start(mut command: Command, timeout: Duration) -> Result<Self, String> {
        command.args(["app-server", "--listen", "stdio://"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        unsafe { command.pre_exec(|| if libc::setpgid(0, 0) == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }); }
        let mut child = command.spawn().map_err(|error| format!("Could not run Codex: {error}"))?;
        let stdout = child.stdout.take().expect("piped stdout");
        let input = child.stdin.take();
        let (send, output) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut lines = BufReader::new(stdout.take(OUTPUT_LIMIT + 1));
            let mut total = 0u64;
            loop {
                let mut bytes = vec![];
                match lines.read_until(b'\n', &mut bytes) {
                    Ok(0) => break,
                    Ok(length) => {
                        total += length as u64;
                        let message = if total > OUTPUT_LIMIT { Err("Codex probe output exceeds its limit".into()) }
                            else { serde_json::from_slice(&bytes).map_err(|_| "Codex returned an invalid protocol message".into()) };
                        let stop = message.is_err();
                        if send.send(message).is_err() || stop { return; }
                    }
                    Err(error) => { let _ = send.send(Err(error.to_string())); return; }
                }
            }
        });
        Ok(Self { child, input, output, reader: Some(reader), until: Instant::now() + timeout, sequence: 0, fixture_diagnostics: false, notifications: Default::default(), cancelled: None })
    }

    pub(crate) fn send(&mut self, value: Value) -> Result<(), String> {
        let input = self.input.as_mut().ok_or("Codex input is closed")?;
        serde_json::to_writer(&mut *input, &value).map_err(|error| error.to_string())?;
        input.write_all(b"\n").and_then(|_| input.flush()).map_err(|error| error.to_string())
    }

    pub(crate) fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.sequence += 1;
        let id = self.sequence;
        self.send(json!({"id":id,"method":method,"params":params}))?;
        loop {
            self.check_cancelled()?;
            let left = self.until.checked_duration_since(Instant::now()).ok_or("Codex probe did not answer in time")?;
            let wait = if self.cancelled.is_some() { left.min(Duration::from_millis(100)) } else { left };
            let message = match self.output.recv_timeout(wait) {
                Ok(message) => message?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("Codex probe ended before answering".into()),
            };
            if message["id"].as_u64() == Some(id) && message.get("method").is_none() {
                if let Some(error) = message.get("error") {
                    if self.fixture_diagnostics {
                        let code = error["code"].as_i64().unwrap_or(0);
                        let detail: String = error["message"].as_str().unwrap_or("Unspecified fixture error").chars().filter(|character| !character.is_control()).take(256).collect();
                        return Err(format!("Codex refused {method}: native RPC {code}: {detail}"));
                    }
                    return Err(format!("Codex refused {method}"));
                }
                return message.get("result").cloned().ok_or_else(|| format!("Codex did not return a result for {method}"));
            }
            if message.get("method").is_some() && message.get("id").is_some() {
                self.send(json!({"id":message["id"],"error":{"code":-32601,"message":"Account probes do not handle interactive requests"}}))?;
            } else if message.get("method").is_some() {
                self.notifications.push_back(message);
            }
        }
    }

    fn check_cancelled(&self) -> Result<(), String> {
        if self.cancelled.as_ref().is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)) {
            return Err("Codex sign-in cancelled; the previous session was preserved".into());
        }
        Ok(())
    }
}

pub fn probe(command: Command, account: AccountKey, now: alondra_foundation::Millis) -> Result<ProbeReport, String> {
    probe_matching(command, account, now, |_| true)
}

/// Checks local thread metadata and its retained rollout without opening a conversation or turn.
pub fn retained_thread(executable: &Path, environment: &BTreeMap<String, String>, thread: &str, directory: &Path) -> Result<bool, String> {
    let normal_absolute = |path: &Path| path.is_absolute() && path.components().all(|part| matches!(part, std::path::Component::RootDir | std::path::Component::Normal(_)));
    if thread.is_empty() || thread.len() > 256 || thread.chars().any(char::is_control) || !normal_absolute(executable) || !normal_absolute(directory) {
        return Ok(false);
    }
    let Some(directory) = directory.to_str() else { return Ok(false) };
    let Some(home) = environment.get("HOME").map(Path::new).filter(|home| normal_absolute(home)) else { return Ok(false) };
    let mut command = Command::new(executable);
    command.env_clear().envs(environment).current_dir(home);
    let mut process = Probe::start(command, PROBE_TIMEOUT)?;
    process.rpc("initialize", json!({"clientInfo":{"name":"alondra","title":"Alondra","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
    process.send(json!({"method":"initialized"}))?;
    let result = process.rpc("thread/read", json!({"threadId":thread,"includeTurns":false}))?;
    if result["thread"]["id"].as_str() != Some(thread) || result["thread"]["cwd"].as_str() != Some(directory) { return Ok(false); }
    let Some(path) = result["thread"]["path"].as_str().map(Path::new).filter(|path| normal_absolute(path)) else { return Ok(false) };
    use std::os::unix::fs::MetadataExt as _;
    let Ok(before) = fs::symlink_metadata(path) else { return Ok(false) };
    if !before.is_file() || before.file_type().is_symlink() || before.len() == 0 { return Ok(false); }
    let Ok(file) = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path) else { return Ok(false) };
    let Ok(opened) = file.metadata() else { return Ok(false) };
    Ok(opened.is_file() && opened.dev() == before.dev() && opened.ino() == before.ino())
}

/// Optional quota never prevents cached model metadata; refused identity publishes neither usage nor catalog.
pub fn probe_matching(command: Command, account: AccountKey, now: alondra_foundation::Millis, accept_account: impl FnOnce(&ProbeReport) -> bool) -> Result<ProbeReport, String> {
    probe_with_diagnostics(command, account, now, accept_account, false)
}

/// Only isolated synthetic-account fixtures expose bounded native RPC errors; never return error data.
pub fn probe_fixture(command: Command, account: AccountKey, now: alondra_foundation::Millis) -> Result<ProbeReport, String> {
    probe_with_diagnostics(command, account, now, |_| true, true)
}

fn probe_with_diagnostics(command: Command, account: AccountKey, now: alondra_foundation::Millis, accept_account: impl FnOnce(&ProbeReport) -> bool, fixture_diagnostics: bool) -> Result<ProbeReport, String> {
    let mut process = Probe::start(command, PROBE_TIMEOUT)?;
    process.fixture_diagnostics = fixture_diagnostics;
    let initialized = process.rpc("initialize", json!({"clientInfo":{"name":"alondra","title":"Alondra","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
    process.send(json!({"method":"initialized"}))?;
    let status = process.rpc("account/read", json!({"refreshToken":false}))?;
    let authentication = process.rpc("getAuthStatus", json!({"includeToken":false,"refreshToken":false}))?;
    let auth_method = authentication["authMethod"].as_str().filter(|method| method.len() <= 64 && !method.chars().any(char::is_control)).map(str::to_owned);
    let requires_auth = status["requiresOpenaiAuth"].as_bool().unwrap_or(true);
    let signed_in = !status["account"].is_null() || !requires_auth;
    let email = status["account"]["email"].as_str().map(str::to_owned);
    let plan = status["account"]["planType"].as_str().map(str::to_owned);
    let account_type = status["account"]["type"].as_str().map(str::to_owned);
    let pay_per_use = auth_method.as_deref() == Some("apikey") || account_type.as_deref() == Some("apiKey");
    let models = read_models(&mut process, signed_in);
    let rate_limits = signed_in.then(|| process.rpc("account/rateLimits/read", json!({})).ok()).flatten();
    let workspace_id = (auth_method.as_deref() == Some("chatgpt") && account_type.as_deref() == Some("chatgpt"))
        .then(|| rate_limits.as_ref().filter(|limits| limits["ordinaryUsageAllowed"].is_boolean())
            .and_then(|limits| limits["accountId"].as_str()).filter(|id| !id.trim().is_empty() && id.len() <= 1024 && !id.chars().any(char::is_control)).map(str::to_owned)).flatten();
    let mut report = ProbeReport { requires_auth, auth_method, account_type, workspace_id, signed_in, email, plan, pay_per_use, rate_limits: None, catalog_error: None,
        catalog: AccountModelCatalog { account, revision: String::new(), models: vec![], native_default: None, measured_at: now } };
    if !accept_account(&report) { return Ok(report); }
    let models = match models {
        Ok(models) => models,
        Err(why) => { report.catalog_error = Some(why); report.rate_limits = rate_limits; return Ok(report); }
    };
    let native_default = models.iter().find(|model| model.is_default && !model.hidden).map(|model| model.model.clone());
    let revision: String = Sha256::digest(serde_json::to_vec(&models).map_err(|error| error.to_string())?).iter().map(|byte| format!("{byte:02x}")).collect();
    let _ = initialized;
    report.rate_limits = rate_limits; report.catalog.revision = revision; report.catalog.models = models; report.catalog.native_default = native_default;
    Ok(report)
}


fn read_models(process: &mut Probe, signed_in: bool) -> Result<Vec<CatalogModel>, String> {
    let mut models = vec![];
    let mut cursor: Option<String> = None;
    if signed_in {
        for page in 0..20 {
            let list = process.rpc("model/list", json!({"cursor":cursor,"limit":100,"includeHidden":false}))?;
            models.extend(parse_models(&list)?);
            cursor = list["nextCursor"].as_str().map(str::to_owned);
            if cursor.is_none() { break; }
            if page == 19 { return Err("Codex model catalog exceeds its page limit".into()); }
        }
    }
    Ok(models)
}


/// Deletes a named Codex account's folder, if it has one.
pub fn remove(home: &Path, key: &AccountKey) -> std::io::Result<()> {
    provider_folder(home, key).filter(|folder| folder.exists()).map_or(Ok(()), fs::remove_dir_all)
}

/// The private file a native Codex session records the account it started on in, `directory`'s
/// `native-auth-account.json`, read only when it is the user's own regular file.
pub fn native_session_identity(directory: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let path = directory.join("native-auth-account.json");
    let expected = std::fs::symlink_metadata(&path).map_err(|_| "This native session has no verified account identity; keep it paused and create a new node after signing in".to_owned())?;
    let owner = std::fs::metadata(directory).map_err(|why| why.to_string())?;
    if !expected.is_file() || expected.file_type().is_symlink() || expected.nlink() != 1 || expected.uid() != owner.uid() || expected.mode() & 0o777 != 0o600 || expected.len() > 4096 {
        return Err("The native session account identity is not a private file".into());
    }
    let mut file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path).map_err(|why| why.to_string())?;
    let actual = file.metadata().map_err(|why| why.to_string())?;
    if expected.ino() != actual.ino() || expected.dev() != actual.dev() { return Err("The native session account identity changed while opening it".into()); }
    let mut bytes = vec![];
    Read::by_ref(&mut file).take(4097).read_to_end(&mut bytes).map_err(|why| why.to_string())?;
    if bytes.len() > 4096 { return Err("The native session account identity is too large".into()); }
    Ok(bytes)
}

#[cfg(test)]
#[path = "codex_account_tests.rs"]
mod tests;
