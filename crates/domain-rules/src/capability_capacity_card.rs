//! Alondra's card for a credential registry with no room for another node: every node holds a
//! credential while its host runs, so this only shows when far more nodes are alive than Alondra
//! ever runs, or credentials of ended hosts were not retired. Starts wait while it is open, and it
//! is withdrawn as soon as a credential is retired.

use serde::{Deserialize, Serialize};

use alondra_core::text::Lang;
use alondra_core::text::{t, tf};

pub const CAPABILITY_CAPACITY_PREFIX: &str = "problem-capability-capacity-";
pub const CAPABILITY_CAPACITY_CAUSE: &str = "capability_capacity";
pub const CAPABILITY_CAPACITY_CODE: &str = "capability_capacity";
pub const CHECK_AGAIN_OPTION: &str = "check_again";
pub const CHECK_AGAIN_LABEL: &str = "Check again";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapabilityCapacityCard {
    /// Credentials of nodes that may still be working, which Alondra cannot retire.
    pub live: u32,
    /// How many credentials the registry holds at most.
    pub limit: u32,
}

impl CapabilityCapacityCard {
    pub fn to_body(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_body(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }

    /// The card's `need` and `why`.
    pub fn text(&self, lang: Lang) -> (String, String) {
        let need = t!(lang, "New tasks can't start: Alondra has no room for another node",
            "Las tareas nuevas no arrancan: Alondra no tiene sitio para otro nodo");
        let why = tf!(lang,
            "{live} of {limit} node credentials belong to nodes that may still work. Tasks wait; closing finished tasks frees them.",
            "{live} de {limit} credenciales de nodo son de nodos que pueden seguir trabajando. Las tareas esperan; cerrar las terminadas las libera.",
            live = self.live, limit = self.limit);
        (need.into(), why)
    }
}

/// The words of the card's option in the viewer's language.
pub fn option_text(id: &str, label: &str, lang: Lang) -> Option<(&'static str, &'static str)> {
    (id == CHECK_AGAIN_OPTION && label == CHECK_AGAIN_LABEL).then(|| (
        t!(lang, "Check again", "Comprobar de nuevo"),
        t!(lang, "Alondra tries the waiting starts again; the card comes back if there is still no room",
            "Alondra vuelve a intentar los arranques en espera; la tarjeta vuelve si sigue sin sitio"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CardSpec;

    #[test]
    fn the_card_says_why_starts_wait_in_both_languages_and_fits_a_card() {
        let card = CapabilityCapacityCard { live: 4096, limit: 4096 };
        assert_eq!(CapabilityCapacityCard::from_body(&card.to_body()), Some(card));
        for lang in [Lang::En, Lang::Es] {
            let (need, why) = card.text(lang);
            assert!(need.chars().count() <= CardSpec::NEED_MAX, "{need}");
            assert!(why.chars().count() <= CardSpec::WHY_MAX, "{why}");
            let (label, description) = option_text(CHECK_AGAIN_OPTION, CHECK_AGAIN_LABEL, lang).unwrap();
            assert!(!label.is_empty() && !description.is_empty());
        }
        assert_eq!(card.text(Lang::Es).0, "Las tareas nuevas no arrancan: Alondra no tiene sitio para otro nodo");
    }
}
