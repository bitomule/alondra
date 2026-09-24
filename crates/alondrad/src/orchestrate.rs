//! The orchestrator's hands on tasks: turning a request into tasks, the nodes a task could go
//! to, launching nodes, and starting every task that can start, at `define` or once its
//! dependencies let it.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use alondra_core::options::{OptionsFile, Resolved};
use alondra_core::tasks::{Dependency, NewTask, Start, Task, TaskOptions, TaskState};
use alondra_core::{Attachment, Event, Message, MessageAbout, MessageFrom, Millis, View, now_millis};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::hub::Hub;
use crate::prs::Gh;

/// Why a call was turned down, with the status it answers.
pub type Refusal = (StatusCode, String);

const EFFORTS: [&str; 3] = ["light", "normal", "hard"];

/// Starts `claude`, so tests and the sandbox can stand in for it.
pub trait Launcher: Sync {
    /// Runs `claude` with `args` in `cwd`: what it printed, or why it did not start.
    fn run(&self, cwd: &str, args: &[String]) -> impl Future<Output = Result<String, Refusal>> + Send;

    /// The subscription's usage, which can refuse a launch.
    fn usage(&self) -> Option<alondra_core::usage::Usage> {
        crate::usage::current(&crate::watcher::claude_home())
    }
}

pub struct ClaudeCli;

impl Launcher for ClaudeCli {
    async fn run(&self, cwd: &str, args: &[String]) -> Result<String, Refusal> {
        let mut cmd = tokio::process::Command::new("claude");
        cmd.current_dir(cwd).args(args).kill_on_drop(true);
        match tokio::time::timeout(Duration::from_secs(60), cmd.output()).await {
            Ok(Ok(out)) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).to_string()),
            Ok(Ok(out)) => Err((StatusCode::BAD_GATEWAY, String::from_utf8_lossy(&out.stderr).to_string())),
            Ok(Err(e)) => Err((StatusCode::BAD_GATEWAY, e.to_string())),
            Err(_) => Err((StatusCode::GATEWAY_TIMEOUT, "claude --bg did not respond within 60 s".into())),
        }
    }
}

pub struct Launch {
    pub name: String,
    pub cwd: String,
    pub prompt: String,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub over_pace_ok: bool,
    pub request_id: Option<String>,
    pub options: Resolved,
    pub task: Option<String>,
    /// The title of the task the launch makes or serves; the node's name when absent.
    pub title: Option<String>,
}

/// Launches a node and records the launch; with `task`, the node takes that task.
pub async fn launch(hub: &Hub, l: Launch, launcher: &impl Launcher) -> Result<Value, Refusal> {
    if let Some(reason) = crate::http::launch_blocked(launcher.usage().as_ref(), l.over_pace_ok) {
        return Err((StatusCode::CONFLICT, reason));
    }
    if !Path::new(&l.cwd).is_dir() {
        return Err((StatusCode::BAD_REQUEST, format!("directory {} does not exist", l.cwd)));
    }
    let args = crate::options::launch_args(&l.name, l.permission_mode.as_deref(), l.model.as_deref(), &l.prompt, &l.options, l.request_id.as_deref());
    let output = launcher.run(&l.cwd, &args).await?;
    let launched = Event::Launched { name: l.name.clone(), request_id: l.request_id.clone(), auto_merge: l.options.auto_merge, task: l.task.clone(), title: l.title.clone() };
    hub.append(launched).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(json!({"ok": true, "output": output, "auto_merge": l.options.auto_merge, "task": l.task}))
}

/// The options a task's node works with: its request's, with what the task changed. An effort
/// the user chose in the request stays binding.
pub fn task_options(hub: &Hub, task: &Task, effort: Option<&str>) -> Resolved {
    let file = crate::options::load(hub).file;
    let effort = effort.or(task.options.effort.as_deref());
    let mut r = match hub.read(|v| v.requests.get(&task.request).cloned()) {
        Some(r) => r.with_orchestrator_effort(&file, effort),
        None => file.orchestrator_defaults(task.options.project.as_deref(), effort),
    };
    r.auto_merge = task.options.auto_merge;
    r.issue = task.options.issue.clone();
    if task.options.project.is_some() {
        r.project = task.options.project.clone();
    }
    r
}

fn live(v: &View, session_id: &str) -> bool {
    !v.removed.contains_key(session_id) && v.nodes.get(session_id).is_some_and(|n| n.alive || n.in_daemon)
}

fn node_name(v: &View, session_id: &str) -> String {
    v.nodes.get(session_id).map(|n| n.display_name().to_string()).unwrap_or_else(|| session_id.chars().take(8).collect())
}

