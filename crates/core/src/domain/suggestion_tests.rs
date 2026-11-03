//! A node's suggestion is settled by one closed decision. Whatever order suggestions, decisions
//! (valid or not), free answers, withdrawals and the end of the task come in, no suggestion is
//! answered without a decision, a new task exists from the command that promised it, and a refused
//! command changes nothing.
use super::*;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

const SLOTS: usize = 7;

fn exec(s: &mut DomainState, actor: &Actor, command: Command) -> Result<CommandOutcome, DomainError> {
    s.execute(actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: format!("c-{}", s.revision).into(),
        expected_revision: s.revision, command,
    })
}

fn must(s: &mut DomainState, actor: Actor, command: Command) {
    let shown = format!("{command:?}");
    if let Err(error) = exec(s, &actor, command) {
        panic!("{shown} was refused: {error}");
    }
}

fn node() -> Actor {
    Actor::Agent { agent_id: "a".into() }
}

fn start() -> DomainState {
    let mut s = DomainState::new("epoch");
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "Node".into(), at: 0 });
    must(&mut s, Actor::User { user_id: "u".into() }, Command::CreateRequest {
        request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
    });
    must(&mut s, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
        task_id: "t".into(), request_id: "r".into(), title: "Work".into(), agent_id: Some("a".into()), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    must(&mut s, node(), Command::StartTask { task_id: "t".into() });
    s
}

fn slot(n: usize) -> InteractionId {
    format!("p{n}").into()
}

#[derive(Clone, Debug)]
enum Decision {
    NewTask { task: usize, request: usize, title: usize, dependency: usize },
    CurrentTask { blank: bool },
    Discard { blank: bool },
}

impl Decision {
    fn build(&self) -> SuggestionDecision {
        match self {
            Self::NewTask { task, request, title, dependency } => SuggestionDecision::NewTask {
                task_id: ["n0", "n1", "n2", "t"][*task].into(),
                request_id: ["q0", "q1", "q2", "r"][*request].into(),
                title: match title {
                    0 => "Update the README flag".into(),
                    1 => String::new(),
                    _ => "x".repeat(61),
                },
                launch: LaunchPlan::new("node", "/tmp"),
                dependencies: match dependency {
                    0 => BTreeSet::new(),
                    1 => BTreeSet::from(["t".into()]),
                    _ => BTreeSet::from(["missing".into()]),
                },
                note: None,
            },
            Self::CurrentTask { blank } => SuggestionDecision::CurrentTask { note: if *blank { " ".into() } else { "Step 3 covers it".into() } },
            Self::Discard { blank } => SuggestionDecision::Discard { reason: if *blank { String::new() } else { "Task 12 covers it".into() } },
        }
    }
}

#[derive(Clone, Debug)]
enum Step {
    Suggest(usize),
    Settle { slot: usize, decision: Decision, actor: usize },
    Answer { slot: usize, option: usize },
    Withdraw(usize),
    Cancel,
}

fn decision() -> impl Strategy<Value = Decision> {
    prop_oneof![
        3 => (0..4usize, 0..4usize, prop_oneof![4 => Just(0usize), 1 => 1..3usize], prop_oneof![4 => Just(0usize), 1 => 1..3usize])
            .prop_map(|(task, request, title, dependency)| Decision::NewTask { task, request, title, dependency }),
        2 => prop_oneof![4 => Just(false), 1 => Just(true)].prop_map(|blank| Decision::CurrentTask { blank }),
        2 => prop_oneof![4 => Just(false), 1 => Just(true)].prop_map(|blank| Decision::Discard { blank }),
    ]
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        4 => (0..SLOTS).prop_map(Step::Suggest),
        5 => (0..SLOTS, decision(), prop_oneof![6 => Just(0usize), 1 => 1..3usize])
            .prop_map(|(slot, decision, actor)| Step::Settle { slot, decision, actor }),
        2 => (0..SLOTS, 0..4usize).prop_map(|(slot, option)| Step::Answer { slot, option }),
        1 => (0..SLOTS).prop_map(Step::Withdraw),
        1 => Just(Step::Cancel),
    ]
}

