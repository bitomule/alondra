use super::*;
use crate::test_support::Directory;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;

const NATIVE: &str = r#"#!/usr/bin/python3
import json,os,sys,socket
from pathlib import Path
home=Path(os.environ['HOME']); mode=(home/'mode').read_text()
profile=Path(os.environ.get('CODEX_HOME',str(home/'.codex')))
if 'login' in sys.argv:
 (home/'.codex/auth.json').unlink(missing_ok=True)
 raise AssertionError('CLI login revokes the old credential')
for line in sys.stdin:
 request=json.loads(line); method=request['method']
 with (home/'calls').open('a') as log: log.write(method+'\n')
 if 'id' not in request: continue
 signed=mode in ['existing','existing-unconnected'] or (profile/'auth.json').exists() and (profile/'auth.json').read_bytes() in [b'new credential',b'concurrent native session']
 if method=='initialize': result={}
 elif method=='account/read': result={'account':{'type':'chatgpt','email':'fixture@example.test','planType':'plus'} if signed else None,'requiresOpenaiAuth':True}
 elif method=='getAuthStatus':
  assert request['params']=={'includeToken':False,'refreshToken':False}
  result={'authMethod':'chatgpt' if signed else None}
 elif method=='config/read':
  if mode=='race': (home/'.codex/auth.json').write_bytes(b'concurrent native session')
  result={'config':{'cli_auth_credentials_store':'file'}}
 elif method=='model/list': result={'data':[{'id':'model','model':'model','displayName':'Model','isDefault':True,'hidden':False,'supportedReasoningEfforts':[]}]}
 elif method=='account/rateLimits/read': result={}
 elif method=='account/login/start':
  assert mode not in ['existing','existing-unconnected'], 'login started despite a native session'
  assert os.environ.get('CODEX_HOME') and profile != home/'.codex'
  assert not (profile/'auth.json').exists(), 'login received the old credential'
  if mode=='blocked':
   gate=socket.socket(socket.AF_UNIX); gate.connect(str(home/'gate')); gate.sendall(b'started'); sys.stdin.read(); break
  result={'loginId':'device-login','verificationUrl':'https://auth.openai.com/codex/device','userCode':'ABCD-1234'}
  print(json.dumps({'id':request['id'],'result':result}),flush=True)
  if mode=='complete' or mode=='gated-complete':
   if mode=='gated-complete':
    gate=socket.socket(socket.AF_UNIX); gate.connect(str(home/'gate')); gate.recv(1); gate.close()
   (profile/'auth.json').write_bytes(b'new credential')
   print(json.dumps({'method':'account/login/completed','params':{'loginId':'device-login','success':True}}),flush=True)
  continue
 elif method=='account/login/cancel': result={}
 else: raise AssertionError(method)
 print(json.dumps({'id':request['id'],'result':result}),flush=True)
"#;

struct Fixture { manager: Arc<SignIns>, user: std::path::PathBuf, _directory: Directory }

impl Drop for Fixture {
    fn drop(&mut self) {
        for cancelled in self.manager.workers.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).values() { cancelled.store(true, Ordering::Release); }
        let threads = std::mem::take(&mut *self.manager.threads.lock().unwrap());
        for thread in threads { let _ = thread.join(); }
    }
}

fn fixture(mode: &str) -> Fixture {
    let directory = Directory::new();
    let home = directory.join("alondra"); let user = directory.join("user");
    std::fs::create_dir_all(&home).unwrap(); std::fs::create_dir_all(user.join(".codex")).unwrap();
    std::fs::write(user.join("mode"), mode).unwrap();
    std::fs::write(user.join(".codex/auth.json"), b"prior credential").unwrap();
    let native = directory.join("codex"); std::fs::write(&native, NATIVE).unwrap();
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut config = alondra_core::accounts::AccountsConfig::fresh();
    if mode != "existing-unconnected" { config.connect(Provider::Codex); }
    std::fs::write(alondra_core::options::path_in(&home), config.to_toml()).unwrap();
    let domain = Arc::new(DomainRuntime::open(&home).unwrap());
    domain.enable_sdk(crate::sdk_owner::SdkBackend { provider: Provider::Codex, origin: None, native,
        node: directory.join("unused-node"), host: directory.join("unused-host"), node_sha256: String::new(), host_sha256: String::new(), native_sha256: String::new(),
        host_environment: BTreeMap::new(), native_environment: BTreeMap::from([("HOME".into(), user.to_string_lossy().into_owned()), ("PATH".into(), "/usr/bin:/bin".into())]),
        confinement: None, state_root: directory.join("host-state"), io_timeout_seconds: 30, installed_native: false });
    Fixture { manager: SignIns::new(domain), _directory: directory, user }
}

