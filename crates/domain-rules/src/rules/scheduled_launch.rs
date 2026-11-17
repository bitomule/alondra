//! A scheduled run hands its Task to Claude once: while its launch waits, the Task must still be
//! the untouched one the run created. A Task that ended first takes the run's launch with it, so
//! nothing ever waits to start work that is over.
use super::*;

/// Why a scheduled run whose Task the user cancelled before it started never launches.
pub const SCHEDULED_TASK_CANCELLED: &str = "Its task was cancelled before it started";
/// Why a scheduled run whose Task ended some other way before it started never launches.
pub const SCHEDULED_TASK_ENDED: &str = "Its task ended before it started";

impl DomainState {
    pub fn is_scheduled_task(&self, task: &TaskId) -> bool {
        self.tasks.get(task).and_then(|task| self.requests.get(&task.request_id))
            .is_some_and(|request| request.origin == RequestOrigin::Schedule)
    }

    /// Reuses the run's project folder, never the worktree its completion may delete.
    pub fn scheduled_message_plan(&self, source: &TaskId, followup: &TaskId) -> Result<LaunchPlan> {
        let intent = self.schedule.runs.values().find(|run| run.task_id.as_ref() == Some(source))
            .and_then(|run| run.launch.as_ref()).map(|launch| &launch.intent)
            .ok_or_else(|| DomainError::new("launch_plan_required", "The scheduled run has no saved launch folder: the message was not sent"))?;
        let mut plan = LaunchPlan::new(format!("followup/{followup}"), &intent.cwd);
        plan.project = self.task(source)?.project.clone();
        Ok(plan)
    }

    pub(super) fn scheduled_message(&mut self, source: &TaskId, text: &str, effects: &mut Vec<DomainEffect>,
        created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
        let task = self.task(source)?.clone();
        let request_id: RequestId = format!("r-message-{}", self.revision + 1).into();
        let task_id: TaskId = format!("t-message-{}", self.revision + 1).into();
        let launch = self.scheduled_message_plan(source, &task_id)?;
        let request = &self.requests[&task.request_id];
        let result = task.final_presentation.as_ref().and_then(|id| self.interactions.get(id))
            .map(|delivery| delivery.body.as_str()).unwrap_or_default();
        let body = format!("User message about scheduled task {source} “{}”:\n\n{text}\n\nOriginal request:\n{}\n\nLatest result:\n{result}", task.title, request.body);
        let auto_merge = request.auto_merge;
        self.apply(&Actor::Orchestrator { generation: self.orchestrator_generation }, &Command::CreateRequest {
            request_id: request_id.clone(), body, auto_merge, maintenance: false, message: false,
        }, effects, created, lifecycles)?;
        let request = self.requests.get_mut(&request_id).unwrap();
        request.title = Some(clip(&task.title, REQUEST_TITLE_LIMIT));
        request.origin = RequestOrigin::User;
        self.apply(&Actor::Orchestrator { generation: self.orchestrator_generation }, &Command::CreateTask {
            task_id, request_id, title: task.title, agent_id: None, review_prs: false,
            dependencies: BTreeSet::new(), stack: None, launch: Some(launch), plan_items: None, continues: None,
        }, effects, created, lifecycles)
    }

    /// Whether the launch of `run_id` may be claimed now: the run waits to launch and its Task
    /// has not started, has no node, waits for nothing and is not over.
    pub fn scheduled_launch_guard(&self, run_id: &ScheduleRunId) -> Result<&TaskId> {
        let run = self.schedule.runs.get(run_id)
            .filter(|run| run.task_id.is_some())
            .ok_or_else(|| DomainError::new("conflict", "Scheduled Run has no Task"))?;
        let task_id = run.task_id.as_ref().expect("filtered above");
        let task = self.tasks.get(task_id)
            .ok_or_else(|| DomainError::new("conflict", "Scheduled Task is missing"))?;
        if task.over()
            || task.agent_id.is_some()
            || task.state != TaskState::NotStarted
            || task.started
            || !task.dependencies.is_empty()
            || task.stack.is_some()
        {
            return fail("conflict", "Scheduled Task is no longer ready to launch");
        }
        if run.state != ScheduleRunState::Sent
            || run.launch.as_ref().is_none_or(|launch| launch.state != ScheduleLaunchState::Pending)
        {
            return fail("conflict", "Schedule launch is not waiting");
        }
        Ok(task_id)
    }

    /// The runs whose launch still waits although their Task is over: each will never launch.
    pub fn scheduled_launches_of_ended_tasks(&self) -> Vec<(ScheduleRunId, &'static str)> {
        self.schedule.runs.values()
            .filter(|run| run.state == ScheduleRunState::Sent
                && run.launch.as_ref().is_some_and(|launch| launch.state == ScheduleLaunchState::Pending))
            .filter_map(|run| {
                let task = self.tasks.get(run.task_id.as_ref()?)?;
                task.over().then(|| (run.id.clone(), if task.cancelled() { SCHEDULED_TASK_CANCELLED } else { SCHEDULED_TASK_ENDED }))
            })
            .collect()
    }

    /// The Task of a scheduled run that never launched is cancelled: its run fails with it, so the
    /// launch leaves every queue.
    pub(super) fn settle_scheduled_launch(&mut self, task_id: &TaskId) -> Result<()> {
        let waiting = self.schedule.runs.values()
            .find(|run| run.task_id.as_ref() == Some(task_id) && run.state == ScheduleRunState::Sent
                && run.launch.as_ref().is_some_and(|launch| launch.state == ScheduleLaunchState::Pending))
            .map(|run| (run.id.clone(), run.finished_at.unwrap_or_default()));
        let Some((run_id, sent_at)) = waiting else { return Ok(()) };
        self.schedule.record_failed(&run_id, SCHEDULED_TASK_CANCELLED.into(), sent_at)
    }
}
