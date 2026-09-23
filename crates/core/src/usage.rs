//! The Claude subscription's usage, as the usage hooks leave it on disk: `usage-state.json`
//! (the 5-hour and 7-day windows) and `usage-daily.json` (where each day of the week started).
//! Today's slice of the week is what is left at the start of the day, split over the days left
//! until the reset, the same rule the usage guard warns with.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::Millis;

pub const STATE_FILE: &str = "usage-state.json";
pub const DAILY_FILE: &str = "usage-daily.json";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Usage {
    pub week_percent: f64,
    pub week_resets_at: Millis,
    #[serde(default)]
    pub today_spent: Option<f64>,
    #[serde(default)]
    pub today_share: Option<f64>,
    pub per_day_left: f64,
    #[serde(default)]
    pub five_hour_percent: Option<f64>,
    #[serde(default)]
    pub five_hour_resets_at: Option<Millis>,
    pub measured_at: Millis,
    #[serde(default)]
    pub over_pace: bool,
}

const DAY_SECS: f64 = 86_400.0;

/// `now` in Unix seconds and `today` as the local `YYYY-MM-DD` the hooks key their days by.
pub fn compute(state: &Value, daily: &Value, now: i64, today: &str) -> Option<Usage> {
    let week = state.get("seven_day")?;
    let week_percent = week.get("used_percentage")?.as_f64()?;
    let resets_at = week.get("resets_at")?.as_i64()?;
    let days_left = ((resets_at - now).max(0) as f64 / DAY_SECS).ceil().max(1.0);
    let entry = daily.get("days").and_then(|d| d.get(today));
    let start = entry.and_then(|e| e.get("start")).and_then(Value::as_f64);
    let last = entry.and_then(|e| e.get("last")).and_then(Value::as_f64);
    let today_spent = start.zip(last).map(|(s, l)| (l - s).max(0.0));
    let today_share = start.map(|s| ((100.0 - s) / days_left).max(0.1));
    let five = state.get("five_hour");
    Some(Usage {
        week_percent,
        week_resets_at: resets_at * 1000,
        today_spent,
        today_share,
        per_day_left: (100.0 - week_percent).max(0.0) / days_left,
        five_hour_percent: five.and_then(|f| f.get("used_percentage")).and_then(Value::as_f64),
        five_hour_resets_at: five.and_then(|f| f.get("resets_at")).and_then(Value::as_i64).map(|s| s * 1000),
        measured_at: state.get("ts").and_then(Value::as_i64).map_or(0, |s| s * 1000),
        over_pace: today_spent.zip(today_share).is_some_and(|(spent, share)| spent >= share),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// This Mac on 23 Sep at ~21:30: 53% of the week used, 12% at the start of the day, reset in
    /// just under 5 days. The guard said "41% against the 18% that today's slice allows".
    fn today() -> (Value, Value, i64) {
        let state = json!({"ts": 1790192202, "five_hour": {"used_percentage": 6, "resets_at": 1790202000}, "seven_day": {"used_percentage": 53, "resets_at": 1790553600}});
        let daily = json!({"week_reset": 1790553600, "days": {"2026-09-22": {"start": 25, "last": 35.0}, "2026-09-23": {"start": 12, "last": 53}}});
        (state, daily, 1790192202)
    }

    #[test]
    fn today_is_measured_against_its_slice_of_the_week() {
        let (state, daily, now) = today();
        let u = compute(&state, &daily, now, "2026-09-23").unwrap();
        assert_eq!(u.week_percent, 53.0);
        assert_eq!(u.today_spent, Some(41.0));
        assert_eq!(u.today_share.map(f64::round), Some(18.0));
        assert_eq!(u.per_day_left.round(), 9.0);
        assert!(u.over_pace);
        assert_eq!(u.five_hour_percent, Some(6.0));
        assert_eq!(u.measured_at, 1790192202000);
    }

    #[test]
    fn a_day_within_its_slice_or_without_a_record_is_not_over_pace() {
        let (state, daily, now) = today();
        let calm = compute(&state, &daily, now, "2026-09-22").unwrap();
        assert!(!calm.over_pace, "10% spent against a ~18% slice");
        let unknown = compute(&state, &daily, now, "2026-09-24").unwrap();
        assert_eq!((unknown.today_spent, unknown.over_pace), (None, false));
        assert!(compute(&json!({}), &daily, now, "2026-09-23").is_none(), "no week window, no usage");
    }
}
