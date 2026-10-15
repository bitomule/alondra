//! A task whose PR merged around the end of its node's session: whatever order the node's turns,
//! its session ending, the listing of sessions, its work after the merge and its cleanup come in,
//! closing a node that is already closed is never refused, and the task never stays in progress
//! with its node closed nor says its session closed before it delivered.
use super::*;
use crate::views::{Client, Lang, LiveSnapshot, RenderContext, Transport};
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

/// A node at work on a task with its PR registered, delivered and accepted, the merge still to come.
fn accepted(work_after_merge: bool) -> DomainState {
    let mut s = DomainState::new("epoch");
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: AGENT.into(), name: "Node".into(), at: 0 });
    must(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask {
        task_id: TASK.into(), request_id: "r".into(), title: "Result".into(), agent_id: Some(AGENT.into()), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    must(&mut s, node(), Command::StartTask { task_id: TASK.into() });
    must(&mut s, Actor::System, Command::MoveNode { agent_id: AGENT.into(), event: NodeEvent::TurnStarted, at: 1 });
    must(&mut s, node(), Command::RegisterPr {
        task_id: TASK.into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(), number: 1,
        head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    if work_after_merge {
        must(&mut s, node(), Command::DeclarePostMergeWork { task_id: TASK.into(), reason: Some("publish the release".into()) });
    }
    must(&mut s, node(), Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], true),
    });
    must(&mut s, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets: targets() });
    s
}

fn merge(s: &mut DomainState) {
    must(s, Actor::System, Command::ObservePrState {
        pr_id: "pr".into(), head: "head1".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()),
    });
}

#[derive(Clone, Debug)]
enum Step {
    Node(NodeEvent),
    WorkAfterMerge(bool),
    CleanedUp,
    /// What Alondra's closer does once the node's session is over: it records the node's end.
    Closer,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        Just(Step::Node(NodeEvent::TurnStarted)),
        Just(Step::Node(NodeEvent::TurnEnded)),
        Just(Step::Node(NodeEvent::SessionLost)),
        Just(Step::Node(NodeEvent::SessionClosed)),
        Just(Step::Node(NodeEvent::Listed { listed: Listed::Absent })),
        Just(Step::Node(NodeEvent::Listed { listed: Listed::Done })),
        Just(Step::Node(NodeEvent::Listed { listed: Listed::Idle })),
        Just(Step::Node(NodeEvent::Listed { listed: Listed::Working })),
        any::<bool>().prop_map(Step::WorkAfterMerge),
        Just(Step::CleanedUp),
        Just(Step::Closer),
    ]
}

fn closes(event: &NodeEvent) -> bool {
    matches!(event, NodeEvent::SessionLost | NodeEvent::SessionClosed | NodeEvent::Listed { listed: Listed::Absent | Listed::Done })
}

fn apply(s: &mut DomainState, step: &Step, at: Millis) {
    match step {
        Step::Node(event) => {
            let refused = exec(s, Actor::System, Command::MoveNode { agent_id: AGENT.into(), event: event.clone(), at }).err();
            if closes(event) {
                assert_eq!(refused.map(|error| error.code), None, "{event:?} on a node in {:?} is refused", s.agents[&AGENT.into()].phase);
            }
        }
        Step::WorkAfterMerge(declared) => {
            let reason = declared.then(|| "publish the release".to_owned());
            let _ = exec(s, node(), Command::DeclarePostMergeWork { task_id: TASK.into(), reason });
        }
        Step::CleanedUp => {
            let _ = exec(s, node(), Command::ConfirmCleanup { task_id: TASK.into() });
        }
        Step::Closer => {
            let task = &s.tasks[&TASK.into()];
            if s.in_cleanup(task) && s.wraps_up(task) && s.node_session_closed(task) {
                must(s, Actor::System, Command::RecordNodeEnd { task_id: TASK.into(), end: NodeEnd::Gone, agent_id: Some(AGENT.into()) });
            }
        }
    }
}

fn render(s: &DomainState, key: &str) -> String {
    let projection = s.projection();
    let view = crate::views::build(key, RenderContext {
        domain: s, projection: &projection, live: &LiveSnapshot::default(), client: Client::Mac, lang: Lang::En,
        transport: Transport::Loopback, cursor: "", rendered_at: "",
    }).unwrap();
    serde_json::to_string(&view).unwrap()
}

/// David's rule, 15 October: a task whose delivery merged is never in progress with its node
/// closed, and never says its session closed before it delivered.
fn merged_task_tells_the_truth(s: &DomainState) {
    let task = &s.tasks[&TASK.into()];
    let phase = &s.agents[&AGENT.into()].phase;
    match task.state {
        TaskState::Running => assert!(task.wrap_up.post_merge_work.is_some() && *phase != NodePhase::Closed,
            "the merged task is in progress with its node {phase:?} and work after the merge {:?}", task.wrap_up.post_merge_work),
        TaskState::Closing | TaskState::Done => {}
        state => panic!("the merged task is {state:?}"),
    }
    assert_ne!(s.task_activity(task), Some(TaskActivity::NodeClosed), "the merged task says its node is closed, node {phase:?}");
    for key in ["tasks", "task:t"] {
        assert!(!render(s, key).contains("before it delivered"), "{key} says the merged task's session closed before it delivered");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn a_merged_task_never_waits_on_a_closed_node_nor_says_it_did_not_deliver(
        work_after_merge in any::<bool>(),
        before in prop::collection::vec(step(), 0..6),
        after in prop::collection::vec(step(), 0..16),
    ) {
        let mut s = accepted(work_after_merge);
        let mut at = 10;
        for step in &before {
            at += 1_000;
            apply(&mut s, step, at);
        }
        merge(&mut s);
        merged_task_tells_the_truth(&s);
        for step in &after {
            at += 1_000;
            apply(&mut s, step, at);
            merged_task_tells_the_truth(&s);
        }
        apply(&mut s, &Step::Node(NodeEvent::SessionLost), at + 1_000);
        apply(&mut s, &Step::Closer, at + 2_000);
        prop_assert_eq!(s.tasks[&TASK.into()].state, TaskState::Done, "once the node's session is over the closer finishes the task");
        merged_task_tells_the_truth(&s);
    }
}

#[test]
fn the_session_of_a_node_with_work_after_the_merge_ends_and_its_task_closes() {
    let mut s = accepted(true);
    merge(&mut s);
    assert_eq!(s.tasks[&TASK.into()].state, TaskState::Running);
    assert_eq!(s.task_activity(&s.tasks[&TASK.into()]), Some(TaskActivity::AfterMerge { reason: "publish the release".into() }));
    assert!(render(&s, "tasks").contains("Finishing after the merge"));
    apply(&mut s, &Step::Node(NodeEvent::SessionLost), 5_000);
    assert_eq!(s.tasks[&TASK.into()].state, TaskState::Closing, "work after the merge cannot run in a session that is over");
    apply(&mut s, &Step::Node(NodeEvent::SessionClosed), 6_000);
    apply(&mut s, &Step::Closer, 7_000);
    assert_eq!(s.tasks[&TASK.into()].state, TaskState::Done);
}
