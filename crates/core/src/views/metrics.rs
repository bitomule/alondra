//! Metrics: what ended tasks and sessions cost and how the tasks went, for a period, beside the
//! period before or before a release. `alondra metrics` prints the same report with these words.
use super::{
    BuildError, Fact, Facts, Icon, Item, Label, Lang, Notice, RenderContext, Section, Tone, View, common,
    text::{t, tf},
};
use crate::metrics::report::{Compare, Group, Metric, MetricsInputs, MetricsReport, Period, Query, Unit, Value, report};

pub use crate::metrics::report::KEY;

const SUMMARY: &[Metric] = &[Metric::Tasks, Metric::Clean, Metric::ChangeRounds, Metric::Interventions, Metric::Reopened,
    Metric::Relaunched, Metric::Cancelled];
const COST: &[Metric] = &[Metric::CostTotal, Metric::CostPerTask, Metric::MedianCostPerTask, Metric::CostPerPr,
    Metric::CostPerLine, Metric::CostPerSession];
const TOKENS_AND_STEPS: &[Metric] = &[Metric::InputTokens, Metric::OutputTokens, Metric::CacheReadTokens,
    Metric::CacheWriteTokens, Metric::StepsPerTask, Metric::MedianSteps, Metric::IdleSteps];
const TIME: &[Metric] = &[Metric::TaskTime, Metric::Working, Metric::WaitingForYou, Metric::Queued, Metric::Blocked];

/// The figures as the screen groups them, each group with its id and title.
pub fn sections(lang: Lang) -> [(&'static str, &'static str, &'static [Metric]); 4] {
    [
        ("summary", t!(lang, "Summary", "Resumen"), SUMMARY),
        ("cost", t!(lang, "Cost", "Coste"), COST),
        ("tokens_and_steps", t!(lang, "Tokens and steps", "Tokens y pasos"), TOKENS_AND_STEPS),
        ("time", t!(lang, "Time", "Tiempo"), TIME),
    ]
}

pub fn label(metric: Metric, lang: Lang) -> &'static str {
    match metric {
        Metric::Tasks => t!(lang, "Tasks ended", "Tareas terminadas"),
        Metric::Clean => t!(lang, "Done with no rework", "Sin retrabajo"),
        Metric::ChangeRounds => t!(lang, "Change rounds per task", "Rondas de cambios por tarea"),
        Metric::Interventions => t!(lang, "Your interventions per task", "Intervenciones tuyas por tarea"),
        Metric::Reopened => t!(lang, "Reopened", "Reabiertas"),
        Metric::Relaunched => t!(lang, "Relaunched", "Relanzadas"),
        Metric::Cancelled => t!(lang, "Cancelled", "Canceladas"),
        Metric::CostTotal => t!(lang, "Total", "Total"),
        Metric::CostPerTask => t!(lang, "Per task", "Por tarea"),
        Metric::MedianCostPerTask => t!(lang, "Median per task", "Mediana por tarea"),
        Metric::CostPerPr => t!(lang, "Per merged PR", "Por PR fusionado"),
        Metric::CostPerLine => t!(lang, "Per changed line", "Por línea cambiada"),
        Metric::CostPerSession => t!(lang, "Per session", "Por sesión"),
        Metric::InputTokens => t!(lang, "Input", "Entrada"),
        Metric::OutputTokens => t!(lang, "Output", "Salida"),
        Metric::CacheReadTokens => t!(lang, "Cache read", "Caché leída"),
        Metric::CacheWriteTokens => t!(lang, "Cache written", "Caché escrita"),
        Metric::StepsPerTask => t!(lang, "Steps per task", "Pasos por tarea"),
        Metric::MedianSteps => t!(lang, "Median steps per task", "Mediana de pasos por tarea"),
        Metric::IdleSteps => t!(lang, "Steps that only look or wait", "Pasos que solo miran o esperan"),
        Metric::TaskTime => t!(lang, "Time per task (median)", "Tiempo por tarea (mediana)"),
        Metric::Working => t!(lang, "Working, per task", "Trabajando, por tarea"),
        Metric::WaitingForYou => t!(lang, "Waiting for you, per task", "Esperando por ti, por tarea"),
        Metric::Queued => t!(lang, "Queued, per task", "En cola, por tarea"),
        Metric::Blocked => t!(lang, "Blocked, per task", "Bloqueada, por tarea"),
    }
}

fn decimals(value: f64, places: usize, lang: Lang) -> String {
    let text = format!("{value:.places$}");
    match lang {
        Lang::En => text,
        Lang::Es => text.replace('.', ","),
    }
}

