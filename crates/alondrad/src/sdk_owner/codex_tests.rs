use super::*;

fn configured(slice: &Slice, mode: &str) -> PathBuf {
    use alondra_core::model_selection::{AccountModelCatalog, CatalogModel};
    let state = slice.domain.state().unwrap();
    let mut accounts = state.accounts.clone();
    if !accounts.iter().any(|account| account.provider == Provider::Codex && account.id.is_default()) {
        accounts.push(alondra_core::accounts::AccountStatus { provider: Provider::Codex, id: AccountId::default_account(), name: "Codex fixture".into(), state: alondra_core::accounts::AccountState::SignedIn, native_identity: Some("fixture-workspace".into()), email: Some("first@example.test".into()), plan: None, billing: Default::default(), sign_ins: 0 });
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
fn selection() -> alondra_core::model_selection::TaskSelection {
    use alondra_core::model_selection::*;
    let key = ModelKey { provider: Provider::Codex, model: "fixture-model".into() };
    let mut selection = TaskSelection::automatic(ModelProfile::General);
    selection.request_model(Some(key.clone()));
    selection.effective = Some(ModelSelection { model: key, reasoning_effort: Some("high".into()), catalog_revision: "fixture-catalog".into() });
    selection.account = Some(AccountKey { provider: Provider::Codex, account: AccountId::default_account() });
    selection.native_tool_denials=Some(vec![]);
    selection
}
fn launch(slice: &Slice) -> Driver { launch_selected(slice, selection()) }
fn launch_selected(slice: &Slice, selected: alondra_core::model_selection::TaskSelection) -> Driver {
    let effective = selected.effective.as_ref().unwrap();
    let options = LaunchOptions { provider: Provider::Codex, model: Some(effective.model.model.clone()), effort: effective.reasoning_effort.clone(), selection: Some(selected), ..LaunchOptions::default() };
    start(Arc::clone(&slice.domain), &slice.backend, Launch {
        task: TASK.into(), name: "Codex release notes".into(), directory: slice.worktree.display().to_string(),
        prompt: "Write the release notes through Codex".into(), plugin: slice.plugin.clone(),
        options,
        model: None, account: AccountId::default_account(),
    }).unwrap()
}
fn transcript(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

#[test]
fn a_codex_node_uses_the_real_owner_store_and_commits_final_public_context_before_ack() {
    let slice = Slice::new();
    let trace = configured(&slice, "complete");
    let acknowledgements = watch_acknowledgements(&slice);
    let mut driver = launch(&slice);
    slice.settle(&mut driver);
    let state = slice.domain.state().unwrap();
    let task = &state.tasks[&TaskId::from(TASK)];
    assert_eq!(task.provider, Provider::Codex);
    assert_eq!(task.selection.as_ref(), Some(&selection()), "the model/account intent commits with its execution admission");
    assert!(!task.over(), "native turn completion cannot complete an Alondra task");
    assert!(!acknowledgements.lock().unwrap().is_empty());
    let native = transcript(&trace);
    let turns: Vec<_> = native.iter().filter(|message| message["method"] == "turn/start").collect();
    assert_eq!(turns.len(), 2, "the task's original input and its one canonical wake each run once");
    assert_eq!(turns[0]["params"]["input"][0]["text"], "Write the release notes through Codex");
    assert_ne!(turns[0]["params"]["clientUserMessageId"], turns[1]["params"]["clientUserMessageId"]);
    let observer = slice.observer();
    let registered: Vec<(String, serde_json::Value)> = observer.prepare("SELECT input_id,metadata FROM provider_input_metadata WHERE task_id=?1 ORDER BY rowid").unwrap()
        .query_map([TASK], |row| { let bytes: Vec<u8> = row.get(1)?; Ok((row.get(0)?, serde_json::from_slice(&bytes).unwrap())) }).unwrap().map(Result::unwrap).collect();
    assert_eq!(registered.len(), 2);
    let journal = &state.lifecycle_journal[&TaskId::from(TASK)];
    let wake = journal.iter().find_map(|record| match record { alondra_core::domain::LifecycleRecord::TurnEnded { wake: Some(wake), text, .. } => Some((wake, text)), _ => None }).unwrap();
    assert_eq!(&registered[1].0, wake.0, "the second input is the domain's durable wake");
    assert_eq!(turns[1]["params"]["input"][0]["text"].as_str(), wake.1.as_deref());
    for (index, (input, metadata)) in registered.iter().enumerate() {
        assert_eq!(turns[index]["params"]["clientUserMessageId"], metadata["message_id"]);
        assert_eq!(journal.iter().filter(|record| matches!(record, alondra_core::domain::LifecycleRecord::TurnStarted { input: seen, .. } if seen == input)).count(), 1);
    }
    assert!(Slice::lifecycle(&slice.domain).pending_inputs().is_empty(), "the native turn acceptance consumed both registered inputs");
    let public: Vec<(String, String)> = observer.prepare("SELECT native_id,content FROM task_context_items WHERE task_id=?1 AND source='assistant'").unwrap()
        .query_map([TASK], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().map(Result::unwrap).collect();
    assert_eq!(public.len(), turns.len());
    assert_ne!(public[0].0, public[1].0, "each native turn contributes its own final item exactly once");
    assert!(public.iter().all(|(_, text)| text == "Codex fixture final answer"), "authoritative final items replace draft deltas");
    assert_eq!(driver.worker.binding().driver(), "codex-app-server");
    driver.stop().unwrap();
}

#[test]
fn a_codex_app_server_survives_owner_and_log_reader_loss_and_adopts_without_reoffering() {
    let slice = Slice::new();
    let trace = configured(&slice, "hold");
    let mut driver = launch(&slice);
    while Slice::lifecycle(&slice.domain).activity() != ActivityView::Working { driver.observe().unwrap(); }
    let group = driver.worker.host_group().unwrap();
    let processes = alondra_process_group::members(group).unwrap();
    assert!(processes.len() >= 2, "the real persistent host and fake native app-server must both be running");
    drop(driver);
    assert!(processes.iter().all(|process| alondra_process_group::alive(process).unwrap()), "losing the owner and its stdout/stderr reader must not terminate native work");
    let restarted = Arc::new(DomainRuntime::open_at(&slice._home, &slice.database).unwrap());
    let mut adopted = adopt(Arc::clone(&restarted), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    assert_eq!(adopted.worker.stopper().and_then(|stopper| stopper.process_group()), Some(group));
    assert_eq!(owner_generation(&slice.database), 2);
    assert!(processes.iter().all(|process| alondra_process_group::alive(process).unwrap()));
    assert_eq!(transcript(&trace).iter().filter(|message| message["method"] == "initialize").count(), 1, "live adoption cannot create a second native app-server");
    assert_eq!(transcript(&trace).iter().filter(|message| message["method"] == "turn/start").count(), 1, "a handed input is never sent again during adoption");
    adopted.stop().unwrap();
    assert!(processes.iter().all(|process| !alondra_process_group::alive(process).unwrap()));
}

fn assert_synthetic_native_auth(home:&Path) {
    let auth:serde_json::Value=serde_json::from_slice(&std::fs::read(home.join("auth.json")).unwrap()).unwrap();
    assert_eq!(auth["tokens"]["account_id"],"fixture-workspace");
    assert_eq!(auth["tokens"]["refresh_token"],"synthetic-never-sent");
    assert!(auth["tokens"]["access_token"].as_str().unwrap().ends_with(".fixture"));
}

struct NativeModel(std::process::Child);
impl Drop for NativeModel {
    fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
}

fn native_model_fixture(slice:&Slice,journey:Option<(&Path,u16)>)->(NativeModel,PathBuf,PathBuf,alondra_core::model_selection::TaskSelection){
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
    let mcp_source=if journey.is_some(){std::fs::canonicalize(env!("CARGO_BIN_EXE_alondra")).unwrap()}else{runfile("FIXTURE_CODEX_MCP_NATIVE")};
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
    let mut probe_command = ProcessCommand::new(&backend.native);
    probe_command.env_clear().env("HOME", &*slice._home).env("CODEX_HOME", &code_home).env("PATH", "/usr/bin:/bin").env("RUST_LOG", "off");
    let observed = alondra_core::codex_account::probe_fixture(probe_command, AccountKey { provider: Provider::Codex, account: AccountId::default_account() }, alondra_core::now_millis()).unwrap();
    assert!(observed.signed_in && observed.requires_auth && observed.auth_method.as_deref()==Some("chatgpt") && !observed.pay_per_use && observed.email.as_deref()==Some("first@example.test"), "the native CLI validates the isolated synthetic managed account, never no-auth or an API key");
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

#[test]
#[ignore = "opt-in: explicit pinned Codex CLI, isolated HOME and loopback model double"]
fn an_actual_codex_cli_runs_through_the_real_owner_store_without_real_credentials_or_external_inference() {
    let slice=Slice::new();configured(&slice,"complete");
    let (_model,model_trace,code_home,selected)=native_model_fixture(&slice,None);
    let acknowledgements = watch_acknowledgements(&slice);
    let mut driver = launch_selected(&slice, selected.clone());
    slice.settle(&mut driver);
    let state = slice.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from(TASK)].provider, Provider::Codex);
    assert!(!state.tasks[&TaskId::from(TASK)].over());
    assert_eq!(state.tasks[&TaskId::from(TASK)].selection.as_ref(), Some(&selected));
    assert!(Slice::lifecycle(&slice.domain).pending_inputs().is_empty());
    let public: Vec<String> = slice.observer().prepare("SELECT content FROM task_context_items WHERE task_id=?1 AND source='assistant' ORDER BY id").unwrap()
        .query_map([TASK], |row| row.get(0)).unwrap().map(Result::unwrap).collect();
    assert_eq!(public, ["Native Codex owner/store proof completed.", "Native Codex owner/store proof completed."]);
    let requests = transcript(&model_trace);
    assert_eq!(requests.len(), 2, "one native model request for the task and one for its canonical wake");
    assert!(requests.iter().all(|request| request["model"].as_str() == selected.effective.as_ref().map(|effective| effective.model.model.as_str())));
    assert!(!acknowledgements.lock().unwrap().is_empty());
    assert_synthetic_native_auth(&code_home);
    driver.stop().unwrap();
}

#[test]
fn an_accepted_native_request_stays_pending_until_its_exact_native_user_item_is_observed() {
    let slice = Slice::new();
    let trace = configured(&slice, "accepted_without_consumption");
    let mut driver = launch(&slice);
    while slice.observations_with("alondra/inputAccepted") == 0 { driver.observe().unwrap(); }
    let state = slice.domain.state().unwrap();
    assert_eq!(Slice::lifecycle(&slice.domain).pending_inputs().len(), 1);
    assert!(!state.lifecycle_journal[&TaskId::from(TASK)].iter().any(|record| matches!(record, alondra_core::domain::LifecycleRecord::TurnStarted { .. } | alondra_core::domain::LifecycleRecord::InputConsumed { .. })));
    assert_eq!(transcript(&trace).iter().filter(|message| message["method"] == "turn/start").count(), 1);
    slice.run(&system(), "release-native-user-item", Command::ReadMcpStatus { task_id: TASK.into() });
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    assert!(Slice::lifecycle(&slice.domain).pending_inputs().is_empty());
    assert_eq!(transcript(&trace).iter().filter(|message| message["method"] == "turn/start").count(), 2, "the delayed consumption did not reoffer the original; only its canonical wake follows");
    driver.stop().unwrap();
}

fn transition_target(slice:&Slice,model:&str,effort:&str)->(alondra_core::model_selection::TaskSelection,alondra_core::model_selection::ModelSelection,String){
    let state=slice.domain.state().unwrap();
    let mut catalog=state.model_catalogs.iter().find(|catalog|catalog.account.provider==Provider::Codex).unwrap().clone();
    let mut changed=false;
    if !catalog.models[0].reasoning_efforts.iter().any(|effort|effort=="low"){catalog.models[0].reasoning_efforts.push("low".into());changed=true;}
    if !catalog.models.iter().any(|model|model.model=="fixture-second"){let mut second=catalog.models[0].clone();second.model="fixture-second".into();second.is_default=false;catalog.models.push(second);changed=true;}
    if changed{slice.run(&system(),"transition-catalog",Command::RecordModelCatalog{catalog});}
    let state=slice.domain.state().unwrap();let task:TaskId=TASK.into();
    let mut intent=state.tasks[&task].selection.clone().unwrap();
    intent.request_model(Some(alondra_core::model_selection::ModelKey{provider:Provider::Codex,model:model.into()}));
    let target=alondra_core::model_selection::ModelSelection{model:intent.requested.clone().unwrap(),reasoning_effort:Some(effort.into()),catalog_revision:"fixture-catalog".into()};
    (intent,target,state.task_selection_stamp(&task).unwrap())
}
fn native_transition_request(slice:&Slice,intent:alondra_core::model_selection::TaskSelection,target:alondra_core::model_selection::ModelSelection,stamp:&str)->serde_json::Value{
    crate::provider_transition::request(&slice.domain,&user(),&TASK.into(),AccountKey::new(Provider::Codex,AccountId::default_account()),target,intent,"User selected this native model",stamp,||Ok(())).unwrap()
}
#[test]
fn codex_transition_rechecks_authority_and_stale_selection_without_persisting_a_partial_intent(){
    let slice=Slice::new();configured(&slice,"complete");slice.domain.enable_sdk(slice.backend.clone());
    let mut driver=launch(&slice);slice.settle(&mut driver);
    let(intent,target,stamp)=transition_target(&slice,"fixture-second","high");
    let before=slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].selection.clone();
    let error=crate::provider_transition::request(&slice.domain,&user(),&TASK.into(),AccountKey::new(Provider::Codex,AccountId::default_account()),target.clone(),intent.clone(),"User selected native model",&stamp,||Err(DomainError::new("forbidden","Device authorization was revoked"))).unwrap_err();assert_eq!(error.code,"forbidden");
    let error=crate::provider_transition::request(&slice.domain,&user(),&TASK.into(),AccountKey::new(Provider::Codex,AccountId::default_account()),target,intent,"User selected native model","stale",||Ok(())).unwrap_err();assert_eq!(error.code,"stale_selection");
    let state=slice.domain.state().unwrap();assert_eq!(state.tasks[&TaskId::from(TASK)].selection,before);assert!(state.tasks[&TaskId::from(TASK)].pending_selection.is_none());
    assert_eq!(slice.observer().query_row("SELECT count(*) FROM provider_transition_steps",[],|row|row.get::<_,i64>(0)).unwrap(),0);
    driver.stop().unwrap();
}
#[tokio::test]
async fn a_cell_started_after_a_model_request_aborts_the_change_and_waits_on_the_original_model() {
    cell_model_request_race(false).await;
}

#[tokio::test]
async fn a_catalog_refresh_does_not_trap_a_running_cell_behind_an_unapplied_model_request() {
    cell_model_request_race(true).await;
}

async fn cell_model_request_race(refresh_catalog: bool) {
    use crate::domain_store::provider_transition::Phase;
    let slice=Slice::new();let trace=configured(&slice,"hold");
    let backend_path=slice._home.join("codex-backend.json");let mut backend:SdkBackend=serde_json::from_slice(&std::fs::read(&backend_path).unwrap()).unwrap();
    backend.native_environment.insert("ALONDRA_FAKE_MODELS".into(),"fixture-model,fixture-second".into());
    backend.native_environment.insert("ALONDRA_FAKE_FINISH_ON_MCP_STATUS".into(),"1".into());
    let fixture=slice._home.join("cell-model-fixture.json");std::fs::write(&fixture,br#"{"codeCellOnStatus":true}"#).unwrap();backend.native_environment.insert("ALONDRA_FAKE_ACCOUNT_FILE".into(),fixture.display().to_string());
    std::fs::write(backend_path,serde_json::to_vec(&backend).unwrap()).unwrap();slice.domain.enable_sdk(slice.backend.clone());
    let mut driver=launch(&slice);slice.until(&mut driver,|slice|Slice::lifecycle(&slice.domain).activity()==ActivityView::Working);let instance=driver.agent.clone();
    assert!(!slice.domain.state().unwrap().has_live_native_code_cells(&TASK.into()));
    let(intent,target,stamp)=transition_target(&slice,"fixture-second","high");native_transition_request(&slice,intent,target.clone(),&stamp);
    let(pulse,mut changed)=tokio::sync::watch::channel(0_u64);let published=pulse.clone();slice.on_acknowledge(move||pulse.send_modify(|count|*count+=1));slice.domain.before_owner_publish(move||published.send_modify(|count|*count+=1));slice.domain.add_sdk_owner(Owner::spawn(driver));
    assert_eq!(crate::provider_transition::advance(&slice.domain,&TASK.into()).await.unwrap(),Phase::Draining);
    slice.run(&system(),"create-cell-after-model-request",Command::ReadMcpStatus{task_id:TASK.into()});
    while slice.observations_with("alondra/codeCellContinuation")==0{changed.changed().await.unwrap();}
    let state=slice.domain.state().unwrap();assert!(state.tasks[&TaskId::from(TASK)].pending_selection.is_some());assert!(state.has_live_native_code_cells(&TASK.into()));
    assert_eq!(state.lifecycle_of(&TASK.into()).unwrap().activity(),ActivityView::OwnWork);
    assert_eq!(state.lifecycle_of(&TASK.into()).unwrap().pending_inputs().len(),1,"exactly one durable cell follow-up waits behind the model fence");
    if refresh_catalog {
        let mut catalog = state.model_catalogs.iter().find(|catalog| catalog.account == AccountKey::native(Provider::Codex)).unwrap().clone();
        catalog.revision = "refreshed-catalog".into();
        slice.run(&system(), "refresh-catalog-while-cell-running", Command::RecordModelCatalog { catalog });
    }
    assert_eq!(crate::provider_transition::advance(&slice.domain,&TASK.into()).await.unwrap(),Phase::Aborted);
    let state=slice.domain.state().unwrap();let task=&state.tasks[&TaskId::from(TASK)];
    assert_eq!(task.selection.as_ref().unwrap().requested,Some(target.model.clone()));assert_eq!(task.selection.as_ref().unwrap().effective,selection().effective);assert!(task.pending_selection.is_none());assert_eq!(task.account_key(),AccountKey::native(Provider::Codex));
    assert!(state.interactions.values().any(|notice|notice.body=="native_code_cells_active" && !notice.blocking));
    assert!(!crate::provider_transition::holds_execution(&slice.domain,&TASK.into()));assert!(slice.control_rows().iter().all(|(kind,_,_)|kind!="set_model"));
    while slice.observations_with("alondra/codeCellFinished")==0 || !Slice::lifecycle(&slice.domain).pending_inputs().is_empty() || Slice::lifecycle(&slice.domain).activity()!=ActivityView::Resting {changed.changed().await.unwrap();}
    let native=transcript(&trace);let turns:Vec<_>=native.iter().filter(|message|message["method"]=="turn/start").collect();let follow=turns.iter().find(|message|message["params"]["input"][0]["text"].as_str().is_some_and(|text|text.contains("functions.wait"))).unwrap();
    assert_eq!(follow["params"]["model"],"fixture-model");assert_eq!(follow["params"]["threadId"],turns[0]["params"]["threadId"]);
    let(intent,mut target,stamp)=transition_target(&slice,"fixture-second","high");
    target.catalog_revision = slice.domain.state().unwrap().model_catalogs.iter().find(|catalog| catalog.account == AccountKey::native(Provider::Codex)).unwrap().revision.clone();
    native_transition_request(&slice,intent,target.clone(),&stamp);
    assert_eq!(crate::provider_transition::advance(&slice.domain,&TASK.into()).await.unwrap(),Phase::Draining);
    while !slice.control_rows().iter().any(|(kind,_,status)|kind=="set_model" && status=="handed"){changed.changed().await.unwrap();}
    assert_eq!(crate::provider_transition::advance(&slice.domain,&TASK.into()).await.unwrap(),Phase::Done);
    let state=slice.domain.state().unwrap();let task=&state.tasks[&TaskId::from(TASK)];assert_eq!(task.agent_id,Some(instance));assert!(task.pending_selection.is_none());assert_eq!(task.selection.as_ref().unwrap().effective,Some(target));assert!(!state.has_live_native_code_cells(&TASK.into()));
    assert_eq!(transcript(&trace).iter().filter(|message|message["method"]=="initialize").count(),1);
    slice.domain.stop_sdk_owners();
}

#[tokio::test]
async fn a_positive_native_model_control_rejection_is_aborted_and_can_be_retried_without_a_new_session() {
    use crate::domain_store::provider_transition::Phase;
    let slice=Slice::new();let trace=configured(&slice,"complete");
    let backend_path=slice._home.join("codex-backend.json");let mut backend:SdkBackend=serde_json::from_slice(&std::fs::read(&backend_path).unwrap()).unwrap();
    backend.native_environment.insert("ALONDRA_FAKE_MODELS".into(),"fixture-model,fixture-second".into());
    let fixture=slice._home.join("model-control-fixture.json");std::fs::write(&fixture,b"{}").unwrap();backend.native_environment.insert("ALONDRA_FAKE_ACCOUNT_FILE".into(),fixture.display().to_string());
    std::fs::write(backend_path,serde_json::to_vec(&backend).unwrap()).unwrap();slice.domain.enable_sdk(slice.backend.clone());
    let mut driver=launch(&slice);slice.settle(&mut driver);let instance=driver.agent.clone();
    let(intent,target,stamp)=transition_target(&slice,"fixture-second","high");native_transition_request(&slice,intent,target.clone(),&stamp);
    std::fs::write(&fixture,br#"{"models":{"data":[{"id":"fixture-model","model":"fixture-model","displayName":"Fixture","isDefault":true,"hidden":false,"supportedReasoningEfforts":[{"reasoningEffort":"high","description":"High"}],"defaultReasoningEffort":"high","inputModalities":["text"]}],"nextCursor":null}}"#).unwrap();
    let(pulse,mut changed)=tokio::sync::watch::channel(0_u64);slice.domain.before_owner_publish(move||pulse.send_modify(|count|*count+=1));slice.domain.add_sdk_owner(Owner::spawn(driver));
    assert_eq!(crate::provider_transition::advance(&slice.domain,&TASK.into()).await.unwrap(),Phase::Draining);
    while !slice.control_rows().iter().any(|(kind,_,status)|kind=="set_model" && status=="rejected"){changed.changed().await.unwrap();}
    assert_eq!(crate::provider_transition::advance(&slice.domain,&TASK.into()).await.unwrap(),Phase::Aborted);
    let state=slice.domain.state().unwrap();let task=&state.tasks[&TaskId::from(TASK)];assert_eq!(task.selection.as_ref().unwrap().requested,Some(target.model.clone()));assert_eq!(task.selection.as_ref().unwrap().effective,selection().effective);assert!(task.pending_selection.is_none());assert_eq!(task.account_key(),AccountKey::native(Provider::Codex));
    assert!(state.interactions.values().any(|notice|notice.body=="native_model_control_rejected" && !notice.blocking));assert!(!crate::provider_transition::holds_execution(&slice.domain,&TASK.into()));
    std::fs::write(&fixture,b"{}").unwrap();let(intent,target,stamp)=transition_target(&slice,"fixture-second","high");native_transition_request(&slice,intent,target.clone(),&stamp);
    assert_eq!(crate::provider_transition::advance(&slice.domain,&TASK.into()).await.unwrap(),Phase::Draining);
    while !slice.control_rows().iter().any(|(kind,_,status)|kind=="set_model" && status=="handed"){changed.changed().await.unwrap();}
    assert_eq!(crate::provider_transition::advance(&slice.domain,&TASK.into()).await.unwrap(),Phase::Done);
    let task=slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].clone();assert_eq!(task.agent_id,Some(instance));assert_eq!(task.selection.as_ref().unwrap().effective,Some(target));assert!(task.pending_selection.is_none());
    assert_eq!(transcript(&trace).iter().filter(|message|message["method"]=="initialize").count(),1);slice.domain.stop_sdk_owners();
}

#[tokio::test]
async fn codex_transition_model_is_pending_until_the_exact_native_configuration_receipt(){
    let slice=Slice::new();let trace=configured(&slice,"complete");
    let backend_path=slice._home.join("codex-backend.json");let mut backend:SdkBackend=serde_json::from_slice(&std::fs::read(&backend_path).unwrap()).unwrap();backend.native_environment.insert("ALONDRA_FAKE_MODELS".into(),"fixture-model,fixture-second".into());std::fs::write(backend_path,serde_json::to_vec(&backend).unwrap()).unwrap();
    slice.domain.enable_sdk(slice.backend.clone());let mut driver=launch(&slice);slice.settle(&mut driver);let instance=driver.agent.clone();
    let(intent,target,stamp)=transition_target(&slice,"fixture-second","high");native_transition_request(&slice,intent,target.clone(),&stamp);
    slice.run(&system(),"queued-during-model-change",Command::TellNode{task_id:TASK.into(),text:"Use the configured model for this next input".into()});let queued=Slice::lifecycle(&slice.domain).pending_inputs()[0].as_str().to_owned();
    let (published,mut after_feed)=tokio::sync::watch::channel(false);let weak=Arc::downgrade(&slice.domain);let expected_input=queued.clone();
    slice.on_acknowledge(move||{if let Some(domain)=weak.upgrade() && !Slice::lifecycle(&domain).is_pending(&alondra_foundation::InputId::new(&expected_input).unwrap()){published.send_replace(true);}});
    let(pulse,mut changed)=tokio::sync::watch::channel(0_u64);slice.domain.before_owner_publish(move||pulse.send_modify(|value|*value+=1));slice.domain.add_sdk_owner(Owner::spawn(driver));
    assert_eq!(crate::provider_transition::advance(&slice.domain,&TASK.into()).await.unwrap(),crate::domain_store::provider_transition::Phase::Draining);
    assert_eq!(slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].selection.as_ref().unwrap().effective,selection().effective);
    while !slice.control_rows().iter().any(|(kind,_,status)|kind=="set_model" && status=="handed"){changed.changed().await.unwrap();}
    assert!(Slice::lifecycle(&slice.domain).is_pending(&alondra_foundation::InputId::new(&queued).unwrap()));
    assert_eq!(crate::provider_transition::advance(&slice.domain,&TASK.into()).await.unwrap(),crate::domain_store::provider_transition::Phase::Done);
    while Slice::lifecycle(&slice.domain).is_pending(&alondra_foundation::InputId::new(&queued).unwrap()){changed.changed().await.unwrap();}
    while !*after_feed.borrow(){after_feed.changed().await.unwrap();}
    assert_eq!(slice.domain.live.mods.session(&instance.0).unwrap().model.as_deref(),Some("fixture-second"),"the owner feed publishes the native accepted model before acknowledging its consumed input");
    let task=slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].clone();assert_eq!(task.agent_id,Some(instance.clone()));assert!(task.pending_selection.is_none());assert_eq!(task.selection.as_ref().unwrap().effective,Some(target.clone()));
    assert_eq!(LaunchOptions::load(&slice.backend.state_root.join(&instance.0)).unwrap().selection.as_ref().unwrap().effective,Some(target));
    let native=transcript(&trace);assert_eq!(native.iter().filter(|message|message["method"]=="initialize").count(),1);let next=native.iter().find(|message|message["method"]=="turn/start" && message["params"]["input"][0]["text"]=="Use the configured model for this next input").unwrap();assert_eq!(next["params"]["model"],"fixture-second");assert!(native.iter().filter(|message|message["method"]=="turn/start").all(|message|message["params"]["threadId"]==next["params"]["threadId"]));slice.domain.stop_sdk_owners();
}

#[test]
fn codex_active_account_provider_and_unverified_effort_changes_have_no_native_or_durable_effects(){
    let slice=Slice::new();let trace=configured(&slice,"complete");slice.domain.enable_sdk(slice.backend.clone());let mut driver=launch(&slice);slice.settle(&mut driver);
    let before=slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].selection.clone();
    let(intent,target,stamp)=transition_target(&slice,"fixture-model","low");
    let error=crate::provider_transition::request(&slice.domain,&user(),&TASK.into(),AccountKey::new(Provider::Codex,AccountId::default_account()),target,intent,"Change effort",&stamp,||Ok(())).unwrap_err();assert_eq!(error.code,"unsupported_live_effort");
    let(intent,target,stamp)=transition_target(&slice,"fixture-second","high");
    for account in [AccountKey::new(Provider::Codex,AccountId::parse("other").unwrap()),AccountKey::new(Provider::Claude,AccountId::default_account())]{
        let error=crate::provider_transition::request(&slice.domain,&user(),&TASK.into(),account,target.clone(),intent.clone(),"Change account",&stamp,||Ok(())).unwrap_err();assert_eq!(error.code,"immutable_account");
    }
    let state=slice.domain.state().unwrap();assert_eq!(state.tasks[&TaskId::from(TASK)].selection,before);assert!(state.tasks[&TaskId::from(TASK)].pending_selection.is_none());
    assert_eq!(slice.observer().query_row("SELECT count(*) FROM provider_transition_steps",[],|row|row.get::<_,i64>(0)).unwrap(),0);assert_eq!(transcript(&trace).iter().filter(|message|message["method"]=="initialize").count(),1);driver.stop().unwrap();
}

fn native_business_evidence(slice:&Slice,stage:&str,model_trace:&std::path::Path)->String{
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

async fn native_business_until(slice:&Slice,changed:&mut tokio::sync::watch::Receiver<u64>,stage:&str,model_trace:&std::path::Path,predicate:impl Fn(&DomainState)->bool){
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
        let next=tokio::time::timeout(std::time::Duration::from_secs(30),changed.changed()).await;
        if next.is_err(){panic!("The native business journey stalled at {stage}: {}",native_business_evidence(slice,stage,model_trace));}
        next.unwrap().unwrap();
    }
}

#[tokio::test(flavor="multi_thread",worker_threads=4)]
#[ignore="opt-in: real pinned Codex, real Alondra MCP CLI and authenticated loopback daemon; no credentials or external inference"]
async fn an_actual_codex_cli_asks_enters_native_plan_and_presents_the_result_through_alondra(){
    let slice=Slice::with_database_name(HostRoot::new(),crate::domain_database_migration::DATABASE);configured(&slice,"complete");
    for args in [vec!["init","-b","main"],vec!["config","user.name","Alondra fixture"],vec!["config","user.email","fixture@example.invalid"],vec!["commit","--allow-empty","-m","Initial fixture"]] {
        let output=std::process::Command::new("/usr/bin/git").args(args).current_dir(&slice.worktree).env_clear().env("HOME",&*slice._home).env("PATH","/usr/bin:/bin").env("GIT_CONFIG_NOSYSTEM","1").env("GIT_CONFIG_GLOBAL","/dev/null").output().unwrap();
        assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    }
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();
    let journey=slice._home.join("native-journey.json");std::fs::write(&journey,serde_json::to_vec(&serde_json::json!({"task":TASK,"expected_answer":"English."})).unwrap()).unwrap();
    let (_model,model_trace,code_home,selected)=native_model_fixture(&slice,Some((&journey,port)));
    let daemon=crate::domain_bootstrap::DomainDaemon::over_shared(&slice._home,Arc::clone(&slice.domain)).unwrap();
    let outbox_domain=Arc::clone(&slice.domain);let outbox_home=slice._home.to_path_buf();
    let push=Arc::new(crate::push::Push::open(&outbox_home).unwrap());assert!(!push.enabled());
    let outbox_client=daemon.client();let(stop_outbox,outbox_stopped)=tokio::sync::oneshot::channel();
    let outbox=tokio::spawn(async move {tokio::select! {
        result=crate::domain_delivery::run(outbox_domain,outbox_home,push,None,outbox_client)=>result,
        _=outbox_stopped=>Ok(()),
    }});
    let auth=Arc::clone(&daemon.auth);let app=crate::domain_bootstrap::router(daemon).layer(axum::middleware::from_fn_with_state(auth,crate::auth::guard));
    let (shutdown,closed)=tokio::sync::oneshot::channel();
    let server=tokio::spawn(async move {axum::serve(listener,app.into_make_service_with_connect_info::<std::net::SocketAddr>()).with_graceful_shutdown(async{let _=closed.await;}).await.unwrap();});
    let domain=Arc::clone(&slice.domain);let fixture=Arc::new(tokio::sync::Mutex::new(slice));let scope=Arc::clone(&fixture);
    let scenario=tokio::spawn(async move {
        let slice=scope.lock().await;
        let (pulse,mut changed)=tokio::sync::watch::channel(0_u64);slice.on_acknowledge(move||pulse.send_modify(|count|*count+=1));
        let effective=selected.effective.clone().unwrap();let backend=slice.backend.clone();let owner_domain=Arc::clone(&slice.domain);
        let launch=Launch{task:TASK.into(),name:"Native report journey".into(),directory:slice.worktree.display().to_string(),prompt:"Write a report. Ask which language it should use, prepare a plan for approval, then present the completed report through Alondra.".into(),plugin:slice.plugin.clone(),options:LaunchOptions{provider:Provider::Codex,selection:Some(selected),model:Some(effective.model.model),effort:effective.reasoning_effort,disallowed_tools:vec![]},model:None,account:AccountId::default_account()};
        let driver=tokio::task::spawn_blocking(move||start(owner_domain,&backend,launch).unwrap()).await.unwrap();slice.domain.add_sdk_owner(Owner::spawn(driver));
        native_business_until(&slice,&mut changed,"language question",&model_trace,|state|state.interactions.contains_key(&InteractionId::from("native-language-decision"))).await;
        assert!(!slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].over());slice.answer("native-language-decision","english",None).unwrap();
        native_business_until(&slice,&mut changed,"native plan after answered question",&model_trace,|state|state.lifecycle_of(&TASK.into()).unwrap().cards().iter().any(|(id,_)|state.interactions.get(id).and_then(|interaction|interaction.card.content.as_ref()).is_some_and(|content|serde_json::to_value(&content.kind).unwrap()==serde_json::json!("plan")))).await;
        let state=slice.domain.state().unwrap();let card=state.lifecycle_of(&TASK.into()).unwrap().cards().iter().find(|(id,_)|state.interactions.get(id).and_then(|interaction|interaction.card.content.as_ref()).is_some_and(|content|serde_json::to_value(&content.kind).unwrap()==serde_json::json!("plan"))).unwrap().0.clone();
        assert!(slice.kept.lock().unwrap().values().any(|bytes|String::from_utf8_lossy(bytes).contains("\"type\":\"plan\"")),"the real Codex CLI emitted a native plan item");
        slice.answer(&card.0,Hold::APPROVE,None).unwrap();
        native_business_until(&slice,&mut changed,"final presentation after approved native plan",&model_trace,|state|state.tasks[&TaskId::from(TASK)].final_presentation.is_some() && state.lifecycle_of(&TASK.into()).unwrap().activity()==ActivityView::Resting && slice.kept.lock().unwrap().values().any(|bytes|{let raw=String::from_utf8_lossy(bytes);raw.contains("\"tool\":\"present_result\"") && raw.contains("\"status\":\"completed\"")})).await;
        let state=slice.domain.state().unwrap();let task=&state.tasks[&TaskId::from(TASK)];assert!(task.final_presentation.is_some());assert!(!task.over(),"canonical delivery still waits for the user's review");
        let requests=transcript(&model_trace);assert!(requests.len()>=6);
        let stages=transcript(&model_trace.with_extension("stages"));
        let plan_requests:Vec<_>=stages.iter().filter(|stage|stage["step"]==5 && stage["advanced"]==true).collect();
        assert_eq!(plan_requests.len(),1,"the model emitted exactly one complete native plan");
        let plan_index=usize::try_from(plan_requests[0]["request"].as_u64().unwrap().checked_sub(1).unwrap()).unwrap();
        let native_plan=requests[plan_index]["input"].as_array().unwrap().iter().filter(|message|message["role"]=="developer").flat_map(|message|message["content"].as_array().into_iter().flatten()).any(|part|part["type"]=="input_text" && part["text"].as_str().is_some_and(|text|text.contains("<collaboration_mode>") && text.contains("# Plan Mode (Conversational)") && text.contains("You are in **Plan Mode** until a developer message explicitly ends it.")));
        assert!(native_plan,"the plan turn used the actual pinned native Plan Mode developer instructions");
        let mut reply_messages=std::collections::BTreeSet::new();let mut first_reply=None;
        for (index,request) in requests.iter().enumerate(){
            for message in request["input"].as_array().unwrap().iter().filter(|message|message["role"]=="user"){
                if message["content"].as_array().is_some_and(|parts|parts.iter().any(|part|part["type"]=="input_text" && part["text"].as_str().is_some_and(|text|text.contains("English.")))){
                    reply_messages.insert(message["id"].as_str().unwrap().to_owned());first_reply.get_or_insert(index);
                }
            }
        }
        assert_eq!(reply_messages.len(),1,"the real outbox delivered one English answer into the same native session");
        assert!(first_reply.unwrap()<plan_index,"the native session received the chosen language before preparing its plan");
        assert!(slice.domain.store.lock().unwrap().pending_messages().unwrap().iter().all(|message|message.recipient_kind!="agent" || message.interaction_id.as_deref()!=Some("native-language-decision")),"the actual producer committed the answer handoff");
        assert_synthetic_native_auth(&code_home);
    });
    let result=scenario.await;
    let _=stop_outbox.send(());let delivered=outbox.await;
    domain.stop_sdk_owners();
    let _=shutdown.send(());server.await.unwrap();
    drop(fixture);
    delivered.unwrap().unwrap();
    if let Err(error)=result {if error.is_panic(){std::panic::resume_unwind(error.into_panic())}else{panic!("the native journey stopped: {error}")}}
}

