//! The user's hands on the fleet's pace: the working-node limit, Pause, Resume and a task's
//! priority. The daemon's gate does the rest (`docs/fleet-pacing.md`).

use super::{
    ActionOption, Confirm, Input, LivePacing, LivePaused, Presentation, RenderContext, Style, action_id::Verb, common,
    text::{t, tf},
};
use crate::domain::{AgentId, Task, TaskPriority};
use crate::options::MAX_WORKING_NODES;

/// The TUI numbers a menu's entries 1 to 9.
const TUI_MENU: u32 = 9;

fn option(id: String, label: String, description: Option<String>) -> ActionOption {
    ActionOption { id, label, description, style: Style::Secondary, input: Input::None, key: None }
}

/// "Limit: automatic (6)" or "Limit: 4, set by you": where the number comes from.
pub(super) fn limit_text(pacing: &LivePacing, lang: super::Lang) -> String {
    let auto = pacing.auto;
    match pacing.fixed {
        None => tf!(lang, "Limit: automatic ({auto})", "Límite: automático ({auto})"),
        Some(fixed) => tf!(lang, "Limit: {fixed}, set by you · automatic would be {auto}",
            "Límite: {fixed}, fijado por ti · el automático sería {auto}"),
    }
}

/// "Nodes at once": automatic or a fixed number from 1 to 12, written to `options.toml`.
pub(super) fn limit_action(pacing: &LivePacing, context: &RenderContext<'_>) -> Option<super::Action> {
    let lang = context.lang;
    let mut action = common::action(Verb::SetLimit, "limit", context.domain.revision,
        t!(lang, "Nodes at once", "Nodos a la vez"), Style::Secondary)?;
    action.presentation = Presentation::Menu;
    let auto = pacing.auto;
    let mut fixed: Vec<u32> = (1..=MAX_WORKING_NODES).collect();
    if context.client == super::Client::Tui {
        let window = TUI_MENU - 1;
        let current = pacing.fixed.unwrap_or(auto);
        let start = current.saturating_sub(window / 2).clamp(1, MAX_WORKING_NODES - window + 1);
        fixed = (start..start + window).collect();
        action.key = Some("L".into());
    }
    action.options.push(option("auto".into(), tf!(lang, "Automatic ({auto})", "Automático ({auto})"),
        Some(t!(lang, "From this Mac's cores and memory", "Según los núcleos y la memoria de este Mac").into())));
    action.options.extend(fixed.into_iter().map(|count| {
        let label = match pacing.fixed == Some(count) {
            true => tf!(lang, "{count} · now", "{count} · ahora"),
            false => count.to_string(),
        };
        option(count.to_string(), label, None)
    }));
    if pacing.fixed.is_none() {
        action.options[0].label = tf!(lang, "Automatic ({auto}) · now", "Automático ({auto}) · ahora");
    }
    Some(action)
}

fn pause_label(lang: super::Lang) -> &'static str {
    t!(lang, "Pause", "Pausar")
}

/// Takes a queued task out of the queue; it stays out until the user resumes it.
pub(super) fn pause_queued_action(task: &Task, context: &RenderContext<'_>) -> Option<super::Action> {
    common::queued(task, context)?;
    let mut action = common::action(Verb::Pause, &task.id.0, context.domain.revision, pause_label(context.lang), Style::Secondary)?;
    action.key = (context.client == super::Client::Tui).then(|| "p".into());
    action.stamp = Some(t!(context.lang, "Paused", "En pausa").into());
    Some(action)
}

