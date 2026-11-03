//! When a local-time schedule is due next.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Frequency { Daily, Weekly, EveryHours, EveryDays, EveryMonths, Once }

/// Uses the Mac's local calendar, including its daylight-saving rules. Search days rather
/// than adding 24 hours, which would drift across a clock change.
pub fn next_after(frequency: Frequency, hour: u8, minute: u8, weekday: Option<u8>, interval: Option<u32>, start_at: Option<i64>, after: i64) -> Option<i64> {
    if frequency == Frequency::Once { return start_at.filter(|at| *at > after); }
    if frequency == Frequency::EveryHours {
        let start = start_at?;
        let step = i64::from(interval?) * 3_600_000;
        return Some(if start > after { start } else { start + (after - start).div_euclid(step).saturating_add(1) * step });
    }
    let seconds = after.div_euclid(1000) as libc::time_t;
    let mut day = unsafe { std::mem::zeroed::<libc::tm>() };
    unsafe { libc::localtime_r(&seconds, &mut day) };
    if matches!(frequency, Frequency::EveryDays | Frequency::EveryMonths) {
        let start = start_at?;
        if start > after { return Some(start); }
        let mut anchor = unsafe { std::mem::zeroed::<libc::tm>() };
        let anchor_seconds = start.div_euclid(1000) as libc::time_t;
        unsafe { libc::localtime_r(&anchor_seconds, &mut anchor) };
        let stride = interval? as i32;
        let difference = if frequency == Frequency::EveryDays {
            // Local calendar dates, not fixed 24-hour periods: DST does not move the wall time.
            let mut a = anchor; a.tm_hour = 12; a.tm_min = 0; a.tm_sec = 0; a.tm_isdst = -1;
            let mut b = day; b.tm_hour = 12; b.tm_min = 0; b.tm_sec = 0; b.tm_isdst = -1;
            ((unsafe { libc::mktime(&mut b) - libc::mktime(&mut a) } + 43_200) / 86_400) as i32
        } else { (day.tm_year - anchor.tm_year) * 12 + day.tm_mon - anchor.tm_mon };
        let mut index = (difference.max(0) / stride).max(0);
        loop {
            let mut candidate = anchor;
            if frequency == Frequency::EveryDays { candidate.tm_mday += index * stride; }
            else {
                candidate.tm_mday = 1;
                candidate.tm_mon += index * stride;
                candidate.tm_isdst = -1;
                unsafe { libc::mktime(&mut candidate) };
                let last_day = days_in_month(candidate.tm_year + 1900, candidate.tm_mon + 1);
                candidate.tm_mday = anchor.tm_mday.min(last_day);
            }
            candidate.tm_isdst = -1;
            let at = unsafe { libc::mktime(&mut candidate) } as i64 * 1000 + start.rem_euclid(1000);
            if at > after { return Some(at); }
            index += 1;
        }
    }
    for offset in 0..=8 {
        let mut candidate = day;
        candidate.tm_mday += offset;
        candidate.tm_hour = hour as i32;
        candidate.tm_min = minute as i32;
        candidate.tm_sec = 0;
        candidate.tm_isdst = -1;
        let at = unsafe { libc::mktime(&mut candidate) } as i64 * 1000;
        if at <= after { continue; }
        if frequency == Frequency::Weekly && weekday != Some(candidate.tm_wday as u8) { continue; }
        // A nonexistent spring-forward time is moved by mktime to the next real minute.
        return Some(at);
    }
    unreachable!("one of the next eight local days matches")
}

fn days_in_month(year: i32, month: i32) -> i32 {
    let next_month = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let mut first = unsafe { std::mem::zeroed::<libc::tm>() };
    first.tm_year = next_month.0 - 1900;
    first.tm_mon = next_month.1 - 1;
    first.tm_mday = 0;
    first.tm_hour = 12;
    first.tm_isdst = -1;
    unsafe { libc::mktime(&mut first) };
    first.tm_mday
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::now_millis;

    fn local_at(year: i32, month: i32, day: i32, hour: i32, minute: i32) -> i64 {
        let mut local = unsafe { std::mem::zeroed::<libc::tm>() };
        local.tm_year = year - 1900;
        local.tm_mon = month - 1;
        local.tm_mday = day;
        local.tm_hour = hour;
        local.tm_min = minute;
        local.tm_isdst = -1;
        unsafe { libc::mktime(&mut local) as i64 * 1000 }
    }

    #[test]
    fn custom_intervals_and_one_time_preserve_the_anchor() {
        let jan31 = local_at(2027, 1, 31, 9, 30);
        let feb28 = local_at(2027, 2, 28, 9, 30);
        let mar31 = local_at(2027, 3, 31, 9, 30);
        assert_eq!(next_after(Frequency::EveryMonths, 0, 0, None, Some(1), Some(jan31), jan31), Some(feb28));
        assert_eq!(next_after(Frequency::EveryMonths, 0, 0, None, Some(1), Some(jan31), feb28), Some(mar31));
        assert_eq!(next_after(Frequency::EveryDays, 0, 0, None, Some(3), Some(jan31), jan31), Some(local_at(2027, 2, 3, 9, 30)));
        assert_eq!(next_after(Frequency::EveryHours, 0, 0, None, Some(5), Some(jan31), jan31 + 5 * 3_600_000), Some(jan31 + 10 * 3_600_000));
        assert_eq!(next_after(Frequency::Once, 0, 0, None, None, Some(jan31), jan31 - 1), Some(jan31));
        assert_eq!(next_after(Frequency::Once, 0, 0, None, None, Some(jan31), jan31), None);
    }

    #[test]
    fn next_occurrence_respects_local_calendar_and_weekday() {
        let now = now_millis();
        for day in 0..7 {
            let next = next_after(Frequency::Weekly, 10, 30, Some(day), None, None, now).unwrap();
            assert!(next > now && next <= now + 8 * 86_400_000);
            let seconds = (next / 1000) as libc::time_t;
            let mut local = unsafe { std::mem::zeroed() };
            unsafe { libc::localtime_r(&seconds, &mut local) };
            assert_eq!((local.tm_wday, local.tm_hour, local.tm_min), (day as i32, 10, 30));
        }
    }
}
