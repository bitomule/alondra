//! Every sentence the user reads about what happened after an action: why it was refused and
//! what a verb did. The `match` over `Refusal` and `OutcomeKind` has no fallback arm, so a new
//! case without words in both languages does not compile.

use crate::text::{t, t as tr};
use crate::{CardAction, Lang};
use alondra_domain_rules::Refusal;
use serde::{Deserialize, Serialize};
pub use crate::refusal::refusal_text;

pub fn scheduled_message_stamp(lang: Lang) -> &'static str {
    tr!(lang, "Message saved as a separate task", "Mensaje guardado como una tarea independiente")
}

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
    /// What the user looked at moved; `next` offers to go on anyway. A kind from a newer daemon
    /// reads as this one: the client shows its title and reloads.
    #[serde(other)]
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
    let mut action = crate::components::button(action_id, label, crate::Role::Advance);
    action.resolves_card = crate::action_id::ActionId::parse(&action.id)
        .is_ok_and(|id| crate::resolves::always_resolves(id.verb));
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

/// The text a client shows when its Mac cannot tell whether an action happened: what happened and
/// what to do, the same in all three.
pub fn unknown_outcome(reason: crate::action_outcome::UnknownReason, lang: Lang) -> Outcome {
    use crate::action_outcome::UnknownReason;
    let body = match reason {
        UnknownReason::Interrupted => t!(lang,
            "Alondra restarted on your Mac while doing it. Check whether the change is there; if not, do it again.",
            "Alondra se reinició en tu Mac mientras lo hacía. Mira si el cambio está; si no, vuelve a hacerlo."),
        UnknownReason::DataReset => t!(lang,
            "Alondra's data on your Mac was reset while this was on its way. Check whether the change is there; if not, do it again.",
            "Los datos de Alondra en tu Mac se restablecieron mientras esto llegaba. Mira si el cambio está; si no, vuelve a hacerlo."),
        UnknownReason::Expired | UnknownReason::NotFound => t!(lang,
            "Your Mac no longer keeps the answer to this. Check whether the change is there; if not, do it again.",
            "Tu Mac ya no guarda la respuesta a esto. Mira si el cambio está; si no, vuelve a hacerlo."),
        UnknownReason::ApprovalChanged => t!(lang,
            "This device's approval changed while your Mac was doing it. Check whether the change is there; if not, do it again.",
            "La aprobación de este dispositivo cambió mientras tu Mac lo hacía. Mira si el cambio está; si no, vuelve a hacerlo."),
        UnknownReason::Unrecorded | UnknownReason::Changed | UnknownReason::Unavailable
        | UnknownReason::Invalid | UnknownReason::Other => t!(lang,
            "Your Mac could not confirm whether it was done. Check whether the change is there; if not, do it again.",
            "Tu Mac no pudo confirmar si se hizo. Mira si el cambio está; si no, vuelve a hacerlo."),
    };
    Outcome {
        kind: OutcomeKind::Failed,
        title: t!(lang, "Your Mac doesn't know if it was done.", "Tu Mac no sabe si se hizo.").into(),
        body: Some(body.into()),
        next: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_domain_rules::MergeMethod;

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
            Refusal::PrClosed { number: 1 }, Refusal::PrAlreadyMerged { number: 1 }, Refusal::PrSuperseded, Refusal::PrDetached, Refusal::PrUncheckable, Refusal::NewerResult, Refusal::UnansweredMessage, Refusal::PendingUserMessage, Refusal::UserMessageUnavailable, Refusal::AlreadyAnswered, Refusal::Withdrawn,
            Refusal::ClarificationPending, Refusal::TaskNotActive, Refusal::TaskClosing, Refusal::Changed, Refusal::NotForYou,
            Refusal::NodeUnreachable { node: "n".into() }, Refusal::NothingToInterrupt, Refusal::DeviceNotApproved, Refusal::StorageUnavailable,
            Refusal::GithubUnreachable, Refusal::NoLongerAvailable, Refusal::DefaultAccountStays { name: "n".into() },
            Refusal::LastAccountOn { name: "n".into() }, Refusal::AccountInUse { name: "n".into() },
        ];
        for refusal in all {
            let (en, es) = (refusal_text(&refusal, Lang::En), refusal_text(&refusal, Lang::Es));
            assert!(!en.is_empty() && !es.is_empty() && en != es, "{refusal:?}");
            assert!(!en.contains('{') && !es.contains('{'), "{refusal:?}");
        }
    }
}
