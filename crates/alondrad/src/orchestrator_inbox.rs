//! Nothing waits in the orchestrator. Once it handled everything it was handed, every open request
//! without a task, an answer or a question of its own is settled: a message was acted on and
//! closes, a request of the user's goes back to it once and then becomes the user's Problem.
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{
    Author, DomainState, InteractionPayload, QuestionOption, Request, RequestId, Scope,
};
use alondra_core::orchestrator_cards::{
    UNSPLIT_CLOSE, UNSPLIT_CLOSE_LABEL, UNSPLIT_NOTICE_PREFIX, UNSPLIT_PROBLEM_PREFIX, UNSPLIT_RETRY, UNSPLIT_RETRY_LABEL,
    UnsplitProblem,
};

const UNSPLIT_CAUSE: &str = "request_unsplit";
const BODY_SHOWN: usize = 2_000;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Close { request: RequestId, withdrawn: bool },
    Remind { request: RequestId, id: String, text: String },
    Raise { request: RequestId, id: String, problem: UnsplitProblem },
}

/// What is left in the orchestrator's inbox after it handled everything it was handed.
pub(crate) fn steps(state: &DomainState) -> Vec<Step> {
    let mut steps = vec![];
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
fn numbered<'a>(state: &'a DomainState, prefix: &str, request: &RequestId) -> Vec<(u32, &'a alondra_core::domain::Interaction)> {
    let lead = format!("{prefix}{request}-");
    state.interactions.values()
        .filter_map(|i| i.id.0.strip_prefix(&lead).and_then(|n| n.parse().ok()).map(|n| (n, i)))
        .collect()
}

fn unsplit(state: &DomainState, request: &Request) -> Option<Step> {
    let reminders = numbered(state, UNSPLIT_NOTICE_PREFIX, &request.id).len() as u32;
    let problems = numbered(state, UNSPLIT_PROBLEM_PREFIX, &request.id);
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

/// Carries the steps out; one that fails is tried again on the next pass.
pub(crate) fn settle(domain: &DomainRuntime) {
    let Ok(state) = domain.state() else { return };
    for step in steps(&state) {
        let done = match &step {
            Step::Close { request, withdrawn } => domain.close_request_by_system(request, *withdrawn).map(drop),
            Step::Remind { id, text, .. } => domain.tell_orchestrator(id, text.clone()).map(drop),
            Step::Raise { request, id, problem } => {
                let (need, why) = problem.text(alondra_core::views::Lang::En);
                let option = |id: &str, label: &str| QuestionOption { id: id.into(), label: label.into(), description: None, inputs: vec![] };
                domain.raise_worded_problem_in(
                    Scope::Orchestrator { request_id: Some(request.clone()) },
                    id, UNSPLIT_CAUSE, need, why, problem.to_body(),
                    vec![option(UNSPLIT_RETRY, UNSPLIT_RETRY_LABEL), option(UNSPLIT_CLOSE, UNSPLIT_CLOSE_LABEL)],
                    Some(UNSPLIT_RETRY),
                ).map(drop)
            }
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
                QuestionOption { id: "a".into(), label: "A".into(), description: None, inputs: vec![] },
                QuestionOption { id: "b".into(), label: "B".into(), description: None, inputs: vec![] },
            ],
            card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Decide, "Pick the app"),
        });
        run(&domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask {
            task_id: "t-1".into(), request_id: "split".into(), title: "Task".into(), agent_id: None, review_prs: false,
            dependencies: Default::default(), stack: None, launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
        assert_eq!(steps(&domain.state().unwrap()), []);
        assert!(matches!(domain.state().unwrap().interactions[&"q-1".into()].payload, InteractionPayload::Question { answer: None, .. }));
    }
}
