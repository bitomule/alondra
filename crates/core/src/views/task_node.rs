use super::{
    Confirm, Fact, Facts, Icon, Item, Markdown, RenderContext, StepState, Style, Tone, Trailing,
    action_id::Verb, common, text::{t, tf},
};
use crate::domain::{AgentId, TaskId};

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
        Some(node) if node.through_sdk => node.sdk_interruptible,
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
        && match common::sdk_live(&agent.id, context) {
            Some(live) => live.is_some(),
            None => agent.phase.live(),
        }
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

pub(super) fn cost(cents: u64, lang: super::Lang) -> String {
    let (units, rest) = (cents / 100, cents % 100);
    tf!(lang, "${units}.{rest:02}", "{units},{rest:02} $", units = units, rest = rest)
}

fn context_label(percent: u32, lang: super::Lang) -> super::Label {
    super::Label {
        text: tf!(lang, "Context {percent}%", "Contexto {percent}%", percent = percent),
        tone: if percent >= 80 { Tone::Warning } else { Tone::Neutral },
        elapsed: None,
    }
}

/// A live node says what runs now, alongside its own step; a stopped node quotes its last sentence.
pub(super) fn doing(node: &super::LiveNode, working: bool, lang: super::Lang) -> Option<(String, Option<String>)> {
    if working {
        let now = super::now_fact::now_fact(Some(node), &crate::domain::Doing::Turn,
            node.state_since.as_deref().unwrap_or("—"), lang).text(lang);
        return Some((step(node, lang).map_or_else(|| now.clone(), |step| format!("{step} · {now}")), None));
    }
    if let Some(step) = step(node, lang) {
        return Some((step, None));
    }
    node.last_message.as_deref().and_then(said).map(|sentence| (
        tf!(lang, "Said: “{sentence}”", "Dijo: «{sentence}»"),
        node.last_message_at.clone(),
    ))
}

/// "Step 3 of 4 · Generate iOS screenshots", while the node's own task list has a step in progress:
/// its number counts the steps already completed, wherever they sit in the list.
pub(super) fn step(node: &super::LiveNode, lang: super::Lang) -> Option<String> {
    let step = node.steps.iter().find(|step| step.state == StepState::InProgress)?;
    let done = node.steps.iter().filter(|step| step.state == StepState::Completed).count();
    let (number, total, subject) = (done + 1, node.steps.len(), &step.subject);
    Some(tf!(lang, "Step {number} of {total} · {subject}", "Paso {number} de {total} · {subject}"))
}

const SAID_LIMIT: usize = 60;

/// The first sentence of what a node said, when it is prose: not a tool, a path or code. Its Markdown
/// headings are skipped and a list item or quote loses its mark.
pub(super) fn said(message: &str) -> Option<String> {
    let line = message.lines().map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && line.chars().any(char::is_alphanumeric))
        .map(item_text)
        .find(|line| !line.is_empty())?;
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

fn item_text(mut line: &str) -> &str {
    loop {
        let numbered = line.find(|c: char| !c.is_ascii_digit())
            .filter(|&at| at > 0 && line[at..].starts_with(['.', ')']))
            .map(|at| &line[at + 1..]);
        match numbered.or_else(|| line.strip_prefix(['-', '*', '+', '>'])) {
            Some(rest) if rest.is_empty() || rest.starts_with(' ') => line = rest.trim_start(),
            _ => return line,
        }
    }
}

const TOOLS: [&str; 12] = ["Bash", "Edit", "Write", "MultiEdit", "NotebookEdit", "Read", "Grep", "Glob", "Agent", "Task", "WebSearch", "WebFetch"];

fn ran_for(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, secs % 3600 / 60),
    }
}

