//! The only place a deadline can exist: the steps Alondra or the provider owes a task that no
//! provider event is guaranteed to end. What the orchestrator owes has none: it is settled by
//! state, when a turn of the orchestrator ends (`orchestrator_inbox`).
use alondra_domain::Owed;

pub type Millis = alondra_foundation::Millis;

/// How long a node's process that holds an input it was handed may write nothing at all before
/// Alondra gives it up as hung.
pub const HAND_INPUT: Millis = 300_000;
pub const WAKE: Millis = 120_000;

pub fn deadline(owed: &Owed) -> Option<Millis> {
    match owed {
        Owed::HandInput => Some(HAND_INPUT),
        Owed::Wake => Some(WAKE),
        _ => None,
    }
}

/// Whether an overdue step is acted on. Telling a node its work ended is. Handing an input is the last resort for a hung process: one that is alive
/// and has written nothing at all for `HAND_INPUT` since its last output, with no event of the
/// provider naming the input. An input the provider holds owes no deadline.
pub fn enforced(owed: &Owed) -> bool {
    matches!(owed, Owed::HandInput | Owed::Wake)
}
