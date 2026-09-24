use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::event::{DaemonObservation, Event, HookEvent};
use crate::model::{
    Answer, Millis, Node, Question, QuestionOrigin, QuestionStatus, ReportKind, Signal, StateSource, ToolInFlight,
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
}

const USAGE_LIMIT_ERRORS: &[&str] = &["rate_limit", "overloaded", "billing_error"];

impl View {
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
    }

    pub fn apply(&mut self, seq: u64, at: Millis, event: &Event) {
        self.seq = seq;
        if let Some(sid) = event.session_id()
            && self.removed.contains(sid)
        {
            return;
        }
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
                let node = self.node_mut(&r.session_id, at);
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
                let sid = r.session_id.clone();
                self.reports.insert(r.id, r);
                Some((sid, Trigger::Report))
            }
            Event::ReportRead { id } => {
                let session = self.reports.get_mut(id).map(|r| {
                    r.read = true;
                    r.session_id.clone()
                });
                let node = session.as_ref().and_then(|s| self.nodes.get_mut(s));
                match node {
                    Some(n) => {
                        n.acknowledged_at = Some(at);
                        session.map(|s| (s, Trigger::Report))
                    }
                    None => None,
                }
            }
            Event::MessageQueued(m) => {
                self.messages.insert(m.id, m.clone());
                Some((m.session_id.clone(), Trigger::Message))
            }
            Event::MessageHanded { id } => {
                let Some(m) = self.messages.get_mut(id) else { return };
                m.handed_at = Some(at);
                let (sid, from_someone) = (m.session_id.clone(), m.from != MessageFrom::Alondra);
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
            Event::PrOpened { url, session_id } => {
                let known = crate::pr::parse_url(url).is_some_and(|(repo, number)| self.prs.values().any(|p| p.repo == repo && p.number == number));
                if !known {
                    let id = self.prs.keys().next_back().copied().unwrap_or(0) + 1;
                    let name = self.nodes.get(session_id).and_then(|n| n.name.clone());
                    if let Some(pr) = crate::pr::PullRequest::opened(id, url, session_id, name, at) {
                        self.prs.insert(id, pr);
                    }
                }
                self.nodes.contains_key(session_id).then(|| (session_id.clone(), Trigger::Other))
            }
            Event::PrUpdated(pr) => {
                if let Some(known) = self.prs.get_mut(&pr.id) {
                    let dismissed = known.state == crate::pr::PrState::Dismissed && pr.state == crate::pr::PrState::Open;
                    let requested = known.changes_requested_on.take();
                    *known = pr.clone();
                    known.changes_requested_on = requested;
                    if dismissed {
                        known.state = crate::pr::PrState::Dismissed;
                    }
                }
                if matches!(pr.state, crate::pr::PrState::Merged | crate::pr::PrState::Closed) {
                    self.withdraw_questions_about(pr.id);
                }
                self.pr_node(pr.id)
            }
            Event::PrMerged { id, method } => {
                if let Some(pr) = self.prs.get_mut(id) {
                    pr.state = crate::pr::PrState::Merged;
                    self.merge_methods.insert(pr.repo.clone(), *method);
                }
                self.withdraw_questions_about(*id);
                self.pr_node(*id)
            }
            Event::PrDismissed { id } => {
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

    /// Once a PR is merged or closed, the open questions that link to it have nothing left to decide.
    fn withdraw_questions_about(&mut self, pr_id: u64) {
        let Some(pr) = self.prs.get(&pr_id) else { return };
        let target = (pr.repo.clone(), pr.number);
        for q in self.questions.values_mut() {
            if q.status == QuestionStatus::Open && q.links.iter().any(|l| crate::pr::parse_url(&l.url).as_ref() == Some(&target)) {
                q.status = QuestionStatus::Withdrawn;
            }
        }
    }

    fn question_update(&mut self, id: u64, f: impl FnOnce(&mut Question)) -> Option<(String, Trigger)> {
        let q = self.questions.get_mut(&id)?;
        f(q);
        let (sid, open, tool_use_id) = (q.session_id.clone(), q.status.is_open(), q.tool_use_id.clone());
        if !open
            && let Some(node) = self.nodes.get_mut(&sid)
            && tool_use_id.is_some()
            && node.tool.as_ref().is_some_and(|t| t.tool_use_id == tool_use_id)
        {
            node.tool = None;
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
            "PermissionRequest" => node.waiting_permission = true,
            "Notification" => {
                if h.notification_type.as_deref() == Some("permission_prompt") {
                    node.waiting_permission = true;
                }
            }
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
        node.daemon_tasks = d.tasks.unwrap_or(0);
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

    /// The PR card a question is about: the first PR it links that has a card waiting for the user.
    pub fn question_pr(&self, q: &Question) -> Option<u64> {
        q.links.iter().filter_map(|l| crate::pr::parse_url(&l.url)).find_map(|(repo, number)| {
            self.prs_awaiting_user().find(|p| p.repo == repo && p.number == number).map(|p| p.id)
        })
    }

    pub fn open_questions(&self) -> impl Iterator<Item = &Question> {
        self.questions.values().filter(|q| q.status.is_open())
    }

    pub fn pending_messages(&self, session_id: &str) -> impl Iterator<Item = &Message> {
        self.messages
            .values()
            .filter(move |m| m.session_id == session_id && m.handed_at.is_none())
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

    fn hook(sid: &str, event: &str) -> Event {
        Event::Hook(HookEvent { session_id: sid.into(), event: event.into(), ..Default::default() })
    }

    fn question(id: u64, sid: &str, origin: QuestionOrigin) -> Question {
        Question {
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
    fn final_report_then_exit_is_done_not_down() {
        let mut v = run(&[hook("s", "UserPromptSubmit")]);
        v.apply(2, 2000, &Event::ReportPosted(Report {
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
            mission: None,
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
                }));
            }
            next(&mut v, at, Event::Pruned { before: at - 2 * hour });
            sizes.push((v.nodes.len(), v.questions.len(), v.messages.len()));
        }
        assert_eq!(sizes.last(), Some(&(31, 33, 33)), "sizes per cycle: {sizes:?}");
    }

    fn report(id: u64, sid: &str, kind: ReportKind, at: Millis) -> Event {
        Event::ReportPosted(Report {
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
            mission: None,
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
            ("merged on GitHub", "https://github.com/a/b/pull/24/files", Box::new(closed(PrState::Merged)), QuestionStatus::Withdrawn),
            ("closed on GitHub", "https://github.com/a/b/pull/24", Box::new(closed(PrState::Closed)), QuestionStatus::Withdrawn),
            ("still open", "https://github.com/a/b/pull/24", Box::new(closed(PrState::Open)), QuestionStatus::Open),
            ("set aside is not settled", "https://github.com/a/b/pull/24", Box::new(|_| Event::PrDismissed { id: 1 }), QuestionStatus::Open),
            ("another PR", "https://github.com/a/b/pull/25", Box::new(|_| Event::PrMerged { id: 1, method: MergeMethod::Merge }), QuestionStatus::Open),
        ];
        for (name, link, event, expected) in cases {
            let mut v = run(&[hook("s", "UserPromptSubmit")]);
            v.apply(2, 2000, &Event::PrOpened { url: "https://github.com/a/b/pull/24".into(), session_id: "s".into() });
            let mut q = question(1, "s", QuestionOrigin::Ask);
            q.links = vec![Link { url: link.into(), title: None }];
            v.apply(3, 3000, &Event::QuestionAsked(q));
            let e = event(&v);
            v.apply(4, 4000, &e);
            assert_eq!(v.questions[&1].status, expected, "{name}");
        }
    }

    #[test]
    fn a_question_that_links_a_pr_waiting_on_its_card_belongs_to_that_card() {
        use crate::model::Link;
        let cases: Vec<(&str, &str, Vec<Event>, Option<u64>)> = vec![
            ("links the PR", "https://github.com/a/b/pull/24", vec![], Some(1)),
            ("links a page of the PR", "https://github.com/a/b/pull/24/files", vec![], Some(1)),
            ("links another PR", "https://github.com/a/b/pull/25", vec![], None),
            ("links something else", "https://example.com/pull/24", vec![], None),
            ("the PR waits for new commits", "https://github.com/a/b/pull/24", vec![Event::PrChangesRequested { id: 1 }], None),
            ("the PR card was set aside", "https://github.com/a/b/pull/24", vec![Event::PrDismissed { id: 1 }], None),
        ];
        for (what, link, events, expected) in cases {
            let mut v = run(&[hook("s", "UserPromptSubmit")]);
            v.apply(2, 2000, &Event::PrOpened { url: "https://github.com/a/b/pull/24".into(), session_id: "s".into() });
            for (i, e) in events.iter().enumerate() {
                v.apply(3 + i as u64, 3000, e);
            }
            let mut q = question(1, "s", QuestionOrigin::Ask);
            q.links = vec![Link { url: link.into(), title: None }];
            assert_eq!(v.question_pr(&q), expected, "{what}");
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
        }));
        assert_ne!(v.nodes["s"].state, NodeState::Working);
        v.apply(4, 4000, &Event::MessageHanded { id: 1 });
        assert_eq!(v.nodes["s"].state, NodeState::Working);
    }
}
