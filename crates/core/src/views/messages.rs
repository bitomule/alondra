//! Every sentence the user reads about what happened after an action: why it was refused and
//! what a verb did. The `match` over `Refusal` and `OutcomeKind` has no fallback arm, so a new
//! case without words in both languages does not compile.

use super::text::{t, tf};
use super::{CardAction, Lang};
use crate::domain::{MergeMethod, Refusal};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    /// It happened.
    Done,
    /// It goes on in the background; the end arrives as the task's state.
    Started,
    /// Alondra did not do it, and says why.
    Refused,
    /// Alondra tried and it failed.
    Failed,
    /// What the user looked at moved; `next` offers to go on anyway.
    Changed,
}

/// What an action did, worded for the user in their language.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub kind: OutcomeKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<CardAction>,
}

impl OutcomeKind {
    /// The word a client may show next to the title, one per kind.
    pub fn label(self, lang: Lang) -> &'static str {
        match self {
            Self::Done => t!(lang, "Done", "Hecho"),
            Self::Started => t!(lang, "In progress", "En marcha"),
            Self::Refused => t!(lang, "Not done", "No se hizo"),
            Self::Failed => t!(lang, "It failed", "Ha fallado"),
            Self::Changed => t!(lang, "It changed", "Ha cambiado"),
        }
    }
}

