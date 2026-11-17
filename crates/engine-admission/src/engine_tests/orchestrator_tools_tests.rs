//! The launch tools' tests that need the gate itself: the disk a launch needs and the slots a
//! launch holds.
use super::*;
use alondra_daemon_engine::domain_orchestrator_tools::*;
use alondra_daemon_engine::domain_orchestrator_tools::tests::*;

#[tokio::test]
async fn a_disk_preflight_refusal_invalidates_admission_before_a_remounted_volume_is_budgeted() {
    wired();
    use crate::admission::{Hold, Key, Limit, Tuning, Waiting};
    use alondra_domain_rules::TaskPriority;
    use std::collections::BTreeMap;
    const GB: u64 = 1_000_000_000;
    for origin in [Origin::Auto, Origin::Admitted] {
        let f = bare_world();
        let gate = &f.domain.admission();
        let watch = &f.domain.machine_watch();
        gate.tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(4)) });
        let mount = PathBuf::from("/Volumes/BuildCache");
        let space = |free| crate::disk::Space { total: 500 * GB, free, purgeable: 0 };
        let working = BTreeMap::from([("builder".into(), Some("app".to_owned()))]);
        watch.give_volumes_holding(vec![(mount.clone(), space(30 * GB), 20 * GB)]);
        watch.measure_volumes(&BTreeMap::new(), 0);
        watch.measure_volumes(&working, 1);
        watch.give_volumes_holding(vec![(mount.clone(), space(27 * GB), 23 * GB)]);
        watch.measure_volumes(&working, 2);
        watch.measure_volumes(&BTreeMap::new(), 3);
        watch.give_volumes(vec![]);
        watch.measure_volumes(&BTreeMap::new(), 4);
        let now = alondra_core::now_millis();
        let waiting = Waiting { key: Key::Task("t".into()), priority: TaskPriority::Normal, resuming: false, since: now };
        assert_eq!(gate.ask(&f.domain, waiting.clone(), now), Ok(()), "the learned volume is confirmed absent");

        watch.set_disk_low(true);
        let refused = launch_from(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None, origin).await.unwrap_err();
        assert_eq!(refused.code, "disk_low");
        assert!(f.task_node().is_none());
        watch.set_disk_low(false);
        watch.give_volumes(vec![(mount.clone(), space(4 * GB))]);
        watch.measure_volumes(&BTreeMap::new(), 5);
        assert!(matches!(gate.ask(&f.domain, waiting.clone(), now), Err(Hold::Disk { .. })),
            "an unused admission from before preflight cannot bypass the first insufficient volume budget");

        watch.give_volumes(vec![(mount, space(10 * GB))]);
        watch.measure_volumes(&BTreeMap::new(), 6);
        assert_eq!(gate.ask(&f.domain, waiting.clone(), now), Ok(()));
        let reserved = watch.disk_room(&BTreeMap::new(), None, now).expect("the new grant reserves its build on the remounted volume");
        assert_eq!(reserved.needed, 11 * GB);
        assert_eq!(gate.ask(&f.domain, waiting, now), Ok(()));
        assert_eq!(watch.disk_room(&BTreeMap::new(), None, now), Some(reserved), "re-reading the same grant charges it once");
    }
}

#[tokio::test]
async fn worktrees_root_admission_checks_storage_space_before_creating_a_worktree() {
    wired();
    let f = world();
    let git = |args: &[&str]| crate::worktree::testing::git(&f.project, args);
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "test@example.invalid"]);
    git(&["config", "user.name", "Test"]);
    git(&["commit", "-q", "--allow-empty", "-m", "base"]);
    let path = f.root.join("worktrees");
    std::fs::create_dir(&path).unwrap();
    let root = alondra_daemon_support::worktrees_root::capture(&path).unwrap();
    let options = alondra_core::options::path_in(&f.home);
    let original = std::fs::read_to_string(&options).unwrap_or_default();
    std::fs::write(&options, alondra_core::options::with_worktrees_root(&original, Some(&root)).unwrap()).unwrap();
    let space = |free| crate::disk::Space { total: 500_000_000_000, free, purgeable: 0 };
    f.domain.machine_watch().give_launch_space(path.clone(), space(1_000_000_000));
    let queued = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    assert_eq!(queued["queued"], true);
    assert!(!path.join("App").exists(), "admission happens before creating the repository or task folder");
    assert!(f.sdk().starts().is_empty(), "the healthy system disk cannot substitute for full worktree storage");
    f.domain.machine_watch().give_launch_space(path.clone(), space(400_000_000_000));
    crate::admission::pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(f.launched(1).await.0, [path.join("App/t")]);
}