/// The badge and the time of something running in the background: since when it runs, or how
/// it ended and how long it ran.
fn with_run_state(row: &mut super::Row, state: super::RunState, since: &str, lang: super::Lang) {
    use super::{RunOutcome, RunState};
    let (text, tone) = match state {
        RunState::Running => (t!(lang, "Running", "En marcha"), Tone::Info),
        RunState::Ended { outcome: RunOutcome::Finished, .. } => (t!(lang, "Finished", "Terminado"), Tone::Neutral),
        RunState::Ended { outcome: RunOutcome::Failed, .. } => (t!(lang, "Failed", "Falló"), Tone::Critical),
        RunState::Ended { outcome: RunOutcome::Stopped, .. } => (t!(lang, "Stopped", "Detenido"), Tone::Neutral),
    };
    row.badges.push(super::Label { text: text.into(), tone, elapsed: None });
    row.trailing = Some(match state {
        RunState::Running => Trailing::Time { at: since.to_owned(), format: super::TimeFormat::Relative },
        RunState::Ended { ran_secs, .. } => Trailing::Text { text: ran_for(ran_secs) },
    });
}

fn run_row(id: &str, run: &super::LiveRun, subtitle: Option<String>, lang: super::Lang) -> super::Row {
    let mut row = common::row(id, "node", Icon::History, run.label.clone(), subtitle);
    row.link = None;
    with_run_state(&mut row, run.state, &run.since, lang);
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
        facts.push(Fact { kind: None, label: t!(lang, "Phase", "Fase").into(), value, tone: None, link: None });
    }
    if workflow.running + workflow.done > 0 {
        let value = tf!(lang, "{running} running · {done} done", "{running} en marcha · {done} terminados",
            running = workflow.running, done = workflow.done);
        let tone = if workflow.running > 0 { Tone::Info } else { Tone::Success };
        facts.push(Fact { kind: None, label: t!(lang, "Agents", "Agentes").into(), value, tone: Some(tone), link: None });
    }
    if let Some(count) = workflow.total_tokens {
        facts.push(Fact { kind: None, label: t!(lang, "Tokens", "Tokens").into(), value: tokens(count), tone: None, link: None });
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
            super::Label { text: t!(lang, "Running", "En marcha").into(), tone: Tone::Info, elapsed: None }
        } else {
            super::Label { text: t!(lang, "Done", "Hecho").into(), tone: Tone::Neutral, elapsed: None }
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
            kind: None,
            label: t!(lang, "Progress", "Progreso").into(),
            value: tf!(lang, "{done} of {total}", "{done} de {total}", done = done, total = node.steps.len()),
            tone: Some(if done == node.steps.len() { Tone::Success } else { Tone::Info }),
            link: None,
        }];
        if let Some(step) = node.steps.iter().find(|step| step.state == StepState::InProgress) {
            facts.push(Fact { kind: None, label: t!(lang, "Now", "Ahora").into(),
                value: step.active_form.clone().unwrap_or_else(|| step.subject.clone()), tone: None, link: None });
        }
        let mut items = vec![Item::Facts(Facts { items: facts })];
        items.extend(node.steps.iter().enumerate().map(|(index, step)| {
            let icon = if step.state == StepState::Completed { Icon::Check } else { Icon::Task };
            let mut row = common::row(&format!("step:{index}"), "node", icon, step.subject.clone(), None);
            row.link = None;
            row.struck = step.state == StepState::Completed;
            if step.state == StepState::InProgress {
                row.badges.push(super::Label { text: t!(lang, "In progress", "En curso").into(), tone: Tone::Info, elapsed: None });
            }
            Item::Row(row)
        }));
        sections.push(common::section("steps", Some(t!(lang, "Steps", "Pasos").into()), items));
    }
    let subagents = node.subagents.iter().enumerate().map(|(index, run)| {
        let about = [run.kind.clone(), run.model.as_deref().map(crate::model_label::model_label)]
            .into_iter().flatten().collect::<Vec<_>>().join(" · ");
        (run.state, Item::Row(run_row(&format!("subagent:{index}"), run, (!about.is_empty()).then_some(about), lang)))
    }).collect();
    sections.extend(running_then_finished("subagents", t!(lang, "Subagents", "Subagentes"),
        t!(lang, "Finished subagents", "Subagentes terminados"), subagents));
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
/// interrupted and its context.
pub(super) fn node_readings(node: &super::LiveNode, context: &RenderContext<'_>) -> Vec<Fact> {
    let lang = context.lang;
    let mut readings = vec![];
    if let Some(tool) = &node.tool {
        let value = match &node.tool_since {
            Some(at) => tf!(lang, "{tool} · since {at}", "{tool} · desde las {at}", tool = tool, at = at),
            None => tool.clone(),
        };
        readings.push(Fact { kind: None, label: t!(lang, "Running", "Ejecutando").into(), value, tone: None, link: None });
    }
    if let Some(text) = &node.background_badge {
        readings.push(Fact { kind: None, label: t!(lang, "In the background", "En segundo plano").into(), value: text.clone(),
            tone: None, link: None });
    }
    if node.last_turn_aborted && !common::working(&node.agent_id, context) {
        readings.push(Fact { kind: None, label: t!(lang, "Last turn", "Último turno").into(),
            value: t!(lang, "Interrupted", "Interrumpido").into(), tone: Some(Tone::Warning), link: None });
    }
    if let Some(percent) = node.context_percent {
        let label = context_label(percent, lang);
        let value = match node.context_window {
            Some(window) => tf!(lang, "{percent}% of {window} tokens", "{percent}% de {window} tokens", percent = percent, window = tokens(window)),
            None => format!("{percent}%"),
        };
        readings.push(Fact { kind: None, label: t!(lang, "Context", "Contexto").into(), value, tone: Some(label.tone), link: None });
    }
    readings
}

