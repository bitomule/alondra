//! Sending a previewed report: the same file `report save` writes, put in parts in a secret gist of
//! whoever sends it, then an issue in the report repository that holds only the version, macOS, the
//! kind of failure and the gist's address. Anyone who sees the issue can read the report. Every step looks first for what an earlier try left, so
//! sending again never makes a second issue or gist. A try GitHub could not take now leaves the
//! report pending; the daemon tries it again on its own.
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::bundle::{self, Files};
use super::github::{GhError, Issue, Repo};

/// Each part of the report's file in its gist weighs at most this.
pub const PART_BYTES: usize = 900 * 1024;
pub const LABEL: &str = "alondra-report";
/// How every report's issue title starts: the way to know one without its label.
pub const TITLE_PREFIX: &str = "Crash report r-";


pub use alondra_client_port::report::send::Kind;
pub use alondra_domain_model::report_held_card::Held;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Pending,
    Sent,
    /// Whoever sent it chose to keep it on this Mac: nothing sends it again on its own.
    Kept,
}

/// `sending.json` in the report's folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sending {
    pub repo: String,
    pub kind: Kind,
    pub state: State,
    /// The SHA-256 of the file that was sent.
    pub package_sha256: String,
    #[serde(default)]
    pub issue: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub attempts: u32,
    /// When the daemon tries a pending report again, in Unix milliseconds.
    #[serde(default)]
    pub next_at: i64,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub sent_at: Option<i64>,
    /// Why the daemon's tries send nothing until whoever sent it chooses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<Held>,
}

pub fn read_sending(dir: &Path) -> Option<Sending> {
    serde_json::from_slice(&std::fs::read(dir.join(bundle::SENDING)).ok()?).ok()
}

fn write_sending(dir: &Path, sending: &Sending) -> Result<()> {
    let next = dir.join(format!("{}.next", bundle::SENDING));
    std::fs::write(&next, serde_json::to_vec_pretty(sending)?)?;
    std::fs::rename(next, dir.join(bundle::SENDING))?;
    Ok(())
}

/// How long a pending report waits before the next try: one minute, doubling up to half an hour.
pub fn backoff_ms(attempts: u32) -> i64 {
    60_000i64.saturating_mul(1 << attempts.saturating_sub(1).min(5)).min(30 * 60_000)
}

/// The description of the gist that holds report `id`.
pub fn gist_description(id: &str) -> String {
    format!("Alondra crash report {id}")
}

fn part_name(index: usize) -> String {
    format!("part-{:02}", index + 1)
}

/// The parts of a report's file, in order.
pub fn parts(file: &str) -> Vec<&str> {
    file.as_bytes().chunks(PART_BYTES).map(|c| std::str::from_utf8(c).unwrap_or_default()).collect()
}

/// What the issue says.
#[derive(Debug)]
pub struct Public {
    pub id: String,
    /// The address of the gist with the report's file; empty before it exists.
    pub gist: String,
    pub alondra: String,
    pub macos: String,
    pub kind: Kind,
    pub created_at: String,
    pub parts: usize,
    pub sha256: String,
}

