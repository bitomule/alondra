use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct LiveStoreRead {
    pub getter: &'static str,
    pub statement_attempts: usize,
    pub rows: usize,
    pub failed: bool,
    pub elapsed_ms: f64,
}

pub(super) type Sink = Arc<Mutex<Vec<LiveStoreRead>>>;

pub(super) struct Read {
    sink: Sink,
    event: LiveStoreRead,
    started: Instant,
}

impl Read {
    pub(super) fn new(sink: Sink, getter: &'static str) -> Self {
        Self { sink, event: LiveStoreRead { getter, statement_attempts: 0, rows: 0, failed: true, elapsed_ms: 0.0 }, started: Instant::now() }
    }
    pub(super) fn statement(&mut self) { self.event.statement_attempts += 1; }
    pub(super) fn row(&mut self) { self.event.rows += 1; }
    pub(super) fn complete(&mut self) { self.event.failed = false; }
}

impl Drop for Read {
    fn drop(&mut self) {
        self.event.elapsed_ms = self.started.elapsed().as_secs_f64() * 1_000.0;
        self.sink.lock().expect("live read trace poisoned").push(self.event.clone());
    }
}
