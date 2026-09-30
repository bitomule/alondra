//! Text export shared by native and daemon action adapters.

/// Format inline comments and a general note using the review format read by Agents.
pub fn text(comments: &[(String, String)], general: &str) -> String {
    let mut sections = Vec::new();
    if !comments.is_empty() {
        sections.push("I reviewed your code and have the following comments. Please address them.".to_owned());
        sections.push(comments.iter().enumerate()
            .map(|(index, (anchor, text))| format!("{}. `{anchor}` - {text}", index + 1))
            .collect::<Vec<_>>()
            .join("\n"));
    }
    let note = general.trim();
    if !note.is_empty() {
        sections.push(note.to_owned());
    }
    sections.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::text;

    #[test]
    fn matches_review_export_order_and_spacing() {
        assert_eq!(
            text(&[("src/auth.rs:new:42".into(), "Handle expiry".into())], "  Keep the token stable.  "),
            "I reviewed your code and have the following comments. Please address them.\n\n1. `src/auth.rs:new:42` - Handle expiry\n\nKeep the token stable."
        );
        assert_eq!(text(&[], "  Please retry.  "), "Please retry.");
        assert_eq!(text(&[], " \n "), "");
    }
}
