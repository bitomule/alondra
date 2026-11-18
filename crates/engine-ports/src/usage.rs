//! The usage clock as the engine's pieces word it, and how they tell the user about it.
use alondra_core::Millis;
use std::future::Future;

pub fn local_at(now: Millis, days_ahead: i32, hour: i32, minute: i32) -> Millis {
    let t = now.div_euclid(1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    tm.tm_mday += days_ahead;
    tm.tm_hour = hour;
    tm.tm_min = minute;
    tm.tm_sec = 0;
    tm.tm_isdst = -1;
    unsafe { libc::mktime(&mut tm) as Millis * 1000 }
}

pub fn local_midnight(millis: Millis) -> Millis {
    local_at(millis, 0, 0, 0)
}

/// "15:45", with the weekday in front when it is not today.
pub fn clock_text(millis: Millis, now: Millis, spanish: bool) -> String {
    let t = millis.div_euclid(1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    let time = format!("{:02}:{:02}", tm.tm_hour, tm.tm_min);
    if local_midnight(millis) == local_midnight(now) {
        return time;
    }
    let days = if spanish { ["dom", "lun", "mar", "mié", "jue", "vie", "sáb"] } else { ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"] };
    format!("{} {time}", days[tm.tm_wday as usize])
}

/// How the user hears it: a push that never carries a card.
pub trait Alerts: Sync {
    fn tell(&self, text: impl Fn(bool) -> String + Send + Sync, passive: bool) -> impl Future<Output = ()> + Send;
}
