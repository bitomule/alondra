//! The wall clock the schedule runner waits on: a timer that fires when the wall clock reaches an
//! instant, also across a sleep, and the changes of the system clock and of the time zone.
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockChange {
    SystemClock,
    TimeZone,
}

/// A value the clock changes while it is kept: dropping it stops what feeds it.
pub struct Watch<T> {
    pub receiver: watch::Receiver<T>,
    _source: Box<dyn Send>,
}

impl<T> Watch<T> {
    pub fn new(receiver: watch::Receiver<T>, source: impl Send + 'static) -> Self {
        Self { receiver, _source: Box::new(source) }
    }
}

pub trait ScheduleClock: Send + Sync {
    /// Wakes on every change of the system clock or the time zone, from now on.
    fn changes(&self) -> Watch<ClockChange>;

    /// Turns true once the wall clock reaches `unix_millis`.
    fn timer_at(&self, unix_millis: i64) -> Watch<bool>;
}
