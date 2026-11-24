use super::*;
use crate::test_support::Directory;
use alondra_core::accounts::AccountId;
use alondra_core::antigravity_account::{Launch, Settings};
use std::collections::BTreeMap;

/// A fake `agy` written for the test: it answers `--version`, the ACP methods a check sends, and
/// `-p /usage`, as `FAKE_AGY` (a JSON object in its environment) says, and records every request
/// to `FAKE_AGY_TRACE`. A prompt fails the test.
const FAKE_AGY: &str = r#"#!/usr/bin/python3
import json, os, sys
behaviour = json.loads(os.environ.get('FAKE_AGY', '{}'))
trace = os.environ['FAKE_AGY_TRACE']
def record(entry):
    with open(trace, 'a') as out: out.write(json.dumps(entry) + '\n')
record({'argv': sys.argv[1:], 'env': {k: v for k, v in os.environ.items() if k not in ('FAKE_AGY', 'FAKE_AGY_TRACE')}})
if sys.argv[1:] == ['--version']:
    print(behaviour.get('version', '1.5.14')); sys.exit(0)
if sys.argv[1:3] == ['-p', '/usage']:
    print(json.dumps(behaviour['usage'])); sys.exit(behaviour.get('usage_exit', 0))
assert sys.argv[1] == 'acp', sys.argv
levels = {'gemini-4-neon': ['low', 'medium', 'high', 'extra'], 'gemini-4-carbon': ['low', 'medium', 'high']}
def options(model):
    return [{'id': 'model', 'category': 'model', 'type': 'select', 'currentValue': model, 'options': [{'value': m, 'name': m.replace('-', ' ').title()} for m in levels]},
            {'id': 'thought_level', 'category': 'thought_level', 'type': 'select', 'currentValue': 'medium', 'options': [{'value': l, 'name': l.title()} for l in levels[model]]}]
authenticated = False
for line in sys.stdin:
    request = json.loads(line)
    record({'method': request.get('method'), 'params': request.get('params')})
    method = request.get('method')
    if 'id' not in request: continue
    def answer(result=None, error=None):
        reply = {'jsonrpc': '2.0', 'id': request['id']}
        if error is not None: reply['error'] = {'code': error, 'message': 'refused'}
        else: reply['result'] = result
        print(json.dumps(reply), flush=True)
    if method == 'initialize':
        answer({'protocolVersion': behaviour.get('protocol', 1), 'agentInfo': {'name': behaviour.get('agent', 'Antigravity ACP Server'), 'version': '1.5.14'},
                'agentCapabilities': {'sessionCapabilities': {'resume': {}, 'close': {}}}, 'authMethods': []})
    elif method == 'authenticate':
        if behaviour.get('authenticate') == 'refuse': answer(error=-32000)
        elif behaviour.get('authenticate') == 'fail': answer(error=-32603)
        else: authenticated = True; answer({})
    elif method == 'session/new':
        if behaviour.get('signed_in', True) or authenticated:
            print(json.dumps({'jsonrpc': '2.0', 'method': 'session/update', 'params': {'update': {'sessionUpdate': 'available_commands_update'}}}), flush=True)
            answer({'sessionId': 'probe-session', 'configOptions': options('gemini-4-carbon')})
        else: answer(error=-32000)
    elif method == 'session/set_config_option':
        answer({'configOptions': options(request['params']['value'])})
    elif method == 'session/close':
        answer({})
    else:
        record({'unexpected': method}); answer(error=-32601)
"#;

struct Fake {
    _root: Directory,
    root: PathBuf,
    executable: PathBuf,
    trace: PathBuf,
}

impl Fake {
    fn new() -> Self {
        let root = Directory::new();
        let base = root.to_path_buf();
        fs::create_dir_all(&base).unwrap();
        let executable = base.join("agy");
        fs::write(&executable, FAKE_AGY).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir_all(base.join("probe")).unwrap();
        Fake { trace: base.join("trace.jsonl"), executable, root: base, _root: root }
    }

