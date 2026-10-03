//! At start, alondrad checks that the Claude Code it launches nodes with can run their node mod:
//! version 2.1.287 or later, the node plugin passes `claude plugin validate --strict`, and its
//! tests pass under `claude plugin test`, which also fails when hooks modules are turned off.
//! A failure is logged and reported by `/daemon`, and re-checked every ten minutes until it passes;
//! each launch still waits for its own mod.
use alondra_core::mods;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Status {
    pub ok: bool,
    pub detail: String,
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);

pub(crate) fn status() -> Option<Status> {
    STATUS.lock().expect("mods check lock poisoned").clone()
}

/// What each step printed, or `None` when it could not run in time.
pub(crate) struct Outcomes {
    pub version: Option<(bool, String)>,
    pub validate: Option<(bool, String)>,
    pub test: Option<(bool, String)>,
}

pub(crate) fn judge(plugin: Option<&Path>, outcomes: &Outcomes) -> Status {
    let failed = |detail: String| Status { ok: false, detail };
    let Some(plugin) = plugin else {
        return failed("Alondra's node plugin was not found: run `alondra setup`".into());
    };
    match &outcomes.version {
        Some((true, text)) if mods::runs_mods(text) => {}
        Some((true, text)) => return failed(format!("Claude Code {} cannot run mods: update it to {} or later (`claude update`)",
            text.trim(), mods::CLAUDE_MIN_TEXT)),
        _ => return failed("Claude Code (`claude`) did not answer `claude --version`".into()),
    }
    match &outcomes.validate {
        Some((true, _)) => {}
        Some((false, text)) => return failed(format!("claude plugin validate --strict {} failed: {}", plugin.display(), mods::tail(text))),
        None => return failed("claude plugin validate did not finish".into()),
    }
    match &outcomes.test {
        Some((true, _)) => Status { ok: true, detail: format!("Claude Code runs the node mod from {}", plugin.display()) },
        Some((false, text)) => failed(format!("claude plugin test {} failed: {}", plugin.display(), mods::tail(text))),
        None => failed("claude plugin test did not finish".into()),
    }
}

async fn claude(args: &[&str], timeout: Duration) -> Option<(bool, String)> {
    let run = tokio::process::Command::new("claude").args(args).kill_on_drop(true)
        .stdin(std::process::Stdio::null()).output();
    let out = tokio::time::timeout(timeout, run).await.ok()?.ok()?;
    Some((out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))))
}

const RECHECK_EVERY: Duration = Duration::from_secs(600);

/// A failing check is retried because Claude Code's rollout switches can flip on after start.
pub(crate) fn next_check(status: &Status) -> Option<Duration> {
    (!status.ok).then_some(RECHECK_EVERY)
}

fn changed(previous: Option<&Status>, status: &Status) -> bool {
    previous != Some(status)
}

pub(crate) async fn run(plugin: Option<PathBuf>) {
    loop {
        let status = check(plugin.as_deref()).await;
        if changed(self::status().as_ref(), &status) {
            if status.ok {
                log!("{}", status.detail);
            } else {
                log!("nodes cannot run their mod: {}", status.detail);
            }
        }
        let wait = next_check(&status);
        *STATUS.lock().expect("mods check lock poisoned") = Some(status);
        let Some(wait) = wait else { return };
        tokio::time::sleep(wait).await;
    }
}

async fn check(plugin: Option<&Path>) -> Status {
    let outcomes = match plugin {
        Some(dir) => {
            let dir = dir.display().to_string();
            Outcomes {
                version: claude(&["--version"], Duration::from_secs(20)).await,
                validate: claude(&["plugin", "validate", "--strict", &dir], Duration::from_secs(60)).await,
                test: claude(&["plugin", "test", &dir], Duration::from_secs(120)).await,
            }
        }
        None => Outcomes { version: None, validate: None, test: None },
    };
    judge(plugin, &outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_check_is_retried_every_ten_minutes_until_it_passes() {
        let failing = Status { ok: false, detail: "hooks modules are turned off".into() };
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
        let passing = || Outcomes { version: ran("2.1.287 (Claude Code)"), validate: ran("✔ Validation passed"), test: ran(" 4 pass\n 0 fail") };
        assert!(judge(Some(plugin), &passing()).ok);

        let old = judge(Some(plugin), &Outcomes { version: ran("2.1.286 (Claude Code)"), ..passing() });
        assert!(!old.ok && old.detail.contains("2.1.286") && old.detail.contains("2.1.287"), "{}", old.detail);
        let invalid = judge(Some(plugin), &Outcomes { validate: Some((false, "✘ Validation failed (--strict treats warnings as errors)".into())), ..passing() });
        assert!(!invalid.ok && invalid.detail.contains("validate --strict /r/plugin") && invalid.detail.contains("Validation failed"), "{}", invalid.detail);
        let off = judge(Some(plugin), &Outcomes { test: Some((false, "claude plugin test: hooks modules are turned off: disableAllHooks".into())), ..passing() });
        assert!(!off.ok && off.detail.contains("hooks modules are turned off"), "{}", off.detail);
        assert!(!judge(Some(plugin), &Outcomes { test: None, ..passing() }).ok);
        assert!(!judge(None, &passing()).ok, "no plugin, nothing to run");
    }
}
