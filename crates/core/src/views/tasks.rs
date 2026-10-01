use super::{
    BuildError, Confirm, Fact, Facts, Icon, Item, Link, LocalLink, Markdown, Notice, RenderContext,
    Style, Tone, Trailing, View, action_id::Verb, common, text::{t, tf},
};
use crate::domain_v2::{DomainState, Request, Task, TaskId, TaskState};
use std::collections::{BTreeMap, BTreeSet};

fn state(task: &Task, lang: super::Lang) -> &'static str {
    match task.state {
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

fn effort_label(effort: &str, lang: super::Lang) -> String {
    common::effort_label(effort, lang).unwrap_or(effort).into()
}

pub(super) fn task_row(task: &Task, context: &RenderContext<'_>) -> super::Row {
    let (icon, tone) = common::task_presentation(task.state);
    let mut row = common::row(
        &task.id.0,
        &format!("task:{}", task.id),
        icon,
        task.title.clone(),
        Some(state(task, context.lang).into()),
    );
    row.struck = task.state == TaskState::Cancelled;
    row.badges.push(super::Label { text: state(task, context.lang).into(), tone });
    row.trailing = Some(Trailing::Count {
        value: context
            .domain
            .projection()
            .inbox
            .iter()
            .filter(|item| item.pending && item.task_id.as_ref() == Some(&task.id))
            .count() as u32,
    });
    row
}

fn request_row(request: &Request, context: &RenderContext<'_>) -> super::Row {
    let tasks: Vec<_> = context
        .domain
        .tasks
        .values()
        .filter(|task| task.request_id == request.id)
        .collect();
    let title = request
        .title
        .clone()
        .or_else(|| (tasks.len() == 1).then(|| tasks[0].title.clone()))
        .unwrap_or_else(|| {
            request
                .body
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(120)
                .collect()
        });
    let mut row = common::row(
        &request.id.0,
        &format!("request:{}", request.id),
        Icon::Mission,
        title,
        Some(
            if request.withdrawn {
                t!(context.lang, "Withdrawn", "Retirada")
            } else if request.closed {
                t!(context.lang, "Finished", "Terminada")
            } else if tasks.is_empty() {
                t!(
                    context.lang,
                    "The orchestrator is splitting it",
                    "El orquestador la está dividiendo"
                )
            } else {
                t!(context.lang, "Active", "Activa")
            }
            .into(),
        ),
    );
    row.trailing = context
        .live
        .request_times
        .get(&request.id.0)
        .map(|at| Trailing::Time {
            at: at.clone(),
            format: super::TimeFormat::Relative,
        });
    if let Some(project) = tasks.iter().filter_map(|task| task.agent_id.as_ref())
        .filter_map(|id| context.live.nodes.iter().find(|node| node.agent_id == id.0))
        .filter_map(|node| node.project.as_deref()).find(|project| *project != "others") {
        row.badges.push(super::Label { text: project.into(), tone: Tone::Neutral });
    }
    row.children = task_tree(&tasks, context);
    row
}

fn task_tree(tasks: &[&Task], context: &RenderContext<'_>) -> Vec<super::Row> {
    let positions: BTreeMap<_, _> = tasks.iter().enumerate().map(|(index, task)| (task.id.0.clone(), index)).collect();
    let mut parents = BTreeMap::<String, String>::new();
    for task in tasks {
        let parent = task.dependencies.iter().filter(|id| id.0 != task.id.0)
            .filter_map(|id| positions.get(&id.0).map(|position| (id.0.as_str(), *position)))
            .max_by_key(|(_, position)| *position);
        if let Some((id, _)) = parent { parents.insert(task.id.0.clone(), id.into()); }
    }
    fn branch(task: &Task, tasks: &[&Task], parents: &BTreeMap<String, String>,
        placed: &mut BTreeSet<String>, depth: u8, context: &RenderContext<'_>) -> Option<super::Row> {
        if !placed.insert(task.id.0.clone()) { return None; }
        let mut row = task_row(task, context);
        row.depth = depth;
        row.children = tasks.iter().filter(|child| parents.get(&child.id.0) == Some(&task.id.0))
            .filter_map(|child| branch(child, tasks, parents, placed, depth.saturating_add(1), context))
            .collect();
        Some(row)
    }
    let mut placed = BTreeSet::new();
    let mut rows = Vec::new();
    for task in tasks.iter().filter(|task| !parents.contains_key(&task.id.0)) {
        if let Some(row) = branch(task, tasks, &parents, &mut placed, 1, context) { rows.push(row); }
    }
    for task in tasks {
        if let Some(row) = branch(task, tasks, &parents, &mut placed, 1, context) { rows.push(row); }
    }
    rows
}

fn merge_order<'a>(domain: &'a DomainState, task: &'a Task) -> Vec<&'a Task> {
    let mut seen = BTreeSet::from([task.id.clone()]);
    fn below<'a>(domain: &'a DomainState, task: &'a Task, seen: &mut BTreeSet<TaskId>, out: &mut Vec<&'a Task>) {
        for id in &task.dependencies {
            if !seen.insert(id.clone()) { continue; }
            if let Some(dependency) = domain.tasks.get(id) {
                below(domain, dependency, seen, out);
                out.push(dependency);
            }
        }
    }
    let mut order = Vec::new();
    below(domain, task, &mut seen, &mut order);
    order.push(task);
    let mut frontier = vec![task];
    while !frontier.is_empty() {
        let next: Vec<_> = domain.tasks.values()
            .filter(|candidate| frontier.iter().any(|parent| candidate.dependencies.contains(&parent.id)))
            .filter(|candidate| seen.insert(candidate.id.clone())).collect();
        order.extend(next.iter().copied());
        frontier = next;
    }
    order
}

