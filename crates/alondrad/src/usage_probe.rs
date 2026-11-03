//! What each provider account has used: the nodes' SDK hosts report every
//! `rate_limit_event` of their account as it comes, and a probe without a prompt reads the rest.
//! The probe runs the SDK host with the account's configuration folder; Claude starts, says which
//! account it signed in with and answers the usage its `/usage` command shows, and no turn runs.
//! It runs when the daemon starts, every 15 minutes for an account nothing else measured meanwhile,
//! and right after Claude refused work for running out. A probe that fails keeps the last good
//! reading, which says how old it is. A stale reading retries at once, then backs off up to 30 seconds.
use crate::locks::Poisonable as _;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alondra_agent_runtime::owned_host::VerifiedArtifact;
use alondra_core::accounts::{AccountBilling, AccountId, AccountState, AccountStatus, Policy, Provider};
use alondra_core::domain::{Actor, Command as DomainCommand, CommandEnvelope, DomainState, PROTOCOL_VERSION};
use alondra_core::usage::{AccountUsage, Share, UsageReading, WindowKind, WindowReading};
use alondra_core::{Millis, now_millis};
use serde::Deserialize;

use crate::domain_runtime::DomainRuntime;
use crate::sdk_owner::SdkBackend;

pub(crate) const EVERY: Millis = 15 * 60_000;
const TIMEOUT: Duration = Duration::from_secs(60);
/// The credential of the bindings that run on the provider's own sign-in on this Mac.
const NATIVE_LOGIN: &str = "native-login";

/// The account a provider binding's executions run on: its credential names it.
pub(crate) fn account_of(credential: &str) -> AccountId {
    match credential {
        NATIVE_LOGIN => AccountId::default_account(),
        other => AccountId::parse(other).unwrap_or_else(|_| AccountId::default_account()),
    }
}

/// The accounts' readings as the domain last committed them, kept where the live view, which has
/// no domain, reads when a limit Claude reported without a time ends.
/// A holder that panics leaves it whole, so it is recovered: a copy of the domain's usage, taken
/// again on every commit.
#[derive(Default)]
pub(crate) struct Measured(Mutex<Vec<AccountUsage>>);

impl Measured {
    pub(crate) fn sync(&self, state: &DomainState) {
        let mut kept = self.0.lock_or_recover("the measured usage");
        if *kept != state.account_usage {
            kept.clone_from(&state.account_usage);
        }
    }

    /// When a limit of `account`, reported without a time, ends.
    pub(crate) fn reset_after_limit(&self, account: &AccountId, now: Millis) -> Option<Millis> {
        self.0
            .lock_or_recover("the measured usage")
            .iter()
            .find(|usage| usage.provider == Provider::Claude && usage.account == *account)
            .and_then(|usage| usage.reset_after_limit(now))
    }
}

/// The accounts' usage together at `now`, as the gauges and the pace card read it.
pub(crate) fn usage(domain: &DomainRuntime, now: Millis) -> Option<alondra_core::usage::Usage> {
    domain.read().index().usage(now, &crate::node_reports::local_day(now))
}

/// Records what `account` said of its usage, unless the domain already holds it.
pub(crate) fn record(domain: &DomainRuntime, account: &AccountId, reading: UsageReading, now: Millis) {
    record_for_provider(domain, &alondra_core::accounts::AccountKey::new(Provider::Claude, account.clone()), reading, now);
}

pub(crate) fn record_for_provider(domain: &DomainRuntime, account: &alondra_core::accounts::AccountKey, reading: UsageReading, now: Millis) {
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: domain.read().state().data_epoch.clone(),
        command_id: format!("account-usage-{}", crate::auth::random_hex(16)).into(),
        expected_revision: 0,
        command: DomainCommand::RecordAccountUsage { provider: account.provider, account: account.account.clone(), reading, at: now, day: crate::node_reports::local_day(now) },
    };
    if let Err(error) = domain.execute_as_system(envelope) {
        log!("could not record the usage of account {}: {error}", account.wire_id());
    }
}

