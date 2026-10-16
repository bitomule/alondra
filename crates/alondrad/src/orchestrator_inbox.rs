//! Nothing waits in the orchestrator. Once it handled everything it was handed, every open request
//! without a task, an answer or a question of its own is settled: a message was acted on and
//! closes, a request of the user's goes back to it once and then becomes the user's Problem.
//! A node's question gets its answer the same way: it goes back once, then becomes the user's
//! Problem; a node's suggestion goes back once.
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{
    Author, CardKind, DomainState, Interaction, InteractionId, InteractionPayload, QuestionOption, Request, RequestId, Scope,
};
use alondra_core::orchestrator_cards::{
    UNANSWERED_NOTICE_PREFIX, UNANSWERED_PROBLEM_PREFIX, UNANSWERED_REMIND, UNANSWERED_REMIND_LABEL, UNANSWERED_WITHDRAW,
    UNANSWERED_WITHDRAW_LABEL, UNSETTLED_NOTICE_PREFIX, UNSPLIT_CLOSE, UNSPLIT_CLOSE_LABEL, UNSPLIT_NOTICE_PREFIX,
    UNSPLIT_PROBLEM_PREFIX, UNSPLIT_RETRY, UNSPLIT_RETRY_LABEL, UnansweredProblem, UnsplitProblem,
};

const UNSPLIT_CAUSE: &str = "request_unsplit";
const UNANSWERED_CAUSE: &str = "question_unanswered";
const BODY_SHOWN: usize = 2_000;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Close { request: RequestId, withdrawn: bool },
    Remind { request: RequestId, id: String, text: String },
    Raise { request: RequestId, id: String, problem: UnsplitProblem },
    /// A reminder about a node's question or suggestion.
    Tell { id: String, text: String },
    RaiseUnanswered { request: Option<RequestId>, id: String, problem: UnansweredProblem },
    Withdraw { question: InteractionId },
}

/// What is left in the orchestrator's inbox after it handled everything it was handed.
pub(crate) fn steps(state: &DomainState) -> Vec<Step> {
    let mut steps = vec![];
    for interaction in state.interactions.values().filter(|i| waits(i)) {
        if interaction.card.kind == CardKind::Propose {
            steps.extend(unsettled(state, interaction));
        } else if task_of(state, interaction).is_some_and(|task| !task.state.terminal()) {
            steps.extend(unanswered(state, interaction));
        }
    }
    for request in state.requests.values().filter(|request| !request.closed) {
        if state.tasks.values().any(|task| task.request_id == request.id) || asked_about(state, &request.id) {
            continue;
        }
        if request.message {
            steps.push(Step::Close { request: request.id.clone(), withdrawn: false });
            continue;
        }
        steps.extend(unsplit(state, request));
    }
    steps
}

/// The orchestrator waits for the user's answer about this request: nothing is left in it.
fn asked_about(state: &DomainState, request: &RequestId) -> bool {
    state.interactions.values().any(|i| {
        i.author != Author::System
            && !i.withdrawn
            && matches!(&i.scope, Scope::Orchestrator { request_id: Some(id) } if id == request)
            && matches!(&i.payload, InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. })
    })
}

/// The cards of one kind about `request`, by their number.
fn numbered<'a>(state: &'a DomainState, prefix: &str, subject: &str) -> Vec<(u32, &'a alondra_core::domain::Interaction)> {
    let lead = format!("{prefix}{subject}-");
    state.interactions.values()
        .filter_map(|i| i.id.0.strip_prefix(&lead).and_then(|n| n.parse().ok()).map(|n| (n, i)))
        .collect()
}