pub(super) fn list(key: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    let page = if key == "tasks" {
        None
    } else {
        let tail = key
            .strip_prefix("tasks?section=")
            .ok_or(BuildError::InvalidKey)?;
        let (section, after) = tail.split_once("&after=").ok_or(BuildError::InvalidKey)?;
        if !matches!(section, "active" | "finished" | "archived") || after.parse::<usize>().is_err() {
            return Err(BuildError::InvalidKey);
        }
        Some((
            section,
            after.parse::<usize>().map_err(|_| BuildError::InvalidKey)?,
        ))
    };
    let mut view = common::view(
        "tasks",
        key,
        Some(t!(context.lang, "Tasks", "Tareas").into()),
        context,
        vec![],
    );
    let mut scheduled = common::section(
        "scheduled",
        None,
        vec![Item::Row(common::row(
            "schedules",
            "schedules",
            Icon::Schedule,
            t!(context.lang, "Scheduled Tasks", "Tareas programadas").into(),
            None,
        ))],
    );
    let scheduled_count = context.domain.schedule.schedules.values()
        .filter(|schedule| schedule.deleted_at.is_none()).count() as u32;
    scheduled.count = (scheduled_count > 0).then_some(scheduled_count);
    view.sections.push(scheduled);
    for (id, title) in [
        ("active", t!(context.lang, "Active", "Activas")),
        ("finished", t!(context.lang, "Finished", "Terminadas")),
        ("archived", t!(context.lang, "Archived", "Archivadas")),
    ] {
        let requests: Vec<_> = context
            .domain
            .requests
            .values()
            .filter(|request| {
                let tasks: Vec<_> = context
                    .domain
                    .tasks
                    .values()
                    .filter(|task| task.request_id == request.id)
                    .collect();
                let archived = !tasks.is_empty() && tasks.iter().all(|task| task.archived);
                let finished = request.closed
                    || (!tasks.is_empty() && tasks.iter().all(|task| task.state.terminal()));
                match id {
                    "archived" => archived,
                    "finished" => finished && !archived,
                    _ => !finished && !archived,
                }
            })
            .collect();
        let after = page
            .filter(|(part, _)| *part == id)
            .map_or(0, |(_, after)| after);
        let shown: Vec<_> = requests
            .iter()
            .skip(after)
            .take(20)
            .map(|request| Item::Row(request_row(request, context)))
            .collect();
        let mut section = common::section(id, Some(title.into()), shown);
        section.count = Some(requests.len() as u32);
        section.collapsed = id != "active" && page.is_none();
        if requests.len() > after + 20 {
            section.more = Some(super::More {
                key: format!("tasks?section={id}&after={}", after + 20),
                remaining: (requests.len() - after - 20) as u32,
            });
        }
        view.sections.push(section);
    }
    if context.domain.tasks.is_empty() {
        view.empty = Some(Notice {
            text: t!(context.lang, "No tasks yet", "Aún no hay tareas").into(),
            tone: Tone::Neutral,
            action: None,
        });
    }
    if context.domain.tasks.values().any(|task| task.state.terminal() && !task.archived)
        && let Some(action) = common::action(
        Verb::ArchiveDone,
        "view",
        context.domain.revision,
        t!(context.lang, "Archive Completed", "Archivar terminadas"),
        Style::Secondary,
    ) {
        view.sections.push(common::section(
            "actions",
            None,
            vec![Item::Actions(super::ActionBar {
                actions: vec![action],
            })],
        ));
    }
    Ok(view)
}

