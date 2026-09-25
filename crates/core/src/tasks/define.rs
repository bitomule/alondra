//! What the orchestrator works with: the dependencies of the tasks it defines, the nodes a task
//! could go to, and what a node is told when Alondra starts a task on it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::NodeStatus;
use crate::pr::PullRequest;
use crate::view::View;

use super::{Board, Start, Task, TaskState};

/// A task a new one depends on: another task of the same definition by its position (0 is the
/// first), or an existing task by id.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum Dependency {
    Index(usize),
    Id(String),
}

/// The ids each new task depends on, or everything that keeps the definition from standing: a
/// dependency that does not exist or was cancelled, a task that depends on itself, a cycle.
/// `labels` name the new tasks in those reasons; `ids` are the ids they get.
pub fn dependencies(board: &Board, labels: &[String], ids: &[String], depends_on: &[Vec<Dependency>]) -> Result<Vec<Vec<String>>, Vec<String>> {
    let mut problems = vec![];
    let mut resolved = vec![];
    let mut edges: Vec<Vec<usize>> = vec![];
    for (i, deps) in depends_on.iter().enumerate() {
        let (mut these, mut inside) = (vec![], vec![]);
        for d in deps {
            let position = match d {
                Dependency::Id(id) if !board.tasks.contains_key(id.trim()) => id.trim().parse().ok().map(Dependency::Index),
                _ => None,
            };
            match position.as_ref().unwrap_or(d) {
                Dependency::Index(j) if *j == i => problems.push(format!("{} depends on itself", labels[i])),
                Dependency::Index(j) if *j >= ids.len() => {
                    problems.push(format!("{} depends on tasks[{j}], but the definition has {} tasks (the first is tasks[0])", labels[i], ids.len()))
                }
                Dependency::Index(j) => {
                    inside.push(*j);
                    these.push(ids[*j].clone());
                }
                Dependency::Id(id) => match board.tasks.get(id.trim()) {
                    None => problems.push(format!("{} depends on {id}, which does not exist", labels[i])),
                    Some(t) if t.state == TaskState::Cancelled => problems.push(format!("{} depends on {id}, which was cancelled: reopen it or leave it out", labels[i])),
                    Some(t) => these.push(t.id.clone()),
                },
            }
        }
        these.dedup();
        resolved.push(these);
        edges.push(inside);
    }
    if let Some(cycle) = cycle(&edges) {
        let path: Vec<&str> = cycle.iter().map(|i| labels[*i].as_str()).collect();
        problems.push(format!("{} form a cycle", path.join(" → ")));
    }
    if problems.is_empty() { Ok(resolved) } else { Err(problems) }
}

/// A cycle among the new tasks, as the path that closes it.
fn cycle(edges: &[Vec<usize>]) -> Option<Vec<usize>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        New,
        Open,
        Closed,
    }
    fn visit(i: usize, edges: &[Vec<usize>], marks: &mut [Mark], path: &mut Vec<usize>) -> Option<Vec<usize>> {
        marks[i] = Mark::Open;
        path.push(i);
        for &j in &edges[i] {
            match marks[j] {
                Mark::Open => {
                    let from = path.iter().position(|p| *p == j).unwrap_or(0);
                    let mut found = path[from..].to_vec();
                    found.push(j);
                    return Some(found);
                }
                Mark::New => {
                    if let Some(found) = visit(j, edges, marks, path) {
                        return Some(found);
                    }
                }
                Mark::Closed => {}
            }
        }
        path.pop();
        marks[i] = Mark::Closed;
        None
    }
    let mut marks = vec![Mark::New; edges.len()];
    (0..edges.len()).find_map(|i| if marks[i] == Mark::New { visit(i, edges, &mut marks, &mut vec![]) } else { None })
}

