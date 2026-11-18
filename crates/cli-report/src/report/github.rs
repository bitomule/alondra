//! GitHub through the user's own `gh` (`gh api`): the issues of the report repository and the
//! user's own gists, where each report's file goes.
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

/// Why a `gh api` call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhError {
    /// `gh` is not installed.
    Missing,
    /// `gh` has no GitHub account signed in, or its sign-in expired.
    SignedOut,
    NotFound,
    /// No network, a GitHub outage or its rate limit: worth trying again later.
    Unavailable(String),
    /// GitHub refused the call: the status and its message.
    Refused(u16, String),
}

impl std::fmt::Display for GhError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GhError::Missing | GhError::SignedOut => f.write_str(tr!(
                "to send the report you need GitHub CLI signed in with your account: gh auth login",
                "para mandar el informe necesitas GitHub CLI con tu cuenta: gh auth login"
            )),
            GhError::NotFound => f.write_str(tr!("GitHub found nothing there", "GitHub no encontró nada ahí")),
            GhError::Unavailable(why) => write!(f, "{} ({why})", tr!("GitHub cannot be reached now", "ahora no se llega a GitHub")),
            GhError::Refused(status, message) => write!(f, "GitHub: {message} ({status})"),
        }
    }
}

impl std::error::Error for GhError {}

pub type GhResult<T> = Result<T, GhError>;

/// One REST call to GitHub: method, path (with its query) and JSON body; and the whole text of a
/// gist file the API answered only in part, from its `raw_url`.
pub trait Github {
    fn api(&self, method: &str, path: &str, body: Option<&Value>) -> GhResult<Value>;
    fn raw(&self, url: &str) -> GhResult<String>;
}

/// Where GitHub serves the text of gist files.
pub const RAW_GISTS: &str = "https://gist.githubusercontent.com/";

/// The `gh` on this Mac's PATH.
pub struct Gh {
    pub program: String,
    pub timeout: Duration,
}

impl Default for Gh {
    fn default() -> Gh {
        Gh { program: "gh".into(), timeout: Duration::from_secs(90) }
    }
}

impl Github for Gh {
    fn api(&self, method: &str, path: &str, body: Option<&Value>) -> GhResult<Value> {
        let mut command = Command::new(&self.program);
        command.args(["api", "--include", "--method", method, "-H", "Accept: application/vnd.github+json", path]);
        if body.is_some() {
            command.args(["--input", "-"]);
        }
        let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => GhError::Missing,
            _ => GhError::Unavailable(error.to_string()),
        })?;
        if let (Some(body), Some(mut stdin)) = (body, child.stdin.take()) {
            stdin.write_all(&serde_json::to_vec(body).unwrap_or_default()).map_err(|e| GhError::Unavailable(e.to_string()))?;
        }
        child.stdin.take();
        let pid = child.id();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(child.wait_with_output());
        });
        let output = match receiver.recv_timeout(self.timeout) {
            Ok(output) => output.map_err(|e| GhError::Unavailable(e.to_string()))?,
            Err(_) => {
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                return Err(GhError::Unavailable(tr!("gh did not answer", "gh no respondió").into()));
            }
        };
        let answer = Answer::parse(&output.stdout);
        if output.status.success() {
            return Ok(answer.and_then(|answer| serde_json::from_str(&answer.body).ok()).unwrap_or(Value::Null));
        }
        Err(classify(output.status.code(), answer.as_ref(), &String::from_utf8_lossy(&output.stderr)))
    }

    fn raw(&self, url: &str) -> GhResult<String> {
        if !url.starts_with(RAW_GISTS) {
            return Err(GhError::Refused(400, format!("{url} is not a gist file")));
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .tls_config(ureq::tls::TlsConfig::builder().provider(ureq::tls::TlsProvider::NativeTls).build())
            .timeout_global(Some(self.timeout))
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into();
        let mut response = agent.get(url).call().map_err(|e| GhError::Unavailable(e.to_string()))?;
        match response.status().as_u16() {
            200 => response.body_mut().with_config().limit(2 * 1024 * 1024).read_to_string().map_err(|e| GhError::Unavailable(e.to_string())),
            404 => Err(GhError::NotFound),
            status if status >= 500 || status == 429 => Err(GhError::Unavailable(format!("HTTP {status}"))),
            status => Err(GhError::Refused(status, format!("{url} answered {status}"))),
        }
    }
}