fn actor() -> Actor { Actor::User { user_id: "fixture-user".into() } }
fn start(f: &Fixture) -> AccountLogin { f.manager.start(&actor(), AccountKey::native(Provider::Codex), Arc::new(|| true), true).unwrap() }
fn idle(manager: &SignIns) {
    let workers = manager.workers.lock().unwrap();
    let (workers, _) = manager.settled.wait_timeout_while(workers, Duration::from_secs(30), |workers| !workers.is_empty()).unwrap();
    let finished = workers.is_empty();
    drop(workers);
    assert!(finished, "sign-in worker did not finish");
    let threads = std::mem::take(&mut *manager.threads.lock().unwrap());
    for thread in threads { thread.join().unwrap(); }
}
async fn phase(f: &Fixture, expected: AccountLoginPhase) {
    let mut changes = f.manager.domain.subscribe();
    loop {
        let current = f.manager.domain.read().state().account_logins[0].clone();
        if current.phase == expected { return; }
        assert!(current.phase.active(), "sign-in ended before {expected:?}: {current:?}");
        changes.changed().await.unwrap();
    }
}

#[tokio::test]
async fn codex_login_existing_native_session_connects_without_any_login() {
    for legacy in [false, true] {
        let f = fixture("existing-unconnected");
        if legacy {
            let mut config = alondra_core::accounts::AccountsConfig::fresh(); config.connected = None;
            std::fs::write(alondra_core::options::path_in(f.manager.domain.home()), config.to_toml()).unwrap();
        }
        start(&f); idle(&f.manager);
        assert!(crate::accounts::config(f.manager.domain.home()).unwrap().is_connected(Provider::Codex), "{:?}", f.manager.domain.read().state().account_logins);
        let calls = std::fs::read_to_string(f.user.join("calls")).unwrap();
        assert!(!calls.contains("account/login/start"), "{calls}");
        assert_eq!(f.manager.domain.read().state().account_logins[0].phase, AccountLoginPhase::Connected);
        assert_eq!(std::fs::read(f.user.join(".codex/auth.json")).unwrap(), b"prior credential");
    }
}

#[tokio::test]
async fn codex_login_noninteractive_connection_reuses_session_and_never_starts_authentication() {
    for mode in ["existing", "pending"] {
        let f = fixture(mode);
        f.manager.start(&actor(), AccountKey::native(Provider::Codex), Arc::new(|| true), false).unwrap(); idle(&f.manager);
        assert_eq!(f.manager.domain.read().state().account_logins[0].phase, if mode == "existing" { AccountLoginPhase::Connected } else { AccountLoginPhase::Failed });
        assert!(!std::fs::read_to_string(f.user.join("calls")).unwrap().contains("account/login/start"));
        assert_eq!(std::fs::read(f.user.join(".codex/auth.json")).unwrap(), b"prior credential");
    }
}

#[tokio::test]
async fn codex_login_cancel_preserves_prior_native_session_and_removes_staging() {
    let f = fixture("pending"); let login = start(&f); phase(&f, AccountLoginPhase::Pending).await;
    let current = f.manager.domain.read().state().account_logins[0].clone();
    assert_eq!(current.challenge.unwrap().code, "ABCD-1234");
    f.manager.cancel(&actor(), login.account, login.attempt).unwrap(); idle(&f.manager);
    assert_eq!(std::fs::read(f.user.join(".codex/auth.json")).unwrap(), b"prior credential");
    assert_eq!(std::fs::read_dir(f.manager.domain.home().join("account-sign-ins")).unwrap().count(), 0);
}

#[tokio::test]
async fn codex_login_cancel_reaps_a_native_start_that_never_answers() {
    let f = fixture("blocked"); let listener = UnixListener::bind(f.user.join("gate")).unwrap();
    let login = start(&f); let (mut gate, _) = listener.accept().unwrap();
    let mut bytes = [0; 7]; gate.read_exact(&mut bytes).unwrap();
    f.manager.cancel(&actor(), login.account, login.attempt).unwrap(); idle(&f.manager);
    assert_eq!(std::fs::read(f.user.join(".codex/auth.json")).unwrap(), b"prior credential");
    assert_eq!(gate.read(&mut bytes).unwrap(), 0, "the blocked process was reaped");
}

