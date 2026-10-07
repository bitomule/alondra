//! What the orchestrator does outside the domain, as 8ccd131's orchestrator tools did: launch an
//! Agent for a Task, type a message into an Agent's session, press Esc in it and read the
//! machine. Each call carries the orchestrator execution's bearer; the domain changes a call
//! implies (the new Agent, an assignment, a reopened Task) are that orchestrator's commands.
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::model_metrics::{Decision, DecisionKind};
use alondra_core::domain::{Actor, AgentId, Command, CommandEnvelope, DomainError, DomainState, ModelChooser, RequestId, TaskId, PROTOCOL_VERSION};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
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
    /// Stops the task's current node and launches this one in its folder, keeping its files.
    #[serde(default)]
    replace: bool,
    /// Orchestrators written when going over pace needed the user's word still send it.
    #[serde(default, rename = "over_pace_ok")]
    _over_pace_ok: bool,
}

impl LaunchBody {
    /// The launch a stored plan describes.
    pub(crate) fn from_plan(task: TaskId, plan: &crate::launch_plans::LaunchPlan) -> LaunchBody {
        LaunchBody {
            name: Some(plan.name.clone()),
            cwd: Some(plan.cwd.clone()),
            task,
            prompt: plan.mission.clone(),
            title: None,
            model: plan.model.clone(),
            effort: plan.effort.clone(),
            approach: plan.approach,
            reason: plan.reason.clone(),
            relaunch_model: None,
            replace: false,
            _over_pace_ok: false,
        }
    }

    /// What the task's stored plan fills in where the call named nothing.
    fn with_plan(mut self, plan: Option<&crate::launch_plans::LaunchPlan>) -> LaunchBody {
        let Some(plan) = plan else { return self };
        self.name = self.name.or_else(|| Some(plan.name.clone()));
        self.cwd = self.cwd.or_else(|| Some(plan.cwd.clone()));
        self.prompt = self.prompt.or_else(|| plan.mission.clone());
        self.approach = self.approach.or(plan.approach);
        if self.relaunch_model.is_none() {
            self.model = self.model.or_else(|| plan.model.clone());
            self.effort = self.effort.or_else(|| plan.effort.clone());
            self.reason = self.reason.or_else(|| plan.reason.clone());
        }
        self
    }
}

/// Who starts a launch: the orchestrator, or Alondra itself for a task whose time has come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    Orchestrator,
    Auto,
}

/// Launches a Claude session for a Task with the options its Request was asked for, gives it its
/// bearer through its settings file and assigns it the Task.
pub(crate) async fn launch(domain: &DomainRuntime, home: &Path, actor: &Actor, body: LaunchBody,
    choices: impl FnOnce(&RequestId) -> Option<Choices>, launcher: &impl crate::orchestrate::Launcher) -> Result<Value, DomainError> {
    launch_from(domain, home, actor, body, choices, launcher, Origin::Orchestrator).await
}

/// `launch`, for either origin. An automatic launch holds the task's launch guard already.
pub(crate) async fn launch_from(domain: &DomainRuntime, home: &Path, actor: &Actor, body: LaunchBody,
    choices: impl FnOnce(&RequestId) -> Option<Choices>, launcher: &impl crate::orchestrate::Launcher, origin: Origin) -> Result<Value, DomainError> {
    if origin == Origin::Orchestrator {
        let _launching = Launching::begin(domain, &body.task)?;
        return launch_guarded(domain, home, actor, body, choices, launcher, origin, None).await;
    }
    launch_guarded(domain, home, actor, body, choices, launcher, origin, None).await
}

/// The right to launch a task's node, given back however the launch ends, a panic included.
struct Launching<'a> {
    domain: &'a DomainRuntime,
    task: String,
}

impl<'a> Launching<'a> {
    fn begin(domain: &'a DomainRuntime, task: &TaskId) -> Result<Self, DomainError> {
        if !domain.live.begin_launch(&task.0) {
            return Err(DomainError::new("conflict", format!("{task} is being launched already")));
        }
        Ok(Self { domain, task: task.0.clone() })
    }
}

