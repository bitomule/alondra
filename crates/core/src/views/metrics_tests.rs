use super::*;
use crate::domain::{DomainState, ReadIndex};
use crate::metrics::fold::Outcome;
use crate::metrics::report::{DAY_MS, DayRow, Release, Spend, TaskRow, Tokens};
use crate::views::{Client, Item, LiveSnapshot, Transport, build as build_view};

const NOW: i64 = 20_752 * DAY_MS;

fn task(days_ago: i64, project: &str, model: &str, change_rounds: i64, cost: Option<i64>) -> TaskRow {
    let finished_at = NOW - days_ago * DAY_MS - 3_600_000;
    TaskRow {
        project: Some(project.into()), model: Some(model.into()), effort: Some("medium".into()), approach: Some("solo".into()), provider: None, kind: None,
        outcome: Outcome::Done, started_at: Some(finished_at - 5_400_000), finished_at, clean: change_rounds == 0, change_rounds,
        reviewer_rounds: 0, interventions: 1, reopened: false, relaunched: false, work_ms: 3_000_000, waiting_user_ms: 1_200_000,
        queued_ms: 120_000, blocked_ms: 0, sessions: 2, steps: 140, subagent_steps: 30, looks: 60, polls: 6, pr_merged: true,
        pr_lines: Some(240), spend: Some(Spend { cost_micros: cost, tokens: Tokens { input: 9_000, output: 60_000, cache_read: 25_000_000, cache_write: 700_000 } }),
    }
}

/// Two weeks of tasks on two projects and two models, one of them never priced, the
/// orchestrator's spend, and two releases.
fn inputs() -> MetricsInputs {
    MetricsInputs {
        now: NOW,
        tasks: vec![
            task(9, "alondra", "sonnet", 0, Some(2_000_000)),
            task(10, "quill", "sonnet", 2, Some(3_000_000)),
            task(1, "alondra", "sonnet", 0, Some(1_500_000)),
            task(2, "alondra", "opus", 1, Some(6_000_000)),
            task(3, "quill", "codex", 0, None),
        ],
        weeks: vec![],
        days: vec![DayRow { day: NOW - DAY_MS, model: "opus".into(), cost_micros: 800_000, costed: 4, sessions: 4,
            tokens: Tokens { input: 2_000, output: 30_000, cache_read: 9_000_000, cache_write: 200_000 } }],
        releases: vec![Release { version: "0.3.4".into(), first_seen_at: NOW - 12 * DAY_MS }, Release { version: "0.3.5".into(), first_seen_at: NOW - 4 * DAY_MS }],
    }
}

fn render(key: &str, live: &LiveSnapshot, lang: Lang) -> View {
    let state = DomainState::new("test-epoch");
    let index = ReadIndex::build(&state);
    build_view(key, RenderContext { domain: &state, index: &index, live, client: Client::Ios, lang, transport: Transport::Loopback,
        cursor: "metrics-cursor", rendered_at: "2026-10-26T10:00:00Z" }).expect("the metrics view renders")
}

fn write_golden(filename: &str, view: &View) {
    if let Ok(dir) = std::env::var("ALONDRA_GOLDEN_OUT") {
        std::fs::write(format!("{dir}/{filename}"), format!("{}\n", serde_json::to_string_pretty(view).unwrap())).unwrap();
    }
}

fn golden(filename: &str, view: &View, expected: &str) {
    let expected: serde_json::Value = serde_json::from_str(expected).unwrap();
    assert_eq!(serde_json::to_value(view).unwrap(), expected, "{filename}");
}

fn live() -> LiveSnapshot {
    LiveSnapshot { metrics: Some(inputs()), ..LiveSnapshot::default() }
}

#[test]
fn the_metrics_view_matches_its_goldens() {
    let key = "metrics?period=7d&compare=previous";
    let (english, spanish) = (render(key, &live(), Lang::En), render(key, &live(), Lang::Es));
    write_golden("metrics-ios.json", &english);
    write_golden("metrics-ios-es.json", &spanish);
    golden("metrics-ios.json", &english, include_str!("../../../../fixtures/views/metrics/metrics-ios.json"));
    golden("metrics-ios-es.json", &spanish, include_str!("../../../../fixtures/views/metrics/metrics-ios-es.json"));
}

fn facts<'a>(view: &'a View, section: &str) -> Vec<(&'a str, &'a str, Option<Tone>)> {
    view.sections.iter().filter(|s| s.id == section).flat_map(|s| &s.items).flat_map(|item| match item {
        Item::Facts(facts) => facts.items.iter().map(|f| (f.label.as_str(), f.value.as_str(), f.tone)).collect(),
        _ => vec![],
    }).collect()
}

fn links(view: &View, section: &str) -> Vec<(String, Option<String>, bool)> {
    view.sections.iter().filter(|s| s.id == section).flat_map(|s| &s.items).filter_map(|item| match item {
        Item::Row(row) => Some((row.title.clone(), row.link.as_ref().map(|link| match link {
            crate::views::Link::View { key } => key.clone(),
            other => panic!("{other:?}"),
        }), !row.badges.is_empty())),
        _ => None,
    }).collect()
}