#[test]
fn codex_native_token_samples_reach_usage_only_through_the_owner_transaction(){
    let slice=Slice::new();configured(&slice,"complete");
    let path=slice._home.join("codex-backend.json");let mut backend:SdkBackend=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();backend.native_environment.insert("ALONDRA_FAKE_TOKEN_USAGE".into(),"1".into());std::fs::write(path,serde_json::to_vec(&backend).unwrap()).unwrap();
    let mut driver=launch(&slice);slice.settle(&mut driver);
    let rows:Vec<(String,i64,i64,i64,i64,Option<i64>,bool)>=slice.observer().prepare("SELECT model,input,output,cache_read,cache_write,cost_micros,complete FROM usage_sessions WHERE task_id=?1 AND provider='codex'").unwrap().query_map([TASK],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?))).unwrap().map(Result::unwrap).collect();
    assert_eq!(rows,vec![("codex/fixture-model".into(),20,10,0,0,None,false)],"the first unknown baseline is skipped and only the second measured delta is attributed, with no invented price");driver.stop().unwrap();
}

#[test]
fn codex_admission_rechecks_billing_and_credential_generation_after_selection(){
    use std::io::Write as _;
    let slice=Slice::new();let trace=configured(&slice,"complete");let selected=selection();
    let skill=slice.plugin.join("skills/alondra/SKILL.md");let mission=std::fs::read(&skill).unwrap();std::fs::remove_file(&skill).unwrap();
    assert!(std::process::Command::new("/usr/bin/mkfifo").args(["-m","600"]).arg(&skill).status().unwrap().success());
    std::thread::scope(|scope|{
        let runner=scope.spawn(||{
            let effective=selected.effective.clone().unwrap();
            start(Arc::clone(&slice.domain),&slice.backend,Launch{task:TASK.into(),name:"Billing race".into(),directory:slice.worktree.display().to_string(),prompt:"This task must not start against an unauthorized paid account".into(),plugin:slice.plugin.clone(),options:LaunchOptions{provider:Provider::Codex,model:Some(effective.model.model),effort:effective.reasoning_effort,selection:Some(selected),disallowed_tools:vec![]},model:None,account:AccountId::default_account()})
        });
        let mut writer=std::fs::OpenOptions::new().write(true).open(&skill).unwrap();
        let state=slice.domain.state().unwrap();let mut accounts=state.accounts.clone();let account=accounts.iter_mut().find(|account|account.provider==Provider::Codex && account.id.is_default()).unwrap();account.billing=alondra_core::accounts::AccountBilling::PayPerUse;account.sign_ins+=1;
        slice.run(&system(),"billing-changed-during-configuration",Command::RecordAccounts{accounts,reserve:state.account_reserve});
        writer.write_all(&mission).unwrap();drop(writer);
        match runner.join().unwrap(){Err(error)=>assert!(error.to_string().contains("paid per use") || error.to_string().contains("credentials changed"),"{error}"),Ok(mut driver)=>{driver.stop().unwrap();panic!("a stale subscription decision started against a changed paid account")}}
    });
    assert!(!trace.exists(),"no native process or inference was admitted");assert!(Slice::lifecycle(&slice.domain).execution().is_none());
}

