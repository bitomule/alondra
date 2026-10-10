//! A node is never stopped and working at once. Every forbidden transition of the node machine is
//! refused, and generated runs of launches, turns (some reported late), cards, deliveries, pauses,
//! resumes, sessions that vanish or close, daemon restarts and listings of the real sessions keep
//! one state per node that the gate, Fleet, the Tasks list and the header all count alike.
use super::*;
use crate::views::{Client, Item, Lang, LiveSnapshot, RenderContext, Transport, View};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestError, TestRng, TestRunner};
use std::collections::{BTreeMap, BTreeSet};

const OPEN: TaskFacts = TaskFacts { open: true, closing: false, held: false, card: false, delivered: false };
const SINCE: Millis = 1_000_000;
const AT: Millis = SINCE + 1;

fn pause(reason: PauseReason) -> Pause {
    Pause { reason, since: SINCE, cut: None, turn_since: None, cleanup: PauseCleanup::Pending, released: false }
}

fn phases() -> Vec<NodePhase> {
    vec![
        NodePhase::Starting { reviving: false },
        NodePhase::Starting { reviving: true },
        NodePhase::Working,
        NodePhase::WaitingUser { in_turn: true },
        NodePhase::WaitingUser { in_turn: false },
        NodePhase::Paused(pause(PauseReason::Memory)),
        NodePhase::Paused(pause(PauseReason::ByUser)),
        NodePhase::Delivered,
        NodePhase::Stopped,
        NodePhase::Closing,
        NodePhase::Closed,
    ]
}

fn events() -> Vec<NodeEvent> {
    let mut all = vec![
        NodeEvent::TurnStarted, NodeEvent::TurnEnded, NodeEvent::SessionLost, NodeEvent::SessionClosed,
        NodeEvent::StartFailed, NodeEvent::RevivalFailed, NodeEvent::Paused { pause: pause(PauseReason::Usage) },
        NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done }, NodeEvent::PauseReleased, NodeEvent::Resumed,
    ];
    all.extend([Listed::Working, Listed::Idle, Listed::Blocked, Listed::Absent].map(|listed| NodeEvent::Listed { listed }));
    all
}

/// The machine's table of refusals, written apart from `next`: the events each phase refuses.
fn forbidden(phase: &NodePhase, event: &NodeEvent) -> Option<&'static str> {
    use NodeEvent as E;
    use NodePhase as P;
    let paused = matches!(phase, P::Paused(_));
    match (phase, event) {
        (P::Closed, _) => Some("node_closed"),
        (P::Paused(Pause { reason: PauseReason::ByUser, .. }), E::TurnStarted) => Some("paused_by_user"),
        (P::Starting { .. }, E::StartFailed | E::RevivalFailed) => None,
        (_, E::StartFailed | E::RevivalFailed) => Some("not_starting"),
        (P::Paused(_), E::Paused { .. }) => Some("already_paused"),
        (P::Starting { .. } | P::Working | P::Stopped, E::Paused { .. }) => None,
        (_, E::Paused { .. }) => Some("not_working"),
        (_, E::PauseCleanup { .. } | E::PauseReleased | E::Resumed) if !paused => Some("not_paused"),
        _ => None,
    }
}

fn refused(phase: NodePhase, event: NodeEvent, at: Millis) -> String {
    match next(&phase, SINCE, &event, &OPEN, at) {
        Ok(moved) => panic!("{phase:?} accepted {event:?} and moved to {moved:?}"),
        Err(error) => error.code,
    }
}

macro_rules! forbid {
    ($($name:ident: $phase:expr, $event:expr => $code:literal;)*) => {$(
        #[test]
        fn $name() {
            assert_eq!(refused($phase, $event, AT), $code);
        }
    )*};
}

