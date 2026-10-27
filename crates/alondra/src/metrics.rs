//! `alondra metrics`: the report the Metrics screen shows, with the same words.
use std::time::Duration;

use alondra_core::metrics::report::{Compare, Group, MetricsReport, Period, Query};
use alondra_core::views::metrics::{compare_label, compared, estimate_notice, group_name, group_summary, label, period_label, sections};
use anyhow::{Result, bail};

use crate::client::Client;

pub struct Options<'a> {
    pub period: &'a str,
    pub compare: &'a str,
    pub project: Option<&'a str>,
    pub model: Option<&'a str>,
}

pub fn run(options: &Options<'_>, json: bool) -> Result<()> {
    let query = query(options)?;
    let report: MetricsReport = Client::new(Duration::from_secs(10)).get(&format!("/metrics?{}", query.query_string()))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&crate::version::with_warning(serde_json::to_value(&report)?))?);
    } else {
        print!("{}", render(&report));
    }
    Ok(())
}

/// The query the flags ask for: `--compare` takes `previous`, `none`, `release:<version>` or a
/// day, `YYYY-MM-DD`.
pub fn query(options: &Options<'_>) -> Result<Query> {
    let Some(period) = Period::parse(options.period) else {
        let period = options.period;
        bail!(trf!("{period} is not a period: use 7d, 30d, 90d or all", "{period} no es un periodo: usa 7d, 30d, 90d o all"))
    };
    let compare_text = if alondra_core::metrics::report::day_millis(options.compare).is_some() {
        format!("date:{}", options.compare)
    } else {
        options.compare.to_owned()
    };
    let Some(compare) = Compare::parse(&compare_text) else {
        let compare = options.compare;
        bail!(trf!("{compare} is not a comparison: use previous, none, release:<version> or a day like 2026-10-19",
            "{compare} no es una comparación: usa previous, none, release:<versión> o un día como 2026-10-19"))
    };
    let text = |value: Option<&str>| value.map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned);
    Ok(Query { period, compare, project: text(options.project), model: text(options.model), effort: None })
}

fn groups(out: &mut String, title: &str, groups: &[Group]) {
    if groups.is_empty() {
        return;
    }
    let lang = crate::i18n::lang();
    out.push_str(&format!("\n{title}\n"));
    for group in groups {
        out.push_str(&format!("  {}: {}\n", group_name(group, lang), group_summary(group, lang)));
    }
}

