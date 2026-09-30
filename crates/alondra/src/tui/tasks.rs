//! The Tasks screen: every request with its task tree, and what the daemon says each task is at.

use std::collections::{HashMap, HashSet};

use alondra_core::domain_v2::{
    AvailableAction, Command, InteractionPayload, MergeAttemptState, PrState, Request, RequestId,
    Task, TaskId, TaskState,
};
use crossterm::event::{KeyCode, KeyEvent};

use super::keys::{self, Cmd, Motion};
use super::{App, Flow, Msg, Pane, Target, md, unavailable};
use crate::ops::{self, MessageKind};

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TaskKey {
    Task(TaskId),
    Request(RequestId),
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
    pub id: TaskId,
    pub depth: usize,
    pub last: bool,
    pub lines: Vec<bool>,
    pub has_children: bool,
}

#[derive(Clone)]
pub enum TaskListRow {
    /// The header of what is still open, with how many requests it holds.
    Active(usize),
    /// A project heading over its requests, when the runtime names the projects.
    Project(String),
    /// A request with one task, as one row: that task.
    Single(TaskId),
    /// The header of a request with several tasks, above their tree: how many finished, of how many.
    Request(RequestId, usize, usize),
    Task(TreeRow),
    /// A request with no task yet, or closed without one.
    Taskless(RequestId),
    Finished(usize),
    Archived(usize),
}

impl TaskListRow {
    /// What selecting the row selects; headers of what they hold select nothing.
    pub fn key(&self) -> Option<TaskKey> {
        match self {
            TaskListRow::Single(id) => Some(TaskKey::Task(id.clone())),
            TaskListRow::Task(row) => Some(TaskKey::Task(row.id.clone())),
            TaskListRow::Taskless(id) => Some(TaskKey::Request(id.clone())),
            TaskListRow::Finished(_) => Some(TaskKey::Finished),
            TaskListRow::Archived(_) => Some(TaskKey::Archived),
            TaskListRow::Active(_) | TaskListRow::Project(_) | TaskListRow::Request(..) => None,
        }
    }
}

pub fn state_label(s: TaskState) -> &'static str {
    match s {
        TaskState::NotStarted => tr!("not started", "sin empezar"),
        TaskState::Running => tr!("working", "trabajando"),
        TaskState::NeedsYou => tr!("needs you", "te necesita"),
        TaskState::InReview => tr!("in review", "en revisión"),
        TaskState::WaitingDependency => tr!("waiting", "en espera"),
        TaskState::Closing => tr!("closing", "cerrando"),
        TaskState::Done => tr!("done", "terminada"),
        TaskState::Cancelled => tr!("cancelled", "descartada"),
    }
}

/// Each task under the dependency it goes after (the last one when it has several); tasks with
/// no dependency among `tasks` are roots, in parallel. A cycle still shows every task.
pub fn tree(tasks: &[&Task]) -> Vec<TreeRow> {
    let mut ordered: Vec<&Task> = tasks.to_vec();
    ordered.sort_by(|a, b| a.id.cmp(&b.id));
    let known: HashSet<&TaskId> = ordered.iter().map(|t| &t.id).collect();
    let parent: HashMap<&TaskId, &TaskId> = ordered
        .iter()
        .filter_map(|t| {
            t.dependencies
                .iter()
                .filter(|d| **d != t.id && known.contains(d))
                .max()
                .map(|p| (&t.id, p))
        })
        .collect();
    let mut children: HashMap<&TaskId, Vec<&TaskId>> = HashMap::new();
    for t in &ordered {
        if let Some(p) = parent.get(&t.id) {
            children.entry(p).or_default().push(&t.id);
        }
    }
    let mut rows = vec![];
    let mut placed = HashSet::new();
    fn place<'a>(
        id: &'a TaskId,
        depth: usize,
        last: bool,
        lines: Vec<bool>,
        children: &HashMap<&TaskId, Vec<&'a TaskId>>,
        placed: &mut HashSet<&'a TaskId>,
        rows: &mut Vec<TreeRow>,
    ) {
        if !placed.insert(id) {
            return;
        }
        let kids: Vec<&TaskId> = children
            .get(id)
            .map(|k| k.iter().copied().filter(|k| !placed.contains(k)).collect())
            .unwrap_or_default();
        rows.push(TreeRow {
            id: id.clone(),
            depth,
            last,
            lines: lines.clone(),
            has_children: !kids.is_empty(),
        });
        let mut below = lines;
        if depth > 0 {
            below.push(!last);
        }
        for (i, k) in kids.iter().enumerate() {
            place(
                k,
                depth + 1,
                i + 1 == kids.len(),
                below.clone(),
                children,
                placed,
                rows,
            );
        }
    }
    let roots: Vec<&TaskId> = ordered
        .iter()
        .map(|t| &t.id)
        .filter(|id| !parent.contains_key(id))
        .collect();
    for (i, r) in roots.iter().enumerate() {
        place(
            r,
            0,
            i + 1 == roots.len(),
            vec![],
            &children,
            &mut placed,
            &mut rows,
        );
    }
    for t in &ordered {
        place(&t.id, 0, true, vec![], &children, &mut placed, &mut rows);
    }
    rows
}

