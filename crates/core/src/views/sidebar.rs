use super::{
    BuildError, Client, Icon, Item, Label, Notice, RenderContext, Tone, Trailing, View, common,
    text::{t, tf},
};
use crate::domain_v2::{Task, TaskState};
use std::collections::BTreeMap;

fn task_subtitle(task: &Task, context: &RenderContext<'_>) -> String {
    let project = task.agent_id.as_ref().and_then(|id| context.live.nodes.iter()
        .find(|node| node.agent_id == id.0).and_then(|node| node.project.as_deref()));
    let state = match task.state {
        TaskState::NotStarted => t!(context.lang, "Not started", "Sin empezar"),
        TaskState::Running => t!(context.lang, "Working", "Trabajando"),
        TaskState::NeedsYou => t!(context.lang, "Needs you", "Te necesita"),
        TaskState::InReview => t!(context.lang, "In review", "En revisión"),
        TaskState::WaitingDependency => t!(context.lang, "Waiting", "Esperando"),
        TaskState::Closing => t!(context.lang, "Closing", "Cerrando"),
        TaskState::Done => t!(context.lang, "Finished", "Terminado"),
        TaskState::Cancelled => t!(context.lang, "Cancelled", "Cancelado"),
    };
    match project {
        Some(project) if !project.is_empty() && project != "others" => format!("{project} · {state}"),
        _ => state.into(),
    }
}

fn task_groups(context: &RenderContext<'_>) -> super::Section {
    let states = [
        (TaskState::NeedsYou, "needs_you", t!(context.lang, "Needs you", "Te necesita")),
        (TaskState::InReview, "in_review", t!(context.lang, "In review", "En revisión")),
        (TaskState::Closing, "closing", t!(context.lang, "Closing", "Cerrando")),
        (TaskState::Running, "running", t!(context.lang, "Working", "Trabajando")),
        (TaskState::WaitingDependency, "waiting_dependency", t!(context.lang, "Waiting", "Esperando")),
        (TaskState::NotStarted, "not_started", t!(context.lang, "Not started", "Sin empezar")),
        (TaskState::Done, "done", t!(context.lang, "Finished", "Terminado")),
        (TaskState::Cancelled, "cancelled", t!(context.lang, "Cancelled", "Cancelado")),
    ];
    let mut rows = Vec::new();
    for (state, id, title) in states {
        let children: Vec<_> = context.domain.tasks.values()
            .filter(|task| !task.archived && task.state == state)
            .map(|task| {
                let mut row = common::row(&task.id.0, &format!("task:{}", task.id),
                    Icon::Task, task.title.clone(), Some(task_subtitle(task, context)));
                row.struck = state == TaskState::Cancelled;
                row.state_key = Some(id.into());
                row.depth = 1;
                row
            }).collect();
        if !children.is_empty() {
            rows.push(group_row(id, title, children));
        }
    }
    let defining: Vec<_> = context.domain.requests.values()
        .filter(|request| !request.closed && !context.domain.tasks.values().any(|task| task.request_id == request.id))
        .map(|request| {
            let title = request.title.clone().unwrap_or_else(|| request.body.lines().next().unwrap_or("").chars().take(80).collect());
            let mut row = common::row(&request.id.0, &format!("request:{}", request.id), Icon::Mission, title, None);
            row.depth = 1;
            row.state_key = Some("defining".into());
            row
        }).collect();
    if !defining.is_empty() {
        rows.insert(rows.len().min(6), group_row("defining", t!(context.lang, "Defining", "Definiendo"), defining));
    }
    let archived: Vec<_> = context.domain.tasks.values().filter(|task| task.archived)
        .map(|task| {
            let mut row = common::row(&task.id.0, &format!("task:{}", task.id), Icon::Task, task.title.clone(), None);
            row.depth = 1;
            row.state_key = Some("archived".into());
            row
        }).collect();
    if !archived.is_empty() {
        rows.push(group_row("archived", t!(context.lang, "Archived", "Archivadas"), archived));
    }
    let open_count = context.domain.tasks.values().filter(|task| !task.archived && !task.state.terminal()).count()
        + context.domain.requests.values().filter(|request| !request.closed && !context.domain.tasks.values().any(|task| task.request_id == request.id)).count();
    let mut section = common::section("tasks", Some(t!(context.lang, "Tasks", "Tareas").into()),
        std::iter::once(Item::Row(common::row("schedules", "schedules", Icon::Schedule,
            t!(context.lang, "Scheduled Tasks", "Tareas programadas").into(), None)))
            .chain(rows.into_iter().map(Item::Row)).collect());
    section.count = Some(open_count as u32);
    section
}

fn group_row(id: &str, title: &str, children: Vec<super::Row>) -> super::Row {
    let mut row = common::row(id, "tasks", Icon::Task, title.into(), None);
    row.link = None;
    row.state_key = Some(id.into());
    row.trailing = Some(Trailing::Count { value: children.len() as u32 });
    row.children = children;
    row
}

