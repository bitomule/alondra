//! No open task is ever left in nobody's hands. Generated runs of tasks, dependencies, a scheduled
//! request, gate admissions, starts that fail, nodes that die or the user closes, deliveries,
//! cancellations, reopenings and answers to cards: after every step, with the daemon's reactions applied,
//! every open task has a holder.
use super::*;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestError, TestRng, TestRunner};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
enum Step {
    SdkOpening,
    UncertainOpening(usize),
    Create { scheduled: bool, plan: bool, deps: Vec<usize> },
    Admit(usize),
    LaunchFails { pick: usize, untrusted: bool },
    Tick(i64),
    NodeDies(usize),
    NodeBack(usize),
    RevivalFails(usize),
    AnswerRevival { pick: usize, retry: bool },
    UserClosesNode(usize),
    NodeStops(usize),
    Deliver(usize),
    AcceptAndFinish(usize),
    Cancel(usize),
    Reopen(usize),
    AnswerProblem { pick: usize, option: usize },
    /// What becomes of a task that waits for a cancelled task: the orchestrator re-points it,
    /// drops the wait or cancels it, or leaves it and the inbox asks the user.
    Replan { pick: usize, outcome: usize, onto: usize },
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        2 => Just(Step::SdkOpening),
        2 => (0usize..8).prop_map(Step::UncertainOpening),
        3 => (any::<bool>(), prop::bool::weighted(0.8), prop::collection::vec(0usize..8, 0..3))
            .prop_map(|(scheduled, plan, deps)| Step::Create { scheduled, plan, deps }),
        3 => (0usize..8).prop_map(Step::Admit),
        2 => (0usize..8, prop::bool::weighted(0.2)).prop_map(|(pick, untrusted)| Step::LaunchFails { pick, untrusted }),
        2 => (1i64..30).prop_map(|minutes| Step::Tick(minutes * 60_000)),
        1 => (0usize..8).prop_map(Step::NodeDies),
        1 => (0usize..8).prop_map(Step::NodeBack),
        1 => (0usize..8).prop_map(Step::RevivalFails),
        1 => (0usize..8, any::<bool>()).prop_map(|(pick, retry)| Step::AnswerRevival { pick, retry }),
        1 => (0usize..8).prop_map(Step::UserClosesNode),
        3 => (0usize..8).prop_map(Step::NodeStops),
        2 => (0usize..8).prop_map(Step::Deliver),
        2 => (0usize..8).prop_map(Step::AcceptAndFinish),
        1 => (0usize..8).prop_map(Step::Cancel),
        2 => (0usize..8).prop_map(Step::Reopen),
        2 => (0usize..8, 0usize..3).prop_map(|(pick, option)| Step::AnswerProblem { pick, option }),
        2 => (0usize..8, 0usize..4, 0usize..8).prop_map(|(pick, outcome, onto)| Step::Replan { pick, outcome, onto }),
    ]
}

struct World {
    s: DomainState,
    facts: BTreeMap<TaskId, StartFacts>,
    attempts: BTreeMap<TaskId, u8>,
    /// Turn ends without a result or a question in a row, as the daemon's Stop guard counts them.
    stops: BTreeMap<TaskId, u32>,
    now: i64,
    made: usize,
    nodes: usize,
    sdk_openings: BTreeMap<TaskId, i64>,
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}

impl World {
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

