use super::{
    Count, Label, Level, RenderContext, Status, SummaryPart, Tone,
    text::t,
    version,
};
/// What waits for the user is the home queue, its count and the app icon badge. The inbox only
/// holds what is addressed to the user, the orchestrator's answers and questions included.
pub(super) fn build(context: &RenderContext<'_>) -> Status {
    let roots = context.waiting();
    let activity = super::activity_counts(context.index);
    let pending = activity.inbox;
    let questions = roots
        .iter()
        .filter(|item| super::home::waits_for(item) == "question")
        .count() as u32;
    let to_read = pending - questions;
    let working = activity.working;
    let part = |id: &str, count: u32, one: &str, other: &str| SummaryPart { id: id.into(), count, one: one.into(), other: other.into() };
    let mut parts = Vec::new();
    if questions > 0 {
        parts.push(part("questions", questions,
            t!(context.lang, "{count} question", "{count} pregunta"),
            t!(context.lang, "{count} questions", "{count} preguntas")));
    }
    if to_read > 0 {
        parts.push(part("to_read", to_read,
            t!(context.lang, "{count} to read", "{count} por leer"),
            t!(context.lang, "{count} to read", "{count} por leer")));
    }
    parts.push(part("working", working,
        t!(context.lang, "{count} task working", "{count} tarea trabajando"),
        t!(context.lang, "{count} tasks working", "{count} tareas trabajando")));
    let summary = parts.iter().map(SummaryPart::text).collect::<Vec<_>>().join(" · ");
    let mut status = Status {
        version: String::new(),
        summary,
        summary_parts: parts,
        badge: pending,
        counts: vec![
            Count {
                id: "inbox".into(),
                label: t!(context.lang, "Inbox", "Bandeja").into(),
                value: pending,
            },
            Count {
                id: "tasks".into(),
                label: t!(context.lang, "Tasks", "Tareas").into(),
                value: super::tasks::active_count(context.domain),
            },
            Count {
                id: "agents".into(),
                label: t!(context.lang, "In progress", "En marcha").into(),
                value: super::tasks::in_progress(context).count() as u32,
            },
            Count {
                id: "working".into(),
                label: t!(context.lang, "working", "trabajando").into(),
                value: working,
            },
            Count {
                id: "waiting".into(),
                label: t!(context.lang, "waiting", "esperando").into(),
                value: activity.waiting,
            },
        ],
        gauges: context.live.gauges.clone(),
        orchestrator: {
            let (text, tone) = match super::common::orchestrator_state(context) {
                super::common::OrchestratorState::Working => (t!(context.lang, "working", "trabajando"), Tone::Info),
                super::common::OrchestratorState::Failed => (t!(context.lang, "failed", "falló"), Tone::Warning),
                super::common::OrchestratorState::Idle => (t!(context.lang, "idle", "inactivo"), Tone::Neutral),
            };
            Label { text: text.into(), tone, elapsed: None }
        },
        update: context.live.update.clone(),
        usage_exhausted: context.live.usage_exhausted.as_ref().map(|exhausted| super::UsageExhausted {
            scope: "this_mac".into(),
            until: exhausted.until.clone(),
            macs_exhausted: 1,
            macs_total: 1,
            paused_by_user: exhausted.paused_by_user,
        }),
        session_wait: context.live.waits_for_session.then(|| super::Notice {
            text: t!(context.lang, "No provider account is available · check Accounts",
                "No hay ninguna cuenta de proveedor disponible · revisa Cuentas").into(),
            tone: Tone::Warning,
            action: None,
        }),
    };
    for gauge in &mut status.gauges {
        if !gauge.value.is_finite() {
            gauge.value = 0.0;
            gauge.level = Level::Calm;
        }
    }
    status.version = version(&status);
    status
}