#[tokio::test]
async fn codex_transition_cancellation_before_source_drain_aborts_without_a_new_native_host(){
    use crate::domain_store::provider_transition::Phase;
    let slice=Slice::new();let trace=configured(&slice,"complete");slice.domain.enable_sdk(slice.backend.clone());
    let mut driver=launch(&slice);slice.settle(&mut driver);
    let(intent,target,stamp)=transition_target(&slice,"fixture-second","high");native_transition_request(&slice,intent,target,&stamp);
    slice.run(&user(),"cancel-during-selection",Command::CancelTask{task_id:TASK.into()});
    assert_eq!(crate::provider_transition::advance(&slice.domain,&TASK.into()).await.unwrap(),Phase::Aborted);
    assert_eq!(transcript(&trace).iter().filter(|message|message["method"]=="initialize").count(),1);
    assert!(slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].pending_selection.is_none());driver.stop().unwrap();
}


#[test]
fn a_codex_plan_approved_after_its_turn_ended_commits_the_new_turn_under_the_same_owner() {
    let slice = Slice::new();
    let trace = configured(&slice, "plan");
    let mut driver = launch(&slice);
    slice.until(&mut driver, |slice| slice.observations_with("turn/completed") == 1);
    let cards = Slice::open_decision_cards(&slice.domain);
    assert_eq!(cards.len(), 1);
    let card = &cards[0];
    let state = slice.domain.state().unwrap();
    let epoch = state.lifecycle_journal[&TaskId::from(TASK)].iter().find_map(|record| match record {
        LifecycleRecord::DecisionAsked { epoch, .. } => Some(*epoch), _ => None,
    }).unwrap();
    assert!(state.interactions[&InteractionId::from(card.as_str())].card.content.as_ref().is_some_and(|content| serde_json::to_value(&content.kind).unwrap() == serde_json::json!("plan")));
    let owner = driver.agent().clone();
    let original_execution = Slice::lifecycle(&slice.domain).execution().cloned().unwrap();
    let before = transcript(&trace);
    let first_turns: Vec<_> = before.iter().filter(|message| message["method"] == "turn/start").collect();
    assert_eq!(first_turns.len(), 1, "the original plan cannot start its implementation before approval");
    let original_thread = first_turns[0]["params"]["threadId"].clone();
    slice.answer(card, Hold::APPROVE, None).unwrap();
    while driver.pending_controls() > 0 || driver.pending_permits() > 0 || driver.waits_for_host() {
        driver.step(&|| false).unwrap();
    }
    let state = slice.domain.state().unwrap();
    let journal = &state.lifecycle_journal[&TaskId::from(TASK)];
    assert_eq!(journal.iter().filter(|record| matches!(record, LifecycleRecord::DecisionSettled { epoch: found } if *found == epoch)).count(), 1);
    assert_eq!(journal.iter().filter(|record| matches!(record, LifecycleRecord::TurnUnprompted { .. })).count(), 1);
    assert_eq!(slice.observations_with("alondra/planContinuationAccepted"), 1);
    assert_eq!(journal.iter().filter(|record| matches!(record, LifecycleRecord::DecisionContinued { epoch: found } if *found == epoch)).count(), 1,
        "Core canonically continues this exact approved epoch once");
    let original_turn = journal.iter().find_map(|record| match record { LifecycleRecord::TurnEnded { turn, .. } => Some(turn.as_str()), _ => None }).unwrap();
    let approved_turn = journal.iter().find_map(|record| match record { LifecycleRecord::TurnUnprompted { turn } => Some(turn.as_str()), _ => None }).unwrap();
    let observer = slice.observer();
    let mut records = observer.prepare("SELECT event FROM lifecycle_events WHERE task_id=?1 AND json_extract(event,'$.kind')='runtime' ORDER BY revision").unwrap();
    let events: Vec<serde_json::Value> = records.query_map([TASK], |row| row.get::<_, String>(0)).unwrap()
        .map(|row| serde_json::from_str(&row.unwrap()).unwrap()).collect();
    let cursor_of = |kind: &str, field: &str, identity: &str| {
        let matching: Vec<_> = events.iter().filter(|event| event["data"]["fact"]["kind"] == kind && event["data"]["fact"]["data"][field].as_str() == Some(identity)).collect();
        assert_eq!(matching.len(), 1, "exactly one committed {kind} for this native plan identity");
        matching[0]["data"]["cursor"].as_u64().unwrap()
    };
    let callback = alondra_domain::decision_callback(original_execution.generation(), epoch);
    let continuation = alondra_domain::decision_continuation(original_execution.generation(), epoch);
    let ended = cursor_of("turn_ended", "turn", original_turn);
    let started = cursor_of("turn_started", "turn", approved_turn);
    let settled = cursor_of("callback_settled", "callback", callback.as_str());
    let continued = cursor_of("continuation_consumed", "continuation", continuation.as_str());
    assert_eq!((started, settled, continued), (ended + 1, ended + 2, ended + 3),
        "immutable committed boundaries prove exactly three native moves: new turn, canonical settlement, canonical continuation");
    let lifecycle = Slice::lifecycle(&slice.domain);
    assert_eq!(lifecycle.activity(), ActivityView::Resting, "no decision continuation may keep the owner waiting after the implementation turn ended");
    assert_eq!(lifecycle.execution(), Some(&original_execution));
    let native = transcript(&trace);
    assert_eq!(native.iter().filter(|message| message["method"] == "initialize").count(), 1);
    let turns: Vec<_> = native.iter().filter(|message| message["method"] == "turn/start").collect();
    assert!(turns.iter().all(|message| message["params"]["threadId"] == original_thread));
    assert_eq!(turns.iter().filter(|message| message["params"]["input"][0]["text"].as_str().is_some_and(|text| text.starts_with("Implement the plan the user just approved."))).count(), 1);
    assert_eq!(driver.agent(), &owner);
    assert!(Slice::lifecycle(&slice.domain).pending_inputs().is_empty());
    assert!(!state.tasks[&TaskId::from(TASK)].over());
    driver.stop().unwrap();
}

