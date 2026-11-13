//! Work a node runs after its PR merged can wait for another Task's work to land. The wait names
//! that Task or its pull request, so it is a dependency Alondra resolves: the task waits on that
//! Task, and its node is woken once, with what landed, by the command that landed it.
//! The release of beta 59 waited on the Mac package fix with only a reason in words: the fix
//! merged and nothing woke its node.
use super::*;
use std::collections::BTreeSet;

const RELEASE: &str = "t";
const FIX: &str = "fix";

fn exec(s: &mut DomainState, actor: Actor, command: Command) -> Result<CommandOutcome, DomainError> {
    s.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision, command,
    })
}

fn must(s: &mut DomainState, actor: Actor, command: Command) -> CommandOutcome {
    let shown = format!("{command:?}");
    exec(s, actor, command).unwrap_or_else(|error| panic!("{shown} was refused: {error}"))
}

fn refused(s: &mut DomainState, actor: Actor, command: Command) -> DomainError {
    let before = s.clone();
    let error = exec(s, actor, command).unwrap_err();
    assert_eq!(*s, before, "a refused command changed the state");
    error
}

fn node(agent: &str) -> Actor {
    Actor::Agent { agent_id: agent.into() }
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}

fn targets(pr: &str) -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::PullRequest { pr_id: pr.into(), head: "head1".into() }]
}

