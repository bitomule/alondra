//! What the daemon does while Claude usage has run out on this Mac. One durable record says
//! until when Claude reported the limit lasts; while it stands, the orchestrator starts no
//! batch, no scheduled launch or node revival calls Claude, and nodes the limit stopped wait for
//! the machine. When the time comes the daemon resumes: it wakes those nodes, and the batch and
//! the deferred schedules run again, each once. A limit that is still there when they run
//! records itself again, later and after a growing backoff.
//!
//! How a limit shows up (Claude Code docs): a node's `StopFailure` hook with `error_type`
//! `rate_limit` carries no reset time (code.claude.com/docs/en/hooks), so the reset comes from
//! the plan windows the node mod measures (`rate_limits[].resets_at`) or from the text
//! "You've hit your session limit · resets 3:45pm" (code.claude.com/docs/en/errors); a `claude -p`
//! stream says it with a `rate_limit_event` whose `rate_limit_info.status` is `rejected` and
//! whose `resetsAt` is Unix seconds, and with an assistant message whose `error` is `rate_limit`
//! (code.claude.com/docs/en/agent-sdk/python, RateLimitEvent).
use alondra_core::{Millis, now_millis};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::watch;

pub(crate) const FILE: &str = "usage-exhausted.json";
const UNKNOWN_RESET_WAIT_MS: Millis = 30 * 60_000;
const FLAP_WINDOW_MS: Millis = 15 * 60_000;
const BACKOFF_BASE_MS: Millis = 60_000;
const BACKOFF_CAP_MS: Millis = 30 * 60_000;
const JITTER_MAX_MS: u64 = 45_000;
pub(crate) const RESUME_MESSAGE: &str = "Your Claude usage limit has reset. Carry on with what you were doing.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Exhausted {
    pub until: Millis,
    pub since: Millis,
    /// The user paused until tomorrow; Claude did not run out.
    #[serde(default)]
    pub by_user: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct Record {
    exhausted: Option<Exhausted>,
    /// The user was told about the standing exhaustion.
    #[serde(default)]
    announced: bool,
    #[serde(default)]
    resumed_at: Option<Millis>,
    /// Exhaustions that came back right after a resume, which lengthen the next wait.
    #[serde(default)]
    strikes: u32,
    /// Pace cards whose answer was already acted on, the latest last.
    #[serde(default)]
    settled_cards: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Started,
    Extended,
    Unchanged,
}

pub(crate) struct UsagePause {
    record: Mutex<Record>,
    file: Option<PathBuf>,
    changed: watch::Sender<Option<Exhausted>>,
}

impl Default for UsagePause {
    fn default() -> Self {
        Self::from_record(Record::default(), None)
    }
}

impl UsagePause {
    pub(crate) fn persisted_in(home: &Path) -> Self {
        let file = home.join(FILE);
        let record = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        Self::from_record(record, Some(file))
    }

    fn from_record(record: Record, file: Option<PathBuf>) -> Self {
        let (changed, _) = watch::channel(record.exhausted.clone());
        Self { record: Mutex::new(record), file, changed }
    }

    pub(crate) fn current(&self) -> Option<Exhausted> {
        self.record.lock().expect("usage pause poisoned").exhausted.clone()
    }

