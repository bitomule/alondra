//! One task in the words of the Tasks list: a short state, the one fact that explains it, since when that fact
//! holds, and the block of the list it belongs to. Every task falls in one row of this table; none is left without
//! a state.
use super::{
    Icon, LiveHold, LivePauseReason, RenderContext, Tone, common,
    text::{t, tf},
};
use alondra_domain::{Capacity, LiveState, Step, Worker};
use crate::domain::{Attention, Instruction, InteractionId, PauseReason, PrState, StallClosing, Task, TaskActivity, TaskEnding, TaskPriority, TaskState, WaitReason};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Block {
    /// It waits for something from the user.
    ForYou,
    InProgress,
    Waiting,
    Ended,
}

pub(super) struct Presentation {
    pub state: String,
    pub tone: Tone,
    pub icon: Icon,
    pub fact: Option<String>,
    pub since: Option<String>,
    pub block: Block,
    /// Its place in the queue of starts.
    pub queue: Option<usize>,
    /// When it last did something, to order the work in progress; a task at work shows no time.
    pub active_at: Option<String>,
    /// Its fact belongs beside its state on the task's screen too, though it is in progress.
    pub fact_in_state: bool,
}

impl Presentation {
    /// Where a request the orchestrator has not split yet sorts, as if it were a task.
    pub(super) fn waiting_request(block: Block, since: Option<String>) -> Self {
        Self { since, ..Self::new(String::new(), Tone::Neutral, block, None) }
    }

    fn new(state: impl Into<String>, tone: Tone, block: Block, fact: Option<String>) -> Self {
        Self { state: state.into(), tone, icon: Icon::Task, fact, since: None, block, queue: None, active_at: None, fact_in_state: false }
    }
}

pub(super) fn present(task: &Task, context: &RenderContext<'_>) -> Presentation {
    let (icon, _) = common::task_presentation(task.state);
    let mut presentation = classify(task, context);
    presentation.icon = if presentation.tone == Tone::Warning && task.state != TaskState::NeedsYou { Icon::Notice } else { icon };
    if presentation.since.is_none() && presentation.active_at.is_none() {
        presentation.since = context.live.task_since.get(&task.id.0).cloned()
            .or_else(|| context.live.task_history.get(&task.id.0).and_then(|history| history.last()).map(|step| step.at.clone()));
    }
    presentation
}

fn classify(task: &Task, context: &RenderContext<'_>) -> Presentation {
    let lang = context.lang;
    use Block::*;
    match task.state {
        TaskState::Done => return Presentation::new(t!(lang, "Finished", "Terminada"), Tone::Success, Ended, Some(done_fact(task, context))),
        TaskState::Cancelled => return match stall_result(task, lang) {
            Some(result) => Presentation { fact_in_state: true,
                ..Presentation::new(t!(lang, "Closed without a delivery", "Cerrada sin entrega"), Tone::Neutral, Ended, Some(result)) },
            None => Presentation::new(t!(lang, "Cancelled", "Cancelada"), Tone::Neutral, Ended,
                context.live.cancel_reasons.get(&task.id.0).map(|reason| reason.trim().to_owned()).filter(|reason| !reason.is_empty())),
        },
        TaskState::Closing => return closing(task, context),
        _ => {}
    }
    if common::held_for_disk(task, context).is_some() {
        return live_state(task, &LiveState::WaitingCapacity(Capacity::Disk), Shown { fact: None, active_at: None, step: None }, context);
    }
    if let Some(paused) = common::paused(task, context) {
        let at = &paused.at;
        let fact = match paused.reason {
            LivePauseReason::User => tf!(lang, "You paused it at {at}", "La pausaste a las {at}"),
            LivePauseReason::Memory => tf!(lang, "For memory since {at} · resumes by itself", "Por memoria desde las {at} · se reanuda sola"),
            LivePauseReason::Usage => t!(lang, "For the usage limit · back when it resets", "Por el límite de uso · vuelve cuando se reinicie").into(),
            LivePauseReason::ApiError | LivePauseReason::ApiOutage => common::api_wait_label(paused, lang),
            LivePauseReason::Machine => match paused.waits_for.as_deref().map(|reason| reason.trim().trim_end_matches('.')).filter(|reason| !reason.is_empty()) {
                Some(reason) => tf!(lang, "{reason} · since {at}", "{reason} · desde las {at}"),
                None => tf!(lang, "Waits for the Mac since {at}", "Espera al Mac desde las {at}"),
            },
        };
        return Presentation::new(t!(lang, "Paused", "Pausada"), Tone::Neutral, Waiting, Some(with_pending(fact, task, context)));
    }
    if let Some(queued) = common::resume_queued(task, context) {
        return in_queue(task, queued, lang);
    }
    let read = context.reading(&task.id);
    if let Some((_, reason)) = read.and_then(|read| read.merge_failure.clone()) {
        return Presentation::new(t!(lang, "Couldn't merge", "No se pudo hacer merge"), Tone::Warning, InProgress, Some(reason));
    }
    let waits = context.waits_for_user(&task.id);
    let attention = context.attention(task);
    if let Attention::Wait(reason @ WaitReason::Answer { .. }) = &attention {
        return if waits { for_you(task, context) } else { waiting_for(task, reason, context) };
    }
    let turn_runs = read.is_some_and(|read| matches!(read.activity, Some(TaskActivity::Live(LiveState::Working(_)))));
    match task.state {
        TaskState::ForYourReview if turn_runs => running(task, context),
        TaskState::NeedsYou | TaskState::ForYourReview if waits => for_you(task, context),
        TaskState::Running | TaskState::NeedsYou => running(task, context),
        TaskState::ForYourReview | TaskState::AwaitingBlocker => match &attention {
            Attention::Wait(reason) => waiting_for(task, reason, context),
            _ if read.is_some_and(|read| read.handed_back.is_some()) => running(task, context),
            _ if task.state == TaskState::ForYourReview =>
                Presentation::new(t!(lang, "Delivered", "Entregada"), Tone::Info, InProgress, review_fact(task, context)),
            _ => Presentation::new(t!(lang, "Waiting", "Esperando"), Tone::Info, Waiting, None),
        },
        TaskState::WaitingDependency => waiting_dependency(task, context),
        TaskState::NotStarted => not_started(task, context),
        TaskState::Done | TaskState::Cancelled | TaskState::Closing => unreachable!("handled above"),
    }
}

