//! The orchestrator's hands on tasks: turning a request into tasks, the nodes a task could go
//! to, launching nodes, and starting every task that can start, at `define` or once its
//! dependencies let it.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use alondra_core::options::{OptionsFile, Resolved};
use alondra_core::tasks::{Dependency, NewTask, Start, Task, TaskOptions, TaskState};
use alondra_core::event::CompactionStep;
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

    /// Runs `claude` for a domain v2 producer, whose capability travels only in
    /// `env`. A launcher that cannot carry it refuses instead of launching a
    /// session that could never act.
    fn run_with_env(&self, cwd: &str, args: &[String], env: &[(String, String)]) -> impl Future<Output = Result<String, Refusal>> + Send {
        let _ = (cwd, args, env);
        async { Err((StatusCode::SERVICE_UNAVAILABLE, "this launcher cannot carry a producer capability".into())) }
    }

    /// The subscription's usage, which can refuse a launch.
    fn usage(&self) -> Option<alondra_core::usage::Usage> {
        crate::usage::current(&crate::watcher::claude_home())
    }

    /// The node plugin every launched session loads.
    fn plugin(&self) -> Option<std::path::PathBuf> {
        crate::plugins::dir(&crate::watcher::claude_home(), crate::plugins::NODE)
    }

    /// The session id behind the short id `claude --bg` printed, which it chose itself.
    fn session_of(&self, short: &str) -> impl Future<Output = Option<String>> + Send {
        let short = short.to_owned();
        async move {
            let out = tokio::time::timeout(std::time::Duration::from_secs(20), tokio::process::Command::new("claude")
                .args(["agents", "--json", "--all"]).kill_on_drop(true).stdin(std::process::Stdio::null()).output())
                .await.ok()?.ok()?;
            crate::domain_sessions::session_of_short(&String::from_utf8_lossy(&out.stdout), &short)
        }
    }
}

pub struct ClaudeCli;

impl Launcher for ClaudeCli {
    async fn run(&self, cwd: &str, args: &[String]) -> Result<String, Refusal> {
        self.run_with_env(cwd, args, &[]).await
    }

    async fn run_with_env(&self, cwd: &str, args: &[String], env: &[(String, String)]) -> Result<String, Refusal> {
        let mut cmd = tokio::process::Command::new("claude");
        cmd.current_dir(cwd).args(args).kill_on_drop(true);
        cmd.env_remove("ALONDRA_DOMAIN_CAPABILITY").env_remove("ALONDRA_DOMAIN_PROTOCOL");
        cmd.envs(env.iter().map(|(key, value)| (key.as_str(), value.as_str())));
        match tokio::time::timeout(alondra_core::LAUNCH_TIMEOUT, cmd.output()).await {
            Ok(Ok(out)) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).to_string()),
            Ok(Ok(out)) => Err((StatusCode::BAD_GATEWAY, String::from_utf8_lossy(&out.stderr).to_string())),
            Ok(Err(e)) => Err((StatusCode::BAD_GATEWAY, e.to_string())),
            Err(_) => Err((StatusCode::GATEWAY_TIMEOUT, format!("claude --bg did not respond within {} s", alondra_core::LAUNCH_TIMEOUT.as_secs()))),
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
    let Some(plugin) = launcher.plugin() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "Alondra's plugin was not found: the Alondra marketplace is not registered in Claude Code. Run `alondra setup` in the Alondra repo.".into(),
        ));
    };
    let mut args = crate::options::launch_args(&l.name, l.permission_mode.as_deref(), l.model.as_deref(), &l.prompt, &l.options, l.request_id.as_deref(), &plugin);
    if let Some(request) = &l.request_id {
        let patterns = hub.schedules.tools_for_request(request)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("cannot load scheduled tool policy: {e}")))?;
        if !patterns.is_empty() {
            let prompt = args.pop().expect("launch prompt");
            let separator = args.pop().expect("launch separator");
            args.extend(["--disallowedTools".into(), patterns.join(" "), separator, prompt]);
        }
    }
    let _launching = hub.launching();
    let output = launcher.run(&l.cwd, &args).await?;
    let session = alondra_core::sessions::launched_short(&output);
    let launched = Event::Launched { name: l.name.clone(), request_id: l.request_id.clone(), auto_merge: l.options.auto_merge, task: l.task.clone(), title: l.title.clone(), session: session.clone() };
    let internal = |e: anyhow::Error| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    hub.append(launched).map_err(internal)?;
    if let Some(short) = &session {
        hub.admit_launched(short).map_err(internal)?;
    }
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
    !v.removed.contains_key(session_id) && v.nodes.get(session_id).is_some_and(|n| n.alive || n.in_daemon || (n.is_orchestrator && v.orchestrator.is_some()))
}

