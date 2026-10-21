//! The Claude plan's limits as the node mod measures them after each turn (`session.measure`):
//! the 5-hour and 7-day windows, how much of each is used and when each resets. Today's slice of
//! the week is what was left at the day's first measurement, split over the days left until the
//! reset, the same rule the usage guard warns with. A window whose reset has passed no longer
//! describes the plan, so it is left out.

use serde::{Deserialize, Serialize};

use crate::model::Millis;

pub const FIVE_HOUR: &str = "five_hour";
pub const SEVEN_DAY: &str = "seven_day";

/// One window of the plan's limits, as `session.measure` reports it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RateLimit {
    pub kind: String,
    pub percent_used: f64,
    #[serde(default)]
    pub resets_at: Option<Millis>,
}

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

/// The cards Alondra raises about the pace of the week's usage. The first is raised once per local
/// day when a launch goes ahead over today's slice; pausing from it raises the second, which
/// resumes early. Their words are written when a client asks for them, in its language.
pub const OVER_PACE_PREFIX: &str = "over-pace-";
pub const OVER_PACE_PAUSE_OPTION: &str = "pause_until_tomorrow";
pub const OVER_PACE_KEEP_OPTION: &str = "keep_going";
pub const PACE_RESUME_PREFIX: &str = "pace-resume-";
pub const PACE_RESUME_OPTION: &str = "resume_now";
pub const PACE_STAY_OPTION: &str = "stay_paused";

/// The whole percentages an over-pace card quotes, kept in the card's body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaceNumbers {
    pub today_spent: u32,
    pub today_share: u32,
    pub week_percent: u32,
}

impl PaceNumbers {
    pub fn of(usage: &Usage) -> Self {
        let whole = |percent: f64| percent.round().max(0.0) as u32;
        Self {
            today_spent: whole(usage.today_spent.unwrap_or(0.0)),
            today_share: whole(usage.today_share.unwrap_or(0.0)),
            week_percent: whole(usage.week_percent),
        }
    }

    pub fn to_body(self) -> String {
        format!("{}/{}/{}", self.today_spent, self.today_share, self.week_percent)
    }

    pub fn from_body(body: &str) -> Option<Self> {
        let mut parts = body.split('/').map(|part| part.parse::<u32>().ok());
        let numbers = Self { today_spent: parts.next()??, today_share: parts.next()??, week_percent: parts.next()?? };
        parts.next().is_none().then_some(numbers)
    }
}

const DAY_MILLIS: f64 = 86_400_000.0;

/// `day_start` is the week's percent at today's first measurement; `now` and `measured_at` are
/// Unix milliseconds.
pub fn compute(limits: &[RateLimit], day_start: Option<f64>, measured_at: Millis, now: Millis) -> Option<Usage> {
    let window = |kind: &str| limits.iter().find(|limit| limit.kind == kind);
    let week = window(SEVEN_DAY)?;
    let resets_at = week.resets_at.filter(|reset| *reset > now)?;
    let days_left = ((resets_at - now).max(0) as f64 / DAY_MILLIS).ceil().max(1.0);
    let today_spent = day_start.map(|start| (week.percent_used - start).max(0.0));
    let today_share = day_start.map(|start| ((100.0 - start) / days_left).max(0.1));
    let five = window(FIVE_HOUR).filter(|limit| limit.resets_at.is_none_or(|reset| reset > now));
    Some(Usage {
        week_percent: week.percent_used,
        week_resets_at: resets_at,
        today_spent,
        today_share,
        per_day_left: (100.0 - week.percent_used).max(0.0) / days_left,
        five_hour_percent: five.map(|limit| limit.percent_used),
        five_hour_resets_at: five.and_then(|limit| limit.resets_at),
        measured_at,
        over_pace: today_spent.zip(today_share).is_some_and(|(spent, share)| spent >= share),
    })
}

fn words(text: &str) -> Vec<&str> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()).collect()
}

/// Text that names memory, disk or CPU is about the Mac, even if it says "usage".
fn about_the_mac(words: &[&str]) -> bool {
    let mac = ["memory", "memoria", "ram", "swap", "disk", "disco", "cpu", "load", "carga"];
    words.iter().any(|word| mac.contains(word))
}

/// Whether a declared wait is about the subscription's usage rather than the Mac's resources.
pub fn usage_wait(reason: &str) -> bool {
    let reason = reason.to_lowercase();
    let words = words(&reason);
    if about_the_mac(&words) {
        return false;
    }
    let usage_words = ["usage", "weekly", "semanal", "quota", "cuota", "token", "tokens", "budget", "presupuesto", "subscription", "suscripción", "suscripcion"];
    let usage_phrases = ["de uso", "uso semanal", "uso de la", "rate limit", "5-hour", "5 hour", "5 horas", "5h"];
    words.iter().any(|word| usage_words.contains(word)) || usage_phrases.iter().any(|phrase| reason.contains(phrase))
}

