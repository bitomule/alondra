use super::*;
use alondra_engine_ports::github::PrObserver;
use std::os::unix::fs::PermissionsExt;

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
const OTHER: &str = "89abcdef0123456789abcdef0123456789abcdef";

fn directory() -> crate::test_support::Directory {
    let directory = crate::test_support::Directory::new();
    std::fs::create_dir_all(&*directory).unwrap();
    directory
}

#[tokio::test]
async fn a_branch_gh_does_not_name_within_the_limit_is_left_out() {
    let fixture = directory();
    let program = fixture.join("gh");
    std::fs::write(&program, "#!/bin/sh\nprintf '%s' '{\"url\":\"https://github.com/owner/site/pull/6\",\"state\":\"OPEN\"}'\n").unwrap();
    std::fs::set_permissions(&program, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let gh = GhCli::at(program);
    assert_eq!(gh.branch_pr_within(&fixture, Duration::from_secs(600)).await, Some(("github.com".into(), "owner/site".into(), 6)));
    assert_eq!(gh.branch_pr_within(&fixture, Duration::ZERO).await, None);
}

#[tokio::test]
async fn gh_reports_the_repository_merge_methods_and_stays_silent_when_it_fails() {
    let fixture = directory();
    let program = fixture.join("gh");
    std::fs::write(&program, format!("#!/bin/sh\ncase \"$*\" in *\"repo view\"*owner/repo*) printf '%s' '{{\"mergeCommitAllowed\":true,\"squashMergeAllowed\":false,\"rebaseMergeAllowed\":true}}';; *\"repo view\"*) echo 'boom' >&2; exit 1;; *) printf '%s' '{{\"state\":\"OPEN\",\"headRefOid\":\"{HEAD}\",\"baseRefName\":\"main\"}}';; esac\n")).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let gh = GhCli::at(program);
    let known = gh.observe("github.com", "owner/repo", 7).await.unwrap();
    assert_eq!(known.allowed_methods, vec![MergeMethod::Merge, MergeMethod::Rebase]);
    let unknown = gh.observe("github.com", "other/repo", 7).await.unwrap();
    assert!(unknown.allowed_methods.is_empty());
}

#[tokio::test]
async fn gh_output_is_parsed_from_a_fake_binary_and_failures_are_reported() {
    let fixture = directory();
    let program = fixture.join("gh");
    let log = fixture.join("calls");
    std::fs::write(&program, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\ncase \"$*\" in *pull/7*) printf '%s' '{{\"state\":\"MERGED\",\"headRefOid\":\"{HEAD}\",\"baseRefName\":\"main\",\"mergeCommit\":{{\"oid\":\"{OTHER}\"}}}}';; *pull/8*) printf '%s' '{{\"state\":\"OPEN\",\"headRefOid\":\"short\",\"baseRefName\":\"main\"}}';; *) echo 'no pull requests found' >&2; exit 1;; esac\n", log.display())).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let gh = GhCli::at(program);
    let merged = gh.observe("github.com", "owner/repo", 7).await.unwrap();
    assert_eq!(merged, PrFacts { head: HEAD.into(), base: "main".into(), state: PrState::Merged, merged_head: Some(HEAD.into()), allowed_methods: vec![],
        mergeable: PrMergeable::Computing, checks: PrChecks::None, identity: None });
    assert_eq!(gh.observe("github.com", "owner/repo", 8).await.unwrap_err().to_string(), "gh reported no PR head");
    assert!(gh.observe("github.com", "owner/repo", 9).await.unwrap_err().to_string().contains("no pull requests found"));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains(&format!("pr view https://github.com/owner/repo/pull/7 --json {OBSERVED_FIELDS}")), "{calls}");
}

#[tokio::test]
async fn a_registration_with_the_bare_host_github_is_polled_at_github_com() {
    let fixture = directory();
    let program = fixture.join("gh");
    let log = fixture.join("calls");
    std::fs::write(&program, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nprintf '%s' '{{\"state\":\"OPEN\",\"headRefOid\":\"{HEAD}\",\"baseRefName\":\"main\"}}'\n", log.display())).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    GhCli::at(program).observe("github", "owner/repo", 7).await.unwrap();
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("pr view https://github.com/owner/repo/pull/7"), "{calls}");
}

