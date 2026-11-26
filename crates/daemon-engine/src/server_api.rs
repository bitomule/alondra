//! What the server asks of the engine beyond the runtime's own operations, each part through its
//! port of `DomainApi`: the live view, the accounts, the messages to nodes, the orchestrator's
//! tools, the pull requests and the MCP servers. Each answer is the engine's own function.
use crate::contexts::ContextsOf as _;
use crate::live::{LiveChangeKey, LiveOf as _};
use alondra_core::accounts::{AccountChange, AccountKey, Provider};
use alondra_core::activity::Activity;
use alondra_core::model_selection::{ModelProfile, RoleModelPolicy, SelectionDecision, TaskSelection};
use alondra_core::options::{ProjectEntry, Resolved};
use alondra_core::{Event, Millis, Node};
use alondra_domain_model::{
    Actor, AgentId, Command, DeliveryTarget, DomainError, DomainState, InteractionId, LaunchPlan, McpNodeReading, MergeVerdict, PrId, PullRequest,
    RequestId, Task, TaskId,
};
use alondra_engine_kernel::sdk_backend::SdkOf as _;
use alondra_engine_ports::Runtime;
use alondra_engine_ports::domain_api::{
    AccountRefusal, AccountsApi, Authorize, LiveApi, McpApi, MessagesApi, Pending, PullRequestsApi, TaskContextQuery, ToolWork, ToolsApi,
};
use alondra_engine_ports::github::{GitHub, PrFacts, PrMerger, PrObserver};
use alondra_engine_ports::journal::OutboxMessage;
use alondra_engine_ports::live_view::{SessionReading, Used};
use alondra_engine_ports::mcp::{McpProbes, Opened, Refused as McpRefused, Target as McpTarget};
use alondra_engine_ports::orchestrator::Exhausted;
use alondra_foundation::changes::{Changed, Cursor, Delta};
use alondra_view_model::Lang;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Api;

fn refusal(refused: crate::accounts::ChangeRefused, lang: Lang) -> AccountRefusal {
    match refused {
        crate::accounts::ChangeRefused::Domain(error) => AccountRefusal::Domain(error),
        crate::accounts::ChangeRefused::Store(refused) => AccountRefusal::Conflict(refused.message(lang)),
    }
}

fn answer(reply: crate::reply::Reply) -> (u16, Value) {
    (reply.status.as_u16(), reply.body)
}

impl LiveApi for Api {
    fn node(&self, runtime: &dyn Runtime, session: &str) -> Option<Node> {
        runtime.live().node(session)
    }

    fn nodes(&self, runtime: &dyn Runtime) -> Vec<Node> {
        runtime.live().nodes()
    }

    fn threads(&self, runtime: &dyn Runtime, session: &str) -> Option<Vec<Activity>> {
        runtime.live().threads(session)
    }

    fn closed_sessions(&self, runtime: &dyn Runtime) -> Vec<String> {
        runtime.live().closed_sessions()
    }

    fn apply(&self, runtime: &dyn Runtime, event: &Event) {
        runtime.live().apply(event);
    }

    fn session(&self, runtime: &dyn Runtime, session: &str) -> Option<SessionReading> {
        runtime.live().mods.session(session).map(|reported| reported.reading())
    }

    fn interrupt(&self, runtime: &dyn Runtime, session: &str) -> bool {
        runtime.live().pushes.interrupt(session).is_ok()
    }

