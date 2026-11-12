use super::{
    BuildError, Client, Icon, Item, Label, Notice, RenderContext, Tone, Trailing, View, common,
    text::{t, tf},
};
use super::task_state::StateLabel;
use crate::domain::Task;
use std::collections::BTreeMap;

fn task_subtitle(task: &Task, fact: Option<&str>) -> Option<String> {
    match (task.project.as_deref(), fact) {
        (Some(project), Some(fact)) => Some(format!("{project} · {fact}")),
        (Some(text), None) | (None, Some(text)) => Some(text.into()),
        (None, None) => None,
    }
}

fn task_groups(context: &RenderContext<'_>) -> super::Section {
    let presented: Vec<_> = context.domain.tasks.values().filter(|task| !task.archived)
        .map(|task| (task, super::task_state::present(task, context))).collect();
    let mut rows = Vec::new();
    for label in StateLabel::ALL {
        let children: Vec<_> = presented.iter().filter(|(_, presentation)| presentation.label == label)
            .map(|(task, presentation)| {
                let mut row = common::row(&task.id.0, &format!("task:{}", task.id), presentation.icon, task.title.clone(),
                    task_subtitle(task, presentation.fact.as_deref()));
                row.struck = label == StateLabel::Cancelled;
                row.badges.push(Label { text: presentation.state.clone(), tone: presentation.tone, elapsed: None });
                row.badges.extend(common::scheduled_mark(task, context));
                row.depth = 1;
                row
            }).collect();
        if !children.is_empty() {
            rows.push(group_row(label, children, context.lang));
        }
    }
    let defining: Vec<_> = context.domain.requests.values()
        .filter(|request| context.is_defining(request))
        .map(|request| {
            let title = request.title.clone().unwrap_or_else(|| request.body.lines().next().unwrap_or("").chars().take(80).collect());
            let mut row = common::row(&request.id.0, &format!("request:{}", request.id), Icon::Mission, title, None);
            row.depth = 1;
            row.badges.push(Label { text: t!(context.lang, "Defining", "Definiendo").into(), tone: common::state_tone("defining"), elapsed: None });
            row
        }).collect();
    let defining_count = defining.len();
    if !defining.is_empty() {
        rows.insert(rows.len().min(6), defining_group(defining, context.lang));
    }
    let open_count = context.domain.tasks.values().filter(|task| !task.archived && !task.state.terminal()).count()
        + defining_count;
    let mut section = common::section("tasks", Some(t!(context.lang, "Tasks", "Tareas").into()),
        std::iter::once(Item::Row(common::row("schedules", "schedules", Icon::Schedule,
            t!(context.lang, "Scheduled Tasks", "Tareas programadas").into(), None)))
            .chain(rows.into_iter().map(Item::Row)).collect());
    section.count = Some(open_count as u32);
    section
}

fn group_row(label: StateLabel, children: Vec<super::Row>, lang: super::Lang) -> super::Row {
    let icon = match label {
        StateLabel::NeedsYou => super::Icon::Action,
        StateLabel::Delivered => super::Icon::PullRequest,
        StateLabel::Finished => super::Icon::Check,
        StateLabel::Working => super::Icon::Mission,
        StateLabel::Waiting => super::Icon::Branch,
        StateLabel::Cancelled => super::Icon::Warning,
        StateLabel::Paused => super::Icon::Task,
    };
    let title = label.text(lang);
    let mut row = common::row(label.key(), "tasks", icon, title.into(), None);
    row.link = None;
    row.badges.push(Label { text: title.into(), tone: common::state_tone(label.key()), elapsed: None });
    row.trailing = Some(Trailing::Count { value: children.len() as u32 });
    row.children = children;
    row
}

fn defining_group(children: Vec<super::Row>, lang: super::Lang) -> super::Row {
    let title = t!(lang, "Defining", "Definiendo");
    let mut row = common::row("defining", "tasks", super::Icon::Task, title.into(), None);
    row.link = None;
    row.badges.push(Label { text: title.into(), tone: common::state_tone("defining"), elapsed: None });
    row.trailing = Some(Trailing::Count { value: children.len() as u32 });
    row.children = children;
    row
}

fn project_sections(context: &RenderContext<'_>) -> Vec<super::Section> {
    let mut projects: BTreeMap<String, Vec<super::Row>> = BTreeMap::new();
    for (task, agent) in super::tasks::in_progress(context) {
        let live = context.live.nodes.iter().find(|node| node.agent_id == agent.id.0);
        let project = task.project.as_deref().unwrap_or(t!(context.lang, "Other", "Otros"));
        let (icon, _) = common::task_presentation(task.state);
        let presented = super::task_state::present(task, context);
        let mut row = common::row(&task.id.0, &format!("task:{}", task.id), icon, task.title.clone(), None);
        row.badges.push(Label { text: presented.state, tone: presented.tone, elapsed: None });
        if let Some(node) = live {
            let summary = node.summary.as_deref().unwrap_or("").lines().next().unwrap_or("");
            row.subtitle = (!summary.is_empty()).then(|| summary.chars().take(140).collect::<String>());
            if let Some(text) = &node.background_badge {
                row.badges.push(Label { text: text.clone(), tone: Tone::Neutral, elapsed: None });
            }
        }
        let pending = context.index.node(&agent.id).map_or(0, |node| node.waiting.len());
        row.trailing = Some(Trailing::Count { value: pending as u32 });
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
    let mut view = common::view("sidebar", "sidebar", None, context, vec![]);
    let pending: Vec<_> = context.waiting().iter().collect();
    let mut waiting = common::section(
        "waiting",
        Some(t!(context.lang, "Waiting for You", "Te espera").into()),
        pending
            .iter()
            .map(|item| {
                let card = super::card::from_item(item, context, false)?;
                let mut row = common::row(
                    &card.id,
                    &format!("card:{}", card.id),
                    card.icon,
                    card.title,
                    card.meta.or(card.subtitle),
                );
                row.badges.extend(card.eyebrow);
                row.kind = card.kind;
                Ok(Item::Row(row))
            })
            .collect::<Result<Vec<_>, BuildError>>()?,
    );
    waiting.count = Some(waiting.items.len() as u32);
    view.sections.push(waiting);
    view.first_cards = super::card::first_cards(pending.iter().map(|item| super::card::public_id(item)), context)?;
    view.sections.push(task_groups(context));
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
        "{pending_count} pending · {running_count} in progress",
        "{pending_count} pendientes · {running_count} en marcha",
        pending_count = pending.len(),
        running_count = super::tasks::in_progress(context).count()
    );
    view.status.version = super::version(&view.status);
    Ok(view)
}
