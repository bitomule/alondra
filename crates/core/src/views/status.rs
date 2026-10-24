use super::{
    Count, Label, Level, RenderContext, Status, Tone,
    text::{t, tf},
    version,
};
/// What waits for the user is the home queue, its count and the app icon badge. The inbox only
/// holds what is addressed to the user, the orchestrator's answers and questions included.
pub(super) fn build(context: &RenderContext<'_>) -> Status {
    let roots = context.waiting();
    let pending = roots.len() as u32;
    let questions = roots
        .iter()
        .filter(|item| super::home::waits_for(item) == "question")
        .count() as u32;
    let to_read = pending - questions;
    let working = context.domain.slots().len() as u32;
    let mut summary = Vec::new();
    if questions > 0 {
        summary.push(if questions == 1 {
            tf!(context.lang, "{questions} question", "{questions} pregunta")
        } else {
            tf!(
                context.lang,
                "{questions} questions",
                "{questions} preguntas"
            )
        });
    }
    if to_read > 0 {
        summary.push(tf!(context.lang, "{to_read} to read", "{to_read} por leer"));
    }
    summary.push(if working == 1 {
        tf!(
            context.lang,
            "{working} node working",
            "{working} nodo trabajando"
        )
    } else {
        tf!(
            context.lang,
            "{working} nodes working",
            "{working} nodos trabajando"
        )
    });
    let mut status = Status {
        version: String::new(),
        summary: summary.join(" · "),
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
                value: context.domain.tasks.values().filter(|task| task.active()).count() as u32,
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
                value: context.domain.agents.values().filter(|agent| context.index.node(&agent.id).is_some_and(|node| {
                    node.waiting.iter().filter_map(|id| context.index.waiting_card(id)).any(|item| item.kind != "information")
                })).count() as u32,
            },
        ],
        gauges: context.live.gauges.clone(),
        orchestrator: {
            let (text, tone) = match super::common::orchestrator_state(context) {
                super::common::OrchestratorState::Working => (t!(context.lang, "working", "trabajando"), Tone::Info),
                super::common::OrchestratorState::Failed => (t!(context.lang, "failed", "falló"), Tone::Warning),
                super::common::OrchestratorState::Idle => (t!(context.lang, "idle", "inactivo"), Tone::Neutral),
            };
            Label { text: text.into(), tone }
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
            text: t!(context.lang, "Waiting for a Claude session · sign in to an account in Claude accounts",
                "Esperando una sesión de Claude · inicia sesión en una cuenta en Cuentas de Claude").into(),
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
