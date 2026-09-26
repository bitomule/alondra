//! What a started task waits for outside its node: the merge of a PR, its own or one of a task it
//! depends on. A task that has nothing left but that merge waits for it (`awaitable_merge`), and
//! every merge reaches the nodes of the tasks it concerns (`merge_notices_due`), whichever way the
//! PR was merged and whenever those nodes can hear it.

use super::{Task, TaskState};
use crate::model::{Message, MessageAbout, MessageFrom, Millis, Node};
use crate::pr::{PrState, PullRequest};
use crate::view::View;

/// Every PR number mentioned as "PR #N", "PR N" or "PR#N", in any case.
pub fn pr_numbers_in(text: &str) -> Vec<u64> {
    let mut found = vec![];
    let lower = text.to_ascii_lowercase();
    for (i, _) in lower.match_indices("pr") {
        let before = lower[..i].chars().next_back();
        if before.is_some_and(|c| c.is_ascii_alphanumeric()) {
            continue;
        }
        let rest = lower[i + 2..].trim_start_matches([' ', '#']);
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse::<u64>()
            && !found.contains(&n)
        {
            found.push(n);
        }
    }
    found
}

/// A PR a node's last words name: a card Alondra keeps, or a URL it has no card for.
#[derive(Debug, PartialEq)]
pub enum Named<'a> {
    Card(&'a PullRequest),
    Uncarded(String),
}

impl View {
    /// The dependency's PR a running task can only wait for: nothing of its own is open for
    /// review, and a task it depends on still has a PR open.
    pub fn awaitable_merge(&self, task: &Task) -> Option<&PullRequest> {
        if task.state != TaskState::Running || task.blocker.is_some() || self.open_prs(task).next().is_some() {
            return None;
        }
        task.depends_on.iter().filter_map(|d| self.board.tasks.get(d)).flat_map(|d| self.open_prs(d)).next()
    }

    /// When the task first left `waiting_dependency`: merges of its dependencies before then are
    /// in its brief, not news.
    fn started_at(task: &Task) -> Millis {
        task.history.iter().find(|h| h.from == TaskState::WaitingDependency && h.to != TaskState::WaitingDependency).map_or(task.created_at, |h| h.at)
    }

    fn started(task: &Task) -> bool {
        match task.state {
            TaskState::Running | TaskState::NeedsYou | TaskState::InReview | TaskState::Closing => true,
            TaskState::WaitingDependency => task.awaiting.is_some(),
            TaskState::Done | TaskState::Cancelled => false,
        }
    }

    /// The merges the nodes of started, unfinished tasks have yet to hear: each merged PR of the
    /// task itself, and each PR of a task it depends on merged after it started. One notice per
    /// task and PR, typed into the task's node once it can take Alondra's messages.
    pub fn merge_notices_due(&self, at: Millis) -> Vec<Message> {
        let mut next_id = self.next_message_id() - 1;
        let mut due = vec![];
        for task in self.board.tasks.values().filter(|t| t.recorded && Self::started(t)) {
            let Some(node) = task.assignee.as_ref().and_then(|a| self.nodes.get(&a.session_id)) else { continue };
            if self.removed.contains_key(&node.session_id) || !(node.alive || node.in_daemon) || !self.may_alondra_write(node) {
                continue;
            }
            let told = |pr: u64, dependency: bool| {
                self.messages.values().any(|m| {
                    m.session_id == node.session_id
                        && m.from == MessageFrom::Alondra
                        && matches!(&m.about, Some(MessageAbout::PrMerged { pr_id, task: named }) if *pr_id == pr && (!dependency || named.as_deref() == Some(task.id.as_str())))
                })
            };
            let merged = |p: &&PullRequest| p.state == PrState::Merged && p.merged_at.is_some();
            let own = task.prs.iter().filter_map(|id| self.prs.get(id)).filter(merged).filter(|p| !told(p.id, false)).map(|p| (p, None));
            let since = Self::started_at(task);
            let dependencies = task
                .depends_on
                .iter()
                .filter_map(|d| self.board.tasks.get(d))
                .flat_map(|d| d.prs.iter().filter_map(|id| self.prs.get(id)).map(move |p| (p, Some(d))))
                .filter(|(p, _)| merged(p) && p.merged_at.is_some_and(|m| m >= since) && !told(p.id, true));
            for (pr, dependency) in own.chain(dependencies).collect::<Vec<_>>() {
                next_id += 1;
                due.push(Message {
                    id: next_id,
                    session_id: node.session_id.clone(),
                    from: MessageFrom::Alondra,
                    text: merge_notice_text(pr, task, dependency),
                    answers_question: None,
                    created_at: at,
                    handed_at: None,
                    delivered_at: None,
                    about: Some(MessageAbout::PrMerged { pr_id: pr.id, task: dependency.map(|_| task.id.clone()) }),
                    attachments: vec![],
                });
            }
        }
        due
    }

