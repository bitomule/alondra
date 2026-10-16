//! A decision of the user on a Task others wait for holds every dependent that has not started
//! until the orchestrator settles its plan review. Whatever order decisions, settlements (valid,
//! refused or stale), assignments, starts, cancellations and the end of the waits come in, no held
//! Task gets a node, only its own settlement releases it (never its end, a reopening or the end of
//! its wait), and a refused command changes nothing.
use super::*;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

const QUESTIONS: usize = 5;
/// `b` is where the user decides; `c` → `b`, `d` → `c`, `e` → `d`; `x` waits for nothing.
const TASKS: [&str; 5] = ["c", "d", "e", "x", "b"];

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

fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

fn task(s: &mut DomainState, id: &str, agent: Option<&str>, dependencies: &[&str]) {
    must(s, orchestrator(), Command::CreateTask {
        task_id: id.into(), request_id: "r".into(), title: format!("Phase {id}"), agent_id: agent.map(Into::into), review_prs: false,
        dependencies: dependencies.iter().map(|d| TaskId::from(*d)).collect(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
        plan_items: None,
    });
}

pub(super) fn chain() -> DomainState {
    let mut s = DomainState::new("epoch");
    for agent in ["a", "z0", "z1", "z2", "z3"] {
        must(&mut s, Actor::System, Command::RegisterAgent { agent_id: agent.into(), name: agent.into(), at: 0 });
    }
    must(&mut s, user(), Command::CreateRequest {
        request_id: "r".into(), body: "Index".into(), auto_merge: false, maintenance: false, message: false,
    });
    must(&mut s, orchestrator(), Command::TitleRequest { request_id: "r".into(), title: "Redraft index".into() });
    task(&mut s, "b", Some("a"), &[]);
    must(&mut s, Actor::Agent { agent_id: "a".into() }, Command::StartTask { task_id: "b".into() });
    task(&mut s, "c", None, &["b"]);
    task(&mut s, "d", None, &["c"]);
    task(&mut s, "e", None, &["d"]);
    task(&mut s, "x", None, &[]);
    s
}

pub(super) fn decide(id: &str) -> Command {
    Command::AskQuestion {
        attachments: vec![], interaction_id: id.into(), scope: Scope::Task { task_id: "b".into() }, recipient: Recipient::User,
        body: "Measured on the iPhone: 11 of 21 groups match".into(), blocking: false,
        options: vec![
            QuestionOption { id: "threshold".into(), label: "Adjust the threshold".into(), description: None },
            QuestionOption { id: "plan-512".into(), label: "512 px prints".into(), description: None },
        ],
        card: CardSpec::new(CardKind::Decide, "Decide the index's next step").why("Phases C, D and E start after it"),
    }
}

pub(super) fn answer(id: &str) -> Command {
    Command::AnswerQuestion { attachments: vec![], interaction_id: id.into(), option_id: "plan-512".into(), inputs: BTreeMap::new() }
}

fn question(n: usize) -> String {
    format!("q{n}")
}

#[derive(Clone, Debug)]
enum Outcome {
    Unchanged,
    Changed { named: bool },
    Cancelled,
    Blank,
    KeptByUser,
}

impl Outcome {
    fn build(&self) -> PlanReviewOutcome {
        match self {
            Self::Unchanged => PlanReviewOutcome::Unchanged { reason: "The decision does not touch phase C".into() },
            Self::Changed { named } => PlanReviewOutcome::PlanChanged {
                launch: LaunchPlan::new(if *named { "node" } else { "" }, "/tmp"),
                reason: "512 px prints instead of the 360 px copy".into(),
            },
            Self::Cancelled => PlanReviewOutcome::Cancelled { reason: "Phase E no longer applies".into() },
            Self::Blank => PlanReviewOutcome::Unchanged { reason: " ".into() },
            Self::KeptByUser => PlanReviewOutcome::KeptByUser,
        }
    }
}

#[derive(Clone, Debug)]
enum Step {
    Ask(usize),
    Answer { question: usize, by_user: bool },
    Settle { question: usize, task: usize, outcome: Outcome, by_orchestrator: bool },
    Assign { task: usize, by_orchestrator: bool },
    Start(usize),
    Cancel(usize),
    Reopen(usize),
    StopWaiting(usize),
}

fn outcome() -> impl Strategy<Value = Outcome> {
    prop_oneof![
        4 => Just(Outcome::Unchanged),
        3 => any::<bool>().prop_map(|named| Outcome::Changed { named }),
        2 => Just(Outcome::Cancelled),
        1 => Just(Outcome::Blank),
        1 => Just(Outcome::KeptByUser),
    ]
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        3 => (0..QUESTIONS).prop_map(Step::Ask),
        4 => (0..QUESTIONS, prop_oneof![5 => Just(true), 1 => Just(false)]).prop_map(|(question, by_user)| Step::Answer { question, by_user }),
        6 => (0..QUESTIONS, 0..TASKS.len(), outcome(), prop_oneof![6 => Just(true), 1 => Just(false)])
            .prop_map(|(question, task, outcome, by_orchestrator)| Step::Settle { question, task, outcome, by_orchestrator }),
        3 => (0..4usize, any::<bool>()).prop_map(|(task, by_orchestrator)| Step::Assign { task, by_orchestrator }),
        3 => (0..4usize).prop_map(Step::Start),
        1 => (0..4usize).prop_map(Step::Cancel),
        1 => (0..4usize).prop_map(Step::Reopen),
        2 => (0..4usize).prop_map(Step::StopWaiting),
    ]
}

