use alondra_core::{accounts::Provider, domain::TaskId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use alondra_core::domain::LaunchPlan;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    pub task: TaskId,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<alondra_core::accounts::Provider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<alondra_core::model_selection::ModelProfile>,
    #[serde(default, alias = "reasoning_effort")]
    pub effort: Option<String>,
    #[serde(default)]
    pub approach: Option<alondra_core::options::Approach>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<alondra_core::task_tags::TaskTags>,
    /// What the task is and why this model: required when the orchestrator chooses the model.
    #[serde(default)]
    pub reason: Option<String>,
    /// What a relaunch passes when the orchestrator chose the model: the one the task had.
    #[serde(skip)]
    pub relaunch_model: Option<String>,
    /// Stops the task's current node and launches this one in its folder, keeping its files.
    #[serde(default)]
    pub replace: bool,
    /// Orchestrators written when going over pace needed the user's word still send it.
    #[serde(default, rename = "over_pace_ok")]
    pub _over_pace_ok: bool,
}

impl LaunchBody {
    /// The launch a stored plan describes.
    pub fn from_plan(task: TaskId, plan: &LaunchPlan) -> LaunchBody {
        LaunchBody {
            name: Some(plan.name.clone()),
            cwd: Some(plan.cwd.clone()),
            task,
            prompt: plan.mission.clone(),
            title: None,
            model: plan.model.clone(),
            provider: None,
            profile: plan.role.map(|role| match role {
                alondra_core::domain::TaskRole::Design => alondra_core::model_selection::ModelProfile::Design,
                alondra_core::domain::TaskRole::Implementation => alondra_core::model_selection::ModelProfile::Implementation,
                alondra_core::domain::TaskRole::Review => alondra_core::model_selection::ModelProfile::Review,
            }),
            effort: plan.effort.clone(),
            approach: plan.approach,
            tags: plan.tags.clone(),
            reason: plan.reason.clone(),
            relaunch_model: None,
            replace: false,
            _over_pace_ok: false,
        }
    }

    pub fn task_id(&self) -> &TaskId {
        &self.task
    }

    /// The launch as the gate's queue keeps it, the model of a relaunch included.
    pub fn to_queue(&self) -> Value {
        json!({"body": serde_json::to_value(self).unwrap_or_default(), "relaunch_model": self.relaunch_model})
    }

    pub fn from_queue(stored: &Value) -> anyhow::Result<LaunchBody> {
        let mut body: LaunchBody = serde_json::from_value(stored["body"].clone())?;
        body.relaunch_model = stored["relaunch_model"].as_str().map(str::to_owned);
        Ok(body)
    }

    /// What the task's stored plan fills in where the call named nothing.
    pub fn with_plan(mut self, plan: Option<&LaunchPlan>) -> LaunchBody {
        let Some(plan) = plan else { return self };
        self.name = self.name.or_else(|| Some(plan.name.clone()));
        self.cwd = self.cwd.or_else(|| Some(plan.cwd.clone()));
        self.prompt = self.prompt.or_else(|| plan.mission.clone());
        self.approach = self.approach.or(plan.approach);
        self.tags = self.tags.or_else(|| plan.tags.clone());
        self.profile = self.profile.or_else(|| plan.role.map(|role| match role {
            alondra_core::domain::TaskRole::Design => alondra_core::model_selection::ModelProfile::Design,
            alondra_core::domain::TaskRole::Implementation => alondra_core::model_selection::ModelProfile::Implementation,
            alondra_core::domain::TaskRole::Review => alondra_core::model_selection::ModelProfile::Review,
        }));
        if self.relaunch_model.is_none() {
            self.model = self.model.or_else(|| plan.model.clone());
            self.effort = self.effort.or_else(|| plan.effort.clone());
            self.reason = self.reason.or_else(|| plan.reason.clone());
        }
        self
    }
}

/// What the task's Claude is launched with besides its prompt: the model and effort the launch
/// resolved and the tools its request denies. Kept beside the execution's host, so a host that
/// resumes the session launches Claude the same way.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LaunchOptions {
    #[serde(default)]
    pub provider: Provider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<alondra_core::model_selection::TaskSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disallowed_tools: Vec<String>,
}

pub fn retain_selection_options(directory:&std::path::Path,selection:alondra_core::model_selection::TaskSelection)->Result<(),Failed>{
    let mut options=LaunchOptions::load(directory)?;
    let effective=selection.effective.as_ref().ok_or_else(||Failed::new("The effective selection is unknown".into()))?;
    options.model=Some(effective.model.model.clone());options.effort=effective.reasoning_effort.clone();options.selection=Some(selection);
    options.save(directory)
}

const LAUNCH_OPTIONS: &str = "launch-options.json";

impl LaunchOptions {
    /// The effort levels the Agent SDK takes; any other a launch resolved is left to Claude.
    pub fn effort(&self) -> Option<String> {
        self.effort.clone().filter(|effort| matches!(effort.as_str(), "low" | "medium" | "high" | "xhigh" | "max"))
    }

    pub fn save(&self, state_directory: &std::path::Path) -> Result<(), Failed> {
        let text = serde_json::to_vec(self).map_err(|error| Failed::new(format!("the launch options: {error}")))?;
        crate::save::atomic(&state_directory.join(LAUNCH_OPTIONS), &text).map_err(|error| Failed::written(format!("keeping the launch options: {error}"), &error))
    }

    pub fn load(state_directory: &std::path::Path) -> Result<Self, Failed> {
        match std::fs::read(state_directory.join(LAUNCH_OPTIONS)) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| Failed::new(format!("reading the launch options: {error}"))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(Failed::new(format!("reading the launch options: {error}"))),
        }
    }
}

#[derive(Debug)]
pub struct Failed {
    pub why: String,
    /// A write it needed found the disk full: once there is space it can go on as it was.
    pub no_space: bool,
}
impl Failed {
    pub fn new(why: String) -> Self {
        Self { why, no_space: false }
    }

    pub fn written(why: String, error: &std::io::Error) -> Self {
        Self { why, no_space: crate::disk::io_no_space(error) }
    }
}
impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.why)
    }
}
