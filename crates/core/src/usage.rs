//! How much of its plan each provider account has used and how much room it has left. A provider
//! reports usage windows (the session's 5 hours, the week, others it names) as a share used, when
//! each resets and the account's plan; Alondra keeps the latest reading of every window per
//! account. From them come each account's margin, the combined margin of all of them weighted by
//! plan, and the day's pace: today's slice of the week is what was left at the day's first
//! reading, split over the days left until the week resets. A window whose reset has passed no
//! longer describes the plan, so it counts as unused.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::accounts::{AccountBilling, AccountId, AccountKey, AccountState, AccountStatus, Policy, Provider};
use crate::model::Millis;

const DAY_MILLIS: f64 = 86_400_000.0;
const DAY_MINUTES: u64 = 24 * 60;
/// Below this share of its week left an account has little room, though it still works.
pub const LOW_WEEK_LEFT: f64 = 0.15;
/// Two readings of the week whose resets are closer than this describe the same week.
const SAME_WEEK_MILLIS: Millis = 3_600_000;
const PLAN_LIMIT: usize = 64;
const WINDOW_NAME_LIMIT: usize = 64;
const WINDOWS_LIMIT: usize = 16;

/// A share of a window from 0 to 1, kept in ten-thousandths so readings compare exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Share(u16);

impl Share {
    pub const NONE: Share = Share(0);
    pub const FULL: Share = Share(10_000);

    pub fn of(fraction: f64) -> Share {
        if fraction.is_nan() {
            return Share::NONE;
        }
        Share((fraction.clamp(0.0, 1.0) * 10_000.0).round() as u16)
    }

    pub fn fraction(self) -> f64 {
        f64::from(self.0) / 10_000.0
    }

    /// Its ten-thousandths, as it is kept.
    pub fn parts(self) -> u16 {
        self.0
    }

    pub fn from_parts(parts: u16) -> Option<Share> {
        (parts <= Share::FULL.0).then_some(Share(parts))
    }
}

/// Kept as its ten-thousandths: domain commands carry whole numbers only.
impl Serialize for Share {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u16(self.0)
    }
}

impl<'de> Deserialize<'de> for Share {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let parts = u16::deserialize(deserializer)?;
        if parts > Share::FULL.0 {
            return Err(serde::de::Error::custom(format!("a share is at most 10000 ten-thousandths, not {parts}")));
        }
        Ok(Share(parts))
    }
}

/// The share of the 5-hour window an account may use before it counts as having no margin.
pub fn default_reserve() -> Share {
    Share::of(crate::accounts::RESERVE)
}

pub fn is_default_reserve(reserve: &Share) -> bool {
    *reserve == default_reserve()
}

/// Which window a reading is about: the provider's short one, its week, or another it names.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WindowKind {
    Session,
    Weekly,
    Other(String),
}

impl WindowKind {
    pub fn is_native_limit_marker(&self) -> bool { matches!(self, Self::Other(name) if name == "codex:account:limit") }
    pub fn duration_minutes(&self) -> Option<u64> {
        match self { Self::Other(name) if name.starts_with("codex:") => name.rsplit(':').next()?.parse().ok(), _ => None }
    }
    pub fn codex_window_label(&self) -> Option<String> {
        let Self::Other(name) = self else { return None };
        if !name.starts_with("codex:") { return None; }
        let parts: Vec<_> = name.split(':').collect();
        let window = parts.get(parts.len().checked_sub(2)?)?;
        let window = match *window { "primary" => "Primary", "secondary" => "Secondary", _ => return None };
        Some(match self.duration_minutes() { Some(minutes) => format!("{window} · {minutes} min"), None => window.into() })
    }
    fn account_wide(&self, provider: Provider) -> bool {
        matches!(self, Self::Session | Self::Weekly) || matches!((provider, self), (Provider::Codex, Self::Other(name)) if name.starts_with("codex:account:"))
    }
    fn same_dimension(&self, other: &Self, provider: Provider) -> bool {
        if self == other { return true; }
        match (provider, self, other) {
            (Provider::Codex, Self::Other(left), Self::Other(right)) if left.starts_with("codex:") && right.starts_with("codex:") => left.rsplit_once(':').map(|(scope, _)| scope) == right.rsplit_once(':').map(|(scope, _)| scope),
            _ => false,
        }
    }
    pub fn named(name: &str) -> WindowKind {
        match name {
            "session" => WindowKind::Session,
            "weekly" => WindowKind::Weekly,
            other => WindowKind::Other(other.to_owned()),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            WindowKind::Session => "session",
            WindowKind::Weekly => "weekly",
            WindowKind::Other(name) => name,
        }
    }
}