impl Public {
    pub fn of(id: &str, files: &Files, kind: Kind, file: &str) -> Public {
        let json = |name: &str| files.iter().find(|(n, _)| n == name).and_then(|(_, t)| serde_json::from_str::<serde_json::Value>(t).ok()).unwrap_or_default();
        let shape = |value: &serde_json::Value| value.as_str().filter(|v| v.len() <= 32 && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | ':' | ' ' | '(' | ')'))).unwrap_or("?").to_owned();
        Public {
            id: id.to_owned(),
            gist: String::new(),
            alondra: shape(&json(bundle::MANIFEST)["alondra"]),
            macos: shape(&json("machine.json")["macos"]),
            kind,
            created_at: shape(&json(bundle::MANIFEST)["created_at"]),
            parts: parts(file).len(),
            sha256: bundle::sha256(file.as_bytes()),
        }
    }

    pub fn title(&self) -> String {
        format!("Crash report {} · Alondra {} · macOS {} · {}", self.id, self.alondra, self.macos, self.kind.word())
    }

    pub fn body(&self) -> String {
        format!(
            "An Alondra crash report, with everything private left out. Read it with alondra report open <this issue's number>.\n\n\
             - Report: {}\n- Alondra: {}\n- macOS: {}\n- Failure: {}\n- Created: {}\n- Gist: {}\n- Parts: {}\n- SHA-256: {}\n",
            self.id,
            self.alondra,
            self.macos,
            self.kind.word(),
            self.created_at,
            self.gist,
            self.parts,
            self.sha256
        )
    }

    /// What an issue body says, if it is a report's.
    pub fn parse(body: &str) -> Option<Public> {
        let field = |name: &str| body.lines().find_map(|l| l.strip_prefix(&format!("- {name}: ")).map(|v| v.trim().to_owned()));
        let id = field("Report").filter(|id| bundle::is_id(id))?;
        let sha256 = field("SHA-256").filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()))?;
        Some(Public {
            gist: field("Gist").filter(|g| g.starts_with("https://gist.github.com/"))?,
            alondra: field("Alondra").unwrap_or_default(),
            macos: field("macOS").unwrap_or_default(),
            kind: field("Failure").and_then(|k| Kind::from_word(&k)).unwrap_or(Kind::Other),
            created_at: field("Created").unwrap_or_default(),
            parts: field("Parts")?.parse().ok().filter(|p| (1..=64).contains(p))?,
            sha256,
            id,
        })
    }
}

/// How a send ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Sent { number: u64, url: String },
    /// GitHub could not take it now; it stays pending and the daemon tries again.
    Pending(String),
}

/// Why a send cannot go on by trying again: the user has to do something first.
#[derive(Debug)]
pub enum Blocked {
    NoRepo,
    NoSuchRepo(String),
    /// A pending report was going to `saved`, and the report repository is now `current`.
    RepoChanged { id: String, saved: String, current: String },
    Github(GhError),
}

impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Blocked::NoRepo => f.write_str(tr!(
                "this Mac knows no report repository: set one with alondra config set report.repo <owner/repo>, or save the report as a file",
                "este Mac no conoce ningún repositorio de informes: pon uno con alondra config set report.repo <dueño/repo>, o guarda el informe como fichero"
            )),
            Blocked::NoSuchRepo(repo) => {
                let repo = repo.as_str();
                write!(f, "{}", trf!(
                    "GitHub has no repository {repo} that takes issues: check report.repo, or save the report as a file",
                    "GitHub no tiene ningún repositorio {repo} que acepte issues: revisa report.repo, o guarda el informe como fichero"
                ))
            }
            Blocked::RepoChanged { id, saved, current } => {
                let (id, saved, current) = (id.as_str(), saved.as_str(), current.as_str());
                write!(f, "{}", trf!(
                    "report {id} was going to {saved}, but report.repo is now {current}: nothing was sent. To send it to {current}: alondra report send {id}",
                    "el informe {id} iba a {saved}, pero report.repo ahora es {current}: no se ha enviado nada. Para enviarlo a {current}: alondra report send {id}"
                ))
            }
            Blocked::Github(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for Blocked {}

fn lock(dir: &Path) -> Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(".sending.lock"))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        bail!("the report's lock failed");
    }
    Ok(file)
}

/// Sends report `id`, whose `files` were read back as the preview showed them, to `repo`.
/// A report GitHub could not take waits `backoff(attempts)` milliseconds for its next try.
pub fn send(repo: &Repo, home: &Path, id: &str, files: &Files, kind: Option<Kind>, now: i64, backoff: &dyn Fn(u32) -> i64) -> Result<Outcome> {
    send_to(repo, home, id, files, kind, now, backoff, false)
}

/// The daemon's try of a pending report: it goes only to the repository it was first sent to. When
/// `repo` is another one, nothing is sent: the report waits the longest backoff and tries again,
/// so it goes on by itself once the repository is the saved one again, or whoever sends it asks for
/// the new one with `alondra report send <id>`.
pub fn retry(repo: &Repo, home: &Path, id: &str, files: &Files, now: i64, backoff: &dyn Fn(u32) -> i64) -> Result<Outcome> {
    send_to(repo, home, id, files, None, now, backoff, true)
}

