//! One task in the words of the Tasks list: one of seven states, the one reason that explains it, since when that
//! reason holds, and the block of the list it belongs to. The state is read from the task's `Situation`, computed
//! once in core; nothing here decides it again.
use super::{
    Icon, LiveHold, LivePauseReason, RenderContext, Tone, common,
    text::{t, tf},
    now_fact::{NowFact, now_fact},
};
use alondra_domain::LiveState;
use crate::domain::{
    Attention, Delivery, Doing, Instruction, InteractionId, PrState, Situation, StallClosing, Task,
    TaskEnding, TaskPriority, WaitReason, WaitsFor,
};
use crate::domain::AlondraStep as Step;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Block {
    /// It waits for something from the user.
    ForYou,
    InProgress,
    Waiting,
    Ended,
}

/// The only seven states a task shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum StateLabel {
    NeedsYou,
    Delivered,
    Working,
    Waiting,
    Paused,
    Finished,
    Cancelled,
}

impl StateLabel {
    pub(super) const ALL: [Self; 7] = [Self::NeedsYou, Self::Delivered, Self::Working, Self::Waiting, Self::Paused, Self::Finished, Self::Cancelled];

    pub(super) fn text(self, lang: super::Lang) -> &'static str {
        match self {
            Self::NeedsYou => t!(lang, "Needs you", "Te necesita"),
            Self::Delivered => t!(lang, "Delivered", "Entregada"),
            Self::Working => t!(lang, "Working", "Trabajando"),
            Self::Waiting => t!(lang, "Waiting", "En espera"),
            Self::Paused => t!(lang, "Paused", "Pausada"),
            Self::Finished => t!(lang, "Finished", "Terminada"),
            Self::Cancelled => t!(lang, "Cancelled", "Cancelada"),
        }
    }

    pub(super) fn key(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs_you",
            Self::Delivered => "delivered",
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Paused => "paused",
            Self::Finished => "done",
            Self::Cancelled => "cancelled",
        }
    }
}

pub(super) struct Presentation {
    pub label: StateLabel,
    pub state: String,
    pub tone: Tone,
    pub icon: Icon,
    pub fact: Option<String>,
    pub since: Option<String>,
    pub block: Block,
    /// Its place in the queue of starts.
    pub queue: Option<usize>,
    /// When it last did something, to order the work in progress.
    pub active_at: Option<String>,
    /// Its fact belongs beside its state on the task's screen too, though it is in progress.
    pub fact_in_state: bool,
}

enum StateFact {
    Working(NowFact),
    NeedsYou(String),
    Delivered(String),
    Waiting(Option<String>),
    Paused(String),
    Finished(String),
    Cancelled(String),
}

impl Presentation {
    /// Where a request the orchestrator has not split yet sorts, as if it were a task.
    pub(super) fn waiting_request(block: Block, since: Option<String>) -> Self {
        Self { since, state: String::new(), ..Self::new(StateFact::Waiting(None), Tone::Neutral, block, super::Lang::En) }
    }

    fn new(state: StateFact, tone: Tone, block: Block, lang: super::Lang) -> Self {
        let (label, fact) = match state {
            StateFact::Working(now) => (StateLabel::Working, Some(now.text(lang))),
            StateFact::NeedsYou(fact) => (StateLabel::NeedsYou, Some(fact)),
            StateFact::Delivered(fact) => (StateLabel::Delivered, Some(fact)),
            StateFact::Waiting(fact) => (StateLabel::Waiting, fact),
            StateFact::Paused(fact) => (StateLabel::Paused, Some(fact)),
            StateFact::Finished(fact) => (StateLabel::Finished, Some(fact)),
            StateFact::Cancelled(fact) => (StateLabel::Cancelled, Some(fact)),
        };
        let icon = match label {
            StateLabel::Working => Icon::Mission,
            StateLabel::NeedsYou => Icon::Action,
            StateLabel::Delivered => Icon::PullRequest,
            StateLabel::Finished => Icon::Check,
            StateLabel::Cancelled => Icon::Warning,
            StateLabel::Waiting | StateLabel::Paused => Icon::Task,
        };
        Self { label, state: label.text(lang).into(), tone, icon, fact, since: None, block, queue: None, active_at: None, fact_in_state: false }
    }
}

