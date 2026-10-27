use alondra_core::accounts::AccountKey;
use alondra_core::model_selection::{ModelCandidate, ModelKey, RoleModelPolicy, TaskSelection};
use serde_json::{Map, Value};

fn text<'a>(fields: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, &'static str> {
    fields.get(key).map(|value| value.as_str().ok_or("Choose an available option")).transpose()
}
fn effort(value: Option<&str>) -> Option<String> { value.filter(|value| *value != "auto" && !value.is_empty()).map(str::to_owned) }
fn boolean(fields: &Map<String, Value>, key: &str) -> Result<Option<bool>, &'static str> {
    fields.get(key).map(|value| value.as_bool().ok_or("Choose an available option")).transpose()
}
fn candidate(model: &str, native_effort: Option<&str>) -> Result<ModelCandidate, &'static str> {
    let key = ModelKey::parse_wire(model).map_err(|_| "Choose an advertised model")?;
    Ok(ModelCandidate { model: key.model, reasoning_effort: effort(native_effort) })
}

pub(super) fn profile(fields: &Map<String, Value>, option: Option<&str>, current: Option<&RoleModelPolicy>, provider: alondra_core::accounts::Provider) -> Result<RoleModelPolicy, &'static str> {
    let checked = |model: &str, native_effort: Option<&str>| {
        let key = ModelKey::parse_wire(model).map_err(|_| "Choose an advertised model")?;
        if key.provider != provider { return Err("Choose a model from this provider"); }
        candidate(model, native_effort)
    };
    if let Some(option) = option {
        if option == "auto" { return Ok(RoleModelPolicy::NativeDefault); }
        let mut candidates = match current { Some(RoleModelPolicy::OrderedCandidates { candidates }) => candidates.clone(), _ => vec![] };
        if let Some(model) = option.strip_prefix("model:") {
            let selected = checked(model, None)?;
            if candidates.first().is_none_or(|first| first.model != selected.model) {
                if !candidates.is_empty() { candidates.remove(0); }
                candidates.retain(|candidate| candidate != &selected);
                candidates.insert(0, selected);
            }
        } else if let Some(native_effort) = option.strip_prefix("effort:") {
            let first = candidates.first_mut().ok_or("Choose a model before its reasoning effort")?;
            first.reasoning_effort = effort(Some(native_effort));
        } else { return Err("Choose an available option"); }
        return Ok(RoleModelPolicy::OrderedCandidates { candidates });
    }
    if fields.keys().filter_map(|key| key.strip_prefix("candidate_").or_else(|| key.strip_prefix("reasoning_effort_"))).any(|position| position.parse::<usize>().map_or(true, |position| !(2..=32).contains(&position))) { return Err("A profile supports at most 32 ordered model candidates"); }
    let model = text(fields, "model")?.ok_or("Choose a model")?;
    if model == "auto" { return Ok(RoleModelPolicy::NativeDefault); }
    let mut candidates = vec![checked(model, text(fields, "reasoning_effort")?)?];
    for position in 2..=32 {
        if let Some(model) = text(fields, &format!("candidate_{position}"))?.filter(|model| !model.is_empty() && *model != "none") {
            candidates.push(checked(model, text(fields, &format!("reasoning_effort_{position}"))?)?);
        }
    }
    Ok(RoleModelPolicy::OrderedCandidates { candidates })
}