forbid! {
    closed_refuses_a_turn_start: NodePhase::Closed, NodeEvent::TurnStarted => "node_closed";
    closed_refuses_a_turn_end: NodePhase::Closed, NodeEvent::TurnEnded => "node_closed";
    closed_refuses_a_lost_session: NodePhase::Closed, NodeEvent::SessionLost => "node_closed";
    closed_refuses_closing_again: NodePhase::Closed, NodeEvent::SessionClosed => "node_closed";
    closed_refuses_a_failed_start: NodePhase::Closed, NodeEvent::StartFailed => "node_closed";
    closed_refuses_a_failed_revival: NodePhase::Closed, NodeEvent::RevivalFailed => "node_closed";
    closed_refuses_a_pause: NodePhase::Closed, NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "node_closed";
    closed_refuses_a_pause_cleanup: NodePhase::Closed, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "node_closed";
    closed_refuses_a_pause_release: NodePhase::Closed, NodeEvent::PauseReleased => "node_closed";
    closed_refuses_a_resume: NodePhase::Closed, NodeEvent::Resumed => "node_closed";
    closed_refuses_a_listed_working_session: NodePhase::Closed, NodeEvent::Listed { listed: Listed::Working } => "node_closed";
    closed_refuses_a_listed_idle_session: NodePhase::Closed, NodeEvent::Listed { listed: Listed::Idle } => "node_closed";
    closed_refuses_a_listed_blocked_session: NodePhase::Closed, NodeEvent::Listed { listed: Listed::Blocked } => "node_closed";
    closed_refuses_an_absent_session: NodePhase::Closed, NodeEvent::Listed { listed: Listed::Absent } => "node_closed";

    paused_by_the_user_refuses_a_turn: NodePhase::Paused(pause(PauseReason::ByUser)), NodeEvent::TurnStarted => "paused_by_user";

    working_refuses_a_failed_start: NodePhase::Working, NodeEvent::StartFailed => "not_starting";
    working_refuses_a_failed_revival: NodePhase::Working, NodeEvent::RevivalFailed => "not_starting";
    waiting_in_turn_refuses_a_failed_start: NodePhase::WaitingUser { in_turn: true }, NodeEvent::StartFailed => "not_starting";
    waiting_in_turn_refuses_a_failed_revival: NodePhase::WaitingUser { in_turn: true }, NodeEvent::RevivalFailed => "not_starting";
    waiting_refuses_a_failed_start: NodePhase::WaitingUser { in_turn: false }, NodeEvent::StartFailed => "not_starting";
    waiting_refuses_a_failed_revival: NodePhase::WaitingUser { in_turn: false }, NodeEvent::RevivalFailed => "not_starting";
    paused_refuses_a_failed_start: NodePhase::Paused(pause(PauseReason::Memory)), NodeEvent::StartFailed => "not_starting";
    paused_refuses_a_failed_revival: NodePhase::Paused(pause(PauseReason::Memory)), NodeEvent::RevivalFailed => "not_starting";
    paused_by_the_user_refuses_a_failed_start: NodePhase::Paused(pause(PauseReason::ByUser)), NodeEvent::StartFailed => "not_starting";
    paused_by_the_user_refuses_a_failed_revival: NodePhase::Paused(pause(PauseReason::ByUser)), NodeEvent::RevivalFailed => "not_starting";
    delivered_refuses_a_failed_start: NodePhase::Delivered, NodeEvent::StartFailed => "not_starting";
    delivered_refuses_a_failed_revival: NodePhase::Delivered, NodeEvent::RevivalFailed => "not_starting";
    stopped_refuses_a_failed_start: NodePhase::Stopped, NodeEvent::StartFailed => "not_starting";
    stopped_refuses_a_failed_revival: NodePhase::Stopped, NodeEvent::RevivalFailed => "not_starting";
    closing_refuses_a_failed_start: NodePhase::Closing, NodeEvent::StartFailed => "not_starting";
    closing_refuses_a_failed_revival: NodePhase::Closing, NodeEvent::RevivalFailed => "not_starting";

    paused_refuses_a_second_pause: NodePhase::Paused(pause(PauseReason::Memory)), NodeEvent::Paused { pause: pause(PauseReason::Usage) } => "already_paused";
    paused_by_the_user_refuses_a_second_pause: NodePhase::Paused(pause(PauseReason::ByUser)), NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "already_paused";
    waiting_in_turn_refuses_a_pause: NodePhase::WaitingUser { in_turn: true }, NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "not_working";
    waiting_refuses_a_pause: NodePhase::WaitingUser { in_turn: false }, NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "not_working";
    delivered_refuses_a_pause: NodePhase::Delivered, NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "not_working";
    closing_refuses_a_pause: NodePhase::Closing, NodeEvent::Paused { pause: pause(PauseReason::Memory) } => "not_working";

    starting_refuses_a_pause_cleanup: NodePhase::Starting { reviving: false }, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    starting_refuses_a_pause_release: NodePhase::Starting { reviving: false }, NodeEvent::PauseReleased => "not_paused";
    starting_refuses_a_resume: NodePhase::Starting { reviving: false }, NodeEvent::Resumed => "not_paused";
    reviving_refuses_a_pause_cleanup: NodePhase::Starting { reviving: true }, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    reviving_refuses_a_pause_release: NodePhase::Starting { reviving: true }, NodeEvent::PauseReleased => "not_paused";
    reviving_refuses_a_resume: NodePhase::Starting { reviving: true }, NodeEvent::Resumed => "not_paused";
    working_refuses_a_pause_cleanup: NodePhase::Working, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    working_refuses_a_pause_release: NodePhase::Working, NodeEvent::PauseReleased => "not_paused";
    working_refuses_a_resume: NodePhase::Working, NodeEvent::Resumed => "not_paused";
    waiting_in_turn_refuses_a_pause_cleanup: NodePhase::WaitingUser { in_turn: true }, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    waiting_in_turn_refuses_a_pause_release: NodePhase::WaitingUser { in_turn: true }, NodeEvent::PauseReleased => "not_paused";
    waiting_in_turn_refuses_a_resume: NodePhase::WaitingUser { in_turn: true }, NodeEvent::Resumed => "not_paused";
    waiting_refuses_a_pause_cleanup: NodePhase::WaitingUser { in_turn: false }, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    waiting_refuses_a_pause_release: NodePhase::WaitingUser { in_turn: false }, NodeEvent::PauseReleased => "not_paused";
    waiting_refuses_a_resume: NodePhase::WaitingUser { in_turn: false }, NodeEvent::Resumed => "not_paused";
    delivered_refuses_a_pause_cleanup: NodePhase::Delivered, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    delivered_refuses_a_pause_release: NodePhase::Delivered, NodeEvent::PauseReleased => "not_paused";
    delivered_refuses_a_resume: NodePhase::Delivered, NodeEvent::Resumed => "not_paused";
    stopped_refuses_a_pause_cleanup: NodePhase::Stopped, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    stopped_refuses_a_pause_release: NodePhase::Stopped, NodeEvent::PauseReleased => "not_paused";
    stopped_refuses_a_resume: NodePhase::Stopped, NodeEvent::Resumed => "not_paused";
    closing_refuses_a_pause_cleanup: NodePhase::Closing, NodeEvent::PauseCleanup { cleanup: PauseCleanup::Done } => "not_paused";
    closing_refuses_a_pause_release: NodePhase::Closing, NodeEvent::PauseReleased => "not_paused";
    closing_refuses_a_resume: NodePhase::Closing, NodeEvent::Resumed => "not_paused";
}

