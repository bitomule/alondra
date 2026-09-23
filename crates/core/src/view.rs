use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::event::{DaemonObservation, Event, HookEvent};
use crate::model::{
    Millis, Node, NodeState, Question, QuestionOrigin, QuestionStatus, ReportKind, StateSource,
    ToolInFlight, TurnSignal,
};
use crate::model::{Message, Report};

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct View {
    pub seq: u64,
    pub nodes: BTreeMap<String, Node>,
    pub questions: BTreeMap<u64, Question>,
    pub reports: BTreeMap<u64, Report>,
    pub messages: BTreeMap<u64, Message>,
    #[serde(default)]
    pub removed: std::collections::BTreeSet<String>,
}

const USAGE_LIMIT_ERRORS: &[&str] = &["rate_limit", "overloaded", "billing_error"];

impl View {
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
                Some(h.session_id.clone())
            }
            Event::DaemonSeen(d) => {
                self.apply_daemon(at, d);
                Some(d.session_id.clone())
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
                Some(session_id.clone())
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
                Some(session_id.clone())
            }
            Event::QuestionAsked(q) => {
                let node = self.node_mut(&q.session_id, at);
                if q.blocking && q.origin == QuestionOrigin::Ask {
                    node.turn_signal = Some(TurnSignal::Asked);
                }
                let mut q = q.clone();
                if q.node_name.is_none() {
                    q.node_name = node.name.clone();
                }
                let sid = q.session_id.clone();
                self.questions.insert(q.id, q);
                Some(sid)
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
            Event::QuestionClosed { id, status } => self.question_update(*id, |q| {
                q.status = *status;
            }),
            Event::ReportPosted(r) => {
                let node = self.node_mut(&r.session_id, at);
                node.turn_signal = Some(match r.kind {
                    ReportKind::Final => TurnSignal::Finished,
                    ReportKind::Delivery | ReportKind::Finding => TurnSignal::Delivered,
                    ReportKind::Progress => node.turn_signal.unwrap_or(TurnSignal::Delivered),
                });
                let mut r = r.clone();
                if r.node_name.is_none() {
                    r.node_name = node.name.clone();
                }
                let sid = r.session_id.clone();
                self.reports.insert(r.id, r);
                Some(sid)
            }
            Event::ReportRead { id } => {
                if let Some(r) = self.reports.get_mut(id) {
                    r.read = true;
                }
                None
            }
            Event::MessageQueued(m) => {
                self.messages.insert(m.id, m.clone());
                Some(m.session_id.clone())
            }
            Event::MessageHanded { id } => {
                let mut sid = None;
                if let Some(m) = self.messages.get_mut(id) {
                    m.handed_at = Some(at);
                    sid = Some(m.session_id.clone());
                }
                sid
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
                Some(session_id.clone())
            }
            Event::WaitingMachine { session_id, .. } => {
                self.node_mut(session_id, at).turn_signal = Some(TurnSignal::WaitingMachine);
                Some(session_id.clone())
            }
        };
        if let Some(sid) = touched {
            self.recompute(&sid, at);
        }
    }

    fn question_update(&mut self, id: u64, f: impl FnOnce(&mut Question)) -> Option<String> {
        let q = self.questions.get_mut(&id)?;
        f(q);
        Some(q.session_id.clone())
    }

    fn node_mut(&mut self, session_id: &str, at: Millis) -> &mut Node {
        self.nodes
            .entry(session_id.to_string())
            .or_insert_with(|| Node::new(session_id, at))
    }

    fn apply_hook(&mut self, at: Millis, h: &HookEvent) {
        let delivered: Vec<u64> = self
            .messages
            .values()
            .filter(|m| m.session_id == h.session_id && m.handed_at.is_some() && m.delivered_at.is_none())
            .map(|m| m.id)
            .collect();
        let starts_work = matches!(h.event.as_str(), "UserPromptSubmit" | "PreToolUse");
        if starts_work {
            for id in &delivered {
                if let Some(m) = self.messages.get_mut(id) {
                    m.delivered_at = Some(at);
                }
            }
        }
        let node = self.node_mut(&h.session_id, at);
        node.has_plugin = true;
        node.alive = true;
        node.last_seen = at;
        if h.cwd.is_some() {
            node.cwd = h.cwd.clone();
        }
        match h.event.as_str() {
            "SessionStart" => {
                node.busy = false;
                node.tool = None;
            }
            "UserPromptSubmit" => {
                node.busy = true;
                node.turn += 1;
                node.turn_signal = None;
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
                if h.last_message.is_some() {
                    node.last_message = h.last_message.clone();
                    node.last_message_at = Some(at);
                }
            }
            "StopFailure" => {
                node.busy = false;
                node.tool = None;
                if h.error_type
                    .as_deref()
                    .is_some_and(|e| USAGE_LIMIT_ERRORS.contains(&e))
                {
                    node.usage_limited = true;
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
        node.daemon_detail = d.detail.clone();
        node.daemon_needs = d.needs.clone();
        node.daemon_updated_at = d.updated_at;
        if !node.has_plugin && d.last_text.is_some() {
            node.last_message = d.last_text.clone();
            node.last_message_at = d.last_text_at.or(d.updated_at);
        }
        if !node.has_plugin {
            node.last_seen = node.last_seen.max(d.updated_at.unwrap_or(at));
            node.busy = d.tempo.as_deref() == Some("active");
            node.usage_limited = false;
            node.turn_signal = match d.state.as_deref() {
                Some("done") => Some(TurnSignal::Delivered),
                _ => None,
            };
        }
    }

    fn recompute(&mut self, session_id: &str, at: Millis) {
        let open_blocking = self.questions.values().any(|q| {
            q.session_id == session_id && q.status.is_open() && q.blocking && !q.to_orchestrator
        });
        let Some(node) = self.nodes.get_mut(session_id) else { return };
        let (state, source) = derive(node, open_blocking);
        if state != node.state {
            node.state = state;
            node.state_since = match source {
                StateSource::Daemon => node.daemon_updated_at.unwrap_or(at).min(at),
                _ => at,
            };
        }
        node.state_source = source;
    }

    pub fn news(&self, now: Millis) -> Vec<&Node> {
        let mut v: Vec<&Node> = self.nodes.values().filter(|n| n.has_news(now)).collect();
        v.sort_by_key(|n| std::cmp::Reverse(n.last_message_at.unwrap_or(n.state_since).max(n.state_since)));
        v
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

pub fn derive(node: &Node, open_blocking_question: bool) -> (NodeState, StateSource) {
    let hooks_or_daemon = if node.has_plugin { StateSource::Hooks } else { StateSource::Daemon };
    if open_blocking_question || node.waiting_permission {
        return (NodeState::WaitingAnswer, hooks_or_daemon);
    }
    if node.busy || node.tool.is_some() {
        return (NodeState::Working, hooks_or_daemon);
    }
    if node.usage_limited {
        return (NodeState::WaitingMachine, hooks_or_daemon);
    }
    match node.turn_signal {
        Some(TurnSignal::Finished) => return (NodeState::Done, hooks_or_daemon),
        Some(TurnSignal::Delivered) => return (NodeState::Delivered, hooks_or_daemon),
        Some(TurnSignal::WaitingMachine) => return (NodeState::WaitingMachine, hooks_or_daemon),
        Some(TurnSignal::Asked) => return (NodeState::Working, hooks_or_daemon),
        None => {}
    }
    if !node.has_plugin && node.daemon_needs.as_deref().is_some_and(|n| !n.trim().is_empty()) {
        return (NodeState::WaitingAnswer, StateSource::Daemon);
    }
    if let Some(c) = &node.classified
        && c.turn == node.turn
    {
        return (c.state, StateSource::Classifier);
    }
    if node.has_plugin && node.turn == 0 && node.last_message.is_none() {
        return (NodeState::Working, StateSource::Hooks);
    }
    (NodeState::Unclear, hooks_or_daemon)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Answer, QuestionOrigin};

    fn hook(sid: &str, event: &str) -> Event {
        Event::Hook(HookEvent { session_id: sid.into(), event: event.into(), ..Default::default() })
    }

    fn question(id: u64, sid: &str, origin: QuestionOrigin) -> Question {
        Question {
            id,
            session_id: sid.into(),
            node_name: None,
            title: "¿Mergeo?".into(),
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
            answer: Answer { text: "sí".into(), option: None, by: None, device: None, at: 9000 },
        });
        assert_ne!(v.nodes["s"].state, NodeState::WaitingAnswer);
    }

    #[test]
    fn turn_without_signal_is_unclear_not_frozen_working() {
        let v = run(&[hook("s", "UserPromptSubmit"), hook("s", "Stop")]);
        assert_eq!(v.nodes["s"].state, NodeState::Unclear);
    }

    #[test]
    fn classification_expires_with_next_turn() {
        let mut v = run(&[hook("s", "UserPromptSubmit"), hook("s", "Stop")]);
        v.apply(3, 3000, &Event::Classified {
            session_id: "s".into(),
            turn: 1,
            state: NodeState::Delivered,
            confidence: Some(0.9),
        });
        assert_eq!(v.nodes["s"].state, NodeState::Delivered);
        v.apply(4, 4000, &hook("s", "UserPromptSubmit"));
        v.apply(5, 5000, &hook("s", "Stop"));
        assert_eq!(v.nodes["s"].state, NodeState::Unclear);
    }

    #[test]
    fn stale_classification_for_old_turn_is_ignored() {
        let mut v = run(&[
            hook("s", "UserPromptSubmit"),
            hook("s", "Stop"),
            hook("s", "UserPromptSubmit"),
            hook("s", "Stop"),
        ]);
        v.apply(9, 9000, &Event::Classified {
            session_id: "s".into(),
            turn: 1,
            state: NodeState::Delivered,
            confidence: None,
        });
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
            title: "Hecho".into(),
            body: None,
            links: vec![],
            attachments: vec![],
            kind: ReportKind::Final,
            created_at: 2000,
            read: false,
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
    fn daemon_only_node_with_needs_is_waiting_answer() {
        let v = run(&[Event::DaemonSeen(DaemonObservation {
            session_id: "s".into(),
            short_id: "s".into(),
            state: Some("blocked".into()),
            tempo: Some("blocked".into()),
            needs: Some("decide merge".into()),
            pid: Some(7),
            ..Default::default()
        })]);
        assert_eq!(v.nodes["s"].state, NodeState::WaitingAnswer);
        assert_eq!(v.nodes["s"].state_source, StateSource::Daemon);
    }

    #[test]
    fn message_delivered_only_when_node_starts_working() {
        let mut v = run(&[hook("s", "UserPromptSubmit"), hook("s", "Stop")]);
        v.apply(3, 3000, &Event::MessageQueued(Message {
            id: 1,
            session_id: "s".into(),
            from: crate::model::MessageFrom::David,
            text: "sigue".into(),
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
    fn dormant_daemon_node_keeps_its_state() {
        let v = run(&[Event::DaemonSeen(DaemonObservation {
            session_id: "s".into(),
            short_id: "s".into(),
            state: Some("blocked".into()),
            needs: Some("decide".into()),
            pid: None,
            ..Default::default()
        })]);
        assert_eq!(v.nodes["s"].state, NodeState::WaitingAnswer);
        assert!(!v.nodes["s"].alive);
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
    fn finished_daemon_node_without_process_is_delivered() {
        let v = run(&[Event::DaemonSeen(DaemonObservation {
            session_id: "s".into(),
            short_id: "s".into(),
            state: Some("done".into()),
            pid: None,
            ..Default::default()
        })]);
        assert_eq!(v.nodes["s"].state, NodeState::Delivered);
    }

    #[test]
    fn daemon_classification_expires_when_detail_changes() {
        let obs = |detail: &str| Event::DaemonSeen(DaemonObservation {
            session_id: "s".into(),
            short_id: "s".into(),
            state: Some("working".into()),
            tempo: Some("idle".into()),
            detail: Some(detail.into()),
            pid: Some(1),
            ..Default::default()
        });
        let mut v = run(&[obs("awaiting machine")]);
        assert_eq!(v.nodes["s"].state, NodeState::Unclear);
        v.apply(2, 2000, &Event::Classified { session_id: "s".into(), turn: 0, state: NodeState::WaitingMachine, confidence: Some(0.9) });
        assert_eq!(v.nodes["s"].state, NodeState::WaitingMachine);
        v.apply(3, 3000, &obs("something else"));
        assert_eq!(v.nodes["s"].state, NodeState::Unclear);
    }

    #[test]
    fn delivered_node_is_news_until_acknowledged_and_again_after_new_turn() {
        let mut v = run(&[hook("s", "UserPromptSubmit")]);
        v.apply(2, 2000, &Event::Classified { session_id: "s".into(), turn: 1, state: NodeState::Delivered, confidence: None });
        v.apply(3, 3000, &Event::Hook(HookEvent { session_id: "s".into(), event: "Stop".into(), last_message: Some("PR #4 lista".into()), ..Default::default() }));
        v.apply(4, 4000, &Event::Classified { session_id: "s".into(), turn: 1, state: NodeState::Delivered, confidence: None });
        assert_eq!(v.news(9000).len(), 1);
        v.apply(5, 5000, &Event::Acknowledged { session_id: "s".into() });
        assert!(v.news(9000).is_empty());
        v.apply(6, 6000, &hook("s", "UserPromptSubmit"));
        v.apply(7, 7000, &Event::Hook(HookEvent { session_id: "s".into(), event: "Stop".into(), last_message: Some("otra cosa".into()), ..Default::default() }));
        v.apply(8, 8000, &Event::Classified { session_id: "s".into(), turn: 2, state: NodeState::Delivered, confidence: None });
        assert_eq!(v.news(9000).len(), 1);
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
}