    /// The PRs the node's last words name, as "PR #N" or by URL, in the repos it works in: its
    /// own PRs', and those of the tasks its tasks depend on.
    pub fn named_prs(&self, node: &Node) -> Vec<Named<'_>> {
        let texts: Vec<&str> = [node.last_message.as_deref(), node.daemon_needs.as_deref()].into_iter().flatten().collect();
        let tasks: Vec<&Task> = self.tasks_of(&node.session_id).collect();
        let dependencies = tasks.iter().flat_map(|t| &t.depends_on).filter_map(|d| self.board.tasks.get(d));
        let mut repos: Vec<&str> = self.prs.values().filter(|p| p.session_id == node.session_id).map(|p| p.repo.as_str()).collect();
        repos.extend(tasks.iter().copied().chain(dependencies).flat_map(|t| &t.prs).filter_map(|id| self.prs.get(id)).map(|p| p.repo.as_str()));
        repos.sort_unstable();
        repos.dedup();
        let card = |repo: &str, number: u64| self.prs.values().find(|p| p.repo == repo && p.number == number);
        let mut found: Vec<Named> = vec![];
        for text in &texts {
            for url in crate::pr::urls_in(text) {
                let Some((repo, number)) = crate::pr::parse_url(&url) else { continue };
                if repos.contains(&repo.as_str()) {
                    found.push(card(&repo, number).map_or(Named::Uncarded(url), Named::Card));
                }
            }
            for number in pr_numbers_in(text) {
                let mut cards = repos.iter().filter_map(|r| card(r, number));
                if let (Some(p), None) = (cards.next(), cards.next()) {
                    found.push(Named::Card(p));
                }
            }
        }
        let mut named = vec![];
        for n in found {
            if !named.contains(&n) {
                named.push(n);
            }
        }
        named
    }
}

