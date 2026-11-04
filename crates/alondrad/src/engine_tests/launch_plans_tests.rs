//! The launch plans alondrad's API accepts for a new task, resolved by the engine.
use alondra_daemon_engine::launch_plans::*;
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::LaunchPlan;
use crate::domain_api::{check_launch_plan, check_launch_shape};
use alondra_core::domain::{Actor, Command, CommandEnvelope, PROTOCOL_VERSION};
use serde_json::json;

struct World {
    root: crate::test_support::Directory,
    home: std::path::PathBuf,
    project: std::path::PathBuf,
    domain: DomainRuntime,
}
impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn world() -> World {
    let root = crate::test_support::Directory::new();
    let home = root.join("home");
    let project = root.join("App");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let domain = DomainRuntime::open(&home).unwrap();
    let state = domain.state().unwrap();
    domain.execute(&Actor::User { user_id: "u".into() }, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "request".into(), expected_revision: state.revision,
        command: Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false },
    }, None, || Ok(())).unwrap();
    World { root, home, project, domain }
}

fn plan(w: &World, extra: serde_json::Value) -> LaunchPlan {
    let mut plan = json!({"name": "app/next", "cwd": w.project.to_str().unwrap(), "model": "sonnet", "reason": "clear"});
    plan.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    serde_json::from_value(plan).unwrap()
}

fn create(deps: &[&str], launch: Option<LaunchPlan>) -> Command {
    Command::CreateTask { plan_items: None, continues: None,
        task_id: "next".into(), request_id: "r".into(), title: "Next".into(), agent_id: None, review_prs: false,
        dependencies: deps.iter().map(|dep| (*dep).into()).collect(), stack: None, launch,
    }
}

#[test]
fn a_new_task_belongs_to_the_project_its_request_asked_for_or_else_to_the_folder_it_works_in() {
    let w = world();
    std::fs::write(alondra_core::options::path_in(&w.home), format!("projects_root = {:?}\n", w.root.to_str().unwrap())).unwrap();
    let stamped = |command: Command| {
        let mut command = command;
        stamp_project(&w.domain, &w.home, &mut command);
        match command {
            Command::CreateTask { launch: Some(plan), .. } => plan.project,
            other => panic!("not a new task: {other:?}"),
        }
    };
    assert_eq!(stamped(create(&[], Some(plan(&w, json!({"project": "Other"}))))).as_deref(), Some("App"),
        "the folder under the projects root, whatever the orchestrator said");
    assert_eq!(stamped(create(&[], Some(plan(&w, json!({"cwd": "/"}))))), None, "a folder outside the projects root has none");
    let state = w.domain.state().unwrap();
    let choices = crate::domain_store::RequestLaunch {
        project: Some("Quill".into()), options: alondra_core::options::OptionsFile::default().orchestrator_defaults(Some("Quill"), None),
        issue: None, issue_text: None, attachments: vec![],
    };
    w.domain.execute_with(&Actor::User { user_id: "u".into() }, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "chosen".into(), expected_revision: state.revision,
        command: Command::CreateRequest { request_id: "chosen".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false },
    }, None, Some(&choices), || Ok(())).unwrap();
    let mut chosen = create(&[], Some(plan(&w, json!({}))));
    if let Command::CreateTask { request_id, .. } = &mut chosen { *request_id = "chosen".into(); }
    assert_eq!(stamped(chosen).as_deref(), Some("Quill"), "the project the user asked for wins");
}

#[test]
fn a_new_tasks_plan_is_checked_against_the_disk_and_the_request() {
    let w = world();
    check_launch_plan(&w.domain, &w.home, &create(&[], Some(plan(&w, json!({}))))).unwrap();
    let missing = check_launch_plan(&w.domain, &w.home, &create(&[], Some(plan(&w, json!({"cwd": "/no/such/folder"}))))).unwrap_err();
    assert_eq!(missing.message, "directory /no/such/folder does not exist");
    check_launch_plan(&w.domain, &w.home, &create(&["a"], None)).unwrap();
    check_launch_plan(&w.domain, &w.home, &Command::CancelTask { task_id: "t".into() }).unwrap();
}

#[test]
fn a_suggestion_that_becomes_a_task_has_its_plan_checked_like_any_new_task() {
    let w = world();
    let settle = |launch: LaunchPlan| Command::SettleSuggestion {
        interaction_id: "p".into(),
        decision: alondra_core::domain::SuggestionDecision::NewTask {
            task_id: "readme".into(), request_id: "r-readme".into(), title: "README".into(), launch,
            dependencies: Default::default(), note: None,
        },
    };
    check_launch_plan(&w.domain, &w.home, &settle(plan(&w, json!({})))).unwrap();
    let missing = check_launch_plan(&w.domain, &w.home, &settle(plan(&w, json!({"cwd": "/no/such/folder"})))).unwrap_err();
    assert_eq!(missing.message, "directory /no/such/folder does not exist");
    assert!(check_launch_shape("POST", "/interactions/p/settlement", &json!({"decision": {"launch": {"name": "x"}}})).is_err());
}