/// A task with a card waiting for the user: the only way into the For You block, so the block
/// never lists a task whose card the inbox and the task's detail do not show.
fn for_you(task: &Task, context: &RenderContext<'_>) -> Presentation {
    let lang = context.lang;
    if context.waits_for_review(&task.id) {
        Presentation::new(t!(lang, "Ready for review", "Para revisar"), Tone::Info, Block::ForYou, review_fact(task, context))
    } else {
        Presentation::new(t!(lang, "Needs you", "Te necesita"), Tone::Warning, Block::ForYou, open_questions(task, context))
    }
}

/// A delivery that waits for the review with nothing running: listed For You only while its card
/// is in the inbox, and read the same either way.
fn in_review(task: &Task, context: &RenderContext<'_>) -> Presentation {
    if context.waits_for_review(&task.id) {
        return for_you(task, context);
    }
    Presentation::new(t!(context.lang, "Ready for review", "Para revisar"), Tone::Info, Block::InProgress, review_fact(task, context))
}

/// What a task that needs the user asks: its one open question, or how many it has. Never the task's own title.
fn open_questions(task: &Task, context: &RenderContext<'_>) -> Option<String> {
    let lang = context.lang;
    let pending: Vec<_> = context.waiting_cards_of(&task.id).collect();
    match pending.as_slice() {
        [] => None,
        [one] if !one.card.need.trim().is_empty() && one.card.need.trim() != task.title.trim() => {
            let interaction = one.id.strip_prefix("interaction:").and_then(|id| context.domain.interactions.get(&InteractionId(id.into())));
            if let Some(said) = interaction.and_then(super::card::node_words) {
                return Some(tf!(lang, "“{said}”", "«{said}»"));
            }
            if let Some((need, _)) = interaction.and_then(|interaction| super::card::own_text(interaction, context.domain, lang)) {
                return Some(need);
            }
            let need = one.card.need.trim();
            Some(tf!(lang, "“{need}”", "«{need}»"))
        }
        [_] => Some(t!(lang, "1 open question", "1 pregunta abierta").into()),
        many => {
            let count = many.len();
            Some(tf!(lang, "{count} open questions", "{count} preguntas abiertas"))
        }
    }
}

/// What Claude holds the task on, while a card of its own waits for it.
fn claude_hold(task: &Task, context: &RenderContext<'_>) -> Option<crate::domain::ClaudeHold> {
    context.index.cards_of_task(&task.id).iter().rev().find_map(|id| {
        let card = context.domain.interactions.get(id).filter(|card| card.author == crate::domain::Author::System && context.domain.open(card))?;
        crate::domain::ClaudeHold::of(&card.id)
    })
}