fn methods(methods: &[MergeMethod]) -> String {
    methods
        .iter()
        .map(|method| match method {
            MergeMethod::Merge => "merge",
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn refusal_text(refusal: &Refusal, lang: Lang) -> String {
    match refusal {
        Refusal::BlockedByQuestion { need } => tf!(lang, "Answer “{need}” first; this result waits for it.",
            "Responde primero a «{need}»; este resultado lo espera.", need = need),
        Refusal::BlockedByTask { title } => tf!(lang, "“{title}” has to finish reviewing this first.",
            "«{title}» tiene que terminar de revisarlo primero.", title = title),
        Refusal::WaitsForTask { title } => tf!(lang, "This task waits for “{title}”: its plan can be approved once that wait ends.",
            "Esta tarea espera a «{title}»: su plan se podrá aprobar cuando termine esa espera.", title = title),
        Refusal::UpstreamPrOpen { number } => tf!(lang, "PR #{number} must merge first: this one builds on it.",
            "Antes hay que hacer merge de la PR #{number}: esta se apoya en ella.", number = number),
        Refusal::MethodNotAllowed { allowed, .. } if allowed.is_empty() => t!(lang,
            "Alondra could not read how this repository allows merging. Try again in a moment.",
            "Alondra no pudo leer qué tipos de merge permite este repositorio. Inténtalo de nuevo en un momento.").into(),
        Refusal::MethodNotAllowed { allowed, .. } => tf!(lang, "This PR only allows: {allowed}.",
            "Esta PR solo admite: {allowed}.", allowed = methods(allowed)),
        Refusal::NoMethodChosen { number } => tf!(lang, "Choose how to merge PR #{number}.",
            "Elige cómo hacer merge de la PR #{number}.", number = number),
        Refusal::StaleHead { number } => tf!(lang, "PR #{number} has new commits since you looked. Review them and try again.",
            "La PR #{number} tiene commits nuevos desde que la miraste. Revísalos y vuelve a intentarlo.", number = number),
        Refusal::ResultBehindPr { number } => tf!(lang, "PR #{number} has commits this result doesn't include, so it can't be merged from here until the node presents it again.",
            "La PR #{number} tiene commits que este resultado no incluye: no se puede hacer merge desde aquí hasta que el nodo la vuelva a presentar.", number = number),
        Refusal::MergeInProgress { number } => tf!(lang, "Alondra is already merging PR #{number}; wait for GitHub to confirm.",
            "Alondra ya está haciendo merge de la PR #{number}; espera a que GitHub lo confirme.", number = number),
        Refusal::PrClosed { number } => tf!(lang, "PR #{number} is closed on GitHub, so it can't be merged.",
            "La PR #{number} está cerrada en GitHub y no se puede hacer merge.", number = number),
        Refusal::PrAlreadyMerged { number } => tf!(lang, "PR #{number} was already merged, so it can't be closed without merging.",
            "La PR #{number} ya estaba mergeada, así que no se puede cerrar sin hacer merge.", number = number),
        Refusal::NewerResult => t!(lang, "There is a newer result: review it.",
            "Hay un resultado más nuevo: revísalo.").into(),
        Refusal::AlreadyAnswered => t!(lang, "You already answered this.", "Ya respondiste a esto.").into(),
        Refusal::Withdrawn => t!(lang, "Its author withdrew this card.", "Su autor retiró esta tarjeta.").into(),
        Refusal::ClarificationPending => t!(lang, "Waiting for the clarification you asked for.",
            "Esperando la aclaración que pediste.").into(),
        Refusal::TaskNotActive => t!(lang, "This task has finished or was cancelled.", "Esta tarea ya terminó o se canceló.").into(),
        Refusal::TaskClosing => t!(lang, "This task's PR is already merged and the task is closing, so it takes no more changes. Ask for more work as a new request.",
            "La PR de esta tarea ya está mergeada y la tarea se está cerrando, así que no admite más cambios. Pide más trabajo como una solicitud nueva.").into(),
        Refusal::Changed => t!(lang, "This changed while you were looking. It's updated now; try again.",
            "Esto cambió mientras lo mirabas. Ya está actualizado; vuelve a intentarlo.").into(),
        Refusal::NotForYou => t!(lang, "This is meant for the orchestrator or a node, not for you.",
            "Esto es para el orquestador o un nodo, no para ti.").into(),
        Refusal::NodeUnreachable { node } => tf!(lang, "“{node}” isn't connected: its session stopped. Nothing was sent.",
            "«{node}» no está conectado: su sesión se detuvo. No se envió nada.", node = node),
        Refusal::NothingToInterrupt => t!(lang, "This node isn't running anything to interrupt.",
            "Este nodo no está haciendo nada que interrumpir.").into(),
        Refusal::InterruptNotAccepted => t!(lang, "Interrupt it first: you can force it to stop only if it keeps going after Claude accepts the interrupt.",
            "Interrúmpelo primero: solo puedes forzar la parada si sigue después de que Claude acepte la interrupción.").into(),
        Refusal::AlreadyStopping => t!(lang, "Alondra is already stopping this node.", "Alondra ya está parando este nodo.").into(),
        Refusal::DeviceNotApproved => t!(lang, "This device is no longer approved. Approve it again from the Mac.",
            "Este dispositivo ya no está aprobado. Vuelve a aprobarlo desde el Mac.").into(),
        Refusal::StorageUnavailable => t!(lang, "Alondra couldn't save this. Nothing changed; try again.",
            "Alondra no pudo guardarlo. No cambió nada; inténtalo de nuevo.").into(),
        Refusal::GithubUnreachable => t!(lang, "Alondra couldn't reach GitHub. Nothing was changed; try again.",
            "Alondra no pudo contactar con GitHub. No cambió nada; inténtalo de nuevo.").into(),
        Refusal::NoLongerAvailable => t!(lang, "This is no longer available.", "Esto ya no está disponible.").into(),
        Refusal::Unchanged => t!(lang, "It was already like that: nothing changed.", "Ya estaba así: no cambió nada.").into(),
    }
}

impl Outcome {
    pub fn refused(refusal: &Refusal, lang: Lang) -> Self {
        let kind = match refusal {
            Refusal::StorageUnavailable | Refusal::GithubUnreachable => OutcomeKind::Failed,
            _ => OutcomeKind::Refused,
        };
        Self { kind, title: refusal_text(refusal, lang), body: None, next: None }
    }

    pub fn done(title: String) -> Self {
        Self { kind: OutcomeKind::Done, title, body: None, next: None }
    }

    pub fn started(title: String, body: Option<String>) -> Self {
        Self { kind: OutcomeKind::Started, title, body, next: None }
    }

    pub fn changed(title: String, body: String, next: CardAction) -> Self {
        Self { kind: OutcomeKind::Changed, title, body: Some(body), next: Some(next) }
    }
}

/// "Send anyway": what a `Changed` outcome offers, the same action with the PR's fresh head.
pub fn send_anyway(action_id: String, label: &str) -> CardAction {
    let mut action = super::card::button(action_id, label, super::Role::Advance);
    action.resolves_card = super::action_id::ActionId::parse(&action.id)
        .is_ok_and(|id| super::resolves::always_resolves(id.verb));
    CardAction::from(action)
}

/// The text a client shows when it could not reach the Mac: the same in all three.
pub fn transport_failure(lang: Lang) -> Outcome {
    Outcome {
        kind: OutcomeKind::Failed,
        title: t!(lang, "Couldn't reach your Mac. Nothing was sent; try again.",
            "No se pudo contactar con tu Mac. No se envió nada; inténtalo de nuevo.").into(),
        body: None,
        next: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_method_the_pr_does_not_allow_says_which_ones_it_does() {
        let refusal = Refusal::MethodNotAllowed { method: MergeMethod::Squash, allowed: vec![MergeMethod::Merge] };
        assert_eq!(refusal_text(&refusal, Lang::Es), "Esta PR solo admite: merge.");
        assert_eq!(refusal_text(&refusal, Lang::En), "This PR only allows: merge.");
    }

    #[test]
    fn every_refusal_reads_in_both_languages() {
        let all = [
            Refusal::BlockedByQuestion { need: "q".into() }, Refusal::BlockedByTask { title: "t".into() }, Refusal::WaitsForTask { title: "t".into() },
            Refusal::UpstreamPrOpen { number: 1 }, Refusal::MethodNotAllowed { method: MergeMethod::Merge, allowed: vec![] },
            Refusal::NoMethodChosen { number: 1 }, Refusal::StaleHead { number: 1 }, Refusal::ResultBehindPr { number: 1 }, Refusal::MergeInProgress { number: 1 },
            Refusal::PrClosed { number: 1 }, Refusal::PrAlreadyMerged { number: 1 }, Refusal::NewerResult, Refusal::AlreadyAnswered, Refusal::Withdrawn,
            Refusal::ClarificationPending, Refusal::TaskNotActive, Refusal::TaskClosing, Refusal::Changed, Refusal::NotForYou,
            Refusal::NodeUnreachable { node: "n".into() }, Refusal::NothingToInterrupt, Refusal::DeviceNotApproved, Refusal::StorageUnavailable,
            Refusal::GithubUnreachable, Refusal::NoLongerAvailable,
        ];
        for refusal in all {
            let (en, es) = (refusal_text(&refusal, Lang::En), refusal_text(&refusal, Lang::Es));
            assert!(!en.is_empty() && !es.is_empty() && en != es, "{refusal:?}");
            assert!(!en.contains('{') && !es.contains('{'), "{refusal:?}");
        }
    }
}