/// What GitHub answered to a `gh api --include` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    /// Header names in lower case.
    pub headers: std::collections::BTreeMap<String, String>,
    pub body: String,
}

impl Answer {
    /// `HTTP/<version> <status> <reason>`, header lines, an empty line, then the body.
    pub fn parse(stdout: &[u8]) -> Option<Answer> {
        let end = stdout.windows(4).position(|window| window == b"\r\n\r\n")?;
        let head = std::str::from_utf8(&stdout[..end]).ok()?;
        let mut lines = head.lines();
        let status = lines.next()?.strip_prefix("HTTP/")?.split_whitespace().nth(1)?.parse().ok()?;
        let headers = lines.filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned())).collect();
        Some(Answer { status, headers, body: String::from_utf8_lossy(&stdout[end + 4..]).into_owned() })
    }

    /// A 429, or a 403 with no requests left or a wait to respect.
    fn rate_limited(&self) -> bool {
        self.status == 429 || (self.status == 403
            && (self.headers.get("x-ratelimit-remaining").is_some_and(|left| left == "0") || self.headers.contains_key("retry-after")))
    }
}

/// What a failed `gh api` means: GitHub's HTTP status and headers when it answered, else `gh`'s exit
/// status (4 is a missing sign-in). The words are only shown.
pub fn classify(code: Option<i32>, answer: Option<&Answer>, stderr: &str) -> GhError {
    let Some(answer) = answer else {
        return if code == Some(4) { GhError::SignedOut } else { GhError::Unavailable(stderr.trim().to_owned()) };
    };
    let message = serde_json::from_str::<Value>(&answer.body).ok().and_then(|v| v["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| stderr.trim().to_owned());
    match answer.status {
        401 => GhError::SignedOut,
        404 => GhError::NotFound,
        _ if answer.rate_limited() => GhError::Unavailable(message),
        status if status >= 500 => GhError::Unavailable(message),
        status => GhError::Refused(status, message),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub body: String,
    pub author: String,
    pub created_at: String,
    pub open: bool,
}

/// A gist: its id, its page and its files (name and text), and its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gist {
    pub id: String,
    pub url: String,
    pub owner: String,
    pub description: String,
    pub files: std::collections::BTreeMap<String, String>,
}

/// The report repository `name` (`owner/repo`) and the user's gists, through `gh`.
pub struct Repo<'a> {
    pub gh: &'a dyn Github,
    pub name: String,
}

fn issue(value: &Value) -> Option<Issue> {
    if value.get("pull_request").is_some_and(|p| !p.is_null()) {
        return None;
    }
    Some(Issue {
        number: value["number"].as_u64()?,
        url: value["html_url"].as_str().unwrap_or_default().to_owned(),
        title: value["title"].as_str().unwrap_or_default().to_owned(),
        body: value["body"].as_str().unwrap_or_default().to_owned(),
        author: value["user"]["login"].as_str().unwrap_or_default().to_owned(),
        created_at: value["created_at"].as_str().unwrap_or_default().to_owned(),
        open: value["state"].as_str() == Some("open"),
    })
}

/// A gist as the API answers it; `pending` names the files it answered only in part, with the
/// address of their whole text.
fn gist(value: &Value) -> GhResult<(Gist, Vec<(String, String)>)> {
    let id = value["id"].as_str().ok_or_else(|| GhError::Refused(200, "GitHub did not return the gist".into()))?.to_owned();
    let mut files = std::collections::BTreeMap::new();
    let mut pending = Vec::new();
    for (name, file) in value["files"].as_object().into_iter().flatten() {
        match (file["truncated"].as_bool(), file["content"].as_str(), file["raw_url"].as_str()) {
            (Some(true), _, Some(raw)) => pending.push((name.clone(), raw.to_owned())),
            (Some(true), _, None) => return Err(GhError::Refused(200, format!("GitHub returned only part of {name}"))),
            (_, Some(text), _) => {
                files.insert(name.clone(), text.to_owned());
            }
            _ => {}
        }
    }
    let gist = Gist {
        url: value["html_url"].as_str().map(str::to_owned).unwrap_or_else(|| format!("https://gist.github.com/{id}")),
        owner: value["owner"]["login"].as_str().unwrap_or_default().to_owned(),
        description: value["description"].as_str().unwrap_or_default().to_owned(),
        id,
        files,
    };
    Ok((gist, pending))
}

impl Repo<'_> {
    fn path(&self, rest: &str) -> String {
        format!("repos/{}/{rest}", self.name)
    }

    pub fn login(&self) -> GhResult<String> {
        self.gh.api("GET", "user", None)?["login"].as_str().map(str::to_owned).ok_or(GhError::SignedOut)
    }

    /// Whether the report repository exists and takes issues.
    pub fn exists(&self) -> GhResult<bool> {
        match self.gh.api("GET", &format!("repos/{}", self.name), None) {
            Ok(repo) => Ok(repo["has_issues"].as_bool() != Some(false)),
            Err(GhError::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub fn issue(&self, number: u64) -> GhResult<Option<Issue>> {
        match self.gh.api("GET", &self.path(&format!("issues/{number}")), None) {
            Ok(value) => Ok(issue(&value)),
            Err(GhError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Issues of the repository, newest first, that match `query` (`creator=…`, `state=…`).
    pub fn issues(&self, query: &str) -> GhResult<Vec<Issue>> {
        let mut all = Vec::new();
        for page in 1..=10 {
            let value = self.gh.api("GET", &self.path(&format!("issues?{query}&per_page=100&page={page}")), None)?;
            let items = value.as_array().cloned().unwrap_or_default();
            all.extend(items.iter().filter_map(issue));
            if items.len() < 100 {
                break;
            }
        }
        Ok(all)
    }

    /// Opens an issue; GitHub drops the label of someone who may not label issues of the repository.
    pub fn create_issue(&self, title: &str, body: &str, label: &str) -> GhResult<Issue> {
        let value = self.gh.api("POST", &self.path("issues"), Some(&json!({"title": title, "body": body, "labels": [label]})))?;
        issue(&value).ok_or_else(|| GhError::Refused(201, "GitHub did not return the issue".into()))
    }

    pub fn close_issue(&self, number: u64) -> GhResult<()> {
        self.gh.api("PATCH", &self.path(&format!("issues/{number}")), Some(&json!({"state": "closed"}))).map(drop)
    }

    /// The signed-in user's gist described `description`, with its files.
    pub fn own_gist(&self, description: &str) -> GhResult<Option<Gist>> {
        for page in 1..=10 {
            let value = self.gh.api("GET", &format!("gists?per_page=100&page={page}"), None)?;
            let items = value.as_array().cloned().unwrap_or_default();
            if let Some(found) = items.iter().find(|g| g["description"].as_str() == Some(description)) {
                let id = found["id"].as_str().unwrap_or_default();
                return self.gist(id);
            }
            if items.len() < 100 {
                break;
            }
        }
        Ok(None)
    }

    /// Gist `id` with the whole text of each file, also of those the API answered only in part.
    pub fn gist(&self, id: &str) -> GhResult<Option<Gist>> {
        let value = match self.gh.api("GET", &format!("gists/{id}"), None) {
            Ok(value) => value,
            Err(GhError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let (mut gist, pending) = gist(&value)?;
        for (name, raw) in pending {
            let text = self.gh.raw(&raw)?;
            gist.files.insert(name, text);
        }
        Ok(Some(gist))
    }

    /// A new secret gist: unlisted, but anyone with its address reads it.
    pub fn create_gist(&self, description: &str, name: &str, text: &str) -> GhResult<Gist> {
        let body = json!({"description": description, "public": false, "files": {name: {"content": text}}});
        let (mut gist, _) = gist(&self.gh.api("POST", "gists", Some(&body))?)?;
        gist.files.insert(name.to_owned(), text.to_owned());
        Ok(gist)
    }

    /// Puts file `name` in gist `id`, replacing it if it is there.
    pub fn put_gist_file(&self, id: &str, name: &str, text: &str) -> GhResult<()> {
        self.gh.api("PATCH", &format!("gists/{id}"), Some(&json!({"files": {name: {"content": text}}}))).map(drop)
    }

    pub fn delete_gist(&self, id: &str) -> GhResult<bool> {
        match self.gh.api("DELETE", &format!("gists/{id}"), None) {
            Ok(_) => Ok(true),
            Err(GhError::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }
}