#[test]
fn requesting_plan_mode_keeps_one_durable_next_turn_input_across_native_replays() {
    let slice=Slice::new();configured(&slice,"complete");slice.domain.enable_sdk(slice.backend.clone());
    let mut driver=launch(&slice);slice.settle(&mut driver);
    let capability=slice.domain.bind_agent(&driver.agent.0).unwrap();
    let bearer=capability.expose_for_child_environment();
    assert!(Slice::lifecycle(&slice.domain).pending_inputs().is_empty());
    let first_result=slice.domain.request_plan_mode(bearer,&TASK.into(),"ab12").unwrap();
    let first:Vec<_>=Slice::lifecycle(&slice.domain).pending_inputs().iter().map(|input|input.as_str().to_owned()).collect();assert_eq!(first.len(),1);
    assert_eq!(slice.domain.request_plan_mode(bearer,&TASK.into(),"ab12").unwrap(),first_result,"a replay keeps its exact committed result");
    slice.domain.request_plan_mode(bearer,&TASK.into(),"ab13").unwrap();
    let again:Vec<_>=Slice::lifecycle(&slice.domain).pending_inputs().iter().map(|input|input.as_str().to_owned()).collect();assert_eq!(again,first,"retry and already-active plan mode do not duplicate its continuation");
    let body:String=slice.observer().query_row("SELECT body FROM lifecycle_inputs WHERE task_id=?1 AND input_id=?2",rusqlite::params![TASK,first[0]],|row|row.get(0)).unwrap();assert!(body.contains("Alondra has enabled plan mode"));
    assert!(slice.domain.request_plan_mode(bearer,&"another-task".into(),"ab14").is_err());driver.stop().unwrap();
}

