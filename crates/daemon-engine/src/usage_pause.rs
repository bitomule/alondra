//! What the daemon does while Claude usage has run out on this Mac: no account the user has can
//! work (each ran out of its session or its week, or is signed out), or Claude reported a limit
//! the accounts' readings do not show yet. When no account has a session at all, the wait has no
//! time: it ends when the accounts check sees one with a session, never by the clock. Otherwise
//! one record says until when it lasts; while it stands, the orchestrator starts no
//! batch, no scheduled launch or node revival calls Claude, and nodes the limit stopped wait for
//! the machine. When the time comes the daemon resumes: it wakes those nodes, and the batch and
//! the deferred schedules run again, each once. A limit that is still there when they run
//! records itself again, later and after a growing backoff.
//!
//! The record lives in memory: after a restart the probe of each account's usage
//! (`usage_probe`) finds again a window that ran out, and a pause the user chose comes back from
//! the answered card, which the domain keeps.
//!
//! How a limit shows up (Claude Code docs): a node's `StopFailure` hook with `error_type`
//! `rate_limit` carries no reset time (code.claude.com/docs/en/hooks), so the reset comes from
//! the plan windows measured for the account (`usage_probe`) or from the text
//! "You've hit your session limit · resets 3:45pm" (code.claude.com/docs/en/errors); a `claude -p`
//! stream says it with a `rate_limit_event` whose `rate_limit_info.status` is `rejected` and
//! whose `resetsAt` is Unix seconds, and with an assistant message whose `error` is `rate_limit`
//! (code.claude.com/docs/en/agent-sdk/python, RateLimitEvent).
use crate::locks::Poisonable as _;
use alondra_core::{Millis, now_millis};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::watch;

const UNKNOWN_RESET_WAIT_MS: Millis = 30 * 60_000;
const FLAP_WINDOW_MS: Millis = 15 * 60_000;
const BACKOFF_BASE_MS: Millis = 60_000;
const BACKOFF_CAP_MS: Millis = 30 * 60_000;
const JITTER_MAX_MS: u64 = 45_000;
pub(crate) const RESUME_MESSAGE: &str = "Your Claude usage limit has reset. Carry on with what you were doing.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exhausted {
    pub until: Millis,
    pub since: Millis,
    /// The user paused until tomorrow; Claude did not run out.
    #[serde(default)]
    pub by_user: bool,
    /// No provider account is available: `until` is no time at all, only an account becoming available ends the wait.
    #[serde(default)]
    pub no_session: bool,
}

#[derive(Default)]
struct Record {
    exhausted: Option<Exhausted>,
    /// The user was told about the standing exhaustion.
    announced: bool,
    resumed_at: Option<Millis>,
    /// Exhaustions that came back right after a resume, which lengthen the next wait.
    strikes: u32,
    /// Pace cards whose answer was already acted on, the latest last.
    settled_cards: Vec<String>,
    /// The standing exhaustion is the accounts' readings saying none can work: they also end it.
    by_margin: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Started,
    Extended,
    Unchanged,
}

pub struct UsagePause {
    /// A holder that panics leaves it whole, so it is recovered: plain assignments of copied values
    /// with only arithmetic between them; the worst a panic leaves is one notice repeated or
    /// missed.
    record: Mutex<Record>,
    changed: watch::Sender<Option<Exhausted>>,
}

impl Default for UsagePause {
    fn default() -> Self {
        let (changed, _) = watch::channel(None);
        Self { record: Mutex::new(Record::default()), changed }
    }
}

impl UsagePause {
    /// Capacity on another provider ends a legacy global limit without waking still-limited Claude nodes.
    pub(crate) fn provider_account_back(&self, now: Millis) -> bool {
        let mut record = self.record.lock_or_recover("the usage pause");
        if !record.exhausted.as_ref().is_some_and(|standing| !standing.by_user) { return false; }
        record.exhausted = None;
        record.by_margin = false;
        record.announced = false;
        record.resumed_at = Some(now);
        self.changed.send_replace(None);
        true
    }

    pub fn current(&self) -> Option<Exhausted> {
        self.record.lock_or_recover("the usage pause").exhausted.clone()
    }

    pub(crate) fn is_active(&self) -> bool {
        self.current().is_some()
    }

    pub fn subscribe(&self) -> watch::Receiver<Option<Exhausted>> {
        self.changed.subscribe()
    }

    /// Claude said usage is out; `until` is the reset it reported, when known. A later reset
    /// extends the standing exhaustion. One that returns right after a resume waits longer
    /// each time.
    pub(crate) fn exhaust(&self, until: Option<Millis>, now: Millis) -> Change {
        self.exhaust_as(until, now, false)
    }

    /// No account can work until `until`, when the soonest one can again: the readings say so, so
    /// a later reading that moves it earlier moves the wait too, and no backoff lengthens it.
    pub(crate) fn exhaust_for_margin(&self, until: Option<Millis>, now: Millis) -> Change {
        self.exhaust_as(until, now, true)
    }

