use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::event::{DaemonObservation, Event, HookEvent};
use crate::model::{
    Answer, Millis, Node, NodeState, Question, QuestionOrigin, QuestionStatus, ReportKind, Signal, StateSource, ToolInFlight,
    TurnSignal,
};
use crate::model::{Message, MessageFrom, Report};
use crate::states::{self, Trigger};

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct View {
    pub seq: u64,
    pub nodes: BTreeMap<String, Node>,
    pub questions: BTreeMap<u64, Question>,
    pub reports: BTreeMap<u64, Report>,
    pub messages: BTreeMap<u64, Message>,
    #[serde(default)]
    pub removed: std::collections::BTreeSet<String>,
    #[serde(default)]
    pub machine_strained: bool,
    #[serde(default)]
    pub orchestrator_launched_at: Option<Millis>,
    #[serde(default)]
    pub prs: BTreeMap<u64, crate::pr::PullRequest>,
    /// The merge method last used in each repo, preselected on its next PR card.
    #[serde(default)]
    pub merge_methods: BTreeMap<String, crate::pr::MergeMethod>,
    /// Artifacts David approved, with when: a later report about one of them asks nothing of him.
    #[serde(default)]
    pub approved_artifacts: BTreeMap<String, Millis>,
    /// The options of each request for new work, by request id.
    #[serde(default)]
    pub requests: BTreeMap<String, crate::options::Resolved>,
    /// Names of nodes launched with auto-merge.
    #[serde(default)]
    pub auto_merge_nodes: std::collections::BTreeSet<String>,
    /// Requests and the tasks they became.
    #[serde(default)]
    pub board: crate::tasks::Board,
    #[serde(skip)]
    pub(crate) task_pass: crate::tasks::Pass,
}

const USAGE_LIMIT_ERRORS: &[&str] = &["rate_limit", "overloaded", "billing_error"];

/// A report that sends an artifact for David to review: a review card of its own.
fn is_review(r: &Report) -> bool {
    !crate::artifact::in_report(r).is_empty()
}

impl View {
    /// Every attachment copy an item Alondra still keeps refers to.
    pub fn stored_files(&self) -> std::collections::HashSet<std::path::PathBuf> {
        let questions = self.questions.values().flat_map(|q| &q.attachments);
        let reports = self.reports.values().flat_map(|r| &r.attachments);
        let messages = self.messages.values().flat_map(|m| &m.attachments);
        questions.chain(reports).chain(messages).filter_map(|a| a.stored_path.as_ref().map(std::path::PathBuf::from)).collect()
    }

    /// Backfills `created_at` on nodes loaded from a snapshot taken before that field existed
    /// (it deserializes as 0 there): the closest thing on record to when they first appeared is
    /// `state_since`. Called once, right after loading a snapshot and before replaying newer
    /// events (those already carry a real `created_at` from `Node::new`).
    pub fn backfill_created_at(&mut self) {
        for node in self.nodes.values_mut() {
            if node.created_at == 0 {
                node.created_at = node.state_since;
            }
        }
    }

    /// Forgets what nobody can act on any more: anything older than the retention window except
    /// the inbox (open questions, unread reports) and nodes Alondra can still watch. A node that
    /// only speaks through hooks has no process to watch, so once it has been silent for the whole
    /// window and leaves nothing in the inbox it is gone. An answer still undelivered after the
    /// window never will be: only nodes in the daemon can be woken to receive one.
    pub fn prune(&mut self, before: Millis) {
        let mut heard: std::collections::HashMap<&str, Millis> = std::collections::HashMap::new();
        let activity = self.questions.values().map(|q| (q.session_id.as_str(), q.asked_at))
            .chain(self.reports.values().map(|r| (r.session_id.as_str(), r.created_at)))
            .chain(self.messages.values().map(|m| (m.session_id.as_str(), m.created_at)));
        for (sid, at) in activity {
            heard.entry(sid).and_modify(|t| *t = (*t).max(at)).or_insert(at);
        }
        let in_inbox: std::collections::HashSet<&str> = self.questions.values().filter(|q| q.status.is_open()).map(|q| q.session_id.as_str())
            .chain(self.reports.values().filter(|r| !r.read).map(|r| r.session_id.as_str()))
            .collect();
        let gone: Vec<String> = self
            .nodes
            .values()
            .filter(|n| {
                let watched = n.in_daemon || n.is_orchestrator || (n.alive && n.pid.is_some());
                let last = heard.get(n.session_id.as_str()).map_or(n.last_seen, |t| (*t).max(n.last_seen));
                !watched && last < before && !in_inbox.contains(n.session_id.as_str())
            })
            .map(|n| n.session_id.clone())
            .collect();
        for sid in &gone {
            self.nodes.remove(sid);
        }
        self.questions.retain(|_, q| q.status.is_open() || q.asked_at >= before);
        self.reports.retain(|_, r| !(r.read && r.created_at < before));
        self.nodes.retain(|_, n| n.alive || n.in_daemon || n.last_seen >= before);
        self.messages.retain(|_, m| m.created_at >= before);
        self.approved_artifacts.retain(|_, at| *at >= before);
        let keep = self.tasks_to_keep();
        self.board.prune(before, |t| keep.contains(&t.id));
    }

    pub fn apply(&mut self, seq: u64, at: Millis, event: &Event) {
        self.seq = seq;
        if let Some(sid) = event.session_id()
            && self.removed.contains(sid)
        {
            return;
        }
        let before = self.before_tasks(event);
        let touched = match event {
            Event::Hook(h) => {
                self.apply_hook(at, h);
                Some((h.session_id.clone(), Trigger::Hook))
            }
            Event::DaemonSeen(d) => {
                self.apply_daemon(at, d);
                Some((d.session_id.clone(), Trigger::Daemon))
            }
            Event::ProcessExited { session_id, pid } => {
                let node = self.node_mut(session_id, at);
                if node.pid == Some(*pid) || node.pid.is_none() {
                    node.alive = false;
                    node.busy = false;
                    node.tool = None;
                    node.waiting_permission = false;
                }
                for q in self.questions.values_mut() {
                    if &q.session_id == session_id
                        && q.status.is_open()
                        && q.origin != QuestionOrigin::Ask
                    {
                        q.status = QuestionStatus::Orphaned;
                    }
                }
                Some((session_id.clone(), Trigger::ProcessExit))
            }
            Event::NodeRemoved { session_id } => {
                self.nodes.remove(session_id);
                self.removed.insert(session_id.clone());
                for q in self.questions.values_mut() {
                    if &q.session_id == session_id && q.status.is_open() {
                        q.status = QuestionStatus::Orphaned;
                    }
                }
                None
            }
            Event::Acknowledged { session_id } => {
                if let Some(n) = self.nodes.get_mut(session_id) {
                    n.acknowledged_at = Some(at);
                }
                None
            }
            Event::MarkedOrchestrator { session_id } => {
                self.node_mut(session_id, at).is_orchestrator = true;
                Some((session_id.clone(), Trigger::Other))
            }
            Event::QuestionAsked(q) => {
                let node = self.node_mut(&q.session_id, at);
                if q.blocking && q.origin == QuestionOrigin::Ask {
                    node.turn_signal = Some(TurnSignal::Asked);
                    node.signal_at = Some(at);
                }
                let mut q = q.clone();
                if q.node_name.is_none() {
                    q.node_name = node.name.clone();
                }
                let sid = q.session_id.clone();
                self.questions.insert(q.id, q);
                Some((sid, Trigger::Question))
            }
            Event::QuestionAnswered { id, answer } => self.question_update(*id, |q| {
                if q.status.is_open() {
                    q.status = QuestionStatus::Answered;
                    q.answer = Some(answer.clone());
                }
            }),
            Event::QuestionDelivered { id } => self.question_update(*id, |q| {
                if q.status == QuestionStatus::Answered {
                    q.status = QuestionStatus::Delivered;
                }
            }),
            Event::QuestionClosed { id, status, note } => self.question_update(*id, |q| {
                q.status = *status;
                if let Some(note) = note {
                    q.answer = Some(Answer { text: note.clone(), option: None, by: None, device: None, at });
                }
            }),
            Event::ReportPosted(r) => {
                let settled = self.about_settled_pr(r) || self.about_approved_artifact(r);
                let node = self.node_mut(&r.session_id, at);
                if settled {
                    node.acknowledged_at = Some(at);
                }
                let signal = match r.kind {
                    ReportKind::Final => Some(TurnSignal::Finished),
                    ReportKind::Notice | ReportKind::Document => Some(TurnSignal::Delivered),
                    ReportKind::Progress => None,
                };
                if let Some(signal) = signal {
                    node.turn_signal = Some(signal);
                    node.signal_at = Some(at);
                }
                let mut r = r.clone();
                if r.node_name.is_none() {
                    r.node_name = node.name.clone();
                }
                r.read |= settled;
                let sid = r.session_id.clone();
                self.reports.insert(r.id, r);
                Some((sid, Trigger::Report))
            }
            Event::ReportRead { id } => self.read_report(*id, at),
            Event::ArtifactReviewed { report, verdict } => {
                if *verdict == crate::event::ReviewVerdict::Approved
                    && let Some(r) = self.reports.get(report)
                {
                    for url in crate::artifact::in_report(r) {
                        self.approved_artifacts.insert(url, at);
                    }
                }
                self.read_report(*report, at)
            }
            Event::MessageQueued(m) => {
                self.messages.insert(m.id, m.clone());
                Some((m.session_id.clone(), Trigger::Message))
            }
            Event::MessageHanded { id } => {
                let reopens = self.messages.get(id).is_some_and(|m| m.from != MessageFrom::Alondra || self.is_merge_notice(m));
                let Some(m) = self.messages.get_mut(id) else { return };
                m.handed_at = Some(at);
                let (sid, from_someone) = (m.session_id.clone(), reopens);
                if let Some(node) = self.nodes.get_mut(&sid) {
                    node.handed_at = Some(at);
                    node.last_seen = node.last_seen.max(at);
                    if from_someone {
                        node.begin_episode(at);
                    }
                }
                Some((sid, Trigger::Message))
            }
            Event::Classified { session_id, turn, state, confidence } => {
                let node = self.node_mut(session_id, at);
                if node.turn == *turn {
                    node.classified = Some(crate::model::Classification {
                        turn: *turn,
                        state: *state,
                        confidence: *confidence,
                    });
                }
                Some((session_id.clone(), Trigger::Other))
            }
            Event::WaitingMachine { session_id, .. } => {
                let node = self.node_mut(session_id, at);
                node.turn_signal = Some(TurnSignal::WaitingMachine);
                node.signal_at = Some(at);
                Some((session_id.clone(), Trigger::Declaration))
            }
            Event::Pruned { before } => {
                self.prune(*before);
                None
            }
            Event::MenuSeen { session_id, waiting_for } => {
                self.node_mut(session_id, at).menu_waiting = waiting_for.clone();
                Some((session_id.clone(), Trigger::Daemon))
            }
            Event::AttachChanged { session_id, attached } => {
                self.node_mut(session_id, at).attached = *attached;
                Some((session_id.clone(), Trigger::Other))
            }
            Event::PrOpened { url, session_id, .. } => {
                let known = crate::pr::parse_url(url).is_some_and(|(repo, number)| self.prs.values().any(|p| p.repo == repo && p.number == number));
                if !known {
                    let id = self.prs.keys().next_back().copied().unwrap_or(0) + 1;
                    let name = self.nodes.get(session_id).and_then(|n| n.name.clone());
                    let auto_merge = name.as_ref().is_some_and(|n| self.auto_merge_nodes.contains(n));
                    if let Some(mut pr) = crate::pr::PullRequest::opened(id, url, session_id, name, at) {
                        pr.auto_merge = auto_merge;
                        self.prs.insert(id, pr);
                    }
                }
                self.nodes.contains_key(session_id).then(|| (session_id.clone(), Trigger::Other))
            }
            Event::PrUpdated(pr) => {
                let settled = matches!(pr.state, crate::pr::PrState::Merged | crate::pr::PrState::Closed);
                if settled {
                    self.read_reports_in(pr.id, at);
                }
                if let Some(known) = self.prs.get_mut(&pr.id) {
                    let dismissed = known.state == crate::pr::PrState::Dismissed && pr.state == crate::pr::PrState::Open;
                    let requested = known.changes_requested_on.take();
                    let (auto_merge, task) = (known.auto_merge, known.task.take());
                    *known = pr.clone();
                    known.changes_requested_on = requested;
                    known.auto_merge = auto_merge;
                    known.task = task.or(known.task.take());
                    if dismissed {
                        known.state = crate::pr::PrState::Dismissed;
                    }
                }
                if settled {
                    self.withdraw_questions_about(pr.id);
                }
                self.pr_node(pr.id)
            }
            Event::PrMerged { id, method } => {
                self.read_reports_in(*id, at);
                if let Some(pr) = self.prs.get_mut(id) {
                    pr.state = crate::pr::PrState::Merged;
                    self.merge_methods.insert(pr.repo.clone(), *method);
                }
                self.withdraw_questions_about(*id);
                self.pr_node(*id)
            }
            Event::PrDismissed { id } => {
                self.read_reports_in(*id, at);
                if let Some(pr) = self.prs.get_mut(id) {
                    pr.state = crate::pr::PrState::Dismissed;
                }
                self.pr_node(*id)
            }
            Event::PrChangesRequested { id } => {
                if let Some(pr) = self.prs.get_mut(id) {
                    pr.changes_requested_on = Some(pr.head_sha.clone().unwrap_or_default());
                }
                self.pr_node(*id)
            }
            Event::PrAsked { id } => self.pr_node(*id),
            Event::TasksRecorded | Event::TaskCreated(_) | Event::TaskAssigned { .. } | Event::TaskMoved { .. } => None,
            Event::RequestSent { request_id, options, .. } => {
                self.requests.insert(request_id.clone(), options.clone());
                None
            }
            Event::Launched { name, auto_merge, .. } => {
                if *auto_merge {
                    self.auto_merge_nodes.insert(name.clone());
                } else {
                    self.auto_merge_nodes.remove(name);
                }
                None
            }
            Event::OrchestratorLaunched { .. } => {
                self.orchestrator_launched_at = Some(at);
                None
            }
            Event::Clock { machine_strained } => {
                self.machine_strained = *machine_strained;
                let sessions: Vec<String> = self.nodes.keys().cloned().collect();
                for sid in sessions {
                    self.recompute(&sid, at, Trigger::Clock);
                }
                debug_assert!(states::limbo(self, at).is_empty(), "nodes in limbo after a sweep: {:?}", states::limbo(self, at));
                None
            }
        };
        if let Some((sid, trigger)) = touched {
            self.recompute(&sid, at, trigger);
        }
        self.derive_tasks(at, event, before);
    }

