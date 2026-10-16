//! The routes the TUI and the CLI call on a domain daemon beyond the domain itself: the
//! live runtime of the Mac and its sessions, and what a session is doing. The domain stays
//! the only authority; nothing here decides an action.
mod close;
mod live;
mod new_work;
mod pr_details;
mod tasks;

use crate::auth::Signed;
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{
    AgentId, DomainError, DomainState, InteractionPayload, Recipient, Scope, TaskId,
};
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{post, put};
use axum::{Extension, Json, Router};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub(crate) use crate::node_close::Sessions;
pub(crate) use new_work::Github;
pub(crate) use live::{ClosedNode, Live};
pub(crate) use pr_details::{hand_back_conflicts, GhView, Merge, PrView};
#[cfg(test)]
pub(crate) use pr_details::FakePrView;

const DETAILS_INTERVAL: Duration = Duration::from_secs(60);

pub(crate) struct Client {
    shared: std::sync::Weak<Self>,
    pub(crate) read_work: crate::blocking_work::BlockingWork,
    pub(crate) domain: Arc<DomainRuntime>,
    home: PathBuf,
    pub(crate) gauges: crate::machine::Gauges,
    pub(crate) views_gauges: std::sync::Mutex<Option<(std::time::Instant, Vec<alondra_core::views::Gauge>, Vec<alondra_core::views::Gauge>)>>,
    pub(crate) details: pr_details::PrDetails,
    pr_view: std::sync::Mutex<Arc<dyn PrView>>,
    pub(crate) views: crate::views::cache::Cache,
    pub(crate) feeds: crate::views::feed::Feeds,
    sessions: std::sync::Mutex<Arc<dyn Sessions>>,
    github: std::sync::Mutex<Arc<dyn Github>>,
    options: std::sync::Mutex<Option<alondra_core::options::OptionsFile>>,
    issue_cache: tokio::sync::Mutex<Option<crate::issues::IssueList>>,
    pub(crate) live_reads: crate::views::live::LiveReadCache,
    pub(crate) live_snapshots: crate::views::live::LiveSnapshots,
    device_languages: std::sync::Mutex<std::collections::HashMap<String, alondra_core::views::Lang>>,
    device_languages_path: PathBuf,
}

/// What a route answers: the status and the JSON body both the local listener and the relay
/// carry.
pub(crate) struct Reply {
    pub status: StatusCode,
    pub body: Value,
}

impl Reply {
    fn ok(body: Value) -> Self {
        Self { status: StatusCode::OK, body }
    }

    fn error(error: DomainError) -> Self {
        Self { status: crate::domain_api::status_for(&error), body: json!(error) }
    }

    pub(crate) fn into_value(self) -> Value {
        json!({"status": self.status.as_u16(), "body": self.body})
    }
}

