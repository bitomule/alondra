//! Managed device login never invokes the CLI login, which revokes the previous credential.
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use serde_json::{Value, json};
use crate::codex_account::Probe;

pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

pub fn verification_url(value: &str) -> bool {
    matches!(value, "https://auth.openai.com/codex/device" | "https://auth.openai.com/codex/device/")
}

pub struct Authentication {
    pub signed_in: bool,
    pub storage: String,
    pub restrictions: Vec<(String, Value)>,
}

fn initialize(process: &mut Probe) -> Result<(), String> {
    process.rpc("initialize", json!({"clientInfo":{"name":"alondra","title":"Alondra","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
    process.send(json!({"method":"initialized"}))
}

pub fn authentication(command: Command) -> Result<Authentication, String> {
    authentication_with_cancel(command, None)
}

pub fn authentication_with_cancel(command: Command, cancelled: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) -> Result<Authentication, String> {
    let mut process = Probe::start(command, Duration::from_secs(30))?;
    process.cancelled = cancelled;
    initialize(&mut process)?;
    let account = process.rpc("account/read", json!({"refreshToken":false}))?;
    let auth = process.rpc("getAuthStatus", json!({"includeToken":false,"refreshToken":false}))?;
    let kind = account["account"]["type"].as_str();
    let mode = auth["authMethod"].as_str();
    let signed_in = match (mode, kind) {
        (Some("chatgpt"), Some("chatgpt")) | (Some("apikey"), Some("apiKey")) => true,
        (None, None) if account.get("account").is_some_and(Value::is_null) => false,
        _ => return Err("The native Codex authentication cannot be connected safely".into()),
    };
    // An existing session needs no storage inspection, migration or authentication ceremony.
    if signed_in { return Ok(Authentication { signed_in, storage: String::new(), restrictions: vec![] }); }
    let config = process.rpc("config/read", json!({"includeLayers":false}))?;
    let config = &config["config"];
    let storage = config["cli_auth_credentials_store"].as_str().unwrap_or("file").to_owned();
    if !matches!(storage.as_str(), "file" | "keyring" | "auto") || (storage != "file" && config["features"]["secret_auth_storage"] == true) {
        return Err("This Codex credential store cannot be updated safely; the existing credentials were left unchanged".into());
    }
    let restrictions = ["forced_login_method", "forced_chatgpt_workspace_id"].into_iter()
        .filter_map(|name| config.get(name).filter(|value| !value.is_null()).map(|value| (name.to_owned(), value.clone()))).collect();
    Ok(Authentication { signed_in, storage, restrictions })
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Challenge {
    pub url: String,
    pub code: String,
}

pub struct DeviceLogin {
    process: Probe,
    login_id: String,
    pub challenge: Challenge,
}

impl DeviceLogin {
    pub fn start(command: Command) -> Result<Self, String> {
        Self::start_with_cancel(command, None)
    }

    pub fn start_with_cancel(command: Command, cancelled: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>) -> Result<Self, String> {
        let mut process = Probe::start(command, Duration::from_secs(30))?;
        process.cancelled = cancelled;
        initialize(&mut process)?;
        let response = process.rpc("account/login/start", json!({"type":"chatgptDeviceCode"}))?;
        let login_id = response["loginId"].as_str().filter(|id| !id.is_empty() && id.len() <= 256).ok_or("Codex did not return a sign-in identifier")?.to_owned();
        let url = response["verificationUrl"].as_str().filter(|url| verification_url(url)).ok_or("Codex did not return a safe verification page")?.to_owned();
        let code = response["userCode"].as_str().filter(|code| !code.is_empty() && code.len() <= 64 && code.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')).ok_or("Codex did not return a verification code")?.to_owned();
        process.extend_deadline(LOGIN_TIMEOUT);
        Ok(Self { process, login_id, challenge: Challenge { url, code } })
    }

    /// None while pending; provider error strings are never exposed, as they may contain secrets.
    pub fn poll(&mut self, wait: Duration) -> Result<Option<()>, String> {
        let Some(message) = self.process.notification(wait)? else { return Ok(None) };
        if message["method"] != "account/login/completed" || message["params"]["loginId"].as_str() != Some(&self.login_id) { return Ok(None); }
        if message["params"]["success"] != true { return Err("Codex sign-in did not complete; the previous session was preserved".into()); }
        Ok(Some(()))
    }

    pub fn cancel(&mut self) {
        let _ = self.process.send(json!({"id":"cancel-login","method":"account/login/cancel","params":{"loginId":self.login_id}}));
    }
}

/// A private empty profile; no prior refresh token is ever supplied to a login process.
pub struct Staging {
    path: std::path::PathBuf,
}

impl Staging {
    pub fn create(path: &Path) -> Result<Self, String> {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new().mode(0o700).create(path).map_err(|_| "Could not create a private Codex sign-in folder")?;
        Ok(Self { path: path.to_owned() })
    }

    pub fn path(&self) -> &Path { &self.path }

    pub fn credential(&self) -> Result<Vec<u8>, String> {
        use std::io::Read as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(self.path.join("auth.json")).map_err(|_| "Codex did not save the new session")?;
        let metadata = file.metadata().map_err(|_| "Could not check the new Codex session")?;
        if !metadata.is_file() || metadata.len() > 1024 * 1024 { return Err("The new Codex credential is invalid".into()); }
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| "Could not read the new Codex session")?;
        if bytes.is_empty() || bytes.len() > 1024 * 1024 { return Err("The new Codex credential is invalid".into()); }
        Ok(bytes)
    }
}

impl Drop for Staging {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.path); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Directory;
    const FAKE: &str = r#"import json,os,sys
from pathlib import Path
profile=Path(os.environ['CODEX_HOME'])
mode=sys.argv[1]
for line in sys.stdin:
 request=json.loads(line); method=request['method']
 with open(os.environ['CALLS'],'a') as calls: calls.write(method+'\n')
 if 'id' not in request: continue
 if method=='initialize': result={}
 elif method=='account/read': result={'account':{'type':'chatgpt','email':'fixture@example.test'} if mode=='existing' or (profile/'auth.json').exists() else None,'requiresOpenaiAuth':True}
 elif method=='getAuthStatus':
  assert request['params']=={'includeToken':False,'refreshToken':False}
  result={'authMethod':'chatgpt' if mode=='existing' or (profile/'auth.json').exists() else None}
 elif method=='config/read': result={'config':{'cli_auth_credentials_store':'keyring' if mode=='encrypted' else 'file','features':{'secret_auth_storage':mode=='encrypted'}}}
 elif method=='account/login/start':
  assert request['params']=={'type':'chatgptDeviceCode'}
  assert not (profile/'auth.json').exists(), 'login must never receive the prior credential'
  result={'loginId':'attempt','verificationUrl':'https://auth.openai.com/codex/device','userCode':'ABCD-1234'}
  if mode in ['complete','failed']:
   (profile/'auth.json').write_bytes(b'new synthetic credential')
   print(json.dumps({'method':'account/login/completed','params':{'loginId':'attempt','success':mode=='complete'}}),flush=True)
 elif method=='account/login/cancel': result={}
 else: raise AssertionError(method)
 if mode=='probe-error' and method=='account/read': print(json.dumps({'id':request['id'],'error':{'code':-1,'message':'not a session state'}}),flush=True)
 else: print(json.dumps({'id':request['id'],'result':result}),flush=True)
"#;

    fn command(directory: &Path, profile: &Path, mode: &str) -> Command {
        let script = directory.join("codex.py");
        std::fs::write(&script, FAKE).unwrap();
        let mut command = Command::new("/usr/bin/python3");
        command.args([script.to_str().unwrap(), mode]).env_clear().env("CODEX_HOME", profile).env("CALLS", directory.join("calls"));
        command
    }

    #[test]
    fn codex_login_existing_session_never_starts_authentication_or_reads_a_token() {
        let directory = Directory::new(); std::fs::create_dir_all(&directory).unwrap();
        let auth = authentication(command(&directory, &directory, "existing")).unwrap();
        assert!(auth.signed_in);
        assert_eq!(std::fs::read_to_string(directory.join("calls")).unwrap().lines().collect::<Vec<_>>(), ["initialize", "initialized", "account/read", "getAuthStatus"]);
    }

    #[test]
    fn codex_login_verification_link_rejects_lookalikes_and_credentials() {
        assert!(verification_url("https://auth.openai.com/codex/device"));
        for url in ["http://auth.openai.com/codex/device", "https://auth.openai.com.evil.test/codex/device", "https://auth.openai.com@evil.test/codex/device", "https://auth.openai.com/codex/device?secret=x", "https://evil.test/"] { assert!(!verification_url(url)); }
    }

    #[test]
    fn codex_login_probe_failure_is_not_a_signed_out_session() {
        let directory = Directory::new(); std::fs::create_dir_all(&directory).unwrap();
        assert!(authentication(command(&directory, &directory, "probe-error")).is_err());
        assert!(!std::fs::read_to_string(directory.join("calls")).unwrap().contains("account/login/start"));
    }

    #[test]
    fn codex_login_unsupported_encrypted_storage_is_refused_before_authentication() {
        let directory = Directory::new(); std::fs::create_dir_all(&directory).unwrap();
        assert!(authentication(command(&directory, &directory, "encrypted")).is_err());
        assert!(!std::fs::read_to_string(directory.join("calls")).unwrap().contains("account/login/start"));
    }

    #[test]
    fn codex_login_interrupted_staging_keeps_previous_credential_byte_for_byte() {
        let directory = Directory::new(); std::fs::create_dir_all(&directory).unwrap();
        let old = directory.join("auth.json"); std::fs::write(&old, b"previous synthetic session").unwrap();
        let path = directory.join("staged");
        {
            let staging = Staging::create(&path).unwrap();
            let mut login = DeviceLogin::start(command(&directory, staging.path(), "pending")).unwrap();
            assert_eq!(login.challenge.code, "ABCD-1234");
            login.cancel();
        }
        assert!(!path.exists());
        assert_eq!(std::fs::read(&old).unwrap(), b"previous synthetic session");
    }

    #[test]
    fn codex_login_completion_before_rpc_response_is_retained_and_only_staged() {
        let directory = Directory::new(); std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("auth.json"), b"previous session").unwrap();
        let staging = Staging::create(&directory.join("staged")).unwrap();
        let mut login = DeviceLogin::start(command(&directory, staging.path(), "complete")).unwrap();
        assert_eq!(login.poll(Duration::ZERO).unwrap(), Some(()));
        assert_eq!(staging.credential().unwrap(), b"new synthetic credential");
        assert_eq!(std::fs::read(directory.join("auth.json")).unwrap(), b"previous session");
    }

    #[test]
    fn codex_login_provider_error_discards_even_a_partially_written_new_credential() {
        let directory = Directory::new(); std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("auth.json"), b"previous session").unwrap();
        let path = directory.join("staged");
        {
            let staging = Staging::create(&path).unwrap();
            let mut login = DeviceLogin::start(command(&directory, staging.path(), "failed")).unwrap();
            assert!(login.poll(Duration::ZERO).is_err());
            assert!(staging.path().join("auth.json").exists());
        }
        assert!(!path.exists());
        assert_eq!(std::fs::read(directory.join("auth.json")).unwrap(), b"previous session");
    }

    #[test]
    fn codex_login_expired_device_flow_keeps_previous_credential_and_cleans_staging() {
        let directory = Directory::new(); std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("auth.json"), b"previous session").unwrap();
        let path = directory.join("staged");
        {
            let staging = Staging::create(&path).unwrap();
            let mut login = DeviceLogin::start(command(&directory, staging.path(), "pending")).unwrap();
            login.process.extend_deadline(Duration::ZERO);
            assert!(login.poll(Duration::ZERO).is_err());
        }
        assert!(!path.exists());
        assert_eq!(std::fs::read(directory.join("auth.json")).unwrap(), b"previous session");
    }
}
