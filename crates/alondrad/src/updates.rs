//! Update awareness: the newest release in the Homebrew tap, checked at start (with jitter) and
//! every 6 hours, quietly. With `auto_update`, a launchd daemon installed by Homebrew upgrades
//! itself once per new version; `relaunch` then restarts it into the new binary.
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use alondra_core::update::{self, Attempt, Status};

const EVERY: Duration = Duration::from_secs(6 * 3600);
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const UPGRADE_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const ATTEMPT_FILE: &str = "auto-update.json";

#[derive(Default)]
struct Known {
    latest: Option<String>,
    checked_at: Option<i64>,
    auto_update: bool,
}

static KNOWN: Mutex<Known> = Mutex::new(Known { latest: None, checked_at: None, auto_update: false });

pub fn status(home: &Path) -> Status {
    let known = KNOWN.lock().expect("update state poisoned");
    let current = env!("CARGO_PKG_VERSION").to_string();
    Status {
        available: known.latest.as_deref().is_some_and(|l| update::newer(l, &current)),
        latest: known.latest.clone(),
        checked_at: known.checked_at,
        auto_update: known.auto_update,
        last_auto_update: last_attempt(home),
        rejected: crate::relaunch::rejected().map(|r| update::Rejected { version: r.version, reason: r.reason }),
        current,
    }
}

pub fn last_attempt(home: &Path) -> Option<Attempt> {
    serde_json::from_slice(&std::fs::read(home.join(ATTEMPT_FILE)).ok()?).ok()
}

fn record_attempt(home: &Path, attempt: &Attempt) {
    let written = serde_json::to_vec(attempt).map_err(std::io::Error::other)
        .and_then(|bytes| std::fs::write(home.join(ATTEMPT_FILE), bytes));
    if let Err(e) = written {
        log!("cannot record the automatic update: {e}");
    }
}

/// The newest release, or nothing when offline, rate limited or answered oddly.
pub async fn fetch(url: &str) -> Option<String> {
    let client = reqwest::Client::builder().timeout(FETCH_TIMEOUT)
        .user_agent(concat!("alondrad/", env!("CARGO_PKG_VERSION"))).build().ok()?;
    let response = client.get(url).header("accept", "application/vnd.github+json").send().await.ok()?;
    let releases: serde_json::Value = response.error_for_status().ok()?.json().await.ok()?;
    update::latest_release(&releases)
}

#[derive(Debug, PartialEq)]
pub enum Plan {
    Nothing,
    NotHomebrew,
    Upgrade { brew: PathBuf, version: String },
}

/// At most one automatic upgrade per released version, and only for a Homebrew install.
pub fn plan(auto_update: bool, latest: Option<&str>, last: Option<&Attempt>, prefix: Option<&Path>) -> Plan {
    let Some(latest) = latest.filter(|l| auto_update && update::newer(l, env!("CARGO_PKG_VERSION"))) else { return Plan::Nothing };
    if last.is_some_and(|a| a.version == latest) { return Plan::Nothing; }
    match prefix {
        Some(prefix) => Plan::Upgrade { brew: prefix.join("bin/brew"), version: latest.to_string() },
        None => Plan::NotHomebrew,
    }
}

pub struct Updater {
    pub home: PathBuf,
    pub url: String,
    pub auto_update: bool,
    /// The Homebrew prefix of the binary launchd runs; `None` when not a launchd Homebrew install.
    pub prefix: Option<PathBuf>,
}

impl Updater {
    pub async fn check_once(&self, upgrade: impl FnOnce(&Path) -> (bool, String) + Send + 'static) {
        let latest = fetch(&self.url).await;
        {
            let mut known = KNOWN.lock().expect("update state poisoned");
            known.auto_update = self.auto_update;
            if latest.is_some() {
                known.latest = latest.clone();
                known.checked_at = Some(alondra_core::now_millis());
            }
        }
        match plan(self.auto_update, latest.as_deref(), last_attempt(&self.home).as_ref(), self.prefix.as_deref()) {
            Plan::Nothing => (),
            Plan::NotHomebrew => log!("{} is available; auto_update only installs through Homebrew under launchd", latest.unwrap_or_default()),
            Plan::Upgrade { brew, version } => {
                log!("{version} is available; running {} upgrade {}", brew.display(), update::FORMULA);
                let (ok, detail) = tokio::task::spawn_blocking(move || upgrade(&brew)).await
                    .unwrap_or_else(|e| (false, format!("upgrade crashed: {e}")));
                log!("automatic update to {version} {}: {detail}", if ok { "installed" } else { "failed" });
                record_attempt(&self.home, &Attempt { version, at: alondra_core::now_millis(), ok, detail });
            }
        }
    }

