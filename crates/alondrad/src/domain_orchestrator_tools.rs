//! What the orchestrator does outside the domain, as 8ccd131's orchestrator tools did: launch an
//! Agent for a Task, type a message into an Agent's session, press Esc in it and read the
//! machine. Each call carries the orchestrator execution's bearer; the domain changes a call
//! implies (the new Agent, an assignment, a reopened Task) are that orchestrator's commands.
use crate::domain_messages::{Session, Sessions};
use crate::domain_runtime::DomainRuntime;
use crate::domain_transport::Terminal;
use alondra_core::domain_v2::{Actor, AgentId, AttachmentRef, Command, CommandEnvelope, DomainError, DomainState, RequestId, TaskId, PROTOCOL_VERSION};
use alondra_core::options::Resolved;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

/// What the user chose when asking for a Request: its project, options, issue and files.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Choices {
    pub project: Option<String>,
    pub options: Resolved,
    pub issue: Option<String>,
    pub issue_text: Option<String>,
    pub attachments: Vec<AttachmentRef>,
}

/// The choices recorded with a Request. Requests carry none until the new-request route stores
/// them (#461), so every Request falls back to the options file's defaults for now.
pub(crate) fn request_choices(domain: &DomainRuntime, request: &RequestId) -> anyhow::Result<Option<Choices>> {
    let _ = (domain, request);
    Ok(None)
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
    over_pace_ok: bool,
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
    if let Some(reason) = crate::http::launch_blocked(launcher.usage().as_ref(), body.over_pace_ok) {
        return Err(conflict(reason));
    }
    if !Path::new(&body.cwd).is_dir() {
        return Err(DomainError::new("invalid_input", format!("directory {} does not exist", body.cwd)));
    }
    let Some(plugin) = launcher.plugin() else {
        return Err(DomainError::new("unavailable", "Alondra's plugin was not found: run `alondra setup` in the Alondra repo"));
    };
    let file = crate::domain_schedule_runner::options_at(home).map_err(|error| DomainError::new("invalid_input", format!("options.toml: {error}")))?;
    let request = current.requests.get(&task.request_id);
    let choices = choices(&task.request_id);
    let resolved = match &choices {
        Some(choices) => choices.options.clone().with_orchestrator_effort(&file, body.effort.as_deref()),
        None => file.orchestrator_defaults(crate::options::project_of(&file, Path::new(&body.cwd)).as_deref(), body.effort.as_deref()),
    };
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
    let prompt = crate::domain_schedule_runner::producer_prompt(&mission, &resolved,
        &crate::domain_schedule_runner::ProducerLaunch { task_id: &task.id, request_id: &task.request_id, state: &current });
    let settings = crate::domain_sessions::prepare(home, &format!("launch-{}", crate::auth::random_hex(8)), &[plugin.as_path()])
        .map_err(|error| DomainError::new("storage_unavailable", format!("could not write the session's settings: {error}")))?;
    let mut args: Vec<String> = ["--bg", "--name", &body.name, "--permission-mode", "auto"].map(String::from).into();
    args.extend(resolved.launch_flags(body.model.as_deref()));
    args.extend(crate::domain_sessions::flags(&[plugin.as_path()], &settings));
    let denied = domain.schedule_request_tools(&task.request_id).ok().flatten().unwrap_or_default();
    if !denied.is_empty() {
        args.extend(["--disallowedTools".into(), denied.join(" ")]);
    }
    args.push("--".into());
    args.push(prompt);
    let discard = |error: DomainError| {
        crate::domain_sessions::remove(&settings);
        error
    };
    let output = launcher.run(&body.cwd, &args).await.map_err(|(_, reason)| discard(DomainError::new("launch_failed", reason)))?;
    let short = alondra_core::sessions::launched_short(&output);
    let session = match &short {
        Some(short) => launcher.session_of(short).await,
        None => None,
    };
    let session = session.ok_or_else(|| discard(DomainError::new("launch_failed", "Claude did not list the launched session")))?;
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
    if let Err(error) = run(domain, actor, Command::AssignTask { task_id: task.id.clone(), agent_id: agent_id.clone() }) {
        let _ = domain.revoke_agent(&session);
        return Err(discard(error));
    }
    Ok(json!({"agent_id": agent_id, "short_id": short, "task": task.id}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SendBody {
    node: String,
    text: String,
    #[serde(default)]
    task: Option<TaskId>,
}

/// Types the orchestrator's text into an Agent's session. With a Task, the Agent is told which
/// one; a Task nobody works on becomes this Agent's, and a closed one is reopened.
pub(crate) async fn send(domain: &DomainRuntime, home: &Path, actor: &Actor, body: SendBody,
    sessions: &impl Sessions, terminal: &Arc<impl Terminal>) -> Result<Value, DomainError> {
    let current = state(domain)?;
    let agent_id = agent(&current, &body.node)?;
    if let Some(task_id) = &body.task {
        let task = current.tasks.get(task_id).ok_or_else(|| DomainError::new("not_found", "task does not exist"))?;
        match &task.agent_id {
            None => run(domain, actor, Command::AssignTask { task_id: task_id.clone(), agent_id: agent_id.clone() })?,
            Some(other) if other != &agent_id => return Err(DomainError::new("conflict", format!("{task_id} belongs to another agent"))),
            Some(_) => (),
        }
        if task.state.terminal() {
            run(domain, actor, Command::ReopenTask { task_id: task_id.clone() })?;
        }
    }
    let message = serde_json::from_value(json!({"text": body.text, "task_id": body.task}))
        .map_err(|error| DomainError::new("invalid_input", error.to_string()))?;
    crate::domain_messages::deliver(domain, home, actor, "The orchestrator", &agent_id, message, sessions, terminal).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NodeBody {
    node: String,
}

/// Presses Esc in an Agent's background session, to stop a call it is stuck in.
pub(crate) async fn interrupt(domain: &DomainRuntime, body: NodeBody, sessions: &impl Sessions,
    escape: impl FnOnce(&str) -> Result<(), String> + Send + 'static) -> Result<Value, DomainError> {
    let current = state(domain)?;
    let agent_id = agent(&current, &body.node)?;
    let short = match sessions.find(&agent_id.0).await {
        Some(Session { interactive: true, .. }) => {
            return Err(DomainError::new("interactive_session", "this agent runs in the user's terminal; Alondra cannot press keys there"));
        }
        Some(session) => session.short,
        None => crate::domain_transport::terminals(&current).remove(&agent_id)
            .ok_or_else(|| DomainError::new("session_unreachable", "no background session is known for this agent"))?,
    };
    let target = short.clone();
    match tokio::task::spawn_blocking(move || escape(&target)).await {
        Ok(Ok(())) => Ok(json!({"interrupted": short})),
        Ok(Err(reason)) => Err(DomainError::new("session_unreachable", reason)),
        Err(_) => Err(DomainError::new("session_unreachable", "the interrupt did not finish")),
    }
}

#[cfg(test)]
mod tests;