impl IntoResponse for Reply {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum Write {
    Close(String),
    NewRequest,
    OrchestratorMessage,
    Presence,
    CancelReason(String),
    ResolveConflicts(String),
}

/// The writes of this module by path, the way both the local listener and the relay name them.
pub(crate) fn parse_write(path: &str) -> Option<Write> {
    match path {
        "/requests" => return Some(Write::NewRequest),
        "/activity" => return Some(Write::Presence),
        _ => {}
    }
    if path == "/orchestrator/messages" {
        return Some(Write::OrchestratorMessage);
    }
    if let Some(id) = path.strip_prefix("/pull-requests/").and_then(|rest| rest.strip_suffix("/resolve-conflicts")) {
        return Some(id).filter(|id| plain_id(id)).map(|id| Write::ResolveConflicts(id.to_owned()));
    }
    path.strip_prefix("/agents/")?.strip_suffix("/close")
        .filter(|id| plain_id(id)).map(|id| Write::Close(id.to_owned()))
}

pub(crate) fn approved_write_path(path: &str) -> bool {
    parse_write(path).is_some()
}

/// The writes that replace something in place, by path.
pub(crate) fn parse_put(path: &str) -> Option<Write> {
    path.strip_prefix("/tasks/")?.strip_suffix("/cancel-reason")
        .filter(|id| plain_id(id)).map(|id| Write::CancelReason(id.to_owned()))
}

/// The writes that are one domain command, which the relay journals atomically with it.
pub(crate) fn atomic_write_path(path: &str) -> bool {
    matches!(parse_write(path), Some(Write::NewRequest | Write::OrchestratorMessage))
}

#[derive(Debug, PartialEq)]
pub(crate) enum Route {
    Runtime,
    Activity(String),
    Search(String),
    FileChunk { reference: String, offset: u64, version: Option<String> },
    Options(Option<String>),
    Projects,
    Issues { refresh: bool },
    TaskHistory(String),
}

fn plain_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Exactly one named parameter with a value.
fn single_query(query: &str, name: &str) -> Option<String> {
    let mut pairs = url::form_urlencoded::parse(query.as_bytes());
    let (key, value) = pairs.next()?;
    (key == name && !value.is_empty() && value.len() <= 2048 && pairs.next().is_none()).then(|| value.into_owned())
}

fn search_query(query: &str) -> Option<String> {
    single_query(query, "q")
}

fn file_chunk_query(query: &str) -> Option<Route> {
    let (mut reference, mut offset, mut version) = (None, None, None);
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "path" if reference.is_none() && !value.is_empty() && value.len() <= 2048 && !value.contains('\0') => {
                reference = Some(value.into_owned());
            }
            "offset" if offset.is_none() && !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
                offset = value.parse::<u64>().ok();
            }
            "version" if version.is_none() && !value.is_empty() && value.len() <= 256
                && value.bytes().all(|b| b.is_ascii_digit() || b == b'-') => {
                version = Some(value.into_owned());
            }
            _ => return None,
        }
    }
    let offset = offset?;
    (offset == 0 || version.is_some()).then_some(Route::FileChunk { reference: reference?, offset, version })
}

/// The reads of this module by path and query, the way both the local listener and the
/// relay name them.
pub(crate) fn parse_read(path_and_query: &str) -> Option<Route> {
    let (path, query) = path_and_query.split_once('?').map_or((path_and_query, None), |(p, q)| (p, Some(q)));
    match (path, query) {
        ("/runtime", None) => Some(Route::Runtime),
        ("/agents/search", Some(query)) => search_query(query).map(Route::Search),
        ("/files/chunk", Some(query)) => file_chunk_query(query),
        ("/options", None) => Some(Route::Options(None)),
        ("/options", Some(query)) => single_query(query, "project").map(|project| Route::Options(Some(project))),
        ("/projects", None) => Some(Route::Projects),
        ("/issues", None) => Some(Route::Issues { refresh: false }),
        ("/issues", Some(query)) => single_query(query, "refresh").filter(|value| value == "1").map(|_| Route::Issues { refresh: true }),
        (path, None) if path.starts_with("/tasks/") => path.strip_prefix("/tasks/")?.strip_suffix("/history")
            .filter(|id| plain_id(id)).map(|id| Route::TaskHistory(id.to_owned())),
        (path, None) => path.strip_prefix("/agents/")?.strip_suffix("/activity")
            .filter(|id| plain_id(id)).map(|id| Route::Activity(id.to_owned())),
        _ => None,
    }
}

pub(crate) fn approved_read_path(path_and_query: &str) -> bool {
    parse_read(path_and_query).is_some_and(|route| !matches!(route, Route::Runtime))
}

/// The steps of a chunked upload, which only the encrypted relay carries: on this Mac a
/// file is uploaded in one request.
pub(crate) fn upload_step(path: &str) -> Option<&str> {
    path.strip_prefix("/uploads/").filter(|step| matches!(*step, "start" | "chunk" | "finish" | "cancel"))
}

impl Client {
    pub(crate) fn shared(&self) -> Arc<Self> {
        self.shared.upgrade().expect("client still owned")
    }