#[allow(clippy::too_many_arguments)]
fn send_to(repo: &Repo, home: &Path, id: &str, files: &Files, kind: Option<Kind>, now: i64, backoff: &dyn Fn(u32) -> i64, retry: bool) -> Result<Outcome> {
    let dir = home.join("reports").join(id);
    let _lock = lock(&dir)?;
    let saved = read_sending(&dir);
    if retry {
        let Some(pending) = saved.as_ref().filter(|s| s.state == State::Pending) else {
            bail!(trf!("report {id} has nothing pending to send again", "el informe {id} no tiene nada pendiente que reenviar"))
        };
        if pending.repo != repo.name {
            let mut saved = pending.clone();
            let blocked = Blocked::RepoChanged { id: id.to_owned(), saved: saved.repo.clone(), current: repo.name.clone() };
            let refused = match &saved.held {
                Some(Held::RepoChanged { current, refused }) if *current == repo.name => refused.clone(),
                _ => None,
            };
            saved.held = Some(Held::RepoChanged { current: repo.name.clone(), refused });
            saved.next_at = now + backoff(u32::MAX);
            saved.last_error = Some(blocked.to_string());
            write_sending(&dir, &saved)?;
            return Err(blocked.into());
        }
    }
    let file = bundle::armored(id, files);
    let package_sha256 = bundle::sha256(file.as_bytes());
    let held_elsewhere = saved.clone().filter(|s| s.repo != repo.name && s.state == State::Pending
        && matches!(&s.held, Some(Held::RepoChanged { current, .. }) if *current == repo.name));
    let earlier = saved.filter(|s| s.repo == repo.name);
    if let Some(sent) = earlier.as_ref().filter(|s| s.state == State::Sent && s.package_sha256 == package_sha256)
        && let (Some(number), Some(url)) = (sent.issue, sent.url.clone())
    {
        return Ok(Outcome::Sent { number, url });
    }
    let kind = kind.or(earlier.as_ref().map(|s| s.kind)).unwrap_or(Kind::Other);
    let mut sending = earlier.clone().unwrap_or(Sending {
        repo: repo.name.clone(),
        kind,
        state: State::Pending,
        package_sha256: package_sha256.clone(),
        issue: None,
        url: None,
        attempts: 0,
        next_at: now,
        last_error: None,
        sent_at: None,
        held: None,
    });
    sending.kind = kind;
    sending.attempts += 1;
    sending.held = None;
    match deliver(repo, id, files, &file, kind) {
        Ok(issue) => {
            sending.state = State::Sent;
            sending.package_sha256 = package_sha256;
            sending.issue = Some(issue.number);
            sending.url = Some(issue.url.clone());
            sending.sent_at = Some(now);
            sending.last_error = None;
            write_sending(&dir, &sending)?;
            Ok(Outcome::Sent { number: issue.number, url: issue.url })
        }
        Err(Failure::Later(why)) => {
            sending.state = State::Pending;
            sending.next_at = now + backoff(sending.attempts);
            sending.last_error = Some(why.clone());
            write_sending(&dir, &sending)?;
            Ok(Outcome::Pending(why))
        }
        Err(Failure::Blocked(blocked)) => {
            if let Some(mut earlier) = earlier {
                earlier.state = State::Pending;
                earlier.next_at = i64::MAX;
                earlier.last_error = Some(blocked.to_string());
                let _ = write_sending(&dir, &earlier);
            } else if let Some(mut held) = held_elsewhere {
                held.held = Some(Held::RepoChanged { current: repo.name.clone(), refused: Some(blocked.to_string()) });
                let _ = write_sending(&dir, &held);
            }
            Err(blocked.into())
        }
        Err(Failure::Other(error)) => Err(error),
    }
}

/// A daemon's try of report `id` that failed before it could reach GitHub (its final check, no
/// report repository) waits the longest backoff, so the daemon does not start it again every minute.
pub fn postpone(home: &Path, id: &str, now: i64, backoff: &dyn Fn(u32) -> i64, why: &str) -> Result<()> {
    let dir = home.join("reports").join(id);
    let _lock = lock(&dir)?;
    let Some(mut sending) = read_sending(&dir).filter(|s| s.state == State::Pending && s.next_at <= now) else { return Ok(()) };
    sending.next_at = now + backoff(u32::MAX);
    sending.last_error = Some(why.to_owned());
    write_sending(&dir, &sending)
}

