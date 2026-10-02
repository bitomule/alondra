use super::{
    BuildError, Confirm, Fact, Facts, Icon, Item, Link, LocalLink, Markdown, RenderContext, Style, Tone,
    Trailing, View, action_id::Verb, common, text::t,
};
use crate::domain_v2::{AgentId, TaskState};
use std::collections::BTreeMap;

fn close_action(id: &str, context: &RenderContext<'_>) -> Option<super::Action> {
    let mut action = common::action(
        Verb::Close,
        id,
        context.domain.revision,
        t!(context.lang, "Close Node", "Cerrar nodo"),
        Style::Destructive,
    )?;
    action.confirm = Some(Confirm {
        title: action.label.clone(),
        message: t!(context.lang, "Close this node?", "¿Cerrar este nodo?").into(),
        button: action.label.clone(),
    });
    Some(action)
}

/// A node's one state, in 8ccd131's words: a question it blocks on, then what the session does,
/// then what waits for the user on it, then where its tasks stand.
pub(super) fn node_state(agent: &AgentId, context: &RenderContext<'_>) -> super::Label {
    let lang = context.lang;
    let live = context.live.nodes.iter().find(|node| node.agent_id == agent.0);
    let key = live.and_then(|node| node.state_key.as_deref());
    let inbox = context.domain.projection().inbox;
    let waiting = |kinds: &[&str]| inbox.iter()
        .any(|item| item.pending && item.agent_id.as_ref() == Some(agent) && kinds.contains(&item.kind.as_str()));
    let tasks: Vec<TaskState> = context.domain.tasks.values()
        .filter(|task| task.agent_id.as_ref() == Some(agent) && !task.archived)
        .map(|task| task.state)
        .collect();
    let (text, tone) = if waiting(&["question", "permission", "plan"]) {
        (t!(lang, "Waiting for an answer", "Esperando respuesta"), Tone::Warning)
    } else if key == Some("stalled") {
        (t!(lang, "Stalled", "Parado"), Tone::Critical)
    } else if live.is_some_and(|node| node.working) {
        (t!(lang, "Working", "Trabajando"), Tone::Info)
    } else if key == Some("waiting_machine") {
        (t!(lang, "Waiting for the machine", "Esperando máquina"), Tone::Neutral)
    } else if waiting(&["action_request"]) || tasks.contains(&TaskState::NeedsYou) {
        (t!(lang, "Your turn", "Te toca"), Tone::YourTurn)
    } else if waiting(&["pull_request", "document_review"]) || tasks.contains(&TaskState::InReview) {
        (t!(lang, "Waiting for your review", "Espera tu revisión"), Tone::Accent)
    } else if waiting(&["delivery"]) {
        (t!(lang, "Delivering results", "Entrega resultado"), Tone::Success)
    } else if !tasks.is_empty() && tasks.iter().all(|state| matches!(state, TaskState::Done | TaskState::Closing | TaskState::Cancelled)) {
        (t!(lang, "Done", "Terminado"), Tone::Success)
    } else {
        (t!(lang, "Unclear what it wants", "No sé qué quiere"), Tone::Unclear)
    };
    super::Label { text: text.into(), tone }
}

pub(super) fn node_row(agent: &crate::domain_v2::Agent, context: &RenderContext<'_>) -> super::Row {
    let live = context
        .live
        .nodes
        .iter()
        .find(|node| node.agent_id == agent.id.0);
    let mut row = common::row(
        &agent.id.0,
        &format!("node:{}", agent.id),
        Icon::Node,
        common::agent_label(&agent.id, context),
        None,
    );
    row.badges.push(node_state(&agent.id, context));
    row.children = common::current_task(context.domain, &agent.id)
        .map(|task| super::tasks::task_row(task, context))
        .into_iter()
        .collect();
    if context.client == super::Client::Ios
        && let Some(node) = live
        && let Some(message) = node.last_message.as_ref().filter(|text| !text.trim().starts_with("mcp__") && !text.trim().is_empty())
    {
        let mut last = common::row("last_message", "node", Icon::Notice,
            message.trim().lines().next().unwrap_or("").into(), None);
        last.link = None;
        last.trailing = node.last_message_at.as_ref().map(|at| Trailing::Time {
            at: at.clone(), format: super::TimeFormat::Relative,
        });
        row.children.push(last);
    }
    row.actions.extend(close_action(&agent.id.0, context));
    row.trailing = Some(Trailing::Count {
        value: context
            .domain
            .projection()
            .inbox
            .iter()
            .filter(|item| item.pending && item.agent_id.as_ref() == Some(&agent.id))
            .count() as u32,
    });
    if let Some(live) = live
        && let Some(text) = &live.background_badge
    {
        row.badges.push(super::Label { text: text.clone(), tone: if live.background_suspect { Tone::Warning } else { Tone::Neutral } });
    }
    row
}