#[test]
fn codex_managed_child_keeps_work_after_parent_turn_and_queues_one_durable_continuation(){
    let slice=Slice::new();let trace=configured(&slice,"child");let mut driver=launch(&slice);
    while Slice::lifecycle(&slice.domain).turn().is_some() || Slice::lifecycle(&slice.domain).activity()!=alondra_domain::ActivityView::OwnWork{advance(&mut driver)}
    assert!(Slice::lifecycle(&slice.domain).pending_inputs().is_empty());assert!(Slice::lifecycle(&slice.domain).cards().is_empty(),"a child plan cannot open the task's global plan approval");assert_eq!(transcript(&trace).iter().filter(|message|message["method"]=="turn/start").count(),1);
    slice.run(&system(),"complete-managed-child",Command::ReadMcpStatus{task_id:TASK.into()});
    while driver.pending_controls()>0 || driver.pending_permits()>0 || driver.waits_for_host(){driver.step(&||false).unwrap();}
    let state=slice.domain.state().unwrap();let journal=&state.lifecycle_journal[&TaskId::from(TASK)];
    let wakes:Vec<_>=journal.iter().filter_map(|record|match record{LifecycleRecord::Told{input,text:Some(text)} if text.starts_with("Managed native work finished.")=>Some(input),_=>None}).collect();assert_eq!(wakes.len(),1);
    assert_eq!(journal.iter().filter(|record|matches!(record,LifecycleRecord::TurnStarted{input,..} if input==wakes[0])).count(),1);
    assert!(journal.iter().any(|record|matches!(record,LifecycleRecord::WorkStarted{work,..} if work.contains("spawn-child"))));assert!(journal.iter().any(|record|matches!(record,LifecycleRecord::WorkContinued{..})));
    assert_ne!(Slice::lifecycle(&slice.domain).activity(),alondra_domain::ActivityView::OwnWork);driver.stop().unwrap();
}

