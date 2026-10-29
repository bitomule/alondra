use super::{
    Action, BuildError, Input, Item, Link, Presentation, RenderContext, Row, Section, Style,
    VIEWS_PROTOCOL, View,
    action_id::{ActionId, Verb},
    text::t,
};

pub(super) fn view(
    name: &str,
    key: &str,
    title: Option<String>,
    context: &RenderContext<'_>,
    sections: Vec<Section>,
) -> View {
    View {
        protocol: VIEWS_PROTOCOL,
        data_epoch: context.domain.data_epoch.0.clone(),
        view: name.into(),
        key: key.into(),
        client: context.client,
        version: String::new(),
        cursor: context.cursor.into(),
        title,
        status: super::status::build(context),
        sections,
        empty: None,
        notices: vec![],
        service: None,
        account_configuration: None,
        first_cards: vec![],
        summary: None,
    }
}

pub(super) fn section(id: &str, title: Option<String>, items: Vec<Item>) -> Section {
    Section {
        id: id.into(),
        title,
        count: None,
        collapsed: false,
        items,
        card_ids: vec![],
        card_waits: vec![],
        more: None, style: None, actions: vec![],
    }
}

pub(super) fn row(
    id: &str,
    key: &str,
    icon: super::Icon,
    title: String,
    subtitle: Option<String>,
) -> Row {
    Row {
        account: None, model: None,
        id: id.into(),
        link: Some(Link::View { key: key.into() }),
        icon,
        title,
        subtitle,
        badges: vec![],
        trailing: None,
        depth: 0,
        struck: false,
        kind: None,
        children: vec![],
        folded: false,
        actions: vec![],
    }
}

pub(super) fn state_tone(state: &str) -> super::Tone {
    use super::Tone;
    match state {
        "needs_you" => Tone::Warning,
        "your_turn" => Tone::YourTurn,
        "in_review" | "for_your_review" | "awaiting_blocker" | "waiting_merge" | "waiting_machine"
        | "waiting_dependency" | "closing" | "defining" => Tone::Info,
        "running" | "working" | "busy" | "free" | "waiting_answer" => Tone::Accent,
        "done" | "delivered" => Tone::Success,
        "cancelled" | "not_started" | "connected" | "idle" => Tone::Neutral,
        "offline" => Tone::Critical,
        _ => Tone::Neutral,
    }
}

pub(super) fn effort_label(effort: &str, lang: super::Lang) -> Option<&'static str> {
    Some(match effort {
        "auto" => t!(lang, "Auto", "Automático"),
        "light" => t!(lang, "Light", "Ligero"),
        "normal" => t!(lang, "Normal", "Normal"),
        "hard" => t!(lang, "Hard", "A fondo"),
        "none" => t!(lang, "None", "Ninguno"),
        "minimal" => t!(lang, "Minimal", "Mínimo"),
        "low" => t!(lang, "Low", "Bajo"),
        "medium" => t!(lang, "Medium", "Medio"),
        "high" => t!(lang, "High", "Alto"),
        "xhigh" => t!(lang, "Extra high", "Muy alto"),
        "max" => t!(lang, "Maximum", "Máximo"),
        "ultra" => t!(lang, "Ultra", "Ultra"),
        _ => return None,
    })
}

/// A working Task whose delivery Alondra sent back says what it told the node to do.
pub(super) fn instruction_label(task: &crate::domain::Task, domain: &crate::domain::DomainState, lang: super::Lang) -> Option<&'static str> {
    use crate::domain::{Instruction, TaskState};
    if task.state != TaskState::Running { return None; }
    if domain.task_merge_failure(&task.id).is_some() {
        return Some(super::text::t!(lang, "Alondra couldn't merge it", "Alondra no pudo hacer el merge"));
    }
    Some(match domain.task_handed_back(&task.id)? {
        Instruction::Changes => super::text::t!(lang, "Changes requested", "Cambios solicitados"),
        Instruction::Conflicts => super::text::t!(lang, "Resolving conflicts", "Resolviendo conflictos"),
        Instruction::Question => super::text::t!(lang, "Answering your question", "Respondiendo a tu pregunta"),
        Instruction::Reply => super::text::t!(lang, "Acting on your reply", "Atendiendo tu respuesta"),
    })
}