fn running(task: &Task, context: &RenderContext<'_>) -> Presentation {
    let lang = context.lang;
    let read = context.reading(&task.id);
    let activity = read.and_then(|read| read.activity.clone()).unwrap_or(TaskActivity::NodeClosed);
    let node = task.agent_id.as_ref().and_then(|id| context.live.nodes.iter().find(|node| node.agent_id == id.0));
    let at_work = matches!(activity, TaskActivity::Working | TaskActivity::WaitingBackground(_) | TaskActivity::Live(LiveState::Working(_)));
    let doing = node.and_then(|node| super::task_node::doing(node, at_work, lang));
    let (fact, said_at) = doing.map_or((None, None), |(fact, at)| (Some(fact), at));
    let active_at = said_at.or_else(|| node.and_then(|node| node.state_since.clone()))
        .or_else(|| context.live.task_since.get(&task.id.0).cloned());
    if !matches!(activity, TaskActivity::Working | TaskActivity::WaitingBackground(_) | TaskActivity::Starting | TaskActivity::Reconnecting
        | TaskActivity::ComingBack | TaskActivity::Paused(_) | TaskActivity::Live(_))
        && context.waits_for_user(&task.id) {
        return for_you(task, context);
    }
    let warn = |state: &str| Presentation { since: active_at.clone(), ..Presentation::new(state, Tone::Warning, Block::Waiting, fact.clone()) };
    let behind = match &activity {
        TaskActivity::Live(LiveState::Working(Worker::OwnWork)) => Some(None),
        TaskActivity::WaitingBackground(work) if work.gone.is_none() => Some(Some(work)),
        _ => None,
    };
    if let Some(stored) = behind {
        return Presentation { active_at, fact_in_state: true, ..Presentation::new(t!(lang, "In the background", "En segundo plano"),
            Tone::Accent, Block::InProgress, left_running(node, stored, lang)) };
    }
    let state = match (&activity, read.and_then(|read| read.handed_back)) {
        (TaskActivity::Live(live), _) => return live_state(task, live,
            Shown { fact: fact.clone(), active_at: active_at.clone(), step: node.and_then(|node| super::task_node::step(node, lang)) }, context),
        (TaskActivity::ComingBack, _) => return match common::coming_back_queued(task, context) {
            Some(queued) => in_queue(task, queued, lang),
            None => Presentation::new(t!(lang, "Waiting for the machine", "Esperando máquina"), Tone::Neutral, Block::Waiting, None),
        },
        (TaskActivity::Reconnecting, _) => t!(lang, "Reconnecting", "Reconectando"),
        (TaskActivity::Working | TaskActivity::WaitingBackground(_) | TaskActivity::Starting, Some(Instruction::Changes)) => t!(lang, "Making changes", "Corrigiendo"),
        (TaskActivity::Working | TaskActivity::WaitingBackground(_) | TaskActivity::Starting, Some(Instruction::Conflicts)) => t!(lang, "Resolving conflicts", "Resolviendo conflictos"),
        (TaskActivity::Working | TaskActivity::WaitingBackground(_) | TaskActivity::Starting, Some(Instruction::Question | Instruction::Reply)) => t!(lang, "Answering you", "Respondiéndote"),
        (TaskActivity::Working | TaskActivity::WaitingBackground(_) | TaskActivity::Starting, _) => t!(lang, "Working", "Trabajando"),
        (TaskActivity::WaitingUser, _) if context.domain.waiting_on(&task.id) == Some(crate::domain::RecordedWait::Orchestrator) => return Presentation {
            since: active_at,
            ..Presentation::new(t!(lang, "Waiting for the orchestrator", "Esperando al orquestador"), Tone::Neutral, Block::Waiting, fact)
        },
        (TaskActivity::WaitingUser, _) => return warn(match claude_hold(task, context) {
            Some(crate::domain::ClaudeHold::SignedOut) => t!(lang, "Waiting for you to sign in to Claude", "Espera a que inicies sesión en Claude"),
            Some(crate::domain::ClaudeHold::Outage) => t!(lang, "Waiting for the Claude API", "Esperando a la API de Claude"),
            Some(crate::domain::ClaudeHold::Usage) => t!(lang, "Waiting for Claude's usage limit", "Esperando al límite de uso de Claude"),
            None => match context.index.mcp_wait_of_task(&task.id) {
                Some(waiting) => {
                    let name = waiting.server.name.as_str();
                    return warn(&tf!(lang, "Waiting for you to sign in to {name}", "Espera a que inicies sesión en {name}"));
                }
                None => t!(lang, "Waiting for an answer", "Esperando respuesta"),
            },
        }),
        (TaskActivity::WaitingDependency, _) => return waiting_dependency(task, context),
        (TaskActivity::Delivered, _) => return Presentation::new(t!(lang, "Delivered", "Entregada"), Tone::Info, Block::InProgress, fact),
        (TaskActivity::AfterMerge { reason }, _) => return Presentation { since: active_at,
            ..Presentation::new(t!(lang, "Finishing after the merge", "Terminando tras el merge"), Tone::Info, Block::InProgress, Some(reason.clone())) },
        (TaskActivity::Paused(pause), _) => {
            let fact = match &pause.reason {
                PauseReason::Memory => t!(lang, "For memory · resumes by itself", "Por memoria · se reanuda sola").to_owned(),
                PauseReason::ByUser => t!(lang, "You paused it", "La pausaste").to_owned(),
                PauseReason::Usage => t!(lang, "For the usage limit · back when it resets", "Por el límite de uso · vuelve cuando se reinicie").to_owned(),
                PauseReason::Disk => t!(lang, "For disk space · resumes by itself", "Por espacio en disco · se reanuda sola").to_owned(),
                PauseReason::ApiError(_) => t!(lang, "Claude API error · retries by itself", "Error de la API de Claude · reintenta sola").to_owned(),
                PauseReason::Declared { reason } => match reason.trim().trim_end_matches('.') {
                    "" => t!(lang, "Waits for the Mac", "Espera al Mac").to_owned(),
                    reason => reason.to_owned(),
                },
            };
            return Presentation::new(t!(lang, "Paused", "Pausada"), Tone::Neutral, Block::Waiting, Some(with_pending(fact, task, context)));
        }
        (TaskActivity::NodeClosed, _) => return Presentation { since: active_at,
            ..Presentation::new(common::node_closed_label(lang), Tone::Warning, Block::ForYou,
                Some(t!(lang, "Its session closed before it delivered", "Su sesión se cerró sin que entregara").into())) },
    };
    let step = node.and_then(|node| super::task_node::step(node, lang));
    Presentation { active_at, ..Presentation::new(state, Tone::Accent, Block::InProgress, step) }
}