impl Serialize for WindowKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for WindowKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(WindowKind::named(&String::deserialize(deserializer)?))
    }
}

/// One window as the provider reported it. `exhausted` when it refused work for this window.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WindowReading {
    pub kind: WindowKind,
    pub used: Share,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<Millis>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exhausted: bool,
}

/// What one observation of an account said: some of its windows and perhaps its plan. A
/// `complete` reading (a probe's) lists every window the account has, so the ones it leaves out
/// no longer apply; a partial one (an event during a turn) changes only the windows it names.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct UsageReading {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<WindowReading>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub complete: bool,
}

impl UsageReading {
    pub fn check(&self) -> Result<(), String> {
        if self.windows.len() > WINDOWS_LIMIT {
            return Err(format!("a reading holds at most {WINDOWS_LIMIT} windows"));
        }
        if let Some(plan) = &self.plan
            && (plan.trim().is_empty() || plan.len() > PLAN_LIMIT || plan.chars().any(char::is_control))
        {
            return Err(format!("a plan is 1 to {PLAN_LIMIT} printable characters"));
        }
        let mut kinds = std::collections::BTreeSet::new();
        for window in &self.windows {
            let name = window.kind.name();
            if name.trim().is_empty() || name.len() > WINDOW_NAME_LIMIT || name.chars().any(char::is_control) {
                return Err(format!("a window's name is 1 to {WINDOW_NAME_LIMIT} printable characters"));
            }
            if !kinds.insert(&window.kind) {
                return Err(format!("the window {name} is read twice"));
            }
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty() && self.plan.is_none() && !self.complete
    }
}

/// A window as Alondra keeps it: the latest reading of it and when it came.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct UsageWindow {
    #[serde(flatten)]
    pub reading: WindowReading,
    pub measured_at: Millis,
}

/// The week's share used at the local day's first reading, which anchors today's slice.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DayStart {
    pub day: String,
    pub weekly: Share,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub week_resets_at: Option<Millis>,
}

/// What Alondra knows of one account's usage.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AccountUsage {
    pub provider: Provider,
    pub account: AccountId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<UsageWindow>,
    /// The plan the provider last reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub day_start: Option<DayStart>,
}

impl AccountUsage {
    pub fn window(&self, kind: &WindowKind) -> Option<&UsageWindow> {
        self.windows.iter().find(|window| window.reading.kind == *kind)
    }

    /// When the latest reading came: how old what Alondra knows is.
    pub fn measured_at(&self) -> Option<Millis> {
        self.windows.iter().map(|window| window.measured_at).max()
    }

    /// The window as it stands at `now`: none once its reset has passed.
    pub fn current(&self, kind: &WindowKind, now: Millis) -> Option<&WindowReading> {
        self.window(kind).map(|window| &window.reading).filter(|reading| reading.resets_at.is_none_or(|reset| reset > now))
    }

    /// The account-wide window of a week (`week`) or of a session, as it stands at `now`: Claude
    /// names them, Codex reports them by how long they last.
    pub fn plan_window(&self, week: bool, now: Millis) -> Option<&WindowReading> {
        let named = if week { WindowKind::Weekly } else { WindowKind::Session };
        self.current(&named, now).or_else(|| {
            self.windows.iter().map(|window| &window.reading).find(|reading| {
                reading.kind.account_wide(self.provider)
                    && reading.kind.duration_minutes().is_some_and(|minutes| (minutes >= DAY_MINUTES) == week)
                    && reading.resets_at.is_none_or(|reset| reset > now)
            })
        })
    }

