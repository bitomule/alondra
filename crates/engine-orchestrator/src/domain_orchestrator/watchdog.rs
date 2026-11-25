use alondra_core::clock;
use alondra_core::clock::Instant;
use super::*;
use std::sync::atomic::Ordering;

const STALLED_AFTER: Duration = Duration::from_secs(5 * 60);

pub(crate) struct HostGuard {
    domain: Arc<DomainRuntime>,
    run: String,
}
impl HostGuard {
    pub(crate) fn attach(
        domain: Arc<DomainRuntime>,
        run: &str,
        stop: alondra_engine_ports::agent_host::HostStop,
    ) -> Result<Self> {
        {
            let mut current = domain
                .orchestrator()
                .host
                .lock_or_recover("the orchestrator host");
            anyhow::ensure!(
                current.is_none(),
                "another orchestrator host is still registered"
            );
            *current = Some((run.to_owned(), stop));
        }
        Ok(Self {
            domain,
            run: run.to_owned(),
        })
    }
}
impl Drop for HostGuard {
    fn drop(&mut self) {
        let mut current = self
            .domain
            .orchestrator()
            .host
            .lock_or_recover("the orchestrator host");
        if current.as_ref().is_some_and(|(run, _)| *run == self.run) {
            *current = None;
        }
    }
}

#[derive(Default)]
pub(super) struct Watchdog {
    progress: u64,
    since: Option<Instant>,
    fired: bool,
    restart_pending: bool,
    /// The checks a test hands the watchdog in place of its interval, each at the time it says.
    checks: Option<tokio::sync::mpsc::UnboundedReceiver<MonitorCheck>>,
}

struct MonitorCheck {
    now: Instant,
    /// Told once the watchdog acted on this check.
    observed: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Watchdog {
    async fn next_check(&mut self, interval: &mut clock::Interval) -> MonitorCheck {
        if let Some(checks) = self.checks.as_mut() {
            return checks
                .recv()
                .await
                .expect("the watchdog test owns its monitor clock");
        }
        interval.tick().await;
        MonitorCheck {
            now: clock::instant(),
            observed: None,
        }
    }

    pub(super) fn observe(
        &mut self,
        now: Instant,
        pending: bool,
        suspended: bool,
        progress: u64,
    ) -> bool {
        if !pending || suspended || self.progress != progress {
            self.since = None;
            self.fired = false;
        }
        self.progress = progress;
        if !pending || suspended {
            return false;
        }
        let since = self.since.get_or_insert(now);
        if !self.fired && now.saturating_duration_since(*since) >= STALLED_AFTER {
            self.fired = true;
            return true;
        }
        false
    }
}

impl Executor {
    pub(super) fn interrupt_host(&self) -> Result<()> {
        let current = self
            .domain
            .orchestrator()
            .host
            .lock_or_recover("the orchestrator host")
            .clone();
        if let Some((_, stop)) = current {
            stop.kill()?;
        }
        Ok(())
    }
    fn waiting_requests(&self) -> Vec<RequestId> {
        let read = self.domain.read();
        let waiting: std::collections::BTreeSet<_> = read
            .state()
            .interactions
            .values()
            .filter_map(|card| {
                if card.withdrawn || card.recipient != Recipient::User {
                    return None;
                }
                let Scope::Orchestrator {
                    request_id: Some(request),
                } = &card.scope
                else {
                    return None;
                };
                matches!(
                    &card.payload,
                    InteractionPayload::Question { answer: None, .. }
                        | InteractionPayload::ActionRequest { answer: None, .. }
                )
                .then_some(request)
            })
            .collect();
        read.state()
            .requests
            .values()
            .filter(|request| {
                !request.closed
                    && read.index().tasks_of_request(&request.id).is_empty()
                    && !waiting.contains(&request.id)
            })
            .map(|request| request.id.clone())
            .collect()
    }

    fn watchdog_due(&self, watchdog: &mut Watchdog, now: Instant) -> bool {
        let requests = self.waiting_requests();
        let suspended = self.domain.engine().usage_pause().is_some()
            || self.domain.engine().account_wait().is_waiting()
            || load(&self.home).is_ok_and(|runtime| {
                runtime.active_run.is_some()
                    && runtime.last_error.is_some()
                    && !matches!(
                        runtime.failure,
                        Some(Failure::Launch | Failure::SessionMissing)
                    )
                    || runtime.active_run.is_none() && runtime.retry_at.is_some_and(|at| at > now_millis())
            });
        if !watchdog.observe(
            now,
            !requests.is_empty(),
            suspended,
            self.domain
                .orchestrator_progress(),
        ) {
            return false;
        }
        log!(
            "orchestrator watchdog: no progress for 5 minutes with {} pending requests ({}); restarting the executor",
            requests.len(),
            requests
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
        true
    }

    /// The worker owns its tick until both the provider and its admitted writers have stopped.
    /// Cancelling or dropping a tick would detach the provider's blocking worker.
    pub(super) async fn supervised_tick(self: &Arc<Self>, watchdog: &mut Watchdog) -> Result<()> {
        self.supervised_tick_at(watchdog, clock::instant()).await
    }

    async fn supervised_tick_at(
        self: &Arc<Self>,
        watchdog: &mut Watchdog,
        now: Instant,
    ) -> Result<()> {
        let (restart, stop) = tokio::sync::watch::channel(watchdog.restart_pending);
        if watchdog.restart_pending || self.watchdog_due(watchdog, now) {
            watchdog.restart_pending = true;
            restart.send_replace(true);
            if let Err(error) = self.interrupt_host() {
                log!("orchestrator watchdog could not interrupt the host: {error:#}");
            }
        }
        let executor = Arc::clone(self);
        let mut worker = tokio::spawn(async move { executor.tick_with_stop(stop).await });
        let mut check = clock::interval(Duration::from_secs(10));
        let result = loop {
            tokio::select! {
                result = &mut worker => break result.context("orchestrator worker stopped")?,
                checked = watchdog.next_check(&mut check) => {
                    if self.watchdog_due(watchdog, checked.now) {
                        watchdog.restart_pending = true;
                        restart.send_replace(true);
                    }
                    let restarting = *restart.borrow();
                    if restarting && let Err(error) = self.interrupt_host() {
                        log!("orchestrator watchdog could not interrupt the host: {error:#}");
                    }
                    if let Some(observed) = checked.observed { let _ = observed.send(()); }
                }
            }
        };
        let restarted = *restart.borrow();
        if restarted {
            let mut runtime = self.startup_recovery().await?;
            if runtime.active_run.is_none() {
                runtime.retry_at = None;
                self.save(&runtime)?;
            }
            *watchdog = Watchdog::default();
        }
        result
    }
}

#[cfg(test)]
#[cfg(not(feature = "real-hosts"))]
#[path = "tests/watchdog.rs"]
mod tests;