pub(super) fn present(task: &Task, context: &RenderContext<'_>) -> Presentation {
    let situation = context.reading(&task.id).map_or(Situation::Waiting(WaitsFor::Slot), |read| read.situation);
    let mut presentation = classify(task, situation, context);
    if presentation.since.is_none() && presentation.active_at.is_none() {
        presentation.since = task_since(task, context);
    }
    presentation
}

fn task_since(task: &Task, context: &RenderContext<'_>) -> Option<String> {
    context.live.task_since.get(&task.id.0).cloned()
        .or_else(|| context.live.task_history.get(&task.id.0).and_then(|history| history.last()).map(|step| step.at.clone()))
}

fn node_of<'a>(task: &Task, context: &'a RenderContext<'_>) -> Option<&'a super::LiveNode> {
    task.agent_id.as_ref().and_then(|id| context.live.nodes.iter().find(|node| node.agent_id == id.0))
}

/// «14:05» out of a local ISO time.
fn clock(at: &str) -> &str {
    at.get(11..16).unwrap_or(at)
}

/// The moment the task's current step began, as a clock time.
fn since_clock(task: &Task, context: &RenderContext<'_>) -> Option<String> {
    node_of(task, context).and_then(|node| node.state_since.clone()).or_else(|| task_since(task, context)).map(|at| clock(&at).to_owned())
}

fn classify(task: &Task, situation: Situation, context: &RenderContext<'_>) -> Presentation {
    let lang = context.lang;
    match situation {
        Situation::Finished => Presentation::new(StateFact::Finished(done_fact(task, context)), Tone::Success, Block::Ended, lang),
        Situation::Cancelled => Presentation { fact_in_state: true,
            ..Presentation::new(StateFact::Cancelled(cancelled_fact(task, context)), Tone::Neutral, Block::Ended, lang) },
        Situation::NeedsYou => Presentation::new(StateFact::NeedsYou(open_questions(task, context).or_else(|| sign_in_fact(task, context))
                .unwrap_or_else(|| t!(lang, "Waiting for your answer", "Espera tu respuesta").into())), Tone::Warning, Block::ForYou, lang),
        Situation::Paused if common::gate_queued(task, context).is_some() => waiting(task, WaitsFor::Slot, context),
        Situation::Paused => Presentation::new(StateFact::Paused(paused_fact(task, context)), Tone::Neutral, Block::Waiting, lang),
        Situation::Delivered(delivery) => delivered(task, delivery, context),
        Situation::Working(doing) => failed_merge(task, working(task, doing, context), context),
        Situation::Waiting(waits) => failed_merge(task, waiting(task, waits, context), context),
    }
}

/// A PR that could not merge says why, whatever its node does about it.
fn failed_merge(task: &Task, presentation: Presentation, context: &RenderContext<'_>) -> Presentation {
    let lang = context.lang;
    match context.reading(&task.id).and_then(|read| read.merge_failure.clone()) {
        Some((_, reason)) => {
            let failure = tf!(lang, "Couldn't merge · {reason}", "No se pudo hacer merge · {reason}");
            let fact = if presentation.label == StateLabel::Working {
                format!("{failure} · {}", presentation.fact.as_deref().unwrap_or_default())
            } else { failure };
            Presentation { fact: Some(fact), ..presentation }
        }
        None => presentation,
    }
}