macro_rules! stale {
    ($($name:ident: $phase:expr, $event:expr;)*) => {$(
        #[test]
        fn $name() {
            assert_eq!(refused($phase, $event, SINCE - 1), "stale");
        }
    )*};
}

stale! {
    a_late_turn_start_never_moves_a_paused_node: NodePhase::Paused(pause(PauseReason::Memory)), NodeEvent::TurnStarted;
    a_late_turn_start_never_moves_a_waiting_node: NodePhase::WaitingUser { in_turn: false }, NodeEvent::TurnStarted;
    a_late_turn_start_never_moves_a_delivered_node: NodePhase::Delivered, NodeEvent::TurnStarted;
    a_late_turn_start_never_moves_a_stopped_node: NodePhase::Stopped, NodeEvent::TurnStarted;
    a_late_turn_end_never_moves_a_working_node: NodePhase::Working, NodeEvent::TurnEnded;
    a_late_lost_session_never_moves_a_working_node: NodePhase::Working, NodeEvent::SessionLost;
    a_late_lost_session_never_moves_a_revived_node: NodePhase::Starting { reviving: false }, NodeEvent::SessionLost;
}

/// The refusal table covers every pair: what it does not forbid, the machine accepts.
#[test]
fn every_transition_the_table_allows_is_accepted_and_every_other_refused() {
    for phase in phases() {
        for event in events() {
            let moved = next(&phase, SINCE, &event, &OPEN, AT);
            match forbidden(&phase, &event) {
                Some(code) => assert_eq!(moved.err().map(|e| e.code).as_deref(), Some(code), "{phase:?} + {event:?}"),
                None => assert!(moved.is_ok(), "{phase:?} + {event:?}: {moved:?}"),
            }
        }
    }
}