impl Drop for Launching<'_> {
    fn drop(&mut self) {
        self.domain.live.end_launch(&self.task);
    }
}

/// The orchestrator's `launch` with replace: it answers once the launch is checked and the old
/// node is about to be stopped, because waiting for that node's turn to end can take minutes.
/// The replacement and the message that tells the orchestrator how it ended run in a task of
/// their own, which neither a dropped call nor a panic in the replacement leaves untold.
pub(crate) async fn launch_replacing<L: crate::orchestrate::Launcher + Send + 'static>(domain: Arc<DomainRuntime>, home: PathBuf, actor: Actor,
    body: LaunchBody, choices: impl FnOnce(&RequestId) -> Option<Choices> + Send + 'static, launcher: Arc<L>) -> Result<Value, DomainError> {
    if !body.replace {
        return launch(&domain, &home, &actor, body, choices, &*launcher).await;
    }
    let task = body.task.clone();
    let (checked_tx, checked) = tokio::sync::oneshot::channel();
    let worker = tokio::spawn({
        let (domain, task) = (domain.clone(), task.clone());
        async move {
            let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let replacement = tokio::spawn({
                let (domain, task, reached) = (domain.clone(), task.clone(), reached.clone());
                async move {
                    let _launching = Launching::begin(&domain, &task)?;
                    let on_checked: Box<dyn FnOnce() + Send> = Box::new(move || {
                        reached.store(true, std::sync::atomic::Ordering::SeqCst);
                        let _ = checked_tx.send(());
                    });
                    launch_guarded(&domain, &home, &actor, body, choices, &*launcher, Origin::Orchestrator, Some(on_checked)).await
                }
            });
            let outcome = replacement.await.unwrap_or_else(|failure| Err(DomainError::new("launch_failed", format!(
                "the replacement stopped unexpectedly ({failure}); check the task with fleet before launching it again"))));
            if reached.load(std::sync::atomic::Ordering::SeqCst) {
                tell_replaced(&domain, &task, &outcome);
            }
            outcome
        }
    });
    if checked.await.is_err() {
        return worker.await.unwrap_or_else(|failure| Err(DomainError::new("launch_failed", format!("the launch stopped unexpectedly: {failure}"))));
    }
    Ok(json!({"task": task, "replacing": true, "message": format!(
        "Replacing the node of “{}”: Alondra waits for its turn to end (interrupting it if it takes too long), stops it and \
         starts the new node in its folder. This can take a few minutes; you are told when the new node works on the task, or \
         why it did not start.", task_title(&domain, &task))}))
}

fn task_title(domain: &DomainRuntime, task: &TaskId) -> String {
    domain.state().ok().and_then(|state| state.tasks.get(task).map(|task| task.title.clone())).unwrap_or_else(|| task.0.clone())
}

fn tell_replaced(domain: &DomainRuntime, task: &TaskId, outcome: &Result<Value, DomainError>) {
    let title = task_title(domain, task);
    let text = match outcome {
        Ok(answer) => format!("“{title}” ({task}) has its new node, {}: it continues the work of the node it replaced.",
            answer["short_id"].as_str().unwrap_or_default()),
        Err(error) => format!("The replacement of the node of “{title}” ({task}) did not finish: {}", error.message),
    };
    if let Err(error) = domain.tell_orchestrator(&format!("replaced-{task}-{}", crate::auth::random_hex(4)), text) {
        log!("could not tell the orchestrator how the replacement of {task}'s node ended: {error:?}");
    }
}

