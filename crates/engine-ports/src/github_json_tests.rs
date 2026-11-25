use super::*;
use alondra_core::pr::MergeMethod;
use serde_json::json;

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn readiness_is_read_from_githubs_typed_fields() {
    let read = |extra: Value| {
        let mut value = json!({"state": "OPEN", "headRefOid": HEAD, "baseRefName": "main"});
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let facts = pr_facts(&value).unwrap();
        (facts.mergeable, facts.checks)
    };
    let run = |name: &str, status: &str, conclusion: &str| json!({"__typename": "CheckRun", "name": name, "status": status, "conclusion": conclusion});
    assert_eq!(read(json!({"mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN", "statusCheckRollup": []})),
        (PrMergeable::Clean, PrChecks::None));
    assert_eq!(read(json!({"mergeable": "CONFLICTING", "mergeStateStatus": "DIRTY",
        "statusCheckRollup": [run("test", "COMPLETED", "SUCCESS"), run("lint", "IN_PROGRESS", "")]})), (PrMergeable::Conflicting, PrChecks::Pending { names: vec!["lint".into()] }));
    assert_eq!(read(json!({"mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN",
        "statusCheckRollup": [run("lint", "IN_PROGRESS", ""), json!({"context": "ci/legacy", "state": "PENDING"}), run("test", "COMPLETED", "SUCCESS")]})),
        (PrMergeable::Clean, PrChecks::Pending { names: vec!["lint".into(), "ci/legacy".into()] }));
    assert_eq!(serde_json::from_str::<PrChecks>(r#"{"kind":"pending"}"#).unwrap(), PrChecks::Pending { names: vec![] });
    assert_eq!(read(json!({"mergeable": "UNKNOWN", "statusCheckRollup": [run("test", "COMPLETED", "FAILURE"), run("lint", "IN_PROGRESS", "")]})),
        (PrMergeable::Computing, PrChecks::Failed { names: vec!["test".into()] }));
    assert_eq!(read(json!({"mergeable": "MERGEABLE", "mergeStateStatus": "BLOCKED",
        "statusCheckRollup": [run("deploy", "COMPLETED", "ACTION_REQUIRED"), run("test", "COMPLETED", "SUCCESS")]})),
        (PrMergeable::Other, PrChecks::ActionRequired { names: vec!["deploy".into()] }));
}

/// T16: what `gh pr view --json` says about which pull request answered, read from GitHub's own
/// JSON: the database id is the key, the node id the locator, and the URL GitHub answers with,
/// after the redirect of a moved repository, the address the pull request lives at now.
#[test]
fn identity_fields_are_parsed_from_gh_json() {
    let moved = json!({
        "state": "OPEN", "headRefOid": HEAD, "baseRefName": "main", "headRefName": "csv-export",
        "fullDatabaseId": "15890129", "id": "PR_kwDOAOXXVc4A8nbR", "url": "https://github.com/Lumo-Org/Lumo/pull/42",
    });
    let read = pr_facts(&moved).unwrap().identity.expect("an answer with its ids names its pull request");
    assert_eq!(read, ReadIdentity {
        key: ProviderPrKey("15890129".into()), locator: ProviderLocator("PR_kwDOAOXXVc4A8nbR".into()),
        repository: "lumo-org/lumo".into(), head_ref: "csv-export".into(),
    });
    let mut numeric = moved.clone();
    numeric["fullDatabaseId"] = json!(15890129);
    assert_eq!(identity(&numeric).unwrap().key, ProviderPrKey("15890129".into()));
    for (field, value) in [("fullDatabaseId", Value::Null), ("fullDatabaseId", json!("")), ("fullDatabaseId", json!("PR_x")),
        ("id", Value::Null), ("url", json!("https://github.com/lumo-org/lumo/issues/42")), ("url", Value::Null)] {
        let mut partial = moved.clone();
        partial[field] = value.clone();
        assert_eq!(pr_facts(&partial).unwrap().identity, None, "{field} = {value} names no pull request");
    }
}

#[test]
fn a_card_takes_what_github_says_about_the_pr() {
    let pr = PrCard::opened(1, "https://github.com/a/b/pull/7", "s", None, 0).unwrap();
    let view = json!({
        "title": "Menus become cards", "body": "What and why", "baseRefName": "main", "headRefName": "menus",
        "additions": 120, "deletions": 8, "changedFiles": 5, "mergeable": "MERGEABLE", "state": "OPEN", "headRefOid": "abc123",
        "statusCheckRollup": [
            {"__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "SUCCESS", "detailsUrl": "https://x/1"},
            {"__typename": "CheckRun", "name": "lint", "status": "COMPLETED", "conclusion": "FAILURE", "detailsUrl": "https://x/2"},
            {"__typename": "CheckRun", "name": "build", "status": "IN_PROGRESS", "conclusion": ""},
            {"__typename": "StatusContext", "context": "ci/legacy", "state": "PENDING", "targetUrl": "https://x/3"}
        ]
    });
    let repo = json!({"allow_merge_commit": true, "allow_squash_merge": false, "allow_rebase_merge": true});
    let r = refreshed(&pr, &view, Some(&repo));
    assert_eq!((r.title.as_str(), r.base.as_deref(), r.changed_files, r.fetched), ("Menus become cards", Some("main"), 5, true));
    assert_eq!(r.checks.iter().map(|c| (c.name.as_str(), c.state)).collect::<Vec<_>>(), [
        ("test", CheckState::Passed),
        ("lint", CheckState::Failed),
        ("build", CheckState::Running),
        ("ci/legacy", CheckState::Running)
    ]);
    assert_eq!(r.merge_methods, [MergeMethod::Merge, MergeMethod::Rebase]);
    let merged = refreshed(&r, &json!({"state": "MERGED"}), None);
    assert_eq!(merged.state, card::PrState::Merged);
    assert_eq!((r.head_sha.as_deref(), merged.head_sha.as_deref()), (Some("abc123"), Some("abc123")), "the head is kept when gh leaves it out");
    assert_eq!(merged.merge_methods, [MergeMethod::Merge, MergeMethod::Rebase], "kept when the repo could not be read");
}

#[test]
fn a_check_run_again_counts_once_by_its_latest_run() {
    let pr = PrCard::opened(1, "https://github.com/a/b/pull/7", "s", None, 0).unwrap();
    let r = refreshed(&pr, &json!({"state": "OPEN", "statusCheckRollup": [
        {"name": "build", "status": "COMPLETED", "conclusion": "FAILURE", "startedAt": "2026-10-27T10:00:00Z"},
        {"name": "lint", "status": "COMPLETED", "conclusion": "SUCCESS", "startedAt": "2026-10-27T10:00:00Z"},
        {"name": "build", "status": "IN_PROGRESS", "conclusion": "", "startedAt": "2026-10-27T10:05:00Z"},
        {"context": "deploy", "state": "FAILURE", "createdAt": "2026-10-27T10:06:00Z"},
        {"context": "deploy", "state": "SUCCESS", "createdAt": "2026-10-27T10:01:00Z"},
    ]}), None);
    let checks: Vec<_> = r.checks.iter().map(|check| (check.name.as_str(), check.state)).collect();
    assert_eq!(checks, [("build", CheckState::Running), ("lint", CheckState::Passed), ("deploy", CheckState::Failed)]);
}

#[test]
fn a_conflict_reaches_the_card_as_merge_state() {
    let pr = PrCard::opened(1, "https://github.com/a/b/pull/7", "s", None, 0).unwrap();
    let r = refreshed(&pr, &json!({"state": "OPEN", "mergeable": "CONFLICTING", "mergeStateStatus": "DIRTY"}), None);
    assert!(r.conflicts() && !r.mergeability_unknown());
    let computing = refreshed(&pr, &json!({"state": "OPEN", "mergeable": "UNKNOWN", "mergeStateStatus": "UNKNOWN"}), None);
    assert!(!computing.conflicts() && computing.mergeability_unknown());
}