#[tokio::test]
async fn worktrees_root_keeps_an_admitted_launch_queued_until_the_volume_returns() {
    wired();
    let f = world();
    let git = |args: &[&str]| crate::worktree::testing::git(&f.project, args);
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "test@example.invalid"]);
    git(&["config", "user.name", "Test"]);
    git(&["commit", "-q", "--allow-empty", "-m", "base"]);
    let path = f.root.join("worktrees");
    let missing = alondra_core::options::WorktreesRoot { path: path.clone(), volume_id: "00000000-0000-0000-0000-000000000001".into() };
    let options = alondra_core::options::path_in(&f.home);
    let original = std::fs::read_to_string(&options).unwrap_or_default();
    let save = |root| std::fs::write(&options, alondra_core::options::with_worktrees_root(&original, Some(root)).unwrap()).unwrap();
    save(&missing);
    let queued = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    assert_eq!(queued["queued"], true);
    for _ in 0..2 {
        crate::admission::pass(&f.domain, &f.home).await.unwrap();
        assert_eq!(f.domain.store.lock().unwrap().launch_queue().unwrap().len(), 1);
        assert!(f.sdk().starts().is_empty());
    }
    assert!(!path.exists());
    std::fs::create_dir(&path).unwrap();
    save(&alondra_daemon_support::worktrees_root::capture(&path).unwrap());
    crate::admission::pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(f.launched(1).await.0, [path.join("App/t")]);
}

#[tokio::test]
async fn a_launch_while_the_disk_is_nearly_full_waits_in_the_queue_and_starts_once_it_has_space() {
    wired();
    let f = world();
    f.domain.machine_watch().set_disk_low(true);
    let queued = launch(&f.domain, &f.home, launch_body(&f, json!({"model": "sonnet"})), |_| None).await.unwrap();
    assert_eq!(queued["queued"], true, "{queued}");
    assert!(queued["message"].as_str().unwrap().contains("the disk has no room for one more build (1.0 GB free of 5.0 GB needed)"), "{queued}");
    assert!(f.sdk().starts().is_empty());
    assert!(f.task_node().is_none(), "the task waits as it was");
    assert_eq!(f.domain.store.lock().unwrap().launch_queue().unwrap().len(), 1);

    f.domain.machine_watch().set_disk_low(false);
    crate::admission::pass(&f.domain, &f.home).await.unwrap();
    assert_eq!(f.launched(1).await.0.len(), 1);
}

#[tokio::test]
async fn an_unstarted_selection_replaces_a_disabled_account_but_a_started_one_cannot_move() {
    wired();
    use alondra_core::accounts::{AccountId, AccountKey, Provider};
    use alondra_core::model_selection::{ModelKey, ModelProfile, ModelSelection, TaskSelection};
    let f = world();
    connect_claude_and_codex(&f);
    let mut catalog = f.domain.state().unwrap().model_catalogs.iter().find(|catalog| catalog.account.provider == Provider::Claude).unwrap().clone();
    with_work_account(&f);
    let work = AccountKey::new(Provider::Claude, AccountId::parse("work").unwrap());
    catalog.account = work.clone();
    command(&f.domain, Actor::System, Command::RecordModelCatalog { catalog });
    std::fs::write(alondra_core::options::path_in(&f.home), "[accounts]\nconnected = [\"claude\"]\n[[accounts.claude]]\nid = \"default\"\nname = \"Default\"\nenabled = false\n[[accounts.claude]]\nid = \"work\"\nname = \"Work\"\n").unwrap();
    let file = crate::domain_schedule_runner::options_at(&f.home).unwrap();
    command(&f.domain, Actor::System, Command::RecordAccountConfiguration { order: vec![AccountKey::native(Provider::Claude), work.clone()], version: alondra_core::accounts::config_version(&file.accounts) });
    let requested = file.orchestrator_defaults(None, None);
    let mut task = f.domain.state().unwrap().tasks[&TaskId::from("t")].clone();
    let mut intent = TaskSelection::automatic(ModelProfile::Implementation);
    intent.account = Some(AccountKey::native(Provider::Claude));
    intent.requested = Some(ModelKey::parse_wire("claude/sonnet").unwrap());
    intent.allow_model_substitution = false;
    intent.effective = Some(ModelSelection { model: intent.requested.clone().unwrap(), reasoning_effort: Some("medium".into()), catalog_revision: "fixture".into() });
    task.selection = Some(intent.clone());
    command(&f.domain, Actor::System, Command::RecordTaskSelection { task_id: "t".into(), selection: intent, pending: None });
    let body = launch_body(&f, json!({"model": "claude/sonnet"}));
    let (resolved, account, _) = native_launch(&f.domain, &f.home, &task, &requested, &file, &body, false, false)
        .expect("a task that never started must reselect an active account").unwrap();
    assert_eq!(account, work);
    assert_eq!(resolved.selection.unwrap().requested, task.selection.as_ref().unwrap().requested, "a compatible model stays binding");
    task.started = true;
    assert!(native_launch(&f.domain, &f.home, &task, &requested, &file, &body, true, false).is_err(), "an existing session never migrates");
    f.domain.machine_watch().set_disk_low(true);
    let queued = launch(&f.domain, &f.home, body, |_| None).await.unwrap();
    assert_eq!(queued["queued"], true);
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].selection.as_ref().unwrap().account, Some(work.clone()), "the move survives a refusal before admission");
    assert!(f.sdk().starts().is_empty());
    f.domain.machine_watch().set_disk_low(false);
    crate::admission::pass(&f.domain, &f.home).await.unwrap();
    f.launched(1).await;
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].account_key(), work, "the admitted execution keeps the reselected account");
}

