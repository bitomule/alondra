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

/// Whether an overdue step is acted on. Telling a node its work ended is, and so is what the
/// orchestrator owes. Handing an input waits for a day of `obligations HandInput` lines measured
/// from the session's own `running` event, which fix its deadline at the larger of `HAND_INPUT`
/// and ten times their p99.
pub fn enforced(owed: &Owed) -> bool {
    matches!(owed, Owed::Wake | Owed::Answer)
}
