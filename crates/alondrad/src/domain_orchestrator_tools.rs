//! What the orchestrator does outside the domain, as 8ccd131's orchestrator tools did: launch an
//! Agent for a Task, type a message into an Agent's session, press Esc in it and read the
//! machine. Each call carries the orchestrator execution's bearer; the domain changes a call
//! implies (the new Agent, an assignment, a reopened Task) are that orchestrator's commands.
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::model_metrics::{Decision, DecisionKind};
use alondra_core::domain::{Actor, AgentId, Command, CommandEnvelope, DomainError, DomainState, ModelChooser, RequestId, TaskId, PROTOCOL_VERSION};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

/// The orchestrator's starting model, for a relaunch of a task that never recorded one.
const DEFAULT_MODEL: &str = "sonnet";

/// What the user chose when asking for a Request: its project, options, issue and files.
pub(crate) type Choices = crate::domain_store::RequestLaunch;

pub(crate) fn request_choices(domain: &DomainRuntime, request: &RequestId) -> anyhow::Result<Option<Choices>> {
    domain.request_launch(request)
}

/// 8ccd131's request text for the orchestrator: the options the user chose, the effort rule, the
/// issue the work comes from and where the attached files are.
pub(crate) fn choices_text(user: &str, home: &Path, choices: &Choices) -> String {
    let mut lines = vec![format!("  options: {}. {}", choices.options.summary(), choices.options.effort_note(user))];
    if let Some(issue) = &choices.issue {
        lines.push(format!("  issue {issue}{}", choices.issue_text.as_deref().map(|text| format!(": {text}")).unwrap_or_default()));
    }
    for attachment in &choices.attachments {
        lines.push(format!("  attached {}: {}", attachment.name, home.join(&attachment.content_reference).display()));
    }
    if !choices.attachments.is_empty() {
        lines.push("  Put the attached files' paths in the mission of the Task that needs them so its Agent can open them.".into());
    }
    lines.join("\n")
}

/// The orchestrator a local bearer belongs to, while its generation is current.
pub(crate) fn orchestrator(domain: &DomainRuntime, bearer: Option<&str>) -> Result<Actor, DomainError> {
    let refused = || DomainError::new("forbidden", "the orchestrator's capability is required");
    let principal = domain.resolve_bearer(bearer.ok_or_else(refused)?).map_err(|_| refused())?;
    match principal {
        crate::domain_authority::Principal::Orchestrator { generation, .. } => Ok(Actor::Orchestrator { generation }),
        _ => Err(refused()),
    }
}

fn run(domain: &DomainRuntime, actor: &Actor, command: Command) -> Result<(), DomainError> {
    let state = domain.state().map_err(|_| DomainError::new("storage_unavailable", "domain state unavailable"))?;
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch,
        command_id: format!("orchestrator-tool-{}", crate::auth::random_hex(16)).into(),
        expected_revision: state.revision, command,
    };
    domain.execute(actor, &envelope, None, || Ok(())).map(|_| ()).map_err(|error| match error {
        crate::domain_store::StoreError::Domain(error) => error,
        crate::domain_store::StoreError::Storage(_) => DomainError::new("storage_unavailable", "domain transaction could not be completed"),
    })
}

fn state(domain: &DomainRuntime) -> Result<DomainState, DomainError> {
    domain.state().map_err(|_| DomainError::new("storage_unavailable", "domain state unavailable"))
}

