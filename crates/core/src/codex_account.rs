pub const SUPPORTED_CLI_VERSION: &str = "0.163.0";
pub fn supported_cli_version(output: &str) -> bool { output.split_whitespace().any(|part| part == SUPPORTED_CLI_VERSION) }

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

use crate::accounts::{AccountKey, Provider, provider_folder};
use crate::model_selection::{AccountModelCatalog, CatalogModel};
use crate::usage::{Share, UsageReading, WindowKind, WindowReading};

const OUTPUT_LIMIT: u64 = 2 * 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

pub fn executable(path: &str) -> Option<PathBuf> {
    std::env::split_paths(path).map(|root| root.join("codex"))
        .find(|path| path.metadata().is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0))
}

pub fn prepare(home: &Path, key: &AccountKey) -> Result<Option<PathBuf>, String> {
    if key.provider != Provider::Codex { return Err("The account is not a Codex account".into()); }
    let Some(folder) = provider_folder(home, key) else { return Ok(None) };
    let managed = home.join(crate::accounts::MANAGED_PROVIDER_FOLDER);
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

pub fn command(executable: &Path, environment: &BTreeMap<String, String>, user_home: &Path, path: &str, username: &str, profile: Option<&Path>) -> Command {
    let mut command = Command::new(executable);
    if profile.is_some() { command.args(["-c", "cli_auth_credentials_store=\"file\""]); }
    command.env_clear().env("HOME", user_home).env("PATH", path).env("USER", username).env("LOGNAME", username).current_dir(user_home);
    for key in ["LANG", "LC_ALL", "TMPDIR", "SSL_CERT_FILE", "SSL_CERT_DIR", "HTTPS_PROXY", "HTTP_PROXY", "NO_PROXY"] {
        if let Some(value) = environment.get(key) { command.env(key, value); }
    }
    if let Some(profile) = profile.or_else(|| environment.get("CODEX_HOME").map(Path::new)) { command.env("CODEX_HOME", profile); }
    command
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProbeReport {
    pub requires_auth: bool,
    pub auth_method: Option<String>,
    pub account_type: Option<String>,
    pub workspace_id: Option<String>,
    pub signed_in: bool,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub pay_per_use: bool,
    pub rate_limits: Option<Value>,
    pub catalog: AccountModelCatalog,
    pub catalog_error: Option<String>,
}

impl ProbeReport {
    pub fn supported_authentication(&self) -> bool {
        matches!((self.auth_method.as_deref(), self.account_type.as_deref()), (Some("chatgpt"), Some("chatgpt")) | (Some("apikey"), Some("apiKey")))
    }

    pub fn native_identity(&self) -> Option<String> {
        self.workspace_id.clone()
    }

    pub fn ordinary_usage_allowed(&self) -> Option<bool> {
        self.rate_limits.as_ref()?.get("ordinaryUsageAllowed")?.as_bool()
    }
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

    pub(crate) fn notification(&mut self, wait: Duration) -> Result<Option<Value>, String> {
        self.check_cancelled()?;
        if let Some(message) = self.notifications.pop_front() { return Ok(Some(message)); }
        let left = self.until.checked_duration_since(Instant::now()).ok_or("Codex sign-in expired")?;
        match self.output.recv_timeout(wait.min(left)) {
            Ok(message) => message.map(Some),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err("Codex sign-in ended before completion".into()),
        }
    }

    fn check_cancelled(&self) -> Result<(), String> {
        if self.cancelled.as_ref().is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)) {
            return Err("Codex sign-in cancelled; the previous session was preserved".into());
        }
        Ok(())
    }

    pub(crate) fn extend_deadline(&mut self, timeout: Duration) { self.until = Instant::now() + timeout; }
}

pub fn probe(command: Command, account: AccountKey, now: crate::Millis) -> Result<ProbeReport, String> {
    probe_matching(command, account, now, |_| true)
}

