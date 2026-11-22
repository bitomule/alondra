//! Codex hosts as the tests here and in the crates above configure them.
use alondra_daemon_engine_model::LaunchOptionsFile as _;
use alondra_engine_sdk::sdk_owner::SdkOwners as _;
use alondra_daemon_engine::live::LiveOf as _;
use alondra_engine_sdk::provider_transition::TransitionsOf as _;
use super::*;
use alondra_engine_admission::AdmissionOf as _;


pub fn configured(slice: &Slice, mode: &str) -> PathBuf {
    use alondra_core::model_selection::{AccountModelCatalog, CatalogModel};
    let state = slice.domain.state().unwrap();
    let mut accounts = state.accounts.clone();
    if !accounts.iter().any(|account| account.provider == Provider::Codex && account.id.is_default()) {
        accounts.push(alondra_core::accounts::AccountStatus { provider: Provider::Codex, id: AccountId::default_account(), name: "Codex fixture".into(), state: alondra_core::accounts::AccountState::SignedIn, native_identity: Some("fixture-workspace".into()), email: Some("first@example.test".into()), plan: None, billing: Default::default(), sign_ins: 0, profile: None });
    }
    slice.run(&system(), "codex-fixture-account", Command::RecordAccounts { accounts, reserve: state.account_reserve });
    slice.run(&system(), "codex-fixture-catalog", Command::RecordModelCatalog { catalog: AccountModelCatalog {
        account: AccountKey { provider: Provider::Codex, account: AccountId::default_account() },
        revision: "fixture-catalog".into(), measured_at: 1, native_default: Some("fixture-model".into()),
        models: vec![CatalogModel { model: "fixture-model".into(), display_name: "Fixture model".into(), reasoning_efforts: vec!["high".into()], default_reasoning_effort: Some("high".into()), context_window: None, input_modalities: vec!["text".into()], is_default: true, hidden: false }],
    } });
    let skill = slice.plugin.join("skills/alondra");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "You are an Alondra task node. Follow the task request and report the public result through Alondra.").unwrap();
    let native = slice._home.join("codex-native");
    std::fs::copy(runfile("FIXTURE_CODEX_NATIVE"), &native).unwrap();
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
    let host = runfile("FIXTURE_CODEX_HOST");
    let trace = slice._home.join("codex-transcript");
    let mut backend = slice.backend.clone();
    backend.provider = Provider::Codex;
    backend.origin = None;
    backend.node = host.clone(); backend.node_sha256 = backend::digest(&host).unwrap();
    backend.host = host; backend.host_sha256 = backend.node_sha256.clone();
    backend.native = native; backend.native_sha256 = backend::digest(&backend.native).unwrap();
    backend.native_environment.insert("ALONDRA_FAKE_MODE".into(), mode.into());
    backend.native_environment.insert("ALONDRA_FAKE_TRANSCRIPT".into(), trace.display().to_string());
    backend.native_environment.insert("ALONDRA_FAKE_FINAL_TEXT".into(), "Codex fixture final answer".into());
    std::fs::write(slice._home.join("codex-backend.json"), serde_json::to_vec(&backend).unwrap()).unwrap();
    trace
}
pub fn selection() -> alondra_core::model_selection::TaskSelection {
    use alondra_core::model_selection::*;
    let key = ModelKey { provider: Provider::Codex, model: "fixture-model".into() };
    let mut selection = TaskSelection::automatic(ModelProfile::General);
    selection.request_model(Some(key.clone()));
    selection.effective = Some(ModelSelection { model: key, reasoning_effort: Some("high".into()), catalog_revision: "fixture-catalog".into() });
    selection.account = Some(AccountKey { provider: Provider::Codex, account: AccountId::default_account() });
    selection.native_tool_denials=Some(vec![]);
    selection
}
pub fn launch(slice: &Slice) -> Driver { launch_selected(slice, selection()) }

