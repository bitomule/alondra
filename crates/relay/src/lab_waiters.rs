//! Laboratory-only aggregate observation. No HTTP surface or identity/payload labels.
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Instant,
};

pub(crate) const THRESHOLD: u64 = 200;
#[derive(Default)]
struct Counts {
    current: u64,
    peak: u64,
    entered: u64,
    exited: u64,
    above_since: Option<Instant>,
    above: std::time::Duration,
    longest: std::time::Duration,
}
impl Counts {
    fn enter(&mut self, now: Instant) {
        self.current += 1;
        self.peak = self.peak.max(self.current);
        self.entered += 1;
        if self.current == THRESHOLD {
            self.above_since = Some(now);
        }
    }
    fn exit(&mut self, now: Instant) {
        if self.current == THRESHOLD {
            let duration = now.duration_since(self.above_since.take().expect("threshold entry"));
            self.above += duration;
            self.longest = self.longest.max(duration);
        }
        self.current -= 1;
        self.exited += 1;
    }
    fn json(&self, now: Instant) -> Value {
        let ongoing = self
            .above_since
            .map(|start| now.duration_since(start))
            .unwrap_or_default();
        json!({"current":self.current,"peak":self.peak,"entered":self.entered,"exited":self.exited,
            "threshold":THRESHOLD,"above_ms":(self.above+ongoing).as_millis() as u64,
            "longest_above_ms":self.longest.max(ongoing).as_millis() as u64})
    }
}
#[derive(Default)]
pub(crate) struct Gauge {
    state: Mutex<Counts>,
}
impl Gauge {
    // One short bookkeeping lock orders threshold crossings with the clock;
    // separate atomics could reorder enter/exit and invent time above threshold.
    fn enter(&self) {
        self.state.lock().unwrap().enter(Instant::now());
    }
    fn exit(&self) {
        self.state.lock().unwrap().exit(Instant::now());
    }
    fn json(&self) -> Value {
        self.state.lock().unwrap().json(Instant::now())
    }
    fn snapshot(&self, origin: Instant) -> Value {
        let state = self.state.lock().unwrap();
        let now = Instant::now();
        let mut value = state.json(now);
        value["observed_uptime_ms"] = json!(now.duration_since(origin).as_millis() as u64);
        value
    }
    pub(crate) fn observe<F: Future>(self: &Arc<Self>, future: F) -> Observed<F> {
        Observed {
            inner: Box::pin(future),
            gauge: self.clone(),
            counted: false,
        }
    }
}
pub(crate) struct Observed<F> {
    inner: Pin<Box<F>>,
    gauge: Arc<Gauge>,
    counted: bool,
}
impl<F> Observed<F> {
    fn leave(&mut self) {
        if self.counted {
            self.gauge.exit();
            self.counted = false;
        }
    }
}
impl<F: Future> Future for Observed<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.leave(); // A scheduled recheck is not an idle wait.
        match this.inner.as_mut().poll(cx) {
            Poll::Ready(value) => Poll::Ready(value),
            Poll::Pending => {
                this.gauge.enter();
                this.counted = true;
                Poll::Pending
            }
        }
    }
}
impl<F> Drop for Observed<F> {
    fn drop(&mut self) {
        self.leave();
    }
}

