//! Sending, listing and opening reports against an in-memory GitHub that keeps the report
//! repository's issues and every user's gists, as the REST API answers them.
use std::cell::RefCell;
use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::github::{GhError, GhResult, Github, Repo, classify};
use super::send::{self, Blocked, Kind, Outcome, Public, State};
use super::tests::{FakeMac, NOW, full_home, hours_before, private_values};
use super::*;
use crate::test_support::Directory;

const REPO: &str = "owner/reports";
/// The account that owns the report repository: the only one whose labels stick.
const OWNER: &str = "owner";

#[derive(Clone)]
struct FakeGist {
    id: String,
    owner: String,
    description: String,
    files: BTreeMap<String, String>,
}

#[derive(Default)]
struct World {
    login: String,
    gists: Vec<FakeGist>,
    issues: Vec<Value>,
    /// The calls that reached GitHub.
    calls: Vec<String>,
    tries: usize,
    /// Every call after this many answers as if the network were gone.
    cut_after: Option<usize>,
    /// What every call answers instead, as `gh` would.
    failing: Option<GhError>,
}

struct FakeGithub(RefCell<World>);

impl FakeGithub {
    fn new() -> FakeGithub {
        FakeGithub(RefCell::new(World { login: OWNER.into(), ..World::default() }))
    }

    fn as_user(&self, login: &str) {
        self.0.borrow_mut().login = login.into();
    }

    fn calls(&self, prefix: &str) -> usize {
        self.0.borrow().calls.iter().filter(|c| c.starts_with(prefix)).count()
    }

    /// As GitHub answers a gist: past about a megabyte of content in all, a file comes empty, marked
    /// truncated, with the address of its whole text.
    fn gist_json(gist: &FakeGist, with_content: bool) -> Value {
        let mut answered = 0;
        let files: serde_json::Map<String, Value> = gist
            .files
            .iter()
            .map(|(name, text)| {
                let raw = format!("{}{}/{}/raw/{name}", super::github::RAW_GISTS, gist.owner, gist.id);
                answered += text.len();
                let file = match (with_content, answered <= 1_000_000) {
                    (false, _) => json!({"filename": name, "raw_url": raw}),
                    (true, true) => json!({"filename": name, "content": text, "truncated": false, "raw_url": raw}),
                    (true, false) => json!({"filename": name, "content": "", "truncated": true, "raw_url": raw}),
                };
                (name.clone(), file)
            })
            .collect();
        json!({"id": gist.id, "html_url": format!("https://gist.github.com/{}", gist.id), "description": gist.description, "public": false, "owner": {"login": gist.owner}, "files": files})
    }
}

impl Github for FakeGithub {
    fn raw(&self, url: &str) -> GhResult<String> {
        let mut world = self.0.borrow_mut();
        world.calls.push(format!("RAW {url}"));
        let rest = url.strip_prefix(super::github::RAW_GISTS).ok_or(GhError::NotFound)?;
        let parts: Vec<&str> = rest.split('/').collect();
        let [_, id, "raw", name] = parts.as_slice() else { return Err(GhError::NotFound) };
        world.gists.iter().find(|g| g.id == *id).and_then(|g| g.files.get(*name).cloned()).ok_or(GhError::NotFound)
    }