pub fn launch_selected(slice: &Slice, selected: alondra_core::model_selection::TaskSelection) -> Driver {
    let effective = selected.effective.as_ref().unwrap();
    let options = LaunchOptions { provider: Provider::Codex, model: Some(effective.model.model.clone()), effort: effective.reasoning_effort.clone(), selection: Some(selected), ..LaunchOptions::default() };
    let mut driver = start(slice.domain.clone(), &slice.backend, Launch {
        task: TASK.into(), name: "Codex release notes".into(), directory: slice.worktree.display().to_string(),
        prompt: "Write the release notes through Codex".into(), plugin: slice.plugin.clone(),
        options,
        model: None, account: AccountId::default_account(),
    }).unwrap();
    driver.replaced_with(slice.plugin.clone());
    driver
}
pub fn transcript(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

pub fn assert_synthetic_native_auth(home:&Path) {
    let auth:serde_json::Value=serde_json::from_slice(&std::fs::read(home.join("auth.json")).unwrap()).unwrap();
    assert_eq!(auth["tokens"]["account_id"],"fixture-workspace");
    assert_eq!(auth["tokens"]["refresh_token"],"synthetic-never-sent");
    assert!(auth["tokens"]["access_token"].as_str().unwrap().ends_with(".fixture"));
}

pub struct NativeModel(std::process::Child);
impl Drop for NativeModel {
    fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
}

/// A native Codex on a model double, with the models `observed` advertises.
pub fn native_model_fixture(slice:&Slice,journey:Option<(&Path,u16)>,observed:alondra_core::codex_account::ProbeReport)->(NativeModel,PathBuf,PathBuf,alondra_core::model_selection::TaskSelection){
    use std::io::BufRead as _;
    use std::process::{Command as ProcessCommand, Stdio};
    let native = std::env::var("CODEX_NATIVE_EXECUTABLE").expect("This opt-in test requires an explicit CODEX_NATIVE_EXECUTABLE");
    let model_trace = slice._home.join("native-model-requests");
    let script = slice._home.join("model-double.py");
    std::fs::copy(runfile("FIXTURE_CODEX_MODEL_DOUBLE"), &script).unwrap();
    let mut command=ProcessCommand::new("/usr/bin/python3");command.arg(&script).arg(&model_trace);
    if let Some((configuration,_))=journey {command.arg(configuration);}
    let model_stderr=std::fs::File::create(model_trace.with_extension("stderr")).unwrap();
    let child = command
        .env_clear().env("HOME", &*slice._home).env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::from(model_stderr)).spawn().unwrap();
    let mut model = NativeModel(child);
    let mut port = String::new();
    std::io::BufReader::new(model.0.stdout.take().unwrap()).read_line(&mut port).unwrap();
    let port: u16 = port.trim().parse().unwrap();
    let code_home = slice._home.join("native-codex-home");
    std::fs::create_dir(&code_home).unwrap();
    std::fs::set_permissions(&code_home, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(code_home.join("config.toml"),format!("cli_auth_credentials_store = \"file\"\nmodel_provider = \"openai\"\nopenai_base_url = \"http://127.0.0.1:{port}/v1\"\nchatgpt_base_url = \"http://127.0.0.1:{port}\"\n[features]\nenable_request_compression = false\napps = false\nplugins = false\nhooks = false\n")).unwrap();
    use base64::Engine as _;
    let claims=serde_json::json!({"email":"first@example.test","exp":4102444800_u64,"https://api.openai.com/auth":{"chatgpt_account_id":"fixture-workspace","chatgpt_user_id":"fixture-user","chatgpt_plan_type":"plus"}});
    let jwt=format!("e30.{}.fixture",base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap()));
    let auth=serde_json::json!({"auth_mode":"chatgpt","tokens":{"id_token":jwt,"access_token":jwt,"refresh_token":"synthetic-never-sent","account_id":"fixture-workspace"},"last_refresh":"2026-10-26T00:00:00Z"});
    std::fs::write(code_home.join("auth.json"),serde_json::to_vec(&auth).unwrap()).unwrap();std::fs::set_permissions(code_home.join("auth.json"),std::fs::Permissions::from_mode(0o600)).unwrap();
    let sandbox = slice._home.join("native.sb");
    std::fs::write(&sandbox, "(version 1)(allow default)(deny file-read* (subpath \"/Users\"))(deny file-write* (subpath \"/Users\"))(deny network*)(allow network-inbound (local ip \"localhost:*\"))(allow network-outbound (remote ip \"localhost:*\"))").unwrap();
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    let executable = slice._home.join("native-codex");
    std::fs::write(&executable, format!("#!/bin/sh\nexec /usr/bin/sandbox-exec -f {} {} \"$@\"\n", quote(&sandbox.display().to_string()), quote(&native))).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let version = ProcessCommand::new(&executable).arg("--version").env_clear()
        .env("HOME", &*slice._home).env("CODEX_HOME", &code_home).env("PATH", "/usr/bin:/bin").output().unwrap();
    assert!(version.status.success(), "{}", String::from_utf8_lossy(&version.stderr));
    assert!(String::from_utf8_lossy(&version.stdout).contains("0.163.0"));
    let plugin_bin = slice.plugin.join("bin");
    std::fs::create_dir_all(&plugin_bin).unwrap();
    let mcp = plugin_bin.join("alondra");
    let mcp_source=if journey.is_some(){runfile("FIXTURE_CLI")}else{runfile("FIXTURE_CODEX_MCP_NATIVE")};
    std::fs::copy(mcp_source,&mcp).unwrap();
    std::fs::set_permissions(&mcp, std::fs::Permissions::from_mode(0o700)).unwrap();
    let backend_path = slice._home.join("codex-backend.json");
    let mut backend: SdkBackend = serde_json::from_slice(&std::fs::read(&backend_path).unwrap()).unwrap();
    backend.native = executable;
    backend.native_sha256 = backend::digest(&backend.native).unwrap();
    backend.native_environment.insert("CODEX_HOME".into(), code_home.display().to_string());
    backend.native_environment.insert("RUST_LOG".into(), "off".into());
    if let Some((_,port))=journey {backend.native_environment.insert("ALONDRA_PORT".into(),port.to_string());}
    std::fs::write(backend_path, serde_json::to_vec(&backend).unwrap()).unwrap();
    let native_model = observed.catalog.models.iter().find(|model| !model.hidden).expect("the actual native model/list must advertise at least one model");
    let effort = native_model.default_reasoning_effort.clone().or_else(|| native_model.reasoning_efforts.first().cloned());
    let effective = observed.catalog.resolve(&alondra_core::model_selection::ModelCandidate { model: native_model.model.clone(), reasoning_effort: effort }).unwrap();
    let mut selected = alondra_core::model_selection::TaskSelection::automatic(alondra_core::model_selection::ModelProfile::General);
    selected.request_model(Some(effective.model.clone()));
    selected.requested_reasoning_effort = effective.reasoning_effort.clone();
    selected.effective = Some(effective);
    selected.account = Some(observed.catalog.account.clone());
    selected.native_tool_denials=Some(vec![]);
    slice.run(&system(), "actual-native-catalog", Command::RecordModelCatalog { catalog: observed.catalog });
    (model,model_trace,code_home,selected)
}

