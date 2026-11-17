use super::*;
use alondra_domain_rules::{PrState, PullRequest};
use std::os::unix::fs::PermissionsExt;

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

fn directory() -> crate::test_support::Directory {
    let directory = crate::test_support::Directory::new();
    std::fs::create_dir_all(&*directory).unwrap();
    directory
}

#[test]
fn required_checks_come_from_rulesets_and_from_classic_protection() {
    let rules = serde_json::json!([
        {"type": "pull_request", "parameters": {}},
        {"type": "required_status_checks", "parameters": {"required_status_checks": [{"context": "build"}, {"context": "lint", "integration_id": 1}]}}
    ]);
    assert_eq!(ruleset_checks(&rules), vec!["build", "lint"]);
    assert!(ruleset_checks(&serde_json::json!([{"type": "non_fast_forward"}])).is_empty());
    let classic = serde_json::json!({"contexts": ["test"], "checks": [{"context": "test", "app_id": null}, {"context": "ui", "app_id": 3}]});
    assert_eq!(classic_checks(&classic), vec!["test", "test", "ui"]);
}

#[test]
fn only_failed_checks_fail_and_action_required_asks_the_user() {
    let checks = |list: serde_json::Value| rollup(&serde_json::json!({"statusCheckRollup": list}));
    let run = |conclusion: &str| serde_json::json!({"__typename": "CheckRun", "name": conclusion, "status": "COMPLETED", "conclusion": conclusion});
    assert_eq!(checks(serde_json::json!([])), Checks::None);
    for passing in ["SUCCESS", "NEUTRAL", "SKIPPED", "CANCELLED", "STALE"] {
        assert_eq!(checks(serde_json::json!([run(passing)])), Checks::Passed, "{passing}");
    }
    for failing in ["FAILURE", "TIMED_OUT", "STARTUP_FAILURE"] {
        assert_eq!(checks(serde_json::json!([run(failing)])), Checks::Failed(vec![failing.into()]), "{failing}");
    }
    assert_eq!(checks(serde_json::json!([run("ACTION_REQUIRED")])), Checks::ActionRequired(vec!["ACTION_REQUIRED".into()]));
    assert_eq!(checks(serde_json::json!([{"name": "ui", "status": "IN_PROGRESS"}, run("SUCCESS")])), Checks::Pending);
    assert_eq!(checks(serde_json::json!([{"context": "ci/legacy", "state": "PENDING"}])), Checks::Pending);
    assert_eq!(checks(serde_json::json!([{"context": "ci/legacy", "state": "ERROR"}])), Checks::Failed(vec!["ci/legacy".into()]));
}

#[test]
fn only_checks_every_requiring_ruleset_lets_the_user_bypass_and_classic_protection_does_not_require_are_bypassable() {
    let rules = serde_json::json!([
        {"type": "pull_request", "ruleset_id": 1},
        {"type": "required_status_checks", "ruleset_id": 1, "parameters": {"required_status_checks": [{"context": "CI"}, {"context": "lint"}, {"context": "docs"}]}},
        {"type": "required_status_checks", "ruleset_id": 2, "parameters": {"required_status_checks": [{"context": "lint"}]}},
        {"type": "required_status_checks", "parameters": {"required_status_checks": [{"context": "unknown-source"}]}},
    ]);
    assert_eq!(check_rulesets(&rules), BTreeSet::from([1, 2]));
    let can_bypass = BTreeMap::from([(1, true), (2, false)]);
    assert_eq!(bypassable(&rules, &can_bypass, &["docs".into()]), ["CI"]);
    for (mode, bypasses) in [("always", true), ("pull_requests_only", true), ("exempt", true), ("never", false)] {
        assert_eq!(viewer_bypasses(&serde_json::json!({"current_user_can_bypass": mode})), bypasses, "{mode}");
    }
    assert!(!viewer_bypasses(&serde_json::json!({})), "a ruleset that does not say lets nobody past");
}

#[tokio::test]
async fn gh_reads_each_rulesets_bypass_and_merges_past_them_with_admin() {
    let home = directory();
    let program = home.join("gh");
    let log = home.join("calls");
    std::fs::write(&program, format!(r#"#!/bin/sh
printf '%s\n' "$*" >> {log}
case "$*" in
  *protection*) printf 'HTTP/2.0 404 Not Found\r\n\r\n{{"message":"Not Found"}}'; exit 1;;
  *rules/branches*) printf 'HTTP/2.0 200 OK\r\n\r\n[{{"type":"required_status_checks","ruleset_id":26,"parameters":{{"required_status_checks":[{{"context":"CI"}}]}}}},{{"type":"required_status_checks","ruleset_id":27,"parameters":{{"required_status_checks":[{{"context":"audit"}}]}}}}]';;
  *rulesets/26*) printf 'HTTP/2.0 200 OK\r\n\r\n{{"id":26,"current_user_can_bypass":"always"}}';;
  *rulesets/27*) printf 'HTTP/2.0 200 OK\r\n\r\n{{"id":27,"current_user_can_bypass":"never"}}';;
