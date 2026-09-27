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

#[derive(Debug)]
struct CaptureError {
    phase: &'static str,
    source: std::io::Error,
}
fn at(phase: &'static str) -> impl FnOnce(std::io::Error) -> CaptureError {
    move |source| CaptureError { phase, source }
}
fn read_challenge(directory: &std::path::Path, name: &str) -> Result<Option<String>, CaptureError> {
    use std::os::unix::fs::OpenOptionsExt;
    // Bind validation and reading to one inode, even when the publisher atomically
    // replaces the pathname. Never follow a symlink and never retry away a failure.
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join(name))
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(at("challenge_open")(e)),
    };
    read_challenge_file(file).map(Some)
}
fn read_challenge_file(file: std::fs::File) -> Result<String, CaptureError> {
    use std::io::Read;
    let metadata = file.metadata().map_err(at("challenge_metadata"))?;
    if !metadata.is_file() || metadata.len() != 32 {
        return Err(at("challenge_shape")(std::io::Error::from(
            std::io::ErrorKind::InvalidData,
        )));
    }
    let mut nonce = String::new();
    file.take(33)
        .read_to_string(&mut nonce)
        .map_err(at("challenge_read"))?;
    if nonce.len() != 32 || !nonce.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(at("challenge_content")(std::io::Error::from(
            std::io::ErrorKind::InvalidData,
        )));
    }
    Ok(nonce)
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
                // Read challenges before counters: an old captured snapshot can
                // never acknowledge an observation request issued later.
                let metrics = metrics.clone();
                let epoch = epoch.clone();
                let write = tokio::task::spawn_blocking(move || -> Result<(), CaptureError> {
                    let challenge=read_challenge(directory,"challenge")?;
                    let sampler_challenge=read_challenge(directory,"sampler-challenge")?;
                    let changes=metrics.changes.snapshot(started);let work=metrics.work.snapshot(started);
                    #[allow(unused_mut)]
                    let mut value=json!({"schema":1,"scope":"lab_pending_futures","epoch":epoch,"sequence":sequence,
                        "challenge":challenge,"sampler_challenge":sampler_challenge,
                        "uptime_ms":changes["observed_uptime_ms"],"changes":changes,"work":work});
                    #[cfg(feature="lab-waiter-metrics")]
                    {value["sql_errors"]=crate::management::lab_metrics::snapshot();}
                    let temporary=format!("/system-metrics/waiters-{epoch}.tmp");
                    use std::io::Write;
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&temporary).map_err(at("snapshot_open"))?;
                    let result = (|| {
                        file.write_all(value.to_string().as_bytes()).map_err(at("snapshot_write"))?;
                        std::fs::rename(&temporary, "/system-metrics/waiters.json").map_err(at("snapshot_rename"))
                    })();
                    if result.is_err() {
                        let _ = std::fs::remove_file(&temporary);
                    }
                    result
                })
                .await;
                if !matches!(write, Ok(Ok(()))) {
                    match write {
                        Ok(Err(error)) => eprintln!(
                            "laboratory waiter metrics stopped phase={} kind={:?} errno={}",
                            error.phase,
                            error.source.kind(),
                            error.source.raw_os_error().unwrap_or(0)
                        ),
                        Err(error) => eprintln!(
                            "laboratory waiter metrics stopped phase=join panic={} cancelled={}",
                            error.is_panic(),
                            error.is_cancelled()
                        ),
                        _ => unreachable!(),
                    }
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
    fn challenge_reads_bound_inode_and_reject_symlink_or_partial_publication() {
        let directory =
            std::env::temp_dir().join(format!("lab-challenge-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("challenge");
        std::fs::write(&path, "a".repeat(32)).unwrap();
        let opened = std::fs::File::open(&path).unwrap();
        std::fs::write(directory.join("next"), "b".repeat(32)).unwrap();
        std::fs::rename(directory.join("next"), &path).unwrap();
        assert_eq!(read_challenge_file(opened).unwrap(), "a".repeat(32));
        assert_eq!(
            read_challenge(&directory, "challenge").unwrap(),
            Some("b".repeat(32))
        );
        std::os::unix::fs::symlink(&path, directory.join("link")).unwrap();
        assert_eq!(
            read_challenge(&directory, "link").unwrap_err().phase,
            "challenge_open"
        );
        std::fs::write(&path, "b").unwrap();
        assert_eq!(
            read_challenge(&directory, "challenge").unwrap_err().phase,
            "challenge_shape"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn atomic_challenge_replacements_keep_reads_complete() {
        let directory =
            std::env::temp_dir().join(format!("lab-challenge-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("challenge"), "a".repeat(32)).unwrap();
        let publish = directory.clone();
        let writer = std::thread::spawn(move || {
            for i in 0..200 {
                let value = if i % 2 == 0 { "a" } else { "b" };
                std::fs::write(publish.join("next"), value.repeat(32)).unwrap();
                std::fs::rename(publish.join("next"), publish.join("challenge")).unwrap();
            }
        });
        for _ in 0..200 {
            let value = read_challenge(&directory, "challenge").unwrap().unwrap();
            assert!(value == "a".repeat(32) || value == "b".repeat(32));
        }
        writer.join().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
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
