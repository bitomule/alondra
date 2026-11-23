//! An Antigravity account: how it reaches Google (its access), the environment every `agy` process
//! of the account starts with, the command that signs it in, and what Alondra reads from `agy`: its
//! version, its model catalog and its usage. The default account is `agy`'s own (`~/.gemini` and
//! the sign-in `agy` keeps); a named one has a private folder of Alondra's, which keeps its sign-in
//! or its key and links what the user set up in `~/.gemini`.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::accounts::AccountBilling;
use crate::model_selection::CatalogModel;

/// The oldest `agy` with `session/resume`, per-session thinking levels and the ACP sign-in.
pub const MINIMUM_VERSION: Version = Version(1, 5, 12);
/// The name `agy acp` gives itself in `initialize`.
pub const AGENT_NAME: &str = "Antigravity ACP Server";
/// The ACP protocol version Alondra speaks.
pub const PROTOCOL_VERSION: u64 = 1;
/// Why a node is refused on an Antigravity account: accounts are checked, but no node runs on one yet.
pub const NODES_NOT_AVAILABLE: &str = "Antigravity nodes are not available yet";
/// The JSON-RPC code ACP answers a session with when the account is not signed in.
pub const AUTH_REQUIRED: i64 = -32000;
/// The file of a named account's folder that keeps its key.
pub const KEY_FILE: &str = "gemini-api-key";
/// The longest key Alondra keeps.
pub const KEY_LIMIT: usize = 512;
/// Why `key` cannot be an account's key: it holds 1 to [`KEY_LIMIT`] characters, none a space or
/// a control character.
pub fn key_problem(key: &str) -> Option<&'static str> {
    (key.is_empty() || key.len() > KEY_LIMIT || key.chars().any(|c| c.is_control() || c.is_whitespace()))
        .then_some("A key is 1 to 512 characters, with no spaces")
}

/// What `SSH_CONNECTION` says so that `agy` keeps a named account's sign-in in its folder: it does
/// so only when it believes it runs remotely.
pub const FILE_STORAGE_SSH_CONNECTION: &str = "127.0.0.1 0 127.0.0.1 0";
/// What a named account's folder links from the user's own `~/.gemini`: rules, skills, plugins,
/// MCP servers, hooks and settings. Never a sign-in, a key or a conversation.
pub const SHARED: [&str; 11] = [
    "GEMINI.md",
    "AGENTS.md",
    "settings.json",
    "skills",
    "commands",
    "extensions",
    "hooks",
    "config/mcp_config.json",
    "antigravity-cli/settings.json",
    "antigravity-cli/mcp",
    "antigravity-cli/plugins",
];
/// The variables that name a Google credential, a project or a folder of `agy`'s: none is ever
/// inherited, so a process never runs on another account's key or project.
pub const CREDENTIAL_VARIABLES: [&str; 11] = [
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_CLOUD_PROJECT",
    "GOOGLE_CLOUD_LOCATION",
    "GOOGLE_CLOUD_QUOTA_PROJECT",
    "GOOGLE_GENAI_USE_VERTEXAI",
    "AGY_ADC_AUTH",
    "GOOGLE_GEMINI_BASE_URL",
    "GEMINI_HOME",
    "AGY_ACP_FORCE_FILE_STORAGE",
];
/// What a process of Alondra's takes from the daemon's environment besides `HOME`, `PATH` and the
/// user's name.
const PASSED_THROUGH: [&str; 8] = ["LANG", "LC_ALL", "TMPDIR", "SSL_CERT_FILE", "SSL_CERT_DIR", "HTTPS_PROXY", "HTTP_PROXY", "NO_PROXY"];
const PROJECT_LIMIT: usize = 128;

/// How an account reaches Google.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// A personal Google account: Free, AI Plus, Pro or Ultra.
    #[default]
    Google,
    /// A Gemini Enterprise licence, by SSO.
    Enterprise,
    /// A Gemini API key from Google AI Studio, billed per use.
    ApiKey,
    /// A Google Cloud project on the Agent Platform, with `gcloud` ADC or an Agent Platform key.
    CloudProject,
}

impl Access {
    pub const ALL: [Access; 4] = [Access::Google, Access::Enterprise, Access::ApiKey, Access::CloudProject];