    fn sandbox(&self, runtime: &dyn Runtime) -> bool {
        runtime.live().sandbox.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn touch(&self, runtime: &dyn Runtime, client: &str) {
        runtime.live().touch(client);
    }

    fn touched_since(&self, runtime: &dyn Runtime, clients: &[&str], since: Millis) -> bool {
        runtime.live().touched_since(clients, since)
    }

    fn usage(&self, runtime: &dyn Runtime) -> Option<Exhausted> {
        runtime.live().usage_pause.current()
    }

    fn usage_changes(&self, runtime: &dyn Runtime) -> tokio::sync::watch::Receiver<Option<Exhausted>> {
        runtime.live().usage_pause.subscribe()
    }

    fn hooks_changes(&self, runtime: &dyn Runtime) -> tokio::sync::watch::Receiver<()> {
        runtime.live().changed.subscribe()
    }

    fn hooks_cursor(&self, runtime: &dyn Runtime) -> Cursor {
        runtime.live().change_cursor()
    }

    fn hooks_since(&self, runtime: &dyn Runtime, cursor: &Cursor) -> Delta<String> {
        let delta = runtime.live().changes_since(cursor);
        let changed = match delta.changed {
            Changed::Reset => Changed::Reset,
            Changed::Keys(keys) => Changed::Keys(keys.into_iter().map(|LiveChangeKey::Node(session)| session).collect()),
        };
        Delta { through: delta.through, changed }
    }

    fn reports_changes(&self, runtime: &dyn Runtime) -> tokio::sync::watch::Receiver<()> {
        runtime.live().mods.changed.subscribe()
    }

    fn reports_cursor(&self, runtime: &dyn Runtime) -> Cursor {
        runtime.live().mods.change_cursor()
    }

    fn reports_since(&self, runtime: &dyn Runtime, cursor: &Cursor) -> Delta<String> {
        runtime.live().mods.changes_since(cursor)
    }

    fn contexts_version(&self, runtime: &dyn Runtime) -> u64 {
        runtime.contexts().version()
    }

    fn contexts_cursor(&self, runtime: &dyn Runtime) -> Cursor {
        runtime.contexts().change_cursor()
    }

    fn contexts_since(&self, runtime: &dyn Runtime, cursor: &Cursor) -> Delta<TaskId> {
        runtime.contexts().changes_since(cursor)
    }

    fn context_used(&self, runtime: &dyn Runtime, task: &TaskId) -> Option<Used> {
        runtime.contexts().used(task)
    }

    fn wait_file(&self, runtime: &dyn Runtime, provider: Provider, instance: &str) -> Option<PathBuf> {
        crate::sdk_live::wait_file(runtime, provider, instance)
    }
}

impl AccountsApi for Api {
    fn read(&self, runtime: &dyn Runtime, home: &Path, lang: Lang, provider_accounts: bool) -> (u16, Value) {
        answer(crate::accounts::for_client(crate::accounts::read(runtime, home, lang), provider_accounts))
    }

    fn write(&self, runtime: &dyn Runtime, home: &Path, body: &Value, lang: Lang, provider_accounts: bool) -> (u16, Value) {
        answer(crate::accounts::for_client(crate::accounts::write(runtime, home, body, lang), provider_accounts))
    }

    fn check(&self, runtime: &dyn Runtime, home: &Path) {
        crate::accounts::check(runtime, home);
    }

    fn check_now(&self, runtime: &dyn Runtime, home: &Path) {
        crate::accounts::check_now(runtime, home);
    }

    fn refresh_models(&self, runtime: &dyn Runtime, home: &Path, key: &AccountKey) -> Result<(), String> {
        crate::accounts::refresh_models(runtime, home, key)
    }

    fn select_for_active_model(&self, runtime: &dyn Runtime, task: &TaskId, intent: &TaskSelection) -> Result<SelectionDecision, DomainError> {
        crate::accounts::select_for_active_model(runtime, task, intent)
    }

    fn connect(&self, runtime: &dyn Runtime, home: &Path, actor: &Actor, provider: Provider, authorize: Authorize<'_>, lang: Lang) -> Result<(), AccountRefusal> {
        crate::accounts::connect(runtime, home, actor, provider, authorize).map_err(|refused| refusal(refused, lang))
    }

