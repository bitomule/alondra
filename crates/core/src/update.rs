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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    pub version: String,
    pub at: Millis,
    pub ok: bool,
    pub detail: String,
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