    /// When the account can work again, when a window it ran out of says so.
    pub fn exhausted_until(&self, now: Millis) -> Option<Millis> {
        self.ran_out(now, |_| true)
    }

    /// When the account's session or week, which hold back every model, comes back after running
    /// out. A window of one model alone leaves the others working.
    pub fn plan_exhausted_until(&self, now: Millis) -> Option<Millis> {
        self.ran_out(now, |kind| kind.account_wide(self.provider))
    }

    fn ran_out(&self, now: Millis, counts: impl Fn(&WindowKind) -> bool) -> Option<Millis> {
        self.windows
            .iter()
            .map(|window| &window.reading)
            .filter(|reading| counts(&reading.kind) && (reading.exhausted || reading.used == Share::FULL))
            .filter_map(|reading| reading.resets_at.filter(|reset| *reset > now))
            .max()
    }

    /// When a limit reported without a time ends: the window that ran out, else the session's,
    /// else the soonest reset the plan has.
    pub fn reset_after_limit(&self, now: Millis) -> Option<Millis> {
        self.exhausted_until(now)
            .or_else(|| self.current(&WindowKind::Session, now).and_then(|reading| reading.resets_at))
            .or_else(|| self.windows.iter().filter_map(|window| window.reading.resets_at.filter(|reset| *reset > now)).min())
    }

    /// Whether the session or the week ran out and has not reset.
    fn plan_exhausted(&self, now: Millis) -> bool {
        self.plan_ran_out(now).next().is_some()
    }

    fn plan_ran_out(&self, now: Millis) -> impl Iterator<Item = &WindowReading> {
        self.windows.iter().map(|window| &window.reading).filter(move |reading| {
            reading.kind.account_wide(self.provider)
                && (reading.exhausted || reading.used == Share::FULL)
                && reading.resets_at.is_none_or(|reset| reset > now)
        })
    }

    /// When the session and the week it ran out of have reset: none when one has no known reset.
    fn can_work_at(&self, now: Millis) -> Option<Millis> {
        let mut at = now;
        for reading in self.plan_ran_out(now) {
            at = at.max(reading.resets_at?);
        }
        Some(at)
    }

    fn exhausted(&self, now: Millis) -> bool {
        self.windows.iter().map(|window| &window.reading).any(|reading| {
            (reading.exhausted || reading.used == Share::FULL) && reading.resets_at.is_none_or(|reset| reset > now)
        })
    }
}

/// `usage` after `reading` came for `account` at `at`, on the local `day`. The week's share at the
/// day's first reading of the week anchors today's slice; a new day or a new week moves it.
pub fn record(usage: Option<&AccountUsage>, provider: Provider, account: &AccountId, reading: &UsageReading, at: Millis, day: &str) -> AccountUsage {
    let mut next = usage.cloned().unwrap_or_else(|| AccountUsage { provider, account: account.clone(), windows: vec![], plan: None, day_start: None });
    if provider == Provider::Codex {
        let primary = reading.windows.iter().find(|window| matches!(&window.kind, WindowKind::Other(name) if name.starts_with("codex:account:primary:")));
        let secondary = reading.windows.iter().find(|window| matches!(&window.kind, WindowKind::Other(name) if name.starts_with("codex:account:secondary:")));
        let positive = |window: &WindowReading| !window.exhausted && window.used < Share::FULL && window.resets_at.is_none_or(|reset| reset > at);
        if primary.is_some_and(positive) && secondary.is_some_and(positive) {
            next.windows.retain(|window| !window.reading.kind.is_native_limit_marker() || window.measured_at >= at);
        }
    }
    if reading.complete {
        next.windows.retain(|window| window.reading.kind.is_native_limit_marker() || reading.windows.iter().any(|read| read.kind.same_dimension(&window.reading.kind, provider)));
    }
    for read in &reading.windows {
        let kept = UsageWindow { reading: read.clone(), measured_at: at };
        match next.windows.iter_mut().find(|window| window.reading.kind.same_dimension(&read.kind, provider)) {
            Some(window) => *window = kept,
            None => next.windows.push(kept),
        }
    }
    next.windows.sort_by(|a, b| a.reading.kind.cmp(&b.reading.kind));
    if let Some(plan) = &reading.plan {
        next.plan = Some(plan.trim().to_owned());
    }
    if let Some(week) = next.current(&WindowKind::Weekly, at).cloned() {
        let same_week = |start: &DayStart| match (start.week_resets_at, week.resets_at) {
            (Some(before), Some(now)) => (before - now).abs() < SAME_WEEK_MILLIS,
            _ => true,
        };
        if !next.day_start.as_ref().is_some_and(|start| start.day == day && same_week(start)) {
            next.day_start = Some(DayStart { day: day.to_owned(), weekly: week.used, week_resets_at: week.resets_at });
        }
    }
    next
}

