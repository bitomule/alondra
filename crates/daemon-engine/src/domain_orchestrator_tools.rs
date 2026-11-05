//! What the orchestrator does outside the domain, as 8ccd131's orchestrator tools did: launch an
//! Agent for a Task, type a message into an Agent's session, press Esc in it and read the
//! machine. Each call carries the orchestrator execution's bearer; the domain changes a call
//! implies (the new Agent, an assignment, a reopened Task) are that orchestrator's commands.
use crate::locks::Poisonable as _;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::model_metrics::{Decision, DecisionKind};
use crate::launch_worktree::Workplace;
use alondra_core::domain::{Actor, AgentId, DomainError, DomainState, ModelChooser, RequestId, TaskId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The orchestrator's starting model, for a relaunch of a task that never recorded one.
const DEFAULT_MODEL: &str = "sonnet";

/// What the user chose when asking for a Request: its project, options, issue and files.
pub(crate) type Choices = crate::domain_store::RequestLaunch;

pub fn request_choices(domain: &DomainRuntime, request: &RequestId) -> anyhow::Result<Option<Choices>> {
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

/// The orchestrator a local bearer belongs to, while its generation is current. A call that may
/// change `what` is recorded against its execution before it acts.
pub fn orchestrator(domain: &DomainRuntime, bearer: Option<&str>, what: Option<&str>) -> Result<Actor, DomainError> {
    let refused = || DomainError::new("forbidden", "the orchestrator's capability is required");
    let bearer = bearer.ok_or_else(refused)?;
    let principal = match what {
        Some(what) => domain.admit_change(bearer, what),
        None => domain.resolve_bearer(bearer),
    }.map_err(|_| refused())?;
    match principal {
        crate::domain_authority::Principal::Orchestrator { generation, .. } => Ok(Actor::Orchestrator { generation }),
        _ => Err(refused()),
    }
}

fn state(domain: &DomainRuntime) -> Result<std::sync::Arc<DomainState>, DomainError> {
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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchBody {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider: Option<alondra_core::accounts::Provider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile: Option<alondra_core::model_selection::ModelProfile>,
    #[serde(default, alias = "reasoning_effort")]
    effort: Option<String>,
    #[serde(default)]
    approach: Option<alondra_core::options::Approach>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tags: Option<alondra_core::task_tags::TaskTags>,
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
            provider: None,
            profile: plan.role.map(|role| match role {
                alondra_core::domain::TaskRole::Design => alondra_core::model_selection::ModelProfile::Design,
                alondra_core::domain::TaskRole::Implementation => alondra_core::model_selection::ModelProfile::Implementation,
                alondra_core::domain::TaskRole::Review => alondra_core::model_selection::ModelProfile::Review,
            }),
            effort: plan.effort.clone(),
            approach: plan.approach,
            tags: plan.tags.clone(),
            reason: plan.reason.clone(),
            relaunch_model: None,
            replace: false,
            _over_pace_ok: false,
        }
    }

    pub(crate) fn task_id(&self) -> &TaskId {
        &self.task
    }

    /// The launch as the gate's queue keeps it, the model of a relaunch included.
    pub(crate) fn to_queue(&self) -> Value {
        json!({"body": serde_json::to_value(self).unwrap_or_default(), "relaunch_model": self.relaunch_model})
    }

    pub(crate) fn from_queue(stored: &Value) -> anyhow::Result<LaunchBody> {
        let mut body: LaunchBody = serde_json::from_value(stored["body"].clone())?;
        body.relaunch_model = stored["relaunch_model"].as_str().map(str::to_owned);
        Ok(body)
    }

    /// What the task's stored plan fills in where the call named nothing.
    fn with_plan(mut self, plan: Option<&crate::launch_plans::LaunchPlan>) -> LaunchBody {
        let Some(plan) = plan else { return self };
        self.name = self.name.or_else(|| Some(plan.name.clone()));
        self.cwd = self.cwd.or_else(|| Some(plan.cwd.clone()));
        self.prompt = self.prompt.or_else(|| plan.mission.clone());
        self.approach = self.approach.or(plan.approach);
        self.tags = self.tags.or_else(|| plan.tags.clone());
        self.profile = self.profile.or_else(|| plan.role.map(|role| match role {
            alondra_core::domain::TaskRole::Design => alondra_core::model_selection::ModelProfile::Design,
            alondra_core::domain::TaskRole::Implementation => alondra_core::model_selection::ModelProfile::Implementation,
            alondra_core::domain::TaskRole::Review => alondra_core::model_selection::ModelProfile::Review,
        }));
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
    /// A launch the orchestrator or the user asked for, which waited at the gate for its turn.
    Admitted,
}

/// Launches a Claude session for a Task with the options its Request was asked for, gives it its
/// bearer through its settings file and assigns it the Task.
pub(crate) async fn launch(domain: &DomainRuntime, home: &Path, body: LaunchBody,
    choices: impl FnOnce(&RequestId) -> Option<Choices>) -> Result<Value, DomainError> {
    launch_from(domain, home, body, choices, Origin::Orchestrator).await
}

/// `launch`, for either origin. An automatic launch holds the task's launch guard already.
pub(crate) async fn launch_from(domain: &DomainRuntime, home: &Path, body: LaunchBody,
    choices: impl FnOnce(&RequestId) -> Option<Choices>, origin: Origin) -> Result<Value, DomainError> {
    let task = body.task.clone();
    let cwd = body.cwd.clone().or_else(|| domain.launch_plan(&task).ok().flatten().map(|plan| plan.cwd));
    let launched = if matches!(origin, Origin::Orchestrator | Origin::Admitted) {
        let _launching = Launching::begin(domain, &body.task)?;
        let body = if matches!(origin, Origin::Admitted) {
            let queue = domain.store.lock_or_exit("the domain store").launch_queue()
                .map_err(|error| DomainError::new("storage_unavailable", error.to_string()))?;
            match queue.into_iter().find(|row| row.task_id == body.task) {
                Some(row) => LaunchBody::from_queue(&row.body)
                    .map_err(|error| DomainError::new("storage_unavailable", error.to_string()))?,
                None => body,
            }
        } else { body };
        launch_guarded(domain, home, body, choices, origin, None).await
    } else {
        launch_guarded(domain, home, body, choices, origin, None).await
    };
    if launched.is_err() && crate::start_failures::ended(domain, &task).unwrap_or(false) {
        return launched;
    }
    match &launched {
        Ok(_) => remember_refusal(domain, &task, None),
        Err(error) if error.code == "launch_failed" => remember_refusal(domain, &task, Some(("launch_failed", Some(&error.message)))),
        Err(_) => {}
    }
    if let (Err(error), false) = (&launched, matches!(origin, Origin::Auto))
        && matches!(error.code.as_str(), "launch_failed" | "workspace_untrusted")
        && let Some(problem) = crate::start_failures::problem_of(&error.code, &error.message, cwd.as_deref())
        && let Err(failure) = crate::start_failures::failed(domain, &task, problem, alondra_core::now_millis())
    {
        log!("could not record that the start of {task} failed: {failure:#}");
    }
    launched
}

/// What the Tasks list says about a task that did not start: kept until it starts.
/// The orchestrator's launch waits in the gate's queue, which starts it by itself once there is room.
fn queue_at_gate(domain: &DomainRuntime, body: &LaunchBody, task: &TaskId, title: &str) -> Result<Value, DomainError> {
    let now = alondra_core::now_millis();
    crate::admission::enqueue(domain, body, "orchestrator", now)
        .map_err(|error| DomainError::new("storage_unavailable", format!("could not queue the launch: {error:#}")))?;
    Ok(json!({"task": task, "queued": true, "message": format!("“{title}” is queued. {}", crate::admission::queued_text(domain, task, now))}))
}

fn remember_refusal(domain: &DomainRuntime, task: &TaskId, refusal: Option<(&str, Option<&str>)>) {
    let recorded = domain.store.lock_or_exit("the domain store")
        .set_launch_refusal(task, refusal, alondra_core::now_millis());
    if let Err(error) = recorded {
        log!("could not record why {task} did not start: {error:?}");
    }
}

/// A launch that waits for usage: the task says so and the gate tries it again once there is some.
fn usage_refusal(domain: &DomainRuntime, task: &TaskId, exhausted: &crate::usage_pause::Exhausted) -> DomainError {
    remember_refusal(domain, task, Some(("usage_exhausted", None)));
    if exhausted.no_session {
        return DomainError::new("usage_exhausted", format!(
            "No Claude account on this Mac has a session: {task} stays as it is and starts once one is signed in"));
    }
    let why = if exhausted.by_user { "The user paused Claude work" } else { "No Claude account on this Mac has usage left" };
    DomainError::new("usage_exhausted", format!(
        "{why} until {}: {task} stays as it is and can be launched after that",
        crate::usage_pause::clock_text(exhausted.until, alondra_core::now_millis(), false)))
}

/// The right to launch a task's node, given back however the launch ends, a panic included.
pub(crate) struct Launching<'a> {
    domain: &'a DomainRuntime,
    task: String,
}

impl<'a> Launching<'a> {
    pub(crate) fn begin(domain: &'a DomainRuntime, task: &TaskId) -> Result<Self, DomainError> {
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
pub async fn launch_replacing(domain: Arc<DomainRuntime>, home: PathBuf,
    body: LaunchBody, choices: impl FnOnce(&RequestId) -> Option<Choices> + Send + 'static) -> Result<Value, DomainError> {
    if !body.replace {
        return launch(&domain, &home, body, choices).await;
    }
    let task = body.task.clone();
    let work_domain = Arc::clone(&domain);
    launch_replacement_work(domain, task, move |checked| async move {
        launch_guarded(&work_domain, &home, body, choices, Origin::Orchestrator, Some(checked)).await
    }).await
}

async fn launch_replacement_work<F, W>(domain: Arc<DomainRuntime>, task: TaskId, work: F) -> Result<Value, DomainError>
where F: FnOnce(Box<dyn FnOnce() + Send>) -> W + Send + 'static,
      W: std::future::Future<Output = Result<Value, DomainError>> + Send + 'static,
{
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
                    work(on_checked).await
                }
            });
            let outcome = replacement.await.unwrap_or_else(|failure| Err(DomainError::new("launch_failed", format!(
                "the replacement stopped unexpectedly ({failure}); check the task with tasks_in_progress before launching it again"))));
            if let (true, Err(error)) = (reached.load(std::sync::atomic::Ordering::SeqCst), &outcome) {
                tell_replace_failed(&domain, &task, error);
            }
            outcome
        }
    });
    if checked.await.is_err() {
        return worker.await.unwrap_or_else(|failure| Err(DomainError::new("launch_failed", format!("the launch stopped unexpectedly: {failure}"))));
    }
    Ok(json!({"task": task, "replacing": true, "message": format!(
        "Replacing the node of “{}”: Alondra waits for its turn to end (interrupting it if it takes too long), stops it and \
         starts the new node in its folder. This can take a few minutes; tasks_in_progress shows the new node once it works on the task. If \
         the new node does not start, Alondra starts it again by itself; you are told only if the old node keeps the task.", task_title(&domain, &task))}))
}

