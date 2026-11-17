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
        mergeable: PrMergeable::Computing, checks: PrChecks::None });
    assert_eq!(gh.observe("github.com", "owner/repo", 8).await.unwrap_err(), "gh reported no PR head");
    assert!(gh.observe("github.com", "owner/repo", 9).await.unwrap_err().contains("no pull requests found"));
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
    gh.close("github.com", "owner/repo", 7, "Cancelled in Alondra: Task").await.unwrap();
    assert!(gh.close("github.com", "owner/repo", 9, "Cancelled in Alondra: Task").await.unwrap_err().contains("no pull requests found"));
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