/// The task's place in the queue of starts waiting for the gate, if it waits there.
pub(super) fn queued<'a>(task: &crate::domain::Task, context: &'a RenderContext<'_>) -> Option<&'a super::LiveQueued> {
    if task.agent_id.is_some() || task.state.terminal() { return None; }
    context.live.pacing.as_ref()?.queued.iter().find(|queued| queued.task_id == task.id.0)
}

/// The place in the gate's queue of a task whose node lost its session and waits there to be
/// restarted. Kept apart from `queued`: such a task is no launch the user can pause or release.
pub(super) fn coming_back_queued<'a>(task: &crate::domain::Task, context: &'a RenderContext<'_>) -> Option<&'a super::LiveQueued> {
    let agent = task.agent_id.as_ref()?;
    (context.domain.node_phase(agent) == Some(&crate::domain::NodePhase::Lost)).then_some(())?;
    gate_queued(task, context)
}

/// The place in the gate's queue of a task whose node the user resumed: it stays paused until the
/// gate has a slot for it.
pub(super) fn resume_queued<'a>(task: &crate::domain::Task, context: &'a RenderContext<'_>) -> Option<&'a super::LiveQueued> {
    let agent = task.agent_id.as_ref()?;
    context.domain.node_pause(agent)?;
    gate_queued(task, context)
}

/// The place in the gate's queue of a task that waits there, whether it has a node or not.
pub(super) fn gate_queued<'a>(task: &crate::domain::Task, context: &'a RenderContext<'_>) -> Option<&'a super::LiveQueued> {
    context.live.pacing.as_ref()?.queued.iter().find(|queued| queued.task_id == task.id.0)
}

/// The free space and what one more build needs, when the gate holds the task's start or its
/// node's return for room on the disk: the task then reads `WaitingCapacity(Disk)`, whatever its
/// lifecycle says.
pub(super) fn held_for_disk(task: &crate::domain::Task, context: &RenderContext<'_>) -> Option<(u64, u64)> {
    match gate_queued(task, context)?.hold {
        super::LiveHold::Disk { free, needed } => Some((free, needed)),
        _ => None,
    }
}

/// «Disk · 3.5 GB free of 13.0 GB needed».
pub(super) fn disk_room_fact(free: u64, needed: u64, lang: super::Lang) -> String {
    use crate::machine_incident::bytes_text;
    let (free, needed) = (bytes_text(free, lang), bytes_text(needed, lang));
    super::text::tf!(lang, "Disk · {free} free of {needed} needed", "Disco · {free} libres de {needed} necesarios")
}

/// What the task of a node that runs through the Agent SDK does, from its lifecycle; `None` for a
/// node of `claude --bg`, whose phase says it.
pub(super) fn sdk_live<'a>(agent: &crate::domain::AgentId, context: &'a RenderContext<'_>) -> Option<Option<&'a alondra_domain::LiveState>> {
    let node = context.index.node(agent).filter(|node| node.through_sdk)?;
    Some(node.task.as_ref().and_then(|task| context.reading(task)).and_then(|read| read.live.as_ref()))
}

pub(super) fn ordinal(position: usize, lang: super::Lang) -> String {
    match lang {
        super::Lang::Es => format!("{position}.º"),
        super::Lang::En => {
            let suffix = match (position % 10, position % 100) {
                (1, 11) | (2, 12) | (3, 13) => "th",
                (1, _) => "st",
                (2, _) => "nd",
                (3, _) => "rd",
                _ => "th",
            };
            format!("{position}{suffix}")
        }
    }
}

/// "Queued · 2nd of 3 · 6 of 6 working", or why the Mac holds every start.
pub(super) fn queued_label(queued: &super::LiveQueued, lang: super::Lang) -> String {
    use super::{LiveHold, text::tf};
    let place = ordinal(queued.position, lang);
    let of = queued.of;
    match &queued.hold {
        LiveHold::Slots { working, limit } => tf!(lang, "Queued · {place} of {of} · {working} of {limit} working",
            "En cola · {place} de {of} · hay {working} de {limit} trabajando"),
        LiveHold::Memory { free: Some(free) } => tf!(lang, "Queued · {place} of {of} · the Mac is short of memory ({free}% free)",
            "En cola · {place} de {of} · el Mac va justo de memoria ({free} % libre)"),
        LiveHold::Memory { free: None } => tf!(lang, "Queued · {place} of {of} · the Mac is short of memory",
            "En cola · {place} de {of} · el Mac va justo de memoria"),
        LiveHold::Cpu { load, cpus } => tf!(lang, "Queued · {place} of {of} · the Mac is very busy (load {load} on {cpus} cores)",
            "En cola · {place} de {of} · el Mac está muy cargado (carga {load} en {cpus} núcleos)"),
        LiveHold::Next if queued.position == 1 => tf!(lang, "Queued · next · starts within a minute",
            "En cola · la siguiente · arranca en menos de un minuto"),
        LiveHold::Next => tf!(lang, "Queued · {place} of {of}", "En cola · {place} de {of}"),
        LiveHold::Disk { free, needed } => {
            let fact = disk_room_fact(*free, *needed, lang);
            tf!(lang, "Waiting for capacity · {fact}", "Esperando capacidad · {fact}")
        }
    }
}

