//! Text export shared by native and daemon action adapters.

/// Format inline comments and a general note using the review format read by Agents.
pub fn text(comments: &[(String, String)], general: &str) -> String {
    let anchored: Vec<_> = comments.iter().map(|(anchor, text)| (anchor.clone(), text.clone(), None)).collect();
    text_with_quotes(&anchored, general)
}

/// The same export, where a comment that carries the excerpt it is about is written as a quoted
/// block followed by the comment, so a reader that never saw the anchor still knows what it means.
pub fn text_with_quotes(comments: &[(String, String, Option<String>)], general: &str) -> String {
    let quoted = comments.iter().any(|(_, _, quote)| quote.as_deref().is_some_and(|quote| !quote.trim().is_empty()));
    let mut sections = Vec::new();
    if quoted {
        sections.push("I reviewed this and have the following comments. Please address them.".to_owned());
        sections.push(comments.iter()
            .map(|(anchor, text, quote)| match quote.as_deref().map(excerpt).filter(|quote| !quote.is_empty()) {
                Some(quote) => format!("> {quote}\n{text}"),
                None => format!("`{anchor}` - {text}"),
            })
            .collect::<Vec<_>>()
            .join("\n\n"));
    } else if !comments.is_empty() {
        sections.push("I reviewed your code and have the following comments. Please address them.".to_owned());
        sections.push(comments.iter().enumerate()
            .map(|(index, (anchor, text, _))| format!("{}. `{anchor}` - {text}", index + 1))
            .collect::<Vec<_>>()
            .join("\n"));
    }
    let note = general.trim();
    if !note.is_empty() {
        sections.push(note.to_owned());
    }
    sections.join("\n\n")
}

const EXCERPT_LIMIT: usize = 200;

fn excerpt(quote: &str) -> String {
    let collapsed = quote.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= EXCERPT_LIMIT {
        return collapsed;
    }
    let cut: String = collapsed.chars().take(EXCERPT_LIMIT - 1).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::{text, text_with_quotes};

    #[test]
    fn matches_review_export_order_and_spacing() {
        assert_eq!(
            text(&[("src/auth.rs:new:42".into(), "Handle expiry".into())], "  Keep the token stable.  "),
            "I reviewed your code and have the following comments. Please address them.\n\n1. `src/auth.rs:new:42` - Handle expiry\n\nKeep the token stable."
        );
        assert_eq!(text(&[], "  Please retry.  "), "Please retry.");
        assert_eq!(text(&[], " \n "), "");
    }

    #[test]
    fn a_comment_with_its_excerpt_is_a_quote_followed_by_the_comment() {
        let comments = [
            ("paragraph:1:0".to_owned(), "Say which simulator".to_owned(), Some("Build   the\narchive".to_owned())),
            ("list:2:0".to_owned(), "Skip this".to_owned(), Some("x".repeat(300))),
        ];
        let cut = format!("{}…", "x".repeat(199));
        assert_eq!(
            text_with_quotes(&comments, "Looks right."),
            format!("I reviewed this and have the following comments. Please address them.\n\n> Build the archive\nSay which simulator\n\n> {cut}\nSkip this\n\nLooks right.")
        );
    }

    #[test]
    fn comments_without_an_excerpt_keep_the_anchored_format() {
        let comments = [("a:new:1".to_owned(), "Fix".to_owned(), None)];
        assert_eq!(text_with_quotes(&comments, ""), text(&[("a:new:1".into(), "Fix".into())], ""));
    }
}
