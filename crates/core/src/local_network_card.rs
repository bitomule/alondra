//! Alondra's card for a Mac whose Local Network permission macOS denies to alondrad: devices on
//! the same Wi-Fi then reach it only through the relay. The body says whether alondrad runs under
//! the launcher, which keeps the permission across updates; without it the card also asks for the
//! one command that sets it up.

use serde::{Deserialize, Serialize};

use crate::views::Lang;
use crate::views::text::{t, tf};

pub const LOCAL_NETWORK_PREFIX: &str = "local-network-";
pub const LOCAL_NETWORK_CAUSE: &str = "local_network";
pub const LOCAL_NETWORK_ALLOWED_OPTION: &str = "allowed";
pub const LOCAL_NETWORK_ALLOWED_LABEL: &str = "I've allowed it";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalNetworkCard {
    /// alondrad runs under the launcher at a fixed path.
    pub launcher: bool,
    /// The user said they allowed it and macOS still denies it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub still: bool,
}

impl LocalNetworkCard {
    pub fn to_body(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_body(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }

    /// The card's `need` and `why`.
    pub fn text(&self, lang: Lang) -> (String, String) {
        let need = if self.still {
            t!(lang, "macOS still keeps Alondra off this Mac's local network", "macOS sigue sin dejar a Alondra usar la red local de este Mac")
        } else {
            t!(lang, "Allow Alondra to use this Mac's local network", "Permite que Alondra use la red local de este Mac")
        };
        let steps = t!(lang,
            "System Settings → Privacy & Security → Local Network → turn on Alondra (alondrad).",
            "Ajustes del Sistema → Privacidad y seguridad → Red local → activa Alondra (alondrad).");
        let then = if self.launcher {
            t!(lang, "Until then devices on this Wi-Fi reach it through the relay, slower.",
                "Hasta entonces los dispositivos en esta Wi-Fi llegan por el relay, más lento.").to_owned()
        } else {
            tf!(lang, "Then run {command} once so updates keep it.",
                "Luego ejecuta una vez {command} para que las actualizaciones lo conserven.", command = "alondra doctor --fix")
        };
        (need.into(), format!("{steps} {then}"))
    }
}

/// The words of the card's option in the viewer's language.
pub fn option_text(id: &str, label: &str, lang: Lang) -> Option<(&'static str, &'static str)> {
    (id == LOCAL_NETWORK_ALLOWED_OPTION && label == LOCAL_NETWORK_ALLOWED_LABEL).then(|| (
        t!(lang, "I've allowed it", "Ya lo he permitido"),
        t!(lang, "Alondra checks again now and closes this card if macOS lets it through",
            "Alondra lo comprueba ahora y cierra esta tarjeta si macOS ya lo deja pasar"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::CardSpec;

    #[test]
    fn the_card_says_where_to_allow_it_in_both_languages_and_fits_a_card() {
        for card in [LocalNetworkCard { launcher: true, still: false }, LocalNetworkCard { launcher: false, still: true }] {
            assert_eq!(LocalNetworkCard::from_body(&card.to_body()), Some(card));
            for lang in [Lang::En, Lang::Es] {
                let (need, why) = card.text(lang);
                assert!(need.chars().count() <= CardSpec::NEED_MAX, "{need}");
                assert!(why.chars().count() <= CardSpec::WHY_MAX, "{why}");
            }
        }
        let (need, why) = LocalNetworkCard { launcher: true, still: false }.text(Lang::Es);
        assert_eq!(need, "Permite que Alondra use la red local de este Mac");
        assert_eq!(why, "Ajustes del Sistema → Privacidad y seguridad → Red local → activa Alondra (alondrad). Hasta entonces los dispositivos en esta Wi-Fi llegan por el relay, más lento.");
    }

    #[test]
    fn without_the_launcher_the_card_asks_for_the_command_that_keeps_it_across_updates() {
        let (_, why) = LocalNetworkCard { launcher: false, still: false }.text(Lang::En);
        assert!(why.ends_with("Then run alondra doctor --fix once so updates keep it."), "{why}");
        assert!(!LocalNetworkCard { launcher: true, still: false }.text(Lang::En).1.contains("doctor"));
    }

    #[test]
    fn only_the_cards_own_option_is_reworded() {
        assert_eq!(option_text(LOCAL_NETWORK_ALLOWED_OPTION, LOCAL_NETWORK_ALLOWED_LABEL, Lang::Es).map(|(label, _)| label), Some("Ya lo he permitido"));
        assert_eq!(option_text(LOCAL_NETWORK_ALLOWED_OPTION, "Something else", Lang::Es), None);
    }
}
