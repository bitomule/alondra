pub use alondra_foundation::Lang;
pub(crate) use alondra_core::text::{t, tf};

/// How long something ran, as a list reads it in either language: "<1 min", "12 min", "1 h 05 min", "2 d 3 h".
pub fn short_duration(secs: u64) -> String {
    match secs {
        0..60 => "<1 min".into(),
        60..3_600 => format!("{} min", secs / 60),
        3_600..86_400 => format!("{} h {:02} min", secs / 3_600, secs % 3_600 / 60),
        _ => format!("{} d {} h", secs / 86_400, secs % 86_400 / 3_600),
    }
}

/// The words of a model's effort level, or none for one Alondra does not know.
pub fn effort_label(effort: &str, lang: Lang) -> Option<&'static str> {
    Some(match effort {
        "auto" => t!(lang, "Auto", "Automático"),
        "light" => t!(lang, "Light", "Ligero"),
        "normal" => t!(lang, "Normal", "Normal"),
        "hard" => t!(lang, "Hard", "A fondo"),
        "none" => t!(lang, "None", "Ninguno"),
        "minimal" => t!(lang, "Minimal", "Mínimo"),
        "low" => t!(lang, "Low", "Bajo"),
        "medium" => t!(lang, "Medium", "Medio"),
        "high" => t!(lang, "High", "Alto"),
        "xhigh" => t!(lang, "Extra high", "Muy alto"),
        "max" => t!(lang, "Maximum", "Máximo"),
        "ultra" => t!(lang, "Ultra", "Ultra"),
        _ => return None,
    })
}