    fn api(&self, method: &str, path: &str, body: Option<&Value>) -> GhResult<Value> {
        let mut world = self.0.borrow_mut();
        world.tries += 1;
        if let Some(error) = world.failing.clone() {
            return Err(error);
        }
        if world.cut_after.is_some_and(|n| world.tries > n) {
            return Err(GhError::Unavailable("dial tcp: lookup api.github.com: no such host".into()));
        }
        world.calls.push(format!("{method} {path}"));
        let login = world.login.clone();
        let (route, query) = path.split_once('?').unwrap_or((path, ""));
        let param = |name: &str| query.split('&').find_map(|p| p.strip_prefix(&format!("{name}="))).map(str::to_owned);
        let body = body.cloned().unwrap_or(Value::Null);
        match (method, route) {
            ("GET", "user") => return Ok(json!({"login": login})),
            ("GET", "gists") => {
                let page: usize = param("page").and_then(|p| p.parse().ok()).unwrap_or(1);
                let own: Vec<Value> = world.gists.iter().rev().filter(|g| g.owner == login).skip((page - 1) * 100).take(100).map(|g| FakeGithub::gist_json(g, false)).collect();
                return Ok(Value::Array(own));
            }
            ("POST", "gists") => {
                let gist = FakeGist {
                    id: format!("{:032x}", world.gists.len() + 1),
                    owner: login,
                    description: body["description"].as_str().unwrap_or_default().into(),
                    files: body["files"].as_object().unwrap().iter().map(|(n, f)| (n.clone(), f["content"].as_str().unwrap().to_owned())).collect(),
                };
                assert_eq!(body["public"], false, "a report's gist is secret");
                world.gists.push(gist.clone());
                return Ok(FakeGithub::gist_json(&gist, true));
            }
            (_, r) if r.starts_with("gists/") => {
                let id = &r["gists/".len()..];
                let Some(at) = world.gists.iter().position(|g| g.id == id) else { return Err(GhError::NotFound) };
                let mine = world.gists[at].owner == login;
                return match method {
                    "GET" => Ok(FakeGithub::gist_json(&world.gists[at], true)),
                    "PATCH" if mine => {
                        for (name, file) in body["files"].as_object().unwrap() {
                            world.gists[at].files.insert(name.clone(), file["content"].as_str().unwrap().to_owned());
                        }
                        Ok(FakeGithub::gist_json(&world.gists[at], true))
                    }
                    "DELETE" if mine => {
                        world.gists.remove(at);
                        Ok(Value::Null)
                    }
                    _ => Err(GhError::NotFound),
                };
            }
            _ => {}
        }
        let Some(rest) = route.strip_prefix(&format!("repos/{REPO}")) else { return Err(GhError::NotFound) };
        match (method, rest.trim_start_matches('/')) {
            ("GET", "") => Ok(json!({"full_name": REPO, "private": false, "has_issues": true})),
            ("POST", "issues") => {
                let number = world.issues.len() as u64 + 1;
                let labels = if login == OWNER { body["labels"].clone() } else { json!([]) };
                let issue = json!({"number": number, "html_url": format!("https://github.com/{REPO}/issues/{number}"), "title": body["title"], "body": body["body"],
                    "labels": labels, "user": {"login": login}, "created_at": "2026-10-25T10:00:00Z", "state": "open"});
                world.issues.push(issue.clone());
                Ok(issue)
            }
            ("GET", "issues") => {
                let creator = param("creator");
                let state = param("state").unwrap_or_else(|| "open".into());
                let page: usize = param("page").and_then(|p| p.parse().ok()).unwrap_or(1);
                let picked: Vec<Value> = world
                    .issues
                    .iter()
                    .rev()
                    .filter(|i| creator.as_ref().is_none_or(|c| i["user"]["login"] == c.as_str()))
                    .filter(|i| state == "all" || i["state"] == state.as_str())
                    .skip((page - 1) * 100)
                    .take(100)
                    .cloned()
                    .collect();
                Ok(Value::Array(picked))
            }
            (method, r) if r.starts_with("issues/") => {
                let number: u64 = r["issues/".len()..].parse().unwrap();
                let Some(issue) = world.issues.iter_mut().find(|i| i["number"] == number) else { return Err(GhError::NotFound) };
                match method {
                    "GET" => Ok(issue.clone()),
                    "PATCH" if login == OWNER || issue["user"]["login"] == login.as_str() => {
                        issue["state"] = body["state"].clone();
                        Ok(issue.clone())
                    }
                    _ => Err(GhError::Refused(403, "Must have admin rights to Repository.".into())),
                }
            }
            _ => Err(GhError::Refused(400, format!("the fake GitHub does not know {method} {path}"))),
        }
    }
}

fn repo(gh: &FakeGithub) -> Repo<'_> {
    Repo { gh, name: REPO.into() }
}

fn built_report(home: &Directory) -> (FakeMac, Built) {
    full_home(home);
    let mac = FakeMac::new(home);
    let built = build(&mac, hours_before(72), Some("It stopped after the Mac slept".into()), None).unwrap();
    assert!(built.hits.is_empty(), "{:?}", built.hits);
    (mac, built)
}

fn send_it(gh: &FakeGithub, mac: &FakeMac, id: &str) -> anyhow::Result<Outcome> {
    let files = checked(mac, id)?;
    send::send(&repo(gh), mac.home(), id, &files, Some(Kind::SessionEnded), NOW, &send::backoff_ms)
}

