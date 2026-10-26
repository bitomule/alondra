//! A node names a PR's commit by its full sha. A branch name or a short sha stands for whatever
//! the branch holds later, so the user would accept one commit and Alondra merge another; and a
//! head that is not a commit never matches what GitHub reports, so the card loses its merge.

use alondra_core::domain::{DomainError, DomainState, PrId};
use serde_json::Value;

/// Checks the heads a node sends with `register_pr` and `present_result`. A pull request target
/// of `present_result` may leave its head out: the domain then presents the commit the PR is at
/// when the delivery commits. Whether a given head is the PR's current one is the domain's to say,
/// once Alondra has asked GitHub about a head it has not seen yet.
pub(crate) fn check(state: &DomainState, method: &str, path: &str, body: &mut Value) -> Result<(), DomainError> {
    if method != "POST" || !path.starts_with("/tasks/") {
        return Ok(());
    }
    if path.ends_with("/pull-requests") {
        let head = body.get("head").and_then(Value::as_str).unwrap_or_default();
        return commit(head, "head", "the commit you pushed: run git rev-parse HEAD after git push and register it");
    }
    if !path.ends_with("/results") {
        return Ok(());
    }
    let Some(targets) = body.get("targets").and_then(Value::as_array) else { return Ok(()) };
    for target in targets.iter().filter(|t| t.get("kind").and_then(Value::as_str) == Some("pull_request")) {
        let Some(head) = target.get("head").filter(|head| !head.is_null()) else { continue };
        let at = target.get("pr_id").and_then(Value::as_str)
            .and_then(|pr_id| state.pull_requests.get(&PrId(pr_id.to_owned())))
            .map_or_else(|| "the commit the PR is at".to_owned(), |pr| format!("the commit PR #{} is at ({})", pr.number, pr.head));
        commit(head.as_str().unwrap_or_default(), "targets.head", &format!("{at}, or leave head out and Alondra uses it"))?;
    }
    Ok(())
}

fn commit(head: &str, field: &str, give: &str) -> Result<(), DomainError> {
    if is_sha(head) {
        return Ok(());
    }
    let mut error = DomainError::new("invalid_input", format!(
        "{field}: \"{head}\" is not a commit. Give the full 40-character commit sha, {give}"));
    error.subject = Some(field.into());
    Err(error)
}

pub(crate) fn is_sha(head: &str) -> bool {
    head.len() == 40 && head.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::{MergeMethod, PrState, PullRequest};
    use serde_json::json;

    const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

    fn state() -> DomainState {
        let mut state = DomainState::new("epoch");
        state.pull_requests.insert(PrId("pr-763".into()), PullRequest {
            id: PrId("pr-763".into()), task_id: "t".into(), host: "github.com".into(), repository: "o/r".into(),
            number: 763, head: HEAD.into(), base: "main".into(), state: PrState::Open, merged_head: None,
            allowed_methods: vec![MergeMethod::Squash], review: None,
        });
        state
    }

    fn present(target: Value) -> Result<Value, DomainError> {
        let mut body = json!({"targets": [target]});
        check(&state(), "POST", "/tasks/t/results", &mut body).map(|()| body["targets"][0].clone())
    }

    #[test]
    fn a_branch_name_is_refused_as_the_head_of_a_presented_pr() {
        let error = present(json!({"kind":"pull_request","pr_id":"pr-763","head":"worktree-model-rules"})).unwrap_err();
        assert_eq!(error.code, "invalid_input");
        assert!(error.message.contains("\"worktree-model-rules\" is not a commit"), "{}", error.message);
        assert!(error.message.contains(HEAD), "it says which commit the PR is at: {}", error.message);
    }

    #[test]
    fn a_short_sha_is_refused() {
        assert!(present(json!({"kind":"pull_request","pr_id":"pr-763","head":&HEAD[..12]})).is_err());
    }

    #[test]
    fn a_head_the_pr_is_not_at_is_left_to_the_domain() {
        let other = "89abcdef0123456789abcdef0123456789abcdef";
        let target = present(json!({"kind":"pull_request","pr_id":"pr-763","head":other})).unwrap();
        assert_eq!(target["head"], other);
    }

    #[test]
    fn a_presented_pr_without_a_head_stays_without_one() {
        let target = present(json!({"kind":"pull_request","pr_id":"pr-763"})).unwrap();
        assert!(target.get("head").is_none(), "{target}");
    }

    #[test]
    fn the_prs_commit_passes_unchanged() {
        let target = present(json!({"kind":"pull_request","pr_id":"pr-763","head":HEAD})).unwrap();
        assert_eq!(target["head"], HEAD);
    }

    #[test]
    fn register_pr_takes_only_a_commit() {
        let mut body = json!({"head":"worktree-model-rules"});
        let error = check(&state(), "POST", "/tasks/t/pull-requests", &mut body).unwrap_err();
        assert!(error.message.contains("git rev-parse HEAD"), "{}", error.message);
        let mut body = json!({"head":HEAD});
        assert!(check(&state(), "POST", "/tasks/t/pull-requests", &mut body).is_ok());
    }
}