/// What the node of `task` hears when `pr`, its own or the PR of `dependency`, was merged.
pub fn merge_notice_text(pr: &PullRequest, task: &Task, dependency: Option<&Task>) -> String {
    match dependency {
        None => format!("[Alondra] {} was merged. It is a PR of your task {} “{}”: do what the merge unblocks, then report final.", pr.url, task.id, task.title),
        Some(d) => {
            let branch = match (pr.head.as_deref(), pr.base.as_deref()) {
                (Some(head), Some(base)) => format!(" Its branch `{head}` is in `{base}` now: if you built on it, bring `origin/{base}` into your branch."),
                _ => String::new(),
            };
            format!(
                "[Alondra] {} was merged: the PR of {} “{}”. Your task {} “{}” depends on it, so what waited for this merge can go ahead now.{branch} Carry on with {}.",
                pr.url, d.id, d.title, task.id, task.title, task.id
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{DaemonObservation, Event, HookEvent};
    use crate::model::{Link, NodeState, Report, ReportKind};
    use crate::pr::MergeMethod;
    use crate::states;
    use crate::tasks::{NewTask, Start, TaskEvent, TaskOptions};

    const U: &str = "a7e1d69b-4ecd-4686-8796-a36ea056dc50";
    const PR264: &str = "https://github.com/acmecorp-private/Redraft/pull/264";
    const FIX: &str = "t-44136";
    const BETA: &str = "t-44137";
    const MIN: Millis = 60_000;
    const DEFINED: Millis = 1_790_284_530_968;
    const PR_OPENED: Millis = 1_790_290_328_427;
    const REPORTED: Millis = 1_790_290_343_322;
    const LAST_TURN: Millis = 1_790_290_638_428;
    const STOPPED: Millis = 1_790_290_643_630;
    const MERGED: Millis = 1_790_303_823_254;
    const LAST_WORDS: &str = "La beta 4.5.0 (192) está preparada y no se subirá hasta que fusiones la PR #264, el arreglo que saca las llamadas a Fotos (PhotoKit) de los hilos de Swift. Ese es el único paso que falta, y es tuyo.";

    fn hook(event: &str, last_message: Option<&str>) -> Event {
        Event::Hook(HookEvent { session_id: U.into(), event: event.into(), last_message: last_message.map(str::to_string), ..Default::default() })
    }

    fn seen(state: &str, needs: Option<&str>) -> Event {
        Event::DaemonSeen(DaemonObservation {
            session_id: U.into(),
            short_id: "a7e1d69b".into(),
            name: Some("redraft/atasco-analizando".into()),
            pid: Some(46180),
            state: Some(state.into()),
            tempo: Some(state.into()),
            needs: needs.map(str::to_string),
            ..Default::default()
        })
    }

    fn defined(id: &str, title: &str, depends_on: &[&str]) -> Event {
        Event::TaskCreated(NewTask {
            id: id.into(),
            request: "r-1790284418392".into(),
            title: title.into(),
            brief: None,
            depends_on: depends_on.iter().map(|d| d.to_string()).collect(),
            options: TaskOptions::default(),
            session_id: None,
            launched: false,
            launched_as: None,
            start: Some(Start::Node { session_id: U.into() }),
        })
    }

    fn assigned(task: &str) -> Event {
        Event::TaskAssigned { task: task.into(), session_id: U.into(), agent_id: None }
    }

    fn told(id: u64, task: &str) -> [Event; 2] {
        let m = Message {
            id,
            session_id: U.into(),
            from: MessageFrom::Orchestrator,
            text: format!("A new task for you: {task}"),
            answers_question: None,
            created_at: PR_OPENED,
            handed_at: None,
            delivered_at: None,
            about: Some(MessageAbout::Task { task: task.into() }),
            attachments: vec![],
        };
        [Event::MessageQueued(m), Event::MessageHanded { id }]
    }

    fn final_report() -> Event {
        Event::ReportPosted(Report {
            id: 243,
            session_id: U.into(),
            node_name: None,
            title: "PR #264: las tarjetas de Inicio ya no bloquean la búsqueda de grupos".into(),
            body: None,
            links: vec![Link { url: PR264.into(), title: Some("PR #264".into()) }],
            attachments: vec![],
            kind: ReportKind::Final,
            created_at: REPORTED,
            read: false,
            to_orchestrator: false,
            mission: None,
            looks_like_question: false,
            task: Some(FIX.into()),
        })
    }

    /// Request r-1790284418392 as the log has it: the PhotoKit fix (t-44136, PR #264) and the
    /// beta that depends on it (t-44137), both on node redraft/atasco-analizando. The beta starts
    /// when PR #264 opens; the node reports the fix final, prepares the beta and ends its turn
    /// waiting for David to merge PR #264.
    struct Redraft {
        view: View,
        log: Vec<(Millis, Event)>,
    }

    impl Redraft {
        fn new() -> Self {
            let mut c = Redraft { view: View::default(), log: vec![] };
            c.on(DEFINED, Event::TasksRecorded);
            c.on(DEFINED, defined(FIX, "Redraft: sacar las llamadas síncronas a PhotoKit de los hilos de Swift", &[]));
            c.on(DEFINED, defined(BETA, "Redraft: beta 4.5.0 (192) con el arreglo y el registro", &[FIX]));
            c.on(DEFINED, seen("working", None)).on(DEFINED, assigned(FIX)).on(DEFINED, hook("UserPromptSubmit", None));
            c.on(PR_OPENED, Event::PrOpened { url: PR264.into(), session_id: U.into(), task: Some(FIX.into()) });
            let mut pr = c.view.prs[&1].clone();
            (pr.fetched, pr.base, pr.head) = (true, Some("main".into()), Some("photokit-off-the-pool".into()));
            c.on(PR_OPENED, Event::PrUpdated(pr)).on(PR_OPENED, assigned(BETA));
            for e in told(665, BETA) {
                c.on(PR_OPENED, e);
            }
            c.on(REPORTED, final_report());
            c.on(LAST_TURN, hook("UserPromptSubmit", None)).on(STOPPED, hook("Stop", Some(LAST_WORDS)));
            c.on(STOPPED + 3000, seen("blocked", Some("merge PR #264 to trigger beta build 192 upload")));
            c
        }

        fn on(&mut self, at: Millis, e: Event) -> &mut Self {
            let written = self.view.record(at, &e);
            self.log.extend(written.into_iter().map(|e| (at, e)));
            self
        }

        /// What the sentinel does on a sweep at `at`, with or without the merge notices.
        fn sweep(&mut self, at: Millis, notices: bool) -> Vec<Message> {
            let mut said = vec![];
            if notices {
                for m in self.view.merge_notices_due(at) {
                    said.push(m.clone());
                    self.on(at, Event::MessageQueued(m));
                }
            }
            let waits: Vec<(String, u64)> = self.view.nodes.values().filter_map(|n| states::awaits_merge(&self.view, n, at)).map(|(t, p)| (t.id.clone(), p.id)).collect();
            for (task, pr) in waits {
                self.on(at, Event::TaskMoved { task, event: TaskEvent::AwaitsMerge { pr } });
            }
            self.on(at, Event::Clock { machine_strained: false });
            said
        }

        fn merge(&mut self) -> &mut Self {
            self.on(MERGED, Event::PrMerged { id: 1, method: MergeMethod::Merge })
        }

        fn task(&self, id: &str) -> &Task {
            &self.view.board.tasks[id]
        }

        fn node(&self) -> &Node {
            &self.view.nodes[U]
        }

        fn row(&self) -> String {
            self.node().evidence.as_ref().map(|e| e.row.clone()).unwrap_or_default()
        }
    }

    type Check = fn(&mut Redraft) -> Result<(), String>;

    fn expect(ok: bool, what: String) -> Result<(), String> {
        if ok { Ok(()) } else { Err(what) }
    }

    /// Each fix of t-48599 on the case it comes from; each row fails with its fix taken out.
    #[test]
    fn the_merge_of_pr_264_reaches_the_beta_that_waited_for_it() {
        let rows: Vec<(&str, Check)> = vec![
            ("the fix's final report says nothing about the beta", |c| {
                let beta = c.task(BETA).clone();
                let n = c.node();
                expect(!states::declared_for(n, &beta) && n.state != NodeState::Delivered, format!("node {:?}", n.state))
            }),
            ("the beta, with nothing left but the merge of PR #264, is shown waiting for it", |c| {
                c.on(STOPPED + 10 * MIN, Event::Clock { machine_strained: false });
                let before = (c.node().state, c.row());
                c.sweep(STOPPED + 10 * MIN, true);
                let beta = c.task(BETA);
                expect(
                    before == (NodeState::WaitingMerge, states::TASK_AWAITS_MERGE.to_string()) && beta.state == TaskState::WaitingDependency && beta.awaiting == Some(1),
                    format!("before the sweep {before:?}, after it {:?} awaiting {:?}", beta.state, beta.awaiting),
                )
            }),
            ("the merge sends the beta back to work", |c| {
                c.sweep(STOPPED + 10 * MIN, true);
                c.merge();
                let states = (c.task(FIX).state, c.task(BETA).state);
                expect(states == (TaskState::Done, TaskState::Running), format!("{states:?}"))
            }),
            ("the merge notice reaches the beta's node, naming the beta, once", |c| {
                c.sweep(STOPPED + 10 * MIN, true);
                c.merge();
                let said = c.sweep(MERGED + 2000, true);
                let about = Some(MessageAbout::PrMerged { pr_id: 1, task: Some(BETA.into()) });
                let told = said.iter().any(|m| m.session_id == U && m.text.contains(PR264) && m.text.contains("Your task t-44137") && m.about == about);
                let again = c.sweep(MERGED + 4000, true);
                expect(told && again.is_empty() && c.node().state == NodeState::WaitingMerge, format!("{said:?} then {again:?}, node {:?}", c.node().state))
            }),
            ("a merge that never reached the node leaves it stalled and nudged, not delivered", |c| {
                c.merge();
                c.sweep(MERGED + 10 * MIN, false);
                let n = c.node();
                let idle = states::idle_task(&c.view, n, MERGED + 10 * MIN).map(|t| t.id.clone());
                let nudge = states::stall_message(&c.view, n, "David", MERGED + 10 * MIN);
                expect(
                    idle.as_deref() == Some(BETA) && n.state == NodeState::Stalled && nudge.is_some_and(|m| m.text.starts_with(states::NUDGE_PREFIX)),
                    format!("idle {idle:?}, node {:?}", n.state),
                )
            }),
            ("the node that still waits on PR #264 hears it was merged", |c| {
                c.merge();
                c.sweep(MERGED + 10 * MIN, false);
                let said = states::stale_wait_messages(&c.view, c.node(), "David", MERGED + 10 * MIN);
                let named = said.iter().any(|m| m.text.contains(PR264) && m.text.contains("was merged 10 min ago") && c.view.is_merge_notice(m));
                expect(said.len() == 1 && named, format!("{said:?}"))
            }),
        ];
        let mut failed = vec![];
        for (what, check) in rows {
            let mut c = Redraft::new();
            if let Err(why) = check(&mut c) {
                failed.push(format!("{what}: {why}"));
            }
        }
        assert!(failed.is_empty(), "{failed:#?}");
    }

    #[test]
    fn a_pr_the_node_names_without_a_card_is_pointed_out_once() {
        let mut c = Redraft::new();
        let words = "La PR https://github.com/acmecorp-private/Redraft/pull/265 con el número de build espera tu revisión.";
        c.on(STOPPED + 5000, hook("UserPromptSubmit", None)).on(STOPPED + 6000, hook("Stop", Some(words)));
        c.sweep(STOPPED + 10 * MIN, true);
        let said = states::stale_wait_messages(&c.view, c.node(), "David", STOPPED + 10 * MIN);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].text.starts_with("[Alondra] https://github.com/acmecorp-private/Redraft/pull/265, which your last message names, has no card"), "{}", said[0].text);
        c.on(STOPPED + 10 * MIN, Event::MessageQueued(said[0].clone()));
        assert!(states::stale_wait_messages(&c.view, c.node(), "David", STOPPED + 11 * MIN).is_empty(), "once");
    }

    #[test]
    fn pr_numbers_are_read_only_after_pr() {
        assert_eq!(pr_numbers_in("fusiones la PR #264; el PR 263 y PR#12, pero no #99 ni sprint 4 ni prueba 7"), [264, 263, 12]);
    }

    #[test]
    fn a_task_waiting_for_a_merge_goes_back_to_work_when_its_node_is_written_to() {
        let mut c = Redraft::new();
        c.sweep(STOPPED + 10 * MIN, true);
        assert_eq!(c.task(BETA).state, TaskState::WaitingDependency);
        for e in told(700, BETA) {
            c.on(STOPPED + 11 * MIN, e);
        }
        assert_eq!((c.task(BETA).state, c.task(BETA).awaiting), (TaskState::Running, None), "the orchestrator wrote to it about the beta");
    }

    #[test]
    fn the_log_replays_the_wait_and_the_merge() {
        let mut c = Redraft::new();
        c.sweep(STOPPED + 10 * MIN, true);
        c.merge();
        c.sweep(MERGED + 2000, true);
        let mut replayed = View::default();
        for (i, (at, e)) in c.log.iter().enumerate() {
            replayed.apply(i as u64 + 1, *at, e);
        }
        assert_eq!(replayed.board, c.view.board);
        let history: Vec<(&str, TaskState)> = c.task(BETA).history.iter().map(|h| (h.event.name(), h.to)).collect();
        assert_eq!(history, [("dependency_ready", TaskState::Running), ("awaits_merge", TaskState::WaitingDependency), ("dependency_ready", TaskState::Running)]);
    }
}
