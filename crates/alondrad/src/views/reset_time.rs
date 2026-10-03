use alondra_core::Millis;
use alondra_core::views::Lang;

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const FIVE_HOURS: i64 = 5 * HOUR;
const SECONDS_PER_DAY: i64 = 86_400;

pub(crate) fn local_offset_secs(millis: Millis) -> i64 {
    let secs = millis.div_euclid(1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() { 0 } else { tm.tm_gmtoff }
}

struct Civil {
    day: i64,
    weekday: usize,
    month: usize,
    month_day: i64,
    hour: i64,
    minute: i64,
}

fn civil(millis: Millis, offset_secs: i64) -> Civil {
    let local = millis.div_euclid(1000) + offset_secs;
    let day = local.div_euclid(SECONDS_PER_DAY);
    let second = local.rem_euclid(SECONDS_PER_DAY);
    let shifted = day + 719_468;
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let march_based_month = (5 * day_of_year + 2) / 153;
    let month_day = day_of_year - (153 * march_based_month + 2) / 5 + 1;
    let month = if march_based_month < 10 { march_based_month + 2 } else { march_based_month - 10 };
    Civil {
        day,
        weekday: (day + 4).rem_euclid(7) as usize,
        month: month as usize,
        month_day,
        hour: second / 3600,
        minute: second % 3600 / 60,
    }
}

const WEEKDAYS_EN: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const WEEKDAYS_ES: [&str; 7] = ["domingo", "lunes", "martes", "miércoles", "jueves", "viernes", "sábado"];
const MONTHS_EN: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTHS_ES: [&str; 12] = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic"];

pub(crate) fn reset_day_and_time(
    resets_at: Millis,
    now: Millis,
    offset_secs: impl Fn(Millis) -> i64,
    lang: Lang,
) -> String {
    let at = civil(resets_at, offset_secs(resets_at));
    let days_ahead = at.day - civil(now, offset_secs(now)).day;
    let time = format!("{:02}:{:02}", at.hour, at.minute);
    match (lang, days_ahead) {
        (Lang::En, 0) => format!("today at {time}"),
        (Lang::Es, 0) => format!("hoy a las {time}"),
        (Lang::En, 1) => format!("tomorrow at {time}"),
        (Lang::Es, 1) => format!("mañana a las {time}"),
        (Lang::En, 2..=6) => format!("{} at {time}", WEEKDAYS_EN[at.weekday]),
        (Lang::Es, 2..=6) => format!("el {} a las {time}", WEEKDAYS_ES[at.weekday]),
        (Lang::En, _) => format!("{} {} at {time}", MONTHS_EN[at.month], at.month_day),
        (Lang::Es, _) => format!("el {} {} a las {time}", at.month_day, MONTHS_ES[at.month]),
    }
}

pub(crate) fn five_hour_reset(
    resets_at: Millis,
    now: Millis,
    offset_secs: impl Fn(Millis) -> i64,
    lang: Lang,
) -> String {
    let remaining = resets_at - now;
    if remaining <= 0 || remaining >= FIVE_HOURS {
        return reset_day_and_time(resets_at, now, offset_secs, lang);
    }
    let at = civil(resets_at, offset_secs(resets_at));
    let minutes = (remaining + MINUTE - 1) / MINUTE;
    let span = match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    };
    let prefix = match lang {
        Lang::En => "in",
        Lang::Es => "en",
    };
    format!("{prefix} {span} ({:02}:{:02})", at.hour, at.minute)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;
    const PLUS_TWO: i64 = 2 * 3600;
    const NOW: Millis = 1_790_000_000_000;

    fn local_midnight_plus(hours: i64, minutes: i64) -> Millis {
        let local_day_start = (NOW / 1000 + PLUS_TWO).div_euclid(SECONDS_PER_DAY) * SECONDS_PER_DAY - PLUS_TWO;
        (local_day_start + hours * 3600 + minutes * 60) * 1000
    }

    fn day(resets_at: Millis, now: Millis, lang: Lang) -> String {
        reset_day_and_time(resets_at, now, |_| PLUS_TWO, lang)
    }

    #[test]
    fn the_fixture_now_is_monday_the_21st_of_september_in_madrid() {
        let at = civil(NOW, PLUS_TWO);
        assert_eq!((at.month, at.month_day, at.weekday, at.hour, at.minute), (8, 21, 1, 16, 13));
    }

    #[test]
    fn same_day_next_day_week_and_far_dates_read_naturally_in_english() {
        let now = local_midnight_plus(10, 0);
        assert_eq!(day(local_midnight_plus(14, 30), now, Lang::En), "today at 14:30");
        assert_eq!(day(local_midnight_plus(24 + 9, 0), now, Lang::En), "tomorrow at 09:00");
        assert_eq!(day(local_midnight_plus(48 + 9, 0), now, Lang::En), "Wednesday at 09:00");
        assert_eq!(day(now + 6 * DAY, now, Lang::En), "Sunday at 10:00");
        assert_eq!(day(now + 7 * DAY, now, Lang::En), "Sep 28 at 10:00");
        assert_eq!(day(now + 21 * DAY, now, Lang::En), "Oct 12 at 10:00");
    }

    #[test]
    fn same_day_next_day_week_and_far_dates_read_naturally_in_spanish() {
        let now = local_midnight_plus(10, 0);
        assert_eq!(day(local_midnight_plus(14, 30), now, Lang::Es), "hoy a las 14:30");
        assert_eq!(day(local_midnight_plus(24 + 9, 0), now, Lang::Es), "mañana a las 09:00");
        assert_eq!(day(local_midnight_plus(48 + 9, 0), now, Lang::Es), "el miércoles a las 09:00");
        assert_eq!(day(now + 6 * DAY, now, Lang::Es), "el domingo a las 10:00");
        assert_eq!(day(now + 7 * DAY, now, Lang::Es), "el 28 sep a las 10:00");
        assert_eq!(day(now + 21 * DAY, now, Lang::Es), "el 12 oct a las 10:00");
    }

    #[test]
    fn tomorrow_is_decided_by_the_local_calendar_not_by_24_hours() {
        let late = local_midnight_plus(23, 50);
        assert_eq!(day(late + 20 * MINUTE, late, Lang::En), "tomorrow at 00:10");
        assert_eq!(day(late - 23 * HOUR, late, Lang::En), "today at 00:50");
    }

    #[test]
    fn the_offset_of_each_instant_is_used_across_a_clock_change() {
        let offsets = |millis: Millis| if millis < NOW + DAY { 3600 } else { 7200 };
        assert_eq!(reset_day_and_time(NOW + 2 * DAY, NOW, offsets, Lang::En), "Wednesday at 16:13");
    }

    #[test]
    fn the_five_hour_window_counts_down_when_it_is_close() {
        let now = local_midnight_plus(12, 15);
        let es = |minutes: i64| five_hour_reset(now + minutes * MINUTE, now, |_| PLUS_TWO, Lang::Es);
        let en = |minutes: i64| five_hour_reset(now + minutes * MINUTE, now, |_| PLUS_TWO, Lang::En);
        assert_eq!(es(135), "en 2 h 15 min (14:30)");
        assert_eq!(en(135), "in 2 h 15 min (14:30)");
        assert_eq!(es(40), "en 40 min (12:55)");
        assert_eq!(en(40), "in 40 min (12:55)");
        assert_eq!(es(120), "en 2 h (14:15)");
        assert_eq!(en(60), "in 1 h (13:15)");
        assert_eq!(es(299), "en 4 h 59 min (17:14)");
    }

    #[test]
    fn the_five_hour_window_falls_back_to_a_day_when_far_or_already_past() {
        let now = local_midnight_plus(12, 15);
        let far = now + 5 * HOUR;
        assert_eq!(five_hour_reset(far, now, |_| PLUS_TWO, Lang::Es), "hoy a las 17:15");
        assert_eq!(five_hour_reset(far, now, |_| PLUS_TWO, Lang::En), "today at 17:15");
        assert_eq!(five_hour_reset(now - MINUTE, now, |_| PLUS_TWO, Lang::En), "today at 12:14");
    }

    #[test]
    fn the_machine_offset_lookup_stays_within_real_zones() {
        assert!(local_offset_secs(NOW).abs() <= 14 * 3600);
    }
}