/// What the host's probe answers.
#[derive(Deserialize, Debug, PartialEq)]
pub(crate) struct ProbeReport {
    #[serde(default)]
    plan: Option<String>,
    rate_limits_available: bool,
    #[serde(default)]
    windows: Vec<ProbeWindow>,
}

#[derive(Deserialize, Debug, PartialEq)]
struct ProbeWindow {
    kind: String,
    used: f64,
    #[serde(default)]
    resets_at: Option<f64>,
}

/// The probe's answer as a complete reading: Claude's 5-hour window is the session's and its
/// 7-day one the week's. An account without plan limits (an API key) has nothing to read.
pub(crate) fn reading(report: ProbeReport) -> Result<UsageReading, String> {
    if !report.rate_limits_available {
        return Err("Claude reports no plan limits for this account".into());
    }
    let windows = report
        .windows
        .into_iter()
        .filter(|window| window.used.is_finite())
        .map(|window| WindowReading {
            kind: match window.kind.as_str() {
                "five_hour" => WindowKind::Session,
                "seven_day" => WindowKind::Weekly,
                other => WindowKind::Other(other.to_owned()),
            },
            used: Share::of(window.used),
            resets_at: window.resets_at.filter(|at| at.is_finite() && *at > 0.0).map(|at| at as Millis),
            exhausted: false,
        })
        .collect();
    let reading = UsageReading { windows, plan: report.plan.map(|plan| plan.trim().to_owned()).filter(|plan| !plan.is_empty()), complete: true };
    reading.check()?;
    Ok(reading)
}

/// Runs the host's probe with `folder` as Claude's configuration, none for the default profile.
fn probe(backend: &SdkBackend, folder: Option<&Path>) -> Result<UsageReading, String> {
    let report = serde_json::from_value(run_probe(backend, folder, "--usage-probe")?).map_err(|error| format!("the probe's answer: {error}"))?;
    reading(report)
}

pub(crate) fn models(backend: &SdkBackend, home: &Path, key: &alondra_core::accounts::AccountKey) -> Result<alondra_core::model_selection::AccountModelCatalog, String> {
    use sha2::Digest;
    if key.provider != Provider::Claude { return Err("The SDK catalog belongs to Claude".into()); }
    let backend = backend.for_provider(key.provider, home).and_then(|backend| backend.for_account_provider(home, key.provider, &key.account, &crate::accounts::registered_provider(home, key.provider)))
        .map_err(|why| format!("{why:#}"))?;
    let report = run_probe(&backend, None, "--model-probe")?;
    let models: Vec<alondra_core::model_selection::CatalogModel> = serde_json::from_value(report["models"].clone()).map_err(|why| format!("The SDK model catalog is invalid: {why}"))?;
    let native_default = report["native_default"].as_str().map(str::to_owned);
    let bytes = serde_json::to_vec(&(&models, &native_default)).map_err(|why| why.to_string())?;
    let revision = sha2::Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(alondra_core::model_selection::AccountModelCatalog { account: key.clone(), revision, models, native_default, measured_at: alondra_core::now_millis() })
}