    pub fn as_str(self) -> &'static str {
        match self {
            Access::Google => "google",
            Access::Enterprise => "enterprise",
            Access::ApiKey => "api_key",
            Access::CloudProject => "cloud_project",
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        Access::ALL.into_iter().find(|access| access.as_str() == text)
            .ok_or_else(|| format!("unknown Antigravity access {text:?}: use google, enterprise, api_key or cloud_project"))
    }

    /// The ACP `authenticate` method of the access.
    pub fn auth_method(self) -> &'static str {
        match self {
            Access::Google => "oauth-personal",
            Access::Enterprise => "oauth-business",
            Access::ApiKey => "gemini-api-key",
            Access::CloudProject => "agent-platform",
        }
    }

    /// A Google sign-in that `agy` keeps itself, in the keyring or the account's folder.
    pub fn signs_in_with_google(self) -> bool {
        matches!(self, Access::Google | Access::Enterprise)
    }

    /// Whether the account may hold a key of Alondra's: a Gemini API key, or an Agent Platform key
    /// instead of ADC.
    pub fn takes_key(self) -> bool {
        matches!(self, Access::ApiKey | Access::CloudProject)
    }

    /// Whether the account names a Google Cloud project and location.
    pub fn takes_project(self) -> bool {
        matches!(self, Access::Enterprise | Access::CloudProject)
    }

    /// Whether `agy -p /usage` reports the account's quota: a subscription's. A key or a project is
    /// billed per use.
    pub fn reads_usage(self) -> bool {
        self.signs_in_with_google()
    }

    /// What using the account costs unless the user says otherwise.
    pub fn default_billing(self) -> AccountBilling {
        match self {
            Access::Google | Access::Enterprise => AccountBilling::default(),
            Access::ApiKey | Access::CloudProject => AccountBilling::PayPerUse,
        }
    }
}

/// What an Antigravity account adds to every account: its access, the project and location of an
/// Enterprise or Cloud account, and the service-account key file a Cloud account's ADC reads.
/// Alondra keeps the file's path, never what it holds.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub access: Access,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_file: Option<PathBuf>,
}

impl Settings {
    pub fn of(access: Access) -> Self {
        Settings { access, ..Settings::default() }
    }

    pub fn check(&self) -> Result<(), String> {
        let word = |what: &str, value: &Option<String>| match value {
            Some(value) if value.is_empty() || value.len() > PROJECT_LIMIT || !value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':')) =>
                Err(format!("an Antigravity {what} has 1 to {PROJECT_LIMIT} letters, digits, dashes, underscores, dots or colons")),
            _ => Ok(()),
        };
        word("project", &self.project)?;
        word("location", &self.location)?;
        if !self.access.takes_project() && (self.project.is_some() || self.location.is_some()) {
            return Err(format!("an Antigravity {} account has no project or location", self.access.as_str()));
        }
        if self.access == Access::CloudProject && self.project.is_none() {
            return Err("an Antigravity cloud_project account needs its project".into());
        }
        match &self.credentials_file {
            Some(_) if self.access != Access::CloudProject => Err("only an Antigravity cloud_project account names a credentials file".into()),
            Some(file) if !file.is_absolute() || file.to_str().is_none_or(|text| text.chars().any(char::is_control)) => Err("an Antigravity credentials file is an absolute path".into()),
            _ => Ok(()),
        }
    }
}

/// What starts an `agy` process of an account: the arguments it adds and its whole environment.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Launch {
    pub args: Vec<String>,
    pub environment: BTreeMap<String, String>,
}

/// Prints every variable but the value of a credential's.
impl std::fmt::Debug for Launch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let environment: BTreeMap<&str, &str> = self.environment.iter()
            .map(|(name, value)| (name.as_str(), if CREDENTIAL_VARIABLES.contains(&name.as_str()) && name.ends_with("_KEY") { "<redacted>" } else { value.as_str() })).collect();
        f.debug_struct("Launch").field("args", &self.args).field("environment", &environment).finish()
    }
}

/// Removes from `environment` every variable that names a Google credential, project or folder.
pub fn remove_credentials(environment: &mut BTreeMap<String, String>) {
    for name in CREDENTIAL_VARIABLES {
        environment.remove(name);
    }
}