pub(super) fn working(task: &Task, doing: Doing, context: &RenderContext<'_>) -> Presentation {
    let lang = context.lang;
    let node = node_of(task, context);
    let active_at = node.and_then(|node| node.state_since.clone()).or_else(|| task_since(task, context));
    let since = active_at.as_deref().unwrap_or("—");
    let now = now_fact(node, &doing, since, lang);
    let mut presentation = Presentation { active_at, fact_in_state: true,
        ..Presentation::new(StateFact::Working(now), Tone::Accent, Block::InProgress, lang) };
    let mut prefixes = Vec::new();
    if let Some(step) = node.and_then(|node| super::task_node::step(node, lang)) {
        prefixes.push(step);
    }
    if doing == Doing::AfterMerge {
        prefixes.push(task.wrap_up.post_merge_work.as_ref().map(|work| work.reason.clone())
            .unwrap_or_else(|| t!(lang, "Finishing after the merge", "Terminando tras el merge").into()));
    } else if let Some(instruction) = context.reading(&task.id).and_then(|read| read.handed_back) {
        prefixes.push(match instruction {
            Instruction::Changes => t!(lang, "Making changes", "Corrigiendo"),
            Instruction::Conflicts => t!(lang, "Resolving conflicts", "Resolviendo conflictos"),
            Instruction::Question | Instruction::Reply => t!(lang, "Answering you", "Respondiéndote"),
        }.into());
    }
    prefixes.extend(presentation.fact.take());
    presentation.fact = Some(prefixes.join(" · "));
    presentation
}

fn waiting(task: &Task, waits: WaitsFor, context: &RenderContext<'_>) -> Presentation {
    let lang = context.lang;
    let since = since_clock(task, context);
    let from = |text: String| match &since {
        Some(at) => tf!(lang, "{text} · since {at}", "{text} · desde las {at}"),
        None => text,
    };
    let waits = match waits {
        WaitsFor::Slot | WaitsFor::Machine if common::held_for_disk(task, context).is_some() => WaitsFor::Disk,
        other => other,
    };
    let mut queue = None;
    let fact = match waits {
        WaitsFor::Slot => {
            let (fact, place) = slot_fact(task, context);
            queue = place;
            fact
        }
        WaitsFor::Machine => machine_fact(task, context),
        WaitsFor::Disk => match common::held_for_disk(task, context) {
            Some((free, needed, volume)) => common::disk_room_fact(free, needed, volume, lang),
            None => t!(lang, "Disk space · resumes by itself", "Espacio en disco · se reanuda sola").into(),
        },
        WaitsFor::Usage => usage_wait(task, context),
        WaitsFor::ProviderApi => match common::paused(task, context).filter(|paused| matches!(paused.reason, LivePauseReason::ApiError | LivePauseReason::ApiOutage)) {
            Some(paused) => common::api_wait_label(paused, lang),
            None => {
                let provider = provider_name(task);
                tf!(lang, "{provider} API · retries by itself", "API de {provider} · reintenta sola")
            }
        },
        WaitsFor::Tasks => tasks_fact(task, context),
        WaitsFor::PlanReview => plan_review_fact(task, context),
        WaitsFor::Step(Step::Starting) => from(t!(lang, "Starting", "Arrancando").into()),
        WaitsFor::Step(Step::Delivering) => {
            let answer = context.domain.lifecycle_of(&task.id).ok().and_then(|lifecycle| lifecycle.obligation())
                .is_some_and(|obligation| obligation.owed == alondra_domain::Owed::HandDecision);
            match (common::gate_queued(task, context), answer) {
                (Some(queued), answer) => {
                    queue = Some(queued.position);
                    let place = common::ordinal(queued.position, lang);
                    let of = queued.of;
                    if answer {
                        tf!(lang, "{place} of {of} · your answer waits for a slot", "{place} de {of} · tu respuesta espera hueco")
                    } else {
                        tf!(lang, "{place} of {of} · your message waits for a slot", "{place} de {of} · tu mensaje espera hueco")
                    }
                }
                (_, true) => from(t!(lang, "Alondra delivers your answer to it", "Alondra le entrega tu respuesta").into()),
                _ => from(match context.reading(&task.id).and_then(|read| read.handed_back) {
                    Some(Instruction::Changes) => t!(lang, "Alondra delivers your changes to it", "Alondra le entrega tus cambios"),
                    Some(Instruction::Conflicts) => t!(lang, "Alondra delivers the PR's conflicts to it", "Alondra le entrega los conflictos de la PR"),
                    _ => t!(lang, "Alondra delivers your message to it", "Alondra le entrega tu mensaje"),
                }.into()),
            }
        }
        WaitsFor::Step(Step::Waking) => from(t!(lang, "Alondra tells it its work finished", "Alondra le avisa de que su trabajo acabó").into()),
        WaitsFor::Step(Step::Answering) => from(t!(lang, "Alondra prepares its next step", "Alondra prepara su siguiente paso").into()),
    };
    let fact = match waits {
        WaitsFor::Slot | WaitsFor::Machine | WaitsFor::Disk | WaitsFor::Usage | WaitsFor::ProviderApi => with_pending(fact, task, context),
        _ => fact,
    };
    Presentation { queue, ..Presentation::new(StateFact::Waiting(Some(fact)), Tone::Neutral, Block::Waiting, lang) }
}

