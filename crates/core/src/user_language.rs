use alondra_foundation::Lang;

pub const SYSTEM_LANGUAGE: &str = "macOS AppleLanguages";

/// The language of the user right now: what the TUI, the MCP of a node and the daemon all follow.
pub fn current() -> Lang {
    detect(|k| if k == SYSTEM_LANGUAGE { system_language() } else { std::env::var(k).ok() })
}

fn system_language() -> Option<String> {
    let out = std::process::Command::new("/usr/bin/defaults").args(["read", "-g", "AppleLanguages"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.split(|c: char| c == '"' || c == '(' || c == ',' || c.is_whitespace()).find(|t| !t.is_empty() && *t != ")")?;
    Some(first.to_string())
}

/// `ALONDRA_LANG` wins, then the macOS system language (what the apps follow), then the POSIX
/// order for messages: LC_ALL, LC_MESSAGES, LANG. Terminals often export en_US regardless.
pub fn detect(get: impl Fn(&str) -> Option<String>) -> Lang {
    ["ALONDRA_LANG", SYSTEM_LANGUAGE, "LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| get(k).filter(|v| !v.trim().is_empty()))
        .map_or(Lang::En, |v| from_locale(&v))
}

fn from_locale(v: &str) -> Lang {
    let v = v.trim().to_ascii_lowercase();
    if v == "es" || v.starts_with("es_") || v.starts_with("es-") || v.starts_with("es.") || v.starts_with("es@") {
        Lang::Es
    } else {
        Lang::En
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| pairs.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.clone())
    }

    #[test]
    fn spanish_locales_pick_spanish_and_everything_else_english() {
        for v in ["es_ES.UTF-8", "es", "es_MX", "es-419", "ES_es.utf8", "es.UTF-8", "es@euro"] {
            assert_eq!(detect(env(&[("LANG", v)])), Lang::Es, "{v}");
        }
        for v in ["en_US.UTF-8", "C", "POSIX", "C.UTF-8", "fr_FR.UTF-8", "est_EE", "eu_ES.UTF-8"] {
            assert_eq!(detect(env(&[("LANG", v)])), Lang::En, "{v}");
        }
        assert_eq!(detect(env(&[])), Lang::En);
    }

    #[test]
    fn precedence_is_override_then_lc_all_then_lc_messages_then_lang() {
        assert_eq!(detect(env(&[("ALONDRA_LANG", "en"), ("LC_ALL", "es_ES.UTF-8")])), Lang::En);
        assert_eq!(detect(env(&[("ALONDRA_LANG", "es"), ("LANG", "en_US.UTF-8")])), Lang::Es);
        assert_eq!(detect(env(&[("LC_ALL", "es_ES.UTF-8"), ("LC_MESSAGES", "en_US"), ("LANG", "en_US")])), Lang::Es);
        assert_eq!(detect(env(&[("LC_MESSAGES", "es_ES"), ("LANG", "en_US")])), Lang::Es);
        assert_eq!(detect(env(&[("ALONDRA_LANG", " "), ("LC_ALL", ""), ("LANG", "es_ES")])), Lang::Es);
    }

    #[test]
    fn system_language_beats_the_terminal_locale_but_not_the_override() {
        assert_eq!(detect(env(&[(SYSTEM_LANGUAGE, "es-ES"), ("LC_ALL", "en_US.UTF-8")])), Lang::Es);
        assert_eq!(detect(env(&[("ALONDRA_LANG", "en"), (SYSTEM_LANGUAGE, "es-ES")])), Lang::En);
    }
}