/// Keeps pending report `id` on this Mac: the daemon no longer sends it, and only
/// `alondra report send <id>` does.
pub fn keep(home: &Path, id: &str) -> Result<()> {
    if !bundle::is_id(id) {
        bail!(trf!("{id} is not a report id; alondra report prints it", "{id} no es el id de un informe; alondra report lo muestra"));
    }
    let dir = home.join("reports").join(id);
    let _lock = lock(&dir)?;
    let Some(mut sending) = read_sending(&dir).filter(|s| s.state == State::Pending) else {
        bail!(trf!("report {id} has nothing pending to keep", "el informe {id} no tiene nada pendiente que guardar"))
    };
    sending.state = State::Kept;
    sending.held = None;
    write_sending(&dir, &sending)
}

enum Failure {
    Later(String),
    Blocked(Blocked),
    Other(anyhow::Error),
}

impl From<GhError> for Failure {
    fn from(error: GhError) -> Failure {
        match error {
            GhError::Unavailable(why) => Failure::Later(why),
            other => Failure::Blocked(Blocked::Github(other)),
        }
    }
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Failure {
        Failure::Other(error)
    }
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Failure {
        Failure::Other(error.into())
    }
}

fn deliver(repo: &Repo, id: &str, files: &Files, file: &str, kind: Kind) -> Result<Issue, Failure> {
    let login = repo.login()?;
    if !repo.exists()? {
        return Err(Failure::Blocked(Blocked::NoSuchRepo(repo.name.clone())));
    }
    let marker = format!("- Report: {id}\n");
    if let Some(issue) = repo.issues(&format!("creator={login}&state=all"))?.into_iter().find(|i| i.body.contains(&marker)) {
        return Ok(issue);
    }
    let parts = parts(file);
    let description = gist_description(id);
    let gist = match repo.own_gist(&description)? {
        Some(gist) => gist,
        None => repo.create_gist(&description, &part_name(0), parts[0])?,
    };
    for (index, part) in parts.iter().enumerate() {
        let name = part_name(index);
        if gist.files.get(&name).map(String::as_str) != Some(part) {
            repo.put_gist_file(&gist.id, &name, part)?;
        }
    }
    let mut public = Public::of(id, files, kind, file);
    public.gist = gist.url;
    Ok(repo.create_issue(&public.title(), &public.body(), LABEL)?)
}

/// The gist's id in its address.
pub fn gist_id(url: &str) -> &str {
    url.trim_end_matches('/').rsplit('/').next().unwrap_or_default()
}

/// The file of the report an issue names, its parts joined and checked against the issue.
pub fn download(repo: &Repo, issue: &Issue) -> Result<(Public, String)> {
    let Some(public) = Public::parse(&issue.body) else {
        let number = issue.number;
        bail!(trf!("issue {number} is not an Alondra crash report", "el issue {number} no es un informe de fallo de Alondra"));
    };
    let url = public.gist.clone();
    let gist = repo.gist(gist_id(&url))?.with_context(|| trf!("the report's gist {url} was deleted", "el gist del informe, {url}, se borró"))?;
    let file = gist_file(&public, &gist)?;
    Ok((public, file))
}

/// Joins a report's parts and verifies the hash its issue names.
pub fn gist_file(public: &Public, gist: &super::github::Gist) -> Result<String> {
    let url = &public.gist;
    let mut file = String::new();
    for index in 0..public.parts {
        let name = part_name(index);
        let part = gist.files.get(&name).with_context(|| {
            trf!("{url} has no {name}: the report never finished uploading", "{url} no tiene {name}: el informe no terminó de subirse")
        })?;
        file.push_str(part);
    }
    if bundle::sha256(file.as_bytes()) != public.sha256 {
        bail!(tr!("the report in its gist is not the one its issue names: it was changed after it was sent", "el informe de su gist no es el que nombra su issue: se cambió después de enviarlo"));
    }
    Ok(file)
}

/// The issue number in `12`, `#12` or an issue URL, and the repository the URL names.
pub fn issue_ref(text: &str) -> Option<(u64, Option<String>)> {
    if let Ok(number) = text.trim_start_matches('#').parse() {
        return Some((number, None));
    }
    let rest = text.strip_prefix("https://github.com/")?;
    let mut parts = rest.trim_end_matches('/').split('/');
    let (owner, repo, kind, number) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    (kind == "issues").then_some(())?;
    Some((number.parse().ok()?, Some(format!("{owner}/{repo}"))))
}