/// Whether `day` names a local day as `YYYY-MM-DD`.
pub fn is_day(day: &str) -> bool {
    let bytes = day.as_bytes();
    bytes.len() == 10 && bytes.iter().enumerate().all(|(i, b)| if i == 4 || i == 7 { *b == b'-' } else { b.is_ascii_digit() })
}

/// How many Pro weeks a plan is worth: Pro 1, Max 5x 5, Max 20x 10; Max without its size, Team
/// and any plan Alondra does not know count as 5.
pub fn plan_weight(plan: Option<&str>) -> u32 {
    let plan: String = plan.unwrap_or_default().to_lowercase().chars().filter(char::is_ascii_alphanumeric).collect();
    match plan.as_str() {
        "pro" | "claudepro" => 1,
        _ if plan.contains("20x") => 10,
        _ => 5,
    }
}

/// What an account counts for in the combined margin: the weight the user set, else its plan's
/// (the one the user set, else the one the provider reported). An account paid per use has none:
/// it never counts.
pub fn weight(billing: &AccountBilling, reported_plan: Option<&str>) -> Option<u32> {
    match billing {
        AccountBilling::PayPerUse => None,
        AccountBilling::Subscription { weight: Some(weight), .. } => Some(*weight),
        AccountBilling::Subscription { plan, weight: None } => Some(plan_weight(plan.as_deref().or(reported_plan))),
    }
}

/// How the account's week goes today: its share used, the slice today allows and what today took.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pace {
    pub week_used: f64,
    pub week_resets_at: Millis,
    pub today_spent: Option<f64>,
    pub today_share: Option<f64>,
    pub per_day_left: f64,
}

impl Pace {
    pub fn over(&self) -> bool {
        self.today_spent.zip(self.today_share).is_some_and(|(spent, share)| spent >= share)
    }

    /// What is left of today's slice, from 0 to 1; all of it before the day's first reading.
    pub fn today_left(&self) -> f64 {
        match self.today_spent.zip(self.today_share) {
            Some((spent, share)) => (1.0 - spent / share).clamp(0.0, 1.0),
            None => 1.0,
        }
    }
}

/// The account's daily pace on the local `today`, when it has a week that has not reset.
pub fn pace(usage: &AccountUsage, now: Millis, today: &str) -> Option<Pace> {
    pace_in(usage, usage.current(&WindowKind::Weekly, now)?, now, today)
}

fn pace_in(usage: &AccountUsage, week: &WindowReading, now: Millis, today: &str) -> Option<Pace> {
    let resets_at = week.resets_at?;
    let days_left = ((resets_at - now).max(0) as f64 / DAY_MILLIS).ceil().max(1.0);
    let used = week.used.fraction();
    let start = usage
        .day_start
        .as_ref()
        .filter(|start| start.day == today && start.week_resets_at.is_none_or(|reset| (reset - resets_at).abs() < SAME_WEEK_MILLIS))
        .map(|start| start.weekly.fraction());
    Some(Pace {
        week_used: used,
        week_resets_at: resets_at,
        today_spent: start.map(|start| (used - start).max(0.0)),
        today_share: start.map(|start| ((1.0 - start) / days_left).max(0.001)),
        per_day_left: (1.0 - used).max(0.0) / days_left,
    })
}