fn provider_name(task: &Task) -> &'static str {
    match task.provider { crate::accounts::Provider::Codex => "Codex", _ => "Claude" }
}

/// Why a task waits for a slot: its retry, the refusal that sent it back, or its place in the queue.
fn slot_fact(task: &Task, context: &RenderContext<'_>) -> (String, Option<usize>) {
    let lang = context.lang;
    if context.domain.open_plan_refusal(&task.id).is_some() {
        return (t!(lang, "The orchestrator fixes its launch plan or cancels it", "El orquestador corrige su plan de arranque o la cancela").into(), None);
    }
    let refusal = context.live.launch_refusals.get(&task.id.0).map(|refusal| match (refusal.reason.as_str(), refusal.detail.as_deref()) {
        ("workspace_untrusted", Some(folder)) => tf!(lang, "Claude Code doesn't trust {folder}", "Claude Code no confía en {folder}"),
        ("workspace_untrusted", None) => t!(lang, "Claude Code doesn't trust its folder", "Claude Code no confía en su carpeta").into(),
        ("usage_exhausted", _) if context.live.waits_for_session => t!(lang, "Waiting for a provider session", "Esperando una sesión de proveedor").into(),
        ("usage_exhausted", _) => t!(lang, "Usage limit reached", "Límite de uso alcanzado").into(),
        ("disk_low", _) => t!(lang, "The disk is nearly full", "El disco está casi lleno").into(),
        (_, Some(error)) => error.lines().next().unwrap_or(error).to_owned(),
        _ => t!(lang, "Its launch failed", "Su lanzamiento falló").into(),
    });
    if let Some(super::LiveStartWait::Retry { at }) = context.live.start_waits.get(&task.id.0) {
        let at = clock(at);
        let retry = tf!(lang, "Trying again at {at}", "Reintento a las {at}");
        return (match refusal { Some(cause) => format!("{retry} · {cause}"), None => retry }, None);
    }
    if let Some(cause) = refusal {
        return (cause, None);
    }
    if let Some(error) = common::failed_launch(task, context) {
        return (if error.is_empty() {
            t!(lang, "The scheduled run could not launch it", "La ejecución programada no pudo lanzarla").into()
        } else {
            error.lines().next().unwrap_or(error).to_owned()
        }, None);
    }
    if let Some(queued) = common::gate_queued(task, context) {
        return (queue_fact(task, queued, lang), Some(queued.position));
    }
    if common::node_closed(task, context) {
        return (t!(lang, "You closed its session before it delivered", "Cerraste su sesión sin que entregara").into(), None);
    }
    if let Some(waits) = chained(task, context) {
        return (waits, None);
    }
    let fact = match context.live.start_waits.get(&task.id.0) {
        Some(_) if context.live.waits_for_session => t!(lang, "Waiting for a provider session · starts once an account has one",
            "Esperando una sesión de proveedor · arranca cuando una cuenta la tenga"),
        Some(_) if context.live.usage_exhausted.is_some() => t!(lang, "Usage limit reached · starts when it is back",
            "Límite de uso alcanzado · arranca cuando vuelva"),
        Some(_) => t!(lang, "Starts when there is room", "Arranca cuando haya sitio"),
        None => t!(lang, "Its session ended before it delivered", "Su sesión terminó sin que entregara"),
    };
    (fact.into(), None)
}

