use super::{
    BuildError, Confirm, Fact, Facts, Icon, Item, Link, LocalLink, Markdown, RenderContext, StepState, Style, Tone,
    Trailing, View, action_id::Verb, common, text::{t, tf},
};
use crate::domain::{AgentId, MergeAttemptState, NodePhase, Pause, PauseReason, PrState, TaskState};
use std::collections::BTreeMap;

fn close_action(id: &str, context: &RenderContext<'_>) -> Option<super::Action> {
    let mut action = common::action(
        Verb::Close,
        id,
        context.domain.revision,
        t!(context.lang, "Close Node", "Cerrar nodo"),
        Style::Destructive,
    )?;
    action.key = (context.client == super::Client::Tui).then(|| "x".into());
    action.confirm = Some(Confirm {
        title: action.label.clone(),
        message: t!(context.lang, "Close this node?", "¿Cerrar este nodo?").into(),
        button: action.label.clone(),
    });
    Some(action)
}

fn message_action(id: &str, context: &RenderContext<'_>) -> Option<super::Action> {
    let mut action = common::action(Verb::Msg, id, context.domain.revision,
        t!(context.lang, "Message", "Mensaje"), Style::Primary)?;
    action.key = (context.client == super::Client::Tui).then(|| "r".into());
    action.input = super::Input::Text {
        placeholder: format!("{} {}…", t!(context.lang, "Write to", "Escribe a"), common::agent_label(&AgentId(id.into()), context)),
        required: true,
        multiline: true,
    };
    action.attachments = true;
    Some(action)
}

/// Stopping a node's current turn, offered only while it works.
fn interrupt_action(id: &str, context: &RenderContext<'_>) -> Option<super::Action> {
    common::working(id, context).then_some(())?;
    let mut action = common::action(Verb::Interrupt, id, context.domain.revision,
        t!(context.lang, "Interrupt", "Interrumpir"), Style::Secondary)?;
    action.key = (context.client == super::Client::Tui).then(|| "i".into());
    action.confirm = Some(Confirm {
        title: action.label.clone(),
        message: t!(context.lang, "Stop what this node is doing now? It keeps its task and waits for you.",
            "¿Detener lo que hace ahora este nodo? Conserva su tarea y te espera.").into(),
        button: action.label.clone(),
    });
    Some(action)
}

/// A node's one state, in 8ccd131's words: a question it blocks on, then what the session does,
/// then what waits for the user on it, then where its tasks stand.
/// A node is in Fleet while its session lives: whatever its phase but closed.
pub(super) fn in_fleet(agent: &crate::domain::Agent, context: &RenderContext<'_>) -> bool {
    agent.id.0 != "orchestrator"
        && context.domain.orchestrator_session.as_ref() != Some(&agent.id)
        && agent.phase.live()
}