pub fn render(report: &MetricsReport) -> String {
    let lang = crate::i18n::lang();
    let query = &report.query;
    let mut heading = vec![tr!("Metrics", "Métricas").to_owned()];
    match (&query.compare, report.previous_window.is_some()) {
        (Compare::Release(_) | Compare::Date(_), true) => heading.push(compare_label(&query.compare, lang)),
        (compare, compared) => {
            heading.push(period_label(query.period, lang).to_owned());
            if compared && *compare == Compare::Previous {
                heading.push(tr!("against the period before", "frente al periodo anterior").to_owned());
            }
        }
    }
    heading.extend(query.project.iter().map(|project| trf!("only {project}", "solo {project}")));
    heading.extend(query.model.iter().map(|model| trf!("only {model}", "solo {model}")));
    let mut out = format!("{}\n", heading.join(" · "));
    if report.figures.sums.tasks == 0 && report.figures.unattributed.sessions == 0 {
        out.push_str(tr!("No task ended in this period.\n", "Ninguna tarea terminó en este periodo.\n"));
        return out;
    }
    out.push_str(&format!("{}\n", estimate_notice(report, lang)));
    for (_, title, metrics) in sections(lang) {
        out.push_str(&format!("\n{title}\n"));
        let width = metrics.iter().map(|metric| label(*metric, lang).chars().count()).max().unwrap_or(0);
        for value in metrics.iter().filter_map(|metric| report.value(*metric)) {
            let name = label(value.metric, lang);
            let pad = " ".repeat(width - name.chars().count());
            out.push_str(&format!("  {name}{pad}  {}\n", compared(value, lang)));
        }
    }
    groups(&mut out, tr!("By model", "Por modelo"), &report.by_model);
    groups(&mut out, tr!("By provider", "Por proveedor"), &report.by_provider);
    groups(&mut out, tr!("By kind of task", "Por tipo de tarea"), &report.by_kind);
    groups(&mut out, tr!("By approach", "Por enfoque"), &report.by_approach);
    groups(&mut out, tr!("By project", "Por proyecto"), &report.by_project);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::metrics::fold::Outcome;
    use alondra_core::metrics::report::{DAY_MS, MetricsInputs, Spend, TaskRow, Tokens, report};
    use alondra_core::views::Lang;

    fn options<'a>(period: &'a str, compare: &'a str) -> Options<'a> {
        Options { period, compare, project: Some("app"), model: None }
    }

    fn task(days_ago: i64, change_rounds: i64) -> TaskRow {
        let finished_at = 100 * DAY_MS - days_ago * DAY_MS;
        TaskRow {
            project: Some("app".into()), model: Some("sonnet".into()), effort: Some("medium".into()), approach: Some("solo".into()), provider: None, kind: None,
            outcome: Outcome::Done, started_at: Some(finished_at - 3_600_000), finished_at, clean: change_rounds == 0, change_rounds,
            reviewer_rounds: 0, interventions: 1, reopened: false, relaunched: false, work_ms: 1_800_000, waiting_user_ms: 600_000,
            queued_ms: 0, blocked_ms: 0, sessions: 1, steps: 80, subagent_steps: 20, looks: 30, polls: 10, pr_merged: true,
            pr_lines: Some(150), spend: Some(Spend { cost_micros: Some(1_500_000), tokens: Tokens { input: 5_000, output: 40_000, cache_read: 12_000_000, cache_write: 300_000 } }),
        }
    }

    fn sample() -> MetricsReport {
        let inputs = MetricsInputs { now: 100 * DAY_MS, tasks: vec![task(1, 1), task(2, 0), task(9, 0)], ..MetricsInputs::default() };
        report(&inputs, &query(&options("7d", "previous")).unwrap())
    }

    #[test]
    fn the_flags_make_the_query_of_the_metrics_screen() {
        crate::i18n::set_for_this_thread(Lang::En);
        assert_eq!(query(&options("30d", "2026-10-19")).unwrap().query_string(), "period=30d&compare=date:2026-10-19&project=app");
        assert_eq!(query(&options("all", "release:0.3.5")).unwrap().key(), "metrics?period=all&compare=release:0.3.5&project=app");
        assert_eq!(query(&options("7d", "none")).unwrap().compare, Compare::None);
        assert!(query(&options("1y", "none")).unwrap_err().to_string().contains("1y is not a period"));
        for bad in ["later", "2026-13-01", "date:2026-10-19x"] {
            assert!(query(&options("7d", bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn json_prints_the_whole_report_as_the_route_sends_it() {
        let document = serde_json::to_value(sample()).unwrap();
        assert_eq!(document["query"]["period"], "week");
        assert_eq!(document["figures"]["sums"]["tasks"], 2);
        assert_eq!(document["previous"]["sums"]["tasks"], 1);
        let rounds = document["values"].as_array().unwrap().iter().find(|value| value["metric"] == "change_rounds").unwrap();
        assert_eq!((&rounds["value"], &rounds["previous"], &rounds["better"]), (&serde_json::json!(0.5), &serde_json::json!(0.0), &serde_json::json!(false)));
        assert_eq!(document["by_model"][0]["name"], "sonnet");
        let back: MetricsReport = serde_json::from_value(document).unwrap();
        assert_eq!(back, sample(), "`--json` reads back into the same report");
    }

    #[test]
    fn the_text_groups_by_provider_and_by_kind_of_task() {
        let mut codex = task(1, 0);
        codex.model = Some("codex/gpt-6.1-sol".into());
        codex.kind = Some("migration".into());
        let inputs = MetricsInputs { now: 100 * DAY_MS, tasks: vec![codex, task(2, 1)], ..MetricsInputs::default() };
        let sample = report(&inputs, &query(&options("7d", "none")).unwrap());
        crate::i18n::set_for_this_thread(Lang::En);
        let text = render(&sample);
        for expected in ["By provider\n  claude: 1 task", "\n  codex: 1 task", "By kind of task\n  Unknown: 1 task", "\n  migration: 1 task"] {
            assert!(text.contains(expected), "{expected} missing from\n{text}");
        }
        crate::i18n::set_for_this_thread(Lang::Es);
        let text = render(&sample);
        for expected in ["Por proveedor\n  claude: 1 tarea", "Por tipo de tarea\n  Desconocido: 1 tarea"] {
            assert!(text.contains(expected), "{expected} missing from\n{text}");
        }
    }

    #[test]
    fn the_text_reads_like_the_screen_in_the_users_language() {
        crate::i18n::set_for_this_thread(Lang::En);
        let text = render(&sample());
        for expected in ["Metrics · Last 7 days · against the period before · only app",
            "Provider's estimate, not your bill · measured in 2 of 2 tasks", "Tasks ended", "2 · before 1 (+100%)",
            "Change rounds per task       0.5 · before 0.0", "Per task          $1.50 · before $1.50\n",
            "By model\n  sonnet · Medium: 2 tasks · 50% no rework · $1.50 per task"] {
            assert!(text.contains(expected), "{expected} missing from\n{text}");
        }
        crate::i18n::set_for_this_thread(Lang::Es);
        let text = render(&sample());
        for expected in ["Métricas · Últimos 7 días · frente al periodo anterior · solo app", "medido en 2 de 2 tareas", "Rondas de cambios por tarea",
            "Por modelo\n  sonnet · Medio: 2 tareas · 50 % sin retrabajo · 1,50 $ por tarea"] {
            assert!(text.contains(expected), "{expected} missing from\n{text}");
        }
        let empty = report(&MetricsInputs::default(), &Query::default());
        assert!(render(&empty).contains("Ninguna tarea terminó en este periodo."));
    }
}
