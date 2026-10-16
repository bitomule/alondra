//! How Alondra starts a task by itself: the launch plan every task is created with,
//! stored beside it, and the checks it must pass when it is given.
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{DomainError, RequestId};
use std::path::Path;

const MISSION_LIMIT: usize = 32_000;

pub(crate) use alondra_core::domain::LaunchPlan;

/// Whether a plan can start a task of this request: a launch with these fields would be accepted.
pub(crate) fn validate(domain: &DomainRuntime, home: &Path, request_id: &RequestId, plan: &LaunchPlan) -> Result<(), DomainError> {
    let invalid = |message: String| DomainError::new("invalid_input", message);
    if let Some(field) = plan.missing() {
        return Err(invalid(format!("{field} is required")));
    }
    let cwd = Path::new(&plan.cwd);
    if !cwd.is_absolute() || !cwd.is_dir() {
        return Err(invalid(format!("directory {} does not exist", plan.cwd)));
    }
    if plan.mission.as_deref().is_some_and(|mission| mission.chars().count() > MISSION_LIMIT) {
        return Err(invalid(format!("launch.mission is longer than {MISSION_LIMIT} characters")));
    }
    let file = crate::domain_schedule_runner::options_at(home).map_err(|error| invalid(format!("options.toml: {error}")))?;
    let requested = match crate::domain_orchestrator_tools::request_choices(domain, request_id).ok().flatten() {
        Some(choices) => choices.options,
        None => file.orchestrator_defaults(crate::options::project_of(&file, cwd).as_deref(), None),
    };
    let auto = requested.effort == alondra_core::options::AUTO;
    let given = |text: &Option<String>| text.as_deref().is_some_and(|text| !text.trim().is_empty());
    if auto && !(given(&plan.model) && given(&plan.reason)) {
        return Err(invalid("effort is auto for this request: the launch plan needs model and reason".into()));
    }
    requested.with_orchestrator_model(&file, plan.model.as_deref(), plan.effort.as_deref()).map_err(invalid)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_api::{check_launch_plan, check_launch_shape};
    use crate::domain_authority::Principal;
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
        Command::CreateTask {
            task_id: "next".into(), request_id: "r".into(), title: "Next".into(), agent_id: None, review_prs: false,
            dependencies: deps.iter().map(|dep| (*dep).into()).collect(), stack: None, launch,
        }
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
}
