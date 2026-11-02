//! Work a node declared it runs after its PR merged is owed by the task's lifecycle until a fact
//! ends it: the node saying so, its cleanup, a turn it began after declaring ending without
//! declaring it again, the user's choice on a card, or its session ending. Whatever the order of those steps and of Alondra trying to close the node,
//! nothing else ends it, and what Alondra shows always matches what the lifecycle owes.
use super::*;
use proptest::prelude::*;
use std::collections::BTreeSet;

const AGENT: &str = "a";
const TASK: &str = "t";

fn exec(s: &mut DomainState, actor: Actor, command: Command) -> Result<CommandOutcome, DomainError> {
    s.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision, command,
    })
}

fn must(s: &mut DomainState, actor: Actor, command: Command) {
    let shown = format!("{command:?}");
    if let Err(error) = exec(s, actor, command) {
        panic!("{shown} was refused: {error}");
    }
}

fn node() -> Actor {
    Actor::Agent { agent_id: AGENT.into() }
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn targets() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }]
}

/// A node whose task declared work after the merge, delivered its PR and had it accepted and merged.
fn merged() -> DomainState {
    let mut s = DomainState::new("epoch");
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: AGENT.into(), name: "Node".into(), at: 0 });
    must(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None,
        task_id: TASK.into(), request_id: "r".into(), title: "Release".into(), agent_id: Some(AGENT.into()), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    must(&mut s, node(), Command::StartTask { task_id: TASK.into() });
    must(&mut s, Actor::System, Command::MoveNode { agent_id: AGENT.into(), event: NodeEvent::TurnStarted, at: 1 });
    must(&mut s, node(), Command::RegisterPr {
        task_id: TASK.into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(), number: 1,
        head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    must(&mut s, node(), Command::DeclarePostMergeWork { task_id: TASK.into(), reason: Some("publish the release".into()), waits_for: vec![] });
    must(&mut s, node(), Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], true),
    });
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets: targets() });
    must(&mut s, Actor::System, Command::ObservePrState {
        pr_id: "pr".into(), head: "head1".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()),
    });
    s
}

#[derive(Clone, Debug)]
enum Step {
    Declare(bool),
    CleanedUp,
    /// The card that asks the user whether to close the node, answered with this option.
    Card(&'static str),
    /// Alondra closing the node itself, as a clock that ran out would.
    ClosedByAlondra,
    SessionOver,
    Node(NodeEvent),
}

impl Step {
    fn is_a_fact(&self) -> bool {
        matches!(self, Step::Declare(false) | Step::CleanedUp | Step::Card(POST_MERGE_CLOSE) | Step::SessionOver | Step::Node(NodeEvent::TurnEnded))
    }
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        any::<bool>().prop_map(Step::Declare),
        Just(Step::CleanedUp),
        Just(Step::Card(POST_MERGE_CLOSE)),
        Just(Step::Card(POST_MERGE_LEAVE)),
        Just(Step::ClosedByAlondra),
        Just(Step::SessionOver),
        Just(Step::Node(NodeEvent::TurnStarted)),
        Just(Step::Node(NodeEvent::TurnEnded)),
    ]
}

fn apply(s: &mut DomainState, step: &Step, n: usize) {
    let _ = match step {
        Step::Declare(declared) => exec(s, node(), Command::DeclarePostMergeWork {
            task_id: TASK.into(), reason: declared.then(|| "publish the release".to_owned()), waits_for: vec![],
        }),
        Step::CleanedUp => exec(s, node(), Command::ConfirmCleanup { task_id: TASK.into(), discard: Vec::new() }),
        Step::Card(option) => answer_card(s, option, n),
        Step::ClosedByAlondra => exec(s, Actor::System, Command::RecordNodeEnd { task_id: TASK.into(), end: NodeEnd::Closed, agent_id: Some(AGENT.into()) }),
        Step::SessionOver => exec(s, Actor::System, Command::RecordNodeEnd { task_id: TASK.into(), end: NodeEnd::Gone, agent_id: Some(AGENT.into()) }),
        Step::Node(event) => exec(s, Actor::System, Command::MoveNode { agent_id: AGENT.into(), event: event.clone(), at: 10 + n as Millis }),
    };
}

/// Raises the card that asks the user whether to close the node, as the closer does, and answers it.
fn answer_card(s: &mut DomainState, option: &str, n: usize) -> Result<CommandOutcome, DomainError> {
    let card: InteractionId = format!("{POST_MERGE_PROBLEM_PREFIX}{TASK}-{n}").into();
    exec(s, Actor::System, Command::RaiseProblem {
        attachments: vec![], interaction_id: card.clone(), scope: Scope::Task { task_id: TASK.into() },
        body: "Still running?".into(),
        options: vec![
            QuestionOption { id: POST_MERGE_CLOSE.into(), label: "Close the node".into(), description: None },
            QuestionOption { id: POST_MERGE_LEAVE.into(), label: "Leave it".into(), description: None },
        ],
        card: CardSpec { system: Some(SystemCard::PostMergeProblem), ..CardSpec::new(CardKind::Problem, "Decide whether to close the session")
            .why("Its node says it still works after the merge and does not answer.")
            .content(ContentKind::Diagnosis, "Still running?") },
    })?;
    exec(s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card, option_id: option.into(), inputs: Default::default() })
}