    fn change(&self, runtime: &dyn Runtime, home: &Path, actor: &Actor, provider: Provider, change: &AccountChange, authorize: Authorize<'_>, lang: Lang)
        -> Result<(), AccountRefusal> {
        crate::accounts::change_for_provider_authorized(runtime, home, actor, provider, change, authorize).map_err(|refused| refusal(refused, lang))
    }

    fn set_credential(&self, runtime: &dyn Runtime, home: &Path, actor: &Actor, key: &AccountKey, secret: Option<alondra_core::antigravity_account::Secret>,
        authorize: Authorize<'_>, lang: Lang) -> Result<(), AccountRefusal> {
        crate::accounts::set_credential_authorized(runtime, home, actor, key, secret, authorize).map_err(|refused| refusal(refused, lang))
    }

    fn set_model_profile(&self, runtime: &dyn Runtime, home: &Path, actor: &Actor, provider: Provider, profile: ModelProfile, policy: &RoleModelPolicy,
        expected_config_version: &str, authorize: Authorize<'_>, lang: Lang) -> Result<(), AccountRefusal> {
        crate::accounts::set_model_profile_authorized(runtime, home, actor, provider, profile, policy, expected_config_version, authorize)
            .map_err(|refused| refusal(refused, lang))
    }

    fn set_order(&self, runtime: &dyn Runtime, home: &Path, actor: &Actor, order: &[AccountKey], expected_config_version: &str, authorize: Authorize<'_>, lang: Lang)
        -> Result<(), AccountRefusal> {
        crate::accounts::set_order_authorized(runtime, home, actor, order, expected_config_version, authorize).map_err(|refused| refusal(refused, lang))
    }
}

impl MessagesApi for Api {
    fn deliver<'a>(&'a self, runtime: &'a dyn Runtime, home: &'a Path, user: &'a Actor, name: &'a str, agent: &'a AgentId, body: Value, as_user: bool)
        -> Option<Pending<'a, Result<Value, DomainError>>> {
        let body = serde_json::from_value::<crate::domain_messages::MessageBody>(body).ok()?;
        Some(Box::pin(crate::domain_messages::deliver(runtime, home, user, name, agent, body, as_user)))
    }

    fn relayed<'a>(&'a self, runtime: &'a dyn Runtime, home: &'a Path, sender: &'a str, agent: AgentId, body: Option<&'a Value>)
        -> Pending<'a, Result<Value, DomainError>> {
        Box::pin(crate::domain_messages::relayed(runtime, home, sender, agent, body))
    }

    fn relayed_to_task(&self, runtime: &dyn Runtime, home: &Path, sender: &str, task: &TaskId, body: Option<&Value>) -> Result<Value, DomainError> {
        crate::domain_messages::relayed_to_task(runtime, home, sender, task, body)
    }

    fn resolve_conflicts<'a>(&'a self, runtime: &'a dyn Runtime, home: &'a Path, user: &'a Actor, name: &'a str, pr: &'a PrId)
        -> Pending<'a, Result<Value, DomainError>> {
        Box::pin(crate::domain_messages::resolve_conflicts(runtime, home, user, name, pr))
    }

    fn tell_sdk_node(&self, runtime: &dyn Runtime, task: &TaskId, author: &str, text: &str, command_id: &str, from_user: bool, answers: Option<InteractionId>)
        -> Result<(), DomainError> {
        crate::domain_messages::tell_sdk_node_answering(runtime, task, author, text, command_id, from_user, answers)
    }

    fn successor(&self, runtime: &dyn Runtime, agent: &AgentId) -> Option<(TaskId, AgentId)> {
        crate::sdk_handover::successor(runtime, agent)
    }

    fn render(&self, state: &DomainState, message: &OutboxMessage) -> String {
        crate::domain_transport::render(state, message)
    }

    fn from_user(&self, state: &DomainState, message: &OutboxMessage) -> bool {
        crate::domain_transport::from_user(state, message)
    }
}

