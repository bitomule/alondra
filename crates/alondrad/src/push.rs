use std::collections::HashMap;
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

/// A PR a task put up for review waits this long for the node's report before it is announced
/// on its own, so one PR makes one notification.
const PR_GRACE_MS: i64 = 2 * 60 * 1000;

/// What the push loop has already looked at.
#[derive(Default)]
struct Seen {
    /// Per task, how many of its moves.
    moves: HashMap<String, usize>,
    last_q: u64,
    last_r: u64,
    /// PR cards a task just put up for review, by when: the first report inside one announces
    /// it, or it is announced on its own once `PR_GRACE_MS` passes.
    waiting: HashMap<u64, i64>,
}

impl Seen {
    fn all_of(v: &View) -> Self {
        Seen {
            moves: v.board.tasks.values().map(|t| (t.id.clone(), t.history.len())).collect(),
            last_q: v.questions.keys().next_back().copied().unwrap_or(0),
            last_r: v.reports.keys().next_back().copied().unwrap_or(0),
            waiting: HashMap::new(),
        }
    }
}

/// What a round pushes, each with whether it sounds. They come from the tasks: one that needs
/// David pushes its blocker, and a PR it puts up for review pushes the PR, again each time the
/// node answers or pushes on it; a document pushes when it arrives. A task finishing, and its
/// notices, are silent. Cards of no task (the orchestrator's) push as they always did.
fn pushes(v: &View, seen: &mut Seen, now: i64, badge: usize) -> Vec<(Value, bool)> {
    let awake = |session: &str| v.nodes.get(session).is_none_or(|n| n.alive && !alondra_core::states::watching(v, n, now));
    let mut out = vec![];
    for t in v.board.tasks.values() {
        let from = seen.moves.insert(t.id.clone(), t.history.len()).unwrap_or(0).min(t.history.len());
        for m in &t.history[from..] {
            match (&m.event, m.to) {
                (TaskEvent::BlockerRaised { blocker, .. }, TaskState::NeedsYou) => {
                    if let Some(q) = v.questions.get(&blocker.question).filter(|q| q.status.is_open() && !q.to_orchestrator && awake(&q.session_id)) {
                        out.push((question_payload(q, badge), true));
                    }
                }
                (TaskEvent::PrOpened { pr }, TaskState::InReview) => {
                    seen.waiting.insert(*pr, now);
                }
                (TaskEvent::NodeUpdated { on }, TaskState::InReview) if m.from != TaskState::InReview => {
                    let report = match on {
                        Item::Report(id) => v.reports.get(id),
                        Item::Pr(_) | Item::Message(_) | Item::Terminal => None,
                    };
                    let pr = match on {
                        Item::Pr(id) => Some(*id),
                        _ => report.and_then(|r| v.report_pr(r)),
                    };
                    if let Some(pr) = pr.and_then(|id| v.prs.get(&id)).filter(|p| awake(&p.session_id)) {
                        seen.waiting.remove(&pr.id);
                        out.push((pr_payload(pr, report, badge), false));
                    }
                }
                _ => {}
            }
        }
    }
    for r in v.reports.range(seen.last_r + 1..).map(|(_, r)| r).filter(|r| r.kind != ReportKind::Progress && !r.read && !r.to_orchestrator && awake(&r.session_id)) {
        let home = v.report_pr(r).and_then(|id| v.prs.get(&id));
        if v.report_task(r).is_none() {
            out.push(match home {
                Some(pr) => (pr_payload(pr, Some(r), badge), false),
                None => (report_payload(r, badge), false),
            });
        } else if let Some(pr) = home.filter(|p| seen.waiting.remove(&p.id).is_some()) {
            out.push((pr_payload(pr, Some(r), badge), false));
        } else if r.kind == ReportKind::Document || v.awaits_review(r) {
            out.push((report_payload(r, badge), false));
        }
    }
    for q in v.questions.range(seen.last_q + 1..).map(|(_, q)| q).filter(|q| q.status.is_open() && !q.to_orchestrator && awake(&q.session_id)) {
        if v.question_task(q).is_none() {
            out.push((question_payload(q, badge), true));
        }
    }
    let due: Vec<u64> = seen.waiting.iter().filter(|(_, since)| now - **since >= PR_GRACE_MS).map(|(id, _)| *id).collect();
    for id in due {
        let Some(pr) = v.prs.get(&id) else {
            seen.waiting.remove(&id);
            continue;
        };
        if !pr.fetched {
            continue;
        }
        seen.waiting.remove(&id);
        if crate::prs::in_inbox(v).any(|p| p.id == id) && awake(&pr.session_id) {
            out.push((pr_payload(pr, None, badge), false));
        }
    }
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

    fn recorded() -> View {
        let mut v = View::default();
        v.record(0, &Event::TasksRecorded);
        v
    }

    fn report(id: u64, sid: &str, kind: ReportKind, url: Option<&str>) -> Report {
        Report {
            related: vec![], id, session_id: sid.into(), node_name: Some("alondra/x".into()), title: format!("Report {id}"), body: None,
            links: url.map(|u| vec![alondra_core::Link { url: u.into(), title: None }]).unwrap_or_default(),
            attachments: vec![], kind, created_at: 0, read: false, to_orchestrator: false, mission: None, looks_like_question: false, task: None,
        }
    }

    fn shape(p: &(Value, bool)) -> (String, u64, bool) {
        (p.0["alondra"]["kind"].as_str().unwrap().to_string(), p.0["alondra"]["id"].as_u64().unwrap(), p.1)
    }

    fn fetched(v: &mut View, id: u64, f: impl FnOnce(&mut alondra_core::pr::PullRequest)) {
        let mut pr = v.prs[&id].clone();
        (pr.fetched, pr.title) = (true, format!("Title {id}"));
        f(&mut pr);
        v.record(0, &Event::PrUpdated(pr));
    }

    /// PR #66, 24 Sep: the PR reached David's phone as the node's "Finished" report, and tapping
    /// it opened the report, not the PR.
    #[test]
    fn a_pr_is_announced_as_a_pr_by_its_report_or_on_its_own_after_the_grace() {
        let min = 60_000;
        let mut v = recorded();
        v.record(0, &Event::Hook(alondra_core::HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), ..Default::default() }));
        let mut seen = Seen::all_of(&v);
        for (id, url) in [(1, "https://github.com/a/b/pull/66"), (2, "https://github.com/a/b/pull/67")] {
            v.record(0, &Event::PrOpened { url: url.into(), session_id: "s".into(), task: None });
            fetched(&mut v, id, |_| {});
        }
        v.record(0, &Event::ReportPosted(report(5, "s", ReportKind::Final, Some("https://github.com/a/b/pull/66"))));
        let first = pushes(&v, &mut seen, min, 1);
        assert_eq!(first.iter().map(shape).collect::<Vec<_>>(), [("pr".to_string(), 1, false)]);
        assert_eq!(first[0].0["aps"]["alert"]["title-loc-key"], PR_READY_KEY);
        assert_eq!(first[0].0["aps"]["alert"]["title-loc-args"], json!(["66"]));
        assert_eq!(first[0].0["aps"]["alert"]["body"], "Report 5 · alondra/x", "the report says what is in it");

        let later = pushes(&v, &mut seen, min + PR_GRACE_MS, 1);
        assert_eq!(later.iter().map(shape).collect::<Vec<_>>(), [("pr".to_string(), 2, false)], "#67 got no report: announced on its own, #66 not again");
        assert!(pushes(&v, &mut seen, min + 2 * PR_GRACE_MS, 1).is_empty(), "once");
    }

    #[test]
    fn pushes_come_from_what_the_tasks_do() {
        let hook = |sid: &str| Event::Hook(alondra_core::HookEvent { session_id: sid.into(), event: "UserPromptSubmit".into(), ..Default::default() });
        let question = |id: u64, sid: &str| Question {
            id, session_id: sid.into(), node_name: None, title: format!("Q{id}"), detail: None, options: vec![], links: vec![], attachments: vec![],
            blocking: true, origin: QuestionOrigin::Ask, tool_use_id: None, to_orchestrator: false, node_context: None,
            status: alondra_core::QuestionStatus::Open, answer: None, asked_at: 0, action: false, steps: vec![], mission: None, related: vec![], task: None, replaces: None,
        };
        let mut v = recorded();
        v.record(0, &hook("s"));
        v.record(0, &Event::MarkedOrchestrator { session_id: "o".into() });
        let mut seen = Seen::all_of(&v);
        let round = |v: &View, seen: &mut Seen| pushes(v, seen, 1, 1).iter().map(shape).collect::<Vec<_>>();

        v.record(0, &Event::QuestionAsked(question(1, "s")));
        v.record(0, &Event::QuestionAsked(question(2, "s")));
        assert_eq!(round(&v, &mut seen), [("question".into(), 1, true)], "the task needs David: its one blocker");
        let answer = alondra_core::Answer { text: "Sí".into(), option: None, by: None, device: None, at: 0 };
        v.record(0, &Event::QuestionAnswered { id: 1, answer });
        assert_eq!(round(&v, &mut seen), [("question".into(), 2, true)], "the one that waited is next");
        v.record(0, &Event::QuestionClosed { id: 2, status: alondra_core::QuestionStatus::Withdrawn, note: None });

        v.record(0, &Event::ReportPosted(report(1, "s", ReportKind::Notice, None)));
        v.record(0, &Event::ReportPosted(report(2, "s", ReportKind::Final, None)));
        assert!(round(&v, &mut seen).is_empty(), "notices and a finished task are silent");
        v.record(0, &Event::MessageQueued(alondra_core::Message { id: 1, session_id: "s".into(), from: alondra_core::MessageFrom::User, text: "one more".into(), answers_question: None, created_at: 0, handed_at: None, delivered_at: None, attachments: vec![], about: None }));
        v.record(0, &Event::MessageHanded { id: 1 });
        v.record(0, &Event::ReportPosted(report(3, "s", ReportKind::Document, None)));
        assert_eq!(round(&v, &mut seen), [("report".into(), 3, false)], "a document to read");

        v.record(0, &Event::PrOpened { url: "https://github.com/a/b/pull/7".into(), session_id: "s".into(), task: None });
        fetched(&mut v, 1, |p| p.head_sha = Some("a1".into()));
        assert!(round(&v, &mut seen).is_empty(), "the PR waits for the node's report");
        v.record(0, &Event::PrChangesRequested { id: 1 });
        fetched(&mut v, 1, |p| p.head_sha = Some("b2".into()));
        assert_eq!(round(&v, &mut seen), [("pr".into(), 1, false)], "the node pushed: David is told again");

        v.record(0, &Event::QuestionAsked(question(3, "o")));
        assert_eq!(round(&v, &mut seen), [("question".into(), 3, true)], "the orchestrator's questions work on no task and push as always");
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
