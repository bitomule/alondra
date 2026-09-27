//! The Tasks screen: every request with its task tree, what is happening to each task now, and
//! what the other screens say about a card's or a node's tasks.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use alondra_core::now_millis;
use alondra_core::pr::CheckState;
use alondra_core::tasks::{BlockerKind, RequestState, TaskState, Transition};
use crossterm::event::{KeyCode, KeyEvent};
use serde_json::{Value, json};

use super::keys::{self, Cmd, Motion};
use super::{App, Flow, Item, Key, Msg, Pane, Target, md};
use crate::client::{Activity, Client, ClientRequest, ClientTask, FleetNode, NotFound, PrCard};
use crate::mcp::urlencode;

/// A request still defining this long after it was sent was left without tasks: the
/// orchestrator defines or closes one within minutes.
pub const LEFT_DEFINING: i64 = 10 * 60 * 1000;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TaskKey {
    Task(String),
    Request(String),
    /// The header of the finished section, which opens and closes it.
    Finished,
    /// The header of the archived section, which opens and closes it.
    Archived,
}

/// A task placed in its request's tree: depth 0 runs in parallel with its siblings, a deeper one
/// goes after its parent. `lines` says, for each level above the parent, whether that ancestor's
/// rail runs on through this row.
#[derive(Clone, PartialEq, Debug)]
pub struct TreeRow {
    pub id: String,
    pub depth: usize,
    pub last: bool,
    pub lines: Vec<bool>,
    pub has_children: bool,
}

#[derive(Clone)]
pub enum TaskListRow {
    /// The header of what is still open, with what the Tasks tab counts.
    Active(usize),
    /// A project heading shared by single tasks, request trees and taskless requests.
    Project(String),
    /// A request with one task, as one row: that task.
    Single(String),
    /// The header of a request with several tasks, above their tree: how many of them finished,
    /// of how many.
    Request(String, usize, usize),
    Task(TreeRow),
    /// A request with no task: still being defined, left without tasks, or finished without one.
    Taskless(String),
    /// The finished section's header: how many requests, and when the last one finished.
    Finished(usize, i64),
    /// The archived section's header, with how much it holds.
    Archived(usize),
}

impl TaskListRow {
    /// What selecting the row selects; headers of what they hold select nothing.
    pub fn key(&self) -> Option<TaskKey> {
        match self {
            TaskListRow::Single(id) => Some(TaskKey::Task(id.clone())),
            TaskListRow::Task(row) => Some(TaskKey::Task(row.id.clone())),
            TaskListRow::Taskless(id) => Some(TaskKey::Request(id.clone())),
            TaskListRow::Finished(..) => Some(TaskKey::Finished),
            TaskListRow::Archived(_) => Some(TaskKey::Archived),
            TaskListRow::Active(_) | TaskListRow::Project(_) | TaskListRow::Request(..) => None,
        }
    }
}

/// One request's rows in the list, and the moment it is sorted by.
struct Group {
    project: String,
    at: i64,
    rows: Vec<TaskListRow>,
}

/// Group a section by app, keeping the newest app first and each app's requests in recency order.
fn by_project(mut groups: Vec<Group>) -> Vec<TaskListRow> {
    groups.sort_by_key(|g| std::cmp::Reverse(g.at));
    let mut names: Vec<String> = vec![];
    for group in &groups {
        if !names.contains(&group.project) { names.push(group.project.clone()); }
    }
    names.into_iter().flat_map(|name| {
        let rows = groups.iter().filter(|g| g.project == name).flat_map(|g| g.rows.iter().cloned());
        std::iter::once(TaskListRow::Project(name.clone())).chain(rows).collect::<Vec<_>>()
    }).collect()
}

fn split_projects<'a>(app: &App, tasks: Vec<&'a ClientTask>) -> Vec<(String, Vec<&'a ClientTask>)> {
    let mut groups: Vec<(String, Vec<&ClientTask>)> = vec![];
    for task in tasks {
        let project = app.task_project(task).unwrap_or_else(|| tr!("Other", "Otros").into());
        if let Some((_, found)) = groups.iter_mut().find(|(name, _)| *name == project) {
            found.push(task);
        } else {
            groups.push((project, vec![task]));
        }
    }
    groups
}

