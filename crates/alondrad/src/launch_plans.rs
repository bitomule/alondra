//! How Alondra starts a task by itself: the launch plan the orchestrator gives a task that waits
//! on others, stored beside the task, and the checks it must pass when it is given.
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{DomainError, RequestId};
use serde::{Deserialize, Serialize};
use std::path::Path;

const MISSION_LIMIT: usize = 32_000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LaunchPlan {
    pub name: String,
    pub cwd: String,
    #[serde(default)]
    pub mission: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub approach: Option<alondra_core::options::Approach>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub role: Option<TaskRole>,
}

/// The part work split into design, implementation and review plays, so each is measured apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskRole {
    Design,
    Implementation,
    Review,
}

impl TaskRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Design => "design",
            Self::Implementation => "implementation",
            Self::Review => "review",
        }
    }
}

/// Whether a plan can start a task of this request: a launch with these fields would be accepted.
pub(crate) fn validate(domain: &DomainRuntime, home: &Path, request_id: &RequestId, plan: &LaunchPlan) -> Result<(), DomainError> {
    let invalid = |message: String| DomainError::new("invalid_input", message);
    if plan.name.trim().is_empty() {
        return Err(invalid("launch.name is required".into()));
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
    use crate::domain_api::{check_launch_plan, take_launch_plan};
    use crate::domain_authority::Principal;
    use alondra_core::domain::{Actor, Command, CommandEnvelope, PROTOCOL_VERSION};
    use serde_json::json;

    struct World {
        root: std::path::PathBuf,
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
        let root = std::env::temp_dir().join(format!("alondra-launch-plans-{}", crate::auth::random_hex(8)));
        let home = root.join("home");
        let project = root.join("App");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        let state = domain.state().unwrap();
        domain.execute(&Actor::User { user_id: "u".into() }, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "request".into(), expected_revision: state.revision,
            command: Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false },
        }, None, || Ok(())).unwrap();
        World { root, home, project, domain }
    }

    fn plan(w: &World, extra: serde_json::Value) -> LaunchPlan {
        let mut plan = json!({"name": "app/next", "cwd": w.project.to_str().unwrap(), "model": "sonnet", "reason": "clear"});
        plan.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(plan).unwrap()
    }

    fn create(deps: &[&str]) -> Command {
        Command::CreateTask {
            task_id: "next".into(), request_id: "r".into(), title: "Next".into(), agent_id: None, review_prs: false,
            dependencies: deps.iter().map(|dep| (*dep).into()).collect(), stack: None,
        }
    }

    #[test]
    fn create_task_with_dependencies_needs_a_launch_plan() {
        let w = world();
        let orchestrator = Principal::Orchestrator { generation: 0, execution_id: "e".into() };
        let refused = check_launch_plan(&w.domain, &w.home, &orchestrator, &create(&["a", "b"]), None).unwrap_err();
        assert_eq!(refused.code, "launch_plan_required");
        assert_eq!(refused.message, "This task depends on a, b: pass launch with at least name and cwd (and, with auto effort, model and reason) so Alondra starts it by itself as soon as they are done.");
        check_launch_plan(&w.domain, &w.home, &orchestrator, &create(&[]), None).unwrap();
        check_launch_plan(&w.domain, &w.home, &orchestrator, &create(&["a"]), Some(&plan(&w, json!({})))).unwrap();
        let agent = Principal::Agent { session_id: "s".into(), task_binding: None, provider_agent_id: None };
        check_launch_plan(&w.domain, &w.home, &agent, &create(&["a"]), None).unwrap();
        check_launch_plan(&w.domain, &w.home, &orchestrator, &Command::CancelTask { task_id: "t".into() }, None).unwrap();
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
    fn the_plan_travels_beside_the_typed_command_and_not_inside_it() {
        let mut body = json!({"data_epoch": "e", "title": "Next", "launch": {"name": "app/next", "cwd": "/tmp"}});
        let plan = take_launch_plan("POST", "/requests/r/tasks", &mut body).unwrap().unwrap();
        assert_eq!((plan.name.as_str(), plan.cwd.as_str()), ("app/next", "/tmp"));
        assert!(body.get("launch").is_none(), "the command stays strict");
        let mut other = json!({"launch": {"name": "x", "cwd": "/tmp"}});
        assert_eq!(take_launch_plan("POST", "/questions", &mut other).unwrap(), None);
        assert!(other.get("launch").is_some(), "only a new task takes one");
        let mut broken = json!({"launch": {"name": "x"}});
        let refused = take_launch_plan("POST", "/requests/r/tasks", &mut broken).unwrap_err();
        assert_eq!(refused.code, "invalid_input");
        assert!(refused.message.starts_with("launch: "), "{}", refused.message);
        let mut unknown = json!({"launch": {"name": "x", "cwd": "/tmp", "speed": "fast"}});
        assert_eq!(take_launch_plan("POST", "/requests/r/tasks", &mut unknown).unwrap_err().code, "invalid_input");
    }

    #[test]
    fn set_launch_plan_replaces_the_plan_and_unblocks_a_task_waiting_for_one() {
        let w = world();
        let state = w.domain.state().unwrap();
        w.domain.execute(&Actor::User { user_id: "u".into() }, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "task".into(), expected_revision: state.revision,
            command: create(&[]),
        }, None, || Ok(())).unwrap();
        w.domain.store.lock().unwrap().set_auto_launch(&"next".into(), crate::domain_store::AutoLaunchState::Blocked,
            Some(crate::domain_store::AutoLaunchReason::NoPlan), None, 1).unwrap();
        let body = |extra: serde_json::Value| serde_json::from_value(json!({"task_id": "next", "launch": plan(&w, extra)})).unwrap();
        let stored = crate::domain_orchestrator_tools::set_launch_plan(&w.domain, &w.home, body(json!({}))).unwrap();
        assert_eq!(stored, json!({"task": "next", "stored": true}));
        assert_eq!(w.domain.store.lock().unwrap().auto_launch(&"next".into()).unwrap(), None, "the next pass starts it");
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
