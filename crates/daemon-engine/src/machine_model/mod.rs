//! A model of the whole machine: the real domain under a daemon that does what Alondra's loops do
//! (the gate, the hosts' owners, the closer, the orchestrator), and the world around it (`world`).
//! A history mixes the user's commands, the provider's facts, hosts that die, daemon restarts,
//! capacity changes and time; after it the machine rests with nobody acting, and `tests` checks
//! where every task ends up.
//!
//! The daemon's loops are modelled at the level of the commands they record, so a history runs in
//! milliseconds and is deterministic. What is real is everything those commands go through: the
//! domain and its saved form (a restart reads it again from there and loses all the daemon kept in
//! memory), the one clock (`obligation_watch`), and what every screen and the gate read
//! (`ReadIndex`, `Situation`, `admission::working_tasks`). A rule the code does not apply yet is in
//! `tests::KNOWN_GAPS`, and the model applies it itself while it is listed.
mod tests;
mod world;

use crate::obligation_watch::{self, Watch};
use alondra_core::domain::obligation_policy;
use alondra_core::domain::{
    Actor, AgentId, CardKind, CardSpec, ContentKind, Command, CommandEnvelope, CommandOutcome, DecisionPrompt, DomainEffect, DeliveryTarget, DomainState, Hold,
    Instruction, InteractionId, InteractionPayload, LaunchPlan, LifecycleRecord, NODE_PAUSE_PREFIX, NODE_PAUSE_RESUME, NodeEnd, NodeEvent,
    PROTOCOL_VERSION, Pause, PauseCleanup, PauseReason, PlanReviewOutcome, QuestionOption, ReadIndex, Recipient, RuntimeObservation, STALL_ATTEMPTS, Scope,
    StallOutcome, TaskId, TaskPriority, is_stall_case,
};
use alondra_core::accounts::{AccountBilling, AccountId, AccountKey, AccountState, AccountStatus, Provider};
use alondra_core::model_selection::{AccountModelCatalog, CatalogModel, MoveRefused};
use alondra_domain::{Capacity, Owed, Party, ResourceEvidence, TaskPhase};
use alondra_foundation::{Millis, Revision};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use world::{Action, Fact, History, Host, MINUTE, PASS, Running, TASKS, TURN_LASTS, Turn, UserAct, WORK_LASTS, Work, World};

/// How long the machine rests after a history, at most.
const REST: Millis = 2 * 60 * MINUTE;
/// A rest ends early once nothing changed, nothing ran and Alondra owed nothing for this long.
const QUIET: Millis = 45 * MINUTE;

/// What the model saw go wrong while it ran, for the properties that judge a command or a pass.
#[derive(Default)]
struct Seen {
    /// A command the domain accepted that changed nothing.
    unchanged: Vec<String>,
    /// A close accepted that left the task running.
    close_left_running: Vec<String>,
    /// A card for the user that a pass of the clock opened.
    clock_cards: Vec<InteractionId>,
    /// The most wake inputs a task got in a row: between two turns that did something, or two
    /// words from the user or the orchestrator.
    most_wakes: u32,
}

struct Machine {
    domain: Arc<DomainState>,
    world: World,
    watch: Watch,
    granted: BTreeSet<TaskId>,
    wakes: BTreeMap<TaskId, u32>,
    agents: u64,
    commands: u64,
    /// Commands refused at a revision: the same command at the same revision is refused again.
    refused: BTreeMap<(Revision, String), String>,
    /// The last fact the daemon took from each execution: saved, so it outlives a restart.
    cursors: BTreeMap<(TaskId, u64), u64>,
    /// What the domain asked to tell nodes and the delivery outbox has not told yet: saved.
    messages: Vec<Message>,
    /// Since when the clock has seen each task owe what it owes now.
    owing: BTreeMap<TaskId, (Revision, Owed, Millis)>,
    /// When each task was last taken over for reading as working with no owner.
    taken: BTreeMap<TaskId, Millis>,
    /// Since when each delivered task has been stranded.
    stranded: BTreeMap<TaskId, Millis>,
    /// The tasks whose move to another account was deferred to their next start, and that account:
    /// saved, as the store keeps a deferred move.
    deferred: BTreeMap<TaskId, AccountKey>,
    /// The revision at which a task's admission was refused: it is tried again once something changed.
    unadmitted: BTreeMap<TaskId, Revision>,
    seen: Seen,
    trace: Vec<String>,
}

/// A message of the delivery outbox for a node.
struct Message {
    agent: AgentId,
    task: Option<TaskId>,
    card: Option<InteractionId>,
    text: String,
}

/// What the clock acts on: what `obligation_policy` enforces, and the rows of `KNOWN_GAPS` the
/// model enforces in its place.
fn enforced(owed: &Owed) -> bool {
    obligation_policy::enforced(owed) || (*owed == Owed::HandInput && tests::gap_open(*owed))
}

/// What the gate tells the clock: a turn waits for a slot until the gate gives it one, and the
/// usage limit holds the orchestrator.
struct Turns<'a> {
    state: &'a DomainState,
    granted: &'a BTreeSet<TaskId>,
    usage_holds: bool,
}

impl obligation_watch::World for Turns<'_> {
    fn waits_for_slot(&self, task: &TaskId) -> bool {
        self.state.waits_for_turn(task) && !self.granted.contains(task)
    }

    fn orchestrator_held(&self) -> bool {
        self.usage_holds
    }
}

/// The two Claude accounts a task can move between.
const ACCOUNTS: [&str; 2] = ["default", "work"];

fn account(id: &str) -> AccountStatus {
    AccountStatus {
        provider: Provider::Claude, id: AccountId::parse(id).expect("an account id"), name: id.into(), state: AccountState::SignedIn,
        native_identity: None, email: None, plan: None, billing: AccountBilling::default(), sign_ins: 0, profile: None,
    }
}

fn catalog(id: &str) -> AccountModelCatalog {
    AccountModelCatalog {
        account: AccountKey::new(Provider::Claude, AccountId::parse(id).expect("an account id")), revision: format!("{id}-opus"), measured_at: 1,
        native_default: Some("opus".into()),
        models: vec![CatalogModel {
            model: "opus".into(), display_name: "Opus".into(), reasoning_efforts: vec!["high".into()], default_reasoning_effort: Some("high".into()),
            context_window: Some(100_000), input_modalities: vec!["text".into()], is_default: true, hidden: false,
        }],
    }
}

fn yes_no() -> Vec<QuestionOption> {
    ["yes", "no"].into_iter().map(|id| QuestionOption { id: id.into(), label: id.into(), description: None }).collect()
}

fn text_targets() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::Text { text: "Done".into() }]
}

fn scope(task: &TaskId) -> Scope {
    Scope::Task { task_id: task.clone() }
}

