//! The user's rule, 19 October: a task owns every worktree it made or took, its node's and those of
//! the subagents the node launched, and no task that ended leaves one of them behind. Whatever
//! order worktrees appear, end, get discarded or stay dirty in, and whether the task is delivered
//! or cancelled, the task only reaches Done or Cancelled once each of its worktrees has ended.
use super::*;
use proptest::prelude::*;
use std::collections::BTreeSet;

const AGENT: &str = "a";
const TASK: &str = "t";
const PATHS: [&str; 3] = [
    "/repo/.claude/worktrees/t",
    "/repo/.claude/worktrees/agent-a9e1782746c156d58",
    "/repo/.claude/worktrees/agent-abd91aef2f770f04e",
];
const ORIGINS: [WorktreeOrigin; 3] = [WorktreeOrigin::Task, WorktreeOrigin::Subagent, WorktreeOrigin::Entered];

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
    vec![DeliveryTarget::Text { text: "Done".into() }]
}

fn working() -> DomainState {
    let mut s = DomainState::new("epoch");
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: AGENT.into(), name: "Node".into(), at: 0 });
    must(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
        task_id: TASK.into(), request_id: "r".into(), title: "Result".into(), agent_id: Some(AGENT.into()), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/repo")),
    });
    must(&mut s, node(), Command::StartTask { task_id: TASK.into() });
    must(&mut s, Actor::System, Command::RecordTaskWorktree {
        task_id: TASK.into(), path: PATHS[0].into(), origin: WorktreeOrigin::Task, branch: Some("worktree-t".into()),
    });
    s
}

#[derive(Clone, Debug)]
enum Step {
    /// The node, or a subagent of it, makes a worktree; Alondra records it.
    Made(usize),
    /// Claude Code removes one, as when a subagent ends with no changes.
    Removed(usize),
    /// The node commits and pushes what was left in one, or leaves new work in it.
    Dirty(usize, bool),
    Deliver,
    Accept,
    Cancel,
    /// The node's cleanup, giving up the work left in the worktrees it names.
    CleanedUp(Vec<usize>),
    /// Alondra's closer: it removes each worktree that is clean or given up, then records the end
    /// of the node, which is refused while one is left.
    Closer,
    /// A node end recorded without going through the closer's worktree pass.
    NodeEnd,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0..PATHS.len()).prop_map(Step::Made),
        (0..PATHS.len()).prop_map(Step::Removed),
        ((0..PATHS.len()), any::<bool>()).prop_map(|(i, dirty)| Step::Dirty(i, dirty)),
        Just(Step::Deliver),
        Just(Step::Accept),
        Just(Step::Cancel),
        prop::collection::vec(0..PATHS.len(), 0..3).prop_map(Step::CleanedUp),
        Just(Step::Closer),
        Just(Step::NodeEnd),
    ]
}

/// What git would say about each worktree on disk.
struct Disk {
    dirty: BTreeSet<usize>,
}

