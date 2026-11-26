use alondra_domain_rules::TaskId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use alondra_domain_rules::LaunchPlan;
pub use alondra_engine_ports::launch::{Failed, LaunchOptions};

/// Where a launch keeps its options: beside the execution's host.
pub trait LaunchOptionsFile: Sized {
    fn save(&self, state_directory: &std::path::Path) -> Result<(), Failed>;
    fn load(state_directory: &std::path::Path) -> Result<Self, Failed>;
}

/// A failure of a write, which knows whether the disk was full.
pub trait WrittenFailure {
    fn written(why: String, error: &std::io::Error) -> Self;
}

impl WrittenFailure for Failed {
    fn written(why: String, error: &std::io::Error) -> Self {
        Self { why, no_space: alondra_engine_ports::disk::io_no_space(error), refusal: None }
    }
}

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
    /// Whether the task goes in stages, and why; its stored launch plan says it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stages: Option<alondra_domain_rules::StageChoice>,
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
                alondra_domain_rules::TaskRole::Design => alondra_core::model_selection::ModelProfile::Design,
                alondra_domain_rules::TaskRole::Implementation => alondra_core::model_selection::ModelProfile::Implementation,
                alondra_domain_rules::TaskRole::Review => alondra_core::model_selection::ModelProfile::Review,
            }),
            effort: plan.effort.clone(),
            approach: plan.approach,
            tags: plan.tags.clone(),
            reason: plan.reason.clone(),
            relaunch_model: None,
            replace: false,
            _over_pace_ok: false,
            stages: plan.stages.clone(),
        }.staged_profile()
    }

    /// The author of a staged task plans it: it starts with the Design profile unless its plan
    /// names another role.
    fn staged_profile(mut self) -> Self {
        if self.stages.as_ref().is_some_and(|choice| choice.staged) && self.profile.is_none() {
            self.profile = Some(alondra_core::model_selection::ModelProfile::Design);
        }
        self
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
            alondra_domain_rules::TaskRole::Design => alondra_core::model_selection::ModelProfile::Design,
            alondra_domain_rules::TaskRole::Implementation => alondra_core::model_selection::ModelProfile::Implementation,
            alondra_domain_rules::TaskRole::Review => alondra_core::model_selection::ModelProfile::Review,
        }));
        self.stages = self.stages.or_else(|| plan.stages.clone());
        if self.relaunch_model.is_none() {
            self.model = self.model.or_else(|| plan.model.clone());
            self.effort = self.effort.or_else(|| plan.effort.clone());
            self.reason = self.reason.or_else(|| plan.reason.clone());
        }
        self.staged_profile()
    }
}

pub fn retain_selection_options(directory:&std::path::Path,selection:alondra_core::model_selection::TaskSelection)->Result<(),Failed>{
    let mut options=LaunchOptions::load(directory)?;
    let effective=selection.effective.as_ref().ok_or_else(||Failed::new("The effective selection is unknown".into()))?;
    options.model=Some(effective.model.model.clone());options.effort=effective.reasoning_effort.clone();options.selection=Some(selection);
    options.save(directory)
}

const LAUNCH_OPTIONS: &str = "launch-options.json";

impl LaunchOptionsFile for LaunchOptions {
    fn save(&self, state_directory: &std::path::Path) -> Result<(), Failed> {
        let text = serde_json::to_vec(self).map_err(|error| Failed::new(format!("the launch options: {error}")))?;
        crate::save::atomic(&state_directory.join(LAUNCH_OPTIONS), &text).map_err(|error| Failed::written(format!("keeping the launch options: {error}"), &error))
    }

    fn load(state_directory: &std::path::Path) -> Result<Self, Failed> {
        match std::fs::read(state_directory.join(LAUNCH_OPTIONS)) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| Failed::new(format!("reading the launch options: {error}"))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(Failed::new(format!("reading the launch options: {error}"))),
        }
    }
}