fn issue(gh: &FakeGithub, number: u64) -> super::github::Issue {
    repo(gh).issue(number).unwrap().unwrap()
}

#[test]
fn a_sent_report_shows_its_issue_only_the_version_macos_and_failure_and_opens_into_the_same_files() {
    let home = Directory::new();
    let (mac, built) = built_report(&home);
    let gh = FakeGithub::new();
    gh.as_user("ana");
    let Outcome::Sent { number, url } = send_it(&gh, &mac, &built.id).unwrap() else { panic!("not sent") };
    assert_eq!(url, format!("https://github.com/{REPO}/issues/{number}"));
    let sent = issue(&gh, number);
    let public = format!("{}\n{}", sent.title, sent.body);
    let mut private = private_values();
    private.extend(["It stopped after the Mac slept", "Ana's MacBook Pro", "analopez-acme", "task-1", "daemon.log"].map(String::from));
    for value in &private {
        assert!(!public.to_lowercase().contains(&value.to_lowercase()), "{value:?} is in the issue:\n{public}");
    }
    let fields = ["Report", "Alondra", "macOS", "Failure", "Created", "Gist", "Parts", "SHA-256"];
    for line in sent.body.lines().skip(1).filter(|l| !l.is_empty()) {
        assert!(fields.iter().any(|f| line.starts_with(&format!("- {f}: "))) && line.len() < 120, "the issue holds more than its fields: {line:.80}");
    }
    let parsed = Public::parse(&sent.body).unwrap();
    assert_eq!((parsed.id.as_str(), parsed.macos.as_str(), parsed.kind), (built.id.as_str(), "26.4", Kind::SessionEnded));
    assert_eq!(sent.title, format!("Crash report {} · Alondra {} · macOS 26.4 · session-ended", built.id, env!("CARGO_PKG_VERSION")));
    assert_eq!(gh.0.borrow().issues[0]["labels"], json!([]), "GitHub drops the label of someone who is not the repository's");
    {
        let world = gh.0.borrow();
        assert_eq!(world.gists.len(), 1);
        let gist = &world.gists[0];
        assert_eq!((gist.owner.as_str(), gist.description.as_str()), ("ana", send::gist_description(&built.id).as_str()));
        assert_eq!(parsed.gist, format!("https://gist.github.com/{}", gist.id));
        let parts: Vec<&String> = gist.files.values().collect();
        assert_eq!(parts.len(), parsed.parts);
        assert!(parts.iter().all(|p| p.len() <= send::PART_BYTES));
        let joined: String = parts.iter().map(|p| p.as_str()).collect();
        assert_eq!(bundle::sha256(joined.as_bytes()), parsed.sha256);
        assert_eq!(joined, bundle::armored(&built.id, &built.files), "the gist holds exactly the file report save writes");
    }
    gh.as_user(OWNER);
    let reader = Directory::new();
    std::fs::create_dir_all(&*reader).unwrap();
    let (id, dir, files) = open_issue(&reader, &repo(&gh), &sent).unwrap();
    assert_eq!((id.as_str(), &files), (built.id.as_str(), &built.files));
    assert!(dir.starts_with(reader.join("reports/received")));
    let sending = send::read_sending(&built.dir).unwrap();
    assert_eq!((sending.state, sending.issue), (State::Sent, Some(number)));
    assert!(checked(&mac, &built.id).is_ok(), "sending.json does not count as a file added after the preview");
}

#[test]
fn sending_again_makes_no_second_issue_or_gist() {
    let home = Directory::new();
    let (mac, built) = built_report(&home);
    let gh = FakeGithub::new();
    gh.as_user("ana");
    let first = send_it(&gh, &mac, &built.id).unwrap();
    let puts = gh.calls("PATCH");
    assert_eq!(send_it(&gh, &mac, &built.id).unwrap(), first);
    std::fs::remove_file(built.dir.join(bundle::SENDING)).unwrap();
    assert_eq!(send_it(&gh, &mac, &built.id).unwrap(), first, "found by its id on GitHub, not only on this Mac");
    assert_eq!((gh.0.borrow().issues.len(), gh.0.borrow().gists.len()), (1, 1));
    assert_eq!((gh.calls("PATCH"), gh.calls("POST gists")), (puts, 1));
}

