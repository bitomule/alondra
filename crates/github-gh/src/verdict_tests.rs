use super::*;
use serde_json::json;

fn view(merge_state: &str, review: Value, checks: Value) -> Value {
    json!({"mergeable": "MERGEABLE", "mergeStateStatus": merge_state, "reviewDecision": review, "statusCheckRollup": checks, "isDraft": false})
}

fn run(name: &str, conclusion: &str) -> Value {
    json!({"name": name, "status": "COMPLETED", "conclusion": conclusion})
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn a_clean_pr_merges_with_nothing_to_say() {
    assert_eq!(derive(&view("CLEAN", Value::Null, json!([run("ci", "SUCCESS")])), &[], &[]), MergeVerdict::mergeable());
}

#[test]
fn a_conflict_blocks_whatever_else_github_says() {
    let mut conflicting = view("DIRTY", Value::Null, json!([]));
    conflicting["mergeable"] = json!("CONFLICTING");
    assert_eq!(derive(&conflicting, &[], &[]), MergeVerdict::Blocked { blockers: vec![MergeBlocker::Conflicts] });
}

#[test]
fn a_state_github_has_not_worked_out_is_unknown() {
    assert_eq!(derive(&view("UNKNOWN", Value::Null, json!([])), &[], &[]), MergeVerdict::Unknown);
    assert_eq!(derive(&json!({"statusCheckRollup": []}), &[], &[]), MergeVerdict::Unknown);
}

#[test]
fn a_failing_check_the_base_does_not_require_merges_with_a_note() {
    let unstable = view("UNSTABLE", Value::Null, json!([run("ci", "SUCCESS"), run("lint", "FAILURE"), json!({"name": "slow", "status": "IN_PROGRESS"})]));
    assert_eq!(derive(&unstable, &[], &[]), MergeVerdict::Mergeable { notes: vec![
        MergeNote::CheckFailingNotRequired { name: "lint".into() }, MergeNote::CheckRunningNotRequired { name: "slow".into() },
    ] });
}

#[test]
fn a_required_review_blocks_a_pr_whose_checks_all_pass() {
    let blocked = view("BLOCKED", json!("REVIEW_REQUIRED"), json!([run("ci", "SUCCESS")]));
    assert_eq!(derive(&blocked, &names(&["ci"]), &[]), MergeVerdict::Blocked { blockers: vec![MergeBlocker::ReviewRequired] });
    let changes = view("BLOCKED", json!("CHANGES_REQUESTED"), json!([run("ci", "SUCCESS")]));
    assert_eq!(derive(&changes, &names(&["ci"]), &[]), MergeVerdict::Blocked { blockers: vec![MergeBlocker::ChangesRequested] });
}

#[test]
fn each_required_check_says_whether_it_failed_runs_waits_or_never_reported() {
    let blocked = view("BLOCKED", Value::Null, json!([
        run("build", "FAILURE"), json!({"name": "test", "status": "IN_PROGRESS"}), run("deploy", "ACTION_REQUIRED"), run("lint", "SUCCESS"),
    ]));
    assert_eq!(derive(&blocked, &names(&["build", "test", "deploy", "lint", "audit"]), &[]), MergeVerdict::Blocked { blockers: vec![
        MergeBlocker::RequiredCheckFailing { name: "build".into() },
        MergeBlocker::RequiredCheckRunning { name: "test".into() },
        MergeBlocker::CheckNeedsAction { name: "deploy".into() },
        MergeBlocker::RequiredCheckMissing { name: "audit".into() },
    ] });
}

#[test]
fn a_block_nothing_explains_says_so_instead_of_inventing_a_reason() {
    assert_eq!(derive(&view("BLOCKED", Value::Null, json!([])), &[], &[]), MergeVerdict::Blocked { blockers: vec![MergeBlocker::Other] });
}

#[test]
fn a_draft_blocks_nothing_because_alondra_marks_it_ready_before_merging() {
    let mut draft = view("BLOCKED", Value::Null, json!([]));
    draft["isDraft"] = json!(true);
    assert_eq!(derive(&draft, &[], &[]), MergeVerdict::mergeable());
}

#[test]
fn a_branch_behind_its_base_blocks_with_that_reason() {
    assert_eq!(derive(&view("BEHIND", Value::Null, json!([])), &[], &[]), MergeVerdict::Blocked { blockers: vec![MergeBlocker::BehindBase] });
}

#[test]
fn required_checks_still_to_report_the_account_may_bypass_merge_with_a_note_but_a_failing_check_or_a_missing_review_never_does() {
    let waiting = view("BLOCKED", Value::Null, json!([json!({"name": "build", "status": "IN_PROGRESS"})]));
    assert_eq!(derive(&waiting, &names(&["build", "audit"]), &names(&["build", "audit"])),
        MergeVerdict::Mergeable { notes: vec![MergeNote::BypassingRules { names: names(&["build", "audit"]) }] });
    assert_eq!(derive(&waiting, &names(&["build"]), &[]), MergeVerdict::Blocked { blockers: vec![MergeBlocker::RequiredCheckRunning { name: "build".into() }] });
    let failing = view("BLOCKED", Value::Null, json!([run("build", "FAILURE")]));
    assert_eq!(derive(&failing, &names(&["build"]), &names(&["build"])), MergeVerdict::Blocked { blockers: vec![MergeBlocker::RequiredCheckFailing { name: "build".into() }] });
    let reviewed = view("BLOCKED", json!("REVIEW_REQUIRED"), json!([json!({"name": "build", "status": "IN_PROGRESS"})]));
    assert_eq!(derive(&reviewed, &names(&["build"]), &names(&["build"])), MergeVerdict::Blocked { blockers: vec![
        MergeBlocker::RequiredCheckRunning { name: "build".into() }, MergeBlocker::ReviewRequired,
    ] });
}
