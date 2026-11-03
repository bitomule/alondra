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
    let mut driver = start(Arc::clone(&slice.domain), &slice.backend, Launch {
        task: TASK.into(), name: "Codex release notes".into(), directory: slice.worktree.display().to_string(),
        prompt: "Write the release notes through Codex".into(), plugin: slice.plugin.clone(),
        options,
        model: None, account: AccountId::default_account(),
    }).unwrap();
    driver.plugin = Some(slice.plugin.clone());
    driver
}
fn transcript(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

#[test]
fn reopening_codex_with_a_retained_mapping_but_a_missing_rollout_starts_a_clean_thread() {
    let slice = Slice::new();
    let trace = configured(&slice, "complete");
    let mut original = launch(&slice);
    slice.settle(&mut original);
    let old_node = original.agent().clone();
    let native = slice.domain.state().unwrap().lifecycle_journal[&TaskId::from(TASK)].iter().rev().find_map(|record| match record {
        LifecycleRecord::Admitted { execution: Some(execution), .. } => Some(execution.clone()), _ => None,
    }).unwrap();
    slice.run(&user(), "cancel-codex-rollout", Command::CancelTask { task_id: TASK.into() });
    original.stop().unwrap();
    drop(original);
    slice.run(&system(), "close-codex-rollout", Command::RecordNodeEnd {
        task_id: TASK.into(), end: NodeEnd::Closed, agent_id: Some(old_node.clone()),
    });
    slice.run(&user(), "reopen-codex-rollout", Command::ReopenTask { task_id: TASK.into() });
    let input = slice.domain.state().unwrap().pending_reopening(&TASK.into()).unwrap().to_owned();
    let positive = prepare_reopening(&slice.domain, &slice.backend, &TASK.into(), Provider::Codex,
        &AccountId::default_account(), &slice.worktree, input.clone(), true).unwrap();
    assert!(positive.recovery.is_some(), "the managed identity, mapping, directory and native rollout originally prove recovery");
    let metadata = slice._home.join(".codex/sessions/fake-thread-00000000-0000-4000-8000-000000000001.json");
    let retained: serde_json::Value = serde_json::from_slice(&std::fs::read(metadata).unwrap()).unwrap();
    std::fs::remove_file(retained["path"].as_str().unwrap()).unwrap();
    std::fs::write(slice._home.join("codex-native.account.json"), serde_json::to_vec(&serde_json::json!({
        "missingRollout": true, "freshThreadId": "00000000-0000-4000-8000-000000000002",
    })).unwrap()).unwrap();
    let reopening = prepare_reopening(&slice.domain, &slice.backend, &TASK.into(), Provider::Codex,
        &AccountId::default_account(), &slice.worktree, input, true).unwrap();
    assert!(reopening.recovery.is_none(), "a mapping alone cannot prove the missing native thread");
    let mission = "Publish the release notes with the original mission after the native rollout disappeared.";
    let options = LaunchOptions { provider: Provider::Codex, model: Some("fixture-model".into()), effort: Some("high".into()), selection: Some(selection()), ..LaunchOptions::default() };
    let mut replacement = start_bound(Arc::clone(&slice.domain), &slice.backend, Launch {
        task: TASK.into(), name: "Reopened Codex release notes".into(), directory: slice.worktree.display().to_string(),
        prompt: mission.into(), plugin: slice.plugin.clone(), options, model: None, account: AccountId::default_account(),
    }, provider_capabilities(Provider::Codex), None, Some(reopening)).unwrap();
    slice.settle(&mut replacement);
    let state = slice.domain.state().unwrap();
    assert_ne!(replacement.agent(), &old_node);
    assert_ne!(state.lifecycle_of(&TASK.into()).unwrap().execution().unwrap().conversation().as_str(), native.conversation);
    assert_eq!(state.lifecycle_of(&TASK.into()).unwrap().execution().unwrap().generation(), 2);
    let requests = transcript(&trace);
    assert_eq!(requests.iter().filter(|request| request["method"] == "thread/start").count(), 2, "the original thread and exactly one clean replacement");
    assert!(!requests.iter().any(|request| request["method"] == "thread/resume"), "the missing rollout is never offered to a failing resume");
    assert_eq!(requests.iter().filter(|request| request["method"] == "turn/start" && request["params"]["input"][0]["text"] == mission).count(), 1);
    assert!(requests.iter().any(|request| request["method"] == "turn/start"
        && request["params"]["threadId"] == "00000000-0000-4000-8000-000000000002"), "the replacement starts a different native thread");
    assert!(slice.domain.store.lock().unwrap().start_retries().unwrap().is_empty());
    replacement.stop().unwrap();
}

#[tokio::test]
async fn a_reopened_task_with_unproven_claude_history_starts_codex_from_the_queue_without_retrying() {
    use alondra_core::accounts::AccountsConfig;

    let slice = Slice::new();
    let trace = configured(&slice, "complete");
    let mut accounts = AccountsConfig::fresh();
    accounts.connect(Provider::Claude);
    accounts.connect(Provider::Codex);
    std::fs::write(alondra_core::options::path_in(&slice._home), accounts.to_toml()).unwrap();
    slice.run(&system(), "reopen-account-order", Command::RecordAccountConfiguration {
        order: slice.domain.state().unwrap().accounts.iter().map(|account| AccountKey::new(account.provider, account.id.clone())).collect(),
        version: alondra_core::accounts::config_version(&accounts),
    });

    let approved = "# Publish the release notes\n\n- [ ] Preserve the approved release scope\n- [ ] Verify the published text\n";
    let targets = vec![DeliveryTarget::Text { text: "The release notes proposal".into() }];
    slice.run(&system(), "reporter", Command::RegisterAgent { agent_id: "reporter".into(), name: "Release notes report".into(), at: 0 });
    slice.run(&user(), "assign-reporter", Command::AssignTask { task_id: TASK.into(), agent_id: "reporter".into(), model: None });
    slice.run(&Actor::Agent { agent_id: "reporter".into() }, "report-plan", Command::PresentResult {
        checklist: vec![], attachments: vec![], task_id: TASK.into(), interaction_id: "report".into(),
        body: "The release notes can be published with the proposed checks.".into(), targets: targets.clone(),
        is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the release notes proposal", &targets, true).plan(approved),
    });
    slice.run(&user(), "approve-report-plan", Command::DecideProposedPlan {
        presentation_id: "report".into(), targets, choice: PlanChoice::Implement,
    });
    slice.run(&system(), "close-reporter", Command::RecordNodeEnd {
        task_id: TASK.into(), end: NodeEnd::Closed, agent_id: Some("reporter".into()),
    });
    let task = TaskId::from("reopened-build");
    let mission = "Publish the release notes. Keep the complete mission after reopening.";
    let plan = LaunchPlan {
        mission: Some(mission.into()), model: Some("claude/sonnet".into()), reason: Some("Prepare the approved release notes".into()),
        ..LaunchPlan::new("release-notes", slice.worktree.to_str().unwrap())
    };
    slice.run(&Actor::Orchestrator { generation: 0 }, "create-implementation", Command::CreateTask {
        plan_items: None, continues: None, task_id: task.clone(), request_id: "implement-report".into(), title: "Publish the release notes".into(),
        agent_id: None, review_prs: false, dependencies: BTreeSet::new(), stack: None, launch: Some(plan),
    });
    let checklist = slice.domain.state().unwrap().checklists[&task].clone();
    let mut original = start(Arc::clone(&slice.domain), &slice.backend, Launch {
        task: task.clone(), name: "Release notes".into(), directory: slice.worktree.display().to_string(),
        prompt: mission.into(), plugin: slice.plugin.clone(), options: LaunchOptions::default(), model: None,
        account: AccountId::default_account(),
    }).unwrap();
    let old_node = original.agent().clone();
    slice.domain.record_model_decision(&crate::domain_store::model_metrics::Decision {
        task_id: task.0.clone(), request_id: "implement-report".into(), agent_id: Some(old_node.0.clone()), at: alondra_core::now_millis(),
        kind: crate::domain_store::model_metrics::DecisionKind::Launch, model: Some("sonnet".into()), effort: None, approach: None,
        chosen_by: ModelChooser::Orchestrator, reason: Some("Original Claude launch".into()), title: "Publish the release notes".into(),
        provider: Some(Provider::Claude), tags: None,
    }).unwrap();
    assert!(slice.domain.store.lock().unwrap().task_was_launched(&task).unwrap());
    let refusal = require_recoverable_session(&slice.domain, &slice.backend, &task, Provider::Claude,
        &AccountId::default_account(), slice.worktree.to_str().unwrap()).unwrap_err();
    assert!(refusal.to_string().contains("The original native session cannot be proven"), "{refusal}");
    slice.run(&user(), "cancel-implementation", Command::CancelTask { task_id: task.clone() });
    original.stop().unwrap();
    drop(original);
    slice.run(&system(), "close-original", Command::RecordNodeEnd {
        task_id: task.clone(), end: NodeEnd::Closed, agent_id: Some(old_node.clone()),
    });
    let terminal = slice.domain.state().unwrap();
    assert_eq!(terminal.tasks[&task].state, alondra_core::domain::TaskState::Cancelled);
    crate::domain_orchestrator_tools::update_launch_plan(&slice.domain, &slice._home, serde_json::from_value(serde_json::json!({
        "task_id": task, "changes": {"model": "codex/fixture-model", "reasoning_effort": "high"},
    })).unwrap()).unwrap();
    assert_eq!(slice.domain.launch_plan(&task).unwrap().unwrap().mission.as_deref(), Some(mission));
    assert_eq!(*slice.domain.state().unwrap(), *terminal, "preparing the provider must not reopen or rewrite the task");
    assert_eq!(slice.domain.state().unwrap().checklists[&task], checklist);

    slice.domain.enable_sdk(slice.backend.clone());
    slice.domain.live.gate.ignore_mac();
    let before_starts = slice.starts();
    crate::admission::pass_at(&slice.domain, &slice._home, alondra_core::now_millis()).await.unwrap();
    crate::auto_launch::pass(&slice.domain, &slice._home).await.unwrap();
    assert_eq!(slice.starts(), before_starts, "admission between updating and reopening starts no node");
    assert!(!trace.exists(), "Codex has not started while the task is terminal");
    {
        let store = slice.domain.store.lock().unwrap();
        assert!(store.launch_queue().unwrap().is_empty());
        assert!(store.start_retries().unwrap().is_empty());
    }
    slice.run(&Actor::Orchestrator { generation: 0 }, "reopen-implementation", Command::ReopenTask { task_id: task.clone() });
    assert_eq!(slice.domain.store.lock().unwrap().launch_queue().unwrap()[0].body["body"]["model"], "codex/fixture-model");
    let mut changed = slice.domain.subscribe();
    crate::admission::pass_at(&slice.domain, &slice._home, alondra_core::now_millis()).await.unwrap();
    let new_node = slice.domain.state().unwrap().tasks[&task].agent_id.clone()
        .expect("the first queue pass starts the reopened task instead of recording a retry");
    assert_ne!(new_node, old_node);
    loop {
        let state = slice.domain.state().unwrap();
        let lifecycle = state.lifecycle_of(&task).unwrap();
        if lifecycle.execution().is_some_and(|execution| execution.node() == &new_node)
            && lifecycle.activity() == ActivityView::Resting && lifecycle.pending_inputs().is_empty()
            && state.lifecycle_journal[&task].iter().filter(|record| matches!(record, LifecycleRecord::TurnEnded { .. })).count() == 2 { break; }
        changed.changed().await.unwrap();
    }
    let state = slice.domain.state().unwrap();
    assert_eq!(state.tasks[&task].provider, Provider::Codex);
    assert_eq!(state.checklists[&task], checklist);
    assert_eq!(state.lifecycle_of(&task).unwrap().execution().unwrap().generation(), 2);
    assert_eq!(state.agents.keys().filter(|agent| agent.0.starts_with("sdk-")).count(), 2);
    assert!(!state.interactions.keys().any(|card| card.0.starts_with("start-problem-")));
    {
        let store = slice.domain.store.lock().unwrap();
        assert!(store.start_retries().unwrap().is_empty(), "a permanent native-session refusal never becomes a retry");
        assert!(!store.launch_queue().unwrap().iter().any(|row| row.task_id == task));
    }
    let native = transcript(&trace);
    assert_eq!(native.iter().filter(|message| message["method"] == "thread/start").count(), 1);
    assert!(!native.iter().any(|message| message["method"] == "thread/resume"), "Codex cannot resume Claude's native history");
    let launch = native.iter().find(|message| message["method"] == "turn/start").unwrap();
    let prompt = launch["params"]["input"][0]["text"].as_str().unwrap();
    assert!(prompt.contains(mission), "{prompt}");
    assert!(prompt.contains(approved.trim()) && prompt.contains("do not plan again"), "{prompt}");
    assert_eq!(prompt.matches("Preserve the approved release scope").count(), 1, "the approved plan is carried once: {prompt}");

    crate::admission::pass_at(&slice.domain, &slice._home, alondra_core::now_millis()).await.unwrap();
    crate::auto_launch::pass(&slice.domain, &slice._home).await.unwrap();
    assert_eq!(slice.domain.state().unwrap().tasks[&task].agent_id.as_ref(), Some(&new_node));
    let after = transcript(&trace);
    for method in ["thread/start", "thread/resume", "turn/start"] {
        assert_eq!(after.iter().filter(|message| message["method"] == method).count(), native.iter().filter(|message| message["method"] == method).count(),
            "another admission pass cannot repeat {method}");
    }
    assert!(slice.domain.store.lock().unwrap().start_retries().unwrap().is_empty());
    slice.domain.stop_sdk_owners();
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
    slice.run(&system(),"queued-during-model-change",Command::TellNode{task_id:TASK.into(),text:"Use the configured model for this next input".into(), from_user: false});let queued=Slice::lifecycle(&slice.domain).pending_inputs()[0].as_str().to_owned();
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
fn replacing_a_pending_native_plan_keeps_the_owner_and_its_approved_decision_across_adoption() {
    let slice = Slice::new();
    let trace = configured(&slice, "replace_plan");
    let mut driver = launch(&slice);
    slice.until(&mut driver, |slice| slice.observations_with("turn/completed") == 1);
    let first = Slice::open_decision_cards(&slice.domain).remove(0);
    let execution = Slice::lifecycle(&slice.domain).execution().cloned().unwrap();
    tell(&slice, &driver, "Revise the plan before I approve it");
    slice.until(&mut driver, |slice| slice.observations_with("turn/completed") == 2);
    let open = Slice::open_decision_cards(&slice.domain);
    assert_eq!(open.len(), 1, "only the revised plan waits for approval");
    let revised = &open[0];
    assert_ne!(revised, &first);
    let state = slice.domain.state().unwrap();
    assert!(state.interactions[&InteractionId::from(first.as_str())].withdrawn);
    assert_eq!(Slice::lifecycle(&slice.domain).execution(), Some(&execution));
    assert!(open_card(&slice.domain, HOST_LOST_PREFIX).is_none());
    slice.answer(revised, Hold::APPROVE, None).unwrap();
    drop(driver);
    let restarted = Arc::new(DomainRuntime::open_at(&slice._home, &slice.database).unwrap());
    let mut adopted = adopt(Arc::clone(&restarted), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    slice.rest(&mut adopted);
    let state = restarted.state().unwrap();
    let lifecycle = Slice::lifecycle(&restarted);
    assert_eq!(lifecycle.execution(), Some(&execution));
    assert!(Slice::open_decision_cards(&restarted).is_empty());
    assert!(matches!(state.interactions[&InteractionId::from(revised.as_str())].payload, InteractionPayload::Question { answer: Some(_), .. }));
    assert_eq!(Slice::decision_cards(&restarted).len(), 2, "adoption never presents another plan");
    assert!(open_card(&restarted, HOST_LOST_PREFIX).is_none());
    assert_eq!(transcript(&trace).iter().filter(|message| message["method"] == "initialize").count(), 1, "the native process stayed alive");
    let turns: Vec<_> = transcript(&trace).into_iter().filter(|message| message["method"] == "turn/start").collect();
    assert_eq!(turns.len(), 4, "two plans, one implementation and the canonical empty-turn wake");
    assert!(turns.iter().all(|message| !message["params"]["input"][0]["text"].as_str().is_some_and(|text| text.starts_with("Revise the plan using this user feedback:"))), "supersession starts no revision turn");
    let wake = state.lifecycle_journal[&TaskId::from(TASK)].iter().find_map(|record| match record {
        LifecycleRecord::TurnEnded { wake: Some(wake), text, .. } => Some((wake, text)), _ => None,
    }).unwrap();
    assert_eq!(turns[3]["params"]["input"][0]["text"].as_str(), wake.1.as_deref());
    assert!(state.lifecycle_journal[&TaskId::from(TASK)].iter().any(|record| matches!(record,
        LifecycleRecord::TurnStarted { input, .. } if input == wake.0)));
    assert_eq!(transcript(&trace).iter().filter(|message| message["method"] == "turn/start" && message["params"]["input"][0]["text"].as_str().is_some_and(|text| text.starts_with("Implement the plan the user just approved."))).count(), 1);
    adopted.stop().unwrap();
}


#[test]
fn a_legacy_codex_host_finishes_native_work_and_an_approved_plan_before_renewing() {
    let slice = Slice::new();
    let trace = configured(&slice, "replace_plan_with_work");
    let backend_path = slice._home.join("codex-backend.json");
    let current: SdkBackend = serde_json::from_slice(&std::fs::read(&backend_path).unwrap()).unwrap();
    let mut legacy = current.clone();
    legacy.host = runfile("FIXTURE_CODEX_PLAN_LEGACY_HOST");
    legacy.node = legacy.host.clone();
    legacy.host_sha256 = backend::digest(&legacy.host).unwrap();
    legacy.node_sha256 = legacy.host_sha256.clone();
    std::fs::write(&backend_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let command_release = slice._home.join("legacy-work-release");
    let implementation_release = slice._home.join("legacy-implementation-release");
    for path in [&command_release, &implementation_release] {
        let path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }
    let pid_file = slice._home.join("legacy-work-pid");
    let sidecar = slice._home.join("codex-native.account.json");
    let fixture = serde_json::json!({
        "ownedCommand": {"release": command_release, "pidFile": pid_file},
        "planImplementationRelease": implementation_release,
    });
    std::fs::write(&sidecar, serde_json::to_vec(&fixture).unwrap()).unwrap();
    let mut original = launch(&slice);
    slice.until(&mut original, |slice| slice.observations_with("turn/completed") == 1);
    let group = original.worker.host_group().unwrap();
    let pid: serde_json::Value = serde_json::from_slice(&std::fs::read(&pid_file).unwrap()).unwrap();
    let child = alondra_process_group::members(group).unwrap().into_iter()
        .find(|member| member.pid == pid["pid"].as_i64().unwrap() as i32).unwrap();
    assert!(alondra_process_group::alive(&child).unwrap());
    assert!(!original.worker.supports_plan_supersession());
    drop(original);
    // The manifest release string is unchanged: support comes from hello, not a version guess.
    std::fs::write(&backend_path, serde_json::to_vec(&current).unwrap()).unwrap();
    let restarted = slice.restart();
    restarted.enable_sdk(slice.backend.clone());
    let mut adopted = slice.take_over(&restarted);
    let revise: crate::domain_messages::MessageBody = serde_json::from_value(serde_json::json!({"text": "Revise the plan before I approve it"})).unwrap();
    tokio::runtime::Runtime::new().unwrap()
        .block_on(crate::domain_messages::deliver(&restarted, &slice._home, &user(), "David", adopted.agent(), revise, true))
        .unwrap();
    while slice.observations_with("turn/completed") < 2 {
        adopted.step(&|| false).unwrap();
    }
    let cards = Slice::open_decision_cards(&restarted);
    assert_eq!(cards.len(), 1);
    let newest = InteractionId::from(cards[0].as_str());
    let status_reads = || slice.control_rows().iter().filter(|(kind, _, status)| kind == "mcp_status" && status == "handed").count();
    let before_reads = status_reads();
    let state = restarted.state().unwrap();
    restarted.execute_as_system(envelope(&state, "legacy-status-behind-supersession", Command::ReadMcpStatus { task_id: TASK.into() })).unwrap();
    while status_reads() == before_reads { adopted.step(&|| false).unwrap(); }
    assert!(!adopted.moves_to_this_release);
    assert!(alondra_process_group::alive(&child).unwrap(), "the command survives adoption and the deferred response");
    assert!(transcript(&trace).iter().filter(|message| message["method"] == "turn/start").all(|message|
        !message["params"]["input"][0]["text"].as_str().unwrap().starts_with("Revise the plan using this user feedback:")));
    // Let the command finish naturally. Its owned continuation still prevents a host move.
    std::fs::write(&command_release, b"done\n").unwrap();
    while slice.observations_with("Work completed naturally") == 0 { adopted.step(&|| false).unwrap(); }
    let before_reads = status_reads();
    let state = restarted.state().unwrap();
    restarted.execute_as_system(envelope(&state, "legacy-status-while-plan-pending", Command::ReadMcpStatus { task_id: TASK.into() })).unwrap();
    while status_reads() == before_reads { adopted.step(&|| false).unwrap(); }
    assert!(!adopted.moves_to_this_release, "the current unanswered plan prevents a move even after native work ends");
    assert!(!alondra_process_group::alive(&child).unwrap());
    let state = restarted.state().unwrap();
    assert!(!state.interactions[&newest].withdrawn);
    assert!(matches!(state.interactions[&newest].payload, InteractionPayload::Question { answer: None, .. }));
    answer(&restarted, &newest, Hold::APPROVE);
    while slice.observations_with("alondra/planContinuationAccepted") == 0 { adopted.step(&|| false).unwrap(); }
    while Slice::lifecycle(&restarted).activity() != ActivityView::Working { adopted.step(&|| false).unwrap(); }
    let before_reads = status_reads();
    let state = restarted.state().unwrap();
    restarted.execute_as_system(envelope(&state, "legacy-status-during-implementation", Command::ReadMcpStatus { task_id: TASK.into() })).unwrap();
    while status_reads() == before_reads { adopted.step(&|| false).unwrap(); }
    assert!(!adopted.moves_to_this_release, "the implementation root turn still runs on the legacy host");
    let approved = restarted.state().unwrap();
    // Cold inference after the move produces no new plan; this test judges retained approvals.
    let mut completed = fixture;
    completed["turnMode"] = serde_json::json!("complete");
    std::fs::write(&sidecar, serde_json::to_vec(&completed).unwrap()).unwrap();
    std::fs::write(&implementation_release, b"done\n").unwrap();
    while !adopted.moves_to_this_release { adopted.step(&|| false).unwrap(); }
    assert!(Slice::lifecycle(&restarted).turn().is_none());
    assert!(open_card(&restarted, HOST_LOST_PREFIX).is_none());
    let controls = std::mem::take(&mut adopted.controls);
    let mut renewed = adopted.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    renewed.controls.extend(controls);
    slice.rest(&mut renewed);
    let state = restarted.state().unwrap();
    assert_eq!(state.interactions[&newest], approved.interactions[&newest]);
    assert_eq!(state.checklists, approved.checklists);
    assert_eq!(Slice::decision_cards(&restarted).len(), 2, "the answered plan is never presented again");
    assert!(open_card(&restarted, HOST_LOST_PREFIX).is_none());
    let next: crate::domain_messages::MessageBody = serde_json::from_value(serde_json::json!({"text": "Report what the implementation changed"})).unwrap();
    tokio::runtime::Runtime::new().unwrap()
        .block_on(crate::domain_messages::deliver(&restarted, &slice._home, &user(), "David", renewed.agent(), next, true))
        .unwrap();
    slice.rest(&mut renewed);
    assert_eq!(Slice::decision_cards(&restarted).len(), 2, "the renewed host presents no plan again");
    assert_eq!(transcript(&trace).iter().filter(|message| message["method"] == "initialize").count(), 2);
    let resumes: Vec<_> = transcript(&trace).into_iter().filter(|message| message["method"] == "thread/resume").collect();
    assert_eq!(resumes.len(), 1);
    assert_eq!(resumes[0]["params"]["threadId"], "00000000-0000-4000-8000-000000000001");
    assert!(transcript(&trace).iter().filter(|message| message["method"] == "turn/start").all(|message|
        !message["params"]["input"][0]["text"].as_str().unwrap().starts_with("Revise the plan using this user feedback:")));
    renewed.stop().unwrap();
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

// Codex ends a turn with a Code Mode script still waiting on a tool and advances the script only in
// a later turn: the node rests instead of owning that tool for good.
#[test]
fn a_turn_that_ends_while_its_script_waits_on_a_tool_leaves_the_node_resting(){
    let slice=Slice::new();configured(&slice,"paused_cell");let mut driver=launch(&slice);
    let ended=|slice:&Slice|slice.domain.state().unwrap().lifecycle_journal[&TaskId::from(TASK)].iter().any(|record|matches!(record,LifecycleRecord::TurnEnded{..}));
    while !ended(&slice) || driver.pending_permits()>0 {advance(&mut driver)}
    let state=slice.domain.state().unwrap();let journal=&state.lifecycle_journal[&TaskId::from(TASK)];
    let paused=journal.iter().find_map(|record|match record{LifecycleRecord::WorkStarted{work,..} if work.contains("exec-paused")=>Some(work.clone()),_=>None}).expect("the script's tool started as work");
    assert!(journal.iter().any(|record|matches!(record,LifecycleRecord::WorkEnded{work,..} if *work==paused)),"the tool still open at the end of its turn ended with it");
    assert_ne!(Slice::lifecycle(&slice.domain).activity(),alondra_domain::ActivityView::OwnWork);
    driver.stop().unwrap();
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
        instruct(&slice.domain,&format!("Continue the same cached session after reauthentication {round}"));slice.settle(&mut driver);
        assert_eq!(driver.worker.host_group(),original);assert_eq!(Slice::lifecycle(&slice.domain).execution(),Some(&original_execution));
    }
    let before=transcript(&trace).iter().filter(|request|request["method"]=="turn/start").count();
    publish("replacement@example.test",true,"native-replaced-identity");instruct(&slice.domain,"This input must remain queued for explicit user recovery");
    let error=driver.step_once(&||false,std::time::Instant::now()).unwrap_err();assert!(error.to_string().contains("different known identity"),"{error}");
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
        instruct(&slice.domain,"Continue through the same native session after same-account reauthentication");slice.settle(&mut driver);
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

fn turns_started(domain: &DomainRuntime) -> usize {
    domain.read().state().lifecycle_journal[&TaskId::from(TASK)].iter().filter(|record| matches!(record, alondra_core::domain::LifecycleRecord::TurnStarted { .. })).count()
}

/// A Codex node is mid-turn when the Mac goes down, host and daemon with it. `meanwhile` changes
/// what the daemon that comes back finds; that daemon takes the node over and its owner runs on its
/// own, as in production, until the cut turn goes on in a second one or the task gets a card.
async fn codex_node_after_a_restart(meanwhile: impl FnOnce(&Slice)) -> (Slice, Arc<DomainRuntime>, Owner, PathBuf) {
    let slice = Slice::new();
    let trace = configured(&slice, "hold");
    let mut driver = launch(&slice);
    while Slice::lifecycle(&slice.domain).activity() != ActivityView::Working { driver.observe().unwrap(); }
    kill_host_and_daemon(driver);
    meanwhile(&slice);
    let restarted = slice.restart();
    restarted.enable_sdk(slice.backend.clone());
    let mut changed = restarted.subscribe();
    let owner = Owner::spawn(slice.take_over(&restarted));
    let reader = Arc::clone(&restarted);
    changed.wait_for(|_| turns_started(&reader) == 2 || open_card(&reader, HOST_LOST_PREFIX).is_some()).await.unwrap();
    (slice, restarted, owner, trace)
}

fn resumed_threads(trace: &Path) -> Vec<serde_json::Value> {
    transcript(trace).into_iter().filter(|message| message["method"] == "thread/resume").collect()
}

fn native_process(driver: &mut Driver) -> alondra_process_group::Member {
    match driver.worker.native_liveness().unwrap() {
        Some(alondra_provider_codex::NativeLiveness::Alive(native)) => native,
        other => panic!("the fixture must have a live app-server: {other:?}"),
    }
}

#[test]
fn an_unopened_codex_replacement_remains_unopened_after_reconciling_a_control() {
    let slice = Slice::new();
    configured(&slice, "hold");
    let mut driver = launch(&slice);
    while Slice::lifecycle(&slice.domain).activity() != ActivityView::Working { driver.observe().unwrap(); }
    kill_host_and_daemon(driver);
    let mut replacement = adopt(Arc::clone(&slice.domain), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    assert_eq!(replacement.worker.native_liveness().unwrap(), Some(alondra_provider_codex::NativeLiveness::NotStarted));
    slice.run(&system(), "cold-mcp-status", Command::ReadMcpStatus { task_id: TASK.into() });
    assert!(replacement.claim_and_record_control_without_sending());
    drop(replacement);
    let mut adopted = adopt(Arc::clone(&slice.domain), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    assert_eq!(slice.control_rows()[0].2, "rejected", "the unopened host explicitly refused a control needing a session");
    assert_eq!(adopted.worker.native_liveness().unwrap(), Some(alondra_provider_codex::NativeLiveness::NotStarted), "a reconciled control cannot prove native startup");
    adopted.stop().unwrap();
}

#[test]
fn adopting_a_legacy_codex_host_with_a_dead_native_replaces_it_before_reconciling_a_control() {
    let slice = Slice::new();
    let trace = configured(&slice, "hold");
    let path = slice._home.join("codex-backend.json");
    let mut backend: SdkBackend = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let host = runfile("FIXTURE_CODEX_LEGACY_HOST");
    backend.node = host.clone(); backend.node_sha256 = backend::digest(&host).unwrap();
    backend.host = host; backend.host_sha256 = backend.node_sha256.clone();
    std::fs::write(&path, serde_json::to_vec(&backend).unwrap()).unwrap();
    let mut driver = launch(&slice);
    while Slice::lifecycle(&slice.domain).activity() != ActivityView::Working { driver.observe().unwrap(); }
    slice.run(&system(), "legacy-mcp-status", Command::ReadMcpStatus { task_id: TASK.into() });
    assert!(driver.claim_and_record_control_without_sending());
    let old_host = driver.worker.stopper().unwrap();
    kill_native(&native_process(&mut driver));
    assert!(old_host.process().unwrap().is_some(), "the legacy host stays alive after native exit");
    drop(driver);
    let mut adopted = adopt(Arc::clone(&slice.domain), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    while turns_started(&slice.domain) < 2 { adopted.step(&|| true).unwrap(); }
    assert!(old_host.process().unwrap().is_none(), "the old host ended before the replacement served");
    assert_eq!(resumed_threads(&trace).len(), 1);
    assert_eq!(slice.control_rows()[0].2, "rejected", "the unsubmitted old control is reconciled from its ended journal");
    adopted.stop().unwrap();
}

fn kill_native(native: &alondra_process_group::Member) {
    let mut exits = alondra_process_group::Exits::new().unwrap();
    exits.watch(native).unwrap();
    signal_codex_process(native.pid, libc::SIGKILL);
    exits.wait().unwrap();
}

// 2026-10-27, 0.3.9, t-corrupt-journal-detail-codex: the app-server of a working Codex node went
// away and its host said so with alondra/transportFailed. The adapter normalizes that notice to a
// lost transport, which the domain does not record, and the owner stopped on «provider observation
// refused: RequiresDomainCommand». The owner takes the notice as the end of the Codex before it
// commits anything, and a host that resumes the same thread takes its place.
#[test]
fn a_codex_transport_failure_ends_the_host_and_the_same_thread_goes_on_in_a_new_one() {
    let slice = Slice::new();
    let trace = configured(&slice, "hold");
    let mut driver = launch(&slice);
    while Slice::lifecycle(&slice.domain).activity() != ActivityView::Working { driver.observe().unwrap(); }
    kill_native(&native_process(&mut driver));
    let error = loop {
        if let Err(error) = driver.observe() { break error; }
    };
    assert!(driver.host_closed, "the transport failure ends the host instead of stopping its owner: {error}");
    assert!(driver.host_to_replace());
    let mut resumed = driver.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    while turns_started(&slice.domain) < 2 { resumed.step(&|| true).unwrap(); }
    assert_eq!(resumed_threads(&trace).len(), 1, "the same thread goes on");
    resumed.stop().unwrap();
}

#[test]
fn an_admitted_codex_replacement_without_a_committed_observation_is_not_mistaken_for_an_unopened_host() {
    let slice = Slice::new();
    let trace = configured(&slice, "hold");
    let path = slice._home.join("codex-backend.json");
    let mut backend: SdkBackend = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let host = runfile("FIXTURE_CODEX_LEGACY_HOST");
    backend.node = host.clone(); backend.node_sha256 = backend::digest(&host).unwrap();
    backend.host = host; backend.host_sha256 = backend.node_sha256.clone();
    std::fs::write(&path, serde_json::to_vec(&backend).unwrap()).unwrap();
    let mut driver = launch(&slice);
    while Slice::lifecycle(&slice.domain).activity() != ActivityView::Working { driver.observe().unwrap(); }
    kill_host_and_daemon(driver);
    let mut replacement = adopt(Arc::clone(&slice.domain), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    let bootstrap: serde_json::Value = serde_json::from_slice(&std::fs::read(replacement.worker.state_directory().join("host.lock")).unwrap()).unwrap();
    let base = bootstrap["journal_base"].as_u64().unwrap();
    let observer = slice.observer();
    let checkpoint = || u64::try_from(observer.query_row("SELECT cursor FROM provider_normalization_checkpoints WHERE task_id=?1", [TASK], |row| row.get::<_, i64>(0)).unwrap()).unwrap();
    assert_eq!(checkpoint(), base);
    replacement.step(&|| true).unwrap();
    assert!(!replacement.replacement, "the continuation was offered to the new native session");
    assert_eq!(checkpoint(), base, "no observation of this host has committed");
    assert_eq!(Slice::lifecycle(&slice.domain).resource(), Some(alondra_domain::ResourceEvidence::Available));
    slice.run(&system(), "unobserved-legacy-mcp-status", Command::ReadMcpStatus { task_id: TASK.into() });
    assert!(replacement.claim_and_record_control_without_sending());
    let old_host = replacement.worker.stopper().unwrap();
    kill_native(&native_process(&mut replacement));
    assert!(old_host.process().unwrap().is_some(), "the legacy host stays available for attachment");
    drop(replacement);
    let mut adopted = adopt(Arc::clone(&slice.domain), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    assert!(old_host.process().unwrap().is_none(), "native admission is detected before reconciling the dead control");
    assert_eq!(slice.control_rows()[0].2, "rejected", "the control never reached the old native process");
    assert_eq!(resumed_threads(&trace).len(), 1, "the replacement used the original native thread; the next host remains lazy");
    assert_eq!(adopted.worker.native_liveness().unwrap(), Some(alondra_provider_codex::NativeLiveness::NotStarted));
    adopted.stop().unwrap();
}

fn assert_codex_resumed_once(trace: &Path, original_input: &str) {
    let resumed = resumed_threads(trace);
    assert_eq!(resumed.len(), 1, "exactly one replacement app-server: {resumed:?}");
    assert_eq!(resumed[0]["params"]["threadId"], "00000000-0000-4000-8000-000000000001");
    let submitted = transcript(trace).into_iter().filter(|request| request["method"] == "turn/start" && request["params"]["clientUserMessageId"] == original_input).count();
    assert_eq!(submitted, 1, "the input the old app-server consumed is not submitted again");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_codex_app_server_killed_mid_turn_resumes_the_same_thread_without_a_second_writer() {
    let slice = Slice::new();
    let trace = configured(&slice, "hold");
    let mut driver = launch(&slice);
    while Slice::lifecycle(&slice.domain).activity() != ActivityView::Working { driver.observe().unwrap(); }
    let original = transcript(&trace).into_iter().find(|request| request["method"] == "turn/start").unwrap()["params"]["clientUserMessageId"].as_str().unwrap().to_owned();
    let group = driver.worker.host_group().unwrap();
    let old_host = driver.worker.stopper().unwrap();
    stop_codex_host(group);
    kill_native(&native_process(&mut driver));
    let detected = driver.step(&|| false);
    if detected.is_ok() { old_host.kill().unwrap(); }
    assert!(detected.is_err(), "native loss is detected mid-turn even if the host cannot report its child's exit");
    slice.domain.enable_sdk(slice.backend.clone());
    let mut changed = slice.domain.subscribe();
    let mut owner = Owner::spawn(driver);
    changed.wait_for(|_| turns_started(&slice.domain) == 2 || owner.ended()).await.unwrap();
    assert_eq!(turns_started(&slice.domain), 2, "the cut turn continued automatically");
    assert_codex_resumed_once(&trace, &original);
    assert!(alondra_process_group::members(group).unwrap().iter().all(|member| member.zombie), "the old writer and its group ended before the new turn");
    owner.stop().unwrap();
}

// Stop the host before killing its app-server: it cannot process the exit or close its socket.
// This reproduces a legacy host left alive without its native process, including after adoption.
async fn idle_codex_without_app_server_receives_its_later_message(adopted: bool) {
    let slice = Slice::new();
    let trace = configured(&slice, "complete");
    let mut driver = launch(&slice);
    slice.settle(&mut driver);
    if adopted {
        drop(driver);
        driver = adopt(Arc::clone(&slice.domain), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    }
    let native = native_process(&mut driver);
    let old_host = driver.worker.stopper().unwrap();
    let group = old_host.process_group().unwrap();
    stop_codex_host(group);
    kill_native(&native);
    slice.run(&system(), "message-after-native-loss", Command::TellNode { task_id: TASK.into(), text: "Deliver this after replacing the dead app-server".into(), from_user: true });
    let result = driver.step(&|| false);
    if result.is_ok() { old_host.kill().unwrap(); }
    assert!(result.is_err(), "a live host with a dead app-server must be lost before attempting delivery");
    assert!(driver.host_closed);
    slice.domain.enable_sdk(slice.backend.clone());
    let mut changed = slice.domain.subscribe();
    let mut owner = Owner::spawn(driver);
    changed.wait_for(|_| turns_started(&slice.domain) >= 3 || owner.ended()).await.unwrap();
    // The resumed input's acceptance commits after the native transcript is written.
    changed.wait_for(|_| Slice::lifecycle(&slice.domain).pending_inputs().is_empty() || owner.ended()).await.unwrap();
    assert!(!owner.ended());
    let messages: Vec<_> = transcript(&trace).into_iter().filter(|request| request["method"] == "turn/start" && request["params"]["input"][0]["text"].as_str().is_some_and(|text| text.contains("Deliver this after replacing"))).collect();
    assert_eq!(messages.len(), 1, "the later message is delivered exactly once");
    assert!(alondra_process_group::members(group).unwrap().iter().all(|member| member.zombie), "the stopped old host cannot still serve");
    owner.stop().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_live_codex_host_without_an_app_server_is_replaced_before_delivering_a_later_message() {
    idle_codex_without_app_server_receives_its_later_message(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_adopted_codex_host_without_an_app_server_is_replaced_before_delivering_a_later_message() {
    idle_codex_without_app_server_receives_its_later_message(true).await;
}

struct HostReaped {
    queue: std::os::fd::OwnedFd,
    pid: libc::pid_t,
}
impl HostReaped {
    fn watch(pid: libc::pid_t) -> Self {
        use std::os::fd::{AsRawFd, FromRawFd};
        // kqueue returns a fresh descriptor owned by this fixture until its child was reaped.
        let descriptor = unsafe { libc::kqueue() };
        assert!(descriptor >= 0);
        let queue = unsafe { std::os::fd::OwnedFd::from_raw_fd(descriptor) };
        let mut change: libc::kevent = unsafe { std::mem::zeroed() };
        change.ident = pid.try_into().unwrap();
        change.filter = libc::EVFILT_PROC;
        change.flags = libc::EV_ADD | libc::EV_ONESHOT;
        change.fflags = libc::NOTE_REAP;
        // Subscription precedes the signal while the fixture still owns the live child.
        assert_eq!(unsafe { libc::kevent(queue.as_raw_fd(), &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) }, 0);
        Self { queue, pid }
    }
    fn wait(self) {
        use std::os::fd::AsRawFd;
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        let timeout = libc::timespec { tv_sec: 10, tv_nsec: 0 };
        // NOTE_REAP proves the launching runtime finished wait(), before an owner can help it.
        assert_eq!(unsafe { libc::kevent(self.queue.as_raw_fd(), std::ptr::null(), 0, &mut event, 1, &timeout) }, 1, "the launching runtime did not reap its host independently");
        assert_eq!(event.flags & libc::EV_ERROR, 0);
        assert_ne!(event.fflags & libc::NOTE_REAP, 0);
        let mut status = 0;
        // WNOHANG cannot block; ECHILD proves the runtime already consumed this child's status.
        let result = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
        let error = std::io::Error::last_os_error();
        assert_eq!(result, -1, "the host's child status was still owned by the test process");
        assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
    }
}

async fn killed_codex_host_is_replaced(signal: libc::c_int, adopted: bool) {
    let slice = Slice::new();
    let trace = configured(&slice, "hold");
    let mut driver = launch(&slice);
    while Slice::lifecycle(&slice.domain).activity() != ActivityView::Working { driver.observe().unwrap(); }
    if adopted {
        drop(driver);
        driver = adopt(Arc::clone(&slice.domain), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    }
    let native = native_process(&mut driver);
    let host = driver.worker.stopper().unwrap().process().unwrap().unwrap();
    let reaped = HostReaped::watch(host.pid);
    let mut exits = alondra_process_group::Exits::new().unwrap(); exits.watch(&host).unwrap();
    signal_codex_process(host.pid, signal);
    exits.wait().unwrap();
    reaped.wait();
    slice.domain.enable_sdk(slice.backend.clone());
    let mut changed = slice.domain.subscribe();
    let mut owner = Owner::spawn(driver);
    changed.wait_for(|_| turns_started(&slice.domain) == 2 || owner.ended()).await.unwrap();
    assert_eq!(turns_started(&slice.domain), 2, "the killed host's turn resumed");
    assert_eq!(resumed_threads(&trace).len(), 1);
    assert!(!alondra_process_group::alive(&native).unwrap(), "the old native writer ended before replacement");
    owner.stop().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sigterm_killed_codex_host_is_reaped_and_its_session_relaunched() {
    killed_codex_host_is_replaced(libc::SIGTERM, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sigkill_killed_codex_host_is_reaped_and_its_session_relaunched() {
    killed_codex_host_is_replaced(libc::SIGKILL, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sigkill_killed_adopted_codex_host_is_detected_and_its_session_relaunched() {
    killed_codex_host_is_replaced(libc::SIGKILL, true).await;
}

#[tokio::test]
async fn a_paused_codex_session_loses_its_app_server_and_delivers_resume_and_message_to_its_replacement() {
    let slice = Slice::new();
    let trace = configured(&slice, "hold");
    let mut driver = launch(&slice);
    while Slice::lifecycle(&slice.domain).activity() != ActivityView::Working { driver.observe().unwrap(); }
    let native = native_process(&mut driver);
    slice.domain.enable_sdk(slice.backend.clone());
    slice.domain.driven_by_hand.lock().unwrap().insert(driver.agent().clone());
    slice.domain.live.gate.ignore_mac();
    let agent = driver.agent().0.clone();
    slice.domain.live.gate.pause_by_user(&slice.domain, &agent, alondra_core::now_millis()).unwrap();
    while !slice.domain.read().state().lifecycle_journal[&TaskId::from(TASK)].iter().any(|record| matches!(record, LifecycleRecord::TurnEnded { .. })) {
        driver.step(&|| true).unwrap();
    }
    std::fs::write(slice._home.join("codex-native.account.json"), r#"{"turnMode":"complete"}"#).unwrap();
    kill_native(&native);
    assert!(driver.step(&|| true).is_err());
    let mut replacement = driver.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    crate::domain_messages::tell_sdk_node(&slice.domain, &TASK.into(), crate::node_push::ALONDRA, "Keep this message through the paused host replacement", "tell-paused-codex", false).unwrap();
    replacement.step(&|| true).unwrap();
    assert!(replacement.paused(), "replacing a dead host does not release a user pause");
    assert_eq!(transcript(&trace).iter().filter(|request| request["method"] == "turn/start").count(), 1);
    assert!(slice.domain.live.gate.resume_now(&slice.domain, &agent, alondra_core::now_millis()));
    crate::admission::pass_at(&slice.domain, &slice._home, alondra_core::now_millis()).await.unwrap();
    while !Slice::lifecycle(&slice.domain).pending_inputs().is_empty() || turns_started(&slice.domain) < 3 {
        replacement.step(&|| true).unwrap();
    }
    let prompts: Vec<_> = transcript(&trace).into_iter().filter(|request| request["method"] == "turn/start").map(|request| request["params"]["input"][0]["text"].as_str().unwrap().to_owned()).collect();
    assert_eq!(prompts.iter().filter(|prompt| prompt.contains("Keep this message through")).count(), 1, "{prompts:?}");
    assert_eq!(prompts.iter().filter(|prompt| prompt.contains("resumed you")).count(), 1, "{prompts:?}");
    assert_eq!(resumed_threads(&trace).len(), 1);
    replacement.stop().unwrap();
}

// Production, 0.3.9: after the release every Codex node stood still for good. Each saved native
// thread was refused for having been started under the previous release's node instructions, its
// host never said why, and its owner never looked again.
#[tokio::test(flavor = "multi_thread")]
async fn a_codex_node_cut_by_a_restart_goes_on_in_its_thread_under_the_new_release_instructions() {
    let (_slice, restarted, mut owner, trace) = codex_node_after_a_restart(|slice| {
        std::fs::write(slice.plugin.join("skills/alondra/SKILL.md"), "The next release's node instructions.").unwrap();
    }).await;
    assert!(open_card(&restarted, HOST_LOST_PREFIX).is_none(), "no card: the node goes on by itself");
    assert_eq!(turns_started(&restarted), 2, "the cut turn goes on in a new one");
    let resumed = resumed_threads(&trace);
    assert_eq!(resumed.len(), 1, "{resumed:?}");
    assert_eq!(resumed[0]["params"]["threadId"], "00000000-0000-4000-8000-000000000001", "the same native thread");
    assert_eq!(resumed[0]["params"]["developerInstructions"], "The next release's node instructions.");
    let turns: Vec<_> = transcript(&trace).into_iter().filter(|message| message["method"] == "turn/start").collect();
    assert!(turns[1]["params"]["input"][0]["text"].as_str().unwrap().starts_with("Your session was cut off when its host stopped"), "{turns:?}");
    owner.stop().unwrap();
}

// A new host whose Codex fails to start once is replaced by another that resumes the same thread,
// and the node goes on with no card.
#[tokio::test(flavor = "multi_thread")]
async fn a_codex_host_that_could_not_start_its_session_is_relaunched_and_the_node_goes_on() {
    let (_slice, restarted, mut owner, trace) = codex_node_after_a_restart(|slice| {
        std::fs::write(slice._home.join("codex-native.account.json"), r#"{"failResumes": 1}"#).unwrap();
    }).await;
    assert!(open_card(&restarted, HOST_LOST_PREFIX).is_none(), "no card: the second host took the input");
    assert_eq!(turns_started(&restarted), 2, "the cut turn goes on in a new one");
    let resumed = resumed_threads(&trace);
    assert_eq!(resumed.len(), 2, "the first new host failed to resume, the next one resumed: {resumed:?}");
    assert!(resumed.iter().all(|message| message["params"]["threadId"] == "00000000-0000-4000-8000-000000000001"));
    owner.stop().unwrap();
}

// A Codex session that can never start again is relaunched a bounded number of times, then the
// task gets the card to try again: it never stands still with its input in doubt.
#[tokio::test(flavor = "multi_thread")]
async fn a_codex_session_that_never_starts_ends_in_a_card_to_try_again_after_bounded_relaunches() {
    let (slice, restarted, owner, trace) = codex_node_after_a_restart(|slice| {
        for saved in std::fs::read_dir(slice.backend.state_root.join("native-threads")).unwrap() {
            std::fs::remove_file(saved.unwrap().path()).unwrap();
        }
    }).await;
    let card = open_card(&restarted, HOST_LOST_PREFIX).expect("the task has a card to try again");
    assert_eq!(card.card.content.as_ref().map(|content| content.body.as_str()), Some("Its new host stopped before it could take the task's next message"));
    assert!(owner.ended(), "the owner owns nothing any more");
    let initialized = transcript(&trace).into_iter().filter(|message| message["method"] == "initialize").count();
    assert_eq!(initialized, 2 + RELAUNCH_WAITS.len(), "the first host, the one the restart put in its place and one per relaunch");
    assert_eq!(turns_started(&restarted), 1, "no turn ran after the restart");
    drop(slice);
}


#[test]
fn stopping_a_codex_host_after_native_death_drains_unread_acceptance_and_final_evidence() {
    use std::io::{Read as _, Write as _};
    let slice = Slice::new();
    configured(&slice, "complete_then_exit");
    let ready_path = slice._home.join("turn-response-ready");
    let release_path = slice._home.join("turn-response-release");
    for path in [&ready_path, &release_path] {
        assert!(std::process::Command::new("/usr/bin/mkfifo").args(["-m", "600"]).arg(path).status().unwrap().success());
    }
    let mut ready = std::fs::OpenOptions::new().read(true).write(true).open(&ready_path).unwrap();
    let mut release = std::fs::OpenOptions::new().read(true).write(true).open(&release_path).unwrap();
    std::fs::write(slice._home.join("codex-native.account.json"), serde_json::to_vec(&serde_json::json!({
        "turnResponseReady": ready_path, "turnResponseBarrier": release_path,
    })).unwrap()).unwrap();
    let mut driver = launch(&slice);
    // The native process has read turn/start, but has not written its response yet.
    use std::os::fd::AsRawFd as _;
    let mut descriptor = libc::pollfd { fd: ready.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    // SAFETY: the one pollfd and its open FIFO remain alive through this bounded readiness wait.
    assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 10000) }, 1, "the native fixture never received turn/start");
    ready.read_exact(&mut [0]).unwrap();
    let native = native_process(&mut driver);
    let host = driver.worker.stopper().unwrap().process().unwrap().unwrap();
    let directory = driver.worker.state_directory().to_path_buf();
    stop_codex_host(host.pid);
    let mut exited = alondra_process_group::Exits::new().unwrap();
    exited.watch(&native).unwrap();
    release.write_all(b"R").unwrap();
    exited.wait().unwrap();
    // The stopped host cannot have read the response or final item left in its pipe.
    driver.stop().unwrap();
    let journal = std::fs::read(directory.join("outbox")).unwrap();
    let mut frames = Vec::new();
    let mut bytes = journal.as_slice();
    while !bytes.is_empty() {
        let size = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
        frames.push(serde_json::from_slice::<serde_json::Value>(&bytes[4..4 + size]).unwrap());
        bytes = &bytes[4 + size..];
    }
    let observations: Vec<_> = frames.iter().filter_map(|frame| (frame["body"]["type"] == "event").then_some(&frame["body"]["observation"])).collect();
    let accepted = observations.iter().position(|seen| seen["payload"]["method"] == "alondra/inputAccepted").expect("the native acceptance already written before death must remain durable");
    let final_item = observations.iter().position(|seen| seen["payload"]["method"] == "item/completed" && seen["payload"]["params"]["item"]["text"] == "Codex fixture final answer").expect("the native final answer already written before death must remain durable");
    let terminal = observations.iter().position(|seen| seen["type"] == "process_exited").expect("the native exit is durable");
    assert!(accepted < final_item && final_item < terminal, "native evidence drains before the terminal observation");
    assert!(alondra_process_group::process(host.pid).unwrap().is_none(), "cleanup reaps the host before returning");
}


fn signal_codex_process(pid: libc::pid_t, signal: libc::c_int) {
    // SAFETY: these tests signal only a kernel-identified process they launched in their fixture.
    assert_eq!(unsafe { libc::kill(pid, signal) }, 0, "{}", std::io::Error::last_os_error());
}

fn stop_codex_host(pid: libc::pid_t) {
    signal_codex_process(pid, libc::SIGSTOP);
    let mut status = 0;
    // SAFETY: status is writable, and pid is this test process's child; WUNTRACED consumes no exit.
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED) }, pid);
    assert!(libc::WIFSTOPPED(status));
}