    pub(crate) fn is_active(&self) -> bool {
        self.current().is_some()
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<Option<Exhausted>> {
        self.changed.subscribe()
    }

    /// Resolves once nothing is exhausted.
    pub(crate) async fn until_clear(&self) {
        let mut changes = self.subscribe();
        let _ = changes.wait_for(Option::is_none).await;
    }

    fn save(&self, record: &Record) {
        let Some(file) = &self.file else { return };
        let next = file.with_extension("next");
        let written = serde_json::to_vec(record).map_err(std::io::Error::from)
            .and_then(|bytes| std::fs::write(&next, bytes))
            .and_then(|()| std::fs::rename(&next, file));
        if let Err(error) = written {
            log!("could not save {}: {error}", file.display());
        }
    }

    /// Claude said usage is out; `until` is the reset it reported, when known. A later reset
    /// extends the standing exhaustion. One that returns right after a resume waits longer
    /// each time.
    pub(crate) fn exhaust(&self, until: Option<Millis>, now: Millis) -> Change {
        let mut record = self.record.lock().expect("usage pause poisoned");
        let reported = until.filter(|at| *at > now).unwrap_or(now + UNKNOWN_RESET_WAIT_MS);
        let change = match record.exhausted.as_mut() {
            Some(standing) if standing.by_user => {
                standing.by_user = false;
                standing.until = standing.until.max(reported);
                record.announced = false;
                Change::Extended
            }
            Some(standing) if reported > standing.until => {
                standing.until = reported;
                Change::Extended
            }
            Some(_) => Change::Unchanged,
            None => {
                let flapping = record.resumed_at.is_some_and(|at| now - at < FLAP_WINDOW_MS);
                record.strikes = if flapping { record.strikes + 1 } else { 0 };
                let backoff = if record.strikes == 0 { 0 } else { (BACKOFF_BASE_MS << record.strikes.min(10)).min(BACKOFF_CAP_MS) };
                record.exhausted = Some(Exhausted { until: reported.max(now + backoff), since: now, by_user: false });
                record.announced = false;
                Change::Started
            }
        };
        if change != Change::Unchanged {
            self.save(&record);
            self.changed.send_replace(record.exhausted.clone());
        }
        change
    }

    /// The user chose to pause until `until`. A standing pause or exhaustion is kept, lasting at
    /// least until then.
    pub(crate) fn pause_by_user(&self, until: Millis, now: Millis) -> Change {
        let mut record = self.record.lock().expect("usage pause poisoned");
        let change = match record.exhausted.as_mut() {
            Some(standing) if until > standing.until => {
                standing.until = until;
                Change::Extended
            }
            Some(_) => Change::Unchanged,
            None => {
                record.exhausted = Some(Exhausted { until, since: now, by_user: true });
                Change::Started
            }
        };
        if change != Change::Unchanged {
            self.save(&record);
            self.changed.send_replace(record.exhausted.clone());
        }
        change
    }

    /// True the first time a pace card's answer is claimed, so it is acted on once.
    pub(crate) fn claim_card(&self, id: &str) -> bool {
        let mut record = self.record.lock().expect("usage pause poisoned");
        if record.settled_cards.iter().any(|settled| settled == id) {
            return false;
        }
        record.settled_cards.push(id.to_owned());
        let excess = record.settled_cards.len().saturating_sub(32);
        record.settled_cards.drain(..excess);
        self.save(&record);
        true
    }

    /// The exhaustion the user has not been told about yet, marked as told, and whether it
    /// returned after a resume.
    pub(crate) fn claim_announcement(&self) -> Option<(Exhausted, bool)> {
        let mut record = self.record.lock().expect("usage pause poisoned");
        if record.announced {
            return None;
        }
        let exhausted = record.exhausted.clone().filter(|standing| !standing.by_user)?;
        record.announced = true;
        self.save(&record);
        Some((exhausted, record.strikes > 0))
    }

    /// Clears the exhaustion; true when the user was told about it, so they hear it is over.
    pub(crate) fn resume(&self, now: Millis) -> Option<bool> {
        let mut record = self.record.lock().expect("usage pause poisoned");
        let ended = record.exhausted.take()?;
        if !ended.by_user {
            record.resumed_at = Some(now);
        }
        let announced = std::mem::take(&mut record.announced);
        self.save(&record);
        self.changed.send_replace(None);
        Some(announced)
    }
}

/// Whether Claude's own words say a plan usage window ran out. A spend limit is not one: it
/// does not come back by itself.
pub(crate) fn says_usage_limit(text: &str) -> bool {
    let text = text.to_lowercase();
    text.contains("hit your") && text.contains("limit") && !text.contains("spend limit") && !text.contains("shared budget")
}

/// The reset time in "You've hit your session limit · resets 3:45pm" or "... · resets Mon 12:00am",
/// the next time the local clock shows it.
pub(crate) fn reset_in(text: &str, now: Millis) -> Option<Millis> {
    let after = &text[text.to_lowercase().find("resets ")? + "resets ".len()..];
    let mut words = after.split_whitespace();
    let mut word = words.next()?;
    let weekday = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"].iter()
        .position(|day| word.len() >= 3 && word[..3].eq_ignore_ascii_case(day));
    if weekday.is_some() {
        word = words.next()?;
    }
    let lower = word.trim_end_matches(|c: char| !c.is_ascii_alphanumeric()).to_lowercase();
    let (clock, pm) = match (lower.strip_suffix("am"), lower.strip_suffix("pm")) {
        (Some(clock), _) => (clock, false),
        (_, Some(clock)) => (clock, true),
        _ => return None,
    };
    let (hour, minute) = match clock.split_once(':') {
        Some((hour, minute)) => (hour.parse::<i32>().ok()?, minute.parse::<i32>().ok()?),
        None => (clock.parse::<i32>().ok()?, 0),
    };
    if !(1..=12).contains(&hour) || !(0..60).contains(&minute) {
        return None;
    }
    let hour24 = hour % 12 + if pm { 12 } else { 0 };
    (0..=8).map(|days| local_at(now, days, hour24, minute)).find(|at| {
        *at > now && weekday.is_none_or(|wanted| local_weekday(*at) == wanted)
    })
}

fn local_at(now: Millis, days_ahead: i32, hour: i32, minute: i32) -> Millis {
    let t = now.div_euclid(1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    tm.tm_mday += days_ahead;
    tm.tm_hour = hour;
    tm.tm_min = minute;
    tm.tm_sec = 0;
    tm.tm_isdst = -1;
    unsafe { libc::mktime(&mut tm) as Millis * 1000 }
}

fn local_midnight(millis: Millis) -> Millis {
    local_at(millis, 0, 0, 0)
}

fn local_weekday(millis: Millis) -> usize {
    let t = millis.div_euclid(1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    tm.tm_wday as usize
}

/// "15:45", with the weekday in front when it is not today.
pub(crate) fn clock_text(millis: Millis, now: Millis, spanish: bool) -> String {
    let t = millis.div_euclid(1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    let time = format!("{:02}:{:02}", tm.tm_hour, tm.tm_min);
    if local_midnight(millis) == local_midnight(now) {
        return time;
    }
    let days = if spanish { ["dom", "lun", "mar", "mié", "jue", "vie", "sáb"] } else { ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"] };
    format!("{} {time}", days[tm.tm_wday as usize])
}

pub(crate) fn out_text(until: Millis, now: Millis, spanish: bool) -> String {
    let clock = clock_text(until, now, spanish);
    if spanish { format!("Sin cuota hasta {clock}") } else { format!("Out of Claude usage until {clock}") }
}

pub(crate) fn back_text(spanish: bool) -> String {
    if spanish { "Vuelve la cuota de Claude: los nodos continúan".into() } else { "Claude usage is back: nodes are carrying on".into() }
}

/// The next local midnight after `now`.
pub(crate) fn next_local_midnight(now: Millis) -> Millis {
    local_at(now, 1, 0, 0)
}

fn option(id: &str, label: &str) -> alondra_core::domain::QuestionOption {
    alondra_core::domain::QuestionOption { id: id.into(), label: label.into(), description: None, inputs: vec![] }
}

/// A launch going ahead over today's slice of the week puts a card in front of the user, once
/// per local day: pause until tomorrow or keep going. The launch is never refused.
pub(crate) fn raise_over_pace_card(domain: &crate::domain_runtime::DomainRuntime, now: Millis) {
    use alondra_core::usage::{OVER_PACE_KEEP_OPTION, OVER_PACE_PAUSE_OPTION, OVER_PACE_PREFIX, PaceNumbers};
    let Some(usage) = domain.live.mods.usage(now).filter(|usage| usage.over_pace) else { return };
    let numbers = PaceNumbers::of(&usage);
    let id = format!("{OVER_PACE_PREFIX}{}", crate::node_reports::local_day(now));
    let options = vec![option(OVER_PACE_PAUSE_OPTION, "Pause until tomorrow"), option(OVER_PACE_KEEP_OPTION, "Keep going")];
    let raised = domain.ask_user(&id, "Today is over its share of the weekly Claude usage".into(),
        format!("{}% used today against a {}% share · {}% of the week used", numbers.today_spent, numbers.today_share, numbers.week_percent),
        numbers.to_body(), options, OVER_PACE_KEEP_OPTION);
    log_unless_repeated(raised, "the usage pace");
}

fn log_unless_repeated(result: Result<alondra_core::domain::CommandOutcome, crate::domain_store::StoreError>, what: &str) {
    if let Err(error) = result
        && !matches!(&error, crate::domain_store::StoreError::Domain(domain) if domain.code == "invalid_input")
    {
        log!("could not tell the user about {what}: {error:?}");
    }
}

/// Acts on the answers to the pace cards, once each: "Pause until tomorrow" pauses everything
/// until the next local midnight and offers to resume early, "Resume now" ends that pause. A
/// resume card that no longer has a pause behind it is withdrawn.
pub(crate) fn settle_pace_cards(domain: &crate::domain_runtime::DomainRuntime, now: Millis) {
    use alondra_core::domain::InteractionPayload;
    use alondra_core::usage::{OVER_PACE_PAUSE_OPTION, OVER_PACE_PREFIX, PACE_RESUME_OPTION, PACE_RESUME_PREFIX, PACE_STAY_OPTION};
    let Ok(state) = domain.state() else { return };
    let pause = &domain.live.usage_pause;
    for (id, interaction) in state.interactions.iter().filter(|(_, interaction)| !interaction.withdrawn) {
        let InteractionPayload::Question { answer: Some(answer), .. } = &interaction.payload else { continue };
        if id.0.starts_with(OVER_PACE_PREFIX) && answer.option_id == OVER_PACE_PAUSE_OPTION && pause.claim_card(&id.0) {
            pause.pause_by_user(next_local_midnight(now), now);
            let resume = format!("{PACE_RESUME_PREFIX}{}", crate::node_reports::local_day(now));
            let options = vec![option(PACE_RESUME_OPTION, "Resume now"), option(PACE_STAY_OPTION, "Stay paused")];
            let raised = domain.ask_user(&resume, "Resume Claude work now?".into(),
                "Paused by you until tomorrow 00:00".into(), "paused".into(), options, PACE_STAY_OPTION);
            log_unless_repeated(raised, "the pause");
        } else if id.0.starts_with(PACE_RESUME_PREFIX) && answer.option_id == PACE_RESUME_OPTION && pause.claim_card(&id.0)
            && pause.current().is_some_and(|standing| standing.by_user)
        {
            pause.resume(now);
            wake_nodes(domain);
        }
    }
    if !pause.current().is_some_and(|standing| standing.by_user) {
        for (id, interaction) in &state.interactions {
            if id.0.starts_with(PACE_RESUME_PREFIX) && !interaction.withdrawn && state.open(interaction) {
                let _ = domain.withdraw_problem(id);
            }
        }
    }
}

async fn watch_pace_cards(domain: std::sync::Arc<crate::domain_runtime::DomainRuntime>) {
    let mut cards = domain.subscribe();
    let mut pauses = domain.live.usage_pause.subscribe();
    loop {
        settle_pace_cards(&domain, now_millis());
        tokio::select! {
            changed = cards.changed() => if changed.is_err() { return },
            changed = pauses.changed() => if changed.is_err() { return },
        }
    }
}

/// How the user hears it: a push that never carries a card.
pub(crate) trait Alerts: Sync {
    fn tell(&self, text: impl Fn(bool) -> String + Send + Sync, passive: bool) -> impl Future<Output = ()> + Send;
}

/// Pushes to the user's devices, in each device's language.
pub(crate) struct PushAlerts {
    pub domain: std::sync::Arc<crate::domain_runtime::DomainRuntime>,
    pub push: std::sync::Arc<crate::push::Push>,
    pub service: Option<std::sync::Arc<crate::service::Service>>,
    pub client: std::sync::Arc<crate::client_routes::Client>,
}

impl Alerts for PushAlerts {
    async fn tell(&self, text: impl Fn(bool) -> String + Send + Sync, passive: bool) {
        let epoch = self.domain.state().map(|state| state.data_epoch).ok();
        let payload = |spanish: bool| serde_json::json!({
            "aps": {
                "alert": {"title": "Alondra", "body": text(spanish)},
                "sound": "default",
                "interruption-level": if passive { "passive" } else { "active" },
                "mutable-content": 1,
            },
            "alondra": {"protocol": alondra_core::views::VIEWS_PROTOCOL, "data_epoch": epoch, "actions": []},
        });
        if let Some(service) = &self.service {
            let sent = service.broadcast_push_report_for_device(!passive, |device| {
                payload(self.client.language_for_device(device) == alondra_core::views::Lang::Es)
            }).await;
            if let Err(error) = sent {
                log!("could not push the usage notice: {error:#}");
            }
        } else if self.push.enabled() {
            self.push.broadcast(&payload(false), !passive).await;
        }
    }
}

/// Watches the exhaustion: tells the user once when it starts and once when it ends, resumes at
/// the reset time plus a little jitter, and wakes what waited.
pub(crate) async fn run(domain: std::sync::Arc<crate::domain_runtime::DomainRuntime>, alerts: impl Alerts) {
    tokio::spawn(watch_pace_cards(domain.clone()));
    watch(domain, alerts, JITTER_MAX_MS).await
}

async fn watch(domain: std::sync::Arc<crate::domain_runtime::DomainRuntime>, alerts: impl Alerts, jitter_max_ms: u64) {
    let pause = &domain.live.usage_pause;
    let mut changes = pause.subscribe();
    loop {
        if let Some((exhausted, again)) = pause.claim_announcement() {
            let now = now_millis();
            alerts.tell(move |spanish| out_text(exhausted.until, now, spanish), again).await;
        }
        let Some(standing) = pause.current() else {
            let _ = changes.changed().await;
            continue;
        };
        let wait = (standing.until - now_millis()).max(0) as u64 + if standing.by_user { 0 } else { jitter(jitter_max_ms) };
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(wait)) => {
                if let Some(announced) = pause.resume(now_millis()) {
                    log!("Claude usage is back: resuming what the limit paused");
                    wake_nodes(&domain);
                    if announced {
                        alerts.tell(back_text, false).await;
                    }
                }
            }
            _ = changes.changed() => {}
        }
    }
}

fn jitter(max_ms: u64) -> u64 {
    u64::from_str_radix(&crate::auth::random_hex(4), 16).unwrap_or_default().checked_rem(max_ms).unwrap_or(0)
}

/// Tells every node the limit stopped to carry on, and brings back the ones whose connection
/// was lost meanwhile.
pub(crate) fn wake_nodes(domain: &crate::domain_runtime::DomainRuntime) {
    for node in domain.live.nodes().into_iter().filter(|node| node.usage_limited && !node.busy) {
        if domain.live.pushes.message(&node.session_id, RESUME_MESSAGE, true).is_err() {
            domain.live.losses.report(&node.session_id);
        }
    }
    let Ok(state) = domain.state() else { return };
    for agent in state.agents.keys().filter(|agent| !domain.live.mods.connected(&agent.0)) {
        domain.live.losses.report(&agent.0);
    }
}

/// Records the exhaustion a Claude message describes: its own reset time when it names one,
/// otherwise the one the plan windows say.
pub(crate) fn record_from_text(domain: &crate::domain_runtime::DomainRuntime, text: &str) -> bool {
    if !says_usage_limit(text) {
        return false;
    }
    let now = now_millis();
    domain.live.usage_pause.exhaust(reset_in(text, now).or_else(|| domain.live.mods.reset_after_limit(now)), now);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day: u32, hour: i32, minute: i32) -> Millis {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_year = 126;
        tm.tm_mon = 9;
        tm.tm_mday = day as i32;
        tm.tm_hour = hour;
        tm.tm_min = minute;
        tm.tm_isdst = -1;
        unsafe { libc::mktime(&mut tm) as Millis * 1000 }
    }

    #[test]
    fn an_exhaustion_is_recorded_extended_and_resumed_and_survives_a_restart() {
        let home = std::env::temp_dir().join(format!("alondra-usage-pause-{}", crate::auth::random_hex(6)));
        std::fs::create_dir_all(&home).unwrap();
        let pause = UsagePause::persisted_in(&home);
        assert!(!pause.is_active());
        assert_eq!(pause.exhaust(Some(5_000_000), 1_000_000), Change::Started);
        assert_eq!(pause.current(), Some(Exhausted { until: 5_000_000, since: 1_000_000, by_user: false }));
        assert_eq!(pause.exhaust(Some(4_000_000), 1_100_000), Change::Unchanged, "an earlier reset changes nothing");
        assert_eq!(pause.exhaust(Some(6_000_000), 1_200_000), Change::Extended);
        assert_eq!(pause.current(), Some(Exhausted { until: 6_000_000, since: 1_000_000, by_user: false }), "extended, still since the start");

        let restarted = UsagePause::persisted_in(&home);
        assert_eq!(restarted.current(), pause.current(), "a restart still knows");
        assert_eq!(restarted.claim_announcement(), Some((Exhausted { until: 6_000_000, since: 1_000_000, by_user: false }, false)));
        assert_eq!(restarted.claim_announcement(), None, "told once");
        assert_eq!(UsagePause::persisted_in(&home).claim_announcement(), None, "and a restart does not tell again");

        assert_eq!(restarted.resume(7_000_000), Some(true));
        assert_eq!(restarted.resume(7_000_001), None);
        assert!(!UsagePause::persisted_in(&home).is_active(), "the resume is persisted too");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn an_unknown_or_past_reset_waits_a_default_and_a_limit_that_returns_right_away_waits_longer() {
        let pause = UsagePause::default();
        pause.exhaust(None, 1_000_000);
        assert_eq!(pause.current().unwrap().until, 1_000_000 + UNKNOWN_RESET_WAIT_MS);
        pause.resume(1_500_000);
        pause.exhaust(Some(1_600_010), 1_600_000);
        assert_eq!(pause.current().unwrap().until, 1_600_000 + 2 * BACKOFF_BASE_MS, "the first return waits 2 min");
        pause.resume(1_700_000);
        pause.exhaust(Some(1_700_010), 1_700_000);
        assert_eq!(pause.current().unwrap().until, 1_700_000 + 4 * BACKOFF_BASE_MS, "the second waits 4 min");
        let later = UsagePause::default();
        later.exhaust(None, 1_000_000);
        later.resume(1_100_000);
        later.exhaust(None, 1_100_000 + FLAP_WINDOW_MS + 1);
        assert_eq!(later.record.lock().unwrap().strikes, 0, "a limit long after a resume is a new one");
    }

    #[test]
    fn only_a_plan_window_is_a_usage_limit() {
        for (text, limited) in [
            ("You've hit your session limit · resets 3:45pm", true),
            ("You've hit your weekly limit · resets Mon 12:00am", true),
            ("You've hit your Opus limit · resets 3:45pm", true),
            ("You've hit your monthly spend limit · raise it at claude.ai/settings/usage", false),
            ("You've hit your team's shared budget · ask your admin to raise it", false),
            ("Rate limit exceeded", false),
        ] {
            assert_eq!(says_usage_limit(text), limited, "{text}");
        }
    }

    #[test]
    fn the_reset_time_is_the_next_time_the_local_clock_shows_it() {
        let now = at(7, 14, 0);
        assert_eq!(reset_in("You've hit your session limit · resets 3:45pm", now), Some(at(7, 15, 45)));
        assert_eq!(reset_in("You've hit your session limit · resets 1:05pm", now), Some(at(8, 13, 5)), "already past today: tomorrow");
        assert_eq!(reset_in("You've hit your session limit · resets 12am (Europe/Madrid)", now), Some(at(8, 0, 0)));
        assert_eq!(reset_in("You've hit your weekly limit · resets Mon 12:00am", now), Some(at(12, 0, 0)), "7 Oct 2026 is a Wednesday");
        assert_eq!(reset_in("You've hit your session limit", now), None);
        assert_eq!(reset_in("resets soon", now), None);
    }

    #[test]
    fn the_notice_names_the_time_in_the_users_language_and_the_day_when_it_is_not_today() {
        let now = at(7, 14, 0);
        assert_eq!(out_text(at(7, 15, 45), now, false), "Out of Claude usage until 15:45");
        assert_eq!(out_text(at(7, 15, 45), now, true), "Sin cuota hasta 15:45");
        assert_eq!(out_text(at(12, 0, 0), now, false), "Out of Claude usage until Mon 00:00");
        assert_eq!(out_text(at(12, 0, 0), now, true), "Sin cuota hasta lun 00:00");
    }

    use crate::domain_runtime::DomainRuntime;
    use crate::node_push::testing::Node;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn domain() -> (Arc<DomainRuntime>, std::path::PathBuf) {
        let home = std::env::temp_dir().join(format!("alondra-usage-domain-{}", crate::auth::random_hex(6)));
        std::fs::create_dir_all(&home).unwrap();
        (Arc::new(DomainRuntime::open(&home).unwrap()), home)
    }

    fn measure(domain: &DomainRuntime, windows: serde_json::Value) {
        let envelope = serde_json::from_value(json!({"session_id": "m", "event": "session.measure", "rate_limits": windows})).unwrap();
        domain.live.mods.apply(envelope, now_millis());
        domain.live.note_measures();
    }

    fn rfc3339(millis: Millis) -> String {
        crate::log::local_iso(millis.div_euclid(1000) as libc::time_t, 0)
    }

    fn stop_failure(session: &str, error: &str, details: Option<&str>) -> serde_json::Value {
        let mut payload = json!({"session_id": session, "hook_event_name": "StopFailure", "error_type": error});
        if let Some(details) = details {
            payload["error_details"] = json!(details);
        }
        payload
    }

    #[test]
    fn a_node_stopped_by_the_limit_sets_the_exhaustion_to_the_reset_claude_names_or_the_plan_reports() {
        let (domain, home) = domain();
        let now = now_millis();
        domain.live.observe(&stop_failure("s1", "overloaded", None));
        assert!(!domain.live.usage_pause.is_active(), "an overloaded API is not an empty plan");

        measure(&domain, json!([{"kind": "five_hour", "percent_used": 80, "resets_at": rfc3339(now + 2 * 3_600_000)}]));
        domain.live.observe(&stop_failure("s1", "rate_limit", None));
        let until = domain.live.usage_pause.current().unwrap().until;
        assert!((until - (now + 2 * 3_600_000)).abs() < 2_000, "the 5-hour window's reset: {until}");

        let (named, named_home) = self::domain();
        named.live.observe(&stop_failure("s1", "rate_limit", Some("You've hit your session limit · resets 11:59pm")));
        let until = named.live.usage_pause.current().unwrap().until;
        assert!(until > now && until <= now + 24 * 3_600_000 && clock_text(until, now, false).ends_with("23:59"), "{until}");

        let (unknown, unknown_home) = self::domain();
        unknown.live.observe(&stop_failure("s1", "rate_limit", None));
        let until = unknown.live.usage_pause.current().unwrap().until;
        assert!((until - (now + UNKNOWN_RESET_WAIT_MS)).abs() < 2_000, "no reset known: the default wait");
        for path in [home, named_home, unknown_home] {
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn a_measurement_with_a_window_used_up_is_the_limit_too() {
        let (domain, home) = domain();
        let now = now_millis();
        measure(&domain, json!([{"kind": "five_hour", "percent_used": 60, "resets_at": rfc3339(now + 3_600_000)}]));
        assert!(!domain.live.usage_pause.is_active());
        measure(&domain, json!([
            {"kind": "five_hour", "percent_used": 100, "resets_at": rfc3339(now + 3_600_000)},
            {"kind": "seven_day", "percent_used": 40, "resets_at": rfc3339(now + 4 * 86_400_000)},
        ]));
        let until = domain.live.usage_pause.current().unwrap().until;
        assert!((until - (now + 3_600_000)).abs() < 2_000, "{until}");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn waking_tells_each_node_the_limit_stopped_to_carry_on_and_nobody_else() {
        let (domain, home) = domain();
        domain.live.observe(&stop_failure("limited", "rate_limit", None));
        domain.live.observe(&json!({"session_id": "fine", "hook_event_name": "UserPromptSubmit", "prompt": "go"}));
        let limited = Node::connect(&domain, "limited");
        let fine = Node::connect(&domain, "fine");
        wake_nodes(&domain);
        tokio::task::yield_now().await;
        assert_eq!(limited.texts(), [RESUME_MESSAGE]);
        assert!(fine.texts().is_empty());
        std::fs::remove_dir_all(home).unwrap();
    }

    struct Told(Mutex<Vec<(String, bool)>>);
    impl Alerts for Arc<Told> {
        async fn tell(&self, text: impl Fn(bool) -> String + Send + Sync, passive: bool) {
            self.0.lock().unwrap().push((text(false), passive));
        }
    }

    #[tokio::test]
    async fn the_watcher_tells_once_when_it_starts_resumes_at_the_reset_wakes_the_nodes_and_tells_once_when_it_ends() {
        let (domain, home) = domain();
        domain.live.apply(&alondra_core::Event::Hook(alondra_core::HookEvent {
            session_id: "limited".into(), event: "StopFailure".into(), error_type: Some("rate_limit".into()), ..Default::default()
        }));
        let node = Node::connect(&domain, "limited");
        let told = Arc::new(Told(Mutex::new(vec![])));
        domain.live.usage_pause.exhaust(Some(now_millis() + 150), now_millis());
        let watcher = tokio::spawn(watch(domain.clone(), told.clone(), 0));
        for _ in 0..200 {
            if !domain.live.usage_pause.is_active() && !node.texts().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        watcher.abort();
        assert!(!domain.live.usage_pause.is_active(), "resumed at the reset");
        assert_eq!(node.texts(), [RESUME_MESSAGE], "the node the limit stopped is told once");
        let told = told.0.lock().unwrap().clone();
        assert_eq!(told.len(), 2, "one when it started and one when it ended: {told:?}");
        assert!(told[0].0.starts_with("Out of Claude usage until "), "{told:?}");
        assert_eq!(told[1].0, "Claude usage is back: nodes are carrying on");
        std::fs::remove_dir_all(home).unwrap();
    }

    fn over_pace_domain() -> (Arc<DomainRuntime>, std::path::PathBuf) {
        let (domain, home) = domain();
        let reset = rfc3339(now_millis() + 5 * 86_400_000);
        for percent in [12.0, 53.0] {
            measure(&domain, json!([{"kind": "seven_day", "percent_used": percent, "resets_at": reset}]));
        }
        assert!(domain.live.mods.usage(now_millis()).unwrap().over_pace);
        (domain, home)
    }

    fn the_card(domain: &DomainRuntime, prefix: &str) -> Option<alondra_core::domain::Interaction> {
        domain.state().unwrap().interactions.values().find(|card| card.id.0.starts_with(prefix) && !card.withdrawn).cloned()
    }

    fn answer(domain: &DomainRuntime, card: &str, option: &str) {
        let state = domain.state().unwrap();
        domain.execute(&alondra_core::domain::Actor::User { user_id: "device:phone".into() }, &alondra_core::domain::CommandEnvelope {
            protocol_version: alondra_core::domain::PROTOCOL_VERSION,
            data_epoch: state.data_epoch,
            command_id: format!("answer-{card}-{option}").into(),
            expected_revision: state.revision,
            command: alondra_core::domain::Command::AnswerQuestion {
                attachments: vec![], interaction_id: card.into(), option_id: option.into(), inputs: Default::default(),
            },
        }, None, || Ok(())).unwrap();
    }

    #[test]
    fn the_over_pace_card_has_the_numbers_and_two_options_and_reads_in_each_devices_language() {
        use alondra_core::views::{Lang, push_content};
        let (domain, home) = over_pace_domain();
        raise_over_pace_card(&domain, now_millis());
        let card = the_card(&domain, "over-pace-").expect("a card for the day");
        assert_eq!(card.id.0, format!("over-pace-{}", crate::node_reports::local_day(now_millis())));
        assert_eq!(card.card.kind, alondra_core::domain::CardKind::Decide);
        assert_eq!(card.author, alondra_core::domain::Author::System);
        assert!(!card.blocking);
        assert_eq!(card.card.recommended.as_deref(), Some("keep_going"));
        let alondra_core::domain::InteractionPayload::Question { options, .. } = &card.payload else { panic!("a question") };
        assert_eq!(options.iter().map(|option| option.id.as_str()).collect::<Vec<_>>(), ["pause_until_tomorrow", "keep_going"]);

        let state = domain.state().unwrap();
        let english = push_content(&state, &card.id.0, Lang::En).unwrap();
        let spanish = push_content(&state, &card.id.0, Lang::Es).unwrap();
        assert_eq!(english.interruption_level, "passive");
        assert_eq!(english.alert.as_ref().unwrap().title, "Today is over its share of the weekly Claude usage");
        assert_eq!(english.alert.unwrap().body.as_deref(), Some("41% used today against a 18% share · 53% of the week used"));
        assert_eq!(spanish.alert.as_ref().unwrap().title, "Hoy vas por encima de tu parte del uso semanal");
        assert_eq!(spanish.alert.unwrap().body.as_deref(), Some("41% usado hoy frente a una parte del 18% · 53% de la semana usado"));
        let labels = |lang| push_content(&state, &card.id.0, lang).unwrap().actions.into_iter().map(|action| action.label).collect::<Vec<_>>();
        let english_labels = labels(Lang::En);
        let spanish_labels = labels(Lang::Es);
        assert!(english_labels.iter().any(|label| label == "Pause until tomorrow") && english_labels.iter().any(|label| label.ends_with("Keep going")), "{english_labels:?}");
        assert!(spanish_labels.iter().any(|label| label == "Pausar hasta mañana") && spanish_labels.iter().any(|label| label.ends_with("Seguir")), "{spanish_labels:?}");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn answering_pause_pauses_until_local_midnight_and_resume_now_clears_it() {
        let (domain, home) = over_pace_domain();
        raise_over_pace_card(&domain, now_millis());
        let id = the_card(&domain, "over-pace-").unwrap().id.0;
        answer(&domain, &id, "pause_until_tomorrow");
        let now = now_millis();
        settle_pace_cards(&domain, now);
        let standing = domain.live.usage_pause.current().expect("paused");
        assert!(standing.by_user);
        assert_eq!(standing.until, next_local_midnight(now));
        assert!(standing.until > now && standing.until <= now + 25 * 3_600_000);
        assert!(domain.live.usage_pause.claim_announcement().is_none(), "the user did this: no push about it");
        let resume = the_card(&domain, "pace-resume-").expect("a way to resume early");

        settle_pace_cards(&domain, now);
        assert_eq!(domain.live.usage_pause.current().unwrap().until, standing.until, "answered once, acted on once");
        answer(&domain, &resume.id.0, "resume_now");
        settle_pace_cards(&domain, now_millis());
        assert!(!domain.live.usage_pause.is_active(), "resume now clears the pause");
        assert_eq!(domain.live.usage_pause.record.lock().unwrap().strikes, 0);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn answering_keep_going_leaves_nothing_paused() {
        let (domain, home) = over_pace_domain();
        raise_over_pace_card(&domain, now_millis());
        let id = the_card(&domain, "over-pace-").unwrap().id.0;
        answer(&domain, &id, "keep_going");
        settle_pace_cards(&domain, now_millis());
        assert!(!domain.live.usage_pause.is_active());
        assert!(the_card(&domain, "pace-resume-").is_none());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_resume_card_goes_when_the_pause_ends_by_itself_and_a_claude_limit_takes_over_a_user_pause() {
        let (domain, home) = over_pace_domain();
        raise_over_pace_card(&domain, now_millis());
        answer(&domain, &the_card(&domain, "over-pace-").unwrap().id.0, "pause_until_tomorrow");
        settle_pace_cards(&domain, now_millis());
        assert!(the_card(&domain, "pace-resume-").is_some());
        let pause = &domain.live.usage_pause;
        let until = pause.current().unwrap().until;
        pause.exhaust(Some(until + 3_600_000), now_millis());
        let standing = pause.current().unwrap();
        assert!(!standing.by_user && standing.until == until + 3_600_000, "Claude's limit is the reason now");
        assert!(pause.claim_announcement().is_some(), "and the user hears about it");
        pause.resume(now_millis());
        settle_pace_cards(&domain, now_millis());
        assert!(the_card(&domain, "pace-resume-").is_none(), "nothing left to resume");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn a_pause_by_the_user_resumes_by_itself_at_its_time_without_telling_anyone() {
        let (domain, home) = domain();
        let told = Arc::new(Told(Mutex::new(vec![])));
        domain.live.usage_pause.pause_by_user(now_millis() + 100, now_millis());
        let watcher = tokio::spawn(watch(domain.clone(), told.clone(), 0));
        for _ in 0..200 {
            if !domain.live.usage_pause.is_active() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        watcher.abort();
        assert!(!domain.live.usage_pause.is_active());
        assert!(told.0.lock().unwrap().is_empty());
        std::fs::remove_dir_all(home).unwrap();
    }
}
