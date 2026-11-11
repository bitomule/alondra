#![allow(dead_code)]

use alondra_domain::*;
use alondra_foundation::{AgentId, TaskId};

pub fn input(value: &str) -> InputId {
    InputId::new(value).unwrap()
}
pub fn turn(value: &str) -> TurnId {
    TurnId::new(value).unwrap()
}
pub fn work(value: &str) -> WorkId {
    WorkId::new(value).unwrap()
}
pub fn continuation(value: &str) -> ContinuationId {
    ContinuationId::new(value).unwrap()
}
pub fn operation(value: &str) -> OperationId {
    OperationId::new(value).unwrap()
}
#[track_caller]
pub fn apply(task: &mut TaskLifecycle, event: Event) {
    task.apply(task.revision(), event).unwrap();
}
pub fn admit(task: &mut TaskLifecycle, instance: &str) {
    apply(
        task,
        Event::Admit {
            node: AgentId::from("node"),
            conversation: ConversationId::new("conversation").unwrap(),
            instance: InstanceId::new(instance).unwrap(),
            host: HostId::new("host").unwrap(),
        },
    );
}
#[track_caller]
pub fn fact(task: &mut TaskLifecycle, fact: RuntimeFact) {
    let stamp = task.execution().unwrap().clone();
    let cursor = task.cursor().unwrap() + 1;
    apply(
        task,
        Event::Runtime {
            stamp,
            cursor,
            fact,
        },
    );
}
pub fn running() -> TaskLifecycle {
    let mut task = TaskLifecycle::planned(TaskId::from("task"), input("initial")).unwrap();
    admit(&mut task, "instance");
    fact(&mut task, RuntimeFact::ResourceAvailable);
    fact(
        &mut task,
        RuntimeFact::TurnStarted {
            turn: turn("root"),
            trigger: Trigger::Input(input("initial")),
        },
    );
    task
}