/// Why a task cannot be handed to a node now: it waits for another, it is finished, or a live
/// node other than `to` works on it.
fn not_handable(v: &View, id: &str, to: Option<&str>) -> Option<Refusal> {
    let Some(task) = v.board.tasks.get(id) else { return Some((StatusCode::NOT_FOUND, format!("task {id} does not exist"))) };
    let conflict = |why: String| Some((StatusCode::CONFLICT, why));
    match task.state {
        TaskState::WaitingDependency => {
            let on = v.task_guards(task).waiting_on.unwrap_or_default();
            return conflict(format!("{id} has not started: it waits for {on}, and Alondra starts it when {on} has its PR open or is done"));
        }
        TaskState::Cancelled => return conflict(format!("{id} was cancelled: reopen it first")),
        TaskState::Done if to.is_none() => return conflict(format!("{id} is done: reopen it first")),
        _ => {}
    }
    match &task.assignee {
        Some(a) if Some(a.session_id.as_str()) != to && live(v, &a.session_id) => {
            conflict(format!("{id} is worked on by node {}: send to it instead", node_name(v, &a.session_id)))
        }
        _ => None,
    }
}

/// What `launch` with a task needs: the options, the request and the prompt, which is the
/// task's own start brief unless the orchestrator wrote one.
pub fn task_launch(hub: &Hub, id: &str, prompt: &str, effort: Option<&str>) -> Result<(Resolved, Option<String>, String), Refusal> {
    let task = hub.read(|v| match not_handable(v, id, None) {
        Some(r) => Err(r),
        None => Ok(v.board.tasks[id].clone()),
    })?;
    let prompt = match prompt.trim() {
        "" => hub.read(|v| v.start_brief(&task, &BTreeMap::new())),
        own => own.to_string(),
    };
    let request = hub.read(|v| v.requests.contains_key(&task.request)).then(|| task.request.clone());
    Ok((task_options(hub, &task, effort), request, prompt))
}

/// Gives the task to the node, when it is not its yet, and types it the text about it.
pub fn hand(hub: &Hub, id: &str, session_id: &str, text: String, from: MessageFrom, attachments: Vec<Attachment>) -> Result<Message, Refusal> {
    let recorded = hub.append_all(|v| {
        if let Some(r) = not_handable(v, id, Some(session_id)) {
            return Err(r);
        }
        let mut events = vec![];
        if !v.board.tasks[id].is_assigned_to(session_id) {
            events.push(Event::TaskAssigned { task: id.to_string(), session_id: session_id.to_string(), agent_id: None });
        }
        events.push(Event::MessageQueued(Message {
            id: v.messages.keys().next_back().copied().unwrap_or(0) + 1,
            session_id: session_id.to_string(),
            from,
            text: text.clone(),
            answers_question: None,
            created_at: now_millis(),
            handed_at: None,
            delivered_at: None,
            attachments: attachments.clone(),
            about: Some(MessageAbout::Task { task: id.to_string() }),
        }));
        Ok(events)
    });
    match recorded {
        Ok(Ok(events)) => events.into_iter().find_map(|(_, e)| if let Event::MessageQueued(m) = e { Some(m) } else { None }).ok_or((StatusCode::INTERNAL_SERVER_ERROR, "the message was not recorded".into())),
        Ok(Err(r)) => Err(r),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

/// What the orchestrator passes to `define`.
#[derive(Deserialize)]
pub struct Definition {
    #[serde(default)]
    pub request: Option<String>,
    /// What the user asked, in their words, for a request they made outside the apps.
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub tasks: Vec<TaskIn>,
}

#[derive(Deserialize)]
pub struct TaskIn {
    #[serde(default)]
    title: String,
    #[serde(default)]
    brief: String,
    #[serde(default)]
    depends_on: Vec<Dependency>,
    #[serde(default)]
    options: OptionsIn,
    /// A live node's name, short id or session id, or `new`.
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize, Default)]
struct OptionsIn {
    #[serde(default, deserialize_with = "lenient_bool")]
    auto_merge: Option<bool>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    issue: Option<String>,
}

fn lenient_bool<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    match Value::deserialize(d)? {
        Value::Bool(b) => Ok(Some(b)),
        Value::Null => Ok(None),
        Value::String(s) if s.trim().eq_ignore_ascii_case("true") => Ok(Some(true)),
        Value::String(s) if s.trim().eq_ignore_ascii_case("false") => Ok(Some(false)),
        other => Err(serde::de::Error::custom(format!("expected true or false, got {other}"))),
    }
}

/// A definition refused: the status, and every reason.
pub struct Refused {
    pub status: StatusCode,
    pub problems: Vec<String>,
}

fn refuse(status: StatusCode, problem: String) -> Refused {
    Refused { status, problems: vec![problem] }
}

fn slug(title: &str) -> String {
    let words: Vec<String> = title
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    let mut out = String::new();
    for w in words {
        if out.len() + w.len() + 1 > 40 {
            break;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&w);
    }
    if out.is_empty() { "task".into() } else { out }
}

