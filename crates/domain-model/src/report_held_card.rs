//! Alondra's card for a crash report that waits on this Mac because `report.repo` is no longer the
//! repository it was first going to: nothing is sent until the user sends it to the new one or
//! keeps it on the Mac. `sending.json` says why a report is held; the card shows it.

use serde::{Deserialize, Serialize};

use alondra_core::text::Lang;
use alondra_core::text::{t, tf};

use crate::{CardSpec, QuestionOption, clip};

pub const REPORT_HELD_PREFIX: &str = "report-held-";
pub const SEND_OPTION: &str = "send";
pub const KEEP_OPTION: &str = "keep";
/// The longest an option's label gets, so a long repository name still fits a button.
const LABEL_MAX: usize = 40;

/// Why a pending report is not sent again, as `sending.json` keeps it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Held {
    /// The report repository is now `current`, not the one the report was going to. `refused` is
    /// what GitHub answered when the user asked to send it to `current` and it could not take it.
    RepoChanged {
        current: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refused: Option<String>,
    },
    /// A reason a newer CLI wrote.
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ReportHeldCard {
    pub report: String,
    /// The repository the report was going to.
    pub saved: String,
    /// The repository `report.repo` names now.
    pub current: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

impl ReportHeldCard {
    pub fn to_body(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_body(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }

    /// What the card is raised for: one report, so each held report has its own card and a new
    /// card for the same report takes the place of the open one.
    pub fn cause(&self) -> String {
        format!("report_held:{}", self.report)
    }

    /// The card's `need` and `why`.
    pub fn text(&self, lang: Lang) -> (String, String) {
        let (report, saved, current) = (self.report.as_str(), self.saved.as_str(), self.current.as_str());
        let need = tf!(lang, "Report {report} is not being sent: report.repo changed", "El informe {report} no se envía: report.repo cambió");
        let why = match &self.refused {
            None => tf!(lang,
                "It was going to {saved} and report.repo is now {current}. Nothing is sent until you choose.",
                "Iba a {saved} y report.repo es ahora {current}. No se envía nada hasta que elijas."),
            Some(refused) => {
                let refused = refused.as_str();
                tf!(lang,
                    "It could not be sent to {current}: {refused}. It was going to {saved}; choose again.",
                    "No se pudo enviar a {current}: {refused}. Iba a {saved}; elige otra vez.")
            }
        };
        (clip(&need, CardSpec::NEED_MAX), clip(&why, CardSpec::WHY_MAX))
    }

    /// The card's options, with the words the card is stored with.
    pub fn options(&self) -> Vec<QuestionOption> {
        [SEND_OPTION, KEEP_OPTION]
            .into_iter()
            .filter_map(|id| {
                let (label, description) = self.option_text(id, Lang::En)?;
                Some(QuestionOption { id: id.into(), label, description: Some(description) })
            })
            .collect()
    }

    /// The words of option `id` in the viewer's language.
    pub fn option_text(&self, id: &str, lang: Lang) -> Option<(String, String)> {
        let (report, current) = (self.report.as_str(), self.current.as_str());
        match id {
            SEND_OPTION => Some((
                clip(&tf!(lang, "Send it to {current}", "Enviarlo a {current}"), LABEL_MAX),
                tf!(lang, "Alondra sends it to {current} now", "Alondra lo envía ahora a {current}"),
            )),
            KEEP_OPTION => Some((
                t!(lang, "Keep it on this Mac", "Dejarlo en este Mac").into(),
                tf!(lang,
                    "Nothing is sent and Alondra stops trying; you can send it later with alondra report send {report}",
                    "No se envía y Alondra deja de reintentarlo; puedes enviarlo luego con alondra report send {report}"),
            )),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(refused: Option<&str>) -> ReportHeldCard {
        ReportHeldCard { report: "r-20261120-1a2b3c4d".into(), saved: "ana/informes".into(), current: "ana/otros".into(), refused: refused.map(Into::into) }
    }

    #[test]
    fn the_card_names_both_repositories_in_its_explanation_in_both_languages() {
        let (need, why) = card(None).text(Lang::Es);
        assert_eq!(need, "El informe r-20261120-1a2b3c4d no se envía: report.repo cambió");
        assert_eq!(why, "Iba a ana/informes y report.repo es ahora ana/otros. No se envía nada hasta que elijas.");
        let (need, why) = card(None).text(Lang::En);
        assert_eq!(need, "Report r-20261120-1a2b3c4d is not being sent: report.repo changed");
        assert_eq!(why, "It was going to ana/informes and report.repo is now ana/otros. Nothing is sent until you choose.");
        let options = card(None).options();
        assert_eq!(options.iter().map(|o| (o.id.as_str(), o.label.as_str())).collect::<Vec<_>>(), [(SEND_OPTION, "Send it to ana/otros"), (KEEP_OPTION, "Keep it on this Mac")]);
        assert_eq!(card(None).option_text(SEND_OPTION, Lang::Es).unwrap().0, "Enviarlo a ana/otros");
        assert_eq!(card(None).option_text(KEEP_OPTION, Lang::Es).unwrap().0, "Dejarlo en este Mac");
        assert_eq!(card(None).option_text("other", Lang::Es), None);
    }

    #[test]
    fn a_refusal_is_said_and_long_names_still_fit_a_card() {
        let (_, why) = card(Some("GitHub has no repository ana/otros that takes issues")).text(Lang::Es);
        assert!(why.starts_with("No se pudo enviar a ana/otros: GitHub has no repository"), "{why}");
        let long = ReportHeldCard { report: "r-20261120-1a2b3c4d".into(), saved: "a".repeat(100), current: "b".repeat(100), refused: Some("c".repeat(300)) };
        for lang in [Lang::En, Lang::Es] {
            let (need, why) = long.text(lang);
            assert!(need.chars().count() <= CardSpec::NEED_MAX);
            assert!(why.chars().count() <= CardSpec::WHY_MAX);
            assert!(long.option_text(SEND_OPTION, lang).unwrap().0.chars().count() <= LABEL_MAX);
        }
        assert_eq!(ReportHeldCard::from_body(&long.to_body()), Some(long));
    }

    #[test]
    fn a_held_reason_reads_old_and_newer_files() {
        let held: Held = serde_json::from_str(r#"{"reason":"repo_changed","current":"ana/otros"}"#).unwrap();
        assert_eq!(held, Held::RepoChanged { current: "ana/otros".into(), refused: None });
        assert_eq!(serde_json::from_str::<Held>(r#"{"reason":"something_new"}"#).unwrap(), Held::Unknown);
    }
}