/// A value in its unit, as both the screen and the CLI write it.
pub fn shown(unit: Unit, value: f64, lang: Lang) -> String {
    match unit {
        Unit::Count => format!("{value:.0}"),
        Unit::Rate => decimals(value, 1, lang),
        Unit::Percent => match lang {
            Lang::En => format!("{value:.0}%"),
            Lang::Es => format!("{value:.0} %"),
        },
        Unit::Micros => {
            let dollars = value / 1_000_000.0;
            let places = if dollars >= 100.0 { 0 } else if dollars >= 0.1 || dollars == 0.0 { 2 } else { 4 };
            match lang {
                Lang::En => format!("${}", decimals(dollars, places, lang)),
                Lang::Es => format!("{} $", decimals(dollars, places, lang)),
            }
        }
        Unit::Tokens => match value {
            v if v < 1_000.0 => format!("{v:.0}"),
            v if v < 1_000_000.0 => format!("{:.0}k", v / 1_000.0),
            v if v < 1_000_000_000.0 => format!("{} M", decimals(v / 1_000_000.0, 1, lang)),
            v => match lang {
                Lang::En => format!("{} B", decimals(v / 1_000_000_000.0, 2, lang)),
                Lang::Es => format!("{} mil M", decimals(v / 1_000_000_000.0, 2, lang)),
            },
        },
        Unit::Millis => {
            let minutes = value / 60_000.0;
            if minutes < 90.0 {
                format!("{minutes:.0} min")
            } else if minutes < 48.0 * 60.0 {
                format!("{} h", decimals(minutes / 60.0, 1, lang))
            } else {
                format!("{} d", decimals(minutes / 1_440.0, 1, lang))
            }
        }
    }
}

/// «A · before B (±x %)», or only A when nothing is compared.
pub fn compared(value: &Value, lang: Lang) -> String {
    let unit = value.metric.unit();
    let now = value.value.map_or_else(|| "—".to_owned(), |v| shown(unit, v, lang));
    let Some(before) = value.previous else { return now };
    let before = shown(unit, before, lang);
    let mut text = tf!(lang, "{now} · before {before}", "{now} · antes {before}");
    if let Some(change) = value.change.filter(|change| change.abs() >= 0.5) {
        text.push_str(&match lang {
            Lang::En => format!(" ({change:+.0}%)"),
            Lang::Es => format!(" ({change:+.0} %)"),
        });
    }
    text
}

pub fn period_label(period: Period, lang: Lang) -> &'static str {
    match period {
        Period::Week => t!(lang, "Last 7 days", "Últimos 7 días"),
        Period::Month => t!(lang, "Last 30 days", "Últimos 30 días"),
        Period::Quarter => t!(lang, "Last 90 days", "Últimos 90 días"),
        Period::All => t!(lang, "All time", "Todo"),
    }
}

pub fn compare_label(compare: &Compare, lang: Lang) -> String {
    match compare {
        Compare::Previous => t!(lang, "The period before", "El periodo anterior").into(),
        Compare::Release(version) => tf!(lang, "Since version {version}, against as long before", "Desde la versión {version}, frente al mismo tiempo antes"),
        Compare::Date(day) => tf!(lang, "Since {day}, against as long before", "Desde el {day}, frente al mismo tiempo antes"),
        Compare::None => t!(lang, "Nothing", "Nada").into(),
    }
}

/// A model and its effort as a row names them.
pub fn group_name(group: &Group, lang: Lang) -> String {
    let name = if group.name.is_empty() { t!(lang, "Unknown", "Desconocido").to_owned() } else { group.name.clone() };
    match group.effort.as_deref() {
        Some(effort) => format!("{name} · {}", common::effort_label(effort, lang).unwrap_or(effort)),
        None => name,
    }
}

/// How many tasks a group has, how many went with no rework and what each cost.
pub fn group_summary(group: &Group, lang: Lang) -> String {
    let figures = &group.figures;
    let tasks = figures.sums.tasks;
    let mut parts = vec![match (tasks, lang) {
        (1, Lang::En) => "1 task".to_owned(),
        (1, Lang::Es) => "1 tarea".to_owned(),
        _ => tf!(lang, "{tasks} tasks", "{tasks} tareas"),
    }];
    if let Some(clean) = figures.value(Metric::Clean) {
        let clean = shown(Unit::Percent, clean, lang);
        parts.push(tf!(lang, "{clean} no rework", "{clean} sin retrabajo"));
    }
    if let Some(cost) = figures.value(Metric::CostPerTask) {
        let cost = shown(Unit::Micros, cost, lang);
        parts.push(tf!(lang, "{cost} per task", "{cost} por tarea"));
    }
    parts.join(" · ")
}

/// «Provider's estimate, not your bill · measured in N of M tasks».
pub fn estimate_notice(report: &MetricsReport, lang: Lang) -> String {
    let (measured, tasks) = (report.figures.sums.measured, report.figures.sums.tasks);
    match (tasks, lang) {
        (1, Lang::En) => format!("Provider's estimate, not your bill · measured in {measured} of 1 task"),
        (1, Lang::Es) => format!("Estimación del proveedor, no la factura · medido en {measured} de 1 tarea"),
        _ => tf!(lang, "Provider's estimate, not your bill · measured in {measured} of {tasks} tasks",
            "Estimación del proveedor, no la factura · medido en {measured} de {tasks} tareas"),
    }
}

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
