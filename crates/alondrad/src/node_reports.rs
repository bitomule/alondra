//! What each session's node mod reports (`plugin/hooks/register.ts`): that it loaded, its turns,
//! the tool it runs, how full its context is, what it has cost, and the plan's limits. Kept in
//! memory beside the hooks' live view; the domain never records it. The open connection the mod
//! holds (`alondra node connect`) is the node's liveness: a newer connection from the same
//! session replaces the older one, and only the current one closing makes the node unreachable.
use alondra_core::usage::{RateLimit, Usage};
use alondra_core::{Millis, ToolInFlight};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// How long a launch waits for the mod's `session.start` before it counts the mods as not loaded.
pub(crate) const MODS_DEADLINE: Duration = Duration::from_secs(30);

pub(crate) const MODS_NOT_LOADED: &str =
    "Claude Code mods did not load in the session: its folder is not trusted, mods are disabled, or Claude Code is older than 2.1.287";

#[derive(Deserialize, Debug, PartialEq)]
#[serde(tag = "event")]
pub(crate) enum Report {
    #[serde(rename = "session.start")]
    SessionStart { version: String, #[serde(default)] cwd: Option<String> },
    #[serde(rename = "session.end")]
    SessionEnd,
    #[serde(rename = "session.measure")]
    Measure {
        #[serde(default)]
        context: Option<ContextIn>,
        #[serde(default)]
        rate_limits: Vec<RateLimitIn>,
        #[serde(default)]
        cost_usd: Option<f64>,
    },
    #[serde(rename = "turn.start")]
    TurnStart,
    #[serde(rename = "turn.complete")]
    TurnComplete { answer: String, #[serde(default)] aborted: bool },
    #[serde(rename = "tool.start")]
    ToolStart { tool: String, #[serde(default)] tool_use_id: Option<String>, #[serde(default)] input: Value },
    #[serde(rename = "tool.end")]
    ToolEnd { #[serde(default)] tool_use_id: Option<String> },
}

#[derive(Deserialize, Debug, PartialEq)]
pub(crate) struct ContextIn {
    #[serde(default)]
    tokens: Option<u64>,
    window: u64,
    #[serde(default)]
    percent: Option<f64>,
}

#[derive(Deserialize, Debug, PartialEq)]
pub(crate) struct RateLimitIn {
    kind: String,
    percent_used: f64,
    #[serde(default)]
    resets_at: Option<String>,
}

/// One report as `alondra node event` hands it over.
#[derive(Deserialize, Debug)]
pub(crate) struct Envelope {
    pub session_id: String,
    #[serde(flatten)]
    pub report: Report,
}

/// What `alondra node connect` says first: the same facts as the mod's `session.start`, so a
/// daemon that restarted while the session ran learns them again from the reconnection.
#[derive(Deserialize, Debug)]
pub(crate) struct Hello {
    pub session_id: String,
    pub version: String,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ContextReading {
    pub percent: Option<f64>,
    pub tokens: Option<u64>,
    pub window: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Answer {
    pub text: String,
    pub at: Millis,
    pub aborted: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Reported {
    pub version: Option<String>,
    pub cwd: Option<String>,
    pub started_at: Option<Millis>,
    /// The current connection, by the number it was given; none while the session is unreachable.
    pub connection: Option<u64>,
    pub ended: bool,
    pub working_since: Option<Millis>,
    pub tool: Option<ToolInFlight>,
    pub answer: Option<Answer>,
    pub context: Option<ContextReading>,
    pub cost_usd: Option<f64>,
}

impl Reported {
    /// The mod loaded and its session has not ended, yet its connection is closed.
    pub(crate) fn unreachable(&self) -> bool {
        self.started_at.is_some() && !self.ended && self.connection.is_none()
    }
}

#[derive(Default)]
struct Limits {
    windows: Vec<RateLimit>,
    measured_at: Millis,
    /// The local day, the week's reset and the week's percent at that day's first measurement.
    day_start: Option<(String, Millis, f64)>,
}

#[derive(Default)]
pub(crate) struct Reports {
    sessions: Mutex<HashMap<String, Reported>>,
    limits: Mutex<Limits>,
    connections: std::sync::atomic::AtomicU64,
    started: tokio::sync::Notify,
}

impl Reports {
    pub(crate) fn apply(&self, envelope: Envelope, now: Millis) {
        self.apply_on(envelope, now, &local_day(now));
    }

    fn apply_on(&self, Envelope { session_id, report }: Envelope, now: Millis, today: &str) {
        if let Report::Measure { rate_limits, .. } = &report
            && !rate_limits.is_empty()
        {
            self.measured(rate_limits, now, today);
        }
        let mut sessions = self.sessions.lock().expect("node reports poisoned");
        let session = sessions.entry(session_id).or_default();
        match report {
            Report::SessionStart { version, cwd } => {
                session.version = Some(version);
                session.cwd = cwd;
                session.started_at = Some(now);
                session.ended = false;
                drop(sessions);
                self.started.notify_waiters();
            }
            Report::SessionEnd => {
                session.ended = true;
                session.connection = None;
                session.tool = None;
                session.working_since = None;
            }
            Report::Measure { context, cost_usd, .. } => {
                if let Some(context) = context {
                    session.context = Some(ContextReading { percent: context.percent, tokens: context.tokens, window: context.window });
                }
                session.cost_usd = cost_usd.or(session.cost_usd);
            }
            Report::TurnStart => session.working_since = Some(now),
            Report::TurnComplete { answer, aborted } => {
                session.working_since = None;
                session.tool = None;
                session.answer = Some(Answer { text: answer, at: now, aborted });
            }
            Report::ToolStart { tool, tool_use_id, input } => {
                let summary = crate::http::tool_summary(Some(&tool), Some(&input));
                session.tool = Some(ToolInFlight { tool_use_id, name: tool, summary, since: now });
            }
            Report::ToolEnd { tool_use_id } => {
                if session.tool.as_ref().is_some_and(|tool| tool.tool_use_id == tool_use_id) {
                    session.tool = None;
                }
            }
        }
    }

    fn measured(&self, rate_limits: &[RateLimitIn], now: Millis, today: &str) {
        let windows: Vec<RateLimit> = rate_limits.iter().map(|limit| RateLimit {
            kind: limit.kind.clone(),
            percent_used: limit.percent_used,
            resets_at: limit.resets_at.as_deref().and_then(crate::watcher::parse_rfc3339_millis),
        }).collect();
        let mut limits = self.limits.lock().expect("plan limits poisoned");
        if let Some(week) = windows.iter().find(|limit| limit.kind == alondra_core::usage::SEVEN_DAY) {
            let reset = week.resets_at.unwrap_or_default();
            let same_day = limits.day_start.as_ref().is_some_and(|(day, week_reset, _)| day == today && *week_reset == reset);
            if !same_day {
                limits.day_start = Some((today.to_owned(), reset, week.percent_used));
            }
        }
        limits.windows = windows;
        limits.measured_at = now;
    }

    pub(crate) fn connect(&self, hello: Hello, now: Millis) -> u64 {
        let number = self.connections.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let mut sessions = self.sessions.lock().expect("node reports poisoned");
        let session = sessions.entry(hello.session_id).or_default();
        session.connection = Some(number);
        session.ended = false;
        session.version = Some(hello.version);
        session.cwd = hello.cwd.or(session.cwd.take());
        session.started_at.get_or_insert(now);
        drop(sessions);
        self.started.notify_waiters();
        number
    }

    /// Only the connection the session holds now can make it unreachable: one it replaced
    /// closing later changes nothing.
    pub(crate) fn disconnect(&self, session_id: &str, number: u64) {
        if let Some(session) = self.sessions.lock().expect("node reports poisoned").get_mut(session_id)
            && session.connection == Some(number)
        {
            session.connection = None;
        }
    }

    /// What a session whose mod loaded reports first, for tests that launch through a fake Claude.
    #[cfg(test)]
    pub(crate) fn loaded_in(&self, session_id: &str) {
        let report = Report::SessionStart { version: "2.1.287".into(), cwd: None };
        self.apply(Envelope { session_id: session_id.into(), report }, alondra_core::now_millis());
    }

    pub(crate) fn session(&self, session_id: &str) -> Option<Reported> {
        self.sessions.lock().expect("node reports poisoned").get(session_id).cloned()
    }

    fn loaded(&self, session_id: &str) -> bool {
        self.sessions.lock().expect("node reports poisoned").get(session_id).is_some_and(|session| session.started_at.is_some())
    }

    /// Whether the session's mod said it loaded within `deadline`.
    pub(crate) async fn wait_loaded(&self, session_id: &str, deadline: Duration) -> bool {
        tokio::time::timeout(deadline, async {
            loop {
                let started = self.started.notified();
                tokio::pin!(started);
                started.as_mut().enable();
                if self.loaded(session_id) {
                    return;
                }
                started.await;
            }
        })
        .await
        .is_ok()
    }

    pub(crate) fn usage(&self, now: Millis) -> Option<Usage> {
        let limits = self.limits.lock().expect("plan limits poisoned");
        let start = limits.day_start.as_ref().filter(|(day, _, _)| *day == local_day(now)).map(|(_, _, start)| *start);
        alondra_core::usage::compute(&limits.windows, start, limits.measured_at, now)
    }
}

fn local_day(millis: Millis) -> String {
    let t = millis.div_euclid(1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report(value: Value) -> Envelope {
        serde_json::from_value(value).unwrap()
    }

    fn hello(session: &str) -> Hello {
        serde_json::from_value(json!({"session_id": session, "version": "2.1.287", "cwd": "/work"})).unwrap()
    }

    #[test]
    fn a_newer_connection_replaces_the_older_one_and_only_the_current_one_closing_counts() {
        let reports = Reports::default();
        let first = reports.connect(hello("s"), 1);
        let second = reports.connect(hello("s"), 2);
        assert_ne!(first, second);
        reports.disconnect("s", first);
        assert_eq!(reports.session("s").unwrap().connection, Some(second), "the replaced connection closing late changes nothing");
        assert!(!reports.session("s").unwrap().unreachable());
        reports.disconnect("s", second);
        assert!(reports.session("s").unwrap().unreachable(), "the current connection closed: the node is unreachable");
        reports.connect(hello("s"), 3);
        assert!(!reports.session("s").unwrap().unreachable());
        reports.apply(report(json!({"session_id": "s", "event": "session.end"})), 4);
        assert!(!reports.session("s").unwrap().unreachable(), "an ended session is not unreachable, it is gone");
    }

    #[test]
    fn turns_and_tools_become_what_the_node_does_and_said_last() {
        let reports = Reports::default();
        reports.apply(report(json!({"session_id": "s", "event": "session.start", "version": "2.1.287", "cwd": "/w"})), 1);
        reports.apply(report(json!({"session_id": "s", "event": "turn.start", "turn_id": "t"})), 2);
        reports.apply(report(json!({"session_id": "s", "event": "tool.start", "tool": "Bash", "tool_use_id": "u1",
            "input": {"command": "cargo test"}})), 3);
        let session = reports.session("s").unwrap();
        assert_eq!((session.version.as_deref(), session.working_since), (Some("2.1.287"), Some(2)));
        let tool = session.tool.unwrap();
        assert_eq!((tool.name.as_str(), tool.summary.as_deref(), tool.since), ("Bash", Some("cargo test"), 3));
        reports.apply(report(json!({"session_id": "s", "event": "tool.end", "tool_use_id": "other"})), 4);
        assert!(reports.session("s").unwrap().tool.is_some(), "another call ending leaves this one running");
        reports.apply(report(json!({"session_id": "s", "event": "tool.end", "tool_use_id": "u1"})), 5);
        assert!(reports.session("s").unwrap().tool.is_none());
        reports.apply(report(json!({"session_id": "s", "event": "turn.complete", "turn_id": "t", "answer": "Stopped.", "aborted": true})), 6);
        let session = reports.session("s").unwrap();
        assert_eq!(session.answer, Some(Answer { text: "Stopped.".into(), at: 6, aborted: true }));
        assert_eq!(session.working_since, None);
    }

    #[test]
    fn measurements_keep_the_context_the_cost_and_the_plan_limits_with_their_reset() {
        let reports = Reports::default();
        let measure = |week: f64, cost: f64| report(json!({"session_id": "s", "event": "session.measure",
            "context": {"tokens": 54944, "window": 1000000, "percent": 5},
            "rate_limits": [{"kind": "five_hour", "percent_used": 10, "resets_at": "2026-10-02T16:40:00Z"},
                            {"kind": "seven_day", "percent_used": week, "resets_at": "2026-10-05T00:00:00Z"}],
            "cost_usd": cost}));
        let now = 1_790_950_000_000;
        reports.apply_on(measure(20.0, 0.5), now, "2026-10-02");
        reports.apply_on(measure(30.0, 1.25), now + 1, "2026-10-02");
        let session = reports.session("s").unwrap();
        assert_eq!(session.context, Some(ContextReading { percent: Some(5.0), tokens: Some(54944), window: 1_000_000 }));
        assert_eq!(session.cost_usd, Some(1.25));
        let limits = reports.limits.lock().unwrap();
        let usage = alondra_core::usage::compute(&limits.windows, limits.day_start.as_ref().map(|d| d.2), limits.measured_at, now).unwrap();
        assert_eq!((usage.week_percent, usage.today_spent), (30.0, Some(10.0)), "today counts from the day's first measurement");
        assert_eq!(usage.week_resets_at, 1_791_158_400_000);
        assert_eq!((usage.five_hour_percent, usage.five_hour_resets_at), (Some(10.0), Some(1_790_959_200_000)));
        drop(limits);
        reports.apply_on(measure(31.0, 2.0), now + 2, "2026-10-03");
        assert_eq!(reports.limits.lock().unwrap().day_start.as_ref().map(|d| d.2), Some(31.0), "a new day starts from its own first measurement");
    }

    #[tokio::test]
    async fn a_launch_waits_for_the_mod_and_gives_up_after_the_deadline() {
        let reports = std::sync::Arc::new(Reports::default());
        assert!(!reports.wait_loaded("silent", Duration::from_millis(20)).await, "no session.start before the deadline");
        let waiting = tokio::spawn({
            let reports = reports.clone();
            async move { reports.wait_loaded("s", MODS_DEADLINE).await }
        });
        tokio::task::yield_now().await;
        reports.apply(report(json!({"session_id": "s", "event": "session.start", "version": "2.1.287"})), 1);
        assert!(waiting.await.unwrap());
        assert!(reports.wait_loaded("s", Duration::ZERO).await, "a session that already reported does not wait");
        reports.connect(hello("reconnected"), 2);
        assert!(reports.wait_loaded("reconnected", Duration::ZERO).await, "a connection says the mod loaded too");
    }
}
