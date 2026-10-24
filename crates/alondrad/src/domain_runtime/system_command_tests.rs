use super::*;
use alondra_core::accounts::{AccountId, Policy};
use alondra_core::usage::{Share, UsageReading, WindowKind, WindowReading};

/// Another writer commits between the moment a system command reads the state and the moment it
/// reaches the writer lock.
fn another_command_commits_first(runtime: &DomainRuntime, name: &'static str) {
    runtime.before_next_commit(move |runtime| {
        let state = runtime.state().unwrap();
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("racer-{name}").into(),
            expected_revision: state.revision,
            command: Command::RegisterAgent { agent_id: format!("racer-{name}").into(), name: name.into(), at: 0 },
        };
        assert!(runtime.execute(&Actor::System, &envelope, None, || Ok(())).unwrap().changed);
    });
}

fn applied_after_the_race(runtime: &DomainRuntime, name: &'static str, command: impl FnOnce(&DomainRuntime)) {
    let before = runtime.state().unwrap().revision;
    another_command_commits_first(runtime, name);
    command(runtime);
    let state = runtime.state().unwrap();
    assert!(state.agents.contains_key(&AgentId(format!("racer-{name}"))), "{name}: the other command committed first");
    assert_eq!(state.revision, before + 2, "{name}: the system command committed after it, at once");
}

// Every command the daemon issues itself is judged on the state it commits against: one that
// reaches the writer lock after another command committed is applied, never refused as stale.
// Each path below only logs a failure when its command returns an error.
#[test]
fn a_system_command_that_loses_the_race_to_another_commit_is_still_applied() {
    crate::domain_store::retention::tests::with_an_ended_task(|runtime| {
        let mut compacted = 0;
        applied_after_the_race(runtime, "compaction", |runtime| compacted = runtime.retain(alondra_core::now_millis()).unwrap().compacted);
        assert_eq!(compacted, 1, "the cards of the ended task are compacted");

        let reading = UsageReading {
            windows: vec![WindowReading { kind: WindowKind::Weekly, used: Share::of(0.3), resets_at: None, exhausted: false }],
            plan: Some("max".into()),
            complete: false,
        };
        applied_after_the_race(runtime, "usage", |runtime| crate::usage_probe::record(runtime, &AccountId::default_account(), reading, 1_000));
        let usage = runtime.state().unwrap().account_usage.clone();
        assert!(usage.iter().any(|usage| usage.account.is_default() && usage.plan.as_deref() == Some("max")), "{usage:?}");

        applied_after_the_race(runtime, "accounts", |runtime| crate::accounts::record_distribution(runtime, Policy::Chain, true));
        let state = runtime.state().unwrap();
        assert_eq!((state.account_policy, state.account_moves), (Policy::Chain, true));

        applied_after_the_race(runtime, "orchestrator", |runtime| runtime.bind_orchestrator_session("session-1").unwrap());
        assert_eq!(runtime.state().unwrap().orchestrator_session.as_ref().map(|session| session.0.as_str()), Some("session-1"));

        applied_after_the_race(runtime, "problem", |runtime| {
            runtime.raise_problem(
                "problem-1", None, Scope::Orchestrator { request_id: None }, "Check the daemon".into(), "It raced".into(), "Another command committed first".into(), vec![],
            ).unwrap();
        });
        assert!(runtime.state().unwrap().interactions.contains_key(&InteractionId::from("problem-1")));
    });
}
