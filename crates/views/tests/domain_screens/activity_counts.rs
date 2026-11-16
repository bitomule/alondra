//! The phone's Live Activity and the status header count from one place: whatever the domain went
//! through, the three counts the daemon sends are the ones the header shows.
use alondra_domain_rules::*;
use alondra_views as views;
use std::collections::{BTreeMap, BTreeSet};
use alondra_views::{Client, Lang, LiveSnapshot, RenderContext, Transport};
use proptest::prelude::*;

fn context<'a>(state: &'a DomainState, index: &'a ReadIndex, live: &'a LiveSnapshot) -> RenderContext<'a> {
    RenderContext {
        domain: state, index, live, client: Client::Ios, lang: Lang::En,
        transport: Transport::Loopback, cursor: "cursor", rendered_at: "",
    }
}

/// What the Live Activity is sent, and what the status header shows of the same state.
fn surfaces(state: &DomainState, index: &ReadIndex) -> (ActivityCounts, ActivityCounts, u32) {
    let live = LiveSnapshot::default();
    let view = views::build("home", context(state, index, &live)).expect("home renders");
    let count = |id: &str| view.status.counts.iter().find(|count| count.id == id).map(|count| count.value).expect(id);
    let header = ActivityCounts {
        inbox: count("inbox"), needs_you: count("needs_you"), working: count("working"), queued: count("queued"),
        paused: count("paused"), waiting: count("other_waiting"),
    };
    assert_eq!(header.tasks(), count("tasks"), "the categories add up to the open tasks");
    (views::activity_counts(&context(state, index, &live)), header, view.status.badge)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn the_activity_counts_are_the_status_counts(steps in prop::collection::vec(step(), 1..30)) {
        let mut s = three_tasks();
        for (n, step) in steps.iter().enumerate() {
            let _ = run(&mut s, step, n);
            let index = ReadIndex::build(&s);
            let (activity, header, badge) = surfaces(&s, &index);
            prop_assert_eq!(activity, header, "after {:?} in {:?}", step, steps);
            prop_assert_eq!(activity.inbox, badge, "the Inbox is the icon badge");
            prop_assert_eq!(activity.inbox, views::inbox_badge(&index));
        }
    }
}

#[test]
fn the_generated_domains_reach_every_count() {
    use Step;
    let mut s = three_tasks();
    for (n, step) in [Step::Start(0), Step::Ask(0), Step::Start(1)].iter().enumerate() {
        let _ = run(&mut s, step, n);
    }
    let (activity, _, _) = surfaces(&s, &ReadIndex::build(&s));
    assert!(activity.inbox > 0 && activity.needs_you > 0 && activity.working > 0, "{activity:?}");
    assert!(activity.any());
    assert!(!ActivityCounts::default().any());
}

const TASKS: [&str; 2] = ["a-task", "b-task"];

const SDK: &str = "s-task";

#[derive(Clone, Debug)]
pub(super) enum Step {
    Start(usize),
    Ask(usize),
    Answer(usize),
    Deliver(usize),
    Accept(usize),
    Complete(usize),
    Reopen(usize),
    Cancel(usize),
    NodeEnded(usize),
    Tell,
    TakeInput,
    TurnEnded,
    Stale,
}

pub(super) fn step() -> impl Strategy<Value = Step> {
    let any_task = 0..TASKS.len() + 1;
    prop_oneof![
        any_task.clone().prop_map(Step::Start),
        any_task.clone().prop_map(Step::Ask),
        any_task.clone().prop_map(Step::Answer),
        any_task.clone().prop_map(Step::Deliver),
        any_task.clone().prop_map(Step::Accept),
        any_task.clone().prop_map(Step::Complete),
        any_task.clone().prop_map(Step::Reopen),
        any_task.clone().prop_map(Step::Cancel),
        any_task.prop_map(Step::NodeEnded),
        Just(Step::Tell),
        Just(Step::TakeInput),
        Just(Step::TurnEnded),
        Just(Step::Stale),
    ]
}

