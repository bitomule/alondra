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
    let projection = context.domain.projection();
    View {
        protocol: VIEWS_PROTOCOL,
        data_epoch: context.domain.data_epoch.0.clone(),
        view: name.into(),
        key: key.into(),
        client: context.client,
        version: String::new(),
        cursor: context.cursor.into(),
        title,
        status: super::status::build(&projection, context),
        sections,
        empty: None,
        notices: vec![],
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
        more: None,
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
        id: id.into(),
        link: Some(Link::View { key: key.into() }),
        icon,
        title,
        subtitle,
        badges: vec![],
        trailing: None,
        depth: 0,
        struck: false,
        children: vec![],
        actions: vec![],
    }
}

pub(super) fn state_tone(state: &str) -> super::Tone {
    use super::Tone;
    match state {
        "needs_you" | "stalled" => Tone::Warning,
        "your_turn" => Tone::YourTurn,
        "in_review" | "waiting_merge" | "waiting_machine"
        | "waiting_dependency" | "closing" | "defining" => Tone::Info,
        "running" | "working" | "busy" | "free" | "waiting_answer" => Tone::Accent,
        "done" | "delivered" => Tone::Success,
        "cancelled" | "archived" | "not_started" | "connected" | "idle" => Tone::Neutral,
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
pub(super) fn instruction_label(task: &crate::domain_v2::Task, domain: &crate::domain_v2::DomainState, lang: super::Lang) -> Option<&'static str> {
    use crate::domain_v2::{Instruction, TaskState};
    if task.state != TaskState::Running { return None; }
    Some(match domain.task_handed_back(&task.id)? {
        Instruction::Changes => super::text::t!(lang, "Changes requested", "Cambios solicitados"),
        Instruction::Conflicts => super::text::t!(lang, "Resolving conflicts", "Resolviendo conflictos"),
        Instruction::Question => super::text::t!(lang, "Answering your question", "Respondiendo a tu pregunta"),
        Instruction::Reply => super::text::t!(lang, "Acting on your reply", "Atendiendo tu respuesta"),
    })
}

pub(super) fn task_presentation(state: crate::domain_v2::TaskState) -> (super::Icon, super::Tone) {
    use crate::domain_v2::TaskState as S;
    use super::{Icon, Tone};
    match state {
        S::Running => (Icon::Mission, Tone::Accent),
        S::NeedsYou => (Icon::Action, Tone::Warning),
        S::InReview => (Icon::PullRequest, Tone::Info),
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
            eprintln!("Alondra omitted an action with an invalid identifier ({verb:?})");
            return None;
        }
    };
    Some(Action {
        id,
        label: label.into(),
        style,
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
    })
}

pub(super) fn view_link(key: &str, label: &str) -> Action {
    Action {
        id: String::new(),
        label: label.into(),
        style: Style::Plain,
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
pub(super) fn current_task<'a>(domain: &'a crate::domain_v2::DomainState, agent_id: &crate::domain_v2::AgentId) -> Option<&'a crate::domain_v2::Task> {
    let mine = || domain.tasks.values().filter(|task| task.agent_id.as_ref() == Some(agent_id) && !task.archived);
    mine().filter(|task| !task.state.terminal()).max_by_key(|task| task.revision)
        .or_else(|| mine().max_by_key(|task| task.revision))
}

pub(super) fn agent_label(agent_id: &crate::domain_v2::AgentId, context: &RenderContext<'_>) -> String {
    let domain = context.domain;
    let name = domain.agents.get(agent_id).map(|agent| agent.name.as_str());
    if name.is_some_and(|name| !name.starts_with("scheduled/")) {
        return name.unwrap_or_default().into();
    }
    let schedule_title = domain.schedule.runs.values().find(|run| {
        run.launch.as_ref().is_some_and(|launch| {
            launch.agent_id.as_ref() == Some(agent_id) || Some(launch.intent.name.as_str()) == name
        })
    }).and_then(|run| {
        run.definition_snapshot.as_ref().map(|snapshot| snapshot.title.clone())
            .or_else(|| domain.schedule.schedules.get(&run.schedule_id).map(|schedule| schedule.title.clone()))
    });
    schedule_title
        .or_else(|| current_task(domain, agent_id).map(|task| task.title.clone()))
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

pub(super) fn orchestrator_label(context: &RenderContext<'_>) -> super::Label {
    let (text, tone) = match orchestrator_state(context) {
        OrchestratorState::Working => (t!(context.lang, "Working", "Trabajando"), super::Tone::Accent),
        OrchestratorState::Failed => (t!(context.lang, "Failed", "Falló"), super::Tone::Warning),
        OrchestratorState::Idle => (t!(context.lang, "Idle", "Inactivo"), super::Tone::Neutral),
    };
    super::Label { text: text.into(), tone }
}

const FAILURE_DETAIL: usize = 300;

/// The orchestrator's standing failure as the user reads it: what happened and what is already
/// done, with Alondra's own text only where the sentence cannot say what went wrong.
pub(super) fn orchestrator_failure(context: &RenderContext<'_>) -> Option<String> {
    let live = context.live.orchestrator.as_ref()?;
    let error = live.summary.as_deref()?;
    let lang = context.lang;
    let (sentence, with_detail) = match live.failure.as_deref() {
        Some("launch") => (t!(lang,
            "Claude could not start. Alondra will try again on its own.",
            "Claude no pudo arrancar. Alondra lo volverá a intentar sola."), true),
        Some("interrupted") => (t!(lang,
            "Its last run stopped partway, so some of what it did may already be done. Check its tasks, then retry or continue without repeating it.",
            "Su última ejecución se cortó a medias: puede que parte de lo que hizo ya esté hecho. Revisa sus tareas y elige reintentar o continuar sin repetir."), true),
        Some("denied") => (t!(lang,
            "Claude refused it one of Alondra's own tools, so its last run did not finish. Some of what it did may already be done.",
            "Claude le negó una de las herramientas de Alondra y su última ejecución no terminó. Puede que parte de lo que hizo ya esté hecho."), false),
        Some("cancelled") => (t!(lang,
            "You stopped its last run. What it had already done stays done.",
            "Detuviste su última ejecución. Lo que ya había hecho sigue hecho."), false),
        Some("session_missing") => (t!(lang,
            "Its saved conversation was gone. It will start a new one with the next message.",
            "Su conversación guardada ya no existía. Empezará una nueva con el próximo mensaje."), false),
        _ => (t!(lang, "Its last run failed.", "Su última ejecución falló."), true),
    };
    let detail: String = error.split_whitespace().collect::<Vec<_>>().join(" ").replace('`', "'")
        .chars().take(FAILURE_DETAIL).collect();
    Some(if with_detail && !detail.is_empty() {
        format!("{sentence}\n\n{}: `{detail}`", t!(lang, "Detail", "Detalle"))
    } else {
        sentence.to_owned()
    })
}