/// What a node left running when its turn ended, by name, the oldest first: what its readings say
/// still runs, else what its phase recorded when the turn ended.
fn left_running(node: Option<&super::LiveNode>, stored: Option<&crate::domain::Background>, lang: super::Lang) -> Option<String> {
    let running = |state: super::RunState| state == super::RunState::Running;
    let mut runs: Vec<(&str, &str)> = node.map(|node| node.background.iter().filter(|item| running(item.state))
        .map(|item| (item.since.as_str(), item.label.as_str()))
        .chain(node.workflows.iter().chain(&node.subagents).filter(|run| running(run.state)).map(|run| (run.since.as_str(), run.label.as_str())))
        .filter(|(_, label)| !label.trim().is_empty())
        .collect()).unwrap_or_default();
    runs.sort();
    let (first, more) = match (runs.first(), stored) {
        (Some((_, label)), _) => (label.trim().to_owned(), runs.len() - 1),
        (None, Some(work)) if !work.label.trim().is_empty() => (work.label.trim().to_owned(), work.others as usize),
        _ => return None,
    };
    Some(match more {
        0 => first,
        more => tf!(lang, "{first} and {more} more", "{first} y {more} más"),
    })
}

/// What the screen knows of a task's node beside its lifecycle: what it says it does, since when,
/// and the step of its turn.
struct Shown {
    fact: Option<String>,
    active_at: Option<String>,
    step: Option<String>,
}