#[test]
fn repeated_same_email_reauthentication_keeps_the_same_native_session_but_identity_replacement_refuses_new_input(){
    use alondra_core::accounts::AccountState;
    let slice=Slice::new();let trace=configured(&slice,"complete");
    let publish=|email:&str,signed_in:bool,id:&str|{
        let state=slice.domain.state().unwrap();let mut accounts=state.accounts.clone();let account=accounts.iter_mut().find(|account|account.provider==Provider::Codex && account.id.is_default()).unwrap();
        account.email=Some(email.into());account.state=if signed_in{AccountState::SignedIn}else{AccountState::SignedOut};
        slice.run(&system(),id,Command::RecordAccounts{accounts,reserve:state.account_reserve});
    };
    publish("first@example.test",true,"native-first-identity");
    // Catalog belongs to this native account, independently of its authentication counter.
    let state=slice.domain.state().unwrap();let catalog=state.model_catalogs.iter().find(|catalog|catalog.account.provider==Provider::Codex).unwrap().clone();
    slice.run(&system(),"native-first-catalog",Command::RecordModelCatalog{catalog});
    let mut driver=launch(&slice);slice.settle(&mut driver);let original=driver.worker.host_group();let original_execution=Slice::lifecycle(&slice.domain).execution().unwrap().clone();
    for round in [1,2]{
        publish("first@example.test",false,&format!("native-logout-{round}"));publish("FIRST@example.test",true,&format!("native-reauth-{round}"));
        carry_on(&slice.domain,&format!("Continue the same cached session after reauthentication {round}"));slice.settle(&mut driver);
        assert_eq!(driver.worker.host_group(),original);assert_eq!(Slice::lifecycle(&slice.domain).execution(),Some(&original_execution));
    }
    let before=transcript(&trace).iter().filter(|request|request["method"]=="turn/start").count();
    publish("replacement@example.test",true,"native-replaced-identity");carry_on(&slice.domain,"This input must remain queued for explicit user recovery");
    let error=driver.step_once(&||false).unwrap_err();assert!(error.to_string().contains("different known identity"),"{error}");
    assert_eq!(transcript(&trace).iter().filter(|request|request["method"]=="turn/start").count(),before);
    assert!(!Slice::lifecycle(&slice.domain).pending_inputs().is_empty());assert_eq!(driver.worker.host_group(),original);
    driver.stop().unwrap();
}


