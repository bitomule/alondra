//! The model profiles a role is set with, and how an action names one.
use alondra_core::accounts::Provider;
use alondra_core::model_selection::ModelProfile;

pub const PROFILES: [(ModelProfile, &str); 5] = [(ModelProfile::Implementation, "implementation"), (ModelProfile::General, "general"), (ModelProfile::Design, "design"), (ModelProfile::Review, "review"), (ModelProfile::Orchestrator, "orchestrator")];

pub fn parse_profile(subject: &str) -> Option<(Provider, ModelProfile)> {
    let (provider, role) = subject.split_once('-')?;
    Some((Provider::parse(provider).ok()?, PROFILES.iter().find(|(_, name)| *name == role)?.0))
}

