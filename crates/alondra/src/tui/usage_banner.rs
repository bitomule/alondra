use alondra_core::views::UsageExhausted;

/// The line shown across the top while every Claude account is at its real limit: only then do launches wait.
pub(super) fn banner_text(usage: &UsageExhausted, now_millis: i64) -> String {
    if usage.paused_by_user {
        let clock = usage.until.get(11..16).unwrap_or(&usage.until);
        return trf!("Paused by you until tomorrow {clock}", "Pausado por ti hasta mañana {clock}");
    }
    let when = when_text(&usage.until, now_millis);
    let (exhausted, total) = (usage.macs_exhausted, usage.macs_total);
    if total > 1 && exhausted >= total {
        trf!(
            "No margin on any of your Macs {when} · every Claude account is at its limit",
            "Ningún Mac tiene margen {when} · todas las cuentas de Claude están en su límite"
        )
    } else if total > 1 {
        trf!(
            "{exhausted} of {total} Macs with no margin {when} · their Claude accounts are at their limit",
            "{exhausted} de {total} Macs sin margen {when} · sus cuentas de Claude están en su límite"
        )
    } else if usage.scope == "this_mac" {
        trf!(
            "No margin on this Mac {when} · every Claude account is at its limit",
            "Sin margen en este Mac {when} · todas las cuentas de Claude están en su límite"
        )
    } else {
        trf!(
            "No margin {when} · every Claude account is at its limit",
            "Sin margen {when} · todas las cuentas de Claude están en su límite"
        )
    }
}

fn when_text(until: &str, now_millis: i64) -> String {
    let Some(until_millis) = parse_millis(until) else {
        return trf!("until {until}", "hasta {until}");
    };
    let minutes = (until_millis - now_millis + 59_999).div_euclid(60_000);
    if (1..60).contains(&minutes) {
        return trf!("for {minutes} more min", "durante {minutes} min más");
    }
    let clock = until.get(11..16).unwrap_or(until);
    trf!("until {clock}", "hasta {clock}")
}

/// Milliseconds since the epoch of "2026-10-05T14:30:00.000+02:00" (fraction and offset as the daemon writes them).
fn parse_millis(iso: &str) -> Option<i64> {
    let number = |range: std::ops::Range<usize>| iso.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    let rest = iso.get(19..)?;
    let (fraction, zone) = match rest.strip_prefix('.') {
        Some(after) => {
            let digits = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
            let millis = format!("{:0<3}", &after[..digits.min(3)]).parse::<i64>().ok()?;
            (millis, &after[digits..])
        }
        None => (0, rest),
    };
    let offset_minutes = match zone {
        "Z" => 0,
        _ => {
            let sign = match zone.get(0..1)? {
                "+" => 1,
                "-" => -1,
                _ => return None,
            };
            sign * (zone.get(1..3)?.parse::<i64>().ok()? * 60 + zone.get(4..6)?.parse::<i64>().ok()?)
        }
    };
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_minutes * 60;
    Some(seconds * 1000 + fraction)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Lang, set_for_this_thread};

    fn usage(scope: &str, until: &str, exhausted: u32, total: u32) -> UsageExhausted {
        UsageExhausted { scope: scope.into(), until: until.into(), macs_exhausted: exhausted, macs_total: total, paused_by_user: false }
    }

    #[test]
    fn iso_with_an_offset_is_read_as_that_instant() {
        assert_eq!(parse_millis("1970-01-01T00:00:00.000+00:00"), Some(0));
        assert_eq!(parse_millis("1970-01-01T02:00:00.500+02:00"), Some(500));
        assert_eq!(parse_millis("2026-10-05T14:30:00Z"), Some(1_791_210_600_000));
        assert_eq!(parse_millis("nonsense"), None);
    }

    #[test]
    fn says_until_when_and_which_macs_in_both_languages() {
        let now = parse_millis("2026-10-05T10:00:00.000+02:00").unwrap();
        let single = usage("this_mac", "2026-10-05T14:30:00.000+02:00", 1, 1);
        set_for_this_thread(Lang::En);
        assert_eq!(banner_text(&single, now), "No margin on this Mac until 14:30 · every Claude account is at its limit");
        set_for_this_thread(Lang::Es);
        assert_eq!(banner_text(&single, now), "Sin margen en este Mac hasta 14:30 · todas las cuentas de Claude están en su límite");
        let some = usage("all_macs", "2026-10-05T14:30:00.000+02:00", 2, 3);
        assert_eq!(banner_text(&some, now), "2 de 3 Macs sin margen hasta 14:30 · sus cuentas de Claude están en su límite");
        let all = usage("all_macs", "2026-10-05T14:30:00.000+02:00", 3, 3);
        assert_eq!(banner_text(&all, now), "Ningún Mac tiene margen hasta 14:30 · todas las cuentas de Claude están en su límite");
        set_for_this_thread(Lang::En);
        assert_eq!(banner_text(&all, now), "No margin on any of your Macs until 14:30 · every Claude account is at its limit");
    }

    #[test]
    fn a_pause_by_the_user_says_so_in_both_languages() {
        let now = parse_millis("2026-10-05T20:00:00.000+02:00").unwrap();
        let paused = UsageExhausted { paused_by_user: true, ..usage("this_mac", "2026-10-06T00:00:00.000+02:00", 1, 1) };
        set_for_this_thread(Lang::En);
        assert_eq!(banner_text(&paused, now), "Paused by you until tomorrow 00:00");
        set_for_this_thread(Lang::Es);
        assert_eq!(banner_text(&paused, now), "Pausado por ti hasta mañana 00:00");
    }

    #[test]
    fn under_an_hour_counts_down() {
        set_for_this_thread(Lang::En);
        let now = parse_millis("2026-10-05T10:00:00.000+02:00").unwrap();
        let soon = usage("this_mac", "2026-10-05T10:25:00.000+02:00", 1, 1);
        assert_eq!(banner_text(&soon, now), "No margin on this Mac for 25 more min · every Claude account is at its limit");
    }
}