/// The account paid per use the user chose for the request, while it is still registered, on and
/// paid per use: its tasks run there whatever the subscriptions' margin. Otherwise Alondra picks.
fn pinned_account(home: &Path, options: &alondra_core::options::Resolved) -> Option<alondra_core::accounts::AccountId> {
    let chosen = options.account()?;
    let config = crate::accounts::config(home).ok()?;
    config
        .accounts(alondra_core::accounts::Provider::Claude)
        .into_iter()
        .find(|account| account.id == chosen && account.enabled && account.billing == alondra_core::accounts::AccountBilling::PayPerUse)
        .map(|account| account.id)
}

pub fn validate_request_selection(domain: &DomainRuntime, resolved: &alondra_core::options::Resolved) -> Result<(), DomainError> {
    let Some(intent) = &resolved.selection else { return Ok(()) };
    let config = crate::accounts::config(domain.home()).map_err(|why| DomainError::new("settings_unavailable", why))?;
    if let Some(account) = &intent.account
        && !config.accounts(account.provider).iter().any(|known| known.id == account.account && known.enabled)
    { return Err(DomainError::new("account_unavailable", "The selected account is not connected and enabled")); }
    if let Some(requested) = &intent.requested {
        let read = domain.read();
        let candidate = alondra_core::model_selection::ModelCandidate { model: requested.model.clone(), reasoning_effort: intent.requested_reasoning_effort.clone() };
        let offered = read.index().model_catalogs().iter().filter(|catalog| catalog.account.provider == requested.provider && intent.account.as_ref().is_none_or(|account| catalog.account == *account))
            .any(|catalog| config.accounts(requested.provider).iter().any(|account| account.id == catalog.account.account && account.enabled) && catalog.resolve(&candidate).is_ok());
        if !offered { return Err(DomainError::new("model_unavailable", "Refresh the connected account's models and choose an advertised model and reasoning effort")); }
    }
    Ok(())
}

pub fn authorize_user_request_selection(domain: &DomainRuntime, resolved: &mut alondra_core::options::Resolved, explicit_account: bool) {
    if let Some(intent) = resolved.selection.as_mut() {
        intent.paid_account_grant = None;
        if explicit_account { intent.authorize_paid_account(&domain.read().state().accounts); }
    }
}

pub(crate) fn validate_native_requirements(intent: Option<&alondra_core::model_selection::TaskSelection>, denied_tools: &[String]) -> Result<(), DomainError> {
    if !denied_tools.is_empty() && intent.is_some_and(|intent| intent.account.as_ref().is_some_and(|account| account.provider == alondra_core::accounts::Provider::Codex)
        || (!intent.allow_model_substitution && intent.requested.as_ref().is_some_and(|model| model.provider == alondra_core::accounts::Provider::Codex)))
    { return Err(DomainError::new("unsupported_capability", "Codex cannot enforce this task's denied tools; choose a compatible provider or permit model substitution with Automatic accounts")); }
    Ok(())
}

/// The connected providers that can serve a model named without its provider.
pub(crate) fn providers_offering(domain: &DomainRuntime, accounts: &alondra_core::accounts::AccountsConfig, model: &str) -> Vec<alondra_core::accounts::Provider> {
    alondra_core::model_selection::providers_offering(domain.read().index().model_catalogs(), accounts.connected_providers(), model)
}