fn disabled_codex_waiting_for_native_capacity() -> (Slice, Driver, PathBuf, alondra_foundation::ExecutionStamp, InteractionId) {
    use alondra_core::accounts::{AccountState, ProviderAccount};
    let slice = Slice::new();
    configured(&slice, "quota");
    slice.domain.enable_sdk(slice.backend.clone());
    let mut config = alondra_core::accounts::AccountsConfig::fresh(); config.connect(Provider::Codex);
    config.codex = vec![ProviderAccount::implicit_default(), ProviderAccount { id: AccountId::parse("spare").unwrap(), name: "Spare".into(), enabled: true, billing: Default::default() }];
    std::fs::write(alondra_core::options::path_in(&slice._home), config.to_toml()).unwrap();
    let state = slice.domain.state().unwrap(); let mut accounts = state.accounts.clone();
    let account = accounts.iter_mut().find(|account| account.provider == Provider::Codex && account.id.is_default()).unwrap();
    account.email = Some("first@example.test".into()); account.native_identity = Some("fixture-workspace".into());
    accounts.push(alondra_core::accounts::AccountStatus { provider: Provider::Codex, id: AccountId::parse("spare").unwrap(), name: "Spare".into(), state: AccountState::SignedOut,
        native_identity: None, email: None, plan: None, billing: Default::default(), sign_ins: 0 });
    slice.run(&system(), "current-codex-known-identity", Command::RecordAccounts { accounts, reserve: state.account_reserve });
    slice.run(&system(), "current-codex-configuration", Command::RecordAccountConfiguration {
        order: config.ordered_accounts().into_iter().map(|(key, _)| key).collect(), version: alondra_core::accounts::config_version(&config) });
    let mut driver = launch(&slice); slice.settle(&mut driver);
    let state = slice.domain.state().unwrap(); let task = TaskId::from(TASK);
    let held = state.claude_held().into_iter().find(|held| held.task == task && held.provider == Provider::Codex && held.hold == alondra_core::domain::ClaudeHold::Usage).expect("the actual native quota result held the existing node");
    let execution = state.lifecycle_of(&task).unwrap().execution().unwrap().clone();
    crate::accounts::change_for_provider(&slice.domain, &slice._home, &user(), Provider::Codex,
        &alondra_core::accounts::AccountChange::Disable { account: AccountId::default_account() }).unwrap();
    slice.run(&system(), "current-codex-past-native-limit", Command::RecordAccountUsage {
        provider: Provider::Codex, account: AccountId::default_account(), at: 1, day: "2026-10-26".into(),
        reading: alondra_core::usage::UsageReading { plan: None, complete: false, windows: vec![alondra_core::usage::WindowReading {
            kind: alondra_core::usage::WindowKind::Other("codex:account:limit".into()), used: alondra_core::usage::Share::NONE, exhausted: true, resets_at: None }] } });
    let sidecar = slice._home.join("codex-native.account.json");
    std::fs::write(&sidecar, serde_json::to_vec(&serde_json::json!({"trace":true})).unwrap()).unwrap();
    (slice, driver, sidecar, execution, held.card)
}

fn assert_no_native_inference_from_account_probe(sidecar: &Path) {
    let calls = transcript(&PathBuf::from(format!("{}.calls", sidecar.display())));
    assert!(calls.iter().any(|request| request["method"] == "account/read"));
    assert!(calls.iter().any(|request| request["method"] == "getAuthStatus" && request["params"] == serde_json::json!({"includeToken":false,"refreshToken":false})));
    assert!(calls.iter().all(|request| !matches!(request["method"].as_str(), Some("turn/start" | "thread/start" | "thread/resume"))), "a recovery probe is native metadata, never an inference or session transfer");
}

#[test]
fn a_disabled_existing_codex_node_recovers_from_real_native_capacity_without_becoming_new_work_eligible() {
    use alondra_core::accounts::AccountState;
    let (slice, mut driver, sidecar, execution, card) = disabled_codex_waiting_for_native_capacity();
    assert!(crate::accounts::probe_current_codex(&slice.domain, &TASK.into(), Some(alondra_core::domain::ClaudeHold::Usage)).unwrap());
    let state = slice.domain.state().unwrap(); let key = AccountKey::native(Provider::Codex);
    let account = state.accounts.iter().find(|account| account.provider == key.provider && account.id == key.account).unwrap();
    assert_eq!(account.state, AccountState::Disabled);
    assert!(!alondra_core::usage::eligible_account_keys(&state.account_order, &state.accounts, &state.account_usage, alondra_core::now_millis()).contains(&key));
    assert!(state.interactions[&card].withdrawn);
    assert!(state.claude_held().iter().all(|held| held.task != TaskId::from(TASK)));
    assert_eq!(state.lifecycle_of(&TASK.into()).unwrap().execution(), Some(&execution));
    assert_eq!(state.tasks[&TaskId::from(TASK)].account_key(), key);
    assert_no_native_inference_from_account_probe(&sidecar);
    let host = driver.worker.host_group();
    std::fs::write(&sidecar, serde_json::to_vec(&serde_json::json!({"trace":true,"turnMode":"complete"})).unwrap()).unwrap();
    slice.settle(&mut driver);
    assert_eq!(driver.worker.host_group(), host, "recovery continues in the original host");
    assert_eq!(Slice::lifecycle(&slice.domain).execution(), Some(&execution));
    let calls = transcript(&PathBuf::from(format!("{}.calls", sidecar.display())));
    assert!(calls.iter().any(|request| request["method"] == "turn/start"), "the real owner delivered the existing task's continuation");
    assert!(calls.iter().all(|request| !matches!(request["method"].as_str(), Some("thread/start" | "thread/resume"))), "an existing disabled node never creates or transfers its native conversation");
    driver.stop().unwrap();
}

#[test]
fn an_existing_codex_probe_records_a_real_workspace_replacement_without_publishing_quota_models_or_resuming() {
    use alondra_core::accounts::AccountState;
    let (slice, mut driver, sidecar, execution, card) = disabled_codex_waiting_for_native_capacity();
    let generation = crate::accounts::generation_provider(&slice.domain.state().unwrap(), &AccountKey::native(Provider::Codex));
    std::fs::write(&sidecar, serde_json::to_vec(&serde_json::json!({"trace":true,"accountRead":{"account":{"type":"chatgpt","email":"first@example.test","planType":"plus"},"requiresOpenaiAuth":true},"rateLimits":{"accountId":"replacement-workspace","ordinaryUsageAllowed":true,"rateLimits":null}})).unwrap()).unwrap();
    assert!(crate::accounts::probe_current_codex(&slice.domain, &TASK.into(), Some(alondra_core::domain::ClaudeHold::Usage)).unwrap_err().contains("original managed account and workspace"));
    let state = slice.domain.state().unwrap(); let key = AccountKey::native(Provider::Codex);
    let account = state.accounts.iter().find(|account| account.provider == key.provider && account.id == key.account).unwrap();
    assert_eq!(account.state, AccountState::Disabled); assert_eq!(account.native_identity.as_deref(), Some("replacement-workspace"));
    assert_eq!(crate::accounts::generation_provider(&state, &key), generation + 1);
    assert!(state.model_catalogs.iter().all(|catalog| catalog.account != key));
    assert!(state.account_usage.iter().all(|usage| usage.provider != key.provider || usage.account != key.account));
    assert!(!state.interactions[&card].withdrawn); assert!(state.interactions[&card].card.cause.is_some());
    assert_eq!(state.lifecycle_of(&TASK.into()).unwrap().execution(), Some(&execution));
    assert_no_native_inference_from_account_probe(&sidecar);
    driver.stop().unwrap();
}

