//! What the Metrics screen and `alondra metrics` show: the ended tasks of a period, the weeks
//! older tasks were added into and what sessions without a task spent, summed and split by model,
//! approach and project, beside another period.
use super::fold::Outcome;
use crate::Millis;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const DAY_MS: Millis = 24 * 60 * 60 * 1_000;
/// How many releases the comparison offers, newest first.
pub const RELEASES_OFFERED: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

/// What a task's sessions spent, once any of them was measured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spend {
    /// None when no model it used was priced.
    pub cost_micros: Option<i64>,
    pub tokens: Tokens,
}

/// One ended task as `task_metrics` keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRow {
    pub project: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub approach: Option<String>,
    /// Of the attempt that ended it; unknown for tasks folded before it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The kind the orchestrator tagged it with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub outcome: Outcome,
    pub started_at: Option<Millis>,
    pub finished_at: Millis,
    pub clean: bool,
    pub change_rounds: i64,
    pub reviewer_rounds: i64,
    pub interventions: i64,
    pub reopened: bool,
    pub relaunched: bool,
    pub work_ms: i64,
    pub waiting_user_ms: i64,
    pub queued_ms: i64,
    pub blocked_ms: i64,
    pub sessions: i64,
    pub steps: i64,
    pub subagent_steps: i64,
    pub looks: i64,
    pub polls: i64,
    pub pr_merged: bool,
    pub pr_lines: Option<i64>,
    pub spend: Option<Spend>,
}

/// The tasks of one week, project, model, effort and approach that ended over 90 days ago. Empty
/// text stands for unknown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeekRow {
    /// Its Monday, midnight UTC.
    pub week: Millis,
    pub project: String,
    pub model: String,
    pub effort: String,
    pub approach: String,
    pub sums: Sums,
}

/// What sessions without a task spent in one day (UTC) on one model: the orchestrator's, and
/// those no task was known for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayRow {
    pub day: Millis,
    pub model: String,
    pub cost_micros: i64,
    /// Sessions whose cost was known.
    pub costed: i64,
    pub sessions: i64,
    pub tokens: Tokens,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub first_seen_at: Millis,
}

/// Everything a report is made from, as the store holds it at `now`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricsInputs {
    pub now: Millis,
    pub tasks: Vec<TaskRow>,
    pub weeks: Vec<WeekRow>,
    pub days: Vec<DayRow>,
    /// Oldest first.
    pub releases: Vec<Release>,
}

/// Additive totals of a set of ended tasks: what `metric_weeks` keeps of each week.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sums {
    pub tasks: i64,
    pub done: i64,
    pub clean: i64,
    pub change_rounds: i64,
    pub reviewer_rounds: i64,
    pub interventions: i64,
    pub reopened: i64,
    pub relaunched: i64,
    pub work_ms: i64,
    pub waiting_user_ms: i64,
    pub queued_ms: i64,
    pub blocked_ms: i64,
    /// Tasks with any session measured.
    pub measured: i64,
    pub sessions: i64,
    pub steps: i64,
    pub subagent_steps: i64,
    pub looks: i64,
    pub polls: i64,
    pub merged_prs: i64,
    pub pr_lines: i64,
    /// Merged PRs whose size is known.
    pub lined_prs: i64,
    pub cost_micros: i64,
    /// Tasks whose cost is known.
    pub costed: i64,
    pub merged_cost_micros: i64,
    pub lined_cost_micros: i64,
    pub tokens: Tokens,
}