fn project_sections(context: &RenderContext<'_>) -> Vec<super::Section> {
    let mut projects: BTreeMap<String, Vec<super::Row>> = BTreeMap::new();
    for agent in context.domain.agents.values() {
        if agent.id.0 == "orchestrator" { continue; }
        let live = context.live.nodes.iter().find(|node| node.agent_id == agent.id.0);
        let project = live.and_then(|node| node.project.as_deref()).filter(|name| *name != "others")
            .unwrap_or(t!(context.lang, "Other", "Otros"));
        let mut row = common::row(&agent.id.0, &format!("node:{}", agent.id), Icon::Node,
            agent.name.clone(), None);
        if let Some(node) = live {
            row.state_key = node.state_key.clone();
            let header = format!("{} · {}", agent.name, node.state_label.as_deref().unwrap_or(""));
            let summary = node.summary.as_deref().unwrap_or("").lines().next().unwrap_or("");
            row.tooltip = Some(if summary.is_empty() { header } else {
                let excerpt: String = summary.chars().take(140).collect();
                format!("{header}\n{excerpt}{}", if summary.chars().count() > 140 { "…" } else { "" })
            });
            if let Some(text) = &node.background_badge {
                row.badges.push(Label { text: text.clone(), tone: if node.background_suspect { Tone::Warning } else { Tone::Neutral } });
            }
        }
        let pending = context.domain.projection().inbox.iter()
            .filter(|item| item.pending && item.agent_id.as_ref() == Some(&agent.id)).count();
        row.trailing = Some(Trailing::Count { value: pending as u32 });
        row.children = context.domain.tasks.values().filter(|task| task.agent_id.as_ref() == Some(&agent.id) && !task.archived)
            .map(|task| {
                let mut child = common::row(&task.id.0, &format!("task:{}", task.id), Icon::Task,
                    task.title.clone(), None);
                child.depth = 1;
                child.state_key = serde_json::to_value(task.state).ok().and_then(|value| value.as_str().map(str::to_owned));
                child
            }).collect();
        projects.entry(project.into()).or_default().push(row);
    }
    projects.into_iter().map(|(project, rows)| {
        let mut section = common::section(&format!("project:{project}"), Some(project), rows.into_iter().map(Item::Row).collect());
        section.count = Some(section.items.len() as u32);
        section
    }).collect()
}

pub(super) fn build(context: &RenderContext<'_>) -> Result<View, BuildError> {
    if context.client != Client::Mac {
        return Err(BuildError::UnsupportedClient);
    }
    let projection = context.domain.projection();
    let mut view = common::view("sidebar", "sidebar", None, context, vec![]);
    let pending: Vec<_> = projection
        .inbox
        .iter()
        .filter(|item| item.pending && !super::home::orchestrator_only(item, context))
        .collect();
    for (id, title, pr) in [
        (
            "waiting",
            t!(context.lang, "Waiting for You", "Te espera"),
            false,
        ),
        (
            "pull_requests",
            t!(context.lang, "Pull requests", "Solicitudes de cambios"),
            true,
        ),
    ] {
        let cards: Vec<_> = pending
            .iter()
            .filter(|item| (item.kind == "pull_request") == pr)
            .collect();
        let mut section = common::section(
            id,
            Some(title.into()),
            cards
                .iter()
                .map(|item| {
                    let card = super::card::from_item(item, context, false)?;
                    Ok(Item::Row(common::row(
                        &card.id,
                        &format!("card:{}", card.id),
                        card.icon,
                        card.title,
                        card.subtitle,
                    )))
                })
                .collect::<Result<Vec<_>, BuildError>>()?,
        );
        section.count = Some(cards.len() as u32);
        view.sections.push(section);
    }
    view.sections.push(task_groups(context));
    let fleet = super::fleet::list(context);
    view.sections.extend(fleet.sections.into_iter().filter(|section| section.id == "orchestrator"));
    view.sections.extend(project_sections(context));
    let (message, action) = if pending.is_empty() {
        (
            t!(context.lang, "All caught up", "Todo al día").to_owned(),
            common::view_link(
                "new_request",
                t!(context.lang, "Ask for Something New…", "Pide algo nuevo…"),
            ),
        )
    } else {
        (
            tf!(
                context.lang,
                "{count} things waiting",
                "{count} cosas pendientes",
                count = pending.len()
            ),
            common::view_link(
                &format!("card:{}", super::card::public_id(pending[0])),
                t!(context.lang, "Start", "Empezar"),
            ),
        )
    };
    view.sections.push(common::section(
        "summary",
        None,
        vec![Item::Notice(Notice {
            text: message,
            tone: Tone::Neutral,
            action: Some(action),
        })],
    ));
    view.status.summary = tf!(
        context.lang,
        "{pending_count} pending · {node_count} nodes",
        "{pending_count} pendientes · {node_count} nodos",
        pending_count = pending.len(),
        node_count = context.domain.agents.len()
    );
    view.status.version = super::version(&view.status);
    Ok(view)
}
