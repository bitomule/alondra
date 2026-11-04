//! How Alondra starts a task by itself: the launch plan every task is created with,
//! stored beside it, and the checks it must pass when it is given.
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{Command, DomainError, RequestId, SuggestionDecision};
use std::path::Path;

const MISSION_LIMIT: usize = 32_000;

pub use alondra_core::domain::LaunchPlan;

/// Whether a plan can start a task of this request: a launch with these fields would be accepted.
pub fn validate(domain: &DomainRuntime, home: &Path, request_id: &RequestId, plan: &LaunchPlan) -> Result<(), DomainError> {
    validate_with_directory(domain, home, request_id, plan, true)
}

/// A closed task preserves its launch directory, which the first reopened admission may rebuild.
pub fn validate_reopening(domain: &DomainRuntime, home: &Path, request_id: &RequestId, plan: &LaunchPlan) -> Result<(), DomainError> {
    validate_with_directory(domain, home, request_id, plan, false)
}

fn validate_with_directory(domain: &DomainRuntime, home: &Path, request_id: &RequestId, plan: &LaunchPlan, require_directory: bool) -> Result<(), DomainError> {
    let invalid = |message: String| DomainError::new("invalid_input", message);
    if let Some(field) = plan.missing() {
        return Err(invalid(format!("{field} is required")));
    }
    let cwd = Path::new(&plan.cwd);
    if !cwd.is_absolute() || (require_directory && !cwd.is_dir()) {
        return Err(invalid(format!("directory {} does not exist", plan.cwd)));
    }
    if plan.mission.as_deref().is_some_and(|mission| mission.chars().count() > MISSION_LIMIT) {
        return Err(invalid(format!("launch.mission is longer than {MISSION_LIMIT} characters")));
    }
    if let Some(Err(why)) = plan.tags.as_ref().map(alondra_core::task_tags::TaskTags::validate) {
        return Err(invalid(format!("launch.{why}")));
    }
    let file = crate::domain_schedule_runner::options_at(home).map_err(|error| invalid(format!("options.toml: {error}")))?;
    let requested = match crate::domain_orchestrator_tools::request_choices(domain, request_id).ok().flatten() {
        Some(choices) => choices.options,
        None => {
            let project = if require_directory { crate::options::project_of(&file, cwd) }
                else { plan.project.clone().or_else(|| crate::options::project_of(&file, cwd)) };
            file.orchestrator_defaults(project.as_deref(), None)
        }
    };
    let auto = requested.effort == alondra_core::options::AUTO;
    let given = |text: &Option<String>| text.as_deref().is_some_and(|text| !text.trim().is_empty());
    let bare_model = plan.model.as_deref().filter(|model| !model.starts_with("codex/") && !model.starts_with("claude/"));
    let offering = bare_model.map(|model| crate::domain_orchestrator_tools::providers_offering(domain, &file.accounts, model)).unwrap_or_default();
    let pinned = requested.selection.as_ref().is_some_and(|intent| intent.requested.is_some() || intent.account.is_some());
    if let Some(model) = bare_model.filter(|_| offering.len() > 1 && !pinned) {
        return Err(crate::domain_orchestrator_tools::ambiguous_model(model, &offering));
    }
    if requested.selection.is_some() || plan.model.as_deref().is_some_and(|model| model.starts_with("codex/") || model.starts_with("claude/")) {
        use alondra_core::model_selection::{ModelKey, ModelProfile, TaskSelection};
        let mut native = requested.clone();
        let mut intent = native.selection.take().unwrap_or_else(|| TaskSelection::automatic(ModelProfile::Implementation));
        if let Some(model) = &plan.model {
            let model = if model.starts_with("codex/") || model.starts_with("claude/") { ModelKey::parse_wire(model).map_err(&invalid)? }
                else {
                    let provider = intent.requested.as_ref().map(|model| model.provider).or_else(|| intent.account.as_ref().map(|account| account.provider))
                        .or_else(|| (offering.len() == 1).then(|| offering[0]))
                        .or_else(|| (file.accounts.connected_providers().len() == 1).then(|| file.accounts.connected_providers()[0]))
                        .ok_or_else(|| crate::domain_orchestrator_tools::ambiguous_model(model, &offering))?;
                    ModelKey { provider, model: if provider == alondra_core::accounts::Provider::Claude { file.model_flag(model).unwrap_or_else(|| model.clone()) } else { model.clone() } }
                };
            if intent.requested.as_ref().is_some_and(|requested| requested != &model) { return Err(invalid("The user-selected model is binding".into())); }
            intent.requested = Some(model);
        }
        if let Some(effort) = &plan.effort {
            if intent.requested_reasoning_effort.as_ref().is_some_and(|requested| requested != effort) { return Err(invalid("The user-selected reasoning effort is binding".into())); }
            intent.requested_reasoning_effort = Some(effort.clone());
        }
        native.selection = Some(intent);
        crate::domain_orchestrator_tools::validate_request_selection(domain, &native)?;
        let tools = domain.schedule_request_tools(request_id).map_err(|why| invalid(why.to_string()))?.unwrap_or_default();
        crate::domain_orchestrator_tools::validate_native_requirements(native.selection.as_ref(), &tools)?;
        return Ok(());
    }
    if auto && !(given(&plan.model) && given(&plan.reason)) {
        return Err(invalid("effort is auto for this request: the launch plan needs model and reason".into()));
    }
    requested.with_orchestrator_model(&file, plan.model.as_deref(), plan.effort.as_deref()).map_err(invalid)?;
    Ok(())
}

/// Sets the project a new task belongs to: the one the user asked for with its request, else the
/// folder under the projects root it works in. Whatever its creator sent is replaced.
pub fn stamp_project(domain: &DomainRuntime, home: &Path, command: &mut Command) {
    let (request_id, plan) = match command {
        Command::CreateTask { request_id, launch: Some(plan), .. }
        | Command::SettleSuggestion { decision: SuggestionDecision::NewTask { request_id, launch: plan, .. }, .. } => (request_id, plan),
        _ => return,
    };
    let chosen = domain.request_launch(request_id).ok().flatten().and_then(|launch| launch.project);
    plan.project = chosen.or_else(|| {
        let file = crate::domain_schedule_runner::options_at(home).unwrap_or_default();
        crate::options::project_of(&file, Path::new(&plan.cwd))
    });
}