#[tokio::test]
async fn gh_closes_a_pr_with_its_comment() {
    let fixture = directory();
    let program = fixture.join("gh");
    let log = fixture.join("calls");
    std::fs::write(&program, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\ncase \"$*\" in *pull/7*) exit 0;; *) echo 'no pull requests found' >&2; exit 1;; esac\n", log.display())).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let gh = GhCli::at(program);
    let key = ProviderPrKey("7007".into());
    gh.close("github.com", "owner/repo", 7, &key, "Cancelled in Alondra: Task").await.unwrap();
    assert!(gh.close("github.com", "owner/repo", 9, &key, "Cancelled in Alondra: Task").await.unwrap_err().to_string().contains("no pull requests found"));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("pr close https://github.com/owner/repo/pull/7 --comment Cancelled in Alondra: Task"), "{calls}");
}

#[test]
fn readiness_is_read_from_githubs_typed_fields() {
    let read = |extra: Value| {
        let mut value = serde_json::json!({"state": "OPEN", "headRefOid": HEAD, "baseRefName": "main"});
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let facts = parse(&value).unwrap();
        (facts.mergeable, facts.checks)
    };
    let run = |name: &str, status: &str, conclusion: &str| serde_json::json!({"__typename": "CheckRun", "name": name, "status": status, "conclusion": conclusion});
    assert_eq!(read(serde_json::json!({"mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN", "statusCheckRollup": []})),
        (PrMergeable::Clean, PrChecks::None));
    assert_eq!(read(serde_json::json!({"mergeable": "CONFLICTING", "mergeStateStatus": "DIRTY",
        "statusCheckRollup": [run("test", "COMPLETED", "SUCCESS"), run("lint", "IN_PROGRESS", "")]})), (PrMergeable::Conflicting, PrChecks::Pending { names: vec!["lint".into()] }));
    assert_eq!(read(serde_json::json!({"mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN",
        "statusCheckRollup": [run("lint", "IN_PROGRESS", ""), serde_json::json!({"context": "ci/legacy", "state": "PENDING"}), run("test", "COMPLETED", "SUCCESS")]})),
        (PrMergeable::Clean, PrChecks::Pending { names: vec!["lint".into(), "ci/legacy".into()] }));
    assert_eq!(serde_json::from_str::<PrChecks>(r#"{"kind":"pending"}"#).unwrap(), PrChecks::Pending { names: vec![] });
    assert_eq!(read(serde_json::json!({"mergeable": "UNKNOWN", "statusCheckRollup": [run("test", "COMPLETED", "FAILURE"), run("lint", "IN_PROGRESS", "")]})),
        (PrMergeable::Computing, PrChecks::Failed { names: vec!["test".into()] }));
    assert_eq!(read(serde_json::json!({"mergeable": "MERGEABLE", "mergeStateStatus": "BLOCKED",
        "statusCheckRollup": [run("deploy", "COMPLETED", "ACTION_REQUIRED"), run("test", "COMPLETED", "SUCCESS")]})),
        (PrMergeable::Other, PrChecks::ActionRequired { names: vec!["deploy".into()] }));
}

/// T16: what `gh pr view --json` says about which pull request answered, read from GitHub's own
/// JSON: the database id is the key, the node id the locator, and the URL GitHub answers with,
/// after the redirect of a moved repository, the address the pull request lives at now.
#[test]
fn identity_fields_are_parsed_from_gh_json() {
    let moved = serde_json::json!({
        "state": "OPEN", "headRefOid": HEAD, "baseRefName": "main", "headRefName": "csv-export",
        "fullDatabaseId": "15890129", "id": "PR_kwDOAOXXVc4A8nbR", "url": "https://github.com/Lumo-Org/Lumo/pull/42",
    });
    let read = parse(&moved).unwrap().identity.expect("an answer with its ids names its pull request");
    assert_eq!(read, ReadIdentity {
        key: ProviderPrKey("15890129".into()), locator: ProviderLocator("PR_kwDOAOXXVc4A8nbR".into()),
        repository: "lumo-org/lumo".into(), head_ref: "csv-export".into(),
    });
    assert!(OBSERVED_FIELDS.split(',').filter(|field| IDENTITY_FIELDS.split(',').any(|id| id == *field)).count() == 4,
        "the observer's one read asks for every identity field");
    let mut numeric = moved.clone();
    numeric["fullDatabaseId"] = serde_json::json!(15890129);
    assert_eq!(identity(&numeric).unwrap().key, ProviderPrKey("15890129".into()));
    for (field, value) in [("fullDatabaseId", serde_json::Value::Null), ("fullDatabaseId", serde_json::json!("")), ("fullDatabaseId", serde_json::json!("PR_x")),
        ("id", serde_json::Value::Null), ("url", serde_json::json!("https://github.com/lumo-org/lumo/issues/42")), ("url", serde_json::Value::Null)] {
        let mut partial = moved.clone();
        partial[field] = value.clone();
        assert_eq!(parse(&partial).unwrap().identity, None, "{field} = {value} names no pull request");
    }
    let located_at = |body: serde_json::Value| located(&HttpAnswer { status: 200, headers: Default::default(), body: body.to_string().into_bytes() });
    let node = serde_json::json!({"data": {"node": {"fullDatabaseId": "15890129", "id": "PR_kwDOAOXXVc4A8nbR",
        "url": "https://github.com/lumo-org/lumo/pull/42", "headRefName": "csv-export"}}});
    assert_eq!(located_at(node).unwrap(), read);
    let gone = located_at(serde_json::json!({"data": {"node": null}, "errors": [{"type": "NOT_FOUND", "message": "Could not resolve to a node with the global id of 'PR_x'"}]}));
    assert!(gone.unwrap_err().not_found(), "a node GitHub no longer has is gone for certain");
    let limited = located(&HttpAnswer { status: 403, headers: [("x-ratelimit-remaining".to_owned(), "0".to_owned())].into(), body: b"{}".to_vec() });
    assert!(!limited.unwrap_err().not_found(), "a rate limit says nothing about the pull request");
}

fn graphql(status: u16, body: serde_json::Value) -> HttpAnswer {
    HttpAnswer { status, headers: Default::default(), body: body.to_string().into_bytes() }
}

/// GitHub's typed `NOT_FOUND` is the only answer that says for certain a pull request is not
/// there; a limit, an outage or an answer that found it say nothing about it.
#[test]
fn only_a_typed_not_found_is_a_definitive_not_found() {
    let gone = graphql(200, serde_json::json!({"data": {"repository": null}, "errors": [{"type": "NOT_FOUND", "message": "Could not resolve to a Repository with the name 'ana/lumo'."}]}));
    assert_eq!(missing(&gone).as_deref(), Some("Could not resolve to a Repository with the name 'ana/lumo'."));
    let answers = [
        graphql(200, serde_json::json!({"data": {"repository": {"pullRequest": {"id": "PR_x"}}}})),
        graphql(200, serde_json::json!({"data": null, "errors": [{"type": "RATE_LIMITED", "message": "API rate limit exceeded"}]})),
        graphql(502, serde_json::json!({"errors": [{"type": "NOT_FOUND"}]})),
        graphql(200, serde_json::json!({})),
    ];
    for answer in answers {
        assert_eq!(missing(&answer), None, "{}", String::from_utf8_lossy(&answer.body));
    }
}

/// A failed read is a definitive not-found only when GitHub's own lookup of the same address
/// answers `NOT_FOUND`; when it finds the pull request, the read's failure stays as it was.
#[tokio::test]
async fn a_failed_read_is_gone_only_when_github_says_not_found() {
    let fixture = directory();
    let program = fixture.join("gh");
    let gone = r#"{"data":{"repository":null},"errors":[{"type":"NOT_FOUND","message":"Could not resolve to a Repository with the name 'ana/lumo'."}]}"#;
    let there = r#"{"data":{"repository":{"pullRequest":{"id":"PR_x"}}}}"#;
    std::fs::write(&program, format!("#!/bin/sh\ncase \"$*\" in \"pr view\"*) echo 'request failed' >&2; exit 1;; *name=lumo*) printf 'HTTP/2.0 200 OK\\r\\n\\r\\n%s' '{gone}';; *) printf 'HTTP/2.0 200 OK\\r\\n\\r\\n%s' '{there}';; esac\n")).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let gh = GhCli::at(program);
    let failure = gh.observe("github.com", "ana/lumo", 42).await.unwrap_err();
    assert!(failure.not_found(), "{failure:?}");
    let failure = gh.observe("github.com", "ana/site", 42).await.unwrap_err();
    assert!(matches!(failure, GhFailure::Exit(Some(1), _)), "{failure:?}");
    let failure = gh.close_checked("github.com", "ana/lumo", 42, &ProviderPrKey("1".into()), "bye").await.unwrap_err();
    assert!(failure.not_found(), "{failure:?}");
}