/// What the orchestrator gets back when a model name without a provider cannot be placed.
pub(crate) fn ambiguous_model(model: &str, offering: &[alondra_core::accounts::Provider]) -> DomainError {
    if offering.len() < 2 {
        return DomainError::new("invalid_input", format!("Specify the provider of “{model}”: name it claude/{model} or codex/{model}"));
    }
    let names: Vec<String> = offering.iter().map(|provider| provider.as_str().to_owned()).collect();
    let qualified: Vec<String> = offering.iter().map(|provider| format!("{}/{model}", provider.as_str())).collect();
    DomainError::new("invalid_input", format!("“{model}” is offered by {}: name it {}", names.join(" and "), qualified.join(" or ")))
}

fn native_launch(domain: &DomainRuntime, home: &Path, task: &alondra_core::domain::Task, requested: &alondra_core::options::Resolved, file: &alondra_core::options::OptionsFile, body: &LaunchBody, prior_native: bool, reopening: bool)
    -> Result<Option<(alondra_core::options::Resolved, alondra_core::accounts::AccountKey, String)>, DomainError>
{
    use alondra_core::accounts::Provider;
    use alondra_core::model_selection::{ModelKey, ModelProfile, TaskSelection};
    let modern = task.selection.is_some() || requested.selection.is_some() || body.provider.is_some() || body.model.as_ref().is_some_and(|model| model.starts_with("codex/") || model.starts_with("claude/"));
    if !modern { return Ok(None); }
    let mut intent = task.selection.clone().or_else(|| requested.selection.clone()).unwrap_or_else(|| TaskSelection::automatic(ModelProfile::Implementation));
    if let Some(profile) = body.profile { intent.profile = profile; }
    if intent.account.is_none() { intent.account = requested.account_key(); }
    let mut initial = intent.clone();
    if let Some(model) = body.model.as_deref() {
        let offered = if model.starts_with("claude/") || model.starts_with("codex/") {
            ModelKey::parse_wire(model).map_err(|why| DomainError::new("invalid_input", why))?
        } else {
            let offering = providers_offering(domain, &file.accounts, model);
            let provider = body.provider.or_else(|| intent.requested.as_ref().map(|model| model.provider))
                .or_else(|| intent.effective.as_ref().map(|model| model.model.provider))
                .or_else(|| (offering.len() == 1).then(|| offering[0]))
                .or_else(|| (file.accounts.connected_providers().len() == 1).then(|| file.accounts.connected_providers()[0]))
                .ok_or_else(|| ambiguous_model(model, &offering))?;
            ModelKey { provider, model: model.into() }
        };
        if body.provider.is_some_and(|provider| provider != offered.provider) { return Err(DomainError::new("invalid_input", "The model belongs to another provider")); }
        if intent.requested.as_ref().is_some_and(|requested| requested != &offered) {
            return Err(DomainError::new("model_is_binding", "The user selected the initial model; a launch cannot replace it before a permitted transition"));
        }
        initial.requested = Some(offered);
    } else if prior_native && initial.requested.is_none() {
        initial.requested = intent.effective.as_ref().map(|effective| effective.model.clone());
        initial.requested_reasoning_effort = intent.effective.as_ref().and_then(|effective| effective.reasoning_effort.clone());
    }
    if let Some(effort) = &body.effort {
        if intent.requested_reasoning_effort.as_ref().is_some_and(|requested| requested != effort) { return Err(DomainError::new("model_is_binding", "The requested reasoning effort is binding")); }
        initial.requested_reasoning_effort = Some(effort.clone());
    }
    if initial.requested.is_none() && initial.requested_reasoning_effort.is_some() { return Err(DomainError::new("invalid_input", "Choose a model before its native reasoning effort")); }
    let active = (!reopening && (prior_native || task.selection.as_ref().and_then(|selection| selection.effective.as_ref()).is_some())).then(||task.account_key());
    let denied_tools = domain.schedule_request_tools(&task.request_id).map_err(|why| DomainError::new("storage_unavailable", why.to_string()))?.unwrap_or_default();
    validate_native_requirements(Some(&initial), &denied_tools)?;
    let decision = crate::accounts::select_for_node_with_tools(domain, home, &initial, active.as_ref(),
        body.provider.filter(|_| denied_tools.is_empty()), &denied_tools, alondra_core::now_millis())?;
    if !reopening && let Some(current) = task.selection.as_ref().and_then(|selection| selection.effective.as_ref())
        && (task.account_key() != decision.account || current.model != decision.selection.model || current.reasoning_effort != decision.selection.reasoning_effort)
    { return Err(DomainError::new("selection_transition_required", "Continue this task through its durable model or account transition so its retained history is carried safely")); }
    intent.effective = Some(decision.selection.clone());
    let mut resolved = requested.clone();
    resolved.flags.model = (decision.account.provider == Provider::Claude).then(|| decision.selection.model.model.clone());
    resolved.flags.effort = decision.selection.reasoning_effort.clone();
    resolved.selection = Some(intent);
    Ok(Some((resolved, decision.account, decision.reason)))
}

fn task_title(domain: &DomainRuntime, task: &TaskId) -> String {
    domain.state().ok().and_then(|state| state.tasks.get(task).map(|task| task.title.clone())).unwrap_or_else(|| task.0.clone())
}

