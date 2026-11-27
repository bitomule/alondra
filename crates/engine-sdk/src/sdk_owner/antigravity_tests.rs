//! A node on Antigravity through the release's Antigravity host and the fake `agy` of the host's
//! own tests: launch, input, interrupt, force stop, adoption by a new daemon and resume after the
//! host ended.
use super::*;
use crate::sdk_owner::fixtures::antigravity::*;
use crate::sdk_owner::fixtures::antigravity::launch;
use alondra_core::antigravity_account::Access;

fn default_account() -> AccountId { AccountId::default_account() }

fn hold() -> serde_json::Value { serde_json::json!([[{"message": "Working on it"}, {"hold": {}}]]) }

// spec: antigravity.nodes/turn-through-host-and-agy
#[test]
fn an_antigravity_node_runs_its_turn_through_the_real_host_and_agy() {
    let slice = Slice::new();
    let record = configured(&slice, &default_account(), Access::Google, serde_json::json!([]));
    let mut driver = launch(&slice, &default_account());
    slice.settle(&mut driver);
    let state = slice.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from(TASK)].provider, Provider::Antigravity);
    assert_eq!(driver.worker.binding().driver(), "antigravity-acp");
    assert!(received(&record, "authenticate").is_empty(), "a Google account uses the sign-in agy keeps");
    let opened = received(&record, "session/new");
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0]["params"]["mcpServers"][0]["name"], "alondra", "the node reaches Alondra through its MCP server");
    assert!(opened[0]["params"]["_meta"]["antigravity"]["systemPrompt"]["append"].as_str().is_some_and(|text| text.contains("Alondra task node")), "the node's instructions go in the session");
    let prompts = received(&record, "session/prompt");
    assert_eq!(prompts[0]["params"]["prompt"][0]["text"], "Write the release notes through Antigravity");
    assert!(received(&record, "session/set_config_option").iter().any(|request| request["params"]["configId"] == "thought_level" && request["params"]["value"] == "high"), "the task's effort is the session's thinking level");
    let started = &recorded(&record, "starts.jsonl")[0];
    assert_eq!(started["values"]["AGY_CLI_DISABLE_AUTO_UPDATE"], "1");
    assert!(started["values"].get("GEMINI_HOME").is_none(), "the default account is agy's own");
    assert_eq!(started["argv"], serde_json::json!(["acp"]));
    let public: Vec<String> = slice.observer().prepare("SELECT content FROM task_context_items WHERE task_id=?1 AND source='assistant'").unwrap()
        .query_map([TASK], |row| row.get(0)).unwrap().map(Result::unwrap).collect();
    assert!(public.iter().any(|text| text.contains("Hello from Antigravity")), "what agy answered is the node's answer: {public:?}");
    driver.stop().unwrap();
}

// spec: antigravity.nodes/interrupt-cancels-the-prompt
#[test]
fn an_interrupt_cancels_the_prompt_and_ends_the_turn() {
    let slice = Slice::new();
    let record = configured(&slice, &default_account(), Access::Google, hold());
    let mut driver = launch(&slice, &default_account());
    slice.until_working(&mut driver);
    slice.run(&user(), "interrupt", Command::InterruptTurn { agent_id: driver.agent().clone() });
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    assert_eq!(received(&record, "session/cancel").len(), 1, "agy is asked to cancel the prompt it runs");
    assert_ne!(Slice::lifecycle(&*slice.domain).activity(), ActivityView::Working, "the cancelled prompt ended the turn");
    driver.stop().unwrap();
}

// spec: antigravity.nodes/force-stop-ends-host-and-agy
#[test]
fn a_force_stop_ends_the_host_and_agy_with_it() {
    let slice = Slice::new();
    configured(&slice, &default_account(), Access::Google, hold());
    let mut driver = launch(&slice, &default_account());
    slice.until_working(&mut driver);
    let group = driver.worker.host_group().unwrap();
    let processes = alondra_process_group::members(group).unwrap();
    assert!(processes.len() >= 2, "the host and its agy run in one group");
    driver.stop().unwrap();
    assert!(processes.iter().all(|process| !alondra_process_group::alive(process).unwrap()), "nothing of the node runs after it stopped");
}

