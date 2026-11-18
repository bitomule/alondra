//! Metrics: what ended tasks and sessions cost and how the tasks went, for a period, beside the
//! period before or before a release. `alondra metrics` prints the same report with these words.
use super::{
    BuildError, Fact, Facts, Icon, Item, Label, Lang, Notice, RenderContext, Section, Tone, View, common,
    text::{t, tf},
};
use alondra_core::metrics::report::{Compare, Group, MetricsInputs, MetricsReport, Period, Query, report};

pub use alondra_view_model::metrics::*;

pub(super) fn build(key: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    let query = Query::of_key(key).ok_or(BuildError::InvalidKey)?;
    let empty = MetricsInputs::default();
    let report = report(context.live.metrics.as_ref().unwrap_or(&empty), &query);
    let lang = context.lang;
    let mut view = common::view("metrics", key, Some(t!(lang, "Metrics", "Métricas").into()), context, vec![]);
    view.sections.push(period_section(&query, lang));
    view.sections.push(compare_section(&query, &report, lang));
    if let Some(section) = filter_section(&query, lang) {
        view.sections.push(section);
    }
    if report.figures.sums.tasks == 0 && report.figures.unattributed.sessions == 0 {
        view.notices.push(Notice { text: t!(lang, "No task ended in this period", "Ninguna tarea terminó en este periodo").into(),
            tone: Tone::Neutral, action: None });
        return Ok(view);
    }
    for (id, title, metrics) in sections(lang) {
        let items = metrics.iter().filter_map(|metric| report.value(*metric)).map(|value| Fact {
            kind: None,
            label: label(value.metric, lang).into(),
            value: compared(value, lang),
            tone: value.better.map(|better| if better { Tone::Success } else { Tone::Warning }),
            link: None,
        }).collect();
        view.sections.push(common::section(id, Some(title.into()), vec![Item::Facts(Facts { items })]));
    }
    let by_model = report.by_model.iter().map(|group| {
        let filtered = Query { model: Some(group.name.clone()).filter(|name| !name.is_empty()), effort: group.effort.clone(), ..query.clone() };
        group_row("model", group, Icon::Node, filtered, &query, lang)
    }).collect();
    view.sections.push(common::section("by_model", Some(t!(lang, "By model", "Por modelo").into()), by_model));
    let by_project = report.by_project.iter().map(|group| {
        let filtered = Query { project: Some(group.name.clone()).filter(|name| !name.is_empty()), ..query.clone() };
        group_row("project", group, Icon::Project, filtered, &query, lang)
    }).collect();
    view.sections.push(common::section("by_project", Some(t!(lang, "By project", "Por proyecto").into()), by_project));
    view.notices.push(Notice { text: estimate_notice(&report, lang), tone: Tone::Info, action: None });
    Ok(view)
}

/// A group's row, which shows only that group when tapped; a group with no name cannot be shown
/// alone, and the one already shown links nowhere.
fn group_row(kind: &str, group: &Group, icon: Icon, filtered: Query, query: &Query, lang: Lang) -> Item {
    let id = format!("{kind}:{}:{}", group.name, group.effort.as_deref().unwrap_or(""));
    let mut row = common::row(&id, &filtered.key(), icon, group_name(group, lang), Some(group_summary(group, lang)));
    if group.name.is_empty() || filtered == *query {
        row.link = None;
    }
    Item::Row(row)
}

fn choice(id: &str, title: String, target: Query, query: &Query, current: bool, lang: Lang) -> Item {
    let mut row = common::row(id, &target.key(), Icon::History, title, None);
    if current || target == *query {
        row.link = None;
        row.badges.push(Label { text: t!(lang, "Shown", "Mostrado").into(), tone: Tone::Accent, elapsed: None });
    }
    Item::Row(row)
}

/// A period of its own; a comparison with a release or a day sets the time itself, so choosing a
/// period goes back to the period before.
fn period_section(query: &Query, lang: Lang) -> Section {
    let split = matches!(query.compare, Compare::Release(_) | Compare::Date(_));
    let items = Period::ALL.iter().map(|&period| {
        let compare = match (&query.compare, period) {
            (Compare::Release(_) | Compare::Date(_) | Compare::Previous, Period::All) => Compare::None,
            (Compare::Release(_) | Compare::Date(_), _) => Compare::Previous,
            (compare, _) => compare.clone(),
        };
        let target = Query { period, compare, ..query.clone() };
        choice(&format!("period:{}", period.as_str()), period_label(period, lang).into(), target, query, !split && period == query.period, lang)
    }).collect();
    common::section("period", Some(t!(lang, "Period", "Periodo").into()), items)
}

fn compare_section(query: &Query, report: &MetricsReport, lang: Lang) -> Section {
    let mut options = vec![];
    if query.period != Period::All {
        options.push(Compare::Previous);
    }
    options.extend(report.releases.iter().map(|release| Compare::Release(release.version.clone())));
    if let Compare::Date(_) = &query.compare {
        options.push(query.compare.clone());
    }
    options.push(Compare::None);
    let items = options.into_iter().map(|compare| {
        let id = format!("compare:{}", compare.as_text());
        let current = compare == query.compare;
        let target = Query { compare: compare.clone(), ..query.clone() };
        choice(&id, compare_label(&compare, lang), target, query, current, lang)
    }).collect();
    common::section("compare", Some(t!(lang, "Compare with", "Comparar con").into()), items)
}

fn filter_section(query: &Query, lang: Lang) -> Option<Section> {
    let mut items = vec![];
    if let Some(project) = &query.project {
        let everything = Query { project: None, ..query.clone() };
        items.push(Item::Row(common::row("filter:project", &everything.key(), Icon::Project, tf!(lang, "Only {project}", "Solo {project}"),
            Some(t!(lang, "Show every project", "Mostrar todos los proyectos").into()))));
    }
    if let Some(model) = &query.model {
        let everything = Query { model: None, effort: None, ..query.clone() };
        let name = match query.effort.as_deref() {
            Some(effort) => format!("{model} · {}", common::effort_label(effort, lang).unwrap_or(effort)),
            None => model.clone(),
        };
        items.push(Item::Row(common::row("filter:model", &everything.key(), Icon::Node, tf!(lang, "Only {name}", "Solo {name}"),
            Some(t!(lang, "Show every model", "Mostrar todos los modelos").into()))));
    }
    (!items.is_empty()).then(|| common::section("filter", Some(t!(lang, "Filter", "Filtro").into()), items))
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
