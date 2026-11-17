//! The tests of Codex nodes that need the gate itself: starts from its queue and pauses it holds.
use super::*;
use alondra_daemon_engine::sdk_owner::tests::codex_tests::launch;
use alondra_daemon_engine::sdk_owner::*;
use alondra_daemon_engine::sdk_owner::tests::*;
use alondra_daemon_engine::sdk_owner::tests::codex_tests::*;

#[tokio::test]
async fn a_task_created_with_a_codex_launch_plan_starts_on_codex() {
    wired();
    use alondra_core::accounts::AccountsConfig;
    let slice = Slice::new();
    let trace = configured(&slice, "complete");
    let mut accounts = AccountsConfig::fresh();
    accounts.connect(Provider::Claude); accounts.connect(Provider::Codex);
    std::fs::write(alondra_core::options::path_in(&slice._home), accounts.to_toml()).unwrap();
    let plan = LaunchPlan { model: Some("codex/fixture-model".into()),
        ..LaunchPlan::new("release-notes", slice.worktree.to_str().unwrap()) };
    slice.domain.store.lock().unwrap().set_launch_plan(&TASK.into(), &plan, 1).unwrap();
    slice.domain.enable_sdk(slice.backend.clone());
    slice.domain.admission().ignore_mac();
    slice.domain.admission().tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(4)) });
    let mut changed = slice.domain.subscribe();
    crate::auto_launch::pass(&slice.domain, &slice._home).await.unwrap();
    loop {
        let state = slice.domain.state().unwrap();
        if state.lifecycle_journal[&TaskId::from(TASK)].iter().any(|record| matches!(record, LifecycleRecord::TurnStarted { .. })) {
            assert_eq!(state.tasks[&TaskId::from(TASK)].provider, Provider::Codex);
            break;
        }
        tokio::time::timeout(std::time::Duration::from_secs(60), changed.changed()).await.expect("the Codex plan never started").unwrap();
    }
    assert!(transcript(&trace).iter().any(|message| message["method"] == "thread/start"));
    slice.domain.stop_sdk_owners();
}