    fn probe(&self, access: Access, behaviour: Value, folder: Option<&Path>, key: Option<&str>) -> ProbeSpec {
        let inherited: BTreeMap<String, String> = [("PATH", "/usr/bin:/bin"), ("GEMINI_API_KEY", "inherited-must-not-reach")].into_iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
        let settings = Settings { access, project: access.takes_project().then(|| "demo-project".into()), ..Settings::default() };
        let mut launch: Launch = antigravity::launch(&inherited, &self.root, &settings, folder, key, true);
        launch.environment.insert("FAKE_AGY".into(), behaviour.to_string());
        launch.environment.insert("FAKE_AGY_TRACE".into(), self.trace.display().to_string());
        ProbeSpec { executable: self.executable.clone(), launch, directory: self.root.join("probe"), access }
    }

    fn trace(&self) -> Vec<Value> {
        fs::read_to_string(&self.trace).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    fn methods(&self) -> Vec<String> {
        self.trace().iter().filter_map(|entry| entry["method"].as_str().map(str::to_owned)).collect()
    }
}

fn work() -> AccountKey {
    AccountKey::new(Provider::Antigravity, AccountId::parse("work").unwrap())
}

#[test]
fn a_named_account_gets_a_private_folder_linked_to_the_users_configuration() {
    let root = Directory::new();
    let (home, gemini) = (root.join("alondra"), root.join("gemini"));
    fs::create_dir_all(gemini.join("config")).unwrap();
    fs::create_dir_all(gemini.join("skills")).unwrap();
    fs::write(gemini.join("GEMINI.md"), "rules").unwrap();
    fs::write(gemini.join("config/mcp_config.json"), "{}").unwrap();
    fs::write(gemini.join("oauth_creds.json"), "a sign-in").unwrap();
    let folder = prepare(&home, &work(), &gemini).unwrap().unwrap();
    assert_eq!(folder, home.join("provider-accounts/antigravity/work"));
    for directory in [&folder, &home.join("provider-accounts/antigravity"), &folder.join("config")] {
        assert_eq!(fs::metadata(directory).unwrap().mode() & 0o777, 0o700, "{}", directory.display());
    }
    assert_eq!(fs::read_link(folder.join("GEMINI.md")).unwrap(), gemini.join("GEMINI.md"));
    assert_eq!(fs::read_link(folder.join("skills")).unwrap(), gemini.join("skills"));
    assert_eq!(fs::read_link(folder.join("config/mcp_config.json")).unwrap(), gemini.join("config/mcp_config.json"));
    assert!(fs::symlink_metadata(folder.join("oauth_creds.json")).is_err(), "a sign-in is never shared");
    fs::write(gemini.join("AGENTS.md"), "more rules").unwrap();
    prepare(&home, &work(), &gemini).unwrap();
    assert!(fs::symlink_metadata(folder.join("AGENTS.md")).unwrap().file_type().is_symlink(), "running it again adds what the user gained");
    assert_eq!(prepare(&home, &AccountKey::native(Provider::Antigravity), &gemini).unwrap(), None, "the default account is agy's own");
    assert!(prepare(&home, &AccountKey::new(Provider::Codex, AccountId::parse("work").unwrap()), &gemini).is_err());
    remove(&home, &work()).unwrap();
    assert!(!folder.exists());
    assert!(gemini.join("GEMINI.md").exists() && gemini.join("skills").is_dir(), "removing the account never removes what its links point to");
}

#[test]
fn a_linked_account_folder_is_refused() {
    let root = Directory::new();
    let home = root.join("alondra");
    fs::create_dir_all(home.join("provider-accounts/antigravity")).unwrap();
    fs::create_dir_all(root.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(root.join("elsewhere"), home.join("provider-accounts/antigravity/work")).unwrap();
    assert!(prepare(&home, &work(), &root.join("gemini")).is_err());
    assert!(set_key(&home, &work(), Some(&Secret::new("k".into()))).is_err());
    remove(&home, &work()).unwrap();
    assert!(root.join("elsewhere").is_dir(), "removing a linked folder removes the link only");
}

#[test]
fn the_key_is_kept_in_a_0600_file_replaced_whole_whatever_the_umask() {
    let root = Directory::new();
    let home = root.join("alondra");
    prepare(&home, &work(), &root.join("gemini")).unwrap();
    let path = home.join("provider-accounts/antigravity/work").join(KEY_FILE);
    let previous = unsafe { libc::umask(0o000) };
    let written = set_key(&home, &work(), Some(&Secret::new("AIza-first".into())));
    let rewritten = set_key(&home, &work(), Some(&Secret::new("AIza-second".into())));
    unsafe { libc::umask(previous) };
    written.unwrap();
    rewritten.unwrap();
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    assert_eq!(key(&home, &work()).unwrap().unwrap().expose(), "AIza-second");
    assert!(key_set(&home, &work()));
    let folder = path.parent().unwrap();
    assert_eq!(fs::read_dir(folder).unwrap().filter_map(Result::ok).filter(|entry| entry.file_name().to_string_lossy().contains(KEY_FILE)).count(), 1, "no staged copy is left");
    let stray = folder.join(format!(".{KEY_FILE}.cut-short"));
    fs::write(&stray, "AIza-left-behind").unwrap();
    set_key(&home, &work(), None).unwrap();
    assert!(!stray.exists(), "clearing removes a copy a write cut short left behind");
    set_key(&home, &work(), Some(&Secret::new("AIza-second".into()))).unwrap();
    assert!(set_key(&home, &work(), Some(&Secret::new("has space".into()))).is_err());
    assert!(set_key(&home, &work(), Some(&Secret::new(String::new()))).is_err());
    set_key(&home, &work(), None).unwrap();
    assert!(!path.exists() && !key_set(&home, &work()));
    assert!(key(&home, &work()).unwrap().is_none());
    set_key(&home, &work(), None).unwrap();
    assert!(set_key(&home, &AccountKey::native(Provider::Antigravity), Some(&Secret::new("k".into()))).is_err(), "the default account keeps no key");
    set_key(&home, &work(), Some(&Secret::new("AIza-third".into()))).unwrap();
    remove(&home, &work()).unwrap();
    assert!(!path.exists(), "removing the account removes its key");
}

#[test]
fn a_linked_or_shared_key_file_is_never_read_or_written_through() {
    let root = Directory::new();
    let home = root.join("alondra");
    prepare(&home, &work(), &root.join("gemini")).unwrap();
    let path = home.join("provider-accounts/antigravity/work").join(KEY_FILE);
    let outside = root.join("outside");
    fs::write(&outside, "not a key").unwrap();
    std::os::unix::fs::symlink(&outside, &path).unwrap();
    assert!(key(&home, &work()).is_err());
    assert!(set_key(&home, &work(), Some(&Secret::new("AIza".into()))).is_err());
    assert_eq!(fs::read_to_string(&outside).unwrap(), "not a key");
    fs::remove_file(&path).unwrap();
    fs::write(&path, "AIza").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(key(&home, &work()).is_err(), "a key others can read is refused");
}

#[test]
fn a_secret_never_prints() {
    let secret = Secret::new("AIza-visible".into());
    assert!(!format!("{secret:?}").contains("AIza"));
}

#[test]
fn a_signed_in_google_account_reads_its_catalog_without_a_prompt_or_a_sign_in() {
    let fake = Fake::new();
    let report = check(&fake.probe(Access::Google, json!({}), None, None));
    assert_eq!(report.version, Some(Version(1, 5, 14)));
    let Check::SignedIn { models, native_default } = report.check else { panic!("{:?}", report.check) };
    assert_eq!(native_default.as_deref(), Some("gemini-4-carbon"));
    let neon = models.iter().find(|model| model.model == "gemini-4-neon").unwrap();
    assert_eq!(neon.reasoning_efforts, ["low", "medium", "high", "xhigh"]);
    assert_eq!(neon.display_name, "Gemini 4 Neon");
    assert_eq!(models.iter().find(|model| model.model == "gemini-4-carbon").unwrap().reasoning_efforts, ["low", "medium", "high"]);
    let methods = fake.methods();
    assert_eq!(methods, ["initialize", "session/new", "session/set_config_option", "session/close"]);
    assert!(!methods.iter().any(|method| method == "authenticate" || method == "session/prompt"), "{methods:?}");
}

#[test]
fn the_check_reports_signed_out_too_old_missing_and_another_agent() {
    let fake = Fake::new();
    assert_eq!(check(&fake.probe(Access::Google, json!({"signed_in": false}), None, None)).check, Check::SignedOut);
    let old = check(&fake.probe(Access::Google, json!({"version": "1.1.22"}), None, None));
    assert_eq!((old.version, old.check), (Some(Version(1, 1, 22)), Check::Unavailable(Unavailable::TooOld)));
    assert_eq!(check(&fake.probe(Access::Google, json!({"agent": "Other Agent"}), None, None)).check, Check::Unavailable(Unavailable::OtherAgent));
    assert_eq!(check(&fake.probe(Access::Google, json!({"protocol": 2}), None, None)).check, Check::Unavailable(Unavailable::OtherAgent));
    let mut missing = fake.probe(Access::Google, json!({}), None, None);
    missing.executable = fake.root.join("not-installed");
    assert_eq!(check(&missing).check, Check::Unavailable(Unavailable::NotInstalled));
}

#[test]
fn a_key_account_authenticates_with_its_own_key_alone() {
    let fake = Fake::new();
    let folder = fake.root.join("account");
    let report = check(&fake.probe(Access::ApiKey, json!({"signed_in": false}), Some(&folder), Some("AIza-own")));
    assert!(matches!(report.check, Check::SignedIn { .. }), "{:?}", report.check);
    let trace = fake.trace();
    let started = trace.iter().find(|entry| entry["argv"][0] == "acp").unwrap();
    assert_eq!(started["env"]["GEMINI_API_KEY"], "AIza-own");
    assert_eq!(started["env"]["GEMINI_HOME"], folder.display().to_string());
    assert_eq!(started["argv"], json!(["acp", "--gemini-dir", folder.display().to_string()]));
    assert!(!trace.iter().any(|entry| entry.to_string().contains("inherited-must-not-reach")));
    assert!(trace.iter().any(|entry| entry["method"] == "authenticate" && entry["params"]["methodId"] == "gemini-api-key"));
    assert_eq!(check(&fake.probe(Access::ApiKey, json!({"authenticate": "refuse", "signed_in": false}), Some(&folder), None)).check, Check::SignedOut);
    assert_eq!(check(&fake.probe(Access::ApiKey, json!({"authenticate": "fail", "signed_in": false}), Some(&folder), Some("AIza-own"))).check,
        Check::Unavailable(Unavailable::Failed), "only ACP's auth-required code means signed out");
}

#[test]
fn the_usage_reading_reads_the_report_and_tells_a_refusal() {
    let fake = Fake::new();
    let report = json!({"status": "SUCCESS", "command": {"name": "usage", "data": {"groups": [{"name": "All Models", "buckets": [
        {"id": "gemini-4-neon-high", "name": "Gemini 4 Neon (High)", "remaining_fraction": 0.25, "reset_time": "2026-11-23T10:00:00Z"},
        {"id": "gemini-4-neon-low", "name": "Gemini 4 Neon (Low)", "remaining_fraction": 0.25, "reset_time": "2026-11-23T10:00:00Z"},
        {"id": "claude-opus-max", "name": "Claude Opus (Max)", "remaining_fraction": 0, "reset_time": "2026-11-24T00:00:00Z"}]}]}}});
    let UsageOutcome::Read(reading) = usage(&fake.probe(Access::Google, json!({"usage": report}), None, None)).unwrap() else { panic!() };
    assert_eq!(reading.windows.len(), 2);
    assert!(reading.complete);
    let argv = &fake.trace()[0]["argv"];
    assert_eq!(argv.as_array().unwrap()[0..4], [json!("-p"), json!("/usage"), json!("--output-format"), json!("json")]);
    let refused = json!({"status": "ERROR", "error": "authentication failed or timed out", "finished_reason": "error"});
    assert_eq!(usage(&fake.probe(Access::Google, json!({"usage": refused, "usage_exit": 1}), None, None)).unwrap(), UsageOutcome::Refused);
    assert!(usage(&fake.probe(Access::Google, json!({"usage": {"status": "SUCCESS"}, "usage_exit": 2}), None, None)).is_err());
}