fn tool<T: serde::de::DeserializeOwned>(request: &Value, work: impl FnOnce(T) -> ToolWork) -> Result<ToolWork, serde_json::Error> {
    serde_json::from_value(request.clone()).map(work)
}

impl ToolsApi for Api {
    fn orchestrator(&self, runtime: &dyn Runtime, bearer: Option<&str>, what: Option<&str>) -> Result<Actor, DomainError> {
        crate::domain_orchestrator_tools::orchestrator(runtime, bearer, what)
    }

    fn launch(&self, runtime: Arc<dyn Runtime>, home: PathBuf, request: &Value) -> Result<ToolWork, serde_json::Error> {
        tool(request, |body| Box::pin(async move {
            let choices = Arc::clone(&runtime);
            crate::domain_orchestrator_tools::launch_replacing(runtime, home, body, move |request| {
                crate::domain_orchestrator_tools::request_choices(&*choices, request).ok().flatten()
            }).await
        }))
    }

    fn set_launch_plan(&self, runtime: Arc<dyn Runtime>, home: PathBuf, request: &Value) -> Result<ToolWork, serde_json::Error> {
        tool(request, |body| Box::pin(async move { crate::domain_orchestrator_tools::set_launch_plan(&*runtime, &home, body) }))
    }

    fn update_launch_plan(&self, runtime: Arc<dyn Runtime>, home: PathBuf, request: &Value) -> Result<ToolWork, serde_json::Error> {
        tool(request, |body| Box::pin(async move { crate::domain_orchestrator_tools::update_launch_plan(&*runtime, &home, body) }))
    }

    fn send(&self, runtime: Arc<dyn Runtime>, home: PathBuf, actor: Actor, request: &Value) -> Result<ToolWork, serde_json::Error> {
        tool(request, |body| Box::pin(async move { crate::domain_orchestrator_tools::send(&*runtime, &home, &actor, body).await }))
    }

    fn interrupt(&self, runtime: Arc<dyn Runtime>, request: &Value) -> Result<ToolWork, serde_json::Error> {
        tool(request, |body| Box::pin(async move { crate::domain_orchestrator_tools::interrupt(&*runtime, body) }))
    }

    fn get_launch_plan(&self, runtime: &dyn Runtime, task: &TaskId) -> Result<Value, DomainError> {
        crate::domain_orchestrator_tools::get_launch_plan(runtime, task)
    }

    fn tracked(&self, runtime: Arc<dyn Runtime>, bearer: String, identity: Option<String>, kind: &'static str, request: Value, work: ToolWork)
        -> Pending<'static, Result<Value, DomainError>> {
        Box::pin(async move {
            crate::domain_runtime::batch_operations::execute(runtime, &bearer, identity.as_deref(), kind, request, work).await
        })
    }

    fn tasks_in_progress(&self, runtime: &dyn Runtime, machine: Value, usage: Value) -> Result<Value, DomainError> {
        crate::domain_orchestrator_reads::tasks_in_progress(runtime, machine, usage)
    }

    fn node(&self, runtime: &dyn Runtime, key: &str) -> Result<Value, DomainError> {
        crate::domain_orchestrator_reads::node(runtime, key)
    }

    fn task_context(&self, runtime: &dyn Runtime, bearer: &str, query: TaskContextQuery) -> Result<Value, DomainError> {
        crate::task_context::read(runtime, bearer, query)
    }

    fn relaunch<'a>(&'a self, runtime: &'a dyn Runtime, home: &'a Path, task: &'a TaskId) -> Pending<'a, Result<Value, DomainError>> {
        Box::pin(crate::domain_orchestrator_tools::relaunch(runtime, home, task))
    }

    fn authorize_user_request_selection(&self, runtime: &dyn Runtime, resolved: &mut Resolved, explicit_account: bool) {
        crate::domain_orchestrator_tools::authorize_user_request_selection(runtime, resolved, explicit_account);
    }