    pub(crate) fn remember_language(&self, device: &str, lang: alondra_core::views::Lang) {
        let mut languages = self.device_languages.lock().expect("device languages poisoned");
        if languages.get(device) == Some(&lang) {
            return;
        }
        let mut updated = languages.clone();
        updated.insert(device.to_owned(), lang);
        let next = self.device_languages_path.with_extension("json.next");
        let saved = serde_json::to_vec(&updated).map_err(anyhow::Error::from)
            .and_then(|bytes| std::fs::write(&next, bytes).map_err(anyhow::Error::from))
            .and_then(|()| std::fs::rename(&next, &self.device_languages_path).map_err(anyhow::Error::from));
        match saved {
            Ok(()) => *languages = updated,
            Err(error) => log!("could not save device language: {error:#}"),
        }
    }

    pub(crate) fn language_for_device(&self, device: &str) -> alondra_core::views::Lang {
        self.device_languages.lock().expect("device languages poisoned").get(device).copied()
            .unwrap_or(alondra_core::views::Lang::En)
    }

    pub(crate) async fn cached_issues(&self) -> Option<crate::issues::IssueList> {
        self.issue_cache.lock().await.clone()
    }

    pub(crate) fn open(domain: Arc<DomainRuntime>, home: &Path, gauges: crate::machine::Gauges) -> Arc<Self> {
        let device_languages_path = home.join("device-languages.json");
        let device_languages = match std::fs::read(&device_languages_path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(languages) => languages,
                Err(error) => {
                    log!("could not read device languages: {error:#}");
                    Default::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Default::default(),
            Err(error) => {
                log!("could not read device languages: {error:#}");
                Default::default()
            }
        };
        let read_work = domain.read_work.clone();
        let client = Arc::new_cyclic(|shared| Self {
            shared: shared.clone(),
            read_work,
            domain,
            home: home.to_owned(),
            gauges,
            views_gauges: Default::default(),
            details: Default::default(),
            pr_view: std::sync::Mutex::new(Arc::new(GhView)),
            views: Default::default(),
            feeds: Default::default(),
            sessions: std::sync::Mutex::new(Arc::new(crate::node_close::Machine)),
            github: std::sync::Mutex::new(Arc::new(new_work::Gh)),
            options: Default::default(),
            issue_cache: Default::default(),
            live_reads: Default::default(),
            live_snapshots: Default::default(),
            device_languages: std::sync::Mutex::new(device_languages),
            device_languages_path,
        });
        crate::views::feed::start_clock(&client);
        client
    }

    #[cfg(test)]
    pub(crate) fn use_sessions(&self, sessions: Arc<dyn Sessions>) {
        *self.sessions.lock().expect("session control poisoned") = sessions;
    }

    pub(crate) async fn write(
        &self,
        user_id: &str,
        route: Write,
        body: &Value,
        transport: Option<crate::domain_store::TransportIdentity<'_>>,
        authorize: &(dyn Fn() -> anyhow::Result<()> + Sync),
    ) -> anyhow::Result<Reply> {
        match route {
            Write::Close(id) => {
                authorize()?;
                self.close_session(&id, body).await
            }
            Write::ResolveConflicts(pr) => {
                authorize()?;
                Ok(self.resolve_conflicts(user_id, &pr, body).await)
            }
            Write::NewRequest => self.new_request(user_id, body, transport, authorize).await,
            Write::OrchestratorMessage => self.orchestrator_message(user_id, body, transport, authorize).await,
            Write::Presence => {
                authorize()?;
                Ok(self.presence(body))
            }
            Write::CancelReason(id) => {
                authorize()?;
                self.cancel_reason(&id, body)
            }
        }
    }

    async fn resolve_conflicts(&self, user_id: &str, pr: &str, body: &Value) -> Reply {
        if !body.as_object().is_some_and(|fields| fields.is_empty()) {
            return Reply::error(DomainError::new("invalid_input", "invalid request"));
        }
        let user = alondra_core::domain::Actor::User { user_id: user_id.to_owned() };
        match crate::domain_messages::resolve_conflicts(&self.domain, &self.home, &user, &crate::domain_messages::user_name(), &pr.into()).await {
            Ok(answer) => Reply::ok(answer),
            Err(error) => Reply::error(error),
        }
    }

    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    /// Keeps the GitHub details of open pull requests current until the daemon stops.
    pub(crate) async fn run(self: Arc<Self>, source: Arc<dyn PrView>) {
        self.use_pr_view(source.clone());
        loop {
            match self.domain.state() {
                Ok(state) => {
                    self.details.refresh(&state, &*source).await;
                    pr_details::hand_back_conflicts(&self.domain, &self.details, &crate::domain_messages::user_name());
                }
                Err(error) => log!("could not read pull requests for their details: {error:#}"),
            }
            tokio::time::sleep(DETAILS_INTERVAL).await;
        }
    }

    pub(crate) async fn read(&self, path_and_query: &str) -> Option<Response> {
        match parse_read(path_and_query)? {
            Route::Issues { .. } | Route::FileChunk { .. } => return self.read_inner(path_and_query).await,
            _ => {}
        }
        let client = self.shared();
        let path = path_and_query.to_owned();
        let runtime = tokio::runtime::Handle::current();
        match self.read_work.run(move || runtime.block_on(client.read_inner(&path))).await {
            Ok(response) => response,
            Err(error) => {
                log!("client read worker failed: {error}");
                Some(storage())
            }
        }
    }

    /// Live gathering already owns the read lane; reacquiring it here would deadlock the lane.
    pub(crate) async fn read_inner(&self, path_and_query: &str) -> Option<Response> {
        Some(match parse_read(path_and_query)? {
            Route::Runtime => self.runtime(),
            Route::Activity(id) => self.activity(&id),
            Route::Search(query) => self.search(&query),
            Route::FileChunk { reference, offset, version } => self.file_chunk(reference, offset, version).await,
            Route::Options(project) => self.options_answer(project),
            Route::Projects => self.projects_answer(),
            Route::Issues { refresh } => self.issues_answer(refresh).await,
            Route::TaskHistory(id) => self.task_history(&id),
        })
    }

    /// A stored copy in verified chunks, for the relay.
    async fn file_chunk(&self, reference: String, offset: u64, version: Option<String>) -> Response {
        let home = self.home.clone();
        let reads = self.domain.stored_reads.clone();
        let chunk = tokio::task::spawn_blocking(move || reads.stored(&home, &reference, offset, version)).await;
        match chunk {
            Ok(Ok(chunk)) => Json(chunk).into_response(),
            _ => crate::domain_api::domain_error(DomainError::new("content_unavailable", "stored file content is unavailable")),
        }
    }

    fn runtime(&self) -> Response {
        let read = self.domain.read();
        let state = read.state();
        let orders: Vec<(String, alondra_core::domain::Revision)> = read.index().queue().iter()
            .filter_map(|id| state.interactions.get(id).map(|card| (format!("interaction:{}", id.0), card.order)))
            .collect();
        let (state_times, request_times, revision_times) = {
            let store = self.domain.store.lock().expect("domain store poisoned");
            let revisions: Vec<_> = orders.iter().map(|(_, order)| *order).collect();
            match (store.task_state_times(), store.request_times(), store.revision_times(&revisions)) {
                (Ok(tasks), Ok(requests), Ok(revisions)) => (tasks, requests, revisions),
                _ => return storage(),
            }
        };
        let (machine, usage) = match &*self.gauges.lock().expect("machine gauges poisoned") {
            Some((machine, usage)) => (json!(machine), json!(usage)),
            None => (Value::Null, Value::Null),
        };
        let nodes: Vec<Value> = self.domain.live.nodes().iter()
            .filter(|node| state.agents.contains_key(&AgentId(node.session_id.clone())))
            .map(|node| {
                let name = state.agents.get(&AgentId(node.session_id.clone())).map(|agent| agent.name.as_str());
                live::client_node(node, name)
            })
            .collect();
        let (launches, cancel_reasons) = {
            let store = self.domain.store.lock().expect("domain store poisoned");
            match (store.request_launch_summaries(), store.cancel_reasons()) {
                (Ok(launches), Ok(reasons)) => (launches, reasons),
                _ => return storage(),
            }
        };
        let live = self.domain.live.nodes();
        let tasks: serde_json::Map<String, Value> = state.tasks.iter().map(|(id, task)| {
            let (asked_project, effort) = launches.get(&task.request_id).cloned().unwrap_or_default();
            let project = asked_project.or_else(|| task.agent_id.as_ref()
                .and_then(|agent| live.iter().find(|node| node.session_id == agent.0))
                .map(|node| node.project()));
            (id.0.clone(), json!({"project": project, "state_since": state_times.get(id),
                "effort": effort, "cancel_reason": cancel_reasons.get(id)}))
        }).collect();
        let requests: serde_json::Map<String, Value> = state.requests.iter().map(|(id, request)| {
            let answer = request.answer.clone().or_else(|| request_answer(&state, &id.0));
            (id.0.clone(), json!({"created_at": request_times.get(id),
                "withdrawn": request.withdrawn, "answer": answer, "title": request.title}))
        }).collect();
        let pull_requests: serde_json::Map<String, Value> = state.pull_requests.keys().map(|id| {
            (id.0.clone(), self.details.get(&id.0).map_or_else(|| json!({}), |details| json!(details)))
        }).collect();
        let inbox_times: serde_json::Map<String, Value> = orders.iter()
            .filter_map(|(id, order)| revision_times.get(order).map(|at| (id.clone(), json!(at)))).collect();
        Json(json!({
            "machine": machine, "usage": usage, "update": crate::updates::status(&self.home),
            "nodes": nodes, "tasks": tasks, "requests": requests,
            "pull_requests": pull_requests, "inbox_times": inbox_times,
            "orchestrator": crate::domain_orchestrator::status(&crate::domain_orchestrator::executor(self.domain.clone(), &self.home)).ok(),
        })).into_response()
    }

    fn activity(&self, id: &str) -> Response {
        let state = match self.domain.state() {
            Ok(state) => state,
            Err(_) => return storage(),
        };
        let agent = AgentId(id.to_owned());
        if !state.agents.contains_key(&agent) {
            return crate::domain_api::domain_error(DomainError::new("not_found", "agent does not exist"));
        }
        let mut threads = self.domain.live.threads(id).unwrap_or_default();
        let only_task = single_open_task(&state, &agent);
        for thread in &mut threads {
            let known = thread.task.as_ref().is_some_and(|task| state.tasks.contains_key(&TaskId(task.clone())));
            if !known {
                thread.task = only_task.clone();
            }
        }
        Json(json!({"activity": threads})).into_response()
    }

    fn search(&self, query: &str) -> Response {
        let state = match self.domain.state() {
            Ok(state) => state,
            Err(_) => return storage(),
        };
        let nodes = self.domain.live.nodes();
        let candidates: Vec<(String, String)> = state.agents.values().map(|agent| {
            let node = nodes.iter().find(|node| node.session_id == agent.id.0);
            let tasks: Vec<&str> = state.tasks.values()
                .filter(|task| task.agent_id.as_ref() == Some(&agent.id)).map(|task| task.title.as_str()).collect();
            let text = format!("{} {} {} {} {}", agent.name, node.map_or(String::new(), |n| n.short_id.clone()),
                node.map_or(String::new(), |n| n.project()),
                node.and_then(|n| n.last_message.clone()).unwrap_or_default(), tasks.join(" "));
            (agent.id.0.clone(), text)
        }).collect();
        Json(json!({"via": "local", "sessions": crate::classify::search(query, candidates)})).into_response()
    }
}

/// The one task of the agent still open: a thread that names no task of the domain works on it.
fn single_open_task(state: &DomainState, agent: &AgentId) -> Option<String> {
    let mut open = state.tasks.values()
        .filter(|task| task.agent_id.as_ref() == Some(agent) && !task.state.terminal());
    let only = open.next()?;
    open.next().is_none().then(|| only.id.0.clone())
}

/// What the orchestrator told the user when it closed a request that needed no task.
fn request_answer(state: &DomainState, request: &str) -> Option<String> {
    state.interactions.values()
        .filter(|interaction| {
            matches!(&interaction.scope, Scope::Orchestrator { request_id: Some(id) } if id.0 == request)
                && interaction.recipient == Recipient::User
                && matches!(interaction.payload, InteractionPayload::Information { .. })
        })
        .max_by_key(|interaction| interaction.order)
        .map(|interaction| interaction.body.clone())
}

fn storage() -> Response {
    crate::domain_api::domain_error(DomainError::new("storage_unavailable", "daemon storage could not be read"))
}

fn forbidden() -> Response {
    crate::domain_api::domain_error(DomainError::new("forbidden", "signed user device required"))
}

async fn write(
    State(client): State<Arc<Client>>,
    signed: Option<Extension<Signed>>,
    uri: Uri,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let home = client.home().to_owned();
    let Some(device) = crate::domain_bootstrap::signed_device(&home, &signed) else {
        return forbidden();
    };
    let Some(route) = parse_write(uri.path()).or_else(|| parse_put(uri.path())) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(Json(body)) = body else {
        return Reply::error(DomainError::new("invalid_input", "invalid request body")).into_response();
    };
    let work = if matches!(&route, Write::Presence | Write::CancelReason(_)) {
        client.domain.command_work.clone()
    } else { client.domain.external_work.clone() };
    let runtime = tokio::runtime::Handle::current();
    match work.run(move || runtime.block_on(async move {
        let authorize = || {
            crate::domain_bootstrap::signed_device(&home, &signed)
                .map(|_| ())
                .ok_or_else(|| anyhow::anyhow!("device approval was revoked"))
        };
        client.write(&format!("device:{}", device.device_id), route, &body, None, &authorize).await
    })).await {
        Ok(Ok(reply)) => reply.into_response(),
        _ => storage(),
    }
}

/// A user resolves an uncertain delivery on this Mac the way a phone does through the relay.
async fn resolve_delivery(
    State(client): State<Arc<Client>>,
    signed: Option<Extension<Signed>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let home = client.home().to_owned();
    let Some(device) = crate::domain_bootstrap::signed_device(&home, &signed) else {
        return forbidden();
    };
    let (Ok(Json(body)), Ok(attempt)) = (body, id.parse::<i64>()) else {
        return Reply::error(DomainError::new("invalid_input", "invalid delivery resolution")).into_response();
    };
    let digest: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(body.to_string().as_bytes()).into();
    let request = format!("local-{}", crate::auth::random_hex(16));
    let domain = client.domain.clone();
    let resolved = tokio::task::spawn_blocking(move || crate::domain_api::encrypted_resolve_delivery_runtime(
        &domain, &home.clone(), &device.device_id, attempt, Some(&body), (&device.device_id, &request, &digest),
        || crate::domain_bootstrap::signed_device(&home, &signed).map(|_| ()).ok_or_else(|| anyhow::anyhow!("device approval was revoked")))).await;
    match resolved {
        Ok(Ok(result)) => {
            let status = StatusCode::from_u16(result["status"].as_u64().unwrap_or(500) as u16).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(result["body"].clone())).into_response()
        }
        _ => storage(),
    }
}

pub(crate) fn router(client: Arc<Client>) -> Router {
    Router::new()
        .route("/orchestrator/messages", post(write))
        .route("/activity", post(write))
        .route("/tasks/{id}/cancel-reason", put(write))
        .route("/delivery-attempts/{id}/resolve", post(resolve_delivery))
        .with_state(client)
}

#[cfg(test)]
pub(crate) mod tests;