impl Sums {
    fn of(task: &TaskRow) -> Self {
        let flag = i64::from;
        let cost = task.spend.and_then(|spend| spend.cost_micros);
        let known = cost.unwrap_or(0);
        Sums {
            tasks: 1,
            done: flag(task.outcome == Outcome::Done),
            clean: flag(task.clean),
            change_rounds: task.change_rounds,
            reviewer_rounds: task.reviewer_rounds,
            interventions: task.interventions,
            reopened: flag(task.reopened),
            relaunched: flag(task.relaunched),
            work_ms: task.work_ms,
            waiting_user_ms: task.waiting_user_ms,
            queued_ms: task.queued_ms,
            blocked_ms: task.blocked_ms,
            measured: flag(task.spend.is_some()),
            sessions: task.sessions,
            steps: task.steps,
            subagent_steps: task.subagent_steps,
            looks: task.looks,
            polls: task.polls,
            merged_prs: flag(task.pr_merged),
            pr_lines: task.pr_lines.unwrap_or(0),
            lined_prs: flag(task.pr_lines.is_some()),
            cost_micros: known,
            costed: flag(cost.is_some()),
            merged_cost_micros: if task.pr_merged { known } else { 0 },
            lined_cost_micros: if task.pr_lines.is_some() { known } else { 0 },
            tokens: task.spend.map(|spend| spend.tokens).unwrap_or_default(),
        }
    }

    fn add(&mut self, other: &Sums) {
        macro_rules! add { ($($field:ident),*) => { $(self.$field += other.$field;)* } }
        add!(tasks, done, clean, change_rounds, reviewer_rounds, interventions, reopened, relaunched, work_ms, waiting_user_ms,
            queued_ms, blocked_ms, measured, sessions, steps, subagent_steps, looks, polls, merged_prs, pr_lines, lined_prs,
            cost_micros, costed, merged_cost_micros, lined_cost_micros);
        self.tokens.input += other.tokens.input;
        self.tokens.output += other.tokens.output;
        self.tokens.cache_read += other.tokens.cache_read;
        self.tokens.cache_write += other.tokens.cache_write;
    }
}

/// What sessions without a task spent: the orchestrator and nodes no task was known for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unattributed {
    pub cost_micros: i64,
    pub costed: i64,
    pub sessions: i64,
    pub tokens: Tokens,
}

/// The middle value of what each task of the period kept on its own row: weeks keep no medians.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Medians {
    /// From its first run until it ended.
    pub span_ms: Option<i64>,
    pub cost_micros: Option<i64>,
    pub steps: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Figures {
    pub sums: Sums,
    pub unattributed: Unattributed,
    pub medians: Medians,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Count,
    /// Per task, with one decimal.
    Rate,
    Percent,
    Micros,
    Tokens,
    Millis,
}

/// Which way a value gets better. Totals grow with the work done, so they get neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Better {
    Higher,
    Lower,
    Neither,
}

macro_rules! metrics {
    ($($metric:ident: $unit:ident, $better:ident;)*) => {
        /// Every figure a report gives, in the order the screen shows them.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum Metric { $($metric),* }
        impl Metric {
            pub const ALL: &[Metric] = &[$(Metric::$metric),*];
            pub fn unit(self) -> Unit { match self { $(Metric::$metric => Unit::$unit),* } }
            pub fn better(self) -> Better { match self { $(Metric::$metric => Better::$better),* } }
        }
    };
}
metrics! {
    Tasks: Count, Neither;
    Clean: Percent, Higher;
    ChangeRounds: Rate, Lower;
    Interventions: Rate, Lower;
    Reopened: Count, Lower;
    Relaunched: Count, Lower;
    Cancelled: Count, Lower;
    CostTotal: Micros, Neither;
    CostPerTask: Micros, Lower;
    MedianCostPerTask: Micros, Lower;
    CostPerPr: Micros, Lower;
    CostPerLine: Micros, Lower;
    CostPerSession: Micros, Lower;
    InputTokens: Tokens, Neither;
    OutputTokens: Tokens, Neither;
    CacheReadTokens: Tokens, Neither;
    CacheWriteTokens: Tokens, Neither;
    StepsPerTask: Rate, Lower;
    MedianSteps: Count, Lower;
    IdleSteps: Percent, Lower;
    TaskTime: Millis, Lower;
    Working: Millis, Lower;
    WaitingForYou: Millis, Lower;
    Queued: Millis, Lower;
    Blocked: Millis, Lower;
}

fn ratio(part: i64, whole: i64) -> Option<f64> {
    (whole > 0).then(|| part as f64 / whole as f64)
}