fn lifecycle_owes(s: &DomainState) -> bool {
    replay(&TASK.into(), &s.lifecycle_journal[&TASK.into()]).unwrap().post_merge().is_some()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn only_a_fact_ends_the_work_after_the_merge(steps in prop::collection::vec(step(), 1..14)) {
        let mut s = merged();
        prop_assert!(lifecycle_owes(&s), "the merged task owes its declared work after the merge");
        for (n, step) in steps.iter().enumerate() {
            let owed = lifecycle_owes(&s);
            apply(&mut s, step, n);
            let still = lifecycle_owes(&s);
            prop_assert!(!owed || still || step.is_a_fact(), "post-merge obligation discharged without a fact by {step:?} after {steps:?}");
            let task = &s.tasks[&TASK.into()];
            prop_assert_eq!(task.wrap_up.post_merge_work.is_some(), still, "what Alondra shows differs from what the lifecycle owes after {:?}", step);
            if still {
                prop_assert!(!matches!(task.state, TaskState::Done), "the task is done while it owes work after the merge");
            }
        }
    }
}

#[test]
fn the_users_choice_on_the_card_ends_the_work_and_leaving_it_does_not() {
    let mut s = merged();
    answer_card(&mut s, POST_MERGE_LEAVE, 0).unwrap();
    assert!(lifecycle_owes(&s));
    answer_card(&mut s, POST_MERGE_CLOSE, 1).unwrap();
    assert!(!lifecycle_owes(&s));
    assert_eq!(s.tasks[&TASK.into()].wrap_up.post_merge_work, None);
    must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: TASK.into(), end: NodeEnd::Closed, agent_id: Some(AGENT.into()) });
    assert_eq!(s.tasks[&TASK.into()].state, TaskState::Done);
}

#[test]
fn cancelling_a_task_owes_its_work_after_the_merge_until_its_node_ends() {
    let mut s = DomainState::new("epoch");
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: AGENT.into(), name: "Node".into(), at: 0 });
    must(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None,
        task_id: TASK.into(), request_id: "r".into(), title: "Release".into(), agent_id: Some(AGENT.into()), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    must(&mut s, node(), Command::StartTask { task_id: TASK.into() });
    must(&mut s, node(), Command::DeclarePostMergeWork { task_id: TASK.into(), reason: Some("publish the release".into()), waits_for: vec![] });
    must(&mut s, user(), Command::CancelTask { task_id: TASK.into() });
    assert!(lifecycle_owes(&s), "the cancelled task's ending owes the work after the merge");
    assert_ne!(s.tasks[&TASK.into()].state, TaskState::Cancelled);
    must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: TASK.into(), end: NodeEnd::Closed, agent_id: Some(AGENT.into()) });
    assert!(!lifecycle_owes(&s));
    assert_eq!(s.tasks[&TASK.into()].state, TaskState::Cancelled);
}

/// After the merge, the turn Alondra starts for the work ends with nothing running and without
/// declaring it again: the work is over. The turn that declared it, a turn that ends behind its own
/// work and one in which it declares again leave it owed, with no clock involved.
#[test]
fn a_turn_after_the_declaration_that_ends_without_declaring_again_ends_the_work() {
    let mut s = merged();
    let event = |s: &mut DomainState, event: NodeEvent, at: Millis| must(s, Actor::System, Command::MoveNode { agent_id: AGENT.into(), event, at });
    event(&mut s, NodeEvent::TurnEnded, 2);
    assert!(lifecycle_owes(&s), "the turn that declared it ended");

    event(&mut s, NodeEvent::TurnStarted, 3);
    let work = Background { work: crate::model::BackgroundKind::Shell, label: "release.sh".into(), since: 3, others: 0, gone: None };
    event(&mut s, NodeEvent::TurnEndedBehind { work }, 4);
    assert!(lifecycle_owes(&s), "its release still runs");

    event(&mut s, NodeEvent::TurnStarted, 5);
    must(&mut s, node(), Command::DeclarePostMergeWork { task_id: TASK.into(), reason: Some("wait for the tap".into()), waits_for: vec![] });
    event(&mut s, NodeEvent::TurnEnded, 6);
    assert!(lifecycle_owes(&s), "it declared the work again in that turn");

    event(&mut s, NodeEvent::TurnStarted, 7);
    event(&mut s, NodeEvent::TurnEnded, 8);
    assert!(!lifecycle_owes(&s), "a turn ended without declaring it again");
    assert_eq!(s.tasks[&TASK.into()].wrap_up.post_merge_work, None);
    assert!(matches!(s.lifecycle_journal[&TASK.into()].last(), Some(LifecycleRecord::PostMergeEnded { fact: PostMergeEnd::TurnEnded, .. })));
}

/// Before the merge, the node's turns end as they like: the work it declared for after the merge
/// is still owed when the merge comes.
#[test]
fn turns_before_the_merge_leave_the_declared_work_owed() {
    let mut s = DomainState::new("epoch");
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: AGENT.into(), name: "Node".into(), at: 0 });
    must(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None,
        task_id: TASK.into(), request_id: "r".into(), title: "Release".into(), agent_id: Some(AGENT.into()), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    must(&mut s, node(), Command::StartTask { task_id: TASK.into() });
    must(&mut s, node(), Command::DeclarePostMergeWork { task_id: TASK.into(), reason: Some("publish the release".into()), waits_for: vec![] });
    for at in 1..5 {
        must(&mut s, Actor::System, Command::MoveNode { agent_id: AGENT.into(), event: NodeEvent::TurnStarted, at: at * 2 });
        must(&mut s, Actor::System, Command::MoveNode { agent_id: AGENT.into(), event: NodeEvent::TurnEnded, at: at * 2 + 1 });
    }
    assert!(lifecycle_owes(&s));
}
