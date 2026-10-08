//! A node names a PR's commit by its full sha. A branch name or a short sha stands for whatever
//! the branch holds later, so the user would accept one commit and Alondra merge another; and a
//! head that is not a commit never matches what GitHub reports, so the card loses its merge.

use alondra_core::domain::{DomainError, DomainState, PrId};
use serde_json::Value;

/// Checks the heads a node sends with `register_pr` and `present_result`. A pull request target
/// of `present_result` without a head gets the commit Alondra knows the PR is at.
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
    let Some(targets) = body.get_mut("targets").and_then(Value::as_array_mut) else { return Ok(()) };
    for target in targets.iter_mut().filter(|t| t.get("kind").and_then(Value::as_str) == Some("pull_request")) {
        let Some(pr_id) = target.get("pr_id").and_then(Value::as_str).map(str::to_owned) else { continue };
        let Some(pr) = state.pull_requests.get(&PrId(pr_id.clone())) else { continue };
        match target.get("head") {
            None | Some(Value::Null) => {
                target["head"] = Value::String(pr.head.clone());
            }
            Some(head) => {
                let head = head.as_str().unwrap_or_default();
                commit(head, "targets.head", &format!(
                    "the commit PR #{} is at ({}), or leave head out and Alondra uses it", pr.number, pr.head))?;
                if head != pr.head {
                    let mut error = DomainError::new("pr_mismatch", format!(
                        "targets.head: PR #{} is at {}, not {head}: push your work, then present again with its current commit or leave head out",
                        pr.number, pr.head));
                    error.subject = Some("targets.head".into());
                    return Err(error);
                }
            }
        }
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
    fn a_head_that_is_not_the_prs_commit_is_refused_with_the_commit_it_is_at() {
        let other = "89abcdef0123456789abcdef0123456789abcdef";
        let error = present(json!({"kind":"pull_request","pr_id":"pr-763","head":other})).unwrap_err();
        assert_eq!(error.code, "pr_mismatch");
        assert!(error.message.contains(&format!("PR #763 is at {HEAD}, not {other}")), "{}", error.message);
    }

    #[test]
    fn a_presented_pr_without_a_head_gets_the_commit_it_is_at() {
        let target = present(json!({"kind":"pull_request","pr_id":"pr-763"})).unwrap();
        assert_eq!(target["head"], HEAD);
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
