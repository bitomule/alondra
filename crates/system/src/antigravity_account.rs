//! An Antigravity account on this Mac: the private folder Alondra makes for a named account, with
//! links to what the user set up in `~/.gemini` and the file that keeps its key, and the `agy` it
//! asks whether the account is signed in, which models and thinking levels it offers, and how
//! much of its quota is left. None of it sends a prompt.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use alondra_core::accounts::{AccountKey, MANAGED_PROVIDER_FOLDER, Provider, provider_folder};
use alondra_core::antigravity_account::{self as antigravity, AGENT_NAME, AUTH_REQUIRED, Access, KEY_FILE, KEY_FILE_LIMIT, PROTOCOL_VERSION, SHARED, Version};
use alondra_core::antigravity_account::{Check, CheckReport, Probe as ProbeSpec, Secret, Unavailable, UsageOutcome};
use alondra_core::model_selection::CatalogModel;

const OUTPUT_LIMIT: u64 = 2 * 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
const VERSION_TIMEOUT: Duration = Duration::from_secs(15);
const MODELS_LIMIT: usize = 64;

/// Makes the private folder of a named Antigravity account, with a link to each name of `SHARED`
/// that `shared_from` (the user's `~/.gemini`) has; none for the default account, which is `agy`'s
/// own. Running it again only adds what `shared_from` gained. No folder on the way is a link.
pub fn prepare(home: &Path, key: &AccountKey, shared_from: &Path) -> Result<Option<PathBuf>, String> {
    if key.provider != Provider::Antigravity { return Err("The account is not an Antigravity account".into()); }
    let Some(folder) = provider_folder(home, key) else { return Ok(None) };
    let managed = home.join(MANAGED_PROVIDER_FOLDER);
    fs::create_dir_all(home).map_err(|error| error.to_string())?;
    for directory in [managed.clone(), managed.join("antigravity"), folder.clone()] {
        private_folder(&directory)?;
    }
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).map_err(|error| error.to_string())?;
    for name in SHARED {
        let target = shared_from.join(name);
        let link = folder.join(name);
        if fs::symlink_metadata(&target).is_err() || fs::symlink_metadata(&link).is_ok() { continue; }
        if let Some(parent) = link.parent().filter(|parent| *parent != folder) { private_folder(parent)?; }
        std::os::unix::fs::symlink(&target, &link).map_err(|error| error.to_string())?;
    }
    Ok(Some(folder))
}

fn private_folder(directory: &Path) -> Result<(), String> {
    match directory.symlink_metadata() {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => Err("The Antigravity account folder must be a real directory".into()),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::DirBuilder::new().mode(0o700).create(directory).map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    }
}

/// Deletes a named account's folder, its key with it: its links go, never what they point to.
pub fn remove(home: &Path, key: &AccountKey) -> std::io::Result<()> {
    let Some(folder) = provider_folder(home, key) else { return Ok(()) };
    match fs::symlink_metadata(&folder) {
        Ok(meta) if meta.file_type().is_symlink() => fs::remove_file(&folder),
        Ok(_) => fs::remove_dir_all(&folder),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The folder of a named account, checked to be a private folder of the user's own.
fn account_folder(home: &Path, key: &AccountKey) -> Result<PathBuf, String> {
    if key.provider != Provider::Antigravity { return Err("The account is not an Antigravity account".into()); }
    let folder = provider_folder(home, key).ok_or("The default account is agy's own sign-in: only a named account keeps a key")?;
    let meta = fs::symlink_metadata(&folder).map_err(|_| "The account's folder is missing: add the account again".to_owned())?;
    let owner = fs::metadata(home).map_err(|error| error.to_string())?;
    if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != owner.uid() || meta.mode() & 0o077 != 0 {
        return Err("The account's folder is not a private folder of Alondra's".into());
    }
    Ok(folder)
}

/// Keeps `secret` as the account's key: written to a new `0600` file of its folder, synced, then
/// renamed over the old one, so a reader sees the old key or the new one whole. `None` removes it.
pub fn set_key(home: &Path, key: &AccountKey, secret: Option<&Secret>) -> Result<(), String> {
    static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _writing = WRITING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let folder = account_folder(home, key)?;
    remove_staged(&folder);
    let path = folder.join(KEY_FILE);
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => return Err("The account's key file is not a regular file".into()),
        _ => {}
    }
    let Some(secret) = secret else {
        return match fs::remove_file(&path) {
            Ok(()) => sync_folder(&folder),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        };
    };
    if let Some(problem) = antigravity::key_problem(secret.expose()) { return Err(problem.into()); }
    let mut random = [0u8; 8];
    getrandom(&mut random);
    let staged = folder.join(format!(".{KEY_FILE}.{}", random.iter().map(|byte| format!("{byte:02x}")).collect::<String>()));
    let written = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&staged)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(secret.expose().as_bytes())?;
        file.sync_all()?;
        fs::rename(&staged, &path)
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&staged);
        return Err(format!("Could not keep the key: {}", error.kind()));
    }
    sync_folder(&folder)
}

