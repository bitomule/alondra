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
    /// The task detail View carries its brief and history.
    fn load_task_history(&mut self) {
        self.load_selected_view();
    }

    /// The task detail View carries the available controls and their current state.
    fn load_task_actions(&mut self) {
        self.load_selected_view();
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