    /// A normal request, and the request of a weekly scheduled run whose own task works.
    fn new() -> World {
        let mut w = World { s: DomainState::new("epoch"), facts: BTreeMap::new(), attempts: BTreeMap::new(), stops: BTreeMap::new(), now: 1_000_000, made: 0, nodes: 0, sdk_openings: BTreeMap::new() };
        w.must(user(), Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        w.must(orchestrator(), Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
        w.must(Actor::System, Command::CreateSchedule { schedule: Schedule {
            paid_account_grant: None,
            id: "weekly".into(), title: "BlurCamera · weekly improvement".into(), text: "Improve BlurCamera".into(), project: Some("BlurCamera".into()),
            options: BTreeMap::new(), disallowed_tools: vec![], frequency: ScheduleFrequency::Daily, hour: 9, minute: 0, weekday: None,
            interval: None, start_at: None, enabled: true, next_run: Some(100), revision: 1, deleted_at: None,
        } });
        w.must(Actor::System, Command::ClaimScheduleRun {
            schedule_id: "weekly".into(), run_id: "run".into(), origin: ScheduleRunOrigin::Due, now: 100, next_after_now: Some(200), expected_schedule_revision: None,
        });
        let mut request = Request::new("weekly".into(), "Improve BlurCamera".into());
        request.title = Some("BlurCamera · weekly improvement".into());
        let task = Task {
            id: "run".into(), request_id: "weekly".into(), agent_id: None, title: "BlurCamera · weekly improvement".into(), state: TaskState::NotStarted,
            revision: 0, started: false, review_prs: false, created_revision: 0, final_presentation: None, pr_id: None, later_prs: vec![], opened_prs: Default::default(), dependencies: BTreeSet::new(), stack: None,
            documents: BTreeMap::new(), archived: false, model: None, waiting_for_tasks: Default::default(), ended_task_waits: Default::default(), delivered_dependencies: Default::default(), blocks: Default::default(),
            verdict: None, review_rounds: 0, wrap_up: WrapUp::default(), priority: Default::default(), ending: None, stalls: 0, stall_closing: None, open_pr: None, continues: None, worktrees: Default::default(), provider: Default::default(), selection: None, pending_selection: None, account: None, allowed: Default::default(), project: None,
        };
        let options = crate::options::OptionsFile::default().resolve(Some("BlurCamera"), &BTreeMap::new()).unwrap();
        w.must(Actor::System, Command::RecordScheduleRunSent {
            run_id: "run".into(), request, task, finished_at: 101,
            launch: ScheduleLaunchIntent { name: "scheduled/weekly-run".into(), cwd: "/synthetic/Projects/BlurCamera".into(),
                prompt: "Improve BlurCamera".into(), options_json: serde_json::to_string(&options).unwrap() },
        });
        w.must(Actor::System, Command::RegisterAgent { agent_id: "weekly-node".into(), name: "scheduled/weekly".into(), at: 0 });
        w.must(Actor::System, Command::ClaimScheduleLaunch { run_id: "run".into(), now: 102 });
        w.must(Actor::System, Command::RecordScheduleLaunchHanded { run_id: "run".into(), session_id: Some("weekly-node".into()),
            agent_id: Some("weekly-node".into()), finished_at: 103 });
        w.must(Actor::Agent { agent_id: "weekly-node".into() }, Command::StartTask { task_id: "run".into() });
        w.facts.insert("run".into(), StartFacts { planned: true, start: Start::Launched, ..Default::default() });
        w
    }

    fn open(&self) -> Vec<TaskId> {
        self.s.tasks.values().filter(|t| !t.state.terminal()).map(|t| t.id.clone()).collect()
    }

    fn pick(&self, n: usize, keep: impl Fn(&Task, &StartFacts) -> bool) -> Option<TaskId> {
        let none = StartFacts::default();
        let found: Vec<TaskId> = self.s.tasks.values()
            .filter(|t| !t.state.terminal() && !self.sdk_openings.contains_key(&t.id) && keep(t, self.facts.get(&t.id).unwrap_or(&none)))
            .map(|t| t.id.clone()).collect();
        (!found.is_empty()).then(|| found[n % found.len()].clone())
    }

    fn fact(&mut self, task: &TaskId) -> &mut StartFacts {
        self.facts.entry(task.clone()).or_default()
    }

    fn unstarted(t: &Task) -> bool {
        t.state == TaskState::NotStarted && t.agent_id.is_none() && !t.started
    }

    /// A launch that worked: a new node registers, gets the task and starts on it.
    fn start_node(&mut self, task: &TaskId) {
        self.nodes += 1;
        let agent: AgentId = format!("node-{}", self.nodes).into();
        self.must(Actor::System, Command::RegisterAgent { agent_id: agent.clone(), name: format!("node {}", self.nodes), at: 0 });
        self.must(Actor::System, Command::AssignTask { task_id: task.clone(), agent_id: agent.clone(), model: None });
        self.must(Actor::Agent { agent_id: agent }, Command::StartTask { task_id: task.clone() });
        self.attempts.remove(task);
        let fact = self.fact(task);
        fact.start = Start::Launched;
        fact.node = NodeFact::Alive;
        fact.closed = false;
    }

    fn agent_of(&self, task: &TaskId) -> Option<AgentId> {
        self.s.tasks.get(task).and_then(|t| t.agent_id.clone())
    }

    fn apply(&mut self, step: &Step) {
        match step.clone() {
            Step::SdkOpening => {
                self.made += 1;
                let id: TaskId = format!("sdk-opening-{}", self.made).into();
                let request = format!("request-{id}");
                self.must(user(), Command::CreateRequest { request_id: request.as_str().into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
                self.must(orchestrator(), Command::TitleRequest { request_id: request.as_str().into(), title: "Work".into() });
                self.must(orchestrator(), Command::CreateTask {
                    task_id: id.clone(), request_id: request.as_str().into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
                    dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")), plan_items: None, continues: None,
                });
                self.must(Actor::System, Command::RegisterAgent { agent_id: format!("node-{id}").into(), name: format!("SDK node {id}"), at: self.now });
                self.must(Actor::System, Command::AdmitExecution { provider: Default::default(),
                    task_id: id.clone(), agent_id: format!("node-{id}").into(), conversation: format!("conversation-{id}"),
                    instance: format!("instance-{id}"), host: "alondra-sdk".into(), model: None, account: None, leaves_limit: None,
                });
                self.facts.insert(id.clone(), StartFacts { planned: true, start: Start::Launched, node: NodeFact::Alive, ..Default::default() });
                self.sdk_openings.insert(id, self.now + 120_000);
            }
            Step::UncertainOpening(pick) => {
                if !self.sdk_openings.is_empty() {
                    let id = self.sdk_openings.keys().nth(pick % self.sdk_openings.len()).unwrap().clone();
                    self.must(Actor::System, Command::TellNode { task_id: id.clone(), text: format!("Queued message at {}", self.s.revision), from_user: true, answers: None });
                    self.recover_opening(&id);
                }
            }
            Step::Create { scheduled, plan, deps } => {
                self.made += 1;
                let id: TaskId = format!("t{}", self.made).into();
                let open = self.open();
                let dependencies: BTreeSet<TaskId> = deps.iter().filter(|_| !open.is_empty()).map(|d| open[d % open.len()].clone())
                    .filter(|d| d.0 != "run").collect();
                let launch = plan.then(|| LaunchPlan::new(format!("blurcamera/{id}"), "/synthetic/Projects/BlurCamera"));
                let created = self.exec(orchestrator(), Command::CreateTask { plan_items: None, continues: None,
                    task_id: id.clone(), request_id: if scheduled { "weekly".into() } else { "r".into() }, title: format!("Task {id}"),
                    agent_id: None, review_prs: false, dependencies, stack: None, launch,
                });
                if created.is_ok() {
                    self.facts.insert(id, StartFacts { planned: plan, ..Default::default() });
                } else {
                    assert!(!self.s.tasks.contains_key(&id), "a refused task never exists");
                }
            }
            Step::Admit(n) => {
                if let Some(task) = self.pick(n, |t, f| Self::unstarted(t) && !f.held && matches!(f.start, Start::Ready | Start::Launching)) {
                    self.start_node(&task);
                }
            }
            Step::LaunchFails { pick, untrusted } => {
                let Some(task) = self.pick(pick, |t, f| Self::unstarted(t) && matches!(f.start, Start::Ready | Start::Launching)) else { return };
                let problem = if untrusted {
                    StartProblem::Untrusted { folder: "/synthetic/Projects/BlurCamera".into(), error: None }
                } else {
                    StartProblem::LaunchFailed { error: "claude exited with status 1".into() }
                };
                let attempts = self.attempts.get(&task).copied().unwrap_or(0);
                match after_failure(attempts, problem, self.now) {
                    AfterFailure::Retry { attempt, at } => {
                        self.attempts.insert(task.clone(), attempt);
                        self.fact(&task).start = Start::Retry { attempt, at };
                    }
                    AfterFailure::Ask(problem) => {
                        self.attempts.remove(&task);
                        self.fact(&task).start = Start::Blocked;
                        self.must(Actor::System, Command::RaiseStartProblem { task_id: task, problem });
                    }
                }
            }
            Step::Tick(ms) => {
                self.now += ms;
                let now = self.now;
                for fact in self.facts.values_mut() {
                    if matches!(fact.start, Start::Retry { at, .. } if at <= now) {
                        fact.start = Start::Ready;
                    }
                }
            }
            Step::NodeDies(n) => {
                if let Some(task) = self.pick(n, |t, f| t.agent_id.is_some() && matches!(f.node, NodeFact::Alive | NodeFact::Stopped { .. })) {
                    let agent = self.agent_of(&task).unwrap();
                    self.must(Actor::System, Command::MoveNode { agent_id: agent, event: NodeEvent::SessionLost, at: 0 });
                    self.fact(&task).node = NodeFact::Reviving;
                }
            }
            Step::NodeBack(n) => {
                if let Some(task) = self.pick(n, |t, f| t.agent_id.is_some() && f.node == NodeFact::Reviving) {
                    let agent = self.agent_of(&task).unwrap();
                    if !self.s.node_phase(&agent).is_some_and(NodePhase::coming_back) {
                        return;
                    }
                    self.must(Actor::System, Command::MoveNode { agent_id: agent, event: NodeEvent::TurnStarted, at: 0 });
                    self.fact(&task).node = NodeFact::Alive;
                }
            }
            Step::RevivalFails(n) => {
                if let Some(task) = self.pick(n, |t, f| t.agent_id.is_some() && f.node == NodeFact::Reviving) {
                    let agent = self.agent_of(&task).unwrap();
                    let options = vec![
                        QuestionOption { id: "retry".into(), label: "Try again".into(), description: None },
                        QuestionOption { id: "cancel".into(), label: "Cancel its task".into(), description: None },
                    ];
                    let id: InteractionId = format!("hold-{}", self.s.revision).into();
                    self.must(Actor::System, Command::HoldHook {
                        interaction_id: id, task_id: task.clone(), agent_id: agent, hold: Hold::Question,
                        body: "The node is not responding and restarting it failed".into(), options,
                        card: CardSpec::derived(CardKind::Decide, "Restart the node or cancel its task"),
                    });
                    self.fact(&task).node = NodeFact::Asking;
                }
            }
            Step::AnswerRevival { pick, retry } => {
                let Some(task) = self.pick(pick, |_, f| f.node == NodeFact::Asking) else { return };
                let Some(card) = self.s.open_user_card(&task).map(|i| i.id.clone()) else { return };
                let option = if retry { "retry" } else { "cancel" };
                self.must(user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card, option_id: option.into(), inputs: BTreeMap::new() });
                if retry {
                    self.fact(&task).node = NodeFact::Reviving;
                } else if !self.s.tasks[&task].over() {
                    self.must(user(), Command::CancelTask { task_id: task });
                }
            }
            Step::UserClosesNode(n) => {
                if let Some(task) = self.pick(n, |t, _| t.agent_id.is_some() && t.final_presentation.is_none() && t.id.0 != "run") {
                    if self.s.tasks[&task].over() {
                        let agent = self.s.tasks[&task].agent_id.clone();
                        self.must(Actor::System, Command::RecordNodeEnd { task_id: task.clone(), end: NodeEnd::Gone, agent_id: agent });
                    } else {
                        self.must(Actor::System, Command::ReleaseTask { task_id: task.clone(), reason: "node closed by the user".into() });
                    }
                    let fact = self.fact(&task);
                    fact.closed = true;
                    fact.start = Start::Launched;
                }
            }
            Step::NodeStops(n) => {
                let Some(task) = self.pick(n, |t, f| t.state == TaskState::Running && t.final_presentation.is_none() && t.agent_id.is_some()
                    && matches!(f.node, NodeFact::Alive | NodeFact::Stopped { sent_back: true }) && self.s.open_user_card(&t.id).is_none()) else { return };
                self.stop_without_delivering(&task);
            }
            Step::Deliver(n) => {
                let Some(task) = self.pick(n, |t, f| t.state == TaskState::Running && t.final_presentation.is_none()
                    && matches!(f.node, NodeFact::Alive | NodeFact::Stopped { .. }) && t.agent_id.is_some()) else { return };
                self.stops.remove(&task);
                self.fact(&task).node = NodeFact::Alive;
                let agent = self.agent_of(&task).unwrap();
                let targets = vec![DeliveryTarget::Text { text: "Done".into() }];
                let _ = self.exec(Actor::Agent { agent_id: agent }, Command::PresentResult {
                    checklist: Vec::new(),
                    attachments: vec![], task_id: task.clone(), interaction_id: format!("final-{task}").into(), body: "Done".into(),
                    targets, is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], true),
                });
            }
            Step::AcceptAndFinish(n) => {
                let Some(task) = self.pick(n, |t, _| t.state == TaskState::ForYourReview) else { return };
                let presentation = self.s.tasks[&task].final_presentation.clone().unwrap();
                let targets = vec![DeliveryTarget::Text { text: "Done".into() }];
                self.must(user(), Command::AcceptDelivery { presentation_id: presentation, targets });
                if self.s.tasks[&task].state == TaskState::Closing {
                    let agent = self.agent_of(&task);
                    self.must(Actor::System, Command::RecordNodeEnd { task_id: task, end: NodeEnd::Closed, agent_id: agent });
                }
            }
            Step::Cancel(n) => {
                if let Some(task) = self.pick(n, |t, _| t.id.0 != "run" && !t.over()) {
                    self.must(user(), Command::CancelTask { task_id: task });
                }
            }
            Step::Reopen(n) => {
                let ended: Vec<TaskId> = self.s.tasks.values()
                    .filter(|t| t.state.terminal() && self.facts.get(&t.id).is_some_and(|f| f.planned)).map(|t| t.id.clone()).collect();
                let Some(task) = (!ended.is_empty()).then(|| ended[n % ended.len()].clone()) else { return };
                if self.exec(orchestrator(), Command::ReopenTask { task_id: task.clone() }).is_err() {
                    return;
                }
                if let Some(agent) = self.agent_of(&task) {
                    let node = match self.s.node_phase(&agent) {
                        Some(phase) if phase.coming_back() && self.s.open_user_card(&task).is_some() => NodeFact::Asking,
                        Some(phase) if phase.coming_back() => NodeFact::Reviving,
                        Some(phase) if phase.live() => NodeFact::Alive,
                        _ => NodeFact::Gone,
                    };
                    self.fact(&task).node = node;
                } else {
                    self.attempts.remove(&task);
                    self.stops.remove(&task);
                    let fact = self.fact(&task);
                    fact.start = Start::Launching;
                    fact.node = NodeFact::Gone;
                }
            }
            Step::Replan { pick, outcome, onto } => {
                let Some(task) = self.pick(pick, |t, _| self.s.cancelled_dependency(t).is_some()) else { return };
                let t = &self.s.tasks[&task];
                let cancelled = self.s.cancelled_dependency(t).unwrap();
                let kept: BTreeSet<TaskId> = t.dependencies.iter().filter(|d| **d != cancelled).cloned().collect();
                match outcome {
                    0 => {
                        let Some(replacement) = self.pick(onto, |other, _| other.id != task && other.id.0 != "run") else { return };
                        let dependencies = kept.into_iter().chain([replacement]).collect();
                        let _ = self.exec(orchestrator(), Command::SetDependencies { task_id: task, dependencies, stack: None });
                    }
                    1 => {
                        let _ = self.exec(orchestrator(), Command::SetDependencies { task_id: task, dependencies: kept, stack: None });
                    }
                    2 => self.must(orchestrator(), Command::CancelTask { task_id: task }),
                    _ => {
                        if self.s.open_start_problem(&task).is_none() {
                            self.must(Actor::System, Command::RaiseStartProblem { task_id: task, problem: StartProblem::DependencyCancelled { dependency: cancelled } });
                        }
                    }
                }
            }
            Step::AnswerProblem { pick, option } => {
                let Some(task) = self.pick(pick, |t, _| self.s.open_start_problem(&t.id).is_some()) else { return };
                let card = self.s.open_start_problem(&task).unwrap();
                let InteractionPayload::Question { options, .. } = &card.payload else { unreachable!() };
                let (id, chosen) = (card.id.clone(), options[option % options.len()].id.clone());
                self.must(user(), Command::AnswerQuestion { attachments: vec![], interaction_id: id, option_id: chosen, inputs: BTreeMap::new() });
            }
        }
    }

    /// The node ends its turn with nothing running, neither a result nor a question: the Stop guard
    /// sends it back until its cap, then tells the user with a Problem card and lets it stop.
    fn stop_without_delivering(&mut self, task: &TaskId) {
        let stops = self.stops.entry(task.clone()).or_default();
        *stops += 1;
        let told = *stops > STOP_CAP;
        if told {
            self.must(Actor::System, Command::RaiseProblem {
                attachments: vec![], interaction_id: format!("problem-undelivered-{task}-{}", self.s.revision).into(),
                scope: Scope::Task { task_id: task.clone() }, body: "It stopped without a result or a question".into(), options: vec![],
                card: CardSpec::new(CardKind::Problem, format!("A node cannot manage to deliver “{task}”"))
                    .why("It ended its turn three times without a result or a question; now it has stopped."),
            });
        }
        self.fact(task).node = NodeFact::Stopped { sent_back: !told };
    }

    /// What the daemon does after every change: `react`, carried out as the daemon carries it out.
    fn react(&mut self) {
        let due: Vec<_> = self.sdk_openings.iter().filter(|(_, deadline)| **deadline <= self.now).map(|(task, _)| task.clone()).collect();
        for task in due { self.recover_opening(&task); }
        for reaction in self.s.react(&self.facts) {
            match reaction {
                Reaction::Ask(task, problem) => {
                    self.must(Actor::System, Command::RaiseStartProblem { task_id: task.clone(), problem });
                    if self.s.tasks[&task].state == TaskState::NotStarted {
                        self.fact(&task).start = Start::Blocked;
                    }
                }
                Reaction::Retry(task) => {
                    let fact = self.fact(&task);
                    fact.start = if fact.closed { Start::Launching } else { Start::Ready };
                    self.attempts.remove(&task);
                }
            }
        }
    }

    fn recover_opening(&mut self, task: &TaskId) {
        let lifecycle = self.s.lifecycle_of(task).unwrap();
        let pending: Vec<_> = lifecycle.pending_inputs().iter().map(|input| (*input).clone()).collect();
        let texts: BTreeMap<_, _> = journal_writes(task, &self.s.lifecycle_journal[task], 0).unwrap().into_iter()
            .filter_map(|write| Some((write.input?, write.text?))).collect();
        self.must(Actor::System, Command::RecordHostEnded { task_id: task.clone(), generation: 1, resumes: true, ended_work: vec![], by_alondra: false });
        for input in pending {
            self.must(Actor::System, Command::RecordDoubtfulHandoff { task_id: task.clone(), generation: 1,
                input: input.as_str().into(), text: texts.get(&input).cloned().unwrap_or_else(|| "The original mission".into()), resumes: true, by_alondra: false,
            });
        }
        self.sdk_openings.insert(task.clone(), self.now + 120_000);
    }

    fn check(&self, steps: &[Step]) -> Result<(), String> {
        let none = StartFacts::default();
        super::lifecycle_tests::assert_aggregate_rules(&self.s);
        for id in self.open() {
            let task = &self.s.tasks[&id];
            if let Some(deadline) = self.sdk_openings.get(&id) {
                if *deadline <= self.now || !self.s.lifecycle_of(&id).unwrap().cards().is_empty() {
                    return Err(format!("SDK task {id} has an expired or user-blocked startup after {steps:?}"));
                }
                let lifecycle = self.s.lifecycle_of(&id).unwrap();
                if task.agent_id.is_none() || task.started || lifecycle.pending_inputs().is_empty() {
                    return Err(format!("SDK startup {id} lost its execution or pending mission after {steps:?}"));
                }
            }
            if task.started_without_node() {
                return Err(format!("“{id}” is in progress with no node (state {:?}) after {steps:?}", task.state));
            }
            if let Err(ownerless) = self.s.holder(task, self.facts.get(&id).unwrap_or(&none)) {
                return Err(format!("“{}” is in nobody's hands: {} (state {:?}) after {steps:?}", ownerless.task, ownerless.why, task.state));
            }
        }
        Ok(())
    }
}

/// Held-back turn ends the daemon's Stop guard allows before it tells the user.
const STOP_CAP: u32 = 3;

#[test]
fn every_open_task_is_always_in_someones_hands() {
    let config = Config { cases: 256, failure_persistence: None, ..Config::default() };
    let mut runner = TestRunner::new_with_rng(config, TestRng::deterministic_rng(RngAlgorithm::ChaCha));
    let result = runner.run(&prop::collection::vec(step(), 1..40), |steps| {
        let mut world = World::new();
        world.react();
        world.check(&[]).map_err(TestCaseError::fail)?;
        for (i, step) in steps.iter().enumerate() {
            world.apply(step);
            world.react();
            world.check(&steps[..=i]).map_err(TestCaseError::fail)?;
        }
        Ok(())
    });
    if let Err(TestError::Fail(reason, steps)) = result {
        panic!("{reason}\nsmallest run: {steps:?}");
    }
    result.unwrap();
}

/// Today's case: the weekly BlurCamera improvement made two tasks without a launch plan, and they
/// sat in "waiting for the orchestrator to give it a plan". Such a task is refused and never
/// exists; with the plan the orchestrator gives it, the gate starts it.
#[test]
fn a_task_a_scheduled_run_asks_for_without_a_plan_is_refused_and_with_one_reaches_the_gate() {
    let mut w = World::new();
    let planless = w.exec(orchestrator(), Command::CreateTask { plan_items: None, continues: None,
        task_id: "t-hf-web-registry-skills".into(), request_id: "weekly".into(), title: "Register blurcamera.app in web-properties.json".into(),
        agent_id: None, review_prs: true, dependencies: BTreeSet::new(), stack: None, launch: None,
    }).unwrap_err();
    assert_eq!(planless.code, "launch_plan_required");
    assert!(!w.s.tasks.contains_key(&"t-hf-web-registry-skills".into()));
    w.must(orchestrator(), Command::CreateTask { plan_items: None, continues: None,
        task_id: "t-hf-web-registry-skills".into(), request_id: "weekly".into(), title: "Register blurcamera.app in web-properties.json".into(),
        agent_id: None, review_prs: true, dependencies: BTreeSet::new(), stack: None,
        launch: Some(LaunchPlan::new("agent-skills/hf-web-registry-skills", "/synthetic/Projects/agent-skills")),
    });
    w.facts.insert("t-hf-web-registry-skills".into(), StartFacts { planned: true, ..Default::default() });
    let task = &w.s.tasks[&"t-hf-web-registry-skills".into()];
    assert_eq!(w.s.holder(task, &w.facts[&task.id]), Ok(Holder::Gate(GateCause::Room)));
    let planless_facts = StartFacts::default();
    assert_eq!(w.s.holder(task, &planless_facts).unwrap_err().why, "has no launch plan", "the invariant names a task without one");
}

/// A start that keeps failing is tried again by itself, then goes to the user, who can try again.
#[test]
fn a_start_that_keeps_failing_is_retried_then_put_to_the_user() {
    let mut w = World::new();
    w.apply(&Step::Create { scheduled: false, plan: true, deps: vec![] });
    let task: TaskId = "t1".into();
    for attempt in 1..=START_RETRIES.len() as u8 {
        w.apply(&Step::LaunchFails { pick: 0, untrusted: false });
        assert!(matches!(w.facts[&task].start, Start::Retry { attempt: a, .. } if a == attempt), "{:?}", w.facts[&task]);
        assert!(matches!(w.s.holder(&w.s.tasks[&task], &w.facts[&task]), Ok(Holder::Gate(GateCause::Retry { .. }))));
        w.apply(&Step::Tick(START_RETRIES[usize::from(attempt) - 1]));
    }
    w.apply(&Step::LaunchFails { pick: 0, untrusted: false });
    let card = w.s.open_start_problem(&task).expect("the user is asked").clone();
    assert_eq!(card.card.effective_title(), "“Task t1” does not start");
    assert_eq!(w.s.holder(&w.s.tasks[&task], &w.facts[&task]), Ok(Holder::User(UserCause::Card(card.id.clone()))));
    w.must(user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.id, option_id: START_RETRY.into(), inputs: BTreeMap::new() });
    assert_eq!(w.s.react(&w.facts), vec![Reaction::Retry(task.clone())]);
    w.react();
    assert_eq!(w.facts[&task].start, Start::Ready);
}

