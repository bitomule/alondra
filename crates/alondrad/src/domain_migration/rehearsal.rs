//! Full cutover rehearsal against a read-only snapshot of a live HOME.
//!
//! The live HOME is only read: SQLite through read-only connections and the
//! backup API (a WAL database is copied with its committed WAL frames), other
//! files through plain reads. No lock is taken on it and nothing is written to
//! it. Prepare, commit and restore run through this binary's own CLI against
//! the snapshot. The v2-only daemon then runs on the snapshot with its relay
//! and APNs configuration removed, wake off, a loopback update URL, a private
//! HOME and TMPDIR, and a PATH whose `claude`, `gh`, `git` and friends only
//! record that they were called.
use super::*;
use crate::domain_runtime::DomainRuntime;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::process::{Child, Command, Stdio};

/// Snapshot order: a schedule run names a Request, so schedules.db is read
/// before alondra.db; stored files are append-only and are copied after it.
pub(crate) const SNAPSHOT_DATABASES: [&str; 5] = [
    "schedules.db",
    "alondra.db",
    "service-receipts.sqlite",
    "service-upload-attempts.sqlite",
    "domain-v2.db",
];
/// Files that would reach relay, APNs or a paired device from the rehearsal daemon.
const OUTWARD_FILES: [&str; 3] = ["service.toml", "service.toml.staged", "apns.json"];
const STUBBED_COMMANDS: [&str; 11] = [
    "claude", "gh", "git", "tmux", "brew", "launchctl", "osascript", "open", "curl",
    "terminal-notifier", "ssh",
];
const SLOW: Duration = Duration::from_secs(1);

#[derive(Debug, Serialize)]
pub(crate) struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Report {
    pub live_home: String,
    pub work: String,
    pub numbers: BTreeMap<String, Value>,
    pub checks: Vec<Check>,
    pub timings_ms: BTreeMap<String, u128>,
}

impl Report {
    fn check(&mut self, name: impl Into<String>, ok: bool, detail: impl Into<String>) -> bool {
        self.checks.push(Check { name: name.into(), ok, detail: detail.into() });
        ok
    }
    fn equal<T: PartialEq + std::fmt::Debug>(&mut self, name: &str, legacy: T, v2: T) -> bool {
        let ok = legacy == v2;
        self.check(name, ok, format!("legacy {legacy:?}, v2 {v2:?}"))
    }
    fn fail(&mut self, name: &str, error: &anyhow::Error) {
        self.check(name, false, format!("{error:#}"));
    }
    fn time(&mut self, name: &str, elapsed: Duration) {
        self.timings_ms.insert(name.into(), elapsed.as_millis());
    }
    pub fn passed(&self) -> bool {
        !self.checks.is_empty() && self.checks.iter().all(|check| check.ok)
    }
    pub fn text(&self) -> String {
        let mut out = format!(
            "Alondra cutover rehearsal: {}\nlive HOME {} (read only), work {}\n\nNumbers\n",
            if self.passed() { "PASSED" } else { "FAILED" },
            self.live_home,
            self.work
        );
        for (name, value) in &self.numbers {
            out.push_str(&format!("  {name}: {value}\n"));
        }
        out.push_str("\nTimings (ms)\n");
        for (name, ms) in &self.timings_ms {
            out.push_str(&format!("  {name}: {ms}\n"));
        }
        let failed: Vec<_> = self.checks.iter().filter(|check| !check.ok).collect();
        out.push_str(&format!(
            "\nChecks: {} passed, {} failed\n",
            self.checks.len() - failed.len(),
            failed.len()
        ));
        for check in failed {
            out.push_str(&format!("  FAIL {}: {}\n", check.name, check.detail));
        }
        out
    }
}

/// What the cutover must import, computed from the legacy snapshot alone.
#[derive(Debug, Default, Serialize)]
pub(crate) struct LegacyExpectations {
    pub inventory: LegacyInventory,
    pub active_tasks: usize,
    pub requests: usize,
    pub assignees: usize,
    pub open_prs: Vec<u64>,
    pub active_tasks_with_prs: Vec<String>,
    pub open_questions: usize,
    pub unread_reports: usize,
    pub documents: usize,
    pub uncertain_deliveries: usize,
    pub schedules: usize,
    pub schedules_enabled: usize,
    pub schedule_runs: usize,
    pub schedule_runs_today: usize,
}

fn legacy_seq(database: &Path) -> Result<i64> {
    let connection = Connection::open_with_flags(
        canonical_leaf(database)?,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    Ok(connection.query_row(
        "SELECT max(coalesce((SELECT max(seq) FROM event_log),0),coalesce((SELECT max(seq) FROM snapshots),0))",
        [],
        |row| row.get(0),
    )?)
}

/// Copies what a cutover backup preserves, plus the domain sidecar, from a
/// running HOME into a new private directory. Each database is one consistent
/// SQLite snapshot; the set is ordered so every cross-reference resolves.
pub(crate) fn snapshot_live_home(live: &Path, target: &Path) -> Result<Value> {
    ensure!(live.is_absolute() && target.is_absolute(), "rehearsal paths must be absolute");
    let live = live.canonicalize()?;
    destination_outside(&live, target)?;
    ensure!(
        crate::domain_cutover::read_marker(&live)?.is_none(),
        "live HOME already carries a domain cutover marker"
    );
    fs::DirBuilder::new().mode(0o700).create(target)?;
    let started = Instant::now();
    let seq_before = legacy_seq(&live.join("alondra.db"))?;
    let mut databases = 0;
    for name in SNAPSHOT_DATABASES {
        if live.join(name).exists() {
            sqlite_backup(&live.join(name), &target.join(name))?;
            databases += 1;
        }
    }
    let mut files = Vec::new();
    for name in BACKUP_FILES {
        if live.join(name).exists() {
            copy_regular(&live.join(name), &target.join(name))?;
            files.push(describe_file(target, &target.join(name))?);
        }
    }
    for directory in BACKUP_TREES {
        if live.join(directory).exists() {
            copy_tree(&live.join(directory), &target.join(directory), target, &mut files)?;
        }
    }
    let window = started.elapsed();
    let seq_after = legacy_seq(&live.join("alondra.db"))?;
    Ok(json!({
        "databases": databases,
        "files": files.len(),
        "file_bytes": files.iter().map(|file| file.bytes).sum::<u64>(),
        "window_ms": window.as_millis() as u64,
        "live_seq_before": seq_before,
        "live_seq_after": seq_after,
        "snapshot_seq": legacy_seq(&target.join("alondra.db"))?,
    }))
}

fn local_midnight_ms(now_ms: i64) -> i64 {
    let seconds = (now_ms / 1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&seconds, &mut tm) };
    tm.tm_hour = 0;
    tm.tm_min = 0;
    tm.tm_sec = 0;
    tm.tm_isdst = -1;
    (unsafe { libc::mktime(&mut tm) }) as i64 * 1000
}

fn valid_pr(pr: &alondra_core::pr::PullRequest) -> bool {
    pr.head_sha.as_ref().is_some_and(|v| !v.trim().is_empty())
        && pr.base.as_ref().is_some_and(|v| !v.trim().is_empty())
        && !pr.merge_methods.is_empty()
}