fn now_line(task: &Task, context: &RenderContext<'_>) -> Option<super::Row> {
    let line = |text: String, link: Option<super::Link>| {
        let mut row = common::row("now", "task", Icon::History, text, None);
        row.link = link;
        row
    };
    match task.state {
        TaskState::NeedsYou => context
            .domain
            .projection()
            .inbox
            .into_iter()
            .find(|item| item.pending && item.task_id.as_ref() == Some(&task.id))
            .map(|item| line(
                tf!(context.lang, "question: {title}", "pregunta: {title}", title = item.title),
                Some(super::Link::View { key: super::card::card_key(&item) }),
            )),
        TaskState::InReview => task.documents.keys().last()
            .and_then(|id| context.domain.documents.get(id))
            .map(|document| line(
                tf!(context.lang, "document: {title}", "documento: {title}", title = document.title), None)),
        TaskState::WaitingDependency => task.stack.as_ref()
            .and_then(|stack| context.domain.pull_requests.get(&stack.pr_id))
            .map(|pr| line(
                tf!(context.lang, "waits for PR #{number} to be merged",
                    "espera a que se fusione el PR #{number}", number = pr.number), None)),
        TaskState::Closing => Some(line(t!(context.lang,
            "merged; the node does what the merge unblocks",
            "fusionada; el nodo hace lo que desbloquea la fusión").into(), None)),
        TaskState::NotStarted | TaskState::Running | TaskState::Done | TaskState::Cancelled => None,
    }
}