/// Two `claude --bg` tasks with their nodes and one task whose SDK execution runs a turn.
pub(super) fn three_tasks() -> DomainState {
    let mut s = world();
    task(&mut s, TASKS[0], Some("a"));
    task(&mut s, TASKS[1], Some("b"));
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: "c".into(), name: "c".into(), at: 0 });
    sdk_turn(&mut s, SDK, "c");
    s
}

fn must(s: &mut DomainState, actor: Actor, command: Command) -> CommandOutcome {
    let shown = format!("{command:?}");
    match exec(s, actor, command) {
        Ok(outcome) => outcome,
        Err(error) => panic!("{shown} was refused: {error}"),
    }
}

fn of(at: usize) -> (&'static str, &'static str) {
    [(TASKS[0], "a"), (TASKS[1], "b"), (SDK, "c")][at]
}

fn delivery(task: &str, n: usize) -> InteractionId {
    format!("delivery-{task}-{n}").into()
}

fn targets() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::Text { text: "Done".into() }]
}

fn last_delivery(s: &DomainState, task: &str) -> Option<InteractionId> {
    s.interactions.values().filter(|i| i.id.0.starts_with(&format!("delivery-{task}-"))).max_by_key(|i| i.order).map(|i| i.id.clone())
}

pub(super) fn run(s: &mut DomainState, step: &Step, n: usize) -> Result<CommandOutcome, DomainError> {
    let options = || vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None }];
    match *step {
        Step::Start(at) => exec(s, node(of(at).1), Command::StartTask { task_id: of(at).0.into() }),
        Step::Ask(at) => exec(s, node(of(at).1), Command::AskQuestion {
            attachments: vec![], interaction_id: format!("question-{n}").into(), scope: Scope::Task { task_id: of(at).0.into() },
            recipient: Recipient::User, body: "Which way?".into(), blocking: true, options: options(),
            card: CardSpec::question("Choose the way", &options()),
        }),
        Step::Answer(at) => {
            let open = s.interactions.values()
                .filter(|i| i.scope == Scope::Task { task_id: of(at).0.into() } && i.id.0.starts_with("question-") && s.open(i))
                .map(|i| i.id.clone()).next().unwrap_or_else(|| "question-none".into());
            exec(s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: open, option_id: "yes".into(), inputs: BTreeMap::new() })
        }
        Step::Deliver(at) => exec(s, node(of(at).1), Command::PresentResult {
            attachments: vec![], task_id: of(at).0.into(), interaction_id: delivery(of(at).0, n), body: "Done".into(), is_final: true,
            nothing_to_review: false, targets: targets(), checklist: vec![], card: CardSpec::delivery("Review the result", &[], true),
        }),
        Step::Accept(at) => {
            let presentation_id = last_delivery(s, of(at).0).unwrap_or_else(|| delivery(of(at).0, n));
            exec(s, user(), Command::AcceptDelivery { presentation_id, targets: targets() })
        }
        Step::Complete(at) => {
            let presentation_id = last_delivery(s, of(at).0).unwrap_or_else(|| delivery(of(at).0, n));
            exec(s, Actor::System, Command::CompleteTask { task_id: of(at).0.into(), presentation_id })
        }
        Step::Reopen(at) => exec(s, user(), Command::ReopenTask { task_id: of(at).0.into() }),
        Step::Cancel(at) => exec(s, user(), Command::CancelTask { task_id: of(at).0.into(), reason: None }),
        Step::NodeEnded(at) => exec(s, Actor::System, Command::RecordNodeEnd { task_id: of(at).0.into(), end: NodeEnd::Closed, agent_id: Some(of(at).1.into()) }),
        Step::Tell => exec(s, Actor::System, Command::TellNode { task_id: SDK.into(), text: format!("Notice {n}"), from_user: false, answers: None }),
        Step::TakeInput => {
            let lifecycle = s.lifecycle_of(&SDK.into())?;
            let input = lifecycle.pending_inputs().first().map_or_else(|| "input:none".into(), |input| input.as_str().to_owned());
            exec(s, Actor::System, Command::RecordRuntimeFacts {
                task_id: SDK.into(), generation: lifecycle.generation(),
                facts: vec![RuntimeObservation::TurnStarted { turn: format!("turn-{n}"), input }],
            })
        }
        Step::TurnEnded => {
            let turn = s.lifecycle_journal[&TaskId::from(SDK)].iter().rev().find_map(|record| match record {
                LifecycleRecord::TurnStarted { turn, .. } => Some(turn.clone()),
                _ => None,
            }).unwrap_or_else(|| "turn-none".into());
            let generation = s.lifecycle_of(&SDK.into())?.generation();
            exec(s, Actor::System, Command::RecordTurnBoundary { task_id: SDK.into(), generation, turn, failure: None, failed: None })
        }
        Step::Stale => s.execute(&Actor::System, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: format!("stale-{n}").into(),
            expected_revision: s.revision.saturating_sub(1), command: Command::TellNode { task_id: SDK.into(), text: "Late".into(), from_user: false, answers: None },
        }),
    }
}

