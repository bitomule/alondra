//! The wall clock of a test: its timers fire when the test moves the clock to their instant, and
//! the test changes the system clock or the time zone. No timer of the Mac is set.
use alondra_engine_ports::schedule_clock::{ClockChange, ScheduleClock, Watch};
use std::sync::{Mutex, PoisonError};
use tokio::sync::watch;

pub struct Clock {
    timers: Mutex<Vec<(i64, watch::Sender<bool>)>>,
    asked: watch::Sender<Vec<i64>>,
    changes: watch::Sender<ClockChange>,
}

impl Default for Clock {
    fn default() -> Self {
        Self { timers: Mutex::default(), asked: watch::channel(Vec::new()).0, changes: watch::channel(ClockChange::SystemClock).0 }
    }
}

impl Clock {
    /// Waits until a timer is asked for at `unix_millis`.
    pub async fn asked_for(&self, unix_millis: i64) {
        let mut asked = self.asked.subscribe();
        asked.wait_for(|asked| asked.contains(&unix_millis)).await.expect("the clock outlives its waiters");
    }

    /// Fires every timer still kept whose instant is `unix_millis` or earlier.
    pub fn reach(&self, unix_millis: i64) {
        let mut timers = self.timers.lock().unwrap_or_else(PoisonError::into_inner);
        timers.retain(|(_, timer)| !timer.is_closed());
        for (_, timer) in timers.iter().filter(|(at, _)| *at <= unix_millis) {
            timer.send_replace(true);
        }
    }

    /// The system clock or the time zone changes.
    pub fn change(&self, change: ClockChange) {
        self.changes.send_replace(change);
    }
}

impl ScheduleClock for Clock {
    fn changes(&self) -> Watch<ClockChange> {
        Watch::new(self.changes.subscribe(), ())
    }

    fn timer_at(&self, unix_millis: i64) -> Watch<bool> {
        let (timer, fired) = watch::channel(false);
        self.timers.lock().unwrap_or_else(PoisonError::into_inner).push((unix_millis, timer));
        self.asked.send_modify(|asked| asked.push(unix_millis));
        Watch::new(fired, ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_timer_fires_only_once_the_clock_reaches_its_instant() {
        let clock = Clock::default();
        let mut timer = clock.timer_at(20);
        clock.asked_for(20).await;
        clock.reach(19);
        assert!(!*timer.receiver.borrow_and_update());
        clock.reach(20);
        assert!(*timer.receiver.borrow_and_update());
    }

    #[tokio::test]
    async fn a_change_of_the_clock_wakes_who_watches_it() {
        let clock = Clock::default();
        let mut changes = clock.changes();
        clock.change(ClockChange::TimeZone);
        changes.receiver.changed().await.unwrap();
        assert_eq!(*changes.receiver.borrow_and_update(), ClockChange::TimeZone);
    }
}