pub(super) fn node_state(agent: &AgentId, context: &RenderContext<'_>) -> super::Label {
    let lang = context.lang;
    let live = context.live.nodes.iter().find(|node| node.agent_id == agent.0);
    let key = live.and_then(|node| node.state_key.as_deref());
    let inbox = &context.projection.inbox;
    let waiting = |kinds: &[&str]| inbox.iter()
        .any(|item| item.pending && item.agent_id.as_ref() == Some(agent) && kinds.contains(&item.kind.as_str()));
    let reviewing = inbox.iter().any(|item| {
        item.pending && item.agent_id.as_ref() == Some(agent) && super::card::is_review_delivery(item, context.domain)
    });
    let tasks: Vec<TaskState> = context.domain.tasks.values()
        .filter(|task| task.agent_id.as_ref() == Some(agent) && !task.archived)
        .map(|task| task.state)
        .collect();
    let merging = context.domain.merge_authorizations.values().any(|authorization| {
        authorization.claimed_by.as_ref() == Some(agent)
            && matches!(authorization.attempt, MergeAttemptState::Claimed | MergeAttemptState::Uncertain)
            && context.domain.pull_requests.get(&authorization.pr_id).is_some_and(|pr| pr.state == PrState::Open)
    });
    let phase = context.domain.node_phase(agent).cloned().unwrap_or(NodePhase::Closed);
    let (text, tone) = match phase {
        NodePhase::Starting { reviving: true } => (t!(lang, "Reconnecting", "Reconectando"), Tone::Info),
        NodePhase::Starting { reviving: false } => (t!(lang, "Starting", "Arrancando"), Tone::Neutral),
        NodePhase::Working if merging => (t!(lang, "Merging", "Fusionando"), Tone::Info),
        NodePhase::Working if key == Some("stalled") => (t!(lang, "Stalled", "Parado"), Tone::Critical),
        NodePhase::Working if key == Some("failed") => (t!(lang, "Failed", "Falló"), Tone::Critical),
        NodePhase::Working => (t!(lang, "Working", "Trabajando"), Tone::Info),
        NodePhase::WaitingUser { .. } if waiting(&["action_request"]) => (t!(lang, "Your turn", "Te toca"), Tone::YourTurn),
        NodePhase::WaitingUser { .. } => (t!(lang, "Waiting for an answer", "Esperando respuesta"), Tone::Warning),
        NodePhase::Paused(Pause { reason: PauseReason::ByUser, .. }) => (t!(lang, "Paused", "En pausa"), Tone::Neutral),
        NodePhase::Paused(_) => (t!(lang, "Waiting for the machine", "Esperando máquina"), Tone::Neutral),
        NodePhase::Delivered if reviewing || tasks.contains(&TaskState::ForYourReview) =>
            (t!(lang, "Waiting for your review", "Espera tu revisión"), Tone::Accent),
        NodePhase::Delivered => (t!(lang, "Delivering results", "Entrega resultado"), Tone::Success),
        NodePhase::Stopped => match awaited_title(agent, context) {
            Some(title) => return super::Label { text: tf!(lang, "Waiting for {title}", "Esperando a {title}", title = title), tone: Tone::Neutral },
            None => (t!(lang, "Stopped", "Detenido"), Tone::Warning),
        },
        NodePhase::Closing => match post_merge_work(agent, context) {
            Some(reason) => return super::Label {
                text: tf!(lang, "Finishing after the merge: {reason}", "Terminando tras fusionar: {reason}", reason = reason),
                tone: Tone::Info,
            },
            None if merging => (t!(lang, "Merging", "Fusionando"), Tone::Info),
            None => (t!(lang, "Done", "Terminado"), Tone::Success),
        },
        NodePhase::Closed if !tasks.is_empty() && tasks.iter().all(|state| matches!(state, TaskState::Done | TaskState::Closing | TaskState::Cancelled)) =>
            (t!(lang, "Done", "Terminado"), Tone::Success),
        NodePhase::Closed => (t!(lang, "Closed", "Cerrado"), Tone::Neutral),
    };
    super::Label { text: text.into(), tone }
}

/// What the node says it still does after its task's PR merged.
fn post_merge_work<'a>(agent: &AgentId, context: &RenderContext<'a>) -> Option<&'a str> {
    context.domain.tasks.values()
        .filter(|task| task.agent_id.as_ref() == Some(agent) && !task.state.terminal())
        .find_map(|task| task.wrap_up.post_merge_work.as_ref().map(|work| work.reason.as_str()))
}

/// The title, clipped, of the task a node's waiting task is held behind.
fn awaited_title(agent: &AgentId, context: &RenderContext<'_>) -> Option<String> {
    let task = common::current_task(context.domain, agent).filter(|task| task.state == TaskState::WaitingDependency)?;
    let upstream = task.dependencies.iter().chain(task.stack.iter().map(|stack| &stack.upstream_task))
        .filter_map(|id| context.domain.tasks.get(id))
        .find(|dependency| !dependency.state.terminal())?;
    let mut title: String = upstream.title.chars().take(40).collect();
    if upstream.title.chars().count() > 40 {
        title.push('…');
    }
    Some(title)
}

fn tokens(count: u64) -> String {
    match count {
        n if n >= 1_000_000 && n % 1_000_000 == 0 => format!("{}M", n / 1_000_000),
        n if n >= 1_000 => format!("{}K", n / 1_000),
        n => n.to_string(),
    }
}

fn cost(cents: u64, lang: super::Lang) -> String {
    let (units, rest) = (cents / 100, cents % 100);
    tf!(lang, "${units}.{rest:02}", "{units},{rest:02} $", units = units, rest = rest)
}