/// The variables that start an `agy` process of an account from the daemon's environment: they
/// are removed first, then only the account's own are set. `folder` is a named account's,
/// `key` the one its folder keeps, and `acp` says the process is `agy acp`, the only one that takes
/// `--gemini-dir`.
pub fn launch(inherited: &BTreeMap<String, String>, user_home: &Path, settings: &Settings, folder: Option<&Path>, key: Option<&str>, acp: bool) -> Launch {
    let mut environment = BTreeMap::new();
    environment.insert("HOME".to_owned(), user_home.display().to_string());
    for name in ["PATH", "USER", "LOGNAME"].into_iter().chain(PASSED_THROUGH) {
        if let Some(value) = inherited.get(name) { environment.insert(name.to_owned(), value.clone()); }
    }
    remove_credentials(&mut environment);
    environment.insert("AGY_CLI_DISABLE_AUTO_UPDATE".into(), "1".into());
    let mut args = vec![];
    if let Some(folder) = folder {
        environment.insert("GEMINI_HOME".into(), folder.display().to_string());
        if acp { args.extend(["--gemini-dir".to_owned(), folder.display().to_string()]); }
        if settings.access.signs_in_with_google() {
            environment.insert("AGY_ACP_FORCE_FILE_STORAGE".into(), "1".into());
            environment.insert("SSH_CONNECTION".into(), FILE_STORAGE_SSH_CONNECTION.into());
        }
    }
    let project = |environment: &mut BTreeMap<String, String>| {
        if let Some(project) = &settings.project { environment.insert("GOOGLE_CLOUD_QUOTA_PROJECT".into(), project.clone()); }
        if let Some(location) = &settings.location { environment.insert("GOOGLE_CLOUD_LOCATION".into(), location.clone()); }
    };
    match settings.access {
        Access::Google => {}
        Access::Enterprise => project(&mut environment),
        Access::ApiKey => {
            if let Some(key) = key { environment.insert("GEMINI_API_KEY".into(), key.to_owned()); }
        }
        Access::CloudProject => {
            project(&mut environment);
            match key {
                Some(key) => { environment.insert("GOOGLE_API_KEY".into(), key.to_owned()); }
                None => {
                    environment.insert("AGY_ADC_AUTH".into(), "true".into());
                    if let Some(file) = &settings.credentials_file { environment.insert("GOOGLE_APPLICATION_CREDENTIALS".into(), file.display().to_string()); }
                }
            }
        }
    }
    Launch { args, environment }
}

/// What the user runs in Terminal on the Mac to sign an account in, `folder` being a named one's
/// as they type it. None for an API-key account, whose key is entered in the app or with
/// `alondra accounts set-key`. Alondra never runs it.
pub fn sign_in_command(settings: &Settings, folder: Option<&str>) -> Option<String> {
    match settings.access {
        Access::Google | Access::Enterprise => Some(match folder {
            Some(folder) => {
                let folder = crate::accounts::shell_folder(folder);
                format!("GEMINI_HOME={folder} AGY_ACP_FORCE_FILE_STORAGE=1 SSH_CONNECTION='{FILE_STORAGE_SSH_CONNECTION}' agy acp --gemini-dir {folder} --login")
            }
            None => "agy acp --login".into(),
        }),
        Access::ApiKey => None,
        Access::CloudProject => Some(match &settings.project {
            Some(project) => format!("gcloud auth application-default login --project {project}"),
            None => "gcloud auth application-default login".into(),
        }),
    }
}

