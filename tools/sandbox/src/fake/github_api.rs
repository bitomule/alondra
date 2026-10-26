//! The `gh api` calls `alondra report` makes, answered from `fakes/github/reports.json`: the report
//! repository's issues and every account's gists. The account is `FAKE_GH_LOGIN`'s. While
//! `fakes/github/offline` exists every call fails as `gh` does without a network.
use std::collections::BTreeMap;
use std::io::Read;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Fakes, write_json};

/// The repository the sandbox relay announces for crash reports.
pub const REPORTS: &str = "sandbox/reports";
/// The GitHub account the fake `gh` is signed in as, unless `FAKE_GH_LOGIN` names another.
pub const LOGIN: &str = "sandbox-user";
/// The account that owns the report repository: the only one whose labels stick.
pub const OWNER: &str = "reports-owner";

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Gist {
    pub id: String,
    pub owner: String,
    pub description: String,
    pub files: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Reports {
    pub gists: Vec<Gist>,
    pub issues: Vec<Value>,
}

fn gist_json(gist: &Gist, with_content: bool) -> Value {
    let files: serde_json::Map<String, Value> = gist
        .files
        .iter()
        .map(|(name, text)| (name.clone(), if with_content { json!({"filename": name, "content": text, "truncated": false}) } else { json!({"filename": name}) }))
        .collect();
    json!({"id": gist.id, "html_url": format!("https://gist.github.com/{}", gist.id), "description": gist.description, "public": false, "owner": {"login": gist.owner}, "files": files})
}

impl Fakes {
    fn reports_path(&self) -> std::path::PathBuf {
        self.dir.join("github/reports.json")
    }

    pub fn reports(&self) -> Result<Reports> {
        match std::fs::read(self.reports_path()) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Reports::default()),
            Err(error) => Err(error.into()),
        }
    }

    /// Makes every `gh api` call fail as without a network, or work again.
    pub fn github_offline(&self, offline: bool) -> Result<()> {
        let flag = self.dir.join("github/offline");
        if offline {
            std::fs::create_dir_all(self.dir.join("github"))?;
            std::fs::write(flag, "")?;
        } else if flag.exists() {
            std::fs::remove_file(flag)?;
        }
        Ok(())
    }
}

enum Answer {
    Ok(Value),
    Http(u16, &'static str),
}

/// `gh api --method <method> -H <accept> <path> [--input -]`: prints what GitHub answers and
/// returns the exit status `gh` gives.
pub fn api(fakes: &Fakes, method: &str, path: &str, input: bool) -> Result<i32> {
    if fakes.dir.join("github/offline").exists() {
        eprintln!("error connecting to api.github.com\ncheck your internet connection or https://githubstatus.com");
        return Ok(1);
    }
    let body: Option<Value> = if input {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        Some(serde_json::from_str(&text)?)
    } else {
        None
    };
    let _lock = fakes.lock("gh")?;
    let mut state = fakes.reports()?;
    let answer = answer(&mut state, method, path, body.unwrap_or(Value::Null));
    std::fs::create_dir_all(fakes.dir.join("github"))?;
    write_json(&fakes.reports_path(), &state)?;
    match answer {
        Answer::Ok(value) => {
            println!("{value}");
            Ok(0)
        }
        Answer::Http(status, message) => {
            println!("{}", json!({"message": message}));
            eprintln!("gh: {message} (HTTP {status})");
            Ok(1)
        }
    }
}

fn answer(state: &mut Reports, method: &str, path: &str, body: Value) -> Answer {
    let login = std::env::var("FAKE_GH_LOGIN").unwrap_or_else(|_| LOGIN.into());
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    let param = |name: &str| query.split('&').find_map(|p| p.strip_prefix(&format!("{name}="))).map(str::to_owned);
    let page: usize = param("page").and_then(|p| p.parse().ok()).unwrap_or(1);
    match (method, route) {
        ("GET", "user") => return Answer::Ok(json!({"login": login})),
        ("GET", "gists") => {
            let own = state.gists.iter().rev().filter(|g| g.owner == login).skip((page - 1) * 100).take(100).map(|g| gist_json(g, false)).collect();
            return Answer::Ok(Value::Array(own));
        }
        ("POST", "gists") => {
            let files = body["files"].as_object().cloned().unwrap_or_default();
            let gist = Gist {
                id: format!("{:032x}", state.gists.len() + 1),
                owner: login,
                description: body["description"].as_str().unwrap_or_default().into(),
                files: files.iter().map(|(name, file)| (name.clone(), file["content"].as_str().unwrap_or_default().to_owned())).collect(),
            };
            state.gists.push(gist.clone());
            return Answer::Ok(gist_json(&gist, true));
        }
        (_, r) if r.starts_with("gists/") => {
            let Some(at) = state.gists.iter().position(|g| g.id == r["gists/".len()..]) else { return Answer::Http(404, "Not Found") };
            let mine = state.gists[at].owner == login;
            return match method {
                "GET" => Answer::Ok(gist_json(&state.gists[at], true)),
                "PATCH" if mine => {
                    for (name, file) in body["files"].as_object().cloned().unwrap_or_default() {
                        state.gists[at].files.insert(name, file["content"].as_str().unwrap_or_default().to_owned());
                    }
                    Answer::Ok(gist_json(&state.gists[at], true))
                }
                "DELETE" if mine => {
                    state.gists.remove(at);
                    Answer::Ok(Value::Null)
                }
                _ => Answer::Http(404, "Not Found"),
            };
        }
        _ => {}
    }
    let Some(rest) = route.strip_prefix(&format!("repos/{REPORTS}")) else { return Answer::Http(404, "Not Found") };
    match (method, rest.trim_start_matches('/')) {
        ("GET", "") => Answer::Ok(json!({"full_name": REPORTS, "private": false, "has_issues": true})),
        ("POST", "issues") => {
            let number = state.issues.len() as u64 + 1;
            let labels: Vec<Value> = if login == OWNER { body["labels"].as_array().cloned().unwrap_or_default().iter().map(|l| json!({"name": l})).collect() } else { Vec::new() };
            let issue = json!({
                "number": number, "html_url": format!("https://github.com/{REPORTS}/issues/{number}"), "title": body["title"], "body": body["body"],
                "labels": labels, "user": {"login": login}, "created_at": "2026-10-26T10:00:00Z", "state": "open",
            });
            state.issues.push(issue.clone());
            Answer::Ok(issue)
        }
        ("GET", "issues") => {
            let creator = param("creator");
            let wanted = param("state").unwrap_or_else(|| "open".into());
            let picked: Vec<Value> = state
                .issues
                .iter()
                .rev()
                .filter(|i| creator.as_ref().is_none_or(|c| i["user"]["login"] == c.as_str()))
                .filter(|i| wanted == "all" || i["state"] == wanted.as_str())
                .skip((page - 1) * 100)
                .take(100)
                .cloned()
                .collect();
            Answer::Ok(Value::Array(picked))
        }
        (method, r) if r.starts_with("issues/") => {
            let Some(issue) = r["issues/".len()..].parse::<u64>().ok().and_then(|n| state.issues.iter_mut().find(|i| i["number"] == n)) else {
                return Answer::Http(404, "Not Found");
            };
            match method {
                "GET" => Answer::Ok(issue.clone()),
                "PATCH" if login == OWNER || issue["user"]["login"] == login.as_str() => {
                    issue["state"] = body["state"].clone();
                    Answer::Ok(issue.clone())
                }
                _ => Answer::Http(403, "Must have admin rights to Repository."),
            }
        }
        _ => Answer::Http(404, "Not Found"),
    }
}