#[test]
fn a_send_cut_halfway_stays_pending_and_the_next_try_finishes_it() {
    let home = Directory::new();
    let (mac, built) = built_report(&home);
    let big: String = (0..250_000u64).map(|i| format!("{:016x}\n", i.wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left((i % 63) as u32))).collect();
    let mut files: Vec<(String, String)> = built.files.iter().filter(|(n, _)| n != bundle::MANIFEST).cloned().collect();
    files.retain(|(n, _)| n != "sdk-host.jsonl");
    files.push(("sdk-host.jsonl".into(), big));
    let manifest = json!({"format": "alondra.report", "format_version": 1, "id": built.id, "created_at": "2026-10-25T10:00:00.000Z", "alondra": "0.3.0",
        "sections": files.iter().filter(|(n, _)| n != bundle::DESCRIPTION).map(|(n, t)| json!({"name": n, "bytes": t.len(), "sha256": bundle::sha256(t.as_bytes())})).collect::<Vec<_>>(),
        "description": files.iter().find(|(n, _)| n == bundle::DESCRIPTION).map(|(_, t)| json!({"bytes": t.len(), "sha256": bundle::sha256(t.as_bytes())}))});
    files.insert(0, (bundle::MANIFEST.into(), manifest.to_string()));
    std::fs::remove_dir_all(&built.dir).unwrap();
    bundle::write(&built.dir, &files, &json!({})).unwrap();
    let files = bundle::read(&built.dir).unwrap();
    let gh = FakeGithub::new();
    gh.as_user("ana");
    gh.0.borrow_mut().cut_after = Some(6);
    let outcome = send::send(&repo(&gh), mac.home(), &built.id, &files, Some(Kind::Other), NOW, &send::backoff_ms).unwrap();
    assert!(matches!(outcome, Outcome::Pending(_)), "{outcome:?}");
    let pending = send::read_sending(&built.dir).unwrap();
    assert_eq!((pending.state, pending.next_at), (State::Pending, NOW + send::backoff_ms(1)));
    assert_eq!(gh.0.borrow().gists[0].files.len(), 2, "the cut came right after the second part");
    assert!(gh.0.borrow().issues.is_empty());
    gh.0.borrow_mut().cut_after = None;
    let Outcome::Sent { number, .. } = send::send(&repo(&gh), mac.home(), &built.id, &files, None, NOW + 60_000, &send::backoff_ms).unwrap() else { panic!() };
    let sent = issue(&gh, number);
    let public = Public::parse(&sent.body).unwrap();
    assert!(public.parts >= 3, "{} parts", public.parts);
    assert_eq!(public.kind, Kind::Other, "the next try keeps the kind of the first");
    assert_eq!(gh.calls("POST gists") + gh.calls("PATCH gists/"), public.parts, "no part goes up twice");
    assert!(gh.calls("RAW ") > 0, "GitHub answered the later parts only in part and they were read whole");
    assert_eq!(gh.0.borrow().gists.len(), 1);
    let reader = Directory::new();
    std::fs::create_dir_all(&*reader).unwrap();
    assert_eq!(open_issue(&reader, &repo(&gh), &sent).unwrap().2, files);
}

#[test]
fn backoff_doubles_from_a_minute_up_to_half_an_hour() {
    assert_eq!([1, 2, 3, 5, 6, 20].map(send::backoff_ms), [60_000, 120_000, 240_000, 960_000, 1_800_000, 1_800_000]);
}

#[test]
fn without_gh_or_without_the_repository_nothing_is_sent_and_it_says_why() {
    let home = Directory::new();
    let (mac, built) = built_report(&home);
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    for (failure, says) in [(GhError::SignedOut, "gh auth login"), (GhError::Missing, "gh auth login")] {
        let gh = FakeGithub::new();
        gh.0.borrow_mut().failing = Some(failure.clone());
        let error = send_it(&gh, &mac, &built.id).unwrap_err();
        assert!(error.to_string().contains(says), "{failure:?}: {error}");
        assert!(gh.0.borrow().issues.is_empty());
    }
    let gh = FakeGithub::new();
    let elsewhere = Repo { gh: &gh, name: "owner/elsewhere".into() };
    let files = checked(&mac, &built.id).unwrap();
    let error = send::send(&elsewhere, mac.home(), &built.id, &files, None, NOW, &send::backoff_ms).unwrap_err();
    assert!(matches!(error.downcast_ref::<Blocked>(), Some(Blocked::NoSuchRepo(_))), "{error}");
    assert!(error.to_string().contains("GitHub has no repository owner/elsewhere that takes issues"), "{error}");
    assert!(gh.0.borrow().issues.is_empty() && gh.0.borrow().gists.is_empty(), "nothing goes up before the repository is known");
}