/// The node's turn is running, as its task's lifecycle says for a node of the Agent SDK and its
/// phase for one of `claude --bg`.
pub(super) fn working(agent: &str, context: &RenderContext<'_>) -> bool {
    let agent = crate::domain::AgentId(agent.into());
    match sdk_live(&agent, context) {
        Some(live) => matches!(live, Some(alondra_domain::LiveState::Working(alondra_domain::Worker::Turn))),
        None => context.domain.node_phase(&agent) == Some(&crate::domain::NodePhase::Working),
    }
}

/// The task's pause, if its node is paused.

pub(super) fn paused<'a>(task: &crate::domain::Task, context: &'a RenderContext<'_>) -> Option<&'a super::LivePaused> {
    if task.state.terminal() { return None; }
    let paused = context.live.pacing.as_ref()?.paused.iter().find(|paused| paused.task_id == task.id.0)?;
    let node_paused = || task.agent_id.as_ref().is_some_and(|agent| context.domain.node_pause(agent).is_some());
    (paused.session.is_none() || node_paused()).then_some(paused)
}

/// "Claude API error at 14:05 · retries at 14:07 (attempt 2 of 3)", or the wait for the API to come back.
pub(super) fn api_wait_label(paused: &super::LivePaused, lang: super::Lang) -> String {
    use super::{LivePauseReason, text::tf};
    let at = &paused.at;
    let since = match paused.reason {
        LivePauseReason::ApiOutage => tf!(lang, "Waiting for the Claude service since {at}", "Esperando al servicio de Claude desde las {at}"),
        _ => tf!(lang, "Claude API error at {at}", "Error de la API de Claude a las {at}"),
    };
    let of = crate::domain::API_RETRIES.len();
    match (&paused.retry, paused.reason) {
        (Some(retry), LivePauseReason::ApiError) => {
            let (when, attempt) = (&retry.at, retry.attempt);
            format!("{since} · {}", tf!(lang, "retries at {when} (attempt {attempt} of {of})", "reintenta a las {when} (intento {attempt} de {of})"))
        }
        (Some(retry), _) => {
            let when = &retry.at;
            format!("{since} · {}", tf!(lang, "tries again at {when}", "vuelve a probar a las {when}"))
        }
        (None, _) => since,
    }
}

/// "Paused since 14:05 for memory · resumes when the Mac has been fine for 5 min (2 so far)".
pub(super) fn paused_label(paused: &super::LivePaused, lang: super::Lang) -> String {
    use super::{LivePauseReason, text::tf};
    let at = &paused.at;
    let why = match paused.reason {
        LivePauseReason::User => return tf!(lang, "Paused · you paused it at {at}", "Pausado · lo pausaste a las {at}"),
        LivePauseReason::Usage => return tf!(lang, "Paused since {at} for the usage limit · resumes in turn once it resets",
            "Pausado desde las {at} por el límite de uso · se reanuda por turno cuando se reinicie"),
        LivePauseReason::Memory => tf!(lang, "Paused since {at} for memory", "Pausado desde las {at} por memoria"),
        LivePauseReason::ApiError | LivePauseReason::ApiOutage => return api_wait_label(paused, lang),
        LivePauseReason::Machine => {
            let reason = paused.waits_for.as_deref().map(|reason| reason.trim().trim_end_matches('.')).filter(|reason| !reason.is_empty());
            let since = match reason {
                Some(reason) => tf!(lang, "Waiting: {reason} · since {at}", "Esperando: {reason} · desde las {at}"),
                None => tf!(lang, "Waiting since {at}", "Esperando desde las {at}"),
            };
            return match paused.next {
                true => format!("{since} · {}", super::text::t!(lang, "resumes next", "se reanuda el siguiente")),
                false => since,
            };
        }
    };
    let when = match paused.fine_minutes {
        _ if paused.next => tf!(lang, "resumes next", "se reanuda el siguiente"),
        Some(minutes) if minutes >= 5 => tf!(lang, "resumes in turn, within the limit", "se reanuda por turno, sin pasar del límite"),
        Some(minutes) => tf!(lang, "resumes when the Mac has been fine for 5 min ({minutes} so far)",
            "se reanuda cuando el Mac lleve 5 min bien ({minutes} hasta ahora)"),
        None => tf!(lang, "resumes when the Mac has been fine for 5 min", "se reanuda cuando el Mac lleve 5 min bien"),
    };
    format!("{why} · {when}")
}