#[tokio::test]
async fn a_reopened_task_with_unproven_claude_history_starts_codex_from_the_queue_without_retrying() {
    wired();
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
    slice.run(&user(), "cancel-implementation", Command::CancelTask { task_id: task.clone(), reason: None });
    original.stop().unwrap();
    drop(original);
    slice.run(&system(), "close-original", Command::RecordNodeEnd {
        task_id: task.clone(), end: NodeEnd::Closed, agent_id: Some(old_node.clone()),
    });
    let terminal = slice.domain.state().unwrap();
    assert_eq!(terminal.tasks[&task].state, alondra_domain_rules::TaskState::Cancelled);
    crate::domain_orchestrator_tools::update_launch_plan(&slice.domain, &slice._home, serde_json::from_value(serde_json::json!({
        "task_id": task, "changes": {"model": "codex/fixture-model", "reasoning_effort": "high"},
    })).unwrap()).unwrap();
    assert_eq!(slice.domain.launch_plan(&task).unwrap().unwrap().mission.as_deref(), Some(mission));
    assert_eq!(*slice.domain.state().unwrap(), *terminal, "preparing the provider must not reopen or rewrite the task");
    assert_eq!(slice.domain.state().unwrap().checklists[&task], checklist);

    slice.domain.enable_sdk(slice.backend.clone());
    slice.domain.admission().ignore_mac();
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
            && state.lifecycle_journal[&task].iter().filter(|record| matches!(record, LifecycleRecord::TurnEnded { .. })).count() == 1 + usize::from(alondra_domain::EMPTY_TURN_WAKES) { break; }
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

#[tokio::test]
async fn a_paused_codex_session_loses_its_app_server_and_delivers_resume_and_message_to_its_replacement() {
    wired();
    let slice = Slice::new();
    let trace = configured(&slice, "hold");
    let mut driver = launch(&slice);
    while Slice::lifecycle(&slice.domain).activity() != ActivityView::Working { driver.observe().unwrap(); }
    let native = native_process(&mut driver);
    slice.domain.enable_sdk(slice.backend.clone());
    slice.domain.sdk_owners().driven_by_hand.lock().unwrap().insert(driver.agent().clone());
    slice.domain.admission().ignore_mac();
    let agent = driver.agent().0.clone();
    slice.domain.gate().pause_by_user(&agent, alondra_core::now_millis()).unwrap();
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
    assert!(slice.domain.gate().resume_now(&agent, alondra_core::now_millis()));
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

#[tokio::test]
async fn a_paused_task_moves_at_once_and_starts_on_the_new_account_when_resumed(){
    wired();
    let slice=Slice::new();let task:TaskId=TASK.into();
    let source=working_on_codex(&slice);
    slice.domain.admission().ignore_mac();
    slice.domain.gate().pause_by_user(&source.0,alondra_core::now_millis()).unwrap();
    slice.domain.subscribe().wait_for(|_|paused_by_the_user(&slice)).await.unwrap();
    request_move(&slice);
    assert_eq!(settled_move(&slice).await,"done");
    let state=slice.domain.state().unwrap();
    assert!(state.tasks[&task].pending_selection.is_none());
    assert!(paused_by_the_user(&slice),"a paused task stays paused");
    assert_eq!(admissions(&slice),1,"nothing starts while it is paused");
    assert!(crate::provider_transition::moving(&slice.domain,&task),"no node but the one on the new account may start it");
    let pause=Slice::lifecycle(&slice.domain).cards().into_iter().find(|(_,reason)|*reason==alondra_domain::WaitReason::Pause).unwrap().0.clone();
    slice.run(&user(),"resume-moved",Command::AnswerQuestion{attachments:vec![],interaction_id:pause,option_id:alondra_domain_rules::NODE_PAUSE_RESUME.into(),inputs:Default::default()});
    let mut changed=slice.domain.subscribe();
    while admissions(&slice)<2{
        crate::provider_transition::pass(&slice.domain).await;
        if admissions(&slice)<2{tokio::time::timeout(std::time::Duration::from_secs(60),changed.changed()).await.expect("the resumed task never started").unwrap();}
    }
    let state=slice.domain.state().unwrap();
    assert_eq!(state.tasks[&task].account_key(),AccountKey::native(Provider::Claude));
    assert!(!crate::provider_transition::moving(&slice.domain,&task));
    slice.domain.stop_sdk_owners();
}

#[tokio::test]
async fn a_delivery_moved_while_reviewed_waits_for_the_user_and_starts_on_the_new_account_once_sent_back(){
    wired();
    let slice=Slice::new();let task:TaskId=TASK.into();
    let source=working_on_codex(&slice);
    let targets=vec![DeliveryTarget::Text{text:"The release notes".into()}];
    slice.run(&Actor::Agent{agent_id:source.clone()},"deliver-notes",Command::PresentResult{
        checklist:vec![],attachments:vec![],task_id:task.clone(),interaction_id:"notes".into(),
        body:"The release notes are written.".into(),targets:targets.clone(),is_final:true,nothing_to_review:false,
        card:CardSpec::delivery("Review the release notes",&targets,true),
    });
    request_move(&slice);
    assert_eq!(settled_move(&slice).await,"done");
    for _ in 0..3{crate::provider_transition::pass(&slice.domain).await;}
    let owed=Slice::lifecycle(&slice.domain).obligation().map(|obligation|(obligation.party,obligation.owed));
    assert_eq!(owed,Some((alondra_domain::Party::User,alondra_domain::Owed::User)),"the delivery waits for the user's review");
    assert_eq!(admissions(&slice),1);
    slice.domain.admission().ignore_mac();
    slice.domain.admission().tune(crate::admission::Tuning{spacing:0,limit:Some(crate::admission::Limit::Fixed(0))});
    slice.run(&user(),"send-back",Command::SubmitFeedback{merge_failure:None,attachments:vec![],presentation_id:"notes".into(),
        targets:targets.clone(),body:"Add the date".into(),instruction:alondra_domain_rules::Instruction::Changes});
    assert_eq!(Slice::lifecycle(&slice.domain).obligation().map(|o|(o.party,o.owed)), Some((alondra_domain::Party::Alondra,alondra_domain::Owed::Start)));
    for _ in 0..3{crate::provider_transition::pass(&slice.domain).await;}
    assert_eq!(admissions(&slice),1,"the gate holds the start while it has no slot");
    let restarted=slice.restart(); restarted.enable_sdk(slice.backend.clone());
    restarted.admission().ignore_mac();
    restarted.admission().tune(crate::admission::Tuning{spacing:0,limit:Some(crate::admission::Limit::Fixed(4))});
    let mut changed=restarted.subscribe();
    let admissions=||restarted.read().state().lifecycle_journal[&task].iter().filter(|record|matches!(record,LifecycleRecord::Admitted{..})).count();
    while admissions()<2{
        crate::provider_transition::pass(&restarted).await;
        if admissions()<2{tokio::time::timeout(std::time::Duration::from_secs(60),changed.changed()).await.expect("the delivery sent back never started").unwrap();}
    }
    assert_eq!(restarted.state().unwrap().tasks[&task].account_key(),AccountKey::native(Provider::Claude));
    assert!(!crate::provider_transition::moving(&restarted,&task));
    restarted.stop_sdk_owners();
}