/// Each task under the dependency it goes after (the newest one when it has several); tasks with
/// no dependency among `tasks` are roots, in parallel. A cycle still shows every task.
pub fn tree(tasks: &[&ClientTask]) -> Vec<TreeRow> {
    let mut ordered: Vec<&ClientTask> = tasks.to_vec();
    ordered.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
    let by_id: HashMap<&str, &ClientTask> = ordered.iter().map(|t| (t.id.as_str(), *t)).collect();
    let parent: HashMap<&str, &str> = ordered
        .iter()
        .filter_map(|t| {
            let newest = t.depends_on.iter().filter(|d| **d != t.id).filter_map(|d| by_id.get(d.as_str())).max_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)))?;
            Some((t.id.as_str(), newest.id.as_str()))
        })
        .collect();
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for t in &ordered {
        if let Some(p) = parent.get(t.id.as_str()) {
            children.entry(p).or_default().push(t.id.as_str());
        }
    }
    let mut rows = vec![];
    let mut placed: HashSet<&str> = HashSet::new();
    fn place<'a>(id: &'a str, depth: usize, last: bool, lines: Vec<bool>, children: &HashMap<&str, Vec<&'a str>>, placed: &mut HashSet<&'a str>, rows: &mut Vec<TreeRow>) {
        if !placed.insert(id) {
            return;
        }
        let kids: Vec<&str> = children.get(id).map(|k| k.iter().copied().filter(|k| !placed.contains(k)).collect()).unwrap_or_default();
        rows.push(TreeRow { id: id.to_string(), depth, last, lines: lines.clone(), has_children: !kids.is_empty() });
        let below = if depth == 0 { lines } else { [lines, vec![!last]].concat() };
        let n = kids.len();
        for (i, kid) in kids.into_iter().enumerate() {
            place(kid, depth + 1, i + 1 == n, below.clone(), children, placed, rows);
        }
    }
    let roots: Vec<&str> = ordered.iter().map(|t| t.id.as_str()).filter(|id| !parent.contains_key(id)).collect();
    let n = roots.len();
    for (i, root) in roots.into_iter().enumerate() {
        place(root, 0, i + 1 == n, vec![], &children, &mut placed, &mut rows);
    }
    for t in &ordered {
        place(t.id.as_str(), 0, true, vec![], &children, &mut placed, &mut rows);
    }
    rows
}

/// `HH:MM` in the Mac's time zone.
pub fn clock(ms: i64) -> String {
    let secs = (ms / 1000) as libc::time_t;
    // SAFETY: `localtime_r` only writes into the `tm` it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return String::new();
    }
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

/// "PR #150 · waiting for checks, 2 of 3", for a task's line.
pub fn pr_progress(p: &PrCard, merges_alone: bool) -> String {
    let n = p.pr.number;
    let checks = &p.pr.checks;
    if p.pr.conflicts() {
        return trf!("PR #{n} · conflicts", "PR #{n} · conflictos");
    }
    if checks.iter().any(|c| c.state == CheckState::Failed) {
        return trf!("PR #{n} · checks failed", "PR #{n} · checks fallidos");
    }
    if let Some(below) = p.below_number {
        return trf!("PR #{n} · merge #{below} first", "PR #{n} · fusiona antes el #{below}");
    }
    if checks.iter().any(|c| c.state == CheckState::Running) {
        let (ok, all) = (checks.iter().filter(|c| c.state == CheckState::Passed).count(), checks.len());
        return trf!("PR #{n} · waiting for checks, {ok} of {all}", "PR #{n} · espera checks, {ok} de {all}");
    }
    if merges_alone {
        return trf!("PR #{n} · Alondra merges it when the checks pass", "PR #{n} · Alondra lo fusiona cuando pasen los checks");
    }
    trf!("PR #{n} · ready to merge", "PR #{n} · listo para fusionar")
}

/// What moved a task, in a few words.
pub fn transition_summary(t: &Transition) -> String {
    use alondra_core::tasks::TaskEvent as E;
    match &t.event {
        E::DependencyReady { .. } => tr!("a task it builds on has its PR open", "la tarea de la que depende ya tiene su PR abierto").into(),
        E::AwaitsMerge { .. } => tr!("the node has nothing left but the merge of a PR it builds on", "al nodo solo le queda que se fusione un PR del que depende").into(),
        E::BlockerRaised { blocker, .. } => match blocker.kind {
            BlockerKind::Question => tr!("the node asked you something", "el nodo te preguntó algo").into(),
            BlockerKind::Permission => tr!("the node needs a permission", "el nodo necesita un permiso").into(),
            BlockerKind::Action => tr!("the node asked you to do something", "el nodo te pidió hacer algo").into(),
        },
        E::PrOpened { .. } => tr!("the node opened a PR", "el nodo abrió un PR").into(),
        E::DocumentSent { .. } => tr!("the node sent a document for review", "el nodo envió un documento para revisar").into(),
        E::AutoMerged { .. } => tr!("Alondra merged its PR", "Alondra fusionó su PR").into(),
        E::FinalReport { .. } => tr!("the node reported it finished", "el nodo dijo que terminó").into(),
        E::Answered { .. } => tr!("you answered", "respondiste").into(),
        E::ChangesRequested { .. } => tr!("you asked for changes", "pediste cambios").into(),
        E::NodeUpdated { .. } => tr!("the node pushed changes and let you know again", "el nodo subió cambios y te avisó otra vez").into(),
        E::Merged { .. } => tr!("merged", "fusionada").into(),
        E::ReviewClosed { .. } => tr!("the review closed without a merge", "la revisión se cerró sin fusionar").into(),
        E::Reopen => tr!("reopened", "reabierta").into(),
        E::Cancel { .. } => tr!("cancelled", "descartada").into(),
    }
}

fn before_its_pr_opens(s: TaskState) -> bool {
    !s.finished() && s != TaskState::InReview && s != TaskState::Closing
}