#[tokio::test]
async fn an_unstarted_selection_waits_without_a_card_then_starts_on_an_active_provider() {
    wired();
    use alondra_core::accounts::{AccountKey, Provider};
    use alondra_core::model_selection::{ModelKey, ModelProfile, TaskSelection};
    let f = world();
    connect_claude_and_codex(&f);
    for mut catalog in f.domain.state().unwrap().model_catalogs.clone() {
        catalog.native_default = Some(catalog.models[0].model.clone());
        command(&f.domain, Actor::System, Command::RecordModelCatalog { catalog });
    }
    let mut intent = TaskSelection::automatic(ModelProfile::Implementation);
    intent.account = Some(AccountKey::native(Provider::Codex));
    intent.requested = Some(ModelKey::parse_wire("codex/gpt-6").unwrap());
    intent.requested_reasoning_effort = Some("medium".into());
    intent.allow_model_substitution = false;
    command(&f.domain, Actor::System, Command::RecordTaskSelection { task_id: "t".into(), selection: intent.clone(), pending: None });
    let plan = alondra_daemon_engine_model::LaunchPlan { model: Some("codex/gpt-6".into()), effort: Some("medium".into()),
        ..alondra_daemon_engine_model::LaunchPlan::new("app/editor", f.project.to_str().unwrap()) };
    f.domain.store.lock().unwrap().set_launch_plan(&"t".into(), &plan, 1).unwrap();
    std::fs::write(alondra_core::options::path_in(&f.home), "[accounts]\nconnected = []\n").unwrap();
    let cards = f.domain.state().unwrap().interactions.len();
    for _ in 0..=alondra_domain_rules::START_RETRIES.len() {
        crate::auto_launch::pass(&f.domain, &f.home).await.unwrap();
        let store = f.domain.store.lock().unwrap();
        assert!(store.start_retries().unwrap().is_empty(), "account availability is not a failed start");
        assert!(store.auto_launch(&"t".into()).unwrap().is_none(), "the next pass can try again");
        drop(store);
        assert_eq!(f.domain.admission().snapshot(&f.domain, alondra_core::now_millis()).unwrap().working, 0, "an unavailable account reserves no working slot");
    }
    assert_eq!(f.domain.state().unwrap().interactions.len(), cards);
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].selection.as_ref(), Some(&intent), "waiting preserves the selection until a destination is known");
    assert!(f.sdk().starts().is_empty());
    std::fs::write(alondra_core::options::path_in(&f.home), "[accounts]\nconnected = [\"claude\"]\n").unwrap();
    let mut catalog = f.domain.state().unwrap().model_catalogs.iter().find(|catalog| catalog.account.provider == Provider::Claude).unwrap().clone();
    let default = catalog.native_default.take();
    command(&f.domain, Actor::System, Command::RecordModelCatalog { catalog: catalog.clone() });
    crate::auto_launch::pass(&f.domain, &f.home).await.unwrap();
    assert!(f.domain.store.lock().unwrap().start_retries().unwrap().is_empty(), "a destination without a compatible model waits too");
    assert!(f.sdk().starts().is_empty());
    catalog.native_default = default;
    command(&f.domain, Actor::System, Command::RecordModelCatalog { catalog });
    crate::auto_launch::pass(&f.domain, &f.home).await.unwrap();
    f.launched(1).await;
    let state = f.domain.state().unwrap();
    let task = &state.tasks[&TaskId::from("t")];
    assert_eq!(task.account_key(), AccountKey::native(Provider::Claude));
    let selection = task.selection.as_ref().unwrap();
    assert_eq!(selection.account, Some(task.account_key()));
    assert_eq!(selection.effective.as_ref().unwrap().model.provider, Provider::Claude);
    assert!(selection.requested.is_none());
    assert_eq!(state.interactions.len(), cards, "no intervention is needed to reselect");
}