fn run_probe(backend: &SdkBackend, folder: Option<&Path>, flag: &str) -> Result<serde_json::Value, String> {
    let started = backend.for_start().map_err(|error| format!("{error:#}"))?;
    let artifact = VerifiedArtifact::verify(&started.node, &started.host, &started.node_sha256, &started.host_sha256)
        .map_err(|error| format!("verifying the SDK host: {error}"))?;
    std::fs::create_dir_all(&started.state_root).map_err(|error| format!("creating the SDK state root: {error}"))?;
    let mut environment = started.native_environment.clone();
    if let Some(folder) = folder {
        environment.insert("CLAUDE_CONFIG_DIR".into(), folder.display().to_string());
    }
    let request = serde_json::json!({
        "executable": started.native, "cwd": started.state_root, "env": environment, "timeout_ms": TIMEOUT.as_millis() as u64,
    });
    let mut command = match &started.confinement {
        Some(profile) => {
            let mut command = Command::new("/usr/bin/sandbox-exec");
            command.arg("-f").arg(profile).arg(artifact.executable());
            command
        }
        None => Command::new(artifact.executable()),
    };
    use std::os::unix::process::CommandExt;
    let mut child = command
        .arg(artifact.artifact())
        .arg(flag)
        .current_dir(&started.state_root)
        .env_clear()
        .envs(&started.host_environment)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| format!("starting the SDK host: {error}"))?;
    let written = child.stdin.take().expect("piped stdin").write_all(request.to_string().as_bytes());
    let (mut stdout, mut stderr) = (child.stdout.take().expect("piped stdout"), child.stderr.take().expect("piped stderr"));
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut out, mut err) = (String::new(), String::new());
        let _ = stdout.read_to_string(&mut out);
        let _ = stderr.read_to_string(&mut err);
        let _ = sender.send((out, err));
    });
    let Ok((out, err)) = receiver.recv_timeout(TIMEOUT + Duration::from_secs(5)) else {
        // The probe leads its own process group: it and the Claude it started go together.
        unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
        let _ = child.wait();
        return Err(format!("the probe did not answer within {} s", TIMEOUT.as_secs()));
    };
    let status = child.wait().map_err(|error| format!("waiting for the probe: {error}"))?;
    written.map_err(|error| format!("giving the probe its request: {error}"))?;
    if !status.success() {
        return Err(format!("the probe failed: {}", err.trim().chars().take(300).collect::<String>()));
    }
    serde_json::from_str(out.trim()).map_err(|error| format!("the probe's answer: {error}"))
}

/// Brings the live view's copy and the Mac-wide pause in step with what the domain committed: when
/// no account can work, the Mac waits until the soonest one can, or until one has a session when
/// none has; when one can again before that, the wait ends.
fn available_accounts(state: &DomainState, now: Millis) -> Vec<alondra_core::accounts::AccountKey> {
    let mut eligible = alondra_core::usage::eligible_account_keys(&state.account_order, &state.accounts, &state.account_usage, now);
    if state.account_policy == Policy::Margin && !state.accounts.is_empty() && state.accounts.iter().all(|account| account.provider == Provider::Claude) {
        let day = crate::node_reports::local_day(now);
        let chosen = alondra_core::usage::choose_account(Policy::Margin, Provider::Claude, &state.accounts, &state.account_usage, state.account_reserve, now, &day);
        eligible.retain(|key| chosen.as_ref() == Some(&key.account));
    }
    // Paid accounts are considered only for readiness. Launch selection still requires the user's explicit grant.
    for account in &state.accounts {
        if account.state == AccountState::SignedIn && account.billing == AccountBilling::PayPerUse {
            let capacity = AccountStatus { billing: AccountBilling::default(), ..account.clone() };
            eligible.extend(alondra_core::usage::eligible_account_keys(&[], &[capacity], &state.account_usage, now));
        }
    }
    eligible
}

pub(crate) fn follow(domain: &DomainRuntime, now: Millis) {
    let read = domain.read();
    let state = read.state();
    domain.live.measured.sync(state);
    let ready = available_accounts(state, now);
    let other_capacity = ready.iter().any(|key| key.provider == Provider::Codex || state.accounts.iter().any(|status|
        status.provider == key.provider && status.id == key.account && status.billing == AccountBilling::PayPerUse));
    drop(read);
    if other_capacity {
        if domain.live.usage_pause.provider_account_back(now) { domain.live.gate.poke(); }
    } else if !ready.is_empty() {
        crate::usage_pause::session_is_back(domain);
        domain.live.usage_pause.margin_back(now);
    } else {
        hold(domain, now);
    }
}

