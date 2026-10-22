//! What a node runs beside its main thread and how far its own plan has got, as its mod reports
//! it: the subagents it spawned, the workflows it started and its Claude Code task list. Kept per
//! session and bounded: finished runs drop out after the latest few, the list stops at a cap.
use crate::node_reports::short_label;
use alondra_core::Millis;
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub(crate) const STEPS_LIMIT: usize = 50;
pub(crate) const FINISHED_LIMIT: usize = 5;
pub(crate) const RUNNING_LIMIT: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StepStatus {
    Pending,
    InProgress,
    Completed,
    Deleted,
    #[serde(other)]
    Unknown,
}

/// One entry of the task list as a `TaskCreate`, `TaskUpdate`, `TaskList` or `TodoWrite` call
/// gave it; an update names only what changed.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub(crate) struct StepIn {
    pub id: String,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub status: Option<StepStatus>,
    #[serde(default)]
    pub active_form: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AgentRun {
    pub id: String,
    pub description: String,
    pub kind: Option<String>,
    pub model: Option<String>,
    pub started_at: Millis,
    pub finished_at: Option<Millis>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WorkflowRun {
    pub id: String,
    pub name: String,
    /// Where the workflow writes one transcript per agent it runs.
    pub transcript_dir: Option<PathBuf>,
    pub started_at: Millis,
    pub finished_at: Option<Millis>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Step {
    pub id: String,
    pub subject: String,
    pub status: StepStatus,
    pub active_form: Option<String>,
}

/// A background task as Claude Code lists it at the end of a turn.
pub(crate) struct Listed<'a> {
    pub id: &'a str,
    pub kind: &'a str,
    pub description: &'a str,
    pub agent_type: Option<&'a str>,
    pub name: Option<&'a str>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct NodeWork {
    pub agents: Vec<AgentRun>,
    pub workflows: Vec<WorkflowRun>,
    pub steps: Vec<Step>,
}

fn keep_recent<T>(items: &mut Vec<T>, started: impl Fn(&T) -> Millis, finished: impl Fn(&T) -> Option<Millis>) {
    for (done, limit) in [(true, FINISHED_LIMIT), (false, RUNNING_LIMIT)] {
        while items.iter().filter(|item| finished(item).is_some() == done).count() > limit {
            let oldest = items.iter().enumerate()
                .filter(|(_, item)| finished(item).is_some() == done)
                .min_by_key(|(_, item)| finished(item).unwrap_or_else(|| started(item)))
                .map(|(index, _)| index);
            let Some(index) = oldest else { break };
            items.remove(index);
        }
    }
}

impl NodeWork {
    pub(crate) fn agent_started(&mut self, id: String, description: Option<String>, kind: Option<String>, model: Option<String>, now: Millis) {
        match self.agents.iter_mut().find(|agent| agent.id == id) {
            Some(agent) => {
                if let Some(description) = description.filter(|text| !text.trim().is_empty()) { agent.description = short_label(&description); }
                agent.kind = kind.or(agent.kind.take());
                agent.model = model.or(agent.model.take());
            }
            None => self.agents.push(AgentRun {
                id,
                description: short_label(description.as_deref().unwrap_or_default()),
                kind,
                model,
                started_at: now,
                finished_at: None,
            }),
        }
        self.trim();
    }

    pub(crate) fn agent_model(&mut self, id: &str, model: String) {
        if let Some(agent) = self.agents.iter_mut().find(|agent| agent.id == id) {
            agent.model = Some(model);
        }
    }

    pub(crate) fn agent_finished(&mut self, id: &str, now: Millis) {
        if let Some(agent) = self.agents.iter_mut().find(|agent| agent.id == id && agent.finished_at.is_none()) {
            agent.finished_at = Some(now);
        }
        self.trim();
    }

    pub(crate) fn workflow_started(&mut self, id: String, name: String, transcript_dir: Option<PathBuf>, now: Millis) {
        match self.workflows.iter_mut().find(|workflow| workflow.id == id) {
            Some(workflow) => {
                workflow.name = short_label(&name);
                workflow.transcript_dir = transcript_dir.or(workflow.transcript_dir.take());
            }
            None => self.workflows.push(WorkflowRun { id, name: short_label(&name), transcript_dir, started_at: now, finished_at: None }),
        }
        self.trim();
    }

    pub(crate) fn workflow_finished(&mut self, id: &str, now: Millis) {
        if let Some(workflow) = self.workflows.iter_mut().find(|workflow| workflow.id == id && workflow.finished_at.is_none()) {
            workflow.finished_at = Some(now);
        }
        self.trim();
    }

    /// The whole list of background work Claude Code gives at the end of a turn: what is in it
    /// runs, and a subagent or workflow that ran and is no longer in it has finished.
    pub(crate) fn listed(&mut self, tasks: &[Listed<'_>], now: Millis) {
        for task in tasks {
            match task.kind {
                "subagent" => self.agent_started(task.id.to_owned(), Some(task.description.to_owned()),
                    task.agent_type.map(str::to_owned), None, now),
                "workflow" => {
                    let name = task.name.filter(|name| !name.is_empty()).unwrap_or(task.description);
                    let adopted = self.workflows.iter_mut().find(|workflow| workflow.finished_at.is_none()
                        && workflow.name == short_label(name) && !tasks.iter().any(|other| other.id == workflow.id));
                    match adopted {
                        Some(workflow) => workflow.id = task.id.to_owned(),
                        None => self.workflow_started(task.id.to_owned(), name.to_owned(), None, now),
                    }
                }
                _ => {}
            }
        }
        let listed = |id: &str| tasks.iter().any(|task| task.id == id);
        for agent in self.agents.iter_mut().filter(|agent| agent.finished_at.is_none() && !listed(&agent.id)) {
            agent.finished_at = Some(now);
        }
        for workflow in self.workflows.iter_mut().filter(|workflow| workflow.finished_at.is_none() && !listed(&workflow.id)) {
            workflow.finished_at = Some(now);
        }
        self.trim();
    }

    pub(crate) fn ended(&mut self, now: Millis) {
        for agent in self.agents.iter_mut().filter(|agent| agent.finished_at.is_none()) {
            agent.finished_at = Some(now);
        }
        for workflow in self.workflows.iter_mut().filter(|workflow| workflow.finished_at.is_none()) {
            workflow.finished_at = Some(now);
        }
        self.trim();
    }

    pub(crate) fn step_created(&mut self, mut step: StepIn) {
        step.status = step.status.filter(|status| *status != StepStatus::Unknown);
        if self.steps.iter().any(|known| known.id == step.id) {
            return self.step_updated(step);
        }
        let Some(subject) = step.subject.filter(|subject| !subject.is_empty()) else { return };
        if step.status == Some(StepStatus::Deleted) {
            return;
        }
        if self.steps.len() >= STEPS_LIMIT {
            let index = self.steps.iter().position(|known| known.status == StepStatus::Completed).unwrap_or(0);
            self.steps.remove(index);
        }
        self.steps.push(Step { id: step.id, subject: short_label(&subject), status: step.status.unwrap_or(StepStatus::Pending), active_form: step.active_form.map(|text| short_label(&text)) });
    }

    pub(crate) fn step_updated(&mut self, mut step: StepIn) {
        step.status = step.status.filter(|status| *status != StepStatus::Unknown);
        if step.status == Some(StepStatus::Deleted) {
            self.steps.retain(|known| known.id != step.id);
            return;
        }
        let Some(known) = self.steps.iter_mut().find(|known| known.id == step.id) else {
            return self.step_created(step);
        };
        if let Some(subject) = step.subject.filter(|subject| !subject.is_empty()) { known.subject = short_label(&subject); }
        if let Some(status) = step.status { known.status = status; }
        if let Some(active_form) = step.active_form { known.active_form = Some(short_label(&active_form)); }
    }

    /// The whole list, as `TaskList` or `TodoWrite` gives it: it replaces what was reconstructed.
    pub(crate) fn steps_listed(&mut self, steps: Vec<StepIn>) {
        let known = std::mem::take(&mut self.steps);
        for step in steps.into_iter().take(STEPS_LIMIT) {
            let Some(subject) = step.subject.filter(|subject| !subject.is_empty()) else { continue };
            let status = match step.status {
                Some(StepStatus::Deleted) => continue,
                Some(StepStatus::Unknown) | None => StepStatus::Pending,
                Some(status) => status,
            };
            let active_form = step.active_form.map(|text| short_label(&text)).or_else(|| known.iter().find(|old| old.id == step.id).and_then(|old| old.active_form.clone()));
            self.steps.push(Step { id: step.id, subject: short_label(&subject), status, active_form });
        }
    }

    fn trim(&mut self) {
        keep_recent(&mut self.agents, |agent| agent.started_at, |agent| agent.finished_at);
        keep_recent(&mut self.workflows, |workflow| workflow.started_at, |workflow| workflow.finished_at);
    }
}

/// How many agents a workflow has started, by the transcripts it wrote; none when its folder
/// cannot be read or holds none yet.
pub(crate) fn workflow_agents(dir: &Path) -> Option<u32> {
    let count = std::fs::read_dir(dir).ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "jsonl"))
        .count();
    (count > 0).then(|| count.min(u32::MAX as usize) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(id: &str, subject: Option<&str>, status: Option<StepStatus>, active_form: Option<&str>) -> StepIn {
        StepIn { id: id.into(), subject: subject.map(Into::into), status, active_form: active_form.map(Into::into) }
    }

    fn states(work: &NodeWork) -> Vec<(&str, StepStatus)> {
        work.steps.iter().map(|step| (step.subject.as_str(), step.status)).collect()
    }

    #[test]
    fn a_task_list_is_created_then_worked_through_and_a_deleted_step_leaves_it() {
        let mut work = NodeWork::default();
        work.step_created(step("1", Some("Read the code"), None, Some("Reading the code")));
        work.step_created(step("2", Some("Write the fix"), None, None));
        work.step_created(step("3", Some("Run the tests"), None, Some("Running the tests")));
        assert_eq!(states(&work), [("Read the code", StepStatus::Pending), ("Write the fix", StepStatus::Pending), ("Run the tests", StepStatus::Pending)]);
        work.step_updated(step("1", None, Some(StepStatus::InProgress), None));
        assert_eq!(work.steps[0].active_form.as_deref(), Some("Reading the code"));
        work.step_updated(step("1", None, Some(StepStatus::Completed), None));
        work.step_updated(step("2", Some("Write the parser fix"), Some(StepStatus::InProgress), Some("Writing the fix")));
        work.step_updated(step("3", None, Some(StepStatus::Deleted), None));
        work.step_updated(step("9", None, Some(StepStatus::Completed), None));
        assert_eq!(states(&work), [("Read the code", StepStatus::Completed), ("Write the parser fix", StepStatus::InProgress)]);
        assert_eq!(work.steps[1].active_form.as_deref(), Some("Writing the fix"));
        work.step_updated(step("2", None, Some(StepStatus::Unknown), None));
        assert_eq!(work.steps[1].status, StepStatus::InProgress, "a status this build does not know changes nothing");
    }

    #[test]
    fn the_whole_list_replaces_the_reconstruction_and_keeps_what_it_does_not_say() {
        let mut work = NodeWork::default();
        work.step_created(step("1", Some("Read"), None, Some("Reading")));
        work.step_created(step("2", Some("Stale"), None, None));
        work.steps_listed(vec![
            step("1", Some("Read"), Some(StepStatus::InProgress), None),
            step("3", Some("Ship"), Some(StepStatus::Pending), None),
            step("4", None, Some(StepStatus::Pending), None),
        ]);
        assert_eq!(states(&work), [("Read", StepStatus::InProgress), ("Ship", StepStatus::Pending)]);
        assert_eq!(work.steps[0].active_form.as_deref(), Some("Reading"));
    }

    #[test]
    fn a_long_task_list_stops_at_its_cap_dropping_a_finished_step_first() {
        let mut work = NodeWork::default();
        for index in 0..STEPS_LIMIT {
            work.step_created(step(&index.to_string(), Some(&format!("Step {index}")), None, None));
        }
        work.step_updated(step("7", None, Some(StepStatus::Completed), None));
        work.step_created(step("new", Some("One more"), None, None));
        assert_eq!(work.steps.len(), STEPS_LIMIT);
        assert!(work.steps.iter().all(|step| step.id != "7"));
        assert_eq!(work.steps.last().unwrap().subject, "One more");
    }

    #[test]
    fn a_subagent_keeps_its_type_and_model_and_ends_with_how_long_it_ran() {
        let mut work = NodeWork::default();
        work.agent_started("a1".into(), Some("Fix the parser".into()), Some("general-purpose".into()), Some("claude-sonnet-5".into()), 1_000);
        work.agent_model("a1", "claude-haiku-5".into());
        work.agent_model("unknown", "claude-opus-5".into());
        assert_eq!(work.agents.len(), 1);
        assert_eq!((work.agents[0].kind.as_deref(), work.agents[0].model.as_deref()), (Some("general-purpose"), Some("claude-haiku-5")));
        work.agent_finished("a1", 61_000);
        work.agent_finished("a1", 99_000);
        assert_eq!(work.agents[0].finished_at, Some(61_000), "the first end is when it finished");
    }

    #[test]
    fn only_the_latest_finished_subagents_are_kept_and_running_ones_stay() {
        let mut work = NodeWork::default();
        work.agent_started("live".into(), Some("Still going".into()), None, None, 0);
        for index in 0..8 {
            let id = format!("done-{index}");
            work.agent_started(id.clone(), Some("Done".into()), None, None, index + 1);
            work.agent_finished(&id, 100 + index);
        }
        assert_eq!(work.agents.len(), FINISHED_LIMIT + 1);
        assert!(work.agents.iter().any(|agent| agent.id == "live" && agent.finished_at.is_none()));
        assert!(work.agents.iter().all(|agent| agent.id != "done-0" && agent.id != "done-2"));
    }

    #[test]
    fn a_workflow_runs_while_claude_code_lists_it_and_finishes_once_it_is_gone() {
        let mut work = NodeWork::default();
        work.workflow_started("w1".into(), "review-loop".into(), Some("/tmp/w1".into()), 1_000);
        let listed = |id, kind, name| Listed { id, kind, description: "Background work", agent_type: None, name };
        work.listed(&[listed("w1", "workflow", Some("review-loop")), listed("s1", "shell", None)], 2_000);
        assert_eq!(work.workflows[0].finished_at, None);
        work.listed(&[listed("t-9", "workflow", Some("review-loop"))], 3_000);
        assert_eq!(work.workflows.len(), 1, "a workflow listed under another id is the one already running by that name");
        assert_eq!((work.workflows[0].id.as_str(), work.workflows[0].transcript_dir.as_deref()), ("t-9", Some(Path::new("/tmp/w1"))));
        work.listed(&[], 4_000);
        assert_eq!(work.workflows[0].finished_at, Some(4_000));
    }

    #[test]
    fn the_end_of_a_turn_finishes_the_subagents_no_longer_listed_and_names_the_type_of_those_that_are() {
        let mut work = NodeWork::default();
        work.agent_started("a1".into(), Some("Explore".into()), None, Some("claude-haiku-5".into()), 0);
        work.agent_started("a2".into(), Some("Foreground".into()), None, None, 0);
        work.listed(&[Listed { id: "a1", kind: "subagent", description: "Explore the repo", agent_type: Some("Explore"), name: None }], 5_000);
        let a1 = work.agents.iter().find(|agent| agent.id == "a1").unwrap();
        assert_eq!((a1.kind.as_deref(), a1.model.as_deref(), a1.finished_at), (Some("Explore"), Some("claude-haiku-5"), None));
        assert_eq!(work.agents.iter().find(|agent| agent.id == "a2").unwrap().finished_at, Some(5_000));
        work.ended(6_000);
        assert!(work.agents.iter().all(|agent| agent.finished_at.is_some()));
    }

    #[test]
    fn a_workflow_counts_the_agent_transcripts_in_its_folder() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = scratch_directory.parent().unwrap().join(format!("alondra-workflow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(workflow_agents(&dir), None);
        for name in ["agent-1.jsonl", "agent-2.jsonl", "script.js"] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        assert_eq!(workflow_agents(&dir), Some(2));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(workflow_agents(&dir), None);
    }
}