/// Optional quota never prevents cached model metadata; refused identity publishes neither usage nor catalog.
pub fn probe_matching(command: Command, account: AccountKey, now: crate::Millis, accept_account: impl FnOnce(&ProbeReport) -> bool) -> Result<ProbeReport, String> {
    probe_with_diagnostics(command, account, now, accept_account, false)
}

/// Only isolated synthetic-account fixtures expose bounded native RPC errors; never return error data.
pub fn probe_fixture(command: Command, account: AccountKey, now: crate::Millis) -> Result<ProbeReport, String> {
    probe_with_diagnostics(command, account, now, |_| true, true)
}

fn probe_with_diagnostics(command: Command, account: AccountKey, now: crate::Millis, accept_account: impl FnOnce(&ProbeReport) -> bool, fixture_diagnostics: bool) -> Result<ProbeReport, String> {
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
    let revision = format!("{:x}", Sha256::digest(serde_json::to_vec(&models).map_err(|error| error.to_string())?));
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

pub fn parse_models(list: &Value) -> Result<Vec<CatalogModel>, String> {
    list["data"].as_array().ok_or("Codex model list has no data array")?.iter().map(|model| {
        let name = model["model"].as_str().or_else(|| model["id"].as_str()).filter(|model| !model.is_empty()).ok_or("Codex returned a model without an ID")?;
        Ok(CatalogModel { model: name.into(), display_name: model["displayName"].as_str().unwrap_or(name).into(),
            reasoning_efforts: model["supportedReasoningEfforts"].as_array().into_iter().flatten().filter_map(|entry| entry["reasoningEffort"].as_str().map(str::to_owned)).collect(),
            default_reasoning_effort: model["defaultReasoningEffort"].as_str().map(str::to_owned),
            context_window: model["contextWindow"].as_u64(), input_modalities: model["inputModalities"].as_array().into_iter().flatten().filter_map(|entry| entry.as_str().map(str::to_owned)).collect(),
            is_default: model["isDefault"].as_bool().unwrap_or(false), hidden: model["hidden"].as_bool().unwrap_or(false) })
    }).collect()
}

pub fn parse_rate_limits(value: &Value, plan: Option<String>) -> Result<UsageReading, String> {
    let mut windows = vec![];
    let mut append = |scope: &str, limits: &Value| -> Result<(), String> {
        for dimension in ["primary", "secondary"] {
            let window = &limits[dimension];
            let Some(percent) = window["usedPercent"].as_f64().filter(|used| used.is_finite() && (0.0..=100.0).contains(used)) else { continue };
            let duration = window["windowDurationMins"].as_u64().map_or_else(|| "unknown".into(), |minutes| minutes.to_string());
            let kind = WindowKind::Other(format!("codex:{scope}:{dimension}:{duration}"));
            windows.push(WindowReading { kind, used: Share::of(percent / 100.0),
                resets_at: window["resetsAt"].as_i64().and_then(|seconds| seconds.checked_mul(1000)), exhausted: percent >= 100.0 });
        }
        Ok(())
    };
    if let Some(common) = value.get("rateLimits").filter(|limits| !limits.is_null()) { append("account", common)?; }
    if let Some(buckets) = value.get("rateLimitsByLimitId").and_then(Value::as_object) {
        for (id, limits) in buckets {
            if id.contains(':') || id.chars().any(char::is_control) { return Err("Codex returned an invalid rate-limit ID".into()); }
            append(&format!("limit:{id}"), limits)?;
        }
    }
    let reading = UsageReading { windows, plan, complete: false };
    reading.check()?;
    Ok(reading)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::AccountId;
    use crate::test_support::Directory;

    #[test]
    fn named_codex_accounts_are_private_and_never_copy_native_auth() {
        let scratch = Directory::new();
        let home = scratch.join("alondra");
        let key = AccountKey::new(Provider::Codex, AccountId::parse("work").unwrap());
        let folder = prepare(&home, &key).unwrap().unwrap();
        assert_eq!(folder, home.join("provider-accounts/codex/work"));
        assert_eq!(fs::metadata(&folder).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::read_to_string(folder.join("config.toml")).unwrap(), "cli_auth_credentials_store = \"file\"\n");
        assert!(!folder.join("auth.json").exists());
        assert_eq!(prepare(&home, &key).unwrap(), Some(folder));
        assert_eq!(prepare(&home, &AccountKey::native(Provider::Codex)).unwrap(), None);
    }

    #[test]
    fn a_named_account_cannot_redirect_credentials_through_a_symlink() {
        let scratch = Directory::new();
        let home = scratch.join("alondra");
        let outside = scratch.join("outside");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, home.join(crate::accounts::MANAGED_PROVIDER_FOLDER)).unwrap();
        let key = AccountKey::new(Provider::Codex, AccountId::parse("work").unwrap());
        assert!(prepare(&home, &key).is_err());
        assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
    }

    #[test]
    fn rate_limit_windows_keep_native_durations_and_partial_reads_preserve_other_dimensions() {
        let reading = parse_rate_limits(&json!({"rateLimits":{"primary":{"usedPercent":100,"windowDurationMins":90,"resetsAt":1000},"secondary":{"usedPercent":20,"windowDurationMins":20160,"resetsAt":2000}}}), Some("plus".into())).unwrap();
        assert_eq!(reading.windows[0].kind.duration_minutes(), Some(90));
        assert_eq!(reading.windows[1].kind.duration_minutes(), Some(20160));
        assert!(reading.windows.iter().all(|window| matches!(window.kind, WindowKind::Other(_))));
        let key = AccountKey::native(Provider::Codex);
        let measured = crate::usage::record(None, key.provider, &key.account, &reading, 1, "2026-10-25");
        assert_eq!(measured.plan_exhausted_until(2), Some(1_000_000));
        let partial = parse_rate_limits(&json!({"rateLimits":{"primary":{"usedPercent":5,"windowDurationMins":60,"resetsAt":3000}}}), None).unwrap();
        let latest = crate::usage::record(Some(&measured), key.provider, &key.account, &partial, 2, "2026-10-25");
        assert_eq!(latest.windows.len(), 2);
        assert!(latest.windows.iter().any(|window| window.reading.kind.duration_minutes() == Some(60)));
        assert!(!latest.windows.iter().any(|window| window.reading.kind.duration_minutes() == Some(90)));
        assert!(latest.plan_exhausted_until(3).is_none());
        assert!(parse_rate_limits(&json!({}), None).unwrap().windows.is_empty());
    }

    #[test]
    fn codex_commands_do_not_inherit_an_api_key_or_claude_profile() {
        let env = BTreeMap::from([("OPENAI_API_KEY".into(), "must-not-inherit".into()), ("CLAUDE_CONFIG_DIR".into(), "/wrong".into()), ("CODEX_HOME".into(), "/native-codex".into())]);
        let command = command(Path::new("/fake/codex"), &env, Path::new("/user"), "/bin", "test", None);
        let env: BTreeMap<_, _> = command.get_envs().map(|(key,value)| (key.to_str().unwrap(), value.map(|value| value.to_str().unwrap()))).collect();
        assert!(!env.contains_key("OPENAI_API_KEY"));
        assert!(!env.contains_key("CLAUDE_CONFIG_DIR"));
        assert_eq!(env["CODEX_HOME"], Some("/native-codex"));
    }

    #[test]
    fn account_probe_uses_native_catalog_without_running_a_turn() {
        let scratch = Directory::new();
        fs::create_dir_all(&*scratch).unwrap();
        let fixture = scratch.join("codex.py");
        fs::write(&fixture, r#"import sys,json
for raw in sys.stdin:
 m=json.loads(raw)
 if 'id' not in m: continue
 method=m['method']
 if method=='initialize': result={'userAgent':'fixture'}
 elif method=='account/read': result={'requiresOpenaiAuth':True,'account':{'type':'chatgpt','email':'test@example.invalid','planType':'plus'}}
 elif method=='getAuthStatus':
  assert m['params']=={'includeToken':False,'refreshToken':False}
  result={'authMethod':'chatgpt','requiresOpenaiAuth':True,'authToken':None}
 elif method=='account/rateLimits/read': result={'accountId':'fixture-workspace','ordinaryUsageAllowed':True,'rateLimits':{'primary':{'usedPercent':20,'windowDurationMins':300,'resetsAt':1000}}}
 elif method=='model/list': result={'data':[{'model':'fixture-model','displayName':'Fixture','isDefault':True,'supportedReasoningEfforts':[{'reasoningEffort':'high'}],'defaultReasoningEffort':'high','inputModalities':['text']}],'nextCursor':None}
 else: raise RuntimeError('Unexpected method: '+method)
 print(json.dumps({'id':m['id'],'result':result}),flush=True)
"#).unwrap();
        let mut command = Command::new("/usr/bin/python3");
        command.arg(fixture).env_clear().env("HOME", &*scratch);
        let report = probe(command, AccountKey::native(Provider::Codex), 123).unwrap();
        assert!(report.signed_in);
        assert!(report.supported_authentication());
        assert_eq!(report.auth_method.as_deref(), Some("chatgpt"));
        assert_eq!(report.native_identity().as_deref(), Some("fixture-workspace"));
        assert_eq!(report.email.as_deref(), Some("test@example.invalid"));
        assert_eq!(report.catalog.native_default.as_deref(), Some("fixture-model"));
        assert_eq!(report.catalog.models[0].reasoning_efforts, ["high"]);
        assert_eq!(report.catalog.measured_at, 123);
    }
    #[test]
    fn account_authentication_modes_are_not_inferred_from_account_read_chatgpt_shape() {
        let scratch = Directory::new(); fs::create_dir_all(&*scratch).unwrap();
        let fixture = scratch.join("native.py");
        fs::write(&fixture, r#"import sys,json
for raw in sys.stdin:
 m=json.loads(raw)
 if 'id' not in m: continue
 method=m['method']
 if method=='initialize': result={}
 elif method=='account/read': result={'requiresOpenaiAuth':True,'account':None if sys.argv[1]=='signedout' else {'type':'apiKey'} if sys.argv[1]=='apikey' else {'type':'chatgpt','email':'test@example.invalid'}}
 elif method=='getAuthStatus':
  assert m['params']=={'includeToken':False,'refreshToken':False}
  result={'authMethod':None if sys.argv[1]=='signedout' else 'apikey' if sys.argv[1]=='apikey-cached' else sys.argv[1],'requiresOpenaiAuth':True,'authToken':None}
 elif method=='account/rateLimits/read': result={'accountId':'native-workspace','ordinaryUsageAllowed':True,'rateLimits':None}
 elif method=='model/list': result={'data':[],'nextCursor':None}
 else: raise RuntimeError('Unexpected inference or metadata method: '+method)
 print(json.dumps({'id':m['id'],'result':result}),flush=True)
"#).unwrap();
        for method in ["chatgpt", "apikey", "chatgptAuthTokens", "agentIdentity", "unknown", "apikey-cached", "signedout"] {
            let mut command = Command::new("/usr/bin/python3"); command.arg(&fixture).arg(method).env_clear().env("HOME", &*scratch);
            let report = probe(command, AccountKey::native(Provider::Codex), 10).unwrap();
            assert_eq!(report.signed_in, method != "signedout", "native account metadata and Alondra support are separate facts");
            assert_eq!(report.supported_authentication(), matches!(method, "chatgpt" | "apikey"));
            if matches!(method, "apikey" | "apikey-cached") { assert!(report.pay_per_use); assert_eq!(report.native_identity(), None, "API-key authentication has no known workspace identity even if stale account metadata has one"); }
            else if method == "signedout" { assert!(!report.pay_per_use, "signed-out metadata never invents API-key billing"); assert_eq!(report.native_identity(), None); }
            else if method == "chatgpt" { assert_eq!(report.workspace_id.as_deref(), Some("native-workspace")); }
            else { assert!(report.native_identity().is_none(), "unsupported auth mode never proves a managed account identity"); }
        }
    }

    #[test]
    fn identity_replacement_survives_missing_model_metadata_and_discards_unmatched_catalogs() {
        let scratch = Directory::new(); fs::create_dir_all(&*scratch).unwrap();
        let fixture = scratch.join("native.py");
        fs::write(&fixture, r#"import sys,json
for raw in sys.stdin:
 m=json.loads(raw)
 if 'id' not in m: continue
 method=m['method']
 if method=='initialize': result={}
 elif method=='account/read': result={'requiresOpenaiAuth':True,'account':{'type':'chatgpt','email':'other@example.invalid'}}
 elif method=='getAuthStatus':
  assert m['params']=={'includeToken':False,'refreshToken':False}
  result={'authMethod':'chatgpt','requiresOpenaiAuth':True,'authToken':None}
 elif method=='account/rateLimits/read': result={'accountId':'replacement-workspace','ordinaryUsageAllowed':True,'rateLimits':None}
 elif method=='model/list':
  print(json.dumps({'id':m['id'],'error':{'code':-32603,'message':'Native model metadata is unavailable'}}),flush=True)
  continue
 else: raise RuntimeError('Unexpected method: '+method)
 print(json.dumps({'id':m['id'],'result':result}),flush=True)
"#).unwrap();
        for mode in ["identity-only", "catalog-error"] {
            let mut command = Command::new("/usr/bin/python3"); command.arg(&fixture).arg(mode).env_clear().env("HOME", &*scratch);
            let report = probe_matching(command, AccountKey::native(Provider::Codex), 10, |_| mode == "catalog-error").unwrap();
            assert!(report.signed_in); assert_eq!(report.native_identity().as_deref(), Some("replacement-workspace"));
            assert!(report.catalog.models.is_empty()); assert!(report.catalog.revision.is_empty(), "an unobserved catalog cannot be published as an empty advertisement");
            assert_eq!(report.catalog_error.is_some(), mode == "catalog-error");
            if mode == "identity-only" { assert!(report.rate_limits.is_none(), "unmatched native metadata is never published as quota or a catalog"); }
        }
    }

    #[test]
    fn only_an_explicit_synthetic_fixture_exposes_bounded_native_rpc_diagnostics() {
        let scratch = Directory::new(); fs::create_dir_all(&*scratch).unwrap(); let fixture = scratch.join("native.py");
        fs::write(&fixture, r#"import sys,json
for raw in sys.stdin:
 m=json.loads(raw)
 if 'id' not in m: continue
 if m['method']=='initialize': answer={'result':{}}
 else: answer={'error':{'code':-32602,'message':'Fixture base URL requires HTTPS','data':{'accessToken':'never-print-error-data'}}}
 print(json.dumps(dict(id=m['id'],**answer)),flush=True)
"#).unwrap();
        let command = || { let mut command = Command::new("/usr/bin/python3"); command.arg(&fixture).env_clear().env("HOME", &*scratch); command };
        assert_eq!(probe(command(), AccountKey::native(Provider::Codex), 0).unwrap_err(), "Codex refused account/read");
        let error = probe_fixture(command(), AccountKey::native(Provider::Codex), 0).unwrap_err();
        assert!(error.contains("native RPC -32602: Fixture base URL requires HTTPS")); assert!(!error.contains("never-print-error-data") && !error.contains("accessToken"));
    }

    #[test]
    fn actual_account_schema_keeps_auth_and_models_usable_without_optional_quota_or_identity() {
        let scratch = Directory::new(); fs::create_dir_all(&*scratch).unwrap(); let fixture = scratch.join("native.py");
        fs::write(&fixture, r#"import sys,json
for raw in sys.stdin:
 m=json.loads(raw)
 if 'id' not in m: continue
 method=m['method']
 if method=='initialize': result={}
 elif method=='account/read': result={'account':{'type':'chatgpt','email':'first@example.test','planType':'plus'},'requiresOpenaiAuth':True}
 elif method=='getAuthStatus': result={'authMethod':'chatgpt','authToken':None,'requiresOpenaiAuth':True}
 elif method=='model/list': result={'data':[{'model':'fixture-model','displayName':'Fixture','isDefault':True}],'nextCursor':None}
 elif method=='account/rateLimits/read':
  if sys.argv[1]=='unavailable':
   print(json.dumps({'id':m['id'],'error':{'code':-32603,'message':'Optional usage unavailable'}}),flush=True);continue
  result={'accountId':'actual-account','ordinaryUsageAllowed':True if sys.argv[1]=='true' else False if sys.argv[1]=='false' else None,'rateLimits':{'primary':{'usedPercent':0},'secondary':{'usedPercent':0}}}
 else: raise RuntimeError('Unexpected method: '+method)
 print(json.dumps({'id':m['id'],'result':result}),flush=True)
"#).unwrap();
        for mode in ["unavailable", "unknown", "false", "true"] {
            let mut command = Command::new("/usr/bin/python3"); command.arg(&fixture).arg(mode).env_clear().env("HOME", &*scratch);
            let report = probe(command, AccountKey::native(Provider::Codex), 1).unwrap();
            assert!(report.signed_in && report.supported_authentication()); assert_eq!(report.email.as_deref(), Some("first@example.test"));
            assert_eq!(report.catalog.native_default.as_deref(), Some("fixture-model")); assert_eq!(report.catalog.models.len(), 1);
            assert_eq!(report.native_identity().as_deref(), matches!(mode, "true" | "false").then_some("actual-account"));
            assert_eq!(report.ordinary_usage_allowed(), match mode { "true" => Some(true), "false" => Some(false), _ => None });
        }
    }

    #[test]
    fn named_codex_profiles_and_a_legacy_claude_account_called_codex_never_share_or_delete_data() {
        let scratch = Directory::new(); let home = scratch.join("home");
        let legacy = crate::accounts::folder(&home, &AccountId::parse("codex").unwrap()).unwrap();
        fs::create_dir_all(legacy.join("projects")).unwrap();
        let history = legacy.join("projects/native-history"); fs::write(&history, "retained Claude history").unwrap();
        let key = AccountKey::new(Provider::Codex, AccountId::parse("projects").unwrap());
        let named = prepare(&home, &key).unwrap().unwrap();
        fs::write(named.join("codex-state"), "retained Codex state").unwrap();
        fs::remove_dir_all(&named).unwrap();
        assert_eq!(fs::read_to_string(&history).unwrap(), "retained Claude history", "removing Codex/projects must not remove Claude/codex/projects");
        let named = prepare(&home, &key).unwrap().unwrap();
        fs::write(named.join("codex-state"), "retained Codex state").unwrap();
        crate::claude_account::remove(&home, &AccountId::parse("codex").unwrap()).unwrap();
        assert_eq!(fs::read_to_string(named.join("codex-state")).unwrap(), "retained Codex state", "removing Claude/codex must not remove any Codex profile");
        assert!(provider_folder(&home, &AccountKey::native(Provider::Codex)).is_none());
    }


}

#[cfg(test)]
#[test]
fn supported_codex_release_requires_an_exact_version_token() {
    assert!(supported_cli_version("codex-cli 0.163.0\n"));
    for output in ["codex-cli 0.163.01","codex-cli 0.163.0-dev","codex-cli 0.162.0",""] { assert!(!supported_cli_version(output),"{output}"); }
}