pub(super) fn list(context: &RenderContext<'_>) -> View {
    let mut view = common::view(
        "fleet",
        "fleet",
        Some(t!(context.lang, "Fleet", "Flota").into()),
        context,
        vec![],
    );
    let mut orchestrator = common::row(
        "orchestrator",
        "node:orchestrator",
        Icon::Orchestrator,
        t!(context.lang, "Orchestrator", "Orquestador").into(),
        context
            .live
            .orchestrator
            .as_ref()
            .and_then(|o| o.summary.clone()),
    );
    orchestrator.trailing = Some(Trailing::Text {
        text: if context
            .live
            .orchestrator
            .as_ref()
            .is_some_and(|o| o.working)
        {
            t!(context.lang, "Working", "Trabajando")
        } else {
            t!(context.lang, "Idle", "Inactivo")
        }
        .into(),
    });
    let orchestrator_state = if context.live.orchestrator.as_ref().is_some_and(|o| o.working) {
        t!(context.lang, "Working", "Trabajando")
    } else { t!(context.lang, "Idle", "Inactivo") };
    orchestrator.badges.push(super::Label {
        text: orchestrator_state.into(),
        tone: common::state_tone(if context.live.orchestrator.as_ref().is_some_and(|o| o.working) { "working" } else { "idle" }),
    });
    view.sections.push(common::section(
        "orchestrator",
        None,
        vec![Item::Row(orchestrator)],
    ));
    let mut projects: BTreeMap<String, Vec<Item>> = BTreeMap::new();
    for agent in context.domain.agents.values().filter(|agent| agent.id.0 != "orchestrator") {
        let project = context.live.nodes.iter().find(|node| node.agent_id == agent.id.0)
            .and_then(|node| node.project.as_deref()).filter(|project| *project != "others")
            .unwrap_or(t!(context.lang, "Other", "Otros"));
        projects.entry(project.into()).or_default().push(Item::Row(node_row(agent, context)));
    }
    for (project, rows) in projects {
        let mut section = common::section(&format!("project:{project}"), Some(project), rows);
        section.count = Some(section.items.len() as u32);
        view.sections.push(section);
    }
    view
}

