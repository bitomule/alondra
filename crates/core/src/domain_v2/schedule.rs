//! Pure scheduled-work state. Calendar arithmetic and external launching belong to adapters.
//! The v2 repository must commit these transitions with Request/Task and receipts before use.
use super::{AgentId, DomainError, MAX_REVISION, RequestId, ScheduleId, ScheduleRunId, TaskId};
use crate::options::Choice;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleFrequency {
    Daily,
    Weekly,
    EveryHours,
    EveryDays,
    EveryMonths,
    Once,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: ScheduleId,
    pub title: String,
    pub text: String,
    pub project: Option<String>,
    pub options: BTreeMap<String, Choice>,
    pub disallowed_tools: Vec<String>,
    pub frequency: ScheduleFrequency,
    pub hour: u8,
    pub minute: u8,
    pub weekday: Option<u8>,
    pub interval: Option<u32>,
    pub start_at: Option<i64>,
    pub enabled: bool,
    pub next_run: Option<i64>,
    pub revision: i64,
    pub deleted_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleDefinitionSnapshot {
    pub title: String,
    pub text: String,
    pub project: Option<String>,
    pub options: BTreeMap<String, Choice>,
    pub disallowed_tools: Vec<String>,
}

/// Technical new-node launch recorded with the Run, Request, and Task before
/// any external process is invoked. `options_json` is the resolved launch
/// configuration captured at that moment, not a future options.toml lookup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleLaunchIntent {
    pub name: String,
    pub cwd: String,
    pub prompt: String,
    pub options_json: String,
}

