pub use alondra_foundation::Lang;
macro_rules! t {
    ($lang:expr, $en:literal, $es:literal) => {
        match $lang {
            $crate::views::Lang::En => $en,
            $crate::views::Lang::Es => $es,
        }
    };
}
pub(crate) use t;
macro_rules! tf { ($lang:expr, $en:literal, $es:literal $(, $name:ident = $value:expr)* $(,)?) => { match $lang { $crate::views::Lang::En => format!($en $(, $name = $value)*), $crate::views::Lang::Es => format!($es $(, $name = $value)*) } }; }
pub(crate) use tf;

/// How long something ran, as a list reads it in either language: "<1 min", "12 min", "1 h 05 min", "2 d 3 h".
pub fn short_duration(secs: u64) -> String {
    match secs {
        0..60 => "<1 min".into(),
        60..3_600 => format!("{} min", secs / 60),
        3_600..86_400 => format!("{} h {:02} min", secs / 3_600, secs % 3_600 / 60),
        _ => format!("{} d {} h", secs / 86_400, secs % 86_400 / 3_600),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn short_durations_read_in_minutes_hours_and_days() {
        let read = |secs| super::short_duration(secs);
        assert_eq!(
            [read(0), read(59), read(60), read(59 * 60 + 59), read(3_600), read(3_900), read(86_399), read(86_400), read(2 * 86_400 + 3 * 3_600 + 59)],
            ["<1 min", "<1 min", "1 min", "59 min", "1 h 00 min", "1 h 05 min", "23 h 59 min", "1 d 0 h", "2 d 3 h"],
        );
    }
}