/// «2nd of 3 · 6 of 6 slots taken», or the one that goes next.
fn queue_fact(task: &Task, queued: &super::LiveQueued, lang: super::Lang) -> String {
    let place = common::ordinal(queued.position, lang);
    let of = queued.of;
    match &queued.hold {
        LiveHold::Next if queued.position == 1 => t!(lang, "Starts within a minute", "Arranca en menos de un minuto").into(),
        LiveHold::Slots { working, limit } => tf!(lang, "{place} of {of} · {working} of {limit} slots taken", "{place} de {of} · {working} de {limit} huecos ocupados"),
        LiveHold::Disk { free, needed, volume } => common::disk_room_fact(*free, *needed, volume.as_deref(), lang),
        LiveHold::Memory { .. } | LiveHold::Cpu { .. } | LiveHold::Next => match priority(task, lang) {
            Some(priority) => tf!(lang, "{place} of {of} · {priority}", "{place} de {of} · {priority}"),
            None => tf!(lang, "{place} of {of}", "{place} de {of}"),
        },
    }
}

fn machine_fact(task: &Task, context: &RenderContext<'_>) -> String {
    let lang = context.lang;
    let declared = |reason: &str| Some(reason.trim().trim_end_matches('.')).filter(|reason| !reason.is_empty()).map(str::to_owned);
    if let Some(paused) = common::paused(task, context) {
        let at = clock(&paused.at);
        return match paused.reason {
            LivePauseReason::Machine => paused.waits_for.as_deref().and_then(declared)
                .map_or_else(|| tf!(lang, "Waits for the Mac since {at}", "Espera al Mac desde las {at}"), |reason| tf!(lang, "{reason} · since {at}", "{reason} · desde las {at}")),
            _ => tf!(lang, "Mac memory since {at} · resumes by itself", "Memoria del Mac desde las {at} · se reanuda sola"),
        };
    }
    t!(lang, "Mac memory · resumes by itself", "Memoria del Mac · se reanuda sola").into()
}

/// The provider and account whose usage limit holds the task, and when it renews.
fn usage_wait(task: &Task, context: &RenderContext<'_>) -> String {
    let lang = context.lang;
    let provider = provider_name(task);
    let account = task.account_key();
    let sharing = context.domain.accounts.iter().filter(|status| status.provider == account.provider).count() > 1;
    let name = context.domain.accounts.iter().find(|status| status.provider == account.provider && status.id == account.account)
        .map(|status| status.name.as_str()).filter(|_| sharing);
    let who = match name {
        Some(name) => format!("{provider} ({name})"),
        None => provider.to_owned(),
    };
    match context.live.task_usage_until.get(&task.id.0).and_then(|until| until.get(11..16)) {
        Some(until) => tf!(lang, "{who} limit until {until}", "Límite de {who} hasta las {until}"),
        None => tf!(lang, "{who} limit", "Límite de {who}"),
    }
}

fn paused_fact(task: &Task, context: &RenderContext<'_>) -> String {
    let lang = context.lang;
    let read = context.reading(&task.id);
    if matches!(read.and_then(|read| read.live.as_ref()), Some(LiveState::Paused)) {
        return t!(lang, "Turn interrupted. Send a message to continue.", "Turno interrumpido. Envía un mensaje para continuar.").into();
    }
    let by_user = common::paused(task, context).filter(|paused| paused.reason == LivePauseReason::User);
    let fact = match by_user {
        Some(paused) => {
            let at = clock(&paused.at);
            tf!(lang, "You paused it at {at}", "La pausaste a las {at}")
        }
        None => t!(lang, "You paused it", "La pausaste").into(),
    };
    with_pending(fact, task, context)
}