/// Stops a working node and keeps it stopped until the user resumes it.
pub(super) fn pause_node_action(agent: &AgentId, context: &RenderContext<'_>) -> Option<super::Action> {
    context.live.nodes.iter().find(|node| node.agent_id == agent.0).filter(|node| node.working)?;
    common::current_task(context.domain, agent)?;
    if paused_node(&agent.0, context).is_some() {
        return None;
    }
    let lang = context.lang;
    let mut action = common::action(Verb::Pause, &agent.0, context.domain.revision, pause_label(lang), Style::Secondary)?;
    action.key = (context.client == super::Client::Tui).then(|| "p".into());
    action.stamp = Some(t!(lang, "Paused", "En pausa").into());
    action.confirm = Some(Confirm {
        title: t!(lang, "Pause this node?", "¿Pausar este nodo?").into(),
        message: t!(lang, "It stops what it is running now and keeps its task. Alondra does not resume it by itself: only you do.",
            "Deja lo que está ejecutando ahora y conserva su tarea. Alondra no lo reanuda solo: solo tú.").into(),
        button: pause_label(lang).into(),
    });
    Some(action)
}

fn paused_node<'a>(session: &str, context: &'a RenderContext<'_>) -> Option<&'a LivePaused> {
    context.live.pacing.as_ref()?.paused.iter().find(|paused| paused.session.as_deref() == Some(session))
}

/// Resumes a paused node or task now, even over the limit (D3).
pub(super) fn resume_action(paused: &LivePaused, context: &RenderContext<'_>) -> Option<super::Action> {
    let subject = paused.session.as_deref().unwrap_or(&paused.task_id);
    let lang = context.lang;
    let mut action = common::action(Verb::Resume, subject, context.domain.revision, t!(lang, "Resume", "Reanudar"), Style::Primary)?;
    action.key = (context.client == super::Client::Tui).then(|| "u".into());
    action.stamp = Some(t!(lang, "Resuming…", "Reanudando…").into());
    action.description = Some(t!(lang, "Now, even over the limit", "Ahora, aunque se pase del límite").into());
    Some(action)
}

/// Pause or Resume, whichever the task's place in the fleet allows.
pub(super) fn task_actions(task: &Task, context: &RenderContext<'_>) -> Vec<super::Action> {
    if let Some(paused) = common::paused(task, context) {
        return resume_action(paused, context).into_iter().collect();
    }
    if let Some(pause) = pause_queued_action(task, context) {
        return vec![pause];
    }
    task.agent_id.as_ref().and_then(|agent| pause_node_action(agent, context)).into_iter().collect()
}

/// Pause or Resume for a node, from its row or its detail.
pub(super) fn node_actions(agent: &AgentId, context: &RenderContext<'_>) -> Vec<super::Action> {
    match paused_node(&agent.0, context) {
        Some(paused) => resume_action(paused, context).into_iter().collect(),
        None => pause_node_action(agent, context).into_iter().collect(),
    }
}

pub(super) fn priority_text(priority: TaskPriority, lang: super::Lang) -> &'static str {
    match priority {
        TaskPriority::High => t!(lang, "High", "Alta"),
        TaskPriority::Normal => t!(lang, "Normal", "Normal"),
        TaskPriority::Low => t!(lang, "Low", "Baja"),
    }
}

/// High, Normal or Low: where the task's node stands when nodes wait for a free slot.
pub(super) fn priority_action(task: &Task, context: &RenderContext<'_>) -> Option<super::Action> {
    if task.state.terminal() {
        return None;
    }
    let lang = context.lang;
    let mut action = common::action(Verb::SetPriority, &task.id.0, context.domain.revision,
        t!(lang, "Priority", "Prioridad"), Style::Secondary)?;
    action.presentation = Presentation::Menu;
    action.key = (context.client == super::Client::Tui).then(|| "P".into());
    action.options = [
        (TaskPriority::High, "high", t!(lang, "Starts before Normal and Low", "Arranca antes que Normal y Baja")),
        (TaskPriority::Normal, "normal", t!(lang, "The default", "La de siempre")),
        (TaskPriority::Low, "low", t!(lang, "Starts when nothing else waits", "Arranca cuando no espera nada más")),
    ].into_iter().map(|(priority, id, description)| {
        let name = priority_text(priority, lang);
        let label = match priority == task.priority {
            true => tf!(lang, "{name} · now", "{name} · ahora"),
            false => name.to_owned(),
        };
        option(id.into(), label, Some(description.into()))
    }).collect();
    Some(action)
}