pub(crate) fn legacy_expectations(home: &Path, view: &View, midnight_ms: i64) -> Result<LegacyExpectations> {
    use alondra_core::{delivery::State, model::QuestionStatus, pr::PrState};
    let active: BTreeMap<&String, &alondra_core::tasks::Task> =
        view.board.tasks.iter().filter(|(_, task)| import::in_flight(task)).collect();
    let requests: BTreeSet<&String> = view.board.requests.iter()
        .filter(|(_, request)| import::open_request(request)).map(|(id, _)| id)
        .chain(active.values().map(|task| &task.request)).collect();
    let assignees: BTreeSet<&str> = active.values()
        .filter_map(|task| task.assignee.as_ref().map(|a| a.session_id.as_str())).collect();
    let on_active = |task: &Option<String>| task.as_ref().is_some_and(|id| active.contains_key(id));
    let mut expectations = LegacyExpectations {
        inventory: inventory(view),
        active_tasks: active.len(),
        requests: requests.len(),
        assignees: assignees.len(),
        open_prs: view.prs.values().filter(|pr| valid_pr(pr) && pr.state == PrState::Open).map(|pr| pr.id).collect(),
        active_tasks_with_prs: active.keys()
            .filter(|id| view.prs.values().any(|pr| valid_pr(pr) && pr.task.as_ref() == Some(*id)))
            .map(|id| (*id).clone()).collect(),
        open_questions: view.questions.values()
            .filter(|q| q.status == QuestionStatus::Open && on_active(&q.task)).count(),
        unread_reports: view.reports.values().filter(|r| !r.read && on_active(&r.task)).count(),
        documents: view.reports.values()
            .filter(|r| r.kind == alondra_core::model::ReportKind::Document && on_active(&r.task)).count(),
        uncertain_deliveries: view.delivery_attempts.values()
            .filter(|attempt| matches!(attempt.state, State::Uncertain | State::Dispatching)
                && active.values().any(|task| task.assignee.as_ref().is_some_and(|a| a.session_id == attempt.session_id)))
            .count(),
        ..LegacyExpectations::default()
    };
    if !home.join("schedules.db").exists() {
        return Ok(expectations);
    }
    let connection = Connection::open_with_flags(
        canonical_leaf(&home.join("schedules.db"))?,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let mut query = connection.prepare("SELECT body FROM schedules")?;
    let bodies = query
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for body in bodies {
        let schedule: Value = serde_json::from_str(&body)?;
        expectations.schedules += 1;
        expectations.schedules_enabled += usize::from(schedule["enabled"] == json!(true));
        if matches!(schedule["status"].as_str(), Some("sent" | "failed")) {
            expectations.schedule_runs += 1;
            if schedule["last_run"].as_i64().is_some_and(|at| at >= midnight_ms) {
                expectations.schedule_runs_today += 1;
            }
        }
    }
    Ok(expectations)
}

fn unresolved_deliveries(view: &View) -> Vec<&alondra_core::delivery::Attempt> {
    view.delivery_attempts.values().filter(|attempt| unresolved_delivery(attempt)).collect()
}

/// Unresolved legacy deliveries by state, channel and reason, with how long
/// ago each group last changed and whether its node is still alive.
pub(crate) fn delivery_blockers(view: &View, now_ms: i64) -> Value {
    let mut groups: BTreeMap<String, (usize, i64, i64, usize)> = BTreeMap::new();
    for attempt in unresolved_deliveries(view) {
        let key = format!(
            "{}/{}/{}",
            serde_json::to_value(attempt.state).unwrap_or_default().as_str().unwrap_or("?"),
            serde_json::to_value(attempt.channel).unwrap_or_default().as_str().unwrap_or("?"),
            attempt.reason.as_deref().unwrap_or("-")
        );
        let entry = groups.entry(key).or_insert((0, i64::MAX, i64::MIN, 0));
        entry.0 += 1;
        entry.1 = entry.1.min(attempt.updated_at);
        entry.2 = entry.2.max(attempt.updated_at);
        entry.3 += usize::from(view.nodes.get(&attempt.session_id).is_some_and(|node| node.alive));
    }
    groups
        .into_iter()
        .map(|(key, (count, oldest, newest, alive))| {
            (key, json!({"attempts": count, "oldest_hours": (now_ms - oldest) / 3_600_000,
                "newest_hours": (now_ms - newest) / 3_600_000, "live_nodes": alive}))
        })
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// Where each schedule run and each legacy Task that held several PRs landed.
fn links(legacy: &LegacyExpectations, state: &d::DomainState, midnight_ms: i64) -> Value {
    let runs: Vec<_> = state.schedule.runs.values().map(|run| json!({
        "run": run.id.0, "schedule": run.schedule_id.0, "state": run.state,
        "request": run.request_id.as_ref().map(|id| &id.0), "task": run.task_id.as_ref().map(|id| &id.0),
        "today": run.finished_at.or(run.claimed_at).is_some_and(|at| at >= midnight_ms),
    })).collect();
    let split: serde_json::Map<_, _> = legacy.inventory.multiple_pr_tasks.iter().map(|task_id| {
        let original = state.tasks.get(&d::TaskId(task_id.clone()));
        let derived: Vec<_> = state.tasks.values()
            .filter(|task| task.id.0.starts_with("legacy-pr-task-")
                && original.is_some_and(|original| original.request_id == task.request_id))
            .map(|task| json!({"task": task.id.0, "pr": task.pr_id.as_ref().map(|id| &id.0), "state": task.state}))
            .collect();
        (task_id.clone(), json!({
            "imported": original.is_some(),
            "pr": original.and_then(|task| task.pr_id.as_ref()).map(|id| &id.0),
            "state": original.map(|task| task.state),
            "derived": derived,
        }))
    }).collect();
    json!({"schedule_runs": runs, "multiple_pr_tasks": split})
}

fn import_number(imported: &Value, field: &str) -> u64 {
    imported[field].as_u64().unwrap_or(u64::MAX)
}

fn is_unread_information(interaction: &d::Interaction) -> bool {
    interaction.recipient == d::Recipient::User
        && matches!(&interaction.payload, d::InteractionPayload::Information { read_by } if read_by.is_empty())
}

/// Compare the committed store with the legacy snapshot and the importer's own report.
pub(crate) fn count_checks(
    report: &mut Report,
    legacy: &LegacyExpectations,
    imported: &Value,
    state: &d::DomainState,
    home: &Path,
    midnight_ms: i64,
) {
    let unowned = d::RequestId(import::UNOWNED_PR_REQUEST.into());
    let synthetic = u64::from(state.requests.contains_key(&unowned));
    report.equal("open Requests and Requests of work in flight", legacy.requests as u64,
        state.requests.len() as u64 - synthetic);
    report.equal("Tasks in flight", legacy.active_tasks as u64, import_number(imported, "tasks"));
    report.equal("Tasks in flight + derived PR Tasks",
        (legacy.active_tasks as u64).saturating_add(import_number(imported, "derived_pr_tasks")),
        state.tasks.len() as u64);
    report.check("only work in flight is imported",
        state.tasks.values().all(|task| !task.archived && !task.state.terminal()),
        format!("{} Tasks", state.tasks.len()));
    report.equal("Agents are the assignees of work in flight", legacy.assignees as u64, state.agents.len() as u64);
    let missing: Vec<_> = legacy.open_prs.iter()
        .filter(|id| !state.pull_requests.contains_key(&d::PrId(format!("legacy-pr-{id}")))).collect();
    report.check("every open PR is imported", missing.is_empty(), format!("{} open, missing {missing:?}", legacy.open_prs.len()));
    let unregistered: Vec<_> = legacy.active_tasks_with_prs.iter()
        .filter(|id| state.tasks.get(&d::TaskId((*id).clone())).is_none_or(|task| task.pr_id.is_none())).collect();
    report.check("every active Task keeps its registered PR", unregistered.is_empty(), format!("{unregistered:?}"));
    let stray: Vec<_> = state.pull_requests.values()
        .filter(|pr| pr.state != d::PrState::Open
            && state.tasks.get(&pr.task_id).is_none_or(|task| task.id.0.starts_with("legacy-pr-task-")))
        .map(|pr| pr.id.0.clone()).collect();
    report.check("closed PRs only as an active Task's registered PR", stray.is_empty(), format!("{stray:?}"));
    report.equal("open questions of work in flight", legacy.open_questions as u64,
        import_number(imported, "questions_reopened").saturating_add(import_number(imported, "questions_unread")));
    let reopened = state.interactions.values()
        .filter(|interaction| matches!(&interaction.payload, d::InteractionPayload::Question { answer: None, .. }))
        .count() as u64;
    report.equal("reopened questions in v2", import_number(imported, "questions_reopened"), reopened);
    report.equal("unread reports of work in flight", legacy.unread_reports as u64, import_number(imported, "reports_unread"));
    report.equal("documents of work in flight", legacy.documents, state.documents.len());
    let stacks = imported["stacked_prs"].as_array().cloned().unwrap_or_default();
    let open_heads: BTreeMap<u64, (&str, &str)> = state
        .pull_requests
        .values()
        .filter(|pr| pr.state == d::PrState::Open)
        .map(|pr| (pr.number, (pr.head.as_str(), pr.base.as_str())))
        .collect();
    let broken: Vec<_> = stacks
        .iter()
        .filter(|pair| {
            let (Some(upper), Some(lower)) = (pair[0].as_u64(), pair[1].as_u64()) else { return true };
            !matches!((open_heads.get(&upper), open_heads.get(&lower)),
                (Some((_, base)), Some((head, _))) if base == head)
        })
        .collect();
    report.check(
        "stacked PRs are open PRs based on each other",
        broken.is_empty(),
        format!("{} stacks, unresolved {broken:?}", stacks.len()),
    );
    let mut references = BTreeMap::new();
    for interaction in state.interactions.values() {
        let answered = match &interaction.payload {
            d::InteractionPayload::Question { answer: Some(answer), .. }
            | d::InteractionPayload::ActionRequest { answer: Some(answer), .. } => answer.attachments.as_slice(),
            _ => &[],
        };
        for attachment in interaction.attachments.iter().chain(answered) {
            references.insert(attachment.content_reference.clone(), attachment.content_digest.clone());
        }
    }
    for revision in state.document_revisions.values() {
        references.insert(revision.content_reference.clone(), revision.content_digest.clone());
    }
    let mismatched: Vec<_> = references
        .iter()
        .filter(|(reference, digest)| verify_content(home, reference, digest).is_err())
        .map(|(reference, _)| reference.clone())
        .collect();
    report.check(
        "stored content matches its SHA-256",
        mismatched.is_empty(),
        format!("{} blobs checked, mismatched {mismatched:?}", references.len()),
    );
    report.equal(
        "final presentations",
        import_number(imported, "final_presentations"),
        state.tasks.values().filter(|task| task.final_presentation.is_some()).count() as u64,
    );
    let runs_today = state
        .schedule
        .runs
        .values()
        .filter(|run| run.finished_at.or(run.claimed_at).is_some_and(|at| at >= midnight_ms))
        .count();
    report.equal("schedules", legacy.schedules, state.schedule.schedules.len());
    report.equal(
        "enabled schedules",
        legacy.schedules_enabled,
        state.schedule.schedules.values().filter(|schedule| schedule.enabled).count(),
    );
    report.equal("schedule runs imported + dropped with their finished Task", legacy.schedule_runs as u64,
        (state.schedule.runs.len() as u64).saturating_add(import_number(imported, "schedule_runs_dropped")));
    report.numbers.insert("schedule_runs_today".into(), json!({"legacy": legacy.schedule_runs_today, "imported": runs_today}));
    let dangling: Vec<_> = state.schedule.runs.values()
        .filter(|run| run.task_id.as_ref().is_some_and(|task| !state.tasks.contains_key(task)))
        .map(|run| run.id.0.clone()).collect();
    report.check("schedule runs link only imported Tasks", dangling.is_empty(), format!("{dangling:?}"));
    let pending_launch = state.schedule.runs.values().filter(|run| run.launch.is_some()).count();
    report.check(
        "no imported schedule run can launch",
        pending_launch == 0,
        format!("{pending_launch} runs carry a launch"),
    );
}

/// Only unknown-outcome deliveries to an active Task's assignee arrive, unread
/// for the user; retries and everything else stay behind and are never resent.
pub(crate) fn delivery_checks(report: &mut Report, legacy: &LegacyExpectations, imported: &Value, state: &d::DomainState) {
    report.equal("unknown-outcome deliveries shown to the user", legacy.uncertain_deliveries as u64,
        import_number(imported, "deliveries_uncertain"));
    let cards: Vec<_> = state.interactions.values().filter(|i| i.id.0.starts_with("legacy-delivery-")).collect();
    report.equal("delivery cards", legacy.uncertain_deliveries, cards.len());
    let wrong: Vec<_> = cards.iter().filter(|card| !is_unread_information(card)).map(|card| card.id.0.clone()).collect();
    report.check("delivery cards are unread for the user", wrong.is_empty(), format!("{wrong:?}"));
    report.check("nothing waits in the outbox", state.revision > 0, "the import queues no delivery");
}

/// Structural rules of the inbox plus the counts the importer promises: only
/// unanswerable questions, unread reports and unknown-outcome deliveries arrive
/// unread, and only questions of an active Task's assignee stay answerable.
pub(crate) fn projection_checks(report: &mut Report, state: &d::DomainState, imported: &Value) {
    let projection = state.projection();
    let visible = |task: Option<&d::TaskId>| task.is_none_or(|id| state.tasks.get(id).is_none_or(|t| !t.archived));
    let mut problems = Vec::new();
    let mut leaves = Vec::new();
    for root in &projection.inbox {
        if root.children.is_empty() {
            leaves.push(root);
            continue;
        }
        if !matches!(root.kind.as_str(), "pull_request" | "document_review") {
            problems.push(format!("{} groups children but is {}", root.id, root.kind));
        }
        if root.pending != root.children.iter().any(|child| child.pending) {
            problems.push(format!("{} pending differs from its children", root.id));
        }
        if root.children.windows(2).any(|pair| (pair[0].order, &pair[0].id) > (pair[1].order, &pair[1].id)) {
            problems.push(format!("{} children out of order", root.id));
        }
        for child in &root.children {
            if child.task_id != root.task_id {
                problems.push(format!("{} holds {} of another Task", root.id, child.id));
            }
            leaves.push(child);
        }
    }
    for leaf in &leaves {
        let kinds: Vec<_> = leaf.available_actions.iter().map(|action| action.kind.as_str()).collect();
        let expected: &[&str] = match leaf.kind.as_str() {
            "information" => &["mark_read"],
            "question" | "action_request" => &["answer_question"],
            "delivery" => &["accept_delivery", "submit_feedback"],
            _ => &[],
        };
        if expected.is_empty() || !kinds.starts_with(expected) {
            problems.push(format!("{} ({}) offers {kinds:?}", leaf.id, leaf.kind));
        }
        if leaf.kind == "information" && leaf.available_actions[0].enabled != leaf.pending {
            problems.push(format!("{} mark_read enabled differs from unread", leaf.id));
        }
    }
    let pending = leaves.iter().filter(|leaf| leaf.pending).count() as u64;
    if projection.counts.roots != projection.inbox.len() as u64 || projection.counts.pending != pending {
        problems.push(format!("counts {:?} differ from inbox", projection.counts));
    }
    report.check("inbox roots, children and actions are well formed", problems.is_empty(), problems.join("; "));
    let unread = state
        .interactions
        .values()
        .filter(|interaction| {
            is_unread_information(interaction)
                && visible(match &interaction.scope {
                    d::Scope::Task { task_id } => Some(task_id),
                    _ => None,
                })
        })
        .count() as u64;
    report.equal("unread information", unread, projection.counts.unread_information);
    let expected_unread = import_number(imported, "questions_unread")
        .saturating_add(import_number(imported, "reports_unread"))
        .saturating_add(import_number(imported, "deliveries_uncertain"));
    report.check(
        "only unanswerable questions, unread reports and uncertain deliveries arrive unread",
        unread <= expected_unread,
        format!("{unread} unread, {expected_unread} expected"),
    );
    let questions = leaves.iter().filter(|leaf| leaf.kind == "question" && leaf.pending).count() as u64;
    report.check(
        "reopened questions are answerable",
        questions <= import_number(imported, "questions_reopened")
            && leaves.iter().filter(|leaf| leaf.kind == "question" && leaf.pending)
                .all(|leaf| leaf.available_actions[0].enabled),
        format!("{questions} pending question items, {} reopened", import_number(imported, "questions_reopened")),
    );
    let deliveries = leaves.iter().filter(|leaf| leaf.kind == "delivery").count();
    let presented = state
        .tasks
        .values()
        .filter(|task| !task.archived && task.final_presentation.is_some())
        .count();
    report.equal("final presentations in inbox", presented, deliveries);
    report.numbers.insert(
        "projection".into(),
        json!({"roots": projection.counts.roots, "pending": projection.counts.pending,
            "unread_information": projection.counts.unread_information, "items": leaves.len(),
            "grouped_roots": projection.inbox.iter().filter(|root| !root.children.is_empty()).count()}),
    );
}

pub(crate) struct Signer {
    device: String,
    key: p256::ecdsa::SigningKey,
}

impl Signer {
    /// The terminal's own paired key when the snapshot carries it; otherwise a
    /// new device paired into the snapshot's devices.json.
    pub(crate) fn for_home(home: &Path) -> Result<(Self, &'static str)> {
        use p256::pkcs8::DecodePrivateKey;
        let auth = crate::auth::Auth::open(home)?;
        if let (Ok(pem), Ok(device)) = (
            fs::read_to_string(home.join("tui-key.pem")),
            fs::read_to_string(home.join("tui-device")),
        ) {
            let key = p256::ecdsa::SigningKey::from_pkcs8_pem(&pem).context("tui-key.pem")?;
            let device = device.trim().to_owned();
            if auth.list().iter().any(|paired| paired.id == device) {
                return Ok((Self { device, key }, "terminal device"));
            }
        }
        use base64::Engine as _;
        let mut seed = [0u8; 32];
        File::open("/dev/urandom")?.read_exact(&mut seed)?;
        let key = p256::ecdsa::SigningKey::from_slice(&seed)?;
        let public = base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_sec1_bytes());
        let code = auth.new_pairing_code();
        let device = auth.pair(&code, "Cutover rehearsal", &public).map_err(anyhow::Error::msg)?;
        Ok((Self { device: device.id, key }, "rehearsal device"))
    }

    fn headers(&self, method: &str, path: &str, body: &[u8]) -> [(&'static str, String); 3] {
        use base64::Engine as _;
        use p256::ecdsa::signature::Signer as _;
        let timestamp = alondra_core::now_millis().to_string();
        let message = crate::auth::signed_message(method, path, &timestamp, body);
        let signature: p256::ecdsa::Signature = self.key.sign(message.as_bytes());
        [
            ("x-alondra-device", self.device.clone()),
            ("x-alondra-timestamp", timestamp),
            (
                "x-alondra-signature",
                base64::engine::general_purpose::STANDARD.encode(signature.to_der().as_bytes()),
            ),
        ]
    }
}

async fn signed_request(
    client: &reqwest::Client, base: &str, signer: &Signer, method: &str, path: &str, body: &[u8],
) -> Result<(u16, Vec<u8>, Duration)> {
    let started = Instant::now();
    let mut request = client
        .request(reqwest::Method::from_bytes(method.as_bytes())?, format!("{base}{path}"))
        .header("content-type", "application/json")
        .body(body.to_vec());
    for (name, value) in signer.headers(method, path, body) {
        request = request.header(name, value);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let body = response.bytes().await?.to_vec();
    Ok((status, body, started.elapsed()))
}

/// Read a sample of every v2 read route as the signed user device and compare
/// each answer with the same view computed from the store in process.
pub(crate) async fn read_checks(report: &mut Report, label: &str, base: &str, signer: &Signer, state: &d::DomainState) -> Result<()> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
    let mut paths = vec![
        "/v2/domain".to_owned(),
        "/v2/domain/capabilities".to_owned(),
        "/v2/domain/schedules".to_owned(),
        "/v2/domain/delivery-attempts?after=0".to_owned(),
        format!("/v2/domain/changes?data_epoch={}&after={}&wait=0", state.data_epoch.0, state.revision),
    ];
    paths.extend(state.schedule.schedules.keys().take(3).map(|id| format!("/v2/domain/schedules/{}", id.0)));
    let tasks: Vec<_> = state.tasks.values().filter(|task| !task.state.terminal()).take(5)
        .chain(state.tasks.values().filter(|task| task.state.terminal()).take(5)).collect();
    paths.extend(tasks.iter().map(|task| format!("/v2/domain/tasks/{}/actions", task.id.0)));
    paths.extend(state.document_revisions.keys().take(5).map(|id| format!("/v2/domain/document-revisions/{}/content?offset=0", id.0)));
    let mut slowest = (Duration::ZERO, String::new());
    let mut failed = Vec::new();
    for path in &paths {
        let (status, body, elapsed) = signed_request(&client, base, signer, "GET", path, b"").await?;
        if elapsed > slowest.0 {
            slowest = (elapsed, path.clone());
        }
        if status != 200 {
            failed.push(format!("{path}: {status} {}", String::from_utf8_lossy(&body)));
            continue;
        }
        if path == "/v2/domain" {
            let served: Value = serde_json::from_slice(&body)?;
            report.time(&format!("{label}: projection read ({} bytes)", body.len()), elapsed);
            report.check(
                format!("{label}: served projection equals the store's"),
                served == serde_json::to_value(state.projection())?,
                format!("revision {}", served["revision"]),
            );
        }
        if let Some(task) = path.strip_prefix("/v2/domain/tasks/").and_then(|rest| rest.strip_suffix("/actions")) {
            let served: Value = serde_json::from_slice(&body)?;
            report.check(
                format!("{label}: actions of Task {task}"),
                served["actions"] == serde_json::to_value(state.task_actions(&d::TaskId(task.into())))?,
                "served vs store",
            );
        }
    }
    report.check(format!("{label}: {} signed reads", paths.len()), failed.is_empty(), failed.join("; "));
    report.time(&format!("{label}: slowest read {}", slowest.1), slowest.0);
    report.check(
        format!("{label}: reads under {} ms", SLOW.as_millis()),
        slowest.0 < SLOW,
        format!("slowest {} ms on {}", slowest.0.as_millis(), slowest.1),
    );
    Ok(())
}

/// Safe user commands through the running daemon, signed by the paired
/// device like the TUI: each must succeed, stay fast and replay its receipt.
pub(crate) async fn command_checks(report: &mut Report, label: &str, base: &str, signer: &Signer, state: &d::DomainState) -> Result<u64> {
    let mut commands: Vec<(String, d::Command)> = state
        .interactions
        .values()
        .filter(|interaction| is_unread_information(interaction))
        .take(3)
        .map(|interaction| {
            (format!("mark read {}", interaction.id.0), d::Command::MarkRead { interaction_id: interaction.id.clone() })
        })
        .collect();
    if let Some(task) = state.tasks.values().find(|task| {
        state.task_actions(&task.id).is_some_and(|actions| {
            actions.iter().any(|action| action.kind == "archive_task" && action.enabled)
        })
    }) {
        for archived in [true, false] {
            commands.push((
                format!("{} {}", if archived { "archive" } else { "unarchive" }, task.id.0),
                d::Command::ArchiveTask { task_id: task.id.clone(), archived },
            ));
        }
    }
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
    let draft = serde_json::to_vec(&json!({"title":"Cutover rehearsal","text":"Never runs","project":null,
        "options":{},"disallowed_tools":[],"frequency":"daily","hour":3,"minute":0,"weekday":null,
        "interval":null,"start_at":null,"enabled":false,"revision":0}))?;
    let (status, created, elapsed) = signed_request(&client, base, signer, "POST", "/v2/domain/schedules", &draft).await?;
    report.time(&format!("{label}: create a disabled schedule"), elapsed);
    report.check(format!("{label}: create a disabled schedule"), status == 201, String::from_utf8_lossy(&created));
    let id = serde_json::from_slice::<Value>(&created).unwrap_or_default()["id"].as_str().unwrap_or_default().to_owned();
    let path = format!("/v2/domain/schedules/{id}");
    let (status, deleted, elapsed) = signed_request(&client, base, signer, "DELETE", &path, b"").await?;
    report.time(&format!("{label}: delete it"), elapsed);
    report.check(format!("{label}: delete the disabled schedule"), status == 200, String::from_utf8_lossy(&deleted));
    let (_, capabilities, _) = signed_request(&client, base, signer, "GET", "/v2/domain/capabilities", b"").await?;
    let mut revision = serde_json::from_slice::<Value>(&capabilities)?["revision"].as_u64().context("no revision")?;
    for (index, (name, command)) in commands.into_iter().enumerate() {
        let body = serde_json::to_vec(&d::CommandEnvelope {
            protocol_version: d::PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("cutover-rehearsal-{index}").into(),
            expected_revision: revision,
            command,
        })?;
        let (status, first, elapsed) = signed_request(&client, base, signer, "POST", "/v2/domain/commands", &body).await?;
        let outcome: Value = serde_json::from_slice(&first).unwrap_or_default();
        revision = outcome["revision"].as_u64().unwrap_or(revision);
        report.time(&format!("{label}: command {name}"), elapsed);
        report.check(format!("{label}: command {name}"), status == 200, String::from_utf8_lossy(&first));
        report.check(format!("{label}: command {name} under {} ms", SLOW.as_millis()), elapsed < SLOW, format!("{} ms", elapsed.as_millis()));
        let (_, retry, _) = signed_request(&client, base, signer, "POST", "/v2/domain/commands", &body).await?;
        report.check(format!("{label}: command {name} retry returns its receipt"), retry == first, String::from_utf8_lossy(&retry));
    }
    Ok(revision)
}

fn digest_file(path: &Path) -> Result<String> {
    Ok(describe_file(path.parent().context("file has no parent")?, path)?.sha256)
}

/// Schema and every row, so a restored SQLite file compares by content rather
/// than by page layout or header counters.
pub(crate) fn sqlite_content_digest(path: &Path) -> Result<String> {
    let connection = Connection::open_with_flags(
        canonical_leaf(path)?,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    connection.execute_batch("BEGIN")?;
    let mut hash = Sha256::new();
    let mut tables = connection.prepare("SELECT name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type,name")?;
    let schema = tables
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (name, sql) in &schema {
        hash.update(sql.as_bytes());
        if !sql.starts_with("CREATE TABLE") {
            continue;
        }
        let mut rows = Vec::new();
        let mut query = connection.prepare(&format!("SELECT * FROM \"{}\"", name.replace('"', "\"\"")))?;
        let columns = query.column_count();
        let mut cursor = query.query([])?;
        while let Some(row) = cursor.next()? {
            let values = (0..columns)
                .map(|index| row.get::<_, rusqlite::types::Value>(index).map(|value| format!("{value:?}")))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.push(values.join("\u{1f}"));
        }
        rows.sort();
        for row in rows {
            hash.update(row.as_bytes());
            hash.update(b"\x1e");
        }
    }
    connection.execute_batch("COMMIT")?;
    Ok(hex(&hash.finalize()))
}

/// Content digests of the files a backup preserves, taken before anything runs on the snapshot.
pub(crate) fn content_digests(home: &Path) -> Result<BTreeMap<String, String>> {
    let mut digests = BTreeMap::new();
    for name in BACKUP_DATABASES {
        if home.join(name).exists() {
            digests.insert(name.to_owned(), sqlite_content_digest(&home.join(name))?);
        }
    }
    let mut files = Vec::new();
    for name in BACKUP_FILES {
        if home.join(name).exists() {
            files.push(describe_file(home, &home.join(name))?);
        }
    }
    for directory in BACKUP_TREES {
        if home.join(directory).exists() {
            collect_stage_files(home, &home.join(directory), &mut files)?;
        }
    }
    digests.extend(files.into_iter().map(|file| (file.path, file.sha256)));
    Ok(digests)
}

struct Sandbox {
    live: PathBuf,
    work: PathBuf,
    home: PathBuf,
    exe: PathBuf,
}

impl Sandbox {
    fn env(&self, command: &mut Command) {
        command
            .env_clear()
            .env("HOME", self.work.join("fakehome"))
            .env("TMPDIR", self.work.join("tmp"))
            .env("PATH", format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.work.join("stubs").display()))
            .env("ALONDRA_HOME", &self.home)
            .env(RECORDED_HOME, &self.live)
            .env("ALONDRA_SERVICE_LABEL", "com.alondra.cutover-rehearsal")
            .env("ALONDRA_UPDATE_URL", "http://127.0.0.1:9/")
            .env("ALONDRA_AUTO_UPDATE", "0")
            .env("ALONDRA_WAKE", "0")
            .stdin(Stdio::null());
        if let Ok(user) = std::env::var("USER") {
            command.env("USER", user);
        }
    }

    fn cli(&self, binary: &Path, args: &[&Path]) -> Result<(String, Duration)> {
        let mut command = Command::new(binary);
        command.args(args);
        self.env(&mut command);
        let started = Instant::now();
        let output = command.output()?;
        let elapsed = started.elapsed();
        ensure!(
            output.status.success(),
            "{} failed: {}",
            args.first().map_or(String::new(), |flag| flag.display().to_string()),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok((String::from_utf8_lossy(&output.stdout).into_owned(), elapsed))
    }

    fn start(&self, port: u16, log: &Path) -> Result<Child> {
        let mut command = Command::new(&self.exe);
        self.env(&mut command);
        command
            .env("ALONDRA_PORT", port.to_string())
            .env("ALONDRA_LISTEN", "127.0.0.1")
            .stdout(File::create(log)?)
            .stderr(File::create(log.with_extension("stderr.log"))?);
        Ok(command.spawn()?)
    }

    fn stub_calls(&self) -> String {
        fs::read_to_string(self.work.join("stub-calls.log")).unwrap_or_default()
    }
}

fn free_port() -> Result<u16> {
    let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    ensure!(port != alondra_core::DEFAULT_PORT, "picked the live daemon's port");
    Ok(port)
}

async fn wait_listening(child: &mut Child, base: &str) -> Result<Duration> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("daemon exited during startup: {status}");
        }
        if let Ok(response) = client.get(format!("{base}/v2/daemon")).send().await {
            if response.status().is_success() {
                return Ok(started.elapsed());
            }
        }
        ensure!(started.elapsed() < Duration::from_secs(120), "daemon did not listen within 120 s");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn stop(child: &mut Child) -> Result<Duration> {
    let started = Instant::now();
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    loop {
        if child.try_wait()?.is_some() {
            return Ok(started.elapsed());
        }
        if started.elapsed() > Duration::from_secs(30) {
            child.kill()?;
            child.wait()?;
            anyhow::bail!("daemon ignored SIGTERM for 30 s");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn log_problems(log: &Path) -> Vec<String> {
    let text = fs::read_to_string(log.with_extension("stderr.log")).unwrap_or_default()
        + &fs::read_to_string(log).unwrap_or_default();
    text.lines()
        .filter(|line| {
            let line = line.to_ascii_lowercase();
            ["error", "fail", "panic", "refus", "disabled"].iter().any(|word| line.contains(word))
        })
        .take(20)
        .map(str::to_owned)
        .collect()
}

fn run_daemon(
    report: &mut Report,
    sandbox: &Sandbox,
    runtime: &tokio::runtime::Runtime,
    label: &str,
    signer: &Signer,
    state: &d::DomainState,
    commands: bool,
) -> Result<Option<u64>> {
    let port = free_port()?;
    let base = format!("http://127.0.0.1:{port}");
    let log = sandbox.work.join(format!("{label}.log"));
    let mut child = sandbox.start(port, &log)?;
    let result = runtime.block_on(async {
        let startup = wait_listening(&mut child, &base).await?;
        report.time(&format!("{label}: startup"), startup);
        read_checks(report, label, &base, signer, state).await?;
        if commands {
            return command_checks(report, label, &base, signer, state).await.map(Some);
        }
        Ok(None)
    });
    let stopped = stop(&mut child);
    let revision = result?;
    report.time(&format!("{label}: stop"), stopped?);
    let problems = log_problems(&log);
    report.check(format!("{label}: log has no errors"), problems.is_empty(), problems.join(" | "));
    Ok(revision)
}

fn write_stubs(work: &Path) -> Result<()> {
    let stubs = work.join("stubs");
    fs::DirBuilder::new().mode(0o700).create(&stubs)?;
    for name in STUBBED_COMMANDS {
        let path = stubs.join(name);
        fs::write(
            &path,
            format!("#!/bin/sh\necho \"{name} $*\" >> '{}'\nexit 1\n", work.join("stub-calls.log").display()),
        )?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Runs every stage and records each outcome; a stage that cannot run fails
/// its own check and the stages that need its result are skipped.
pub(crate) fn run(live: &Path, work: &Path, legacy: &Path) -> Result<Report> {
    ensure!(live.is_absolute() && work.is_absolute() && legacy.is_absolute(), "rehearsal paths must be absolute");
    destination_outside(live, work)?;
    ensure!(fs::symlink_metadata(legacy)?.is_file(), "legacy daemon must be a regular file");
    fs::DirBuilder::new().mode(0o700).create(work)?;
    let work = &work.canonicalize()?;
    let live = &live.canonicalize()?;
    for directory in ["tmp", "fakehome", "fakehome/Projects"] {
        fs::DirBuilder::new().mode(0o700).create(work.join(directory))?;
    }
    write_stubs(work)?;
    let sandbox = Sandbox { live: live.to_owned(), work: work.to_owned(), home: work.join("home"), exe: std::env::current_exe()? };
    let mut report = Report {
        live_home: live.display().to_string(),
        work: work.display().to_string(),
        ..Report::default()
    };
    let midnight = local_midnight_ms(alondra_core::now_millis());
    let started = Instant::now();
    match snapshot_live_home(live, &sandbox.home) {
        Ok(snapshot) => {
            report.time("snapshot", started.elapsed());
            report.numbers.insert("snapshot".into(), snapshot);
        }
        Err(error) => {
            report.fail("snapshot", &error);
            return Ok(report);
        }
    }
    if let Err(error) = rehearse_snapshot(&mut report, &sandbox, legacy, midnight) {
        report.fail("rehearsal stopped", &error);
    }
    let calls = sandbox.stub_calls();
    let effectful: Vec<_> = calls.lines().filter(|call| !read_only_github_query(call)).collect();
    report.numbers.insert("intercepted_commands".into(), json!(calls.lines().collect::<Vec<_>>()));
    report.check("nothing but read-only GitHub queries was attempted, and none left the sandbox",
        effectful.is_empty(), effectful.join("; "));
    Ok(report)
}

/// The v2 daemon observes the state of open PRs; that is the only outside call it may try.
fn read_only_github_query(call: &str) -> bool {
    let words: Vec<_> = call.split_whitespace().collect();
    matches!(words.as_slice(), ["gh", "pr", "view" | "list" | "checks", ..])
        || (words.starts_with(&["gh", "api"]) && !words.iter().any(|word| matches!(*word, "-X" | "--method" | "-f" | "-F")))
}

/// Writes `<prefix>.json` and `<prefix>.txt`, prints the text, and returns whether every check passed.
pub(crate) fn run_and_write(live: &Path, work: &Path, legacy: &Path, prefix: &Path) -> Result<bool> {
    let report = run(live, work, legacy)?;
    let text = report.text();
    fs::write(prefix.with_extension("json"), serde_json::to_vec_pretty(&report)?)?;
    fs::write(prefix.with_extension("txt"), &text)?;
    print!("{text}");
    Ok(report.passed())
}

/// Differences between the restored HOME and the content digests taken before the cutover.
pub(crate) fn home_differences(reference: &BTreeMap<String, String>, home: &Path) -> Result<Vec<String>> {
    let mut differences = Vec::new();
    for (path, digest) in reference {
        let file = home.join(path);
        if !file.exists() {
            differences.push(format!("{path} missing"));
            continue;
        }
        let found = if BACKUP_DATABASES.contains(&path.as_str()) {
            sqlite_content_digest(&file)?
        } else {
            digest_file(&file)?
        };
        if &found != digest {
            differences.push(format!("{path} changed"));
        }
    }
    Ok(differences)
}

fn legacy_cli(legacy: &Path) -> PathBuf {
    legacy.with_file_name("alondra")
}

fn rehearse_snapshot(report: &mut Report, sandbox: &Sandbox, legacy: &Path, midnight: i64) -> Result<()> {
    let home = &sandbox.home;
    let view = load_legacy_readonly(&home.join("alondra.db"))?;
    report.numbers.insert("legacy_unresolved_deliveries".into(), delivery_blockers(&view, alondra_core::now_millis()));
    let expectations = legacy_expectations(home, &view, midnight)?;
    report.numbers.insert("legacy".into(), serde_json::to_value(&expectations)?);
    let reference = content_digests(home)?;
    let integrity: Vec<_> = SNAPSHOT_DATABASES
        .iter()
        .filter(|name| home.join(name).exists())
        .filter_map(|name| {
            let connection = Connection::open_with_flags(home.join(name), OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
            let result: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0)).ok()?;
            (result != "ok").then(|| format!("{name}: {result}"))
        })
        .collect();
    report.check("snapshot integrity", integrity.is_empty(), integrity.join("; "));

    let backup = sandbox.work.join("backup");
    let (output, elapsed) = sandbox.cli(
        &sandbox.exe,
        &[Path::new("--cutover-domain-v2"), &backup, legacy, &legacy_cli(legacy)],
    )?;
    report.time("cutover", elapsed);
    let cutover: Value = serde_json::from_str(&output)?;
    let imported = cutover["imported"].clone();
    report.numbers.insert("imported".into(), imported.clone());
    report.check("cutover committed", cutover["marker"]["phase"] == "committed", cutover["marker"].to_string());
    report.check("legacy stores archived inside HOME",
        !home.join("alondra.db").exists() && home.join(crate::domain_cutover::ARCHIVE).join("alondra.db").exists(),
        crate::domain_cutover::ARCHIVE);
    report.check("legacy binaries kept in the backup",
        backup.join(LEGACY_BINARIES).join("alondrad").exists() && backup.join(LEGACY_BINARIES).join("alondra").exists(),
        LEGACY_BINARIES);
    let (again, _) = sandbox.cli(
        &sandbox.exe,
        &[Path::new("--cutover-domain-v2"), &backup, legacy, &legacy_cli(legacy)],
    )?;
    report.check("a second cutover run only reports",
        serde_json::from_str::<Value>(&again)?["already_committed"] == true, "already committed");

    let connection = Connection::open_with_flags(home.join("domain-v2.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    let foreign: i64 = connection.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| row.get(0))?;
    report.check("committed store integrity", integrity == "ok" && foreign == 0, format!("{integrity}, {foreign} foreign key errors"));
    drop(connection);

    let state = DomainRuntime::open(home)?.state()?;
    count_checks(report, &expectations, &imported, &state, home, midnight);
    delivery_checks(report, &expectations, &imported, &state);
    report.numbers.insert("links".into(), links(&expectations, &state, midnight));
    projection_checks(report, &state, &imported);

    let mut removed = Vec::new();
    for name in OUTWARD_FILES {
        if home.join(name).exists() {
            fs::remove_file(home.join(name))?;
            removed.push(name);
        }
    }
    report.numbers.insert("outward_files_removed_from_copy".into(), json!(removed));
    let (signer, identity) = Signer::for_home(home)?;
    report.numbers.insert("read_identity".into(), json!(identity));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let revision = run_daemon(report, sandbox, &runtime, "first start", &signer, &state, true)?;
    let after = DomainRuntime::open(home)?.state()?;
    report.equal("revision after commands", revision, Some(after.revision));
    run_daemon(report, sandbox, &runtime, "restart", &signer, &after, false)?;

    let (_, elapsed) = sandbox.cli(&sandbox.exe, &[Path::new("--rollback-domain-v2"), &backup])?;
    report.time("rollback", elapsed);
    let differences = home_differences(&reference, home)?;
    report.check("rollback restores the legacy snapshot exactly", differences.is_empty(), differences.join("; "));
    report.check("rollback removes the marker", crate::domain_cutover::read_marker(home)?.is_none(), "marker");
    report.equal(
        "rollback legacy inventory",
        serde_json::to_value(&expectations.inventory)?,
        serde_json::to_value(inventory(&load_legacy_readonly(&home.join("alondra.db"))?))?,
    );
    let restored = Sandbox { live: sandbox.live.clone(), home: home.clone(), work: sandbox.work.clone(), exe: legacy.to_owned() };
    match restored.cli(legacy, &[Path::new("--preflight")]) {
        Ok((out, _)) => report.check("legacy binary starts on the restored HOME", out.contains("preflight ok"), out.trim()),
        Err(error) => report.check("legacy binary starts on the restored HOME", false, format!("{error:#}")),
    };
    match sandbox.cli(&sandbox.exe, &[Path::new("--preflight")]) {
        Ok(_) => report.check("the v2 binary refuses the restored legacy HOME", false, "preflight passed"),
        Err(error) => report.check("the v2 binary refuses the restored legacy HOME",
            format!("{error:#}").contains("still holds legacy data"), format!("{error:#}")),
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("alondra-rehearsal-{name}-{}", crate::auth::random_hex(8)));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        root
    }

    fn tree_digest(home: &Path) -> BTreeMap<String, String> {
        let mut files = Vec::new();
        collect_stage_files(home, home, &mut files).unwrap();
        files
            .into_iter()
            .filter(|file| !file.path.ends_with("-shm"))
            .map(|file| (file.path, file.sha256))
            .collect()
    }

    #[test]
    fn snapshot_keeps_uncheckpointed_wal_frames_and_leaves_the_live_home_unchanged() {
        let root = root("snapshot");
        let live = root.join("live");
        fs::DirBuilder::new().mode(0o700).create(&live).unwrap();
        let view = synthetic::write_live_shaped_home(&live);
        fs::write(live.join("files").join("blob"), b"attachment").unwrap();
        fs::write(live.join("config.toml"), "name = \"Test\"\n").unwrap();
        let writer = Connection::open(live.join("alondra.db")).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;").unwrap();
        let event = serde_json::to_string(&Event::Acknowledged { session_id: "node-0".into() }).unwrap();
        writer
            .execute(
                "INSERT INTO event_log(seq,at,kind,body) VALUES(?1,1,'acknowledged',?2)",
                rusqlite::params![view.seq as i64 + 1, event],
            )
            .unwrap();
        assert!(fs::metadata(live.join("alondra.db-wal")).unwrap().len() > 0, "the event must sit in the WAL");
        let before = tree_digest(&live);
        let copy = root.join("copy");
        let snapshot = snapshot_live_home(&live, &copy).unwrap();
        assert_eq!(snapshot["snapshot_seq"], json!(view.seq + 1));
        assert_eq!(load_legacy_readonly(&copy.join("alondra.db")).unwrap().seq, view.seq + 1);
        assert_eq!(fs::read(copy.join("files").join("blob")).unwrap(), b"attachment");
        assert_eq!(fs::read_to_string(copy.join("config.toml")).unwrap(), "name = \"Test\"\n");
        assert_eq!(tree_digest(&live), before);
        assert!(snapshot_live_home(&live, &live.join("inside")).is_err());
        drop(writer);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_read_only_github_queries_are_tolerated_from_the_rehearsal_daemon() {
        assert!(read_only_github_query("gh pr view https://github.com/o/r/pull/1 --json state,headRefOid"));
        assert!(read_only_github_query("gh api repos/o/r/pulls/1"));
        for call in ["gh pr merge 1 --squash", "gh api -X PATCH repos/o/r/pulls/1", "claude --bg x", "tmux send-keys", "git push"] {
            assert!(!read_only_github_query(call), "{call}");
        }
    }

    #[test]
    fn snapshot_order_covers_every_database_a_backup_keeps() {
        for name in BACKUP_DATABASES {
            assert!(SNAPSHOT_DATABASES.contains(&name), "{name}");
        }
        let position = |name| SNAPSHOT_DATABASES.iter().position(|n| *n == name).unwrap();
        assert!(position("schedules.db") < position("alondra.db"));
    }

    #[test]
    fn sqlite_content_digest_ignores_layout_and_sees_rows() {
        let root = root("digest");
        let first = root.join("first.db");
        let connection = Connection::open(&first).unwrap();
        connection.execute_batch("CREATE TABLE t(a TEXT); INSERT INTO t VALUES('x'),('y');").unwrap();
        drop(connection);
        let second = root.join("second.db");
        sqlite_backup(&first, &second).unwrap();
        assert_eq!(sqlite_content_digest(&first).unwrap(), sqlite_content_digest(&second).unwrap());
        Connection::open(&second).unwrap().execute("UPDATE t SET a='z' WHERE a='y'", []).unwrap();
        assert_ne!(sqlite_content_digest(&first).unwrap(), sqlite_content_digest(&second).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    /// A live-shaped HOME, snapshotted and cut over in place, as the rehearsal does.
    fn cut_over_live_shaped(root: &Path) -> (PathBuf, PathBuf, BTreeMap<String, String>, LegacyExpectations, Value) {
        let live = root.join("live");
        fs::DirBuilder::new().mode(0o700).create(&live).unwrap();
        synthetic::write_live_shaped_home(&live);
        let legacy = root.join("alondrad");
        fs::write(&legacy, b"legacy daemon --preflight").unwrap();
        let home = root.join("home");
        snapshot_live_home(&live, &home).unwrap();
        let view = load_legacy_readonly(&home.join("alondra.db")).unwrap();
        let expectations = legacy_expectations(&home, &view, 0).unwrap();
        let reference = content_digests(&home).unwrap();
        let backup = root.join("backup");
        let wait = crate::domain_cutover::LockWait { attempts: 1, interval: Duration::ZERO };
        let cutover = crate::domain_cutover::cutover_offline(&home, &backup, &live, &[&legacy], &wait, |_| ()).unwrap();
        let imported = serde_json::to_value(cutover.imported.unwrap()).unwrap();
        (home, backup, reference, expectations, imported)
    }

    #[test]
    fn live_shaped_cutover_passes_count_projection_and_rollback_checks() {
        let root = root("checks");
        let (home, backup, reference, expectations, imported) = cut_over_live_shaped(&root);
        let state = DomainRuntime::open(&home).unwrap().state().unwrap();
        let mut report = Report::default();
        count_checks(&mut report, &expectations, &imported, &state, &home, 0);
        delivery_checks(&mut report, &expectations, &imported, &state);
        projection_checks(&mut report, &state, &imported);
        assert!(report.passed(), "{}", report.text());
        assert!(report.checks.iter().any(|check| check.name == "delivery cards" && check.detail == "legacy 2, v2 2"));

        let mut broken = imported.clone();
        broken["derived_pr_tasks"] = json!(imported["derived_pr_tasks"].as_u64().unwrap() + 1);
        let mut report = Report::default();
        count_checks(&mut report, &expectations, &broken, &state, &home, 0);
        assert!(!report.passed());
        assert!(report.text().contains("Tasks in flight + derived PR Tasks"), "{}", report.text());

        let wait = crate::domain_cutover::LockWait { attempts: 1, interval: Duration::ZERO };
        crate::domain_cutover::rollback_offline(&home, &backup, &wait, |_| ()).unwrap();
        assert_eq!(home_differences(&reference, &home).unwrap(), Vec::<String>::new());
        Connection::open(home.join("alondra.db")).unwrap().execute("UPDATE snapshots SET at=at+1", []).unwrap();
        fs::remove_file(home.join("config.toml")).ok();
        let differences = home_differences(&reference, &home).unwrap();
        assert!(differences.contains(&"alondra.db changed".to_owned()), "{differences:?}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn projection_checks_catch_a_grouping_that_breaks_its_rules() {
        let root = root("projection");
        let (home, _, _, _, imported) = cut_over_live_shaped(&root);
        let mut state = DomainRuntime::open(&home).unwrap().state().unwrap();
        let unread = state.interactions.values().filter(|i| is_unread_information(i)).count();
        let read = state
            .interactions
            .values_mut()
            .find(|interaction| matches!(&interaction.payload, d::InteractionPayload::Information { read_by } if !read_by.is_empty())
                && interaction.recipient == d::Recipient::User)
            .unwrap();
        read.payload = d::InteractionPayload::Information { read_by: BTreeSet::new() };
        let mut report = Report::default();
        projection_checks(&mut report, &state, &imported);
        assert!(!report.passed(), "an extra unread card must exceed the historical open questions ({unread})");
        assert!(report.text().contains("only unanswerable questions, unread reports and uncertain deliveries arrive unread"), "{}", report.text());
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn signed_reads_match_the_store_and_commands_return_receipts() {
        let root = root("reads");
        let (home, _, _, _, _) = cut_over_live_shaped(&root);
        fs::create_dir(root.join("projects")).unwrap();
        fs::write(home.join("options.toml"), format!("projects_root = \"{}\"\n", root.join("projects").display())).unwrap();
        let (signer, identity) = Signer::for_home(&home).unwrap();
        assert_eq!(identity, "rehearsal device");
        let epoch = DomainRuntime::open(&home).unwrap().state().unwrap().data_epoch.0;
        let state = crate::domain_bootstrap::V2Only::open(&home, &epoch).unwrap();
        let expected = state.domain().state().unwrap();
        let auth = std::sync::Arc::new(crate::auth::Auth::open(&home).unwrap());
        let app = crate::domain_bootstrap::router(state)
            .layer(axum::middleware::from_fn_with_state(auth, crate::auth::guard));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
        });
        let mut report = Report::default();
        read_checks(&mut report, "fixture", &base, &signer, &expected).await.unwrap();
        let reads: Vec<_> = report.checks.iter().filter(|check| !check.name.contains("under")).collect();
        assert!(reads.iter().all(|check| check.ok), "{}", report.text());
        assert!(report.checks.iter().any(|check| check.name.contains("served projection")));
        let mut report = Report::default();
        let revision = command_checks(&mut report, "fixture", &base, &signer, &expected).await.unwrap();
        server.abort();
        let commands: Vec<_> = report.checks.iter().filter(|check| !check.name.contains("under")).collect();
        assert!(commands.iter().all(|check| check.ok), "{}", report.text());
        assert!(commands.len() > 3, "{}", report.text());
        assert!(revision > expected.revision);
        fs::remove_dir_all(root).unwrap();
    }
}