impl App {
    pub fn task(&self, id: &str) -> Option<&ClientTask> {
        self.data.tasks.iter().find(|t| t.id == id)
    }

    pub fn request(&self, id: &str) -> Option<&ClientRequest> {
        self.data.requests.iter().find(|r| r.id == id)
    }

    pub fn request_tasks(&self, r: &ClientRequest) -> Vec<&ClientTask> {
        let listed: Vec<&ClientTask> = r.tasks.iter().filter_map(|id| self.task(id)).collect();
        if listed.is_empty() { self.data.tasks.iter().filter(|t| t.request == r.id).collect() } else { listed }
    }

    /// A probe session's task, which the Tasks tab leaves out: a node's own task on a request
    /// made up for it, never recorded, whose node was a test node or that ended cancelled
    /// without delivering anything. Nothing of a request the user made is hidden.
    pub fn hidden(&self, t: &ClientTask) -> bool {
        if t.recorded || !self.request(&t.request).is_some_and(|r| r.synthetic) {
            return false;
        }
        let delivered = !t.prs.is_empty() || !t.documents.is_empty() || t.final_report.is_some();
        t.title.starts_with(alondra_core::PROBE_PREFIX) || (t.state == TaskState::Cancelled && !delivered)
    }

    /// A request left defining: the orchestrator neither defined it nor closed it.
    pub fn left_defining(&self, r: &ClientRequest) -> bool {
        r.state == RequestState::Defining && now_millis() - r.created_at >= LEFT_DEFINING
    }

    /// What is open, the request that changed last first; then the finished requests folded
    /// under one header, the last to finish first; then the archived section. Probe sessions are
    /// left out, and a request with one task is one row.
    pub fn task_rows(&self) -> Vec<TaskListRow> {
        let (mut active, mut finished, mut archived): (Vec<Group>, Vec<Group>, Vec<Group>) = (vec![], vec![], vec![]);
        let mut requests: Vec<&ClientRequest> = self.data.requests.iter().collect();
        requests.sort_by(|a, b| (b.created_at, &b.id).cmp(&(a.created_at, &a.id)));
        for r in requests {
            let all = self.request_tasks(r);
            if all.is_empty() {
                if r.synthetic {
                    continue;
                }
                let rows = vec![TaskListRow::Taskless(r.id.clone())];
                let project = self.request_project(r).unwrap_or_else(|| tr!("Other", "Otros").into());
                match (r.archived_at, r.state) {
                    (Some(at), _) => archived.push(Group { project, at, rows }),
                    (None, RequestState::Done) => finished.push(Group { project, at: r.closed.as_ref().map_or(r.created_at, |c| c.at), rows }),
                    (None, _) => active.push(Group { project, at: r.created_at, rows }),
                }
                continue;
            }
            let (old, current): (Vec<&ClientTask>, Vec<&ClientTask>) = all.into_iter().filter(|t| !self.hidden(t)).partition(|t| t.archived_at.is_some());
            for (project, tasks) in split_projects(self, old) {
                let at = tasks.iter().filter_map(|t| t.archived_at).max().unwrap_or(r.created_at);
                archived.push(Group { project, at, rows: group_rows(r, &tasks) });
            }
            for (project, tasks) in split_projects(self, current) {
                if tasks.iter().all(|t| t.state.finished()) {
                    let at = tasks.iter().map(|t| t.state_since).max().unwrap_or(r.created_at);
                    finished.push(Group { project, at, rows: group_rows(r, &tasks) });
                } else {
                    let at = tasks.iter().map(|t| t.last_change.unwrap_or(t.state_since).max(t.state_since)).max().unwrap_or(r.created_at);
                    active.push(Group { project, at, rows: group_rows(r, &tasks) });
                }
            }
        }
        let finished_n = finished.len();
        let last = finished.iter().map(|g| g.at).max();
        let mut rows = vec![TaskListRow::Active(active.len())];
        rows.extend(by_project(active));
        if let Some(last) = last {
            rows.push(TaskListRow::Finished(finished_n, last));
            if self.finished_open {
                rows.extend(by_project(finished));
            }
        }
        let n = self.archived_count();
        if n > 0 {
            rows.push(TaskListRow::Archived(n));
            if self.archived_open {
                rows.extend(by_project(archived));
            }
        }
        rows
    }

    pub fn task_keys(&self) -> Vec<TaskKey> {
        self.task_rows().iter().filter_map(TaskListRow::key).collect()
    }

    /// Archived tasks, and requests archived without tasks.
    pub fn archived_count(&self) -> usize {
        let tasks = self.data.tasks.iter().filter(|t| t.archived_at.is_some() && !self.hidden(t)).count();
        tasks + self.data.requests.iter().filter(|r| r.archived_at.is_some() && !r.synthetic && self.request_tasks(r).is_empty()).count()
    }

    /// What Shift+A archives: finished tasks, and requests that finished without tasks.
    pub fn archivable_count(&self) -> usize {
        let tasks = self.data.tasks.iter().filter(|t| t.state.finished() && t.archived_at.is_none()).count();
        let taskless = self.data.requests.iter().filter(|r| r.state == RequestState::Done && r.archived_at.is_none() && self.request_tasks(r).is_empty());
        tasks + taskless.count()
    }