fn context_label(percent: u32, lang: super::Lang) -> super::Label {
    super::Label {
        text: tf!(lang, "Context {percent}%", "Contexto {percent}%", percent = percent),
        tone: if percent >= 80 { Tone::Warning } else { Tone::Neutral },
    }
}

/// What the node runs now while it works, otherwise what it said last, with when.
pub(super) fn activity(node: &super::LiveNode, context: &RenderContext<'_>) -> Option<super::Row> {
    let (text, at) = doing(node, common::working(&node.agent_id, context), context.lang)?;
    let mut row = common::row("activity", "node", Icon::Notice, text, None);
    row.link = None;
    row.trailing = at.map(|at| Trailing::Time { at, format: super::TimeFormat::Relative });
    Some(row)
}

/// What a node does, only as facts it reports: the step of its own task list while it works; once it stopped,
/// the first sentence it said and when. Nothing is inferred from the commands it runs, and a node at work carries
/// no time, so its row never goes stale.
pub(super) fn doing(node: &super::LiveNode, working: bool, lang: super::Lang) -> Option<(String, Option<String>)> {
    if let Some(step) = step(node, lang) {
        return Some((step, None));
    }
    if working {
        return None;
    }
    node.last_message.as_deref().and_then(said).map(|sentence| (
        tf!(lang, "Said: “{sentence}”", "Dijo: «{sentence}»"),
        node.last_message_at.clone(),
    ))
}

/// "Step 3 of 4 · Generate iOS screenshots", while the node's own task list has a step in progress.
pub(super) fn step(node: &super::LiveNode, lang: super::Lang) -> Option<String> {
    let (index, step) = node.steps.iter().enumerate().find(|(_, step)| step.state == StepState::InProgress)?;
    let (number, total, subject) = (index + 1, node.steps.len(), &step.subject);
    Some(tf!(lang, "Step {number} of {total} · {subject}", "Paso {number} de {total} · {subject}"))
}

const SAID_LIMIT: usize = 80;

/// The first sentence of what a node said, when it is prose: not a tool, a path or code.
fn said(message: &str) -> Option<String> {
    let line = message.lines().map(str::trim).find(|line| !line.is_empty())?;
    if line.starts_with(['`', '/', '~', '$']) || line.starts_with("mcp__") || line.starts_with("./")
        || line.split_once(':').is_some_and(|(head, _)| TOOLS.contains(&head)) {
        return None;
    }
    let sentence = line.char_indices()
        .find(|(at, c)| matches!(c, '.' | '?' | '!') && line[at + c.len_utf8()..].starts_with(' '))
        .map_or(line, |(at, c)| &line[..at + c.len_utf8()]);
    if sentence.chars().count() <= SAID_LIMIT {
        return Some(sentence.to_owned());
    }
    let cut: String = sentence.chars().take(SAID_LIMIT - 1).collect();
    Some(format!("{}…", cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head)))
}

const TOOLS: [&str; 12] = ["Bash", "Edit", "Write", "MultiEdit", "NotebookEdit", "Read", "Grep", "Glob", "Agent", "Task", "WebSearch", "WebFetch"];

/// How far the node's task list has got ("3/5"), while a step is still open.
fn progress_badge(node: &super::LiveNode) -> Option<super::Label> {
    let done = node.steps.iter().filter(|step| step.state == StepState::Completed).count();
    (done < node.steps.len()).then(|| super::Label { text: format!("{done}/{}", node.steps.len()), tone: Tone::Neutral })
}

fn ran_for(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, secs % 3600 / 60),
    }
}

fn run_row(id: &str, run: &super::LiveRun, subtitle: Option<String>, lang: super::Lang) -> super::Row {
    let mut row = common::row(id, "node", Icon::History, run.label.clone(), subtitle);
    row.link = None;
    match run.ran_secs {
        Some(secs) => {
            row.badges.push(super::Label { text: t!(lang, "Finished", "Terminado").into(), tone: Tone::Neutral });
            row.trailing = Some(Trailing::Text { text: ran_for(secs) });
        }
        None => {
            row.badges.push(super::Label { text: t!(lang, "Running", "En marcha").into(), tone: Tone::Info });
            row.trailing = Some(Trailing::Time { at: run.since.clone(), format: super::TimeFormat::Relative });
        }
    }
    if run.suspect {
        row.badges.push(super::Label { text: t!(lang, "Possibly hung", "Quizá colgado").into(), tone: Tone::Warning });
    }
    row
}

