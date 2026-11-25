pub(crate) fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
}

pub(crate) fn is_scenario_id(value: &str) -> bool {
    is_identifier(value) && !value.contains('.')
}

/// Splits `<spec>/<scenario>` when both halves are valid identifiers.
pub(crate) fn split_subject(subject: &str) -> Option<(&str, &str)> {
    let (spec, scenario) = subject.split_once('/')?;
    (is_identifier(spec) && is_scenario_id(scenario)).then_some((spec, scenario))
}