/// The error of the scheduled launch that should have started this task and did not.
pub(super) fn failed_launch<'a>(task: &crate::domain::Task, context: &'a RenderContext<'_>) -> Option<&'a str> {
    if task.state != crate::domain::TaskState::NotStarted || task.agent_id.is_some() { return None; }
    context.index.failed_launch(&task.id)
}

/// "Scheduled" on a task a scheduled run created, so it reads apart from the work the user asked for.
pub(super) fn scheduled_mark(task: &crate::domain::Task, context: &RenderContext<'_>) -> Option<super::Label> {
    let lang = context.lang;
    context.index.scheduled(&task.id).then(|| super::Label {
        text: super::text::t!(lang, "Scheduled", "Programada").into(),
        tone: super::Tone::Neutral,
        elapsed: None,
    })
}

/// How long the task has been running since it first started, for the client to count, or how long it took once over.
/// Clients that do not count it read the text: "running since 10:05", "took 1 h 05 min", or "since 10:05", "1 h 05 min"
/// when the row already `named` what it measures.
pub(super) fn elapsed_mark(task: &crate::domain::Task, named: bool, context: &RenderContext<'_>) -> Option<super::Label> {
    use super::text::tf;
    let start = context.live.task_started.get(&task.id.0)?;
    let lang = context.lang;
    let text = match start.ran_secs {
        Some(secs) => {
            let took = super::text::short_duration(secs);
            if named { took } else { tf!(lang, "took {took}", "duró {took}") }
        }
        None => {
            let at = start.since.get(11..16).unwrap_or_default();
            if named { tf!(lang, "since {at}", "desde las {at}") } else { tf!(lang, "running since {at}", "en marcha desde las {at}") }
        }
    };
    Some(super::Label {
        text,
        tone: super::Tone::Neutral,
        elapsed: Some(super::Elapsed { since: start.since.clone(), until: start.until.clone() }),
    })
}

/// A task whose node the user closed and nobody took over yet.
pub(super) fn node_closed(task: &crate::domain::Task, context: &super::RenderContext<'_>) -> bool {
    task.state == crate::domain::TaskState::NotStarted && task.agent_id.is_none() && !task.archived
        && context.live.closed_tasks.contains(&task.id.0)
}

pub(super) fn node_closed_label(lang: super::Lang) -> &'static str {
    super::text::t!(lang, "Stopped", "Detenida")
}

pub(super) fn launch_failed_label(lang: super::Lang) -> &'static str {
    super::text::t!(lang, "Didn't start", "No arrancó")
}

pub(super) fn task_presentation(state: crate::domain::TaskState) -> (super::Icon, super::Tone) {
    use crate::domain::TaskState as S;
    use super::{Icon, Tone};
    match state {
        S::Running => (Icon::Mission, Tone::Accent),
        S::NeedsYou => (Icon::Action, Tone::Warning),
        S::ForYourReview => (Icon::PullRequest, Tone::Info),
        S::AwaitingBlocker => (Icon::Branch, Tone::Info),
        S::WaitingDependency => (Icon::Branch, Tone::Info),
        S::Done | S::Closing => (Icon::Check, Tone::Success),
        S::Cancelled => (Icon::Warning, Tone::Neutral),
        S::NotStarted => (Icon::Task, Tone::Neutral),
    }
}