    pub fn selected_task(&self) -> Option<&ClientTask> {
        match &self.task_sel {
            Some(TaskKey::Task(id)) => self.task(id),
            _ => None,
        }
    }

    /// The selected request while the orchestrator has not defined it: the user can withdraw it.
    pub fn selected_defining_request(&self) -> Option<&ClientRequest> {
        match &self.task_sel {
            Some(TaskKey::Request(id)) => self.request(id).filter(|r| r.state == RequestState::Defining),
            _ => None,
        }
    }

    pub fn tasks_on(&self, session: &str) -> Vec<&ClientTask> {
        let mut tasks: Vec<&ClientTask> = self.data.tasks.iter().filter(|t| t.archived_at.is_none() && t.is_assigned_to(session) && !self.hidden(t)).collect();
        tasks.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        tasks
    }

    /// Open, unarchived tasks, probe sessions left out: what the Tasks tab counts.
    pub fn open_task_count(&self) -> usize {
        self.data.tasks.iter().filter(|t| !t.state.finished() && t.archived_at.is_none() && !self.hidden(t)).count()
    }

    /// A short list label. The user's message is content, not a title; it belongs in the detail.
    pub fn request_title(&self, r: &ClientRequest) -> String {
        let tasks = self.request_tasks(r);
        if let Some(title) = r.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            return title.to_string();
        }
        match tasks.as_slice() {
            [one] => one.title.clone(),
            [_, _, ..] => self.request_project(r).unwrap_or_else(|| tr!("Work", "Trabajo").into()),
            [] if r.closed.as_ref().is_some_and(|c| c.withdrawn) => tr!("Withdrawn request", "Petición retirada").into(),
            [] if r.state == RequestState::Done => tr!("Orchestrator reply", "Respuesta del orquestador").into(),
            [] if r.state == RequestState::Defining => tr!("Preparing request", "Preparando petición").into(),
            [] => tr!("Request without tasks", "Petición sin tareas").into(),
        }
    }

    /// The title of a request shown as its one task's row: the orchestrator's, else the task's.
    pub fn single_title(&self, t: &ClientTask) -> String {
        let given = self.request(&t.request).and_then(|r| r.title.as_deref()).map(str::trim).filter(|x| !x.is_empty());
        given.map_or_else(|| t.title.clone(), str::to_string)
    }

    pub fn task_project(&self, t: &ClientTask) -> Option<String> {
        let named = |p: &String| (!p.is_empty() && p != alondra_core::OTHERS_PROJECT).then(|| p.clone());
        t.options
            .project
            .as_ref()
            .and_then(named)
            .or_else(|| t.assignee.as_ref().and_then(|a| self.node(&a.session_id)).and_then(|n| n.project.as_ref()).and_then(named))
            .or_else(|| self.request(&t.request).and_then(|r| r.options.as_ref()).and_then(|o| o.project.as_ref()).and_then(named))
    }

    pub fn request_project(&self, r: &ClientRequest) -> Option<String> {
        r.options.as_ref().and_then(|o| o.project.clone()).filter(|p| !p.is_empty() && p != alondra_core::OTHERS_PROJECT).or_else(|| self.request_tasks(r).iter().find_map(|t| self.task_project(t)))
    }

    /// What the node does now, per thread, as its own route last said; until it answers,
    /// or from an alondrad without the activity ledger, the main thread's tool in flight.
    pub fn node_activity(&self, n: &FleetNode) -> Vec<Activity> {
        let asked = self.threads.as_ref().filter(|(s, _)| *s == n.session_id).map(|(_, t)| t.clone());
        let mut a = asked.unwrap_or_else(|| {
            n.tool.as_ref().map(|t| vec![Activity { tool: Some(t.name.clone()), summary: t.summary.clone(), since: t.since, ..Default::default() }]).unwrap_or_default()
        });
        a.sort_by_key(|x| std::cmp::Reverse(x.since));
        a
    }

    pub fn task_prs(&self, t: &ClientTask) -> Vec<&PrCard> {
        let mut prs: Vec<&PrCard> = self.data.prs.iter().filter(|p| t.prs.contains(&p.pr.id) || p.task.as_deref() == Some(t.id.as_str())).collect();
        prs.sort_by_key(|p| p.train_place());
        prs
    }

    pub fn dependencies(&self, t: &ClientTask) -> Vec<&ClientTask> {
        t.depends_on.iter().filter_map(|id| self.task(id)).collect()
    }

    pub fn dependents(&self, t: &ClientTask) -> Vec<&ClientTask> {
        let mut out: Vec<&ClientTask> = self.data.tasks.iter().filter(|x| x.depends_on.contains(&t.id)).collect();
        out.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        out
    }

    /// The task, everything it builds on and everything built on it, in the order their PRs merge.
    pub fn merge_order<'a>(&'a self, t: &'a ClientTask) -> Vec<&'a ClientTask> {
        let mut seen: HashSet<&str> = HashSet::from([t.id.as_str()]);
        fn down<'a>(app: &'a App, t: &ClientTask, seen: &mut HashSet<&'a str>, out: &mut Vec<&'a ClientTask>) {
            for d in app.dependencies(t) {
                if seen.insert(d.id.as_str()) {
                    down(app, d, seen, out);
                    out.push(d);
                }
            }
        }
        let mut below = vec![];
        down(self, t, &mut seen, &mut below);
        let mut above = vec![];
        let mut frontier = vec![t];
        while !frontier.is_empty() {
            let next: Vec<&ClientTask> = frontier.iter().flat_map(|f| self.dependents(f)).filter(|d| seen.insert(d.id.as_str())).collect();
            above.extend(next.iter().copied());
            frontier = next;
        }
        below.into_iter().chain(std::iter::once(t)).chain(above).collect()
    }

    /// The task a card belongs to: the one it names, else the one that carries it.
    pub fn task_of(&self, key: Key) -> Option<&ClientTask> {
        match key {
            Key::Q(id) => {
                let named = self.data.open_questions.iter().find(|q| q.id == id).and_then(|q| q.task.as_deref());
                named.and_then(|t| self.task(t)).or_else(|| self.data.tasks.iter().find(|t| t.blocker.is_some_and(|b| b.question == id)))
            }
            Key::R(id) => {
                let named = self.data.news.iter().find(|r| r.id == id).and_then(|r| r.task.as_deref());
                named
                    .and_then(|t| self.task(t))
                    .or_else(|| self.data.tasks.iter().find(|t| t.documents.contains(&id) || t.notices.contains(&id) || t.final_report == Some(id)))
            }
            Key::P(id) => {
                let named = self.data.prs.iter().find(|p| p.pr.id == id).and_then(|p| p.task.as_deref());
                named.and_then(|t| self.task(t)).or_else(|| self.data.tasks.iter().find(|t| t.prs.contains(&id)))
            }
        }
    }

    /// The task's cards that wait in the inbox.
    pub fn task_items(&self, t: &ClientTask) -> Vec<Key> {
        self.inbox_keys().into_iter().filter(|k| self.task_of(*k).is_some_and(|x| x.id == t.id)).collect()
    }

    /// The dependency's PR a started task waits to see merged.
    fn awaited_pr(&self, t: &ClientTask) -> Option<&PrCard> {
        t.awaiting.and_then(|id| self.data.prs.iter().find(|p| p.pr.id == id))
    }

    /// One line on what is happening to the task now, or nothing to add to its state.
    pub fn task_now(&self, t: &ClientTask) -> Option<String> {
        match t.state {
            TaskState::NeedsYou => {
                let b = t.blocker?;
                let title = md::one_line(&self.data.open_questions.iter().find(|q| q.id == b.question)?.title);
                Some(match b.kind {
                    BlockerKind::Question => trf!("question: {title}", "pregunta: {title}"),
                    BlockerKind::Permission => trf!("permission: {title}", "permiso: {title}"),
                    BlockerKind::Action => trf!("your turn: {title}", "te toca: {title}"),
                })
            }
            TaskState::InReview => {
                if let Some(p) = self.task_prs(t).first() {
                    return Some(pr_progress(p, t.auto_merge_pending));
                }
                let doc = t.documents.iter().rev().find_map(|id| self.report_by_id(*id))?;
                let title = md::one_line(&doc.title);
                Some(trf!("document: {title}", "documento: {title}"))
            }
            TaskState::WaitingDependency => {
                if let Some(n) = self.awaited_pr(t).map(|p| p.pr.number) {
                    return Some(trf!("waits for PR #{n} to be merged", "espera a que se fusione el PR #{n}"));
                }
                if let Some((n, below)) = self.task_prs(t).first().and_then(|p| p.below_number.map(|b| (p.pr.number, b))) {
                    return Some(trf!("PR #{n} on top of #{below}", "PR #{n} va encima del #{below}"));
                }
                None
            }
            TaskState::Closing => Some(tr!("merged; the node does what the merge unblocks", "fusionada; el nodo hace lo que desbloquea la fusión").into()),
            TaskState::Running | TaskState::Done | TaskState::Cancelled => None,
        }
    }

    /// Why a task waits, in a sentence, for its detail.
    pub fn waiting_reason(&self, t: &ClientTask) -> Option<String> {
        if t.state != TaskState::WaitingDependency {
            return None;
        }
        if let Some(p) = self.awaited_pr(t) {
            let n = p.pr.number;
            let title = p.task.as_deref().and_then(|id| self.task(id)).map_or_else(|| p.pr.title.clone(), |d| d.title.clone());
            return Some(trf!(
                "It started, and what is left of it waits for PR #{n} of “{title}” to be merged: it goes on once that PR is merged.",
                "Empezó, y lo que le queda espera a que se fusione el PR #{n} de «{title}»: sigue en cuanto se fusione."
            ));
        }
        let first = self.dependencies(t).into_iter().find(|d| before_its_pr_opens(d.state))?;
        let (title, state) = (&first.title, crate::i18n::task_state(first.state));
        Some(trf!(
            "It starts once “{title}” ({state}) has its PR open, builds on top of it, and its own PR merges after that one.",
            "Empieza cuando «{title}» ({state}) tenga su PR abierto, se construye encima y su PR se fusiona después."
        ))
    }

    pub fn report_by_id(&self, id: u64) -> Option<&alondra_core::Report> {
        self.data
            .news
            .iter()
            .map(|n| &n.report)
            .find(|r| r.id == id)
            .or_else(|| self.data.reports.iter().find(|r| r.id == id))
            .or_else(|| self.task_reports.values().flatten().find(|r| r.id == id))
    }

    pub(super) fn reconcile_tasks(&mut self) {
        let keys = self.task_keys();
        match self.task_sel.as_ref().and_then(|s| keys.iter().position(|k| k == s)) {
            Some(i) => self.task_idx = i,
            None => {
                self.task_idx = self.task_idx.min(keys.len().saturating_sub(1));
                let next = keys.get(self.task_idx).cloned();
                if next != self.task_sel {
                    self.task_detail_scroll = 0;
                }
                self.task_sel = next;
            }
        }
        if self.screen == super::Screen::Tasks {
            self.load_history();
        }
    }

    /// The selected task's history, which only its own endpoint carries; asked once per state.
    fn load_history(&mut self) {
        let Some((id, since)) = self.selected_task().map(|t| (t.id.clone(), t.last_change.unwrap_or(t.state_since))) else { return };
        if self.history_missing || self.histories.get(&id).is_some_and(|(at, _)| *at == since) || !self.history_asked.insert((id.clone(), since)) {
            return;
        }
        self.spawn(move |tx| {
            let result = Client::for_tui(Duration::from_secs(10)).get::<Value>(&format!("/v1/tasks/{}", urlencode(&id)));
            let history = match result {
                Ok(v) => {
                    let reports = serde_json::from_value::<Vec<alondra_core::Report>>(v["reports"].clone()).unwrap_or_default();
                    let task = if v.get("task").is_some() { v["task"].clone() } else { v };
                    Ok((serde_json::from_value::<Vec<Transition>>(task["history"].clone()).unwrap_or_default(), reports, task["brief"].as_str().map(str::to_string)))
                }
                Err(e) => Err(e.downcast_ref::<NotFound>().is_some()),
            };
            let _ = tx.send(Msg::TaskHistory { id, since, history });
        });
    }

    pub(super) fn history_loaded(&mut self, id: String, since: i64, history: Result<(Vec<Transition>, Vec<alondra_core::Report>, Option<String>), bool>) {
        match history {
            Ok((h, reports, brief)) => {
                if !reports.is_empty() {
                    self.task_reports.insert(id.clone(), reports);
                }
                if let Some(brief) = brief {
                    self.task_briefs.insert(id.clone(), brief);
                }
                self.histories.insert(id, (since, h));
            }
            Err(true) => self.history_missing = true,
            Err(false) => {
                self.history_asked.remove(&(id, since));
            }
        }
    }

    pub(super) fn move_tasks(&mut self, to: isize) {
        let keys = self.task_keys();
        if keys.is_empty() {
            return;
        }
        let idx = to.clamp(0, keys.len() as isize - 1) as usize;
        if self.task_sel.as_ref() != Some(&keys[idx]) {
            self.task_detail_scroll = 0;
        }
        self.task_idx = idx;
        self.task_sel = Some(keys[idx].clone());
        self.load_history();
    }

    pub(super) fn tasks_cmd(&mut self, cmd: Cmd) {
        let i = self.task_idx as isize;
        let detail = self.task_zoom || (self.focus == Pane::Detail && self.task_view.shown);
        match cmd {
            Cmd::Move(m) if detail => self.task_detail_scroll = keys::scroll(self.task_detail_scroll, m, self.task_view.h, self.task_view.total),
            Cmd::Move(Motion::Down) => self.move_tasks(i + 1),
            Cmd::Move(Motion::Up) => self.move_tasks(i - 1),
            Cmd::Move(Motion::Top) => self.move_tasks(0),
            Cmd::Move(Motion::Bottom) => self.move_tasks(isize::MAX),
            Cmd::Move(m) => self.task_detail_scroll = keys::scroll(self.task_detail_scroll, m, self.task_view.h, self.task_view.total),
            Cmd::Right | Cmd::Expand if self.task_sel == Some(TaskKey::Finished) => self.finished_open = !self.finished_open,
            Cmd::Right | Cmd::Expand if self.task_sel == Some(TaskKey::Archived) => self.archived_open = !self.archived_open,
            Cmd::Right if self.task_view.shown && !self.task_zoom => self.focus = Pane::Detail,
            Cmd::Right => self.task_zoom = self.task_sel.is_some(),
            Cmd::Expand => self.task_zoom = !self.task_zoom && self.task_sel.is_some(),
            Cmd::Left | Cmd::Back if self.task_zoom => self.task_zoom = false,
            Cmd::Left | Cmd::Back if self.focus == Pane::Detail => self.focus = Pane::List,
            Cmd::Left | Cmd::Back | Cmd::Pending | Cmd::Other(_) => {}
        }
    }

    pub(super) fn tasks_key(&mut self, k: KeyEvent) -> Flow {
        match k.code {
            KeyCode::Char('J') => self.task_detail_scroll = keys::by(self.task_detail_scroll, 5, self.task_view.h, self.task_view.total),
            KeyCode::Char('K') => self.task_detail_scroll = keys::by(self.task_detail_scroll, -5, self.task_view.h, self.task_view.total),
            KeyCode::Char('x') if self.selected_defining_request().is_some() => self.withdraw_request(),
            KeyCode::Char('x') => self.cancel_task(),
            KeyCode::Char('R') => self.reopen_task(),
            KeyCode::Char('A') => self.archive_finished(),
            KeyCode::Enter if self.task_sel == Some(TaskKey::Finished) => self.finished_open = !self.finished_open,
            KeyCode::Enter if self.task_sel == Some(TaskKey::Archived) => self.archived_open = !self.archived_open,
            KeyCode::Char('r') | KeyCode::Char('m') => {
                if let Some(n) = self.selected_task().and_then(|t| t.assignee.as_ref()).and_then(|a| self.node(&a.session_id)) {
                    let target = Target::Message { session: n.session_id.clone(), name: n.display_name().to_string() };
                    self.compose(target, "");
                }
            }
            KeyCode::Char('a') | KeyCode::Enter => {
                if let Some(session) = self.selected_task().and_then(|t| t.assignee.as_ref()).map(|a| a.session_id.clone()) {
                    return self.attach_to(&session);
                }
            }
            _ => {}
        }
        Flow::Go
    }

    fn cancel_task(&mut self) {
        let Some((id, title, finished)) = self.selected_task().map(|t| (t.id.clone(), md::truncate(&t.title, 60), t.state.finished())) else { return };
        if self.task_actions_missing {
            self.fail(tr!("This alondrad cannot cancel tasks yet", "Este alondrad todavía no sabe descartar tareas"));
            return;
        }
        if finished {
            self.fail(tr!("It is already finished; Shift+R reopens it", "Ya está terminada; Shift+R la reabre"));
            return;
        }
        let now = now_millis();
        if !self.pending_cancel.as_ref().is_some_and(|(p, until)| *p == id && now < *until) {
            self.pending_cancel = Some((id, now + 4000));
            self.fail(trf!("Press x again to cancel “{title}”", "Pulsa x otra vez para descartar «{title}»"));
            return;
        }
        self.pending_cancel = None;
        self.task_action(format!("/v1/tasks/{}/cancel", urlencode(&id)), trf!("Cancelled: {title}", "Descartada: {title}"), Msg::TaskAction);
    }

    /// Closes a request the orchestrator never defined, as withdrawn: then it can be archived.
    fn withdraw_request(&mut self) {
        let Some((id, title)) = self.selected_defining_request().map(|r| (r.id.clone(), md::truncate(&self.request_title(r), 60))) else { return };
        let now = now_millis();
        if !self.pending_cancel.as_ref().is_some_and(|(p, until)| *p == id && now < *until) {
            self.pending_cancel = Some((id, now + 4000));
            self.fail(trf!("Press x again to withdraw “{title}”", "Pulsa x otra vez para retirar «{title}»"));
            return;
        }
        self.pending_cancel = None;
        self.post(None, vec![(format!("/v1/requests/{}/close", urlencode(&id)), json!({"withdrawn": true}))], trf!("Withdrawn: {title}", "Retirada: {title}"));
    }

    fn reopen_task(&mut self) {
        let Some((id, title, finished)) = self.selected_task().map(|t| (t.id.clone(), md::truncate(&t.title, 60), t.state.finished())) else { return };
        if self.task_actions_missing {
            self.fail(tr!("This alondrad cannot reopen tasks yet", "Este alondrad todavía no sabe reabrir tareas"));
            return;
        }
        if !finished {
            self.fail(tr!("Only a finished or cancelled task reopens", "Solo se reabre una tarea terminada o descartada"));
            return;
        }
        self.task_action(format!("/v1/tasks/{}/reopen", urlencode(&id)), trf!("Reopened: {title}", "Reabierta: {title}"), Msg::TaskAction);
    }

    fn archive_finished(&mut self) {
        if self.archive_missing {
            self.fail(tr!("This alondrad cannot archive tasks yet", "Este alondrad todavía no sabe archivar tareas"));
            return;
        }
        let n = self.archivable_count();
        if n == 0 {
            self.fail(tr!("Nothing finished to archive", "No hay nada terminado que archivar"));
            return;
        }
        self.task_action("/v1/tasks/archive".into(), trf!("Archived: {n}", "Archivadas: {n}"), Msg::TasksArchived);
    }

    fn task_action(&self, path: String, ok: String, done: fn(Result<String, (String, bool)>) -> Msg) {
        #[cfg(test)]
        self.posted.borrow_mut().push((vec![path.clone()], ok.clone()));
        self.spawn(move |tx| {
            let result = match Client::for_tui(Duration::from_secs(15)).post(&path, &json!({})) {
                Ok(_) => Ok(ok),
                Err(e) => Err((e.to_string(), e.downcast_ref::<NotFound>().is_some())),
            };
            let _ = tx.send(done(result));
        });
    }

    pub(super) fn archive_done(&mut self, result: Result<String, (String, bool)>) {
        match result {
            Ok(text) => self.flash(text),
            Err((_, true)) => {
                self.archive_missing = true;
                self.fail(tr!("This alondrad cannot archive tasks yet", "Este alondrad todavía no sabe archivar tareas"));
            }
            Err((e, false)) => self.fail(trf!("Not sent: {e}", "No se envió: {e}")),
        }
    }

    pub(super) fn task_action_done(&mut self, result: Result<String, (String, bool)>) {
        match result {
            Ok(text) => self.flash(text),
            Err((_, true)) => {
                self.task_actions_missing = true;
                self.fail(tr!("This alondrad cannot cancel or reopen tasks yet", "Este alondrad todavía no sabe descartar ni reabrir tareas"));
            }
            Err((e, false)) => self.fail(trf!("Not sent: {e}", "No se envió: {e}")),
        }
    }

    /// The item a key points at, for the lines a card's task adds: the task's title, unless it
    /// is only its own node's name repeated — the fallback a task gets until something names it
    /// for real (t-47118) — right above that same name in the card's meta line. Then it reads as
    /// the node's main task instead of showing the name twice.
    pub fn card_task_title(&self, item: &Item) -> Option<String> {
        let (key, session) = match item {
            Item::Q(q) => (Key::Q(q.id), q.session_id.as_str()),
            Item::R(r) => (Key::R(r.id), r.session_id.as_str()),
            Item::P(p) => (Key::P(p.pr.id), p.pr.session_id.as_str()),
        };
        let task = self.task_of(key)?;
        match self.node(session) {
            Some(n) => Some(task.fleet_title(n)),
            None => Some(task.title.clone()),
        }
    }
}