impl Figures {
    /// None when nothing measured it: a cost no provider priced is unknown, not 0.
    pub fn value(&self, metric: Metric) -> Option<f64> {
        let (s, u, m) = (&self.sums, &self.unattributed, &self.medians);
        let tokens = |of: fn(&Tokens) -> i64| (s.measured > 0 || u.sessions > 0).then(|| (of(&s.tokens) + of(&u.tokens)) as f64);
        let per_task = |total: i64| ratio(total, s.tasks);
        match metric {
            Metric::Tasks => Some(s.tasks as f64),
            Metric::Clean => ratio(s.clean, s.done).map(|share| share * 100.0),
            Metric::ChangeRounds => per_task(s.change_rounds),
            Metric::Interventions => per_task(s.interventions),
            Metric::Reopened => Some(s.reopened as f64),
            Metric::Relaunched => Some(s.relaunched as f64),
            Metric::Cancelled => Some((s.tasks - s.done) as f64),
            Metric::CostTotal => (s.costed + u.costed > 0).then(|| (s.cost_micros + u.cost_micros) as f64),
            Metric::CostPerTask => ratio(s.cost_micros, s.costed),
            Metric::MedianCostPerTask => m.cost_micros.map(|cost| cost as f64),
            Metric::CostPerPr => (s.costed > 0).then(|| ratio(s.merged_cost_micros, s.merged_prs)).flatten(),
            Metric::CostPerLine => (s.costed > 0).then(|| ratio(s.lined_cost_micros, s.pr_lines)).flatten(),
            Metric::CostPerSession => (s.costed + u.costed > 0).then(|| ratio(s.cost_micros + u.cost_micros, s.sessions + u.sessions)).flatten(),
            Metric::InputTokens => tokens(|t| t.input),
            Metric::OutputTokens => tokens(|t| t.output),
            Metric::CacheReadTokens => tokens(|t| t.cache_read),
            Metric::CacheWriteTokens => tokens(|t| t.cache_write),
            Metric::StepsPerTask => ratio(s.steps + s.subagent_steps, s.measured),
            Metric::MedianSteps => m.steps.map(|steps| steps as f64),
            Metric::IdleSteps => ratio(s.looks + s.polls, s.steps + s.subagent_steps).map(|share| share * 100.0),
            Metric::TaskTime => m.span_ms.map(|span| span as f64),
            Metric::Working => per_task(s.work_ms),
            Metric::WaitingForYou => per_task(s.waiting_user_ms),
            Metric::Queued => per_task(s.queued_ms),
            Metric::Blocked => per_task(s.blocked_ms),
        }
    }
}

/// How `current` moved from `previous`, in percent; none when there was nothing to move from.
pub fn change(current: f64, previous: f64) -> Option<f64> {
    (previous != 0.0).then(|| (current - previous) / previous.abs() * 100.0)
}