/// The account's own margin, from 0 to 1: none when its sign-in expired, it ran out or its
/// session window went past `reserve`; otherwise the least of what is left of its week and of
/// today's slice. Unknown for a disabled account or one never measured.
pub fn own_margin(state: AccountState, usage: Option<&AccountUsage>, reserve: Share, now: Millis, today: &str) -> Option<f64> {
    match state {
        AccountState::Disabled => return None,
        AccountState::SignedOut => return Some(0.0),
        AccountState::Unchecked | AccountState::SignedIn | AccountState::Unavailable => {}
    }
    let usage = usage?;
    if usage.exhausted(now) || usage.current(&WindowKind::Session, now).is_some_and(|session| session.used > reserve) {
        return Some(0.0);
    }
    let week_left = usage.current(&WindowKind::Weekly, now).map_or(1.0, |week| 1.0 - week.used.fraction());
    let today_left = pace(usage, now, today).map_or(1.0, |pace| pace.today_left());
    Some(week_left.min(today_left).clamp(0.0, 1.0))
}

/// One account's part in the combined margin.
#[derive(Clone, Debug, PartialEq)]
pub struct AccountMargin {
    pub provider: Provider,
    pub account: AccountId,
    pub weight: u32,
    pub own: f64,
    /// `weight × own`: how many Pro weeks of room the account has.
    pub absolute: f64,
    pub pace: Option<Pace>,
    pub measured_at: Option<Millis>,
}

/// Every account that counts, in the user's order, and their margin together: the sum of their
/// absolute margins over the sum of their weights. Accounts paid per use, disabled ones and the
/// ones never measured do not count; with none that counts there is no combined margin.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Margins {
    pub accounts: Vec<AccountMargin>,
    pub combined: Option<f64>,
}

/// Until the daemon records the accounts, the provider's own sign-in is the one there is.
fn or_implicit(accounts: &[AccountStatus]) -> std::borrow::Cow<'_, [AccountStatus]> {
    if !accounts.is_empty() {
        return std::borrow::Cow::Borrowed(accounts);
    }
    let account = crate::accounts::ProviderAccount::implicit_default();
    std::borrow::Cow::Owned(vec![AccountStatus {
        provider: Provider::Claude, id: account.id, name: account.name, state: AccountState::Unchecked, native_identity: None, email: None, plan: None, billing: account.billing, sign_ins: 0,
    }])
}

pub fn margins(accounts: &[AccountStatus], usage: &[AccountUsage], reserve: Share, now: Millis, today: &str) -> Margins {
    let accounts = or_implicit(accounts);
    let counted: Vec<AccountMargin> = accounts
        .iter()
        .filter(|account| account.provider == Provider::Claude)
        .filter_map(|account| {
            let measured = usage.iter().find(|u| u.provider == account.provider && u.account == account.id);
            let weight = weight(&account.billing, measured.and_then(|u| u.plan.as_deref()).or(account.plan.as_deref()))?;
            let own = own_margin(account.state, measured, reserve, now, today)?;
            Some(AccountMargin {
                provider: account.provider,
                account: account.id.clone(),
                weight,
                own,
                absolute: f64::from(weight) * own,
                pace: measured.and_then(|u| pace(u, now, today)),
                measured_at: measured.and_then(AccountUsage::measured_at),
            })
        })
        .collect();
    let weights: f64 = counted.iter().map(|m| f64::from(m.weight)).sum();
    let combined = (weights > 0.0).then(|| counted.iter().map(|m| m.absolute).sum::<f64>() / weights);
    Margins { accounts: counted, combined }
}

/// Whether the account can take work at `now`: on, paid by subscription, signed in as far as
/// Alondra knows and with neither its session nor its week run out. A window of one model alone,
/// its reserve and today's slice only rank it: they never hold work back. An account never measured can, until a reading says otherwise.
fn can_work(account: &AccountStatus, usage: Option<&AccountUsage>, now: Millis) -> bool {
    if matches!(account.state, AccountState::Disabled | AccountState::SignedOut) || matches!(account.billing, AccountBilling::PayPerUse) {
        return false;
    }
    usage.is_none_or(|usage| !usage.plan_exhausted(now))
}