/// Where a workflow has got: its phase, how many of its agents run and are done, and the most
/// recent of them.
fn workflow_items(id: &str, workflow: &super::LiveWorkflow, lang: super::Lang) -> Vec<Item> {
    let mut facts = vec![];
    if let Some(phase) = &workflow.phase {
        let value = match (workflow.phase_index, workflow.phase_count) {
            (Some(index), Some(count)) => tf!(lang, "{phase} ({index} of {count})", "{phase} ({index} de {count})",
                phase = phase, index = index, count = count),
            _ => phase.clone(),
        };
        facts.push(Fact { label: t!(lang, "Phase", "Fase").into(), value, tone: None, link: None });
    }
    if workflow.running + workflow.done > 0 {
        let value = tf!(lang, "{running} running · {done} done", "{running} en marcha · {done} terminados",
            running = workflow.running, done = workflow.done);
        let tone = if workflow.running > 0 { Tone::Info } else { Tone::Success };
        facts.push(Fact { label: t!(lang, "Agents", "Agentes").into(), value, tone: Some(tone), link: None });
    }
    if let Some(count) = workflow.total_tokens {
        facts.push(Fact { label: t!(lang, "Tokens", "Tokens").into(), value: tokens(count), tone: None, link: None });
    }
    let mut items = vec![];
    if !facts.is_empty() {
        items.push(Item::Facts(Facts { items: facts }));
    }
    for (index, agent) in workflow.agents.iter().take(super::WORKFLOW_AGENT_ROWS).enumerate() {
        let about = [agent.phase.clone(), agent.model.as_deref().map(crate::model_label::launched_model_label)]
            .into_iter().flatten().collect::<Vec<_>>().join(" · ");
        let icon = if agent.running { Icon::Task } else { Icon::Check };
        let mut row = common::row(&format!("{id}:agent:{index}"), "node", icon, agent.label.clone(), (!about.is_empty()).then_some(about));
        row.link = None;
        row.badges.push(if agent.running {
            super::Label { text: t!(lang, "Running", "En marcha").into(), tone: Tone::Info }
        } else {
            super::Label { text: t!(lang, "Done", "Hecho").into(), tone: Tone::Neutral }
        });
        items.push(Item::Row(row));
    }
    let shown = workflow.agents.len().min(super::WORKFLOW_AGENT_ROWS);
    let more = (workflow.running + workflow.done) as usize;
    if more > shown {
        let mut row = common::row(&format!("{id}:more"), "node", Icon::History,
            tf!(lang, "+{count} more", "+{count} más", count = more - shown), None);
        row.link = None;
        items.push(Item::Row(row));
    }
    items
}

/// What the node runs beside its main thread and how far its task list has got: shown on the
/// node and on the task it works.
pub(super) fn work_sections(node: &super::LiveNode, lang: super::Lang) -> Vec<super::Section> {
    let mut sections = vec![];
    if !node.steps.is_empty() {
        let done = node.steps.iter().filter(|step| step.state == StepState::Completed).count();
        let mut facts = vec![Fact {
            label: t!(lang, "Progress", "Progreso").into(),
            value: tf!(lang, "{done} of {total}", "{done} de {total}", done = done, total = node.steps.len()),
            tone: Some(if done == node.steps.len() { Tone::Success } else { Tone::Info }),
            link: None,
        }];
        if let Some(step) = node.steps.iter().find(|step| step.state == StepState::InProgress) {
            facts.push(Fact { label: t!(lang, "Now", "Ahora").into(),
                value: step.active_form.clone().unwrap_or_else(|| step.subject.clone()), tone: None, link: None });
        }
        let mut items = vec![Item::Facts(Facts { items: facts })];
        items.extend(node.steps.iter().enumerate().map(|(index, step)| {
            let icon = if step.state == StepState::Completed { Icon::Check } else { Icon::Task };
            let mut row = common::row(&format!("step:{index}"), "node", icon, step.subject.clone(), None);
            row.link = None;
            row.struck = step.state == StepState::Completed;
            if step.state == StepState::InProgress {
                row.badges.push(super::Label { text: t!(lang, "In progress", "En curso").into(), tone: Tone::Info });
            }
            Item::Row(row)
        }));
        sections.push(common::section("steps", Some(t!(lang, "Steps", "Pasos").into()), items));
    }
    if !node.subagents.is_empty() {
        let rows = node.subagents.iter().enumerate().map(|(index, run)| {
            let about = [run.kind.clone(), run.model.as_deref().map(crate::model_label::model_label)]
                .into_iter().flatten().collect::<Vec<_>>().join(" · ");
            Item::Row(run_row(&format!("subagent:{index}"), run, (!about.is_empty()).then_some(about), lang))
        }).collect();
        sections.push(common::section("subagents", Some(t!(lang, "Subagents", "Subagentes").into()), rows));
    }
    if !node.workflows.is_empty() {
        let items = node.workflows.iter().enumerate().flat_map(|(index, run)| {
            let agents = run.agents.map(|count| match count {
                1 => t!(lang, "1 agent", "1 agente").into(),
                count => tf!(lang, "{count} agents", "{count} agentes", count = count),
            });
            let mut items = vec![Item::Row(run_row(&format!("workflow:{index}"), run, agents, lang))];
            if let Some(workflow) = &run.workflow {
                items.extend(workflow_items(&format!("workflow:{index}"), workflow, lang));
            }
            items
        }).collect();
        sections.push(common::section("workflows", Some(t!(lang, "Workflows", "Flujos de trabajo").into()), items));
    }
    sections
}