pub(super) fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}

pub(super) fn node(id: &str) -> Actor {
    Actor::Agent { agent_id: id.into() }
}

pub(super) fn exec(s: &mut DomainState, actor: Actor, command: Command) -> Result<CommandOutcome, DomainError> {
    s.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision, command,
    })
}

pub(super) fn task(s: &mut DomainState, id: &str, agent: Option<&str>) {
    must(s, orchestrator(), Command::CreateTask {
        task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: agent.map(Into::into), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")), plan_items: None, continues: None,
    });
}

pub(super) fn world() -> DomainState {
    let mut s = DomainState::new("epoch");
    for agent in ["a", "b"] {
        must(&mut s, Actor::System, Command::RegisterAgent { agent_id: agent.into(), name: agent.into(), at: 0 });
    }
    must(&mut s, user(), Command::CreateRequest {
        request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
    });
    must(&mut s, orchestrator(), Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
    s
}

/// An SDK task whose observed turn started: admitted, resource available, the planned input taken.
pub(super) fn sdk_turn(s: &mut DomainState, id: &str, node: &str) -> String {
    sdk_turn_named(s, id, node, "turn-1")
}

fn sdk_turn_named(s: &mut DomainState, id: &str, node: &str, turn: &str) -> String {
    sdk_turn_on(s, id, node, turn, None)
}

/// An SDK task whose observed turn started on `account`.
fn sdk_turn_on(s: &mut DomainState, id: &str, node: &str, turn: &str, account: Option<&str>) -> String {
    sdk_turn_on_provider(s,alondra_core::accounts::Provider::Claude,id,node,turn,account)
}

fn sdk_turn_on_provider(s: &mut DomainState, provider: alondra_core::accounts::Provider, id: &str, node: &str, turn: &str, account: Option<&str>) -> String {
    task(s, id, None);
    must(s, Actor::System, Command::AdmitExecution { provider,
        task_id: id.into(), agent_id: node.into(), conversation: format!("conversation-{id}"), instance: format!("instance-{id}"), host: "sdk-host".into(), model: None,
        account: account.map(|account| alondra_core::accounts::AccountId::parse(account).unwrap()), leaves_limit: None,
    });
    let LifecycleRecord::Planned { input } = s.lifecycle_journal[&TaskId::from(id)][0].clone() else { panic!("planned first") };
    must(s, Actor::System, Command::RecordRuntimeFacts { task_id: id.into(), generation: 1, facts: vec![
        RuntimeObservation::ResourceAvailable,
        RuntimeObservation::TurnStarted { turn: turn.into(), input },
    ] });
    format!("{TURN_ENDED_PREFIX}{id}-1-{turn}")
}