pub(super) fn detail(id: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    common::id(id)?;
    let task = context
        .domain
        .tasks
        .get(&TaskId(id.into()))
        .ok_or(BuildError::NotFound)?;
    let mut view = common::view(
        "task",
        &format!("task:{id}"),
        Some(task.title.clone()),
        context,
        vec![],
    );
    let request = context.domain.requests.get(&task.request_id);
    let mut facts = vec![Fact {
        label: t!(context.lang, "State", "Estado").into(),
        value: state(task, context.lang).into(),
        tone: None,
        link: None,
    }];
    if let Some(agent_id) = &task.agent_id {
        facts.push(Fact {
            label: t!(context.lang, "Node", "Nodo").into(),
            value: common::agent_label(agent_id, context),
            tone: None,
            link: Some(super::Link::View {
                key: format!("node:{agent_id}"),
            }),
        });
    }
    facts.push(Fact {
        label: t!(context.lang, "Request", "Solicitud").into(),
        value: request
            .and_then(|r| r.title.clone())
            .or_else(|| request.map(|r| r.body.lines().next().unwrap_or("").trim().chars().take(120).collect()))
            .filter(|title: &String| !title.is_empty())
            .unwrap_or_else(|| t!(context.lang, "Request details unavailable", "Detalles de la solicitud no disponibles").into()),
        tone: None,
        link: Some(super::Link::View {
            key: format!("request:{}", task.request_id),
        }),
    });
    view.sections.push(common::section(
        "header",
        None,
        vec![Item::Facts(Facts { items: facts })],
    ));
    if let Some(now) = now_line(task, context) {
        view.sections.push(common::section("now", None, vec![Item::Row(now)]));
    }
    if let Some(history) = context
        .live
        .task_history
        .get(id)
        .filter(|items| !items.is_empty())
    {
        let rows = history
            .iter()
            .map(|step| {
                let title = match step.state.as_str() {
                    "not_started" => t!(context.lang, "Not started", "Sin empezar"),
                    "running" => t!(context.lang, "Working", "Trabajando"),
                    "needs_you" => t!(context.lang, "Needs you", "Te necesita"),
                    "in_review" => t!(context.lang, "In review", "En revisión"),
                    "waiting_dependency" => t!(context.lang, "Waiting", "Esperando"),
                    "closing" => t!(context.lang, "Closing", "Cerrando"),
                    "done" => t!(context.lang, "Finished", "Terminado"),
                    "cancelled" => t!(context.lang, "Cancelled", "Cancelado"),
                    _ => step.state.as_str(),
                };
                let mut row = common::row(
                    &step.at,
                    "task",
                    Icon::History,
                    title.into(),
                    Some(step.summary.clone()),
                );
                row.link = None;
                row.trailing = Some(Trailing::Time {
                    at: step.at.clone(),
                    format: super::TimeFormat::Relative,
                });
                Item::Row(row)
            })
            .collect();
        view.sections.push(common::section(
            "history",
            Some(t!(context.lang, "History", "Historial").into()),
            rows,
        ));
    }
    let waiting: Vec<_> = context
        .domain
        .projection()
        .inbox
        .into_iter()
        .filter(|item| item.pending && item.task_id.as_ref() == Some(&task.id))
        .map(|item| super::card::from_item(&item, context, false).map(Item::Card))
        .collect::<Result<Vec<_>, BuildError>>()?;
    if !waiting.is_empty() {
        view.sections.push(common::section(
            "waiting",
            Some(t!(context.lang, "Waiting for You", "Te espera").into()),
            waiting,
        ));
    }
    if let Some(request) = request {
        view.sections.push(common::section(
            "request",
            Some(t!(context.lang, "Request", "Solicitud").into()),
            vec![Item::Markdown(Markdown {
                text: request.body.clone(),
                truncated: false,
                more: None,
                commentable: false,
            })],
        ));
    }
    let prs: Vec<_> = merge_order(context.domain, task).into_iter()
        .flat_map(|owner| context.domain.pull_requests.values().filter(move |pr| pr.task_id == owner.id)
            .map(move |pr| (owner, pr)))
        .enumerate()
        .map(|(index, (owner, pr))| {
            let details = context.live.pull_requests.iter().find(|details| details.id == pr.id.0);
            let title = details.and_then(|details| details.title.as_deref())
                .map(str::to_owned).unwrap_or_else(|| format!("PR #{}", pr.number));
            let branches = details.and_then(|details| details.head_ref.as_deref())
                .map(|head| format!("{head} → {}", details.and_then(|details| details.base_ref.as_deref()).unwrap_or(&pr.base)));
            Item::Row(common::row(
                &pr.id.0,
                &format!("pull_request:{}", pr.id),
                Icon::PullRequest,
                format!("{} · {title}", index + 1),
                branches.map(|branches| if owner.id == task.id { branches }
                    else { format!("{} · {branches}", owner.title) }),
            ))
        })
        .collect();
    if !prs.is_empty() {
        view.sections.push(common::section(
            "pull_requests",
            Some(t!(context.lang, "Pull requests", "Solicitudes de cambios").into()),
            prs,
        ));
    }
    let deps: Vec<_> = task
        .dependencies
        .iter()
        .filter_map(|id| context.domain.tasks.get(id))
        .map(|dep| Item::Row(task_row(dep, context)))
        .collect();
    if !deps.is_empty() {
        view.sections.push(common::section(
            "dependencies",
            Some(t!(context.lang, "Dependencies", "Dependencias").into()),
            deps,
        ));
    }
    let mut task_options = vec![Fact {
            label: t!(context.lang, "PR handling", "Gestión de PR").into(),
            value: if task.review_prs {
                t!(context.lang, "Review before merge", "Revisar antes de fusionar")
            } else {
                t!(context.lang, "No PR review; merge still needs your authorization", "Sin revisión de PR; fusionar sigue requiriendo tu autorización")
            }.into(),
            tone: None,
            link: None,
        }];
    if let Some(options) = context.live.task_options.get(id) {
        if let Some(project) = options.project.as_ref().filter(|project| !project.is_empty()) {
            task_options.push(Fact {
                label: t!(context.lang, "Project", "Proyecto").into(),
                value: project.clone(),
                tone: None,
                link: None,
            });
        }
        if let Some(effort) = &options.effort {
            task_options.push(Fact {
                label: t!(context.lang, "Effort", "Esfuerzo").into(),
                value: effort_label(effort, context.lang),
                tone: None,
                link: None,
            });
        }
    }
    view.sections.push(common::section(
        "options",
        Some(t!(context.lang, "Options", "Opciones").into()),
        vec![Item::Facts(Facts { items: task_options })],
    ));
    let documents: Vec<_> = task
        .documents
        .iter()
        .filter_map(|(id, _)| context.domain.documents.get(id))
        .map(|doc| {
            Item::Row(common::row(
                &doc.id.0,
                &format!("document:{}", doc.current_revision),
                Icon::Document,
                doc.title.clone(),
                None,
            ))
        })
        .collect();
    if !documents.is_empty() {
        view.sections.push(common::section(
            "documents",
            Some(t!(context.lang, "Documents", "Documentos").into()),
            documents,
        ));
    }
    if context.client == super::Client::Tui
        && let Some(short_id) = task
            .agent_id
            .as_ref()
            .and_then(|agent| context.live.nodes.iter().find(|node| node.agent_id == agent.0))
            .and_then(|node| node.short_id.as_ref())
    {
        let mut attach = common::row(
            "attach",
            "node",
            Icon::Node,
            t!(context.lang, "Attach", "Conectar").into(),
            None,
        );
        attach.link = Some(Link::Local {
            local: LocalLink::Attach {
                session: short_id.clone(),
            },
        });
        view.sections
            .push(common::section("attach", None, vec![Item::Row(attach)]));
    }
    let mut actions = vec![];
    if let Some(agent_id) = task.agent_id.as_ref().filter(|id| context.domain.agents.contains_key(*id)) {
        if let Some(mut message) = common::action(
            Verb::Msg, &agent_id.0, context.domain.revision,
            t!(context.lang, "Message", "Mensaje"), Style::Secondary,
        ) {
            match super::action_id::ActionId::new(
                Verb::Msg, &agent_id.0, context.domain.revision, Some(task.id.0.clone()),
            ) {
                Ok(id) => {
                message.id = id.to_string();
                message.key = (context.client == super::Client::Tui).then(|| "M".into());
                message.input = super::Input::Text {
                    placeholder: t!(context.lang, "Write to this task's node…", "Escribe al nodo de esta tarea…").into(),
                    required: true,
                    multiline: true,
                };
                message.attachments = true;
                actions.push(message);
                }
                Err(_) => eprintln!("Alondra omitted an action with an invalid identifier ({:?})", Verb::Msg),
            }
        }
    }
    for available in context.domain.task_actions(&task.id).unwrap_or_default() {
        let (verb, label, style) = match available.kind.as_str() {
            "cancel_task" => (
                Verb::Cancel,
                t!(context.lang, "Cancel Task", "Cancelar tarea"),
                Style::Destructive,
            ),
            "reopen_task" => (
                Verb::Reopen,
                t!(context.lang, "Reopen", "Reabrir"),
                Style::Secondary,
            ),
            "archive_task" => (
                Verb::Archive,
                t!(context.lang, "Archive", "Archivar"),
                Style::Secondary,
            ),
            "unarchive_task" => (
                Verb::Unarchive,
                t!(context.lang, "Unarchive", "Desarchivar"),
                Style::Secondary,
            ),
            _ => continue,
        };
        if let Some(mut action) =
            common::action(verb, id, available.expected_revision, label, style)
        {
            action.enabled = available.enabled;
            action.disabled_reason = available.disabled_reason
                .as_deref()
                .map(|code| super::card::disabled_reason(code, context.lang).to_owned());
            if verb == Verb::Cancel {
                action.confirm = Some(Confirm {
                    title: label.into(),
                    message: t!(context.lang, "Cancel this task?", "¿Cancelar esta tarea?").into(),
                    button: label.into(),
                });
            }
            actions.push(action);
        }
    }
    view.sections.push(common::section(
        "actions",
        None,
        vec![Item::Actions(super::ActionBar { actions })],
    ));
    Ok(view)
}