    /// Whether time alone has changed what some node shows, so a sweep at `now` has to record
    /// an `Event::Clock`.
    pub fn due(&self, now: Millis, machine_strained: bool) -> bool {
        self.nodes.values().any(|n| {
            let o = states::evaluate_with(self, n, now, Trigger::Clock, machine_strained);
            o.state != n.state || o.orchestrator != n.orchestrator_state || o.drop_tool
        })
    }

    /// A change to a PR card can change its node's state: the card is a decision waiting for the user.
    fn pr_node(&self, id: u64) -> Option<(String, Trigger)> {
        let sid = &self.prs.get(&id)?.session_id;
        self.nodes.contains_key(sid).then(|| (sid.clone(), Trigger::Other))
    }

    /// Once a PR is merged or closed, the open questions of its task that link to it have nothing
    /// left to decide. Another task's cards are never touched by it (#78 withdrew action #217).
    fn withdraw_questions_about(&mut self, pr_id: u64) {
        let Some(pr) = self.prs.get(&pr_id) else { return };
        let target = (pr.repo.clone(), pr.number);
        let task = self.pr_task_of(pr).map(|t| t.id.clone());
        let moot: Vec<u64> = self
            .questions
            .values()
            .filter(|q| q.status == QuestionStatus::Open && !q.action && q.links.iter().any(|l| crate::pr::parse_url(&l.url).as_ref() == Some(&target)))
            .filter(|q| task.is_some() && self.question_task(q).map(|t| t.id.clone()) == task)
            .map(|q| q.id)
            .collect();
        for id in moot {
            if let Some(q) = self.questions.get_mut(&id) {
                q.status = QuestionStatus::Withdrawn;
            }
        }
    }

    /// A PR card that leaves for good (merged, closed or set aside) takes the reports inside it
    /// along: they count as read, so their node is reviewed.
    fn read_reports_in(&mut self, pr_id: u64, at: Millis) {
        let inside: Vec<u64> = self.reports.values().filter(|r| !r.read && self.report_pr(r) == Some(pr_id)).map(|r| r.id).collect();
        for id in inside {
            let Some(r) = self.reports.get_mut(&id) else { continue };
            r.read = true;
            let sid = r.session_id.clone();
            if let Some(n) = self.nodes.get_mut(&sid) {
                n.acknowledged_at = Some(at);
                self.recompute(&sid, at, Trigger::Report);
            }
        }
    }

    /// Reading a report reads the earlier ones stacked in its card, and the user has now seen what
    /// its node delivered.
    fn read_report(&mut self, id: u64, at: Millis) -> Option<(String, Trigger)> {
        let earlier = self.reports.get(&id).map(|r| self.report_stack(r)).unwrap_or_default();
        for e in earlier {
            if let Some(r) = self.reports.get_mut(&e) {
                r.read = true;
            }
        }
        let session = self.reports.get_mut(&id).map(|r| {
            r.read = true;
            r.session_id.clone()
        })?;
        let node = self.nodes.get_mut(&session)?;
        node.acknowledged_at = Some(at);
        Some((session, Trigger::Report))
    }

    fn question_update(&mut self, id: u64, f: impl FnOnce(&mut Question)) -> Option<(String, Trigger)> {
        let q = self.questions.get_mut(&id)?;
        f(q);
        let (sid, open, tool_use_id, origin) = (q.session_id.clone(), q.status.is_open(), q.tool_use_id.clone(), q.origin);
        if !open && let Some(node) = self.nodes.get_mut(&sid) {
            if tool_use_id.is_some() && node.tool.as_ref().is_some_and(|t| t.tool_use_id == tool_use_id) {
                node.tool = None;
            }
            if origin == QuestionOrigin::Elicitation {
                node.waiting_permission = false;
            }
        }
        Some((sid, Trigger::Question))
    }

    fn node_mut(&mut self, session_id: &str, at: Millis) -> &mut Node {
        self.nodes
            .entry(session_id.to_string())
            .or_insert_with(|| Node::new(session_id, at))
    }

    fn apply_hook(&mut self, at: Millis, h: &HookEvent) {
        let handed: Vec<u64> = self
            .messages
            .values()
            .filter(|m| m.session_id == h.session_id && m.handed_at.is_some() && m.delivered_at.is_none())
            .map(|m| m.id)
            .collect();
        let starts_work = matches!(h.event.as_str(), "UserPromptSubmit" | "PreToolUse");
        if starts_work {
            for id in &handed {
                if let Some(m) = self.messages.get_mut(id) {
                    m.delivered_at = Some(at);
                }
            }
        }
        let node = self.node_mut(&h.session_id, at);
        if starts_work || h.event == "PostToolUse" {
            node.menu_waiting = None;
        }
        node.has_plugin = true;
        node.alive = true;
        node.last_seen = at;
        if h.cwd.is_some() && (!node.in_daemon || node.cwd.is_none()) {
            node.cwd = h.cwd.clone();
        }
        if matches!(h.event.as_str(), "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PostToolUseFailure") {
            node.last_work_at = Some(at);
        }
        match h.event.as_str() {
            "SessionStart" => {
                node.busy = false;
                node.tool = None;
            }
            "UserPromptSubmit" => {
                node.turn_since = at;
                if h.prompt.is_some() {
                    node.last_prompt = h.prompt.clone();
                    node.last_prompt_at = Some(at);
                }
                node.busy = true;
                node.turn += 1;
                node.classified = None;
                node.usage_limited = false;
                node.waiting_permission = false;
            }
            "PreToolUse" => {
                node.busy = true;
                node.usage_limited = false;
                if h.agent_id.is_none() {
                    node.tool = Some(ToolInFlight {
                        tool_use_id: h.tool_use_id.clone(),
                        name: h.tool_name.clone().unwrap_or_default(),
                        summary: h.tool_summary.clone(),
                        since: at,
                    });
                }
            }
            "PostToolUse" | "PostToolUseFailure" => {
                node.waiting_permission = false;
                let same = match (&node.tool, &h.tool_use_id) {
                    (Some(t), Some(id)) => t.tool_use_id.as_deref() == Some(id.as_str()),
                    (Some(_), None) => true,
                    _ => false,
                };
                if same {
                    node.tool = None;
                }
                if let Some(id) = &h.tool_use_id {
                    for q in self.questions.values_mut() {
                        if q.tool_use_id.as_deref() == Some(id.as_str()) && q.status.is_open() {
                            q.status = QuestionStatus::AnsweredInTerminal;
                        }
                    }
                }
            }
            "PermissionRequest" | "Elicitation" => node.waiting_permission = true,
            "Notification" => match h.notification_type.as_deref() {
                Some("permission_prompt" | "elicitation_dialog" | "elicitation_url_dialog") => node.waiting_permission = true,
                Some("elicitation_response") => node.waiting_permission = false,
                _ => {}
            },
            "Stop" => {
                node.busy = false;
                node.tool = None;
                node.waiting_permission = false;
                node.last_stop_at = Some(at);
                if h.last_message.is_some() {
                    node.last_message = h.last_message.clone();
                    node.last_message_at = Some(at);
                }
            }
            "StopFailure" => {
                node.busy = false;
                node.tool = None;
                node.last_stop_at = Some(at);
                if h.error_type
                    .as_deref()
                    .is_some_and(|e| USAGE_LIMIT_ERRORS.contains(&e))
                {
                    node.usage_limited = true;
                    node.usage_limited_at = Some(at);
                }
            }
            _ => {}
        }
    }

