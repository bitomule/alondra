use super::{
    Count, Label, Level, RenderContext, Status, Tone,
    text::{t, tf},
    version,
};
use crate::domain_v2::DomainProjection;

pub(super) fn build(projection: &DomainProjection, context: &RenderContext<'_>) -> Status {
    let roots: Vec<_> = projection
        .inbox
        .iter()
        .filter(|item| item.pending && !super::home::orchestrator_only(item, context))
        .collect();
    let pending = roots.len() as u32;
    let questions = roots
        .iter()
        .filter(|item| matches!(item.kind.as_str(), "question" | "action_request"))
        .count() as u32;
    let to_read = pending - questions;
    let working = context
        .live
        .nodes
        .iter()
        .filter(|node| node.working)
        .count() as u32;
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
                value: projection.tasks.len() as u32,
            },
            Count {
                id: "agents".into(),
                label: t!(context.lang, "Fleet", "Flota").into(),
                value: context.live.nodes.len() as u32,
            },
            Count {
                id: "working".into(),
                label: t!(context.lang, "working", "trabajando").into(),
                value: working,
            },
            Count {
                id: "waiting".into(),
                label: t!(context.lang, "waiting", "esperando").into(),
                value: context.live.nodes.iter().filter(|node| node.waiting).count() as u32,
            },
        ],
        gauges: context.live.gauges.clone(),
        orchestrator: Label {
            text: if context
                .live
                .orchestrator
                .as_ref()
                .is_some_and(|o| o.working)
            {
                t!(context.lang, "working", "trabajando")
            } else {
                t!(context.lang, "idle", "inactivo")
            }
            .into(),
            tone: if context
                .live
                .orchestrator
                .as_ref()
                .is_some_and(|o| o.working)
            {
                Tone::Info
            } else {
                Tone::Neutral
            },
        },
        update: context.live.update.clone(),
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