#[test]
fn a_paused_node_holds_no_slot_and_only_a_turn_it_began_after_the_pause_brings_it_back() {
    let paused = NodePhase::Paused(pause(PauseReason::Declared { reason: "The build queue is full".into() }));
    assert!(!paused.holds_slot());
    assert_eq!(next(&paused, SINCE, &NodeEvent::TurnEnded, &OPEN, AT).unwrap(), paused, "the turn the pause cut ends");
    assert_eq!(next(&paused, SINCE, &NodeEvent::Listed { listed: Listed::Working }, &OPEN, AT).unwrap(), paused,
        "a listing has no turn times: it never undoes a pause");
    assert_eq!(next(&paused, SINCE, &NodeEvent::TurnStarted, &OPEN, AT).unwrap(), NodePhase::Working, "it went on by itself");
}

#[test]
fn an_open_card_or_a_delivery_takes_the_node_out_of_the_slots_even_inside_a_turn() {
    let card = TaskFacts { card: true, ..OPEN };
    assert_eq!(next(&NodePhase::Stopped, SINCE, &NodeEvent::TurnStarted, &card, AT).unwrap(), NodePhase::WaitingUser { in_turn: true });
    let delivered = TaskFacts { delivered: true, ..OPEN };
    assert_eq!(next(&NodePhase::Working, SINCE, &NodeEvent::TurnEnded, &delivered, AT).unwrap(), NodePhase::Delivered);
    assert_eq!(settle(NodePhase::WaitingUser { in_turn: true }, &OPEN), NodePhase::Working, "the answer lets its turn go on");
    assert_eq!(settle(NodePhase::WaitingUser { in_turn: false }, &OPEN), NodePhase::Stopped);
}

#[test]
fn a_session_missing_from_the_listing_frees_its_slot_once_its_start_had_time() {
    let starting = NodePhase::Starting { reviving: false };
    let absent = NodeEvent::Listed { listed: Listed::Absent };
    assert_eq!(next(&starting, SINCE, &absent, &OPEN, SINCE + 1).unwrap(), starting, "a start in progress is not listed yet");
    assert_eq!(next(&starting, SINCE, &absent, &OPEN, SINCE + START_GRACE).unwrap(), NodePhase::Starting { reviving: true });
    let reviving = NodePhase::Starting { reviving: true };
    assert_eq!(next(&reviving, SINCE, &absent, &OPEN, SINCE + 24 * 60 * 60_000).unwrap(), reviving,
        "a node being brought back holds no slot and waits for its revival to end");
    assert_eq!(next(&NodePhase::Working, SINCE, &absent, &TaskFacts::default(), AT).unwrap(), NodePhase::Closed,
        "a node without an open task whose session is gone is closed");
}

