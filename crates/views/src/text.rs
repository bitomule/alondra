pub use alondra_foundation::Lang;
pub use alondra_view_model::text::short_duration;
pub(crate) use alondra_core::text::{t, tf};


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