/// A node and its one task as one Fleet item. On iOS it reads as the task, names the node only
/// when the name adds something, and says what it does now; the TUI lists the task under the node.
pub(super) fn node_row(agent: &crate::domain::Agent, context: &RenderContext<'_>) -> super::Row {
    let live = context
        .live
        .nodes
        .iter()
        .find(|node| node.agent_id == agent.id.0);
    let name = common::agent_label(&agent.id, context);
    let task = common::current_task(context.domain, &agent.id);
    let mut row = common::row(&agent.id.0, &format!("node:{}", agent.id), Icon::Node, name.clone(), None);
    row.badges.push(node_state(&agent.id, context));
    if context.client == super::Client::Ios {
        if let Some(task) = task {
            row.title = task.title.clone();
            row.subtitle = (task.title != name).then_some(name);
        }
        row.children.extend(live.and_then(|node| activity(node, context)));
        row.actions.extend(interrupt_action(&agent.id.0, context));
        row.actions.extend(super::pacing::node_actions(&agent.id, context));
    } else {
        if let Some(task) = task {
            row.title = task.title.clone();
        }
        row.subtitle = live.filter(|node| common::working(&node.agent_id, context)).and_then(|node| node.tool.clone())
            .or_else(|| (row.title != name).then_some(name));
    }
    row.actions.extend(close_action(&agent.id.0, context));
    let waiting_until = live.and_then(|node| node.waiting_until.clone());
    if let Some(at) = waiting_until {
        row.trailing = Some(Trailing::Time { at, format: super::TimeFormat::Time });
    } else if context.client == super::Client::Tui {
        row.trailing = live.and_then(|node| node.state_since.clone())
            .map(|at| Trailing::Time { at, format: super::TimeFormat::Relative });
    } else {
        row.trailing = Some(Trailing::Count {
            value: context
                .projection
                .inbox
                .iter()
                .filter(|item| item.pending && item.agent_id.as_ref() == Some(&agent.id))
                .count() as u32,
        });
    }
    if let Some(live) = live
        && let Some(text) = &live.background_badge
    {
        row.badges.push(super::Label { text: text.clone(), tone: if live.background_suspect { Tone::Warning } else { Tone::Neutral } });
    }
    row.badges.extend(live.and_then(progress_badge));
    if let Some(model) = live.and_then(|live| live.model.as_deref()) {
        row.badges.push(super::Label { text: crate::model_label::model_label(model), tone: Tone::Neutral });
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
        common::orchestrator_failure(context).and_then(|text| text.lines().next().map(str::to_owned)),
    );
    let state = common::orchestrator_label(context);
    orchestrator.trailing = Some(Trailing::Text { text: state.text.clone() });
    orchestrator.badges.push(state);
    view.sections.push(common::section(
        "orchestrator",
        None,
        vec![Item::Row(orchestrator)],
    ));
    if let Some(section) = pacing_section(context) {
        view.sections.push(section);
    }
    let mut projects: BTreeMap<String, Vec<(u8, Item)>> = BTreeMap::new();
    let mut idle: Vec<(Recency, Item)> = vec![];
    for agent in context.domain.agents.values().filter(|agent| in_fleet(agent, context)) {
        let row = Item::Row(node_row(agent, context));
        match urgency(&agent.id, context) {
            Some(urgency) => projects.entry(project_of(&agent.id, context)).or_default().push((urgency, row)),
            None => idle.push((recency(&agent.id, context), row)),
        }
    }
    let other = t!(context.lang, "Other", "Otros");
    let mut ordered: Vec<_> = projects.into_iter().map(|(project, mut rows)| {
        rows.sort_by_key(|(urgency, _)| *urgency);
        (project, rows)
    }).collect();
    ordered.sort_by_key(|(project, rows)| (rows.first().map(|(urgency, _)| *urgency), project.as_str() == other));
    for (project, rows) in ordered {
        let items = rows.into_iter().map(|(_, item)| item).collect::<Vec<_>>();
        let mut section = common::section(&format!("project:{project}"), Some(project), items);
        section.count = Some(section.items.len() as u32);
        view.sections.push(section);
    }
    if !idle.is_empty() {
        idle.sort_by(|a, b| b.0.cmp(&a.0));
        let items = idle.into_iter().map(|(_, item)| item).collect::<Vec<_>>();
        let mut section = common::section("idle", Some(t!(context.lang, "Idle", "Inactivos").into()), items);
        section.count = Some(section.items.len() as u32);
        section.collapsed = true;
        view.sections.push(section);
    }
    view
}

