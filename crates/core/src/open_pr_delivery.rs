//! The delivery Alondra makes of a task's open pull request when the task was closed without
//! delivering it. Its body holds the PR and what GitHub said of it, so every client words it in
//! its own language.

use serde::{Deserialize, Serialize};

use crate::domain::{CardSpec, OpenPrStatus, PrChecks, PrMergeable, PrState, clip};
use crate::views::Lang;
use crate::views::text::{t, tf};

pub const OPEN_PR_DELIVERY_PREFIX: &str = "open-pr-";
const NAMES_SHOWN: usize = 5;

/// What GitHub said of the PR when Alondra delivered it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrReadiness {
    /// Its checks passed and it merges cleanly.
    Ready,
    /// No check ran and it merges cleanly.
    NoChecks,
    Conflicting { base: String },
    ChecksFailed { names: Vec<String> },
    ActionRequired { names: Vec<String> },
    /// Checks still running: the PR is delivered without waiting for them, and only the merge does.
    ChecksRunning { names: Vec<String> },
    /// GitHub only tells whether it merges when Alondra tries.
    Unconfirmed,
    Merged,
}

impl PrReadiness {
    pub fn of(status: &OpenPrStatus) -> Self {
        match (status.state, &status.checks, status.mergeable) {
            (PrState::Merged | PrState::Closed, ..) => Self::Merged,
            (_, _, PrMergeable::Conflicting) => Self::Conflicting { base: status.base.clone() },
            (_, PrChecks::Failed { names }, _) => Self::ChecksFailed { names: names.clone() },
            (_, PrChecks::ActionRequired { names }, _) => Self::ActionRequired { names: names.clone() },
            (_, PrChecks::Pending { names }, _) => Self::ChecksRunning { names: names.clone() },
            (_, _, PrMergeable::Other | PrMergeable::Computing) => Self::Unconfirmed,
            (_, PrChecks::None, PrMergeable::Clean) => Self::NoChecks,
            (_, PrChecks::Passed, PrMergeable::Clean) => Self::Ready,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct OpenPrDelivery {
    pub title: String,
    pub repository: String,
    pub number: u64,
    pub readiness: PrReadiness,
    /// What the orchestrator wrote of the work when it closed the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

fn names(names: &[String]) -> String {
    let mut shown = names.iter().take(NAMES_SHOWN).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > NAMES_SHOWN {
        shown.push_str(&format!(" (+{})", names.len() - NAMES_SHOWN));
    }
    shown
}

impl OpenPrDelivery {
    pub fn to_body(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_body(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }

    /// What the card asks and why, in `lang`.
    pub fn text(&self, lang: Lang) -> (String, String) {
        let need = tf!(lang, "Review PR #{number} of “{title}”", "Revisa el PR #{number} de «{title}»",
            number = self.number, title = self.title);
        let why = match &self.readiness {
            PrReadiness::Ready => t!(lang,
                "Its task ended without delivering it. Its checks pass and it merges cleanly.",
                "Su tarea terminó sin entregarlo. Sus checks pasan y el merge no tiene conflictos.").to_owned(),
            PrReadiness::NoChecks => t!(lang,
                "Its task ended without delivering it. It merges cleanly; no check ran on it.",
                "Su tarea terminó sin entregarlo. El merge no tiene conflictos; no corrió ningún check.").to_owned(),
            PrReadiness::Conflicting { base } => tf!(lang,
                "Its task ended without delivering it. It conflicts with {base}.",
                "Su tarea terminó sin entregarlo. Tiene conflictos con {base}.", base = base),
            PrReadiness::ChecksFailed { names: failed } => tf!(lang,
                "Its task ended without delivering it. Failing checks: {names}.",
                "Su tarea terminó sin entregarlo. Checks fallando: {names}.", names = names(failed)),
            PrReadiness::ActionRequired { names: waiting } => tf!(lang,
                "Its task ended without delivering it. Checks waiting for someone on GitHub: {names}.",
                "Su tarea terminó sin entregarlo. Checks que esperan a alguien en GitHub: {names}.", names = names(waiting)),
            PrReadiness::ChecksRunning { names: running } => tf!(lang,
                "Its task ended without delivering it. Checks still running: {names}.",
                "Su tarea terminó sin entregarlo. Checks todavía en marcha: {names}.", names = names(running)),
            PrReadiness::Unconfirmed => t!(lang,
                "Its task ended without delivering it. GitHub only says whether it merges when Alondra tries.",
                "Su tarea terminó sin entregarlo. GitHub solo dice si se puede hacer merge cuando Alondra lo intenta.").to_owned(),
            PrReadiness::Merged => t!(lang,
                "Its task ended without delivering it, and it is already merged.",
                "Su tarea terminó sin entregarlo, y ya tiene el merge hecho.").to_owned(),
        };
        (clip(&need, CardSpec::NEED_MAX), clip(&why, CardSpec::WHY_MAX))
    }

    /// The card's detail: what accepting does and what got done.
    pub fn detail(&self, lang: Lang) -> String {
        let mut text = tf!(lang,
            "Its task was closed while pull request {repository}#{number} was still open, so Alondra delivers it instead of leaving it open with nobody to answer for it.",
            "Su tarea se cerró con el pull request {repository}#{number} todavía abierto, así que Alondra te lo entrega en vez de dejarlo abierto sin nadie que responda por él.",
            repository = self.repository, number = self.number);
        text.push_str(match self.readiness {
            PrReadiness::Merged => t!(lang, "", ""),
            PrReadiness::Ready | PrReadiness::NoChecks => t!(lang,
                "\n\n**Accept:** Alondra merges it.",
                "\n\n**Aceptar:** Alondra hace el merge."),
            PrReadiness::ChecksRunning { .. } => t!(lang,
                "\n\n**Accept:** Alondra merges it as soon as GitHub allows it, which depends on the checks the repository requires.",
                "\n\n**Aceptar:** Alondra hace el merge en cuanto GitHub lo permite, lo que depende de los checks que exija el repositorio."),
            _ => t!(lang,
                "\n\n**Accept:** Alondra tries to merge it; if GitHub refuses, the task goes back to work to fix it.",
                "\n\n**Aceptar:** Alondra intenta hacer el merge; si GitHub lo rechaza, la tarea vuelve a trabajo para arreglarlo."),
        });
        if let Some(result) = self.result.as_deref().map(str::trim).filter(|result| !result.is_empty()) {
            text.push_str(t!(lang, "\n\n**What got done:**\n\n", "\n\n**Lo que se hizo:**\n\n"));
            text.push_str(result);
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: PrState, mergeable: PrMergeable, checks: PrChecks) -> OpenPrStatus {
        OpenPrStatus { state, head: "a".repeat(40), base: "main".into(), mergeable, checks }
    }

    #[test]
    fn readiness_reads_githubs_typed_fields() {
        let failed = PrChecks::Failed { names: vec!["test".into()] };
        assert_eq!(PrReadiness::of(&status(PrState::Open, PrMergeable::Clean, PrChecks::Passed)), PrReadiness::Ready);
        assert_eq!(PrReadiness::of(&status(PrState::Open, PrMergeable::Clean, PrChecks::None)), PrReadiness::NoChecks);
        assert_eq!(PrReadiness::of(&status(PrState::Open, PrMergeable::Conflicting, failed.clone())),
            PrReadiness::Conflicting { base: "main".into() });
        assert_eq!(PrReadiness::of(&status(PrState::Open, PrMergeable::Clean, failed)),
            PrReadiness::ChecksFailed { names: vec!["test".into()] });
        assert_eq!(PrReadiness::of(&status(PrState::Open, PrMergeable::Other, PrChecks::Passed)), PrReadiness::Unconfirmed);
        assert_eq!(PrReadiness::of(&status(PrState::Open, PrMergeable::Clean, PrChecks::Pending { names: vec!["lint".into()] })),
            PrReadiness::ChecksRunning { names: vec!["lint".into()] });
        assert_eq!(PrReadiness::of(&status(PrState::Merged, PrMergeable::Clean, PrChecks::Passed)), PrReadiness::Merged);
    }

    #[test]
    fn the_card_keeps_its_body_and_fits_in_both_languages() {
        let card = OpenPrDelivery { title: "t".repeat(200), repository: "owner/repo".into(), number: 7,
            readiness: PrReadiness::ChecksFailed { names: (0..9).map(|n| format!("check-{n}")).collect() }, result: Some("Done".into()) };
        let running = OpenPrDelivery { readiness: PrReadiness::ChecksRunning { names: vec!["e2e".into()] }, ..card.clone() };
        for lang in [Lang::En, Lang::Es] {
            assert!(running.text(lang).1.contains("e2e") && running.detail(lang).contains("Done"));
        }
        assert_eq!(OpenPrDelivery::from_body(&card.to_body()), Some(card.clone()));
        for lang in [Lang::En, Lang::Es] {
            let (need, why) = card.text(lang);
            assert!(need.chars().count() <= CardSpec::NEED_MAX && why.chars().count() <= CardSpec::WHY_MAX);
            assert!(why.contains("(+4)"));
            assert!(card.detail(lang).contains("Done"));
        }
    }
}