/// An Agent by its id or its name.
fn agent(state: &DomainState, node: &str) -> Result<AgentId, DomainError> {
    let found: Vec<&AgentId> = state.agents.values().filter(|a| a.id.0 == node || a.name == node).map(|a| &a.id).collect();
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(DomainError::new("not_found", format!("no agent is called {node}"))),
        _ => Err(DomainError::new("conflict", format!("{node} names several agents; use its id"))),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LaunchBody {
    name: String,
    cwd: String,
    task: TaskId,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    approach: Option<alondra_core::options::Approach>,
    /// What the task is and why this model: required when the orchestrator chooses the model.
    #[serde(default)]
    reason: Option<String>,
    /// What a relaunch passes when the orchestrator chose the model: the one the task had.
    #[serde(skip)]
    relaunch_model: Option<String>,
    /// Orchestrators written when going over pace needed the user's word still send it.
    #[serde(default, rename = "over_pace_ok")]
    _over_pace_ok: bool,
}

/// Launches a Claude session for a Task with the options its Request was asked for, gives it its
/// bearer through its settings file and assigns it the Task.
pub(crate) async fn launch(domain: &DomainRuntime, home: &Path, actor: &Actor, body: LaunchBody,
    choices: impl FnOnce(&RequestId) -> Option<Choices>, launcher: &impl crate::orchestrate::Launcher) -> Result<Value, DomainError> {
    let conflict = |message: String| DomainError::new("conflict", message);
    let current = state(domain)?;
    let task = current.tasks.get(&body.task).ok_or_else(|| DomainError::new("not_found", "task does not exist"))?;
    if task.state.terminal() {
        return Err(conflict(format!("{} is closed; reopen it first", body.task)));
    }
    if task.agent_id.as_ref().and_then(|id| current.agents.get(id)).is_some_and(|a| a.connected) {
        return Err(conflict(format!("{} already has a connected agent; send it what to do", body.task)));
    }
    if let Some(exhausted) = domain.live.usage_pause.current() {
        let why = if exhausted.by_user { "The user paused Claude work" } else { "Claude usage ran out on this Mac" };
        return Err(DomainError::new("usage_exhausted", format!(
            "{why} until {}: {} stays as it is and can be launched after that",
            crate::usage_pause::clock_text(exhausted.until, alondra_core::now_millis(), false), body.task)));
    }
    if !Path::new(&body.cwd).is_dir() {
        return Err(DomainError::new("invalid_input", format!("directory {} does not exist", body.cwd)));
    }
    if let Some(repository) = crate::claude_trust::check(Path::new(&body.cwd)) {
        return Err(DomainError::new("workspace_untrusted", crate::claude_trust::instructions(&repository)));
    }
    let Some(plugin) = launcher.plugin() else {
        return Err(DomainError::new("unavailable", "Alondra's plugin was not found: run `alondra setup` in the Alondra repo"));
    };
    let file = crate::domain_schedule_runner::options_at(home).map_err(|error| DomainError::new("invalid_input", format!("options.toml: {error}")))?;
    let request = current.requests.get(&task.request_id);
    let choices = choices(&task.request_id);
    let requested = match &choices {
        Some(choices) => choices.options.clone(),
        None => file.orchestrator_defaults(crate::options::project_of(&file, Path::new(&body.cwd)).as_deref(), None),
    };
    let auto = requested.effort == alondra_core::options::AUTO;
    let model = body.model.as_deref().or(body.relaunch_model.as_deref().filter(|_| auto));
    let resolved = requested.with_orchestrator_model(&file, model, body.effort.as_deref())
        .map_err(|reason| DomainError::new("invalid_input", reason))?;
    let reason = given(body.reason.as_deref());
    if auto && body.relaunch_model.is_none() && reason.is_none() {
        return Err(DomainError::new("invalid_input", "effort is auto for this request: pass reason, one short line on what the task is and why this model fits it"));
    }
    let mission = body.prompt.clone().unwrap_or_else(|| {
        let mut text = format!("{}\n\n{}", task.title, request.map(|r| r.body.as_str()).unwrap_or_default());
        if let Some(choices) = &choices {
            if let Some(issue) = choices.issue_text.as_deref() {
                text.push_str(&format!("\n\n{issue}"));
            }
            for attachment in &choices.attachments {
                text.push_str(&format!("\n\nAttached {}: {}", attachment.name, home.join(&attachment.content_reference).display()));
            }
        }
        text
    });
    let prompt = crate::domain_schedule_runner::producer_prompt(&mission, &resolved, &task.id, body.approach.unwrap_or_default());
    let key = format!("launch-{}", crate::auth::random_hex(8));
    let settings = crate::domain_sessions::prepare(home, &key)
        .map_err(|error| DomainError::new("storage_unavailable", format!("could not write the session's settings: {error}")))?;
    let mut args: Vec<String> = ["--bg", "--name", &body.name, "--permission-mode", "auto"].map(String::from).into();
    args.extend(resolved.launch_flags(None));
    args.extend(crate::domain_sessions::flags(&[plugin.as_path()], &settings));
    let denied = domain.schedule_request_tools(&task.request_id).ok().flatten().unwrap_or_default();
    if !denied.is_empty() {
        args.extend(["--disallowedTools".into(), denied.join(" ")]);
    }
    args.push("--".into());
    args.push(prompt);
    let discard = |error: DomainError| {
        crate::domain_sessions::release(&settings);
        error
    };
    let output = launcher.run(&body.cwd, &args).await.map_err(|(_, reason)| discard(DomainError::new("launch_failed", reason)))?;
    let Some(session) = domain.live.mods.wait_launched(&key, launcher.mods_deadline()).await else {
        if let Some(short) = alondra_core::sessions::launched_short(&output) {
            launcher.stop(&short).await;
        }
        return Err(discard(DomainError::new("launch_failed", crate::node_reports::MODS_NOT_LOADED)));
    };
    let agent_id = AgentId(session.clone());
    let name = body.title.clone().unwrap_or_else(|| body.name.clone());
    match run(domain, &Actor::System, Command::RegisterAgent { agent_id: agent_id.clone(), name }) {
        Ok(()) => (),
        Err(error) if error.code == "invalid_input" => (),
        Err(error) => return Err(discard(error)),
    }
    let bound = domain.bind_agent(&session).map_err(anyhow::Error::from)
        .and_then(|capability| crate::domain_sessions::bind(&settings, &agent_id, capability.expose_for_child_environment()));
    if let Err(error) = bound {
        let _ = domain.revoke_agent(&session);
        return Err(discard(DomainError::new("storage_unavailable", format!("could not issue the session's capability: {error}"))));
    }
    if let Err(error) = run(domain, actor, Command::AssignTask { task_id: task.id.clone(), agent_id: agent_id.clone(), model: resolved.launch_model() }) {
        let _ = domain.revoke_agent(&session);
        return Err(discard(error));
    }
    record(domain, Decision {
        task_id: task.id.0.clone(), request_id: task.request_id.0.clone(), agent_id: Some(session.clone()), at: alondra_core::now_millis(),
        kind: if body.relaunch_model.is_some() { DecisionKind::Relaunch } else { DecisionKind::Launch },
        model: resolved.flags.model.as_deref().map(alondra_core::model_label::model_family),
        effort: resolved.flags.effort.clone(),
        approach: Some(body.approach.unwrap_or_default()),
        chosen_by: resolved.launch_model().map_or_else(|| chooser(auto), |launched| launched.chosen_by),
        reason, title: task.title.clone(),
    });
    domain.live.forget_closed_task(&task.id.0);
    crate::usage_pause::raise_over_pace_card(domain, alondra_core::now_millis());
    Ok(json!({"agent_id": agent_id, "short_id": session.chars().take(8).collect::<String>(), "task": task.id}))
}

/// A new node for a task whose node the user closed, launched with the task's mission in the
/// folder the closed node worked in, else the project its request was asked for.
pub(crate) async fn relaunch(domain: &DomainRuntime, home: &Path, actor: &Actor, task_id: &TaskId, launcher: &impl crate::orchestrate::Launcher) -> Result<Value, DomainError> {
    let current = state(domain)?;
    let waiting = current.tasks.get(task_id)
        .filter(|task| task.state == alondra_core::domain::TaskState::NotStarted && task.agent_id.is_none() && !task.archived);
    let (Some(task), Some(closed)) = (waiting, domain.live.closed_task(&task_id.0)) else {
        return Err(DomainError::new("conflict", "This task is not waiting for a relaunch"));
    };
    let asked = request_choices(domain, &task.request_id).ok().flatten();
    let root = crate::domain_schedule_runner::options_at(home).ok().map(|file| file.projects_root(&crate::options::user_home()));
    let project_dir = asked.as_ref().and_then(|choices| choices.project.as_ref()).zip(root.as_ref()).map(|(project, root)| root.join(project));
    let cwd = closed.cwd.map(std::path::PathBuf::from).filter(|dir| dir.is_dir())
        .or(project_dir.filter(|dir| dir.is_dir()))
        .or(root)
        .and_then(|dir| dir.to_str().map(str::to_owned))
        .ok_or_else(|| DomainError::new("invalid_input", "no folder to launch the node in"))?;
    let mut body: LaunchBody = serde_json::from_value(json!({"name": closed.name, "cwd": cwd, "task": task_id}))
        .map_err(|error| DomainError::new("invalid_input", error.to_string()))?;
    body.relaunch_model = Some(task.model.as_ref().map_or(DEFAULT_MODEL, |m| m.model.as_str()).to_owned());
    launch(domain, home, actor, body, |request| request_choices(domain, request).ok().flatten(), launcher).await
}

fn given(text: Option<&str>) -> Option<String> {
    text.map(str::trim).filter(|text| !text.is_empty()).map(str::to_owned)
}

fn chooser(auto: bool) -> ModelChooser {
    if auto { ModelChooser::Orchestrator } else { ModelChooser::User }
}

/// The decision is evidence for tuning the model policy; failing to keep it never undoes the work.
fn record(domain: &DomainRuntime, decision: Decision) {
    if let Err(error) = domain.record_model_decision(&decision) {
        log!("could not record the model decision for {}: {error:#}", decision.task_id);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SendBody {
    node: String,
    text: String,
    #[serde(default)]
    task: Option<TaskId>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    approach: Option<alondra_core::options::Approach>,
    #[serde(default)]
    reason: Option<String>,
}

/// Sends the orchestrator's text to an Agent's session. With a Task, the Agent is told which
/// one; a Task nobody works on becomes this Agent's, and a closed one is reopened. An Agent works
/// on one Task at a time, so a Task for an Agent busy with another is refused.
pub(crate) async fn send(domain: &DomainRuntime, home: &Path, actor: &Actor, body: SendBody) -> Result<Value, DomainError> {
    let current = state(domain)?;
    let agent_id = agent(&current, &body.node)?;
    let node_runs = domain.live.mods.session(&agent_id.0).and_then(|reported| reported.model);
    let node_model = node_runs.as_deref().map(alondra_core::model_label::model_family);
    if let Some(task_id) = &body.task {
        let task = current.tasks.get(task_id).ok_or_else(|| DomainError::new("not_found", "task does not exist"))?;
        if let Some(wanted) = &body.model {
            let file = crate::domain_schedule_runner::options_at(home).map_err(|error| DomainError::new("invalid_input", format!("options.toml: {error}")))?;
            let family = file.model_family(wanted).ok_or_else(|| DomainError::new("invalid_input", format!("{wanted} is not a model; use one of {}", file.model_aliases().join(", "))))?;
            if node_model.as_deref() != Some(family.as_str()) {
                let has = node_runs.as_deref().map_or("an unknown model".to_owned(), alondra_core::model_label::launched_model_label);
                return Err(DomainError::new("model_mismatch", format!("{} runs {has}, not {wanted}: launch a new node for {task_id}", body.node)));
            }
        }
        if let Some(other) = current.busy_with(&agent_id, task_id) {
            return Err(DomainError::new("agent_busy", format!("{} works on {} until it ends: launch a new node for {task_id}, and tell it what {} is doing if they touch the same thing", body.node, other.id.0, body.node)));
        }
        match &task.agent_id {
            None => run(domain, actor, Command::AssignTask { task_id: task_id.clone(), agent_id: agent_id.clone(), model: None })?,
            Some(other) if other != &agent_id => return Err(DomainError::new("conflict", format!("{task_id} belongs to another agent"))),
            Some(_) => (),
        }
        if task.state.terminal() {
            run(domain, actor, Command::ReopenTask { task_id: task_id.clone() })?;
        }
    }
    let text = match body.approach {
        Some(approach) => format!("{}\n\n{}", body.text.trim_end(), approach.how_to_work().join("\n")),
        None => body.text,
    };
    let message = serde_json::from_value(json!({"text": text, "task_id": body.task}))
        .map_err(|error| DomainError::new("invalid_input", error.to_string()))?;
    let delivered = crate::domain_messages::deliver(domain, home, actor, "The orchestrator", &agent_id, message, false).await?;
    if let Some(task) = body.task.as_ref().and_then(|id| current.tasks.get(id))
        && (body.model.is_some() || body.approach.is_some())
    {
        let auto = request_choices(domain, &task.request_id).ok().flatten().is_none_or(|choices| choices.options.effort == alondra_core::options::AUTO);
        record(domain, Decision {
            task_id: task.id.0.clone(), request_id: task.request_id.0.clone(), agent_id: Some(agent_id.0.clone()), at: alondra_core::now_millis(),
            kind: DecisionKind::Send, model: node_model, effort: None, approach: body.approach, chosen_by: chooser(auto),
            reason: given(body.reason.as_deref()), title: task.title.clone(),
        });
    }
    Ok(delivered)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NodeBody {
    node: String,
}

/// Aborts the turn an Agent's session is running, to stop a call it is stuck in; its mod does it.
pub(crate) fn interrupt(domain: &DomainRuntime, body: NodeBody) -> Result<Value, DomainError> {
    let current = state(domain)?;
    let agent_id = agent(&current, &body.node)?;
    domain.live.pushes.interrupt(&agent_id.0)
        .map_err(|_| DomainError::new("session_unreachable", "the node is not connected to Alondra: its session is stopped or its Claude Code mod is not running. Nothing was sent"))?;
    Ok(json!({"interrupted": agent_id.0}))
}

#[cfg(test)]
mod tests;
