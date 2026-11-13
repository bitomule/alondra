//! A node declares the language of each card it sends. The declaration is compared with the user's
//! language; the text of the card is never read.

use alondra_core::domain::{Command, DomainError};
use alondra_core::views::Lang;

pub const MISMATCH: &str = "card_language_mismatch";

fn code(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "en",
        Lang::Es => "es",
    }
}

fn name(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "English",
        Lang::Es => "Spanish",
    }
}

pub fn declared(command: &Command) -> Option<Lang> {
    match command {
        Command::PublishInformation { card, .. }
        | Command::AskQuestion { card, .. }
        | Command::RequestAction { card, .. }
        | Command::PresentResult { card, .. } => card.language,
        Command::AskQuestions { language, .. } => *language,
        _ => None,
    }
}

/// Why a card that declares another language than the user's is not sent. A card that declares
/// none, from a session that started before the field existed, passes.
pub fn refusal(command: &Command, user: Lang) -> Option<DomainError> {
    let declared = declared(command)?;
    (declared != user).then(|| {
        let mut refusal = DomainError::new(
            MISMATCH,
            format!(
                "language: this card declares {}, but the user reads {}. Rewrite the whole card (title, need, why, content and attachments' names) in {} and send it again with the same interaction_id and language \"{}\"",
                name(declared), name(user), name(user), code(user),
            ),
        );
        refusal.subject = Some("language".into());
        refusal
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::{CardKind, CardSpec, InteractionId, Recipient, Scope, TaskId};

    fn card(language: Option<Lang>) -> CardSpec {
        CardSpec { language, ..CardSpec::new(CardKind::Know, "Release design") }
    }

    fn notice(language: Option<Lang>) -> Command {
        Command::PublishInformation {
            attachments: vec![], interaction_id: InteractionId("i".into()), scope: Scope::Task { task_id: TaskId("t".into()) },
            recipient: Recipient::User, body: "Hola".into(), card: card(language),
        }
    }

    fn result(language: Option<Lang>) -> Command {
        Command::PresentResult {
            attachments: vec![], task_id: TaskId("t".into()), interaction_id: InteractionId("i".into()), body: "Hecho".into(),
            targets: vec![], is_final: true, nothing_to_review: true, checklist: vec![], card: card(language),
        }
    }

    fn ask(language: Option<Lang>) -> Command {
        Command::AskQuestions {
            interaction_id: InteractionId("g".into()), scope: Scope::Task { task_id: TaskId("t".into()) }, recipient: Recipient::User,
            blocking: false, why: String::new(), questions: vec![], language,
        }
    }

    #[test]
    fn the_declared_language_of_the_user_passes() {
        for command in [notice(Some(Lang::Es)), result(Some(Lang::Es)), ask(Some(Lang::Es))] {
            assert_eq!(refusal(&command, Lang::Es), None);
        }
    }

    #[test]
    fn another_language_than_the_users_is_refused_with_a_typed_code_that_names_the_fix() {
        for command in [notice(Some(Lang::En)), result(Some(Lang::En)), ask(Some(Lang::En))] {
            let refused = refusal(&command, Lang::Es).expect("refused");
            assert_eq!((refused.code.as_str(), refused.subject.as_deref()), (MISMATCH, Some("language")));
            assert!(refused.message.contains("declares English") && refused.message.contains("language \"es\""), "{}", refused.message);
        }
    }

    #[test]
    fn a_session_that_declares_no_language_passes() {
        for command in [notice(None), result(None), ask(None)] {
            assert_eq!(refusal(&command, Lang::Es), None);
        }
    }

    #[test]
    fn the_text_is_never_read() {
        let mut command = notice(Some(Lang::Es));
        if let Command::PublishInformation { card, body, .. } = &mut command {
            card.need = "Review the whole release".into();
            *body = "This is entirely written in English".into();
        }
        assert_eq!(refusal(&command, Lang::Es), None);
    }
}
