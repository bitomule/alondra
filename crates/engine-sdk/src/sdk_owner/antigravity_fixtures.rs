//! Antigravity nodes as the tests here configure them: the release's Antigravity host, with the
//! fake `agy` of the host's own tests run by the pinned Node in place of Antigravity CLI.
use super::*;
use alondra_core::accounts::{AccountChange, AccountsConfig, AntigravityStatus};
use alondra_core::antigravity_account::{Access, KEY_FILE, Settings};
use alondra_core::model_selection::{AccountModelCatalog, CatalogModel, ModelKey, ModelProfile, ModelSelection, TaskSelection};

/// The model the fake agy opens its sessions on.
pub const MODEL: &str = "gemini-4-carbon";
/// A key nothing but the account's file and its agy may hold.
pub const KEY: &str = "AIza-fixture-key-never-written-elsewhere";

/// A node of `account`, signed in with `access`, whose agy follows `script` (one list of steps per
/// prompt, as `agent-host/fake-agy-acp.mjs` reads it). Returns the folder where the fake agy
/// records the messages it received and each of its starts.
pub fn configured(slice: &Slice, account: &AccountId, access: Access, script: serde_json::Value) -> PathBuf {
    let record = slice._home.join("agy-record");
    std::fs::create_dir_all(&record).unwrap();
    let native = slice._home.join("agy");
    let quote = |path: PathBuf| format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
    std::fs::write(&native, format!("#!/bin/sh\nexec {} {} \"$@\"\n", quote(runfile("FIXTURE_NODE_RUNTIME")), quote(runfile("FIXTURE_ANTIGRAVITY_NATIVE")))).unwrap();
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
    let environment = BTreeMap::from([("ALONDRA_FAKE_AGY_RECORD".to_owned(), record.display().to_string()), ("ALONDRA_FAKE_AGY_SCRIPT".to_owned(), script.to_string())]);
    configured_with(slice, account, access, KEY, native, environment);
    record
}

/// A node of `account` whose host starts `native` as its agy, with `environment` besides the
/// daemon's; an account that takes a key keeps `key` in its file.
pub fn configured_with(slice: &Slice, account: &AccountId, access: Access, key: &str, native: PathBuf, environment: BTreeMap<String, String>) {
    let state = slice.domain.state().unwrap();
    let mut accounts = state.accounts.clone();
    accounts.retain(|status| !(status.provider == Provider::Antigravity && status.id == *account));
    accounts.push(alondra_core::accounts::AccountStatus {
        antigravity: Some(AntigravityStatus::of(&Settings::of(access), access.takes_key())), provider: Provider::Antigravity, id: account.clone(),
        name: "Antigravity fixture".into(), state: alondra_core::accounts::AccountState::SignedIn, native_identity: None,
        email: Some("ana@example.com".into()), plan: None, billing: access.default_billing(), sign_ins: 0, profile: None,
    });
    slice.run(&system(), "antigravity-fixture-account", Command::RecordAccounts { accounts, reserve: state.account_reserve });
    slice.run(&system(), "antigravity-fixture-catalog", Command::RecordModelCatalog { catalog: AccountModelCatalog {
        account: AccountKey::new(Provider::Antigravity, account.clone()), revision: "fixture-catalog".into(), measured_at: 1, native_default: Some(MODEL.into()),
        models: vec![CatalogModel { model: MODEL.into(), display_name: "Gemini 4 Carbon".into(), reasoning_efforts: vec!["low".into(), "medium".into(), "high".into()],
            default_reasoning_effort: Some("medium".into()), context_window: None, input_modalities: vec!["text".into()], is_default: true, hidden: false }],
    } });
    let mut config = AccountsConfig::fresh();
    config.connect(Provider::Antigravity);
    if !account.is_default() {
        config = config.changed(Provider::Antigravity, &AccountChange::Add { name: account.as_str().into(), antigravity: Some(Settings::of(access)) }).unwrap();
        let folder = alondra_core::accounts::provider_folder(&slice._home, &AccountKey::new(Provider::Antigravity, account.clone())).unwrap();
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&folder).unwrap();
        if access.takes_key() {
            let file = folder.join(KEY_FILE);
            std::fs::write(&file, key).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    std::fs::write(alondra_core::options::path_in(&slice._home), config.to_toml()).unwrap();
    let skill = slice.plugin.join("skills/alondra");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "You are an Alondra task node. Follow the task request and report the public result through Alondra.").unwrap();
    let host = runfile("FIXTURE_ANTIGRAVITY_HOST");
    let mut backend = slice.backend.clone();
    backend.provider = Provider::Antigravity;
    backend.origin = None;
    backend.node = host.clone(); backend.node_sha256 = backend::digest(&host).unwrap();
    backend.host = host; backend.host_sha256 = backend.node_sha256.clone();
    backend.native = native; backend.native_sha256 = backend::digest(&backend.native).unwrap();
    backend.native_environment.extend(environment);
    std::fs::write(slice._home.join("antigravity-backend.json"), serde_json::to_vec(&backend).unwrap()).unwrap();
}

pub fn selection(account: &AccountId) -> TaskSelection {
    let key = ModelKey { provider: Provider::Antigravity, model: MODEL.into() };
    let mut selection = TaskSelection::automatic(ModelProfile::General);
    selection.request_model(Some(key.clone()));
    selection.effective = Some(ModelSelection { model: key, reasoning_effort: Some("high".into()), catalog_revision: "fixture-catalog".into() });
    selection.account = Some(AccountKey::new(Provider::Antigravity, account.clone()));
    // A named account here takes a key, which is paid per use: only the user's choice runs work on it.
    selection.paid_account_grant = (!account.is_default()).then(|| AccountKey::new(Provider::Antigravity, account.clone()));
    selection.native_tool_denials = Some(vec![]);
    selection
}

pub fn launch(slice: &Slice, account: &AccountId) -> Driver {
    let options = LaunchOptions { provider: Provider::Antigravity, model: Some(MODEL.into()), effort: Some("high".into()), selection: Some(selection(account)), ..LaunchOptions::default() };
    let mut driver = start(slice.domain.clone(), &slice.backend, Launch {
        task: TASK.into(), name: "Antigravity release notes".into(), directory: slice.worktree.display().to_string(),
        prompt: "Write the release notes through Antigravity".into(), plugin: slice.plugin.clone(),
        options, model: None, account: account.clone(),
    }).unwrap();
    driver.replaced_with(slice.plugin.clone());
    driver
}

/// What the fake agy recorded in `file` of `record`, complete lines only: its writer is another
/// process.
pub fn recorded(record: &Path, file: &str) -> Vec<serde_json::Value> {
    std::fs::read_to_string(record.join(file)).unwrap_or_default().split_inclusive('\n')
        .filter_map(|line| line.strip_suffix('\n')).map(|line| serde_json::from_str(line).unwrap()).collect()
}

/// The requests of `method` agy received.
pub fn received(record: &Path, method: &str) -> Vec<serde_json::Value> {
    recorded(record, "messages.jsonl").into_iter().filter(|message| message["method"] == method).collect()
}