/// Deletes the copies of a key a write cut short left in `folder`.
fn remove_staged(folder: &Path) {
    let prefix = format!(".{KEY_FILE}.");
    for entry in fs::read_dir(folder).into_iter().flatten().flatten() {
        if entry.file_name().to_str().is_some_and(|name| name.starts_with(&prefix)) { let _ = fs::remove_file(entry.path()); }
    }
}

fn getrandom(bytes: &mut [u8]) {
    unsafe { libc::arc4random_buf(bytes.as_mut_ptr().cast(), bytes.len()) };
}

fn sync_folder(folder: &Path) -> Result<(), String> {
    fs::File::open(folder).and_then(|folder| folder.sync_all()).map_err(|error| error.to_string())
}

/// The account's key, when its folder keeps one: read without following a link, only from the
/// user's own regular file that no one else may read or write and that has no other link, by the
/// rule the Antigravity host reads it with (`antigravity::key_in_file`).
pub fn key(home: &Path, key: &AccountKey) -> Result<Option<Secret>, String> {
    if key.account.is_default() { return Ok(None); }
    let folder = account_folder(home, key)?;
    let path = folder.join(KEY_FILE);
    let expected = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let owner = fs::metadata(&folder).map_err(|error| error.to_string())?.uid();
    if !expected.is_file() || expected.file_type().is_symlink() || expected.nlink() != 1 || expected.uid() != owner
        || !antigravity::key_file_private(expected.mode()) || expected.len() > KEY_FILE_LIMIT as u64 {
        return Err("The account's key file is not a private regular file".into());
    }
    let mut file = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(&path).map_err(|error| error.to_string())?;
    let opened = file.metadata().map_err(|error| error.to_string())?;
    if opened.ino() != expected.ino() || opened.dev() != expected.dev() { return Err("The account's key file changed while opening it".into()); }
    let mut bytes = Vec::with_capacity(KEY_FILE_LIMIT + 1);
    Read::by_ref(&mut file).take(KEY_FILE_LIMIT as u64 + 1).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    if bytes.len() > KEY_FILE_LIMIT { bytes.fill(0); return Err("The account's key file is not a private regular file".into()); }
    let text = match String::from_utf8(bytes) {
        Ok(text) => Secret::new(text),
        Err(error) => { error.into_bytes().fill(0); return Err("The account's key file is not text".into()); }
    };
    match antigravity::key_in_file(text.expose()) {
        Ok(found) => Ok(found.map(|found| Secret::new(found.to_owned()))),
        Err(problem) => Err(format!("The account's key file does not hold a key: {problem}")),
    }
}

/// Whether the account's folder keeps a key `key` reads: a file it refuses is no key.
pub fn key_set(home: &Path, account: &AccountKey) -> bool {
    matches!(key(home, account), Ok(Some(_)))
}

/// What `agy --version` says, run with the account's environment.
pub fn version(probe: &ProbeSpec) -> Result<Version, Unavailable> {
    let mut command = Command::new(&probe.executable);
    command.arg("--version").env_clear().envs(&probe.launch.environment).current_dir(&probe.directory)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let output = run_bounded(command, VERSION_TIMEOUT).map_err(|kind| match kind {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied => Unavailable::NotInstalled,
        _ => Unavailable::Failed,
    })?;
    Version::parse(&String::from_utf8_lossy(&output.1)).ok_or(Unavailable::UnknownVersion)
}