    /// An account can work again before the reset the exhaustion waited for: an exhaustion the
    /// readings started ends now, after the usual jitter, as at its reset. Any other stands.
    pub(crate) fn margin_back(&self, now: Millis) -> bool {
        let mut record = self.record.lock_or_recover("the usage pause");
        let by_margin = record.by_margin;
        let Some(standing) = record.exhausted.as_mut().filter(|standing| by_margin && !standing.by_user && standing.until > now) else { return false };
        standing.until = now;
        self.changed.send_replace(record.exhausted.clone());
        true
    }

    /// No Claude account has a session: new work waits with no time to end it, until an account has
    /// one. A wait the accounts' readings started becomes this one; the user's pause and a limit
    /// Claude reported stand.
    pub(crate) fn wait_for_session(&self, now: Millis) -> Change {
        let mut record = self.record.lock_or_recover("the usage pause");
        let by_margin = record.by_margin;
        let change = match record.exhausted.as_mut() {
            None => {
                record.exhausted = Some(Exhausted { until: now, since: now, by_user: false, no_session: true });
                Change::Started
            }
            Some(standing) if by_margin && !standing.by_user && !standing.no_session => {
                *standing = Exhausted { until: now, since: now, by_user: false, no_session: true };
                Change::Extended
            }
            Some(_) => return Change::Unchanged,
        };
        record.announced = false;
        record.by_margin = true;
        self.changed.send_replace(record.exhausted.clone());
        change
    }

    pub(crate) fn waits_for_session(&self) -> bool {
        self.current().is_some_and(|standing| standing.no_session)
    }

    /// An account has a session: a wait for one ends now. Any other pause stands.
    pub(crate) fn session_back(&self) -> bool {
        let mut record = self.record.lock_or_recover("the usage pause");
        if !record.exhausted.as_ref().is_some_and(|standing| standing.no_session) {
            return false;
        }
        record.exhausted = None;
        record.by_margin = false;
        record.announced = false;
        self.changed.send_replace(None);
        true
    }

    fn exhaust_as(&self, until: Option<Millis>, now: Millis, by_margin: bool) -> Change {
        let mut record = self.record.lock_or_recover("the usage pause");
        let standing = record.exhausted.is_some();
        let follows_readings = by_margin && record.by_margin;
        let reported = until.filter(|at| *at > now).unwrap_or(now + UNKNOWN_RESET_WAIT_MS);
        let change = match record.exhausted.as_mut() {
            Some(standing) if standing.no_session => {
                *standing = Exhausted { until: reported, since: now, by_user: false, no_session: false };
                record.announced = false;
                Change::Extended
            }
            Some(standing) if follows_readings && !standing.by_user && reported < standing.until => {
                standing.until = reported;
                Change::Extended
            }
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
                let backoff = if record.strikes == 0 || by_margin { 0 } else { (BACKOFF_BASE_MS << record.strikes.min(10)).min(BACKOFF_CAP_MS) };
                record.exhausted = Some(Exhausted { until: reported.max(now + backoff), since: now, by_user: false, no_session: false });
                record.announced = false;
                Change::Started
            }
        };
        record.by_margin = if standing { record.by_margin && by_margin } else { by_margin };
        if change != Change::Unchanged {
            self.changed.send_replace(record.exhausted.clone());
        }
        change
    }

    /// The user chose to pause until `until`. A standing pause or exhaustion is kept, lasting at
    /// least until then.
    pub(crate) fn pause_by_user(&self, until: Millis, now: Millis) -> Change {
        let mut record = self.record.lock_or_recover("the usage pause");
        let change = match record.exhausted.as_mut() {
            Some(standing) => {
                let changed = !standing.by_user || standing.no_session || until > standing.until;
                standing.until = standing.until.max(until);
                standing.by_user = true;
                standing.no_session = false;
                if changed { Change::Extended } else { Change::Unchanged }
            }
            None => {
                record.exhausted = Some(Exhausted { until, since: now, by_user: true, no_session: false });
                Change::Started
            }
        };
        if change != Change::Unchanged {
            self.changed.send_replace(record.exhausted.clone());
        }
        change
    }

    /// True the first time a pace card's answer is claimed, so it is acted on once.
    pub(crate) fn claim_card(&self, id: &str) -> bool {
        let mut record = self.record.lock_or_recover("the usage pause");
        if record.settled_cards.iter().any(|settled| settled == id) {
            return false;
        }
        record.settled_cards.push(id.to_owned());
        let excess = record.settled_cards.len().saturating_sub(32);
        record.settled_cards.drain(..excess);
        true
    }

    /// The exhaustion the user has not been told about yet, marked as told, and whether it
    /// returned after a resume.
    pub(crate) fn claim_announcement(&self) -> Option<(Exhausted, bool)> {
        let mut record = self.record.lock_or_recover("the usage pause");
        if record.announced {
            return None;
        }
        let exhausted = record.exhausted.clone().filter(|standing| !standing.by_user)?;
        record.announced = true;
        Some((exhausted, record.strikes > 0))
    }

    /// Clears the exhaustion; true when the user was told about it, so they hear it is over.
    pub(crate) fn resume(&self, now: Millis) -> Option<bool> {
        self.resume_matching(now, false)
    }

    fn resume_due(&self, now: Millis) -> Option<bool> {
        self.resume_matching(now, true)
    }

    fn resume_matching(&self, now: Millis, due_only: bool) -> Option<bool> {
        let mut record = self.record.lock_or_recover("the usage pause");
        if due_only && record.exhausted.as_ref().is_none_or(|standing| standing.no_session || standing.until > now) {
            return None;
        }
        let ended = record.exhausted.take()?;
        record.by_margin = false;
        if !ended.by_user {
            record.resumed_at = Some(now);
        }
        let announced = std::mem::take(&mut record.announced);
        self.changed.send_replace(None);
        Some(announced)
    }
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