#[allow(clippy::too_many_arguments)]
async fn launch_guarded(domain: &DomainRuntime, home: &Path, actor: &Actor, body: LaunchBody,
    choices: impl FnOnce(&RequestId) -> Option<Choices>, launcher: &impl crate::orchestrate::Launcher, origin: Origin,
    checked: Option<Box<dyn FnOnce() + Send>>) -> Result<Value, DomainError> {
    let conflict = |message: String| DomainError::new("conflict", message);
    let current = state(domain)?;
    let task = current.tasks.get(&body.task).ok_or_else(|| DomainError::new("not_found", "task does not exist"))?;
    let mut assigned_before = task.agent_id.clone();
    let replaced = match (&assigned_before, body.replace) {
        (Some(old), true) => Some(old.clone()),
        (None, true) => return Err(DomainError::new("invalid_input", format!("{} has no node to replace: launch it without replace", body.task))),
        (_, false) => None,
    };
    let mut body = body;
    let mut inherited = None;
    if let Some(old) = &replaced {
        inherited = owned_worktree(domain, &old.0).filter(|dir| body.cwd.as_ref().is_none_or(|cwd| cwd == dir));
        body.cwd = body.cwd.or_else(|| node_folder(domain, &old.0));
        body.name = body.name.or_else(|| current.agents.get(old).map(|agent| agent.name.clone()));
    }
    let plan = domain.launch_plan(&task.id).map_err(|_| DomainError::new("storage_unavailable", "domain state unavailable"))?;
    let body = body.with_plan(plan.as_ref());
    let (Some(name), Some(cwd)) = (body.name.clone(), body.cwd.clone()) else {
        return Err(DomainError::new("invalid_input", "name and cwd are required: this task has no stored launch plan"));
    };
    if task.state.terminal() {
        return Err(conflict(format!("{} is closed; reopen it first", body.task)));
    }
    if replaced.is_none() && task.agent_id.as_ref().and_then(|id| current.agents.get(id)).is_some_and(|a| a.connected) {
        return Err(conflict(format!("{} already has a connected agent; send it what to do", body.task)));
    }
    if let Some(exhausted) = domain.live.usage_pause.current() {
        let why = if exhausted.by_user { "The user paused Claude work" } else { "Claude usage ran out on this Mac" };
        return Err(DomainError::new("usage_exhausted", format!(
            "{why} until {}: {} stays as it is and can be launched after that",
            crate::usage_pause::clock_text(exhausted.until, alondra_core::now_millis(), false), body.task)));
    }
    if !Path::new(&cwd).is_dir() {
        return Err(DomainError::new("invalid_input", format!("directory {cwd} does not exist")));
    }
    if let Some(repository) = crate::claude_trust::check(Path::new(&cwd)) {
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
        None => file.orchestrator_defaults(crate::options::project_of(&file, Path::new(&cwd)).as_deref(), None),
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
    let mission = format!("{mission}{}{}", prefixed("\n\n", follow_up_context(&current, task)), prefixed("\n\n", asked_changes(&current, task)));
    let mission = match &replaced {
        Some(_) => format!("{mission}\n\n{}", continuing_in(Path::new(&cwd)).await),
        None => mission,
    };
    let prompt = crate::domain_schedule_runner::producer_prompt(&mission, &resolved, &task.id, body.approach.unwrap_or_default());
    let key = format!("launch-{}", crate::auth::random_hex(8));
    let settings = crate::domain_sessions::prepare(home, &key)
        .map_err(|error| DomainError::new("storage_unavailable", format!("could not write the session's settings: {error}")))?;
    let mut args: Vec<String> = ["--bg", "--name", &name, "--permission-mode", "auto"].map(String::from).into();
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
    if let Some(checked) = checked {
        checked();
    }
    if let Some(old) = &replaced {
        stop_replaced(domain, old, launcher).await.map_err(|reason| discard(DomainError::new("replace_failed", format!(
            "Alondra could not stop the node of {}, so no new node was started: {reason}. It is still assigned to that node; \
             if that node ends later, the task is freed and you are told.", task.id))))?;
        crate::node_revival::take_out(domain, &old.0, "node replaced by the orchestrator", Some(cwd.clone()))
            .map_err(|error| discard(DomainError::new("storage_unavailable", format!(
                "the node of {} was stopped, but Alondra could not free the task from it: {error:#}", task.id))))?;
    }
    let discard = |error: DomainError| {
        let error = discard(error);
        if replaced.is_none() {
            return error;
        }
        let message = format!("{} The node it replaced was stopped, so “{}” ({}) has no node now: start one with launch (task {}), without replace.",
            error.message, task.title, task.id, task.id);
        DomainError { message, ..error }
    };
    if replaced.is_some() {
        assigned_before = state(domain).map_err(discard)?.tasks.get(&task.id).and_then(|t| t.agent_id.clone());
    }
    let output = launcher.run(&cwd, &args).await.map_err(|(_, reason)| discard(DomainError::new("launch_failed", reason)))?;
    let session = match crate::mods_wait::wait(&domain.live.mods, &key, launcher).await {
        Ok(session) => session,
        Err(waited) => {
            if let Some(short) = alondra_core::sessions::launched_short(&output) {
                launcher.stop(&short).await;
            }
            let reason = crate::mods_wait::not_loaded(Path::new(&cwd), &waited, launcher).await;
            return Err(discard(DomainError::new("launch_failed", reason)));
        }
    };
    let agent_id = AgentId(session.clone());
    let agent_name = body.title.clone().unwrap_or_else(|| name.clone());
    match run(domain, &Actor::System, Command::RegisterAgent { agent_id: agent_id.clone(), name: agent_name }) {
        Ok(()) => (),
        Err(error) if error.code == "invalid_input" => (),
        Err(error) => return Err(discard(error)),
    }
    if let (Some(old), Some(dir)) = (&replaced, &inherited)
        && let Err(error) = domain.set_inherited_worktree(&session, dir, &old.0)
    {
        log!("could not record that {session} took over {dir}: {error:#}");
    }
    let bound = domain.bind_agent(&session).map_err(anyhow::Error::from)
        .and_then(|capability| crate::domain_sessions::bind(&settings, &agent_id, capability.expose_for_child_environment()));
    if let Err(error) = bound {
        let _ = domain.revoke_agent(&session);
        return Err(discard(DomainError::new("storage_unavailable", format!("could not issue the session's capability: {error}"))));
    }
    let short = alondra_core::sessions::launched_short(&output).unwrap_or_else(|| session.chars().take(8).collect());
    let taken = state(domain).map(|now| now.tasks.get(&task.id).is_none_or(|t| t.state.terminal() || t.agent_id != assigned_before));
    let assigned = match taken {
        Ok(false) => run(domain, actor, Command::AssignTask { task_id: task.id.clone(), agent_id: agent_id.clone(), model: resolved.launch_model() })
            .map_err(discard),
        Ok(true) => Err(conflict(format!("{} was closed or taken while its node started", task.id))),
        Err(error) => Err(discard(error)),
    };
    if let Err(error) = assigned {
        launcher.stop(&short).await;
        launcher.remove(&short).await;
        let _ = domain.revoke_agent(&session);
        crate::domain_sessions::release(&settings);
        return Err(error);
    }
    if origin == Origin::Auto {
        let _ = domain.store.lock().expect("domain store poisoned")
            .set_auto_launch(&task.id, crate::domain_store::AutoLaunchState::Launched, None, None, alondra_core::now_millis());
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

/// Stops the node being replaced once its turn ends, so it is not cut mid-edit: past the launcher's
/// turn deadline it is interrupted, and then stopped either way. Its watch keeps the revival off it
/// meanwhile; if the stop fails it is handed back to the revival and the task keeps it.
async fn stop_replaced(domain: &DomainRuntime, old: &AgentId, launcher: &impl crate::orchestrate::Launcher) -> Result<(), String> {
    let busy = || domain.live.node(&old.0).is_some_and(|node| node.busy);
    if !until_idle(busy, launcher.turn_deadline()).await {
        let _ = domain.live.pushes.interrupt(&old.0);
        until_idle(busy, launcher.turn_deadline()).await;
    }
    let short = domain.live.node(&old.0).map_or_else(|| old.0.chars().take(8).collect(), |node| node.short_id);
    let watched = domain.live.begin_watch(&old.0);
    let stopped = match launcher.stop_checked(&short).await {
        Err(_) if crate::node_revival::session_ended(domain, &old.0) => Ok(()),
        stopped => stopped,
    };
    match &stopped {
        Ok(()) => domain.live.mark_closed(&old.0),
        Err(reason) => domain.live.note_aborted_replace(&old.0, reason),
    }
    if watched {
        domain.live.end_watch(&old.0);
    }
    if stopped.is_err() {
        domain.live.losses.report(&old.0);
    }
    stopped
}

/// Waits until `busy` is false, at most `deadline`; true when it is.
async fn until_idle(busy: impl Fn() -> bool, deadline: std::time::Duration) -> bool {
    let until = tokio::time::Instant::now() + deadline;
    while busy() {
        if tokio::time::Instant::now() >= until {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250).min(deadline)).await;
    }
    true
}

/// What the node that takes over a worktree is told about where it works.
async fn continuing_in(dir: &Path) -> String {
    match crate::worktree::branch_in(dir).await {
        Some(branch) => format!("You continue in {}, branch {branch}: its files are the earlier node's work. Do not create another worktree and do not use EnterWorktree.", dir.display()),
        None => format!("You continue in {}: its files are the earlier node's work. Do not create another worktree and do not use EnterWorktree.", dir.display()),
    }
}

/// The worktree Alondra may delete with the node: the one Claude Code made for it, or the one it
/// inherited from the node it replaced. Never a folder it was only launched in.
fn owned_worktree(domain: &DomainRuntime, session: &str) -> Option<String> {
    match crate::worktree::origin_in(&crate::watcher::claude_home(), session) {
        Some(crate::worktree::WorktreeOrigin::Made(dir)) => dir.to_str().map(str::to_owned),
        _ => domain.inherited_worktree(session).ok().flatten(),
    }
}

/// Where a node works: the worktree Claude Code made for it, else the folder it started in.
fn node_folder(domain: &DomainRuntime, session: &str) -> Option<String> {
    crate::worktree::worktree_of(session).and_then(|dir| dir.to_str().map(str::to_owned))
        .or_else(|| domain.live.mods.session(session).and_then(|reported| reported.cwd))
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

const RESULT_LIMIT: usize = 4_000;
const FOLLOW_UP_LIMIT: usize = 12_000;

fn prefixed(prefix: &str, text: String) -> String {
    if text.is_empty() { text } else { format!("{prefix}{text}") }
}

fn clipped(text: &str, limit: usize, mark: &str) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    format!("{}{mark}", text.chars().take(limit).collect::<String>())
}

/// What a task that builds on others starts from: each earlier task's final result and its pull
/// request, so its node does not need to be told by hand.
fn follow_up_context(state: &DomainState, task: &alondra_core::domain::Task) -> String {
    let mut sources: std::collections::BTreeSet<&TaskId> = task.dependencies.iter().chain(&task.delivered_dependencies).collect();
    if let Some(stack) = &task.stack {
        sources.insert(&stack.upstream_task);
    }
    let entries: Vec<String> = sources.into_iter().filter_map(|id| state.tasks.get(id)).map(|earlier| {
        let status = if earlier.state == alondra_core::domain::TaskState::Done {
            "done"
        } else if earlier.final_presentation.is_some() {
            "delivered, not finished yet"
        } else {
            "not finished yet"
        };
        let body = match earlier.final_presentation.as_ref().and_then(|id| state.interactions.get(id)) {
            Some(result) => clipped(result.body.trim(), RESULT_LIMIT, "\n[shortened]"),
            None => "No final result was recorded for it.".to_owned(),
        };
        let mut entry = format!("### “{}” ({status})\n{body}", earlier.title);
        if let Some(pr) = earlier.pr_id.as_ref().and_then(|id| state.pull_requests.get(id)) {
            let pr_state = match pr.state {
                alondra_core::domain::PrState::Merged => "merged",
                alondra_core::domain::PrState::Open => "open",
                alondra_core::domain::PrState::Closed => "closed",
            };
            entry.push_str(&format!("\nPull request: https://{}/{}/pull/{} ({pr_state})", pr.host, pr.repository, pr.number));
        }
        entry
    }).collect();
    if entries.is_empty() {
        return String::new();
    }
    clipped(&format!("## Earlier work this task builds on\n\n{}", entries.join("\n\n")), FOLLOW_UP_LIMIT, "")
}

/// The changes the user asked for on a result the task's earlier node delivered: they go to the
/// node that takes the task over, since the one they were sent to is gone.
fn asked_changes(state: &DomainState, task: &alondra_core::domain::Task) -> String {
    use alondra_core::domain::InteractionPayload;
    let Some(InteractionPayload::Delivery { feedback, acceptance, .. }) = task.final_presentation.as_ref()
        .and_then(|id| state.interactions.get(id)).map(|result| &result.payload)
    else {
        return String::new();
    };
    if feedback.is_empty() || acceptance.as_ref().is_some_and(|accepted| accepted.valid) {
        return String::new();
    }
    let asked: Vec<String> = feedback.iter().map(|change| format!("- {}", change.body.trim())).collect();
    format!("## Changes the user asked for on the result you take over\n\n{}", asked.join("\n"))
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
    approach: Option<alondra_core::options::Approach>,
    #[serde(default)]
    reason: Option<String>,
}

/// Sends the orchestrator's text to an Agent's session. With a Task, the Agent is told which one;
/// the Task must be the Agent's own: a node works on one Task for its whole life and closes with
/// it, so a message never gives a node another Task nor brings a closed one back.
pub(crate) async fn send(domain: &DomainRuntime, home: &Path, actor: &Actor, body: SendBody) -> Result<Value, DomainError> {
    let current = state(domain)?;
    let agent_id = agent(&current, &body.node)?;
    let node_runs = domain.live.mods.session(&agent_id.0).and_then(|reported| reported.model);
    let node_model = node_runs.as_deref().map(alondra_core::model_label::model_family);
    if let Some(task_id) = &body.task {
        let task = current.tasks.get(task_id).ok_or_else(|| DomainError::new("not_found", "task does not exist"))?;
        if task.agent_id.as_ref() != Some(&agent_id) {
            return Err(DomainError::new("not_assigned", format!("{task_id} is not {}'s task: a node works only on the task it was launched for. Launch a new node for {task_id} with launch.", body.node)));
        }
        if task.state.terminal() {
            return Err(DomainError::new("task_closed", format!("{task_id} is closed, and its node closes with it. For more work, create a new task with create_task and dependencies [{task_id}]: its node gets this task's result.")));
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
        && body.approach.is_some()
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
pub(crate) struct LaunchPlanBody {
    task_id: TaskId,
    launch: crate::launch_plans::LaunchPlan,
}

/// Gives a task the plan Alondra starts it with by itself; a task waiting for one is started at
/// the next pass.
pub(crate) fn set_launch_plan(domain: &DomainRuntime, home: &Path, body: LaunchPlanBody) -> Result<Value, DomainError> {
    let current = state(domain)?;
    let task = current.tasks.get(&body.task_id).ok_or_else(|| DomainError::new("not_found", "task does not exist"))?;
    if task.state.terminal() {
        return Err(DomainError::new("task_not_active", format!("{} is closed", task.id)));
    }
    crate::launch_plans::validate(domain, home, &task.request_id, &body.launch)?;
    let stored = |error: anyhow::Error| { log!("could not store the launch plan of {}: {error:#}", task.id); DomainError::new("storage_unavailable", "the launch plan could not be stored") };
    let mut store = domain.store.lock().expect("domain store poisoned");
    store.set_launch_plan(&task.id, &body.launch, alondra_core::now_millis()).map_err(stored)?;
    let waiting = store.auto_launch(&task.id).map_err(stored)?
        .is_some_and(|row| row.state == crate::domain_store::AutoLaunchState::Blocked && row.reason == Some(crate::domain_store::AutoLaunchReason::NoPlan));
    if waiting {
        store.forget_auto_launch(&task.id).map_err(stored)?;
    }
    Ok(json!({"task": task.id, "stored": true}))
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