#[test]
fn a_launch_plan_needs_model_and_reason_with_auto_effort_and_an_existing_folder() {
    let w = world();
    let request = alondra_core::domain::RequestId::from("r");
    validate(&w.domain, &w.home, &request, &plan(&w, json!({}))).unwrap();
    let without_model = validate(&w.domain, &w.home, &request, &plan(&w, json!({"model": null}))).unwrap_err();
    assert_eq!(without_model.message, "effort is auto for this request: the launch plan needs model and reason");
    let without_reason = validate(&w.domain, &w.home, &request, &plan(&w, json!({"reason": "  "}))).unwrap_err();
    assert_eq!(without_reason.code, "invalid_input");
    let missing = validate(&w.domain, &w.home, &request, &plan(&w, json!({"cwd": "/no/such/folder"}))).unwrap_err();
    assert_eq!(missing.message, "directory /no/such/folder does not exist");
    let relative = validate(&w.domain, &w.home, &request, &plan(&w, json!({"cwd": "App"}))).unwrap_err();
    assert_eq!(relative.code, "invalid_input");
    assert_eq!(validate(&w.domain, &w.home, &request, &plan(&w, json!({"name": " "}))).unwrap_err().code, "invalid_input");
    let unknown = validate(&w.domain, &w.home, &request, &plan(&w, json!({"model": "nonsense"}))).unwrap_err();
    assert!(unknown.message.contains("nonsense is not a model"), "{}", unknown.message);
    let long = validate(&w.domain, &w.home, &request, &plan(&w, json!({"mission": "x".repeat(32_001)}))).unwrap_err();
    assert_eq!(long.code, "invalid_input");
}

#[test]
fn preparing_reopening_uses_the_retained_project_defaults_without_the_directory_and_keeps_explicit_request_choices_binding() {
    let w = world();
    std::fs::write(alondra_core::options::path_in(&w.home), "[projects.App]\ndefaults = { effort = \"hard\" }\n").unwrap();
    let saved = plan(&w, json!({"project": "App", "model": null}));
    std::fs::remove_dir(&w.project).unwrap();
    assert!(crate::options::project_of(&crate::domain_schedule_runner::options_at(&w.home).unwrap(), &w.project).is_none(),
        "the retained workspace cannot infer its original project from the current project root");
    validate_reopening(&w.domain, &w.home, &"r".into(), &saved).unwrap();
    assert!(!w.project.exists());
    assert_eq!(validate(&w.domain, &w.home, &"r".into(), &saved).unwrap_err().code, "invalid_input");
    let relative = plan(&w, json!({"project": "App", "model": null, "cwd": "App"}));
    assert_eq!(validate_reopening(&w.domain, &w.home, &"r".into(), &relative).unwrap_err().code, "invalid_input");

    let choices = crate::domain_store::RequestLaunch {
        project: Some("App".into()), options: alondra_core::options::OptionsFile::default().orchestrator_defaults(Some("App"), None),
        issue: None, issue_text: None, attachments: vec![],
    };
    let state = w.domain.state().unwrap();
    w.domain.execute_with(&Actor::User { user_id: "u".into() }, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "explicit-choices".into(), expected_revision: state.revision,
        command: Command::CreateRequest { request_id: "explicit".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false },
    }, None, Some(&choices), || Ok(())).unwrap();
    let error = validate_reopening(&w.domain, &w.home, &"explicit".into(), &saved).unwrap_err();
    assert_eq!(error.message, "effort is auto for this request: the launch plan needs model and reason",
        "explicit Request choices take precedence over the retained project's defaults");
}

#[test]
fn a_malformed_plan_is_refused_saying_what_is_wrong_with_it() {
    let body = json!({"data_epoch": "e", "title": "Next", "launch": {"name": "app/next", "cwd": "/tmp"}});
    check_launch_shape("POST", "/requests/r/tasks", &body).unwrap();
    check_launch_shape("POST", "/questions", &json!({"launch": {"name": "x"}})).unwrap();
    let refused = check_launch_shape("POST", "/requests/r/tasks", &json!({"launch": {"name": "x"}})).unwrap_err();
    assert_eq!(refused.code, "invalid_input");
    assert!(refused.message.starts_with("launch: "), "{}", refused.message);
    let unknown = json!({"launch": {"name": "x", "cwd": "/tmp", "speed": "fast"}});
    assert_eq!(check_launch_shape("POST", "/requests/r/tasks", &unknown).unwrap_err().code, "invalid_input");
}

#[test]
fn set_launch_plan_replaces_the_plan() {
    let w = world();
    let state = w.domain.state().unwrap();
    w.domain.execute(&Actor::Orchestrator { generation: 0 }, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "task".into(), expected_revision: state.revision,
        command: create(&[], Some(plan(&w, json!({})))),
    }, None, || Ok(())).unwrap();
    assert_eq!(w.domain.launch_plan(&"next".into()).unwrap().unwrap().name, "app/next", "stored with the task");
    let body = |extra: serde_json::Value| serde_json::from_value(json!({"task_id": "next", "launch": plan(&w, extra)})).unwrap();
    let stored = crate::domain_orchestrator_tools::set_launch_plan(&w.domain, &w.home, body(json!({}))).unwrap();
    assert_eq!(stored, json!({"task": "next", "stored": true}));
    crate::domain_orchestrator_tools::set_launch_plan(&w.domain, &w.home, body(json!({"name": "app/replaced"}))).unwrap();
    assert_eq!(w.domain.launch_plan(&"next".into()).unwrap().unwrap().name, "app/replaced");
    let invalid = crate::domain_orchestrator_tools::set_launch_plan(&w.domain, &w.home, body(json!({"cwd": "/no/such"}))).unwrap_err();
    assert_eq!(invalid.code, "invalid_input");
    assert_eq!(w.domain.launch_plan(&"next".into()).unwrap().unwrap().name, "app/replaced", "a refused plan changes nothing");
    let unknown = crate::domain_orchestrator_tools::set_launch_plan(&w.domain, &w.home,
        serde_json::from_value(json!({"task_id": "nope", "launch": plan(&w, json!({}))})).unwrap()).unwrap_err();
    assert_eq!(unknown.code, "not_found");
}