/// A release of `agy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// The first `major.minor.patch` in what `agy --version` printed.
    pub fn parse(output: &str) -> Option<Version> {
        output.split(|c: char| c.is_whitespace() || c == 'v').find_map(|word| {
            let mut parts = word.splitn(3, '.');
            let major = parts.next()?.parse().ok()?;
            let minor = parts.next()?.parse().ok()?;
            let patch: String = parts.next()?.chars().take_while(char::is_ascii_digit).collect();
            Some(Version(major, minor, patch.parse().ok()?))
        })
    }

    pub fn supported(self) -> bool {
        self >= MINIMUM_VERSION
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// The family a model of the live catalog belongs to, from its id: `gemini-4` for
/// `gemini-4-neon`, `gemini-3.8` for `gemini-3.8-flash`. An id of another shape is its own family.
pub fn model_family(model: &str) -> String {
    let model = model.trim().to_ascii_lowercase();
    let mut parts = model.splitn(3, '-');
    match (parts.next(), parts.next()) {
        (Some(name), Some(version)) if !name.is_empty() && version.starts_with(|c: char| c.is_ascii_digit()) => format!("{name}-{version}"),
        _ => model,
    }
}

/// The Alondra effort a thinking level of Antigravity is offered as: `extra` is `xhigh`; any
/// other level keeps its own id.
pub fn effort_of(level: &str) -> String {
    match level {
        "extra" => "xhigh".into(),
        other => other.into(),
    }
}

/// The thinking level of `offered` that runs `effort`: its own id, or `extra` for `xhigh`.
pub fn level_for<'a>(effort: &str, offered: &'a [String]) -> Option<&'a str> {
    offered.iter().map(String::as_str).find(|level| *level == effort || effort_of(level) == effort)
}

/// One `select` option of an ACP session (`configOptions`): its current value and the values it
/// offers with their names.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SelectOption {
    pub current: Option<String>,
    pub values: Vec<(String, String)>,
}

/// The option of category `category` among an ACP session's `configOptions`.
pub fn select_option(options: &serde_json::Value, category: &str) -> Option<SelectOption> {
    let option = options.as_array()?.iter().find(|option| option["category"].as_str() == Some(category) && option["type"].as_str() == Some("select"))?;
    let valid = |text: &&str| !text.is_empty() && text.len() <= 256 && !text.chars().any(char::is_control);
    let values = option["options"].as_array()?.iter().filter_map(|value| {
        let id = value["value"].as_str().filter(valid)?;
        Some((id.to_owned(), value["name"].as_str().filter(valid).unwrap_or(id).to_owned()))
    }).collect();
    Some(SelectOption { current: option["currentValue"].as_str().filter(valid).map(str::to_owned), values })
}

/// A catalog entry for `model`, named `name`, with the thinking levels its session offered and
/// the one it had before Alondra set any.
pub fn catalog_model(model: &str, name: &str, levels: &SelectOption, is_default: bool) -> CatalogModel {
    CatalogModel {
        model: model.into(),
        display_name: name.into(),
        reasoning_efforts: levels.values.iter().map(|(level, _)| effort_of(level)).collect(),
        default_reasoning_effort: levels.current.as_deref().map(effort_of),
        context_window: None,
        input_modalities: vec![],
        is_default,
        hidden: false,
    }
}

/// A key as the daemon holds it on its way to the account's file or to its `agy` process: it never
/// prints, never serializes, and its bytes are cleared when it goes.
pub struct Secret(String);

impl Secret {
    pub fn new(text: String) -> Self {
        Secret(text)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(redacted)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // SAFETY: zero bytes are valid UTF-8, and the string is not used again.
        unsafe { self.0.as_bytes_mut().fill(0) };
    }
}

/// How the daemon starts an `agy` of an account for a check or a usage reading: the executable,
/// what its process starts with, the folder it runs in (an empty one of Alondra's) and its access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    pub executable: PathBuf,
    pub launch: Launch,
    pub directory: PathBuf,
    pub access: Access,
}

/// Why an account could not be checked.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Unavailable {
    /// `agy` is not installed, or not on the daemon's `PATH`.
    NotInstalled,
    /// `agy` is older than [`MINIMUM_VERSION`].
    TooOld,
    /// `agy --version` printed no version.
    UnknownVersion,
    /// The program answering is not Antigravity's ACP server, or lacks what Alondra needs of it.
    OtherAgent,
    /// It did not answer, or answered something Alondra cannot read.
    Failed,
}

/// What an account check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// Signed in, with the models and thinking levels the account offers.
    SignedIn { models: Vec<CatalogModel>, native_default: Option<String> },
    SignedOut,
    Unavailable(Unavailable),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckReport {
    pub version: Option<Version>,
    pub check: Check,
}

/// What a usage reading found: the account's windows, or `agy` refusing it (status `ERROR`), which
/// only an account check can tell apart from a sign-in gone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsageOutcome {
    Read(crate::usage::UsageReading),
    Refused,
}

