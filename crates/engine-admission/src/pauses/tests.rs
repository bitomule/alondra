use super::*;
use alondra_core::Millis;

const MIN: Millis = 60_000;

#[test]
fn a_pause_by_the_user_never_returns_by_itself_and_a_declared_wait_returns_without_waiting_out_any_time() {
    let paused = |reason| Paused { task: "t".into(), reason, since: 0, cut: None, turn_since: None, cleanup: Cleanup::Done, released: false };
    assert!(!paused(Reason::ByUser).returns(true, false, 10 * 60 * MIN));
    assert!(paused(Reason::Memory).returns(true, false, 0));
    assert!(!paused(Reason::Usage).returns(false, false, 0));
    let declared = paused(Reason::Declared { reason: "build queue".into() });
    assert!(declared.returns(true, false, 0), "the gate's measures alone say when it has room");
    let retry = alondra_domain_rules::ApiRetry { error: "server_error".into(), detail: None, attempt: 1, at: 30_000, outage: false };
    let failed = paused(Reason::ApiError(retry));
    assert!(!failed.returns(true, false, 29_999), "an API error waits for its retry");
    assert!(failed.returns(true, false, 30_000));
    assert!(failed.returns(true, true, 0), "the model answering another node lets it go at once");
}

#[test]
fn the_resume_message_says_when_and_what_was_cut() {
    let paused = Paused { task: "t".into(), reason: Reason::Memory, since: 0, cut: Some("bazel test //crates/...".into()), turn_since: None, cleanup: Cleanup::Done, released: false };
    let text = resume_text(&paused, 0);
    assert!(text.starts_with("Alondra stopped you at "), "{text}");
    assert!(text.contains("because the Mac ran out of memory, in the middle of `bazel test //crates/...`."), "{text}");
    assert!(text.ends_with("The Mac has room again: carry on with your task and run again what was cut."), "{text}");
}
