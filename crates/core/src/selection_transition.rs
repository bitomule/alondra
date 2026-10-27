//! Pure selection policy. The daemon must still drain and fence an execution before admission.
use crate::accounts::{AccountKey, Provider};
use crate::model_selection::{ModelKey, ModelSelection};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveSelection {
    pub account: AccountKey,
    pub model: ModelSelection,
    pub configuration: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinuationIdentity {
    pub source_driver: String,
    pub target_driver: String,
    pub source_history: String,
    pub target_history: String,
    pub native_session: String,
    pub verified: bool,
}

impl ContinuationIdentity {
    fn compatible(&self) -> bool {
        self.verified && !self.native_session.is_empty() && !self.source_driver.is_empty()
            && self.source_driver == self.target_driver && !self.source_history.is_empty()
            && self.source_history == self.target_history
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransitionCapabilities {
    pub turn_scoped_model: bool,
    pub turn_scoped_reasoning: bool,
    pub native_resume: bool,
    pub context_window: Option<u64>,
    pub observed_context_tokens: Option<u64>,
    pub input_modalities: BTreeSet<String>,
    pub required_modalities: BTreeSet<String>,
    pub pinned_account: Option<AccountKey>,
    pub requested_model: Option<ModelKey>,
    pub requested_reasoning_effort: Option<String>,
    pub allow_model_substitution: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectionTransition {
    Keep,
    ApplyNextTurn,
    RestartAndResume,
    CreateWithHandoff,
    Reject(String),
}

pub fn classify_transition(current: &ActiveSelection, target: &ActiveSelection, capabilities: &TransitionCapabilities, continuation_identity: Option<&ContinuationIdentity>) -> SelectionTransition {
    use SelectionTransition::*;
    if target.account.provider != target.model.model.provider { return Reject("The model belongs to a different provider than the account".into()); }
    if capabilities.pinned_account.as_ref().is_some_and(|account| account != &target.account) { return Reject("The task is pinned to its selected account".into()); }
    if !capabilities.allow_model_substitution && capabilities.requested_model.as_ref().is_some_and(|model| model != &target.model.model) { return Reject("The task does not allow model substitution".into()); }
    if capabilities.requested_model.as_ref().is_none_or(|model| model == &target.model.model)
        && capabilities.requested_reasoning_effort.as_ref().is_some_and(|effort| target.model.reasoning_effort.as_ref() != Some(effort)) { return Reject("The destination does not preserve the requested reasoning effort".into()); }
    if !capabilities.required_modalities.is_subset(&capabilities.input_modalities) { return Reject("The destination model cannot receive every required attachment modality".into()); }
    if current.account == target.account && current.configuration == target.configuration
        && current.model.model == target.model.model && current.model.reasoning_effort == target.model.reasoning_effort { return Keep; }
    if current.account.provider != target.account.provider { return CreateWithHandoff; }
    // A Codex home is also a native history namespace. Distinct accounts do not share it in
    // this release, even if a future adapter advertises native resume support.
    if current.account != target.account && target.account.provider == Provider::Codex { return CreateWithHandoff; }
    if capabilities.context_window.zip(capabilities.observed_context_tokens).is_some_and(|(capacity, used)| used >= capacity) { return CreateWithHandoff; }
    let same_execution = current.account == target.account && current.configuration == target.configuration;
    let model_compatible = current.model.model == target.model.model || capabilities.turn_scoped_model;
    let effort_compatible = current.model.reasoning_effort == target.model.reasoning_effort || capabilities.turn_scoped_reasoning;
    if same_execution && model_compatible && effort_compatible { return ApplyNextTurn; }
    if capabilities.native_resume && continuation_identity.is_some_and(ContinuationIdentity::compatible) { return RestartAndResume; }
    CreateWithHandoff
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected(provider: Provider, account: &str, model: &str) -> ActiveSelection {
        ActiveSelection { account: AccountKey { provider, account: crate::accounts::AccountId::parse(account).unwrap() },
            model: ModelSelection { model: ModelKey { provider, model: model.into() }, reasoning_effort: None, catalog_revision: "catalog".into() }, configuration: "config".into() }
    }

    #[test]
    fn selection_transition_matrix_distinguishes_native_turn_changes_and_handoff() {
        let claude = selected(Provider::Claude, "default", "native-default");
        let codex = selected(Provider::Codex, "default", "only-model");
        let other_codex = selected(Provider::Codex, "work", "only-model");
        let capabilities = TransitionCapabilities { turn_scoped_model: true, turn_scoped_reasoning: true, native_resume: true, allow_model_substitution: true, ..Default::default() };
        assert_eq!(classify_transition(&claude, &claude, &capabilities, None), SelectionTransition::Keep);
        assert_eq!(classify_transition(&claude, &codex, &capabilities, None), SelectionTransition::CreateWithHandoff);
        assert_eq!(classify_transition(&codex, &claude, &capabilities, None), SelectionTransition::CreateWithHandoff);
        assert_eq!(classify_transition(&codex, &other_codex, &capabilities, None), SelectionTransition::CreateWithHandoff);
        let mut changed = codex.clone();
        changed.model.reasoning_effort = Some("high".into());
        assert_eq!(classify_transition(&codex, &changed, &capabilities, None), SelectionTransition::ApplyNextTurn);
        changed.model.model.model = "another-model".into();
        assert_eq!(classify_transition(&codex, &changed, &capabilities, None), SelectionTransition::ApplyNextTurn);
    }

    #[test]
    fn restart_requires_proven_continuity_and_never_crosses_codex_homes() {
        let current = selected(Provider::Claude, "default", "one-model");
        let target = selected(Provider::Claude, "work", "one-model");
        let capabilities = TransitionCapabilities { native_resume: true, allow_model_substitution: true, ..Default::default() };
        let mut identity = ContinuationIdentity { source_driver: "claude".into(), target_driver: "claude".into(), source_history: "same".into(), target_history: "same".into(), native_session: "session".into(), verified: true };
        assert_eq!(classify_transition(&current, &target, &capabilities, Some(&identity)), SelectionTransition::RestartAndResume);
        identity.target_history = "different".into();
        assert_eq!(classify_transition(&current, &target, &capabilities, Some(&identity)), SelectionTransition::CreateWithHandoff);
        identity.target_history = identity.source_history.clone();
        identity.verified = false;
        assert_eq!(classify_transition(&current, &target, &capabilities, Some(&identity)), SelectionTransition::CreateWithHandoff);
    }

    #[test]
    fn pinned_accounts_models_and_modalities_cannot_be_bypassed_by_transition() {
        let current = selected(Provider::Claude, "default", "native-default");
        let target = selected(Provider::Codex, "default", "only-model");
        let mut capabilities = TransitionCapabilities { pinned_account: Some(current.account.clone()), ..Default::default() };
        assert!(matches!(classify_transition(&current, &target, &capabilities, None), SelectionTransition::Reject(_)));
        capabilities.pinned_account = None;
        capabilities.requested_model = Some(current.model.model.clone());
        assert!(matches!(classify_transition(&current, &target, &capabilities, None), SelectionTransition::Reject(_)));
        capabilities.allow_model_substitution = true;
        capabilities.requested_reasoning_effort = Some("source-provider-effort".into());
        assert_eq!(classify_transition(&current,&target,&capabilities,None),SelectionTransition::CreateWithHandoff,"a permitted different model uses its own native effort without rewriting the original request");
        capabilities.required_modalities.insert("image".into());
        assert!(matches!(classify_transition(&current, &target, &capabilities, None), SelectionTransition::Reject(_)));
        capabilities.input_modalities.insert("image".into());
        assert_eq!(classify_transition(&current, &target, &capabilities, None), SelectionTransition::CreateWithHandoff);
    }

    #[test]
    fn smaller_context_requires_repacking_instead_of_blind_native_resume() {
        let current = selected(Provider::Codex, "default", "large");
        let target = selected(Provider::Codex, "default", "small");
        let capabilities = TransitionCapabilities { turn_scoped_model: true, allow_model_substitution: true, context_window: Some(100), observed_context_tokens: Some(100), ..Default::default() };
        assert_eq!(classify_transition(&current, &target, &capabilities, None), SelectionTransition::CreateWithHandoff);
    }
}