pub(super) fn needs_section(node: &super::LiveNode, context: &RenderContext<'_>) -> Option<super::Section> {
    let needs = node.needs.as_ref().filter(|needs| !needs.is_empty())?;
    Some(common::section("needs", Some(t!(context.lang, "What it needs", "Qué necesita").into()),
        vec![Item::Markdown(Markdown { text: needs.clone(), truncated: false, more: None, commentable: false })]))
}

/// What runs now in `id`; once two or more have ended, those go to `{id}_finished`, folded, so the
/// section shows what is happening. A single ended one stays in place: folding it saves no room.
fn running_then_finished(id: &str, title: &str, finished_title: &str, rows: Vec<(super::RunState, Item)>) -> Vec<super::Section> {
    if rows.is_empty() {
        return vec![];
    }
    let ended = rows.iter().filter(|(state, _)| *state != super::RunState::Running).count();
    if ended < 2 {
        return vec![common::section(id, Some(title.into()), rows.into_iter().map(|(_, item)| item).collect())];
    }
    let (running, finished): (Vec<_>, Vec<_>) = rows.into_iter().partition(|(state, _)| *state == super::RunState::Running);
    let mut sections = vec![];
    if !running.is_empty() {
        sections.push(common::section(id, Some(title.into()), running.into_iter().map(|(_, item)| item).collect()));
    }
    let mut folded = common::section(&format!("{id}_finished"), Some(finished_title.into()),
        finished.into_iter().map(|(_, item)| item).collect());
    folded.collapsed = true;
    folded.count = Some(ended as u32);
    sections.push(folded);
    sections
}

pub(super) fn background_sections(node: &super::LiveNode, context: &RenderContext<'_>) -> Vec<super::Section> {
    let rows = node.background.iter().map(|background| {
        let mut row = common::row(&background.since, "node", Icon::History, background.label.clone(),
            Some(match background.kind.as_str() {
                "agent" => t!(context.lang, "Subagent", "Subagente"),
                "shell" => t!(context.lang, "Shell", "Shell"),
                _ => t!(context.lang, "Task", "Tarea"),
            }.into()));
        row.link = None;
        with_run_state(&mut row, background.state, &background.since, context.lang);
        (background.state, Item::Row(row))
    }).collect();
    running_then_finished("background", t!(context.lang, "In the background", "En segundo plano"),
        t!(context.lang, "Finished in the background", "Terminado en segundo plano"), rows)
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
    let activity = context.live.activity.get(&agent.id.0).filter(|items| !items.is_empty()
        && matches!(common::sdk_live(&agent.id, context), Some(Some(alondra_domain::LiveState::Working(alondra_domain::Worker::Turn)))))?;
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