/// A node the user closes asks whether to relaunch its task.
#[test]
fn a_closed_node_asks_the_user() {
    let mut w = World::new();
    w.apply(&Step::Create { scheduled: false, plan: true, deps: vec![] });
    w.apply(&Step::Admit(0));
    w.apply(&Step::UserClosesNode(0));
    w.react();
    let closed = w.s.open_start_problem(&"t1".into()).expect("a card for the closed node");
    assert_eq!(closed.card.effective_title(), "Relaunch “Task t1” or cancel it");
}

/// A task with `dependencies` on a task that is cancelled waits for its re-plan.
fn waiting_for_a_cancelled_task() -> (World, TaskId) {
    let mut w = World::new();
    w.apply(&Step::Create { scheduled: false, plan: true, deps: vec![] });
    w.apply(&Step::Create { scheduled: false, plan: true, deps: vec![1] });
    let waiting: TaskId = "t2".into();
    assert_eq!(w.s.tasks[&waiting].dependencies, BTreeSet::from([TaskId::from("t1")]));
    w.must(user(), Command::CancelTask { task_id: "t1".into() });
    w.react();
    (w, waiting)
}

/// 20 October: the user cancelled a task that another one waited for and was
/// asked at once whether to start the Alondra task that waited for it without it. That re-plan is
/// the orchestrator's: no card reaches the user, and no node starts it while it waits for the
/// cancelled task. Only once the orchestrator leaves it does the user decide, and starting it
/// without the cancelled task still works.
#[test]
fn a_task_whose_dependency_is_cancelled_goes_to_the_orchestrator_and_never_starts() {
    let (mut w, waiting) = waiting_for_a_cancelled_task();
    let t = &w.s.tasks[&waiting];
    assert_eq!(t.state, TaskState::WaitingDependency);
    assert_eq!(w.s.cancelled_dependency(t), Some("t1".into()));
    assert!(w.s.open_start_problem(&waiting).is_none(), "the user is not asked");
    assert_eq!(w.s.holder(t, &w.facts[&waiting]), Ok(Holder::Orchestrator));
    let agent: AgentId = "eager".into();
    w.must(Actor::System, Command::RegisterAgent { agent_id: agent.clone(), name: "eager".into(), at: 0 });
    let refused = w.exec(Actor::System, Command::AssignTask { task_id: waiting.clone(), agent_id: agent.clone(), model: None });
    assert!(refused.is_err() || w.exec(Actor::Agent { agent_id: agent }, Command::StartTask { task_id: waiting.clone() }).is_err(),
        "no node starts it while it waits for a cancelled task");
    assert!(!w.s.tasks[&waiting].started);

    w.must(Actor::System, Command::RaiseStartProblem { task_id: waiting.clone(), problem: StartProblem::DependencyCancelled { dependency: "t1".into() } });
    let card = w.s.open_start_problem(&waiting).expect("the user's card once the orchestrator left it").clone();
    assert_eq!(card.card.effective_title(), "“Task t1” was cancelled: start “Task t2” without it?");
    assert_eq!(w.s.holder(&w.s.tasks[&waiting], &w.facts[&waiting]), Ok(Holder::User(UserCause::Card(card.id.clone()))));
    w.must(user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card.id, option_id: START_WITHOUT.into(), inputs: BTreeMap::new() });
    let t = &w.s.tasks[&waiting];
    assert!(t.dependencies.is_empty() && t.state == TaskState::NotStarted);
    assert_eq!(w.s.holder(t, &w.facts[&waiting]), Ok(Holder::Gate(GateCause::Room)));
}

