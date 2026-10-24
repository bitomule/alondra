use super::*;

fn history(states: &[(Millis, &str)]) -> Vec<(Millis, String)> {
    states.iter().map(|(at, state)| (*at, (*state).to_owned())).collect()
}

fn launch(agent: &str, model: Option<&str>, effort: Option<&str>) -> Decision {
    Decision { kind: DecisionKind::Launch, agent_id: Some(agent.into()), model: model.map(Into::into), effort: effort.map(Into::into), approach: None }
}

#[test]
fn a_task_done_the_first_time_is_clean_and_its_time_is_split_by_state() {
    let facts = TaskFacts {
        history: history(&[(0, "not_started"), (10, "running"), (110, "needs_you"), (140, "running"), (240, "for_your_review"), (300, "closing"), (320, "done")]),
        decisions: vec![launch("a", Some("opus"), Some("high"))],
        ..TaskFacts::default()
    };
    let metrics = fold_task(&facts).unwrap();
    assert_eq!(metrics.outcome, Outcome::Done);
    assert!(metrics.clean);
    assert_eq!((metrics.queued_ms, metrics.work_ms, metrics.waiting_user_ms, metrics.blocked_ms), (10, 200, 90, 0));
    assert_eq!((metrics.started_at, metrics.finished_at), (Some(10), 320));
    assert_eq!(metrics.interventions, 1, "one question to the user");
    assert_eq!((metrics.model.as_deref(), metrics.effort.as_deref()), (Some("opus"), Some("high")));
}

#[test]
fn rounds_of_changes_and_the_users_feedback_are_rework_and_interventions() {
    let facts = TaskFacts {
        history: history(&[(0, "running"), (5, "for_your_review"), (9, "running"), (12, "waiting_dependency"), (20, "done")]),
        change_rounds: vec![false, true],
        user_feedback: 2,
        decisions: vec![launch("a", Some("sonnet"), None)],
    };
    let metrics = fold_task(&facts).unwrap();
    assert!(!metrics.clean);
    assert_eq!((metrics.change_rounds, metrics.reviewer_rounds, metrics.interventions), (2, 1, 2));
    assert_eq!(metrics.blocked_ms, 8);
}

#[test]
fn a_reopened_or_relaunched_task_is_not_clean_and_the_attempt_that_ended_it_names_the_model() {
    let reopened = TaskFacts { history: history(&[(0, "running"), (5, "done"), (50, "running"), (60, "done")]), ..TaskFacts::default() };
    let metrics = fold_task(&reopened).unwrap();
    assert!(metrics.reopened && !metrics.clean);
    assert_eq!(metrics.work_ms, 15, "the time it stayed done is no one's");

    let relaunched = TaskFacts {
        history: history(&[(0, "running"), (5, "done")]),
        decisions: vec![
            launch("a", Some("sonnet"), Some("low")),
            Decision { kind: DecisionKind::Send, agent_id: Some("a".into()), model: None, effort: None, approach: Some("solo".into()) },
            Decision { kind: DecisionKind::Relaunch, agent_id: Some("b".into()), model: Some("opus".into()), effort: None, approach: None },
            Decision { kind: DecisionKind::Send, agent_id: Some("b".into()), model: None, effort: Some("max".into()), approach: None },
        ],
        ..TaskFacts::default()
    };
    let metrics = fold_task(&relaunched).unwrap();
    assert!(metrics.relaunched && !metrics.clean);
    assert_eq!((metrics.model.as_deref(), metrics.effort), (Some("opus"), None), "a message to the same node continues the attempt");
}

#[test]
fn a_handover_to_another_node_is_a_new_attempt_but_not_a_relaunch() {
    let facts = TaskFacts {
        history: history(&[(0, "running"), (5, "done")]),
        decisions: vec![
            launch("a", Some("opus"), Some("high")),
            launch("b", None, Some("low")),
            Decision { kind: DecisionKind::Send, agent_id: Some("c".into()), model: None, effort: None, approach: None },
        ],
        ..TaskFacts::default()
    };
    let metrics = fold_task(&facts).unwrap();
    assert!(metrics.relaunched, "a second launch relaunches");
    assert_eq!(metrics.model, None, "node c was never given a model");
    let handed = TaskFacts { decisions: vec![launch("a", Some("opus"), None), Decision { kind: DecisionKind::Send, agent_id: Some("b".into()), model: None, effort: None, approach: None }], ..facts };
    assert!(!fold_task(&handed).unwrap().relaunched);
}

#[test]
fn a_cancelled_task_ends_but_an_open_one_does_not() {
    let cancelled = TaskFacts { history: history(&[(0, "not_started"), (7, "cancelled")]), ..TaskFacts::default() };
    let metrics = fold_task(&cancelled).unwrap();
    assert_eq!(metrics.outcome, Outcome::Cancelled);
    assert!(!metrics.clean);
    assert_eq!((metrics.started_at, metrics.queued_ms), (None, 7));
    assert_eq!(fold_task(&TaskFacts { history: history(&[(0, "running")]), ..TaskFacts::default() }), None);
    assert_eq!(fold_task(&TaskFacts::default()), None);
}
