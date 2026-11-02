//! Alondra's own cards are what the command that created them says, never what their id looks
//! like: a node that names a card like one of them gets no effect of theirs.
use super::*;
use std::collections::BTreeSet;

fn exec(s: &mut DomainState, a: Actor, c: Command) -> Result<CommandOutcome, DomainError> {
    let envelope = CommandEnvelope {
        protocol_version: 2,
        data_epoch: s.data_epoch.clone(),
        command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision,
        command: c,
    };
    s.execute(&a, &envelope)
}
fn ok(s: &mut DomainState, a: Actor, c: Command) {
    exec(s, a, c).unwrap();
}
fn refused(s: &mut DomainState, a: Actor, c: Command) -> DomainError {
    let before = s.clone();
    let error = exec(s, a, c).unwrap_err();
    assert_eq!(*s, before, "a refused command changed the state");
    error
}
fn agent() -> Actor {
    Actor::Agent { agent_id: "a".into() }
}
fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}
fn fixture() -> DomainState {
    let mut s = DomainState::new("epoch");
    ok(&mut s, Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "Node A".into(), at: 0 });
    ok(&mut s, Actor::User { user_id: "u".into() }, Command::CreateRequest { request_id: "r".into(), body: "Build it".into(), auto_merge: false, maintenance: false, message: false });
    ok(&mut s, orchestrator(), Command::CreateTask { plan_items: None,
        task_id: "t".into(), request_id: "r".into(), title: "Build it".into(), agent_id: Some("a".into()),
        review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    s
}
fn ask(id: &str, card: CardSpec) -> Command {
    Command::AskQuestion {
        attachments: vec![], interaction_id: id.into(), scope: Scope::Task { task_id: "t".into() },
        recipient: Recipient::User, body: "Which database?".into(), blocking: false, card,
        options: vec![
            QuestionOption { id: "resume".into(), label: "SQLite".into(), description: Some("Keep the file database".into()) },
            QuestionOption { id: "other".into(), label: "PostgreSQL".into(), description: Some("Move to the server".into()) },
        ],
    }
}
fn decide() -> CardSpec {
    CardSpec::new(CardKind::Decide, "Choose the database for the cache").why("The cache needs a store before it ships.")
}

#[test]
fn a_node_cannot_name_its_card_like_one_of_alondras() {
    let mut s = fixture();
    for id in ["pace-resume-x", "node-pause-user-1-t", "machine-setup", "problem-close-t-1", "unheard-t-9"] {
        let error = refused(&mut s, agent(), ask(id, decide()));
        assert_eq!((error.code.as_str(), error.subject.as_deref()), ("invalid_input", Some("interaction_id")), "{id}");
    }
    ok(&mut s, agent(), ask("database-choice", decide()));
    assert_eq!(s.interactions[&"database-choice".into()].system_card, None);
}

#[test]
fn only_alondra_says_a_card_is_one_of_its_own() {
    let mut s = fixture();
    let card = CardSpec { system: Some(SystemCard::PaceResume { day: "2026-11-02".into() }), ..decide() };
    let error = refused(&mut s, agent(), ask("database-choice", card.clone()));
    assert_eq!((error.code.as_str(), error.subject.as_deref()), ("forbidden", Some("card.system")));
    let error = refused(&mut s, orchestrator(), Command::AskQuestion {
        attachments: vec![], interaction_id: "orchestrator-choice".into(), scope: Scope::Orchestrator { request_id: None },
        recipient: Recipient::User, body: "Which?".into(), blocking: false, card,
        options: vec![QuestionOption { id: "resume".into(), label: "Resume".into(), description: Some("Resume the work".into()) },
            QuestionOption { id: "stay".into(), label: "Stay".into(), description: Some("Stay paused".into()) }],
    });
    assert_eq!(error.code, "forbidden");
}

#[test]
fn the_kind_alondra_gives_a_card_is_kept_and_not_shown_in_its_card() {
    let mut s = fixture();
    let card = CardSpec { system: Some(SystemCard::Machine), ..CardSpec::new(CardKind::Problem, "Free up memory").why("Nodes wait for memory.") };
    ok(&mut s, Actor::System, Command::RaiseProblem {
        attachments: vec![], interaction_id: "machine-1000".into(), scope: Scope::Orchestrator { request_id: None },
        body: "{}".into(), options: vec![QuestionOption { id: "fixed".into(), label: "Fixed".into(), description: None }], card,
    });
    let raised = &s.interactions[&"machine-1000".into()];
    assert_eq!((raised.system_card.as_ref(), raised.card.system.as_ref()), (Some(&SystemCard::Machine), None));
}

#[test]
fn a_card_named_like_a_pause_but_not_one_pauses_nothing() {
    let mut s = fixture();
    let id = InteractionId::from("node-pause-user-5-t");
    let untyped = Interaction { shortened: false,
        attachments: vec![], id: id.clone(), scope: Scope::Task { task_id: "t".into() }, author: Author::System,
        recipient: Recipient::User, body: "Resume".into(), blocking: false, subject: None,
        payload: InteractionPayload::Question { options: vec![], answer: None }, revision: 1, order: 1, withdrawn: false,
        card: CardSpec::new(CardKind::Decide, "Resume"), clarification: None, snoozed_until: None, resurfaced_at_revision: None,
        system_card: None,
    };
    s.interactions.insert(id.clone(), untyped.clone());
    assert!(!s.node_pause_card(&id) && !s.out_of_sight(&untyped), "an id is not a pause");
    let typed = Interaction { system_card: Some(SystemCard::NodePause { pause: NodePauseKind::User, since: 5 }), ..untyped };
    s.interactions.insert(id.clone(), typed.clone());
    assert!(s.node_pause_card(&id) && s.out_of_sight(&typed));
}

#[test]
fn a_hold_card_is_a_claude_hold_by_its_kind_alone() {
    let card = |id: &str, system_card| Interaction { shortened: false,
        attachments: vec![], id: id.into(), scope: Scope::Task { task_id: "t".into() }, author: Author::System,
        recipient: Recipient::User, body: String::new(), blocking: false, subject: None,
        payload: InteractionPayload::Question { options: vec![], answer: None }, revision: 1, order: 1, withdrawn: false,
        card: CardSpec::new(CardKind::Problem, "Sign in"), clarification: None, snoozed_until: None, resurfaced_at_revision: None,
        system_card,
    };
    assert_eq!(ClaudeHold::of(&card("claude-signed-out-t-1", None)), None);
    assert_eq!(ClaudeHold::of(&card("anything", Some(SystemCard::ClaudeUsage { resets_at: Some(9) }))), Some(ClaudeHold::Usage));
}

#[test]
fn a_stored_card_gets_the_kind_its_id_was_written_with() {
    let cases = [
        ("node-pause-machine-1700-t-1", Some(SystemCard::NodePause { pause: NodePauseKind::Machine, since: 1700 })),
        ("claude-usage-limit-until-4102444800-t-3", Some(SystemCard::ClaudeUsage { resets_at: Some(4_102_444_800) })),
        ("claude-usage-limit-t-3", Some(SystemCard::ClaudeUsage { resets_at: None })),
        ("over-pace-2026-11-02", Some(SystemCard::OverPace { day: "2026-11-02".into() })),
        ("pace-resume-2026-11-02", Some(SystemCard::PaceResume { day: "2026-11-02".into() })),
        ("machine-1000-2", Some(SystemCard::Machine)),
        ("problem-launch-run-7", Some(SystemCard::LaunchProblem { run: "run-7".into() })),
        ("problem-orchestrator-batch-3", Some(SystemCard::BatchProblem { run: "batch-3".into() })),
        ("problem-stuck-orchestrator-batch-3", Some(SystemCard::StuckProblem)),
        ("ended-unanswered-q-1", Some(SystemCard::EndedUnanswered { question: "q-1".into() })),
        ("question-unanswered-q-1-2", Some(SystemCard::Inbox { about: InboxCard::UnansweredNotice, subject: "q-1".into(), number: 2 })),
        ("suggestion-unsettled-s-1", Some(SystemCard::Inbox { about: InboxCard::UnsettledNotice, subject: "s-1".into(), number: 1 })),
        ("request-done-r-1", Some(SystemCard::RequestDone)),
        ("database-choice", None),
    ];
    for (id, kind) in cases {
        assert_eq!(SystemCard::of_stored_id(id), kind, "{id}");
    }
}

#[test]
fn a_kind_a_newer_daemon_wrote_reads_as_unknown() {
    let card: SystemCard = serde_json::from_str(r#"{"kind":"something_new","detail":1}"#).unwrap();
    assert_eq!(card, SystemCard::Unknown);
}
