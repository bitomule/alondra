use super::{
    Confirm, Fact, Facts, Icon, Item, Markdown, RenderContext, StepState, Style, Tone, Trailing,
    action_id::Verb, common, text::{t, tf},
};
use crate::domain::{AgentId, NodePhase, TaskId};

/// Stopping a node's current turn, offered only while it works: for a node that runs through the
/// SDK, while its execution runs a turn or work of its own. Once Claude accepted the interrupt and
/// the turn still runs, stopping it means ending Claude's process; once that was asked, nothing
/// more is offered until the process exits.
pub(super) fn interrupt_action(id: &str, context: &RenderContext<'_>) -> Option<super::Action> {
    let node = context.index.node(&AgentId(id.into()));
    match node.filter(|node| node.through_sdk).and_then(|node| node.sdk_turn_stop) {
        Some(alondra_domain::TurnStop::InterruptAccepted) => return force_stop_action(id, context),
        Some(alondra_domain::TurnStop::ForceRequested) => return None,
        Some(alondra_domain::TurnStop::Running) | None => {}
    }
    let working = match node {
        Some(node) if node.through_sdk => node.sdk_running,
        _ => common::working(id, context),
    };
    working.then_some(())?;
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

fn force_stop_action(id: &str, context: &RenderContext<'_>) -> Option<super::Action> {
    let mut action = common::action(Verb::ForceStop, id, context.domain.revision,
        t!(context.lang, "Force stop", "Forzar parada"), Style::Destructive)?;
    action.key = (context.client == super::Client::Tui).then(|| "i".into());
    action.stamp = Some(t!(context.lang, "Stopping…", "Parando…").into());
    action.confirm = Some(Confirm {
        title: action.label.clone(),
        message: t!(context.lang, "Claude accepted the interrupt but keeps going. End its process now? The task keeps its conversation; your next message resumes it.",
            "Claude aceptó la interrupción pero sigue. ¿Terminar su proceso ahora? La tarea conserva su conversación; tu próximo mensaje la retoma.").into(),
        button: action.label.clone(),
    });
    Some(action)
}

/// A node works on its task while Alondra runs it on an open task in a session that exists or that it is
/// restarting: never the orchestrator, a node whose task ended, nor one whose session is lost and
/// waits for the gate to be restarted.
pub(super) fn on_open_task(agent: &crate::domain::Agent, context: &RenderContext<'_>) -> bool {
    agent.id.0 != "orchestrator"
        && context.domain.orchestrator_session.as_ref() != Some(&agent.id)
        && agent.phase.live()
        && agent.phase != NodePhase::Lost
        && node_tasks(&agent.id, context).any(|task| !task.state.terminal())
}

/// A node's tasks that are not archived.
pub(super) fn node_tasks<'a>(agent: &AgentId, context: &RenderContext<'a>) -> impl Iterator<Item = &'a crate::domain::Task> {
    let domain = context.domain;
    context.index.node(agent).into_iter().flat_map(|node| &node.tasks).filter_map(move |id| domain.tasks.get(id))
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

/// What a live node reads now: the tool it runs, its background work, a last turn that was
/// interrupted, its context and its cost.
pub(super) fn node_readings(node: &super::LiveNode, context: &RenderContext<'_>) -> Vec<Fact> {
    let lang = context.lang;
    let mut readings = vec![];
    if let Some(tool) = &node.tool {
        readings.push(Fact { label: t!(lang, "Running", "Ejecutando").into(), value: tool.clone(), tone: None, link: None });
    }
    if let Some(text) = &node.background_badge {
        readings.push(Fact { label: t!(lang, "In the background", "En segundo plano").into(), value: text.clone(),
            tone: node.background_suspect.then_some(Tone::Warning), link: None });
    }
    if node.last_turn_aborted && !common::working(&node.agent_id, context) {
        readings.push(Fact { label: t!(lang, "Last turn", "Último turno").into(),
            value: t!(lang, "Interrupted", "Interrumpido").into(), tone: Some(Tone::Warning), link: None });
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
    readings
}

pub(super) fn folder(node: &super::LiveNode, context: &RenderContext<'_>) -> Option<Fact> {
    node.cwd.clone().map(|cwd| Fact { label: t!(context.lang, "Folder", "Carpeta").into(), value: cwd, tone: None, link: None })
}

pub(super) fn needs_section(node: &super::LiveNode, context: &RenderContext<'_>) -> Option<super::Section> {
    let needs = node.needs.as_ref().filter(|needs| !needs.is_empty())?;
    Some(common::section("needs", Some(t!(context.lang, "What it needs", "Qué necesita").into()),
        vec![Item::Markdown(Markdown { text: needs.clone(), truncated: false, more: None, commentable: false })]))
}

pub(super) fn background_section(node: &super::LiveNode, context: &RenderContext<'_>) -> Option<super::Section> {
    if node.background.is_empty() {
        return None;
    }
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
    Some(common::section("background", Some(t!(context.lang, "Running in the background", "En segundo plano").into()), rows))
}

pub(super) fn last_message_section(node: &super::LiveNode, context: &RenderContext<'_>) -> Option<super::Section> {
    let message = node.last_message.as_ref().filter(|text| !text.trim().starts_with("mcp__") && !text.trim().is_empty())?;
    let mut items = vec![Item::Markdown(Markdown { text: message.clone(), truncated: false, more: None, commentable: false })];
    if context.client == super::Client::Ios
        && let Some(at) = node.last_message_at.as_ref() {
        let mut row = common::row("last_message_time", "node", Icon::History,
            t!(context.lang, "Last message", "Último mensaje").into(), None);
        row.link = None;
        row.trailing = Some(Trailing::Time { at: at.clone(), format: super::TimeFormat::Relative });
        items.push(Item::Row(row));
    }
    Some(common::section("last_message", Some(t!(context.lang, "What it said last", "Lo último que dijo").into()), items))
}

/// What a node does right now, one row per running thread, while it is in a turn. A subagent's row
/// says so, and each names the task it works for by its title, but in that task's own detail
/// (`shown`).
pub(super) fn activity_rows(agent: &crate::domain::Agent, context: &RenderContext<'_>, shown: Option<&TaskId>) -> Option<Vec<Item>> {
    let activity = context.live.activity.get(&agent.id.0).filter(|items| !items.is_empty() && agent.phase.in_turn())?;
    Some(activity.iter().map(|entry| {
        let title = entry.summary.clone().or_else(|| entry.tool.clone())
            .unwrap_or_else(|| t!(context.lang, "Working", "Trabajando").into());
        let subagent = entry.agent_id.as_ref().map(|_| match &entry.agent_type {
            Some(kind) => format!("{} · {kind}", t!(context.lang, "Subagent", "Subagente")),
            None => t!(context.lang, "Subagent", "Subagente").into(),
        });
        let task = entry.task.as_ref().filter(|task| shown.is_none_or(|shown| shown.0 != **task))
            .map(|task| context.domain.tasks.get(&TaskId(task.clone())).map_or_else(|| task.clone(), |known| known.title.clone()))
            .map(|task| format!("{} {task}", t!(context.lang, "Task", "Tarea")));
        let subtitle = match (subagent, task) {
            (Some(subagent), Some(task)) => Some(format!("{subagent} · {task}")),
            (subagent, task) => subagent.or(task),
        };
        let mut row = common::row(&entry.since, "node", Icon::History, title, subtitle);
        row.link = None;
        row.trailing = Some(Trailing::Time { at: entry.since.clone(), format: super::TimeFormat::Relative });
        Item::Row(row)
    }).collect())
}
