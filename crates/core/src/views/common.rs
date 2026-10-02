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
        "unclear" => Tone::Unclear,
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

pub(super) fn node_presentation(state: Option<&str>) -> (super::Icon, super::Tone) {
    use super::{Icon, Tone};
    match state.unwrap_or("free") {
        "working" | "busy" => (Icon::Node, Tone::Success),
        "waiting_machine" => (Icon::Machine, Tone::Info),
        "waiting_answer" => (Icon::Question, Tone::Accent),
        "your_turn" => (Icon::Action, Tone::YourTurn),
        "waiting_merge" => (Icon::Merge, Tone::Info),
        "unclear" => (Icon::Notice, Tone::Unclear),
        "stalled" => (Icon::Warning, Tone::Critical),
        _ => (Icon::Node, Tone::Neutral),
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
    fn node_states_keep_their_0_2_9_tones() {
        let tones: Vec<Tone> = ["waiting_answer", "your_turn", "unclear", "done"]
            .iter().map(|state| node_presentation(Some(state)).1).collect();
        assert_eq!(tones, [Tone::Accent, Tone::YourTurn, Tone::Unclear, Tone::Neutral]);
    }

    #[test]
    fn your_turn_and_unclear_are_told_apart_in_every_tone_mapping() {
        assert_ne!(node_presentation(Some("your_turn")).1, node_presentation(Some("unclear")).1);
        assert_ne!(state_tone("your_turn"), state_tone("unclear"));
        assert_eq!(state_tone("your_turn"), node_presentation(Some("your_turn")).1);
        assert_eq!(state_tone("unclear"), node_presentation(Some("unclear")).1);
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