/// A request's rows: one row for a single task, else its header above the tree.
fn group_rows(r: &ClientRequest, tasks: &[&ClientTask]) -> Vec<TaskListRow> {
    match tasks {
        [one] => vec![TaskListRow::Single(one.id.clone())],
        _ => {
            let done = tasks.iter().filter(|t| t.state.finished()).count();
            let total = tasks.len();
            let tasks = tree(tasks).into_iter().map(TaskListRow::Task);
            if r.title.as_deref().is_some_and(|title| !title.trim().is_empty()) {
                std::iter::once(TaskListRow::Request(r.id.clone(), done, total)).chain(tasks).collect()
            } else {
                tasks.collect()
            }
        }
    }
}

/// "Bash bazel build //app", or whichever half is known.
pub fn activity_doing(a: &Activity) -> Option<String> {
    let text = [a.tool.as_deref(), a.summary.as_deref()].into_iter().flatten().map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ClientTask;

    fn task(id: &str, depends_on: &[&str], created_at: i64) -> ClientTask {
        serde_json::from_value(json!({
            "id": id, "request": "r-1", "title": format!("Task {id}"), "depends_on": depends_on,
            "state": "running", "state_since": created_at, "created_at": created_at,
        }))
        .unwrap()
    }

    fn shape(tasks: &[ClientTask]) -> Vec<(String, usize, bool, Vec<bool>, bool)> {
        let refs: Vec<&ClientTask> = tasks.iter().collect();
        tree(&refs).into_iter().map(|r| (r.id, r.depth, r.last, r.lines, r.has_children)).collect()
    }

    #[test]
    fn sequential_tasks_nest_under_the_one_they_follow() {
        let tasks = [task("a", &[], 1), task("b", &["a"], 2), task("c", &["b"], 3)];
        assert_eq!(
            shape(&tasks),
            vec![("a".into(), 0, true, vec![], true), ("b".into(), 1, true, vec![], true), ("c".into(), 2, true, vec![false], false)]
        );
    }

    #[test]
    fn parallel_tasks_share_a_level_and_a_second_root_runs_beside_the_first() {
        let tasks = [task("d", &[], 4), task("c", &["a"], 3), task("b", &["a"], 2), task("a", &[], 1)];
        assert_eq!(
            shape(&tasks),
            vec![
                ("a".into(), 0, false, vec![], true),
                ("b".into(), 1, false, vec![], false),
                ("c".into(), 1, true, vec![], false),
                ("d".into(), 0, true, vec![], false),
            ]
        );
    }

    #[test]
    fn a_task_with_two_dependencies_goes_under_the_newest_and_a_cycle_keeps_every_task() {
        let joined = [task("a", &[], 1), task("b", &[], 2), task("c", &["a", "b"], 3)];
        let rows = shape(&joined);
        assert_eq!(rows.iter().map(|r| (r.0.as_str(), r.1)).collect::<Vec<_>>(), vec![("a", 0), ("b", 0), ("c", 1)]);
        let cycle = [task("x", &["y"], 1), task("y", &["x"], 2)];
        let ids: HashSet<String> = shape(&cycle).into_iter().map(|r| r.0).collect();
        assert_eq!(ids, HashSet::from(["x".to_string(), "y".to_string()]));
    }
}
