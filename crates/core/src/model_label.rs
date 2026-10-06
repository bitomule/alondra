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

#[cfg(test)]
mod tests {
    use super::model_label;

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