#[derive(Clone, Debug)]
enum Step {
    Launch,
    Turn { pick: usize, start: bool, late: bool },
    Ask(usize),
    Answer(usize),
    Deliver(usize),
    Accept(usize),
    Pause { pick: usize, by_user: bool },
    Resume(usize),
    Vanish(usize),
    Lost(usize),
    UserCloses(usize),
    Cancel(usize),
    Tick(i64),
    Reconcile,
    Restart,
    Replay,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        3 => Just(Step::Launch),
        6 => (0usize..8, any::<bool>(), prop::bool::weighted(0.25)).prop_map(|(pick, start, late)| Step::Turn { pick, start, late }),
        2 => (0usize..8).prop_map(Step::Ask),
        2 => (0usize..8).prop_map(Step::Answer),
        2 => (0usize..8).prop_map(Step::Deliver),
        1 => (0usize..8).prop_map(Step::Accept),
        2 => (0usize..8, prop::bool::weighted(0.3)).prop_map(|(pick, by_user)| Step::Pause { pick, by_user }),
        2 => (0usize..8).prop_map(Step::Resume),
        2 => (0usize..8).prop_map(Step::Vanish),
        1 => (0usize..8).prop_map(Step::Lost),
        1 => (0usize..8).prop_map(Step::UserCloses),
        1 => (0usize..8).prop_map(Step::Cancel),
        2 => (1i64..20).prop_map(|minutes| Step::Tick(minutes * 60_000)),
        2 => Just(Step::Reconcile),
        1 => Just(Step::Restart),
        2 => Just(Step::Replay),
    ]
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

struct World {
    s: DomainState,
    now: Millis,
    /// The sessions Claude Code really runs, as `claude agents` would list them.
    real: BTreeMap<AgentId, Listed>,
    nodes: usize,
    last: Option<(AgentId, NodeEvent, Millis)>,
}