/// The orchestrator replaced the cancelled task: the dependent follows its dependencies, waits for
/// the replacement and starts once it is done, with nobody asked.
#[test]
fn a_task_re_pointed_to_a_replacement_waits_for_it_and_nobody_is_asked() {
    let (mut w, waiting) = waiting_for_a_cancelled_task();
    w.apply(&Step::Create { scheduled: false, plan: true, deps: vec![] });
    let replacement: TaskId = "t3".into();
    w.must(orchestrator(), Command::SetDependencies { task_id: waiting.clone(), dependencies: BTreeSet::from([replacement.clone()]), stack: None });
    w.react();
    let t = &w.s.tasks[&waiting];
    assert_eq!(w.s.cancelled_dependency(t), None);
    assert_eq!(w.s.holder(t, &w.facts[&waiting]), Ok(Holder::Task(replacement.clone())));
    assert!(w.s.open_start_problem(&waiting).is_none());

    w.start_node(&replacement);
    let agent = w.agent_of(&replacement).unwrap();
    w.must(Actor::Agent { agent_id: agent }, Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: replacement.clone(), interaction_id: "final-t3".into(), body: "Done".into(),
        targets: vec![DeliveryTarget::Text { text: "Done".into() }], is_final: true, nothing_to_review: false,
        card: CardSpec::delivery("Review the result", &[], true),
    });
    w.apply(&Step::AcceptAndFinish(0));
    w.react();
    assert_eq!(w.s.tasks[&replacement].state, TaskState::Done);
    let t = &w.s.tasks[&waiting];
    assert_eq!(t.state, TaskState::NotStarted, "its wait is over");
    assert_eq!(w.s.holder(t, &w.facts[&waiting]), Ok(Holder::Gate(GateCause::Room)));
    assert!(w.s.open_start_problem(&waiting).is_none());
}