/// "Working 6/6 · Queued 3 · Mac: memory 12% free (tight)": how many nodes work against the
/// limit and how the Mac is, with every queued task under it in the order it starts.
fn pacing_section(context: &RenderContext<'_>) -> Option<super::Section> {
    let pacing = context.live.pacing.as_ref()?;
    let lang = context.lang;
    let (working, limit, queued) = (pacing.working, pacing.limit, pacing.queued.len());
    let mut parts = vec![tf!(lang, "Working {working}/{limit}", "Trabajando {working}/{limit}")];
    if queued > 0 {
        parts.push(tf!(lang, "Queued {queued}", "En cola {queued}"));
    }
    let paused = pacing.paused.len();
    if paused > 0 {
        parts.push(tf!(lang, "Paused {paused}", "En pausa {paused}"));
    }
    if let Some(free) = pacing.memory_free {
        parts.push(match pacing.tight {
            true => tf!(lang, "Mac: memory {free}% free (tight)", "Mac: memoria {free} % libre (justo)"),
            false => tf!(lang, "Mac: memory {free}% free", "Mac: memoria {free} % libre"),
        });
    }
    let mut header = common::row("pacing", "fleet", Icon::Machine, parts.join(" · "), Some(super::pacing::limit_text(pacing, lang)));
    header.link = None;
    let mut items = vec![Item::Row(header)];
    for place in &pacing.queued {
        let Some(task) = context.domain.tasks.get(&crate::domain::TaskId(place.task_id.clone())) else { continue };
        let mut row = common::row(&format!("queued:{}", task.id.0), &format!("task:{}", task.id.0), Icon::Task,
            task.title.clone(), Some(common::queued_label(place, lang)));
        row.actions.extend(super::pacing::pause_queued_action(task, context));
        items.push(Item::Row(row));
    }
    for paused in &pacing.paused {
        let Some(task) = context.domain.tasks.get(&crate::domain::TaskId(paused.task_id.clone())) else { continue };
        let mut row = common::row(&format!("paused:{}", task.id.0), &format!("task:{}", task.id.0), Icon::Task,
            task.title.clone(), Some(common::paused_label(paused, lang)));
        row.actions.extend(super::pacing::resume_action(paused, context));
        items.push(Item::Row(row));
    }
    Some(common::section("pacing", None, items))
}

