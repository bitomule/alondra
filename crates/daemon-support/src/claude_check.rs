//! At start, alondrad checks that the Claude Code its Agent SDK host runs can run Alondra's node
//! plugin: version 2.1.321 or later, and the plugin passes `claude plugin validate --strict`. A
//! Claude Code below that release is updated by Alondra, once per release it finds and every six
//! hours at most. A failure is logged and reported by `/daemon`, and re-checked every ten minutes
//! until it passes; each host start checks the version again for itself.
use crate::locks::Poisonable as _;
use alondra_core::claude_code;
use alondra_core::claude_update::{self, Outcome as Update};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Status {
    pub ok: bool,
    pub detail: String,
}

/// A holder that panics leaves it whole, so it is recovered: one assignment, overwritten every
/// check.
static STATUS: Mutex<Option<Status>> = Mutex::new(None);

pub fn status() -> Option<Status> {
    STATUS.lock_or_recover("the Claude Code check").clone()
}

/// What each step printed, or `None` when it could not run in time.
pub struct Outcomes {
    pub version: Option<(bool, String)>,
    pub validate: Option<(bool, String)>,
    /// What Alondra's own update of a Claude Code below the minimum came to.
    pub update: Option<Update>,
}

pub fn judge(plugin: Option<&Path>, outcomes: &Outcomes) -> Status {
    let failed = |detail: String| Status { ok: false, detail };
    let Some(plugin) = plugin else {
        return failed("Alondra's node plugin was not found: run `alondra doctor --fix`".into());
    };
    match &outcomes.version {
        Some((true, text)) if claude_code::compatible(text) => {}
        Some((true, text)) => {
            let why = outcomes.update.as_ref().map_or_else(|| "update it with `claude update`".to_owned(), Update::english);
            return failed(format!("Claude Code {} is older than {}, the release Alondra needs, and {why}", text.trim(), claude_code::MIN_TEXT));
        }
        _ => return failed("Claude Code (`claude`) did not answer `claude --version`".into()),
    }
    match &outcomes.validate {
        Some((true, _)) => Status { ok: true, detail: format!("Claude Code runs Alondra's node plugin from {}", plugin.display()) },
        Some((false, text)) => failed(format!("claude plugin validate --strict {} failed: {}", plugin.display(), claude_code::tail(text))),
        None => failed("claude plugin validate did not finish".into()),
    }
}

async fn claude(args: &[&str], timeout: Duration) -> Option<(bool, String)> {
    let run = tokio::process::Command::new("claude").args(args).kill_on_drop(true)
        .stdin(std::process::Stdio::null()).output();
    let out = tokio::time::timeout(timeout, run).await.ok()?.ok()?;
    Some((out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))))
}

const RECHECK_EVERY: Duration = Duration::from_secs(600);

/// A failing check is retried: the user may update Claude Code or fix the plugin meanwhile.
pub fn next_check(status: &Status) -> Option<Duration> {
    (!status.ok).then_some(RECHECK_EVERY)
}

fn changed(previous: Option<&Status>, status: &Status) -> bool {
    previous != Some(status)
}

pub async fn run(plugin: Option<PathBuf>) {
    loop {
        let status = check(plugin.as_deref()).await;
        if changed(self::status().as_ref(), &status) {
            if status.ok {
                log!("{}", status.detail);
            } else {
                log!("no task can start until Claude Code is fixed: {}", status.detail);
            }
        }
        let wait = next_check(&status);
        *STATUS.lock_or_recover("the Claude Code check") = Some(status);
        let Some(wait) = wait else { return };
        tokio::time::sleep(wait).await;
    }
}

const RETRY_UPDATE_AFTER: Duration = Duration::from_secs(6 * 3600);

struct Attempt {
    version: String,
    at: Instant,
    outcome: Update,
}

/// A holder that panics leaves it whole, so it is recovered: one assignment per attempt.
static ATTEMPT: Mutex<Option<Attempt>> = Mutex::new(None);

/// An update is tried for each release found once every six hours; one that only waited for a
/// running session to end is tried again at the next check.
fn due(last: Option<&Attempt>, version: &str, now: Instant) -> bool {
    match last {
        Some(last) if last.version == version && last.outcome != Update::Waiting => now.duration_since(last.at) >= RETRY_UPDATE_AFTER,
        _ => true,
    }
}