fn node_name(v: &View, session_id: &str) -> String {
    v.nodes.get(session_id).map(|n| n.display_name().to_string()).unwrap_or_else(|| session_id.chars().take(8).collect())
}

/// Why a task cannot be handed to a node now: it waits for another, it is finished, a node was
/// just launched for it and has not shown up yet, or a live node other than `to` works on it.
fn not_handable(v: &View, id: &str, to: Option<&str>, now: Millis) -> Option<Refusal> {
    let Some(task) = v.board.tasks.get(id) else { return Some((StatusCode::NOT_FOUND, format!("task {id} does not exist"))) };
    let conflict = |why: String| Some((StatusCode::CONFLICT, why));
    match task.state {
        TaskState::WaitingDependency if task.awaiting.is_none() => {
            let on = v.task_guards(task).waiting_on.unwrap_or_default();
            return conflict(format!("{id} has not started: it waits for {on}, and Alondra starts it when {on} has its PR open or is done"));
        }
        TaskState::Cancelled => return conflict(format!("{id} was cancelled: reopen it first")),
        TaskState::Done if to.is_none() => return conflict(format!("{id} is done: reopen it first")),
        _ => {}
    }
    let on_its_way = task.launched && task.assignee.is_none() && task.launched_at.is_some_and(|at| now - at < alondra_core::tasks::LAUNCH_WINDOW);
    if on_its_way {
        return conflict(format!(
            "{id} was just launched in node {}, which takes it when it shows up (within 10 min): wait for it rather than giving the task to another node",
            task.launch_name()
        ));
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
    let task = hub.read(|v| match not_handable(v, id, None, now_millis()) {
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

/// Gives the task to the node, when it is not its yet, and types it the text about it: after a
/// `/compact` when the node is idle with its context full past `compact_idle_nodes_at`.
pub fn hand(hub: &Hub, id: &str, session_id: &str, text: String, from: MessageFrom, attachments: Vec<Attachment>) -> Result<Message, Refusal> {
    let recorded = hub.append_all(|v| hand_events(hub, v, id, session_id, &text, from, &attachments));
    match recorded {
        Ok(Ok(events)) => events.into_iter().find_map(|(_, e)| if let Event::MessageQueued(m) = e { Some(m) } else { None }).ok_or((StatusCode::INTERNAL_SERVER_ERROR, "the message was not recorded".into())),
        Ok(Err(r)) => Err(r),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

/// Builds the durable assignment/compaction/message intent; delivery happens later in wake.
pub(crate) fn hand_events(hub: &Hub, v: &View, id: &str, session_id: &str,
    text: &str, from: MessageFrom, attachments: &[Attachment]) -> Result<Vec<Event>, Refusal> {
    if hub.is_closing(session_id) {
        return Err((StatusCode::CONFLICT, format!("node {} is being closed: give {id} to another node, or launch one for it", node_name(v, session_id))));
    }
    if let Some(r) = not_handable(v, id, Some(session_id), now_millis()) {
        return Err(r);
    }
    let mut events = vec![];
    if !v.board.tasks[id].is_assigned_to(session_id) {
        events.push(Event::TaskAssigned { task: id.to_string(), session_id: session_id.to_string(), agent_id: None });
    }
    if v.nodes.get(session_id).is_some_and(|n| alondra_core::upkeep::compaction_due(v, n, now_millis(), hub.config.compact_idle_nodes_at)) {
        events.push(Event::Compaction { session_id: session_id.to_string(), step: CompactionStep::Requested });
    }
    events.push(Event::MessageQueued(Message {
        id: v.next_message_id(),
        session_id: session_id.to_string(),
        from,
        text: text.to_string(),
        answers_question: None,
        created_at: now_millis(),
        handed_at: None,
        delivered_at: None,
        attachments: attachments.to_vec(),
        about: Some(MessageAbout::Task { task: id.to_string() }),
    }));
    Ok(events)
}

/// What the orchestrator passes to `define`.
#[derive(Deserialize)]
pub struct Definition {
    #[serde(default)]
    pub request: Option<String>,
    /// What the user asked, in their words, for a request they made outside the apps.
    #[serde(default)]
    pub text: Option<String>,
    /// A short name for the request, which the user reads in place of its text.
    #[serde(default)]
    pub title: Option<String>,
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
        Some(id) if v.board.requests.get(id).and_then(|r| r.closed.as_ref()).is_some_and(|c| c.withdrawn) => {
            return Err(refuse(StatusCode::CONFLICT, format!("{user} withdrew {id}: do not define it")));
        }
        Some(id) if v.board.requests.get(id).is_some_and(|r| r.closed.is_some()) => {
            return Err(refuse(StatusCode::CONFLICT, format!("request {id} is closed: if {user} wants the work after all, define it as a new request, with text")));
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
    if let Some(title) = d.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        events.push(Event::RequestTitled { request_id: request.clone(), title: title.to_string() });
    }
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
                let launched = Event::Launched { name: name.clone(), request_id: Some(task.request.clone()), auto_merge: task.options.auto_merge, task: Some(task.id.clone()), title: None, session: None };
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
            let text = format!("A new task from the orchestrator: “{}”.\n\n{brief}\n\n{}", task.title, pr_handling(task.options.auto_merge, &hub.config.name));
            hand(hub, &task.id, session_id, text, MessageFrom::Orchestrator, vec![]).map(|_| format!("on node {name}")).map_err(|(_, e)| e)
        }
        None => Err("it has no start".into()),
    }
}

/// Alondra's message to the orchestrator, when one is live.
pub(crate) fn orchestrator_message(v: &View, text: String) -> Option<Event> {
    let o = v.nodes.values().find(|n| n.is_orchestrator && live(v, &n.session_id))?;
    Some(Event::MessageQueued(Message {
        id: v.next_message_id(),
        session_id: o.session_id.clone(),
        from: MessageFrom::Alondra,
        text,
        answers_question: None,
        created_at: now_millis(),
        handed_at: None,
        delivered_at: None,
        attachments: vec![],
        about: None,
    }))
}

/// What the orchestrator is told of tasks nobody works on yet, which wait for it.
pub(crate) fn unstarted_notice(view: &View, ids: &[String]) -> String {
    let titles: Vec<&str> = ids.iter().filter_map(|id| view.board.tasks.get(id).map(|task| task.title.as_str())).collect();
    match titles.as_slice() {
        [] => "Some newly defined work is waiting for an agent.".into(),
        [title] => format!("The task “{title}” is waiting for an agent."),
        _ => format!("These tasks are waiting for agents: {}.", titles.iter().map(|title| format!("“{title}”")).collect::<Vec<_>>().join(", ")),
    }
}

pub(crate) fn tell_orchestrator(hub: &Hub, text: String) {
    let told = hub.append_with(|v| orchestrator_message(v, text));
    if let Err(e) = told {
        log!("could not tell the orchestrator: {e}");
    }
}

/// Where a started task builds, for the orchestrator.
fn built_on(v: &View, task: &Task, heads: &BTreeMap<u64, String>) -> String {
    let branch = |p: &alondra_core::pr::PullRequest| heads.get(&p.id).or(p.head.as_ref()).map(|h| format!(" (`{h}`)")).unwrap_or_default();
    let below: Vec<String> = v.stacked_on(task).into_iter().map(|(d, p)| format!("PR #{} of {}{}", p.number, d.title, branch(p))).collect();
    if below.is_empty() { "on the default branch".into() } else { format!("on top of {}", below.join(", ")) }
}

/// The orchestrator heard that this start of the task failed: starting it again is its call.
fn start_failure_told(v: &View, task: &Task) -> bool {
    let about = format!("Could not start “{}”:", task.title);
    v.messages.values().any(|m| m.from == MessageFrom::Alondra && m.created_at >= task.state_since && m.text.starts_with(&about))
}

/// Starts every task that can start and has not: the ones just defined with nothing to wait
/// for, and the ones whose dependencies now have their PR open or are done. A start that fails
/// is left to the orchestrator, told once, until the task moves again (a reopen); `given_up`
/// keeps those by task and since when it is in its state, and the orchestrator's messages keep
/// them across a restart.
pub async fn start_due(hub: &Hub, launcher: &impl Launcher, gh: &impl Gh, given_up: &mut HashSet<(String, Millis)>) -> Vec<String> {
    let _one_at_a_time = hub.starts.lock().await;
    let due: Vec<Task> = hub.read(|v| {
        v.tasks_to_start().into_iter().filter(|t| !given_up.contains(&(t.id.clone(), t.state_since)) && !start_failure_told(v, t)).cloned().collect()
    });
    let mut started = vec![];
    for task in due {
        if matches!(&task.start, Some(Start::NewNode { name, .. }) if name.starts_with("scheduled/"))
            && hub.schedules.failed_launch(&task.request).unwrap_or(true) {
            continue;
        }
        if !hub.read(|v| v.board.tasks.get(&task.id).is_some_and(Task::needs_start)) {
            continue;
        }
        match start(hub, &task, launcher, gh).await {
            Ok((how, place)) => {
                log!("task {} started {how}, {place}", task.id);
                if !task.depends_on.is_empty() {
                    let dependencies = hub.read(|view| task.depends_on.iter().filter_map(|id| view.board.tasks.get(id).map(|dependency| format!("“{}”", dependency.title))).collect::<Vec<_>>().join(", "));
                    tell_orchestrator(hub, format!("“{}” has started {how}, {place}; its dependencies ({dependencies}) now have an open PR or are done.", task.title));
                }
                started.push(task.id.clone());
            }
            Err(why) => {
                log!("task {} could not start: {why}", task.id);
                given_up.insert((task.id.clone(), task.state_since));
                if matches!(&task.start, Some(Start::NewNode { name, .. }) if name.starts_with("scheduled/")) {
                    if let Err(e) = hub.schedules.launch_failed(&task.request, &why) {
                        log!("scheduled task {}: could not save launch error: {e}", task.id);
                    }
                } else {
                    tell_orchestrator(
                        hub,
                        format!("Could not start “{}”: {why}.", task.title),
                    );
                }
            }
        }
    }
    started
}

fn cancelled_with_text(task: &Task, dependency_title: &str) -> String {
    format!(
        "The task “{}” was cancelled because it depended on “{dependency_title}”, which was also cancelled. If the work is still wanted, reopen the dependency and define the task again.",
        task.title
    )
}

/// The dependency a task was cancelled with, when the machine cancelled it for that.
fn cancelled_with(task: &Task) -> Option<&str> {
    let alondra_core::tasks::TaskEvent::Cancel { reason: Some(reason) } = &task.history.last()?.event else { return None };
    task.depends_on.iter().find(|d| *reason == alondra_core::tasks::dependency_cancelled(d)).map(String::as_str)
}

/// What the orchestrator has not heard of the tasks cancelled since `since` because one they
/// waited for was cancelled. Whether it heard is read from its messages, so a restart tells
/// nothing twice; those go with the retention window while a cancelled task stays until it is
/// archived, so a cancel older than the window is never looked at: it was told, or went untold
/// while the orchestrator was away, and is no news now.
fn cancelled_dependents_untold(v: &View, since: Millis) -> Vec<String> {
    let Some(o) = v.nodes.values().find(|n| n.is_orchestrator && live(v, &n.session_id)) else { return vec![] };
    v.board
        .tasks
        .values()
        .filter(|t| t.state == TaskState::Cancelled && t.state_since >= since)
        .filter_map(|t| cancelled_with(t).and_then(|d| v.board.tasks.get(d).map(|dependency| (t, cancelled_with_text(t, &dependency.title)))) )
        .filter(|(t, text)| !v.messages.values().any(|m| m.session_id == o.session_id && m.created_at >= t.state_since && m.text == *text))
        .map(|(_, text)| text)
        .collect()
}

/// Tells the orchestrator about each task cancelled because one it waited for was cancelled.
pub fn tell_cancelled_dependents(hub: &Hub) {
    let window = (hub.config.retention_hours * 3_600_000.0) as Millis;
    for text in hub.read(|v| cancelled_dependents_untold(v, now_millis() - window)) {
        tell_orchestrator(hub, text);
    }
}

pub async fn run(hub: Arc<Hub>) {
    let mut given_up = HashSet::new();
    loop {
        let seen = hub.changes();
        start_due(&hub, &ClaudeCli, &crate::prs::GhCli, &mut given_up).await;
        tell_cancelled_dependents(&hub);
        hub.wait_changes(seen, Duration::from_secs(30)).await;
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
        printed: Option<&'static str>,
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
            Ok(self.printed.unwrap_or("started").into())
        }

        fn usage(&self) -> Option<alondra_core::usage::Usage> {
            None
        }

        fn plugin(&self) -> Option<std::path::PathBuf> {
            Some("/Users/x/Projects/alondra/plugin".into())
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

    /// Round 2: define took a request the orchestrator had closed, whose tasks then ran under a
    /// request shown as done.
    #[test]
    fn a_closed_request_is_not_defined_and_the_orchestrator_hears_to_make_a_new_one() {
        let (hub, home) = fleet("closed-request");
        let cwd = home.display().to_string();
        hub.append(Event::RequestClosed { request_id: "r-1".into(), answer: Some("It is in review".into()), withdrawn: false }).unwrap();
        let before = hub.read(|v| v.seq);
        let refused = define(&hub, definition(json!({"request": "r-1", "tasks": [{"title": "Model", "brief": "Split the menu model.", "assignee": "new", "cwd": cwd}]}))).err().expect("refused");
        assert_eq!(refused.status, StatusCode::CONFLICT);
        assert_eq!(refused.problems, ["request r-1 is closed: if Ana wants the work after all, define it as a new request, with text"]);
        assert_eq!(hub.read(|v| v.seq), before, "nothing recorded");
        assert_eq!(hub.read(|v| v.board.request_state("r-1")), alondra_core::tasks::RequestState::Done);
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
        assert!(told.iter().any(|t| t.starts_with("“Menu UI” has started in a new node app/menu-ui, on top of PR #7 of Menu model")), "{told:?}");
    }

    #[tokio::test]
    async fn a_start_that_fails_is_left_to_the_orchestrator_once() {
        let (hub, home) = fleet("fails");
        define(&hub, definition(json!({"request": "r-1", "tasks": [{"title": "Model", "brief": "b", "assignee": "new", "cwd": home.display().to_string()}]}))).ok().unwrap();
        let claude = FakeClaude { refuse: Some("no such command"), ..Default::default() };
        let mut given_up = HashSet::new();
        assert!(start_due(&hub, &claude, &FakeGithub, &mut given_up).await.is_empty());
        assert!(start_due(&hub, &claude, &FakeGithub, &mut given_up).await.is_empty());
        let told: Vec<String> = orchestrator_told(&hub).into_iter().filter(|t| t.contains("Could not start")).collect();
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(told[0].contains("no such command") && !told[0].contains("task_id") && !told[0].contains("launch or send"), "{}", told[0]);
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

    fn send_task(hub: &Arc<Hub>, key: &str, task: &str) -> impl Future<Output = Response> + use<> {
        let b: crate::http::MessageBody = serde_json::from_value(json!({"text": "Take this one.", "from": "orchestrator", "task": task})).unwrap();
        crate::http::send_message(State(hub.clone()), UrlPath(key.to_string()), Json(b))
    }

    fn hook(sid: &str, event: &str) -> Event {
        Event::Hook(alondra_core::HookEvent { session_id: sid.into(), event: event.into(), ..Default::default() })
    }

    /// Reusing an idle node whose context is past `compact_idle_nodes_at` types `/compact` first
    /// and holds the task's text until the node's `PostCompact`: the new task starts from a
    /// summary of the old work, not on top of it. Below the threshold, in a turn, or with the
    /// setting off, the text goes straight in.
    #[tokio::test]
    async fn an_idle_node_full_of_context_is_compacted_before_its_new_task_is_typed() {
        let (hub, home) = fleet("compact");
        let cwd = home.display().to_string();
        hub.append(seen("s2", "app/busy")).unwrap();
        hub.append(hook("s2", "UserPromptSubmit")).unwrap();
        hub.append(seen("s3", "app/light")).unwrap();
        for (sid, percent) in [("s0", 45.0), ("s2", 80.0), ("s3", 20.0)] {
            hub.append(Event::ContextMeasured { session_id: sid.into(), percent, window: 1_000_000 }).unwrap();
        }
        let d = define(&hub, definition(json!({"request": "r-1", "tasks": [
            {"title": "Model", "brief": "b", "assignee": "new", "cwd": cwd},
            {"title": "UI", "brief": "b", "assignee": "new", "cwd": cwd},
            {"title": "Docs", "brief": "b", "assignee": "new", "cwd": cwd},
        ]}))).ok().unwrap();
        let task = |i: usize| d["tasks"][i]["id"].as_str().unwrap().to_string();
        let typeable = |sid: &str| hub.read(|v| v.deliverable_messages(sid).count());
        let compacting = |sid: &str| hub.read(|v| v.nodes[sid].compaction.is_some());

        assert_eq!(body(send_task(&hub, "app/old", &task(0)).await).await.0, StatusCode::OK);
        assert!(compacting("s0"), "45 % is past the 30 % threshold");
        assert_eq!(typeable("s0"), 0, "the task's text waits for the compaction");
        hub.append(Event::Compaction { session_id: "s0".into(), step: CompactionStep::Typed }).unwrap();
        assert_eq!(typeable("s0"), 0, "for its end, not for /compact to be typed");
        hub.append(hook("s0", "PostCompact")).unwrap();
        assert_eq!(typeable("s0"), 1, "the compaction ended: the task goes in");
        assert!(hub.read(|v| v.nodes["s0"].context.is_none()), "the measure from before the compaction is void");

        assert_eq!(body(send_task(&hub, "app/busy", &task(1)).await).await.0, StatusCode::OK);
        assert!(!compacting("s2"), "a node in a turn is not compacted: /compact would queue behind the turn");
        assert_eq!(typeable("s2"), 1);

        assert_eq!(body(send_task(&hub, "app/light", &task(2)).await).await.0, StatusCode::OK);
        assert!(!compacting("s3"), "20 % is below the threshold");
        assert_eq!(typeable("s3"), 1);

        let off = alondra_core::config::Config::resolve(home.join("off"), &alondra_core::config::FileConfig::default(), |k| (k == "ALONDRA_COMPACT_IDLE_NODES_AT").then(|| "off".to_string()), || Some("Ana".into()));
        let hub = Arc::new(Hub::open(off, false).unwrap());
        hub.append(seen("s0", "app/old")).unwrap();
        hub.append(Event::ContextMeasured { session_id: "s0".into(), percent: 90.0, window: 200_000 }).unwrap();
        let options = alondra_core::options::OptionsFile::default().orchestrator_defaults(Some("app"), None);
        hub.append(Event::RequestSent { request_id: "r-1".into(), options, text: Some("x".into()), attachments: vec![] }).unwrap();
        let d = define(&hub, definition(json!({"request": "r-1", "tasks": [{"title": "Model", "brief": "b", "assignee": "new", "cwd": cwd}]}))).ok().unwrap();
        assert_eq!(body(send_task(&hub, "app/old", d["tasks"][0]["id"].as_str().unwrap()).await).await.0, StatusCode::OK);
        assert!(!hub.read(|v| v.nodes["s0"].compaction.is_some()), "compact_idle_nodes_at = off");
    }

    /// The start loop launched a node for the task; before that node showed up, `send` gave the
    /// task to another node, and the Launched event then took it back: two nodes on one task.
    #[tokio::test]
    async fn a_task_launched_by_the_start_loop_goes_to_no_other_node_before_its_own_shows_up() {
        let (hub, home) = fleet("launched");
        let cwd = home.display().to_string();
        let d = define(&hub, definition(json!({"request": "r-1", "tasks": [{"title": "Model", "brief": "Split the model.", "assignee": "new", "cwd": cwd}]}))).ok().unwrap();
        let model = d["tasks"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(start_due(&hub, &FakeClaude::default(), &FakeGithub, &mut HashSet::new()).await, [model.clone()]);

        let (status, said) = body(send_task(&hub, "app/old", &model).await).await;
        assert_eq!(status, StatusCode::CONFLICT, "{said}");
        assert!(said["error"].as_str().unwrap().contains("was just launched in node app/model"), "{said}");
        let relaunch: crate::http::LaunchBody = serde_json::from_value(json!({"name": "app/model-2", "cwd": cwd, "task": model})).unwrap();
        assert_eq!(body(crate::http::launch_with(&hub, relaunch, &FakeClaude::default()).await).await.0, StatusCode::CONFLICT, "nor a second launch");
        assert_eq!(hub.read(|v| v.board.tasks[&model].assignee.clone()), None);

        hub.append(seen("s1", "app/model")).unwrap();
        assert!(hub.read(|v| v.board.tasks[&model].is_assigned_to("s1")), "its own node takes it");
    }

    /// A task made by `launch` went by its node's name in the apps.
    #[tokio::test]
    async fn a_launch_with_a_title_names_the_task_it_makes() {
        let (hub, home) = fleet("launch-title");
        let b: crate::http::LaunchBody = serde_json::from_value(json!({"name": "app/login", "cwd": home.display().to_string(), "prompt": "Fix the crash.", "title": "Crash del login"})).unwrap();
        assert_eq!(body(crate::http::launch_with(&hub, b, &FakeClaude::default()).await).await.0, StatusCode::OK);
        hub.append(seen("s9", "app/login")).unwrap();
        let task = hub.read(|v| v.session_task("s9").map(|t| (t.title.clone(), t.launch_name().to_string())));
        assert_eq!(task, Some(("Crash del login".to_string(), "app/login".to_string())));
    }

    /// A launched node loads Alondra's plugin for itself only, and the short id `claude --bg`
    /// printed is recorded, so the session is Alondra's even before its hooks say so.
    #[tokio::test]
    async fn a_launch_loads_the_plugin_and_records_the_session_it_printed() {
        let (hub, home) = fleet("launch-plugin");
        let printed = "backgrounded · 4e5f6a7b · app/login\n  claude agents             list sessions\n  claude attach 4e5f6a7b    open in this terminal\n";
        let claude = FakeClaude { printed: Some(printed), ..Default::default() };
        let b: crate::http::LaunchBody = serde_json::from_value(json!({"name": "app/login", "cwd": home.display().to_string(), "prompt": "Fix the crash."})).unwrap();
        assert_eq!(body(crate::http::launch_with(&hub, b, &claude).await).await.0, StatusCode::OK);
        let args = claude.launched.lock().unwrap()[0].1.clone();
        let dir = args.iter().position(|a| a == "--plugin-dir").expect("the plugin is loaded with the launch");
        assert_eq!(args[dir + 1], "/Users/x/Projects/alondra/plugin");
        assert!(args.iter().any(|a| a.contains(alondra_core::LAUNCHED_ENV)), "{args:?}");
        assert!(hub.read(|v| v.launch_printed("4e5f6a7b-1111-4222-8333-944445555666")));
    }

    /// A task handed to a node Alondra was closing (a free node closed on its own) was cancelled
    /// by the close with nobody told.
    #[tokio::test]
    async fn a_task_is_not_handed_to_a_node_alondra_is_closing() {
        let (hub, _home) = fleet("closing");
        let d = define(&hub, definition(json!({"request": "r-1", "tasks": [{"title": "Docs", "brief": "Document it.", "assignee": "new", "cwd": "/tmp"}]}))).ok().unwrap();
        let docs = d["tasks"][0]["id"].as_str().unwrap().to_string();
        let closing = hub.closing("s0");
        let (status, said) = body(send_task(&hub, "app/old", &docs).await).await;
        assert_eq!(status, StatusCode::CONFLICT, "{said}");
        assert!(said["error"].as_str().unwrap().contains("app/old is being closed"), "{said}");
        drop(closing);
        assert_eq!(body(send_task(&hub, "app/old", &docs).await).await.0, StatusCode::OK, "once the close is over");
    }

    /// A start that failed was kept in memory by task id: a reopen never started it again, and a
    /// restart told the orchestrator again.
    #[tokio::test]
    async fn a_failed_start_waits_for_the_orchestrator_across_a_restart_and_is_tried_again_after_a_reopen() {
        let (hub, home) = fleet("given-up");
        let d = define(&hub, definition(json!({"request": "r-1", "tasks": [{"title": "Model", "brief": "b", "assignee": "new", "cwd": home.display().to_string()}]}))).ok().unwrap();
        let model = d["tasks"][0]["id"].as_str().unwrap().to_string();
        let refusing = FakeClaude { refuse: Some("no such command"), ..Default::default() };
        assert!(start_due(&hub, &refusing, &FakeGithub, &mut HashSet::new()).await.is_empty());
        drop(hub);
        let hub = hub_at(&home);
        assert!(start_due(&hub, &refusing, &FakeGithub, &mut HashSet::new()).await.is_empty());
        let told = |hub: &Hub| orchestrator_told(hub).into_iter().filter(|t| t.contains("Could not start")).count();
        assert_eq!(told(&hub), 1, "a restart does not tell it again");

        let mut given_up = HashSet::new();
        for event in [alondra_core::tasks::TaskEvent::Cancel { reason: None }, alondra_core::tasks::TaskEvent::Reopen] {
            hub.append(Event::TaskMoved { task: model.clone(), event }).unwrap();
        }
        let claude = FakeClaude::default();
        assert_eq!(start_due(&hub, &claude, &FakeGithub, &mut given_up).await, [model.clone()], "reopened, it starts again");
        assert_eq!(claude.names(), ["app/model"]);
    }

    /// A task cancelled because the one it waited for was cancelled: the orchestrator hears it once.
    #[tokio::test]
    async fn the_orchestrator_hears_once_that_a_task_was_cancelled_with_the_one_it_waited_for() {
        let (hub, home) = fleet("cascade");
        let cwd = home.display().to_string();
        let d = define(&hub, definition(json!({"request": "r-1", "tasks": [
            {"title": "Model", "brief": "Split the model.", "assignee": "new", "cwd": cwd},
            {"title": "UI", "brief": "Show it.", "assignee": "new", "cwd": cwd, "depends_on": [0]},
        ]}))).ok().unwrap();
        let (model, ui) = (d["tasks"][0]["id"].as_str().unwrap().to_string(), d["tasks"][1]["id"].as_str().unwrap().to_string());
        let (status, _) = body(crate::http::cancel_task(State(hub.clone()), UrlPath(model.clone()), None).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(hub.read(|v| v.board.tasks[&ui].state), TaskState::Cancelled);
        tell_cancelled_dependents(&hub);
        tell_cancelled_dependents(&hub);
        let told: Vec<String> = orchestrator_told(&hub).into_iter().filter(|t| t.contains("was cancelled because it depended on")).collect();
        assert_eq!(told, ["The task “UI” was cancelled because it depended on “Model”, which was also cancelled. If the work is still wanted, reopen the dependency and define the task again.".to_string()]);
        drop(hub);
        tell_cancelled_dependents(&hub_at(&home));
        assert_eq!(orchestrator_told(&hub_at(&home)).into_iter().filter(|t| t.contains("was cancelled because it depended on")).count(), 1, "nor after a restart");
    }

    /// Round 2: the notice it was told with goes with the retention window, while the cancelled
    /// task stays until archived: it was told again every 14 days.
    #[test]
    fn a_cascade_cancel_is_not_told_again_once_the_window_prunes_its_notice() {
        const DAY: Millis = 86_400_000;
        let task = |id: &str, depends_on: &[&str]| {
            Event::TaskCreated(NewTask {
                id: id.into(), request: "r-1".into(), title: id.into(), brief: Some("b".into()), depends_on: depends_on.iter().map(|d| d.to_string()).collect(),
                options: TaskOptions::default(), session_id: None, launched: false, launched_as: None,
                start: Some(Start::NewNode { name: format!("app/{id}"), cwd: "/tmp".into(), model: None }),
            })
        };
        let options = OptionsFile::default().orchestrator_defaults(Some("app"), None);
        let mut v = View::default();
        for (at, e) in [
            (1, Event::TasksRecorded),
            (2, seen("orch", "orchestrator")),
            (3, Event::MarkedOrchestrator { session_id: "orch".into() }),
            (10, Event::RequestSent { request_id: "r-1".into(), options, text: Some("Menu".into()), attachments: vec![] }),
            (20, task("t-a", &[])),
            (21, task("t-b", &["t-a"])),
            (30, Event::TaskMoved { task: "t-a".into(), event: alondra_core::tasks::TaskEvent::Cancel { reason: None } }),
        ] {
            v.record(at, &e);
        }
        assert_eq!(v.board.tasks["t-b"].state, TaskState::Cancelled);
        let untold = cancelled_dependents_untold(&v, 0);
        assert_eq!(untold.len(), 1, "{untold:?}");
        let told = Message { id: 1, session_id: "orch".into(), from: MessageFrom::Alondra, text: untold[0].clone(), answers_question: None, created_at: 40, handed_at: Some(41), delivered_at: Some(42), attachments: vec![], about: None };
        v.record(40, &Event::MessageQueued(told));
        assert!(cancelled_dependents_untold(&v, 0).is_empty());

        let (now, window) = (15 * DAY, 14 * DAY);
        v.record(now, &Event::Pruned { before: now - window });
        assert_eq!(v.board.tasks["t-b"].state, TaskState::Cancelled, "the cancelled task outlives the window until archived");
        assert!(v.messages.is_empty(), "its notice does not");
        assert!(cancelled_dependents_untold(&v, now - window).is_empty(), "so it is not told again");
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