// spec: antigravity.nodes/restarted-daemon-adopts-the-host
#[test]
fn a_new_daemon_adopts_the_running_host_without_a_second_agy_or_prompt() {
    let slice = Slice::new();
    let record = configured(&slice, &default_account(), Access::Google, hold());
    let mut driver = launch(&slice, &default_account());
    slice.until_working(&mut driver);
    let group = driver.worker.host_group().unwrap();
    drop(driver);
    let restarted = slice.restart();
    let mut adopted = adopt(restarted.clone(), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    assert_eq!(adopted.worker.stopper().and_then(|stopper| stopper.process_group()), Some(group), "the same host goes on");
    assert_eq!(received(&record, "initialize").len(), 1, "adoption starts no second agy");
    assert_eq!(received(&record, "session/prompt").len(), 1, "a handed input is never sent again");
    adopted.stop().unwrap();
}

// spec: antigravity.nodes/dead-agy-resumes-the-session
#[test]
fn a_turn_whose_agy_died_resumes_the_same_session_in_a_new_host() {
    let slice = Slice::new();
    let record = configured(&slice, &default_account(), Access::Google, serde_json::json!([[{"message": "Working on it"}, {"hold": {}}], [{"message": "Back where I was"}, {"end": "end_turn"}]]));
    let mut driver = launch(&slice, &default_account());
    until_working(&slice, &mut driver, &record);
    let agy = recorded(&record, "starts.jsonl")[0]["pid"].as_i64().unwrap() as i32;
    // SAFETY: the test signals only the fake agy its own node started, by the pid that agy recorded.
    assert_eq!(unsafe { libc::kill(agy, libc::SIGKILL) }, 0, "{}", std::io::Error::last_os_error());
    let error = loop {
        if let Err(error) = driver.observe() { break error; }
    };
    assert!(driver.host_to_replace(), "the host that lost its agy is replaced: {error}");
    let mut resumed = driver.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    drive_until(&slice, &mut resumed, &record, |slice, _| super::codex_tests::turns_started(&slice.domain) >= 2);
    assert_eq!(received(&record, "session/resume")[0]["params"]["sessionId"], "fixture-acp-session-1", "the new host resumes the session the first one opened");
    assert_eq!(received(&record, "session/new").len(), 1, "no second session opens");
    resumed.stop().unwrap();
}

/// An API key reaches only the agy of its account, from its file, through its environment: no
/// file Alondra writes holds it.
// spec: antigravity.nodes/key-reaches-only-its-agy
#[test]
fn an_api_key_reaches_only_its_accounts_agy_and_no_file_alondra_writes() {
    let slice = Slice::new();
    let studio = AccountId::parse("studio").unwrap();
    let record = configured(&slice, &studio, Access::ApiKey, serde_json::json!([]));
    let mut driver = launch(&slice, &studio);
    drive_until(&slice, &mut driver, &record, |_, driver| driver.pending_permits() == 0 && !driver.waits_for_host());
    let folder = alondra_core::accounts::provider_folder(&slice._home, &AccountKey::new(Provider::Antigravity, studio.clone())).unwrap();
    assert_eq!(received(&record, "authenticate")[0]["params"]["methodId"], "gemini-api-key");
    let started = &recorded(&record, "starts.jsonl")[0];
    assert_eq!(started["geminiKeySha256"], key_digest(KEY), "agy gets the key of its account's file in its environment");
    assert!(started["googleKeySha256"].is_null(), "and no Agent Platform key");
    assert_eq!(started["values"]["GEMINI_HOME"], folder.display().to_string());
    assert_eq!(started["argv"], serde_json::json!(["acp", "--gemini-dir", folder.display().to_string()]));
    driver.stop().unwrap();
    let key_file = folder.join(alondra_core::antigravity_account::KEY_FILE);
    let mut holders = Vec::new();
    let mut folders = vec![slice._home.to_path_buf()];
    while let Some(next) = folders.pop() {
        for entry in std::fs::read_dir(&next).unwrap().filter_map(Result::ok) {
            let (path, kind) = (entry.path(), entry.file_type().unwrap());
            if kind.is_dir() { folders.push(path); }
            else if kind.is_file() && path != key_file && std::fs::read(&path).is_ok_and(|bytes| bytes.windows(KEY.len()).any(|window| window == KEY.as_bytes())) { holders.push(path); }
        }
    }
    assert_eq!(holders, Vec::<PathBuf>::new(), "the key is in no journal, lock, log, state or record of Alondra's");
}

fn key_digest(key: &str) -> serde_json::Value {
    use sha2::Digest as _;
    serde_json::Value::String(sha2::Sha256::digest(key.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect())
}

/// The user removes the key while a node of the account works: its file goes, and the host that
/// takes the node over after its agy ended starts no agy without a key, nor with the one removed.
// spec: antigravity.nodes/removed-key-starts-no-later-agy
#[test]
fn a_key_removed_while_a_node_runs_reaches_no_later_agy() {
    let slice = Slice::new();
    let studio = AccountId::parse("studio").unwrap();
    let account = AccountKey::new(Provider::Antigravity, studio.clone());
    let record = configured(&slice, &studio, Access::ApiKey, hold());
    let mut driver = launch(&slice, &studio);
    until_working(&slice, &mut driver, &record);
    let file = alondra_core::accounts::provider_folder(&slice._home, &account).unwrap().join(alondra_core::antigravity_account::KEY_FILE);
    std::fs::remove_file(&file).unwrap();
    slice.run(&user(), "remove-key", Command::RecordAccountCredential { account: account.clone(), set: false });
    let agy = recorded(&record, "starts.jsonl")[0]["pid"].as_i64().unwrap() as i32;
    // SAFETY: the test signals only the fake agy its own node started, by the pid that agy recorded.
    assert_eq!(unsafe { libc::kill(agy, libc::SIGKILL) }, 0, "{}", std::io::Error::last_os_error());
    let error = loop {
        if let Err(error) = driver.observe() { break error; }
    };
    assert!(driver.host_to_replace(), "the host that lost its agy is replaced: {error}");
    if let Ok(mut replaced) = driver.replace_ended_host(&slice.backend, Some(&slice.plugin)) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while replaced.observe().is_ok() && std::time::Instant::now() < deadline && !replaced.waits_for_host() {}
        let _ = replaced.stop();
    }
    let starts = recorded(&record, "starts.jsonl");
    assert_eq!(starts.len(), 1, "no agy started after the key went: {starts:?}");
    assert!(!file.exists());
}

/// A run on a real Antigravity CLI and a Gemini API key, by hand: `ALONDRA_E2E_AGY` names the `agy`
/// and `ALONDRA_E2E_GEMINI_KEY_FILE` a `0600` file that holds the key, so the key itself is never a
/// variable of the build. A prompt is answered, a second one is interrupted, and a host that died
/// resumes the session and answers a third. The release runs the tests above, on the fake agy.
#[test]
#[ignore = "opt-in: a real Antigravity CLI and a Gemini API key, named by ALONDRA_E2E_AGY and ALONDRA_E2E_GEMINI_KEY_FILE"]
fn an_actual_agy_runs_a_node_on_an_api_key_and_resumes_its_session() {
    let agy = std::env::var("ALONDRA_E2E_AGY").map(PathBuf::from).expect("the release names its Antigravity CLI in ALONDRA_E2E_AGY");
    let key = std::fs::read_to_string(std::env::var("ALONDRA_E2E_GEMINI_KEY_FILE").expect("the release names the file of its Gemini API key in ALONDRA_E2E_GEMINI_KEY_FILE")).unwrap();
    let slice = Slice::new();
    let studio = AccountId::parse("studio").unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    configured_with(&slice, &studio, Access::ApiKey, key.trim(), agy, BTreeMap::from([("PATH".to_owned(), path)]));
    let mut driver = launch(&slice, &studio);
    slice.settle(&mut driver);
    let answered = |slice: &Slice| slice.observer().query_row("SELECT count(*) FROM task_context_items WHERE task_id=?1 AND source='assistant'", [TASK], |row| row.get::<_, i64>(0)).unwrap();
    assert!(answered(&slice) >= 1, "agy answered the task's first input");
    slice.run(&user(), "e2e-long-input", Command::TellNode { task_id: TASK.into(), text: "Count slowly from one to two hundred, one number per line.".into(), from_user: true, answers: None });
    slice.until_working(&mut driver);
    slice.run(&user(), "e2e-interrupt", Command::InterruptTurn { agent_id: driver.agent().clone() });
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    assert_ne!(Slice::lifecycle(&*slice.domain).activity(), ActivityView::Working, "the interrupt ended the turn");
    slice.kill_host(&driver);
    while driver.observe().is_ok() {}
    assert!(driver.host_to_replace());
    let mut resumed = driver.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    let before = answered(&slice);
    slice.run(&user(), "e2e-after-resume", Command::TellNode { task_id: TASK.into(), text: "Say in one word that you are back.".into(), from_user: true, answers: None });
    slice.settle(&mut resumed);
    assert!(answered(&slice) > before, "the resumed session answered");
    resumed.stop().unwrap();
}