impl World {
    fn new() -> World {
        let mut w = World { s: DomainState::new("epoch"), now: SINCE, real: BTreeMap::new(), nodes: 0, last: None };
        w.must(user(), Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        w.must(Actor::Orchestrator { generation: 0 }, Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
        w
    }

    fn exec(&mut self, actor: Actor, command: Command) -> Result<CommandOutcome, DomainError> {
        self.s.execute(&actor, &CommandEnvelope {
            protocol_version: 2, data_epoch: self.s.data_epoch.clone(), command_id: format!("c-{}", self.s.revision).into(),
            expected_revision: self.s.revision, command,
        })
    }

    fn must(&mut self, actor: Actor, command: Command) {
        let shown = format!("{command:?}");
        if let Err(error) = self.exec(actor, command) {
            panic!("{shown} was refused: {error}");
        }
    }

    fn event(&mut self, agent: &AgentId, event: NodeEvent, at: Millis) -> bool {
        let accepted = self.exec(Actor::System, Command::MoveNode { agent_id: agent.clone(), event: event.clone(), at }).is_ok();
        if accepted {
            self.last = Some((agent.clone(), event, at));
        }
        accepted
    }

    fn pick(&self, n: usize, keep: impl Fn(&Agent, Option<&Task>) -> bool) -> Option<AgentId> {
        let found: Vec<AgentId> = self.s.agents.values().filter(|a| keep(a, self.s.node_task(&a.id))).map(|a| a.id.clone()).collect();
        (!found.is_empty()).then(|| found[n % found.len()].clone())
    }

    fn open_task(&self, agent: &AgentId) -> Option<TaskId> {
        self.s.node_task(agent).filter(|t| !t.state.terminal()).map(|t| t.id.clone())
    }

    fn listing(&self) -> BTreeMap<AgentId, Listed> {
        self.s.agents.keys().map(|id| (id.clone(), self.real.get(id).copied().unwrap_or(Listed::Absent))).collect()
    }

    fn reconcile(&mut self) {
        let listed = self.listing();
        self.must(Actor::System, Command::ReconcileNodes { listed: listed.clone(), at: self.now });
        let again = self.exec(Actor::System, Command::ReconcileNodes { listed, at: self.now }).unwrap();
        assert!(!again.changed, "the same listing twice moves nothing the second time");
        for (id, agent) in &self.s.agents {
            if self.real.contains_key(id) {
                continue;
            }
            let young = self.now - agent.since < START_GRACE;
            assert!(match agent.phase {
                NodePhase::Closed | NodePhase::Paused(_) | NodePhase::Starting { reviving: true } => true,
                NodePhase::Starting { reviving: false } => young,
                _ => false,
            }, "{id:?} has no session yet is {:?}", agent.phase);
        }
    }

    fn apply(&mut self, step: &Step) {
        self.now += 1_000;
        if !matches!(step, Step::Replay) {
            self.last = None;
        }
        match step.clone() {
            Step::Launch => {
                self.nodes += 1;
                let (agent, task): (AgentId, TaskId) = (format!("node-{}", self.nodes).into(), format!("t-{}", self.nodes).into());
                self.must(Actor::Orchestrator { generation: 0 }, Command::CreateTask {
                    task_id: task.clone(), request_id: "r".into(), title: format!("Task {}", self.nodes), agent_id: None, review_prs: false,
                    dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
                });
                self.must(Actor::System, Command::RegisterAgent { agent_id: agent.clone(), name: format!("node {}", self.nodes), at: self.now });
                assert_eq!(self.s.node_phase(&agent), Some(&NodePhase::Starting { reviving: false }));
                self.must(Actor::System, Command::AssignTask { task_id: task.clone(), agent_id: agent.clone(), model: None });
                self.must(Actor::Agent { agent_id: agent.clone() }, Command::StartTask { task_id: task });
                self.real.insert(agent, Listed::Idle);
            }
            Step::Turn { pick, start, late } => {
                let Some(agent) = self.pick(pick, |a, _| a.phase.live()) else { return };
                let at = if late { self.s.agents[&agent].since - 1 } else { self.now };
                let event = if start { NodeEvent::TurnStarted } else { NodeEvent::TurnEnded };
                let before = self.s.agents[&agent].clone();
                let accepted = self.event(&agent, event, at);
                if late {
                    assert!(!accepted || self.s.agents[&agent] == before, "a late report moved {agent:?}");
                }
                if accepted && !late && self.real.contains_key(&agent) {
                    self.real.insert(agent, if start { Listed::Working } else { Listed::Idle });
                }
            }
            Step::Ask(n) => {
                let Some(agent) = self.pick(n, |a, t| a.phase.live() && t.is_some_and(|t| t.state == TaskState::Running)) else { return };
                let task = self.open_task(&agent).unwrap();
                let _ = self.exec(Actor::Agent { agent_id: agent }, Command::AskQuestion {
                    attachments: vec![], interaction_id: format!("q-{}", self.s.revision).into(), scope: Scope::Task { task_id: task },
                    recipient: Recipient::User, body: "Which name?".into(), blocking: false,
                    options: vec![
                        QuestionOption { id: "a".into(), label: "Atlas".into(), description: None, inputs: vec![] },
                        QuestionOption { id: "b".into(), label: "Borealis".into(), description: None, inputs: vec![] },
                    ],
                    card: CardSpec::new(CardKind::Decide, "Choose the release name").why("The name goes on the release"),
                });
            }
            Step::Answer(n) => {
                let Some(agent) = self.pick(n, |a, t| t.is_some_and(|t| a.phase.live() && !t.state.terminal())) else { return };
                let task = self.open_task(&agent).unwrap();
                let Some(card) = self.s.open_user_card(&task).map(|i| i.id.clone()) else { return };
                self.must(user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card, option_id: "a".into(), inputs: BTreeMap::new() });
            }
            Step::Deliver(n) => {
                let Some(agent) = self.pick(n, |a, t| a.phase.live() && t.is_some_and(|t| t.state == TaskState::Running)) else { return };
                let task = self.open_task(&agent).unwrap();
                let _ = self.exec(Actor::Agent { agent_id: agent }, Command::PresentResult {
                    checklist: Vec::new(),
                    attachments: vec![], task_id: task.clone(), interaction_id: format!("final-{task}-{}", self.s.revision).into(),
                    body: "Done".into(), targets: vec![DeliveryTarget::Text { text: "Done".into() }], is_final: true,
                    nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], true),
                });
            }
            Step::Accept(n) => {
                let Some(agent) = self.pick(n, |_, t| t.is_some_and(|t| t.state == TaskState::ForYourReview)) else { return };
                let task = self.open_task(&agent).unwrap();
                let presentation = self.s.tasks[&task].final_presentation.clone().unwrap();
                self.must(user(), Command::AcceptDelivery { presentation_id: presentation, targets: vec![DeliveryTarget::Text { text: "Done".into() }] });
                if self.s.tasks[&task].state == TaskState::Closing {
                    self.must(Actor::System, Command::RecordNodeEnd { task_id: task, end: NodeEnd::Closed, agent_id: Some(agent.clone()) });
                }
                if self.s.node_phase(&agent).is_some_and(NodePhase::live) {
                    self.event(&agent, NodeEvent::SessionClosed, self.now);
                }
                self.real.remove(&agent);
            }
            Step::Pause { pick, by_user } => {
                let Some(agent) = self.pick(pick, |a, _| a.phase.holds_slot()) else { return };
                let reason = if by_user { PauseReason::ByUser } else { PauseReason::Memory };
                let pause = Pause { reason, since: self.now, cut: None, turn_since: None, cleanup: PauseCleanup::Pending, released: false };
                assert!(self.event(&agent, NodeEvent::Paused { pause }, self.now), "a node at work can always be paused");
                if self.real.contains_key(&agent) {
                    self.real.insert(agent, Listed::Idle);
                }
            }
            Step::Resume(n) => {
                let Some(agent) = self.pick(n, |a, _| matches!(a.phase, NodePhase::Paused(_))) else { return };
                assert!(self.event(&agent, NodeEvent::Resumed, self.now));
            }
            Step::Vanish(n) => {
                let Some(agent) = self.pick(n, |a, _| self.real.contains_key(&a.id)) else { return };
                self.real.remove(&agent);
            }
            Step::Lost(n) => {
                let Some(agent) = self.pick(n, |a, _| a.phase.live()) else { return };
                self.real.remove(&agent);
                self.event(&agent, NodeEvent::SessionLost, self.now);
            }
            Step::UserCloses(n) => {
                let Some(agent) = self.pick(n, |a, _| a.phase.live()) else { return };
                if let Some(task) = self.open_task(&agent) {
                    let _ = self.exec(Actor::System, Command::ReleaseTask { task_id: task, reason: "node closed by the user".into() });
                }
                self.real.remove(&agent);
                assert!(self.event(&agent, NodeEvent::SessionClosed, self.now));
            }
            Step::Cancel(n) => {
                let Some(agent) = self.pick(n, |_, t| t.is_some_and(|t| !t.state.terminal())) else { return };
                let task = self.open_task(&agent).unwrap();
                self.must(user(), Command::CancelTask { task_id: task });
            }
            Step::Tick(by) => self.now += by,
            Step::Reconcile => self.reconcile(),
            Step::Restart => {
                let saved = serde_json::to_string(&self.s).unwrap();
                self.s = serde_json::from_str(&saved).unwrap();
                self.reconcile();
            }
            Step::Replay => {
                let Some((agent, event, at)) = self.last.clone() else { return };
                let before = self.s.clone();
                let _ = self.exec(Actor::System, Command::MoveNode { agent_id: agent, event: event.clone(), at });
                assert_eq!(self.s.agents, before.agents, "{event:?} delivered twice moves nothing the second time");
            }
        }
    }

    fn render(&self, key: &str) -> View {
        let projection = self.s.projection();
        crate::views::build(key, RenderContext {
            domain: &self.s, projection: &projection, live: &LiveSnapshot::default(), client: Client::Mac, lang: Lang::En,
            transport: Transport::Loopback, cursor: "", rendered_at: "",
        }).unwrap()
    }

    /// After every step: one valid state per node, and the gate, Fleet, the Tasks list and the
    /// header count the same nodes at work.
    fn check(&self) {
        self.s.validate_nodes().unwrap();
        let slots: BTreeSet<String> = self.s.slots().iter().map(|a| a.id.0.clone()).collect();
        assert!(self.s.paused_nodes().keys().all(|id| !slots.contains(&id.0)), "a paused node holds a slot");

        let fleet = self.render("fleet");
        let at_work = ["Working", "Starting", "Merging", "Stalled", "Failed"];
        let in_fleet = badged(&fleet, &at_work);
        assert_eq!(in_fleet, slots, "Fleet shows at work exactly the nodes that hold a slot");
        let live: BTreeSet<String> = self.s.agents.values().filter(|a| a.phase.live()).map(|a| a.id.0.clone()).collect();
        assert_eq!(rows(&fleet).into_iter().filter(|id| self.s.agents.contains_key(&AgentId(id.clone()))).collect::<BTreeSet<_>>(), live,
            "Fleet lists every live node and no closed one");

        let tasks = self.render("tasks");
        let task_slots: BTreeSet<String> = self.s.slots().iter()
            .filter_map(|a| self.s.node_task(&a.id).filter(|t| !t.state.terminal()).map(|t| t.id.0.clone())).collect();
        let listed = badged(&tasks, &["Working", "Making changes", "Resolving conflicts", "Answering you"]);
        assert_eq!(listed, task_slots, "the Tasks list says at work of the tasks whose node holds a slot");

        let header = fleet.status.counts.iter().find(|count| count.id == "working").map(|count| count.value as usize);
        assert_eq!(header, Some(slots.len()), "the header counts the same nodes");
    }
}

