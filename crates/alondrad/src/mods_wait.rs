//! How long a launch waits for its session's node mod, and why it says the mod did not load.
//! A saturated Mac starts sessions slowly: measured on 11 cores, the mod reported after 13-17 s
//! at load 47, 62 s at load 311 and 83 s at load 478. The wait grows with the highest load seen
//! while waiting, and a launch that still times out names the one cause it finds.
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::mods_check::Status;
use crate::node_reports::Reports;
use crate::orchestrate::Launcher;

/// Extra wait per unit of load per core: twice what the measurements needed.
const PER_LOAD: Duration = Duration::from_secs(3);
const MAX_WAIT: Duration = Duration::from_secs(300);
/// How often a wait re-reads the load, so a Mac that saturates mid-wait extends it.
const STEP: Duration = Duration::from_secs(5);
/// Above this load per core, a launch that timed out is put down to the Mac being overloaded.
const OVERLOADED: f64 = 2.0;

/// The one-minute load average and the cores it is shared by.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Load {
    pub one: f64,
    pub cpus: usize,
}

impl Load {
    pub(crate) fn current() -> Option<Load> {
        let cpus = std::thread::available_parallelism().ok()?.get();
        Some(Load { one: sysinfo::System::load_average().one, cpus })
    }

    fn per_core(&self) -> f64 {
        self.one / self.cpus.max(1) as f64
    }
}

/// A launch whose mod never reported: how long it waited and the highest load it saw.
#[derive(Debug, PartialEq)]
pub(crate) struct Waited {
    pub waited: Duration,
    pub peak: Option<Load>,
}

pub(crate) fn deadline(base: Duration, load: Option<Load>) -> Duration {
    let extra = load.map_or(Duration::ZERO, |load| PER_LOAD.mul_f64(load.per_core().max(0.0)));
    (base + extra).min(MAX_WAIT.max(base))
}

fn higher(peak: Option<Load>, now: Option<Load>) -> Option<Load> {
    match (peak, now) {
        (Some(peak), Some(now)) if now.per_core() > peak.per_core() => Some(now),
        (Some(peak), _) => Some(peak),
        (None, now) => now,
    }
}

/// The session whose mod reported `key`, waiting longer the busier the Mac is.
pub(crate) async fn wait(reports: &Reports, key: &str, launcher: &impl Launcher) -> Result<String, Waited> {
    let base = launcher.mods_deadline();
    let started = tokio::time::Instant::now();
    let mut peak = None;
    loop {
        peak = higher(peak, launcher.machine_load());
        let deadline = deadline(base, peak);
        let waited = started.elapsed();
        if waited >= deadline {
            return Err(Waited { waited, peak });
        }
        if let Some(session) = reports.wait_launched(key, (deadline - waited).min(STEP)).await {
            return Ok(session);
        }
    }
}

/// Why the mod of a session launched in `cwd` did not report, checked cause by cause.
pub(crate) async fn not_loaded(cwd: &Path, waited: &Waited, launcher: &impl Launcher) -> String {
    let untrusted = crate::claude_trust::check(cwd);
    let version = match untrusted {
        Some(_) => None,
        None => launcher.claude_version().await,
    };
    explain(untrusted, version.as_deref(), crate::mods_check::status().as_ref(), waited)
}