fn unsplit(state: &DomainState, request: &Request) -> Option<Step> {
    let reminders = numbered(state, UNSPLIT_NOTICE_PREFIX, &request.id.0).len() as u32;
    let problems = numbered(state, UNSPLIT_PROBLEM_PREFIX, &request.id.0);
    let mut last: Option<(u32, Option<&str>)> = None;
    for (n, problem) in &problems {
        let answer = match &problem.payload {
            InteractionPayload::Question { answer: Some(answer), .. } => Some(answer.option_id.as_str()),
            InteractionPayload::Question { answer: None, .. } if !problem.withdrawn => return None,
            _ => None,
        };
        if last.is_none_or(|(m, _)| *n > m) {
            last = Some((*n, answer));
        }
    }
    if let Some((_, Some(UNSPLIT_CLOSE))) = last {
        return Some(Step::Close { request: request.id.clone(), withdrawn: true });
    }
    let raised = problems.len() as u32;
    if reminders > raised {
        let title = request.title.clone().unwrap_or_else(|| request.body.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default().to_owned());
        return Some(Step::Raise {
            request: request.id.clone(),
            id: format!("{UNSPLIT_PROBLEM_PREFIX}{}-{}", request.id, raised + 1),
            problem: UnsplitProblem { request_id: request.id.0.clone(), title },
        });
    }
    let body: String = request.body.trim().chars().take(BODY_SHOWN).collect();
    Some(Step::Remind {
        request: request.id.clone(),
        id: format!("{UNSPLIT_NOTICE_PREFIX}{}-{}", request.id, reminders + 1),
        text: format!(
            "Request {} has no Task, no answer and no open question of yours, and nothing else waits for you. Handle it now: create its Task, answer it and close it with close_request, or ask the user. An answer written only in this conversation reaches nobody. If it no longer applies, close it with close_request and say why.\nRequest: {body}",
            request.id),
    })
}

/// A problem per question: two questions left unanswered never replace each other's card.
fn unanswered_cause(question: &str) -> String {
    let room = alondra_core::domain::CardSlot::CAUSE_MAX - UNANSWERED_CAUSE.len() - 1;
    format!("{UNANSWERED_CAUSE}:{}", question.chars().take(room).collect::<String>())
}

fn task_of<'a>(state: &'a DomainState, i: &Interaction) -> Option<&'a alondra_core::domain::Task> {
    match &i.scope {
        Scope::Task { task_id } => state.tasks.get(task_id),
        Scope::Orchestrator { .. } => None,
    }
}

/// A node's question or suggestion to the orchestrator still without its answer.
fn waits(i: &Interaction) -> bool {
    matches!(i.author, Author::Agent { .. })
        && i.recipient == alondra_core::domain::Recipient::Orchestrator
        && !i.withdrawn
        && matches!(&i.payload, InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. })
}

fn quoted(i: &Interaction) -> String {
    let body: String = i.body.trim().chars().take(BODY_SHOWN).collect();
    format!("\"{}\"\n{body}", i.card.need)
}

fn unsettled(state: &DomainState, suggestion: &Interaction) -> Option<Step> {
    let id = format!("{UNSETTLED_NOTICE_PREFIX}{}", suggestion.id);
    (!state.interactions.contains_key(&InteractionId(id.clone()))).then(|| Step::Tell {
        id,
        text: format!(
            "Suggestion {} from a node has no decision, and nothing else waits for you. Settle it now with settle_suggestion: new_task (the call creates it), current_task or discard with the reason. If you asked the user about it, settle it once they answer.\nSuggestion: {}",
            suggestion.id, quoted(suggestion)),
    })
}

fn unanswered(state: &DomainState, question: &Interaction) -> Option<Step> {
    let reminders = numbered(state, UNANSWERED_NOTICE_PREFIX, &question.id.0).len() as u32;
    let problems = numbered(state, UNANSWERED_PROBLEM_PREFIX, &question.id.0);
    let mut last: Option<(u32, Option<&str>)> = None;
    for (n, problem) in &problems {
        let answer = match &problem.payload {
            InteractionPayload::Question { answer: Some(answer), .. } => Some(answer.option_id.as_str()),
            InteractionPayload::Question { answer: None, .. } if !problem.withdrawn => return None,
            _ => None,
        };
        if last.is_none_or(|(m, _)| *n > m) {
            last = Some((*n, answer));
        }
    }
    if let Some((_, Some(UNANSWERED_WITHDRAW))) = last {
        return Some(Step::Withdraw { question: question.id.clone() });
    }
    let raised = problems.len() as u32;
    if reminders > raised {
        let task = task_of(state, question)?;
        return Some(Step::RaiseUnanswered {
            request: state.requests.get(&task.request_id).filter(|request| !request.closed).map(|request| request.id.clone()),
            id: format!("{UNANSWERED_PROBLEM_PREFIX}{}-{}", question.id, raised + 1),
            problem: UnansweredProblem { question_id: question.id.0.clone(), task_title: task.title.clone(), question: question.card.need.clone() },
        });
    }
    Some(Step::Tell {
        id: format!("{UNANSWERED_NOTICE_PREFIX}{}-{}", question.id, reminders + 1),
        text: format!(
            "Reminder {}: question {} from a node has no answer, and nothing else waits for you: its node stands still until you answer. Answer it now with answer_question, also if what it asked for is already done (say so). An answer written only in this conversation never reaches the node.\nQuestion: {}",
            reminders + 1, question.id, quoted(question)),
    })
}

