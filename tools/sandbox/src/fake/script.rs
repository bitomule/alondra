//! What a run tells the fakes to do: per request, the tasks the orchestrator creates and what each
//! task's node does on its own.
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{Checks, Fakes, read_lines};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RequestScript {
    pub request_id: String,
    pub tasks: Vec<TaskScript>,
    /// An answer the orchestrator closes the request with, for a request that needs no task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TaskScript {
    pub task_id: String,
    pub title: String,
    /// What the node does once it has its task.
    #[serde(default)]
    pub on_start: Vec<Step>,
    /// What it does when Alondra hands it requested changes or a failed merge.
    #[serde(default)]
    pub on_feedback: Vec<Step>,
    /// Whether it answers Alondra's clean-up request with `cleanup_done`.
    #[serde(default = "yes")]
    pub cleans_up: bool,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Step {
    /// Writes `file` in the worktree and commits everything.
    Commit { file: String, text: String },
    /// Writes `file` without committing it.
    Dirty { file: String },
    Push,
    /// Opens a PR on the fake GitHub for the worktree's branch.
    OpenPr {
        number: u64,
        #[serde(default)]
        draft: bool,
        #[serde(default)]
        checks: Checks,
    },
    RegisterPr { number: u64 },
    /// `present_result`, with the PR and its current head when `pr` is set.
    Deliver {
        need: String,
        #[serde(default)]
        pr: Option<u64>,
        #[serde(rename = "final", default = "yes")]
        is_final: bool,
    },
    Ask { id: String, need: String, options: Vec<String> },
    /// `post_merge_work`: work that still runs after the merge, or none when `reason` is absent.
    PostMergeWork {
        #[serde(default)]
        reason: Option<String>,
    },
    /// A turn that ends with `said` in the terminal and no card, behind a command of its own when
    /// `behind` names one. Claude Code then lists the session as `listed` says: blocked, waiting on
    /// the user, unless told otherwise.
    Speak {
        said: String,
        #[serde(default)]
        behind: Option<String>,
        #[serde(default)]
        listed: Option<String>,
    },
    Cleanup,
    /// The session ends as when the user exits it: `session.end`, then the process goes.
    End,
    /// The process dies without a word, as a crashed session does.
    Die,
}

impl Step {
    /// Commit, push, open and register PR `number`, and deliver it as final.
    pub fn work(number: u64, file: &str) -> Vec<Step> {
        vec![
            Step::Commit { file: file.into(), text: format!("{file} from the sandbox\n") },
            Step::Push,
            Step::OpenPr { number, draft: false, checks: Checks::Success },
            Step::RegisterPr { number },
            Step::Deliver { need: format!("Review {file}"), pr: Some(number), is_final: true },
        ]
    }
}

impl Fakes {
    fn scripts(&self) -> PathBuf {
        self.dir.join("scripts")
    }

    pub fn write_script(&self, script: &RequestScript) -> Result<()> {
        super::write_json(&self.scripts().join(format!("{}.json", script.request_id)), script)
    }

    pub fn request_scripts(&self) -> Result<Vec<RequestScript>> {
        let mut scripts = vec![];
        let Ok(entries) = std::fs::read_dir(self.scripts()) else { return Ok(scripts) };
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                scripts.push(serde_json::from_slice(&std::fs::read(&path)?)?);
            }
        }
        scripts.sort_by(|a: &RequestScript, b| a.request_id.cmp(&b.request_id));
        Ok(scripts)
    }

    /// The script of the task whose id `text` names, the longest id first.
    pub fn task_script_in(&self, text: &str) -> Result<Option<TaskScript>> {
        let mut tasks: Vec<TaskScript> = self.request_scripts()?.into_iter().flat_map(|script| script.tasks).collect();
        tasks.sort_by_key(|task| std::cmp::Reverse(task.task_id.len()));
        Ok(tasks.into_iter().find(|task| text.contains(&task.task_id)))
    }

    pub fn task_script(&self, task_id: &str) -> Result<Option<TaskScript>> {
        Ok(self.request_scripts()?.into_iter().flat_map(|script| script.tasks).find(|task| task.task_id == task_id))
    }

    /// What the orchestrator did, in order.
    pub fn orchestrator_actions(&self) -> Result<Vec<serde_json::Value>> {
        read_lines(&self.dir.join("orchestrator/actions.jsonl"))
    }
}