/// A task whose node runs through the Agent SDK, by its lifecycle's `LiveState` alone: one state
/// for each, and none that says its node is gone or still starting once its session runs.
fn live_state(task: &Task, live: &LiveState, shown: Shown, context: &RenderContext<'_>) -> Presentation {
    let lang = context.lang;
    let Shown { fact, active_at, step } = shown;
    let warn = |state: &str| Presentation { since: active_at.clone(), ..Presentation::new(state, Tone::Warning, Block::Waiting, fact.clone()) };
    match live {
        LiveState::Working(Worker::AfterMerge) => Presentation { since: active_at,
            ..Presentation::new(t!(lang, "Finishing after the merge", "Terminando tras el merge"), Tone::Info, Block::InProgress,
                task.wrap_up.post_merge_work.as_ref().map(|work| work.reason.clone())) },
        LiveState::Working(Worker::Alondra(Step::Merging | Step::Finishing)) => closing(task, context),
        LiveState::Working(Worker::Alondra(Step::Answering)) if task.stalls > 0 => Presentation { active_at,
            ..Presentation::new(t!(lang, "Alondra is getting it going again", "Alondra lo está reconduciendo"), Tone::Accent, Block::InProgress, step) },
        LiveState::Working(Worker::Alondra(Step::Delivering)) => Presentation { active_at,
            ..Presentation::new(t!(lang, "Waiting for Claude to start", "Esperando a que Claude empiece"), Tone::Accent, Block::InProgress, step) },
        LiveState::Working(_) => {
            let state = match context.reading(&task.id).and_then(|read| read.handed_back) {
                Some(Instruction::Changes) => t!(lang, "Making changes", "Corrigiendo"),
                Some(Instruction::Conflicts) => t!(lang, "Resolving conflicts", "Resolviendo conflictos"),
                Some(Instruction::Question | Instruction::Reply) => t!(lang, "Answering you", "Respondiéndote"),
                None => t!(lang, "Working", "Trabajando"),
            };
            Presentation { active_at, ..Presentation::new(state, Tone::Accent, Block::InProgress, step) }
        }
        LiveState::WaitingUser(cards) if cards.iter().all(|card| task.final_presentation.as_ref() == Some(card)) => in_review(task, context),
        LiveState::WaitingUser(cards) if cards.iter().all(|card| card.0.starts_with(crate::domain::NODE_PAUSE_PREFIX)) =>
            Presentation::new(t!(lang, "Paused", "Pausada"), Tone::Neutral, Block::Waiting,
                Some(with_pending(t!(lang, "You paused it", "La pausaste").to_owned(), task, context))),
        LiveState::WaitingUser(_) => match (claude_hold(task, context), context.index.mcp_wait_of_task(&task.id)) {
            (Some(crate::domain::ClaudeHold::SignedOut), _) => warn(t!(lang, "Waiting for you to sign in to Claude", "Espera a que inicies sesión en Claude")),
            (_, Some(waiting)) => {
                let name = waiting.server.name.as_str();
                warn(&tf!(lang, "Waiting for you to sign in to {name}", "Espera a que inicies sesión en {name}"))
            }
            _ => warn(t!(lang, "Waiting for an answer", "Esperando respuesta")),
        },
        LiveState::WaitingCapacity(Capacity::Usage) => warn(t!(lang, "Waiting for Claude's usage limit", "Esperando al límite de uso de Claude")),
        LiveState::WaitingCapacity(Capacity::ClaudeApi) => warn(t!(lang, "Waiting for the Claude API", "Esperando a la API de Claude")),
        LiveState::WaitingCapacity(Capacity::Disk) => {
            let fact = match common::held_for_disk(task, context) {
                Some((free, needed, volume)) => common::disk_room_fact(free, needed, volume, lang),
                None => t!(lang, "Disk", "Disco").into(),
            };
            Presentation {
                queue: common::gate_queued(task, context).map(|queued| queued.position),
                ..Presentation::new(t!(lang, "Waiting for capacity", "Esperando capacidad"), Tone::Neutral, Block::Waiting, Some(fact))
            }
        }
        LiveState::WaitingCapacity(Capacity::Gate | Capacity::Machine) => {
            let queued = common::gate_queued(task, context).and_then(|queued| in_queue(task, queued, lang).fact);
            Presentation::new(t!(lang, "Waiting for the machine", "Esperando máquina"), Tone::Neutral, Block::Waiting, queued)
        }
        LiveState::WaitingTask(_) => waiting_dependency(task, context),
    }
}

/// What a paused task's node still owes the user, when its delivery went back to it.
fn with_pending(fact: String, task: &Task, context: &RenderContext<'_>) -> String {
    let lang = context.lang;
    let pending = match context.reading(&task.id).and_then(|read| read.handed_back) {
        Some(Instruction::Changes) => t!(lang, "has your changes pending", "tiene pendientes tus cambios"),
        Some(Instruction::Question) => t!(lang, "has your question pending", "tiene pendiente tu pregunta"),
        Some(Instruction::Conflicts) => t!(lang, "has the PR's conflicts pending", "tiene pendientes los conflictos de la PR"),
        Some(Instruction::Reply) => t!(lang, "has your reply pending", "tiene pendiente tu respuesta"),
        None => return fact,
    };
    format!("{fact} · {pending}")
}

/// What a task closed because its node kept stopping says got done.
fn stall_result(task: &Task, lang: super::Lang) -> Option<String> {
    Some(match task.stall_closing.as_ref()? {
        StallClosing::Settled { result } => result.trim().to_owned(),
        StallClosing::Unsettled => t!(lang, "Its node kept stopping without a result or a question, and Alondra closed it",
            "Su nodo se paraba una y otra vez sin entregar ni preguntar, y Alondra la cerró").to_owned(),
    })
}

