//! The only place a deadline can exist: the steps Alondra or the provider owes a task that no
//! provider event is guaranteed to end.
use alondra_domain::Owed;

pub type Millis = alondra_foundation::Millis;

pub const HAND_INPUT: Millis = 120_000;
pub const WAKE: Millis = 120_000;
pub const ANSWER: Millis = 600_000;

pub fn deadline(owed: &Owed) -> Option<Millis> {
    match owed {
        Owed::HandInput => Some(HAND_INPUT),
        Owed::Wake => Some(WAKE),
        Owed::Answer => Some(ANSWER),
        _ => None,
    }
}

/// Whether an overdue step is acted on. None is, in this phase: the clock only measures.
pub fn enforced(_owed: &Owed) -> bool {
    false
}