/// Whether a move is good news, bad news or neither.
pub fn verdict(metric: Metric, current: f64, previous: f64) -> Option<bool> {
    match metric.better() {
        _ if current == previous => None,
        Better::Higher => Some(current > previous),
        Better::Lower => Some(current < previous),
        Better::Neither => None,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Period {
    #[default]
    Week,
    Month,
    Quarter,
    All,
}

impl Period {
    pub const ALL: &[Period] = &[Period::Week, Period::Month, Period::Quarter, Period::All];

    pub fn as_str(self) -> &'static str {
        match self {
            Period::Week => "7d",
            Period::Month => "30d",
            Period::Quarter => "90d",
            Period::All => "all",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|period| period.as_str() == text)
    }

    pub fn days(self) -> Option<i64> {
        match self {
            Period::Week => Some(7),
            Period::Month => Some(30),
            Period::Quarter => Some(90),
            Period::All => None,
        }
    }
}

/// What the period is set beside.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "at", rename_all = "snake_case")]
pub enum Compare {
    /// The same length of time right before it.
    #[default]
    Previous,
    /// From the first start of this daemon version until now, beside as long before it.
    Release(String),
    /// From this day (UTC) until now, beside as long before it.
    Date(String),
    None,
}

impl Compare {
    pub fn as_text(&self) -> String {
        match self {
            Compare::Previous => "previous".into(),
            Compare::Release(version) => format!("release:{version}"),
            Compare::Date(day) => format!("date:{day}"),
            Compare::None => "none".into(),
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "previous" => Some(Compare::Previous),
            "none" => Some(Compare::None),
            _ => if let Some(version) = text.strip_prefix("release:").filter(|v| !v.is_empty()) {
                Some(Compare::Release(version.into()))
            } else {
                let day = text.strip_prefix("date:")?;
                day_millis(day).map(|_| Compare::Date(day.into()))
            },
        }
    }
}

/// What a report is asked for: a view key's query, or `/metrics`'s.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Query {
    pub period: Period,
    pub compare: Compare,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

pub const KEY: &str = "metrics";

impl Query {
    /// `period=7d&compare=previous&project=…`: any part left out takes its default; an unknown
    /// part or value is refused.
    pub fn parse(query: &str) -> Option<Self> {
        let mut parsed = Query::default();
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (name, value) = pair.split_once('=')?;
            let value = decode(value)?;
            match name {
                "period" => parsed.period = Period::parse(&value)?,
                "compare" => parsed.compare = Compare::parse(&value)?,
                "project" => parsed.project = Some(value).filter(|v| !v.is_empty()),
                "model" => parsed.model = Some(value).filter(|v| !v.is_empty()),
                "effort" => parsed.effort = Some(value).filter(|v| !v.is_empty()),
                _ => return None,
            }
        }
        Some(parsed)
    }

    /// The query of a view key: `metrics` or `metrics?…`.
    pub fn of_key(key: &str) -> Option<Self> {
        if key == KEY { return Some(Query::default()); }
        Self::parse(key.strip_prefix(KEY)?.strip_prefix('?')?)
    }

    pub fn query_string(&self) -> String {
        let mut parts = vec![format!("period={}", self.period.as_str()), format!("compare={}", encode(&self.compare.as_text()))];
        for (name, value) in [("project", &self.project), ("model", &self.model), ("effort", &self.effort)] {
            if let Some(value) = value { parts.push(format!("{name}={}", encode(value))); }
        }
        parts.join("&")
    }

    pub fn key(&self) -> String {
        format!("{KEY}?{}", self.query_string())
    }

    /// The window shown and the one it is set beside, as [from, until).
    fn windows(&self, now: Millis, releases: &[Release]) -> (Window, Option<Window>) {
        let split = match &self.compare {
            Compare::Release(version) => releases.iter().find(|r| &r.version == version).map(|r| r.first_seen_at),
            Compare::Date(day) => day_millis(day),
            Compare::Previous | Compare::None => None,
        };
        if let Some(split) = split.filter(|split| *split < now) {
            let length = now - split;
            return (Window { from: Some(split), until: now }, Some(Window { from: Some(split - length), until: split }));
        }
        let Some(days) = self.period.days() else { return (Window { from: None, until: now }, None) };
        let from = now - days * DAY_MS;
        let previous = (self.compare == Compare::Previous).then(|| Window { from: Some(from - days * DAY_MS), until: from });
        (Window { from: Some(from), until: now }, previous)
    }
}

fn decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
                decoded.push(u8::from_str_radix(hex, 16).ok()?);
                index += 3;
            }
            b'+' => { decoded.push(b' '); index += 1; }
            byte => { decoded.push(byte); index += 1; }
        }
    }
    String::from_utf8(decoded).ok().filter(|text| text.len() <= 256 && !text.chars().any(char::is_control))
}

fn encode(value: &str) -> String {
    value.bytes().map(|byte| match byte {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => (byte as char).to_string(),
        _ => format!("%{byte:02X}"),
    }).collect()
}

/// Midnight UTC of `YYYY-MM-DD`.
pub fn day_millis(day: &str) -> Option<Millis> {
    let mut parts = day.splitn(3, '-');
    let (year, month, date): (i64, i64, i64) = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    if day.len() != 10 || !(1..=12).contains(&month) || !(1..=31).contains(&date) { return None; }
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + date - 1;
    let of_era_days = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    Some((era * 146_097 + of_era_days - 719_468) * DAY_MS)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub from: Option<Millis>,
    pub until: Millis,
}