/// A section's requests under their project's heading, projects in the order they first
/// appear; without any project known, the requests as they are.
fn by_project(groups: Vec<(Option<String>, Vec<TaskListRow>)>) -> Vec<TaskListRow> {
    if groups.iter().all(|(p, _)| p.is_none()) {
        return groups.into_iter().flat_map(|(_, rows)| rows).collect();
    }
    let other = || tr!("Other", "Otros").to_string();
    let mut names: Vec<String> = vec![];
    for (p, _) in &groups {
        let name = p.clone().unwrap_or_else(other);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
        .into_iter()
        .flat_map(|name| {
            let rows = groups
                .iter()
                .filter(|(p, _)| p.clone().unwrap_or_else(other) == name)
                .flat_map(|(_, rows)| rows.iter().cloned())
                .collect::<Vec<_>>();
            std::iter::once(TaskListRow::Project(name)).chain(rows)
        })
        .collect()
}

/// A request's rows: one row for a single task, else its header above the tree.
fn group_rows(r: &Request, tasks: &[&Task]) -> Vec<TaskListRow> {
    match tasks {
        [one] => vec![TaskListRow::Single(one.id.clone())],
        _ => {
            let done = tasks.iter().filter(|t| t.state.terminal()).count();
            std::iter::once(TaskListRow::Request(r.id.clone(), done, tasks.len()))
                .chain(tree(tasks).into_iter().map(TaskListRow::Task))
                .collect()
        }
    }
}

impl App {
    pub fn task(&self, id: &TaskId) -> Option<&Task> {
        self.domain.as_ref()?.tasks.iter().find(|t| &t.id == id)
    }

    pub fn request(&self, id: &RequestId) -> Option<&Request> {
        self.domain.as_ref()?.requests.iter().find(|r| &r.id == id)
    }

    pub fn request_tasks(&self, r: &RequestId) -> Vec<&Task> {
        self.domain
            .as_ref()
            .map(|d| d.tasks.iter().filter(|t| &t.request_id == r).collect())
            .unwrap_or_default()
    }

    /// The project the runtime names for a task.
    pub fn task_project(&self, t: &Task) -> Option<String> {
        self.runtime.as_ref()?.task(&t.id.0)?.project.clone()
    }

    fn request_project(&self, tasks: &[&Task]) -> Option<String> {
        tasks.iter().find_map(|t| self.task_project(t))
    }

    /// The tasks that wait on this one.
    pub fn dependents(&self, t: &Task) -> Vec<&Task> {
        self.domain
            .as_ref()
            .map(|d| {
                d.tasks
                    .iter()
                    .filter(|o| o.dependencies.contains(&t.id) && o.id != t.id)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The stack this task's PR belongs to, bottom first: the tasks it goes on top of, itself,
    /// and the tasks stacked on it.
    pub fn merge_order<'a>(&'a self, t: &'a Task) -> Vec<&'a Task> {
        let mut below = vec![];
        let mut seen: HashSet<&TaskId> = HashSet::from([&t.id]);
        let mut at = t;
        while let Some(up) = at.stack.as_ref().and_then(|s| self.task(&s.upstream_task)) {
            if !seen.insert(&up.id) {
                break;
            }
            below.push(up);
            at = up;
        }
        below.reverse();
        below.push(t);
        let mut at = t;
        while let Some(next) = self.domain.as_ref().and_then(|d| {
            d.tasks
                .iter()
                .find(|o| o.stack.as_ref().is_some_and(|s| s.upstream_task == at.id))
        }) {
            if !seen.insert(&next.id) {
                break;
            }
            below.push(next);
            at = next;
        }
        below
    }

    /// What is open first; then the finished work and the archived work, each folded under one header.
    pub fn task_rows(&self) -> Vec<TaskListRow> {
        let Some(d) = &self.domain else {
            return vec![TaskListRow::Active(0)];
        };
        let (mut active, mut finished, mut archived) = (vec![], vec![], vec![]);
        let (mut open_n, mut finished_n, mut archived_n) = (0, 0, 0);
        for r in &d.requests {
            let all = self.request_tasks(&r.id);
            if all.is_empty() {
                if r.closed {
                    finished_n += 1;
                    finished.push((None, vec![TaskListRow::Taskless(r.id.clone())]));
                } else {
                    open_n += 1;
                    active.push((None, vec![TaskListRow::Taskless(r.id.clone())]));
                }
                continue;
            }
            let (old, current): (Vec<&Task>, Vec<&Task>) =
                all.into_iter().partition(|t| t.archived);
            if !old.is_empty() {
                archived_n += old.len();
                archived.push((self.request_project(&old), group_rows(r, &old)));
            }
            if current.is_empty() {
                continue;
            }
            let project = self.request_project(&current);
            if current.iter().all(|t| t.state.terminal()) {
                finished_n += 1;
                finished.push((project, group_rows(r, &current)));
            } else {
                open_n += 1;
                active.push((project, group_rows(r, &current)));
            }
        }
        let mut rows = vec![TaskListRow::Active(open_n)];
        rows.extend(by_project(active));
        if finished_n > 0 {
            rows.push(TaskListRow::Finished(finished_n));
            if self.finished_open {
                rows.extend(by_project(finished));
            }
        }
        if archived_n > 0 {
            rows.push(TaskListRow::Archived(archived_n));
            if self.archived_open {
                rows.extend(by_project(archived));
            }
        }
        rows
    }

    pub fn task_keys(&self) -> Vec<TaskKey> {
        self.task_rows()
            .iter()
            .filter_map(TaskListRow::key)
            .collect()
    }

    pub fn open_task_count(&self) -> usize {
        self.domain.as_ref().map_or(0, |d| {
            d.tasks
                .iter()
                .filter(|t| !t.state.terminal() && !t.archived)
                .count()
        })
    }

    pub(super) fn running_tasks(&self) -> bool {
        self.domain.as_ref().is_some_and(|d| {
            d.tasks
                .iter()
                .any(|t| t.state == TaskState::Running && !t.archived)
        })
    }

    pub fn selected_task(&self) -> Option<&Task> {
        match &self.task_sel {
            Some(TaskKey::Task(id)) => self.task(id),
            _ => None,
        }
    }

    pub fn selected_request(&self) -> Option<&Request> {
        match &self.task_sel {
            Some(TaskKey::Request(id)) => self.request(id),
            _ => None,
        }
    }

    pub fn request_title(&self, r: &Request) -> String {
        match self.request_tasks(&r.id).as_slice() {
            [one] => one.title.clone(),
            _ => md::truncate(&md::one_line(&r.body), 80),
        }
    }

    /// Where the task's result stands, one fact at a time, as the daemon recorded it: presented,
    /// accepted, merge authorized, merged, and whether it is done. Nothing here is inferred.
    pub fn task_progress(&self, t: &Task) -> Vec<String> {
        let Some(d) = &self.domain else { return vec![] };
        let mut out = vec![];
        let presented = t
            .final_presentation
            .as_ref()
            .and_then(|id| self.interaction(id));
        let accepted = presented.is_some_and(|i| matches!(&i.payload, InteractionPayload::Delivery { acceptance: Some(a), .. } if a.valid));
        if presented.is_some() {
            out.push(tr!("presented", "presentada").to_string());
        }
        if accepted {
            out.push(tr!("accepted", "aceptada").to_string());
        }
        let pr = t.pr_id.as_ref().and_then(|id| self.pull_request(id));
        let authorized = d
            .merge_authorizations
            .iter()
            .any(|a| a.task_id == t.id && a.valid && a.attempt != MergeAttemptState::Confirmed);
        if authorized {
            out.push(tr!("merge authorized", "fusión autorizada").to_string());
        }
        if pr.is_some_and(|p| p.state == PrState::Merged) {
            out.push(tr!("merged", "fusionado").to_string());
        }
        match t.state {
            TaskState::Done => out.push(tr!("done", "terminada").to_string()),
            TaskState::Cancelled => {}
            _ if accepted || authorized => {
                out.push(tr!("waiting for the agent", "pendiente del agente").to_string())
            }
            _ => {}
        }
        out
    }

    /// One line on what the task is at now, when its state alone does not say it.
    pub fn task_now(&self, t: &Task) -> Option<String> {
        let waiting = self
            .inbox_rows()
            .into_iter()
            .filter(|(i, _)| {
                i.pending && i.task_id.as_ref() == Some(&t.id) && i.children.is_empty()
            })
            .count();
        if waiting > 0 && !t.state.terminal() {
            return Some(trf!("{waiting} waiting for you", "{waiting} te esperan"));
        }
        let progress = self.task_progress(t);
        if !progress.is_empty() && !t.state.terminal() {
            return Some(progress.join(" · "));
        }
        if t.state == TaskState::WaitingDependency {
            let titles: Vec<String> = t
                .dependencies
                .iter()
                .filter_map(|id| self.task(id))
                .filter(|d| !d.state.terminal())
                .map(|d| md::one_line(&d.title))
                .collect();
            if !titles.is_empty() {
                let titles = titles.join(", ");
                return Some(trf!("waits for {titles}", "espera a {titles}"));
            }
        }
        None
    }

    pub(super) fn reconcile_tasks(&mut self) {
        let keys = self.task_keys();
        match self
            .task_sel
            .as_ref()
            .and_then(|s| keys.iter().position(|k| k == s))
        {
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
            self.load_task_actions();
            self.load_task_history();
        }
    }

    /// The task detail View carries its brief and history.
    fn load_task_history(&mut self) {
        self.load_selected_view();
    }

    /// The task detail View carries the available controls and their current state.
    fn load_task_actions(&mut self) {
        self.load_selected_view();
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
        self.load_task_actions();
        self.load_task_history();
    }

    pub(super) fn tasks_cmd(&mut self, cmd: Cmd) {
        let i = self.task_idx as isize;
        let detail = self.task_zoom || (self.focus == Pane::Detail && self.task_view.shown);
        match cmd {
            Cmd::Move(m) if detail => {
                self.task_detail_scroll = keys::scroll(
                    self.task_detail_scroll,
                    m,
                    self.task_view.h,
                    self.task_view.total,
                )
            }
            Cmd::Move(Motion::Down) => self.move_tasks(i + 1),
            Cmd::Move(Motion::Up) => self.move_tasks(i - 1),
            Cmd::Move(Motion::Top) => self.move_tasks(0),
            Cmd::Move(Motion::Bottom) => self.move_tasks(isize::MAX),
            Cmd::Move(m) => {
                self.task_detail_scroll = keys::scroll(
                    self.task_detail_scroll,
                    m,
                    self.task_view.h,
                    self.task_view.total,
                )
            }
            Cmd::Right | Cmd::Expand if self.task_sel == Some(TaskKey::Finished) => {
                self.finished_open = !self.finished_open
            }
            Cmd::Right | Cmd::Expand if self.task_sel == Some(TaskKey::Archived) => {
                self.archived_open = !self.archived_open
            }
            Cmd::Right if self.task_view.shown && !self.task_zoom => self.focus = Pane::Detail,
            Cmd::Right => self.task_zoom = self.task_sel.is_some(),
            Cmd::Expand => self.task_zoom = !self.task_zoom && self.task_sel.is_some(),
            Cmd::Left | Cmd::Back if self.task_zoom => self.task_zoom = false,
            Cmd::Left | Cmd::Back if self.focus == Pane::Detail => self.focus = Pane::List,
            Cmd::Left | Cmd::Back | Cmd::Pending | Cmd::Other(_) => {}
        }
    }

    #[cfg(test)]
    pub(super) fn tasks_key(&mut self, k: KeyEvent) -> Flow {
        match k.code {
            KeyCode::Char('J') => {
                self.task_detail_scroll = keys::by(
                    self.task_detail_scroll,
                    5,
                    self.task_view.h,
                    self.task_view.total,
                )
            }
            KeyCode::Char('K') => {
                self.task_detail_scroll = keys::by(
                    self.task_detail_scroll,
                    -5,
                    self.task_view.h,
                    self.task_view.total,
                )
            }
            KeyCode::Char('x') if self.selected_request().is_some() => self.withdraw_request(),
            KeyCode::Char('x') => self.task_command("cancel_task"),
            KeyCode::Char('R') => self.task_command("reopen_task"),
            KeyCode::Char('A') => self.archive_finished(),
            KeyCode::Char('r') | KeyCode::Char('m') => self.message_task_agent(),
            KeyCode::Enter if self.task_sel == Some(TaskKey::Finished) => {
                self.finished_open = !self.finished_open
            }
            KeyCode::Enter if self.task_sel == Some(TaskKey::Archived) => {
                self.archived_open = !self.archived_open
            }
            KeyCode::Char('a') | KeyCode::Enter => {
                if let Some(agent) = self.selected_task().and_then(|t| t.agent_id.clone()) {
                    return Flow::Attach(self.attach_id(&agent));
                }
            }
            _ => {}
        }
        Flow::Go
    }

    fn message_task_agent(&mut self) {
        let Some(t) = self.selected_task().cloned() else {
            return;
        };
        let Some(agent) = t.agent_id.clone() else {
            self.fail(tr!(
                "This task has no agent yet",
                "Esta tarea todavía no tiene agente"
            ));
            return;
        };
        let name = self.agent_name(Some(&agent));
        let message = ops::Message::to(Some(agent), MessageKind::Message, String::new());
        self.compose(
            Target::Message {
                key: None,
                message,
                name,
                title: md::one_line(&t.title),
            },
            "",
        );
    }

    /// The daemon's own control for the task, when it has already said whether it is on.
    fn task_control(&self, id: &TaskId, kind: &str) -> Option<AvailableAction> {
        self.task_actions
            .get(id)
            .and_then(|(_, actions)| actions.iter().find(|a| a.kind == kind).cloned())
    }

    #[cfg(test)]
    fn task_command(&mut self, kind: &str) {
        let Some((id, title, revision)) = self.selected_task().map(|t| {
            (
                t.id.clone(),
                md::truncate(&md::one_line(&t.title), 60),
                self.domain.as_ref().map_or(0, |d| d.revision),
            )
        }) else {
            return;
        };
        let expected = match self.task_control(&id, kind) {
            Some(a) if !a.enabled => {
                let why = unavailable(&a);
                self.fail(why);
                return;
            }
            Some(a) => a.expected_revision,
            None => revision,
        };
        let (command, ok, ask) = if kind == "cancel_task" {
            (
                Command::CancelTask {
                    task_id: id.clone(),
                },
                trf!("Cancelled: {title}", "Descartada: {title}"),
                Some(trf!(
                    "Press x again to cancel “{title}”",
                    "Pulsa x otra vez para descartar «{title}»"
                )),
            )
        } else {
            (
                Command::ReopenTask {
                    task_id: id.clone(),
                },
                trf!("Reopened: {title}", "Reabierta: {title}"),
                None,
            )
        };
        let slot = format!("{kind}:{id}");
        if let Some(ask) = ask
            && !self.sending.contains_key(&slot)
            && !self.confirmed(&slot, ask)
        {
            return;
        }
        self.send(slot, None, expected, command, ok, None);
    }

    /// Closes a request no task was made for; it takes a second press.
    #[cfg(test)]
    fn withdraw_request(&mut self) {
        let Some(r) = self.selected_request().cloned() else {
            return;
        };
        if r.closed {
            self.fail(tr!("It is already closed", "Ya está cerrada"));
            return;
        }
        let title = md::truncate(&self.request_title(&r), 60);
        let slot = format!("close_request:{}", r.id);
        if !self.sending.contains_key(&slot)
            && !self.confirmed(
                &slot,
                trf!(
                    "Press x again to withdraw “{title}”",
                    "Pulsa x otra vez para retirar «{title}»"
                ),
            )
        {
            return;
        }
        let revision = self.domain.as_ref().map_or(0, |d| d.revision);
        self.send(
            slot,
            None,
            revision,
            Command::CloseRequest {
                request_id: r.id.clone(),
                answer: None,
                withdrawn: true,
                title: None,
            },
            trf!("Withdrawn: {title}", "Retirada: {title}"),
            None,
        );
    }

    /// Archives every finished task, one command each.
    #[cfg(test)]
    fn archive_finished(&mut self) {
        let Some(d) = &self.domain else { return };
        let revision = d.revision;
        let done: Vec<TaskId> = d
            .tasks
            .iter()
            .filter(|t| t.state.terminal() && !t.archived)
            .map(|t| t.id.clone())
            .collect();
        if done.is_empty() {
            self.fail(tr!(
                "Nothing finished to archive",
                "No hay nada terminado que archivar"
            ));
            return;
        }
        for id in done {
            let title = self
                .task(&id)
                .map(|t| md::truncate(&md::one_line(&t.title), 60))
                .unwrap_or_default();
            self.send(
                format!("archive:{id}"),
                None,
                revision,
                Command::ArchiveTask {
                    task_id: id,
                    archived: true,
                },
                trf!("Archived: {title}", "Archivada: {title}"),
                None,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    fn task(id: &str, deps: &[&str]) -> Task {
        Task {
            id: id.into(),
            request_id: "r".into(),
            agent_id: None,
            title: id.into(),
            state: TaskState::Running,
            revision: 0,
            started: true,
            review_prs: false,
            final_presentation: None,
            pr_id: None,
            dependencies: deps
                .iter()
                .map(|d| TaskId::from(*d))
                .collect::<BTreeSet<_>>(),
            stack: None,
            documents: BTreeMap::new(),
            archived: false,
        }
    }

    fn shape(tasks: &[Task]) -> Vec<(String, usize, bool)> {
        tree(&tasks.iter().collect::<Vec<_>>())
            .into_iter()
            .map(|r| (r.id.0, r.depth, r.last))
            .collect()
    }

    #[test]
    fn a_task_nests_under_its_dependency_and_parallel_ones_share_a_level() {
        let tasks = [task("a", &[]), task("b", &["a"]), task("c", &[])];
        assert_eq!(
            shape(&tasks),
            vec![
                ("a".into(), 0, false),
                ("b".into(), 1, true),
                ("c".into(), 0, true)
            ]
        );
    }

    #[test]
    fn a_cycle_still_shows_every_task() {
        let tasks = [task("a", &["b"]), task("b", &["a"])];
        let ids: Vec<String> = shape(&tasks).into_iter().map(|(id, ..)| id).collect();
        assert_eq!(ids.len(), 2);
    }
}