/// Runs `command` in a process group of its own, ending it after `timeout`: its exit code and
/// what it printed, cut to the probe's limit.
fn run_bounded(mut command: Command, timeout: Duration) -> Result<(Option<i32>, Vec<u8>), std::io::ErrorKind> {
    unsafe { command.pre_exec(|| if libc::setpgid(0, 0) == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }); }
    let mut child = command.spawn().map_err(|error| error.kind())?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut bytes = vec![];
        let _ = Read::by_ref(&mut stdout).take(OUTPUT_LIMIT).read_to_end(&mut bytes);
        bytes
    });
    let until = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= until => break None,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => { end(&mut child); return Err(error.kind()); }
        }
    };
    let Some(status) = status else { end(&mut child); let _ = reader.join(); return Err(std::io::ErrorKind::TimedOut) };
    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL); }
    let bytes = reader.join().unwrap_or_default();
    Ok((status.code(), bytes))
}

fn end(child: &mut Child) {
    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL); }
    let _ = child.kill();
    let _ = child.wait();
}

/// An `agy acp` the account check speaks JSON-RPC to.
struct Acp {
    child: Child,
    input: Option<ChildStdin>,
    output: Receiver<Result<Value, ()>>,
    reader: Option<std::thread::JoinHandle<()>>,
    until: Instant,
    sequence: u64,
}

/// How a request was answered: its result, or the JSON-RPC code it was refused with.
enum Answer {
    Result(Value),
    Refused(i64),
}

impl Drop for Acp {
    fn drop(&mut self) {
        self.input.take();
        end(&mut self.child);
        if let Some(reader) = self.reader.take() { let _ = reader.join(); }
    }
}

impl Acp {
    fn start(probe: &ProbeSpec) -> Result<Self, std::io::ErrorKind> {
        let mut command = Command::new(&probe.executable);
        command.arg("acp").args(&probe.launch.args).env_clear().envs(&probe.launch.environment).current_dir(&probe.directory)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        unsafe { command.pre_exec(|| if libc::setpgid(0, 0) == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }); }
        let mut child = command.spawn().map_err(|error| error.kind())?;
        let stdout = child.stdout.take().expect("piped stdout");
        let input = child.stdin.take();
        let (send, output) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut lines = BufReader::new(stdout.take(OUTPUT_LIMIT + 1));
            let mut total = 0u64;
            loop {
                let mut bytes = vec![];
                match lines.read_until(b'\n', &mut bytes) {
                    Ok(0) | Err(_) => return,
                    Ok(length) => {
                        total += length as u64;
                        if bytes.iter().all(u8::is_ascii_whitespace) { continue; }
                        let message = if total > OUTPUT_LIMIT { Err(()) } else { serde_json::from_slice(&bytes).map_err(drop) };
                        let stop = message.is_err();
                        if send.send(message).is_err() || stop { return; }
                    }
                }
            }
        });
        Ok(Self { child, input, output, reader: Some(reader), until: Instant::now() + PROBE_TIMEOUT, sequence: 0 })
    }

    fn send(&mut self, value: &Value) -> Result<(), ()> {
        let input = self.input.as_mut().ok_or(())?;
        serde_json::to_writer(&mut *input, value).map_err(drop)?;
        input.write_all(b"\n").and_then(|_| input.flush()).map_err(drop)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Answer, ()> {
        self.sequence += 1;
        let id = self.sequence;
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        loop {
            let left = self.until.checked_duration_since(Instant::now()).ok_or(())?;
            let message = self.output.recv_timeout(left).map_err(drop)??;
            if message.get("method").is_some() {
                if message.get("id").is_some() {
                    self.send(&json!({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32601,"message":"An account check takes no requests"}}))?;
                }
                continue;
            }
            if message["id"].as_u64() != Some(id) { continue; }
            if let Some(error) = message.get("error") { return Ok(Answer::Refused(error["code"].as_i64().unwrap_or(0))); }
            return message.get("result").cloned().map(Answer::Result).ok_or(());
        }
    }
}

/// Asks the account's `agy acp` whether it is signed in and, when it is, which models and thinking
/// levels it offers. It opens one session in `probe.directory`, selects each model in turn and
/// closes it: no prompt is sent, so no quota is spent. A Google or Enterprise sign-in is never
/// started here: without one, `agy` refuses the session (`AUTH_REQUIRED`).
pub fn check(probe: &ProbeSpec) -> CheckReport {
    let version = match version(probe) {
        Ok(version) => version,
        Err(why) => return CheckReport { version: None, check: Check::Unavailable(why) },
    };
    if !version.supported() { return CheckReport { version: Some(version), check: Check::Unavailable(Unavailable::TooOld) }; }
    let check = match Acp::start(probe) {
        Ok(mut acp) => session(&mut acp, probe.access, &probe.directory).unwrap_or(Check::Unavailable(Unavailable::Failed)),
        Err(std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied) => Check::Unavailable(Unavailable::NotInstalled),
        Err(_) => Check::Unavailable(Unavailable::Failed),
    };
    CheckReport { version: Some(version), check }
}