/// A node name nobody uses yet: the one asked for, or `<project>/<title>` made unique.
fn node_name_for(asked: Option<&str>, project: Option<&str>, cwd: &str, title: &str, taken: &HashSet<String>) -> Result<String, String> {
    if let Some(name) = asked.map(str::trim).filter(|n| !n.is_empty()) {
        return if taken.contains(name) { Err(format!("a node named {name} is already running or about to be launched")) } else { Ok(name.to_string()) };
    }
    let project = project.map(str::to_string).or_else(|| Path::new(cwd).file_name().map(|f| f.to_string_lossy().to_string())).unwrap_or_else(|| "work".into());
    let base = format!("{project}/{}", slug(title));
    Ok((1..).map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") }).find(|n| !taken.contains(n)).expect("some suffix is free"))
}

/// The events a definition records, worked out on the view it will be recorded on, or every
/// reason it cannot stand. Task ids come after the view's last event, so no launch takes them.
fn plan(v: &View, d: &Definition, file: &OptionsFile, root: &Path, user: &str, now: Millis) -> Result<Vec<Event>, Refused> {
    if d.tasks.is_empty() {
        return Err(refuse(StatusCode::BAD_REQUEST, "define at least one task".into()));
    }
    let (request, options, sent) = match d.request.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        Some(id) if !v.board.requests.contains_key(id) => {
            return Err(refuse(StatusCode::NOT_FOUND, format!("request {id} does not exist: pass the request_id of {user}'s request, or text for one {user} made outside the apps")));
        }
        Some(id) => (id.to_string(), v.requests.get(id).cloned().unwrap_or_else(|| file.orchestrator_defaults(None, None)), None),
        None => {
            let Some(text) = d.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) else {
                return Err(refuse(StatusCode::BAD_REQUEST, format!("pass request with the request_id of {user}'s request, or text with what {user} asked for")));
            };
            let id = format!("r-{now}");
            let options = file.orchestrator_defaults(None, None);
            let sent = Event::RequestSent { request_id: id.clone(), options: options.clone(), text: Some(text.to_string()), attachments: vec![] };
            (id, options, Some(sent))
        }
    };
    let labels: Vec<String> = d.tasks.iter().enumerate().map(|(i, t)| format!("tasks[{i}] “{}”", t.title.trim())).collect();
    let ids: Vec<String> = (1..=d.tasks.len() as u64).map(|i| format!("t-{}", v.seq + i)).collect();
    let mut taken: HashSet<String> = v.nodes.values().filter(|n| live(v, &n.session_id)).filter_map(|n| n.name.clone()).collect();
    taken.extend(v.board.tasks.values().filter(|t| t.needs_start() || (t.launched && t.assignee.is_none())).filter_map(|t| match &t.start {
        Some(Start::NewNode { name, .. }) => Some(t.launched_as.clone().unwrap_or_else(|| name.clone())),
        _ => t.launched.then(|| t.launch_name().to_string()),
    }));
    let mut problems = vec![];
    let mut starts = vec![];
    for (i, t) in d.tasks.iter().enumerate() {
        let label = &labels[i];
        let title = t.title.trim();
        if title.is_empty() {
            problems.push(format!("{label}: the title is empty"));
        }
        if t.brief.trim().is_empty() {
            problems.push(format!("{label}: the brief is empty, and it is what the node is told"));
        }
        if let Some(same) = v.board.tasks.values().find(|o| o.request == request && o.title == title && o.state != TaskState::Cancelled) {
            problems.push(format!("{label}: {} “{}” of this request already exists", same.id, same.title));
        }
        if let Some(e) = t.options.effort.as_deref().filter(|e| !EFFORTS.contains(e)) {
            problems.push(format!("{label}: effort {e} is not light, normal or hard"));
        }
        if t.options.auto_merge == Some(true) && !options.auto_merge {
            problems.push(format!("{label}: auto-merge needs {user} to choose it in the request"));
        }
        let project = t.options.project.clone().or(options.project.clone());
        let start = match t.assignee.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
            None => Err(format!("{label}: no assignee: name a live node (see candidates) or \"new\"")),
            Some("new") => match t.cwd.clone().or_else(|| project.as_ref().map(|p| root.join(p).display().to_string())) {
                None => Err(format!("{label}: no project nor cwd to launch its node in")),
                Some(cwd) if !Path::new(&cwd).is_dir() => Err(format!("{label}: directory {cwd} does not exist")),
                Some(cwd) => node_name_for(t.name.as_deref(), project.as_deref(), &cwd, title, &taken).map_err(|e| format!("{label}: {e}")).map(|name| {
                    taken.insert(name.clone());
                    Start::NewNode { name, cwd, model: t.model.clone().filter(|m| !m.trim().is_empty()) }
                }),
            },
            Some(key) => match v.resolve_node(key) {
                None if v.removed.contains_key(key) => Err(format!("{label}: node {key} was closed: pick a live node (see candidates) or \"new\"")),
                None => Err(format!("{label}: there is no node {key}: pick a live node (see candidates) or \"new\"")),
                Some(n) if n.is_orchestrator => Err(format!("{label}: {key} is the orchestrator: give the task to a node, or \"new\"")),
                Some(n) if !live(v, &n.session_id) => Err(format!("{label}: node {key} is closed: pick a live node (see candidates) or \"new\"")),
                Some(n) => Ok(Start::Node { session_id: n.session_id.clone() }),
            },
        };
        match start {
            Ok(s) => starts.push(Some(s)),
            Err(p) => {
                problems.push(p);
                starts.push(None);
            }
        }
    }
    let wanted: Vec<Vec<Dependency>> = d.tasks.iter().map(|t| t.depends_on.clone()).collect();
    let depends = alondra_core::tasks::dependencies(&v.board, &labels, &ids, &wanted).unwrap_or_else(|p| {
        problems.extend(p);
        vec![]
    });
    if !problems.is_empty() {
        return Err(Refused { status: StatusCode::UNPROCESSABLE_ENTITY, problems });
    }
    let effort = |asked: Option<&String>| if options.effort == alondra_core::options::AUTO { asked.cloned().or(Some(options.effort.clone())) } else { Some(options.effort.clone()) };
    let mut events: Vec<Event> = sent.into_iter().collect();
    for (((t, id), depends_on), start) in d.tasks.iter().zip(&ids).zip(depends).zip(starts) {
        events.push(Event::TaskCreated(NewTask {
            id: id.clone(),
            request: request.clone(),
            title: t.title.trim().to_string(),
            brief: Some(t.brief.trim().to_string()),
            depends_on,
            options: TaskOptions {
                auto_merge: t.options.auto_merge.unwrap_or(options.auto_merge),
                effort: effort(t.options.effort.as_ref()),
                project: t.options.project.clone().or(options.project.clone()),
                issue: t.options.issue.clone().or(options.issue.clone()),
            },
            session_id: None,
            launched: false,
            launched_as: None,
            start,
        }));
    }
    Ok(events)
}

