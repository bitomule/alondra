//! A scheduled run hands its Task to Claude once: while its launch waits, the Task must still be
//! the untouched one the run created. A Task that ended first takes the run's launch with it, so
//! nothing ever waits to start work that is over.
use super::*;

pub(crate) fn scheduled_message(domain: &mut DomainState, source: &TaskId, text: &str, effects: &mut Vec<DomainEffect>,
    created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
    let task = domain.task(source)?.clone();
    let request_id: RequestId = format!("r-message-{}", domain.revision + 1).into();
    let task_id: TaskId = format!("t-message-{}", domain.revision + 1).into();
    let launch = domain.scheduled_message_plan(source, &task_id)?;
    let request = &domain.requests[&task.request_id];
    let result = task.final_presentation.as_ref().and_then(|id| domain.interactions.get(id))
        .map(|delivery| delivery.body.as_str()).unwrap_or_default();
    let body = format!("User message about scheduled task {source} “{}”:\n\n{text}\n\nOriginal request:\n{}\n\nLatest result:\n{result}", task.title, request.body);
    let auto_merge = request.auto_merge;
    apply(domain, &Actor::Orchestrator { generation: domain.orchestrator_generation }, &Command::CreateRequest {
        request_id: request_id.clone(), body, auto_merge, maintenance: false, message: false,
    }, effects, created, lifecycles)?;
    let request = domain.requests.get_mut(&request_id).unwrap();
    request.title = Some(clip(&task.title, REQUEST_TITLE_LIMIT));
    request.origin = RequestOrigin::User;
    apply(domain, &Actor::Orchestrator { generation: domain.orchestrator_generation }, &Command::CreateTask { priority: Default::default(),
        task_id, request_id, title: task.title, agent_id: None, review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(launch), plan_items: None, continues: None,
    }, effects, created, lifecycles)
}
