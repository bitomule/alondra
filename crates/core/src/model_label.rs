pub fn model_label(id: &str) -> String {
    let base = id.split('[').next().unwrap_or(id);
    let Some(rest) = base.strip_prefix("claude-") else {
        return id.to_owned();
    };
    let mut parts: Vec<&str> = rest.split('-').collect();
    if parts.last().is_some_and(|part| part.len() == 8 && part.bytes().all(|byte| byte.is_ascii_digit())) {
        parts.pop();
    }
    let Some((family, version)) = parts.split_first() else {
        return id.to_owned();
    };
    let family_ok = !family.is_empty() && family.bytes().all(|byte| byte.is_ascii_lowercase());
    let version_ok = !version.is_empty() && version.iter().all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
    if !family_ok || !version_ok {
        return id.to_owned();
    }
    let mut name = family.to_string();
    name[..1].make_ascii_uppercase();
    format!("{name} {}", version.join("."))
}

/// The family a model id or Claude Code alias belongs to: `sonnet` for `claude-sonnet-5-5`,
/// `sonnet[1m]` and `sonnet`.
pub fn model_family(id: &str) -> String {
    let base = id.split('[').next().unwrap_or(id).trim().to_ascii_lowercase();
    match base.strip_prefix("claude-") {
        Some(rest) => rest.split('-').next().unwrap_or(rest).to_owned(),
        None => base,
    }
}

/// A model as the user reads it: a full id as `model_label` shows it, an alias capitalised.
pub fn launched_model_label(model: &str) -> String {
    if model.starts_with("claude-") {
        return model_label(model);
    }
    let mut name = model.split('[').next().unwrap_or(model).to_owned();
    if let Some(first) = name.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    name
}

#[cfg(test)]
mod tests {
    use super::{launched_model_label, model_family, model_label};

    #[test]
    fn families_ignore_version_and_context_suffix() {
        for (id, family) in [("claude-sonnet-5-5", "sonnet"), ("claude-opus-5-5[1m]", "opus"), ("sonnet", "sonnet"), ("Opus[1m]", "opus"), ("gpt-5", "gpt-5")] {
            assert_eq!(model_family(id), family, "{id}");
        }
    }

    #[test]
    fn launched_models_read_as_names() {
        assert_eq!(launched_model_label("sonnet"), "Sonnet");
        assert_eq!(launched_model_label("claude-opus-5-5"), "Opus 5.5");
    }

    #[test]
    fn known_ids_read_as_family_and_version() {
        assert_eq!(model_label("claude-sonnet-5-5"), "Sonnet 5.5");
        assert_eq!(model_label("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_label("claude-opus-5-5[1m]"), "Opus 5.5");
        assert_eq!(model_label("claude-haiku-4-5-20251001"), "Haiku 4.5");
        assert_eq!(model_label("claude-fable-5-1"), "Fable 5.1");
        assert_eq!(model_label("claude-opus-5"), "Opus 5");
    }

    #[test]
    fn unknown_ids_are_shown_as_they_are() {
        assert_eq!(model_label("gpt-5"), "gpt-5");
        assert_eq!(model_label("claude-"), "claude-");
        assert_eq!(model_label("claude-sonnet"), "claude-sonnet");
        assert_eq!(model_label("claude-3-5-sonnet"), "claude-3-5-sonnet");
    }
}