fn closing(task: &Task, context: &RenderContext<'_>) -> Presentation {
    let lang = context.lang;
    if let Some(result) = stall_result(task, lang) {
        return Presentation { fact_in_state: true,
            ..Presentation::new(t!(lang, "Closing without a delivery", "Cerrando sin entrega"), Tone::Neutral, Block::InProgress, Some(result)) };
    }
    if task.ending == Some(TaskEnding::Cancelling) {
        let fact = if task.agent_id.is_some() && task.wrap_up.node.is_none() {
            t!(lang, "Alondra closes its node", "Alondra cierra su nodo")
        } else {
            t!(lang, "Alondra removes what it left", "Alondra retira lo que dejó")
        };
        return Presentation::new(t!(lang, "Cancelling", "Cancelando"), Tone::Neutral, Block::InProgress, Some(fact.into()));
    }
    let fact = if task.wrap_up.asked_at.is_some() {
        t!(lang, "Cleaning up before closing", "Limpiando antes de cerrar")
    } else {
        t!(lang, "Accepted · Alondra merges its PR", "Aceptada · Alondra hace merge de su PR")
    };
    Presentation::new(t!(lang, "Closing", "Cerrando"), Tone::Success, Block::InProgress, Some(fact.into()))
}

fn waiting_for(task: &Task, reason: &WaitReason, context: &RenderContext<'_>) -> Presentation {
    let lang = context.lang;
    match reason {
        WaitReason::Answer { card, need } => {
            let need = context.domain.interactions.get(card).map_or_else(|| need.clone(), |card| super::card::shown_need(card, context.domain, lang));
            Presentation::new(t!(lang, "Waiting for an answer", "Esperando respuesta"), Tone::Info, Block::Waiting, Some(tf!(lang, "“{need}”", "«{need}»")))
        }
        WaitReason::UpstreamPr { number } => waiting_pr(*number, lang),
        WaitReason::NodeFixing { .. } => Presentation::new(t!(lang, "Resolving conflicts", "Resolviendo conflictos"), Tone::Accent,
            Block::InProgress, None),
        WaitReason::Merging { .. } | WaitReason::Closing => closing(task, context),
        WaitReason::BlockedBy { task: blocker } => {
            let title = context.domain.tasks.get(blocker).map_or(blocker.0.as_str(), |blocker| blocker.title.as_str());
            Presentation::new(t!(lang, "Waiting", "Esperando"), Tone::Info, Block::Waiting,
                Some(tf!(lang, "Waits for “{title}”", "Espera a «{title}»")))
        }
    }
}

fn waiting_pr(number: u64, lang: super::Lang) -> Presentation {
    Presentation::new(t!(lang, "Waiting for PR", "Esperando PR"), Tone::Info, Block::Waiting,
        Some(tf!(lang, "Waits for PR #{number} to merge", "Espera al merge de la PR #{number}")))
}

/// The open tasks this one still waits for.
pub(super) fn open_dependencies<'a>(task: &Task, context: &RenderContext<'a>) -> Vec<&'a Task> {
    task.dependencies.iter().chain(&task.delivered_dependencies)
        .filter(|id| **id != task.id)
        .filter_map(|id| context.domain.tasks.get(id))
        .filter(|dependency| !dependency.state.terminal())
        .collect()
}

/// A task waiting for other open tasks: "Waits for “Build a feature”", or for several "Waits for Install PR 4, 5
/// and 7", each by the part of its title before ":", with what they share said once.
fn chained(task: &Task, context: &RenderContext<'_>) -> Presentation {
    let lang = context.lang;
    let dependencies = open_dependencies(task, context);
    let fact = match dependencies.as_slice() {
        [] => None,
        [only] => Some(tf!(lang, "Waits for “{title}”", "Espera a «{title}»", title = only.title)),
        several => {
            let names: Vec<String> = several.iter()
                .map(|dependency| dependency.title.split_once(':').map_or(dependency.title.as_str(), |(head, _)| head).trim().to_owned())
                .collect();
            let names = shared_prefix_once(&names);
            let (last, head) = names.split_last().expect("several names");
            let head = head.join(", ");
            Some(tf!(lang, "Waits for {head} and {last}", "Espera a {head} y {last}"))
        }
    };
    Presentation::new(t!(lang, "Waiting", "Esperando"), Tone::Info, Block::Waiting, fact)
}

