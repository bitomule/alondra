//! The processes running on the Mac and the signals sent to them, for the engine to tell what a
//! paused node still runs and to end it.
pub use alondra_process_group::Entry as Process;

pub trait Processes: Send + Sync {
    /// Every process running now; empty when the table cannot be read.
    fn table(&self) -> Vec<Process>;
    /// Sends `signal` to each of `pids`.
    fn signal(&self, pids: &[u32], signal: i32);
}