impl Window {
    fn holds(&self, at: Millis) -> bool {
        self.from.is_none_or(|from| at >= from) && at < self.until
    }
}

/// What each kind of row is split by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Model,
    Approach,
    Project,
    Provider,
    Kind,
}

/// The provider a row ran on: the one recorded, else the one its provider-qualified model names.
/// A bare model was Claude's, the only provider before providers were recorded.
pub fn provider_of(provider: Option<&str>, model: Option<&str>) -> Option<crate::accounts::Provider> {
    use crate::accounts::Provider;
    provider.and_then(|provider| Provider::parse(provider).ok())
        .or_else(|| model.map(|model| model.split_once('/').and_then(|(provider, _)| Provider::parse(provider).ok()).unwrap_or(Provider::Claude)))
}

/// What a row is split by.
#[derive(Clone, Copy)]
struct Keys<'a> {
    project: Option<&'a str>,
    model: Option<&'a str>,
    effort: Option<&'a str>,
    approach: Option<&'a str>,
    provider: Option<&'static str>,
    kind: Option<&'a str>,
}

impl<'a> Keys<'a> {
    fn of_task(task: &'a TaskRow) -> Self {
        let model = task.model.as_deref();
        Keys { project: task.project.as_deref(), model, effort: task.effort.as_deref(), approach: task.approach.as_deref(),
            provider: provider_of(task.provider.as_deref(), model).map(|provider| provider.as_str()), kind: task.kind.as_deref() }
    }

    fn of_week(week: &'a WeekRow) -> Self {
        let model = known(&week.model);
        Keys { project: known(&week.project), model, effort: known(&week.effort), approach: known(&week.approach),
            provider: provider_of(None, model).map(|provider| provider.as_str()), kind: None }
    }