/// The flaky snapshots node: its question answered, it stopped without delivering. While Alondra
/// still sends it back it holds its task; at the cap the card that tells the user holds it; a
/// node stopped this way that nobody told is a task in nobody's hands.
#[test]
fn a_node_stopped_without_a_result_or_a_question_is_sent_back_then_put_to_the_user() {
    let mut w = World::new();
    w.apply(&Step::Create { scheduled: false, plan: true, deps: vec![] });
    w.apply(&Step::Admit(0));
    let task: TaskId = "t1".into();
    let agent = w.agent_of(&task).unwrap();
    w.must(Actor::Agent { agent_id: agent.clone() }, Command::AskQuestion {
        attachments: vec![], interaction_id: "ask-1".into(), scope: Scope::Task { task_id: task.clone() }, recipient: Recipient::User,
        body: "Deliver only the daemon fix?".into(), blocking: true,
        options: vec![
            QuestionOption { id: "daemon".into(), label: "Only the daemon fix".into(), description: None },
            QuestionOption { id: "both".into(), label: "Both fixes".into(), description: None },
        ],
        card: CardSpec::derived(CardKind::Decide, "Choose what to deliver"),
    });
    w.must(user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "ask-1".into(), option_id: "daemon".into(), inputs: BTreeMap::new() });

    for _ in 0..STOP_CAP {
        w.stop_without_delivering(&task);
        assert_eq!(w.s.holder(&w.s.tasks[&task], &w.facts[&task]), Ok(Holder::Node), "Alondra sends it back");
    }
    let unattended = StartFacts { node: NodeFact::Stopped { sent_back: false }, ..w.facts[&task].clone() };
    assert_eq!(w.s.holder(&w.s.tasks[&task], &unattended).unwrap_err().why,
        "its node stopped without a result or a question and nobody was told");
    w.stop_without_delivering(&task);
    let card = w.s.open_user_card(&task).expect("the user is told").id.clone();
    assert_eq!(w.s.holder(&w.s.tasks[&task], &w.facts[&task]), Ok(Holder::User(UserCause::Card(card))));
}