/// Availability is checked across providers. A missing account or unknown reset cannot imply a timed quota limit.
pub(crate) fn hold(domain: &DomainRuntime, now: Millis) {
    let read = domain.read();
    let state = read.state();
    if !available_accounts(state, now).is_empty() { return; }
    let accounts: Vec<_> = state.accounts.iter().filter(|status| status.state == AccountState::SignedIn && status.billing != AccountBilling::PayPerUse).cloned().collect();
    let until = accounts.iter().map(|status| status.provider).collect::<std::collections::BTreeSet<_>>().into_iter()
        .filter_map(|provider| alondra_core::usage::margin_returns_at(provider, &accounts, &state.account_usage, now)).min();
    drop(read);
    if until.is_none() {
        domain.live.usage_pause.wait_for_session(now);
    } else {
        domain.live.usage_pause.exhaust_for_margin(until, now);
    }
}

/// Records that Claude refused `account` work for its usage, until `until` when Claude said: its
/// readings say it ran out, so no new work starts on it, and the next probe measures the rest.
pub(crate) fn record_limit(domain: &DomainRuntime, account: &AccountId, until: Option<Millis>, now: Millis) {
    let until = until.or_else(|| domain.live.measured.reset_after_limit(account, now)).filter(|until| *until > now);
    let window = WindowReading { kind: WindowKind::Session, used: Share::FULL, resets_at: until, exhausted: true };
    record(domain, account, UsageReading { windows: vec![window], plan: None, complete: false }, now);
    follow(domain, now);
}

/// Accounts whose usage limits apply: on, and paid by subscription.
fn probed_accounts(state: &DomainState) -> Vec<&AccountStatus> {
    state
        .accounts
        .iter()
        .filter(|account| account.provider == Provider::Claude && account.state != AccountState::Disabled)
        .filter(|account| !matches!(account.billing, AccountBilling::PayPerUse))
        .collect()
}

/// Which accounts to probe at `now` and when the next one is due. An account is due 15 minutes
/// after it was last measured or probed, and at once when it ran out after its last probe.
pub(crate) fn due(accounts: &[AccountId], usage: &[AccountUsage], probed: &HashMap<AccountId, Millis>, now: Millis) -> (Vec<AccountId>, Option<Millis>) {
    let mut ready = vec![];
    let mut next: Option<Millis> = None;
    for account in accounts {
        let measured = usage.iter().find(|u| u.provider == Provider::Claude && u.account == *account);
        let last_probe = probed.get(account).copied();
        let refused_since = measured.is_some_and(|u| {
            u.windows.iter().any(|window| window.reading.exhausted && last_probe.is_none_or(|at| window.measured_at > at))
        });
        let last = measured.and_then(AccountUsage::measured_at).into_iter().chain(last_probe).max();
        let at = if refused_since { now } else { last.map_or(now, |last| last + EVERY) };
        if at <= now {
            ready.push(account.clone());
        } else {
            next = Some(next.map_or(at, |next| next.min(at)));
        }
    }
    (ready, next)
}

#[derive(Default)]
struct ProbeSchedule {
    probed: HashMap<AccountId, Millis>,
    retries: HashMap<AccountId, (crate::native_probe::Backoff, Millis)>,
}

impl ProbeSchedule {
    fn due(&self, accounts: &[AccountId], usage: &[AccountUsage], now: Millis) -> (Vec<AccountId>, Option<Millis>) {
        let ordinary: Vec<_> = accounts.iter().filter(|account| !self.retries.contains_key(*account)).cloned().collect();
        let (mut ready, mut next) = due(&ordinary, usage, &self.probed, now);
        for account in accounts {
            if let Some((_, at)) = self.retries.get(account) {
                if *at <= now { ready.push(account.clone()); }
                else { next = Some(next.map_or(*at, |next| next.min(*at))); }
            }
        }
        (ready, next)
    }

    fn finished(&mut self, account: &AccountId, outcome: crate::native_probe::Outcome, now: Millis) {
        match outcome {
            crate::native_probe::Outcome::Stale => {
                let (backoff, at) = self.retries.entry(account.clone()).or_default();
                *at = now + backoff.next_delay().as_millis() as Millis;
            }
            crate::native_probe::Outcome::Finished => { self.retries.remove(account); }
        }
    }
}

