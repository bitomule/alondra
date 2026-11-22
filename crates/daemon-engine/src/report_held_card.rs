//! The card for a crash report held on this Mac because `report.repo` changed since it was first
//! sent: one per report, opened while `sending.json` says it is held and withdrawn once it is not
//! (sent another way, kept, or going on by itself because the repository is the saved one again).
//! "Send it" runs `alondra report send <id>`, the user's confirmation of the new repository;
//! "Keep it" runs `alondra report keep <id>`. A send GitHub refuses leaves the report held, and the
//! card comes back saying why.
use alondra_core::clock;
use alondra_daemon_support::reports::{self, HeldReport};
use alondra_domain_rules::report_held_card::{Held, KEEP_OPTION, REPORT_HELD_PREFIX, ReportHeldCard, SEND_OPTION};
use alondra_domain_rules::{InteractionId, InteractionPayload, SystemCard};
use alondra_engine_ports::agent_host::{AgentHost, ProbeRun};
use alondra_view_model::Lang;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use alondra_engine_ports::Runtime;

const TICK: Duration = Duration::from_secs(5);
const ONE_RUN: Duration = Duration::from_secs(15 * 60);

/// Watches the reports of `home` and runs `cli`, through the hosts' edge, when the user answers.
pub async fn run(domain: Arc<dyn Runtime>, home: PathBuf, cli: Option<PathBuf>, fast: bool) {
    let (Some(cli), Some(hosts)) = (cli, domain.ports().port::<dyn AgentHost>()) else { return };
    let cli_home = home.clone();
    let run_cli = move |args: &[&str]| {
        let mut command = std::process::Command::new(&cli);
        command.args(args).env("ALONDRA_HOME", &cli_home).stdin(std::process::Stdio::null());
        if let Err(error) = hosts.run(ProbeRun { command, input: Vec::new(), timeout: Some(ONE_RUN) }) {
            log!("could not run alondra {}: {error}", args.join(" "));
        }
    };
    let repo_home = home.clone();
    watch(domain, home.join("reports"), move || configured_repo(&repo_home), run_cli, if fast { Duration::from_secs(1) } else { TICK }).await
}

/// The report repository this Mac's settings name, when they name one.
fn configured_repo(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(alondra_core::config::path_in(home)).unwrap_or_default();
    let file = alondra_core::config::FileConfig::parse(&text).ok()?;
    alondra_core::config::Config::resolve(home.to_owned(), &file, |key| std::env::var(key).ok(), || None).report_repo
}

struct Ours {
    id: InteractionId,
    card: Option<ReportHeldCard>,
    answer: Option<String>,
}

fn ours(domain: &dyn Runtime) -> Vec<Ours> {
    let state = domain.state();
    state.interactions.values()
        .filter(|card| matches!(card.system_card, Some(SystemCard::ReportHeld { .. })) && !card.withdrawn)
        .filter_map(|card| match &card.payload {
            InteractionPayload::Question { answer, .. } => Some(Ours {
                id: card.id.clone(),
                card: ReportHeldCard::from_body(&card.body),
                answer: answer.as_ref().map(|answer| answer.option_id.clone()),
            }),
            _ => None,
        })
        .collect()
}

fn wanted(held: &HeldReport) -> Option<ReportHeldCard> {
    match &held.held {
        Held::RepoChanged { current, refused } => Some(ReportHeldCard { report: held.id.clone(), saved: held.saved.clone(), current: current.clone(), refused: refused.clone() }),
        Held::Unknown => None,
    }
}

fn raise(domain: &dyn Runtime, card: &ReportHeldCard) {
    let id = format!("{REPORT_HELD_PREFIX}{}-{}", card.report, alondra_core::now_millis());
    let (need, why) = card.text(Lang::En);
    let system = SystemCard::ReportHeld { report: card.report.clone() };
    if let Err(error) = domain.raise_worded_problem(&id, Some(system), &card.cause(), need, why, card.to_body(), card.options(), None) {
        log!("could not put the held report card in front of the user: {error:?}");
    }
}

/// What the watcher remembers between checks: the answers it already acted on, and the reports it
/// already asked to try again for a repository the settings name.
#[derive(Default)]
struct Memory {
    handled: BTreeSet<InteractionId>,
    nudged: BTreeSet<(String, String)>,
}

/// One check: act on new answers, try again a held report whose repository the settings changed,
/// then leave exactly one open card per held report, saying what `sending.json` says now.
fn check(domain: &dyn Runtime, reports: &Path, repo: Option<String>, cli: &dyn Fn(&[&str]), memory: &mut Memory) {
    let held = reports::held(reports);
    for card in &ours(domain) {
        let (Some(answer), Some(asked)) = (&card.answer, &card.card) else { continue };
        if !memory.handled.insert(card.id.clone()) {
            continue;
        }
        let still = held.iter().filter_map(wanted).any(|now| now.report == asked.report && now.current == asked.current);
        let same_repo = repo.as_ref().is_none_or(|repo| *repo == asked.current);
        match answer.as_str() {
            SEND_OPTION if still && same_repo => cli(&["report", "send", &asked.report]),
            KEEP_OPTION if still => cli(&["report", "keep", &asked.report]),
            _ => {}
        }
    }
    let mut held = reports::held(reports);
    if let Some(repo) = &repo {
        let mut nudged = false;
        for report in &held {
            if let Held::RepoChanged { current, .. } = &report.held
                && current != repo
                && memory.nudged.insert((report.id.clone(), repo.clone()))
            {
                cli(&["report", "send", &report.id, "--retry"]);
                nudged = true;
            }
        }
        if nudged {
            held = reports::held(reports);
        }
    }
    let wanted: Vec<ReportHeldCard> = held.iter().filter_map(wanted).collect();
    let mut shown = Vec::new();
    for card in ours(domain).iter().filter(|card| card.answer.is_none()) {
        match &card.card {
            Some(body) if wanted.contains(body) && !shown.contains(body) => shown.push(body.clone()),
            _ => {
                let _ = domain.withdraw_problem(&card.id);
            }
        }
    }
    for card in wanted.iter().filter(|card| !shown.contains(card)) {
        raise(domain, card);
    }
}

async fn watch(
    domain: Arc<dyn Runtime>,
    reports: PathBuf,
    repo: impl Fn() -> Option<String> + Send + Sync + 'static,
    cli: impl Fn(&[&str]) + Send + Sync + 'static,
    tick: Duration,
) {
    let (repo, cli) = (Arc::new(repo), Arc::new(cli));
    let starting = domain.clone();
    let mut memory = tokio::task::spawn_blocking(move || Memory {
        handled: ours(&*starting).into_iter().filter(|card| card.answer.is_some()).map(|card| card.id).collect(),
        nudged: BTreeSet::new(),
    }).await.expect("held report card worker failed");
    let mut ticks = clock::interval(tick);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticks.tick().await;
        let (domain, reports, repo, cli) = (domain.clone(), reports.clone(), repo.clone(), cli.clone());
        memory = clock::spawn_blocking(move || {
            check(&*domain, &reports, repo(), &*cli, &mut memory);
            memory
        }).await.expect("held report card worker failed");
    }
}

#[cfg(test)]
#[path = "report_held_card_tests.rs"]
mod tests;