/// One step of an arbitrary history: an event the task may or may not accept.
pub fn arbitrary_event(task: &TaskLifecycle, action: u8) -> Option<Event> {
    let runtime = |fact| task.execution().cloned().map(|stamp| Event::Runtime { stamp, cursor: task.cursor().unwrap() + 1, fact });
    match action {
        0 => Some(Event::Admit { node: "node".into(), conversation: ConversationId::new("conversation").unwrap(), instance: InstanceId::new("instance").unwrap(), host: HostId::new("host").unwrap() }),
        1 => runtime(RuntimeFact::ResourceAvailable),
        2 => runtime(RuntimeFact::TurnStarted { turn: turn("root"), trigger: Trigger::Input(input("initial")) }),
        3 => runtime(RuntimeFact::WorkStarted { turn: turn("root"), work: work("child") }),
        4 => runtime(RuntimeFact::WorkEnded { work: work("child"), continuation: continuation("wake") }),
        5 => runtime(RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: None, stalled: false }),
        6 => runtime(RuntimeFact::Ask { card: "first".into(), callback: CallbackId::new("first").unwrap(), reason: WaitReason::Permission }),
        7 => Some(Event::Decide { card: "first".into(), decision: Decision::Allow }),
        8 => Some(Event::Present { delivery: "result".into() }),
        9 => Some(Event::Accept { delivery: "result".into(), merge: None }),
        10 => Some(Event::Finish { intent: FinishIntent::Cancel, cleanup: operation("cleanup"), post_merge: None }),
        11 => task.drain().next().cloned().map(|obligation| Event::DrainObserved { obligation }),
        12 => Some(Event::Reopen { input: input("initial") }),
        13 => runtime(RuntimeFact::ResourceReleased { problem_card: "recovery".into() }),
        14 => runtime(RuntimeFact::ConnectionLost { problem_card: "recovery".into() }),
        15 => runtime(RuntimeFact::Ask { card: "second".into(), callback: CallbackId::new("second").unwrap(), reason: WaitReason::Permission }),
        16 => runtime(RuntimeFact::CallbackAborted { callback: CallbackId::new("first").unwrap(), unheard_card: "unheard".into() }),
        17 => runtime(RuntimeFact::CallbackSettled { callback: CallbackId::new("first").unwrap(), continuation: continuation("callback-wake") }),
        18 => Some(Event::PlanApproved),
        19 => Some(Event::WaitForDependencies { dependencies: vec![TaskId::from("dependency")] }),
        20 => Some(Event::DependenciesResolved { input: input("dependencies") }),
        21 => Some(Event::Pause { card: "pause".into(), owner: Owner::User }),
        39 => Some(Event::Pause { card: "pause".into(), owner: Owner::Capacity(Capacity::Usage) }),
        40 => Some(Event::Pause { card: "pause".into(), owner: Owner::Capacity(Capacity::ClaudeApi) }),
        41 => Some(Event::Accept { delivery: "result".into(), merge: Some(operation("merge")) }),
        42 => Some(Event::Finish { intent: FinishIntent::Complete, cleanup: operation("cleanup"), post_merge: Some(operation("after-merge")) }),
        43 => Some(Event::PostMergeDeclared { operation: operation("after-merge") }),
        44 => runtime(RuntimeFact::HostEnded { unheard_card: "ended".into(), resume: None }),
        45 => runtime(RuntimeFact::HostEnded { unheard_card: "ended".into(), resume: Some(input("resumed")) }),
        46 => runtime(RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: Some(input("wake")), awaits: None, stalled: false }),
        47 => runtime(RuntimeFact::TurnStarted { turn: turn("woken"), trigger: Trigger::Input(input("wake")) }),
        48 => runtime(RuntimeFact::TurnEnded { turn: turn("woken"), unheard_card: "unheard-woken".into(), wake: Some(input("wake-again")), awaits: None, stalled: false }),
        49 => runtime(RuntimeFact::TurnEnded { turn: turn("told"), unheard_card: "unheard-told".into(), wake: Some(input("wake")), awaits: None, stalled: false }),
        50 => runtime(RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: Some(Await::User("question".into())), stalled: false }),
        51 => runtime(RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "unheard".into(), wake: None, awaits: Some(Await::Tasks(Several::new([TaskId::from("dependency")]).unwrap())), stalled: false }),
        52 => runtime(RuntimeFact::TurnEnded { turn: turn("woken"), unheard_card: "unheard-woken".into(), wake: None, awaits: Some(Await::Machine), stalled: false }),
        53 => runtime(RuntimeFact::TurnStarted { turn: turn("woken"), trigger: Trigger::Input(input("wake-again")) }),
        54 => Some(Event::Pause { card: "pause".into(), owner: Owner::Capacity(Capacity::Machine) }),
        55 => Some(Event::EndPause { card: "pause".into() }),
        56 => runtime(RuntimeFact::TurnEnded { turn: turn("told"), unheard_card: "unheard-told".into(), wake: None, awaits: None, stalled: false }),
        57 => runtime(RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "stalled".into(), wake: Some(input("wake")), awaits: None, stalled: true }),
        58 => runtime(RuntimeFact::TurnEnded { turn: turn("root"), unheard_card: "stalled".into(), wake: None, awaits: None, stalled: true }),
        59 => runtime(RuntimeFact::TurnEnded { turn: turn("woken"), unheard_card: "stalled-woken".into(), wake: Some(input("wake-again")), awaits: None, stalled: true }),
        60 => runtime(RuntimeFact::TurnEnded { turn: turn("woken"), unheard_card: "stalled-woken".into(), wake: None, awaits: None, stalled: true }),
        61 => runtime(RuntimeFact::TurnEnded { turn: turn("woken"), unheard_card: "stalled-woken".into(), wake: None, awaits: Some(Await::Orchestrator("question".into())), stalled: true }),
        62 => Some(Event::AwaitEnded { unheard_card: "stalled-lapsed".into(), wake: Some(input("wake-lapsed")), stalled: true }),
        63 => Some(Event::AwaitEnded { unheard_card: "stalled-lapsed".into(), wake: None, stalled: true }),
        64 => runtime(RuntimeFact::TurnStarted { turn: turn("woken"), trigger: Trigger::Input(input("wake-lapsed")) }),
        65 => Some(Event::Answer { card: "stalled-woken".into(), input: input("instruction") }),
        66 => runtime(RuntimeFact::TurnStarted { turn: turn("woken"), trigger: Trigger::Input(input("instruction")) }),
        67 => Some(Event::Unfinished { unheard_card: "stalled-owed".into(), wake: Some(input("owed")) }),
        68 => Some(Event::Unfinished { unheard_card: "stalled-owed".into(), wake: None }),
        69 => runtime(RuntimeFact::Ask { card: "asked".into(), callback: CallbackId::new("asked").unwrap(), reason: WaitReason::Question }),
        70 => Some(Event::KeepQuestion { card: "asked".into() }),
        71 => Some(Event::Answer { card: "asked".into(), input: input("kept-answer") }),
        22 => Some(Event::Resume { card: "pause".into(), input: input("resume") }),
        23 => runtime(RuntimeFact::Notification),
        24 => Some(Event::RetireReleasedExecution { problem_card: "recovery".into(), input: input("retry") }),
        25 => Some(Event::RequestControl { control: operation("interrupt"), kind: Control::Interrupt { turn: task.turn().cloned() } }),
        26 => Some(Event::RequestControl { control: operation("model"), kind: Control::SetModel { model: "claude-sonnet".into() } }),
        27 => Some(Event::RequestControl { control: operation("mcp"), kind: Control::ReadMcpStatus }),
        28 => Some(Event::Compact { input: input("compact") }),
        29 => runtime(RuntimeFact::AnsweredByPolicy { callback: CallbackId::new("policy").unwrap(), decision: Decision::Deny { message: "Nodes cannot act as the user".into() } }),
        30 => Some(Event::Decide { card: "second".into(), decision: Decision::Deny { message: "No".into() } }),
        31 => Some(Event::MoveCard { from: "first".into(), to: "moved".into() }),
        32 => Some(Event::MoveCard { from: "moved".into(), to: "first".into() }),
        34 => task.turn().cloned().and_then(|turn| runtime(RuntimeFact::InterruptAccepted { turn })),
        35 => task.turn().cloned().map(|turn| Event::RequestControl { control: operation("force"), kind: Control::ForceStop { turn } }),
        36 => Some(Event::Tell { input: input("told"), answers: None }),
        37 => runtime(RuntimeFact::TurnStarted { turn: turn("told"), trigger: Trigger::Input(input("told")) }),
        38 => runtime(RuntimeFact::TurnStarted { turn: turn("own"), trigger: Trigger::Unprompted }),
        72 => runtime(RuntimeFact::TakenOver),
        73 => runtime(RuntimeFact::TakeoverUnanswered),
        _ => Some(Event::HandBack { delivery: "result".into(), input: input("changes") }),
    }
}
