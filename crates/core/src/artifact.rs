//! Claude artifacts a report points to: pages published on claude.ai that open in the browser.

use crate::model::Report;

const KINDS: [&str; 2] = ["claude.ai/code/artifact/", "claude.ai/artifact/"];

/// Every artifact link in `text`, as `https://claude.ai/…/<id>`, in the order written, once each.
pub fn urls_in(text: &str) -> Vec<String> {
    let mut found: Vec<(usize, String)> = vec![];
    for kind in KINDS {
        for (i, _) in text.match_indices(kind) {
            if kind == "claude.ai/artifact/" && text[..i].ends_with("claude.ai/code/") {
                continue;
            }
            let before = &text[..i];
            let host_starts = before.ends_with("https://") || before.ends_with("http://") || before.ends_with("www.") || !before.ends_with(|c: char| c.is_alphanumeric() || c == '.' || c == '-');
            if !host_starts {
                continue;
            }
            let rest = &text[i + kind.len()..];
            let id: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')).collect();
            if id.is_empty() {
                continue;
            }
            let url = format!("https://{kind}{id}");
            if !found.iter().any(|(_, u)| *u == url) {
                found.push((i, url));
            }
        }
    }
    found.sort_by_key(|(at, _)| *at);
    found.into_iter().map(|(_, url)| url).collect()
}

/// The artifacts a report links.
pub fn in_report(r: &Report) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for url in r.links.iter().flat_map(|l| urls_in(&l.url)) {
        if !out.contains(&url) {
            out.push(url);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_links_are_found_wherever_a_node_writes_them() {
        let cases: &[(&str, &str, &[&str])] = &[
            ("a bare link", "https://claude.ai/artifact/abc123", &["https://claude.ai/artifact/abc123"]),
            ("a Claude Code artifact", "https://claude.ai/code/artifact/0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b", &["https://claude.ai/code/artifact/0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b"]),
            ("inside Markdown", "Plan: [the page](https://claude.ai/artifact/XyZ_9-a).", &["https://claude.ai/artifact/XyZ_9-a"]),
            ("ending a sentence", "Read it at https://claude.ai/artifact/abc123.", &["https://claude.ai/artifact/abc123"]),
            ("with a query", "https://claude.ai/artifact/abc123?tab=2", &["https://claude.ai/artifact/abc123"]),
            ("without a scheme", "see claude.ai/artifact/abc123 please", &["https://claude.ai/artifact/abc123"]),
            ("two, repeated", "https://claude.ai/artifact/b and https://claude.ai/code/artifact/a, again https://claude.ai/artifact/b", &["https://claude.ai/artifact/b", "https://claude.ai/code/artifact/a"]),
            ("no id", "https://claude.ai/artifact/ is empty", &[]),
            ("another host", "https://notclaude.ai/artifact/abc and https://claude.ai/chat/abc", &[]),
            ("a PR", "https://github.com/bitomule/alondra/pull/61", &[]),
        ];
        for (what, text, want) in cases {
            assert_eq!(urls_in(text), *want, "{what}");
        }
    }

    #[test]
    fn a_report_carries_the_artifacts_it_links_once_and_none_its_body_only_mentions() {
        let mut r: Report = serde_json::from_value(serde_json::json!({
            "id": 1, "session_id": "s", "node_name": null, "title": "Plan", "kind": "document", "created_at": 0,
            "body": "Also https://claude.ai/code/artifact/two and https://claude.ai/artifact/one",
            "links": [{"url": "https://claude.ai/artifact/one", "title": "The plan"}, {"url": "https://github.com/a/b/pull/1"}, {"url": "https://claude.ai/artifact/one"}]
        }))
        .unwrap();
        assert_eq!(in_report(&r), ["https://claude.ai/artifact/one"]);
        r.links.clear();
        assert!(in_report(&r).is_empty(), "an artifact the body only mentions makes no review card");
    }
}