    pub async fn run(self) {
        tokio::time::sleep(Duration::from_secs(30 + u64::from_str_radix(&crate::auth::random_hex(2), 16).unwrap_or(0) % 300)).await;
        loop {
            self.check_once(brew_upgrade).await;
            tokio::time::sleep(EVERY).await;
        }
    }
}

pub fn updater(config: &alondra_core::config::Config) -> Updater {
    let prefix = crate::relaunch::supervised(&config.service_label, |k| std::env::var(k).ok())
        .then(crate::relaunch::launch_path).flatten()
        .and_then(|path| update::brew_prefix(&path));
    Updater {
        home: config.home.clone(),
        url: std::env::var("ALONDRA_UPDATE_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| update::RELEASES_URL.into()),
        auto_update: config.auto_update,
        prefix,
    }
}

fn brew_upgrade(brew: &Path) -> (bool, String) {
    let child = std::process::Command::new(brew).args(["upgrade", update::FORMULA])
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .spawn();
    let child = match child {
        Ok(child) => child,
        Err(e) => return (false, format!("cannot run {}: {e}", brew.display())),
    };
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || { let _ = tx.send(child.wait_with_output()); });
    let Ok(Ok(out)) = rx.recv_timeout(UPGRADE_TIMEOUT) else {
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        return (false, "brew upgrade did not finish".into());
    };
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let last = text.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("no output").to_string();
    (out.status.success(), last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const NEWER: &str = "999.0.0";

    fn attempt(version: &str) -> Attempt {
        Attempt { version: version.into(), at: 1, ok: false, detail: "x".into() }
    }

    #[test]
    fn upgrades_only_when_asked_newer_homebrew_and_not_yet_tried() {
        let prefix = Path::new("/opt/homebrew");
        assert_eq!(plan(false, Some(NEWER), None, Some(prefix)), Plan::Nothing);
        assert_eq!(plan(true, None, None, Some(prefix)), Plan::Nothing);
        assert_eq!(plan(true, Some(env!("CARGO_PKG_VERSION")), None, Some(prefix)), Plan::Nothing);
        assert_eq!(plan(true, Some("0.0.1"), None, Some(prefix)), Plan::Nothing);
        assert_eq!(plan(true, Some(NEWER), None, None), Plan::NotHomebrew);
        assert_eq!(plan(true, Some(NEWER), Some(&attempt(NEWER)), Some(prefix)), Plan::Nothing, "at most once per version");
        assert_eq!(plan(true, Some(NEWER), Some(&attempt("998.0.0")), Some(prefix)),
            Plan::Upgrade { brew: prefix.join("bin/brew"), version: NEWER.into() });
    }

    async fn releases_server(body: serde_json::Value) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = axum::Router::new().route("/releases", axum::routing::get(move |headers: axum::http::HeaderMap| {
            let body = body.clone();
            async move {
                assert!(headers.get("user-agent").is_some_and(|v| v.to_str().unwrap().starts_with("alondrad/")), "GitHub refuses requests without a User-Agent");
                axum::Json(body)
            }
        }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}/releases")
    }

    #[tokio::test]
    async fn a_newer_release_is_installed_once_and_the_outcome_kept() {
        let home = std::env::temp_dir().join(format!("alondra-updates-{}", crate::auth::random_hex(6)));
        std::fs::create_dir_all(&home).unwrap();
        let url = releases_server(serde_json::json!([
            {"tag_name": format!("alondra-v{NEWER}"), "draft": false, "prerelease": false},
            {"tag_name": "alondra-v0.0.1", "draft": false, "prerelease": false},
        ])).await;
        let updater = Updater { home: home.clone(), url, auto_update: true, prefix: Some("/opt/homebrew".into()) };
        let runs = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let counted = runs.clone();
            updater.check_once(move |brew| {
                counted.fetch_add(1, Ordering::SeqCst);
                assert_eq!(brew, Path::new("/opt/homebrew/bin/brew"));
                (true, "Upgrading bitomule/tap/alondra".into())
            }).await;
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1, "a version is upgraded to at most once");
        let status = status(&home);
        assert_eq!(status.latest.as_deref(), Some(NEWER));
        assert!(status.available && status.auto_update && status.checked_at.is_some());
        let last = status.last_auto_update.unwrap();
        assert_eq!((last.version.as_str(), last.ok), (NEWER, true));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn an_unreachable_or_odd_answer_changes_nothing() {
        assert_eq!(fetch("http://127.0.0.1:1/releases").await, None);
        let url = releases_server(serde_json::json!({"message": "API rate limit exceeded"})).await;
        assert_eq!(fetch(&url).await, None);
    }
}