#[test]
fn an_existing_codex_probe_never_turns_new_api_key_billing_into_paid_authorization() {
    use alondra_core::accounts::{AccountBilling, AccountState};
    let (slice, mut driver, sidecar, execution, card) = disabled_codex_waiting_for_native_capacity();
    let before = slice.domain.state().unwrap();
    std::fs::write(&sidecar, serde_json::to_vec(&serde_json::json!({"trace":true,"accountRead":{"account":{"type":"apiKey"},"requiresOpenaiAuth":true}})).unwrap()).unwrap();
    assert!(crate::accounts::probe_current_codex(&slice.domain, &TASK.into(), Some(alondra_core::domain::ClaudeHold::Usage)).is_err());
    let state = slice.domain.state().unwrap(); let key = AccountKey::native(Provider::Codex);
    let account = state.accounts.iter().find(|account| account.provider == key.provider && account.id == key.account).unwrap();
    assert_eq!(account.state, AccountState::Disabled); assert_eq!(account.billing, AccountBilling::PayPerUse);
    assert_eq!(state.account_usage, before.account_usage, "a billing-mode change does not prove quota capacity or a new known identity");
    assert!(state.model_catalogs.iter().all(|catalog| catalog.account != key));
    assert!(!state.interactions[&card].withdrawn);
    assert_eq!(state.tasks[&TaskId::from(TASK)].selection.as_ref().unwrap().paid_account_grant, None);
    assert_eq!(state.lifecycle_of(&TASK.into()).unwrap().execution(), Some(&execution));
    assert_no_native_inference_from_account_probe(&sidecar);
    driver.stop().unwrap();
}

#[test]
fn same_cached_managed_session_still_reports_quota_and_authentication_after_same_identity_reauthentication(){
    use alondra_core::accounts::AccountState;
    for outcome in ["quota","auth"]{
        let slice=Slice::new();let trace=configured(&slice,"complete");let mut driver=launch(&slice);slice.settle(&mut driver);
        let original=driver.worker.host_group();let execution=Slice::lifecycle(&slice.domain).execution().unwrap().clone();let generation=driver.worker.binding().credential_generation();
        for (signed_in,id) in [(false,"managed-logout"),(true,"managed-reauth")]{
            let state=slice.domain.state().unwrap();let mut accounts=state.accounts.clone();let account=accounts.iter_mut().find(|account|account.provider==Provider::Codex && account.id.is_default()).unwrap();account.state=if signed_in{AccountState::SignedIn}else{AccountState::SignedOut};
            slice.run(&system(),id,Command::RecordAccounts{accounts,reserve:state.account_reserve});
        }
        assert!(crate::accounts::generation_provider(&slice.domain.state().unwrap(),&AccountKey::new(Provider::Codex,AccountId::default_account()))>generation);
        std::fs::write(slice._home.join("codex-native.account.json"),serde_json::to_vec(&serde_json::json!({"turnMode":outcome})).unwrap()).unwrap();
        carry_on(&slice.domain,"Continue through the same native session after same-account reauthentication");slice.settle(&mut driver);
        let state=slice.domain.state().unwrap();let account=state.accounts.iter().find(|account|account.provider==Provider::Codex && account.id.is_default()).unwrap();
        if outcome=="auth"{assert_eq!(account.state,AccountState::SignedOut,"a proven same-identity host can report a new native authentication failure");}
        else{assert!(state.account_usage.iter().any(|usage|usage.provider==Provider::Codex && usage.account.is_default() && usage.windows.iter().any(|window|window.reading.kind.is_native_limit_marker() && window.reading.exhausted)),"same-identity login-counter changes do not discard confirmed native quota");}
        assert_eq!(driver.worker.host_group(),original);assert_eq!(Slice::lifecycle(&slice.domain).execution(),Some(&execution));assert_eq!(transcript(&trace).iter().filter(|request|request["method"]=="initialize").count(),1);assert_eq!(transcript(&trace).iter().filter(|request|request["method"]=="thread/start").count(),1);driver.stop().unwrap();
    }
}

#[test]
fn initial_native_policy_rejection_keeps_original_mission_for_explicit_same_account_retry(){
    let slice=Slice::new();let trace=configured(&slice,"complete");let account=slice._home.join("codex-native.account.json");
    std::fs::write(&account,serde_json::to_vec(&serde_json::json!({"accountRead":{"account":{"type":"apiKey"},"requiresOpenaiAuth":true},"authStatus":{"authMethod":"apikey","authToken":null,"requiresOpenaiAuth":true}})).unwrap()).unwrap();
    let mut driver=launch(&slice);let original=Slice::lifecycle(&slice.domain).pending_inputs()[0].as_str().to_owned();let execution=Slice::lifecycle(&slice.domain).execution().unwrap().clone();
    let failure=driver.observe().unwrap_err();assert!(failure.to_string().contains("original paid authorization"),"{failure}");
    assert_eq!(Slice::lifecycle(&slice.domain).resource(),Some(alondra_domain::ResourceEvidence::Reserved));assert_eq!(Slice::lifecycle(&slice.domain).pending_inputs()[0].as_str(),original);assert!(Slice::lifecycle(&slice.domain).turn().is_none());
    let db=slice.observer();let status:String=db.query_row("SELECT status FROM lifecycle_dispatches WHERE task_id=?1 AND input_id=?2 ORDER BY id DESC LIMIT 1",rusqlite::params![TASK,original],|row|row.get(0)).unwrap();assert_eq!(status,"rejected","the positive rejected-before-offer receipt prevents a doubtful handoff");
    let checkpoint:Vec<u8>=db.query_row("SELECT bytes FROM provider_normalization_checkpoints WHERE task_id=?1",[TASK],|row|row.get(0)).unwrap();assert!(alondra_provider_codex::blocked_policy(&checkpoint).is_some());drop(db);driver.stop().unwrap();drop(driver);
    let automatic=adopt(Arc::clone(&slice.domain),&slice.backend,TASK.into(),Some(&slice.plugin)).err().expect("automatic restart cannot bypass persisted native policy");assert!(automatic.to_string().contains("original paid authorization"));assert_eq!(transcript(&trace).iter().filter(|request|request["method"]=="initialize").count(),1);assert!(!transcript(&trace).iter().any(|request|request["method"]=="turn/start"));
    std::fs::write(&account,b"{}").unwrap();
    let mut resumed=take_host(Arc::clone(&slice.domain),&slice.backend,TASK.into(),Some(&slice.plugin),true,true,true).unwrap().unwrap();
    let retry_input=Slice::lifecycle(&slice.domain).pending_inputs()[0].as_str().to_owned();
    assert!(slice.domain.state().unwrap().lifecycle_journal[&TaskId::from(TASK)].iter().any(|record|matches!(record,LifecycleRecord::HandoffResent{input,resent,..} if input==&original && resent==&retry_input)),"only positive rejected-before-offer evidence authorizes the original mission's successor");
    assert!(resumed.pending_permits()>0,"the authorized retry has a prepared permit before any native ready event");
    slice.settle(&mut resumed);
    let calls=transcript(&trace);let offered:Vec<_>=calls.iter().filter(|request|request["method"]=="turn/start").collect();assert!(!offered.is_empty());assert!(offered[0]["params"]["input"][0]["text"].as_str().unwrap().contains("Write the release notes through Codex"),"the first real native input remains the original mission");
    assert_eq!(offered.iter().filter(|request|request["params"]["input"][0]["text"].as_str().unwrap().contains("Write the release notes through Codex")).count(),1);assert_eq!(calls.iter().filter(|request|request["method"]=="initialize").count(),2);assert_eq!(calls.iter().filter(|request|request["method"]=="thread/start").count(),1);assert_eq!(Slice::lifecycle(&slice.domain).execution(),Some(&execution));assert!(Slice::lifecycle(&slice.domain).pending_inputs().is_empty());
    let db=slice.observer();let checkpoint:Vec<u8>=db.query_row("SELECT bytes FROM provider_normalization_checkpoints WHERE task_id=?1",[TASK],|row|row.get(0)).unwrap();assert!(alondra_provider_codex::blocked_policy(&checkpoint).is_none());drop(db);resumed.stop().unwrap();
}

#[cfg(target_os = "macos")]
#[path = "codex_process_tests.rs"]
mod process_adoption;