fn delivered(task: &Task, delivery: Delivery, context: &RenderContext<'_>) -> Presentation {
    let lang = context.lang;
    match delivery {
        Delivery::ForReview => {
            let fact = match review_fact(task, context) {
                Some(fact) => tf!(lang, "For review · {fact}", "Para revisar · {fact}"),
                None => t!(lang, "For review", "Para revisar").into(),
            };
            let asks_the_orchestrator = matches!(context.attention(task), Attention::Wait(WaitReason::Answer { .. }));
            let block = if context.waits_for_review(&task.id) && !asks_the_orchestrator { Block::ForYou } else { Block::InProgress };
            Presentation { fact_in_state: true, ..Presentation::new(StateFact::Delivered(fact), Tone::Info, block, lang) }
        }
        Delivery::Blocked => Presentation { fact_in_state: true,
            ..Presentation::new(StateFact::Delivered(tasks_fact(task, context)), Tone::Info, Block::Waiting, lang) },
        Delivery::Closing => {
            let fact = if task.wrap_up.asked_at.is_some() {
                t!(lang, "Cleaning up before closing", "Limpiando antes de cerrar")
            } else {
                t!(lang, "Accepted · Alondra merges its PR", "Aceptada · Alondra hace merge de su PR")
            };
            Presentation { fact_in_state: true, ..Presentation::new(StateFact::Delivered(fact.into()), Tone::Success, Block::InProgress, lang) }
        }
    }
}

fn cancelled_fact(task: &Task, context: &RenderContext<'_>) -> String {
    let lang = context.lang;
    if let Some(result) = stall_result(task, lang) {
        return result;
    }
    if task.ending == Some(TaskEnding::Cancelling) {
        return if task.agent_id.is_some() && task.wrap_up.node.is_none() {
            t!(lang, "Alondra closes its node", "Alondra cierra su nodo")
        } else {
            t!(lang, "Alondra removes what it left", "Alondra retira lo que dejó")
        }.into();
    }
    context.live.cancel_reasons.get(&task.id.0).map(|reason| reason.trim().to_owned()).filter(|reason| !reason.is_empty())
        .unwrap_or_else(|| t!(lang, "No reason recorded", "Sin motivo registrado").into())
}

/// What a task waits for from other tasks: their PR to merge, or the tasks themselves.
fn tasks_fact(task: &Task, context: &RenderContext<'_>) -> String {
    let lang = context.lang;
    match context.attention(task) {
        Attention::Wait(WaitReason::UpstreamPr { number }) => return tf!(lang, "Waits for PR #{number} to merge", "Espera al merge de la PR #{number}"),
        Attention::Wait(WaitReason::BlockedBy { task: blocker }) => {
            let title = context.domain.tasks.get(&blocker).map_or(blocker.0.as_str(), |blocker| blocker.title.as_str());
            return tf!(lang, "Waits for “{title}”", "Espera a «{title}»");
        }
        _ => {}
    }
    if let Some(pr) = task.stack.as_ref().and_then(|stack| context.domain.pull_requests.get(&stack.pr_id)).filter(|pr| pr.state == PrState::Open) {
        let number = pr.number;
        return tf!(lang, "Waits for PR #{number} to merge", "Espera al merge de la PR #{number}");
    }
    chained(task, context).unwrap_or_else(|| t!(lang, "Waits for another task", "Espera a otra tarea").into())
}