/// Carries the steps out; one that fails is tried again on the next pass.
pub(crate) fn settle(domain: &DomainRuntime) {
    let Ok(state) = domain.state() else { return };
    for step in steps(&state) {
        let done = match &step {
            Step::Close { request, withdrawn } => domain.close_request_by_system(request, *withdrawn).map(drop),
            Step::Remind { id, text, .. } => domain.tell_orchestrator(id, text.clone()).map(drop),
            Step::Raise { request, id, problem } => {
                let (need, why) = problem.text(alondra_core::views::Lang::En);
                let option = |id: &str, label: &str| QuestionOption { id: id.into(), label: label.into(), description: None };
                domain.raise_worded_problem_in(
                    Scope::Orchestrator { request_id: Some(request.clone()) },
                    id, UNSPLIT_CAUSE, need, why, problem.to_body(),
                    vec![option(UNSPLIT_RETRY, UNSPLIT_RETRY_LABEL), option(UNSPLIT_CLOSE, UNSPLIT_CLOSE_LABEL)],
                    Some(UNSPLIT_RETRY),
                ).map(drop)
            }
            Step::Tell { id, text } => domain.tell_orchestrator(id, text.clone()).map(drop),
            Step::RaiseUnanswered { request, id, problem } => {
                let (need, why) = problem.text(alondra_core::views::Lang::En);
                let option = |id: &str, label: &str| QuestionOption { id: id.into(), label: label.into(), description: None };
                domain.raise_worded_problem_in(
                    Scope::Orchestrator { request_id: request.clone() },
                    id, &unanswered_cause(&problem.question_id), need, why, problem.to_body(),
                    vec![option(UNANSWERED_REMIND, UNANSWERED_REMIND_LABEL), option(UNANSWERED_WITHDRAW, UNANSWERED_WITHDRAW_LABEL)],
                    Some(UNANSWERED_REMIND),
                ).map(drop)
            }
            Step::Withdraw { question } => domain.withdraw_problem(question).map(drop),
        };
        if let Err(error) = done {
            log!("could not settle the orchestrator's inbox ({step:?}): {error:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::{Actor, Command, CommandEnvelope, InteractionPayload, PROTOCOL_VERSION};

    fn run(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(),
            expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    fn request(domain: &DomainRuntime, id: &str, message: bool) {
        run(domain, Actor::User { user_id: "u".into() }, id, Command::CreateRequest {
            request_id: id.into(), body: format!("Do {id}"), auto_merge: false, maintenance: false, message,
        });
    }

    fn closed(domain: &DomainRuntime, id: &str) -> bool {
        domain.state().unwrap().requests[&id.into()].closed
    }

    fn cards(domain: &DomainRuntime, prefix: &str) -> Vec<String> {
        domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with(prefix)).map(|id| id.0.clone()).collect()
    }

    /// 8 October: three messages the orchestrator acted on stayed open with no task for ever.
    #[test]
    fn a_message_acted_on_without_tasks_closes_once_the_orchestrator_is_done() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        request(&domain, "m-1", true);
        settle(&domain);
        settle(&domain);
        assert!(closed(&domain, "m-1"));
        assert!(cards(&domain, UNSPLIT_NOTICE_PREFIX).is_empty(), "a message is never sent back");
    }

    #[test]
    fn a_request_left_without_tasks_goes_back_once_then_becomes_the_users_problem() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        request(&domain, "r-1", false);
        settle(&domain);
        assert_eq!(cards(&domain, UNSPLIT_NOTICE_PREFIX), [format!("{UNSPLIT_NOTICE_PREFIX}r-1-1")]);
        assert!(cards(&domain, UNSPLIT_PROBLEM_PREFIX).is_empty(), "the orchestrator gets one more go first");
        settle(&domain);
        let problem = format!("{UNSPLIT_PROBLEM_PREFIX}r-1-1");
        assert_eq!(cards(&domain, UNSPLIT_PROBLEM_PREFIX), [problem.clone()]);
        settle(&domain);
        assert_eq!(cards(&domain, UNSPLIT_PROBLEM_PREFIX).len(), 1, "an open problem is not raised twice");
        assert!(!closed(&domain, "r-1"), "a request of the user's never closes by itself");
        run(&domain, Actor::User { user_id: "u".into() }, "close-it", Command::AnswerQuestion {
            attachments: vec![], interaction_id: problem.as_str().into(), option_id: UNSPLIT_CLOSE.into(), inputs: Default::default(),
        });
        settle(&domain);
        let state = domain.state().unwrap();
        assert!(state.requests[&"r-1".into()].closed && state.requests[&"r-1".into()].withdrawn);
    }

    #[test]
    fn a_request_with_a_task_or_an_open_question_of_the_orchestrator_is_left_alone() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        request(&domain, "asked", false);
        request(&domain, "split", true);
        run(&domain, Actor::Orchestrator { generation: 0 }, "ask", Command::AskQuestion {
            attachments: vec![], interaction_id: "q-1".into(), scope: Scope::Orchestrator { request_id: Some("asked".into()) },
            recipient: alondra_core::domain::Recipient::User, body: "Which app?".into(), blocking: true,
            options: vec![
                QuestionOption { id: "a".into(), label: "A".into(), description: None },
                QuestionOption { id: "b".into(), label: "B".into(), description: None },
            ],
            card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Decide, "Pick the app"),
        });
        run(&domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None,
            task_id: "t-1".into(), request_id: "split".into(), title: "Task".into(), agent_id: None, review_prs: false,
            dependencies: Default::default(), stack: None, launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
        assert_eq!(steps(&domain.state().unwrap()), []);
        assert!(matches!(domain.state().unwrap().interactions[&"q-1".into()].payload, InteractionPayload::Question { answer: None, .. }));
    }

    fn node_with_task(domain: &DomainRuntime) {
        run(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "Node".into(), at: 0 });
        request(domain, "r", false);
        run(domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None,
            task_id: "t".into(), request_id: "r".into(), title: "Resume the migration".into(), agent_id: Some("a".into()),
            review_prs: false, dependencies: Default::default(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
        run(domain, Actor::Agent { agent_id: "a".into() }, "start", Command::StartTask { task_id: "t".into() });
    }

    fn ask(domain: &DomainRuntime, id: &str) {
        run(domain, Actor::Agent { agent_id: "a".into() }, &format!("ask-{id}"), Command::AskQuestion {
            attachments: vec![], interaction_id: id.into(), scope: Scope::Task { task_id: "t".into() },
            recipient: alondra_core::domain::Recipient::Orchestrator, body: "Create one task per phase of the migration".into(),
            blocking: true, options: vec![
                QuestionOption { id: "done".into(), label: "Done".into(), description: None },
                QuestionOption { id: "no".into(), label: "No".into(), description: None },
            ],
            card: alondra_core::domain::CardSpec::new(alondra_core::domain::CardKind::Decide, "Create the phase tasks").why("The plan needs them"),
        });
    }

    fn answer(domain: &DomainRuntime, actor: Actor, card: &str, option: &str) {
        run(domain, actor, &format!("answer-{card}"), Command::AnswerQuestion {
            attachments: vec![], interaction_id: card.into(), option_id: option.into(), inputs: Default::default(),
        });
    }

    fn open(domain: &DomainRuntime, id: &str) -> bool {
        let state = domain.state().unwrap();
        state.open(&state.interactions[&id.into()])
    }

    /// 16 October: a node asked the orchestrator to create one task per phase of a migration; the
    /// orchestrator created the tasks and never answered, and the node could not deliver its PR.
    #[test]
    fn a_question_left_unanswered_goes_back_once_then_becomes_the_users_problem() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        node_with_task(&domain);
        ask(&domain, "q");
        run(&domain, Actor::Orchestrator { generation: 0 }, "phase-request", Command::CreateRequest {
            request_id: "phases".into(), body: "Phases".into(), auto_merge: false, maintenance: false, message: true,
        });
        settle(&domain);
        let reminder = format!("{UNANSWERED_NOTICE_PREFIX}q-1");
        assert_eq!(cards(&domain, UNANSWERED_NOTICE_PREFIX), [reminder.clone()]);
        assert!(domain.state().unwrap().interactions[&reminder.as_str().into()].body.contains("Create one task per phase"));
        assert!(cards(&domain, UNANSWERED_PROBLEM_PREFIX).is_empty(), "the orchestrator gets one more go first");
        settle(&domain);
        let problem = format!("{UNANSWERED_PROBLEM_PREFIX}q-1");
        assert_eq!(cards(&domain, UNANSWERED_PROBLEM_PREFIX), [problem.clone()]);
        settle(&domain);
        assert_eq!((cards(&domain, UNANSWERED_NOTICE_PREFIX).len(), cards(&domain, UNANSWERED_PROBLEM_PREFIX).len()), (1, 1),
            "nothing more while the user decides");
        answer(&domain, Actor::User { user_id: "u".into() }, &problem, UNANSWERED_REMIND);
        settle(&domain);
        assert_eq!(cards(&domain, UNANSWERED_NOTICE_PREFIX).len(), 2, "reminded again");
        settle(&domain);
        let second = format!("{UNANSWERED_PROBLEM_PREFIX}q-2");
        assert!(cards(&domain, UNANSWERED_PROBLEM_PREFIX).contains(&second));
        answer(&domain, Actor::User { user_id: "u".into() }, &second, UNANSWERED_WITHDRAW);
        settle(&domain);
        let state = domain.state().unwrap();
        assert!(state.interactions[&"q".into()].withdrawn, "the question is withdrawn and the node told");
        assert!(!state.waits_for_orchestrator(&"t".into()));
        settle(&domain);
        assert_eq!(cards(&domain, UNANSWERED_NOTICE_PREFIX).len(), 2);
    }

    /// Found by the property below: the second question's problem withdrew the first one's.
    #[test]
    fn two_questions_left_unanswered_each_get_their_own_problem() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        node_with_task(&domain);
        ask(&domain, "q0");
        run(&domain, Actor::Agent { agent_id: "a".into() }, "ask-q1", Command::AskQuestion {
            attachments: vec![], interaction_id: "q1".into(), scope: Scope::Task { task_id: "t".into() },
            recipient: alondra_core::domain::Recipient::Orchestrator, body: "Which branch?".into(), blocking: false,
            options: vec![
                QuestionOption { id: "main".into(), label: "Main".into(), description: None },
                QuestionOption { id: "other".into(), label: "Other".into(), description: None },
            ],
            card: alondra_core::domain::CardSpec::new(alondra_core::domain::CardKind::Decide, "Pick the branch").why("It decides the base"),
        });
        settle(&domain);
        settle(&domain);
        for question in ["q0", "q1"] {
            assert!(open(&domain, &format!("{UNANSWERED_PROBLEM_PREFIX}{question}-1")), "{question} has its own open problem");
        }
    }

    #[test]
    fn an_answered_question_or_one_of_a_task_that_ended_is_left_alone() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        node_with_task(&domain);
        ask(&domain, "answered");
        answer(&domain, Actor::Orchestrator { generation: 0 }, "answered", "done");
        assert!(!open(&domain, "answered"));
        ask(&domain, "late");
        run(&domain, Actor::User { user_id: "u".into() }, "cancel", Command::CancelTask { task_id: "t".into() });
        assert!(steps(&domain.state().unwrap()).iter().all(|step| !matches!(step, Step::Tell { .. } | Step::RaiseUnanswered { .. })),
            "{:?}", steps(&domain.state().unwrap()));
    }

    #[test]
    fn a_suggestion_left_unsettled_goes_back_once_and_never_to_the_user() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        node_with_task(&domain);
        run(&domain, Actor::Agent { agent_id: "a".into() }, "suggest", Command::AskQuestion {
            attachments: vec![], interaction_id: "p".into(), scope: Scope::Task { task_id: "t".into() },
            recipient: alondra_core::domain::Recipient::Orchestrator, body: "README.md:12".into(), blocking: false,
            options: alondra_core::domain::suggestion_options(),
            card: alondra_core::domain::CardSpec::new(alondra_core::domain::CardKind::Propose, "The README names the old flag").why("Not this task"),
        });
        settle(&domain);
        settle(&domain);
        let reminder = format!("{UNSETTLED_NOTICE_PREFIX}p");
        assert_eq!(cards(&domain, UNSETTLED_NOTICE_PREFIX), [reminder.clone()]);
        assert!(domain.state().unwrap().interactions[&reminder.as_str().into()].body.contains("settle_suggestion"));
        assert!(cards(&domain, "problem-").is_empty(), "a suggestion never becomes the user's card");
    }

    #[derive(Clone, Debug)]
    enum Op {
        Ask(usize),
        Answer(usize),
        Withdraw(usize),
        BatchEnds,
        Decide { question: usize, remind: bool },
    }

    fn op() -> impl proptest::strategy::Strategy<Value = Op> {
        use proptest::prelude::*;
        prop_oneof![
            3 => (0..4usize).prop_map(Op::Ask),
            2 => (0..4usize).prop_map(Op::Answer),
            1 => (0..4usize).prop_map(Op::Withdraw),
            4 => Just(Op::BatchEnds),
            2 => (0..4usize, any::<bool>()).prop_map(|(question, remind)| Op::Decide { question, remind }),
        ]
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig { cases: 48, ..proptest::prelude::ProptestConfig::default() })]

        /// Whatever order nodes ask, the orchestrator answers some, nodes withdraw and the user
        /// decides, no question to the orchestrator is left open with nothing to bring it an answer
        /// when the orchestrator ends its batch.
        #[test]
        fn no_question_to_the_orchestrator_is_left_without_an_answer_when_its_batch_ends(ops in proptest::collection::vec(op(), 1..24)) {
            let home = crate::test_support::Directory::new();
            std::fs::create_dir_all(&home).unwrap();
            let domain = DomainRuntime::open(&home).unwrap();
            node_with_task(&domain);
            for (at, op) in ops.into_iter().enumerate() {
                let state = domain.state().unwrap();
                let question = |n: usize| format!("q{n}");
                let exists = |n: usize| state.interactions.contains_key(&question(n).as_str().into());
                let execute = |actor: Actor, command: Command| {
                    let _ = domain.execute(&actor, &CommandEnvelope {
                        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("op-{at}").into(),
                        expected_revision: state.revision, command,
                    }, None, || Ok(()));
                };
                match op {
                    Op::Ask(n) if !exists(n) => execute(Actor::Agent { agent_id: "a".into() }, Command::AskQuestion {
                        attachments: vec![], interaction_id: question(n).as_str().into(), scope: Scope::Task { task_id: "t".into() },
                        recipient: alondra_core::domain::Recipient::Orchestrator, body: format!("Where does change {n} go?"),
                        blocking: n == 0, options: vec![
                            QuestionOption { id: "done".into(), label: "Done".into(), description: None },
                            QuestionOption { id: "no".into(), label: "No".into(), description: None },
                        ],
                        card: alondra_core::domain::CardSpec::new(alondra_core::domain::CardKind::Decide, "Where does it go?").why("It decides the branch"),
                    }),
                    Op::Answer(n) if exists(n) => execute(Actor::Orchestrator { generation: 0 }, Command::AnswerQuestion {
                        attachments: vec![], interaction_id: question(n).as_str().into(), option_id: "done".into(), inputs: Default::default(),
                    }),
                    Op::Withdraw(n) if exists(n) => execute(Actor::Agent { agent_id: "a".into() }, Command::Withdraw { interaction_id: question(n).as_str().into() }),
                    Op::Decide { question: n, remind } => {
                        let problem = state.interactions.values().find(|i| i.id.0.starts_with(&format!("{UNANSWERED_PROBLEM_PREFIX}{}-", question(n))) && state.open(i));
                        if let Some(problem) = problem {
                            execute(Actor::User { user_id: "u".into() }, Command::AnswerQuestion {
                                attachments: vec![], interaction_id: problem.id.clone(),
                                option_id: if remind { UNANSWERED_REMIND } else { UNANSWERED_WITHDRAW }.into(), inputs: Default::default(),
                            });
                        }
                    }
                    Op::BatchEnds => {
                        let before = domain.state().unwrap().revision;
                        settle(&domain);
                        let after = domain.state().unwrap();
                        for q in after.interactions.values().filter(|i| DomainState::asks_the_orchestrator(i) && after.open(i)) {
                            let problems: Vec<_> = after.interactions.values()
                                .filter(|i| i.id.0.starts_with(&format!("{UNANSWERED_PROBLEM_PREFIX}{}-", q.id)) && after.open(i)).collect();
                            proptest::prop_assert!(problems.len() <= 1, "two open problems for {}", q.id);
                            let reminded = after.interactions.values().any(|i| i.id.0.starts_with(&format!("{UNANSWERED_NOTICE_PREFIX}{}-", q.id))
                                && i.revision > before);
                            proptest::prop_assert!(!problems.is_empty() || reminded,
                                "{} is open with no answer, no reminder and no problem once the batch ended", q.id);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}