pub(super) fn action(
    verb: Verb,
    subject: &str,
    revision: u64,
    label: &str,
    style: Style,
) -> Option<Action> {
    let id = match ActionId::new(verb, subject, revision, None) {
        Ok(id) => id.to_string(),
        Err(_) => {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "Alondra omitted an action with an invalid identifier ({verb:?})");
            return None;
        }
    };
    Some(Action {
        id,
        label: label.into(),
        style,
        role: Some(style.role()),
        option_id: None,
        description: None,
        presentation: Presentation::Button,
        options: vec![],
        input: Input::None,
        attachments: false,
        comments: None,
        confirm: None,
        enabled: true,
        disabled_reason: None,
        stamp: None,
        key: None,
        link: None,
        resolves_card: false,
    })
}

pub(super) fn view_link(key: &str, label: &str) -> Action {
    Action {
        id: String::new(),
        label: label.into(),
        style: Style::Plain,
        role: Some(Style::Plain.role()),
        option_id: None,
        description: None,
        presentation: Presentation::Button,
        options: vec![],
        input: Input::None,
        attachments: false,
        comments: None,
        confirm: None,
        enabled: true,
        disabled_reason: None,
        stamp: None,
        key: None,
        link: Some(Link::View { key: key.into() }),
        resolves_card: false,
    }
}

pub(super) fn id(value: &str) -> Result<&str, BuildError> {
    if super::card::plain_id(value) {
        Ok(value)
    } else {
        Err(BuildError::InvalidKey)
    }
}

pub(super) fn key_param(
    key: &str,
    prefix: &str,
    param: &str,
) -> Result<Option<String>, BuildError> {
    if key == prefix {
        return Ok(None);
    }
    let rest = key
        .strip_prefix(prefix)
        .and_then(|tail| tail.strip_prefix('?'))
        .ok_or(BuildError::InvalidKey)?;
    let value = rest
        .strip_prefix(param)
        .and_then(|tail| tail.strip_prefix('='))
        .ok_or(BuildError::InvalidKey)?;
    if value.len() > 768 || value.bytes().any(|b| b < 0x20 || b == b'&') {
        return Err(BuildError::InvalidKey);
    }
    Ok(Some(decode_component(value)?))
}

pub(super) fn decode_component(value: &str) -> Result<String, BuildError> {
    let mut decoded = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let pair = bytes
                    .get(index + 1..index + 3)
                    .ok_or(BuildError::InvalidKey)?;
                let hex = |byte: u8| match byte {
                    b'0'..=b'9' => Some(byte - b'0'),
                    b'a'..=b'f' => Some(byte - b'a' + 10),
                    b'A'..=b'F' => Some(byte - b'A' + 10),
                    _ => None,
                };
                decoded.push(
                    (hex(pair[0]).ok_or(BuildError::InvalidKey)? << 4)
                        | hex(pair[1]).ok_or(BuildError::InvalidKey)?,
                );
                index += 3;
            }
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    if decoded.len() > 256 || decoded.iter().any(|byte| *byte < 0x20 || *byte == 0x7f) {
        return Err(BuildError::InvalidKey);
    }
    String::from_utf8(decoded).map_err(|_| BuildError::InvalidKey)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::Tone;

    #[test]
    fn your_turn_has_its_own_tone() {
        assert_ne!(state_tone("your_turn"), state_tone("idle"));
        assert_eq!(state_tone("your_turn"), Tone::YourTurn);
    }
}

/// The Task a node is on: its open one, else the last it had. A node works on one Task at a time;
/// state saved before that rule may still give it several open ones, and the newest stands for it.
pub(super) fn current_task<'a>(context: &'a RenderContext<'_>, agent_id: &crate::domain::AgentId) -> Option<&'a crate::domain::Task> {
    context.node_task(agent_id)
}

pub(super) fn agent_label(agent_id: &crate::domain::AgentId, context: &RenderContext<'_>) -> String {
    let domain = context.domain;
    let name = domain.agents.get(agent_id).map(|agent| agent.name.as_str());
    if name.is_some_and(|name| !name.starts_with("scheduled/")) {
        return name.unwrap_or_default().into();
    }
    context.index.schedule_title(agent_id, name).map(str::to_owned)
        .or_else(|| current_task(context, agent_id).map(|task| task.title.clone()))
        .unwrap_or_else(|| t!(context.lang, "Node", "Nodo").into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OrchestratorState { Working, Failed, Idle }

pub(super) fn orchestrator_state(context: &RenderContext<'_>) -> OrchestratorState {
    match context.live.orchestrator.as_ref() {
        Some(live) if live.working => OrchestratorState::Working,
        Some(live) if live.summary.is_some() => OrchestratorState::Failed,
        _ => OrchestratorState::Idle,
    }
}