    fn apply_daemon(&mut self, at: Millis, d: &DaemonObservation) {
        let node = self.node_mut(&d.session_id, at);
        node.short_id = d.short_id.clone();
        if d.name.is_some() {
            node.name = d.name.clone();
        }
        if d.cwd.is_some() {
            node.cwd = d.cwd.clone();
        }
        node.in_daemon = true;
        node.alive = d.pid.is_some();
        if d.pid.is_some() {
            node.pid = d.pid;
        }
        if !node.has_plugin && (node.daemon_detail != d.detail || node.daemon_needs != d.needs) {
            node.classified = None;
        }
        let woke = d.tempo.as_deref() == Some("active") && node.daemon_tempo.as_deref() != Some("active");
        node.daemon_detail = d.detail.clone();
        node.daemon_needs = d.needs.clone();
        node.daemon_updated_at = d.updated_at;
        node.daemon_state = d.state.clone();
        node.daemon_tempo = d.tempo.clone();
        let tasks = d.tasks.unwrap_or(0);
        if tasks != node.daemon_tasks || node.daemon_tasks_since.is_none() {
            node.daemon_tasks_since = Some(at);
        }
        node.daemon_tasks = tasks;
        node.background = if node.daemon_tasks == 0 { vec![] } else { d.background.clone() };
        node.daemon_seen_at = Some(at);
        if d.intent.is_some() {
            node.mission = d.intent.clone();
        }
        if !node.has_plugin && d.last_prompt.is_some() {
            node.last_prompt = d.last_prompt.clone();
        }
        if !node.has_plugin && d.last_text.is_some() {
            node.last_message = d.last_text.clone();
            node.last_message_at = d.last_text_at.or(d.updated_at);
        }
        if !node.has_plugin {
            node.last_seen = node.last_seen.max(d.updated_at.unwrap_or(at));
            node.usage_limited = false;
            if woke {
                node.turn += 1;
                node.turn_since = at;
                node.begin_episode(at);
            }
        }
    }

    fn recompute(&mut self, session_id: &str, at: Millis, trigger: Trigger) {
        let Some(node) = self.nodes.get(session_id) else { return };
        let outcome = states::evaluate(self, node, at, trigger);
        let Some(node) = self.nodes.get_mut(session_id) else { return };
        if outcome.drop_tool {
            node.tool = None;
        }
        let source = match outcome.evidence.signal {
            Signal::DaemonActive | Signal::BackgroundTask => StateSource::Daemon,
            _ if node.has_plugin => StateSource::Hooks,
            _ => StateSource::Daemon,
        };
        if outcome.state != node.state {
            node.state = outcome.state;
            if outcome.state == NodeState::Done {
                node.done_at = Some(at);
            }
            node.state_since = match source {
                StateSource::Daemon if !node.has_plugin => node.daemon_updated_at.unwrap_or(at).min(at),
                _ => at,
            };
        }
        node.state_source = source;
        node.orchestrator_state = outcome.orchestrator;
        node.evidence = Some(outcome.evidence);
    }

    pub fn news(&self, now: Millis) -> Vec<&Node> {
        let mut v: Vec<&Node> = self.nodes.values().filter(|n| n.has_news(now)).collect();
        v.sort_by_key(|n| std::cmp::Reverse(n.last_message_at.unwrap_or(n.state_since).max(n.state_since)));
        v
    }

    /// Open PR cards the user has to act on: not merged, closed, set aside or waiting for new commits.
    pub fn prs_awaiting_user(&self) -> impl Iterator<Item = &crate::pr::PullRequest> {
        self.prs.values().filter(|p| p.state == crate::pr::PrState::Open && !p.awaiting_push())
    }

    /// The open PR card that is home to a task's cards: the one of the task's PRs the card links,
    /// else the task's newest. PRs Alondra merges on its own have no card. `parked` counts a card
    /// waiting for the node's next push.
    fn task_pr_home(&self, task: &crate::tasks::Task, links: &[&str], parked: bool) -> Option<u64> {
        let homes: Vec<&crate::pr::PullRequest> = task
            .prs
            .iter()
            .filter_map(|id| self.prs.get(id))
            .filter(|p| p.state == crate::pr::PrState::Open && !p.auto_merge && (parked || !p.awaiting_push()))
            .collect();
        let linked = links.iter().filter_map(|u| crate::pr::parse_url(u)).find_map(|(repo, number)| homes.iter().find(|p| p.repo == repo && p.number == number));
        linked.or_else(|| homes.iter().max_by_key(|p| p.id)).map(|p| p.id)
    }

    /// The PR card a question of the node shows in: its task's open PR card. Questions Claude Code
    /// itself raised (permissions, plans, menus) and questions for the orchestrator stay cards of
    /// their own.
    pub fn question_pr(&self, q: &Question) -> Option<u64> {
        if q.origin != QuestionOrigin::Ask || q.to_orchestrator {
            return None;
        }
        let links: Vec<&str> = q.links.iter().map(|l| l.url.as_str()).collect();
        self.task_pr_home(self.question_task(q)?, &links, false)
    }

    /// The report links a PR the user already merged or closed on its card, and no open one: it
    /// is filed read, as merging reads the reports inside the card. PR #61, 24 Sep: the node's
    /// "deployed" report after the merge stayed as a card of its own asking for David.
    fn about_settled_pr(&self, r: &Report) -> bool {
        let settled = |repo: &str, number: u64| {
            self.prs.values().any(|p| p.repo == repo && p.number == number && matches!(p.state, crate::pr::PrState::Merged | crate::pr::PrState::Closed))
        };
        let texts = r.links.iter().map(|l| l.url.as_str()).chain([r.title.as_str(), r.body.as_deref().unwrap_or("")]);
        self.report_pr(r).is_none() && texts.flat_map(crate::pr::urls_in).filter_map(|u| crate::pr::parse_url(&u)).any(|(repo, number)| settled(&repo, number))
    }

    /// Every artifact the report links was already approved by David: it is filed read, as the
    /// approval already answered it.
    fn about_approved_artifact(&self, r: &Report) -> bool {
        let artifacts = crate::artifact::in_report(r);
        !artifacts.is_empty() && artifacts.iter().all(|url| self.approved_artifacts.contains_key(url))
    }

    /// Unread reports that send an artifact for David to review. Each is a review card of its
    /// own: its node waits until David approves it or sends it his comments, and nothing but that
    /// verdict takes it out of the inbox.
    pub fn reviews_awaiting_user(&self) -> impl Iterator<Item = &Report> {
        self.reports.values().filter(|r| self.awaits_review(r))
    }

    pub fn awaits_review(&self, r: &Report) -> bool {
        !r.read && r.kind != ReportKind::Progress && is_review(r)
    }

    /// David's verdict on an artifact, still on its way to the node: it waits for the review until
    /// the verdict lands.
    pub fn review_reply_due(&self, session_id: &str) -> Option<Millis> {
        self.pending_messages(session_id).filter(|m| matches!(m.about, Some(crate::model::MessageAbout::ArtifactReview { .. }))).map(|m| m.created_at).max()
    }

    /// The PR card a report shows in: its task's open PR card, also while that card waits for
    /// new commits. A review card is never folded into one.
    pub fn report_pr(&self, r: &Report) -> Option<u64> {
        if is_review(r) {
            return None;
        }
        let links: Vec<String> = r.links.iter().map(|l| l.url.clone()).chain([r.title.as_str(), r.body.as_deref().unwrap_or("")].into_iter().flat_map(crate::pr::urls_in)).collect();
        let links: Vec<&str> = links.iter().map(String::as_str).collect();
        self.task_pr_home(self.report_task(r)?, &links, true)
    }

    /// Unread reports that reach the inbox: not progress, not carried by a question or another
    /// report, and not inside a PR card that waits off the inbox for new commits.
    pub fn inbox_reports(&self) -> impl Iterator<Item = &Report> {
        let absorbed = self.absorbed_reports();
        let stacked = self.stacked_reports();
        self.reports.values().rev().filter(move |r| {
            let parked = self.report_pr(r).is_some_and(|id| self.prs.get(&id).is_some_and(|p| p.awaiting_push()));
            !r.read && !r.to_orchestrator && r.kind != ReportKind::Progress && !absorbed.contains(&r.id) && !stacked.contains(&r.id) && !parked
        })
    }

    /// A report of a task another card can carry: unread, not progress, not for the orchestrator,
    /// not a review card and not in a PR card.
    fn carriable(&self, r: &Report) -> Option<String> {
        let carriable = !r.read && !r.to_orchestrator && r.kind != ReportKind::Progress && !is_review(r) && self.report_pr(r).is_none();
        carriable.then(|| self.report_task(r).map(|t| t.id.clone())).flatten()
    }

