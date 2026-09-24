//! Pull requests a node opened, shown as their own card: what they change, their checks, and
//! merge or request changes from any client.

use serde::{Deserialize, Serialize};

use crate::model::Millis;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Open,
    Merged,
    Closed,
    Dismissed,
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    pub fn flag(self) -> &'static str {
        match self {
            MergeMethod::Merge => "--merge",
            MergeMethod::Squash => "--squash",
            MergeMethod::Rebase => "--rebase",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MergeMethod::Merge => "merge commit",
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Passed,
    Failed,
    Running,
    #[serde(other)]
    Other,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Check {
    pub name: String,
    pub state: CheckState,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct PullRequest {
    pub id: u64,
    pub url: String,
    pub repo: String,
    pub number: u64,
    pub session_id: String,
    #[serde(default)]
    pub node_name: Option<String>,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub head: Option<String>,
    pub state: PrState,
    #[serde(default)]
    pub checks: Vec<Check>,
    #[serde(default)]
    pub merge_methods: Vec<MergeMethod>,
    #[serde(default)]
    pub additions: u64,
    #[serde(default)]
    pub deletions: u64,
    #[serde(default)]
    pub changed_files: u64,
    #[serde(default)]
    pub mergeable: Option<String>,
    /// GitHub's `mergeStateStatus`: `DIRTY` means conflicts, `BEHIND`, `BLOCKED`, `CLEAN`…
    #[serde(default)]
    pub merge_state: Option<String>,
    pub opened_at: Millis,
    #[serde(default)]
    pub fetched: bool,
    #[serde(default)]
    pub head_sha: Option<String>,
    /// The head commit when changes were last requested; empty when it was not known yet.
    #[serde(default)]
    pub changes_requested_on: Option<String>,
}

impl PullRequest {
    pub fn opened(id: u64, url: &str, session_id: &str, node_name: Option<String>, at: Millis) -> Option<Self> {
        let (repo, number) = parse_url(url)?;
        Some(PullRequest {
            id,
            url: canonical(&repo, number),
            repo,
            number,
            session_id: session_id.to_string(),
            node_name,
            title: format!("PR #{number}"),
            body: None,
            base: None,
            head: None,
            state: PrState::Open,
            checks: vec![],
            merge_methods: vec![],
            additions: 0,
            deletions: 0,
            changed_files: 0,
            mergeable: None,
            merge_state: None,
            opened_at: at,
            fetched: false,
            head_sha: None,
            changes_requested_on: None,
        })
    }

    /// GitHub cannot create the merge commit: the branch conflicts with its base.
    pub fn conflicts(&self) -> bool {
        self.mergeable.as_deref() == Some("CONFLICTING") || self.merge_state.as_deref() == Some("DIRTY")
    }

    /// GitHub is still computing whether the PR merges: it answers `UNKNOWN` right after the base
    /// moves, until someone asks again.
    pub fn mergeability_unknown(&self) -> bool {
        !self.conflicts() && matches!(self.mergeable.as_deref(), None | Some("UNKNOWN"))
    }

    /// Changes were requested and the node has not pushed since: the card waits off the inbox.
    pub fn awaiting_push(&self) -> bool {
        self.changes_requested_on.as_deref().is_some_and(|on| on == self.head_sha.as_deref().unwrap_or(""))
    }
}

pub fn canonical(repo: &str, number: u64) -> String {
    format!("https://github.com/{repo}/pull/{number}")
}

/// `owner/name` and the number of a GitHub pull request URL.
pub fn parse_url(url: &str) -> Option<(String, u64)> {
    let rest = url.trim().strip_prefix("https://github.com/")?;
    let mut parts = rest.split('/');
    let (owner, name, kind, number) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let number: u64 = number.split(['#', '?']).next()?.trim_end_matches(['.', ':', ';', '!']).parse().ok()?;
    let valid = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    (kind == "pull" && valid(owner) && valid(name)).then(|| (format!("{owner}/{name}"), number))
}

/// Every pull request URL mentioned in `text`, canonical and without repeats.
pub fn urls_in(text: &str) -> Vec<String> {
    let mut found: Vec<String> = vec![];
    for (i, _) in text.match_indices("https://github.com/") {
        let end = text[i..].find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '"' | '\'' | '>' | '<' | ',' | '`')).map_or(text.len(), |e| i + e);
        if let Some((repo, number)) = parse_url(&text[i..end]) {
            let url = canonical(&repo, number);
            if !found.contains(&url) {
                found.push(url);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_request_urls_are_found_wherever_a_node_writes_them() {
        let text = "Opened https://github.com/bitomule/alondra/pull/12 (see [PR](https://github.com/bitomule/alondra/pull/12#discussion)) and https://github.com/a/b.c/pull/7.\nNot https://github.com/a/b/issues/3 nor https://github.com/a/b/pull/x";
        assert_eq!(urls_in(text), ["https://github.com/bitomule/alondra/pull/12", "https://github.com/a/b.c/pull/7"]);
        assert_eq!(urls_in("no links"), Vec::<String>::new());
    }

    #[test]
    fn a_pr_gets_one_card_that_merging_or_dismissing_closes() {
        use crate::event::Event;
        use crate::view::View;
        let mut v = View::default();
        let opened = |url: &str| Event::PrOpened { url: url.into(), session_id: "s".into() };
        v.apply(1, 10, &opened("https://github.com/a/b/pull/3"));
        v.apply(2, 11, &opened("https://github.com/a/b/pull/3/files"));
        assert_eq!(v.prs.len(), 1, "the same PR mentioned twice");
        let mut fetched = v.prs[&1].clone();
        fetched.title = "Fix the menu".into();
        fetched.fetched = true;
        v.apply(3, 12, &Event::PrUpdated(fetched));
        assert_eq!(v.prs[&1].title, "Fix the menu");
        v.apply(4, 13, &Event::PrMerged { id: 1, method: MergeMethod::Squash });
        assert_eq!(v.prs[&1].state, PrState::Merged);
        assert_eq!(v.merge_methods.get("a/b"), Some(&MergeMethod::Squash));

        v.apply(5, 14, &opened("https://github.com/a/b/pull/4"));
        v.apply(6, 15, &Event::PrDismissed { id: 2 });
        let mut still_open = v.prs[&2].clone();
        still_open.state = PrState::Open;
        v.apply(7, 16, &Event::PrUpdated(still_open));
        assert_eq!(v.prs[&2].state, PrState::Dismissed, "a refresh does not bring back a dismissed card");
    }

    #[test]
    fn a_card_waits_for_new_commits_after_changes_are_requested() {
        use crate::event::Event;
        use crate::view::View;
        let cases: &[(Option<&str>, Option<&str>, bool)] = &[
            (Some("a1"), Some("a1"), true),
            (Some("a1"), Some("b2"), false),
            (None, None, true),
            (None, Some("b2"), false),
        ];
        for &(head_at_review, head_now, waits) in cases {
            let mut v = View::default();
            v.apply(1, 10, &Event::PrOpened { url: "https://github.com/a/b/pull/3".into(), session_id: "s".into() });
            let mut pr = v.prs[&1].clone();
            pr.head_sha = head_at_review.map(str::to_string);
            v.apply(2, 11, &Event::PrUpdated(pr.clone()));
            assert!(!v.prs[&1].awaiting_push(), "no review yet");
            v.apply(3, 12, &Event::PrChangesRequested { id: 1 });
            pr.head_sha = head_now.map(str::to_string);
            v.apply(4, 13, &Event::PrUpdated(pr));
            assert_eq!(v.prs[&1].awaiting_push(), waits, "head {head_at_review:?} at review, {head_now:?} now");
        }
    }

    #[test]
    fn a_pull_request_url_gives_its_repo_and_number() {
        assert_eq!(parse_url("https://github.com/bitomule/alondra/pull/12/files"), Some(("bitomule/alondra".into(), 12)));
        assert_eq!(parse_url("https://gitlab.com/a/b/pull/1"), None);
        assert_eq!(parse_url("https://github.com/a/b/pull/"), None);
    }
}