/// ["Install PR 4", "Install PR 5"] reads as ["Install PR 4", "5"]: the words they share are said once.
fn shared_prefix_once(names: &[String]) -> Vec<String> {
    let [first, rest @ ..] = names else { return Vec::new() };
    if rest.is_empty() {
        return vec![first.clone()];
    }
    let mut shared = first.as_str();
    for name in rest {
        while !name.starts_with(shared) {
            shared = match shared.trim_end().rsplit_once(' ') {
                Some((head, _)) => &first[..head.len() + 1],
                None => "",
            };
        }
    }
    let shared = &shared[..shared.rfind(' ').map_or(0, |at| at + 1)];
    if shared.is_empty() || names.iter().any(|name| name.len() == shared.len()) {
        return names.to_vec();
    }
    std::iter::once(first.clone()).chain(rest.iter().map(|name| name[shared.len()..].to_owned())).collect()
}

/// The one line the Waiting block says when the Mac itself holds every new start: no figure that changes by
/// itself, and nothing while the Mac lets the next one start.
pub(super) fn start_hold(listed: &[&Task], context: &RenderContext<'_>) -> Option<String> {
    let lang = context.lang;
    let hold = listed.iter().find_map(|task| common::gate_queued(task, context)).map(|queued| &queued.hold)?;
    match hold {
        LiveHold::Memory { .. } => Some(t!(lang, "New starts wait: the Mac is short of memory",
            "Los arranques nuevos esperan: el Mac va justo de memoria").into()),
        LiveHold::Cpu { .. } => Some(t!(lang, "New starts wait: the Mac is very busy",
            "Los arranques nuevos esperan: el Mac está muy cargado").into()),
        LiveHold::Slots { .. } | LiveHold::Next | LiveHold::Disk { .. } => None,
    }
}

/// A decision of the user on a task this one waits for holds it until the orchestrator reviews its plan.
fn plan_review(task: &Task, context: &RenderContext<'_>) -> Option<Presentation> {
    let (_, review) = context.domain.plan_reviews_holding(&task.id).next()?;
    let lang = context.lang;
    let fact = context.domain.tasks.get(&review.task_id).map(|source| tf!(lang,
        "You decided on “{title}”; the orchestrator reviews its plan", "Decidiste en «{title}»; el orquestador revisa su plan", title = source.title));
    Some(Presentation::new(t!(lang, "Plan review pending", "Revisión de plan pendiente"), Tone::Neutral, Block::Waiting, fact))
}

fn waiting_dependency(task: &Task, context: &RenderContext<'_>) -> Presentation {
    if let Some(held) = plan_review(task, context) {
        return held;
    }
    if let Some(pr) = task.stack.as_ref().and_then(|stack| context.domain.pull_requests.get(&stack.pr_id))
        .filter(|pr| pr.state == PrState::Open) {
        return waiting_pr(pr.number, context.lang);
    }
    if !open_dependencies(task, context).is_empty() {
        return chained(task, context);
    }
    Presentation::new(t!(context.lang, "Waiting", "Esperando"), Tone::Info, Block::Waiting, None)
}

fn not_started(task: &Task, context: &RenderContext<'_>) -> Presentation {
    let lang = context.lang;
    use Block::*;
    if task.agent_id.is_some() {
        return Presentation::new(t!(lang, "Starting", "Arrancando"), Tone::Accent, InProgress,
            Some(t!(lang, "Opening its session", "Abriendo su sesión").into()));
    }
    if let Some(held) = plan_review(task, context) {
        return held;
    }
    let didnt_start = |fact: String| Presentation::new(common::launch_failed_label(lang), Tone::Warning, Waiting, Some(fact));
    if let Some(super::LiveStartWait::Retry { at }) = context.live.start_waits.get(&task.id.0) {
        return Presentation::new(common::launch_failed_label(lang), Tone::Warning, Waiting,
            Some(tf!(lang, "Trying again at {at}", "Reintento a las {at}")));
    }
    if let Some(refusal) = context.live.launch_refusals.get(&task.id.0) {
        return didnt_start(match (refusal.reason.as_str(), refusal.detail.as_deref()) {
            ("workspace_untrusted", Some(folder)) => tf!(lang, "Claude Code doesn't trust {folder}", "Claude Code no confía en {folder}"),
            ("workspace_untrusted", None) => t!(lang, "Claude Code doesn't trust its folder", "Claude Code no confía en su carpeta").into(),
            ("usage_exhausted", _) if context.live.waits_for_session => t!(lang, "Waiting for a provider session", "Esperando una sesión de proveedor").into(),
            ("usage_exhausted", _) => t!(lang, "Usage limit reached", "Límite de uso alcanzado").into(),
            ("disk_low", _) => t!(lang, "The disk is nearly full", "El disco está casi lleno").into(),
            (_, Some(error)) => error.lines().next().unwrap_or(error).to_owned(),
            _ => t!(lang, "Its launch failed", "Su lanzamiento falló").into(),
        });
    }
    if let Some(error) = common::failed_launch(task, context) {
        return didnt_start(if error.is_empty() {
            t!(lang, "The scheduled run could not launch it", "La ejecución programada no pudo lanzarla").into()
        } else {
            error.lines().next().unwrap_or(error).to_owned()
        });
    }
    if let Some(queued) = common::queued(task, context) {
        return in_queue(task, queued, lang);
    }
    if common::node_closed(task, context) {
        return Presentation::new(common::node_closed_label(lang), Tone::Warning, Waiting,
            Some(t!(lang, "You closed its session before it delivered", "Cerraste su sesión sin que entregara").into()));
    }
    if !open_dependencies(task, context).is_empty() {
        return chained(task, context);
    }
    match context.live.start_waits.get(&task.id.0) {
        Some(_) if context.live.waits_for_session => Presentation::new(
            t!(lang, "Queued", "En cola"), Tone::Neutral, Waiting,
            Some(t!(lang, "Waiting for a provider session · starts once an account has one", "Esperando una sesión de proveedor · arranca cuando una cuenta la tenga").into())),
        Some(_) if context.live.usage_exhausted.is_some() => Presentation::new(
            t!(lang, "Queued", "En cola"), Tone::Neutral, Waiting,
            Some(t!(lang, "Usage limit reached · starts when it is back", "Límite de uso alcanzado · arranca cuando vuelva").into())),
        Some(_) => Presentation::new(t!(lang, "Queued", "En cola"), Tone::Neutral, Waiting,
            Some(t!(lang, "Starts when there is room", "Arranca cuando haya sitio").into())),
        None => Presentation::new(common::node_closed_label(lang), Tone::Warning, Waiting,
            Some(t!(lang, "Its session ended before it delivered", "Su sesión terminó sin que entregara").into())),
    }
}

