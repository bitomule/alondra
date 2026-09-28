//! Local word matching for the fleet search.

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Local word matching across node details. Results retain fleet order.
pub fn search(query: &str, candidates: Vec<(String, String)>) -> Vec<String> {
    let terms = words(query);
    if terms.is_empty() {
        return vec![];
    }
    candidates
        .into_iter()
        .filter_map(|(session, text)| {
            let hay = words(&text);
            terms
                .iter()
                .all(|term| hay.iter().any(|word| word.contains(term)))
                .then_some(session)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_search_matches_words_without_a_service() {
        let candidates = vec![
            ("a".into(), "Lumo camera permission".into()),
            ("b".into(), "Quill release".into()),
        ];
        assert_eq!(search("cam perm", candidates), vec!["a"]);
    }
}