/// The sign-in a task waits for on a card shared with the other tasks held by the same thing.
fn sign_in_fact(task: &Task, context: &RenderContext<'_>) -> Option<String> {
    let lang = context.lang;
    let held = context.index.cards_of_task(&task.id).iter().rev().find_map(|id| {
        let card = context.domain.interactions.get(id).filter(|card| card.author == crate::domain::Author::System && context.domain.open(card))?;
        crate::domain::ClaudeHold::of(card)
    });
    if held == Some(crate::domain::ClaudeHold::SignedOut) {
        return Some(t!(lang, "Waiting for you to sign in to Claude", "Espera a que inicies sesión en Claude").into());
    }
    let waiting = context.index.mcp_wait_of_task(&task.id)?;
    let name = waiting.server.name.as_str();
    Some(tf!(lang, "Waiting for you to sign in to {name}", "Espera a que inicies sesión en {name}"))
}

/// What a task that needs the user asks: its one open question, or how many it has.
fn open_questions(task: &Task, context: &RenderContext<'_>) -> Option<String> {
    let lang = context.lang;
    let pending: Vec<_> = context.waiting_cards_of(&task.id).collect();
    match pending.as_slice() {
        [] => None,
        [one] if !one.card.effective_title().trim().is_empty() => {
            let interaction = one.id.strip_prefix("interaction:").and_then(|id| context.domain.interactions.get(&InteractionId(id.into())));
            if interaction.is_some_and(|interaction| context.domain.holds.get(&interaction.id) == Some(&crate::domain::Hold::Plan)) {
                return Some(t!(lang, "Review the plan", "Revisa el plan").into());
            }
            if let Some(said) = interaction.and_then(super::card::node_words) {
                return Some(tf!(lang, "“{said}”", "«{said}»"));
            }
            if let Some((need, _)) = interaction.and_then(|interaction| super::card::own_text(interaction, context.domain, lang)) {
                return Some(need);
            }
            let need = one.card.effective_title().trim();
            Some(tf!(lang, "“{need}”", "«{need}»"))
        }
        [_] => Some(t!(lang, "1 open question", "1 pregunta abierta").into()),
        many => {
            let count = many.len();
            Some(tf!(lang, "{count} open questions", "{count} preguntas abiertas"))
        }
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

/// The open tasks this one still waits for.
pub(super) fn open_dependencies<'a>(task: &Task, context: &RenderContext<'a>) -> Vec<&'a Task> {
    task.dependencies.iter().chain(task.waiting_for_tasks.iter().filter(|id| !task.ended_task_waits.contains_key(*id))).chain(&task.delivered_dependencies)
        .filter(|id| **id != task.id)
        .filter_map(|id| context.domain.tasks.get(id))
        .filter(|dependency| !dependency.state.terminal())
        .collect()
}

/// A task waiting for other open tasks: "Waits for “Build a feature”", or for several "Waits for Install PR 4, 5
/// and 7", each by the part of its title before ":", with what they share said once.
fn chained(task: &Task, context: &RenderContext<'_>) -> Option<String> {
    let lang = context.lang;
    let dependencies = open_dependencies(task, context);
    match dependencies.as_slice() {
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
    }
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

/// A decision of the user on a task this one waits for holds it until Alondra reviews its plan.
fn plan_review_fact(task: &Task, context: &RenderContext<'_>) -> String {
    let lang = context.lang;
    context.domain.plan_reviews_holding(&task.id).next()
        .and_then(|(_, review)| context.domain.tasks.get(&review.task_id))
        .map(|source| tf!(lang, "You decided on “{title}”; Alondra reviews its plan", "Decidiste en «{title}»; Alondra revisa su plan", title = source.title))
        .unwrap_or_else(|| t!(lang, "Alondra reviews its plan", "Alondra revisa su plan").into())
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
    if let Some(pr) = context.domain.main_pr(task) {
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
    if let Some(pr) = context.domain.merged_pr(task) {
        let number = pr.number;
        return tf!(lang, "PR #{number} merged", "PR #{number} mergeada");
    }
    if !task.documents.is_empty() {
        return t!(lang, "You accepted the document", "Aceptaste el documento").into();
    }
    t!(lang, "Delivered", "Entregada").into()
}