/// Where a node stands in Fleet: 0 when it needs the user, 1 while it works, 2 while it waits;
/// `None` when it holds no open task, which puts it among the idle ones.
fn urgency(agent: &AgentId, context: &RenderContext<'_>) -> Option<u8> {
    let live_working = common::working(&agent.0, context);
    let open_task = context.domain.tasks.values()
        .any(|task| task.agent_id.as_ref() == Some(agent) && !task.archived && !task.state.terminal());
    let owes_user = context.projection.inbox.iter()
        .any(|item| item.pending && item.agent_id.as_ref() == Some(agent));
    if !(live_working || open_task || owes_user) {
        return None;
    }
    Some(match node_state(agent, context).tone {
        Tone::Warning | Tone::Critical | Tone::YourTurn | Tone::Accent => 0,
        Tone::Info => 1,
        _ if owes_user => 0,
        _ => 2,
    })
}

type Recency = (Option<String>, u64);

fn recency(agent: &AgentId, context: &RenderContext<'_>) -> Recency {
    let live = context.live.nodes.iter().find(|node| node.agent_id == agent.0);
    let at = live.and_then(|node| node.last_message_at.clone().max(node.state_since.clone()));
    (at, common::current_task(context.domain, agent).map_or(0, |task| task.revision))
}

/// The live node's project; failing that, the project its task's Request is known by through
/// another node on it, or the project its pull request's repository is named after.
fn project_of(agent: &AgentId, context: &RenderContext<'_>) -> String {
    let named = |id: &AgentId| context.live.nodes.iter().find(|node| node.agent_id == id.0)
        .and_then(|node| node.project.as_deref()).filter(|project| *project != "others");
    let from_request = |task: &crate::domain::Task| {
        context.domain.tasks.values().filter(|sibling| sibling.request_id == task.request_id)
            .filter_map(|sibling| sibling.agent_id.as_ref()).find_map(named)
    };
    let from_repository = |task: &crate::domain::Task| {
        let pr = context.domain.pull_requests.get(task.pr_id.as_ref()?)?;
        let name = pr.repository.rsplit('/').next()?;
        context.live.projects.iter().map(|entry| entry.name.as_str())
            .find(|project| project.eq_ignore_ascii_case(name))
    };
    let current = common::current_task(context.domain, agent);
    let latest = context.domain.tasks.values()
        .filter(|task| task.agent_id.as_ref() == Some(agent) && !task.archived)
        .max_by_key(|task| task.revision);
    named(agent)
        .or_else(|| current.and_then(from_request))
        .or_else(|| latest.and_then(from_request))
        .or_else(|| current.and_then(from_repository))
        .unwrap_or(t!(context.lang, "Other", "Otros"))
        .into()
}