impl ScheduleLaunchIntent {
    pub fn validate(&self, definition: &ScheduleDefinitionSnapshot) -> Result<(), DomainError> {
        if !self.name.starts_with("scheduled/")
            || self.name.len() > 128
            || !Path::new(&self.cwd).is_absolute()
            || self.prompt != definition.text
        {
            return Err(invalid("invalid scheduled launch intent"));
        }
        let options: crate::options::Resolved = serde_json::from_str(&self.options_json)
            .map_err(|_| invalid("invalid resolved launch options"))?;
        if options.project != definition.project {
            return Err(invalid("launch project differs from claimed Schedule"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleLaunchState {
    Pending,
    Dispatching,
    Handed,
    Failed,
    Uncertain,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleLaunch {
    pub intent: ScheduleLaunchIntent,
    pub state: ScheduleLaunchState,
    pub claimed_at: Option<i64>,
    pub finished_at: Option<i64>,
    /// The short id `claude --bg` printed; the terminal `claude attach` opens.
    pub session_id: Option<String>,
    /// The Agent the handed session runs as; the Task is assigned to it.
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    pub error: Option<String>,
}

impl ScheduleLaunch {
    fn validate(&self, definition: &ScheduleDefinitionSnapshot) -> Result<(), DomainError> {
        self.intent.validate(definition)?;
        if self.claimed_at.is_some_and(|at| at < 0)
            || self.finished_at.is_some_and(|at| at < 0)
            || self.claimed_at.zip(self.finished_at).is_some_and(|(claim, finish)| finish < claim)
            || self.session_id.as_ref().is_some_and(|id| id.is_empty() || id.len() > 256)
            || self.error.as_ref().is_some_and(|error| error.is_empty())
        {
            return Err(invalid("invalid scheduled launch result"));
        }
        let valid = match self.state {
            ScheduleLaunchState::Pending => self.claimed_at.is_none()
                && self.finished_at.is_none() && self.session_id.is_none() && self.error.is_none(),
            ScheduleLaunchState::Dispatching => self.claimed_at.is_some()
                && self.finished_at.is_none() && self.session_id.is_none() && self.error.is_none(),
            ScheduleLaunchState::Handed => self.claimed_at.is_some()
                && self.finished_at.is_some() && self.error.is_none(),
            ScheduleLaunchState::Failed => self.finished_at.is_some()
                && self.session_id.is_none() && self.error.is_some(),
            ScheduleLaunchState::Uncertain => self.claimed_at.is_some()
                && self.finished_at.is_none() && self.session_id.is_none() && self.error.is_some(),
        };
        if !valid { return Err(invalid("invalid scheduled launch lifecycle")); }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleRunOrigin {
    Due,
    Manual,
    LegacyLastObserved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleRunState {
    Launching,
    Sent,
    Failed,
    Interrupted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleRun {
    pub id: ScheduleRunId,
    pub schedule_id: ScheduleId,
    pub origin: ScheduleRunOrigin,
    pub due_at: Option<i64>,
    pub claimed_at: Option<i64>,
    /// Schedule revision immediately after claim. NULL only for legacy last-observed runs.
    pub claim_revision: Option<i64>,
    pub finished_at: Option<i64>,
    pub state: ScheduleRunState,
    pub request_id: Option<RequestId>,
    pub task_id: Option<TaskId>,
    pub error: Option<String>,
    pub definition_snapshot: Option<ScheduleDefinitionSnapshot>,
    /// Present only for a v2 run whose Request and Task were atomically saved.
    /// Imported legacy runs cannot acquire an invented launch intent.
    #[serde(default)]
    pub launch: Option<ScheduleLaunch>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleState {
    pub schedules: BTreeMap<ScheduleId, Schedule>,
    pub runs: BTreeMap<ScheduleRunId, ScheduleRun>,
}

fn invalid(message: &str) -> DomainError {
    DomainError::new("invalid_input", message)
}

impl Schedule {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.id.0.is_empty()
            || self.revision < 1
            || self.revision > MAX_REVISION as i64
            || self.title.trim().is_empty()
            || self.title.len() > 120
            || self.text.trim().is_empty()
            || self.text.len() > 20_000
        {
            return Err(invalid(
                "invalid schedule identity, revision, title, or mission",
            ));
        }
        if self.hour > 23
            || self.minute > 59
            || self
                .project
                .as_ref()
                .is_some_and(|project| project.trim().is_empty())
        {
            return Err(invalid("invalid schedule time or project"));
        }
        if self.frequency == ScheduleFrequency::Weekly && !matches!(self.weekday, Some(0..=6)) {
            return Err(invalid("weekly schedule needs a weekday"));
        }
        if matches!(
            self.frequency,
            ScheduleFrequency::EveryHours
                | ScheduleFrequency::EveryDays
                | ScheduleFrequency::EveryMonths
        ) && (!matches!(self.interval, Some(1..=365)) || self.start_at.is_none())
        {
            return Err(invalid(
                "interval schedule needs an anchor and interval 1–365",
            ));
        }
        if self.frequency == ScheduleFrequency::Once && self.start_at.is_none() {
            return Err(invalid("one-time schedule needs a start date"));
        }
        if matches!(
            self.frequency,
            ScheduleFrequency::EveryHours
                | ScheduleFrequency::EveryDays
                | ScheduleFrequency::EveryMonths
                | ScheduleFrequency::Once
        ) && self
            .start_at
            .is_some_and(|at| !(0..=4_102_444_800_000).contains(&at))
            || self.next_run.is_some_and(|at| at < 0)
            || self.deleted_at.is_some_and(|at| at < 0)
        {
            return Err(invalid("invalid schedule timestamp"));
        }
        if (!self.enabled && self.next_run.is_some()) || (self.deleted_at.is_some() && self.enabled)
        {
            return Err(invalid("disabled or deleted schedule cannot be due"));
        }
        if self.disallowed_tools.len() > 100
            || self.disallowed_tools.iter().any(|pattern| {
                pattern.is_empty()
                    || pattern.len() > 200
                    || pattern.chars().any(char::is_whitespace)
                    || pattern.starts_with('-')
            })
        {
            return Err(invalid("invalid disallowed tool pattern"));
        }
        Ok(())
    }

    fn snapshot(&self) -> ScheduleDefinitionSnapshot {
        ScheduleDefinitionSnapshot {
            title: self.title.clone(),
            text: self.text.clone(),
            project: self.project.clone(),
            options: self.options.clone(),
            disallowed_tools: self.disallowed_tools.clone(),
        }
    }
}

impl ScheduleRun {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.id.0.is_empty() || self.schedule_id.0.is_empty() {
            return Err(invalid("invalid schedule run identity"));
        }
        if self.due_at.is_some_and(|at| at < 0)
            || self.claimed_at.is_some_and(|at| at < 0)
            || self.finished_at.is_some_and(|at| at < 0)
            || self.request_id.as_ref().is_some_and(|id| id.0.is_empty())
            || self.task_id.as_ref().is_some_and(|id| id.0.is_empty())
        {
            return Err(invalid("invalid schedule run timestamp or linked identity"));
        }
        if self.origin == ScheduleRunOrigin::Due && self.due_at.is_none()
            || self.origin != ScheduleRunOrigin::Due && self.due_at.is_some()
        {
            return Err(invalid("invalid due time for schedule run origin"));
        }
        if self.origin == ScheduleRunOrigin::LegacyLastObserved {
            if self.claimed_at.is_some()
                || self.claim_revision.is_some()
                || self.definition_snapshot.is_some()
                || self.launch.is_some()
            {
                return Err(invalid(
                    "legacy last run cannot invent claim or definition snapshot",
                ));
            }
        } else if self.claimed_at.is_none()
            || self
                .claim_revision
                .is_none_or(|revision| revision < 1 || revision > MAX_REVISION as i64)
            || self.definition_snapshot.is_none()
        {
            return Err(invalid(
                "new schedule run needs claim and definition snapshot",
            ));
        }
        if self.state == ScheduleRunState::Sent
            && (self.request_id.is_none()
                || self.task_id.is_none()
                || self.finished_at.is_none()
                || self.error.is_some())
        {
            return Err(invalid("sent run needs Request, Task, and finish time"));
        }
        if self.request_id.is_some() != self.task_id.is_some() {
            return Err(invalid(
                "schedule run Request and Task must be linked together",
            ));
        }
        if self.origin != ScheduleRunOrigin::LegacyLastObserved
            && self.request_id.is_some() && self.launch.is_none() {
            return Err(invalid("new sent run needs a durable launch intent"));
        }
        if self.state == ScheduleRunState::Launching
            && (self.finished_at.is_some() || self.error.is_some() || self.request_id.is_some())
        {
            return Err(invalid("launching run cannot have a result"));
        }
        if self.state == ScheduleRunState::Failed && self.error.as_deref().is_none_or(str::is_empty)
            || self.state == ScheduleRunState::Interrupted
                && self.error.as_deref().is_none_or(str::is_empty)
        {
            return Err(invalid("failed or interrupted run needs a reason"));
        }
        if let Some(launch) = &self.launch {
            if self.request_id.is_none() || self.task_id.is_none() {
                return Err(invalid("launch intent needs Request and Task links"));
            }
            launch.validate(self.definition_snapshot.as_ref().ok_or_else(|| invalid("launch intent needs claim snapshot"))?)?;
            if self.state == ScheduleRunState::Sent && launch.state == ScheduleLaunchState::Failed
                || self.state == ScheduleRunState::Failed && launch.state != ScheduleLaunchState::Failed
                || self.state == ScheduleRunState::Failed && launch.error != self.error
                || !matches!(self.state, ScheduleRunState::Sent | ScheduleRunState::Failed)
            {
                return Err(invalid("launch state disagrees with Run result"));
            }
        }
        Ok(())
    }
}

impl ScheduleState {
    pub fn validate(&self) -> Result<(), DomainError> {
        for (id, schedule) in &self.schedules {
            schedule.validate()?;
            if id != &schedule.id {
                return Err(invalid("schedule map key mismatch"));
            }
        }
        let mut requests = BTreeSet::new();
        let mut tasks = BTreeSet::new();
        let mut launching = BTreeSet::new();
        let mut claim_revisions = BTreeSet::new();
        for (id, run) in &self.runs {
            run.validate()?;
            if id != &run.id || !self.schedules.contains_key(&run.schedule_id) {
                return Err(invalid("schedule run has missing or mismatched Schedule"));
            }
            if run.state == ScheduleRunState::Launching && !launching.insert(&run.schedule_id) {
                return Err(invalid("schedule has two active claims"));
            }
            if run.claim_revision.is_some_and(|revision| {
                revision > self.schedules[&run.schedule_id].revision
                    || !claim_revisions.insert((&run.schedule_id, revision))
            }) {
                return Err(invalid("invalid or duplicate schedule claim revision"));
            }
            if run
                .request_id
                .as_ref()
                .is_some_and(|id| !requests.insert(id))
                || run.task_id.as_ref().is_some_and(|id| !tasks.insert(id))
            {
                return Err(invalid("Request or Task belongs to multiple schedule runs"));
            }
        }
        Ok(())
    }

    /// `next_after_now` is computed by the existing local-calendar adapter using `now`,
    /// never by adding one interval to the overdue slot. For Once it may be None.
    pub fn claim(
        &mut self,
        schedule_id: &ScheduleId,
        run_id: ScheduleRunId,
        origin: ScheduleRunOrigin,
        now: i64,
        next_after_now: Option<i64>,
    ) -> Result<ScheduleRun, DomainError> {
        if origin == ScheduleRunOrigin::LegacyLastObserved || now < 0 || run_id.0.is_empty() {
            return Err(invalid("invalid live schedule claim"));
        }
        if self.runs.contains_key(&run_id) {
            return Err(DomainError::new(
                "idempotency_conflict",
                "schedule run ID already exists",
            ));
        }
        if self
            .runs
            .values()
            .any(|run| &run.schedule_id == schedule_id && run.state == ScheduleRunState::Launching)
        {
            return Err(DomainError::new(
                "conflict",
                "schedule run already launching",
            ));
        }
        let schedule = self
            .schedules
            .get_mut(schedule_id)
            .ok_or_else(|| DomainError::new("not_found", "schedule not found"))?;
        schedule.validate()?;
        if schedule.deleted_at.is_some() {
            return Err(DomainError::new("not_found", "schedule was deleted"));
        }
        if origin == ScheduleRunOrigin::Due
            && (!schedule.enabled || schedule.next_run.is_none_or(|at| at > now))
        {
            return Err(DomainError::new("conflict", "schedule is not due"));
        }
        if schedule.enabled {
            if schedule.frequency != ScheduleFrequency::Once && next_after_now.is_none()
                || next_after_now.is_some_and(|next| next <= now)
            {
                return Err(invalid("next run must be after claim time"));
            }
            if schedule.frequency == ScheduleFrequency::Once
                && next_after_now != schedule.start_at.filter(|at| *at > now)
            {
                return Err(invalid(
                    "one-time schedule next run must match its future date",
                ));
            }
        } else if next_after_now.is_some() {
            return Err(invalid("disabled schedule has no next run"));
        }
        if schedule.revision >= MAX_REVISION as i64 {
            return Err(invalid("schedule revision overflow"));
        }
        schedule.revision += 1;
        let run = ScheduleRun {
            id: run_id.clone(),
            schedule_id: schedule_id.clone(),
            origin,
            due_at: (origin == ScheduleRunOrigin::Due)
                .then_some(schedule.next_run)
                .flatten(),
            claimed_at: Some(now),
            claim_revision: Some(schedule.revision),
            finished_at: None,
            state: ScheduleRunState::Launching,
            request_id: None,
            task_id: None,
            error: None,
            definition_snapshot: Some(schedule.snapshot()),
            launch: None,
        };
        schedule.next_run = next_after_now;
        self.runs.insert(run_id, run.clone());
        Ok(run)
    }

    /// Called only once Request and Task have been committed with this Run in the same
    /// repository transaction. This does not mean the external Claude launch succeeded.
    pub fn record_sent(
        &mut self,
        run_id: &ScheduleRunId,
        request_id: RequestId,
        task_id: TaskId,
        intent: ScheduleLaunchIntent,
        finished_at: i64,
    ) -> Result<(), DomainError> {
        if request_id.0.is_empty() || task_id.0.is_empty() || finished_at < 0 {
            return Err(invalid("invalid Request, Task, or finish time"));
        }
        if self.runs.values().any(|run| {
            &run.id != run_id
                && (run.request_id.as_ref() == Some(&request_id)
                    || run.task_id.as_ref() == Some(&task_id))
        }) {
            return Err(DomainError::new(
                "conflict",
                "Request or Task belongs to another run",
            ));
        }
        let run = self
            .runs
            .get(run_id)
            .ok_or_else(|| DomainError::new("not_found", "schedule run not found"))?;
        if run.state != ScheduleRunState::Launching {
            return Err(DomainError::new(
                "conflict",
                "schedule run is not launching",
            ));
        }
        let schedule = self
            .schedules
            .get_mut(&run.schedule_id)
            .ok_or_else(|| invalid("schedule run has no Schedule"))?;
        if schedule.revision >= MAX_REVISION as i64 {
            return Err(invalid("schedule revision overflow"));
        }
        schedule.revision += 1;
        let run = self.runs.get_mut(run_id).expect("run checked above");
        run.state = ScheduleRunState::Sent;
        run.finished_at = Some(finished_at);
        run.request_id = Some(request_id);
        run.task_id = Some(task_id);
        run.launch = Some(ScheduleLaunch {
            intent,
            state: ScheduleLaunchState::Pending,
            claimed_at: None,
            finished_at: None,
            session_id: None,
            agent_id: None,
            error: None,
        });
        Ok(())
    }

    /// One durable claim must precede the external Claude call. A second
    /// dispatcher, including one after restart, cannot claim this intent.
    pub fn claim_launch(&mut self, run_id: &ScheduleRunId, now: i64) -> Result<(), DomainError> {
        if now < 0 { return Err(invalid("invalid launch claim time")); }
        let run = self.runs.get_mut(run_id)
            .ok_or_else(|| DomainError::new("not_found", "Schedule run not found"))?;
        if run.state != ScheduleRunState::Sent { return Err(DomainError::new("conflict", "Run is not sent")); }
        let launch = run.launch.as_mut()
            .ok_or_else(|| DomainError::new("conflict", "Run has no v2 launch intent"))?;
        if launch.state != ScheduleLaunchState::Pending {
            return Err(DomainError::new("conflict", "Schedule launch was already claimed"));
        }
        launch.state = ScheduleLaunchState::Dispatching;
        launch.claimed_at = Some(now);
        Ok(())
    }

    pub fn record_launch_handed(
        &mut self, run_id: &ScheduleRunId, session_id: Option<String>, agent_id: Option<AgentId>,
        finished_at: i64,
    ) -> Result<(), DomainError> {
        if finished_at < 0 || session_id.as_ref().is_some_and(|id| id.is_empty() || id.len() > 256) {
            return Err(invalid("invalid launch handoff result"));
        }
        let run = self.runs.get_mut(run_id)
            .ok_or_else(|| DomainError::new("not_found", "Schedule run not found"))?;
        let launch = run.launch.as_mut()
            .ok_or_else(|| DomainError::new("conflict", "Run has no v2 launch intent"))?;
        if run.state != ScheduleRunState::Sent || launch.state != ScheduleLaunchState::Dispatching {
            return Err(DomainError::new("conflict", "Schedule launch was not dispatching"));
        }
        if launch.claimed_at.is_some_and(|claim| finished_at < claim) {
            return Err(invalid("launch finish precedes its claim"));
        }
        launch.state = ScheduleLaunchState::Handed;
        launch.finished_at = Some(finished_at);
        launch.session_id = session_id;
        launch.agent_id = agent_id;
        Ok(())
    }

    /// A later launcher failure changes the last visible result, but keeps the
    /// Request/Task link and the original dispatch finish time.
    pub fn record_failed(
        &mut self,
        run_id: &ScheduleRunId,
        reason: String,
        finished_at: i64,
    ) -> Result<(), DomainError> {
        if reason.is_empty() || finished_at < 0 {
            return Err(invalid("invalid failure result"));
        }
        let run = self
            .runs
            .get(run_id)
            .ok_or_else(|| DomainError::new("not_found", "schedule run not found"))?;
        if !matches!(
            run.state,
            ScheduleRunState::Launching | ScheduleRunState::Sent
        ) {
            return Err(DomainError::new(
                "conflict",
                "schedule run already resolved",
            ));
        }
        if run.launch.as_ref().is_some_and(|launch| !matches!(launch.state,
            ScheduleLaunchState::Pending | ScheduleLaunchState::Dispatching)) {
            return Err(DomainError::new("conflict", "Schedule launch outcome is already final or uncertain"));
        }
        if run.launch.as_ref().and_then(|launch| launch.claimed_at)
            .is_some_and(|claim| finished_at < claim) {
            return Err(invalid("launch failure precedes its claim"));
        }
        let newer_run_exists = self.runs.values().any(|other| {
            other.schedule_id == run.schedule_id
                && other.id != run.id
                && other.claim_revision > run.claim_revision
        });
        if !newer_run_exists {
            let schedule = self
                .schedules
                .get_mut(&run.schedule_id)
                .ok_or_else(|| invalid("schedule run has no Schedule"))?;
            if schedule.revision >= MAX_REVISION as i64 {
                return Err(invalid("schedule revision overflow"));
            }
            schedule.revision += 1;
        }
        let run = self.runs.get_mut(run_id).expect("run checked above");
        match run.state {
            ScheduleRunState::Launching => run.finished_at = Some(finished_at),
            ScheduleRunState::Sent => (),
            _ => unreachable!("checked above"),
        }
        run.state = ScheduleRunState::Failed;
        run.error = Some(reason);
        if let Some(launch) = run.launch.as_mut() {
            launch.state = ScheduleLaunchState::Failed;
            launch.finished_at = Some(finished_at);
            launch.error = run.error.clone();
        }
        Ok(())
    }

    /// Startup recovery never restores a consumed due slot or invents a Request.
    pub fn interrupt_on_restart(&mut self) {
        for launch in self.runs.values_mut().filter_map(|run| run.launch.as_mut()) {
            if launch.state == ScheduleLaunchState::Dispatching {
                launch.state = ScheduleLaunchState::Uncertain;
                launch.error = Some("Alondra stopped after claiming the Claude launch".into());
            }
        }
        for run in self
            .runs
            .values_mut()
            .filter(|run| run.state == ScheduleRunState::Launching)
        {
            run.state = ScheduleRunState::Interrupted;
            run.error = Some("Alondra stopped while sending this run".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(enabled: bool, next_run: Option<i64>) -> Schedule {
        Schedule {
            id: "s".into(),
            title: "Review".into(),
            text: "Review project".into(),
            project: Some("alondra".into()),
            options: BTreeMap::new(),
            disallowed_tools: vec!["mcp__example__write-*".into()],
            frequency: ScheduleFrequency::Daily,
            hour: 9,
            minute: 0,
            weekday: None,
            interval: None,
            start_at: None,
            enabled,
            next_run,
            revision: 1,
            deleted_at: None,
        }
    }

    fn state(enabled: bool, next_run: Option<i64>) -> ScheduleState {
        let mut state = ScheduleState::default();
        state
            .schedules
            .insert("s".into(), schedule(enabled, next_run));
        state
    }

    fn intent() -> ScheduleLaunchIntent {
        ScheduleLaunchIntent {
            name: "scheduled/run-1".into(),
            cwd: "/synthetic/Projects/alondra".into(),
            prompt: "Review project".into(),
            options_json: serde_json::to_string(&crate::options::OptionsFile::default()
                .resolve(Some("alondra"), &BTreeMap::new()).unwrap()).unwrap(),
        }
    }

    #[test]
    fn due_claim_consumes_one_observed_slot_and_restart_does_not_replay_it() {
        let mut state = state(true, Some(100));
        let run = state
            .claim(
                &"s".into(),
                "run-1".into(),
                ScheduleRunOrigin::Due,
                105,
                Some(200),
            )
            .unwrap();
        assert_eq!(run.due_at, Some(100));
        assert_eq!(run.state, ScheduleRunState::Launching);
        assert_eq!(state.schedules[&ScheduleId::from("s")].next_run, Some(200));
        assert!(
            state
                .claim(
                    &"s".into(),
                    "run-2".into(),
                    ScheduleRunOrigin::Due,
                    105,
                    Some(200)
                )
                .is_err()
        );
        let revision = state.schedules[&ScheduleId::from("s")].revision;
        state.interrupt_on_restart();
        assert_eq!(
            state.runs[&ScheduleRunId::from("run-1")].state,
            ScheduleRunState::Interrupted
        );
        assert_eq!(state.schedules[&ScheduleId::from("s")].revision, revision);
        assert_eq!(state.schedules[&ScheduleId::from("s")].next_run, Some(200));
        assert!(
            state
                .claim(
                    &"s".into(),
                    "run-3".into(),
                    ScheduleRunOrigin::Due,
                    105,
                    Some(200)
                )
                .is_err()
        );
        state.validate().unwrap();
    }

    #[test]
    fn manual_disabled_can_run_again_after_finish_without_completing_task() {
        let mut state = state(false, None);
        let first = state
            .claim(
                &"s".into(),
                "run-1".into(),
                ScheduleRunOrigin::Manual,
                100,
                None,
            )
            .unwrap();
        assert!(first.due_at.is_none());
        assert!(
            state
                .claim(
                    &"s".into(),
                    "run-2".into(),
                    ScheduleRunOrigin::Manual,
                    101,
                    None
                )
                .is_err()
        );
        state
            .record_sent(&"run-1".into(), "r1".into(), "t1".into(), intent(), 102)
            .unwrap();
        assert_eq!(
            state.runs[&ScheduleRunId::from("run-1")].state,
            ScheduleRunState::Sent
        );
        assert_eq!(state.schedules[&ScheduleId::from("s")].revision, 3);
        assert!(
            state
                .record_sent(&"run-1".into(), "r1".into(), "t1".into(), intent(), 103)
                .is_err()
        );
        let second = state
            .claim(
                &"s".into(),
                "run-2".into(),
                ScheduleRunOrigin::Manual,
                104,
                None,
            )
            .unwrap();
        assert_eq!(second.state, ScheduleRunState::Launching);
        assert!(
            state
                .record_sent(&"run-2".into(), "r1".into(), "t2".into(), intent(), 105)
                .is_err()
        );
        state
            .record_sent(&"run-2".into(), "r2".into(), "t2".into(), intent(), 105)
            .unwrap();
        assert_eq!(state.schedules[&ScheduleId::from("s")].revision, 5);
        state.validate().unwrap();
    }

    #[test]
    fn technical_launch_is_one_shot_and_restart_leaves_dispatch_uncertain() {
        let mut state = state(false, None);
        state.claim(&"s".into(), "first".into(), ScheduleRunOrigin::Manual, 100, None).unwrap();
        state.record_sent(&"first".into(), "r1".into(), "t1".into(), intent(), 101).unwrap();
        assert_eq!(state.runs[&ScheduleRunId::from("first")].launch.as_ref().unwrap().state,
            ScheduleLaunchState::Pending);
        state.claim_launch(&"first".into(), 102).unwrap();
        assert!(state.claim_launch(&"first".into(), 103).is_err());
        state.interrupt_on_restart();
        let run = &state.runs[&ScheduleRunId::from("first")];
        assert_eq!(run.state, ScheduleRunState::Sent);
        assert_eq!(run.launch.as_ref().unwrap().state, ScheduleLaunchState::Uncertain);
        assert!(state.claim_launch(&"first".into(), 104).is_err());
        assert!(state.record_failed(&"first".into(), "guess".into(), 104).is_err());
        state.validate().unwrap();

        state.claim(&"s".into(), "second".into(), ScheduleRunOrigin::Manual, 105, None).unwrap();
        state.record_sent(&"second".into(), "r2".into(), "t2".into(), intent(), 106).unwrap();
        state.interrupt_on_restart();
        assert_eq!(state.runs[&ScheduleRunId::from("second")].launch.as_ref().unwrap().state,
            ScheduleLaunchState::Pending);
        state.claim_launch(&"second".into(), 107).unwrap();
        state.record_launch_handed(&"second".into(), Some("session-1".into()), None, 108).unwrap();
        assert!(state.record_failed(&"second".into(), "late".into(), 109).is_err());
        state.validate().unwrap();
    }

    #[test]
    fn returned_launcher_failure_changes_run_and_launch_together() {
        let mut state = state(false, None);
        state.claim(&"s".into(), "failed".into(), ScheduleRunOrigin::Manual, 100, None).unwrap();
        state.record_sent(&"failed".into(), "request".into(), "task".into(), intent(), 101).unwrap();
        state.claim_launch(&"failed".into(), 102).unwrap();
        state.record_failed(&"failed".into(), "Claude refused".into(), 103).unwrap();
        let run = &state.runs[&ScheduleRunId::from("failed")];
        assert_eq!(run.state, ScheduleRunState::Failed);
        let launch = run.launch.as_ref().unwrap();
        assert_eq!(launch.state, ScheduleLaunchState::Failed);
        assert_eq!(launch.error.as_deref(), Some("Claude refused"));
        state.validate().unwrap();
        state.runs.get_mut(&ScheduleRunId::from("failed")).unwrap()
            .launch.as_mut().unwrap().error = Some("different failure".into());
        assert!(state.validate().is_err());
    }

    #[test]
    fn failure_before_and_after_request_preserves_truth() {
        let mut state = state(true, Some(100));
        state
            .claim(
                &"s".into(),
                "early".into(),
                ScheduleRunOrigin::Due,
                100,
                Some(200),
            )
            .unwrap();
        state
            .record_failed(&"early".into(), "project missing".into(), 101)
            .unwrap();
        assert_eq!(state.schedules[&ScheduleId::from("s")].revision, 3);
        let early = &state.runs[&ScheduleRunId::from("early")];
        assert_eq!(early.state, ScheduleRunState::Failed);
        assert!(early.request_id.is_none());
        assert_eq!(state.schedules[&ScheduleId::from("s")].next_run, Some(200));

        state
            .claim(
                &"s".into(),
                "late".into(),
                ScheduleRunOrigin::Due,
                200,
                Some(300),
            )
            .unwrap();
        state
            .record_sent(&"late".into(), "request".into(), "task".into(), intent(), 201)
            .unwrap();
        state
            .record_failed(&"late".into(), "launcher unavailable".into(), 202)
            .unwrap();
        assert_eq!(state.schedules[&ScheduleId::from("s")].revision, 6);
        let late = &state.runs[&ScheduleRunId::from("late")];
        assert_eq!(late.state, ScheduleRunState::Failed);
        assert_eq!(late.request_id, Some("request".into()));
        assert_eq!(late.task_id, Some("task".into()));
        assert_eq!(late.finished_at, Some(201));
        state.validate().unwrap();
    }

    #[test]
    fn imported_last_run_has_no_fabricated_claim_or_definition() {
        let mut state = state(true, Some(200));
        state.runs.insert(
            "legacy".into(),
            ScheduleRun {
                id: "legacy".into(),
                schedule_id: "s".into(),
                origin: ScheduleRunOrigin::LegacyLastObserved,
                due_at: None,
                claimed_at: None,
                claim_revision: None,
                finished_at: Some(100),
                state: ScheduleRunState::Sent,
                request_id: Some("r".into()),
                task_id: Some("t".into()),
                error: None,
                definition_snapshot: None,
                launch: None,
            },
        );
        state.validate().unwrap();
        state
            .runs
            .get_mut(&ScheduleRunId::from("legacy"))
            .unwrap()
            .error = Some("not sent".into());
        assert!(state.validate().is_err());
        state
            .runs
            .get_mut(&ScheduleRunId::from("legacy"))
            .unwrap()
            .error = None;
        state
            .runs
            .get_mut(&ScheduleRunId::from("legacy"))
            .unwrap()
            .claimed_at = Some(99);
        assert!(state.validate().is_err());
    }

    #[test]
    fn ignored_legacy_fields_stay_importable_and_once_due_is_consumed() {
        let mut daily = schedule(true, Some(100));
        daily.weekday = Some(7);
        daily.start_at = Some(-1);
        daily.validate().unwrap();

        let mut once = schedule(true, Some(100));
        once.frequency = ScheduleFrequency::Once;
        once.start_at = Some(100);
        let mut state = ScheduleState::default();
        state.schedules.insert("s".into(), once);
        assert!(
            state
                .claim(
                    &"s".into(),
                    "bad".into(),
                    ScheduleRunOrigin::Due,
                    100,
                    Some(200)
                )
                .is_err()
        );
        state
            .claim(
                &"s".into(),
                "good".into(),
                ScheduleRunOrigin::Due,
                100,
                None,
            )
            .unwrap();
        assert_eq!(state.schedules[&ScheduleId::from("s")].next_run, None);
    }

    #[test]
    fn late_failure_of_older_run_does_not_change_latest_schedule_revision() {
        let mut state = state(false, None);
        state
            .claim(
                &"s".into(),
                "first".into(),
                ScheduleRunOrigin::Manual,
                100,
                None,
            )
            .unwrap();
        state
            .record_sent(&"first".into(), "r1".into(), "t1".into(), intent(), 101)
            .unwrap();
        state
            .claim(
                &"s".into(),
                "second".into(),
                ScheduleRunOrigin::Manual,
                101,
                None,
            )
            .unwrap();
        state
            .record_sent(&"second".into(), "r2".into(), "t2".into(), intent(), 102)
            .unwrap();
        let revision = state.schedules[&ScheduleId::from("s")].revision;
        assert!(
            state.runs[&ScheduleRunId::from("first")].claim_revision
                < state.runs[&ScheduleRunId::from("second")].claim_revision
        );
        state
            .record_failed(&"first".into(), "old launcher failed".into(), 103)
            .unwrap();
        assert_eq!(state.schedules[&ScheduleId::from("s")].revision, revision);
        assert_eq!(
            state.runs[&ScheduleRunId::from("first")].state,
            ScheduleRunState::Failed
        );
        assert_eq!(
            state.runs[&ScheduleRunId::from("second")].state,
            ScheduleRunState::Sent
        );
        state.validate().unwrap();
    }

    #[test]
    fn tombstone_and_revision_bounds_match_store_constraints() {
        let mut deleted = schedule(true, Some(100));
        deleted.deleted_at = Some(0);
        assert!(deleted.validate().is_err());
        deleted.enabled = false;
        deleted.next_run = None;
        deleted.validate().unwrap();

        let mut state = state(false, None);
        state
            .schedules
            .get_mut(&ScheduleId::from("s"))
            .unwrap()
            .revision = MAX_REVISION as i64;
        assert!(
            state
                .claim(
                    &"s".into(),
                    "overflow".into(),
                    ScheduleRunOrigin::Manual,
                    100,
                    None
                )
                .is_err()
        );
        assert!(state.runs.is_empty());
        state
            .schedules
            .get_mut(&ScheduleId::from("s"))
            .unwrap()
            .revision += 1;
        assert!(state.validate().is_err());
    }
}