/// "15:45", with the weekday in front when it is not today.
pub fn clock_text(millis: Millis, now: Millis, spanish: bool) -> String {
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

pub(crate) fn out_text(exhausted: &Exhausted, now: Millis, spanish: bool) -> String {
    if exhausted.no_session {
        return if spanish { "No hay ninguna cuenta de proveedor disponible: revisa Cuentas".into() }
            else { "No provider account is available: check Accounts".into() };
    }
    let clock = clock_text(exhausted.until, now, spanish);
    if spanish { format!("Sin margen hasta {clock}") } else { format!("No usage capacity until {clock}") }
}

pub(crate) fn back_text(spanish: bool) -> String {
    if spanish { "Vuelve a haber margen de uso".into() } else { "Usage capacity is available again".into() }
}

/// The next local midnight after `now`.
pub(crate) fn next_local_midnight(now: Millis) -> Millis {
    local_at(now, 1, 0, 0)
}

fn option(id: &str, label: &str) -> alondra_core::domain::QuestionOption {
    alondra_core::domain::QuestionOption { id: id.into(), label: label.into(), description: None }
}

/// A launch going ahead over today's slice of the week puts a card in front of the user, once
/// per local day: pause until tomorrow or keep going. The launch is never refused.
pub(crate) fn raise_over_pace_card(domain: &crate::domain_runtime::DomainRuntime, now: Millis) {
    use alondra_core::domain::SystemCard;
    use alondra_core::usage::{OVER_PACE_KEEP_OPTION, OVER_PACE_PAUSE_OPTION, OVER_PACE_PREFIX, PaceNumbers};
    let Some(usage) = crate::usage_probe::usage(domain, now).filter(|usage| usage.over_pace) else { return };
    let numbers = PaceNumbers::of(&usage);
    let day = alondra_daemon_engine_model::local_day(now);
    let id = format!("{OVER_PACE_PREFIX}{day}");
    let options = vec![option(OVER_PACE_PAUSE_OPTION, "Pause until tomorrow"), option(OVER_PACE_KEEP_OPTION, "Keep going")];
    let raised = domain.ask_user(&id, Some(SystemCard::OverPace { day: day.clone() }), "Today is over its share of the weekly Claude usage".into(),
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

/// Acts on the answers to the pace cards, once each: "Pause until tomorrow" on today's card pauses
/// everything until the next local midnight and offers to resume early, "Resume now" ends that
/// pause. A resume card that no longer has a pause behind it is withdrawn. After a restart the
/// answered cards bring the pause back as the user left it.
pub(crate) fn settle_pace_cards(domain: &crate::domain_runtime::DomainRuntime, now: Millis) {
    use alondra_core::domain::InteractionPayload;
    use alondra_core::domain::SystemCard;
    use alondra_core::usage::{OVER_PACE_PAUSE_OPTION, PACE_RESUME_OPTION, PACE_RESUME_PREFIX, PACE_STAY_OPTION};
    let Ok(state) = domain.state() else { return };
    let pause = &domain.live.usage_pause;
    for (id, interaction) in state.interactions.iter().filter(|(_, interaction)| !interaction.withdrawn) {
        let InteractionPayload::Question { answer: Some(answer), .. } = &interaction.payload else { continue };
        let today = alondra_daemon_engine_model::local_day(now);
        let over_today = matches!(&interaction.system_card, Some(SystemCard::OverPace { day }) if *day == today);
        if over_today && answer.option_id == OVER_PACE_PAUSE_OPTION && pause.claim_card(&id.0) {
            pause.pause_by_user(next_local_midnight(now), now);
            let day = alondra_daemon_engine_model::local_day(now);
            let resume = format!("{PACE_RESUME_PREFIX}{day}");
            let options = vec![option(PACE_RESUME_OPTION, "Resume now"), option(PACE_STAY_OPTION, "Stay paused")];
            let raised = domain.ask_user(&resume, Some(SystemCard::PaceResume { day: day.clone() }), "Resume Claude work now?".into(),
                "Paused by you until tomorrow 00:00".into(), "paused".into(), options, PACE_STAY_OPTION);
            log_unless_repeated(raised, "the pause");
        } else if matches!(interaction.system_card, Some(SystemCard::PaceResume { .. })) && answer.option_id == PACE_RESUME_OPTION && pause.claim_card(&id.0)
            && pause.current().is_some_and(|standing| standing.by_user)
        {
            pause.resume(now);
            wake_legacy_nodes(domain, false);
        }
    }
    if !pause.current().is_some_and(|standing| standing.by_user) {
        for (id, interaction) in &state.interactions {
            if matches!(interaction.system_card, Some(SystemCard::PaceResume { .. })) && !interaction.withdrawn && state.open(interaction) {
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
pub trait Alerts: Sync {
    fn tell(&self, text: impl Fn(bool) -> String + Send + Sync, passive: bool) -> impl Future<Output = ()> + Send;
}

/// Watches the exhaustion: tells the user once when it starts and once when it ends, resumes at
/// the reset time plus a little jitter, and wakes what waited.
pub async fn run(domain: std::sync::Arc<crate::domain_runtime::DomainRuntime>, alerts: impl Alerts) {
    tokio::spawn(watch_pace_cards(domain.clone()));
    watch(domain, alerts, JITTER_MAX_MS).await
}

async fn watch(domain: std::sync::Arc<crate::domain_runtime::DomainRuntime>, alerts: impl Alerts, jitter_max_ms: u64) {
    let pause = &domain.live.usage_pause;
    let mut changes = pause.subscribe();
    loop {
        if let Some((exhausted, again)) = pause.claim_announcement() {
            let now = now_millis();
            let passive = again || !exhausted.no_session;
            alerts.tell(move |spanish| out_text(&exhausted, now, spanish), passive).await;
        }
        let Some(standing) = pause.current().filter(|standing| !standing.no_session) else {
            let _ = changes.changed().await;
            continue;
        };
        let wait = (standing.until - now_millis()).max(0) as u64 + if standing.by_user { 0 } else { jitter(jitter_max_ms) };
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(wait)) => {
                let now = now_millis();
                crate::usage_probe::follow(&domain, now);
                if let Some(announced) = pause.resume_due(now) {
                    log!("the usage pause ended: checking each account before resuming");
                    wake_nodes(&domain);
                    if announced {
                        alerts.tell(back_text, true).await;
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

/// An account has a session again: a wait for one ends now and what it held carries on.
pub(crate) fn session_is_back(domain: &crate::domain_runtime::DomainRuntime) {
    if domain.live.usage_pause.session_back() {
        log!("a Claude account has a session: resuming what waited for one");
        wake_nodes(domain);
    }
}

/// Puts every node the limit stopped in the admission gate's queue, which tells them to carry on one
/// at a time, brings back the ones whose connection was lost meanwhile, and probes Claude with a
/// task of each account whose limit holds tasks of the Agent SDK.
pub(crate) fn wake_nodes(domain: &crate::domain_runtime::DomainRuntime) {
    crate::claude_access::probe_usage_holds(domain);
    wake_legacy_nodes(domain, true);
}

fn wake_legacy_nodes(domain: &crate::domain_runtime::DomainRuntime, require_capacity: bool) {
    let now = now_millis();
    let Ok(state) = domain.state() else { return };
    for node in domain.live.nodes().into_iter().filter(|node| node.usage_limited && !node.busy) {
        if !legacy_wake_ready(&state, &alondra_core::domain::AgentId(node.session_id.clone()), now, require_capacity) {
            continue;
        }
        if !domain.live.gate.hold_back(domain, &node.session_id, crate::pauses::Reason::Usage, now) {
            let _ = crate::sdk_handover::tell(domain, &node.session_id, crate::node_push::ALONDRA, RESUME_MESSAGE, true);
        }
    }
}

fn legacy_wake_ready(state: &alondra_core::domain::DomainState, agent: &alondra_core::domain::AgentId, now: Millis, require_capacity: bool) -> bool {
    use alondra_core::accounts::{AccountKey, Provider};
    if crate::sdk_owner::runs_through_sdk(state, agent) { return false; }
    let mut tasks = state.tasks.values().filter(|task| task.agent_id.as_ref() == Some(agent));
    let key = if let Some(task) = tasks.next() {
        let key = task.account_key();
        if key.provider != Provider::Claude || tasks.any(|task| task.account_key() != key) { return false; }
        key
    } else {
        // An unbound legacy session has no credential scope in a mixed or named-account installation.
        if state.accounts.len() != 1 || state.accounts[0].provider != Provider::Claude || state.accounts[0].id != alondra_core::accounts::AccountId::default_account() { return false; }
        AccountKey { provider: Provider::Claude, account: alondra_core::accounts::AccountId::default_account() }
    };
    account_retry_ready(state, &key, now, now, require_capacity)
}

pub(crate) fn task_usage_ready(state: &alondra_core::domain::DomainState, task: &alondra_core::domain::TaskId, since: Millis, now: Millis) -> bool {
    state.tasks.get(task).is_some_and(|task| account_retry_ready(state, &task.account_key(), since, now, true))
}

fn account_retry_ready(state: &alondra_core::domain::DomainState, key: &alondra_core::accounts::AccountKey, since: Millis, now: Millis, require_capacity: bool) -> bool {
    use alondra_core::accounts::{AccountState, Provider};
    if key.provider == Provider::Codex { return crate::claude_access::native_capacity_ready(state, key, since); }
    let Some(account) = state.accounts.iter().find(|account| account.provider == key.provider && account.id == key.account) else {
        return state.accounts.is_empty() && state.account_config_version.is_empty();
    };
    if !matches!(account.state, AccountState::SignedIn | AccountState::Disabled) { return false; }
    !require_capacity || state.account_usage.iter().find(|usage| usage.provider == key.provider && usage.account == key.account)
        .is_none_or(|usage| legacy_account_retry_ready(usage, now))
}

fn legacy_account_retry_ready(usage: &alondra_core::usage::AccountUsage, now: Millis) -> bool {
    use alondra_core::accounts::Provider;
    use alondra_core::usage::{Share, WindowKind};
    usage.provider == Provider::Claude && !usage.windows.iter().any(|window| {
        matches!(window.reading.kind, WindowKind::Session | WindowKind::Weekly)
            && (window.reading.exhausted || window.reading.used == Share::FULL)
            && window.reading.resets_at.map_or_else(|| window.measured_at.saturating_add(UNKNOWN_RESET_WAIT_MS) > now, |reset| reset > now)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_usage_pause_goes_on_after_a_panic_holding_it() {
        let pause = UsagePause::default();
        crate::locks::poison(&pause.record);
        pause.exhaust(Some(10_000), 0);
        assert!(pause.is_active());
    }

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
    fn a_captured_deadline_cannot_resume_an_extended_pause_or_an_account_wait() {
        let pause = UsagePause::default();
        pause.exhaust(Some(2_000), 1_000);
        pause.exhaust(Some(9_000), 1_100);
        assert_eq!(pause.resume_due(2_000), None);
        assert_eq!(pause.current().unwrap().until, 9_000);
        assert_eq!(pause.resume_due(9_000), Some(false));
        pause.wait_for_session(10_000);
        assert_eq!(pause.resume_due(99_000), None);
        assert!(pause.waits_for_session());
    }

    #[test]
    fn a_legacy_account_retries_only_after_its_own_reset_or_unknown_reset_wait() {
        use alondra_core::accounts::{AccountId, Provider};
        use alondra_core::usage::{AccountUsage, UsageWindow, WindowReading};
        let mut usage = AccountUsage { provider: Provider::Claude, account: AccountId::default_account(), windows: vec![UsageWindow {
            reading: WindowReading { kind: WindowKind::Session, used: Share::FULL, resets_at: Some(9_000), exhausted: true }, measured_at: 1_000,
        }], plan: None, day_start: None };
        assert!(!legacy_account_retry_ready(&usage, 2_000), "another account's earlier reset cannot wake this one");
        assert!(legacy_account_retry_ready(&usage, 9_000));
        usage.windows[0].reading.resets_at = None;
        assert!(!legacy_account_retry_ready(&usage, 1_000 + UNKNOWN_RESET_WAIT_MS - 1));
        assert!(legacy_account_retry_ready(&usage, 1_000 + UNKNOWN_RESET_WAIT_MS), "the existing legacy retry remains tied to its own observation");
        usage.provider = Provider::Codex;
        assert!(!legacy_account_retry_ready(&usage, 1_000 + UNKNOWN_RESET_WAIT_MS), "native SDK holds use their qualified recovery path");
    }

    #[test]
    fn an_unbound_legacy_session_is_not_assumed_to_belong_to_a_codex_or_named_account() {
        let (domain, _home) = domain();
        let mut state = (*domain.state().unwrap()).clone();
        let agent = alondra_core::domain::AgentId("unbound".into());
        assert!(legacy_wake_ready(&state, &agent, 1_000, true));
        state.account_usage.push(alondra_core::usage::AccountUsage {
            provider: alondra_core::accounts::Provider::Claude, account: alondra_core::accounts::AccountId::default_account(),
            windows: vec![alondra_core::usage::UsageWindow { measured_at: 500,
                reading: WindowReading { kind: WindowKind::Session, used: Share::FULL, resets_at: Some(9_000), exhausted: true } }],
            plan: None, day_start: None,
        });
        assert!(!legacy_wake_ready(&state, &agent, 1_000, true), "automatic recovery waits for this account");
        assert!(legacy_wake_ready(&state, &agent, 1_000, false), "the existing User resume action may retry its same account");
        state.accounts[0].provider = alondra_core::accounts::Provider::Codex;
        assert!(!legacy_wake_ready(&state, &agent, 1_000, true));
        state.accounts[0].provider = alondra_core::accounts::Provider::Claude;
        state.accounts[0].id = alondra_core::accounts::AccountId::parse("work").unwrap();
        assert!(!legacy_wake_ready(&state, &agent, 1_000, true));
    }

    #[test]
    fn native_codex_capacity_does_not_clear_another_accounts_legacy_usage_wait() {
        use alondra_core::accounts::{AccountKey, Provider};
        use alondra_core::usage::{AccountUsage, UsageWindow};
        let (domain, _home) = domain();
        let mut state = (*domain.state().unwrap()).clone();
        let mut codex = state.accounts[0].clone(); codex.provider = Provider::Codex; state.accounts.push(codex);
        let account = alondra_core::accounts::AccountId::default_account();
        let window = |kind, used| UsageWindow { measured_at: 1_000, reading: WindowReading { kind, used, resets_at: Some(9_000), exhausted: false } };
        state.account_usage = vec![
            AccountUsage { provider: Provider::Claude, account: account.clone(), windows: vec![window(WindowKind::Session, Share::FULL)], plan: None, day_start: None },
            AccountUsage { provider: Provider::Codex, account: account.clone(), windows: vec![
                window(WindowKind::Other("codex:account:primary:300".into()), Share::of(0.2)),
                window(WindowKind::Other("codex:account:secondary:10080".into()), Share::of(0.2)),
            ], plan: None, day_start: None },
        ];
        assert!(account_retry_ready(&state, &AccountKey::new(Provider::Codex, account.clone()), 500, 2_000, true));
        assert!(!account_retry_ready(&state, &AccountKey::new(Provider::Claude, account), 500, 2_000, true));
    }

    #[test]
    fn an_explicit_user_pause_survives_provider_capacity_returning_after_a_session_wait() {
        let pause = UsagePause::default();
        pause.wait_for_session(1_000);
        assert_eq!(pause.pause_by_user(9_000, 2_000), Change::Extended);
        let standing = pause.current().unwrap();
        assert!(standing.by_user);
        assert!(!standing.no_session);
        assert_eq!(standing.until, 9_000);
        assert!(!pause.provider_account_back(3_000), "connecting a provider cannot override the user's pause");
        assert_eq!(pause.current().unwrap(), standing);
    }

    #[test]
    fn an_exhaustion_is_recorded_extended_told_once_and_resumed() {
        let pause = UsagePause::default();
        assert!(!pause.is_active());
        assert_eq!(pause.exhaust(Some(5_000_000), 1_000_000), Change::Started);
        assert_eq!(pause.current(), Some(Exhausted { until: 5_000_000, since: 1_000_000, by_user: false, no_session: false }));
        assert_eq!(pause.exhaust(Some(4_000_000), 1_100_000), Change::Unchanged, "an earlier reset changes nothing");
        assert_eq!(pause.exhaust(Some(6_000_000), 1_200_000), Change::Extended);
        assert_eq!(pause.current(), Some(Exhausted { until: 6_000_000, since: 1_000_000, by_user: false, no_session: false }), "extended, still since the start");
        assert_eq!(pause.claim_announcement(), Some((Exhausted { until: 6_000_000, since: 1_000_000, by_user: false, no_session: false }, false)));
        assert_eq!(pause.claim_announcement(), None, "told once");
        assert_eq!(pause.resume(7_000_000), Some(true));
        assert_eq!(pause.resume(7_000_001), None);
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
    fn a_wait_for_a_session_has_no_time_to_end_it_and_only_a_session_or_a_real_limit_replace_it() {
        let pause = UsagePause::default();
        assert_eq!(pause.exhaust_for_margin(None, 1_000_000), Change::Started);
        assert_eq!(pause.wait_for_session(1_000_000), Change::Extended, "the readings' wait becomes a wait for a session");
        assert_eq!(pause.current(), Some(Exhausted { until: 1_000_000, since: 1_000_000, by_user: false, no_session: true }));
        assert!(!pause.margin_back(1_000_000 + UNKNOWN_RESET_WAIT_MS), "no reset ends it");
        assert!(pause.waits_for_session());
        let (told, _) = pause.claim_announcement().unwrap();
        assert_eq!(out_text(&told, 1_000_000, true), "No hay ninguna cuenta de proveedor disponible: revisa Cuentas");
        assert!(pause.session_back(), "a session ends it");
        assert!(!pause.is_active());
        assert!(!pause.session_back());

        pause.wait_for_session(2_000_000);
        assert_eq!(pause.exhaust(Some(9_000_000), 2_000_000), Change::Extended, "Claude reporting a limit is the quota again");
        assert_eq!(pause.current(), Some(Exhausted { until: 9_000_000, since: 2_000_000, by_user: false, no_session: false }));
        assert_eq!(pause.wait_for_session(2_100_000), Change::Unchanged, "a limit Claude reported stands");
        assert!(!pause.session_back());

        let by_user = UsagePause::default();
        by_user.pause_by_user(9_000_000, 1_000_000);
        assert_eq!(by_user.wait_for_session(1_000_000), Change::Unchanged, "the user's pause stands");
    }

    #[test]
    fn the_notice_names_the_time_in_the_users_language_and_the_day_when_it_is_not_today() {
        let now = at(7, 14, 0);
        let out_text = |until, now, spanish| out_text(&Exhausted { until, since: now, by_user: false, no_session: false }, now, spanish);
        assert_eq!(out_text(at(7, 15, 45), now, false), "No usage capacity until 15:45");
        assert_eq!(out_text(at(7, 15, 45), now, true), "Sin margen hasta 15:45");
        assert_eq!(out_text(at(12, 0, 0), now, false), "No usage capacity until Mon 00:00");
        assert_eq!(out_text(at(12, 0, 0), now, true), "Sin margen hasta lun 00:00");
    }

    use crate::domain_runtime::DomainRuntime;
    use crate::node_push::testing::Node;
    use alondra_core::usage::{Share, UsageReading, WindowKind, WindowReading};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn domain() -> (Arc<DomainRuntime>, crate::test_support::Directory) {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        let state = domain.state().unwrap();
        domain.execute_as_system(alondra_core::domain::CommandEnvelope {
            protocol_version: alondra_core::domain::PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "fixture-claude-account".into(),
            expected_revision: state.revision, command: alondra_core::domain::Command::RecordAccounts {
                accounts: vec![alondra_core::accounts::AccountStatus { provider: alondra_core::accounts::Provider::Claude,
                    id: alondra_core::accounts::AccountId::default_account(), name: "Personal".into(), state: alondra_core::accounts::AccountState::SignedIn,
                    native_identity: None, email: None, plan: None, billing: Default::default(), sign_ins: 0, profile: None }], reserve: alondra_core::usage::default_reserve(),
            },
        }).unwrap();
        (domain, home)
    }

    /// The default account's windows as a node's events report them: `(kind, share used, reset)`.
    fn measure(domain: &DomainRuntime, windows: &[(WindowKind, f64, Millis)]) {
        let reading = UsageReading {
            windows: windows.iter().map(|(kind, used, resets_at)| WindowReading { kind: kind.clone(), used: Share::of(*used), resets_at: Some(*resets_at), exhausted: false }).collect(),
            plan: None, complete: false,
        };
        crate::usage_probe::record(domain, &alondra_core::accounts::AccountId::default_account(), reading, now_millis());
        crate::usage_probe::follow(domain, now_millis());
    }

    fn stop_failure(session: &str, error: &str, details: Option<&str>) -> serde_json::Value {
        let mut payload = json!({"session_id": session, "hook_event_name": "StopFailure", "error_type": error});
        if let Some(details) = details {
            payload["error_details"] = json!(details);
        }
        payload
    }

    #[test]
    fn a_node_stopped_by_the_limit_sets_the_exhaustion_to_the_reset_the_plan_reports() {
        let (domain, home) = domain();
        let now = now_millis();
        domain.live.observe(&stop_failure("s1", "overloaded", None));
        assert!(!domain.live.usage_pause.is_active(), "an overloaded API is not an empty plan");

        measure(&domain, &[(WindowKind::Session, 0.8, now + 2 * 3_600_000)]);
        domain.live.observe(&stop_failure("s1", "rate_limit", None));
        let until = domain.live.usage_pause.current().unwrap().until;
        assert!((until - (now + 2 * 3_600_000)).abs() < 2_000, "the 5-hour window's reset: {until}");

        let (named, named_home) = self::domain();
        let before = now_millis();
        named.live.observe(&stop_failure("s1", "rate_limit", Some("You've hit your session limit · resets 11:59pm")));
        let after = now_millis();
        let until = named.live.usage_pause.current().unwrap().until;
        assert!((before + UNKNOWN_RESET_WAIT_MS..=after + UNKNOWN_RESET_WAIT_MS).contains(&until), "a reset in the error's words is not one: {until}");

        let (unknown, unknown_home) = self::domain();
        let before = now_millis();
        unknown.live.observe(&stop_failure("s1", "rate_limit", None));
        let after = now_millis();
        let until = unknown.live.usage_pause.current().unwrap().until;
        assert!((before + UNKNOWN_RESET_WAIT_MS..=after + UNKNOWN_RESET_WAIT_MS).contains(&until), "no reset known: the default wait");
        for path in [home, named_home, unknown_home] {
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn a_measurement_with_a_window_used_up_is_the_limit_too() {
        let (domain, _home) = domain();
        let now = now_millis();
        measure(&domain, &[(WindowKind::Session, 0.6, now + 3_600_000)]);
        assert!(!domain.live.usage_pause.is_active());
        measure(&domain, &[(WindowKind::Session, 1.0, now + 3_600_000), (WindowKind::Weekly, 0.4, now + 4 * 86_400_000)]);
        let until = domain.live.usage_pause.current().unwrap().until;
        assert_eq!(until, now + 3_600_000);

        let (opus, _opus_home) = self::domain();
        measure(&opus, &[(WindowKind::Other("seven_day_opus".into()), 1.0, now + 86_400_000)]);
        assert!(!opus.live.usage_pause.is_active(), "one model's window alone leaves the others working");
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
        assert!(told[0].0.starts_with("No usage capacity until "), "{told:?}");
        assert_eq!(told[1].0, "Usage capacity is available again");
        assert!(told.iter().all(|(_, passive)| *passive), "a pause and its end ask nothing of the user: {told:?}");
        std::fs::remove_dir_all(home).unwrap();
    }

    fn over_pace_domain() -> (Arc<DomainRuntime>, crate::test_support::Directory) {
        let (domain, home) = domain();
        let reset = now_millis() + 5 * 86_400_000;
        for used in [0.12, 0.53] {
            measure(&domain, &[(WindowKind::Weekly, used, reset)]);
        }
        assert!(crate::usage_probe::usage(&domain, now_millis()).unwrap().over_pace);
        (domain, home)
    }

    fn the_card(domain: &DomainRuntime, prefix: &str) -> Option<alondra_core::domain::Interaction> {
        domain.state().unwrap().interactions.values().find(|card| card.id.0.starts_with(prefix) && !card.withdrawn).cloned()
    }

    fn answer(domain: &DomainRuntime, card: &str, option: &str) {
        let state = domain.state().unwrap();
        domain.execute(&alondra_core::domain::Actor::User { user_id: "device:phone".into() }, &alondra_core::domain::CommandEnvelope {
            protocol_version: alondra_core::domain::PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
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
        assert_eq!(card.id.0, format!("over-pace-{}", alondra_daemon_engine_model::local_day(now_millis())));
        assert_eq!(card.card.kind, alondra_core::domain::CardKind::Decide);
        assert_eq!(card.author, alondra_core::domain::Author::System);
        assert!(!card.blocking);
        assert_eq!(card.card.recommended.as_deref(), Some("keep_going"));
        let alondra_core::domain::InteractionPayload::Question { options, .. } = &card.payload else { panic!("a question") };
        assert_eq!(options.iter().map(|option| option.id.as_str()).collect::<Vec<_>>(), ["pause_until_tomorrow", "keep_going"]);

        let read = domain.read();
        let english = push_content(&read, &card.id.0, Lang::En).unwrap();
        let spanish = push_content(&read, &card.id.0, Lang::Es).unwrap();
        assert_eq!(english.interruption_level, "passive");
        assert_eq!(english.alert.as_ref().unwrap().title, "Today is over its share of the weekly Claude usage");
        assert_eq!(english.alert.unwrap().body.as_deref(), Some("41% used today against a 18% share · 53% of the week used"));
        assert_eq!(spanish.alert.as_ref().unwrap().title, "Hoy vas por encima de tu parte del uso semanal");
        assert_eq!(spanish.alert.unwrap().body.as_deref(), Some("41% usado hoy frente a una parte del 18% · 53% de la semana usado"));
        let labels = |lang| push_content(&read, &card.id.0, lang).unwrap().actions.into_iter().map(|action| action.label).collect::<Vec<_>>();
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
    fn the_answer_to_a_pace_card_pauses_only_on_its_own_day_as_after_a_restart() {
        let (domain, _home) = over_pace_domain();
        raise_over_pace_card(&domain, now_millis());
        answer(&domain, &the_card(&domain, "over-pace-").unwrap().id.0, "pause_until_tomorrow");
        settle_pace_cards(&domain, now_millis() + 86_400_000);
        assert!(!domain.live.usage_pause.is_active(), "yesterday's answer paused yesterday only");
        settle_pace_cards(&domain, now_millis());
        assert!(domain.live.usage_pause.current().is_some_and(|standing| standing.by_user), "today's answer brings the pause back");
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
    async fn no_account_with_a_session_sounds_because_only_the_user_can_fix_it() {
        let (domain, home) = domain();
        let told = Arc::new(Told(Mutex::new(vec![])));
        domain.live.usage_pause.wait_for_session(now_millis());
        let watcher = tokio::spawn(watch(domain.clone(), told.clone(), 0));
        for _ in 0..200 {
            if !told.0.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        watcher.abort();
        assert_eq!(told.0.lock().unwrap().clone(), [("No provider account is available: check Accounts".to_owned(), false)]);
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