    /// The question that carries a task's reports: its blocker when the node asked it, else its
    /// newest open question of the node's own. Actions keep their card as it is.
    fn carrier(&self, task: &crate::tasks::Task) -> Option<u64> {
        let own = |q: &Question| q.status.is_open() && q.origin == QuestionOrigin::Ask && !q.action && !q.to_orchestrator && self.question_task(q).is_some_and(|t| t.id == task.id);
        task.blocker
            .and_then(|b| self.questions.get(&b.question))
            .filter(|q| own(q))
            .or_else(|| self.questions.values().rev().find(|q| own(q)))
            .map(|q| q.id)
    }

    /// The reports an open question carries: when it is the one that speaks for its task, every
    /// unread report of the task outside its PR card and its review cards. Answering it reads
    /// them.
    pub fn question_reports(&self, q: &Question) -> Vec<u64> {
        let Some(task) = self.question_task(q).filter(|t| self.carrier(t) == Some(q.id)) else { return vec![] };
        self.reports.values().filter(|r| self.carriable(r).as_deref() == Some(task.id.as_str())).map(|r| r.id).collect()
    }

    /// The unread reports the head of a task's stack carries, oldest first, when `r` is that
    /// head: the task's other reports no question carries. The head is the newest document, or
    /// the newest report when there is none, so a document stays a card of its own and a short
    /// notice never hides it.
    pub fn report_stack(&self, r: &Report) -> Vec<u64> {
        self.stack_beside(r, &self.absorbed_reports())
    }

    fn stack_beside(&self, r: &Report, absorbed: &std::collections::HashSet<u64>) -> Vec<u64> {
        let Some(task) = self.carriable(r).filter(|_| !absorbed.contains(&r.id)) else { return vec![] };
        let mut group: Vec<u64> = self.reports.values().filter(|x| !absorbed.contains(&x.id) && self.carriable(x).as_deref() == Some(task.as_str())).map(|x| x.id).collect();
        let document = |id: &&u64| self.reports[*id].kind == ReportKind::Document;
        let head = group.iter().filter(document).max().or(group.iter().max());
        if head != Some(&r.id) {
            return vec![];
        }
        group.retain(|id| *id != r.id);
        group
    }

    /// Reports carried by the head of their task's stack: they leave the inbox with it.
    pub fn stacked_reports(&self) -> std::collections::HashSet<u64> {
        let absorbed = self.absorbed_reports();
        self.reports.values().filter(|r| !r.read && r.kind != ReportKind::Progress).flat_map(|r| self.stack_beside(r, &absorbed)).collect()
    }

    /// Reports some open question has taken in: they leave the inbox while it is open.
    pub fn absorbed_reports(&self) -> std::collections::HashSet<u64> {
        self.open_questions().flat_map(|q| self.question_reports(q)).collect()
    }

    pub fn open_questions(&self) -> impl Iterator<Item = &Question> {
        self.questions.values().filter(|q| q.status.is_open())
    }

    pub fn pending_messages(&self, session_id: &str) -> impl Iterator<Item = &Message> {
        self.messages
            .values()
            .filter(move |m| m.session_id == session_id && m.handed_at.is_none())
    }

    /// Done is final for Alondra: only David or the orchestrator, writing on purpose, reopen a
    /// done node. Alondra itself never types into one.
    pub fn may_alondra_write(&self, node: &Node) -> bool {
        node.is_orchestrator || node.state != NodeState::Done
    }

    /// The only way Alondra writes to a session: nothing when the node is done or unknown.
    pub fn alondra_message(&self, session_id: &str, text: String, at: Millis) -> Option<Message> {
        let node = self.nodes.get(session_id)?;
        self.may_alondra_write(node).then(|| Message {
            id: self.messages.keys().next_back().copied().unwrap_or(0) + 1,
            session_id: session_id.to_string(),
            from: MessageFrom::Alondra,
            text,
            answers_question: None,
            created_at: at,
            handed_at: None,
            delivered_at: None,
            about: None,
            attachments: vec![],
        })
    }

    /// What may be typed into the session now: everything from David or the orchestrator, and
    /// what Alondra queued only while the node may take it and was not done since.
    pub fn deliverable_messages(&self, session_id: &str) -> impl Iterator<Item = &Message> {
        let node = self.nodes.get(session_id);
        self.pending_messages(session_id).filter(move |m| {
            m.from != MessageFrom::Alondra || node.is_some_and(|n| self.may_alondra_write(n) && n.done_at.is_none_or(|done| m.created_at > done))
        })
    }

    /// Alondra's notice that a PR the node opened was merged: the node still has to act on it.
    pub fn is_merge_notice(&self, m: &Message) -> bool {
        let Some(crate::model::MessageAbout::PrMerged { pr_id }) = m.about else { return false };
        self.prs.get(&pr_id).is_some_and(|p| p.session_id == m.session_id && p.state == crate::pr::PrState::Merged)
    }

    /// A merge notice still on its way to the node: it waits for the merge until the notice lands.
    pub fn merge_notice_due(&self, session_id: &str) -> Option<Millis> {
        self.pending_messages(session_id).filter(|m| self.is_merge_notice(m)).map(|m| m.created_at).max()
    }

    /// Messages waiting for an interactive session: Alondra cannot type into a terminal it did not
    /// start, so they wait for the session's next Stop hook (see `turn_end_delivery`).
    pub fn held_for_turn_end(&self, session_id: &str) -> usize {
        match self.nodes.get(session_id) {
            Some(n) if !n.in_daemon => self.deliverable_messages(session_id).count(),
            _ => 0,
        }
    }

