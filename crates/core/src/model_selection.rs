use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::accounts::{AccountKey, Provider};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ModelKey {
    pub provider: Provider,
    pub model: String,
}

impl ModelKey {
    pub fn wire_id(&self) -> String { format!("{}/{}", self.provider.as_str(), self.model) }
    pub fn parse_wire(value: &str) -> Result<Self, String> {
        let (provider, model) = value.split_once('/').ok_or("Choose a provider-qualified model")?;
        if model.trim().is_empty() || model.chars().any(char::is_control) { return Err("A model ID cannot be empty or contain control characters".into()); }
        Ok(Self { provider: Provider::parse(provider)?, model: model.into() })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelSelection {
    pub model: ModelKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub catalog_revision: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ModelProfile { Implementation, General, Design, Review, Orchestrator }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelCandidate {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoleModelPolicy {
    #[default]
    NativeDefault,
    OrderedCandidates { candidates: Vec<ModelCandidate> },
}

pub type ModelProfiles = BTreeMap<Provider, BTreeMap<ModelProfile, RoleModelPolicy>>;

pub fn profiles_version(profiles: &ModelProfiles) -> String {
    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(profiles).expect("model profiles serialize");
    Sha256::digest(bytes).iter().take(8).map(|byte| format!("{byte:02x}")).collect()
}

pub fn validate_profiles(profiles: &ModelProfiles) -> Result<(), String> {
    for provider in profiles.values() {
        for policy in provider.values() {
            if let RoleModelPolicy::OrderedCandidates { candidates } = policy {
                if candidates.is_empty() || candidates.len() > 32 { return Err("A model profile has between 1 and 32 ordered candidates".into()); }
                let mut seen = std::collections::BTreeSet::new();
                for candidate in candidates {
                    if candidate.model.trim().is_empty() || candidate.model.len() > 512 || candidate.model.chars().any(char::is_control) { return Err("A model candidate needs a valid native model ID".into()); }
                    if candidate.reasoning_effort.as_ref().is_some_and(|effort| effort.is_empty() || effort.len() > 64 || effort.chars().any(char::is_control)) { return Err("A model candidate needs a valid native reasoning effort".into()); }
                    if !seen.insert((&candidate.model, &candidate.reasoning_effort)) { return Err("A model profile repeats the same candidate".into()); }
                }
            }
        }
    }
    Ok(())
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CatalogModel {
    pub model: String,
    pub display_name: String,
    #[serde(default)]
    pub reasoning_efforts: Vec<String>,
    #[serde(default)]
    pub default_reasoning_effort: Option<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub input_modalities: Vec<String>,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AccountModelCatalog {
    pub account: AccountKey,
    pub revision: String,
    pub models: Vec<CatalogModel>,
    #[serde(default)]
    pub native_default: Option<String>,
    pub measured_at: crate::Millis,
}

impl AccountModelCatalog {
    pub fn resolve(&self, candidate: &ModelCandidate) -> Result<ModelSelection, String> {
        let entry = self.models.iter().find(|entry| entry.model == candidate.model && !entry.hidden)
            .ok_or_else(|| format!("{} is not advertised by this account", candidate.model))?;
        if candidate.reasoning_effort.as_ref().is_some_and(|effort| !entry.reasoning_efforts.contains(effort)) {
            return Err(format!("{} does not advertise reasoning effort {}", candidate.model, candidate.reasoning_effort.as_deref().unwrap_or_default()));
        }
        Ok(ModelSelection {
            model: ModelKey { provider: self.account.provider, model: entry.model.clone() },
            reasoning_effort: candidate.reasoning_effort.clone().or_else(|| entry.default_reasoning_effort.clone()),
            catalog_revision: self.revision.clone(),
        })
    }

    pub fn select(&self, policy: &RoleModelPolicy) -> Result<ModelSelection, String> {
        match policy {
            RoleModelPolicy::NativeDefault => {
                let model = self.native_default.as_deref()
                    .or_else(|| self.models.iter().find(|entry| entry.is_default && !entry.hidden).map(|entry| entry.model.as_str()))
                    .ok_or_else(|| "Choose a model for this account in Settings: its native default is not known".to_owned())?;
                self.resolve(&ModelCandidate { model: model.into(), reasoning_effort: None })
            }
            RoleModelPolicy::OrderedCandidates { candidates } => candidates.iter().find_map(|candidate| self.resolve(candidate).ok())
                .ok_or_else(|| "None of the configured models is advertised with the requested effort".into()),
        }
    }

    pub fn validate(&self, selection: &ModelSelection) -> Result<(), String> {
        if selection.model.provider != self.account.provider { return Err("The model belongs to another provider".into()); }
        if selection.catalog_revision != self.revision { return Err("The model catalog changed: refresh the selection".into()); }
        self.resolve(&ModelCandidate { model: selection.model.model.clone(), reasoning_effort: selection.reasoning_effort.clone() }).map(drop)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskSelection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paid_account_grant: Option<AccountKey>,
    #[serde(default)]
    pub requested: Option<ModelKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_reasoning_effort: Option<String>,
    #[serde(default)]
    pub effective: Option<ModelSelection>,
    pub profile: ModelProfile,
    pub allow_model_substitution: bool,
    #[serde(default)]
    pub substitution_was_edited: bool,
    #[serde(default)]
    pub account: Option<AccountKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_tool_denials: Option<Vec<String>>,
}

impl TaskSelection {
    pub fn automatic(profile: ModelProfile) -> Self {
        Self { paid_account_grant: None, requested: None, requested_reasoning_effort: None, effective: None, profile, allow_model_substitution: true, substitution_was_edited: false, account: None, native_tool_denials: None }
    }

    pub fn authorize_paid_account(&mut self, accounts: &[crate::accounts::AccountStatus]) {
        self.paid_account_grant = self.account.as_ref().filter(|key| accounts.iter().any(|account| account.provider == key.provider
            && account.id == key.account && account.billing == crate::accounts::AccountBilling::PayPerUse)).cloned();
    }

    pub fn request_model(&mut self, model: Option<ModelKey>) {
        if !self.substitution_was_edited { self.allow_model_substitution = model.is_none(); }
        self.requested = model;
        self.requested_reasoning_effort = None;
    }

    pub fn allows(&self, account: &AccountKey, model: &ModelKey) -> bool {
        account.provider == model.provider
            && self.account.as_ref().is_none_or(|pinned| pinned == account)
            && (self.allow_model_substitution || self.bound_model().is_none_or(|requested| requested == model))
    }

    fn bound_model(&self) -> Option<&ModelKey> {
        self.requested.as_ref().or_else(|| (!self.allow_model_substitution).then(|| self.effective.as_ref().map(|effective| &effective.model)).flatten())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SelectionDecision {
    pub account: AccountKey,
    pub selection: ModelSelection,
    pub reason: String,
    pub rejected_candidates: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PendingTaskSelection {
    pub account: AccountKey,
    pub model: ModelSelection,
    pub reason: String,
}

pub fn resolve_selection(intent: &TaskSelection, profiles: &ModelProfiles, catalogs: &[AccountModelCatalog], eligible: &[AccountKey]) -> Result<SelectionDecision, Vec<String>> {
    let mut rejected = vec![];
    let accounts: Vec<_> = eligible.iter().filter(|key| intent.account.as_ref().is_none_or(|pinned| pinned == *key)).collect();
    if let Some(requested) = intent.bound_model() {
        let effort = intent.requested_reasoning_effort.clone().or_else(|| (!intent.allow_model_substitution && intent.requested.is_none())
            .then(|| intent.effective.as_ref().and_then(|effective| effective.reasoning_effort.clone())).flatten());
        for account in &accounts {
            if account.provider != requested.provider { continue; }
            let Some(catalog) = catalogs.iter().find(|catalog| catalog.account == **account) else {
                rejected.push(format!("{}: model catalog is not available", account.wire_id())); continue;
            };
            match catalog.resolve(&ModelCandidate { model: requested.model.clone(), reasoning_effort: effort.clone() }) {
                Ok(selection) => return Ok(SelectionDecision { account: (*account).clone(), selection, reason: "Requested model on the first compatible account".into(), rejected_candidates: rejected }),
                Err(why) => rejected.push(format!("{}: {why}", account.wire_id())),
            }
        }
        if !intent.allow_model_substitution {
            rejected.push("The requested model cannot continue and model substitution is off".into());
            return Err(rejected);
        }
    }
    for account in accounts {
        let Some(catalog) = catalogs.iter().find(|catalog| catalog.account == *account) else {
            rejected.push(format!("{}: model catalog is not available", account.wire_id())); continue;
        };
        let policy = profiles.get(&account.provider).and_then(|profiles| profiles.get(&intent.profile)).cloned().unwrap_or_default();
        match catalog.select(&policy) {
            Ok(selection) => return Ok(SelectionDecision { account: account.clone(), selection,
                reason: if intent.requested.is_some() { "Requested model unavailable; authorized model substitution" } else { "Task profile on the first compatible account" }.into(), rejected_candidates: rejected }),
            Err(why) => rejected.push(format!("{}: {why}", account.wire_id())),
        }
    }
    if accounts_are_empty(eligible, intent) { rejected.push("No eligible account satisfies this task's account selection".into()); }
    Err(rejected)
}

fn accounts_are_empty(eligible: &[AccountKey], intent: &TaskSelection) -> bool {
    !eligible.iter().any(|account| intent.account.as_ref().is_none_or(|pinned| pinned == account))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> AccountModelCatalog {
        AccountModelCatalog { account: AccountKey::native(Provider::Codex), revision: "catalog".into(), native_default: Some("only-model".into()), measured_at: 1,
            models: vec![CatalogModel { model: "only-model".into(), display_name: "Only model".into(), reasoning_efforts: vec!["high".into()], default_reasoning_effort: Some("high".into()), context_window: Some(1000), input_modalities: vec!["text".into()], is_default: true, hidden: false }] }
    }

    #[test]
    fn a_codex_only_native_default_does_not_require_a_suggested_preset() {
        let selected = catalog().select(&RoleModelPolicy::NativeDefault).unwrap();
        assert_eq!(selected.model, ModelKey { provider: Provider::Codex, model: "only-model".into() });
        assert_eq!(selected.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn explicit_model_and_effort_take_precedence_over_the_first_accounts_profile() {
        let codex = catalog();
        let mut claude = codex.clone();
        claude.account = AccountKey::native(Provider::Claude);
        claude.models[0].model = "other-native-model".into();
        claude.native_default = Some("other-native-model".into());
        let mut intent = TaskSelection::automatic(ModelProfile::Implementation);
        intent.request_model(Some(ModelKey { provider: Provider::Codex, model: "only-model".into() }));
        intent.requested_reasoning_effort = Some("high".into());
        intent.allow_model_substitution = true;
        let decision = resolve_selection(&intent, &ModelProfiles::new(), &[claude.clone(), codex.clone()], &[claude.account.clone(), codex.account.clone()]).unwrap();
        assert_eq!(decision.account, codex.account);
        assert_eq!(decision.selection.reasoning_effort.as_deref(), Some("high"));
        let decision = resolve_selection(&intent, &ModelProfiles::new(), &[claude.clone()], &[claude.account.clone()]).unwrap();
        assert_eq!(decision.account, claude.account);
        assert!(decision.reason.contains("authorized"));
        assert_eq!(intent.requested.as_ref().unwrap().provider, Provider::Codex);
        assert_eq!(intent.requested_reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn substitution_does_not_override_a_pinned_account_or_fabricate_a_default() {
        let mut offered = catalog();
        let mut intent = TaskSelection::automatic(ModelProfile::Review);
        intent.account = Some(AccountKey::native(Provider::Claude));
        assert!(resolve_selection(&intent, &ModelProfiles::new(), &[offered.clone()], &[offered.account.clone()]).is_err());
        intent.account = None;
        offered.native_default = None;
        offered.models[0].is_default = false;
        assert!(resolve_selection(&intent, &ModelProfiles::new(), &[offered.clone()], &[offered.account.clone()]).unwrap_err()[0].contains("default is not known"));
        let key = ModelKey::parse_wire("codex/vendor/model").unwrap();
        assert_eq!(key.model, "vendor/model");
        assert_eq!(key.wire_id(), "codex/vendor/model");
        assert!(ModelKey::parse_wire("codex/").is_err());
    }

    #[test]
    fn a_catalog_never_substitutes_an_unavailable_explicit_model_or_effort() {
        let catalog = catalog();
        assert!(catalog.resolve(&ModelCandidate { model: "opus".into(), reasoning_effort: None }).is_err());
        assert!(catalog.resolve(&ModelCandidate { model: "only-model".into(), reasoning_effort: Some("medium".into()) }).is_err());
        let mut selected = catalog.select(&RoleModelPolicy::NativeDefault).unwrap();
        selected.model.provider = Provider::Claude;
        assert!(catalog.validate(&selected).is_err());
    }

    #[test]
    fn explicit_substitution_choice_survives_model_picker_changes() {
        let mut task = TaskSelection::automatic(ModelProfile::Design);
        let codex = ModelKey { provider: Provider::Codex, model: "only-model".into() };
        task.request_model(Some(codex.clone()));
        assert!(!task.allow_model_substitution);
        assert!(task.allows(&AccountKey::native(Provider::Codex), &codex));
        assert!(!task.allows(&AccountKey::native(Provider::Claude), &ModelKey { provider: Provider::Claude, model: "opus".into() }));
        task.substitution_was_edited = true;
        task.allow_model_substitution = true;
        task.request_model(None);
        task.request_model(Some(codex));
        assert!(task.allow_model_substitution);
    }

    #[test]
    fn automatic_without_substitution_binds_the_first_effective_model_across_accounts() {
        let first = catalog();
        let mut intent = TaskSelection::automatic(ModelProfile::Implementation);
        intent.allow_model_substitution = false;
        intent.substitution_was_edited = true;
        intent.effective = Some(first.select(&RoleModelPolicy::NativeDefault).unwrap());
        let mut second = first.clone();
        second.account.account = crate::accounts::AccountId::parse("work").unwrap();
        second.models[0].model = "other-native-model".into();
        second.native_default = Some("other-native-model".into());
        assert!(resolve_selection(&intent, &Default::default(), &[second.clone()], &[second.account.clone()]).is_err());
        assert!(!intent.allows(&second.account, &ModelKey { provider: Provider::Codex, model: "other-native-model".into() }));
        second.models.push(first.models[0].clone());
        let selected = resolve_selection(&intent, &Default::default(), &[second.clone()], &[second.account.clone()]).unwrap();
        assert_eq!(selected.selection.model, intent.effective.as_ref().unwrap().model);
        assert_eq!(selected.selection.reasoning_effort, intent.effective.as_ref().unwrap().reasoning_effort);
        assert!(intent.requested.is_none());
    }
}