/// A started task of its own node with PR `number` registered.
fn task_with_pr(s: &mut DomainState, task: &str, agent: &str, title: &str, number: u64) {
    must(s, Actor::System, Command::RegisterAgent { agent_id: agent.into(), name: agent.into(), at: 0 });
    must(s, orchestrator(), Command::CreateTask { plan_items: None, continues: None,
        task_id: task.into(), request_id: "r".into(), title: title.into(), agent_id: Some(agent.into()), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    must(s, node(agent), Command::StartTask { task_id: task.into() });
    must(s, node(agent), Command::RegisterPr {
        task_id: task.into(), pr_id: format!("pr-{task}").into(), host: "github.com".into(), repository: "owner/repo".into(), number,
        head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
}

/// The task's PR presented as its final result, accepted, and merged on GitHub.
fn deliver_and_merge(s: &mut DomainState, task: &str, agent: &str, body: &str) -> CommandOutcome {
    let pr = format!("pr-{task}");
    let presentation = format!("final-{task}");
    must(s, node(agent), Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: task.into(), interaction_id: presentation.clone().into(), body: body.into(),
        targets: targets(&pr), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the result", &[], true),
    });
    must(s, user(), Command::AcceptDelivery { presentation_id: presentation.into(), targets: targets(&pr) });
    must(s, Actor::System, Command::ObservePrState {
        pr_id: pr.into(), head: "head1".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()),
    })
}

/// The release task, whose PR merged with work declared after the merge, and the fix it will
/// wait for, still open with its PR #2.
fn release_and_fix() -> DomainState {
    let mut s = DomainState::new("epoch");
    must(&mut s, user(), Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
    task_with_pr(&mut s, RELEASE, "a", "Release beta 59", 1);
    must(&mut s, user(), Command::TitleRequest { request_id: "r".into(), title: "Beta 59".into() });
    must(&mut s, node("a"), declare(Some("publish beta 59"), vec![]));
    deliver_and_merge(&mut s, RELEASE, "a", "Version bumped");
    task_with_pr(&mut s, FIX, "b", "Fix the Mac package", 2);
    s
}

fn declare(reason: Option<&str>, waits_for: Vec<WaitTarget>) -> Command {
    Command::DeclarePostMergeWork { task_id: RELEASE.into(), reason: reason.map(str::to_owned), waits_for }
}

fn fix_pr() -> WaitTarget {
    WaitTarget::PullRequest { repository: "owner/repo".into(), number: 2 }
}

fn told(outcome: &CommandOutcome, agent: &str) -> Vec<String> {
    outcome.effects.iter().filter_map(|effect| match effect {
        DomainEffect::NotifyAgent { agent_id, message, .. } if agent_id.0 == agent => Some(message.clone()),
        _ => None,
    }).collect()
}

fn state(s: &DomainState, task: &str) -> TaskState {
    s.tasks[&task.into()].state
}

#[test]
fn a_wait_after_the_merge_holds_the_task_on_what_it_names_and_wakes_its_node_once_with_what_landed() {
    let mut s = release_and_fix();
    assert_eq!(state(&s, RELEASE), TaskState::Running);
    must(&mut s, node("a"), declare(Some("publish beta 59"), vec![fix_pr()]));
    let release = &s.tasks[&RELEASE.into()];
    assert_eq!(release.state, TaskState::WaitingDependency);
    assert_eq!(release.dependencies, BTreeSet::from([FIX.into()]));
    assert_eq!(s.holder(release, &StartFacts::default()), Ok(Holder::Task(FIX.into())));
    assert_eq!(s.waited_tasks(&RELEASE.into()), vec!["Fix the Mac package".to_owned()]);

    let landed = deliver_and_merge(&mut s, FIX, "b", "The Mac package no longer ships ._ entries");
    assert_eq!(state(&s, FIX), TaskState::Closing, "the fix's node has not cleaned up yet");
    assert_eq!(state(&s, RELEASE), TaskState::Running);
    let expected = post_merge_wait_over_notice(&RELEASE.into(), "publish beta 59", &[Landed {
        title: "Fix the Mac package".into(), cancelled: false,
        result: Some("The Mac package no longer ships ._ entries".into()),
        merged_pr: Some("https://github.com/owner/repo/pull/2".into()),
    }]);
    assert_eq!(told(&landed, "a"), vec![expected]);
    assert_eq!(s.tasks[&RELEASE.into()].wrap_up.post_merge_work.as_ref().unwrap().declared_at, s.revision,
        "the watch on the work after the merge starts over when the node is woken");

    let later = must(&mut s, Actor::System, Command::RecordNodeEnd { task_id: FIX.into(), end: NodeEnd::Closed, agent_id: None });
    assert_eq!(state(&s, FIX), TaskState::Done);
    assert!(told(&later, "a").is_empty(), "the node is woken once");
}

#[test]
fn a_dependency_the_orchestrator_adds_to_work_after_the_merge_also_holds_it_and_wakes_it() {
    let mut s = release_and_fix();
    must(&mut s, orchestrator(), Command::SetDependencies { task_id: RELEASE.into(), dependencies: BTreeSet::from([FIX.into()]), stack: None, reason: None });
    assert_eq!(state(&s, RELEASE), TaskState::WaitingDependency);
    let landed = deliver_and_merge(&mut s, FIX, "b", "Fixed");
    assert_eq!(state(&s, RELEASE), TaskState::Running);
    assert_eq!(told(&landed, "a").len(), 1);
}

#[test]
fn a_cancelled_task_ends_the_wait_and_the_node_is_told_nothing_landed() {
    let mut s = release_and_fix();
    must(&mut s, node("a"), declare(Some("publish beta 59"), vec![WaitTarget::Task { task_id: FIX.into() }]));
    let cancelled = must(&mut s, user(), Command::CancelTask { task_id: FIX.into(), reason: None });
    assert_eq!(state(&s, RELEASE), TaskState::Running);
    let expected = post_merge_wait_over_notice(&RELEASE.into(), "publish beta 59", &[Landed {
        title: "Fix the Mac package".into(), cancelled: true, result: None, merged_pr: None,
    }]);
    assert_eq!(told(&cancelled, "a"), vec![expected]);
}

#[test]
fn a_task_waiting_to_go_on_goes_on_when_the_pr_it_waits_for_merges() {
    let mut s = release_and_fix();
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: "c".into(), name: "c".into(), at: 0 });
    must(&mut s, orchestrator(), Command::CreateTask { plan_items: None, continues: None,
        task_id: "next".into(), request_id: "r".into(), title: "Next".into(), agent_id: Some("c".into()), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    must(&mut s, node("c"), Command::StartTask { task_id: "next".into() });
    must(&mut s, orchestrator(), Command::SetDependencies { task_id: "next".into(), dependencies: BTreeSet::from([FIX.into()]), stack: None, reason: None });
    assert_eq!(state(&s, "next"), TaskState::WaitingDependency);
    let landed = deliver_and_merge(&mut s, FIX, "b", "Fixed");
    assert_eq!(state(&s, "next"), TaskState::Running, "its work is on main before the fix's node cleans up");
    let expected = dependencies_done_notice(&[Landed {
        title: "Fix the Mac package".into(), cancelled: false, result: Some("Fixed".into()),
        merged_pr: Some("https://github.com/owner/repo/pull/2".into()),
    }], BranchUpdate::Rebase);
    assert_eq!(told(&landed, "c"), vec![expected]);
}

#[test]
fn a_wait_names_work_alondra_can_see_land() {
    let mut s = release_and_fix();
    let code = |e: DomainError| e.code;
    assert_eq!(code(refused(&mut s, node("a"), declare(None, vec![fix_pr()]))), "invalid_input", "a wait says what comes after it");
    assert_eq!(code(refused(&mut s, node("a"), declare(Some("publish"), vec![
        WaitTarget::PullRequest { repository: "owner/repo".into(), number: 99 }]))), "invalid_input", "a PR no task registered");
    assert_eq!(code(refused(&mut s, node("a"), declare(Some("publish"), vec![
        WaitTarget::Task { task_id: "nope".into() }]))), "invalid_input");
    assert_eq!(code(refused(&mut s, node("a"), declare(Some("publish"), vec![
        WaitTarget::Task { task_id: RELEASE.into() }]))), "invalid_input", "not on itself");
    let error = refused(&mut s, node("b"), Command::DeclarePostMergeWork { task_id: FIX.into(), reason: Some("deploy".into()),
        waits_for: vec![WaitTarget::Task { task_id: RELEASE.into() }] });
    assert_eq!(error.code, "invalid_input", "before its own PR merged, a node declares wait_for_tasks: {error}");
    assert!(error.message.contains("wait_for_tasks"), "{error}");

    deliver_and_merge(&mut s, FIX, "b", "Fixed");
    let error = refused(&mut s, node("a"), declare(Some("publish"), vec![fix_pr()]));
    assert!(error.message.contains("already landed"), "{error}");
    let mut other = release_and_fix();
    must(&mut other, user(), Command::CancelTask { task_id: FIX.into(), reason: None });
    assert_eq!(code(refused(&mut other, node("a"), declare(Some("publish"), vec![fix_pr()]))), "invalid_input", "a cancelled task never lands");
}

#[test]
fn a_node_names_a_task_a_pr_or_its_url() {
    assert_eq!(WaitTarget::parse("t-apple-pkg"), WaitTarget::Task { task_id: "t-apple-pkg".into() });
    assert_eq!(WaitTarget::parse(" Bitomule/Alondra#1142 "), WaitTarget::PullRequest { repository: "bitomule/alondra".into(), number: 1142 });
    assert_eq!(WaitTarget::parse("https://github.com/bitomule/alondra/pull/1142/"),
        WaitTarget::PullRequest { repository: "bitomule/alondra".into(), number: 1142 });
}

/// A started task of node `c` waiting on the fix, and one without a node waiting on it too.
fn waiting_on_the_fix() -> DomainState {
    let mut s = release_and_fix();
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: "c".into(), name: "c".into(), at: 0 });
    must(&mut s, orchestrator(), Command::CreateTask { plan_items: None, continues: None,
        task_id: "next".into(), request_id: "r".into(), title: "Next".into(), agent_id: Some("c".into()), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    must(&mut s, node("c"), Command::StartTask { task_id: "next".into() });
    must(&mut s, orchestrator(), Command::SetDependencies { task_id: "next".into(), dependencies: BTreeSet::from([FIX.into()]), stack: None, reason: None });
    must(&mut s, orchestrator(), Command::CreateTask { plan_items: None, continues: None,
        task_id: "unowned".into(), request_id: "r".into(), title: "Unowned".into(), agent_id: None, review_prs: false,
        dependencies: BTreeSet::from([FIX.into()]), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    assert_eq!((state(&s, "next"), state(&s, "unowned")), (TaskState::WaitingDependency, TaskState::WaitingDependency));
    s
}

/// What a daemon opens after a release that counted fewer dependencies as landed: the rows keep
/// the waits the older one saved.
fn opened_after_an_upgrade(mut s: DomainState) -> DomainState {
    for id in ["next", "unowned"] {
        s.tasks.get_mut(&id.into()).unwrap().state = TaskState::WaitingDependency;
    }
    s.derive_loaded_states();
    s
}

/// «Pebble: progreso diario» waited on «Publicar el daemon 0.3.6», whose PR had merged with no
/// work declared after it. The 0.3.5 daemon saved the wait; the 0.3.6 one counted the release as
/// landed while it opened its domain, moved the task on in memory only and told its node nothing.
#[test]
fn a_wait_that_ended_while_the_daemon_was_closed_goes_on_with_its_node_told_once() {
    let mut s = waiting_on_the_fix();
    deliver_and_merge(&mut s, FIX, "b", "Fixed");
    let mut s = opened_after_an_upgrade(s);
    assert_eq!((state(&s, "next"), state(&s, "unowned")), (TaskState::WaitingDependency, TaskState::WaitingDependency),
        "opening the domain changes no saved wait");
    assert_eq!(s.ended_waits(), vec![TaskId::from("next"), TaskId::from("unowned")]);

    let reviewed = must(&mut s, Actor::System, Command::ReviewWaits);
    assert!(reviewed.changed);
    assert_eq!(state(&s, "next"), TaskState::Running);
    assert_eq!(state(&s, "unowned"), TaskState::NotStarted, "a task without a node is left for the queue to start");
    let expected = dependencies_done_notice(&[Landed {
        title: "Fix the Mac package".into(), cancelled: false, result: Some("Fixed".into()),
        merged_pr: Some("https://github.com/owner/repo/pull/2".into()),
    }], BranchUpdate::Rebase);
    assert_eq!(told(&reviewed, "c"), vec![expected]);
    assert!(s.ended_waits().is_empty());

    let again = must(&mut s, Actor::System, Command::ReviewWaits);
    assert!(!again.changed && told(&again, "c").is_empty(), "the node is told once");
}

#[test]
fn a_dependency_that_ends_with_the_first_command_after_opening_wakes_its_node() {
    let mut s = opened_after_an_upgrade(waiting_on_the_fix());
    assert!(s.ended_waits().is_empty());
    let landed = deliver_and_merge(&mut s, FIX, "b", "Fixed");
    assert_eq!((state(&s, "next"), state(&s, "unowned")), (TaskState::Running, TaskState::NotStarted));
    assert_eq!(told(&landed, "c").len(), 1);
}

#[test]
fn only_the_system_reviews_the_waits() {
    let mut s = opened_after_an_upgrade(waiting_on_the_fix());
    refused(&mut s, user(), Command::ReviewWaits);
    refused(&mut s, orchestrator(), Command::ReviewWaits);
}