/// A live node a task could go to, and why.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Candidate {
    pub node: String,
    pub short_id: String,
    pub project: String,
    pub status: NodeStatus,
    pub tasks: Vec<CandidateTask>,
    pub why: Vec<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct CandidateTask {
    pub id: String,
    pub title: String,
    pub state: TaskState,
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// The name part of `owner/name`, or the whole of a plain name.
fn repo_name(repo: &str) -> &str {
    repo.rsplit('/').next().unwrap_or(repo)
}

impl View {
    /// Tasks Alondra has to start now, oldest first.
    pub fn tasks_to_start(&self) -> Vec<&Task> {
        let mut due: Vec<&Task> = self.board.tasks.values().filter(|t| t.needs_start()).collect();
        due.sort_by_key(|t| (t.created_at, t.id.len(), t.id.clone()));
        due
    }

    /// The open PRs of a task's dependencies: its own work builds on them.
    pub fn stacked_on<'a>(&'a self, task: &'a Task) -> Vec<(&'a Task, &'a PullRequest)> {
        task.depends_on.iter().filter_map(|d| self.board.tasks.get(d)).flat_map(|d| self.open_prs(d).map(move |p| (d, p))).collect()
    }

    /// Which branch a task starts from and where its PR goes, when it builds on dependencies.
    /// `heads` holds the branch of a PR GitHub has not told Alondra yet.
    pub fn stack_note(&self, task: &Task, heads: &BTreeMap<u64, String>) -> Option<String> {
        if task.depends_on.is_empty() {
            return None;
        }
        let branch = |p: &PullRequest| heads.get(&p.id).cloned().or_else(|| p.head.clone());
        let below = self.stacked_on(task);
        let described: Vec<String> = below.iter().map(|(d, p)| format!("{} “{}” (PR #{}, {})", d.id, d.title, p.number, p.url)).collect();
        Some(match below.as_slice() {
            [] => format!("The tasks it comes after ({}) have no PR waiting: start from the default branch, up to date.", task.depends_on.join(", ")),
            [(_, p)] => match branch(p) {
                Some(b) => format!(
                    "It builds on {}, not merged yet. Base your work on its branch `{b}`, not on the default branch: `git fetch origin {b}`, create your branch from `origin/{b}`, and open your PR against it (`gh pr create --base {b}`). Your PR merges after PR #{}.",
                    described[0], p.number
                ),
                None => format!(
                    "It builds on {}, not merged yet. Base your work on that PR's branch (`gh pr view {} --json headRefName` names it) and open your PR against it. Your PR merges after PR #{}.",
                    described[0], p.url, p.number
                ),
            },
            [(_, first), ..] => {
                let branches: Vec<String> = below.iter().map(|(_, p)| branch(p).map(|b| format!("`{b}`")).unwrap_or_else(|| format!("the branch of PR #{}", p.number))).collect();
                format!(
                    "It builds on {}, none merged yet. Base your work on {}, merge {} into your branch, and open your PR against {}. Your PR merges after all of them.",
                    described.join(", "),
                    branches[0],
                    branches[1..].join(", "),
                    branch(first).map(|b| format!("`{b}`")).unwrap_or_else(|| "that branch".into())
                )
            }
        })
    }

    /// What a node is told when Alondra starts a task on it: the orchestrator's brief, which task
    /// it is, and where to build it.
    pub fn start_brief(&self, task: &Task, heads: &BTreeMap<u64, String>) -> String {
        let brief = task.brief.as_deref().map(str::trim).filter(|b| !b.is_empty()).unwrap_or(&task.title);
        let mut section = vec![format!("## Task {} in Alondra", task.id), String::new(), format!("This is task {} (“{}”) of request {}.", task.id, task.title, task.request)];
        if matches!(task.start, Some(Start::Node { .. })) {
            section.push(format!(
                "You work on other tasks too: pass task \"{}\" on every ask, request_action, report and withdraw about this one, and work it in a subagent of its own so the tasks do not mix (the Alondra skill says how).",
                task.id
            ));
        }
        if let Some(note) = self.stack_note(task, heads) {
            section.push(note);
        }
        format!("{brief}\n\n{}", section.join("\n"))
    }

    /// Live nodes a task, or work in a repo or project, could go to: the ones already in that
    /// project or repo, or working on tasks it is tied to, with why and whether they are free.
    pub fn candidates(&self, task: Option<&Task>, repo: Option<&str>) -> Vec<Candidate> {
        let project = repo.map(repo_name).or_else(|| task.and_then(|t| t.options.project.as_deref()));
        let mut out: Vec<Candidate> = self
            .nodes
            .values()
            .filter(|n| !n.is_orchestrator && (n.alive || n.in_daemon) && !self.removed.contains_key(&n.session_id))
            .filter_map(|n| {
                let mine: Vec<&Task> = self.tasks_of(&n.session_id).collect();
                let mut why = vec![];
                if let Some(p) = project.filter(|p| same(p, &n.project())) {
                    why.push(format!("works in project {p}"));
                }
                if let Some(p) = project {
                    for pr in self.prs.values().filter(|pr| pr.session_id == n.session_id && pr.state == crate::pr::PrState::Open && same(repo_name(&pr.repo), p)) {
                        why.push(format!("has PR #{} open in {}", pr.number, pr.repo));
                    }
                }
                if let Some(t) = task {
                    if t.is_assigned_to(&n.session_id) {
                        why.push(format!("works on {} itself", t.id));
                    }
                    for m in mine.iter().filter(|m| m.id != t.id) {
                        if t.depends_on.contains(&m.id) {
                            why.push(format!("works on {} “{}”, which {} builds on", m.id, m.title, t.id));
                        } else if m.depends_on.contains(&t.id) {
                            why.push(format!("works on {} “{}”, which builds on {}", m.id, m.title, t.id));
                        } else if m.request == t.request {
                            why.push(format!("works on {} “{}” of the same request", m.id, m.title));
                        }
                    }
                }
                (!why.is_empty()).then(|| Candidate {
                    node: n.display_name().to_string(),
                    short_id: n.short_id.clone(),
                    project: n.project(),
                    status: n.status,
                    tasks: mine.iter().filter(|m| !m.state.finished()).map(|m| CandidateTask { id: m.id.clone(), title: m.title.clone(), state: m.state }).collect(),
                    why,
                })
            })
            .collect();
        out.sort_by_key(|c| (std::cmp::Reverse(c.why.len()), c.status != NodeStatus::Free, c.node.clone()));
        out
    }

    /// What the orchestrator is told when its turn ended with a request it received still
    /// defining: neither defined nor closed. Once per request, read from its messages, and not
    /// while something of the user's is still on its way to it or a question of its own waits.
    pub fn defining_nudges(&self, user: &str, now: crate::model::Millis) -> Vec<crate::model::Message> {
        let Some(o) = self.nodes.values().find(|n| n.is_orchestrator && n.alive) else { return vec![] };
        let Some(stopped) = o.last_stop_at.filter(|stop| *stop >= o.turn_since) else { return vec![] };
        if self.pending_messages(&o.session_id).next().is_some() {
            return vec![];
        }
        let asked_since = |at: crate::model::Millis| {
            self.questions.values().any(|q| q.session_id == o.session_id && q.asked_at >= at && (q.status.is_open() || q.status == crate::model::QuestionStatus::Answered))
        };
        let mut next_id = self.messages.keys().next_back().copied().unwrap_or(0);
        self.board
            .requests
            .values()
            .filter(|r| !r.synthetic && r.closed.is_none() && r.created_at < stopped && now - r.created_at < DEFINING_NUDGE_WINDOW)
            .filter(|r| self.board.request_state(&r.id) == super::RequestState::Defining && !asked_since(r.created_at))
            .filter(|r| !self.messages.values().any(|m| m.session_id == o.session_id && m.text.starts_with(&defining_prefix(&r.id))))
            .map(|r| {
                next_id += 1;
                let asked: String = r.text.as_deref().unwrap_or("").chars().take(80).collect();
                crate::model::Message {
                    id: next_id,
                    session_id: o.session_id.clone(),
                    from: crate::model::MessageFrom::Alondra,
                    text: format!(
                        "{}“{asked}”) is still defining: your turn ended without defining it or closing it. If it needs work, `define` it. \
                         If it was a question or a status check you already answered, or {user} withdrew it, close it with `close_request`.",
                        defining_prefix(&r.id)
                    ),
                    answers_question: None,
                    created_at: now,
                    handed_at: None,
                    delivered_at: None,
                    about: None,
                    attachments: vec![],
                }
            })
            .collect()
    }
}