#[tokio::test]
async fn reopening_a_legacy_task_admits_its_retained_project_policy_after_the_projects_root_changes() {
    wired();
    let f = world();
    main_checkout(&f);
    let original = f.project.join(".claude/worktrees/t");
    git_in(&f.project, &["worktree", "add", "-q", "-b", "worktree-t", original.to_str().unwrap()]);
    let options = alondra_core::options::path_in(&f.home);
    std::fs::write(&options, format!("projects_root = {:?}\n[projects.App]\ndefaults = {{ effort = \"hard\" }}\n", f.root.to_str().unwrap())).unwrap();
    let saved = alondra_daemon_engine_model::LaunchPlan {
        project: Some("App".into()), mission: Some("Preserve the approved editor colours and contrast.".into()),
        ..alondra_daemon_engine_model::LaunchPlan::new("app/editor", original.to_str().unwrap())
    };
    crate::launch_plans::validate(&f.domain, &f.home, &"r".into(), &saved).unwrap();
    f.domain.store.lock().unwrap().set_launch_plan(&"t".into(), &saved, 1).unwrap();
    f.domain.set_task_worktree(&"t".into(), original.to_str().unwrap()).unwrap();
    give(&f, "t", "agent-b");
    command(&f.domain, Actor::Agent { agent_id: "agent-b".into() }, Command::StartTask { task_id: "t".into() });
    command(&f.domain, user(), Command::CancelTask { task_id: "t".into(), reason: None });
    git_in(&f.project, &["worktree", "remove", original.to_str().unwrap()]);
    command(&f.domain, Actor::System, Command::ReleaseTaskWorktree {
        task_id: "t".into(), path: original.display().to_string(), end: alondra_domain_rules::WorktreeEnd::Removed,
    });
    command(&f.domain, Actor::System, Command::RecordNodeEnd {
        task_id: "t".into(), end: alondra_domain_rules::NodeEnd::Closed, agent_id: Some("agent-b".into()),
    });
    assert!(!original.exists());
    assert!(f.domain.state().unwrap().tasks[&TaskId::from("t")].selection.is_none(), "the previous release used legacy effort options");
    assert!(request_choices(&f.domain, &"r".into()).unwrap().is_none());

    std::fs::write(&options, format!("projects_root = {:?}\n[projects.App]\ndefaults = {{ effort = \"hard\" }}\n", f.root.join("new-projects").to_str().unwrap())).unwrap();
    let file = crate::domain_schedule_runner::options_at(&f.home).unwrap();
    assert!(crate::options::project_of(&file, &original).is_none(), "the old folder is outside the newly configured projects root");
    assert_eq!(file.orchestrator_defaults(None, None).effort, alondra_core::options::AUTO);
    partial_plan(&f, "t", json!({"reason": "Continue the approved work with its original project policy"})).unwrap();
    command(&f.domain, the_orchestrator(&f.domain), Command::ReopenTask { task_id: "t".into() });
    assert!(f.domain.state().unwrap().pending_reopening(&"t".into()).is_some());
    crate::admission::pass_at(&f.domain, &f.home, alondra_core::now_millis()).await.unwrap();
    assert!(f.task_node().is_some(), "admission must use App's hard policy instead of rejecting a missing model under global auto");
    let (folders, prompts) = f.launched(1).await;
    assert_eq!(f.sdk().starts().len(), 1);
    assert!(same(&folders[0], &original), "the managed worktree is reconstructed before the native launch: {folders:?}");
    assert_eq!(f.sdk().flag("--model").as_deref(), Some("opus"));
    assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("t")].model,
        Some(TaskModel { model: "opus".into(), chosen_by: ModelChooser::User }));
    assert!(prompts[0].contains(saved.mission.as_deref().unwrap()));
    let store = f.domain.store.lock().unwrap();
    assert!(store.launch_queue().unwrap().is_empty());
    assert!(store.start_retries().unwrap().is_empty());
}