impl Machine {
    fn new(history: &History) -> Self {
        let mut machine = Self {
            domain: Arc::new(DomainState::new("machine")), world: World::new(history.script.clone(), history.slots), watch: Watch::enforcing(enforced),
            granted: BTreeSet::new(), wakes: BTreeMap::new(), agents: 0, commands: 0, refused: BTreeMap::new(),
            cursors: BTreeMap::new(), messages: Vec::new(), owing: BTreeMap::new(), taken: BTreeMap::new(), stranded: BTreeMap::new(),
            deferred: BTreeMap::new(), unadmitted: BTreeMap::new(), seen: Seen::default(), trace: Vec::new(),
        };
        let user = Actor::User { user_id: "u".into() };
        let orchestrator = Actor::Orchestrator { generation: 0 };
        machine.must(user.clone(), Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        machine.must(orchestrator.clone(), Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
        machine.must(Actor::System, Command::RecordAccounts {
            accounts: [ACCOUNTS[0], ACCOUNTS[1]].into_iter().map(account).collect(), reserve: alondra_core::usage::default_reserve(),
        });
        for id in ACCOUNTS {
            machine.must(Actor::System, Command::RecordModelCatalog { catalog: catalog(id) });
        }
        for task in TASKS {
            machine.must(orchestrator.clone(), Command::CreateTask {
                task_id: task.into(), request_id: "r".into(), title: format!("Task {task}"), agent_id: None, review_prs: false,
                dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")), plan_items: None, continues: None,
            });
        }
        machine.settle();
        machine
    }

    fn state(&self) -> Arc<DomainState> {
        Arc::clone(&self.domain)
    }

    fn revision(&self) -> Revision {
        self.state().revision
    }

    fn must(&mut self, actor: Actor, command: Command) {
        let shown = format!("{command:?}");
        if let Err(error) = self.run(actor, command) {
            panic!("{shown} was refused: {error}");
        }
    }

    /// Runs `command` as the daemon's API would, and notes an acknowledgement that changed nothing.
    fn run(&mut self, actor: Actor, command: Command) -> Result<CommandOutcome, String> {
        let state = self.state();
        let key = (state.revision, format!("{actor:?} {command:?}"));
        if let Some(refusal) = self.refused.get(&key) {
            return Err(refusal.clone());
        }
        self.commands += 1;
        let name: String = format!("{command:?}").chars().take(160).collect();
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("model-{}", self.commands).into(),
            expected_revision: state.revision, command,
        };
        drop(state);
        let result = Arc::make_mut(&mut self.domain).execute(&actor, &envelope).map_err(|error| error.code);
        let state_revision = key.0;
        match &result {
            Ok(_) if self.revision() == state_revision => self.seen.unchanged.push(name.clone()),
            Ok(outcome) => {
                for effect in &outcome.effects {
                    if let DomainEffect::NotifyAgent { agent_id, task_id, interaction_id, message } = effect {
                        self.messages.push(Message { agent: agent_id.clone(), task: task_id.clone(), card: interaction_id.clone(), text: message.clone() });
                    }
                }
            }
            Err(refusal) => {
                self.refused.insert(key, refusal.clone());
            }
        }
        self.trace.push(format!("{} {name} -> {}", self.world.now, result.as_ref().map_or_else(Clone::clone, |_| "ok".into())));
        result
    }

    /// The node a new execution runs as: the SDK owner registers it before it admits the execution.
    fn new_agent(&mut self) -> AgentId {
        self.agents += 1;
        let agent = AgentId(format!("node-{}", self.agents));
        let _ = self.run(Actor::System, Command::RegisterAgent { agent_id: agent.clone(), name: agent.0.clone(), at: 0 });
        agent
    }

    fn agent(&self, task: &TaskId) -> Option<AgentId> {
        self.state().tasks.get(task)?.agent_id.clone()
    }

    fn generation(&self, task: &TaskId) -> Option<u64> {
        self.state().lifecycle_of(task).ok()?.execution().map(|stamp| stamp.generation())
    }

    fn open_user_cards(&self) -> BTreeSet<InteractionId> {
        let state = self.state();
        state.interactions.values().filter(|card| card.recipient == Recipient::User && state.open(card)).map(|card| card.id.clone()).collect()
    }

    /// The tasks the gate counts at work: those holding a slot but the turns that still wait for one.
    fn at_work(&self, state: &DomainState) -> usize {
        tests::slot_tasks(state).into_iter().filter(|task| !state.waits_for_turn(task) || self.granted.contains(task)).count()
    }

    /// How long the clock has seen `task` owe what it owes now.
    fn owed_for(&self, task: &TaskId) -> Millis {
        self.owing.get(task).map_or(0, |(_, _, since)| self.world.now - since)
    }

    /// Does one thing of the history.
    fn act(&mut self, action: &Action) {
        let task = |index: &usize| TaskId::from(TASKS[*index]);
        match action {
            Action::User(act, index) => self.user(*act, &task(index)),
            Action::TurnEnds(index) => self.end_turn(&task(index)),
            Action::WorkEnds { task: index, notifies } => self.end_work(&task(index), Some(*notifies)),
            Action::IgnoresNextInput(index) => {
                if let Some(host) = self.world.host(&task(index)) { host.ignores = true; }
            }
            Action::HostDies(index) => {
                if let Some(host) = self.world.host(&task(index)) { host.alive = false; }
            }
            Action::FactsLate(index) => {
                let until = self.world.now + MINUTE;
                if let Some(host) = self.world.host(&task(index)) { host.late_until = until; }
            }
            Action::FactsAgain(index) => {
                if let Some(host) = self.world.host(&task(index)) {
                    let sent = host.sent.clone();
                    host.outbox.extend(sent);
                }
            }
            Action::Restart => self.restart(),
            Action::Slots(slots) => self.world.slots = *slots,
            Action::MemoryTight(tight) => self.world.memory_tight = *tight,
            Action::DiskFull(full) => self.world.disk_full = *full,
            Action::Usage(lasts) => self.world.usage_until = lasts.map(|lasts| self.world.now + lasts),
            Action::OrchestratorAnswers(answers) => self.world.orchestrator_answers = *answers,
            Action::Clock(lasts) => {
                let until = self.world.now + lasts;
                while self.world.now < until {
                    self.world.now += PASS;
                    self.pass();
                }
            }
        }
        self.settle();
    }

    /// One pass of the clock and of every loop after time moved.
    fn pass(&mut self) {
        self.settle();
        self.clock();
        self.settle();
    }

    /// The machine with nobody acting: the provider ends what it runs as documented, and Alondra's
    /// loops and its clock go on, for at most two hours. `each` judges every pass.
    fn rest(&mut self, mut each: impl FnMut(&Machine)) {
        let end = self.world.now + REST;
        let mut quiet_since = self.world.now;
        let mut revision = self.revision();
        while self.world.now < end {
            self.world.now += PASS;
            self.provider_goes_on();
            self.pass();
            each(self);
            let state = self.state();
            let alondra_owes = state.tasks.keys().any(|task| state.lifecycle_of(task).ok().and_then(|lifecycle| lifecycle.obligation())
                .is_some_and(|obligation| obligation.party == Party::Alondra));
            let busy = self.world.hosts.values().any(|host| host.runs() || host.offered.is_some() || !host.outbox.is_empty())
                || !self.world.retired.is_empty() || self.world.usage_holds() || alondra_owes;
            if busy || self.revision() != revision {
                quiet_since = self.world.now;
                revision = self.revision();
            } else if self.world.now - quiet_since >= QUIET {
                break;
            }
        }
    }

    /// What the provider does with nobody acting: a turn ends a minute after it started, and work
    /// left running ends after two, telling its session as the session said it would.
    fn provider_goes_on(&mut self) {
        let now = self.world.now;
        let tasks: Vec<TaskId> = self.world.hosts.keys().cloned().collect();
        for task in tasks {
            let Some(host) = self.world.hosts.get(&task).filter(|host| host.alive) else { continue };
            let turn_due = host.turn.as_ref().is_some_and(|turn| turn.decision.is_none() && now - turn.since >= TURN_LASTS);
            let work_due = host.work.first().is_some_and(|work| now - work.since >= WORK_LASTS);
            if turn_due {
                self.end_turn(&task);
            }
            if work_due {
                self.end_work(&task, None);
            }
        }
    }

    /// The daemon stops and starts again: everything it kept in memory is lost, the domain is read
    /// again from what it saved, and the hosts, which outlive it, send again what it had not taken.
    fn restart(&mut self) {
        let saved = serde_json::to_string(&*self.domain).expect("the domain saves");
        self.domain = Arc::new(serde_json::from_str(&saved).expect("the domain reads what it saved"));
        self.watch = Watch::enforcing(enforced);
        self.owing.clear();
        self.refused.clear();
        self.taken.clear();
        self.stranded.clear();
        self.unadmitted.clear();
        self.granted.clear();
        for host in self.world.hosts.values_mut() {
            let unacknowledged: Vec<(u64, Fact)> = host.sent.drain(..).chain(host.outbox.drain(..)).collect();
            host.outbox.extend(unacknowledged);
        }
    }

    /// Every loop of the daemon, until none has anything left to do.
    fn settle(&mut self) {
        for _ in 0..40 {
            let revision = self.revision();
            let mut moved = self.deliver_facts();
            moved |= self.deliver_messages();
            moved |= self.end_dead_hosts();
            moved |= self.hold_for_disk();
            moved |= self.start_moved();
            moved |= self.admit();
            moved |= self.hand_inputs();
            moved |= self.hand_decisions();
            moved |= self.consume_inputs_in_turn();
            moved |= self.take_unowned();
            moved |= self.send_back_stranded();
            moved |= self.close_ending();
            moved |= self.orchestrate();
            if !moved && self.revision() == revision {
                return;
            }
        }
    }

    /// The daemon takes what each host observed, in order; a fact it already took, sent again, is
    /// dropped by its place in the order, as the store's cursor does.
    fn deliver_facts(&mut self) -> bool {
        let now = self.world.now;
        let mut batches: Vec<(TaskId, u64, Vec<Fact>)> = Vec::new();
        for (task, host) in self.world.hosts.iter_mut().chain(self.world.retired.iter_mut().map(|(task, host)| (&*task, host))) {
            if host.late_until > now || host.outbox.is_empty() {
                continue;
            }
            let observed: Vec<(u64, Fact)> = host.outbox.drain(..).collect();
            host.sent = observed.clone();
            let taken = self.cursors.entry((task.clone(), host.generation)).or_default();
            let mut facts = Vec::new();
            for (cursor, fact) in observed {
                if cursor > *taken {
                    *taken = cursor;
                    facts.push(fact);
                }
            }
            batches.push((task.clone(), host.generation, facts));
        }
        self.world.retired.retain(|(_, host)| !host.outbox.is_empty());
        let moved = !batches.is_empty();
        for (task, generation, facts) in batches {
            let mut runtime = Vec::new();
            for fact in facts {
                match fact {
                    Fact::Runtime(observation) => runtime.push(observation),
                    Fact::TurnEnded(turn) => {
                        self.record_facts(&task, generation, std::mem::take(&mut runtime));
                        self.record_turn_end(&task, generation, turn);
                    }
                }
            }
            self.record_facts(&task, generation, runtime);
        }
        moved
    }

    /// What the domain owes a node (an answer to its card, a notice) reaches it as an input, as the
    /// delivery outbox does.
    fn deliver_messages(&mut self) -> bool {
        let messages: Vec<Message> = std::mem::take(&mut self.messages);
        let moved = !messages.is_empty();
        for message in messages {
            let state = self.state();
            let task = message.task.or_else(|| state.tasks.values().find(|task| task.agent_id.as_ref() == Some(&message.agent) && !task.state.terminal()).map(|task| task.id.clone()));
            let Some(task) = task else { continue };
            let card = message.card.as_ref().and_then(|card| state.interactions.get(card));
            let answers = card.filter(|card| {
                let answered = matches!(&card.payload, InteractionPayload::Question { answer: Some(_), .. } | InteractionPayload::ActionRequest { answer: Some(_), .. });
                let node = state.tasks.get(&task).and_then(|task| task.agent_id.clone());
                answered && matches!(&card.author, alondra_core::domain::Author::Agent { agent_id } if Some(agent_id) == node.as_ref()) && card.scope == scope(&task)
            }).map(|card| card.id.clone());
            let from_user = answers.is_some() && card.is_some_and(|card| card.recipient == Recipient::User);
            let text = if message.text.trim().is_empty() { "Alondra has news for you".to_owned() } else { message.text };
            if self.run(Actor::System, Command::TellNode { task_id: task.clone(), text, from_user, answers }).is_ok() {
                self.take_if_unowned(&task);
            }
        }
        moved
    }

    /// Whether no host of this daemon runs the task's current execution: `sdk_owner::to_take`.
    fn unowned(&self, task: &TaskId) -> bool {
        let state = self.state();
        let Ok(lifecycle) = state.lifecycle_of(task) else { return false };
        let Some(generation) = lifecycle.execution().map(|stamp| stamp.generation()) else { return false };
        !matches!(lifecycle.phase(), TaskPhase::Completed | TaskPhase::Cancelled)
            && self.world.hosts.get(task).is_none_or(|host| !host.alive || host.generation != generation)
    }

    /// A host that resumes the task's session takes its execution over, as `sdk_owner::take_again` does.
    fn retake(&mut self, task: &TaskId) -> bool {
        let Some(generation) = self.generation(task) else { return false };
        let facts = |fact| Command::RecordRuntimeFacts { task_id: task.clone(), generation, facts: vec![fact] };
        let taken = match self.run(Actor::System, facts(RuntimeObservation::TakenOver)) {
            Err(code) if code == alondra_core::domain::REPEATED_TAKEOVER => {
                let _ = self.run(Actor::System, facts(RuntimeObservation::TakeoverUnanswered));
                self.run(Actor::System, facts(RuntimeObservation::TakenOver))
            }
            taken => taken,
        };
        if taken.is_err() {
            return false;
        }
        if let Some(old) = self.world.hosts.insert(task.clone(), Host::new(generation)) {
            self.world.retired.push((task.clone(), old));
        }
        true
    }

    /// Something was told to a task no host of this daemon runs: one takes it over (`retake_told`).
    fn take_if_unowned(&mut self, task: &TaskId) {
        if self.unowned(task) {
            self.retake(task);
        }
    }

    /// A task that reads as working, starting or woken with no host of its own is taken over, at
    /// most once a minute (`sdk_owner::take_working_unowned`).
    fn take_unowned(&mut self) -> bool {
        let state = self.state();
        let now = self.world.now;
        let due: Vec<TaskId> = TASKS.into_iter().map(TaskId::from).filter(|task| {
            state.tasks.get(task).is_some_and(|held| !held.over()) && state.lifecycle_of(task).is_ok_and(|lifecycle| {
                matches!(lifecycle.activity(), alondra_domain::ActivityView::Working | alondra_domain::ActivityView::OwnWork
                    | alondra_domain::ActivityView::Starting | alondra_domain::ActivityView::Continuation)
                    && !lifecycle.cards().into_iter().any(|(card, reason)| reason == alondra_domain::WaitReason::Pause
                        && (lifecycle.card_owner(card) == Some(alondra_domain::Owner::User) || lifecycle.takeover() != alondra_domain::Takeover::None))
            })
        }).filter(|task| self.unowned(task) && self.taken.get(task).is_none_or(|at| now - at >= MINUTE)).collect();
        let mut moved = false;
        for task in due {
            self.taken.insert(task.clone(), now);
            moved |= self.retake(&task);
        }
        moved
    }

    /// A delivered task its node owes work for and nothing starts goes back to work after five
    /// minutes, once the gate has room (`admission::send_back_stranded`).
    fn send_back_stranded(&mut self) -> bool {
        let state = self.state();
        let now = self.world.now;
        let stranded = state.stranded_tasks();
        self.stranded.retain(|task, _| stranded.contains(task));
        let mut moved = false;
        for task in stranded {
            let since = *self.stranded.entry(task.clone()).or_insert(now);
            if now - since < crate::admission::STRANDED_AFTER || self.at_work(&state) >= self.world.slots {
                continue;
            }
            if self.run(Actor::System, Command::RecordStranded { task_id: task.clone() }).is_ok() {
                self.stranded.remove(&task);
                self.take_if_unowned(&task);
                moved = true;
            }
        }
        moved
    }

    fn record_facts(&mut self, task: &TaskId, generation: u64, facts: Vec<RuntimeObservation>) {
        if !facts.is_empty() {
            let _ = self.run(Actor::System, Command::RecordRuntimeFacts { task_id: task.clone(), generation, facts });
        }
    }

    fn record_turn_end(&mut self, task: &TaskId, generation: u64, turn: String) {
        if self.run(Actor::System, Command::RecordTurnBoundary { task_id: task.clone(), generation, turn, failure: None, failed: None }).is_err() {
            return;
        }
        let woken = self.state().lifecycle_journal.get(task).and_then(|journal| journal.last().cloned())
            .is_some_and(|record| matches!(record, LifecycleRecord::TurnEnded { wake: Some(_), .. }));
        if woken {
            let wakes = self.wakes.entry(task.clone()).or_default();
            *wakes += 1;
            self.seen.most_wakes = self.seen.most_wakes.max(*wakes);
        }
    }

    fn progress(&mut self, task: &TaskId) {
        self.wakes.remove(task);
    }

    /// A host that died: its owner records the end, and while the task lives starts a host that
    /// resumes the same session, as `sdk_owner` does.
    fn end_dead_hosts(&mut self) -> bool {
        let dead: Vec<TaskId> = self.world.hosts.iter().filter(|(_, host)| !host.alive && host.outbox.is_empty()).map(|(task, _)| task.clone()).collect();
        let mut moved = false;
        for task in dead {
            let host = self.world.hosts.remove(&task).expect("a dead host");
            let state = self.state();
            let Ok(lifecycle) = state.lifecycle_of(&task) else { continue };
            moved = true;
            if lifecycle.execution().map(|stamp| stamp.generation()) != Some(host.generation) || lifecycle.resource() == Some(ResourceEvidence::Released) {
                continue;
            }
            let live = !state.tasks[&task].over();
            let ended_work = host.work.iter().map(|work| work.id.clone()).collect();
            let _ = self.run(Actor::System, Command::RecordHostEnded { task_id: task.clone(), generation: host.generation, resumes: live, ended_work, by_alondra: false });
            if let Some(input) = host.offered.as_ref().filter(|input| self.state().lifecycle_of(&task).is_ok_and(|lifecycle| lifecycle.pending_inputs().iter().any(|pending| pending.as_str() == input.as_str()))) {
                let _ = self.run(Actor::System, Command::RecordDoubtfulHandoff {
                    task_id: task.clone(), generation: host.generation, input: input.clone(), text: "Model input".into(), resumes: live, by_alondra: false,
                });
            }
            if live {
                self.world.hosts.insert(task, Host::new(host.generation));
            }
        }
        moved
    }

    /// The disk's floor pauses the nodes at work, and lets them go on once it has room, as the gate does.
    fn hold_for_disk(&mut self) -> bool {
        let state = self.state();
        let mut moved = false;
        let at = self.world.now;
        for name in TASKS {
            let task = TaskId::from(name);
            let Some(agent) = state.tasks.get(&task).filter(|task| !task.over()).and_then(|task| task.agent_id.clone()) else { continue };
            let pause = state.node_pause(&agent);
            if self.world.disk_full && pause.is_none() && state.lifecycle_of(&task).is_ok_and(|lifecycle| {
                matches!(lifecycle.activity(), alondra_domain::ActivityView::Working | alondra_domain::ActivityView::OwnWork)
            }) {
                let pause = Pause { reason: PauseReason::Disk, since: at, cut: None, turn_since: None, cleanup: PauseCleanup::Done, released: false };
                moved |= self.run(Actor::System, Command::MoveNode { agent_id: agent, event: NodeEvent::Paused { pause }, at }).is_ok();
            } else if !self.world.disk_full && pause.is_some_and(|pause| pause.reason == PauseReason::Disk) {
                let _ = self.run(Actor::System, Command::TellNode { task_id: task.clone(), text: "The disk has room again: carry on".into(), from_user: false, answers: None });
                moved |= self.run(Actor::System, Command::MoveNode { agent_id: agent, event: NodeEvent::Resumed, at }).is_ok();
            }
        }
        moved
    }

    /// The gate starts queued tasks, by priority and then age, while a slot is free and the Mac and the usage limit
    /// let it.
    fn admit(&mut self) -> bool {
        if self.world.memory_tight || self.world.disk_full || self.world.usage_holds() {
            return false;
        }
        let state = self.state();
        let mut queued: Vec<(TaskPriority, Revision, TaskId)> = state.tasks.values().filter(|task| !task.over() && !self.deferred.contains_key(&task.id))
            .filter_map(|task| {
                let obligation = state.lifecycle_of(&task.id).ok()?.obligation()?;
                (obligation.owed == Owed::Room(Capacity::Gate) && !state.waits_for_turn(&task.id)).then(|| (task.priority, obligation.since, task.id.clone()))
            })
            .collect();
        queued.sort();
        let mut working = self.at_work(&state);
        let mut moved = false;
        for (_, _, task) in queued {
            if working >= self.world.slots {
                break;
            }
            if self.unadmitted.get(&task) == Some(&state.revision) {
                continue;
            }
            let agent = self.new_agent();
            let admitted = self.run(Actor::System, Command::AdmitExecution {
                provider: Provider::Claude, task_id: task.clone(), agent_id: agent,
                conversation: format!("conversation-{task}-{}", self.agents), instance: format!("instance-{task}-{}", self.agents),
                host: "sdk-host".into(), model: None, account: state.tasks[&task].account.clone(), leaves_limit: None,
            });
            if admitted.is_err() {
                self.unadmitted.insert(task, state.revision);
                continue;
            }
            moved = true;
            working += 1;
            self.granted.insert(task.clone());
            let generation = self.generation(&task).expect("an admitted execution");
            if let Some(old) = self.world.hosts.remove(&task) {
                self.world.retired.push((task.clone(), old));
            }
            let mut host = Host::new(generation);
            host.emit(Fact::Runtime(RuntimeObservation::ResourceAvailable));
            self.world.hosts.insert(task, host);
        }
        moved
    }

    /// A task whose move was deferred starts on the account it moved to once it has something to go
    /// on with and the gate has room, as `provider_transition::start_moved` does: in progress and at
    /// work or queued, or delivered and sent back.
    fn start_moved(&mut self) -> bool {
        if self.world.memory_tight || self.world.disk_full || self.world.usage_holds() {
            return false;
        }
        let state = self.state();
        self.deferred.retain(|task, _| state.tasks.get(task).is_some_and(|held| !held.over()));
        let ready: Vec<(TaskId, AccountKey)> = self.deferred.iter().filter(|(task, _)| state.lifecycle_of(task).is_ok_and(|lifecycle| {
            let goes_on = match lifecycle.phase() {
                TaskPhase::InProgress => matches!(lifecycle.live(), Some(alondra_domain::LiveState::Working(_) | alondra_domain::LiveState::WaitingCapacity(Capacity::Gate))),
                TaskPhase::Delivered => lifecycle.obligation().is_some_and(|obligation| obligation.owed == Owed::Start),
                _ => false,
            };
            !lifecycle.pending_inputs().is_empty() && goes_on
        })).map(|(task, account)| (task.clone(), account.clone())).collect();
        let mut moved = false;
        for (task, account) in ready {
            if self.at_work(&self.state()) >= self.world.slots {
                break;
            }
            let agent = self.new_agent();
            let admitted = self.run(Actor::System, Command::AdmitExecution {
                provider: Provider::Claude, task_id: task.clone(), agent_id: agent,
                conversation: format!("conversation-{task}-{}", self.agents), instance: format!("instance-{task}-{}", self.agents),
                host: "sdk-host".into(), model: None, account: Some(account.account.clone()), leaves_limit: None,
            });
            if admitted.is_err() {
                continue;
            }
            self.deferred.remove(&task);
            self.granted.insert(task.clone());
            let generation = self.generation(&task).expect("an admitted execution");
            if let Some(old) = self.world.hosts.remove(&task) {
                self.world.retired.push((task.clone(), old));
            }
            let mut host = Host::new(generation);
            host.emit(Fact::Runtime(RuntimeObservation::ResourceAvailable));
            self.world.hosts.insert(task, host);
            moved = true;
        }
        moved
    }

    /// A pending input whose turn the gate gave a slot goes to the host, whose session takes it
    /// unless it ignores it.
    fn hand_inputs(&mut self) -> bool {
        let state = self.state();
        let mut working = self.at_work(&state);
        let mut waiting: Vec<(TaskPriority, Revision, TaskId)> = state.tasks.values()
            .filter(|task| state.lifecycle_of(&task.id).is_ok_and(|lifecycle| lifecycle.input_starts_turn()
                || (lifecycle.activity() == alondra_domain::ActivityView::Continuation && lifecycle.resource() == Some(ResourceEvidence::Available)
                    && !lifecycle.pending_inputs().is_empty())))
            .filter_map(|task| Some((task.priority, state.lifecycle_of(&task.id).ok()?.obligation()?.since, task.id.clone())))
            .collect();
        waiting.sort();
        let mut moved = false;
        for (_, _, task) in waiting {
            if state.waits_for_turn(&task) && !self.granted.contains(&task) {
                if working >= self.world.slots {
                    continue;
                }
                self.granted.insert(task.clone());
                working += 1;
            }
            let Ok(lifecycle) = state.lifecycle_of(&task) else { continue };
            let Some(input) = lifecycle.pending_inputs().first().map(|input| input.as_str().to_owned()) else { continue };
            let turn = self.world.id("turn");
            let now = self.world.now;
            let Some(host) = self.world.hosts.get_mut(&task).filter(|host| host.alive && host.turn.is_none() && host.offered.is_none()) else { continue };
            host.offered = Some(input.clone());
            if host.ignores {
                host.ignores = false;
                continue;
            }
            host.offered = None;
            host.turn = Some(Running { id: turn.clone(), since: now, decision: None });
            host.emit(Fact::Runtime(RuntimeObservation::TurnStarted { turn, input }));
            moved = true;
        }
        let state = self.state();
        self.granted.retain(|task| state.waits_for_turn(task)
            || state.lifecycle_of(task).is_ok_and(|lifecycle| lifecycle.resource() == Some(ResourceEvidence::Reserved)));
        moved
    }

    /// An answer the user gave to a permission of a running turn reaches it once the gate has a
    /// slot for it, asked or not.
    fn hand_decisions(&mut self) -> bool {
        let state = self.state();
        let mut moved = false;
        for name in TASKS {
            let task = TaskId::from(name);
            let Ok(lifecycle) = state.lifecycle_of(&task) else { continue };
            if !lifecycle.answer_goes_on() || self.at_work(&state) >= self.world.slots {
                continue;
            }
            let Some(generation) = lifecycle.execution().map(|stamp| stamp.generation()) else { continue };
            let Some(epoch) = self.world.hosts.get(&task).filter(|host| host.alive && host.generation == generation)
                .and_then(|host| host.turn.as_ref()?.decision) else { continue };
            if self.run(Actor::System, Command::RecordDecisionSettled { task_id: task.clone(), generation, epoch }).is_ok() {
                let now = self.world.now;
                if let Some(turn) = self.world.host(&task).and_then(|host| host.turn.as_mut()) {
                    turn.decision = None;
                    turn.since = now;
                }
                moved = true;
            }
        }
        moved
    }

    /// Claude reads what it is told while its turn runs.
    fn consume_inputs_in_turn(&mut self) -> bool {
        let state = self.state();
        let mut moved = false;
        for (task, host) in self.world.hosts.iter_mut().filter(|(_, host)| host.alive && host.turn.is_some()) {
            let Ok(lifecycle) = state.lifecycle_of(task) else { continue };
            if lifecycle.turn().is_none() || lifecycle.execution().map(|stamp| stamp.generation()) != Some(host.generation) {
                continue;
            }
            for input in lifecycle.pending_inputs().into_iter().map(|input| input.as_str().to_owned()) {
                if host.read.insert(input.clone()) {
                    host.emit(Fact::Runtime(RuntimeObservation::InputConsumed { input }));
                    moved = true;
                }
            }
        }
        moved
    }

    /// The closer stops what a closing task still runs and records that nothing of it is left.
    fn close_ending(&mut self) -> bool {
        let state = self.state();
        let mut moved = false;
        for name in TASKS {
            let task = TaskId::from(name);
            if state.tasks[&task].state.terminal() || state.lifecycle_of(&task).ok().and_then(|lifecycle| lifecycle.obligation()).is_none_or(|obligation| obligation.owed != Owed::Close) {
                continue;
            }
            if let Some(host) = self.world.hosts.remove(&task) {
                if host.alive && self.generation(&task) == Some(host.generation) {
                    let _ = self.run(Actor::System, Command::RecordHostEnded { task_id: task.clone(), generation: host.generation, resumes: false, ended_work: vec![], by_alondra: true });
                }
            }
            let revision = self.revision();
            let agent = self.agent(&task);
            let _ = self.run(Actor::System, Command::RecordNodeEnd { task_id: task.clone(), end: NodeEnd::Closed, agent_id: agent });
            let _ = self.run(Actor::System, Command::RecordTaskCleanup { task_id: task.clone() });
            moved |= self.revision() != revision;
        }
        moved
    }

    /// The orchestrator, while it answers and the usage limit lets it, settles what it is asked.
    fn orchestrate(&mut self) -> bool {
        if !self.world.orchestrator_answers || self.world.usage_holds() {
            return false;
        }
        let mut moved = false;
        for (task, card) in self.orchestrator_cards() {
            moved |= self.answer_for_the_orchestrator(&task, &card, "Go on with your task.");
        }
        moved
    }

    fn orchestrator_cards(&self) -> Vec<(TaskId, InteractionId)> {
        let state = self.state();
        state.interactions.values().filter(|card| card.recipient == Recipient::Orchestrator && state.open(card))
            .filter_map(|card| match &card.scope {
                Scope::Task { task_id } => Some((task_id.clone(), card.id.clone())),
                _ => None,
            })
            .collect()
    }

    fn answer_for_the_orchestrator(&mut self, task: &TaskId, card: &InteractionId, text: &str) -> bool {
        let state = self.state();
        let Some(interaction) = state.interactions.get(card) else { return false };
        let orchestrator = Actor::Orchestrator { generation: 0 };
        if is_stall_case(interaction) {
            let outcome = if state.tasks.get(task).is_some_and(|task| task.stalls > STALL_ATTEMPTS) {
                StallOutcome::Close { result: "It stopped again with nothing to show.".into() }
            } else {
                StallOutcome::Instruct { text: text.into() }
            };
            let closes = matches!(outcome, StallOutcome::Close { .. });
            let settled = self.run(orchestrator, Command::SettleStall { case_id: card.clone(), task_id: task.clone(), outcome });
            if settled.is_ok() {
                self.progress(task);
            }
            if closes && settled.is_ok() {
                self.judge_close(task, "the orchestrator's close of a stall");
            }
            return settled.is_ok();
        }
        let option = match &interaction.payload {
            InteractionPayload::Question { options, .. } | InteractionPayload::ActionRequest { options, .. } => options.first().map(|option| option.id.clone()),
            _ => None,
        };
        let Some(option_id) = option else { return false };
        let inputs = BTreeMap::from([(alondra_core::domain::ANSWER_TEXT.to_owned(), alondra_core::domain::InputValue::Text(text.into()))]);
        let answered = self.run(orchestrator, Command::AnswerQuestion { attachments: vec![], interaction_id: card.clone(), option_id, inputs }).is_ok();
        if answered {
            self.progress(task);
        }
        answered
    }

    /// A close accepted leaves the task ending, ended, or delivered to the user.
    fn judge_close(&mut self, task: &TaskId, what: &str) {
        let state = self.state();
        let phase = state.lifecycle_of(task).map(|lifecycle| lifecycle.phase());
        if !state.tasks[task].over() && !matches!(phase, Ok(alondra_domain::TaskPhase::Delivered)) {
            self.seen.close_left_running.push(format!("{what} of {task} left it {phase:?}"));
        }
    }

    /// The running turn does what the script says next and ends, unless it waits on a decision.
    fn end_turn(&mut self, task: &TaskId) {
        let Some(running) = self.world.hosts.get(task).filter(|host| host.alive).and_then(|host| host.turn.clone()) else { return };
        if running.decision.is_some() {
            return;
        }
        let Some(agent) = self.agent(task) else { return };
        let generation = self.world.hosts[task].generation;
        let node = Actor::Agent { agent_id: agent };
        let turn = self.world.next_turn(task);
        if turn != Turn::Empty {
            self.progress(task);
        }
        match turn {
            Turn::Deliver => {
                let id = self.world.id("delivery");
                let _ = self.run(node, Command::PresentResult {
                    attachments: vec![], task_id: task.clone(), interaction_id: id.into(), body: "Done".into(), is_final: true, nothing_to_review: false,
                    targets: text_targets(), checklist: vec![], card: CardSpec::delivery("Review the result", &text_targets(), true),
                });
            }
            Turn::AskUser | Turn::AskOrchestrator => {
                let recipient = if turn == Turn::AskUser { Recipient::User } else { Recipient::Orchestrator };
                let id = self.world.id("question");
                let _ = self.run(node, Command::AskQuestion {
                    attachments: vec![], interaction_id: id.into(), scope: scope(task), recipient, body: "Which way?".into(), blocking: true,
                    options: yes_no(), card: CardSpec::question("Which way?", &yes_no()),
                });
            }
            Turn::AskPermission => {
                let epoch = self.world.id("epoch").trim_start_matches("epoch-").parse().expect("a number");
                let body = "Run the build";
                let asked = self.run(Actor::System, Command::RecordDecisionRequest {
                    task_id: task.clone(), generation, epoch, hold: Hold::Permission, tool: Some("Bash".into()),
                    input: serde_json::json!({ "command": "make build" }), mcp_server: None,
                    prompt: DecisionPrompt::Card { body: body.into(), options: Hold::Permission.options_offering(true), card: CardSpec::new(CardKind::Authorize, body) },
                });
                if asked.is_ok() {
                    if let Some(turn) = self.world.host(task).and_then(|host| host.turn.as_mut()) { turn.decision = Some(epoch); }
                    return;
                }
            }
            Turn::Background => {
                let work = self.world.id("work");
                let now = self.world.now;
                let host = self.world.host(task).expect("the host");
                host.emit(Fact::Runtime(RuntimeObservation::WorkStarted { turn: running.id.clone(), work: work.clone() }));
                host.work.push(Work { id: work, since: now, notifies: true });
            }
            Turn::Empty => {}
        }
        let host = self.world.host(task).expect("the host");
        host.turn = None;
        host.emit(Fact::TurnEnded(running.id));
    }

    /// Work the session left running ends; the session opens a turn to take it up when it says so.
    fn end_work(&mut self, task: &TaskId, notifies: Option<bool>) {
        let continuation = self.world.id("continuation");
        let turn = self.world.id("turn");
        let now = self.world.now;
        let Some(host) = self.world.host(task).filter(|host| host.alive && !host.work.is_empty()) else { return };
        let work = host.work.remove(0);
        host.emit(Fact::Runtime(RuntimeObservation::WorkEnded { work: work.id, continuation: continuation.clone() }));
        if !notifies.unwrap_or(work.notifies) {
            return;
        }
        if host.turn.is_some() {
            host.emit(Fact::Runtime(RuntimeObservation::WorkContinued { continuation }));
        } else {
            host.emit(Fact::Runtime(RuntimeObservation::TurnResumed { turn: turn.clone(), continuation }));
            host.turn = Some(Running { id: turn, since: now, decision: None });
        }
    }

    fn user(&mut self, act: UserAct, task: &TaskId) {
        let user = Actor::User { user_id: "u".into() };
        self.progress(task);
        let at = self.world.now;
        match act {
            UserAct::Answer => {
                let Some((card, option_id)) = self.card_to_answer(task) else { return };
                let _ = self.run(user, Command::AnswerQuestion { attachments: vec![], interaction_id: card, option_id, inputs: BTreeMap::new() });
            }
            UserAct::Tell => {
                if self.run(Actor::System, Command::TellNode { task_id: task.clone(), text: "Also check the docs".into(), from_user: true, answers: None }).is_ok() {
                    self.take_if_unowned(task);
                }
            }
            UserAct::Pause => {
                let Some(agent) = self.agent(task) else { return };
                let pause = Pause { reason: PauseReason::ByUser, since: at, cut: None, turn_since: None, cleanup: PauseCleanup::Done, released: false };
                if self.run(Actor::System, Command::MoveNode { agent_id: agent, event: NodeEvent::Paused { pause }, at }).is_ok()
                    && let Some(host) = self.world.host(task).filter(|host| host.alive)
                    && let Some(turn) = host.turn.take() {
                    host.emit(Fact::TurnEnded(turn.id));
                }
            }
            UserAct::Resume => {
                let state = self.state();
                let Some(card) = state.lifecycle_of(task).ok().and_then(|lifecycle| lifecycle.cards().into_iter()
                    .find(|(card, _)| card.0.starts_with(NODE_PAUSE_PREFIX)).map(|(card, _)| card.clone())) else { return };
                let _ = self.run(user, Command::AnswerQuestion { attachments: vec![], interaction_id: card, option_id: NODE_PAUSE_RESUME.into(), inputs: BTreeMap::new() });
            }
            UserAct::Cancel => {
                if self.run(user, Command::CancelTask { task_id: task.clone() }).is_ok() {
                    self.judge_close(task, "the user's cancel");
                }
            }
            UserAct::Move => self.move_task(task),
            UserAct::Accept => {
                let Some(delivery) = self.state().tasks[task].final_presentation.clone() else { return };
                let _ = self.run(user, Command::AcceptDelivery { presentation_id: delivery, targets: text_targets() });
            }
            UserAct::Prioritize => {
                let _ = self.run(user, Command::SetPriority { task_id: task.clone(), priority: TaskPriority::High });
            }
            UserAct::Return => {
                let Some(delivery) = self.state().tasks[task].final_presentation.clone() else { return };
                let _ = self.run(user, Command::SubmitFeedback {
                    attachments: vec![], presentation_id: delivery, targets: text_targets(), body: "Do it again".into(),
                    instruction: Instruction::Changes, merge_failure: None,
                });
            }
        }
    }

    /// The user moves the task to the other account, and the move runs as `provider_transition`
    /// does: a task that ended aborts it; otherwise the source's host is ended, and the task starts
    /// on the new account if it has something to go on with, or the move is deferred to its next start.
    fn move_task(&mut self, task: &TaskId) {
        let state = self.state();
        let Some(held) = state.tasks.get(task) else { return };
        let target = ACCOUNTS.into_iter().map(|id| AccountKey::new(Provider::Claude, AccountId::parse(id).expect("an account id")))
            .find(|account| *account != held.account_key()).expect("another account");
        let Some(expected_selection) = state.task_selection_stamp(task) else { return };
        let user = Actor::User { user_id: "u".into() };
        if self.run(user, Command::MoveTask { task_id: task.clone(), expected_selection, account: target.clone() }).is_err() {
            return;
        }
        let state = self.state();
        let held = &state.tasks[task];
        if held.pending_selection.is_none() {
            return;
        }
        let Ok(lifecycle) = state.lifecycle_of(task) else { return };
        if matches!(lifecycle.phase(), TaskPhase::Finishing | TaskPhase::Completed | TaskPhase::Cancelled) {
            let refused = MoveRefused { account: target, reason: "The task ended".into() };
            let _ = self.run(Actor::System, Command::AbortTaskSelection { task_id: task.clone(), selection: held.selection.clone(), refused: Some(refused) });
            return;
        }
        if let Some(host) = self.world.hosts.remove(task) {
            if self.generation(task) == Some(host.generation) && self.state().lifecycle_of(task).is_ok_and(|lifecycle| lifecycle.resource() != Some(ResourceEvidence::Released)) {
                let _ = self.run(Actor::System, Command::RecordHostEnded { task_id: task.clone(), generation: host.generation, resumes: false, ended_work: vec![], by_alondra: true });
            }
            self.world.retired.push((task.clone(), host));
        }
        let state = self.state();
        let ready = state.lifecycle_of(task).is_ok_and(|lifecycle| !lifecycle.pending_inputs().is_empty()
            && matches!(lifecycle.live(), Some(alondra_domain::LiveState::Working(_) | alondra_domain::LiveState::WaitingCapacity(Capacity::Gate))));
        let moved = if ready {
            let agent = self.new_agent();
            self.run(Actor::System, Command::AdmitExecution {
                provider: Provider::Claude, task_id: task.clone(), agent_id: agent,
                conversation: format!("conversation-{task}-{}", self.agents), instance: format!("instance-{task}-{}", self.agents),
                host: "sdk-host".into(), model: None, account: Some(target.account.clone()), leaves_limit: None,
            })
        } else {
            self.run(Actor::System, Command::DeferTaskMove { task_id: task.clone(), account: target.clone() })
        };
        match moved {
            Ok(_) if ready => {
                self.granted.insert(task.clone());
                if let Some(selection) = self.state().tasks[task].selection.clone() {
                    let _ = self.run(Actor::System, Command::RecordTaskSelection { task_id: task.clone(), selection, pending: None });
                }
                let generation = self.generation(task).expect("an admitted execution");
                let mut host = Host::new(generation);
                host.emit(Fact::Runtime(RuntimeObservation::ResourceAvailable));
                self.world.hosts.insert(task.clone(), host);
            }
            Ok(_) => {
                self.deferred.insert(task.clone(), target);
            }
            Err(reason) => {
                let selection = self.state().tasks[task].selection.clone();
                let _ = self.run(Actor::System, Command::AbortTaskSelection { task_id: task.clone(), selection, refused: Some(MoveRefused { account: target, reason }) });
            }
        }
    }

    /// The first card of `task` the inbox lists for the user to answer, and its first option.
    fn card_to_answer(&self, task: &TaskId) -> Option<(InteractionId, String)> {
        let state = self.state();
        let index = ReadIndex::build(&state);
        let reading = index.task(task)?;
        reading.waiting.iter().filter(|card| state.tasks[task].final_presentation.as_ref() != Some(*card)).find_map(|card| {
            match &state.interactions.get(card)?.payload {
                InteractionPayload::Question { options, answer: None } | InteractionPayload::ActionRequest { options, answer: None } => {
                    Some((card.clone(), options.first()?.id.clone()))
                }
                _ => None,
            }
        })
    }

    /// One pass of the clock: what is past its deadline, and for the rows in `KNOWN_GAPS`, what
    /// the model does about it in their place.
    fn clock(&mut self) {
        let state = self.state();
        let now = self.world.now;
        let obligations: BTreeMap<TaskId, (Revision, Owed)> = state.tasks.keys()
            .filter_map(|task| Some((task.clone(), state.lifecycle_of(task).ok()?.obligation().map(|obligation| (obligation.since, obligation.owed))?)))
            .collect();
        self.owing.retain(|task, (since, owed, _)| obligations.get(task) == Some(&(*since, *owed)));
        for (task, (since, owed)) in obligations {
            self.owing.entry(task).or_insert((since, owed, now));
        }
        let late = self.watch.step(&state, &Turns { state: &state, granted: &self.granted, usage_holds: self.world.usage_holds() }, now);
        for (task, action) in self.watch.enforce(&late, now) {
            self.by_the_clock(|machine| machine.apply(&task, action));
        }
    }

    /// Runs what a pass of the clock does, noting every card for the user it opens.
    fn by_the_clock(&mut self, act: impl FnOnce(&mut Self)) {
        let before = self.open_user_cards();
        act(self);
        let opened: Vec<InteractionId> = self.open_user_cards().difference(&before).cloned().collect();
        self.seen.clock_cards.extend(opened);
    }

    /// What `obligation_watch::apply` does for `action`, through the commands `sdk_owner::untaken`
    /// stages and the owner's requeue.
    fn apply(&mut self, task: &TaskId, action: obligation_watch::Action) {
        let Some(generation) = self.generation(task) else { return };
        match action {
            obligation_watch::Action::OfferAgain => {
                let state = self.state();
                let Ok(lifecycle) = state.lifecycle_of(task) else { return };
                let doubted: Vec<String> = lifecycle.doubts().into_iter().map(|(_, input)| input.as_str().to_owned()).collect();
                let offered = self.world.hosts.get(task).and_then(|host| host.offered.clone()).filter(|input| !doubted.contains(input));
                if let Some(input) = offered {
                    if let Some(host) = self.world.host(task) { host.offered = None; }
                    let _ = self.run(Actor::System, Command::RecordDoubtfulHandoff {
                        task_id: task.clone(), generation, input, text: "Model input".into(), resumes: true, by_alondra: true,
                    });
                }
            }
            obligation_watch::Action::Requeue => {
                let ended = self.run(Actor::System, Command::RecordHostEnded {
                    task_id: task.clone(), generation, resumes: true, ended_work: vec![], by_alondra: true,
                });
                if ended.is_ok() && let Some(old) = self.world.hosts.insert(task.clone(), Host::new(generation)) {
                    self.world.retired.push((task.clone(), old));
                }
            }
            obligation_watch::Action::Wake => {
                let state = self.state();
                let Ok(lifecycle) = state.lifecycle_of(task) else { return };
                let paused = state.tasks.get(task).and_then(|held| held.agent_id.as_ref()).and_then(|agent| state.node_pause(agent)).is_some_and(|pause| !pause.released);
                if lifecycle.resource() != Some(ResourceEvidence::Available) || lifecycle.activity() != alondra_domain::ActivityView::Continuation
                    || !lifecycle.pending_inputs().is_empty() || !lifecycle.cards().is_empty() || lifecycle.awaits().is_some()
                    || !matches!(lifecycle.phase(), TaskPhase::InProgress | TaskPhase::Delivered) || paused {
                    return;
                }
                let _ = self.run(Actor::System, Command::TellNode { task_id: task.clone(), text: crate::sdk_owner::untaken::WORK_ENDED.into(), from_user: false, answers: None });
            }
            obligation_watch::Action::Answer => {
                let state = self.state();
                for (step, _) in crate::orchestrator_inbox::overdue(&state, &BTreeSet::from([task.clone()])) {
                    self.carry_out(&step);
                }
            }
        }
    }

    /// What `orchestrator_inbox::carry_out` does with a step the orchestrator owes past its
    /// deadline, through the same commands.
    fn carry_out(&mut self, step: &crate::orchestrator_inbox::Step) {
        use crate::orchestrator_inbox::Step;
        match step {
            Step::Tell { id, card, text } | Step::Remind { id, card, text, .. } => {
                if self.state().interactions.contains_key(&InteractionId(id.clone())) {
                    return;
                }
                let _ = self.run(Actor::System, Command::PublishInformation {
                    attachments: vec![], interaction_id: id.clone().into(), scope: Scope::Orchestrator { request_id: None }, recipient: Recipient::Orchestrator,
                    card: CardSpec { system: Some(card.clone()), ..CardSpec::new(CardKind::Know, "Task update").content(ContentKind::Report, text) },
                    body: text.clone(),
                });
            }
            Step::Withdraw { question } => {
                let _ = self.run(Actor::System, Command::Withdraw { interaction_id: question.clone() });
            }
            Step::SettleStall { case, task, outcome } => {
                let closes = matches!(outcome, StallOutcome::Unsettled);
                if self.run(Actor::System, Command::SettleStall { case_id: case.clone(), task_id: task.clone(), outcome: outcome.clone() }).is_ok() && closes {
                    self.judge_close(task, "Alondra's close of an unsettled stall");
                }
            }
            Step::ReleaseUnreviewed { review, task, note } => {
                let _ = self.run(Actor::System, Command::SettlePlanReview {
                    review_id: review.clone(), task_id: task.clone(), outcome: PlanReviewOutcome::Unchanged { reason: note.clone() },
                });
            }
            _ => {}
        }
    }
}
