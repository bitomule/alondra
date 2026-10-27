//! The usage limit as a fact of a task's lifecycle. Whatever Claude does while the limit is out
//! (its own turns, background work ending, the user's pauses), every turn the limit ends is
//! recorded, no task is left at work with nothing that works for it, and once usage is back no
//! task still waits for it. Nor, whatever Claude does, even in a turn its host never reported, is
//! a task left at work with nothing that works for it once a turn of its node ends.
use super::lifecycle_tests::{exec, sdk_turn, world};
use super::*;
use alondra_domain::{Capacity, LiveState, Worker};
use proptest::prelude::*;

const TASKS: [(&str, &str); 2] = [("t", "a"), ("u", "b")];

#[derive(Clone, Debug)]
enum Act {
    Tell(usize),
    TakeInput(usize),
    OwnTurn(usize),
    StartWork(usize),
    EndWork(usize),
    TakeUpWork(usize),
    UnheardTurn(usize),
    RegisterPr(usize),
    EndTurn(usize),
    Pause(usize, bool),
    Resume(usize),
    LimitOut,
    LimitBack,
}

fn act() -> impl Strategy<Value = Act> {
    let task = 0..TASKS.len();
    prop_oneof![
        2 => task.clone().prop_map(Act::Tell),
        3 => task.clone().prop_map(Act::TakeInput),
        2 => task.clone().prop_map(Act::OwnTurn),
        2 => task.clone().prop_map(Act::StartWork),
        2 => task.clone().prop_map(Act::EndWork),
        2 => task.clone().prop_map(Act::TakeUpWork),
        1 => task.clone().prop_map(Act::UnheardTurn),
        1 => task.clone().prop_map(Act::RegisterPr),
        4 => task.clone().prop_map(Act::EndTurn),
        1 => (task.clone(), any::<bool>()).prop_map(|(task, by_user)| Act::Pause(task, by_user)),
        1 => task.prop_map(Act::Resume),
        2 => Just(Act::LimitOut),
        2 => Just(Act::LimitBack),
    ]
}

/// What the task's Claude is doing, as its host would report it.
#[derive(Clone, Debug, Default)]
struct Claude {
    turn: Option<String>,
    work: Vec<String>,
    /// Work that ended after its turn: the next turn Claude opens reads its notification.
    ended: Vec<String>,
    /// Claude ran a turn its host never reported, and no reported turn has ended since.
    unheard: bool,
}

struct World {
    s: DomainState,
    claude: [Claude; 2],
    out: bool,
}

impl World {
    fn new() -> Self {
        let mut s = world();
        for (task, node) in TASKS {
            sdk_turn(&mut s, task, node);
        }
        let claude = Claude { turn: Some("turn-1".into()), ..Claude::default() };
        Self { s, claude: [claude.clone(), claude], out: false }
    }

    fn facts(&mut self, at: usize, facts: Vec<RuntimeObservation>) -> bool {
        exec(&mut self.s, Actor::System, Command::RecordRuntimeFacts { task_id: TASKS[at].0.into(), generation: 1, facts }).is_ok()
    }

    fn pending(&self, at: usize) -> Option<String> {
        self.s.lifecycle_of(&TASKS[at].0.into()).ok()?.pending_inputs().first().map(|input| input.as_str().to_owned())
    }

    /// The turn of Claude's `at` ends: on the limit while it is out. Never refused.
    fn end_turn(&mut self, at: usize) -> Result<(), String> {
        let Some(turn) = self.claude[at].turn.clone() else { return Ok(()) };
        let failure = self.out.then(|| SdkApiFailure { error: "rate_limit".into(), status: Some(429), resets_at: None });
        exec(&mut self.s, Actor::System, Command::RecordTurnBoundary { task_id: TASKS[at].0.into(), generation: 1, turn: turn.clone(), failure, failed: None })
            .map_err(|error| format!("the end of {turn} of {} was refused: {error}", TASKS[at].0))?;
        self.claude[at].turn = None;
        self.claude[at].unheard = false;
        Ok(())
    }

    fn take_input(&mut self, at: usize, n: usize) -> bool {
        let Some(input) = self.pending(at).filter(|_| self.claude[at].turn.is_none()) else { return false };
        let turn = format!("turn-{n}");
        let taken = self.facts(at, vec![RuntimeObservation::TurnStarted { turn: turn.clone(), input }]);
        if taken {
            self.claude[at].turn = Some(turn);
            self.claude[at].ended.clear();
        }
        taken
    }