#[tokio::test]
async fn terminal_launch_choices_can_be_prepared_without_enqueuing_or_starting_the_task() {
    wired();
    let f = world();
    let before = cancelled_launch_plan_with_history(&f);
    let state = f.domain.state().unwrap();
    assert!(state.tasks[&TaskId::from("t")].started, "a closed task keeps its start as history");
    assert_eq!(state.tasks[&TaskId::from("t")].agent_id, Some("agent-b".into()));
    partial_plan(&f, "t", json!({"model": "codex/gpt-6", "reasoning_effort": "high", "reason": "Use Codex for the approved implementation"})).unwrap();
    assert_eq!(*f.domain.state().unwrap(), *state, "preparing choices does not reopen or rewrite the task and its approvals");
    let prepared = f.domain.launch_plan(&"t".into()).unwrap().unwrap();
    assert_eq!(prepared, alondra_daemon_engine_model::LaunchPlan { model: Some("codex/gpt-6".into()), effort: Some("high".into()),
        reason: Some("Use Codex for the approved implementation".into()), ..before });
    crate::admission::pass_at(&f.domain, &f.home, alondra_core::now_millis()).await.unwrap();
    crate::auto_launch::pass(&f.domain, &f.home).await.unwrap();
    assert!(f.sdk().starts().is_empty(), "admission between preparation and reopening cannot start a terminal task");
    {
        let store = f.domain.store.lock().unwrap();
        assert!(store.launch_queue().unwrap().is_empty());
        assert!(store.start_retries().unwrap().is_empty());
        assert!(store.task_was_launched(&"t".into()).unwrap(), "launch history is never erased to edit choices");
    }
    command(&f.domain, the_orchestrator(&f.domain), Command::ReopenTask { task_id: "t".into() });
    assert_eq!(f.domain.store.lock().unwrap().launch_queue().unwrap()[0].body["body"]["model"], "codex/gpt-6");
    partial_plan(&f, "t", json!({"reason": "The reopened task keeps the prepared provider"})).unwrap();
    assert_eq!(f.domain.launch_plan(&"t".into()).unwrap().unwrap().model.as_deref(), Some("codex/gpt-6"),
        "historical launch decisions do not block an edit before this reopening's first admission");
    let pending = f.domain.launch_plan(&"t".into()).unwrap().unwrap();
    for changes in [json!({"tags": {"kind": "feature"}}), json!({"approach": "subagents"})] {
        assert_eq!(partial_plan(&f, "t", changes).unwrap_err().code, "invalid_input");
    }
    let replacement = serde_json::from_value(json!({"task_id": "t", "launch": pending})).unwrap();
    assert_eq!(set_launch_plan(&f.domain, &f.home, replacement).unwrap_err().code, "invalid_input");
    assert_eq!(f.domain.launch_plan(&"t".into()).unwrap().unwrap(), pending,
        "a pending reopening preserves its mission, directory, name and approved scope too");
}

#[tokio::test]
async fn preparing_terminal_choices_preserves_a_deleted_launch_directory_without_recreating_it() {
    wired();
    let f = world();
    let before = cancelled_launch_plan_with_history(&f);
    std::fs::remove_dir(&f.project).unwrap();
    partial_plan(&f, "t", json!({"model": "codex/gpt-6", "reasoning_effort": "high"})).unwrap();
    let after = f.domain.launch_plan(&"t".into()).unwrap().unwrap();
    assert_eq!(after, alondra_daemon_engine_model::LaunchPlan { model: Some("codex/gpt-6".into()), effort: Some("high".into()), ..before });
    crate::admission::pass_at(&f.domain, &f.home, alondra_core::now_millis()).await.unwrap();
    assert!(!f.project.exists(), "only the reopened admission reconstructs its workspace");
    assert!(f.sdk().starts().is_empty());
    let store = f.domain.store.lock().unwrap();
    assert!(store.launch_queue().unwrap().is_empty());
    assert!(store.start_retries().unwrap().is_empty());
}
