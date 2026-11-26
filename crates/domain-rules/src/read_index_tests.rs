//! The read index says what the state answers on demand, and the lifecycles the domain keeps are
//! the ones its journals replay to, after any sequence of commands.
use super::lifecycle_cache_tests::three_tasks;
use super::lifecycle_tests::{exec, node, user};
use super::situation_tests::{apply, moves, Move};
use super::*;
use proptest::prelude::*;

fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}

const TASKS: [(&str, &str); 3] = [("a-task", "a"), ("b-task", "b"), ("s-task", "c")];

#[derive(Clone, Debug)]
enum Play {
    Move(Move),
    RegisterPr(usize),
    ObservePr(usize, PrState),
    PostMergeWork(usize, bool),
    Inform(usize),
    MarkRead,
    Unsnooze,
    Resurface,
    Archive(usize, bool),
    Depend(usize, usize),
    WaitFor(usize, usize),
    Problem(usize),
    End(usize),
    MergeMethod(usize),
}

fn plays() -> impl Strategy<Value = Play> {
    let task = 0..TASKS.len();
    let pr_state = prop_oneof![Just(PrState::Open), Just(PrState::Closed), Just(PrState::Merged)];
    prop_oneof![
        12 => moves().prop_map(Play::Move),
        1 => task.clone().prop_map(Play::RegisterPr),
        1 => (task.clone(), pr_state).prop_map(|(task, state)| Play::ObservePr(task, state)),
        1 => (task.clone(), any::<bool>()).prop_map(|(task, reason)| Play::PostMergeWork(task, reason)),
        1 => task.clone().prop_map(Play::Inform),
        1 => Just(Play::MarkRead),
        1 => Just(Play::Unsnooze),
        1 => Just(Play::Resurface),
        2 => (task.clone(), prop::bool::weighted(0.8)).prop_map(|(task, archived)| Play::Archive(task, archived)),
        1 => (task.clone(), task.clone()).prop_map(|(task, on)| Play::Depend(task, on)),
        1 => (task.clone(), task.clone()).prop_map(|(task, on)| Play::WaitFor(task, on)),
        1 => task.clone().prop_map(Play::Problem),
        1 => task.clone().prop_map(Play::End),
        1 => (0..MergeMethod::ALL.len()).prop_map(Play::MergeMethod),
    ]
}

fn pr_of(task: usize) -> PrId {
    hosted_pr_id("github.com", "acme/app", task as u64 + 1)
}

fn first_user_card(s: &DomainState, keep: impl Fn(&Interaction) -> bool) -> Option<InteractionId> {
    s.interactions.values().filter(|i| i.recipient == Recipient::User && keep(i)).map(|i| i.id.clone()).next()
}

fn play(s: &mut DomainState, play: &Play, n: usize) {
    let id = |task: usize| TaskId::from(TASKS[task].0);
    let agent = |task: usize| node(TASKS[task].1);
    let _ = match play {
        Play::Move(step) => {
            apply(s, step, n);
            return;
        }
        Play::RegisterPr(task) => exec(s, agent(*task), Command::RegisterPr {
            task_id: id(*task), pr_id: pr_of(*task), host: "github.com".into(), repository: "acme/app".into(),
            number: *task as u64 + 1, head: format!("head-{task}"), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
        }),
        Play::ObservePr(task, state) => exec(s, Actor::System, Command::ObservePrState {
            pr_id: pr_of(*task), head: format!("head-{task}"), base: "main".into(), state: *state,
            merged_head: (*state == PrState::Merged).then(|| format!("head-{task}")),
        }),
        Play::PostMergeWork(task, reason) => exec(s, agent(*task), Command::DeclarePostMergeWork {
            task_id: id(*task), reason: reason.then(|| "Publishing the release".into()), waits_for: vec![],
        }),
        Play::Inform(task) => exec(s, Actor::System, Command::PublishInformation {
            attachments: vec![], interaction_id: format!("information-{n}").into(), scope: Scope::Task { task_id: id(*task) },
            recipient: Recipient::User, body: "The build is green".into(), card: CardSpec::derived(CardKind::Know, "Read the update"),
        }),
        Play::MarkRead => {
            let unread = |i: &Interaction| matches!(&i.payload, InteractionPayload::Information { read_by } if read_by.is_empty());
            let Some(card) = first_user_card(s, unread) else { return };
            exec(s, user(), Command::MarkRead { interaction_id: card })
        }
        Play::Unsnooze => {
            let Some(card) = first_user_card(s, |i| i.snoozed_until.is_some()) else { return };
            exec(s, user(), Command::UnsnoozeCard { interaction_id: card })
        }
        Play::Resurface => exec(s, Actor::System, Command::ResurfaceDueCards { now: 1_000_000 + n as i64 }),
        Play::Archive(task, archived) => exec(s, user(), Command::ArchiveTask { task_id: id(*task), archived: *archived }),
        Play::Depend(task, on) => exec(s, orchestrator(), Command::SetDependencies {
            task_id: id(*task), dependencies: [id(*on)].into(), stack: None, reason: None,
        }),
        Play::WaitFor(task, on) => exec(s, agent(*task), Command::WaitForTasks { task_id: id(*task), tasks: [id(*on)].into() }),
        Play::Problem(task) => exec(s, Actor::System, Command::RaiseProblem {
            attachments: vec![], interaction_id: format!("problem-{n}").into(), scope: Scope::Task { task_id: id(*task) },
            body: "The launch failed".into(),
            options: vec![
                QuestionOption { id: "retry".into(), label: "Retry".into(), description: None },
                QuestionOption { id: "cancel".into(), label: "Cancel the task".into(), description: None },
            ],
            card: CardSpec::new(CardKind::Problem, "Decide what to do with the failed launch").why("The task has no node"),
        }),
        Play::End(task) => {
            let _ = exec(s, user(), Command::CancelTask { task_id: id(*task), reason: None });
            exec(s, Actor::System, Command::RecordNodeEnd { task_id: id(*task), end: NodeEnd::Closed, agent_id: Some(TASKS[*task].1.into()) })
        }
        Play::MergeMethod(method) => exec(s, user(), Command::SetMergeMethod { method: MergeMethod::ALL[*method] }),
    };
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, failure_persistence: None, rng_seed: proptest::test_runner::RngSeed::Fixed(11), ..ProptestConfig::default() })]

    #[test]
    fn every_command_leaves_an_index_that_reads_what_the_state_answers(history in prop::collection::vec(plays(), 1..60)) {
        let mut s = three_tasks();
        assert_index_matches(&s);
        for (n, step) in history.iter().enumerate() {
            play(&mut s, step, n);
            assert_index_matches(&s);
        }
    }
}
