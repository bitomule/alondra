use super::{
    BuildError, Client, Icon, Item, Label, Notice, RenderContext, Tone, Trailing, View, common,
    text::{t, tf},
};
use crate::domain_v2::{Task, TaskState};
use std::collections::BTreeMap;

fn task_subtitle(task: &Task, context: &RenderContext<'_>) -> String {
    let project = task.agent_id.as_ref().and_then(|id| context.live.nodes.iter()
        .find(|node| node.agent_id == id.0).and_then(|node| node.project.as_deref()));
    let state = common::instruction_label(task, context.domain, context.lang)
        .or_else(|| common::failed_launch(task, context.domain).map(|_| common::launch_failed_label(context.lang)))
        .unwrap_or_else(|| task_state_label(task.state, context.lang));
    match project {
        Some(project) if !project.is_empty() && project != "others" => format!("{project} · {state}"),
        _ => state.into(),
    }
}

fn task_state_label(state: TaskState, lang: super::Lang) -> &'static str {
    match state {
        TaskState::NotStarted => t!(lang, "Not started", "Sin empezar"),
        TaskState::Running => t!(lang, "Working", "Trabajando"),
        TaskState::NeedsYou => t!(lang, "Needs you", "Te necesita"),
        TaskState::InReview => t!(lang, "In review", "En revisión"),
        TaskState::WaitingDependency => t!(lang, "Waiting", "Esperando"),
        TaskState::Closing => t!(lang, "Closing", "Cerrando"),
        TaskState::Done => t!(lang, "Finished", "Terminado"),
        TaskState::Cancelled => t!(lang, "Cancelled", "Cancelado"),
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
                let (icon, tone) = common::task_presentation(state);
                let mut row = common::row(&task.id.0, &format!("task:{}", task.id),
                    icon, task.title.clone(), Some(task_subtitle(task, context)));
                row.struck = state == TaskState::Cancelled;
                row.badges.push(Label { text: title.into(), tone });
                row.depth = 1;
                row
            }).collect();
        if !children.is_empty() {
            rows.push(group_row(id, title, children));
        }
    }
    let defining: Vec<_> = context.domain.requests.values()
        .filter(|request| context.domain.is_defining(request))
        .map(|request| {
            let title = request.title.clone().unwrap_or_else(|| request.body.lines().next().unwrap_or("").chars().take(80).collect());
            let mut row = common::row(&request.id.0, &format!("request:{}", request.id), Icon::Mission, title, None);
            row.depth = 1;
            row.badges.push(Label { text: t!(context.lang, "Defining", "Definiendo").into(), tone: common::state_tone("defining") });
            row
        }).collect();
    if !defining.is_empty() {
        rows.insert(rows.len().min(6), group_row("defining", t!(context.lang, "Defining", "Definiendo"), defining));
    }
    let archived: Vec<_> = context.domain.tasks.values().filter(|task| task.archived)
        .map(|task| {
            let mut row = common::row(&task.id.0, &format!("task:{}", task.id), Icon::Task, task.title.clone(), None);
            row.depth = 1;
            row.badges.push(Label { text: t!(context.lang, "Archived", "Archivada").into(), tone: common::state_tone("archived") });
            row
        }).collect();
    if !archived.is_empty() {
        rows.push(group_row("archived", t!(context.lang, "Archived", "Archivadas"), archived));
    }
    let open_count = context.domain.tasks.values().filter(|task| !task.archived && !task.state.terminal()).count()
        + context.domain.requests.values().filter(|request| context.domain.is_defining(request)).count();
    let mut section = common::section("tasks", Some(t!(context.lang, "Tasks", "Tareas").into()),
        std::iter::once(Item::Row(common::row("schedules", "schedules", Icon::Schedule,
            t!(context.lang, "Scheduled Tasks", "Tareas programadas").into(), None)))
            .chain(rows.into_iter().map(Item::Row)).collect());
    section.count = Some(open_count as u32);
    section
}

fn group_row(id: &str, title: &str, children: Vec<super::Row>) -> super::Row {
    let state = match id {
        "needs_you" => "needs_you", "in_review" => "in_review", "closing" => "closing",
        "running" => "running", "waiting_dependency" => "waiting_dependency",
        "not_started" => "not_started", "done" => "done", "cancelled" => "cancelled",
        "archived" => "archived", _ => "defining",
    };
    let icon = match state {
        "needs_you" => super::Icon::Action,
        "in_review" => super::Icon::PullRequest,
        "closing" | "done" => super::Icon::Check,
        "running" => super::Icon::Mission,
        "waiting_dependency" => super::Icon::Branch,
        "cancelled" => super::Icon::Warning,
        _ => super::Icon::Task,
    };
    let mut row = common::row(id, "tasks", icon, title.into(), None);
    row.link = None;
    row.badges.push(Label { text: title.into(), tone: common::state_tone(state) });
    row.trailing = Some(Trailing::Count { value: children.len() as u32 });
    row.children = children;
    row
}

fn project_sections(context: &RenderContext<'_>) -> Vec<super::Section> {
    let mut projects: BTreeMap<String, Vec<super::Row>> = BTreeMap::new();
    for agent in context.domain.agents.values() {
        if !super::fleet::in_fleet(agent, context) { continue; }
        let live = context.live.nodes.iter().find(|node| node.agent_id == agent.id.0);
        let project = live.and_then(|node| node.project.as_deref()).filter(|name| *name != "others")
            .unwrap_or(t!(context.lang, "Other", "Otros"));
        let mut row = common::row(&agent.id.0, &format!("node:{}", agent.id), Icon::Node,
            common::agent_label(&agent.id, context), None);
        row.badges.push(super::fleet::node_state(&agent.id, context));
        if let Some(node) = live {
            let summary = node.summary.as_deref().unwrap_or("").lines().next().unwrap_or("");
            row.subtitle = (!summary.is_empty()).then(|| summary.chars().take(140).collect::<String>());
            if let Some(text) = &node.background_badge {
                row.badges.push(Label { text: text.clone(), tone: if node.background_suspect { Tone::Warning } else { Tone::Neutral } });
            }
        }
        let pending = context.domain.projection().inbox.iter()
            .filter(|item| item.pending && item.agent_id.as_ref() == Some(&agent.id)).count();
        row.trailing = Some(Trailing::Count { value: pending as u32 });
        row.children = common::current_task(context.domain, &agent.id).into_iter()
            .map(|task| {
                let (icon, tone) = common::task_presentation(task.state);
                let mut child = common::row(&task.id.0, &format!("task:{}", task.id), icon,
                    task.title.clone(), Some(task_subtitle(task, context)));
                child.depth = 1;
                child.badges.push(Label { text: common::instruction_label(task, context.domain, context.lang)
                    .unwrap_or_else(|| task_state_label(task.state, context.lang)).into(), tone });
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
    let pending = super::home::waiting(&projection);
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
            .filter(|item| super::card::carries_pull_request(item, context.domain) == pr)
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
        section.count = Some(section.items.len() as u32);
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