#[test]
fn a_report_the_final_check_refuses_never_reaches_github() {
    let home = Directory::new();
    let (mac, built) = built_report(&home);
    let leaked = "2026-10-25T05:00:00.000Z repo acme/billing is busy\n";
    std::fs::write(built.dir.join("daemon.log"), leaked).unwrap();
    let manifest_path = built.dir.join(bundle::MANIFEST);
    let mut manifest: Value = serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    for section in manifest["sections"].as_array_mut().unwrap() {
        if section["name"] == "daemon.log" {
            section["sha256"] = json!(bundle::sha256(leaked.as_bytes()));
        }
    }
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
    let gh = FakeGithub::new();
    let error = send_it(&gh, &mac, &built.id).unwrap_err().to_string();
    assert!(error.contains("daemon.log, line 1"), "{error}");
    assert_eq!(gh.0.borrow().tries, 0, "{:?}", gh.0.borrow().calls);
}

/// What `gh api --include` writes for a GitHub answer with `status`, `headers` and `body`.
fn answered(status: &str, headers: &[&str], body: &str) -> super::github::Answer {
    let headers: String = ["Content-Type: application/json"].iter().chain(headers).map(|header| format!("{header}\r\n")).collect();
    super::github::Answer::parse(format!("HTTP/2.0 {status}\n{headers}\r\n{body}").as_bytes()).unwrap()
}