pub fn eligible_account_keys(order: &[AccountKey], accounts: &[AccountStatus], usage: &[AccountUsage], now: Millis) -> Vec<AccountKey> {
    let keys = if order.is_empty() {
        accounts.iter().map(|account| AccountKey::new(account.provider, account.id.clone())).collect()
    } else { order.to_vec() };
    let mut eligible = vec![];
    for key in keys {
        if eligible.contains(&key) { continue; }
        let Some(status) = accounts.iter().find(|status| status.provider == key.provider && status.id == key.account) else { continue };
        let measured = usage.iter().find(|reading| reading.provider == key.provider && reading.account == key.account);
        if status.state == AccountState::SignedIn && can_work(status, measured, now) { eligible.push(key); }
    }
    eligible
}

/// The account a new launch of `provider` runs on, as `policy` spreads the work over the
/// accounts in the user's order. By margin: the one with the most absolute margin, the first of
/// them when several have as much. In a chain: the first with margin. Today's slice only ranks
/// them: when every account that can work has no margin left, the one with the most of its week left (by
/// margin) or the first (in a chain) is chosen. A session window past the reserve leaves the
/// account no margin, and so ranks it last, but it is still chosen when no other can work. An account paid per use is never chosen here.
/// None when no account can work: the launch waits until one can.
pub fn choose_account(policy: Policy, provider: Provider, accounts: &[AccountStatus], usage: &[AccountUsage], reserve: Share, now: Millis, today: &str) -> Option<AccountId> {
    let accounts = or_implicit(accounts);
    let candidates: Vec<(&AccountStatus, f64, f64)> = accounts
        .iter()
        .filter(|account| account.provider == provider)
        .filter_map(|account| {
            let measured = usage.iter().find(|u| u.provider == account.provider && u.account == account.id);
            if !can_work(account, measured, now) {
                return None;
            }
            let weight = f64::from(weight(&account.billing, measured.and_then(|u| u.plan.as_deref()).or(account.plan.as_deref()))?);
            let own = own_margin(account.state, measured, reserve, now, today).unwrap_or(1.0);
            let week_left = measured.and_then(|u| u.current(&WindowKind::Weekly, now)).map_or(1.0, |week| 1.0 - week.used.fraction());
            Some((account, weight * own, weight * week_left))
        })
        .collect();
    let chosen = match policy {
        Policy::Margin => {
            let rank = |(_, absolute, week): &(&AccountStatus, f64, f64)| (*absolute, if *absolute > 0.0 { 0.0 } else { *week });
            candidates.iter().fold(None, |best: Option<&(&AccountStatus, f64, f64)>, candidate| match best {
                Some(best) if rank(candidate) <= rank(best) => Some(best),
                _ => Some(candidate),
            })
        }
        Policy::Chain => candidates.iter().find(|(_, absolute, _)| *absolute > 0.0).or(candidates.first()),
    };
    chosen.map(|(account, _, _)| account.id.clone())
}

/// When some account of `provider` can work again by itself, while none can: the soonest moment
/// the session and week one of them ran out of have reset. None when an account can work now, or none will by waiting (they
/// are signed out, off or paid per use).
pub fn margin_returns_at(provider: Provider, accounts: &[AccountStatus], usage: &[AccountUsage], now: Millis) -> Option<Millis> {
    let accounts = or_implicit(accounts);
    let mut soonest: Option<Millis> = None;
    for account in accounts.iter().filter(|account| account.provider == provider) {
        let measured = usage.iter().find(|u| u.provider == account.provider && u.account == account.id);
        if can_work(account, measured, now) {
            return None;
        }
        if matches!(account.state, AccountState::Disabled | AccountState::SignedOut) || matches!(account.billing, AccountBilling::PayPerUse) {
            continue;
        }
        if let Some(back) = measured.and_then(|measured| measured.can_work_at(now)) {
            soonest = Some(soonest.map_or(back, |soonest| soonest.min(back)));
        }
    }
    soonest
}

/// The accounts' usage together, weighted by plan, as the usage gauges and the pace card read it:
/// the week, today's slice and the session window, in percent.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margin: Option<f64>,
    /// Every account of the provider in the user's order, as the margin's detail lists them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<AccountLine>,
}