    fn perform(&mut self, act: &Act, n: usize) -> Result<(), String> {
        let node = |at: usize| TASKS[at].1;
        match *act {
            Act::Tell(at) => drop(exec(&mut self.s, Actor::System, Command::TellNode { task_id: TASKS[at].0.into(), text: format!("Notice {n}") })),
            Act::TakeInput(at) => drop(self.take_input(at, n)),
            Act::OwnTurn(at) if self.claude[at].turn.is_none() => {
                let turn = format!("turn-{n}");
                if self.facts(at, vec![RuntimeObservation::TurnUnprompted { turn: turn.clone() }]) {
                    self.claude[at].turn = Some(turn);
                    self.claude[at].ended.clear();
                }
            }
            Act::StartWork(at) => if let Some(turn) = self.claude[at].turn.clone() {
                let work = format!("claude-task:{n}");
                if self.facts(at, vec![RuntimeObservation::WorkStarted { turn, work: work.clone() }]) {
                    self.claude[at].work.push(work);
                }
            },
            Act::EndWork(at) => if let Some(work) = self.claude[at].work.first().cloned() {
                let continuation = format!("claude-notification:{n}");
                let mut facts = vec![RuntimeObservation::WorkEnded { work, continuation: continuation.clone() }];
                let inside = self.claude[at].turn.is_some();
                if inside {
                    facts.push(RuntimeObservation::WorkContinued { continuation: continuation.clone() });
                }
                if self.facts(at, facts) {
                    self.claude[at].work.remove(0);
                    if !inside {
                        self.claude[at].ended.push(continuation);
                    }
                }
            },
            Act::TakeUpWork(at) => if let Some(continuation) = self.claude[at].ended.first().cloned().filter(|_| self.claude[at].turn.is_none()) {
                let turn = format!("turn-{n}");
                if self.facts(at, vec![RuntimeObservation::TurnResumed { turn: turn.clone(), continuation }]) {
                    self.claude[at].ended.clear();
                    self.claude[at].turn = Some(turn);
                }
            },
            Act::UnheardTurn(at) => if self.claude[at].turn.is_none() && !self.claude[at].ended.is_empty() {
                self.claude[at].ended.clear();
                self.claude[at].unheard = true;
            },
            Act::RegisterPr(at) => drop(exec(&mut self.s, super::lifecycle_tests::node(node(at)), Command::RegisterPr {
                task_id: TASKS[at].0.into(), pr_id: format!("pr-{at}").into(), host: "github.com".into(), repository: "owner/repo".into(), number: 1000 + at as u64,
                head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
            })),
            Act::EndTurn(at) => self.end_turn(at)?,
            Act::Pause(at, by_user) => {
                let reason = if by_user { PauseReason::ByUser } else { PauseReason::Declared { reason: "The build queue is full".into() } };
                let pause = Pause { reason, since: n as Millis, cut: None, turn_since: None, cleanup: PauseCleanup::Done, released: false };
                let _ = exec(&mut self.s, Actor::System, Command::MoveNode { agent_id: node(at).into(), event: NodeEvent::Paused { pause }, at: n as Millis });
            }
            Act::Resume(at) => {
                let _ = exec(&mut self.s, Actor::System, Command::TellNode { task_id: TASKS[at].0.into(), text: format!("Carry on {n}") });
                let _ = exec(&mut self.s, Actor::System, Command::MoveNode { agent_id: node(at).into(), event: NodeEvent::Resumed, at: n as Millis });
            }
            Act::LimitOut => self.out = true,
            Act::LimitBack => {
                self.out = false;
                self.usage_back(n)?;
            }
            Act::OwnTurn(_) => {}
        }
        Ok(())
    }

    /// What the daemon does once usage is back: it probes Claude with a task the limit holds, whose
    /// Claude answers, which lets every other task the limit held go on.
    fn usage_back(&mut self, n: usize) -> Result<(), String> {
        let Ok(_) = exec(&mut self.s, Actor::System, Command::ProbeClaudeAccess { provider: crate::accounts::Provider::Claude, hold: ClaudeHold::Usage, account: None }) else { return Ok(()) };
        let probed = (0..TASKS.len()).find(|at| matches!(self.s.lifecycle_journal[&TaskId::from(TASKS[*at].0)].last(), Some(LifecycleRecord::Resumed { .. })))
            .ok_or("the probe resumed no task")?;
        if self.claude[probed].turn.is_none() && !self.take_input(probed, n) {
            return Err(format!("the probe of {} could not take its input", TASKS[probed].0));
        }
        if !self.facts(probed, vec![RuntimeObservation::ModelAnswered]) {
            return Err(format!("the answer of {}'s Claude was refused", TASKS[probed].0));
        }
        Ok(())
    }

    fn holds(&self) -> Result<(), String> {
        for (at, (task, _)) in TASKS.iter().enumerate() {
            let live = self.s.live_state(&TaskId::from(*task));
            let claude = &self.claude[at];
            let works = !claude.work.is_empty() || !claude.ended.is_empty();
            match &live {
                Some(LiveState::Working(Worker::Turn)) if claude.turn.is_none() => return Err(format!("{task} is in a turn its Claude does not run")),
                Some(LiveState::Working(Worker::OwnWork)) if !works => return Err(format!("{task} runs work its Claude does not have")),
                Some(LiveState::Working(_)) if claude.turn.is_none() && !works && !claude.unheard && self.pending(at).is_none() =>
                    return Err(format!("{task} is at work with no turn, no work and nothing to read: {live:?}")),
                Some(LiveState::WaitingCapacity(Capacity::Usage)) if !self.out => return Err(format!("{task} waits for usage that is back")),
                _ => {}
            }
        }
        Ok(())
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn a_usage_limit_and_its_end_leave_no_task_at_work_for_nothing_nor_waiting_for_usage_that_is_back(acts in prop::collection::vec(act(), 1..60)) {
        let mut world = World::new();
        for (n, act) in acts.iter().enumerate() {
            let n = n + 2;
            if let Err(why) = world.perform(act, n).and_then(|()| world.holds()) {
                prop_assert!(false, "after {:?} (step {}) in {:?}: {}", act, n, acts, why);
            }
        }
    }
}

/// The two cases of 25 Oct as one sequence: the limit ends a turn while background work runs, the
/// work wakes Claude's own turn, which the limit ends again, and usage coming back lets the task go on.
#[test]
fn the_limit_twice_in_a_row_and_its_end_leave_the_task_at_work_again() {
    let mut world = World::new();
    let acts = [Act::StartWork(0), Act::LimitOut, Act::EndTurn(0), Act::EndWork(0), Act::TakeUpWork(0), Act::EndTurn(0), Act::LimitBack];
    for (n, act) in acts.iter().enumerate() {
        world.perform(act, n + 2).unwrap();
        world.holds().unwrap();
    }
    assert!(matches!(world.s.live_state(&"t".into()), Some(LiveState::Working(_))), "{:?}", world.s.live_state(&"t".into()));
}