#[test]
fn gh_failures_are_told_apart() {
    let not_found = answered("404 Not Found", &[], r#"{"message":"Not Found"}"#);
    assert_eq!(classify(Some(1), Some(&not_found), "gh: Not Found (HTTP 404)"), GhError::NotFound);
    assert_eq!(classify(Some(1), Some(&answered("401 Unauthorized", &[], "")), "gh: Bad credentials (HTTP 401)"), GhError::SignedOut);
    assert_eq!(classify(Some(4), None, "To get started with GitHub CLI, please run:  gh auth login"), GhError::SignedOut);
    assert!(matches!(classify(Some(1), Some(&answered("502 Bad Gateway", &[], "")), "gh: Server Error (HTTP 502)"), GhError::Unavailable(_)));
    let limited = answered("403 Forbidden", &["X-Ratelimit-Remaining: 0"], r#"{"message":"You have exceeded a secondary limit"}"#);
    assert!(matches!(classify(Some(1), Some(&limited), "gh: You have exceeded a secondary limit (HTTP 403)"), GhError::Unavailable(_)));
    let wait = answered("403 Forbidden", &["Retry-After: 60", "X-Ratelimit-Remaining: 4999"], r#"{"message":"Slow down"}"#);
    assert!(matches!(classify(Some(1), Some(&wait), ""), GhError::Unavailable(_)));
    assert!(matches!(classify(Some(1), None, "error connecting to api.github.com"), GhError::Unavailable(_)));
    let exists = answered("422 Unprocessable Entity", &[], r#"{"message":"Reference already exists"}"#);
    assert_eq!(classify(Some(1), Some(&exists), ""), GhError::Refused(422, "Reference already exists".into()));
    let admin = answered("403 Forbidden", &["X-Ratelimit-Remaining: 4999"], r#"{"message":"Must have admin rights to the rate limit settings"}"#);
    assert_eq!(classify(Some(1), Some(&admin), "gh: Must have admin rights (HTTP 403)"),
        GhError::Refused(403, "Must have admin rights to the rate limit settings".into()), "the words «rate limit» decide nothing");
    assert_eq!(classify(Some(1), None, "gh: Not Found (HTTP 404)"), GhError::Unavailable("gh: Not Found (HTTP 404)".into()),
        "without an answer from GitHub its status is not read from the error text");
    let missing = super::github::Gh { program: "/nonexistent/gh".into(), timeout: std::time::Duration::from_secs(5) };
    assert_eq!(missing.api("GET", "user", None), Err(GhError::Missing));
}

#[test]
fn an_issue_is_named_by_number_or_url_and_its_body_reads_back() {
    assert_eq!(send::issue_ref("12"), Some((12, None)));
    assert_eq!(send::issue_ref("#7"), Some((7, None)));
    assert_eq!(send::issue_ref("https://github.com/owner/reports/issues/3"), Some((3, Some("owner/reports".into()))));
    assert_eq!(send::issue_ref("https://github.com/owner/reports/pull/3"), None);
    assert_eq!(send::issue_ref("report.txt"), None);
    assert!(Public::parse("just an issue someone opened").is_none());
}

#[test]
fn listing_finds_reports_by_their_title_and_delete_closes_the_issue_and_takes_the_sender_s_gist() {
    let home = Directory::new();
    let (mac, built) = built_report(&home);
    let gh = FakeGithub::new();
    gh.as_user("ana");
    let Outcome::Sent { number, .. } = send_it(&gh, &mac, &built.id).unwrap() else { panic!() };
    gh.as_user(OWNER);
    repo(&gh).create_issue("A bug", "Not a report", "bug").unwrap();
    repo(&gh).create_issue(&format!("{}not one", send::TITLE_PREFIX), "- Report: r-20261025-00000000\n", "bug").unwrap();
    let reports = open_reports(&repo(&gh)).unwrap();
    assert_eq!(reports.iter().map(|(i, _)| i.number).collect::<Vec<_>>(), vec![number], "an unlabelled report is found by its title and a body that reads as one");
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let text = reports_text(REPO, &reports);
    assert!(text.starts_with(&format!("#{number:<5} ana")), "{text}");
    assert!(text.contains("macOS 26.4") && text.contains("A node's session ended on its own"), "{text}");
    let said = delete_issue_report(&repo(&gh), &issue(&gh, number)).unwrap();
    assert!(said.starts_with(&format!("Closed issue #{number}")) && said.contains("belongs to ana"), "{said}");
    assert_eq!(gh.0.borrow().gists.len(), 1, "the owner of the repository cannot delete someone else's gist");
    assert!(!issue(&gh, number).open);
    gh.as_user("ana");
    let said = delete_issue_report(&repo(&gh), &issue(&gh, number)).unwrap();
    assert!(said.starts_with("Deleted the report's gist"), "{said}");
    assert!(gh.0.borrow().gists.is_empty());
    let error = send::download(&repo(&gh), &issue(&gh, number)).unwrap_err();
    assert!(error.to_string().contains("was deleted"), "{error}");
    assert!(open_reports(&repo(&gh)).unwrap().is_empty(), "a closed report is not listed");
}

#[test]
fn delete_refuses_an_issue_pointing_to_an_unrelated_gist() {
    let home = Directory::new();
    let (mac, built) = built_report(&home);
    let gh = FakeGithub::new();
    let Outcome::Sent { number, .. } = send_it(&gh, &mac, &built.id).unwrap() else { panic!() };
    let mut public = Public::parse(&issue(&gh, number).body).unwrap();
    let notes = "My private notes";
    let foreign = repo(&gh).create_gist("my private notes", "part-01", notes).unwrap();
    public.gist = foreign.url;
    public.parts = 1;
    public.sha256 = bundle::sha256(notes.as_bytes());
    gh.as_user("stranger");
    let forged = repo(&gh).create_issue(&public.title(), &public.body(), send::LABEL).unwrap();
    gh.as_user(OWNER);
    let result = delete_issue_report(&repo(&gh), &forged);
    assert_eq!(gh.calls("DELETE gists/"), 0, "an issue cannot authorize deleting unrelated notes");
    assert!(result.is_err(), "{result:?}");
    assert_eq!(gh.0.borrow().gists.len(), 2);
    assert!(issue(&gh, forged.number).open);
}

fn assert_delete_refuses_change(change: &str) {
    let home = Directory::new();
    let (mac, built) = built_report(&home);
    let gh = FakeGithub::new();
    let Outcome::Sent { number, .. } = send_it(&gh, &mac, &built.id).unwrap() else { panic!() };
    {
        let mut world = gh.0.borrow_mut();
        let gist = &mut world.gists[0];
        match change {
            "description" => gist.description = send::gist_description("r-19000101-00000000"),
            "content" => gist.files.get_mut("part-01").unwrap().push_str("changed"),
            "missing part" => { gist.files.remove("part-01"); }
            "extra file" => { gist.files.insert("notes.txt".into(), "My private notes".into()); }
            _ => unreachable!(),
        }
    }
    let result = delete_issue_report(&repo(&gh), &issue(&gh, number));
    assert_eq!(gh.calls("DELETE gists/"), 0, "{change}");
    assert!(result.is_err(), "{change}: {result:?}");
    assert_eq!(gh.0.borrow().gists.len(), 1, "{change}");
    assert!(issue(&gh, number).open, "{change}");
}

#[test]
fn delete_requires_the_same_report_id_in_the_description() {
    assert_delete_refuses_change("description");
}

#[test]
fn delete_requires_the_same_content_hash() {
    assert_delete_refuses_change("content");
}

#[test]
fn delete_refuses_a_missing_part() {
    assert_delete_refuses_change("missing part");
}

#[test]
fn delete_refuses_files_outside_the_report() {
    assert_delete_refuses_change("extra file");
}

#[test]
fn delete_verifies_all_parts_of_a_genuine_report() {
    let gh = FakeGithub::new();
    let id = "r-20261025-00000000";
    let file = format!("{}{}end", "A".repeat(send::PART_BYTES), "B".repeat(send::PART_BYTES));
    let mut public = Public::of(id, &vec![], Kind::Other, &file);
    let parts = send::parts(&file);
    let gist = repo(&gh).create_gist(&send::gist_description(id), "part-01", parts[0]).unwrap();
    for (index, part) in parts.iter().enumerate().skip(1) {
        repo(&gh).put_gist_file(&gist.id, &format!("part-{:02}", index + 1), part).unwrap();
    }
    public.gist = gist.url;
    let sent = repo(&gh).create_issue(&public.title(), &public.body(), send::LABEL).unwrap();
    delete_issue_report(&repo(&gh), &sent).unwrap();
    assert!(gh.0.borrow().gists.is_empty());
    assert!(!issue(&gh, sent.number).open);
    assert_eq!(gh.calls("DELETE gists/"), 1);
    assert!(gh.calls("RAW ") > 0, "the hash includes the whole text of truncated parts");
}

#[test]
fn a_part_changed_in_its_gist_fails_the_issue_s_hash() {
    let home = Directory::new();
    let (mac, built) = built_report(&home);
    let gh = FakeGithub::new();
    let Outcome::Sent { number, .. } = send_it(&gh, &mac, &built.id).unwrap() else { panic!() };
    let id = gh.0.borrow().gists[0].id.clone();
    let part = gh.0.borrow().gists[0].files["part-01"].clone();
    repo(&gh).put_gist_file(&id, "part-01", &part.replacen('A', "B", 1)).unwrap();
    let error = send::download(&repo(&gh), &issue(&gh, number)).unwrap_err();
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    assert!(error.to_string().contains("changed after it was sent"), "{error}");
}

#[test]
fn the_repository_is_the_chosen_one_else_the_announced_one_else_the_last_announced() {
    let home = Directory::new();
    std::fs::create_dir_all(&*home).unwrap();
    let announcing = |_: &str| Some(json!({"protocol": 1, "epoch": "e", "report_repo": "owner/reports"}));
    let silent = |_: &str| Some(json!({"protocol": 1, "epoch": "e"}));
    let unreachable = |_: &str| None;
    assert_eq!(repo_name(&home, Some("me/mine".into()), &announcing).as_deref(), Some("me/mine"));
    assert_eq!(repo_name(&home, None, &announcing).as_deref(), Some("owner/reports"));
    assert_eq!(repo_name(&home, None, &unreachable).as_deref(), Some("owner/reports"));
    assert_eq!(repo_name(&home, None, &silent), None);
    assert_eq!(repo_name(&home, None, &unreachable), None);
    let bad = |_: &str| Some(json!({"report_repo": "not a repo"}));
    assert_eq!(repo_name(&home, None, &bad), None);
    std::fs::write(home.join("service.toml"), "relay_url = \"https://other.example/\"\n").unwrap();
    assert_eq!(repo_name(&home, None, &announcing).as_deref(), Some("owner/reports"));
    std::fs::write(home.join("service.toml"), "relay_url = \"https://third.example\"\n").unwrap();
    assert_eq!(repo_name(&home, None, &unreachable), None, "what another relay announced does not count");
}