esac
"#, log = log.display())).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let gh = GhCli::at(program);
    let pr = PullRequest { id: "pr".into(), task_id: "t".into(), host: "github".into(), repository: "owner/repo".into(),
        number: 7, head: HEAD.into(), base: "main".into(), state: PrState::Open, merged_head: None,
        allowed_methods: vec![MergeMethod::Squash], review: None };
    assert_eq!(gh.bypassable_checks(&pr, "main").await, Ok(vec!["CI".to_owned()]));
    gh.merge_bypassing(&pr, HEAD, MergeMethod::Squash).await.unwrap();
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("repos/owner/repo/rulesets/26") && calls.contains("repos/owner/repo/rulesets/27"), "{calls}");
    assert!(calls.contains(&format!("pr merge https://github.com/owner/repo/pull/7 --squash --match-head-commit {HEAD} --admin")), "{calls}");
}

#[tokio::test]
async fn gh_merges_with_the_method_only_at_the_presented_head_and_reads_status_and_queue() {
    let home = directory();
    let program = home.join("gh");
    let log = home.join("calls");
    std::fs::write(&program, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\ncase \"$*\" in *statusCheckRollup*) printf '%s' '{{\"state\":\"OPEN\",\"headRefOid\":\"{HEAD}\",\"baseRefName\":\"main\",\"statusCheckRollup\":[{{\"name\":\"build\",\"status\":\"COMPLETED\",\"conclusion\":\"SUCCESS\"}},{{\"name\":\"lint\",\"status\":\"COMPLETED\",\"conclusion\":\"FAILURE\"}}]}}';; *graphql*) echo true;; esac\n", log.display())).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let gh = GhCli::at(program);
    let pr = PullRequest { id: "pr".into(), task_id: "t".into(), host: "github".into(), repository: "owner/repo".into(),
        number: 7, head: HEAD.into(), base: "main".into(), state: PrState::Open, merged_head: None,
        allowed_methods: vec![MergeMethod::Squash], review: None };
    let status = gh.status(&pr).await.unwrap();
    assert_eq!((status.state, status.head.as_str(), status.checks), (PrState::Open, HEAD, Checks::Failed(vec!["lint".into()])));
    gh.merge(&pr, HEAD, MergeMethod::Squash).await.unwrap();
    assert!(gh.in_queue(&pr).await.unwrap());
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains(&format!("pr merge https://github.com/owner/repo/pull/7 --squash --match-head-commit {HEAD}")), "{calls}");
    assert!(calls.contains("api graphql --hostname github.com") && calls.contains("number=7"), "{calls}");
    assert!(!calls.contains("--delete-branch") && !calls.contains("--auto"), "{calls}");
}

#[tokio::test]
async fn gh_reads_the_structured_default_branch_before_retargeting_a_pr() {
    let home = directory();
    let program = home.join("gh");
    let log = home.join("calls");
    let response = home.join("default.json");
    std::fs::write(&response, r#"{"defaultBranchRef":{"name":"develop"}}"#).unwrap();
    std::fs::write(&program, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1 $2\" = 'repo view' ]; then cat '{}'; fi\n", log.display(), response.display())).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let gh = GhCli::at(program);
    let pr = PullRequest { id: "pr".into(), task_id: "t".into(), host: "github.com".into(), repository: "acme/widgets".into(),
        number: 7, head: HEAD.into(), base: "feature".into(), state: PrState::Open, merged_head: None,
        allowed_methods: vec![MergeMethod::Squash], review: None };
    let base = gh.default_branch(&pr).await.unwrap();
    assert_eq!(base, "develop");
    gh.retarget(&pr, &base).await.unwrap();
    assert_eq!(std::fs::read_to_string(&log).unwrap(),
        "repo view https://github.com/acme/widgets --json defaultBranchRef\npr edit https://github.com/acme/widgets/pull/7 --base develop\n");
    for invalid in ["{}", r#"{"defaultBranchRef":{"name":""}}"#, r#"{"defaultBranchRef":null}"#, "not json"] {
        std::fs::write(&response, invalid).unwrap();
        assert!(matches!(gh.default_branch(&pr).await, Err(GhFailure::Unreadable(_))));
    }
}