pub(super) fn request(id: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    common::id(id)?;
    let request = context
        .domain
        .requests
        .get(&crate::domain_v2::RequestId(id.into()))
        .ok_or(BuildError::NotFound)?;
    let mut view = common::view(
        "request",
        &format!("request:{id}"),
        request.title.clone(),
        context,
        vec![],
    );
    view.sections.push(common::section(
        "facts",
        None,
        vec![Item::Facts(Facts {
            items: vec![Fact {
                label: t!(context.lang, "State", "Estado").into(),
                value: if request.withdrawn {
                    t!(context.lang, "Withdrawn", "Retirada")
                } else if request.closed {
                    t!(context.lang, "Finished", "Terminada")
                } else {
                    t!(context.lang, "Active", "Activa")
                }
                .into(),
                tone: None,
                link: None,
            }],
        })],
    ));
    let tasks = context
        .domain
        .tasks
        .values()
        .filter(|task| task.request_id == request.id)
        .map(|task| Item::Row(task_row(task, context)))
        .collect();
    view.sections.push(common::section(
        "tasks",
        Some(t!(context.lang, "Tasks", "Tareas").into()),
        tasks,
    ));
    view.sections.push(common::section(
        "body",
        None,
        vec![Item::Markdown(Markdown {
            text: request.body.clone(),
            truncated: false,
            more: None,
            commentable: false,
        })],
    ));
    if !request.closed && !request.withdrawn {
        if let Some(mut action) = common::action(
            Verb::Withdraw,
            id,
            context.domain.revision,
            t!(context.lang, "Withdraw Request", "Retirar solicitud"),
            Style::Destructive,
        ) {
            action.confirm = Some(Confirm {
                title: action.label.clone(),
                message: t!(
                    context.lang,
                    "Withdraw this request?",
                    "¿Retirar esta solicitud?"
                )
                .into(),
                button: action.label.clone(),
            });
            view.sections.push(common::section(
                "actions",
                None,
                vec![Item::Actions(super::ActionBar {
                    actions: vec![action],
                })],
            ));
        }
    }
    Ok(view)
}