/// What happens next to a task just defined, in the orchestrator's terms.
fn next_step(v: &View, t: &Task) -> String {
    match (&t.start, t.state) {
        (_, TaskState::WaitingDependency) => {
            let on = v.task_guards(t).waiting_on.unwrap_or_default();
            format!("waits for {on}: Alondra starts it when every task it depends on has its PR open or is done")
        }
        (Some(Start::NewNode { name, cwd, .. }), _) => format!("Alondra launches node {name} in {cwd} with its brief now"),
        (Some(Start::Node { session_id }), _) => format!("Alondra hands it to node {} with its brief now", node_name(v, session_id)),
        (None, _) => "nothing starts it".into(),
    }
}

/// Records the tasks a request becomes. Tasks with nothing to wait for start right away.
pub fn define(hub: &Hub, d: Definition) -> Result<Value, Refused> {
    let file = crate::options::load(hub).file;
    let root = file.projects_root(&crate::options::user_home());
    let user = hub.config.name.clone();
    let recorded = match hub.append_all(|v| plan(v, &d, &file, &root, &user, now_millis())) {
        Ok(r) => r?,
        Err(e) => return Err(refuse(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    };
    let ids: Vec<String> = recorded.iter().filter_map(|(_, e)| if let Event::TaskCreated(t) = e { Some(t.id.clone()) } else { None }).collect();
    Ok(hub.read(|v| {
        let tasks: Vec<Value> = ids
            .iter()
            .filter_map(|id| v.board.tasks.get(id))
            .map(|t| json!({"id": t.id, "title": t.title, "state": t.state, "depends_on": t.depends_on, "next": next_step(v, t)}))
            .collect();
        let request = ids.first().and_then(|id| v.board.tasks.get(id)).map(|t| t.request.clone());
        json!({"request": request, "tasks": tasks})
    }))
}

/// Live nodes a task, or work in a repo, could go to.
pub fn candidates(hub: &Hub, task: Option<&str>, repo: Option<&str>) -> Result<Value, Refusal> {
    let task = task.map(str::trim).filter(|t| !t.is_empty());
    let repo = repo.map(str::trim).filter(|r| !r.is_empty());
    if task.is_none() && repo.is_none() {
        return Err((StatusCode::BAD_REQUEST, "pass a task id or a repo".into()));
    }
    hub.read(|v| {
        let t = match task {
            Some(id) => Some(v.board.tasks.get(id).ok_or((StatusCode::NOT_FOUND, format!("task {id} does not exist")))?),
            None => None,
        };
        Ok(json!({"candidates": v.candidates(t, repo)}))
    })
}

fn pr_handling(auto_merge: bool, user: &str) -> String {
    if auto_merge {
        "PR handling for this task: auto-merge. Open the PR and end your turn: Alondra merges it once its checks pass and types the merge notice here.".into()
    } else {
        format!("PR handling for this task: {user} reviews its PR on its card; do not merge it yourself.")
    }
}

/// The branches of the dependencies' open PRs GitHub has not told Alondra yet.
async fn heads(hub: &Hub, task: &Task, gh: &impl Gh) -> BTreeMap<u64, String> {
    let missing: Vec<(u64, String)> = hub.read(|v| v.stacked_on(task).into_iter().filter(|(_, p)| p.head.is_none()).map(|(_, p)| (p.id, p.url.clone())).collect());
    let mut out = BTreeMap::new();
    for (id, url) in missing {
        if let Some(head) = gh.view(&url).await.and_then(|v| v.get("headRefName").and_then(Value::as_str).map(str::to_string)) {
            out.insert(id, head);
        }
    }
    out
}

/// Starts one task as it was defined: launches its node, or hands it to the node it was given to.
async fn start(hub: &Hub, task: &Task, launcher: &impl Launcher, gh: &impl Gh) -> Result<(String, String), String> {
    let heads = heads(hub, task, gh).await;
    let (brief, place) = hub.read(|v| (v.start_brief(task, &heads), built_on(v, task, &heads)));
    start_with(hub, task, launcher, brief).await.map(|how| (how, place))
}

async fn start_with(hub: &Hub, task: &Task, launcher: &impl Launcher, brief: String) -> Result<String, String> {
    match &task.start {
        Some(Start::NewNode { name, cwd, model }) => {
            let waiting = hub.read(|v| v.nodes.values().any(|n| n.name.as_deref() == Some(name) && live(v, &n.session_id) && v.tasks_of(&n.session_id).next().is_none()));
            if waiting {
                let launched = Event::Launched { name: name.clone(), request_id: Some(task.request.clone()), auto_merge: task.options.auto_merge, task: Some(task.id.clone()), title: None };
                hub.append(launched).map_err(|e| e.to_string())?;
                return Ok(format!("in node {name}, already running"));
            }
            let request_id = hub.read(|v| v.requests.contains_key(&task.request)).then(|| task.request.clone());
            let l = Launch {
                name: name.clone(),
                cwd: cwd.clone(),
                prompt: brief,
                model: model.clone(),
                permission_mode: None,
                over_pace_ok: false,
                request_id,
                options: task_options(hub, task, None),
                task: Some(task.id.clone()),
                title: None,
            };
            launch(hub, l, launcher).await.map(|_| format!("in a new node {name}")).map_err(|(_, e)| e)
        }
        Some(Start::Node { session_id }) => {
            let (alive, name) = hub.read(|v| (live(v, session_id), node_name(v, session_id)));
            if !alive {
                return Err(format!("its node {name} is closed"));
            }
            let text = format!("[Alondra] A new task for you, from the orchestrator: {} “{}”.\n\n{brief}\n\n{}", task.id, task.title, pr_handling(task.options.auto_merge, &hub.config.name));
            hand(hub, &task.id, session_id, text, MessageFrom::Orchestrator, vec![]).map(|_| format!("on node {name}")).map_err(|(_, e)| e)
        }
        None => Err("it has no start".into()),
    }
}

pub(crate) fn tell_orchestrator(hub: &Hub, text: String) {
    let told = hub.append_with(|v| {
        let o = v.nodes.values().find(|n| n.is_orchestrator && live(v, &n.session_id))?;
        Some(Event::MessageQueued(Message {
            id: v.messages.keys().next_back().copied().unwrap_or(0) + 1,
            session_id: o.session_id.clone(),
            from: MessageFrom::Alondra,
            text: text.clone(),
            answers_question: None,
            created_at: now_millis(),
            handed_at: None,
            delivered_at: None,
            attachments: vec![],
            about: None,
        }))
    });
    if let Err(e) = told {
        log!("could not tell the orchestrator: {e}");
    }
}

/// Where a started task builds, for the orchestrator.
fn built_on(v: &View, task: &Task, heads: &BTreeMap<u64, String>) -> String {
    let branch = |p: &alondra_core::pr::PullRequest| heads.get(&p.id).or(p.head.as_ref()).map(|h| format!(" (`{h}`)")).unwrap_or_default();
    let below: Vec<String> = v.stacked_on(task).into_iter().map(|(d, p)| format!("PR #{} of {}{}", p.number, d.id, branch(p))).collect();
    if below.is_empty() { "on the default branch".into() } else { format!("on top of {}", below.join(", ")) }
}

/// Starts every task that can start and has not: the ones just defined with nothing to wait
/// for, and the ones whose dependencies now have their PR open or are done. A start that fails
/// is left to the orchestrator, told once; `given_up` keeps those.
pub async fn start_due(hub: &Hub, launcher: &impl Launcher, gh: &impl Gh, given_up: &mut HashSet<String>) -> Vec<String> {
    let _one_at_a_time = hub.starts.lock().await;
    let due: Vec<Task> = hub.read(|v| v.tasks_to_start().into_iter().filter(|t| !given_up.contains(&t.id)).cloned().collect());
    let mut started = vec![];
    for task in due {
        if !hub.read(|v| v.board.tasks.get(&task.id).is_some_and(Task::needs_start)) {
            continue;
        }
        match start(hub, &task, launcher, gh).await {
            Ok((how, place)) => {
                log!("task {} started {how}, {place}", task.id);
                if !task.depends_on.is_empty() {
                    tell_orchestrator(hub, format!("[Alondra] Task {} “{}” started {how}, {place}: what it depended on ({}) has its PR open or is done.", task.id, task.title, task.depends_on.join(", ")));
                }
                started.push(task.id.clone());
            }
            Err(why) => {
                log!("task {} could not start: {why}", task.id);
                given_up.insert(task.id.clone());
                tell_orchestrator(
                    hub,
                    format!("[Alondra] Task {} “{}” could not start: {why}. Start it with launch or send with task \"{}\", or cancel it.", task.id, task.title, task.id),
                );
            }
        }
    }
    started
}

pub async fn run(hub: Arc<Hub>) {
    let mut given_up = HashSet::new();
    loop {
        let seen = hub.tick();
        start_due(&hub, &ClaudeCli, &crate::prs::GhCli, &mut given_up).await;
        hub.wait_change(seen, Duration::from_secs(30)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::DaemonObservation;
    use axum::extract::{Path as UrlPath, State};
    use axum::response::Response;
    use axum::Json;
    use std::sync::Mutex;

    const PR: &str = "https://github.com/acme/app/pull/7";

    /// `claude` as the tests see it: it records every launch, and refuses them all when told to.
    #[derive(Default)]
    struct FakeClaude {
        launched: Mutex<Vec<(String, Vec<String>)>>,
        refuse: Option<&'static str>,
    }

    impl FakeClaude {
        fn names(&self) -> Vec<String> {
            self.launched.lock().unwrap().iter().map(|(_, args)| args[2].clone()).collect()
        }

        fn prompt(&self, name: &str) -> String {
            self.launched.lock().unwrap().iter().find(|(_, a)| a[2] == name).map(|(_, a)| a.last().unwrap().clone()).unwrap_or_default()
        }
    }

    impl Launcher for FakeClaude {
        async fn run(&self, cwd: &str, args: &[String]) -> Result<String, Refusal> {
            if let Some(why) = self.refuse {
                return Err((StatusCode::BAD_GATEWAY, why.into()));
            }
            self.launched.lock().unwrap().push((cwd.into(), args.to_vec()));
            Ok("started".into())
        }

        fn usage(&self) -> Option<alondra_core::usage::Usage> {
            None
        }
    }

    /// GitHub knows every PR, and its branch.
    struct FakeGithub;

    impl Gh for FakeGithub {
        async fn edit_base(&self, _url: &str, _base: &str) -> Result<(), String> {
            Ok(())
        }

        async fn view(&self, _url: &str) -> Option<Value> {
            Some(json!({"headRefName": "feat/model", "baseRefName": "main"}))
        }

        async fn exists(&self, _url: &str) -> bool {
            true
        }
    }

    fn hub_at(home: &Path) -> Arc<Hub> {
        let config = alondra_core::config::Config::resolve(home.to_path_buf(), &alondra_core::config::FileConfig::default(), |_| None, || Some("Ana".into()));
        Arc::new(Hub::open(config, false).unwrap())
    }

    fn seen(sid: &str, name: &str) -> Event {
        Event::DaemonSeen(DaemonObservation { session_id: sid.into(), short_id: sid.into(), name: Some(name.into()), cwd: Some("/Users/x/Projects/app".into()), pid: Some(1), ..Default::default() })
    }

    /// A fleet with an orchestrator, a live node `app/old`, a closed one, and request r-1.
    fn fleet(tag: &str) -> (Arc<Hub>, std::path::PathBuf) {
        let home = std::env::temp_dir().join(format!("alondra-orchestrate-{tag}-{}-{}", std::process::id(), now_millis()));
        let hub = hub_at(&home);
        hub.append(seen("orch", "orchestrator")).unwrap();
        hub.append(Event::MarkedOrchestrator { session_id: "orch".into() }).unwrap();
        hub.append(seen("s0", "app/old")).unwrap();
        hub.append(seen("gone", "app/gone")).unwrap();
        hub.append(Event::NodeRemoved { session_id: "gone".into() }).unwrap();
        let options = alondra_core::options::OptionsFile::default().orchestrator_defaults(Some("app"), None);
        hub.append(Event::RequestSent { request_id: "r-1".into(), options, text: Some("Menu by sections".into()), attachments: vec![] }).unwrap();
        (hub, home)
    }

    fn definition(v: Value) -> Definition {
        serde_json::from_value(v).unwrap()
    }

    fn orchestrator_told(hub: &Hub) -> Vec<String> {
        hub.read(|v| v.messages.values().filter(|m| m.session_id == "orch").map(|m| m.text.clone()).collect())
    }

    async fn body(r: Response) -> (StatusCode, Value) {
        let status = r.status();
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[test]
    fn a_definition_that_cannot_stand_is_refused_with_every_reason_and_records_nothing() {
        let (hub, home) = fleet("refused");
        let cwd = home.display().to_string();
        let before = hub.read(|v| v.seq);
        let refused = |d: Value| define(&hub, definition(d)).err().map(|r| (r.status, r.problems)).expect("refused");

        assert_eq!(refused(json!({"request": "r-9", "tasks": [{"title": "x", "brief": "y", "assignee": "new", "cwd": cwd}]})).0, StatusCode::NOT_FOUND);
        assert_eq!(refused(json!({"request": "r-1", "tasks": []})), (StatusCode::BAD_REQUEST, vec!["define at least one task".to_string()]));
        let (status, problems) = refused(json!({"request": "r-1", "tasks": [
            {"title": "Model", "brief": "Split the menu model.", "assignee": "new", "cwd": cwd, "depends_on": [2]},
            {"title": "UI", "brief": "", "assignee": "app/nobody"},
            {"title": "Docs", "brief": "Document it.", "assignee": "gone", "depends_on": [0, "t-99"]},
            {"title": "Merge alone", "brief": "b", "assignee": "orchestrator", "options": {"auto_merge": true, "effort": "max"}},
        ]}));
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(problems, [
            "tasks[1] “UI”: the brief is empty, and it is what the node is told",
            "tasks[1] “UI”: there is no node app/nobody: pick a live node (see candidates) or \"new\"",
            "tasks[2] “Docs”: node gone was closed: pick a live node (see candidates) or \"new\"",
            "tasks[3] “Merge alone”: effort max is not light, normal or hard",
            "tasks[3] “Merge alone”: auto-merge needs Ana to choose it in the request",
            "tasks[3] “Merge alone”: orchestrator is the orchestrator: give the task to a node, or \"new\"",
            "tasks[2] “Docs” depends on t-99, which does not exist",
            "tasks[0] “Model” → tasks[2] “Docs” → tasks[0] “Model” form a cycle",
        ]);
        assert_eq!(hub.read(|v| (v.seq, v.board.tasks.len())), (before, 0), "a refused definition leaves nothing behind");
    }

    /// Model, then UI on top of it, and Docs in parallel on a node that already has the context.
    #[tokio::test]
    async fn tasks_start_at_define_or_when_their_dependency_opens_a_pr_and_never_twice() {
        let (hub, home) = fleet("starts");
        let cwd = home.display().to_string();
        let defined = define(&hub, definition(json!({"request": "r-1", "tasks": [
            {"title": "Menu model", "brief": "Split the menu model by sections.", "assignee": "new", "cwd": cwd},
            {"title": "Menu UI", "brief": "Show the sections.", "assignee": "new", "cwd": cwd, "depends_on": [0]},
            {"title": "Docs", "brief": "Document the sections.", "assignee": "app/old"},
        ]}))).unwrap_or_else(|r| panic!("{:?}", r.problems));
        let ids: Vec<String> = defined["tasks"].as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap().to_string()).collect();
        let states: Vec<&str> = defined["tasks"].as_array().unwrap().iter().map(|t| t["state"].as_str().unwrap()).collect();
        assert_eq!(states, ["running", "waiting_dependency", "running"]);
        assert!(defined["tasks"][1]["next"].as_str().unwrap().starts_with(&format!("waits for {}", ids[0])), "{defined}");

        let claude = FakeClaude::default();
        let mut given_up = HashSet::new();
        assert_eq!(start_due(&hub, &claude, &FakeGithub, &mut given_up).await, [ids[0].clone(), ids[2].clone()]);
        assert_eq!(claude.names(), ["app/menu-model"]);
        assert!(claude.prompt("app/menu-model").starts_with("Split the menu model by sections.\n\n## Task"));
        let handed = hub.read(|v| v.messages.values().find(|m| m.session_id == "s0").cloned()).unwrap();
        assert_eq!(handed.about, Some(MessageAbout::Task { task: ids[2].clone() }));
        assert!(handed.text.contains(&format!("pass task \"{}\"", ids[2])) && handed.text.contains("Ana reviews its PR"), "{}", handed.text);
        assert!(hub.read(|v| v.board.tasks[&ids[2]].is_assigned_to("s0")));

        assert!(start_due(&hub, &claude, &FakeGithub, &mut given_up).await.is_empty(), "nothing starts twice");
        drop(hub);
        let hub = hub_at(&home);
        assert!(start_due(&hub, &claude, &FakeGithub, &mut HashSet::new()).await.is_empty(), "nor after a restart");

        hub.append(seen("s1", "app/menu-model")).unwrap();
        assert!(hub.read(|v| v.board.tasks[&ids[0]].is_assigned_to("s1")));
        hub.append(Event::PrOpened { url: PR.into(), session_id: "s1".into(), task: None }).unwrap();
        assert_eq!(hub.read(|v| v.board.tasks[&ids[1]].state), TaskState::Running);
        assert_eq!(start_due(&hub, &claude, &FakeGithub, &mut given_up).await, [ids[1].clone()]);
        let prompt = claude.prompt("app/menu-ui");
        assert!(prompt.contains("Base your work on its branch `feat/model`") && prompt.contains("gh pr create --base feat/model"), "{prompt}");
        let told = orchestrator_told(&hub);
        assert!(told.iter().any(|t| t.starts_with(&format!("[Alondra] Task {} “Menu UI” started in a new node app/menu-ui, on top of PR #7 of {}", ids[1], ids[0]))), "{told:?}");
    }

    #[tokio::test]
    async fn a_start_that_fails_is_left_to_the_orchestrator_once() {
        let (hub, home) = fleet("fails");
        define(&hub, definition(json!({"request": "r-1", "tasks": [{"title": "Model", "brief": "b", "assignee": "new", "cwd": home.display().to_string()}]}))).ok().unwrap();
        let claude = FakeClaude { refuse: Some("no such command"), ..Default::default() };
        let mut given_up = HashSet::new();
        assert!(start_due(&hub, &claude, &FakeGithub, &mut given_up).await.is_empty());
        assert!(start_due(&hub, &claude, &FakeGithub, &mut given_up).await.is_empty());
        let told: Vec<String> = orchestrator_told(&hub).into_iter().filter(|t| t.contains("could not start")).collect();
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(told[0].contains("no such command") && told[0].contains("Start it with launch or send with task"), "{}", told[0]);
    }

    #[tokio::test]
    async fn send_and_launch_with_a_task_give_it_to_one_node_only() {
        let (hub, home) = fleet("routes");
        let cwd = home.display().to_string();
        let d = define(&hub, definition(json!({"request": "r-1", "tasks": [
            {"title": "Model", "brief": "Split the model.", "assignee": "new", "cwd": cwd},
            {"title": "UI", "brief": "Show it.", "assignee": "new", "cwd": cwd, "depends_on": [0]},
        ]}))).ok().unwrap();
        let (model, ui) = (d["tasks"][0]["id"].as_str().unwrap().to_string(), d["tasks"][1]["id"].as_str().unwrap().to_string());
        hub.append(seen("s2", "app/other")).unwrap();
        let send = |key: &str, task: &str| {
            let b: crate::http::MessageBody = serde_json::from_value(json!({"text": "Take this one.", "from": "orchestrator", "task": task})).unwrap();
            crate::http::send_message(State(hub.clone()), UrlPath(key.to_string()), Json(b))
        };

        let (status, said) = body(send("app/old", &ui).await).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(said["error"].as_str().unwrap().starts_with(&format!("{ui} has not started: it waits for {model}")), "{said}");

        let (status, said) = body(send("app/old", &model).await).await;
        assert_eq!(status, StatusCode::OK, "{said}");
        let m = hub.read(|v| v.messages[&said["id"].as_u64().unwrap()].clone());
        assert_eq!((m.text, m.about), (format!("[Task {model} “Model”] Take this one."), Some(MessageAbout::Task { task: model.clone() })));
        assert!(hub.read(|v| v.board.tasks[&model].is_assigned_to("s0")), "a task no node had becomes this node's");
        assert!(start_due(&hub, &FakeClaude::default(), &FakeGithub, &mut HashSet::new()).await.is_empty(), "so Alondra does not launch one for it");

        let (status, said) = body(send("app/other", &model).await).await;
        assert_eq!((status, said["error"].as_str().unwrap()), (StatusCode::CONFLICT, format!("{model} is worked on by node app/old: send to it instead").as_str()));

        let claude = FakeClaude::default();
        let launch = |task: &str| {
            let b: crate::http::LaunchBody = serde_json::from_value(json!({"name": "app/model-2", "cwd": cwd, "task": task})).unwrap();
            crate::http::launch_with(&hub, b, &claude)
        };
        assert_eq!(body(launch(&model).await).await.0, StatusCode::CONFLICT, "its node is alive");
        hub.append(Event::NodeRemoved { session_id: "s0".into() }).unwrap();
        assert_eq!(body(launch(&model).await).await.1["error"], format!("{model} was cancelled: reopen it first"), "closing its node dropped it");
        hub.append(Event::TaskMoved { task: model.clone(), event: alondra_core::tasks::TaskEvent::Reopen }).unwrap();
        let (status, said) = body(launch(&model).await).await;
        assert_eq!(status, StatusCode::OK, "{said}");
        assert!(claude.prompt("app/model-2").starts_with("Split the model."), "without a prompt the node gets the task's brief");
        hub.append(seen("s3", "app/model-2")).unwrap();
        assert!(hub.read(|v| v.board.tasks[&model].is_assigned_to("s3")), "the new node takes the task its old node left");
    }

    #[test]
    fn candidates_need_a_task_or_a_repo() {
        let (hub, home) = fleet("candidates");
        let d = define(&hub, definition(json!({"request": "r-1", "tasks": [{"title": "Model", "brief": "b", "assignee": "new", "cwd": home.display().to_string(), "options": {"project": "app"}}]}))).ok().unwrap();
        let id = d["tasks"][0]["id"].as_str().unwrap();
        assert_eq!(candidates(&hub, None, None).unwrap_err().0, StatusCode::BAD_REQUEST);
        assert_eq!(candidates(&hub, Some("t-404"), None).unwrap_err().0, StatusCode::NOT_FOUND);
        let found = candidates(&hub, Some(id), None).unwrap();
        assert_eq!(found["candidates"][0]["node"], "app/old", "{found}");
        assert_eq!(found["candidates"][0]["why"], json!(["works in project app"]));
    }
}