async fn updated(version: &str) -> Option<Update> {
    let now = Instant::now();
    if !due(ATTEMPT.lock_or_recover("the Claude Code update").as_ref(), version, now) {
        return ATTEMPT.lock_or_recover("the Claude Code update").as_ref().map(|last| last.outcome.clone());
    }
    let launcher = claude_update::find_on_path(&std::env::var("PATH").unwrap_or_default(), "claude")?;
    log!("Claude Code {} is older than {}: updating it", version.split_whitespace().next().unwrap_or_default(), claude_code::MIN_TEXT);
    let host = claude_update::ProcessHost { launcher, environment: None };
    let outcome = tokio::task::spawn_blocking(move || claude_update::update(&host)).await.ok()?;
    log!("{}", outcome.english());
    *ATTEMPT.lock_or_recover("the Claude Code update") = Some(Attempt { version: version.to_owned(), at: now, outcome: outcome.clone() });
    Some(outcome)
}

async fn check(plugin: Option<&Path>) -> Status {
    let Some(dir) = plugin else { return judge(None, &Outcomes { version: None, validate: None, update: None }) };
    let mut version = claude(&["--version"], Duration::from_secs(20)).await;
    let mut update = None;
    if let Some((true, text)) = &version && !claude_code::compatible(text) {
        update = updated(text).await;
        if matches!(update, Some(Update::Updated { .. })) {
            version = claude(&["--version"], Duration::from_secs(20)).await;
        }
    }
    let dir = dir.display().to_string();
    let validate = claude(&["plugin", "validate", "--strict", &dir], Duration::from_secs(60)).await;
    judge(plugin, &Outcomes { version, validate, update })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_check_is_retried_every_ten_minutes_until_it_passes() {
        let failing = Status { ok: false, detail: "Claude Code 2.1.200 is older than 2.1.321".into() };
        let passing = Status { ok: true, detail: "runs".into() };
        assert_eq!(next_check(&failing), Some(Duration::from_secs(600)));
        assert_eq!(next_check(&passing), None);
    }

    #[test]
    fn only_a_different_status_is_logged() {
        let failing = Status { ok: false, detail: "off".into() };
        let passing = Status { ok: true, detail: "runs".into() };
        assert!(changed(None, &failing), "the first reading is always logged");
        assert!(!changed(Some(&failing), &failing.clone()));
        assert!(changed(Some(&failing), &passing));
    }

    fn ran(text: &str) -> Option<(bool, String)> {
        Some((true, text.into()))
    }

    #[test]
    fn every_step_must_pass_and_the_first_failure_says_why() {
        let plugin = Path::new("/r/plugin");
        let passing = || Outcomes { version: ran("2.1.321 (Claude Code)"), validate: ran("✔ Validation passed"), update: None };
        assert!(judge(Some(plugin), &passing()).ok);

        let old = judge(Some(plugin), &Outcomes { version: ran("2.1.320 (Claude Code)"), ..passing() });
        assert!(!old.ok && old.detail.contains("2.1.320") && old.detail.contains("2.1.321") && old.detail.contains("claude update"), "{}", old.detail);
        let invalid = judge(Some(plugin), &Outcomes { validate: Some((false, "✘ Validation failed (--strict treats warnings as errors)".into())), ..passing() });
        assert!(!invalid.ok && invalid.detail.contains("validate --strict /r/plugin") && invalid.detail.contains("Validation failed"), "{}", invalid.detail);
        assert!(!judge(Some(plugin), &Outcomes { validate: None, ..passing() }).ok);
        assert!(!judge(None, &passing()).ok, "no plugin, nothing to run");
    }

    #[test]
    fn an_update_is_tried_once_per_release_every_six_hours_and_a_wait_is_tried_again_at_once() {
        let at = Instant::now();
        let attempt = |version: &str, outcome| Attempt { version: version.into(), at, outcome };
        let refused = || Update::Failed(claude_update::Failure::TimedOut);
        assert!(due(None, "2.1.291", at));
        assert!(!due(Some(&attempt("2.1.291", refused())), "2.1.291", at + Duration::from_secs(3599)));
        assert!(due(Some(&attempt("2.1.291", refused())), "2.1.291", at + RETRY_UPDATE_AFTER));
        assert!(due(Some(&attempt("2.1.291", refused())), "2.1.300", at), "another release is a new try");
        assert!(due(Some(&attempt("2.1.291", Update::Waiting)), "2.1.291", at), "a running session ended meanwhile");
    }

    #[test]
    fn a_release_below_the_minimum_says_why_it_could_not_be_updated() {
        let plugin = Path::new("/r/plugin");
        let old = |update| judge(Some(plugin), &Outcomes { version: ran("2.1.291 (Claude Code)"), validate: ran("ok"), update });
        let refused = old(Some(Update::Failed(claude_update::Failure::Refused("Updates are disabled (DISABLE_UPDATES)".into()))));
        assert!(!refused.ok && refused.detail.contains("2.1.291") && refused.detail.contains("DISABLE_UPDATES"), "{}", refused.detail);
        let waiting = old(Some(Update::Waiting));
        assert!(waiting.detail.contains("as soon as none does"), "{}", waiting.detail);
    }
}