#[derive(Default)]
pub(crate) struct Metrics {
    pub changes: Arc<Gauge>,
    pub work: Arc<Gauge>,
}
impl Metrics {
    pub(crate) fn start(self: &Arc<Self>, epoch: String) -> anyhow::Result<()> {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let Ok(path) = std::env::var("ALONDRA_LAB_WAITER_FILE") else {
            return Ok(());
        };
        anyhow::ensure!(
            path == "/system-metrics/waiters.json",
            "laboratory metrics path must be fixed"
        );
        let directory = std::path::Path::new("/system-metrics");
        let metadata = std::fs::symlink_metadata(directory)?;
        anyhow::ensure!(
            metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
            "private laboratory metrics directory required"
        );
        let metrics = self.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            let mut sequence = 0u64;
            loop {
                sequence += 1;
                let changes = metrics.changes.snapshot(started);
                let work = metrics.work.snapshot(started);
                let value = json!({"schema":1,"scope":"lab_pending_futures","epoch":epoch,"sequence":sequence,
                    "uptime_ms":changes["observed_uptime_ms"],"changes":changes,"work":work});
                // Dedicated owned mount only; replace a complete file atomically.
                // Blocking I/O is offloaded and does not hold a request/auth lock.
                let temporary = format!("/system-metrics/waiters-{epoch}.tmp");
                let write = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
                    use std::io::Write;
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&temporary)?;
                    let result = (|| {
                        file.write_all(value.to_string().as_bytes())?;
                        std::fs::rename(&temporary, "/system-metrics/waiters.json")
                    })();
                    if result.is_err() {
                        let _ = std::fs::remove_file(&temporary);
                    }
                    result
                })
                .await;
                if !matches!(write, Ok(Ok(()))) {
                    eprintln!("laboratory waiter metrics stopped");
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn immediate_ready_is_never_counted_and_cancellation_releases_pending() {
        let gauge = Arc::new(Gauge::default());
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut ready = Box::pin(gauge.observe(std::future::ready(7)));
        assert_eq!(ready.as_mut().poll(&mut cx), Poll::Ready(7));
        assert_eq!(gauge.json()["peak"], 0);
        let mut pending = Box::pin(gauge.observe(std::future::pending::<()>()));
        assert!(pending.as_mut().poll(&mut cx).is_pending());
        assert_eq!(gauge.json()["current"], 1);
        assert!(pending.as_mut().poll(&mut cx).is_pending());
        assert_eq!(gauge.json()["current"], 1);
        drop(pending);
        assert_eq!(gauge.json()["current"], 0);
        assert_eq!(gauge.json()["entered"], gauge.json()["exited"]);
    }
    #[test]
    fn peak_tracks_multiple_actual_pending_futures_not_constructed_requests() {
        let gauge = Arc::new(Gauge::default());
        let mut a = Box::pin(gauge.observe(std::future::pending::<()>()));
        let mut b = Box::pin(gauge.observe(std::future::pending::<()>()));
        assert_eq!(gauge.json()["current"], 0);
        let mut cx = Context::from_waker(std::task::Waker::noop());
        assert!(a.as_mut().poll(&mut cx).is_pending());
        assert!(b.as_mut().poll(&mut cx).is_pending());
        assert_eq!(gauge.json()["current"], 2);
        assert_eq!(gauge.json()["peak"], 2);
        drop(a);
        assert_eq!(gauge.json()["current"], 1);
        drop(b);
        assert_eq!(gauge.json()["current"], 0);
        assert_eq!(gauge.json()["peak"], 2);
    }
    #[test]
    fn threshold_time_excludes_below_threshold_and_keeps_continuous_duration() {
        use std::time::Duration;
        let mut state = Counts::default();
        let start = Instant::now();
        for _ in 0..199 {
            state.enter(start);
        }
        assert_eq!(state.json(start + Duration::from_secs(10))["above_ms"], 0);
        state.enter(start + Duration::from_secs(10));
        assert_eq!(
            state.json(start + Duration::from_secs(16))["above_ms"],
            6000
        );
        state.exit(start + Duration::from_secs(17));
        state.enter(start + Duration::from_secs(20));
        state.exit(start + Duration::from_secs(22));
        let sample = state.json(start + Duration::from_secs(30));
        assert_eq!(sample["above_ms"], 9000);
        assert_eq!(sample["longest_above_ms"], 7000);
    }
    #[tokio::test]
    async fn real_pending_future_is_removed_after_notification() {
        let gauge = Arc::new(Gauge::default());
        let notify = Arc::new(tokio::sync::Notify::new());
        let g = gauge.clone();
        let n = notify.clone();
        let task = tokio::spawn(async move { g.observe(n.notified()).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while gauge.json()["current"] != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        notify.notify_one();
        task.await.unwrap();
        assert_eq!(gauge.json()["current"], 0);
        assert_eq!(gauge.json()["peak"], 1);
    }
}