pub(super) fn task(fields: &Map<String, Value>, mut intent: TaskSelection, current: &AccountKey) -> Result<TaskSelection, &'static str> {
    if fields.contains_key("reasoning_effort") { return Err("Choose reasoning effort when creating another node"); }
    if fields.keys().any(|key| !matches!(key.as_str(), "model" | "allow_model_substitution" | "substitution_was_edited")) {
        return Err("Create another node to use a different account or provider");
    }
    if let Some(model) = text(fields, "model")? {
        let model = if model == "auto" { None } else { Some(ModelKey::parse_wire(model).map_err(|_| "Choose an advertised model")?) };
        if model.as_ref().is_some_and(|model| model.provider != current.provider) { return Err("Create another node to use a different account or provider"); }
        let current_effort = intent.effective.as_ref().and_then(|selection| selection.reasoning_effort.clone());
        intent.request_model(model);
        intent.requested_reasoning_effort = intent.requested.as_ref().and(current_effort);
    }
    if let Some(value) = boolean(fields, "allow_model_substitution")? { intent.allow_model_substitution = value; }
    if let Some(value) = boolean(fields, "substitution_was_edited")? { intent.substitution_was_edited = value; }
    Ok(intent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::accounts::Provider;
    use alondra_core::model_selection::ModelProfile;
    use serde_json::json;
    #[test]
    fn changing_primary_model_preserves_ordered_fallbacks() {
        let original = RoleModelPolicy::OrderedCandidates { candidates: vec![ModelCandidate { model: "first".into(), reasoning_effort: Some("high".into()) }, ModelCandidate { model: "second".into(), reasoning_effort: None }] };
        let changed = profile(&Map::new(), Some("model:codex/third"), Some(&original), Provider::Codex).unwrap();
        assert_eq!(changed, RoleModelPolicy::OrderedCandidates { candidates: vec![ModelCandidate { model: "third".into(), reasoning_effort: None }, ModelCandidate { model: "second".into(), reasoning_effort: None }] });
        assert!(profile(&Map::new(), Some("model:claude/third"), Some(&original), Provider::Codex).is_err());
    }
    #[test]
    fn model_edits_preserve_original_account_and_paid_authority() {
        let current = AccountKey::parse_wire("codex/work").unwrap();
        let input = json!({"model":"codex/gpt-test","allow_model_substitution":false});
        let intent = task(input.as_object().unwrap(), TaskSelection::automatic(ModelProfile::Implementation), &current).unwrap();
        assert!(intent.account.is_none()); assert!(intent.paid_account_grant.is_none());
        assert_eq!(intent.requested.unwrap().wire_id(), "codex/gpt-test"); assert!(!intent.allow_model_substitution);
        let mut pinned = TaskSelection::automatic(ModelProfile::Implementation); pinned.account = Some(current.clone()); pinned.paid_account_grant = Some(current.clone());
        let changed = task(input.as_object().unwrap(), pinned, &current).unwrap();
        assert_eq!(changed.account.as_ref(), Some(&current)); assert_eq!(changed.paid_account_grant.as_ref(), Some(&current));
    }
    #[test]
    fn model_changes_keep_the_committed_native_effort_without_accepting_an_effort_field() {
        use alondra_core::model_selection::{ModelKey, ModelSelection};
        let account = AccountKey::parse_wire("codex/work").unwrap();
        let mut original = TaskSelection::automatic(ModelProfile::Implementation);
        original.effective = Some(ModelSelection { model: ModelKey { provider: Provider::Codex, model: "old-native".into() },
            reasoning_effort: Some("high".into()), catalog_revision: "catalog".into() });
        let changed = task(json!({"model":"codex/new-native"}).as_object().unwrap(), original, &account).unwrap();
        assert_eq!(changed.requested_reasoning_effort.as_deref(), Some("high"));
        assert_eq!(changed.effective.as_ref().unwrap().reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn active_model_input_rejects_account_provider_and_consent_edits() {
        let current = AccountKey::parse_wire("codex/work").unwrap();
        assert_eq!(task(json!({"reasoning_effort":"high"}).as_object().unwrap(), TaskSelection::automatic(ModelProfile::Implementation), &current).unwrap_err(), "Choose reasoning effort when creating another node");
        for input in [json!({"account":"codex/other"}), json!({"pin_account":true}), json!({"account_was_edited":true}),
            json!({"paid_account_grant":"codex/work"}), json!({"model":"claude/native-large"})] {
            assert_eq!(task(input.as_object().unwrap(), TaskSelection::automatic(ModelProfile::Implementation), &current).unwrap_err(),
                "Create another node to use a different account or provider");
        }
    }
}
