//! The notice an auto-merged pull request gives its node, which alondrad's API words.
use alondra_daemon_engine::domain_github::tests::*;
use alondra_core::domain::*;

#[test]
fn the_auto_merge_notice_says_alondra_merges_without_protocol_or_git_metadata() {
    let (_fixture, domain) = open("notice");
    presented(&domain, true);
    let register = Command::RegisterPr {
        task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
        number: 7, head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    };
    let notice = crate::domain_api::auto_merge_notice(&domain, &register).unwrap();
    assert!(notice.contains("Alondra merges it") && notice.contains("do not merge it yourself"), "{notice}");
    for internal in ["github.com", "owner/repo", HEAD, "task_id", "revision", "authorization", "claim"] {
        assert!(!notice.contains(internal), "{internal} leaked in {notice}");
    }
}

#[test]
fn only_a_request_that_merges_by_itself_gets_the_auto_merge_notice() {
    for auto_merge in [true, false] {
        let (_fixture, domain) = open("presented");
        presented(&domain, auto_merge);
        let register = Command::RegisterPr {
            task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
            number: 7, head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
        };
        assert_eq!(crate::domain_api::auto_merge_notice(&domain, &register).is_some(), auto_merge);
    }
}