/// Whether a node's question asks whether to stop, pause or wait for the subscription's usage.
/// That is never a node's call, nor the orchestrator's: the user decides on the usage card and
/// Alondra paces the nodes. Narrower than `usage_wait`: a question must name the usage and a stop,
/// and a lone "token" (a login token, say) is not usage.
pub fn usage_stop_question(text: &str) -> bool {
    let text = text.to_lowercase();
    let words = words(&text);
    if about_the_mac(&words) {
        return false;
    }
    let usage_words = ["usage", "quota", "quotas", "cuota", "budget", "presupuesto", "subscription", "suscripción", "suscripcion", "5h"];
    let usage_phrases = [
        "de uso", "uso semanal", "uso de la", "rate limit", "weekly limit", "5-hour", "5 hour", "límite semanal", "limite semanal",
        "límite de 5", "limite de 5", "token budget", "tokens left", "out of tokens",
    ];
    let stop_words = [
        "stop", "stops", "stopping", "pause", "pausing", "hold", "holding", "wait", "waiting", "halt", "parar", "paro", "pare", "paramos",
        "detener", "detengo", "pausar", "pauso", "pausa", "esperar", "espero", "espere", "esperamos", "frenar", "freno",
    ];
    let stop_phrases = ["until the reset", "until it resets", "hasta el reinicio", "hasta que se reinicie", "hasta que reinicie"];
    let usage = words.iter().any(|word| usage_words.contains(word)) || usage_phrases.iter().any(|phrase| text.contains(phrase));
    let stop = words.iter().any(|word| stop_words.contains(word)) || stop_phrases.iter().any(|phrase| text.contains(phrase));
    usage && stop
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_for_the_subscriptions_usage_is_refused_and_the_macs_resources_are_not() {
        for usage in [
            "Límite de uso semanal casi agotado (98 %); retomo el lunes 12 oct a las 02:00",
            "weekly usage limit at 99%",
            "Rate limit reached",
            "Presupuesto de tokens agotado",
            "límite de 5 horas",
        ] {
            assert!(usage_wait(usage), "{usage}");
        }
        for mac in [
            "Sin memoria: el build murió por falta de memoria",
            "disk full",
            "Disco casi lleno",
            "memory usage at 95%",
            "CPU load too high",
            "uso de memoria al 95 %",
        ] {
            assert!(!usage_wait(mac), "{mac}");
        }
    }

    #[test]
    fn a_question_about_stopping_for_usage_is_told_apart_from_other_questions() {
        for stop in [
            "Weekly usage is at 92%: should I stop until the reset?",
            "Hold until the reset? The 5-hour limit is almost used",
            "¿Paro hasta que se reinicie el límite de uso?",
            "Uso semanal al 95 %: ¿espero al lunes?",
            "Pause the task to save quota?",
        ] {
            assert!(usage_stop_question(stop), "{stop}");
        }
        for other in [
            "Which API should I use?",
            "Should I wait for the token refresh fix to land?",
            "¿Espero a que termine la revisión de la PR 12?",
            "Memory usage is at 95%: should I wait before building?",
            "Show the weekly usage on the Tasks screen?",
            "¿Paro el servidor de pruebas al acabar?",
        ] {
            assert!(!usage_stop_question(other), "{other}");
        }
    }

    /// This Mac on 23 Sep at ~21:30: 53% of the week used, 12% at the day's first measurement,
    /// reset in just under 5 days. The guard said "41% against the 18% that today's slice allows".
    fn today() -> (Vec<RateLimit>, Millis) {
        let limits = vec![
            RateLimit { kind: FIVE_HOUR.into(), percent_used: 6.0, resets_at: Some(1_790_202_000_000) },
            RateLimit { kind: SEVEN_DAY.into(), percent_used: 53.0, resets_at: Some(1_790_553_600_000) },
        ];
        (limits, 1_790_192_202_000)
    }

    #[test]
    fn today_is_measured_against_its_slice_of_the_week() {
        let (limits, now) = today();
        let u = compute(&limits, Some(12.0), now, now).unwrap();
        assert_eq!(u.week_percent, 53.0);
        assert_eq!(u.week_resets_at, 1_790_553_600_000);
        assert_eq!(u.today_spent, Some(41.0));
        assert_eq!(u.today_share.map(f64::round), Some(18.0));
        assert_eq!(u.per_day_left.round(), 9.0);
        assert!(u.over_pace);
        assert_eq!(u.five_hour_percent, Some(6.0));
        assert_eq!(u.five_hour_resets_at, Some(1_790_202_000_000));
        assert_eq!(u.measured_at, now);
    }

    #[test]
    fn a_day_within_its_slice_or_without_a_start_is_not_over_pace() {
        let (limits, now) = today();
        assert!(!compute(&limits, Some(45.0), now, now).unwrap().over_pace, "8% spent against a ~11% slice");
        let unknown = compute(&limits, None, now, now).unwrap();
        assert_eq!((unknown.today_spent, unknown.over_pace), (None, false));
        assert!(compute(&limits[..1], Some(12.0), now, now).is_none(), "no week window, no usage");
    }

    #[test]
    fn a_window_whose_reset_has_passed_is_left_out() {
        let (limits, now) = today();
        let after_five_hours = 1_790_202_000_001;
        let usage = compute(&limits, Some(12.0), now, after_five_hours).unwrap();
        assert_eq!(usage.five_hour_percent, None, "the 5-hour window reset");
        assert_eq!(usage.week_percent, 53.0);
        assert!(compute(&limits, Some(12.0), now, 1_790_553_600_001).is_none(), "the week reset");
    }
}