impl Usage {
    /// No account can take work: each is at its limit, signed out, off or paid per use. Only then
    /// do launches wait; a margin of 0 from the reserve or today's slice alone holds nothing back.
    pub fn none_can_work(&self) -> bool {
        !self.accounts.is_empty() && !self.accounts.iter().any(|line| line.stand == AccountStand::CanWork)
    }
}

/// Where one account stands for new work.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccountStand {
    CanWork,
    /// Its session or its week ran out; `until` is when both have reset, when known.
    AtLimit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<Millis>,
    },
    SignedOut,
    Disabled,
    /// Paid per use: only a request that chooses it runs on it.
    PayPerUse,
}

/// How much room an account, a provider or all of them have, from best to worst.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum UsageState {
    WithRoom,
    /// Today's share of the week is spent: going on takes from the days left.
    OverPace,
    /// Little of the week is left, or the 5-hour window is nearly full.
    Low,
    /// It cannot take work: at its limit or signed out.
    Out,
}

/// Where a subscription account stands; none for one that is off or paid per use, which limits nothing.
pub fn usage_state(line: &AccountLine, reserve: Share) -> Option<UsageState> {
    match line.stand {
        AccountStand::Disabled | AccountStand::PayPerUse => None,
        AccountStand::AtLimit { .. } | AccountStand::SignedOut => Some(UsageState::Out),
        AccountStand::CanWork => Some(
            if line.week_percent.is_some_and(|week| week > 100.0 * (1.0 - LOW_WEEK_LEFT))
                || line.five_hour_percent.is_some_and(|five| five > 100.0 * reserve.fraction())
            {
                UsageState::Low
            } else if line.today_spent.zip(line.today_share).is_some_and(|(spent, share)| spent >= share) {
                UsageState::OverPace
            } else {
                UsageState::WithRoom
            },
        ),
    }
}

/// One provider's accounts, in the user's order.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ProviderUsage {
    pub provider: Provider,
    pub accounts: Vec<AccountLine>,
}

impl ProviderUsage {
    /// As its best account: Alondra starts new work on the one with the most room.
    pub fn state(&self) -> Option<UsageState> {
        self.accounts.iter().filter_map(|line| line.state).min()
    }

    /// The soonest an account at its limit comes back.
    pub fn back_at(&self) -> Option<Millis> {
        self.accounts.iter().filter_map(|line| match line.stand { AccountStand::AtLimit { until } => until, _ => None }).min()
    }
}

/// Every provider with accounts, each with its accounts' lines. An account Alondra has not checked
/// yet stands nowhere: it may have no sign-in at all.
pub fn providers(accounts: &[AccountStatus], usage: &[AccountUsage], reserve: Share, now: Millis, today: &str) -> Vec<ProviderUsage> {
    Provider::ALL
        .into_iter()
        .map(|provider| ProviderUsage { provider, accounts: account_lines(provider, accounts, usage, reserve, now, today) })
        .filter(|provider| !provider.accounts.is_empty())
        .collect()
}

/// The provider in the worst state, with that state: a task for one provider cannot run on another.
pub fn worst(providers: &[ProviderUsage]) -> Option<(&ProviderUsage, UsageState)> {
    providers.iter().filter_map(|provider| Some((provider, provider.state()?))).max_by_key(|(_, state)| *state)
}

/// One account in the margin's detail: its own margin and the windows it comes from.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AccountLine {
    pub name: String,
    pub stand: AccountStand,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margin: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub week_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hour_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub today_spent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub today_share: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub week_resets_at: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hour_resets_at: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_at: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<UsageState>,
}

