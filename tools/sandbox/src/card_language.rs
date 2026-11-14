const SOURCES: [(&str, &str); 5] = [
    ("fake/work.rs", include_str!("fake/work.rs")),
    ("journey/seed.rs", include_str!("journey/seed.rs")),
    ("journey/demo.rs", include_str!("journey/demo.rs")),
    ("journey/session.rs", include_str!("journey/session.rs")),
    ("journey/sdk/workers.rs", include_str!("journey/sdk/workers.rs")),
];

fn enclosing_json(source: &str, at: usize) -> &str {
    let start = source[..at].rfind("json!(").expect("a card is built inside json!") + "json!(".len();
    let (mut depth, mut in_string, mut escaped) = (1, false, false);
    for (offset, c) in source[start..].char_indices() {
        match (in_string, escaped, c) {
            (true, true, _) => escaped = false,
            (true, false, '\\') => escaped = true,
            (true, false, '"') => in_string = false,
            (true, false, _) => {}
            (false, _, '"') => in_string = true,
            (false, _, '(') => depth += 1,
            (false, _, ')') => {
                depth -= 1;
                if depth == 0 {
                    return &source[start..start + offset];
                }
            }
            _ => {}
        }
    }
    panic!("json! is never closed");
}

#[test]
fn every_card_the_sandbox_sends_declares_its_language() {
    let missing: Vec<String> = SOURCES
        .iter()
        .flat_map(|(file, source)| {
            source.match_indices("\"interaction_id\"").filter_map(move |(at, _)| {
                let card = enclosing_json(source, at);
                (!card.contains("\"language\"")).then(|| format!("{file}: {}", card.lines().next().unwrap_or("").trim()))
            })
        })
        .collect();
    assert!(missing.is_empty(), "cards without language: {missing:#?}");
}