/// A request older than this is not nudged: it predates the nudge, or the orchestrator long
/// since let it be.
pub const DEFINING_NUDGE_WINDOW: crate::model::Millis = 24 * 3600 * 1000;

fn defining_prefix(request: &str) -> String {
    format!("[Alondra] Request {request} (")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{DaemonObservation, Event};
    use crate::model::{Message, MessageAbout, MessageFrom, Millis};
    use crate::tasks::{NewTask, TaskEvent, TaskOptions};
    use Dependency::{Id, Index};
    use TaskState::*;

    const PR: &str = "https://github.com/acme/app/pull/7";

    struct Daemon {
        view: View,
        log: Vec<(Millis, Event)>,
        at: Millis,
    }

    impl Daemon {
        fn new() -> Self {
            let mut d = Daemon { view: View::default(), log: vec![], at: 0 };
            d.on(Event::TasksRecorded);
            d
        }

        fn on(&mut self, e: Event) -> &mut Self {
            self.at += 1000;
            let at = self.at;
            let written = self.view.record(at, &e);
            self.log.extend(written.into_iter().map(|e| (at, e)));
            self
        }

        fn state(&self, id: &str) -> TaskState {
            self.view.board.tasks[id].state
        }

        fn due(&self) -> Vec<String> {
            self.view.tasks_to_start().into_iter().map(|t| t.id.clone()).collect()
        }

        fn replayed(&self) -> View {
            let mut v = View::default();
            for (i, (at, e)) in self.log.iter().enumerate() {
                v.apply(i as u64 + 1, *at, e);
            }
            v
        }
    }

    fn defined(id: &str, depends_on: &[&str], start: Start) -> NewTask {
        NewTask {
            id: id.into(),
            request: "r-1".into(),
            title: format!("Task {id}"),
            brief: Some(format!("Build {id}.")),
            depends_on: depends_on.iter().map(|d| d.to_string()).collect(),
            options: TaskOptions::default(),
            session_id: None,
            launched: false,
            launched_as: None,
            start: Some(start),
        }
    }

    fn created(id: &str, depends_on: &[&str], start: Start) -> Event {
        Event::TaskCreated(defined(id, depends_on, start))
    }

    fn new_node(name: &str) -> Start {
        Start::NewNode { name: name.into(), cwd: "/tmp".into(), model: None }
    }

    fn seen(sid: &str, name: &str, cwd: &str) -> Event {
        Event::DaemonSeen(DaemonObservation { session_id: sid.into(), short_id: sid.into(), name: Some(name.into()), cwd: Some(cwd.into()), pid: Some(1), ..Default::default() })
    }

    fn launched(name: &str, task: &str) -> Event {
        Event::Launched { name: name.into(), request_id: Some("r-1".into()), auto_merge: false, task: Some(task.into()), title: None, session: None }
    }

    fn opened(sid: &str) -> Event {
        Event::PrOpened { url: PR.into(), session_id: sid.into(), task: None }
    }

    #[test]
    fn a_definition_names_its_dependencies_by_position_or_id() {
        let mut d = Daemon::new();
        d.on(created("t-1", &[], new_node("a"))).on(created("t-2", &[], new_node("b")));
        d.on(Event::TaskMoved { task: "t-2".into(), event: TaskEvent::Cancel { reason: None } });
        let board = d.view.board.clone();
        let labels: Vec<String> = (0..3).map(|i| format!("tasks[{i}]")).collect();
        let ids: Vec<String> = ["t-10", "t-11", "t-12"].map(String::from).into();
        let deps = |d: Vec<Vec<Dependency>>| dependencies(&board, &labels, &ids, &d);

        assert_eq!(deps(vec![vec![], vec![Index(0)], vec![Id("t-1".into()), Index(0), Index(0)]]), Ok(vec![vec![], vec!["t-10".into()], vec!["t-1".into(), "t-10".into()]]));
        assert_eq!(deps(vec![vec![], vec![Id("0".into())], vec![]]), Ok(vec![vec![], vec!["t-10".into()], vec![]]), "a position sent as text still counts");

        let refused = |d: Vec<Vec<Dependency>>| deps(d).expect_err("refused");
        assert_eq!(refused(vec![vec![Id("t-9".into())], vec![], vec![]]), ["tasks[0] depends on t-9, which does not exist"]);
        assert_eq!(refused(vec![vec![], vec![Id("t-2".into())], vec![]]), ["tasks[1] depends on t-2, which was cancelled: reopen it or leave it out"]);
        assert_eq!(refused(vec![vec![], vec![Index(1)], vec![]]), ["tasks[1] depends on itself"]);
        assert_eq!(refused(vec![vec![], vec![], vec![Index(5)]]), ["tasks[2] depends on tasks[5], but the definition has 3 tasks (the first is tasks[0])"]);
        assert_eq!(refused(vec![vec![Index(2)], vec![Index(0)], vec![Index(1)]]), ["tasks[0] → tasks[2] → tasks[1] → tasks[0] form a cycle"]);
    }

    /// A then B, and C in parallel with both: A and C start at once, B once A's PR is open, on
    /// A's branch.
    #[test]
    fn a_waiting_task_starts_once_its_dependency_has_its_pr_open_and_builds_on_its_branch() {
        let mut d = Daemon::new();
        d.on(created("t-a", &[], new_node("app/model"))).on(created("t-b", &["t-a"], new_node("app/ui"))).on(created("t-c", &[], new_node("app/docs")));
        assert_eq!((d.state("t-a"), d.state("t-b"), d.state("t-c")), (Running, WaitingDependency, Running));
        assert_eq!(d.due(), ["t-a", "t-c"], "the tasks with nothing to wait for start at once");

        d.on(launched("app/model", "t-a")).on(launched("app/docs", "t-c"));
        assert!(d.due().is_empty(), "a launched task is started, whether or not its node showed up yet");
        d.on(seen("s", "app/model", "/Users/x/Projects/app"));
        assert!(d.view.board.tasks["t-a"].is_assigned_to("s"), "the node launched for it takes it");
        assert_eq!(d.state("t-b"), WaitingDependency);

        d.on(opened("s"));
        assert_eq!((d.state("t-a"), d.state("t-b")), (InReview, Running));
        assert_eq!(d.due(), ["t-b"]);
        assert!(d.log.iter().any(|(_, e)| matches!(e, Event::TaskMoved { task, event: TaskEvent::DependencyReady { task: on } } if task == "t-b" && on == "t-a")));

        let unknown = d.view.stack_note(&d.view.board.tasks["t-b"], &BTreeMap::new()).unwrap();
        assert!(unknown.contains(&format!("gh pr view {PR} --json headRefName")), "{unknown}");
        let mut pr = d.view.prs[&1].clone();
        (pr.base, pr.head, pr.fetched) = (Some("main".into()), Some("feat/model".into()), true);
        d.on(Event::PrUpdated(pr));
        let brief = d.view.start_brief(&d.view.board.tasks["t-b"], &BTreeMap::new());
        assert!(brief.starts_with("Build t-b.\n\n## Task t-b in Alondra"), "{brief}");
        assert!(brief.contains("Base your work on its branch `feat/model`") && brief.contains("gh pr create --base feat/model") && brief.contains("Your PR merges after PR #7."), "{brief}");
        assert_eq!(d.view.task_guards(&d.view.board.tasks["t-b"]).unmerged_dependency, Some(7), "its PR merges after t-a's");

        let replayed = d.replayed();
        assert_eq!(replayed.board, d.view.board, "the log holds every start and every move");
        assert_eq!(replayed.tasks_to_start().into_iter().map(|t| t.id.clone()).collect::<Vec<_>>(), ["t-b"], "what was started stays started after a restart");
    }

    #[test]
    fn a_task_defined_on_a_dependency_that_is_ready_starts_at_once() {
        let mut d = Daemon::new();
        d.on(created("t-a", &[], new_node("app/model"))).on(launched("app/model", "t-a")).on(seen("s", "app/model", "/tmp")).on(opened("s"));
        d.on(created("t-b", &["t-a"], Start::Node { session_id: "s".into() }));
        assert_eq!(d.state("t-b"), Running);
        let brief = d.view.start_brief(&d.view.board.tasks["t-b"], &BTreeMap::from([(1, "feat/model".to_string())]));
        assert!(brief.contains("pass task \"t-b\" on every ask") && brief.contains("`feat/model`"), "a node given a second task is told to name it: {brief}");
    }

    #[test]
    fn a_message_about_a_task_moves_that_task_not_the_newest() {
        let mut d = Daemon::new();
        d.on(created("t-a", &[], new_node("app/model"))).on(launched("app/model", "t-a")).on(seen("s", "app/model", "/tmp"));
        d.on(Event::TaskMoved { task: "t-a".into(), event: TaskEvent::FinalReport { report: 1 } });
        d.on(created("t-b", &[], Start::Node { session_id: "s".into() })).on(Event::TaskAssigned { task: "t-b".into(), session_id: "s".into(), agent_id: None });
        assert_eq!((d.state("t-a"), d.state("t-b")), (Done, Running));
        let about = |id: u64, task: Option<&str>| Message {
            id,
            session_id: "s".into(),
            from: MessageFrom::Orchestrator,
            text: "one more thing".into(),
            answers_question: None,
            created_at: 0,
            handed_at: None,
            delivered_at: None,
            attachments: vec![],
            about: task.map(|t| MessageAbout::Task { task: t.into() }),
        };
        d.on(Event::MessageQueued(about(1, None))).on(Event::MessageHanded { id: 1 });
        assert_eq!(d.state("t-a"), Done, "a message about no task in particular goes to the newest");
        d.on(Event::MessageQueued(about(2, Some("t-a")))).on(Event::MessageHanded { id: 2 });
        assert_eq!((d.state("t-a"), d.state("t-b")), (Running, Running), "one about t-a reopens t-a");
    }

    #[test]
    fn candidates_are_live_nodes_tied_to_the_task_by_project_dependency_or_request() {
        let mut d = Daemon::new();
        d.on(created("t-a", &[], new_node("app/model"))).on(launched("app/model", "t-a")).on(seen("s1", "app/model", "/Users/x/Projects/app"));
        d.on(created("t-c", &[], new_node("web/copy"))).on(launched("web/copy", "t-c")).on(seen("s2", "web/copy", "/Users/x/Projects/web"));
        d.on(seen("s3", "other/thing", "/Users/x/Projects/other")).on(seen("s4", "app/old", "/Users/x/Projects/app"));
        d.on(seen("o", "orchestrator", "/Users/x")).on(Event::MarkedOrchestrator { session_id: "o".into() });
        d.on(Event::NodeRemoved { session_id: "s4".into() });
        let mut b = defined("t-b", &["t-a"], new_node("app/ui"));
        b.options.project = Some("app".into());
        d.on(Event::TaskCreated(b));

        let got = d.view.candidates(Some(&d.view.board.tasks["t-b"]), None);
        let summary: Vec<(&str, Vec<&str>)> = got.iter().map(|c| (c.node.as_str(), c.why.iter().map(String::as_str).collect())).collect();
        assert_eq!(
            summary,
            [("app/model", vec!["works in project app", "works on t-a “Task t-a”, which t-b builds on"]), ("web/copy", vec!["works on t-c “Task t-c” of the same request"])],
            "the orchestrator, closed nodes and unrelated ones are left out"
        );
        assert_eq!((got[0].status, got[0].tasks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>()), (NodeStatus::Free, vec!["t-a"]));
        let by_repo: Vec<String> = d.view.candidates(None, Some("acme/web")).into_iter().map(|c| c.node).collect();
        assert_eq!(by_repo, ["web/copy"]);
    }

    /// r-1790277835004, 24 Sep: "En qué punto tenemos a BlurCamera?" arrived, the orchestrator asked
    /// #240, David answered, and the orchestrator stopped with no tool call: the request sat in
    /// defining, where nothing could move it.
    #[test]
    fn a_request_left_defining_when_the_orchestrators_turn_ends_gets_one_nudge() {
        use crate::event::HookEvent;
        use crate::model::{Answer, Question, QuestionOrigin, QuestionStatus};
        let hook = |event: &str| Event::Hook(HookEvent { session_id: "o".into(), event: event.into(), prompt: Some("go".into()), ..Default::default() });
        let options = crate::options::Resolved { project: None, auto_merge: false, effort: "normal".into(), flags: Default::default(), picked: vec![], issue: None };
        let question = Question {
            id: 240, session_id: "o".into(), node_name: None, title: "¿Qué parte?".into(), detail: None, options: vec![], links: vec![], attachments: vec![],
            blocking: true, origin: QuestionOrigin::Ask, tool_use_id: None, to_orchestrator: false, node_context: None, status: QuestionStatus::Open,
            answer: None, asked_at: 4000, action: false, steps: vec![], mission: None, replaces: None, about_pr: None, task: None,
        };
        let mut d = Daemon::new();
        d.on(Event::MarkedOrchestrator { session_id: "o".into() }).on(hook("SessionStart"));
        d.on(Event::RequestSent { request_id: "r-1".into(), options, text: Some("En qué punto tenemos a BlurCamera?".into()), attachments: vec![] });
        d.on(hook("UserPromptSubmit")).on(Event::QuestionAsked(question)).on(hook("Stop"));
        assert!(d.view.defining_nudges("Ana", d.at).is_empty(), "it waits for Ana's answer");
        d.on(Event::QuestionAnswered { id: 240, answer: Answer { text: "El estado".into(), option: None, by: None, device: None, at: 0 } });
        assert!(d.view.defining_nudges("Ana", d.at).is_empty(), "the answer is on its way");
        d.on(Event::QuestionDelivered { id: 240 }).on(hook("UserPromptSubmit"));
        assert!(d.view.defining_nudges("Ana", d.at).is_empty(), "its turn is under way");
        d.on(hook("Stop"));
        let nudges = d.view.defining_nudges("Ana", d.at);
        assert_eq!(nudges.len(), 1);
        assert!(nudges[0].session_id == "o" && nudges[0].text.starts_with("[Alondra] Request r-1 (“En qué punto tenemos a BlurCamera?”) is still defining"), "{}", nudges[0].text);
        assert!(nudges[0].text.contains("close_request"));
        d.on(Event::MessageQueued(nudges[0].clone())).on(Event::MessageHanded { id: nudges[0].id }).on(hook("UserPromptSubmit")).on(hook("Stop"));
        assert!(d.view.defining_nudges("Ana", d.at).is_empty(), "once");
        assert!(d.replayed().defining_nudges("Ana", d.at).is_empty(), "a restart reads the nudge from the log");
        d.on(Event::RequestClosed { request_id: "r-1".into(), answer: Some("BlurCamera está en revisión".into()), withdrawn: false });
        assert_eq!(d.view.board.request_state("r-1"), crate::tasks::RequestState::Done);
    }

    /// Round 2: a request closed and then given a task (a define before closed ones were refused,
    /// or a launch naming it) stayed done while its task ran.
    #[test]
    fn a_closed_request_that_gets_a_task_goes_by_its_task() {
        use crate::tasks::RequestState;
        let options = crate::options::Resolved { project: None, auto_merge: false, effort: "normal".into(), flags: Default::default(), picked: vec![], issue: None };
        let mut d = Daemon::new();
        d.on(Event::RequestSent { request_id: "r-1".into(), options, text: Some("How is it going?".into()), attachments: vec![] });
        d.on(Event::RequestClosed { request_id: "r-1".into(), answer: Some("answered".into()), withdrawn: false });
        assert_eq!(d.view.board.request_state("r-1"), RequestState::Done, "closed with no task");
        d.on(created("t-1", &[], new_node("app/one")));
        assert_eq!(d.state("t-1"), Running);
        assert_eq!(d.view.board.request_state("r-1"), RequestState::InProgress, "a request with a running task is in progress");
        assert_eq!(d.replayed().board.request_state("r-1"), RequestState::InProgress);
        d.on(Event::TaskMoved { task: "t-1".into(), event: TaskEvent::Cancel { reason: None } });
        assert_eq!(d.view.board.request_state("r-1"), RequestState::Done);
    }
}