async fn measure_account(domain: &DomainRuntime, home: &Path, account: &AccountId) -> crate::native_probe::Outcome {
    if !probed_accounts(domain.read().state()).iter().any(|status| status.id == *account) {
        return crate::native_probe::Outcome::Finished;
    }
    let Some((backend, _)) = domain.sdk() else { return crate::native_probe::Outcome::Finished };
    let (backend, folder) = (backend.clone(), alondra_core::accounts::folder(home, account));
    if folder.as_ref().is_some_and(|folder| !folder.is_dir()) {
        return crate::native_probe::Outcome::Finished;
    }
    let (fence, file) = match crate::accounts::probe_fence(domain, home) {
        Ok(fence) => fence,
        Err(why) => { log!("Could not prepare the account usage probe: {why}"); return crate::native_probe::Outcome::Finished; }
    };
    if !file.accounts.accounts(Provider::Claude).iter().any(|configured| configured.id == *account && configured.enabled && !matches!(configured.billing, AccountBilling::PayPerUse)) {
        return crate::native_probe::Outcome::Finished;
    }
    let measured_at = now_millis();
    match tokio::task::spawn_blocking(move || probe(&backend, folder.as_deref())).await {
        Ok(Ok(reading)) => {
            let command = DomainCommand::RecordAccountUsage { provider: Provider::Claude, account: account.clone(), reading, at: measured_at, day: crate::node_reports::local_day(measured_at) };
            match crate::accounts::publish_current_probe(domain, home, fence, vec![command]) {
                Ok(()) => crate::accounts::answered(domain),
                Err(why) => {
                    log!("Discarded stale native usage metadata: {why}");
                    return crate::native_probe::Outcome::of(&why);
                },
            }
        }
        Ok(Err(why)) => log!("could not measure the usage of account {account}: {why}"),
        Err(_) => log!("the usage probe of account {account} stopped"),
    }
    crate::native_probe::Outcome::Finished
}

/// Keeps every account measured, and the live view's copy and the Mac-wide pause in step with
/// what the accounts say.
pub(crate) async fn run(domain: Arc<DomainRuntime>, home: std::path::PathBuf) {
    let mut changes = domain.subscribe();
    let mut pauses = domain.live.usage_pause.subscribe();
    let mut schedule = ProbeSchedule::default();
    loop {
        let now = now_millis();
        follow(&domain, now);
        let read = domain.read();
        let state = read.state();
        let accounts: Vec<AccountId> = probed_accounts(state).into_iter().map(|account| account.id.clone()).collect();
        let (mut ready, _) = schedule.due(&accounts, &state.account_usage, now);
        drop(read);
        // A `claude --bg` node's hook paused the Mac: measure the default account, the one they run on.
        let nodes = AccountId::default_account();
        let refused = domain.live.usage_pause.current().filter(|standing| !standing.by_user).map(|standing| standing.since);
        if refused.is_some_and(|since| schedule.probed.get(&nodes).is_none_or(|probe| *probe < since)) && accounts.contains(&nodes) && !schedule.retries.contains_key(&nodes) && !ready.contains(&nodes) {
            ready.push(nodes);
        }
        for account in ready {
            schedule.probed.insert(account.clone(), now_millis());
            let outcome = measure_account(&domain, &home, &account).await;
            schedule.finished(&account, outcome, now_millis());
        }
        let now = now_millis();
        let read = domain.read();
        let accounts: Vec<_> = probed_accounts(read.state()).into_iter().map(|account| account.id.clone()).collect();
        schedule.retries.retain(|account, _| accounts.contains(account));
        let (ready, next) = schedule.due(&accounts, &read.state().account_usage, now);
        drop(read);
        let wait = if !ready.is_empty() { Duration::ZERO }
            else { next.map_or(Duration::from_millis(EVERY as u64), |at| Duration::from_millis((at - now).max(0) as u64)) };
        tokio::select! {
            changed = changes.changed() => if changed.is_err() { return },
            changed = pauses.changed() => if changed.is_err() { return },
            () = tokio::time::sleep(wait) => {}
        }
    }
}

#[cfg(test)]
#[path = "usage_probe_tests.rs"]
mod tests;
