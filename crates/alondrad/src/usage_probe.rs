//! What each provider account has used, measured this way: the nodes' SDK hosts report every
//! `rate_limit_event` of their account as it comes, and a probe without a prompt reads the rest.
//! The probe runs the SDK host with the account's configuration folder; Claude starts, says which
//! account it signed in with and answers the usage its `/usage` command shows, and no turn runs.
//! It runs when the daemon starts, every 15 minutes for an account nothing else measured meanwhile,
//! and right after Claude refused work for running out. A probe that fails keeps the last good
//! reading, which says how old it is.
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alondra_agent_runtime::owned_host::VerifiedArtifact;
use alondra_core::accounts::{AccountBilling, AccountId, AccountState, AccountStatus, Provider};
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
#[derive(Default)]
pub(crate) struct Measured(Mutex<Vec<AccountUsage>>);

impl Measured {
    pub(crate) fn sync(&self, state: &DomainState) {
        let mut kept = self.0.lock().expect("measured usage poisoned");
        if *kept != state.account_usage {
            kept.clone_from(&state.account_usage);
        }
    }

    /// When a limit of the account nodes run on, reported without a time, ends.
    pub(crate) fn reset_after_limit(&self, now: Millis) -> Option<Millis> {
        self.0.lock().expect("measured usage poisoned").iter().find(|usage| nodes_account(usage)).and_then(|usage| usage.reset_after_limit(now))
    }
}

/// Nodes run on the provider's own sign-in until each launch picks its account.
fn nodes_account(usage: &AccountUsage) -> bool {
    usage.provider == Provider::Claude && usage.account.is_default()
}

/// The accounts' usage together at `now`, as the gauges and the pace card read it.
pub(crate) fn usage(domain: &DomainRuntime, now: Millis) -> Option<alondra_core::usage::Usage> {
    domain.read().index().usage(now, &crate::node_reports::local_day(now))
}

/// Records what `account` said of its usage, unless the domain already holds it.
pub(crate) fn record(domain: &DomainRuntime, account: &AccountId, reading: UsageReading, now: Millis) {
    let day = crate::node_reports::local_day(now);
    for _ in 0..5 {
        let read = domain.read();
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: read.state().data_epoch.clone(),
            command_id: format!("account-usage-{}", crate::auth::random_hex(16)).into(),
            expected_revision: read.state().revision,
            command: DomainCommand::RecordAccountUsage { provider: Provider::Claude, account: account.clone(), reading: reading.clone(), at: now, day: day.clone() },
        };
        match domain.execute(&Actor::System, &envelope, None, || Ok(())) {
            Ok(_) => return,
            Err(error) => log!("could not record the usage of account {account}: {error}"),
        }
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
        .arg("--usage-probe")
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
    let report: ProbeReport = serde_json::from_str(out.trim()).map_err(|error| format!("the probe's answer: {error}"))?;
    reading(report)
}

/// Brings the live view's copy and the Mac-wide pause in step with what the domain committed: the
/// session or the week of the account nodes run on running out pauses the Mac until it resets.
pub(crate) fn follow(domain: &DomainRuntime, now: Millis) {
    let read = domain.read();
    domain.live.measured.sync(read.state());
    if let Some(until) = read.state().account_usage.iter().find(|usage| nodes_account(usage)).and_then(|usage| usage.plan_exhausted_until(now)) {
        domain.live.usage_pause.exhaust(Some(until), now);
    }
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

/// Keeps every account measured, and the live view's copy and the Mac-wide pause in step with
/// what the account nodes run on says.
pub(crate) async fn run(domain: Arc<DomainRuntime>, home: std::path::PathBuf) {
    let mut changes = domain.subscribe();
    let mut pauses = domain.live.usage_pause.subscribe();
    let mut probed: HashMap<AccountId, Millis> = HashMap::new();
    loop {
        let now = now_millis();
        follow(&domain, now);
        let read = domain.read();
        let state = read.state();
        let accounts: Vec<AccountId> = probed_accounts(state).into_iter().map(|account| account.id.clone()).collect();
        let (mut ready, next) = due(&accounts, &state.account_usage, &probed, now);
        drop(read);
        // Claude refused work Mac-wide (a hook, the orchestrator): measure the account nodes run on.
        let nodes = AccountId::default_account();
        let refused = domain.live.usage_pause.current().filter(|standing| !standing.by_user).map(|standing| standing.since);
        if refused.is_some_and(|since| probed.get(&nodes).is_none_or(|probe| *probe < since)) && accounts.contains(&nodes) && !ready.contains(&nodes) {
            ready.push(nodes);
        }
        for account in ready {
            probed.insert(account.clone(), now_millis());
            let Some((backend, _)) = domain.sdk() else { break };
            let (backend, folder) = (backend.clone(), alondra_core::accounts::folder(&home, &account));
            if folder.as_ref().is_some_and(|folder| !folder.is_dir()) {
                continue;
            }
            match tokio::task::spawn_blocking(move || probe(&backend, folder.as_deref())).await {
                Ok(Ok(reading)) => record(&domain, &account, reading, now_millis()),
                Ok(Err(why)) => log!("could not measure the usage of account {account}: {why}"),
                Err(_) => log!("the usage probe of account {account} stopped"),
            }
        }
        let wait = next.map_or(Duration::from_millis(EVERY as u64), |at| Duration::from_millis((at - now_millis()).max(1_000) as u64));
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