    fn name(&self, split: Split) -> Option<&'a str> {
        match split {
            Split::Model => self.model,
            Split::Approach => self.approach,
            Split::Project => self.project,
            Split::Provider => self.provider,
            Split::Kind => self.kind,
        }
    }

    fn in_split(&self, split: Option<(Split, &str, Option<&str>)>) -> bool {
        match split {
            None => true,
            Some((Split::Model, name, effort)) => self.model.unwrap_or("") == name && self.effort == effort,
            Some((split, name, _)) => self.name(split).unwrap_or("") == name,
        }
    }

    fn matches(&self, query: &Query) -> bool {
        matches(query, self.project, self.model, self.effort)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Group {
    /// Empty for unknown.
    pub name: String,
    /// The model's effort, for a model group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub figures: Figures,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Value {
    pub metric: Metric,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<f64>,
    /// In percent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<f64>,
    /// Whether the move is better (true) or worse (false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub better: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetricsReport {
    pub query: Query,
    pub window: Window,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_window: Option<Window>,
    pub figures: Figures,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<Figures>,
    pub values: Vec<Value>,
    pub by_model: Vec<Group>,
    pub by_approach: Vec<Group>,
    pub by_project: Vec<Group>,
    #[serde(default)]
    pub by_provider: Vec<Group>,
    #[serde(default)]
    pub by_kind: Vec<Group>,
    /// The releases the period can be set beside, newest first.
    pub releases: Vec<Release>,
}

impl MetricsReport {
    pub fn value(&self, metric: Metric) -> Option<&Value> {
        self.values.iter().find(|value| value.metric == metric)
    }
}

fn matches(query: &Query, project: Option<&str>, model: Option<&str>, effort: Option<&str>) -> bool {
    let wanted = |filter: &Option<String>, value: Option<&str>| filter.as_deref().is_none_or(|filter| value == Some(filter));
    wanted(&query.project, project) && wanted(&query.model, model) && wanted(&query.effort, effort)
}

fn known(text: &str) -> Option<&str> {
    (!text.is_empty()).then_some(text)
}

fn median(mut values: Vec<i64>) -> Option<i64> {
    values.sort_unstable();
    let middle = values.len() / 2;
    match values.len() {
        0 => None,
        n if n % 2 == 1 => Some(values[middle]),
        _ => Some((values[middle - 1] + values[middle]) / 2),
    }
}

/// The figures of a window: of the rows the query lets through and, for a group, of its rows.
fn figures(inputs: &MetricsInputs, query: &Query, window: Window, split: Option<(Split, &str, Option<&str>)>) -> Figures {
    let mut figures = Figures::default();
    let (mut spans, mut costs, mut steps) = (vec![], vec![], vec![]);
    for task in &inputs.tasks {
        let keys = Keys::of_task(task);
        if !window.holds(task.finished_at) || !keys.matches(query) || !keys.in_split(split) { continue; }
        figures.sums.add(&Sums::of(task));
        spans.extend(task.started_at.map(|started| (task.finished_at - started).max(0)));
        costs.extend(task.spend.and_then(|spend| spend.cost_micros));
        if task.spend.is_some() { steps.push(task.steps + task.subagent_steps); }
    }
    for week in &inputs.weeks {
        let keys = Keys::of_week(week);
        if !window.holds(week.week) || !keys.matches(query) || !keys.in_split(split) { continue; }
        figures.sums.add(&week.sums);
    }
    if split.is_none() && query.project.is_none() && query.effort.is_none() {
        for day in inputs.days.iter().filter(|day| window.holds(day.day) && matches(query, None, Some(&day.model), None)) {
            let u = &mut figures.unattributed;
            u.cost_micros += day.cost_micros;
            u.costed += day.costed;
            u.sessions += day.sessions;
            u.tokens.input += day.tokens.input;
            u.tokens.output += day.tokens.output;
            u.tokens.cache_read += day.tokens.cache_read;
            u.tokens.cache_write += day.tokens.cache_write;
        }
    }
    figures.medians = Medians { span_ms: median(spans), cost_micros: median(costs), steps: median(steps) };
    figures
}

/// The groups of a window, the busiest first.
fn groups(inputs: &MetricsInputs, query: &Query, window: Window, split: Split) -> Vec<Group> {
    let mut names: BTreeMap<(String, Option<String>), ()> = BTreeMap::new();
    let mut note = |keys: Keys<'_>| {
        if keys.matches(query) {
            let effort = (split == Split::Model).then(|| keys.effort.map(str::to_owned)).flatten();
            names.insert((keys.name(split).unwrap_or("").to_owned(), effort), ());
        }
    };
    for task in inputs.tasks.iter().filter(|task| window.holds(task.finished_at)) {
        note(Keys::of_task(task));
    }
    for week in inputs.weeks.iter().filter(|week| window.holds(week.week)) {
        note(Keys::of_week(week));
    }
    let mut groups: Vec<Group> = names.into_keys().map(|(name, effort)| {
        let figures = figures(inputs, query, window, Some((split, &name, effort.as_deref())));
        Group { name, effort, figures }
    }).collect();
    groups.sort_by(|a, b| b.figures.sums.tasks.cmp(&a.figures.sums.tasks).then_with(|| a.name.cmp(&b.name)));
    groups
}

/// The report `query` asks for.
pub fn report(inputs: &MetricsInputs, query: &Query) -> MetricsReport {
    let (window, previous_window) = query.windows(inputs.now, &inputs.releases);
    let current = figures(inputs, query, window, None);
    let previous = previous_window.map(|window| figures(inputs, query, window, None));
    let values = Metric::ALL.iter().map(|&metric| {
        let value = current.value(metric);
        let before = previous.as_ref().and_then(|previous| previous.value(metric));
        let (change, better) = match (value, before) {
            (Some(now), Some(before)) => (change(now, before), verdict(metric, now, before)),
            _ => (None, None),
        };
        Value { metric, value, previous: before, change, better }
    }).collect();
    MetricsReport {
        query: query.clone(),
        window,
        previous_window,
        figures: current,
        previous,
        values,
        by_model: groups(inputs, query, window, Split::Model),
        by_approach: groups(inputs, query, window, Split::Approach),
        by_project: groups(inputs, query, window, Split::Project),
        by_provider: groups(inputs, query, window, Split::Provider),
        by_kind: groups(inputs, query, window, Split::Kind),
        releases: inputs.releases.iter().rev().take(RELEASES_OFFERED).cloned().collect(),
    }
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