pub(crate) fn explain(untrusted: Option<PathBuf>, version: Option<&str>, mods: Option<&Status>, waited: &Waited) -> String {
    if let Some(repository) = untrusted {
        return format!("The session's mod did not load: {}", crate::claude_trust::instructions(&repository));
    }
    if let Some(version) = version.filter(|version| !alondra_core::mods::runs_mods(version)) {
        return format!("The session's mod did not load: Claude Code {} cannot run mods; update it to {} or later (`claude update`)",
            version.trim(), alondra_core::mods::CLAUDE_MIN_TEXT);
    }
    if let Some(mods) = mods.filter(|mods| !mods.ok) {
        return format!("The session's mod did not load: {}", mods.detail);
    }
    let seconds = waited.waited.as_secs();
    let claude = version.map_or("Claude Code (its version could not be read)".to_string(), |version| format!("Claude Code {}", version.trim()));
    match waited.peak.filter(|load| load.per_core() > OVERLOADED) {
        Some(load) => format!(
            "The Mac is overloaded (load {:.0} on {} cores), so the session did not start within {seconds} s. \
             The folder is trusted and {claude} runs mods: launch it again when the load drops.",
            load.one, load.cpus),
        None => format!(
            "The session's mod did not report within {seconds} s, although the folder is trusted, {claude} runs mods \
             and the Mac is not overloaded."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrate::Refusal;
    use serde_json::json;
    use std::sync::Arc;

    const BASE: Duration = Duration::from_secs(30);

    fn load(one: f64) -> Option<Load> {
        Some(Load { one, cpus: 11 })
    }

    struct Busy(Option<Load>);

    impl Launcher for Busy {
        async fn run(&self, _: &str, _: &[String]) -> Result<String, Refusal> {
            unreachable!()
        }
        async fn stop(&self, _: &str) {}
        fn mods_deadline(&self) -> Duration {
            Duration::from_millis(50)
        }
        fn machine_load(&self) -> Option<Load> {
            self.0
        }
    }

    #[test]
    fn the_wait_grows_with_the_load_per_core_and_stops_at_five_minutes() {
        assert_eq!(deadline(BASE, None), BASE);
        assert_eq!(deadline(BASE, load(0.0)), BASE);
        assert_eq!(deadline(BASE, load(478.0)).as_secs(), 160, "twice the 83 s measured at load 478 on 11 cores");
        assert!(deadline(BASE, load(311.0)) > Duration::from_secs(62 * 3 / 2), "well above the 62 s measured at load 311");
        assert_eq!(deadline(BASE, load(5000.0)), MAX_WAIT);
    }

    async fn reported_after(after: Duration, launcher: Busy) -> Result<String, Waited> {
        let reports = Arc::new(Reports::default());
        tokio::spawn({
            let reports = reports.clone();
            async move {
                tokio::time::sleep(after).await;
                let report = serde_json::from_value(json!({"session_id": "slow", "event": "session.start", "version": "2.1.291", "launch_key": "launch-1"})).unwrap();
                reports.apply(report, 1);
            }
        });
        wait(&reports, "launch-1", &launcher).await
    }

    const SLOW_START: Duration = Duration::from_millis(400);

    #[tokio::test]
    async fn a_session_slower_than_the_base_wait_on_a_saturated_mac_still_launches() {
        let launched = reported_after(SLOW_START, Busy(load(478.0))).await;
        assert_eq!(launched, Ok("slow".into()));
    }

    #[tokio::test]
    async fn an_idle_mac_still_gives_up_after_the_base_wait() {
        let Err(waited) = reported_after(SLOW_START, Busy(load(0.0))).await else { panic!("launched") };
        assert!(waited.waited >= Duration::from_millis(50) && waited.waited < SLOW_START, "{:?}", waited.waited);
        assert_eq!(waited.peak, load(0.0));
    }

    fn waited(peak: Option<Load>) -> Waited {
        Waited { waited: Duration::from_secs(160), peak }
    }

    #[test]
    fn an_untrusted_folder_is_the_only_cause_named() {
        let text = explain(Some("/w/repo".into()), Some("2.1.291 (Claude Code)"), None, &waited(load(478.0)));
        assert!(text.contains("does not trust /w/repo"), "{text}");
        assert!(!text.contains("overloaded") && !text.contains("update it"), "{text}");
    }

    #[test]
    fn an_old_claude_code_is_the_only_cause_named() {
        let text = explain(None, Some("2.1.200 (Claude Code)"), None, &waited(load(1.0)));
        assert!(text.contains("Claude Code 2.1.200 (Claude Code) cannot run mods") && text.contains("2.1.287"), "{text}");
        assert!(!text.contains("trust") && !text.contains("overloaded"), "{text}");
    }

    #[test]
    fn mods_turned_off_is_the_only_cause_named() {
        let off = Status { ok: false, detail: "claude plugin test failed: hooks modules are turned off".into() };
        let text = explain(None, Some("2.1.291 (Claude Code)"), Some(&off), &waited(load(1.0)));
        assert!(text.ends_with("hooks modules are turned off"), "{text}");
        assert!(!text.contains("trust") && !text.contains("overloaded"), "{text}");
    }

    #[test]
    fn a_saturated_mac_is_told_apart_from_the_setup_causes() {
        let fine = Status { ok: true, detail: "runs".into() };
        let text = explain(None, Some("2.1.291 (Claude Code)"), Some(&fine), &waited(load(311.0)));
        assert!(text.starts_with("The Mac is overloaded (load 311 on 11 cores)") && text.contains("within 160 s"), "{text}");
        assert!(text.contains("folder is trusted") && text.contains("2.1.291"), "{text}");
        assert!(!text.contains("did not load"), "{text}");
    }

    #[test]
    fn with_every_cause_ruled_out_it_says_so_instead_of_listing_them() {
        let text = explain(None, None, None, &waited(load(11.0)));
        assert_eq!(text, "The session's mod did not report within 160 s, although the folder is trusted, Claude Code (its version could not be read) runs mods and the Mac is not overloaded.");
    }
}