/// A window name's prefix for Antigravity quota.
pub const WINDOW_PREFIX: &str = "antigravity:";
const SLUG_LIMIT: usize = 24;

/// Milliseconds since the epoch of a UTC time written `YYYY-MM-DDTHH:MM:SS[.fff]Z` (or `+00:00`).
pub fn parse_utc(text: &str) -> Option<crate::Millis> {
    let text = text.strip_suffix('Z').or_else(|| text.strip_suffix("+00:00"))?;
    let (date, time) = text.split_once('T')?;
    if !date.bytes().all(|b| b.is_ascii_digit() || b == b'-') || !time.bytes().all(|b| b.is_ascii_digit() || matches!(b, b':' | b'.')) { return None; }
    if date.split('-').any(str::is_empty) || time.split(':').any(str::is_empty) { return None; }
    let mut date = date.splitn(3, '-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let (time, fraction) = time.split_once('.').unwrap_or((time, ""));
    let mut time = time.splitn(3, ':').map(|part| part.parse::<i64>().ok());
    let (hour, minute, second) = (time.next()??, time.next()??, time.next()??);
    if !(1970..=9999).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 || !fraction.bytes().all(|b| b.is_ascii_digit()) { return None; }
    let millis: i64 = format!("{fraction:0<3}").get(..3)?.parse().ok()?;
    let (y, m) = if month <= 2 { (year - 1, month + 9) } else { (year, month - 3) };
    let era = y.div_euclid(400);
    let of_era = y - era * 400;
    let of_year = (153 * m + 2) / 5 + day - 1;
    let of_cycle = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    let days = era * 146_097 + of_cycle - 719_468;
    Some(((days * 86_400 + hour * 3_600 + minute * 60 + second) * 1_000 + millis) as crate::Millis)
}

fn slug(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() || c == '.' { slug.push(c); } else if !slug.is_empty() && !slug.ends_with('-') { slug.push('-'); }
        if slug.len() >= SLUG_LIMIT { break; }
    }
    let slug = slug.trim_matches('-').to_owned();
    if slug.is_empty() { "models".into() } else { slug }
}

/// The windows of a `/usage` report (`command.data`). Each group lists one bucket per model and
/// thinking level, holding the share left of the limit of that model's pool closest to running
/// out. Buckets that read alike in a group share a pool: each pool is one window,
/// `antigravity:<group>:<first model of the pool>`, which runs out on its own. A reset at the epoch
/// means none is pending.
pub fn parse_usage(data: &serde_json::Value) -> Result<crate::usage::UsageReading, String> {
    use crate::usage::{Share, UsageReading, WindowKind, WindowReading};
    let groups = data["groups"].as_array().ok_or("agy returned a usage report without groups")?;
    let mut windows: Vec<WindowReading> = vec![];
    for group in groups {
        let group_name = slug(group["name"].as_str().unwrap_or_default());
        let mut pools: Vec<(String, f64, Option<i64>)> = vec![];
        for bucket in group["buckets"].as_array().into_iter().flatten() {
            let Some(left) = bucket["remaining_fraction"].as_f64().filter(|left| left.is_finite() && (0.0..=1.0).contains(left)) else { continue };
            let Some(id) = bucket["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control)) else { continue };
            let resets_at = bucket["reset_time"].as_str().and_then(parse_utc).filter(|at| *at > 0);
            if !pools.iter().any(|(_, other, reset)| (other - left).abs() < 1e-9 && *reset == resets_at) {
                pools.push((id.to_owned(), left, resets_at));
            }
        }
        for (id, left, resets_at) in pools {
            let name: String = format!("{WINDOW_PREFIX}{group_name}:{}", slug(&id)).chars().take(60).collect();
            let Some(kind) = (1..=99).map(|n| if n == 1 { name.clone() } else { format!("{name}-{n}") }).map(WindowKind::Other)
                .find(|kind| !windows.iter().any(|window| window.kind == *kind)) else { continue };
            windows.push(WindowReading { kind, used: Share::of(1.0 - left), resets_at, exhausted: left <= 0.0 });
        }
    }
    windows.truncate(16);
    let reading = UsageReading { windows, plan: None, complete: true };
    reading.check()?;
    Ok(reading)
}

#[cfg(test)]
#[path = "antigravity_account_tests.rs"]
mod tests;