/// Every account of `provider`, in the user's order, with where it stands and its own margin.
pub fn account_lines(provider: Provider, accounts: &[AccountStatus], usage: &[AccountUsage], reserve: Share, now: Millis, today: &str) -> Vec<AccountLine> {
    let accounts = or_implicit(accounts);
    accounts
        .iter()
        .filter(|account| account.provider == provider)
        .map(|account| {
            let measured = usage.iter().find(|u| u.provider == account.provider && u.account == account.id);
            let stand = match (&account.billing, account.state) {
                (_, AccountState::Disabled) => AccountStand::Disabled,
                (AccountBilling::PayPerUse, _) => AccountStand::PayPerUse,
                (_, AccountState::SignedOut) => AccountStand::SignedOut,
                _ if measured.is_some_and(|measured| measured.plan_exhausted(now)) => {
                    AccountStand::AtLimit { until: measured.and_then(|measured| measured.can_work_at(now)) }
                }
                _ => AccountStand::CanWork,
            };
            let window = |week: bool| measured.and_then(|u| u.plan_window(week, now));
            let pace = measured.zip(window(true)).and_then(|(u, week)| pace_in(u, week, now, today));
            let mut line = AccountLine {
                name: account.name.clone(),
                margin: match stand {
                    AccountStand::Disabled | AccountStand::PayPerUse => None,
                    _ => own_margin(account.state, measured, reserve, now, today),
                },
                week_percent: window(true).map(|reading| 100.0 * reading.used.fraction()),
                five_hour_percent: window(false).map(|reading| 100.0 * reading.used.fraction()),
                today_spent: pace.and_then(|pace| pace.today_spent).map(|spent| 100.0 * spent),
                today_share: pace.and_then(|pace| pace.today_share).map(|share| 100.0 * share),
                week_resets_at: window(true).and_then(|reading| reading.resets_at),
                five_hour_resets_at: window(false).and_then(|reading| reading.resets_at),
                measured_at: measured.and_then(AccountUsage::measured_at),
                stand,
                state: None,
            };
            line.state = if account.state == AccountState::Unchecked { None } else { usage_state(&line, reserve) };
            line
        })
        .collect()
}

pub fn summary(accounts: &[AccountStatus], usage: &[AccountUsage], reserve: Share, now: Millis, today: &str) -> Option<Usage> {
    let margins = margins(accounts, usage, reserve, now, today);
    let paced: Vec<(f64, Pace)> = margins.accounts.iter().filter_map(|m| Some((f64::from(m.weight), m.pace?))).collect();
    let weights: f64 = paced.iter().map(|(weight, _)| weight).sum();
    if weights <= 0.0 {
        return None;
    }
    let weighted = |value: &dyn Fn(&Pace) -> f64| paced.iter().map(|(weight, pace)| weight * value(pace)).sum::<f64>() / weights;
    let every_day_known = paced.iter().all(|(_, pace)| pace.today_spent.is_some());
    let today_spent = every_day_known.then(|| 100.0 * weighted(&|pace| pace.today_spent.unwrap_or_default()));
    let today_share = every_day_known.then(|| 100.0 * weighted(&|pace| pace.today_share.unwrap_or_default()));
    let sessions: Vec<(f64, &WindowReading)> = margins
        .accounts
        .iter()
        .filter_map(|m| {
            let measured = usage.iter().find(|u| u.provider == m.provider && u.account == m.account)?;
            Some((f64::from(m.weight), measured.current(&WindowKind::Session, now)?))
        })
        .collect();
    let session_weights: f64 = sessions.iter().map(|(weight, _)| weight).sum();
    Some(Usage {
        week_percent: 100.0 * weighted(&|pace| pace.week_used),
        week_resets_at: paced.iter().map(|(_, pace)| pace.week_resets_at).min().unwrap_or_default(),
        today_spent,
        today_share,
        per_day_left: 100.0 * weighted(&|pace| pace.per_day_left),
        five_hour_percent: (session_weights > 0.0)
            .then(|| 100.0 * sessions.iter().map(|(weight, session)| weight * session.used.fraction()).sum::<f64>() / session_weights),
        five_hour_resets_at: sessions.iter().filter_map(|(_, session)| session.resets_at).min(),
        measured_at: margins.accounts.iter().filter_map(|m| m.measured_at).max().unwrap_or_default(),
        over_pace: today_spent.zip(today_share).is_some_and(|(spent, share)| spent >= share),
        margin: margins.combined,
        accounts: account_lines(Provider::Claude, accounts, usage, reserve, now, today),
    })
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
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod margin_tests;