/// A task waiting at the gate: its place in the queue and what holds it there.
fn in_queue(task: &Task, queued: &super::LiveQueued, lang: super::Lang) -> Presentation {
    let place = common::ordinal(queued.position, lang);
    let of = queued.of;
    let fact = match &queued.hold {
        LiveHold::Next if queued.position == 1 => Some(t!(lang, "Starts within a minute", "Arranca en menos de un minuto").into()),
        LiveHold::Slots { working, limit } => Some(tf!(lang, "{working} of {limit} working", "Hay {working} de {limit} trabajando")),
        LiveHold::Disk { free, needed, volume } => Some(common::disk_room_fact(*free, *needed, volume.as_deref(), lang)),
        LiveHold::Memory { .. } | LiveHold::Cpu { .. } | LiveHold::Next => priority(task, lang),
    };
    Presentation {
        queue: Some(queued.position),
        ..Presentation::new(tf!(lang, "Queued · {place} of {of}", "En cola · {place} de {of}"), Tone::Neutral, Block::Waiting, fact)
    }
}

fn priority(task: &Task, lang: super::Lang) -> Option<String> {
    match task.priority {
        TaskPriority::High => Some(t!(lang, "High priority", "Prioridad alta").into()),
        TaskPriority::Low => Some(t!(lang, "Low priority", "Prioridad baja").into()),
        TaskPriority::Normal => None,
    }
}

fn pr_title(id: &str, context: &RenderContext<'_>) -> Option<String> {
    context.live.pull_requests.iter().find(|pr| pr.id == id).and_then(|pr| pr.title.clone())
}

fn review_fact(task: &Task, context: &RenderContext<'_>) -> Option<String> {
    let lang = context.lang;
    if let Some(pr) = task.pr_id.as_ref().and_then(|id| context.domain.pull_requests.get(id)) {
        let number = pr.number;
        return Some(match pr_title(&pr.id.0, context) {
            Some(title) => tf!(lang, "PR #{number} · {title}", "PR #{number} · {title}"),
            None => tf!(lang, "PR #{number}", "PR #{number}"),
        });
    }
    task.documents.keys().last().and_then(|id| context.domain.documents.get(id))
        .map(|document| tf!(lang, "Document: {title}", "Documento: {title}", title = document.title))
}

fn done_fact(task: &Task, context: &RenderContext<'_>) -> String {
    let lang = context.lang;
    if let Some(pr) = task.pr_id.as_ref().and_then(|id| context.domain.pull_requests.get(id)).filter(|pr| pr.state == PrState::Merged) {
        let number = pr.number;
        return tf!(lang, "PR #{number} merged", "PR #{number} mergeada");
    }
    if !task.documents.is_empty() {
        return t!(lang, "You accepted the document", "Aceptaste el documento").into();
    }
    t!(lang, "Delivered", "Entregada").into()
}