#[tokio::test]
async fn codex_login_new_native_session_between_probe_and_snapshot_is_reused() {
    let f = fixture("race"); start(&f); idle(&f.manager);
    assert_eq!(f.manager.domain.read().state().account_logins[0].phase, AccountLoginPhase::Connected);
    assert_eq!(std::fs::read(f.user.join(".codex/auth.json")).unwrap(), b"concurrent native session");
    assert!(!std::fs::read_to_string(f.user.join("calls")).unwrap().contains("account/login/start"));
}

#[tokio::test]
async fn codex_login_success_replaces_credential_only_after_native_confirmation() {
    let f = fixture("complete"); start(&f); idle(&f.manager);
    assert_eq!(f.manager.domain.read().state().account_logins[0].phase, AccountLoginPhase::Connected);
    assert_eq!(std::fs::read(f.user.join(".codex/auth.json")).unwrap(), b"new credential");
}

#[tokio::test]
async fn codex_login_revoked_device_cannot_publish_a_completed_credential() {
    let f = fixture("gated-complete"); let listener = UnixListener::bind(f.user.join("gate")).unwrap();
    let allowed = Arc::new(AtomicBool::new(true)); let check = allowed.clone();
    f.manager.start(&actor(), AccountKey::native(Provider::Codex), Arc::new(move || check.load(Ordering::Acquire)), true).unwrap();
    let (mut gate, _) = listener.accept().unwrap(); phase(&f, AccountLoginPhase::Pending).await;
    allowed.store(false, Ordering::Release); let _ = gate.write_all(b"x"); idle(&f.manager);
    assert_eq!(f.manager.domain.read().state().account_logins[0].phase, AccountLoginPhase::Failed);
    assert_eq!(std::fs::read(f.user.join(".codex/auth.json")).unwrap(), b"prior credential");
}

#[tokio::test]
async fn codex_login_restart_settles_abandoned_operation_and_removes_private_staging() {
    let f = fixture("pending"); let account = AccountKey::native(Provider::Codex);
    let attempt = "0123456789abcdef0123456789abcdef";
    f.manager.domain.execute_with(&actor(), &envelope(&f.manager.domain, Command::BeginAccountLogin { account, attempt: attempt.into(), started_at: 1 }), None, None, || Ok(())).unwrap();
    let root = f.manager.domain.home().join("account-sign-ins"); private_root(&root).unwrap();
    let path = root.join(attempt); std::fs::create_dir(&path).unwrap(); std::fs::write(path.join("auth.json"), b"abandoned synthetic credential").unwrap();
    let pending = f.user.join(".codex").join(format!(".alondra-auth-{attempt}")); std::fs::write(&pending, b"abandoned pending credential").unwrap();
    let restarted = Arc::new(DomainRuntime::open(f.manager.domain.home()).unwrap());
    let recovered = SignIns::new(restarted.clone());
    assert!(pending.exists(), "native profiles are resolved only after the backend is ready");
    restarted.enable_sdk(f.manager.domain.sdk().unwrap().0.clone());
    recovered.recover_after_backend(); idle(&recovered);
    assert_eq!(recovered.domain.read().state().account_logins[0].phase, AccountLoginPhase::Failed);
    assert!(!path.exists()); assert!(!pending.exists());
    assert_eq!(std::fs::read(f.user.join(".codex/auth.json")).unwrap(), b"prior credential");
}

#[tokio::test]
async fn codex_login_restart_finishes_a_committed_publication_using_native_session_status() {
    let f = fixture("pending"); let account = AccountKey::native(Provider::Codex);
    f.manager.domain.execute_with(&actor(), &envelope(&f.manager.domain, Command::BeginAccountLogin { account, attempt: "attempt".into(), started_at: 1 }), None, None, || Ok(())).unwrap();
    let mut login = f.manager.domain.read().state().account_logins[0].clone(); login.phase = AccountLoginPhase::Publishing;
    f.manager.domain.execute_as_system(envelope(&f.manager.domain, Command::RecordAccountLogin { login })).unwrap();
    std::fs::write(f.user.join(".codex/auth.json"), b"new credential").unwrap();
    let restarted = Arc::new(DomainRuntime::open(f.manager.domain.home()).unwrap()); let recovered = SignIns::new(restarted.clone());
    assert_eq!(recovered.domain.read().state().account_logins[0].phase, AccountLoginPhase::Publishing);
    restarted.enable_sdk(f.manager.domain.sdk().unwrap().0.clone()); recovered.recover_after_backend(); idle(&recovered);
    assert_eq!(recovered.domain.read().state().account_logins[0].phase, AccountLoginPhase::Connected);
    assert_eq!(std::fs::read(f.user.join(".codex/auth.json")).unwrap(), b"new credential");
}