fn held(s: &DomainState) -> BTreeSet<(InteractionId, TaskId)> {
    s.plan_reviews.iter().flat_map(|(id, review)| review.open().map(move |task| (id.clone(), task.clone()))).collect()
}

fn check(s: &DomainState) {
    for (review, task) in held(s) {
        let t = &s.tasks[&task];
        assert!(t.agent_id.is_none() && !t.started, "{task} has a node while the review of {review} holds it");
        assert!(s.plan_review_pending(&task));
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn a_held_dependent_never_gets_a_node_and_only_its_own_settlement_releases_it(steps in prop::collection::vec(step(), 1..40)) {
        let mut s = chain();
        for step in steps {
            let before = s.clone();
            let result = match &step {
                Step::Ask(n) => exec(&mut s, &Actor::Agent { agent_id: "a".into() }, decide(&question(*n))),
                Step::Answer { question: n, by_user } => {
                    let actor = if *by_user { user() } else { orchestrator() };
                    exec(&mut s, &actor, answer(&question(*n)))
                }
                Step::Settle { question: n, task, outcome, by_orchestrator } => {
                    let actor = if *by_orchestrator { orchestrator() } else { user() };
                    exec(&mut s, &actor, Command::SettlePlanReview {
                        review_id: question(*n).into(), task_id: TASKS[*task].into(), outcome: outcome.build(),
                    })
                }
                Step::Assign { task, by_orchestrator } => {
                    let actor = if *by_orchestrator { orchestrator() } else { Actor::System };
                    exec(&mut s, &actor, Command::AssignTask { task_id: TASKS[*task].into(), agent_id: format!("z{task}").into(), model: None })
                }
                Step::Start(task) => exec(&mut s, &Actor::Agent { agent_id: format!("z{task}").into() }, Command::StartTask { task_id: TASKS[*task].into() }),
                Step::Cancel(task) => exec(&mut s, &user(), Command::CancelTask { task_id: TASKS[*task].into() }),
                Step::Reopen(task) => exec(&mut s, &user(), Command::ReopenTask { task_id: TASKS[*task].into() }),
                Step::StopWaiting(task) => exec(&mut s, &orchestrator(), Command::SetDependencies {
                    task_id: TASKS[*task].into(), dependencies: BTreeSet::new(), stack: None,
                }),
            };
            let Ok(outcome) = &result else {
                prop_assert_eq!(&s, &before, "a refused command changed state");
                continue;
            };
            super::lifecycle_tests::assert_aggregate_rules(&s);
            for (id, review) in &before.plan_reviews {
                for (task, settled) in review.dependents.iter().filter(|(_, settled)| settled.is_some()) {
                    prop_assert_eq!(s.plan_reviews[id].dependents.get(task), Some(settled), "a settled review changed");
                }
            }
            let released: BTreeSet<_> = held(&before).difference(&held(&s)).cloned().collect();
            for (review, task) in &released {
                let settled = s.plan_reviews[review].dependents[task].clone().expect("released means settled");
                match (&step, &settled.outcome) {
                    (Step::Settle { question: n, task: t, .. }, outcome) => {
                        prop_assert_eq!(review, &InteractionId::from(question(*n)));
                        prop_assert_eq!(task, &TaskId::from(TASKS[*t]));
                        prop_assert!(!matches!(outcome, PlanReviewOutcome::KeptByUser), "only the user keeps the plans");
                    }
                    (step, outcome) => prop_assert!(false, "{:?} released {} from {} with {:?}", step, task, review, outcome),
                }
            }
            match &step {
                Step::Answer { question: n, .. } => {
                    let id = InteractionId::from(question(*n));
                    let expected = before.unstarted_dependents(&"b".into());
                    match s.plan_reviews.get(&id) {
                        Some(review) => {
                            prop_assert_eq!(review.dependents.keys().cloned().collect::<BTreeSet<_>>(), expected);
                            prop_assert_eq!(review.at_revision, outcome.revision, "the hold is the answer's own commit");
                            let told = DomainEffect::ReviewDependentPlans { review_id: id.clone(), task_id: "b".into() };
                            prop_assert!(outcome.effects.contains(&told), "the orchestrator is told in the same commit");
                        }
                        None => prop_assert!(expected.is_empty(), "a decision with unstarted dependents held none"),
                    }
                }
                Step::Assign { task, .. } | Step::Start(task) =>
                    prop_assert!(!before.plan_review_pending(&TASKS[*task].into()), "a held task got a node"),
                _ => prop_assert_eq!(before.plan_reviews.len(), s.plan_reviews.len(), "only an answer holds dependents"),
            }
            check(&s);
        }
    }
}

#[test]
fn the_user_decision_holds_the_whole_unstarted_chain_and_tells_the_orchestrator() {
    let mut s = chain();
    must(&mut s, Actor::Agent { agent_id: "a".into() }, decide("q"));
    let outcome = exec(&mut s, &user(), answer("q")).unwrap();
    assert_eq!(outcome.effects.iter().filter(|e| matches!(e, DomainEffect::ReviewDependentPlans { .. })).count(), 1);
    let review = &s.plan_reviews[&InteractionId::from("q")];
    assert_eq!(review.dependents.keys().map(|t| t.0.as_str()).collect::<Vec<_>>(), ["c", "d", "e"]);
    assert_eq!(review.decisions, [InteractionId::from("q")]);
    let refused = exec(&mut s, &orchestrator(), Command::AssignTask { task_id: "c".into(), agent_id: "z0".into(), model: None }).unwrap_err();
    assert_eq!(refused.code, "plan_review_pending");
}

#[test]
fn ending_the_wait_does_not_release_the_hold_only_the_settlement_does() {
    let mut s = chain();
    must(&mut s, Actor::Agent { agent_id: "a".into() }, decide("q"));
    must(&mut s, user(), answer("q"));
    must(&mut s, orchestrator(), Command::SetDependencies { task_id: "c".into(), dependencies: BTreeSet::new(), stack: None });
    must(&mut s, orchestrator(), Command::AssignTask { task_id: "x".into(), agent_id: "z3".into(), model: None });
    assert!(s.plan_review_pending(&"c".into()));
    assert_eq!(exec(&mut s, &Actor::System, Command::AssignTask { task_id: "c".into(), agent_id: "z0".into(), model: None }).unwrap_err().code, "plan_review_pending");
    must(&mut s, orchestrator(), Command::SettlePlanReview {
        review_id: "q".into(), task_id: "c".into(),
        outcome: PlanReviewOutcome::PlanChanged { launch: LaunchPlan::new("node", "/tmp"), reason: "512 px prints".into() },
    });
    assert!(!s.plan_review_pending(&"c".into()));
    must(&mut s, Actor::System, Command::AssignTask { task_id: "c".into(), agent_id: "z0".into(), model: None });
    let again = exec(&mut s, &orchestrator(), Command::SettlePlanReview {
        review_id: "q".into(), task_id: "c".into(), outcome: PlanReviewOutcome::Unchanged { reason: "again".into() },
    }).unwrap_err();
    assert_eq!(again.code, "plan_review_settled");
}

#[test]
fn a_later_decision_holds_again_and_an_old_settlement_does_not_release_it() {
    let mut s = chain();
    must(&mut s, Actor::Agent { agent_id: "a".into() }, decide("q1"));
    must(&mut s, user(), answer("q1"));
    must(&mut s, Actor::Agent { agent_id: "a".into() }, decide("q2"));
    must(&mut s, user(), answer("q2"));
    for task in ["c", "d", "e"] {
        must(&mut s, orchestrator(), Command::SettlePlanReview {
            review_id: "q1".into(), task_id: task.into(), outcome: PlanReviewOutcome::Unchanged { reason: "Same plan".into() },
        });
    }
    assert!(["c", "d", "e"].iter().all(|t| s.plan_review_pending(&(*t).into())), "the second decision still holds them");
}

#[test]
fn cancelling_from_the_review_cancels_the_task_in_the_same_commit() {
    let mut s = chain();
    must(&mut s, Actor::Agent { agent_id: "a".into() }, decide("q"));
    must(&mut s, user(), answer("q"));
    let outcome = exec(&mut s, &orchestrator(), Command::SettlePlanReview {
        review_id: "q".into(), task_id: "e".into(), outcome: PlanReviewOutcome::Cancelled { reason: "Phase E no longer applies".into() },
    }).unwrap();
    assert!(s.tasks[&TaskId::from("e")].cancelled() && s.tasks[&TaskId::from("e")].state == TaskState::Closing);
    assert_eq!(s.tasks[&TaskId::from("e")].revision, outcome.revision);
    must(&mut s, Actor::System, Command::RecordTaskCleanup { task_id: "e".into() });
    assert_eq!(s.tasks[&TaskId::from("e")].state, TaskState::Cancelled);
    assert!(matches!(s.plan_reviews[&InteractionId::from("q")].dependents[&TaskId::from("e")],
        Some(PlanReviewSettlement { outcome: PlanReviewOutcome::Cancelled { .. }, .. })));
}

#[test]
fn a_decision_without_unstarted_dependents_holds_nothing() {
    let mut s = chain();
    for task in ["c", "d", "e"] {
        must(&mut s, user(), Command::CancelTask { task_id: task.into() });
    }
    must(&mut s, Actor::Agent { agent_id: "a".into() }, decide("q"));
    let outcome = exec(&mut s, &user(), answer("q")).unwrap();
    assert!(s.plan_reviews.is_empty());
    assert!(!outcome.effects.iter().any(|e| matches!(e, DomainEffect::ReviewDependentPlans { .. })));
}

#[test]
fn a_held_task_that_is_cancelled_and_reopened_is_still_held() {
    let mut s = chain();
    must(&mut s, Actor::Agent { agent_id: "a".into() }, decide("q"));
    must(&mut s, user(), answer("q"));
    must(&mut s, user(), Command::CancelTask { task_id: "c".into() });
    assert!(s.plan_review_waiting(&s.plan_reviews[&InteractionId::from("q")]).all(|t| t.id.0 != "c"), "a closed task asks for no review");
    must(&mut s, Actor::System, Command::RecordTaskCleanup { task_id: "c".into() });
    must(&mut s, user(), Command::ReopenTask { task_id: "c".into() });
    assert!(s.plan_review_pending(&"c".into()), "its plan still predates the decision");
    assert_eq!(exec(&mut s, &Actor::System, Command::AssignTask { task_id: "c".into(), agent_id: "z0".into(), model: None }).unwrap_err().code, "plan_review_pending");
}