/// Only a replacement that did not finish is told: one that did shows in tasks_in_progress and the
/// task, and one that left the task without a node is followed by the node Alondra starts by itself.
fn tell_replace_failed(domain: &DomainRuntime, task: &TaskId, error: &DomainError) {
    if domain.live.closed_task(&task.0).is_some_and(|node| node.relaunch == crate::live::Relaunch::ByItself) {
        log!("the replacement of {task}'s node did not finish ({}); Alondra starts a new node by itself", error.message);
        return;
    }
    let title = task_title(domain, task);
    let text = format!("The replacement of the node of “{title}” ({task}) did not finish: {}", error.message);
    if let Err(error) = domain.tell_orchestrator(&format!("replaced-{task}-{}", crate::auth::random_hex(4)), None, text) {
        log!("could not tell the orchestrator how the replacement of {task}'s node ended: {error:?}");
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn launch_guarded(domain: &DomainRuntime, home: &Path, body: LaunchBody,
    choices: impl FnOnce(&RequestId) -> Option<Choices>, origin: Origin,
    checked: Option<Box<dyn FnOnce() + Send>>) -> Result<Value, DomainError> {
    let conflict = |message: String| DomainError::new("conflict", message);
    let current = state(domain)?;
    let task = current.tasks.get(&body.task).ok_or_else(|| DomainError::new("not_found", "task does not exist"))?;
    if crate::provider_transition::moving(domain, &task.id) {
        return Err(DomainError::new("task_moving", format!("{} is moving to another account: Alondra starts its next node there itself", task.id)));
    }
    let reopening = current.pending_reopening(&task.id).map(str::to_owned);
    let prior_native = reopening.is_none() && (task.started || current.lifecycle_journal.get(&task.id).is_some_and(|records|records.iter()
        .any(|record|matches!(record,alondra_core::domain::LifecycleRecord::Admitted{execution:Some(_),..}|alondra_core::domain::LifecycleRecord::ResourceAvailable))));
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
    if prior_native && body.cwd.is_none() {
        body.cwd = domain.task_worktree(&task.id).map_err(|error| DomainError::new("storage_unavailable", error.to_string()))?;
    }
    let plan = domain.launch_plan(&task.id).map_err(|_| DomainError::new("storage_unavailable", "domain state unavailable"))?;
    let body = body.with_plan(plan.as_ref());
    if let Some(Err(why)) = body.tags.as_ref().map(alondra_core::task_tags::TaskTags::validate) {
        return Err(DomainError::new("invalid_input", why));
    }
    let (Some(name), Some(mut cwd)) = (body.name.clone(), body.cwd.clone()) else {
        return Err(DomainError::new("invalid_input", "name and cwd are required: this task has no stored launch plan"));
    };
    if task.over() {
        return Err(conflict(format!("{} is closed; reopen it first", body.task)));
    }
    if let Some(why) = crate::plan_reviews::held(&current, &task.id) {
        return Err(DomainError::new("plan_review_pending", why));
    }
    if origin != Origin::Admitted && crate::admission::held_by_user(domain, &task.id) {
        return Err(conflict(format!("The user paused “{}” in the queue: only the user resumes it", task.title)));
    }
    if replaced.is_none() && task.agent_id.as_ref().and_then(|id| current.agents.get(id)).is_some_and(|a| a.phase.live() && !a.phase.coming_back()) {
        return Err(conflict(format!("{} already has a connected agent; send it what to do", body.task)));
    }
    let choices = choices(&task.request_id);
    let file = crate::domain_schedule_runner::options_at(home).map_err(|error| DomainError::new("invalid_input", format!("options.toml: {error}")))?;
    let requested = match &choices {
        Some(choices) => choices.options.clone(),
        None => {
            let project = reopening.as_ref().and_then(|_| plan.as_ref().and_then(|plan| plan.project.clone()))
                .or_else(|| crate::options::project_of(&file, Path::new(&cwd)));
            file.orchestrator_defaults(project.as_deref(), None)
        }
    };
    if prior_native && (requested.account_key().is_some_and(|account|account!=task.account_key()) || body.provider.is_some_and(|provider|provider!=task.provider)) {
        return Err(DomainError::new("active_account_immutable","Choose account and provider when creating a node. The existing native session keeps its original account"));
    }
    let native = native_launch(domain, home, task, &requested, &file, &body, prior_native, reopening.is_some())
        .map_err(|error| match domain.live.usage_pause.current() {
            Some(exhausted) if error.code == "account_unavailable" => usage_refusal(domain, &task.id, &exhausted),
            _ => error,
        })?;
    let provider = native.as_ref().map_or(alondra_core::accounts::Provider::Claude, |(_, account, _)| account.provider);
    let pinned = choices.as_ref().and_then(|choices| pinned_account(home, &choices.options));
    let account = match (&native, &pinned) {
        (Some((_, key, _)), _) => Some(key.account.clone()),
        (None, Some(pinned)) => Some(pinned.clone()),
        (None, None) if prior_native => Some(task.account_key().account),
        (None, None) => crate::accounts::choose_for_launch(domain, home, alondra_core::now_millis()),
    };
    if prior_native && alondra_core::accounts::AccountKey::new(provider,account.clone().unwrap_or_else(alondra_core::accounts::AccountId::default_account)) != task.account_key() {
        return Err(DomainError::new("active_account_immutable","Choose account and provider when creating a node. The existing native session keeps its original account"));
    }
    if let Some(exhausted) = domain.live.usage_pause.current().filter(|exhausted| exhausted.by_user || (native.is_none() && pinned.is_none())) {
        return Err(usage_refusal(domain, &task.id, &exhausted));
    }
    if reopening.is_none() && !Path::new(&cwd).is_dir() {
        return Err(DomainError::new("invalid_input", format!("directory {cwd} does not exist")));
    }
    if let Some(why) = domain.live.machine_watch.refuse_launch(Path::new(&cwd)) {
        if origin == Origin::Orchestrator && replaced.is_none() {
            return queue_at_gate(domain, &body, &task.id, &task.title);
        }
        remember_refusal(domain, &task.id, Some(("disk_low", None)));
        return Err(DomainError::new("disk_low", format!("{why}. {} stays as it is and can be launched after that", body.task)));
    }
    let projects_root = crate::domain_schedule_runner::options_at(home).ok().map(|file| file.projects_root(&crate::options::user_home()));
    let account = account.unwrap_or_else(alondra_core::accounts::AccountId::default_account);
    let key = alondra_core::accounts::AccountKey::new(provider, account.clone());
    if !prior_native { crate::accounts::log_choice(domain, &format!("node for {}", task.id), &key, alondra_core::now_millis()); }
    let known = if reopening.is_some() {
        crate::sdk_owner::original_directory(domain, &task.id)
            .map_err(|error| DomainError::new("storage_unavailable", error.to_string()))?
            .or(domain.task_worktree(&task.id).map_err(plan_storage)?)
    } else { domain.task_worktree(&task.id).map_err(plan_storage)? }.map(PathBuf::from);
    if prior_native {
        let (backend, runtime) = domain.sdk().ok_or_else(|| DomainError::new("unavailable", crate::sdk_owner::NO_HOST))?;
        let original_directory = known.clone().unwrap_or_else(|| PathBuf::from(&cwd));
        crate::sdk_owner::require_recoverable_session(&runtime, backend, &task.id, provider, &account, original_directory.to_string_lossy().as_ref())
            .map_err(|error| DomainError::new("native_session_required", error.to_string()))?;
        if !crate::launch_worktree::recovery_directory_matches(Path::new(&cwd), &original_directory).await {
            return Err(DomainError::new("native_session_required", "Recover this node in its original native directory. Create another node to choose a different workspace"));
        }
        cwd = original_directory.to_string_lossy().into_owned();
    }
    let profile = alondra_core::accounts::provider_folder(home, &key);
    if let Some(repository) = (provider == alondra_core::accounts::Provider::Claude).then(|| crate::claude_trust::ensure_for(profile.as_deref(), Path::new(&cwd), projects_root.as_deref())).flatten() {
        remember_refusal(domain, &task.id, Some(("workspace_untrusted", Some(&repository.to_string_lossy()))));
        return Err(DomainError::new("workspace_untrusted", crate::claude_trust::instructions(&repository)));
    }
    let pr_head = current.pull_requests.values().filter(|pr| pr.task_id == task.id).map(|pr| pr.head.as_str()).last();
    let slug = crate::launch_worktree::slug(&task.id.0);
    let (workplace, recover_directory) = if reopening.is_some() {
        crate::launch_worktree::reopen(Path::new(&cwd), &slug, known.as_deref().unwrap_or_else(|| Path::new(&cwd)), pr_head).await
    } else {
        crate::launch_worktree::isolate(Path::new(&cwd), &slug, known.as_deref(), pr_head).await.map(|workplace| (workplace, false))
    }.map_err(|why| DomainError::new("main_checkout", format!("{why}. {} stays as it is and was not launched", task.id)))?;
    let reopening = reopening.map(|input| {
        let (backend, _) = domain.sdk().ok_or_else(|| DomainError::new("unavailable", crate::sdk_owner::NO_HOST))?;
        crate::sdk_owner::prepare_reopening(domain, backend, &task.id, provider, &account, workplace.dir(), input, recover_directory)
            .map_err(|error| DomainError::new("native_session_required", error.to_string()))
    }).transpose()?;
    if prior_native {
        let (backend, runtime) = domain.sdk().ok_or_else(|| DomainError::new("unavailable", crate::sdk_owner::NO_HOST))?;
        crate::sdk_owner::require_recoverable_session(&runtime, backend, &task.id, provider, &account, workplace.dir().to_string_lossy().as_ref())
            .map_err(|error| DomainError::new("native_session_required", error.to_string()))?;
    }
    own_worktree(domain, &task.id, &workplace).await?;
    let cwd = workplace.dir().to_string_lossy().into_owned();
    let Some(plugin) = crate::plugins::node_in(home) else {
        return Err(DomainError::new("unavailable", "Alondra's plugin was not found: run `alondra doctor --fix`"));
    };
    let request = current.requests.get(&task.request_id);
    let auto = requested.effort == alondra_core::options::AUTO;
    let model = body.model.as_deref().or(body.relaunch_model.as_deref().filter(|_| auto));
    let resolved = match &native {
        Some((resolved, _, _)) => resolved.clone(),
        None => requested.with_orchestrator_model(&file, model, body.effort.as_deref()).map_err(|reason| DomainError::new("invalid_input", reason))?,
    };
    let reason = given(body.reason.as_deref()).or_else(|| native.as_ref().map(|(_, _, reason)| reason.clone()));
    if native.is_none() && auto && body.relaunch_model.is_none() && reason.is_none() {
        return Err(DomainError::new("invalid_input", "effort is auto for this request: pass reason, one short line on what the task is and why this model fits it"));
    }
    if origin == Origin::Orchestrator && replaced.is_none() {
        use crate::admission::{Key, Waiting};
        let now = alondra_core::now_millis();
        let key = Key::Task(task.id.clone());
        let waiting = Waiting { key: key.clone(), priority: task.priority, resuming: false, since: now };
        if domain.live.gate.ask(domain, waiting, now).is_err() {
            domain.live.gate.forget(&key);
            return queue_at_gate(domain, &body, &task.id, &task.title);
        }
    }
    let mission = body.prompt.clone().unwrap_or_else(|| {
        let body = request.filter(|r| !current.request_plans.contains_key(&r.id)).map(|r| r.body.as_str()).unwrap_or_default();
        let mut text = format!("{}\n\n{body}", task.title);
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
    let mission = format!("{mission}{}{}{}", prefixed("\n\n", approved_plan(&current, task)), prefixed("\n\n", follow_up_context(&current, task)), prefixed("\n\n", asked_changes(&current, task)));
    let mission = match (&workplace, &replaced) {
        (Workplace::Made(dir), _) => format!("{mission}\n\n{}", made_for(dir).await),
        (Workplace::Reused(_), _) | (_, Some(_)) => format!("{mission}\n\n{}", continuing_in(Path::new(&cwd)).await),
        (Workplace::Given(_), None) => mission,
    };
    let prompt = crate::domain_schedule_runner::producer_prompt(&mission, &resolved, &task.id, body.approach.unwrap_or_default());
    if domain.sdk().is_none() {
        return Err(DomainError::new("unavailable", crate::sdk_owner::NO_HOST));
    }
    if let Some(checked) = checked {
        checked();
    }
    {
        match &replaced {
            Some(old) if crate::sdk_owner::runs_through_sdk(&current, old) => {
                let (backend, runtime) = domain.sdk().expect("the SDK backend was just found");
                let backend = backend.clone();
                crate::task_closer::blocking(|| {
                    domain.stop_sdk_host(old)?;
                    crate::sdk_owner::end_unowned_host(runtime, &backend, task.id.clone())
                })
                .map_err(|crate::sdk_owner::Failed { why, .. }| conflict(format!("the node of {} could not be stopped to replace it: {why}", task.id)))?;
                crate::node_revival::take_out(domain, &old.0, "node replaced by the orchestrator", Some(cwd.clone()), crate::live::Relaunch::ByItself)
                    .map_err(|error| DomainError::new("storage_unavailable", format!(
                        "the Agent SDK node of {} was stopped, but Alondra could not free the task from it: {error:#}", task.id)))?;
            }
            Some(old) => {
                stop_replaced(domain, old).await.map_err(|reason| DomainError::new("replace_failed", format!(
                    "Alondra could not stop the claude --bg node of {}, so no node was started through the Agent SDK: {reason}. It is still assigned to that node.", task.id)))?;
                crate::node_revival::take_out(domain, &old.0, "node replaced by the orchestrator", Some(cwd.clone()), crate::live::Relaunch::ByItself)
                    .map_err(|error| DomainError::new("storage_unavailable", format!(
                        "the claude --bg node of {} was stopped, but Alondra could not free the task from it: {error:#}", task.id)))?;
            }
            None => {}
        }
        let selected = resolved.selection.as_ref().and_then(|selection| selection.effective.as_ref());
        let options = crate::sdk_owner::LaunchOptions {
            provider,
            selection: resolved.selection.clone(),
            model: selected.map(|selection| selection.model.model.clone()).or_else(|| resolved.flags.model.clone()),
            effort: selected.and_then(|selection| selection.reasoning_effort.clone()).or_else(|| resolved.flags.effort.clone()),
            disallowed_tools: domain.schedule_request_tools(&task.request_id).ok().flatten().unwrap_or_default(),
        };
        let name = body.title.clone().unwrap_or(name);
        let launched = crate::sdk_owner::launch_prepared(domain, crate::sdk_owner::Launch { task: task.id.clone(), name, directory: cwd.clone(), prompt, plugin, options,
            model: resolved.launch_model(), account }, reopening)
            .await
            .map_err(|error| match &replaced {
                Some(_) => DomainError { message: format!("{} The node it replaced was stopped, so Alondra starts a new node for “{}” ({}) by itself.",
                    error.message, task.title, task.id), ..error },
                None => error,
            })?;
        record_worktree(domain, launched["agent_id"].as_str(), &workplace, replaced.as_ref().zip(inherited.as_ref()));
        if origin == Origin::Auto {
            let _ = domain.store.lock_or_exit("the domain store")
                .set_auto_launch(&task.id, crate::domain_store::AutoLaunchState::Launched, None, None, alondra_core::now_millis());
        }
        record(domain, Decision {
            task_id: task.id.0.clone(), request_id: task.request_id.0.clone(),
            agent_id: launched["agent_id"].as_str().map(str::to_owned), at: alondra_core::now_millis(),
            kind: if body.relaunch_model.is_some() { DecisionKind::Relaunch } else { DecisionKind::Launch },
            model: selected.map(|selection| selection.model.wire_id()).or_else(|| resolved.flags.model.as_deref().map(alondra_core::model_label::model_family)),
            effort: selected.and_then(|selection| selection.reasoning_effort.clone()).or_else(|| resolved.flags.effort.clone()),
            approach: Some(body.approach.unwrap_or_default()),
            chosen_by: resolved.launch_model().map_or_else(|| chooser(auto), |launched| launched.chosen_by),
            reason, title: task.title.clone(), provider: Some(provider), tags: body.tags.clone(),
        });
        crate::usage_pause::raise_over_pace_card(domain, alondra_core::now_millis());
        Ok(launched)
    }
}

/// The worktree a launch works in is the task's own from the moment it exists, before any node
/// runs in it: it is cleaned with the task and its subagents' worktrees go beside it, even when the
/// launch fails. A task that ended while the worktree was being made already had its close, so the
/// worktree goes at once, unless it holds work not saved.
pub(crate) async fn own_worktree(domain: &DomainRuntime, task: &TaskId, workplace: &Workplace) -> Result<(), DomainError> {
    let dir = workplace.dir();
    if workplace.chosen().is_none() || !dir.join(".git").is_file() {
        return Ok(());
    }
    let shown = dir.to_string_lossy();
    domain.set_task_worktree(task, &shown)
        .map_err(|error| DomainError::new("storage_unavailable", format!("could not record that {task} works in {shown}: {error:#}")))?;
    let state = domain.state().map_err(|error| DomainError::new("storage_unavailable", format!("{error:#}")))?;
    if !state.tasks.get(task).is_some_and(|ended| ended.state.terminal()) {
        return Ok(());
    }
    match crate::worktree::release_recorded_worktree(dir, false).await {
        Ok(crate::worktree::TaskWorktree::Unsaved(left)) => log!("the worktree of {task}, which ended while it started, is kept: {left}"),
        Ok(_) => {}
        Err(why) => log!("could not delete the worktree of {task}, which ended while it started: {why}"),
    }
    Err(DomainError::new("task_not_active", format!("Task is terminal: {task} ended while it started")))
}

/// The node took over the worktree Alondra chose for the task, or the one of the node it replaced.
fn record_worktree(domain: &DomainRuntime, agent: Option<&str>, workplace: &Workplace, inherited: Option<(&AgentId, &String)>) {
    let owned = match inherited {
        Some((old, dir)) => Some((dir.clone(), old.0.clone())),
        None => workplace.chosen().map(|dir| (dir.to_string_lossy().into_owned(), "alondra".to_owned())),
    };
    if let (Some(agent), Some((dir, from))) = (agent, owned)
        && let Err(error) = domain.set_inherited_worktree(agent, &dir, &from)
    {
        log!("could not record that {agent} took over {dir}: {error:#}");
    }
}

/// Stops the `claude --bg` node being replaced once its turn ends, so it is not cut mid-edit: past
/// the turn deadline it is interrupted, and then stopped either way. Its watch keeps the revival off
/// it meanwhile; if the stop fails it is handed back to the revival and the task keeps it.
async fn stop_replaced(domain: &DomainRuntime, old: &AgentId) -> Result<(), String> {
    let busy = || domain.live.node(&old.0).is_some_and(|node| node.busy);
    let deadline = domain.handovers.turn_deadline();
    if !until_idle(busy, deadline).await {
        let _ = domain.live.pushes.interrupt(&old.0);
        until_idle(busy, deadline).await;
    }
    let watched = domain.live.begin_watch(&old.0);
    let stopped = match domain.handovers.sessions().stop(&old.0).await {
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

/// What the node Alondra made a worktree for is told about where it works.
async fn made_for(dir: &Path) -> String {
    let branch = crate::worktree::branch_in(dir).await.map(|branch| format!(", branch {branch}")).unwrap_or_default();
    format!("You work in {}{branch}, the worktree Alondra made for this task. Do not create another worktree and do not use EnterWorktree.", dir.display())
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
pub async fn relaunch(domain: &DomainRuntime, home: &Path, task_id: &TaskId) -> Result<Value, DomainError> {
    relaunch_from(domain, home, task_id, Origin::Orchestrator).await
}

/// `relaunch`, for either origin. An automatic relaunch holds the task's launch guard already.
pub(crate) async fn relaunch_from(domain: &DomainRuntime, home: &Path, task_id: &TaskId, origin: Origin) -> Result<Value, DomainError> {
    let current = state(domain)?;
    let waiting = current.tasks.get(task_id)
        .filter(|task| task.state == alondra_core::domain::TaskState::NotStarted && task.agent_id.is_none() && !task.archived);
    let (Some(task), Some(closed)) = (waiting, domain.live.closed_task(&task_id.0)) else {
        return Err(DomainError::new("conflict", "This task is not waiting for a relaunch"));
    };
    let asked = request_choices(domain, &task.request_id).ok().flatten();
    let root = crate::domain_schedule_runner::options_at(home).ok().map(|file| file.projects_root(&crate::options::user_home()));
    let project_dir = asked.as_ref().and_then(|choices| choices.project.as_ref()).zip(root.as_ref()).map(|(project, root)| root.join(project));
    let original = domain.task_worktree(task_id).map_err(|error| DomainError::new("storage_unavailable", error.to_string()))?;
    let cwd = original.map(PathBuf::from).filter(|dir| dir.is_dir())
        .or(closed.cwd.map(PathBuf::from).filter(|dir| dir.is_dir()))
        .or(project_dir.filter(|dir| dir.is_dir()))
        .or(root)
        .and_then(|dir| dir.to_str().map(str::to_owned))
        .ok_or_else(|| DomainError::new("invalid_input", "no folder to launch the node in"))?;
    let mut body: LaunchBody = serde_json::from_value(json!({"name": closed.name, "cwd": cwd, "task": task_id}))
        .map_err(|error| DomainError::new("invalid_input", error.to_string()))?;
    body.relaunch_model = Some(task.model.as_ref().map_or(DEFAULT_MODEL, |m| m.model.as_str()).to_owned());
    launch_from(domain, home, body, |request| request_choices(domain, request).ok().flatten(), origin).await
}

const FOLLOW_UP_LIMIT: usize = 12_000;
pub(crate) const CANONICAL_CONTEXT_LIMIT: usize = 32_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CanonicalTaskContext {
    pub text: String,
    pub complete: bool,
    pub omitted_sources: Vec<String>,
}

pub(crate) fn canonical_task_context(state: &DomainState, task: &alondra_core::domain::Task) -> CanonicalTaskContext {
    use alondra_core::domain::{InteractionPayload, Recipient, Scope};
    let mut context = CanonicalTaskContext { text: format!("Task {}: {}\nRequest: {}", task.id, task.title, task.request_id), complete: true, omitted_sources: vec![] };
    let mut append = |source: String, text: String| {
        if text.is_empty() { return; }
        if context.text.chars().count() + text.chars().count() + 2 <= CANONICAL_CONTEXT_LIMIT {
            context.text.push_str("\n\n"); context.text.push_str(&text);
        } else {
            context.complete = false; context.omitted_sources.push(source);
        }
    };
    if let Some(request) = state.requests.get(&task.request_id) {
        append(format!("request:{}", request.id), format!("## Original request\n{}", request.body));
    } else {
        append(format!("request:{}", task.request_id), "[The original request is missing.]".into());
    }
    let plan = approved_plan(state, task);
    let plan_source = state.checklists.get(&task.id).map_or_else(|| format!("task:{}:approved-plan", task.id), |checklist| format!("interaction:{}", checklist.plan));
    append(plan_source, plan);
    let mut missing_inputs = Vec::new();
    let mut included_inputs = std::collections::BTreeSet::new();
    for record in state.lifecycle_journal.get(&task.id).into_iter().flat_map(|journal| journal.iter()) {
        let (input, text) = match record {
            alondra_core::domain::LifecycleRecord::Told { input, text, .. }
            | alondra_core::domain::LifecycleRecord::Answered { input, text, .. }
            | alondra_core::domain::LifecycleRecord::HandedBack { input, text, .. } => (input, text),
            _ => continue,
        };
        if !included_inputs.insert(input) { continue; }
        let source = format!("task:{}:input:{input}", task.id);
        match text {
            Some(text) => append(source, format!("## Public user input ({input})\n{text}")),
            None => missing_inputs.push(source),
        }
    }
    append(format!("task:{}:asked-changes", task.id), asked_changes(state, task));
    let mut public: Vec<_> = state.interactions.values().filter(|interaction| interaction.scope == (Scope::Task { task_id: task.id.clone() }) && interaction.recipient == Recipient::User).collect();
    public.sort_by_key(|interaction| interaction.order);
    for interaction in public {
        if let InteractionPayload::Question { options, answer: Some(answer) } | InteractionPayload::ActionRequest { options, answer: Some(answer) } = &interaction.payload {
            let choice = options.iter().find(|option| option.id == answer.option_id).map_or(answer.option_id.as_str(), |option| option.label.as_str());
            append(format!("interaction:{}", interaction.id), format!("Public user decision {}: {} (revision {})", interaction.id, choice, answer.at_revision));
        }
    }
    for (id, revision) in &task.documents {
        append(format!("document:{id}@{revision}"), format!("Document attached to this task: {id}, revision {revision}"));
    }
    let dependencies: std::collections::BTreeSet<_> = task.dependencies.iter().chain(&task.delivered_dependencies)
        .chain(task.stack.iter().map(|stack| &stack.upstream_task)).collect();
    for dependency in dependencies {
        let source = format!("dependency:{dependency}");
        match state.tasks.get(dependency) {
            Some(earlier) => {
                let mut text = format!("## Earlier work {}: {}", earlier.id, earlier.title);
                if let Some(result) = earlier.final_presentation.as_ref().and_then(|id| state.interactions.get(id)) {
                    text.push_str(&format!("\n{}", result.body));
                    if result.shortened {
                        missing_inputs.push(format!("interaction:{}", result.id));
                    }
                }
                for pr in state.task_prs(earlier) {
                    text.push_str(&format!("\nPull request: https://{}/{}/pull/{}", pr.host, pr.repository, pr.number));
                }
                append(source, text);
            }
            None => missing_inputs.push(source),
        }
    }
    if !missing_inputs.is_empty() {
        context.complete = false; context.omitted_sources.extend(missing_inputs);
    }
    if !state.requests.contains_key(&task.request_id) {
        context.complete = false; context.omitted_sources.push(format!("request:{}", task.request_id));
    }
    context
}

fn prefixed(prefix: &str, text: String) -> String {
    if text.is_empty() { text } else { format!("{prefix}{text}") }
}

fn clipped(text: &str, limit: usize, mark: &str) -> alondra_core::domain::Clipped {
    if text.chars().count() <= limit {
        return alondra_core::domain::Clipped { text: text.to_owned(), shortened: false };
    }
    alondra_core::domain::Clipped { text: format!("{}{mark}", text.chars().take(limit).collect::<String>()), shortened: true }
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
            Some(result) => alondra_core::domain::result_excerpt(&result.body).text,
            None => "No final result was recorded for it.".to_owned(),
        };
        let mut entry = format!("### “{}” ({status})\n{body}", earlier.title);
        for pr in state.task_prs(earlier) {
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
    clipped(&format!("## Earlier work this task builds on\n\n{}", entries.join("\n\n")), FOLLOW_UP_LIMIT, "").text
}

/// The approved plan this node needs to implement, including its predecessor's own plan when
/// the task reopens, with only this task's points assigned when the plan is in phases.
fn approved_plan(state: &DomainState, task: &alondra_core::domain::Task) -> String {
    use alondra_core::domain::{checklist_items, InteractionPayload, Scope};
    let Some(checklist) = state.checklists.get(&task.id) else { return String::new() };
    let Some(source) = state.interactions.get(&checklist.plan) else { return String::new() };
    if source.scope == (Scope::Task { task_id: task.id.clone() }) && state.pending_reopening(&task.id).is_none() {
        return String::new();
    }
    let plan = match &source.payload {
        InteractionPayload::Delivery { .. } => source.card.plan.as_deref().unwrap_or_default(),
        _ => source.body.as_str(),
    };
    let part = if checklist.items.len() < checklist_items(plan).len() {
        let numbers: Vec<&str> = checklist.items.iter().map(|item| item.id.as_str()).collect();
        format!(" Your part of it is points {}; the other points are other tasks: do not implement them.", numbers.join(", "))
    } else {
        String::new()
    };
    format!("## The plan the user approved

The user approved this plan to be implemented: it is already this task's approved plan, so do not plan again or send it for approval.{part} Implement it, and mark every point of yours done with its evidence in your final result. If a point turns out wrong or impossible, send a revised plan with ExitPlanMode.

{}", plan.trim())
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
pub fn record(domain: &DomainRuntime, decision: Decision) {
    if let Err(error) = domain.record_model_decision(&decision) {
        log!("could not record the model decision for {}: {error:#}", decision.task_id);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendBody {
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
pub async fn send(domain: &DomainRuntime, home: &Path, actor: &Actor, body: SendBody) -> Result<Value, DomainError> {
    let current = state(domain)?;
    let agent_id = agent(&current, &body.node)?;
    if let Some(paused) = domain.live.gate.paused(domain, &agent_id.0) {
        let why = match paused.reason {
            crate::pauses::Reason::ByUser => format!("{} paused it and only {} resumes it", crate::domain_messages::user_name(), crate::domain_messages::user_name()),
            _ => "Alondra paused it for the machine and resumes it by itself in its turn, within the limit".into(),
        };
        return Err(DomainError::new("node_paused", format!("{} is paused: {why}. Nothing was sent; a message now would start it again past the limit.", body.node)));
    }
    let node_runs =domain.live.mods.session(&agent_id.0).and_then(|reported| reported.model);
    let node_model = node_runs.as_deref().map(alondra_core::model_label::model_family);
    if let Some(task_id) = &body.task {
        let task = current.tasks.get(task_id).ok_or_else(|| DomainError::new("not_found", "task does not exist"))?;
        if task.agent_id.as_ref() != Some(&agent_id) {
            return Err(DomainError::new("not_assigned", format!("{task_id} is not {}'s task: a node works only on the task it was launched for. Launch a new node for {task_id} with launch.", body.node)));
        }
        if task.over() {
            return Err(DomainError::new("task_closed", format!("{task_id} is closed, and its node closes with it. For more work, create a new task with create_task and dependencies [{task_id}]: its node gets this task's result.")));
        }
    }
    let text = match body.approach {
        Some(approach) => {
            let provider = body.task.as_ref().and_then(|id| current.tasks.get(id)).map(|task| task.provider);
            format!("{}\n\n{}", body.text.trim_end(), approach.how_to_work_for(provider).join("\n"))
        }
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
            reason: given(body.reason.as_deref()), title: task.title.clone(), provider: Some(task.provider), tags: None,
        });
    }
    Ok(delivered)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchPlanBody {
    task_id: TaskId,
    launch: crate::launch_plans::LaunchPlan,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateLaunchPlanBody {
    task_id: TaskId,
    changes: LaunchPlanChanges,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LaunchPlanChanges {
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "non_null")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "non_null", alias = "reasoning_effort")]
    effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "non_null")]
    approach: Option<alondra_core::options::Approach>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "non_null")]
    tags: Option<alondra_core::task_tags::TaskTags>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "non_null")]
    reason: Option<String>,
}

fn non_null<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

fn plan_storage(error: anyhow::Error) -> DomainError {
    log!("could not access the launch plan: {error:#}");
    DomainError::new("storage_unavailable", "the launch plan could not be read or stored")
}

pub fn get_launch_plan(domain: &DomainRuntime, task_id: &TaskId) -> Result<Value, DomainError> {
    let store = domain.store.lock_or_exit("the domain store");
    if !state(domain)?.tasks.contains_key(task_id) {
        return Err(DomainError::new("not_found", "task does not exist"));
    }
    let launch = store.launch_plan(task_id).map_err(plan_storage)?
        .ok_or_else(|| DomainError::new("not_found", "task has no stored launch plan"))?;
    Ok(json!({"task": task_id, "launch": launch}))
}

/// Replaces the plan Alondra starts a task with by itself.
pub fn set_launch_plan(domain: &DomainRuntime, home: &Path, body: LaunchPlanBody) -> Result<Value, DomainError> {
    change_launch_plan(domain, home, &body.task_id, Some(body.launch), None)
}

pub fn update_launch_plan(domain: &DomainRuntime, home: &Path, body: UpdateLaunchPlanBody) -> Result<Value, DomainError> {
    let changes = serde_json::to_value(body.changes).map_err(|error| DomainError::new("invalid_input", error.to_string()))?;
    if changes.as_object().is_none_or(|fields| fields.is_empty()) {
        return Err(DomainError::new("invalid_input", "changes must contain at least one launch-plan field"));
    }
    change_launch_plan(domain, home, &body.task_id, None, Some(changes))
}

fn editable_plan<'a>(state: &'a DomainState, task_id: &TaskId, launched: bool) -> Result<&'a alondra_core::domain::Task, DomainError> {
    let task = state.tasks.get(task_id).ok_or_else(|| DomainError::new("not_found", "task does not exist"))?;
    if task.archived {
        return Err(DomainError::new("task_not_active", format!("{} is closed", task.id)));
    }
    if let Some(why) = crate::plan_reviews::held(state, task_id) {
        return Err(DomainError::new("plan_review_pending", format!(
            "{why}. Use settle_plan_review with the plan_changed outcome: launch-plan changes cannot bypass a plan review")));
    }
    if task.state.terminal() {
        if !state.requests.get(&task.request_id).is_some_and(|request| !request.closed) {
            return Err(DomainError::new("task_not_active", "Open Request required to prepare a task for reopening"));
        }
        let ended = matches!(task.wrap_up.node, Some(alondra_core::domain::NodeEnd::Closed | alondra_core::domain::NodeEnd::Gone))
            || (task.agent_id.is_none() && task.wrap_up.node.is_none());
        if task.ending.is_some() || !ended {
            return Err(DomainError::new("conflict", "The previous node must have ended before preparing this task's next launch choices"));
        }
        return Ok(task);
    }
    if task.over() {
        return Err(DomainError::new("task_not_active", format!("{} is closed", task.id)));
    }
    let prior_native = state.lifecycle_journal.get(task_id).is_some_and(|records| records.iter().any(|record| matches!(record,
        alondra_core::domain::LifecycleRecord::Admitted { execution: Some(_), .. } | alondra_core::domain::LifecycleRecord::ResourceAvailable)));
    if !matches!(task.state, alondra_core::domain::TaskState::NotStarted | alondra_core::domain::TaskState::WaitingDependency) || task.started || task.agent_id.is_some() || ((launched || prior_native) && state.pending_reopening(task_id).is_none()) {
        return Err(DomainError::new("conflict", "the launch plan can only change before the task has started"));
    }
    Ok(task)
}

fn change_launch_plan(domain: &DomainRuntime, home: &Path, task_id: &TaskId,
    replacement: Option<crate::launch_plans::LaunchPlan>, changes: Option<Value>) -> Result<Value, DomainError> {
    let _launching = Launching::begin(domain, task_id)?;
    let current = state(domain)?;
    let task = editable_plan(&current, task_id, false)?;
    let preparing_reopening = task.state.terminal() || current.pending_reopening(task_id).is_some();
    if preparing_reopening && (replacement.is_some() || changes.as_ref().and_then(Value::as_object)
        .is_none_or(|fields| fields.keys().any(|field| !matches!(field.as_str(), "model" | "effort" | "reason")))) {
        return Err(DomainError::new("invalid_input", "A terminal or reopened task can only change its next model, reasoning effort and reason; its mission, folder and approved scope stay unchanged"));
    }
    let original = domain.launch_plan(task_id).map_err(plan_storage)?;
    let plan = match replacement {
        Some(plan) => plan,
        None => {
            let original = original.as_ref().ok_or_else(|| DomainError::new("not_found", "task has no stored launch plan"))?;
            let mut merged = serde_json::to_value(original).map_err(|error| DomainError::new("invalid_input", error.to_string()))?;
            merged.as_object_mut().unwrap().extend(changes.as_ref().unwrap().as_object().unwrap().clone());
            serde_json::from_value(merged).map_err(|error| DomainError::new("invalid_input", error.to_string()))?
        }
    };
    if let Some(choices) = request_choices(domain, &task.request_id).map_err(plan_storage)?
        && choices.options.effort != alondra_core::options::AUTO {
        return Err(DomainError::new("invalid_input", format!("the user chose effort {} for this request, which fixes its launch model and effort", choices.options.effort)));
    }
    if preparing_reopening {
        crate::launch_plans::validate_reopening(domain, home, &task.request_id, &plan)?;
    } else {
        crate::launch_plans::validate(domain, home, &task.request_id, &plan)?;
    }
    let mut store = domain.store.lock_or_exit("the domain store");
    let now = state(domain)?;
    editable_plan(&now, task_id, store.task_was_launched(task_id).map_err(plan_storage)?)?;
    if now.revision != current.revision || store.launch_plan(task_id).map_err(plan_storage)? != original {
        return Err(DomainError::new("conflict", "the task or its launch plan changed; read it again before updating"));
    }
    store.change_launch_plan(task_id, &plan, changes.as_ref(), alondra_core::now_millis()).map_err(plan_storage)?;
    Ok(json!({"task": task_id, "stored": true}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeBody {
    node: String,
}

/// Aborts the turn an Agent's session is running, to stop a call it is stuck in; its mod does it.
pub fn interrupt(domain: &DomainRuntime, body: NodeBody) -> Result<Value, DomainError> {
    let current = state(domain)?;
    let agent_id = agent(&current, &body.node)?;
    domain.live.pushes.interrupt(&agent_id.0)
        .map_err(|_| DomainError::new("session_unreachable", "the node is not connected to Alondra: its session is stopped or its Claude Code mod is not running. Nothing was sent"))?;
    Ok(json!({"interrupted": agent_id.0}))
}

#[cfg(any(test, feature = "test-hooks"))]
pub mod tests;