pub(super) fn detail(id: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    common::id(id)?;
    if let crate::domain::Target::Orchestrator(_) = context.domain.resolve_target(id) {
        return orchestrator(id, context);
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
    let mut readings = vec![];
    if let Some(node) = live {
        let lang = context.lang;
        if context.domain.node_phase(&AgentId(node.agent_id.clone())) == Some(&NodePhase::Starting { reviving: true }) {
            readings.push(Fact { label: t!(lang, "Connection", "Conexión").into(), value: t!(lang, "Reconnecting", "Reconectando").into(),
                tone: Some(Tone::Info), link: None });
        }
        if let Some(tool) = &node.tool {
            readings.push(Fact { label: t!(lang, "Running", "Ejecutando").into(), value: tool.clone(), tone: None, link: None });
        }
        if node.last_turn_aborted && !common::working(&node.agent_id, context) {
            readings.push(Fact { label: t!(lang, "Last turn", "Último turno").into(),
                value: t!(lang, "Interrupted", "Interrumpido").into(), tone: Some(Tone::Warning), link: None });
        }
        if let Some(model) = &node.model {
            readings.push(Fact { label: t!(lang, "Model", "Modelo").into(), value: crate::model_label::model_label(model), tone: None, link: None });
        }
        if let Some(percent) = node.context_percent {
            let label = context_label(percent, lang);
            let value = match node.context_window {
                Some(window) => tf!(lang, "{percent}% of {window} tokens", "{percent}% de {window} tokens", percent = percent, window = tokens(window)),
                None => format!("{percent}%"),
            };
            readings.push(Fact { label: t!(lang, "Context", "Contexto").into(), value, tone: Some(label.tone), link: None });
        }
        if let Some(cents) = node.cost_cents {
            readings.push(Fact { label: t!(lang, "Cost", "Coste").into(), value: cost(cents, lang), tone: None, link: None });
        }
    }
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
    if let Some(task) = common::current_task(context.domain, &agent.id)
        .filter(|task| !task.state.terminal() && context.client != super::Client::Tui) {
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
    if !readings.is_empty() {
        view.sections.push(common::section("readings", None, vec![Item::Facts(Facts { items: readings })]));
    }
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
        .projection
        .inbox
        .iter()
        .filter(|item| item.pending && item.agent_id.as_ref() == Some(&agent.id))
        .map(|item| super::card::from_item(item, context, false).map(Item::Card))
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
            Item::Row(if context.client == super::Client::Tui {
                super::tasks::task_row(task, context)
            } else {
                common::row(
                    &task.id.0,
                    &format!("task:{}", task.id),
                    Icon::Task,
                    task.title.clone(),
                    None,
                )
            })
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
    if let Some(node) = live {
        view.sections.extend(work_sections(node, context.lang));
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
    let mut actions: Vec<_> = message_action(id, context).into_iter().collect();
    actions.extend(interrupt_action(id, context));
    actions.extend(super::pacing::node_actions(&AgentId(id.into()), context));
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

fn orchestrator(id: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    let mut view = common::view(
        "node",
        &format!("node:{id}"),
        Some(t!(context.lang, "Orchestrator", "Orquestador").into()),
        context,
        vec![],
    );
    let live = context.live.orchestrator.as_ref();
    let state = common::orchestrator_label(context);
    view.sections.push(common::section(
        "header",
        None,
        vec![Item::Facts(Facts {
            items: vec![Fact {
                label: t!(context.lang, "State", "Estado").into(),
                value: state.text,
                tone: Some(state.tone),
                link: None,
            }, Fact {
                label: t!(context.lang, "Queued messages", "Mensajes en cola").into(),
                value: live.map_or(0, |o| o.queue_depth).to_string(),
                tone: None,
                link: None,
            }].into_iter().chain(context.domain.orchestrator_session.as_ref().map(|session| Fact {
                label: t!(context.lang, "Session", "Sesión").into(),
                value: session.0.clone(),
                tone: None,
                link: None,
            })).collect(),
        })],
    ));
    if let Some(failure) = common::orchestrator_failure(context) {
        view.sections.push(common::section(
            "failure",
            None,
            vec![Item::Markdown(Markdown {
                text: failure,
                truncated: false,
                more: None,
                commentable: false,
            })],
        ));
    }
    let mut actions = Vec::new();
    if let Some(mut message) = common::action(Verb::Msg, "view", context.domain.revision,
        t!(context.lang, "Message", "Mensaje"), Style::Primary) {
        message.key = (context.client == super::Client::Tui).then(|| "r".into());
        message.input = super::Input::Text { placeholder: t!(context.lang, "Write to the orchestrator…", "Escribe al orquestador…").into(),
            required: true, multiline: true };
        message.attachments = true;
        actions.push(message);
    }
    actions.extend(orchestrator_decisions(context));
    view.sections.push(common::section("actions", None, vec![Item::Actions(super::ActionBar { actions })]));
    Ok(view)
}

/// What the user can do with the orchestrator's current run: retry or continue past a failed
/// one, or stop one in progress.
pub(super) fn orchestrator_decisions(context: &RenderContext<'_>) -> Vec<super::Action> {
    let live = context.live.orchestrator.as_ref();
    let mut actions = Vec::new();
    if let Some(run) = live.and_then(|o| o.active_run_id.as_deref()) {
        for (enabled, verb, label) in [
            (live.is_some_and(|o| o.can_retry), Verb::OrchRetry, t!(context.lang, "Retry", "Reintentar")),
            (live.is_some_and(|o| o.can_retry), Verb::OrchAck, t!(context.lang, "Continue without repeating", "Continuar sin repetir")),
            (live.is_some_and(|o| o.can_cancel), Verb::OrchStop, t!(context.lang, "Stop", "Detener")),
        ] {
            if !enabled { continue; }
            let style = if verb == Verb::OrchStop { Style::Destructive } else { Style::Secondary };
            if let Some(mut action) = common::action(verb, "orchestrator", context.domain.revision, label, style) {
                action.key = (context.client == super::Client::Tui).then(|| match verb {
                    Verb::OrchRetry => "R",
                    Verb::OrchAck => "C",
                    _ => "S",
                }.into());
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
    actions
}
