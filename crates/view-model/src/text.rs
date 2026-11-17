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
