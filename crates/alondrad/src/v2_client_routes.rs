//! The routes the TUI and the CLI call on a v2-only daemon beyond the domain itself: the
//! live runtime of the Mac and its sessions, and what a session is doing. The domain stays
//! the only authority; nothing here decides an action.
mod live;
mod pr_details;

use crate::auth::Signed;
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain_v2::{
    AgentId, DomainError, DomainState, InteractionPayload, Recipient, Scope, TaskId,
};
use axum::extract::State;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub(crate) use live::Live;
pub(crate) use pr_details::{GhView, PrView};

const DETAILS_INTERVAL: Duration = Duration::from_secs(60);

pub(crate) struct Client {
    domain: Arc<DomainRuntime>,
    home: PathBuf,
    gauges: crate::machine::Gauges,
    details: pr_details::PrDetails,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Route {
    Runtime,
    Activity(String),
    Search(String),
}

fn plain_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn search_query(query: &str) -> Option<String> {
    let mut pairs = url::form_urlencoded::parse(query.as_bytes());
    let (key, value) = pairs.next()?;
    (key == "q" && !value.is_empty() && value.len() <= 2048 && pairs.next().is_none()).then(|| value.into_owned())
}

/// The reads of this module by path and query, the way both the local listener and the
/// relay name them.
pub(crate) fn parse_read(path_and_query: &str) -> Option<Route> {
    let (path, query) = path_and_query.split_once('?').map_or((path_and_query, None), |(p, q)| (p, Some(q)));
    match (path, query) {
        ("/v2/runtime", None) => Some(Route::Runtime),
        ("/v2/agents/search", Some(query)) => search_query(query).map(Route::Search),
        (path, None) => path.strip_prefix("/v2/agents/")?.strip_suffix("/activity")
            .filter(|id| plain_id(id)).map(|id| Route::Activity(id.to_owned())),
        _ => None,
    }
}

pub(crate) fn approved_read_path(path_and_query: &str) -> bool {
    parse_read(path_and_query).is_some()
}

impl Client {
    pub(crate) fn open(domain: Arc<DomainRuntime>, home: &Path, gauges: crate::machine::Gauges) -> Arc<Self> {
        Arc::new(Self {
            domain,
            home: home.to_owned(),
            gauges,
            details: Default::default(),
        })
    }

    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    /// Keeps the GitHub details of open pull requests current until the daemon stops.
    pub(crate) async fn run(self: Arc<Self>, source: Arc<dyn PrView>) {
        loop {
            match self.domain.state() {
                Ok(state) => self.details.refresh(&state, &*source).await,
                Err(error) => log!("could not read pull requests for their details: {error:#}"),
            }
            tokio::time::sleep(DETAILS_INTERVAL).await;
        }
    }

    pub(crate) fn read(&self, path_and_query: &str) -> Option<Response> {
        Some(match parse_read(path_and_query)? {
            Route::Runtime => self.runtime(),
            Route::Activity(id) => self.activity(&id),
            Route::Search(query) => self.search(&query),
        })
    }

    fn runtime(&self) -> Response {
        let state = match self.domain.state() {
            Ok(state) => state,
            Err(_) => return storage(),
        };
        let snapshot = match self.domain.reads.snapshot(&self.domain) {
            Ok(snapshot) => snapshot,
            Err(_) => return storage(),
        };
        let orders = snapshot.inbox_orders();
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
        let tasks: serde_json::Map<String, Value> = state.tasks.iter().map(|(id, task)| {
            let project = task.agent_id.as_ref()
                .and_then(|agent| self.domain.live.nodes().into_iter().find(|node| node.session_id == agent.0))
                .map(|node| node.project());
            (id.0.clone(), json!({"project": project, "state_since": state_times.get(id),
                "effort": null, "cancel_reason": null}))
        }).collect();
        let requests: serde_json::Map<String, Value> = state.requests.iter().map(|(id, request)| {
            let answer = request_answer(&state, &id.0);
            let has_tasks = state.tasks.values().any(|task| task.request_id == *id);
            (id.0.clone(), json!({"created_at": request_times.get(id),
                "withdrawn": request.closed && answer.is_none() && !has_tasks, "answer": answer}))
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

async fn read(State(client): State<Arc<Client>>, signed: Option<Extension<Signed>>, uri: Uri) -> Response {
    if crate::domain_bootstrap::signed_device(client.home(), &signed).is_none() {
        return forbidden();
    }
    let path_and_query = uri.path_and_query().map_or("/", |value| value.as_str());
    client.read(path_and_query).unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}

pub(crate) fn router(client: Arc<Client>) -> Router {
    Router::new()
        .route("/v2/runtime", get(read))
        .route("/v2/agents/search", get(read))
        .route("/v2/agents/{id}/activity", get(read))
        .with_state(client)
}

#[cfg(test)]
mod tests;