#[test]
fn each_value_reads_against_the_period_before_in_green_when_better_and_orange_when_worse() {
    let view = render("metrics?period=7d&compare=previous", &live(), Lang::En);
    let summary = facts(&view, "summary");
    assert!(summary.contains(&("Tasks ended", "3 · before 2 (+50%)", None)), "{summary:?}");
    assert!(summary.contains(&("Your interventions per task", "1.0 · before 1.0", None)), "an unchanged value has no percentage: {summary:?}");
    assert!(summary.contains(&("Change rounds per task", "0.3 · before 1.0 (-67%)", Some(Tone::Success))), "{summary:?}");
    assert!(summary.contains(&("Done with no rework", "67% · before 50% (+33%)", Some(Tone::Success))), "{summary:?}");
    let cost = facts(&view, "cost");
    assert!(cost.contains(&("Per task", "$3.75 · before $2.50 (+50%)", Some(Tone::Warning))), "an unpriced task stays out of the mean: {cost:?}");
    assert!(cost.contains(&("Total", "$8.30 · before $5.00 (+66%)", None)), "the orchestrator's spend is in the total, which grows with the work: {cost:?}");
    assert_eq!(view.notices.last().map(|n| n.text.as_str()), Some("Provider's estimate, not your bill · measured in 3 of 3 tasks"));
}

#[test]
fn the_selectors_and_the_groups_link_to_the_keys_they_show() {
    let view = render("metrics?period=7d&compare=previous", &live(), Lang::En);
    assert_eq!(links(&view, "period"), [
        ("Last 7 days".into(), None, true),
        ("Last 30 days".into(), Some("metrics?period=30d&compare=previous".into()), false),
        ("Last 90 days".into(), Some("metrics?period=90d&compare=previous".into()), false),
        ("All time".into(), Some("metrics?period=all&compare=none".into()), false),
    ]);
    assert_eq!(links(&view, "compare"), [
        ("The period before".into(), None, true),
        ("Since version 0.3.5, against as long before".into(), Some("metrics?period=7d&compare=release:0.3.5".into()), false),
        ("Since version 0.3.4, against as long before".into(), Some("metrics?period=7d&compare=release:0.3.4".into()), false),
        ("Nothing".into(), Some("metrics?period=7d&compare=none".into()), false),
    ]);
    assert_eq!(links(&view, "by_project")[0], ("alondra".into(), Some("metrics?period=7d&compare=previous&project=alondra".into()), false));
    assert_eq!(links(&view, "by_model")[0].1.as_deref(), Some("metrics?period=7d&compare=previous&model=codex&effort=medium"));

    let filtered = render("metrics?period=7d&compare=release:0.3.5&project=alondra", &live(), Lang::Es);
    assert!(links(&filtered, "period").iter().all(|(_, link, shown)| !shown && link.as_deref().is_some_and(|key| !key.contains("release"))),
        "a release sets the time itself: choosing a period leaves it");
    assert_eq!(links(&filtered, "filter"), [("Solo alondra".into(), Some("metrics?period=7d&compare=release:0.3.5".into()), false)]);
    assert_eq!(links(&filtered, "by_project"), [("alondra".into(), None, false)], "the group already shown links nowhere");
}

#[test]
fn a_period_where_nothing_ended_says_so_and_keeps_its_selectors() {
    let view = render("metrics?period=7d&compare=none", &LiveSnapshot::default(), Lang::Es);
    assert_eq!(view.notices.iter().map(|n| n.text.as_str()).collect::<Vec<_>>(), ["Ninguna tarea terminó en este periodo"]);
    assert_eq!(view.sections.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["period", "compare"]);
    assert_eq!(super::super::Family::of("metrics"), Some(super::super::Family::Metrics));
    for refused in ["metrics?period=1y", "metrics?sort=cost"] {
        let state = DomainState::new("test-epoch");
        let index = ReadIndex::build(&state);
        let context = RenderContext { domain: &state, index: &index, live: &LiveSnapshot::default(), client: Client::Ios, lang: Lang::En,
            transport: Transport::Loopback, cursor: "", rendered_at: "" };
        assert_eq!(build_view(refused, context).unwrap_err(), BuildError::InvalidKey, "{refused}");
    }
}

#[test]
fn values_are_written_in_their_unit_and_language() {
    assert_eq!(shown(Unit::Micros, 1_234_567.0, Lang::En), "$1.23");
    assert_eq!(shown(Unit::Micros, 1_234_567.0, Lang::Es), "1,23 $");
    assert_eq!(shown(Unit::Micros, 4_200.0, Lang::En), "$0.0042", "a line costs fractions of a cent");
    assert_eq!(shown(Unit::Micros, 830_000.0, Lang::En), "$0.83");
    assert_eq!(shown(Unit::Tokens, 8_220_000_000.0, Lang::En), "8.22 B");
    assert_eq!(shown(Unit::Tokens, 8_220_000_000.0, Lang::Es), "8,22 mil M");
    assert_eq!(shown(Unit::Tokens, 293_000.0, Lang::En), "293k");
    assert_eq!(shown(Unit::Millis, 45.0 * 60_000.0, Lang::En), "45 min");
    assert_eq!(shown(Unit::Millis, 150.0 * 60_000.0, Lang::Es), "2,5 h");
    assert_eq!(shown(Unit::Percent, 42.2, Lang::Es), "42 %");
}
