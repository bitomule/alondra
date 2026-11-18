//! The words of the metrics report: the screen and `alondra metrics` print the same ones.
use crate::text::{t, tf, Lang};
use alondra_core::metrics::report::{Compare, Group, Metric, MetricsReport, Period, Unit, Value};

pub use alondra_core::metrics::report::KEY;

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
        Some(effort) => format!("{name} · {}", crate::text::effort_label(effort, lang).unwrap_or(effort)),
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

