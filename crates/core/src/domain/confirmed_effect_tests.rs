//! A command the domain accepts changed the state, and says what it changed. One that would change
//! nothing is refused with `no_effect` and its reason, so nobody reads a confirmation that nothing
//! backs: the orchestrator closed a task three times and was told "Recorded" each time while the
//! task stayed running.
use super::lifecycle_tests::{exec, must, node, sdk_turn, task, user, world};
use super::*;
use proptest::prelude::*;

fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}

/// Task `t` running on node `a`, task `u` not started.
fn two_tasks() -> DomainState {
    let mut s = world();
    sdk_turn(&mut s, "t", "a");
    task(&mut s, "u", None);
    for id in ["note-i1", "note-i2"] {
        must(&mut s, Actor::System, inform(id));
    }
    s
}

fn ask(id: &str) -> Command {
    let options = vec![
        QuestionOption { id: "keep".into(), label: "Keep the current name".into(), description: None },
        QuestionOption { id: "rename".into(), label: "Rename it".into(), description: None },
    ];
    Command::AskQuestion {
        attachments: vec![], interaction_id: id.into(), scope: Scope::Task { task_id: "t".into() }, recipient: Recipient::User,
        body: "Which name?".into(), blocking: false, card: CardSpec::question("Pick the name", &options), options,
    }
}

fn inform(id: &str) -> Command {
    Command::PublishInformation {
        attachments: vec![], interaction_id: id.into(), scope: Scope::Task { task_id: "u".into() }, recipient: Recipient::User,
        body: format!("Halfway there: {id}"), card: CardSpec::derived(CardKind::Know, "Read the progress"),
    }
}

#[test]
fn a_command_that_changes_nothing_is_refused_with_its_reason_and_records_nothing() {
    let mut s = two_tasks();
    must(&mut s, Actor::System, inform("note"));
    must(&mut s, user(), Command::MarkRead { interaction_id: "note".into() });
    let before = s.clone();
    let refused = exec(&mut s, user(), Command::MarkRead { interaction_id: "note".into() }).unwrap_err();
    assert_eq!(refused.code, "no_effect", "{refused}");
    assert!(refused.message.starts_with("mark_read changed nothing"), "{}", refused.message);
    assert_eq!(s, before, "a refused command leaves the state and its revision as they were");
}

#[test]
fn an_accepted_command_says_what_it_changed() {
    let mut s = two_tasks();
    let cancelled = must(&mut s, orchestrator(), Command::CancelTask { task_id: "u".into() });
    assert!(cancelled.changed);
    assert!(cancelled.effect.iter().any(|line| line.starts_with("Task u went from not started to ")), "{:?}", cancelled.effect);
    let asked = must(&mut s, node("a"), ask("name"));
    assert!(asked.effect.contains(&"Card name created.".to_owned()), "{:?}", asked.effect);
    let retitled = must(&mut s, orchestrator(), Command::RetitleTask { task_id: "t".into(), title: "A clearer title".into() });
    assert_eq!(retitled.effect, vec!["Task t updated.".to_owned()]);
}

/// The commands users, the orchestrator and nodes send, over a small set of values so that the
/// same command comes again in a state that already has what it asks for.
fn command() -> impl Strategy<Value = (Actor, Command)> {
    let task = prop_oneof![Just("t"), Just("u")];
    let card = prop_oneof![Just("i1"), Just("i2")];
    let title = prop_oneof![Just("First"), Just("Second")];
    prop_oneof![
        task.clone().prop_map(|t| (orchestrator(), Command::CancelTask { task_id: t.into() })),
        task.clone().prop_map(|t| (user(), Command::CancelTask { task_id: t.into() })),
        task.clone().prop_map(|t| (user(), Command::ReopenTask { task_id: t.into() })),
        (task.clone(), any::<bool>()).prop_map(|(t, archived)| (user(), Command::ArchiveTask { task_id: t.into(), archived })),
        (task.clone(), prop_oneof![Just(TaskPriority::High), Just(TaskPriority::Normal)])
            .prop_map(|(t, priority)| (orchestrator(), Command::SetPriority { task_id: t.into(), priority })),
        (task.clone(), title.clone()).prop_map(|(t, title)| (orchestrator(), Command::RetitleTask { task_id: t.into(), title: title.into() })),
        title.prop_map(|title| (orchestrator(), Command::TitleRequest { request_id: "r".into(), title: title.into() })),
        prop_oneof![Just(MergeMethod::Squash), Just(MergeMethod::Merge)].prop_map(|method| (user(), Command::SetMergeMethod { method })),
        prop_oneof![Just(BranchUpdate::Rebase), Just(BranchUpdate::Merge)].prop_map(|update| (user(), Command::SetBranchUpdate { update })),
        card.clone().prop_map(|id| (node("a"), ask(id))),
        card.clone().prop_map(|id| (node("a"), Command::Withdraw { interaction_id: id.into() })),
        card.prop_map(|id| (user(), Command::MarkRead { interaction_id: format!("note-{id}").into() })),
        Just((node("a"), Command::WaitForTasks { task_id: "t".into(), tasks: ["u".into()].into() })),
        Just((orchestrator(), Command::SetDependencies { task_id: "u".into(), dependencies: ["t".into()].into(), stack: None })),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn ok_means_the_state_changed(steps in proptest::collection::vec(command(), 1..24)) {
        let mut s = two_tasks();
        for (actor, command) in steps {
            let before = s.clone();
            let name = command_name(&command);
            match exec(&mut s, actor, command) {
                Ok(outcome) => {
                    prop_assert!(s != before, "{} answered Ok and changed nothing", name);
                    prop_assert!(outcome.changed && !outcome.effect.is_empty(), "{} answered Ok without its effect", name);
                    prop_assert_eq!(outcome.revision, before.revision + 1);
                }
                Err(_) => prop_assert!(s == before, "a refused {} changed the state", name),
            }
        }
    }
}