pub(super) fn detail(id: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    common::id(id)?;
    if id == "orchestrator" {
        return orchestrator(context);
    }
    let agent = context
        .domain
        .agents
        .get(&AgentId(id.into()))
        .ok_or(BuildError::NotFound)?;
    let live = context.live.nodes.iter().find(|node| node.agent_id == id);
    let mut view = common::view(
        "node",
        &format!("node:{id}"),
        Some(common::agent_label(&agent.id, context)),
        context,
        vec![],
    );
    let state = node_state(&agent.id, context);
    let mut facts = vec![Fact {
        label: t!(context.lang, "State", "Estado").into(),
        value: state.text,
        tone: Some(state.tone),
        link: None,
    }];
    if let Some(cwd) = live.and_then(|node| node.cwd.clone()) {
        facts.push(Fact {
            label: t!(context.lang, "Folder", "Carpeta").into(),
            value: cwd,
            tone: None,
            link: None,
        });
    }
    let tasks: Vec<_> = context
        .domain
        .tasks
        .values()
        .filter(|task| task.agent_id.as_ref() == Some(&agent.id))
        .collect();
    if let Some(task) = common::current_task(context.domain, &agent.id).filter(|task| !task.state.terminal()) {
        facts.push(Fact {
            label: t!(context.lang, "Task", "Tarea").into(),
            value: task.title.clone(),
            tone: None,
            link: None,
        });
    }
    view.sections.push(common::section(
        "header",
        None,
        vec![Item::Facts(Facts { items: facts })],
    ));
    if let Some(since) = live.and_then(|node| node.state_since.as_ref()) {
        let mut row = common::row("since", "node", Icon::History, t!(context.lang, "Since", "Desde").into(), None);
        row.link = None;
        row.trailing = Some(Trailing::Time { at: since.clone(), format: super::TimeFormat::Relative });
        view.sections.push(common::section("since", None, vec![Item::Row(row)]));
    }
    if let Some(short_id) = live.and_then(|node| node.short_id.as_ref()).filter(|_| context.client == super::Client::Tui) {
        let mut attach = common::row("attach", "node", Icon::Node,
            t!(context.lang, "Attach", "Conectar").into(), None);
        attach.link = Some(Link::Local { local: LocalLink::Attach { session: short_id.clone() } });
        view.sections.push(common::section("attach", None, vec![Item::Row(attach)]));
    }
    let waiting = context
        .domain
        .projection()
        .inbox
        .into_iter()
        .filter(|item| item.pending && item.agent_id.as_ref() == Some(&agent.id))
        .map(|item| super::card::from_item(&item, context, false).map(Item::Card))
        .collect::<Result<Vec<_>, BuildError>>()?;
    view.sections.push(common::section(
        "waiting",
        Some(t!(context.lang, "Waiting for You", "Te espera").into()),
        waiting,
    ));
    let rows = tasks
        .into_iter()
        .filter(|task| task.state != TaskState::Cancelled)
        .map(|task| {
            Item::Row(common::row(
                &task.id.0,
                &format!("task:{}", task.id),
                Icon::Task,
                task.title.clone(),
                None,
            ))
        })
        .collect();
    view.sections.push(common::section(
        "tasks",
        Some(t!(context.lang, "Tasks", "Tareas").into()),
        rows,
    ));
    if let Some(needs) = live.and_then(|node| node.needs.as_ref()).filter(|needs| !needs.is_empty()) {
        view.sections.push(common::section("needs", Some(t!(context.lang, "What it needs", "Qué necesita").into()),
            vec![Item::Markdown(Markdown { text: needs.clone(), truncated: false, more: None, commentable: false })]));
    }
    if let Some(node) = live.filter(|node| !node.background.is_empty()) {
        let rows = node.background.iter().map(|background| {
            let mut row = common::row(&background.since, "node", Icon::History, background.label.clone(),
                Some(match background.kind.as_str() {
                    "agent" => t!(context.lang, "Subagent", "Subagente"),
                    "shell" => t!(context.lang, "Shell", "Shell"),
                    _ => t!(context.lang, "Task", "Tarea"),
                }.into()));
            row.link = None;
            row.trailing = Some(Trailing::Time { at: background.since.clone(), format: super::TimeFormat::Relative });
            if background.suspect { row.badges.push(super::Label { text: t!(context.lang, "Possibly hung", "Quizá colgado").into(), tone: Tone::Warning }); }
            Item::Row(row)
        }).collect();
        view.sections.push(common::section("background", Some(t!(context.lang, "Running in the background", "En segundo plano").into()), rows));
    }
    if let Some(message) = live
        .and_then(|node| node.last_message.as_ref())
        .filter(|text| !text.trim().starts_with("mcp__") && !text.trim().is_empty())
    {
        let mut items = vec![Item::Markdown(Markdown {
            text: message.clone(),
            truncated: false,
            more: None,
            commentable: false,
        })];
        if context.client == super::Client::Ios
            && let Some(at) = live.and_then(|node| node.last_message_at.as_ref()) {
            let mut row = common::row("last_message_time", "node", Icon::History,
                t!(context.lang, "Last message", "Último mensaje").into(), None);
            row.link = None;
            row.trailing = Some(Trailing::Time { at: at.clone(), format: super::TimeFormat::Relative });
            items.push(Item::Row(row));
        }
        view.sections.push(common::section(
            "last_message",
            Some(t!(context.lang, "What it said last", "Lo último que dijo").into()),
            items,
        ));
    }
    if let Some(activity) = context
        .live
        .activity
        .get(id)
        .filter(|items| !items.is_empty())
    {
        let rows = activity
            .iter()
            .map(|entry| {
                let title = entry
                    .summary
                    .clone()
                    .or_else(|| entry.tool.clone())
                    .unwrap_or_else(|| t!(context.lang, "Working", "Trabajando").into());
                let subtitle = entry
                    .task
                    .as_ref()
                    .map(|task| format!("{} {task}", t!(context.lang, "Task", "Tarea")));
                let mut row = common::row(&entry.since, "node", Icon::History, title, subtitle);
                row.link = None;
                row.trailing = Some(super::Trailing::Time {
                    at: entry.since.clone(),
                    format: super::TimeFormat::Relative,
                });
                Item::Row(row)
            })
            .collect();
        view.sections.push(common::section(
            "now",
            Some(t!(context.lang, "Right now", "Ahora mismo").into()),
            rows,
        ));
    }
    let mut actions = vec![];
    if let Some(mut action) = common::action(
        Verb::Msg,
        id,
        context.domain.revision,
        t!(context.lang, "Message", "Mensaje"),
        Style::Primary,
    ) {
        action.input = super::Input::Text {
            placeholder: format!(
                "{} {}…",
                t!(context.lang, "Write to", "Escribe a"),
                common::agent_label(&agent.id, context)
            ),
            required: true,
            multiline: true,
        };
        action.attachments = true;
        actions.push(action);
    }
    actions.extend(close_action(id, context));
    if !actions.is_empty() {
        view.sections.push(common::section(
            "actions",
            None,
            vec![Item::Actions(super::ActionBar { actions })],
        ));
    }
    Ok(view)
}