fn apply(s: &mut DomainState, disk: &mut Disk, step: &Step) {
    match step {
        Step::Made(i) => {
            let _ = exec(s, Actor::System, Command::RecordTaskWorktree {
                task_id: TASK.into(), path: PATHS[*i].into(), origin: ORIGINS[*i], branch: None,
            });
        }
        Step::Removed(i) => {
            if exec(s, Actor::System, Command::ReleaseTaskWorktree { task_id: TASK.into(), path: PATHS[*i].into(), end: WorktreeEnd::Removed }).is_ok() {
                disk.dirty.remove(i);
            }
        }
        Step::Dirty(i, true) => {
            disk.dirty.insert(*i);
        }
        Step::Dirty(i, false) => {
            disk.dirty.remove(i);
        }
        Step::Deliver => {
            let _ = exec(s, node(), Command::PresentResult {
                checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: format!("final-{}", s.revision).into(),
                body: "Done".into(), targets: targets(), is_final: true, nothing_to_review: false,
                card: CardSpec::delivery("Review the result", &[], true),
            });
        }
        Step::Accept => {
            if let Some(presentation) = s.tasks[&TASK.into()].final_presentation.clone() {
                let _ = exec(s, user(), Command::AcceptDelivery { presentation_id: presentation, targets: targets() });
            }
        }
        Step::Cancel => {
            let _ = exec(s, user(), Command::CancelTask { task_id: TASK.into(), reason: None });
        }
        Step::CleanedUp(given_up) => {
            let discard = given_up.iter().map(|i| PATHS[*i].to_owned()).collect();
            let _ = exec(s, node(), Command::ConfirmCleanup { task_id: TASK.into(), discard });
        }
        Step::Closer => {
            let task = &s.tasks[&TASK.into()];
            if !(task.ending() || s.in_cleanup(task) && s.wraps_up(task)) || task.wrap_up.node.is_some() {
                return;
            }
            let removable: Vec<String> = task.open_worktrees()
                .filter(|(path, w)| w.discard || !disk.dirty.contains(&PATHS.iter().position(|p| p == path).unwrap()))
                .map(|(path, _)| path.clone())
                .collect();
            for path in removable {
                must(s, Actor::System, Command::ReleaseTaskWorktree { task_id: TASK.into(), path: path.clone(), end: WorktreeEnd::Removed });
                disk.dirty.remove(&PATHS.iter().position(|p| *p == path).unwrap());
            }
            let task = &s.tasks[&TASK.into()];
            let left = task.open_worktrees().count();
            let ended = match task.agent_id {
                Some(_) => exec(s, Actor::System, Command::RecordNodeEnd { task_id: TASK.into(), end: NodeEnd::Closed, agent_id: Some(AGENT.into()) }),
                None => exec(s, Actor::System, Command::RecordTaskCleanup { task_id: TASK.into() }),
            };
            if left > 0 {
                assert_eq!(ended.err().map(|e| e.code), Some("worktrees_open".into()), "the task's end was recorded with {left} worktrees left");
            }
        }
        Step::NodeEnd => {
            let _ = exec(s, Actor::System, Command::RecordNodeEnd { task_id: TASK.into(), end: NodeEnd::Gone, agent_id: Some(AGENT.into()) });
        }
    }
}

fn no_ended_task_leaves_a_worktree(s: &DomainState) {
    let task = &s.tasks[&TASK.into()];
    if task.state.terminal() {
        let left: Vec<&String> = task.open_worktrees().map(|(path, _)| path).collect();
        assert!(left.is_empty(), "task {:?} left its worktrees {left:?}", task.state);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn no_task_that_ended_leaves_a_worktree_of_its_own(steps in prop::collection::vec(step(), 0..24)) {
        let mut s = working();
        let mut disk = Disk { dirty: BTreeSet::new() };
        for step in &steps {
            apply(&mut s, &mut disk, step);
            no_ended_task_leaves_a_worktree(&s);
        }
    }
}

#[test]
fn a_dirty_subagent_worktree_holds_the_node_end_until_it_is_given_up() {
    let mut s = working();
    let mut disk = Disk { dirty: BTreeSet::from([1]) };
    apply(&mut s, &mut disk, &Step::Made(1));
    apply(&mut s, &mut disk, &Step::Cancel);
    apply(&mut s, &mut disk, &Step::Closer);
    let task = &s.tasks[&TASK.into()];
    assert_eq!(task.open_worktrees().map(|(path, _)| path.as_str()).collect::<Vec<_>>(), [PATHS[1]], "only the dirty one is left");
    assert!(!task.state.terminal(), "the task ended with a dirty worktree left");
    apply(&mut s, &mut disk, &Step::CleanedUp(vec![1]));
    apply(&mut s, &mut disk, &Step::Closer);
    assert_eq!(s.tasks[&TASK.into()].state, TaskState::Cancelled);
}

#[test]
fn a_node_the_user_keeps_keeps_its_worktrees() {
    let mut s = working();
    apply(&mut s, &mut Disk { dirty: BTreeSet::new() }, &Step::Cancel);
    must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: TASK.into(), end: NodeEnd::KeptByUser, agent_id: Some(AGENT.into()) });
    assert_eq!(s.tasks[&TASK.into()].worktrees[PATHS[0]].end, Some(WorktreeEnd::KeptByUser));
}

#[test]
fn a_node_discards_only_worktrees_of_its_task_that_still_exist() {
    let mut s = working();
    apply(&mut s, &mut Disk { dirty: BTreeSet::new() }, &Step::Cancel);
    let error = exec(&mut s, node(), Command::ConfirmCleanup { task_id: TASK.into(), discard: vec!["/elsewhere".into()] }).unwrap_err();
    assert_eq!(error.code, "not_found");
}