    /// What an interactive session's Stop hook hands it: the ids of its pending messages (with the
    /// question each one answers) and the text that keeps its turn going. Background jobs get
    /// their messages typed in by `wake` instead, so they get nothing here.
    pub fn turn_end_delivery(&self, session_id: &str, user: &str) -> Option<(Vec<(u64, Option<u64>)>, String)> {
        if self.nodes.get(session_id)?.in_daemon {
            return None;
        }
        let pending: Vec<&Message> = self.deliverable_messages(session_id).collect();
        if pending.is_empty() {
            return None;
        }
        let said = pending
            .iter()
            .map(|m| {
                let from = match m.from {
                    crate::model::MessageFrom::User => user,
                    crate::model::MessageFrom::Orchestrator => "the orchestrator",
                    crate::model::MessageFrom::Alondra => "Alondra",
                };
                format!("From {from}:\n{}", m.text)
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let text = format!("Messages sent to you through Alondra while you worked. Act on them before you stop:\n\n{said}");
        Some((pending.iter().map(|m| (m.id, m.answers_question)).collect(), text))
    }

    pub fn resolve_node(&self, key: &str) -> Option<&Node> {
        if let Some(n) = self.nodes.get(key) {
            return Some(n);
        }
        let by_short: Vec<&Node> = self.nodes.values().filter(|n| n.short_id == key).collect();
        if by_short.len() == 1 {
            return Some(by_short[0]);
        }
        let mut by_name: Vec<&Node> = self
            .nodes
            .values()
            .filter(|n| n.name.as_deref() == Some(key))
            .collect();
        by_name.sort_by_key(|n| std::cmp::Reverse((n.alive, n.last_seen)));
        by_name.first().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Answer, NodeState, OrchestratorState, QuestionOrigin};

    /// A node from a snapshot older than `created_at` gets `state_since` as a stand-in, so it
    /// still sorts as older than anything created after the upgrade; a node that already has a
    /// real `created_at` is left alone.
    #[test]
    fn backfill_created_at_only_touches_nodes_that_never_had_one() {
        let mut v = View::default();
        let mut old = Node::new("old", 500);
        old.created_at = 0; // as if deserialized from a pre-migration snapshot
        old.state_since = 800;
        let mut migrated_already = Node::new("already", 200);
        migrated_already.state_since = 900; // later state change; created_at must not follow it
        v.nodes.insert(old.session_id.clone(), old);
        v.nodes.insert(migrated_already.session_id.clone(), migrated_already);
        v.backfill_created_at();
        assert_eq!(v.nodes["old"].created_at, 800);
        assert_eq!(v.nodes["already"].created_at, 200);
    }

    fn hook(sid: &str, event: &str) -> Event {
        Event::Hook(HookEvent { session_id: sid.into(), event: event.into(), ..Default::default() })
    }

    fn question(id: u64, sid: &str, origin: QuestionOrigin) -> Question {
        Question {
            related: vec![],
            id,
            session_id: sid.into(),
            node_name: None,
            title: "Merge it?".into(),
            detail: None,
            options: vec![],
            links: vec![],
            attachments: vec![],
            blocking: true,
            origin,
            tool_use_id: None,
            to_orchestrator: false,
            node_context: None,
            status: QuestionStatus::Open,
            answer: None,
            asked_at: 0,
            action: false,
            steps: vec![],
            mission: None,
            replaces: None,
            task: None,
        }
    }

    fn run(events: &[Event]) -> View {
        let mut v = View::default();
        for (i, e) in events.iter().enumerate() {
            v.apply(i as u64 + 1, i as i64 * 1000, e);
        }
        v
    }

    #[test]
    fn working_while_in_turn() {
        let v = run(&[hook("s", "UserPromptSubmit")]);
        assert_eq!(v.nodes["s"].state, NodeState::Working);
    }

    #[test]
    fn open_question_means_waiting_answer_until_answered() {
        let v = run(&[
            hook("s", "UserPromptSubmit"),
            Event::QuestionAsked(question(1, "s", QuestionOrigin::Ask)),
            hook("s", "Stop"),
        ]);
        assert_eq!(v.nodes["s"].state, NodeState::WaitingAnswer);
        let mut v = v;
        v.apply(9, 9000, &Event::QuestionAnswered {
            id: 1,
            answer: Answer { text: "yes".into(), option: None, by: None, device: None, at: 9000 },
        });
        assert_ne!(v.nodes["s"].state, NodeState::WaitingAnswer);
    }

    /// Closing a node withdraws its own open question instead of the close refusing over it
    /// (91933d17, 23 Sep): the note explains why to anyone reading the question later.
    #[test]
    fn withdrawing_a_question_with_a_note_records_it_as_the_answer() {
        let mut v = run(&[hook("s", "UserPromptSubmit"), Event::QuestionAsked(question(1, "s", QuestionOrigin::Ask)), hook("s", "Stop")]);
        assert_eq!(v.nodes["s"].state, NodeState::WaitingAnswer);
        v.apply(9, 9000, &Event::QuestionClosed { id: 1, status: QuestionStatus::Withdrawn, note: Some("the user closed the node".into()) });
        assert_eq!(v.questions[&1].status, QuestionStatus::Withdrawn);
        assert_eq!(v.questions[&1].answer.as_ref().map(|a| a.text.as_str()), Some("the user closed the node"));
        assert_ne!(v.nodes["s"].state, NodeState::WaitingAnswer);
    }

    #[test]
    fn turn_without_signal_is_unclear_not_frozen_working() {
        let v = run(&[hook("s", "UserPromptSubmit"), hook("s", "Stop")]);
        assert_eq!(v.nodes["s"].state, NodeState::Unclear);
    }

    #[test]
    fn process_exit_ends_working_but_node_stays_listed() {
        let mut v = run(&[hook("s", "UserPromptSubmit"), hook("s", "PreToolUse")]);
        v.nodes.get_mut("s").unwrap().pid = Some(42);
        v.apply(3, 3000, &Event::ProcessExited { session_id: "s".into(), pid: 42 });
        assert!(!v.nodes["s"].alive);
        assert_ne!(v.nodes["s"].state, NodeState::Working);
    }

    #[test]
    fn a_report_about_a_pr_already_merged_or_closed_is_filed_read() {
        use crate::pr::MergeMethod;
        let report = |id: u64, url: &str| {
            Event::ReportPosted(Report {
                related: vec![],
                id,
                session_id: "s".into(),
                node_name: None,
                title: "PR #61 deployed".into(),
                body: None,
                links: vec![crate::model::Link { url: url.into(), title: None }],
                attachments: vec![],
                kind: ReportKind::Final,
                created_at: 2000,
                read: false,
                to_orchestrator: false,
                mission: None,
                looks_like_question: false,
                task: None,
            })
        };
        let mut v = run(&[hook("s", "UserPromptSubmit")]);
        v.apply(2, 2000, &Event::PrOpened { url: "https://github.com/a/b/pull/61".into(), session_id: "s".into(), task: None });
        v.apply(3, 3000, &Event::PrOpened { url: "https://github.com/a/b/pull/62".into(), session_id: "s".into(), task: None });
        v.apply(4, 4000, &Event::PrMerged { id: 1, method: MergeMethod::Merge });
        v.apply(5, 5000, &Event::PrDismissed { id: 2 });
        v.apply(6, 6000, &report(1, "https://github.com/a/b/pull/61"));
        v.apply(7, 7000, &report(2, "https://github.com/a/b/pull/62"));
        v.apply(8, 8000, &hook("s", "Stop"));
        assert!(v.reports[&1].read, "merged: nothing left for the user to do on it");
        assert!(!v.reports[&2].read, "only set aside: the user has not settled it");
        assert_eq!(v.inbox_reports().map(|r| r.id).collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn final_report_then_exit_is_done_not_down() {
        let mut v = run(&[hook("s", "UserPromptSubmit")]);
        v.apply(2, 2000, &Event::ReportPosted(Report {
            related: vec![],
            id: 1,
            session_id: "s".into(),
            node_name: None,
            title: "Done".into(),
            body: None,
            links: vec![],
            attachments: vec![],
            kind: ReportKind::Final,
            created_at: 2000,
            read: false,
            to_orchestrator: false,
            mission: None,
            looks_like_question: false,
            task: None,
        }));
        v.apply(3, 3000, &hook("s", "Stop"));
        assert_eq!(v.nodes["s"].state, NodeState::Done);
        v.apply(4, 4000, &Event::ProcessExited { session_id: "s".into(), pid: 0 });
        assert_eq!(v.nodes["s"].state, NodeState::Done);
    }

    #[test]
    fn permission_prompt_waits_and_clears_after_tool() {
        let mut v = run(&[hook("s", "UserPromptSubmit"), hook("s", "PermissionRequest")]);
        assert_eq!(v.nodes["s"].state, NodeState::WaitingAnswer);
        v.apply(3, 3000, &hook("s", "PostToolUse"));
        assert_eq!(v.nodes["s"].state, NodeState::Working);
    }

    /// Quill, 24 Sep: parked on context7's "requests your input" dialog while Alondra showed it working.
    #[test]
    fn an_mcp_elicitation_waits_on_the_user_until_it_is_answered() {
        let notification = |kind: &str| Event::Hook(HookEvent { session_id: "s".into(), event: "Notification".into(), notification_type: Some(kind.into()), ..Default::default() });
        let held = question(1, "s", QuestionOrigin::Elicitation);
        let answered = Event::QuestionAnswered { id: 1, answer: Answer { text: "Accept".into(), option: Some("Accept".into()), by: None, device: None, at: 0 } };
        let cases: Vec<(&str, Vec<Event>, NodeState)> = vec![
            ("the hook is held", vec![hook("s", "UserPromptSubmit"), hook("s", "Elicitation")], NodeState::WaitingAnswer),
            ("the dialog is on screen", vec![hook("s", "UserPromptSubmit"), notification("elicitation_dialog")], NodeState::WaitingAnswer),
            ("a browser link is asked for", vec![hook("s", "UserPromptSubmit"), notification("elicitation_url_dialog")], NodeState::WaitingAnswer),
            ("answered in the terminal", vec![hook("s", "UserPromptSubmit"), notification("elicitation_dialog"), notification("elicitation_response")], NodeState::Working),
            (
                "its card is answered",
                vec![hook("s", "UserPromptSubmit"), hook("s", "Elicitation"), Event::QuestionAsked(held.clone()), answered.clone(), Event::QuestionDelivered { id: 1 }],
                NodeState::Working,
            ),
            ("an idle prompt is not a dialog", vec![hook("s", "UserPromptSubmit"), notification("idle_prompt")], NodeState::Working),
        ];
        for (what, events, expected) in cases {
            assert_eq!(run(&events).nodes["s"].state, expected, "{what}");
        }
    }

    #[test]
    fn usage_limit_is_waiting_machine() {
        let v = run(&[
            hook("s", "UserPromptSubmit"),
            Event::Hook(HookEvent {
                session_id: "s".into(),
                event: "StopFailure".into(),
                error_type: Some("rate_limit".into()),
                ..Default::default()
            }),
        ]);
        assert_eq!(v.nodes["s"].state, NodeState::WaitingMachine);
    }

    #[test]
    fn held_question_orphaned_when_process_dies() {
        let mut v = run(&[
            hook("s", "UserPromptSubmit"),
            Event::QuestionAsked(question(1, "s", QuestionOrigin::AskUserQuestion)),
        ]);
        v.apply(3, 3000, &Event::ProcessExited { session_id: "s".into(), pid: 0 });
        assert_eq!(v.questions[&1].status, QuestionStatus::Orphaned);
    }

    #[test]
    fn menu_answered_in_terminal_closes_question() {
        let mut q = question(1, "s", QuestionOrigin::AskUserQuestion);
        q.tool_use_id = Some("t1".into());
        let mut v = run(&[hook("s", "UserPromptSubmit"), Event::QuestionAsked(q)]);
        v.apply(3, 3000, &Event::Hook(HookEvent {
            session_id: "s".into(),
            event: "PostToolUse".into(),
            tool_use_id: Some("t1".into()),
            ..Default::default()
        }));
        assert_eq!(v.questions[&1].status, QuestionStatus::AnsweredInTerminal);
    }

    #[test]
    fn message_delivered_only_when_node_starts_working() {
        let mut v = run(&[hook("s", "UserPromptSubmit"), hook("s", "Stop")]);
        v.apply(3, 3000, &Event::MessageQueued(Message {
            id: 1,
            session_id: "s".into(),
            from: crate::model::MessageFrom::User,
            text: "go on".into(),
            answers_question: None,
            created_at: 3000,
            handed_at: None,
            delivered_at: None,
            about: None,
            attachments: vec![],
        }));
        v.apply(4, 4000, &Event::MessageHanded { id: 1 });
        assert!(v.messages[&1].delivered_at.is_none());
        v.apply(5, 5000, &hook("s", "UserPromptSubmit"));
        assert_eq!(v.messages[&1].delivered_at, Some(5000));
    }

    #[test]
    fn removed_node_disappears_and_orphans_its_questions() {
        let mut v = run(&[
            hook("s", "UserPromptSubmit"),
            Event::QuestionAsked(question(1, "s", QuestionOrigin::Ask)),
        ]);
        v.apply(3, 3000, &Event::NodeRemoved { session_id: "s".into() });
        assert!(!v.nodes.contains_key("s"));
        assert_eq!(v.questions[&1].status, QuestionStatus::Orphaned);
    }

    #[test]
    fn late_events_do_not_resurrect_a_removed_node() {
        let mut v = run(&[hook("s", "UserPromptSubmit")]);
        v.apply(2, 2000, &Event::NodeRemoved { session_id: "s".into() });
        v.apply(3, 3000, &Event::ProcessExited { session_id: "s".into(), pid: 1 });
        v.apply(4, 4000, &hook("s", "SessionEnd"));
        assert!(!v.nodes.contains_key("s"));
    }

    #[test]
    fn old_report_kinds_still_replay() {
        let k: crate::model::ReportKind = serde_json::from_str("\"delivery\"").unwrap();
        assert_eq!(k, crate::model::ReportKind::Notice);
    }

    #[test]
    fn project_comes_from_the_repo_folder_even_in_worktrees() {
        let mut n = Node::new("s", 0);
        n.cwd = Some("/Users/d/Projects/Lumo/.claude/worktrees/phase-c".into());
        assert_eq!(n.project(), "Lumo");
        n.cwd = None;
        n.name = Some("hive/server".into());
        assert_eq!(n.project(), "hive");
        n.is_orchestrator = true;
        n.cwd = Some("/Users/d/Projects/Lumo".into());
        assert_eq!(n.project(), "orchestrator");
    }

    #[test]
    fn a_background_node_keeps_its_launch_folder_when_it_runs_commands_elsewhere() {
        let mut v = View::default();
        v.apply(1, 1000, &Event::DaemonSeen(DaemonObservation {
            session_id: "s".into(),
            short_id: "s".into(),
            cwd: Some("/Users/d/Projects".into()),
            ..Default::default()
        }));
        let mut h = HookEvent { session_id: "s".into(), event: "PreToolUse".into(), ..Default::default() };
        h.cwd = Some("/Users/d/Projects/Lumo".into());
        v.apply(2, 2000, &Event::Hook(h));
        assert_eq!(v.nodes["s"].cwd.as_deref(), Some("/Users/d/Projects"));
    }

    #[test]
    fn resolve_prefers_live_node_with_same_name() {
        let mut v = View::default();
        for (i, sid) in ["old", "new"].iter().enumerate() {
            v.apply(i as u64 + 1, i as i64, &Event::DaemonSeen(DaemonObservation {
                session_id: sid.to_string(),
                short_id: sid.to_string(),
                name: Some("lumo/x".into()),
                pid: Some(100 + i as i32),
                ..Default::default()
            }));
        }
        v.nodes.get_mut("old").unwrap().alive = false;
        assert_eq!(v.resolve_node("lumo/x").unwrap().session_id, "new");
    }

    #[test]
    fn prune_drops_old_things_that_are_not_in_the_inbox() {
        let mut v = View::default();
        v.apply(1, 1000, &hook("s", "Stop"));
        let q = |id: u64, status: QuestionStatus, at: Millis| Question {
            related: vec![],
            id,
            session_id: "s".into(),
            node_name: None,
            title: "t".into(),
            detail: None,
            options: vec![],
            links: vec![],
            attachments: vec![],
            blocking: true,
            origin: QuestionOrigin::Ask,
            tool_use_id: None,
            to_orchestrator: false,
            node_context: None,
            status,
            answer: None,
            asked_at: at,
            action: false,
            steps: vec![],
            mission: None,
            replaces: None,
            task: None,
        };
        v.questions.insert(1, q(1, QuestionStatus::Delivered, 100));
        v.questions.insert(2, q(2, QuestionStatus::Open, 100));
        v.questions.insert(3, q(3, QuestionStatus::Answered, 100));
        v.questions.insert(4, q(4, QuestionStatus::Delivered, 9000));
        v.apply(2, 10_000, &Event::Pruned { before: 5000 });
        assert_eq!(v.questions.keys().copied().collect::<Vec<_>>(), vec![2, 4]);
    }

    #[test]
    fn view_stays_bounded_when_hook_only_nodes_go_silent_with_answers_never_picked_up() {
        let mut v = View::default();
        let (mut seq, hour) = (0u64, 3_600_000);
        let mut next = |v: &mut View, at: Millis, e: Event| {
            seq += 1;
            v.apply(seq, at, &e);
        };
        let mut sizes = vec![];
        for cycle in 0..20i64 {
            let at = cycle * hour;
            for i in 0..11 {
                let sid = if i == 10 { "live".to_string() } else { format!("s{cycle}-{i}") };
                let id = (cycle * 11 + i) as u64 + 1;
                next(&mut v, at, hook(&sid, "PreToolUse"));
                let mut q = question(id, &sid, QuestionOrigin::Ask);
                q.asked_at = at;
                next(&mut v, at, Event::QuestionAsked(q));
                let answer = crate::model::Answer { text: "Yes".into(), option: None, by: None, device: None, at };
                next(&mut v, at, Event::QuestionAnswered { id, answer });
                next(&mut v, at, Event::MessageQueued(Message {
                    id,
                    session_id: sid.clone(),
                    from: MessageFrom::User,
                    text: "Yes".into(),
                    answers_question: Some(id),
                    created_at: at,
                    handed_at: None,
                    delivered_at: None,
                    about: None,
                    attachments: vec![],
                }));
            }
            next(&mut v, at, Event::Pruned { before: at - 2 * hour });
            sizes.push((v.nodes.len(), v.questions.len(), v.messages.len()));
        }
        assert_eq!(sizes.last(), Some(&(31, 33, 33)), "sizes per cycle: {sizes:?}");
    }

    fn report(id: u64, sid: &str, kind: ReportKind, at: Millis) -> Event {
        Event::ReportPosted(Report {
            related: vec![],
            id,
            session_id: sid.into(),
            node_name: None,
            title: "Result".into(),
            body: None,
            links: vec![],
            attachments: vec![],
            kind,
            created_at: at,
            read: false,
            to_orchestrator: false,
            mission: None,
            looks_like_question: false,
            task: None,
        })
    }

    fn seen(sid: &str, state: &str, tempo: &str, needs: Option<&str>, pid: Option<i32>) -> Event {
        Event::DaemonSeen(DaemonObservation {
            session_id: sid.into(),
            short_id: sid.into(),
            state: Some(state.into()),
            tempo: Some(tempo.into()),
            needs: needs.map(str::to_string),
            pid,
            ..Default::default()
        })
    }

    #[test]
    fn a_classification_never_decides_the_state() {
        let mut v = run(&[hook("s", "UserPromptSubmit"), hook("s", "Stop")]);
        v.apply(3, 3000, &Event::Classified { session_id: "s".into(), turn: 1, state: NodeState::WaitingAnswer, confidence: Some(0.97) });
        assert_eq!(v.nodes["s"].state, NodeState::Unclear);
    }

    #[test]
    fn a_daemon_node_with_needs_goes_to_the_sentinel_until_it_has_a_question() {
        let mut v = run(&[seen("s", "blocked", "blocked", Some("decide merge"), Some(7))]);
        assert_eq!(v.nodes["s"].state, NodeState::Unclear);
        assert_eq!(v.nodes["s"].state_source, StateSource::Daemon);
        v.apply(2, 2000, &Event::QuestionAsked(question(1, "s", QuestionOrigin::Classifier)));
        assert_eq!(v.nodes["s"].state, NodeState::WaitingAnswer);
    }

    #[test]
    fn a_dormant_node_keeps_its_open_question() {
        let v = run(&[seen("s", "blocked", "blocked", Some("decide"), None), Event::QuestionAsked(question(1, "s", QuestionOrigin::Ask))]);
        assert_eq!(v.nodes["s"].state, NodeState::WaitingAnswer);
        assert!(!v.nodes["s"].alive);
    }

    #[test]
    fn a_daemon_saying_done_is_not_done_without_a_report() {
        let mut v = run(&[seen("s", "done", "idle", None, None)]);
        assert_eq!(v.nodes["s"].state, NodeState::Unclear);
        v.apply(2, 2000, &report(1, "s", ReportKind::Notice, 2000));
        assert_eq!(v.nodes["s"].state, NodeState::Delivered);
    }

    #[test]
    fn a_final_report_from_a_node_without_the_plugin_is_not_its_own() {
        let v = run(&[seen("s", "stopped", "idle", None, None), report(1, "s", ReportKind::Final, 1000)]);
        assert_eq!(v.nodes["s"].state, NodeState::Delivered);
    }

    #[test]
    fn delivered_node_is_news_until_acknowledged_and_again_after_new_episode() {
        let mut v = run(&[hook("s", "UserPromptSubmit")]);
        v.apply(2, 2000, &Event::Hook(HookEvent { session_id: "s".into(), event: "Stop".into(), last_message: Some("PR #4 ready".into()), ..Default::default() }));
        v.apply(3, 3000, &report(1, "s", ReportKind::Notice, 3000));
        assert_eq!(v.news(9000).len(), 1);
        v.apply(4, 4000, &Event::Acknowledged { session_id: "s".into() });
        assert!(v.news(9000).is_empty());
        v.apply(5, 5000, &hook("s", "UserPromptSubmit"));
        v.apply(6, 6000, &Event::Hook(HookEvent { session_id: "s".into(), event: "Stop".into(), last_message: Some("something else".into()), ..Default::default() }));
        v.apply(7, 7000, &report(2, "s", ReportKind::Notice, 7000));
        assert_eq!(v.news(9000).len(), 1);
    }

    #[test]
    fn a_merged_or_closed_pr_withdraws_the_open_questions_that_link_to_it() {
        use crate::model::Link;
        use crate::pr::{MergeMethod, PrState};
        let closed = |state: PrState| move |v: &View| {
            let mut pr = v.prs[&1].clone();
            pr.state = state;
            Event::PrUpdated(pr)
        };
        let cases: Vec<(&str, &str, Box<dyn Fn(&View) -> Event>, QuestionStatus)> = vec![
            ("merged from a card", "https://github.com/a/b/pull/24", Box::new(|_| Event::PrMerged { id: 1, method: MergeMethod::Merge }), QuestionStatus::Withdrawn),
            ("an action to do once it is merged", "action https://github.com/a/b/pull/24", Box::new(|_| Event::PrMerged { id: 1, method: MergeMethod::Merge }), QuestionStatus::Open),
            ("merged on GitHub", "https://github.com/a/b/pull/24/files", Box::new(closed(PrState::Merged)), QuestionStatus::Withdrawn),
            ("closed on GitHub", "https://github.com/a/b/pull/24", Box::new(closed(PrState::Closed)), QuestionStatus::Withdrawn),
            ("still open", "https://github.com/a/b/pull/24", Box::new(closed(PrState::Open)), QuestionStatus::Open),
            ("set aside is not settled", "https://github.com/a/b/pull/24", Box::new(|_| Event::PrDismissed { id: 1 }), QuestionStatus::Open),
            ("another PR", "https://github.com/a/b/pull/25", Box::new(|_| Event::PrMerged { id: 1, method: MergeMethod::Merge }), QuestionStatus::Open),
        ];
        for (name, link, event, expected) in cases {
            let mut v = run(&[hook("s", "UserPromptSubmit")]);
            v.apply(2, 2000, &Event::PrOpened { url: "https://github.com/a/b/pull/24".into(), session_id: "s".into(), task: None });
            let mut q = question(1, "s", QuestionOrigin::Ask);
            let (action, link) = match link.strip_prefix("action ") {
                Some(url) => (true, url),
                None => (false, link),
            };
            q.action = action;
            q.links = vec![Link { url: link.into(), title: None }];
            v.apply(3, 3000, &Event::QuestionAsked(q));
            let e = event(&v);
            v.apply(4, 4000, &e);
            assert_eq!(v.questions[&1].status, expected, "{name}");
            if action {
                assert_eq!(v.nodes["s"].state, crate::model::NodeState::YourTurn, "{name}: the node waits on the user, it is not stalled");
            }
        }
    }

    #[test]
    fn a_question_of_a_task_with_an_open_pr_card_shows_in_that_card() {
        use crate::model::Link;
        const PR: &str = "https://github.com/a/b/pull/24";
        let link = |url: &str| vec![Link { url: url.into(), title: None }];
        let cases: Vec<(&str, &str, QuestionOrigin, Vec<Link>, Vec<Event>, Option<u64>)> = vec![
            ("links the PR", "s", QuestionOrigin::Ask, link(PR), vec![], Some(1)),
            ("links nothing: the newest", "s", QuestionOrigin::Ask, vec![], vec![], Some(2)),
            ("links the task's other PR", "s", QuestionOrigin::Ask, link("https://github.com/a/b/pull/25"), vec![], Some(2)),
            ("links another node's PR", "s", QuestionOrigin::Ask, link("https://github.com/a/b/pull/26"), vec![], Some(2)),
            ("another node's question linking the PR: its own card", "t", QuestionOrigin::Ask, link(PR), vec![], Some(3)),
            ("a permission prompt", "s", QuestionOrigin::Permission, vec![], vec![], None),
            ("the PRs wait for new commits", "s", QuestionOrigin::Ask, link(PR), vec![Event::PrChangesRequested { id: 1 }, Event::PrChangesRequested { id: 2 }], None),
            ("the PR cards were set aside", "s", QuestionOrigin::Ask, link(PR), vec![Event::PrDismissed { id: 1 }, Event::PrDismissed { id: 2 }], None),
        ];
        for (what, asker, origin, links, events, expected) in cases {
            let mut v = run(&[hook("s", "UserPromptSubmit"), hook("t", "UserPromptSubmit")]);
            for (seq, url, sid) in [(3, PR, "s"), (4, "https://github.com/a/b/pull/25", "s"), (5, "https://github.com/a/b/pull/26", "t")] {
                v.apply(seq, 2000, &Event::PrOpened { url: url.into(), session_id: sid.into(), task: None });
            }
            for (i, e) in events.iter().enumerate() {
                v.apply(6 + i as u64, 3000, e);
            }
            let mut q = question(1, asker, origin);
            q.links = links;
            assert_eq!(v.question_pr(&q), expected, "{what}");
        }
    }

    fn report_about(id: u64, sid: &str, kind: ReportKind, at: Millis, links: &[&str], title: &str, body: &str) -> Event {
        let Event::ReportPosted(mut r) = report(id, sid, kind, at) else { unreachable!() };
        r.links = links.iter().map(|url| crate::model::Link { url: (*url).into(), title: None }).collect();
        r.title = title.into();
        r.body = Some(body.to_string()).filter(|b| !b.is_empty());
        Event::ReportPosted(r)
    }

    const ARTIFACT: &str = "https://claude.ai/code/artifact/plan-1";

    /// R#146 "PR #50: salir de un nodo…" and the card of alondra#50, 24 Sep: two cards for one PR.
    /// A report of the task goes into the task's PR card, whatever it links.
    #[test]
    fn a_report_of_a_task_with_an_open_pr_card_lives_inside_that_card() {
        const PR: &str = "https://github.com/a/b/pull/24";
        let cases: Vec<(&str, &str, Vec<&str>, Vec<Event>, Option<u64>, bool)> = vec![
            ("links the PR", "s", vec![PR], vec![], Some(1), true),
            ("links nothing", "s", vec![], vec![], Some(1), true),
            ("links another PR", "s", vec!["https://github.com/a/b/pull/25"], vec![], Some(1), true),
            ("another node's report linking the PR", "t", vec![PR], vec![], None, true),
            ("an artifact to review", "s", vec![ARTIFACT, PR], vec![], None, true),
            ("the card waits for new commits", "s", vec![PR], vec![Event::PrChangesRequested { id: 1 }], Some(1), true),
        ];
        for (what, sid, links, before, inside, in_inbox) in cases {
            let mut v = run(&[hook("s", "UserPromptSubmit"), hook("t", "UserPromptSubmit")]);
            v.apply(3, 2000, &Event::PrOpened { url: PR.into(), session_id: "s".into(), task: None });
            for (i, e) in before.iter().enumerate() {
                v.apply(4 + i as u64, 3000, e);
            }
            v.apply(10, 10_000, &report_about(7, sid, ReportKind::Notice, 10_000, &links, "Ready", ""));
            assert_eq!(v.report_pr(&v.reports[&7]), inside, "{what}");
            assert_eq!(v.inbox_reports().any(|r| r.id == 7), in_inbox, "{what}");
        }
    }

    /// R#113 "Los menús que Alondra no veía ya llegan como tarjeta…" and then Q#123 "Siguiente:
    /// ¿uso de la suscripción…?" (23 Sep): two cards for one task.
    #[test]
    fn a_question_carries_the_reports_of_its_task() {
        const PR: &str = "https://github.com/a/b/pull/24";
        let q = || Event::QuestionAsked(question(1, "s", QuestionOrigin::Ask));
        let cases: Vec<(&str, Vec<Event>, bool)> = vec![
            ("a report before the question", vec![report(7, "s", ReportKind::Final, 3000), q()], true),
            ("a report after the question", vec![q(), report(7, "s", ReportKind::Notice, 3000)], true),
            ("another node's report", vec![report(7, "t", ReportKind::Final, 3000), q()], false),
            ("a read report", vec![report(7, "s", ReportKind::Final, 3000), Event::ReportRead { id: 7 }, q()], false),
            ("a report inside its PR card", vec![Event::PrOpened { url: PR.into(), session_id: "s".into(), task: None }, report_about(7, "s", ReportKind::Final, 3000, &[PR], "Listo", ""), q()], false),
            ("an artifact to review", vec![report_about(7, "s", ReportKind::Document, 3000, &[ARTIFACT], "Plan", ""), q()], false),
        ];
        for (what, events, carries) in cases {
            let mut v = run(&[hook("s", "UserPromptSubmit"), hook("t", "UserPromptSubmit")]);
            for (i, e) in events.iter().enumerate() {
                v.apply(10 + i as u64, 3000, e);
            }
            assert_eq!(v.question_reports(&v.questions[&1]).contains(&7), carries, "{what}");
            let pr_card = v.report_pr(&v.reports[&7]).is_some();
            assert_eq!(v.inbox_reports().any(|r| r.id == 7), !carries && !v.reports[&7].read, "{what}{}", if pr_card { " (in its PR card)" } else { "" });
        }
    }

    /// R#232, a document linking an artifact, stacked under question #237 (24 Sep): answering the
    /// question read it and the review card was gone before David saw it. Only his verdict takes
    /// a review card out of the inbox.
    #[test]
    fn answering_a_question_never_takes_a_review_card_with_it() {
        let mut v = run(&[hook("s", "UserPromptSubmit"), report_about(7, "s", ReportKind::Document, 1500, &[ARTIFACT], "Plan", ""), report(8, "s", ReportKind::Notice, 1600)]);
        v.apply(10, 2000, &Event::QuestionAsked(question(1, "s", QuestionOrigin::Ask)));
        assert_eq!(v.question_reports(&v.questions[&1]), [8]);
        assert!(v.reviews_awaiting_user().any(|r| r.id == 7));
        assert_eq!(v.inbox_reports().map(|r| r.id).collect::<Vec<_>>(), [7]);
        assert!(v.report_stack(&v.reports[&7]).is_empty(), "the review card carries nothing a question took");
    }

    /// R#223 (document, with its attachment) and R#224 "Informe … entregado (#223)" (alondra/
    /// estados-por-tarea, 24 Sep): the final report hid the document.
    #[test]
    fn a_document_heads_its_stack_and_short_notices_go_under_it() {
        let cases = [
            ("final after the document", [(ReportKind::Document, 1), (ReportKind::Final, 2)], 1, vec![2]),
            ("notice after the document", [(ReportKind::Document, 1), (ReportKind::Notice, 2)], 1, vec![2]),
            ("document after the notice", [(ReportKind::Notice, 1), (ReportKind::Document, 2)], 2, vec![1]),
            ("two finals: the newest heads", [(ReportKind::Final, 1), (ReportKind::Final, 2)], 2, vec![1]),
        ];
        for (what, posted, head, under) in cases {
            let mut v = run(&[hook("s", "UserPromptSubmit")]);
            let [(first, a), (second, b)] = posted;
            v.apply(2, 2000, &report_about(a, "s", first, 2000, &[], "Estados por tarea", ""));
            v.apply(3, 3000, &report_about(b, "s", second, 3000, &[], "Entregado", ""));
            assert_eq!(v.inbox_reports().map(|r| r.id).collect::<Vec<_>>(), [head], "{what}");
            assert_eq!(v.report_stack(&v.reports[&head]), under, "{what}");
        }
    }

    /// R#29 "Auditoría de jev rehecha…" and R#30 "Annex to the jev audit…" (23 Sep): one delivery,
    /// two cards. A task's reports are one card; a review card stays apart.
    #[test]
    fn a_tasks_reports_are_one_card_and_a_review_card_stays_apart() {
        let mut v = run(&[hook("s", "UserPromptSubmit"), hook("t", "UserPromptSubmit")]);
        v.apply(3, 2000, &report_about(1, "s", ReportKind::Final, 2000, &[], "Auditoría", ""));
        v.apply(4, 3000, &report_about(2, "s", ReportKind::Final, 3000, &[], "Anexo", ""));
        v.apply(5, 3500, &report_about(3, "t", ReportKind::Notice, 3500, &[], "Otra cosa", ""));
        v.apply(6, 3600, &report_about(4, "s", ReportKind::Document, 3600, &[ARTIFACT], "Plan", ""));
        v.apply(7, 4000, &report_about(5, "s", ReportKind::Notice, 4000, &[], "Otro anexo", ""));
        let inbox = |v: &View| v.inbox_reports().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(inbox(&v), [5, 4, 3], "the newest carries the task's others; the review and the other node's stay apart");
        assert_eq!(v.report_stack(&v.reports[&5]), [1, 2]);
        assert!(v.report_stack(&v.reports[&2]).is_empty(), "only the newest carries the others");

        v.apply(8, 5000, &Event::ReportRead { id: 5 });
        assert!(v.reports[&1].read && v.reports[&2].read, "reading the newest reads the ones it carries");
        assert!(!v.reports[&3].read && !v.reports[&4].read);
        assert_eq!(inbox(&v), [4, 3]);
    }

    #[test]
    fn a_question_does_not_take_a_report_out_of_its_pr_card() {
        const PR: &str = "https://github.com/a/b/pull/24";
        let mut v = run(&[hook("s", "UserPromptSubmit")]);
        v.apply(2, 2000, &Event::PrOpened { url: PR.into(), session_id: "s".into(), task: None });
        v.apply(3, 3000, &report_about(7, "s", ReportKind::Notice, 3000, &[PR], "Ready", ""));
        v.apply(4, 4000, &Event::QuestionAsked(question(1, "s", QuestionOrigin::Ask)));
        assert!(v.question_reports(&v.questions[&1]).is_empty());
        assert_eq!(v.report_pr(&v.reports[&7]), Some(1));
        assert_eq!(v.question_pr(&v.questions[&1]), Some(1), "the question is in the PR card too");
    }

    #[test]
    fn a_pr_card_that_leaves_for_good_takes_its_reports_along() {
        use crate::pr::{MergeMethod, PrState};
        const PR: &str = "https://github.com/a/b/pull/24";
        let github = |state: PrState| {
            move |v: &View| {
                let mut pr = v.prs[&1].clone();
                pr.state = state;
                Event::PrUpdated(pr)
            }
        };
        let cases: Vec<(&str, ReportKind, Box<dyn Fn(&View) -> Event>, bool, NodeState)> = vec![
            ("notice, merged from the card", ReportKind::Notice, Box::new(|_| Event::PrMerged { id: 1, method: MergeMethod::Merge }), true, NodeState::Done),
            ("final, merged from the card", ReportKind::Final, Box::new(|_| Event::PrMerged { id: 1, method: MergeMethod::Squash }), true, NodeState::Done),
            ("notice, merged on GitHub", ReportKind::Notice, Box::new(github(PrState::Merged)), true, NodeState::Done),
            ("notice, closed on GitHub", ReportKind::Notice, Box::new(github(PrState::Closed)), true, NodeState::Done),
            ("notice, card set aside", ReportKind::Notice, Box::new(|_| Event::PrDismissed { id: 1 }), true, NodeState::Done),
            ("notice, changes requested", ReportKind::Notice, Box::new(|_| Event::PrChangesRequested { id: 1 }), false, NodeState::Delivered),
            ("notice, still open", ReportKind::Notice, Box::new(github(PrState::Open)), false, NodeState::WaitingMerge),
        ];
        for (what, kind, event, read, state) in cases {
            let mut v = run(&[hook("s", "UserPromptSubmit")]);
            v.apply(2, 2000, &Event::PrOpened { url: PR.into(), session_id: "s".into(), task: None });
            v.apply(3, 3000, &hook("s", "Stop"));
            v.apply(4, 4000, &report_about(7, "s", kind, 4000, &[PR], "Ready", ""));
            v.apply(5, 4000, &report_about(8, "t", ReportKind::Notice, 4000, &[], "Something else", ""));
            assert_eq!(v.nodes["s"].state, NodeState::WaitingMerge, "{what}: the card waits on the user");
            let e = event(&v);
            v.apply(6, 6000, &e);
            assert_eq!(v.reports[&7].read, read, "{what}");
            assert!(!v.reports[&8].read, "{what}: a report about something else stays");
            assert_eq!(v.nodes["s"].state, state, "{what}");
        }
    }

    #[test]
    fn the_orchestrator_has_its_own_states() {
        let mut v = run(&[Event::MarkedOrchestrator { session_id: "o".into() }, hook("o", "UserPromptSubmit")]);
        assert_eq!(v.nodes["o"].orchestrator_state, Some(OrchestratorState::Handling));
        assert_eq!(v.nodes["o"].state, NodeState::Working);
        v.apply(3, 3000, &hook("o", "Stop"));
        assert_eq!(v.nodes["o"].orchestrator_state, Some(OrchestratorState::OnDuty));
        assert_eq!(v.nodes["o"].state, NodeState::Delivered);
        v.apply(4, 4000, &Event::QuestionAsked(question(1, "o", QuestionOrigin::Ask)));
        assert_eq!(v.nodes["o"].orchestrator_state, Some(OrchestratorState::WaitingForYou));
        assert_eq!(v.nodes["o"].state, NodeState::WaitingAnswer);
        v.apply(5, 5000, &Event::QuestionClosed { id: 1, status: QuestionStatus::Withdrawn, note: None });
        v.apply(6, 6000, &Event::ProcessExited { session_id: "o".into(), pid: 0 });
        assert_eq!(v.nodes["o"].orchestrator_state, Some(OrchestratorState::Offline));
    }

    #[test]
    fn the_orchestrator_never_waits_on_the_user_through_a_guess() {
        let mut v = run(&[Event::MarkedOrchestrator { session_id: "o".into() }, hook("o", "UserPromptSubmit"), hook("o", "Stop")]);
        v.apply(4, 4000, &Event::Classified { session_id: "o".into(), turn: 1, state: NodeState::WaitingAnswer, confidence: Some(0.95) });
        assert_eq!(v.nodes["o"].orchestrator_state, Some(OrchestratorState::OnDuty));
    }

    #[test]
    fn a_node_that_asked_and_then_stopped_is_not_working() {
        let mut v = run(&[hook("s", "UserPromptSubmit"), Event::QuestionAsked(question(1, "s", QuestionOrigin::Ask))]);
        assert_eq!(v.nodes["s"].state, NodeState::WaitingAnswer);
        v.apply(3, 3000, &Event::QuestionClosed { id: 1, status: QuestionStatus::Withdrawn, note: None });
        assert_eq!(v.nodes["s"].state, NodeState::Working);
        v.apply(4, 4000, &hook("s", "Stop"));
        assert_eq!(v.nodes["s"].state, NodeState::Unclear);
    }

    #[test]
    fn messages_for_an_interactive_session_wait_for_its_turn_to_end() {
        let queued = |id: u64, sid: &str| {
            Event::MessageQueued(Message {
                id,
                session_id: sid.into(),
                from: crate::model::MessageFrom::User,
                text: "go on".into(),
                answers_question: None,
                created_at: 3000,
                handed_at: None,
                delivered_at: None,
                about: None,
                attachments: vec![],
            })
        };
        let mut v = View::default();
        v.apply(1, 1000, &hook("interactive", "UserPromptSubmit"));
        v.apply(2, 1000, &Event::DaemonSeen(DaemonObservation { session_id: "job".into(), short_id: "job".into(), ..Default::default() }));
        for (i, sid) in ["interactive", "interactive", "job"].iter().enumerate() {
            v.apply(3 + i as u64, 3000, &queued(i as u64 + 1, sid));
        }
        assert_eq!(v.held_for_turn_end("interactive"), 2);
        assert_eq!(v.held_for_turn_end("job"), 0, "a background job gets them typed in");
        assert_eq!(v.held_for_turn_end("nobody"), 0);

        let (ids, text) = v.turn_end_delivery("interactive", "Ana").unwrap();
        assert_eq!(ids, vec![(1, None), (2, None)]);
        assert_eq!(text.matches("From Ana:\ngo on").count(), 2, "{text}");
        assert_eq!(v.turn_end_delivery("job", "Ana"), None, "wake types into background jobs");
        assert_eq!(v.turn_end_delivery("nobody", "Ana"), None);

        v.apply(6, 4000, &Event::MessageHanded { id: 1 });
        assert_eq!(v.held_for_turn_end("interactive"), 1);
        v.apply(7, 4000, &Event::MessageHanded { id: 2 });
        assert_eq!(v.turn_end_delivery("interactive", "Ana"), None, "a handed message is not handed again");
    }

    #[test]
    fn a_node_that_was_just_written_to_is_working() {
        let mut v = View::default();
        v.apply(1, 1000, &hook("s", "UserPromptSubmit"));
        v.apply(2, 2000, &hook("s", "Stop"));
        v.apply(3, 3000, &Event::MessageQueued(Message {
            id: 1,
            session_id: "s".into(),
            from: crate::model::MessageFrom::User,
            text: "go on".into(),
            answers_question: None,
            created_at: 3000,
            handed_at: None,
            delivered_at: None,
            about: None,
            attachments: vec![],
        }));
        assert_ne!(v.nodes["s"].state, NodeState::Working);
        v.apply(4, 4000, &Event::MessageHanded { id: 1 });
        assert_eq!(v.nodes["s"].state, NodeState::Working);
    }

    /// Report #136 (the day-one audit) and the orchestrator's question #152 about it, 24 Sep: the
    /// orchestrator works on no task, so what it relays stays a card of its own.
    #[test]
    fn a_question_of_the_orchestrator_carries_nothing() {
        let mut v = run(&[Event::MarkedOrchestrator { session_id: "orq".into() }, hook("node", "UserPromptSubmit"), report(7, "node", ReportKind::Notice, 1000)]);
        let mut q = question(1, "orq", QuestionOrigin::Ask);
        q.detail = Some("La auditoría (informe #7) encontró 13 cosas".into());
        v.apply(10, 2000, &Event::QuestionAsked(q));
        assert!(v.question_reports(&v.questions[&1]).is_empty());
        assert!(v.absorbed_reports().is_empty());
    }

    #[test]
    fn a_read_report_or_an_answered_question_takes_nothing_in() {
        let asked = |v: &mut View| v.apply(10, 2000, &Event::QuestionAsked(question(1, "node", QuestionOrigin::Ask)));
        let mut v = run(&[hook("node", "UserPromptSubmit"), report(7, "node", ReportKind::Notice, 1000)]);
        asked(&mut v);
        assert_eq!(v.question_reports(&v.questions[&1]), [7]);
        v.apply(11, 3000, &Event::QuestionAnswered { id: 1, answer: Answer { text: "Sí".into(), option: None, by: None, device: None, at: 3000 } });
        assert!(v.question_reports(&v.questions[&1]).is_empty(), "only an open question holds the report");
        assert!(v.absorbed_reports().is_empty());

        let mut v = run(&[hook("node", "UserPromptSubmit"), report(7, "node", ReportKind::Notice, 1000), Event::ReportRead { id: 7 }]);
        asked(&mut v);
        assert!(v.question_reports(&v.questions[&1]).is_empty(), "a read report is out of the inbox already");
    }
}
