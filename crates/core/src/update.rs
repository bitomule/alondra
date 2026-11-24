//! Released versions come from the Homebrew tap's GitHub releases, tagged `alondra-vX.Y.Z`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Millis;

pub const FORMULA: &str = "bitomule/tap/alondra";
pub const RELEASES_URL: &str = "https://api.github.com/repos/bitomule/homebrew-tap/releases?per_page=100";
const TAG_PREFIX: &str = "alondra-v";

pub fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

pub fn newer(candidate: &str, current: &str) -> bool {
    matches!((parse(candidate), parse(current)), (Some(c), Some(r)) if c > r)
}

/// The highest published, non-prerelease `alondra-v` tag in a GitHub releases listing.
pub fn latest_release(releases: &Value) -> Option<String> {
    releases.as_array()?.iter()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false) && !r["prerelease"].as_bool().unwrap_or(false))
        .filter_map(|r| r["tag_name"].as_str()?.strip_prefix(TAG_PREFIX))
        .filter_map(|v| parse(v).map(|p| (p, v.to_string())))
        .max_by_key(|(p, _)| *p)
        .map(|(_, v)| v)
}

/// What the daemon knows about updates, as the overview and `/v1/health` show it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub current: String,
    #[serde(default)]
    pub latest: Option<String>,
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub checked_at: Option<Millis>,
    #[serde(default)]
    pub auto_update: bool,
    #[serde(default)]
    pub last_auto_update: Option<Attempt>,
    /// A binary that was installed but failed `alondrad --preflight`, so this one kept running.
    #[serde(default)]
    pub rejected: Option<Rejected>,
}

/// A failed automatic update is tried again this many times for the same version.
pub const RETRIES: u32 = 3;
const FIRST_RETRY_AFTER_MS: Millis = 30 * 60_000;

/// The last automatic update of a version: `attempts` counts every try of that version.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    pub version: String,
    pub at: Millis,
    pub ok: bool,
    pub detail: String,
    #[serde(default = "one")]
    pub attempts: u32,
}

fn one() -> u32 {
    1
}

impl Attempt {
    /// A new try of `version`: the count goes on from a failed try of the same version.
    pub fn after(last: Option<&Attempt>, version: String, at: Millis, ok: bool, detail: String) -> Self {
        let attempts = last.filter(|l| l.version == version && !l.ok).map_or(1, |l| l.attempts.saturating_add(1));
        Self { version, at, ok, detail, attempts }
    }

    /// Every retry failed: this version waits for the next release or a manual `alondra update`.
    pub fn gave_up(&self) -> bool {
        !self.ok && self.attempts > RETRIES
    }

    /// When a failed try may run again: 30 min, 2 h and 8 h after the first, second and third.
    pub fn retry_at(&self) -> Option<Millis> {
        let wait = FIRST_RETRY_AFTER_MS.saturating_mul(4i64.saturating_pow(self.attempts.saturating_sub(1)));
        (!self.ok && !self.gave_up()).then(|| self.at.saturating_add(wait))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rejected {
    #[serde(default)]
    pub version: Option<String>,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_alondra_releases_count_and_the_highest_wins() {
        let releases = json!([
            {"tag_name": "alondra-v0.1.9", "draft": false, "prerelease": false},
            {"tag_name": "alondra-v0.1.12", "draft": false, "prerelease": false},
            {"tag_name": "alondra-v0.2.0", "draft": true, "prerelease": false},
            {"tag_name": "alondra-v0.3.0", "draft": false, "prerelease": true},
            {"tag_name": "apple-review-0.9.9-build3", "draft": false, "prerelease": false},
            {"tag_name": "other-v9.0.0", "draft": false, "prerelease": false},
            {"tag_name": "alondra-vnext", "draft": false, "prerelease": false},
        ]);
        assert_eq!(latest_release(&releases).as_deref(), Some("0.1.12"));
        assert_eq!(latest_release(&json!([])), None);
        assert_eq!(latest_release(&json!({"message": "API rate limit exceeded"})), None);
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(newer("0.1.13", "0.1.12"));
        assert!(newer("0.1.10", "0.1.9"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("0.1.12", "0.1.12"));
        assert!(!newer("0.1.11", "0.1.12"));
        assert!(!newer("garbage", "0.1.12"));
        assert_eq!(parse("0.1.2.3"), None);
    }
}