    fn validate_request_selection(&self, runtime: &dyn Runtime, resolved: &Resolved) -> Result<(), DomainError> {
        crate::domain_orchestrator_tools::validate_request_selection(runtime, resolved)
    }

    fn validate_launch_plan(&self, runtime: &dyn Runtime, home: &Path, request: &RequestId, plan: &LaunchPlan) -> Result<(), DomainError> {
        crate::launch_plans::validate(runtime, home, request, plan)
    }

    fn stamp_project(&self, runtime: &dyn Runtime, home: &Path, command: &mut Command) {
        crate::launch_plans::stamp_project(runtime, home, command);
    }
}

impl PullRequestsApi for Api {
    fn catch_up_task<'a>(&'a self, runtime: &'a dyn Runtime, github: &'a GitHub, source: &'a dyn PrObserver, task: &'a TaskId, is_final: bool)
        -> Pending<'a, anyhow::Result<()>> {
        Box::pin(crate::domain_github::catch_up_task_using(runtime, github, source, task, is_final))
    }

    fn observe(&self, runtime: &dyn Runtime, pr: &PrId, facts: &PrFacts) -> anyhow::Result<()> {
        crate::domain_github::observe(runtime, pr, facts)
    }

    fn record_verdict(&self, runtime: &dyn Runtime, pr: &PullRequest, verdict: MergeVerdict) -> anyhow::Result<()> {
        crate::domain_verdicts::record(runtime, pr, verdict)
    }

    fn draft_refusal<'a>(&'a self, state: &'a DomainState, targets: &'a [DeliveryTarget], merger: &'a dyn PrMerger) -> Pending<'a, Option<DomainError>> {
        Box::pin(crate::domain_merges::draft_refusal(state, targets, merger))
    }

    fn leftovers<'a>(&'a self, runtime: &'a dyn Runtime, task: &'a Task) -> Pending<'a, Vec<(String, String)>> {
        Box::pin(crate::task_worktrees::leftovers(runtime, task))
    }
}

impl McpApi for Api {
    fn connect<'a>(&'a self, runtime: &'a dyn Runtime, target: &'a McpTarget) -> Pending<'a, Result<Opened, DomainError>> {
        Box::pin(crate::mcp_waits::connect(runtime, target))
    }

    fn give_back<'a>(&'a self, runtime: Arc<dyn Runtime>, target: &'a McpTarget, address: String) -> Pending<'a, Result<(), DomainError>> {
        Box::pin(crate::mcp_waits::give_back(runtime, target, address))
    }

    fn reconnect(&self, runtime: &dyn Runtime, target: &McpTarget) -> Result<usize, DomainError> {
        crate::mcp_waits::reconnect(runtime, target)
    }

    fn sign_out(&self, runtime: &dyn Runtime, target: &McpTarget) -> Result<String, DomainError> {
        crate::mcp_waits::sign_out(runtime, target)
    }

    fn check(&self, runtime: &dyn Runtime, probes: Arc<McpProbes>, project: String, directory: PathBuf, done: Box<dyn FnOnce() + Send>) -> bool {
        let (Some((backend, _)), Ok(hosts)) = (runtime.sdk(), alondra_engine_ports::agent_host::of(runtime.ports())) else { return false };
        crate::mcp_probe::check(probes, hosts, backend.clone(), project, directory, done)
    }

    fn remove(&self, runtime: &dyn Runtime, nodes: &[McpNodeReading], probes: &McpProbes, projects: &[ProjectEntry], subject: &str)
        -> Result<String, (Option<String>, McpRefused)> {
        let (Some((backend, _)), Ok(hosts)) = (runtime.sdk(), alondra_engine_ports::agent_host::of(runtime.ports())) else {
            return Err((None, McpRefused::Gone));
        };
        crate::mcp_remove::remove(nodes, probes, projects, &*hosts, backend, subject)
    }
}