pub fn native_business_evidence(slice:&Slice,stage:&str,model_trace:&std::path::Path)->String{
    use std::io::Read as _;
    let state=slice.domain.state().unwrap();
    let native:Vec<serde_json::Value>=slice.kept.lock().unwrap().values().map(|bytes|{
        let value:serde_json::Value=serde_json::from_slice(bytes).unwrap();
        let payload=&value["body"]["observation"]["payload"];
        let params=&payload["params"];let item=&params["item"];
        serde_json::json!({"method":payload["method"],"input_id":params["inputId"],"control_id":params["controlId"],"turn_id":params["turn"]["id"],"turn_status":params["turn"]["status"],"turn_error":params["turn"]["error"],"item_id":item["id"],"item_type":item["type"],"tool":item["tool"],"tool_status":item["status"],"tool_result":item["result"],"tool_error":item["error"],"item_text":item["text"]})
    }).collect();
    let diagnostic=serde_json::to_vec_pretty(&serde_json::json!({"stage":stage,"task":state.tasks.get(&TaskId::from(TASK)),"lifecycle":state.lifecycle_journal.get(&TaskId::from(TASK)),"native":native})).unwrap();
    let files=[(model_trace.to_path_buf(),"native-business-model-requests.jsonl",8*1024*1024_u64),(model_trace.with_extension("stderr"),"native-business-model-stderr.log",64*1024),(model_trace.with_extension("stages"),"native-business-model-stages.jsonl",64*1024),(model_trace.with_extension("error.json"),"native-business-model-error.json",16*1024)];
    let mut summaries=Vec::new();
    let output=std::env::var_os("TEST_UNDECLARED_OUTPUTS_DIR").map(std::path::PathBuf::from);
    if let Some(output)=&output{std::fs::write(output.join("native-business-last-stage.json"),&diagnostic).unwrap();}
    for(path,name,limit)in files{
        let mut bytes=Vec::new();if let Ok(file)=std::fs::File::open(path){let _=file.take(limit).read_to_end(&mut bytes);}
        if let Some(output)=&output{std::fs::write(output.join(name),&bytes).unwrap();}
        let text=String::from_utf8_lossy(&bytes);summaries.push(format!("{name}: {}",text.chars().rev().take(if name.ends_with("jsonl"){4000}else{8000}).collect::<String>().chars().rev().collect::<String>()));
    }
    format!("{}; {}",String::from_utf8_lossy(&diagnostic).chars().take(24000).collect::<String>(),summaries.join("; "))
}

pub async fn native_business_until(slice:&Slice,changed:&mut tokio::sync::watch::Receiver<u64>,stage:&str,model_trace:&std::path::Path,predicate:impl Fn(&DomainState)->bool){
    loop {
        let state=slice.domain.state().unwrap();
        if predicate(&state){return}
        let mut failed=None;
        if model_trace.with_extension("error.json").is_file(){failed=Some("The loopback model fixture rejected its actual request".to_owned());}
        for bytes in slice.kept.lock().unwrap().values(){
            let value:serde_json::Value=serde_json::from_slice(bytes).unwrap();
            let payload=&value["body"]["observation"]["payload"];
            let method=payload["method"].as_str();
            let terminal_failure=method==Some("turn/completed") && payload["params"]["turn"]["status"]=="failed";
            let required_mcp_failed=method==Some("mcpServer/startupStatus/updated") && payload["params"]["name"]=="alondra" && payload["params"]["status"]=="failed";
            let fatal=matches!(method,Some("alondra/transportFailed"|"alondra/inputRefused"|"alondra/accountBlocked"));
            let text=String::from_utf8_lossy(bytes);
            if terminal_failure || required_mcp_failed || fatal || text.contains("\"isError\":true"){failed=Some(text.chars().take(2000).collect::<String>());break}
        }
        if let Some(failed)=failed{panic!("Native business callback failed at {stage}: {failed}; {}",native_business_evidence(slice,stage,model_trace));}
        let next=alondra_core::clock::timeout(std::time::Duration::from_secs(30),changed.changed()).await;
        if next.is_err(){panic!("The native business journey stalled at {stage}: {}",native_business_evidence(slice,stage,model_trace));}
        next.unwrap().unwrap();
    }
}