fn answered(s: &DomainState, id: &InteractionId) -> Option<Answer> {
    match s.interactions.get(id).map(|i| &i.payload) {
        Some(InteractionPayload::Question { answer, .. }) => answer.clone(),
        _ => None,
    }
}

fn check(s: &DomainState) {
    for i in s.interactions.values().filter(|i| i.card.kind == CardKind::Propose) {
        let Some(answer) = answered(s, &i.id) else { continue };
        let decision = answer.decision.as_ref().unwrap_or_else(|| panic!("{} is answered without a decision: {answer:?}", i.id));
        assert_eq!(answer.option_id, decision.option_id());
        match decision {
            SuggestionDecision::NewTask { task_id, request_id, .. } => {
                let task = s.tasks.get(task_id).unwrap_or_else(|| panic!("{} promised task {task_id}, which does not exist", i.id));
                assert_eq!(&task.request_id, request_id);
            }
            SuggestionDecision::CurrentTask { note } => assert!(!note.trim().is_empty()),
            SuggestionDecision::Discard { reason } => assert!(!reason.trim().is_empty()),
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn a_suggestion_is_settled_once_by_one_decision_and_its_new_task_exists(steps in prop::collection::vec(step(), 1..40)) {
        let mut s = start();
        for step in steps {
            let before = s.clone();
            let (result, target) = match &step {
                Step::Suggest(n) => (exec(&mut s, &node(), Command::AskQuestion {
                    attachments: vec![], interaction_id: slot(*n), scope: Scope::Task { task_id: "t".into() }, recipient: Recipient::Orchestrator,
                    body: format!("Seen while working: {n}"), blocking: false, options: suggestion_options(),
                    card: CardSpec::new(CardKind::Propose, "Update the README's old flag").why("It is not part of this task"),
                }), None),
                Step::Settle { slot: n, decision, actor } => {
                    let actor = [Actor::Orchestrator { generation: 0 }, Actor::User { user_id: "u".into() }, node()][*actor].clone();
                    (exec(&mut s, &actor, Command::SettleSuggestion { interaction_id: slot(*n), decision: decision.build() }), Some(*n))
                }
                Step::Answer { slot: n, option } => {
                    let (option_id, inputs) = match option {
                        0 => (String::new(), BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text("It goes as a separate task".into()))])),
                        n => (SUGGESTION_ANSWERS[n - 1].0.to_owned(), BTreeMap::new()),
                    };
                    let refused = exec(&mut s, &Actor::Orchestrator { generation: 0 }, Command::AnswerQuestion {
                        attachments: vec![], interaction_id: slot(*n), option_id, inputs,
                    });
                    prop_assert!(refused.is_err(), "a suggestion was answered without a decision");
                    (refused, None)
                }
                Step::Withdraw(n) => (exec(&mut s, &node(), Command::Withdraw { interaction_id: slot(*n) }), None),
                Step::Cancel => (exec(&mut s, &Actor::User { user_id: "u".into() }, Command::CancelTask { task_id: "t".into() }), None),
            };
            match &result {
                Err(_) => prop_assert_eq!(&s, &before, "a refused command changed state"),
                Ok(_) => {
                    let created = s.tasks.len() - before.tasks.len();
                    match (&step, target) {
                        (Step::Settle { .. }, Some(n)) => {
                            prop_assert!(answered(&before, &slot(n)).is_none(), "a suggestion was settled twice");
                            let answer = answered(&s, &slot(n)).expect("settled");
                            match answer.decision.as_ref().expect("a decision") {
                                SuggestionDecision::NewTask { task_id, .. } => {
                                    prop_assert_eq!(created, 1);
                                    prop_assert!(!before.tasks.contains_key(task_id));
                                    prop_assert_eq!(s.tasks[task_id].revision, answer.at_revision, "the task and its answer are one commit");
                                }
                                _ => prop_assert_eq!(created, 0),
                            }
                        }
                        _ => prop_assert_eq!(created, 0, "only a new_task decision creates a task"),
                    }
                }
            }
            check(&s);
        }
    }
}