fn session(acp: &mut Acp, access: Access, directory: &Path) -> Result<Check, ()> {
    let Answer::Result(initialized) = acp.request("initialize", json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientInfo": {"name": "alondra", "title": "Alondra", "version": env!("CARGO_PKG_VERSION")},
        "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
    }))? else { return Ok(Check::Unavailable(Unavailable::Failed)) };
    let resumes = initialized["agentCapabilities"]["sessionCapabilities"].get("resume").is_some_and(Value::is_object);
    if initialized["agentInfo"]["name"].as_str() != Some(AGENT_NAME) || initialized["protocolVersion"].as_u64() != Some(PROTOCOL_VERSION) || !resumes {
        return Ok(Check::Unavailable(Unavailable::OtherAgent));
    }
    if !access.signs_in_with_google() {
        match acp.request("authenticate", json!({"methodId": access.auth_method()}))? {
            Answer::Result(_) => {}
            Answer::Refused(AUTH_REQUIRED) => return Ok(Check::SignedOut),
            Answer::Refused(_) => return Ok(Check::Unavailable(Unavailable::Failed)),
        }
    }
    let opened = match acp.request("session/new", json!({"cwd": directory, "mcpServers": []}))? {
        Answer::Result(opened) => opened,
        Answer::Refused(AUTH_REQUIRED) => return Ok(Check::SignedOut),
        Answer::Refused(_) => return Ok(Check::Unavailable(Unavailable::Failed)),
    };
    let session = opened["sessionId"].as_str().filter(|id| !id.is_empty() && id.len() <= 256).ok_or(())?.to_owned();
    let models = catalog(acp, &session, &opened["configOptions"])?;
    if initialized["agentCapabilities"]["sessionCapabilities"].get("close").is_some_and(Value::is_object) {
        acp.until = acp.until.min(Instant::now() + Duration::from_secs(5));
        let _ = acp.request("session/close", json!({"sessionId": session}));
    }
    let native_default = models.iter().find(|model| model.is_default).map(|model| model.model.clone());
    Ok(Check::SignedIn { models, native_default })
}

fn catalog(acp: &mut Acp, session: &str, options: &Value) -> Result<Vec<CatalogModel>, ()> {
    let models = antigravity::select_option(options, "model").ok_or(())?;
    let initial = antigravity::select_option(options, "thought_level").unwrap_or_default();
    let first = models.current.clone();
    let mut catalog = vec![];
    for (model, name) in models.values.iter().take(MODELS_LIMIT) {
        let levels = if Some(model) == first.as_ref() { initial.clone() } else {
            let Answer::Result(selected) = acp.request("session/set_config_option", json!({"sessionId": session, "configId": "model", "value": model}))? else { continue };
            antigravity::select_option(&selected["configOptions"], "thought_level").unwrap_or_default()
        };
        catalog.push(antigravity::catalog_model(model, name, &levels, Some(model) == first.as_ref()));
    }
    Ok(catalog)
}

/// What `agy -p /usage --output-format json` says of the account's quota. It starts no turn.
pub fn usage(probe: &ProbeSpec) -> Result<UsageOutcome, String> {
    let mut command = Command::new(&probe.executable);
    command.args(["-p", "/usage", "--output-format", "json", "--print-timeout", "60s"]).env_clear().envs(&probe.launch.environment)
        .current_dir(&probe.directory).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let (code, output) = run_bounded(command, PROBE_TIMEOUT).map_err(|kind| format!("Could not run agy: {kind}"))?;
    let answer: Value = serde_json::from_slice(&output).map_err(|_| "agy returned no usage report".to_owned())?;
    match (code, answer["status"].as_str()) {
        (Some(0), Some("SUCCESS")) => antigravity::parse_usage(&answer["command"]["data"]).map(UsageOutcome::Read),
        (_, Some("ERROR")) => Ok(UsageOutcome::Refused),
        _ => Err("agy did not report the usage".into()),
    }
}

#[cfg(test)]
#[path = "antigravity_account_tests.rs"]
mod tests;
