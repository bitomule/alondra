//! What a launch carries and how it fails, as the ports speak of them.
use alondra_core::accounts::Provider;

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

impl LaunchOptions {
    /// The effort levels the Agent SDK takes; any other a launch resolved is left to Claude.
    pub fn effort(&self) -> Option<String> {
        self.effort.clone().filter(|effort| matches!(effort.as_str(), "low" | "medium" | "high" | "xhigh" | "max"))
    }
}

#[derive(Debug)]
pub struct Failed {
    pub why: String,
    /// A write it needed found the disk full: once there is space it can go on as it was.
    pub no_space: bool,
    /// The domain's refusal, when the domain refused it.
    pub refusal: Option<alondra_domain_rules::DomainError>,
}

impl Failed {
    pub fn new(why: String) -> Self {
        Self { why, no_space: false, refusal: None }
    }
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.why)
    }
}