fn orchestrator(context: &RenderContext<'_>) -> Result<View, BuildError> {
    let mut view = common::view(
        "node",
        "node:orchestrator",
        Some(t!(context.lang, "Orchestrator", "Orquestador").into()),
        context,
        vec![],
    );
    let live = context.live.orchestrator.as_ref();
    view.sections.push(common::section(
        "header",
        None,
        vec![Item::Facts(Facts {
            items: vec![Fact {
                label: t!(context.lang, "State", "Estado").into(),
                value: if live.is_some_and(|o| o.working) {
                    t!(context.lang, "Working", "Trabajando")
                } else {
                    t!(context.lang, "Idle", "Inactivo")
                }
                .into(),
                tone: None,
                link: None,
            }, Fact {
                label: t!(context.lang, "Queued messages", "Mensajes en cola").into(),
                value: live.map_or(0, |o| o.queue_depth).to_string(),
                tone: None,
                link: None,
            }],
        })],
    ));
    if let Some(summary) = live.and_then(|o| o.summary.as_ref()) {
        view.sections.push(common::section(
            "summary",
            None,
            vec![Item::Markdown(Markdown {
                text: summary.clone(),
                truncated: false,
                more: None,
                commentable: false,
            })],
        ));
    }
    let mut actions = Vec::new();
    if let Some(mut message) = common::action(Verb::Msg, "view", context.domain.revision,
        t!(context.lang, "Message", "Mensaje"), Style::Primary) {
        message.key = (context.client == super::Client::Tui).then(|| "M".into());
        message.input = super::Input::Text { placeholder: t!(context.lang, "Write to the orchestrator…", "Escribe al orquestador…").into(),
            required: true, multiline: true };
        message.attachments = true;
        message.key = (context.client == super::Client::Tui).then(|| "M".into());
        actions.push(message);
    }
    if let Some(run) = live.and_then(|o| o.active_run_id.as_deref()) {
        for (enabled, verb, label) in [
            (live.is_some_and(|o| o.can_retry), Verb::OrchRetry, t!(context.lang, "Retry", "Reintentar")),
            (live.is_some_and(|o| o.can_retry), Verb::OrchAck, t!(context.lang, "Continue without repeating", "Continuar sin repetir")),
            (live.is_some_and(|o| o.can_cancel), Verb::OrchStop, t!(context.lang, "Stop", "Detener")),
        ] {
            if !enabled { continue; }
            let style = if verb == Verb::OrchStop { Style::Destructive } else { Style::Secondary };
            if let Some(mut action) = common::action(verb, "orchestrator", context.domain.revision, label, style) {
                match super::action_id::ActionId::new(verb, "orchestrator", context.domain.revision, Some(run.into())) {
                    Ok(id) => {
                    action.id = id.to_string();
                    let confirmation = match verb {
                        Verb::OrchRetry => t!(context.lang,
                            "Some actions may already have completed. Check the activity and tasks before retrying to avoid repeating work.",
                            "Puede que algunas acciones ya se hayan completado. Revisa la actividad y las tareas antes de reintentarlo para evitar repetir trabajo."),
                        Verb::OrchAck => t!(context.lang,
                            "The interrupted work will not be repeated. Check what was completed first; pending messages will continue.",
                            "El trabajo interrumpido no se repetirá. Revisa primero qué se completó; los mensajes pendientes continuarán."),
                        Verb::OrchStop => t!(context.lang,
                            "Actions already completed will not be undone. You can review the result before continuing.",
                            "Las acciones ya completadas no se desharán. Puedes revisar el resultado antes de continuar."),
                        _ => unreachable!(),
                    };
                    action.confirm = Some(Confirm { title: label.into(), message: confirmation.into(), button: label.into() });
                    actions.push(action);
                    }
                    Err(_) => eprintln!("Alondra omitted an action with an invalid identifier ({verb:?})"),
                }
            }
        }
    }
    view.sections.push(common::section("actions", None, vec![Item::Actions(super::ActionBar { actions })]));
    Ok(view)
}