fn rows(view: &View) -> Vec<String> {
    fn walk(items: &[Item], out: &mut Vec<(String, Option<String>)>) {
        for item in items {
            if let Item::Row(row) = item {
                out.push((row.id.clone(), row.badges.first().map(|badge| badge.text.clone())));
                children(&row.children, out);
            }
        }
    }
    fn children(rows: &[crate::views::Row], out: &mut Vec<(String, Option<String>)>) {
        for row in rows {
            out.push((row.id.clone(), row.badges.first().map(|badge| badge.text.clone())));
            children(&row.children, out);
        }
    }
    let mut out = vec![];
    for section in &view.sections {
        walk(&section.items, &mut out);
    }
    out.into_iter().map(|(id, _)| id).collect()
}

fn badged(view: &View, states: &[&str]) -> BTreeSet<String> {
    fn walk(items: &[Item], states: &[&str], out: &mut BTreeSet<String>) {
        for item in items {
            if let Item::Row(row) = item {
                row_and_children(row, states, out);
            }
        }
    }
    fn row_and_children(row: &crate::views::Row, states: &[&str], out: &mut BTreeSet<String>) {
        if row.badges.first().is_some_and(|badge| states.contains(&badge.text.as_str())) {
            out.insert(row.id.clone());
        }
        for child in &row.children {
            row_and_children(child, states, out);
        }
    }
    let mut out = BTreeSet::new();
    for section in &view.sections {
        walk(&section.items, states, &mut out);
    }
    out
}

#[test]
fn generated_runs_keep_one_state_per_node_that_every_view_counts_alike() {
    let mut runner = TestRunner::new_with_rng(Config { cases: 256, failure_persistence: None, ..Config::default() },
        TestRng::deterministic_rng(RngAlgorithm::ChaCha));
    let result = runner.run(&prop::collection::vec(step(), 1..60), |steps| {
        let mut world = World::new();
        for step in &steps {
            world.apply(step);
            world.check();
        }
        Ok(())
    });
    if let Err(TestError::Fail(reason, steps)) = result {
        panic!("{reason}\nsteps: {steps:#?}");
    }
}
