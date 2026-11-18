use super::*;
use alondra_core::codex_account::*;
use alondra_core::accounts::AccountId;
use crate::test_support::Directory;

struct RetainedThreadFixture {
    _root: Directory,
    executable: PathBuf,
    environment: BTreeMap<String, String>,
    directory: PathBuf,
    rollout: PathBuf,
    metadata: PathBuf,
    trace: PathBuf,
}

impl RetainedThreadFixture {
    fn new() -> Self {
        let root = Directory::new();
        let directory = root.join("repo");
        fs::create_dir_all(&directory).unwrap();
        let executable = root.join("native.py");
        fs::write(&executable, r#"#!/usr/bin/python3
import json, os, sys
for line in sys.stdin:
    request = json.loads(line)
    with open(os.environ['THREAD_FIXTURE_TRACE'], 'a') as trace: trace.write(json.dumps(request) + '\n')
    method = request['method']
    if method == 'initialized': continue
    if method == 'initialize': result = {}
    elif method == 'thread/read':
        assert request['params'] == {'threadId':'retained-thread', 'includeTurns':False}
        with open(os.environ['THREAD_FIXTURE_METADATA']) as source: result = json.load(source)
        if 'error' in result:
            print(json.dumps({'id':request['id'], 'error':result['error']}), flush=True)
            continue
    else: raise RuntimeError('The metadata probe must not open a session, turn, or inference request')
    print(json.dumps({'id':request['id'], 'result':result}), flush=True)
"#).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let rollout = root.join("rollout.jsonl");
        fs::write(&rollout, "Retained native history\n").unwrap();
        let metadata = root.join("metadata.json");
        let trace = root.join("requests.jsonl");
        let mut environment = crate::test_support::fixture_environment(&root);
        environment.extend([
            ("THREAD_FIXTURE_METADATA".into(), metadata.display().to_string()), ("THREAD_FIXTURE_TRACE".into(), trace.display().to_string()),
        ]);
        Self { _root: root, executable, environment, directory, rollout, metadata, trace }
    }

    fn record(&self) -> Value { json!({"thread":{"id":"retained-thread", "cwd":self.directory, "path":self.rollout}}) }
    fn check(&self, record: &Value) -> Result<bool, String> {
        fs::write(&self.metadata, serde_json::to_vec(record).unwrap()).unwrap();
        retained_thread(&self.executable, &self.environment, "retained-thread", &self.directory)
    }
}

#[test]
fn retained_thread_probe_reads_only_metadata_and_preserves_the_exact_identity_and_directory() {
    let f = RetainedThreadFixture::new();
    assert!(f.check(&f.record()).unwrap());
    let requests: Vec<Value> = fs::read_to_string(&f.trace).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(requests.iter().map(|request| request["method"].as_str().unwrap()).collect::<Vec<_>>(), ["initialize", "initialized", "thread/read"]);
    assert_eq!(requests[2]["params"], json!({"threadId":"retained-thread", "includeTurns":false}));
    for (field, value) in [("id", json!("another-thread")), ("cwd", json!("/another/repo")), ("path", Value::Null), ("path", json!("relative/rollout"))] {
        let mut record = f.record(); record["thread"][field] = value;
        assert!(!f.check(&record).unwrap(), "{record}");
    }
}

#[test]
fn stale_empty_and_symbolic_rollout_files_do_not_prove_a_retained_thread() {
    let f = RetainedThreadFixture::new();
    fs::remove_file(&f.rollout).unwrap();
    assert!(!f.check(&f.record()).unwrap());
    fs::write(&f.rollout, "").unwrap();
    assert!(!f.check(&f.record()).unwrap());
    fs::remove_file(&f.rollout).unwrap();
    let other = f._root.join("other-rollout"); fs::write(&other, "History\n").unwrap();
    std::os::unix::fs::symlink(other, &f.rollout).unwrap();
    assert!(!f.check(&f.record()).unwrap());
}

#[test]
fn an_unavailable_thread_is_not_recovered_and_native_errors_are_not_exposed() {
    let f = RetainedThreadFixture::new();
    let error = f.check(&json!({"error":{"code":-32603,"message":"Sensitive native thread detail"}})).unwrap_err();
    assert_eq!(error, "Codex refused thread/read");
    assert!(!retained_thread(&f.executable, &BTreeMap::new(), "retained-thread", &f.directory).unwrap());
}

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
    std::os::unix::fs::symlink(&outside, home.join(alondra_core::accounts::MANAGED_PROVIDER_FOLDER)).unwrap();
    let key = AccountKey::new(Provider::Codex, AccountId::parse("work").unwrap());
    assert!(prepare(&home, &key).is_err());
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
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
    command.arg(fixture).env_clear().env("HOME", &*scratch).envs(crate::test_support::xcrun_cache());
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
        let mut command = Command::new("/usr/bin/python3"); command.arg(&fixture).arg(method).env_clear().env("HOME", &*scratch).envs(crate::test_support::xcrun_cache());
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
        let mut command = Command::new("/usr/bin/python3"); command.arg(&fixture).arg(mode).env_clear().env("HOME", &*scratch).envs(crate::test_support::xcrun_cache());
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
    let command = || { let mut command = Command::new("/usr/bin/python3"); command.arg(&fixture).env_clear().env("HOME", &*scratch).envs(crate::test_support::xcrun_cache()); command };
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
        let mut command = Command::new("/usr/bin/python3"); command.arg(&fixture).arg(mode).env_clear().env("HOME", &*scratch).envs(crate::test_support::xcrun_cache());
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
    let legacy = alondra_core::accounts::folder(&home, &AccountId::parse("codex").unwrap()).unwrap();
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

/// The doubled accounts the engine's tests run on answer for Codex what the repository's fake Codex
/// answers this probe, so a test on the double sees what one on the fake would.
#[test]
fn the_doubles_codex_answers_what_the_repositorys_fake_codex_does() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = Directory::new();
    fs::create_dir_all(scratch.join("codex-home")).unwrap();
    let native = scratch.join("codex-native");
    fs::copy(std::env::var("FIXTURE_CODEX_NATIVE").unwrap(), &native).unwrap();
    fs::set_permissions(&native, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = Command::new(&native);
    command.env_clear().env("HOME", &*scratch).env("CODEX_HOME", scratch.join("codex-home")).env("PATH", "/usr/bin:/bin");
    let key = AccountKey::native(Provider::Codex);
    let real = probe_fixture(command, key.clone(), 7).unwrap();
    let double = alondra_engine_doubles::accounts::codex_fixture();
    assert_eq!((real.requires_auth, &real.auth_method, &real.account_type, &real.workspace_id, real.signed_in, &real.email, &real.plan, real.pay_per_use),
        (double.requires_auth, &double.auth_method, &double.account_type, &double.workspace_id, double.signed_in, &double.email, &double.plan, double.pay_per_use));
    assert_eq!(real.rate_limits, double.rate_limits);
    assert_eq!((&real.catalog.models, &real.catalog.native_default), (&double.catalog.models, &double.catalog.native_default));
}
