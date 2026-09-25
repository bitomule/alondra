use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alondra_core::tasks::{Item, TaskEvent, TaskState};
use alondra_core::{Question, QuestionOrigin, Report, ReportKind, View, now_millis};
use anyhow::{Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::hub::Hub;

const JWT_LIFETIME_MS: i64 = 50 * 60 * 1000;
const POLL: Duration = Duration::from_secs(3);

#[derive(Deserialize)]
struct ApnsConfig {
    key_path: PathBuf,
    key_id: String,
    team_id: String,
    topic: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Registration {
    pub token: String,
    pub environment: String,
    #[serde(default)]
    pub quiet_start: Option<String>,
    #[serde(default)]
    pub quiet_end: Option<String>,
    pub updated_at: i64,
}

pub struct Push {
    path: PathBuf,
    registrations: Mutex<HashMap<String, Registration>>,
    apns: Option<Apns>,
}

struct Apns {
    key: SigningKey,
    key_id: String,
    team_id: String,
    topic: String,
    client: reqwest::Client,
    jwt: Mutex<Option<(String, i64)>>,
}

impl Push {
    pub fn open(home: &Path) -> Result<Self> {
        let path = home.join("push.json");
        let registrations = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let apns = match std::fs::read_to_string(home.join("apns.json")) {
            Ok(text) => Some(Apns::load(serde_json::from_str(&text).context("apns.json ilegible")?)?),
            Err(_) => None,
        };
        if apns.is_none() {
            log!("no ~/.alondra/apns.json, notifications are not sent");
        }
        Ok(Push { path, registrations: Mutex::new(registrations), apns })
    }

    pub fn register(&self, device: &str, reg: Registration) -> Result<()> {
        let mut regs = self.registrations.lock().expect("push lock poisoned");
        regs.retain(|d, r| d == device || r.token != reg.token);
        regs.insert(device.to_string(), reg);
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&*regs)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn enabled(&self) -> bool {
        self.apns.is_some()
    }

    fn registrations(&self) -> Vec<Registration> {
        self.registrations.lock().expect("push lock poisoned").values().cloned().collect()
    }

    fn forget_token(&self, token: &str) {
        let mut regs = self.registrations.lock().expect("push lock poisoned");
        regs.retain(|_, r| r.token != token);
    }

    pub async fn check(&self) -> Result<(u16, String)> {
        let apns = self.apns.as_ref().context("APNs not configured")?;
        apns.send(&"0".repeat(64), "sandbox", &json!({"aps": {"alert": "check"}}), 10).await
    }

    pub async fn broadcast(&self, payload: &Value, with_sound: bool) {
        let Some(apns) = &self.apns else { return };
        for reg in self.registrations() {
            let mut body = payload.clone();
            let quiet = in_quiet_hours(reg.quiet_start.as_deref(), reg.quiet_end.as_deref(), local_minutes());
            let level = if quiet { "passive" } else { "active" };
            body["aps"]["interruption-level"] = json!(level);
            if (quiet || !with_sound)
                && let Some(aps) = body["aps"].as_object_mut()
            {
                aps.remove("sound");
            }
            let priority = if quiet { 5 } else { 10 };
            match apns.send(&reg.token, &reg.environment, &body, priority).await {
                Ok((200, _)) => {}
                Ok((410, _)) | Ok((400, _)) => {
                    log!("APNs rejects a token, forgetting it");
                    self.forget_token(&reg.token);
                }
                Ok((status, text)) => log!("APNs {status}: {text}"),
                Err(e) => log!("APNs does not respond: {e}"),
            }
        }
    }
}

impl Apns {
    fn load(cfg: ApnsConfig) -> Result<Self> {
        let pem = std::fs::read_to_string(&cfg.key_path).with_context(|| format!("cannot read {}", cfg.key_path.display()))?;
        let key = SigningKey::from_pkcs8_pem(&pem).map_err(|e| anyhow::anyhow!("invalid .p8 key: {e}"))?;
        let client = reqwest::Client::builder().http2_prior_knowledge().timeout(Duration::from_secs(15)).build()?;
        Ok(Apns { key, key_id: cfg.key_id, team_id: cfg.team_id, topic: cfg.topic, client, jwt: Mutex::new(None) })
    }

    fn jwt(&self) -> String {
        let mut cached = self.jwt.lock().expect("jwt lock poisoned");
        if let Some((token, at)) = cached.as_ref()
            && now_millis() - at < JWT_LIFETIME_MS
        {
            return token.clone();
        }
        let header = B64URL.encode(json!({"alg": "ES256", "kid": self.key_id}).to_string());
        let claims = B64URL.encode(json!({"iss": self.team_id, "iat": now_millis() / 1000}).to_string());
        let unsigned = format!("{header}.{claims}");
        let signature: Signature = self.key.sign(unsigned.as_bytes());
        let token = format!("{unsigned}.{}", B64URL.encode(signature.to_bytes()));
        *cached = Some((token.clone(), now_millis()));
        token
    }

    async fn send(&self, token: &str, environment: &str, body: &Value, priority: u8) -> Result<(u16, String)> {
        let host = if environment == "production" { "api.push.apple.com" } else { "api.sandbox.push.apple.com" };
        let resp = self
            .client
            .post(format!("https://{host}/3/device/{token}"))
            .bearer_auth(self.jwt())
            .header("apns-topic", &self.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", priority.to_string())
            .json(body)
            .send()
            .await?;
        let status = resp.status().as_u16();
        Ok((status, resp.text().await.unwrap_or_default()))
    }
}

fn local_minutes() -> u32 {
    let t: libc::time_t = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    (tm.tm_hour * 60 + tm.tm_min) as u32
}

fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.split_once(':')?;
    Some(h.parse::<u32>().ok()? * 60 + m.parse::<u32>().ok()?)
}

pub fn in_quiet_hours(start: Option<&str>, end: Option<&str>, now: u32) -> bool {
    let (Some(s), Some(e)) = (start.and_then(parse_hhmm), end.and_then(parse_hhmm)) else { return false };
    if s <= e { now >= s && now < e } else { now >= s || now < e }
}

fn question_payload(q: &Question, badge: usize) -> Value {
    let category = match q.origin {
        QuestionOrigin::Permission => "ALONDRA_PERMISSION",
        QuestionOrigin::Plan => "ALONDRA_PLAN",
        _ => "ALONDRA_QUESTION",
    };
    let origin = match q.origin {
        QuestionOrigin::Ask => "ask",
        QuestionOrigin::AskUserQuestion => "ask_user_question",
        QuestionOrigin::Permission => "permission",
        QuestionOrigin::Plan => "plan",
        QuestionOrigin::Classifier => "classifier",
        QuestionOrigin::Elicitation => "elicitation",
    };
    let who = q.node_name.clone().unwrap_or_else(|| "a node".into());
    let options = q.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>().join(" · ");
    let body = if options.is_empty() { who.clone() } else { format!("{who} · {options}") };
    json!({
        "aps": {"alert": {"title": q.title, "body": body}, "sound": "default", "category": category,
                "thread-id": q.session_id, "badge": badge, "mutable-content": 1},
        "alondra": {"kind": "question", "id": q.id, "origin": origin,
                    "options": q.options.iter().map(|o| o.label.clone()).collect::<Vec<_>>()}
    })
}

fn report_payload(r: &Report, badge: usize) -> Value {
    let kind = r.kind.label();
    let who = r.node_name.clone().unwrap_or_else(|| "a node".into());
    json!({
        "aps": {"alert": {"title": r.title, "body": format!("{kind} · {who}")}, "category": "ALONDRA_READ",
                "thread-id": r.session_id, "badge": badge},
        "alondra": {"kind": "report", "id": r.id, "report_kind": kind_key(r.kind)}
    })
}

/// The iPhone translates the title from this key in the app's string catalog.
const PR_READY_KEY: &str = "PR #%@ ready to review";

/// A PR card ready for the user, announced once: by the first report inside it, or on its own
/// when no report comes within `PR_GRACE_MS`. Tapping it opens the PR card.
fn pr_payload(pr: &alondra_core::pr::PullRequest, report: Option<&Report>, badge: usize) -> Value {
    let who = report.and_then(|r| r.node_name.clone()).or_else(|| pr.node_name.clone()).unwrap_or_else(|| "a node".into());
    let what = report.map_or(pr.title.as_str(), |r| r.title.as_str());
    json!({
        "aps": {"alert": {"title": format!("PR #{} ready to review", pr.number), "title-loc-key": PR_READY_KEY,
                          "title-loc-args": [pr.number.to_string()], "body": format!("{what} · {who}")},
                "sound": "default", "thread-id": pr.session_id, "badge": badge},
        "alondra": {"kind": "pr", "id": pr.id}
    })
}

/// A PR card opened without a report waits this long for the node's report before it is
/// announced on its own, so one PR makes one notification.
const PR_GRACE_MS: i64 = 2 * 60 * 1000;

/// What the push loop has already looked at.
struct Seen {
    last_q: u64,
    last_r: u64,
    /// PR cards already announced.
    announced: HashSet<u64>,
    /// Per task, how many of its moves.
    moves: HashMap<String, usize>,
}

impl Seen {
    fn all_of(v: &View) -> Self {
        Seen {
            last_q: v.questions.keys().next_back().copied().unwrap_or(0),
            last_r: v.reports.keys().next_back().copied().unwrap_or(0),
            announced: v.prs.keys().copied().collect(),
            moves: v.board.tasks.values().map(|t| (t.id.clone(), t.history.len())).collect(),
        }
    }
}

/// A final report of a task whose PR David merged, or whose document he approved, before it:
/// he already acted on what it reports.
fn after_davids_merge(v: &View, r: &Report) -> bool {
    let Some(t) = v.report_task(r).filter(|_| r.kind == ReportKind::Final) else { return false };
    let finished = t.history.iter().position(|h| h.to == TaskState::Done && matches!(h.event, TaskEvent::FinalReport { report } if report == r.id));
    let merged = t.history.iter().position(|h| matches!(h.event, TaskEvent::Merged { .. }));
    matches!((merged, finished), (Some(m), Some(f)) if m < f)
}

/// PR cards whose task went back to review since the last round because the node pushed new
/// commits after David asked for changes: David is told again.
fn back_in_review(v: &View, moves: &mut HashMap<String, usize>) -> Vec<u64> {
    moves.retain(|id, _| v.board.tasks.contains_key(id));
    let mut out = vec![];
    for t in v.board.tasks.values() {
        let from = moves.insert(t.id.clone(), t.history.len()).unwrap_or(0).min(t.history.len());
        for m in &t.history[from..] {
            if let TaskEvent::NodeUpdated { on: Item::Pr(pr) } = m.event
                && m.to == TaskState::InReview
                && m.from != TaskState::InReview
            {
                out.push(pr);
            }
        }
    }
    out
}

/// What the new reports and PR cards push: a report inside an open PR card announces that PR;
/// any other report pushes as a report; a PR card nobody reported on is announced once its
/// grace passes, and again when the node pushes after David asked for changes, unless a report
/// of it just did.
fn announcements(v: &View, reports: &[Report], announced: &mut HashSet<u64>, now: i64, badge: usize, back: &[u64]) -> Vec<Value> {
    let mut out = vec![];
    let mut by_report = HashSet::new();
    for r in reports {
        match v.report_pr(r).and_then(|id| v.prs.get(&id)) {
            Some(pr) => {
                announced.insert(pr.id);
                by_report.insert(pr.id);
                out.push(pr_payload(pr, Some(r), badge));
            }
            None => out.push(report_payload(r, badge)),
        }
    }
    for pr in back.iter().filter(|id| !by_report.contains(*id)).filter_map(|id| crate::prs::in_inbox(v).find(|p| p.id == *id)) {
        out.push(pr_payload(pr, None, badge));
    }
    for pr in crate::prs::in_inbox(v).filter(|p| p.fetched && now - p.opened_at >= PR_GRACE_MS) {
        if announced.insert(pr.id) {
            out.push(pr_payload(pr, None, badge));
        }
    }
    out
}

/// A round's pushes, each with whether it sounds: every new open question for David, and what
/// `announcements` makes of the new reports. The one push the task model drops is the final
/// report of a task David already merged or approved.
fn pushes(v: &View, seen: &mut Seen, now: i64, badge: usize) -> Vec<(Value, bool)> {
    let awake = |session: &str| v.nodes.get(session).is_none_or(|n| n.alive && !alondra_core::states::watching(v, n, now));
    let questions = v.questions.range(seen.last_q + 1..).map(|(_, q)| q).filter(|q| q.status.is_open() && !q.to_orchestrator && awake(&q.session_id));
    let mut out: Vec<(Value, bool)> = questions.map(|q| (question_payload(q, badge), true)).collect();
    let reports: Vec<Report> = v
        .reports
        .range(seen.last_r + 1..)
        .map(|(_, r)| r)
        .filter(|r| r.kind != ReportKind::Progress && !r.read && awake(&r.session_id) && !after_davids_merge(v, r))
        .cloned()
        .collect();
    let back: Vec<u64> = back_in_review(v, &mut seen.moves).into_iter().filter(|id| v.prs.get(id).is_some_and(|p| awake(&p.session_id))).collect();
    out.extend(announcements(v, &reports, &mut seen.announced, now, badge, &back).into_iter().map(|p| (p, false)));
    seen.last_q = seen.last_q.max(v.questions.keys().next_back().copied().unwrap_or(0));
    seen.last_r = seen.last_r.max(v.reports.keys().next_back().copied().unwrap_or(0));
    out
}

fn kind_key(k: ReportKind) -> &'static str {
    match k {
        ReportKind::Notice => "notice",
        ReportKind::Document => "document",
        ReportKind::Progress => "progress",
        ReportKind::Final => "final",
    }
}

/// After the user last did something in a client, pushes stay quiet for this long: whatever
/// arrives meanwhile is already on the screen they are using.
pub const ACTIVE_WINDOW_MS: i64 = 5 * 60 * 1000;

fn in_a_client(activity: &std::collections::HashMap<String, i64>, now: i64) -> bool {
    activity.values().any(|at| now - at <= ACTIVE_WINDOW_MS)
}

pub async fn run(hub: Arc<Hub>, push: Arc<Push>) {
    if !push.enabled() {
        return;
    }
    let mut seen = hub.read(Seen::all_of);
    loop {
        hub.wait_event(POLL).await;
        let now = alondra_core::now_millis();
        let payloads = hub.read(|v| {
            let badge = v.questions.values().filter(|q| q.status.is_open() && !q.to_orchestrator).count()
                + v.reports.values().filter(|r| !r.read && r.kind != ReportKind::Progress).count();
            pushes(v, &mut seen, now, badge)
        });
        if in_a_client(&hub.activity(), now) {
            continue;
        }
        for (payload, sound) in payloads {
            push.broadcast(&payload, sound).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::Event;

    #[test]
    fn using_any_client_in_the_last_5_minutes_silences_pushes() {
        let min = 60_000;
        let now = 100 * min;
        let cases: Vec<(&str, Vec<(&str, i64)>, bool)> = vec![
            ("no client ever used", vec![], false),
            ("the TUI a minute ago", vec![("tui", now - min)], true),
            ("the Mac 4 minutes ago", vec![("mac", now - 4 * min)], true),
            ("the Mac 6 minutes ago", vec![("mac", now - 6 * min)], false),
            ("the TUI long ago, the Mac just now", vec![("tui", now - 60 * min), ("mac", now)], true),
        ];
        for (what, used, silent) in cases {
            let activity = used.into_iter().map(|(c, at)| (c.to_string(), at)).collect();
            assert_eq!(in_a_client(&activity, now), silent, "{what}");
        }
    }

    fn report(id: u64, sid: &str, kind: ReportKind, url: Option<&str>) -> Report {
        Report {
            id, session_id: sid.into(), node_name: Some("alondra/x".into()), title: format!("Report {id}"), body: None,
            links: url.map(|u| vec![alondra_core::Link { url: u.into(), title: None }]).unwrap_or_default(),
            attachments: vec![], kind, created_at: 0, read: false, to_orchestrator: false, mission: None, looks_like_question: false, task: None,
        }
    }

    fn question(id: u64, sid: &str, blocking: bool) -> Question {
        Question {
            id, session_id: sid.into(), node_name: None, title: format!("Q{id}"), detail: None, options: vec![], links: vec![], attachments: vec![],
            blocking, origin: QuestionOrigin::Ask, tool_use_id: None, to_orchestrator: false, node_context: None,
            status: alondra_core::QuestionStatus::Open, answer: None, asked_at: 0, action: false, steps: vec![], mission: None, task: None, replaces: None, about_pr: None,
        }
    }

    fn shape(p: &(Value, bool)) -> (String, u64, bool) {
        (p.0["alondra"]["kind"].as_str().unwrap().to_string(), p.0["alondra"]["id"].as_u64().unwrap(), p.1)
    }

    fn started() -> View {
        let mut v = View::default();
        v.record(0, &Event::TasksRecorded);
        v.record(0, &Event::Hook(alondra_core::HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), ..Default::default() }));
        v
    }

    fn fetched(v: &mut View, id: u64, f: impl FnOnce(&mut alondra_core::pr::PullRequest)) {
        let mut pr = v.prs[&id].clone();
        (pr.fetched, pr.title) = (true, format!("Title {id}"));
        f(&mut pr);
        v.record(0, &Event::PrUpdated(pr));
    }

    fn round(v: &View, seen: &mut Seen) -> Vec<(String, u64, bool)> {
        pushes(v, seen, 1, 1).iter().map(shape).collect()
    }

    /// PR #66, 24 Sep: the PR reached David's phone as the node's "Finished" report, and tapping
    /// it opened the report, not the PR.
    #[test]
    fn a_pr_is_announced_as_a_pr_by_its_report_or_on_its_own_after_the_grace() {
        let min = 60_000;
        let mut v = View::default();
        for (seq, url) in [(1, "https://github.com/a/b/pull/66"), (2, "https://github.com/c/d/pull/67")] {
            v.apply(seq + 20, 0, &Event::Hook(alondra_core::HookEvent { session_id: format!("s{seq}"), event: "UserPromptSubmit".into(), ..Default::default() }));
            v.apply(seq, 0, &Event::PrOpened { url: url.into(), session_id: format!("s{seq}"), task: None });
            let mut pr = v.prs[&seq].clone();
            (pr.fetched, pr.title) = (true, format!("Title {seq}"));
            v.apply(seq + 10, 0, &Event::PrUpdated(pr));
        }
        let shape = |p: &Value| (p["alondra"]["kind"].as_str().unwrap().to_string(), p["alondra"]["id"].as_u64().unwrap());
        let mut announced = HashSet::new();

        let first = announcements(&v, &[report(5, "s1", ReportKind::Final, Some("https://github.com/a/b/pull/66")), report(6, "t", ReportKind::Final, None)], &mut announced, min, 1, &[]);
        assert_eq!(first.iter().map(shape).collect::<Vec<_>>(), [("pr".to_string(), 1), ("report".to_string(), 6)]);
        assert_eq!(first[0]["aps"]["alert"]["title-loc-key"], PR_READY_KEY);
        assert_eq!(first[0]["aps"]["alert"]["title-loc-args"], json!(["66"]));
        assert_eq!(first[0]["aps"]["alert"]["body"], "Report 5 · alondra/x", "the report says what is in it");

        let later = announcements(&v, &[], &mut announced, PR_GRACE_MS, 1, &[]);
        assert_eq!(later.iter().map(shape).collect::<Vec<_>>(), [("pr".to_string(), 2)], "#67 never got a report: announced on its own, #66 not again");
        assert!(announcements(&v, &[], &mut announced, 2 * PR_GRACE_MS, 1, &[]).is_empty(), "once");
    }

    #[test]
    fn every_question_notice_document_and_final_report_pushes_as_before() {
        let mut v = started();
        let mut seen = Seen::all_of(&v);
        v.record(0, &Event::QuestionAsked(question(1, "s", true)));
        v.record(0, &Event::QuestionAsked(question(2, "s", false)));
        assert_eq!(round(&v, &mut seen), [("question".into(), 1, true), ("question".into(), 2, true)], "a question that blocks nothing pushes too");
        let answer = alondra_core::Answer { text: "Sí".into(), option: None, by: None, device: None, at: 0 };
        v.record(0, &Event::QuestionAnswered { id: 1, answer });
        for (id, kind) in [(1, ReportKind::Notice), (2, ReportKind::Document), (3, ReportKind::Final)] {
            v.record(0, &Event::ReportPosted(report(id, "s", kind, None)));
        }
        assert_eq!(round(&v, &mut seen), [("report".into(), 1, false), ("report".into(), 2, false), ("report".into(), 3, false)], "a notice, a document and a final report push");
    }

    /// 23-24 Sep: nodes reported final again after David merged their PR; he had already acted.
    #[test]
    fn a_final_report_after_david_merged_the_tasks_pr_is_silent() {
        let mut v = started();
        v.record(0, &Event::PrOpened { url: "https://github.com/a/b/pull/7".into(), session_id: "s".into(), task: None });
        fetched(&mut v, 1, |_| {});
        v.record(0, &Event::PrMerged { id: 1, method: alondra_core::pr::MergeMethod::Squash });
        let mut seen = Seen::all_of(&v);
        v.record(0, &Event::ReportPosted(report(1, "s", ReportKind::Notice, None)));
        v.record(0, &Event::ReportPosted(report(2, "s", ReportKind::Final, None)));
        assert_eq!(round(&v, &mut seen), [("report".into(), 1, false)], "the notice pushes, the final report after the merge does not");

        let mut auto = View::default();
        auto.record(0, &Event::TasksRecorded);
        auto.record(0, &Event::Launched { name: "app/fix".into(), request_id: None, auto_merge: true, task: None, title: None, session: None });
        auto.record(0, &Event::DaemonSeen(alondra_core::DaemonObservation { session_id: "s".into(), short_id: "s".into(), name: Some("app/fix".into()), pid: Some(1), ..Default::default() }));
        auto.record(0, &Event::PrOpened { url: "https://github.com/a/b/pull/7".into(), session_id: "s".into(), task: None });
        fetched(&mut auto, 1, |p| p.state = alondra_core::pr::PrState::Merged);
        let mut seen = Seen::all_of(&auto);
        auto.record(0, &Event::ReportPosted(report(1, "s", ReportKind::Final, None)));
        assert_eq!(round(&auto, &mut seen), [("report".into(), 1, false)], "David did not merge an auto-merged PR: its final report pushes");
    }

    /// A report of a task goes into its open PR card and pushes that PR as ready, once; the node
    /// pushing new commits after changes were requested tells David again.
    #[test]
    fn a_report_in_a_pr_card_pushes_the_pr_once_and_new_commits_push_it_again() {
        let mut v = started();
        v.record(0, &Event::PrOpened { url: "https://github.com/a/b/pull/7".into(), session_id: "s".into(), task: None });
        fetched(&mut v, 1, |p| p.head_sha = Some("a1".into()));
        let mut seen = Seen::all_of(&v);
        v.record(0, &Event::ReportPosted(report(1, "s", ReportKind::Final, None)));
        assert_eq!(round(&v, &mut seen), [("pr".into(), 1, false)], "the report inside the card pushes the PR, not itself");
        assert!(pushes(&v, &mut seen, 1 + PR_GRACE_MS, 1).is_empty(), "the PR is not announced again after the grace");

        v.record(0, &Event::PrChangesRequested { id: 1 });
        fetched(&mut v, 1, |p| p.head_sha = Some("b2".into()));
        assert_eq!(round(&v, &mut seen), [("pr".into(), 1, false)], "new commits: David is told again");

        v.record(0, &Event::PrChangesRequested { id: 1 });
        fetched(&mut v, 1, |p| p.head_sha = Some("c3".into()));
        v.record(0, &Event::ReportPosted(report(2, "s", ReportKind::Notice, Some("https://github.com/a/b/pull/7"))));
        assert_eq!(round(&v, &mut seen), [("pr".into(), 1, false)], "new commits and a report about them push once");
    }

    #[test]
    fn quiet_hours_across_midnight() {
        assert!(in_quiet_hours(Some("23:00"), Some("08:00"), 23 * 60 + 30));
        assert!(in_quiet_hours(Some("23:00"), Some("08:00"), 3 * 60));
        assert!(!in_quiet_hours(Some("23:00"), Some("08:00"), 12 * 60));
        assert!(in_quiet_hours(Some("13:00"), Some("15:00"), 14 * 60));
        assert!(!in_quiet_hours(None, Some("08:00"), 3 * 60));
    }
}
