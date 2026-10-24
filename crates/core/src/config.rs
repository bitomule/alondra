use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "config.toml";
pub const DEFAULT_LABEL: &str = "dev.alondra.alondrad";
pub const DEFAULT_RETENTION_HOURS: f64 = 14.0 * 24.0;
pub const DEFAULT_DEVICE_EXPIRY_DAYS: f64 = 30.0;
pub const DEFAULT_CLOSE_FREE_NODES_AFTER: Duration = Duration::from_secs(30 * 60);
pub const DEFAULT_COMPACT_IDLE_NODES_AT: f64 = 30.0;
pub const DEFAULT_COMPACT_ORCHESTRATOR_AT: f64 = 50.0;
pub const OFF: &str = "off";

/// What `config.toml` says. Every key is optional: a missing key falls back to the default,
/// and an environment variable beats the file.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct FileConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listen: Option<IpAddr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legacy_bridge: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wake: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_hours: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_dead_nodes: Option<bool>,
    /// How long Alondra waits for the node of a finished task to clean up before it closes the node
    /// anyway, as `30m`, `2h` or `7d`, or `off` to close it only once the node says it cleaned up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_free_nodes_after: Option<String>,
    /// How full an idle node's context may be, in percent, before Alondra compacts it to give it
    /// a new task. 0 turns it off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compact_idle_nodes_at: Option<f64>,
    /// Compact the on-demand orchestrator after a turn at this context percentage; zero disables.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compact_orchestrator_at: Option<f64>,
    /// Accepted so an older file still loads; Alondra no longer reads it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_tasks_days: Option<f64>,
    /// Days a paired device may go unused before Alondra unpairs it on its own; 0 turns it off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_expiry_days: Option<f64>,
    /// Install a newer release with Homebrew as soon as the daemon sees it. Off unless set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_update: Option<bool>,
}

impl FileConfig {
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: FileConfig = toml::from_str(text).map_err(|e| e.to_string())?;
        if let Some(after) = &file.close_free_nodes_after {
            parse_switchable_duration(after).map_err(|e| format!("close_free_nodes_after: {e}"))?;
        }
        if file.compact_idle_nodes_at.is_some_and(|p| !(0.0..=100.0).contains(&p)) {
            return Err("compact_idle_nodes_at: a percentage from 0 (off) to 100".into());
        }
        if file.device_expiry_days.is_some_and(|days| !(days.is_finite() && days >= 0.0)) {
            return Err("device_expiry_days: a number of days, 0 to turn it off".into());
        }
        if file.compact_orchestrator_at.is_some_and(|p| !(0.0..=100.0).contains(&p)) {
            return Err("compact_orchestrator_at: a percentage from 0 (off) to 100".into());
        }
        Ok(file)
    }

    pub fn render(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    /// Keys set in `over` replace the ones in `self`.
    pub fn overlay(mut self, over: &FileConfig) -> FileConfig {
        macro_rules! take {
            ($($f:ident),*) => { $( if over.$f.is_some() { self.$f = over.$f.clone(); } )* };
        }
        take!(name, port, listen, public_url, legacy_bridge, wake, retention_hours, service_label, resume_dead_nodes, close_free_nodes_after, compact_idle_nodes_at, compact_orchestrator_at, archived_tasks_days, device_expiry_days, auto_update);
        self
    }
}

/// A duration, or `off` for none.
pub fn parse_switchable_duration(text: &str) -> Result<Option<Duration>, String> {
    if text.trim().eq_ignore_ascii_case(OFF) { Ok(None) } else { parse_duration(text).map(Some) }
}

/// `90s`, `30m`, `2h`, `1.5d`: a positive number and its unit.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let unit = text.chars().last().ok_or("empty")?;
    let seconds = match unit {
        's' => 1.0,
        'm' => 60.0,
        'h' => 3600.0,
        'd' => 86400.0,
        _ => return Err(format!("\"{text}\" needs a unit: s, m, h or d")),
    };
    let amount: f64 = text[..text.len() - 1].trim().parse().map_err(|_| format!("\"{text}\" is not a duration like 30m, 2h or 1d"))?;
    if !(amount.is_finite() && amount > 0.0) {
        return Err(format!("\"{text}\" must be longer than zero"));
    }
    Ok(Duration::from_secs_f64(amount * seconds))
}

#[derive(Clone, PartialEq, Debug)]
pub struct Config {
    pub home: PathBuf,
    pub name: String,
    pub port: u16,
    pub listen: IpAddr,
    pub public_url: Option<String>,
    pub legacy_bridge: bool,
    pub wake: bool,
    pub retention_hours: f64,
    pub service_label: String,
    /// A throwaway instance for probes and end-to-end runs: only it takes reports and questions from `test/` nodes.
    pub sandbox: bool,
    /// Only in a sandbox (`ALONDRA_SANDBOX_TIMING=fast`): node revival, pull request polling and the
    /// spacing between node starts take seconds instead of minutes, and the Mac never holds a start,
    /// so an end-to-end run sees them happen on a busy Mac.
    pub fast_timing: bool,
    /// `ALONDRA_IGNORE_MAC_LOAD=1`: the admission gate starts nodes whatever the Mac's memory and load,
    /// as the upgrade rehearsal needs on a Mac other builds keep busy.
    pub ignore_mac_load: bool,
    /// Restart a node whose process ended while it still had work. Only with `wake`.
    pub resume_dead_nodes: bool,
    /// How long Alondra waits for the node of a finished task to clean up before it closes the node
    /// anyway. 30 minutes unless set; `None` closes it only once the node says it cleaned up.
    pub close_free_nodes_after: Option<Duration>,
    /// Compact an idle node whose context is at least this full, in percent, before typing it
    /// a new task. 30 unless set; `None` is off.
    pub compact_idle_nodes_at: Option<f64>,
    pub compact_orchestrator_at: Option<f64>,
    /// Days a paired device may go unused before Alondra unpairs it on its own. 30 unless set;
    /// `None` is off.
    pub device_expiry_days: Option<f64>,
    pub auto_update: bool,
}

pub fn home_dir(env: &impl Fn(&str) -> Option<String>) -> PathBuf {
    PathBuf::from(env("HOME").unwrap_or_else(|| "/tmp".into()))
}

pub fn alondra_home(env: &impl Fn(&str) -> Option<String>) -> PathBuf {
    env("ALONDRA_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home_dir(env).join(".alondra"))
}

pub fn path_in(home: &Path) -> PathBuf {
    home.join(FILE_NAME)
}

fn process_env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

impl Config {
    /// Reads `$ALONDRA_HOME/config.toml` (absent is fine) and applies the environment on top.
    pub fn load() -> Result<Config, String> {
        let home = alondra_home(&process_env);
        let path = path_in(&home);
        let file = match std::fs::read_to_string(&path) {
            Ok(text) => FileConfig::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileConfig::default(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        Ok(Config::resolve(home, &file, process_env, system_name))
    }

    /// Defaults with no file, for when the file is broken and something must still run.
    pub fn defaults() -> Config {
        Config::resolve(alondra_home(&process_env), &FileConfig::default(), process_env, system_name)
    }

    /// Environment beats the file, the file beats the default. `wake` is the exception that
    /// keeps test instances harmless: an instance whose port comes from `ALONDRA_PORT` only
    /// wakes nodes when `ALONDRA_WAKE=1` says so explicitly.
    pub fn resolve(home: PathBuf, file: &FileConfig, env: impl Fn(&str) -> Option<String>, fallback_name: impl FnOnce() -> Option<String>) -> Config {
        let var = |k: &str| env(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let flag = |v: String| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on");
        let port_from_env = var("ALONDRA_PORT").and_then(|p| p.parse().ok());
        let wake = match var("ALONDRA_WAKE") {
            Some(v) => v == "1",
            None if port_from_env.is_some() => false,
            None => file.wake.unwrap_or(false),
        };
        Config {
            name: var("ALONDRA_NAME")
                .or_else(|| file.name.clone().filter(|n| !n.trim().is_empty()))
                .or_else(fallback_name)
                .unwrap_or_else(|| "the user".into()),
            port: port_from_env.or(file.port).unwrap_or(crate::DEFAULT_PORT),
            listen: var("ALONDRA_LISTEN").and_then(|v| v.parse().ok()).or(file.listen).unwrap_or([127, 0, 0, 1].into()),
            public_url: var("ALONDRA_PUBLIC_URL").or_else(|| file.public_url.clone()).filter(|u| !u.trim().is_empty()),
            legacy_bridge: var("ALONDRA_LEGACY_BRIDGE").map(flag).or(file.legacy_bridge).unwrap_or(false),
            wake,
            retention_hours: var("ALONDRA_RETENTION_HOURS")
                .and_then(|v| v.parse().ok())
                .or(file.retention_hours)
                .unwrap_or(DEFAULT_RETENTION_HOURS),
            service_label: var("ALONDRA_SERVICE_LABEL").or_else(|| file.service_label.clone()).unwrap_or_else(|| DEFAULT_LABEL.into()),
            sandbox: var("ALONDRA_SANDBOX").is_some_and(flag),
            fast_timing: var("ALONDRA_SANDBOX").is_some_and(flag) && var("ALONDRA_SANDBOX_TIMING").as_deref() == Some("fast"),
            ignore_mac_load: var("ALONDRA_IGNORE_MAC_LOAD").is_some_and(flag),
            resume_dead_nodes: var("ALONDRA_RESUME_DEAD_NODES").map(flag).or(file.resume_dead_nodes).unwrap_or(true),
            close_free_nodes_after: var("ALONDRA_CLOSE_FREE_NODES_AFTER")
                .and_then(|v| parse_switchable_duration(&v).ok())
                .or_else(|| file.close_free_nodes_after.as_deref().and_then(|v| parse_switchable_duration(v).ok()))
                .unwrap_or(Some(DEFAULT_CLOSE_FREE_NODES_AFTER)),
            compact_idle_nodes_at: Some(
                var("ALONDRA_COMPACT_IDLE_NODES_AT")
                    .and_then(|v| if v.eq_ignore_ascii_case(OFF) { Some(0.0) } else { v.parse::<f64>().ok().filter(|p| (0.0..=100.0).contains(p)) })
                    .or(file.compact_idle_nodes_at)
                    .unwrap_or(DEFAULT_COMPACT_IDLE_NODES_AT),
            )
            .filter(|p| *p > 0.0),
            compact_orchestrator_at: Some(
                var("ALONDRA_COMPACT_ORCHESTRATOR_AT")
                    .and_then(|v| if v.eq_ignore_ascii_case(OFF) { Some(0.0) } else { v.parse::<f64>().ok().filter(|p| (0.0..=100.0).contains(p)) })
                    .or(file.compact_orchestrator_at)
                    .unwrap_or(DEFAULT_COMPACT_ORCHESTRATOR_AT),
            ).filter(|p| *p > 0.0),
            device_expiry_days: Some(
                var("ALONDRA_DEVICE_EXPIRY_DAYS")
                    .and_then(|v| v.parse::<f64>().ok().filter(|d| d.is_finite() && *d >= 0.0))
                    .or(file.device_expiry_days)
                    .unwrap_or(DEFAULT_DEVICE_EXPIRY_DAYS),
            )
            .filter(|days| *days > 0.0),
            auto_update: var("ALONDRA_AUTO_UPDATE").map(flag).or(file.auto_update).unwrap_or(false),
            home,
        }
    }
}

/// First word of the macOS account's full name, then the login name.
pub fn system_name() -> Option<String> {
    let full = std::process::Command::new("/usr/bin/id").arg("-F").output().ok().filter(|o| o.status.success());
    full.and_then(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().next().map(str::to_string))
        .or_else(|| std::env::var("USER").ok().filter(|u| !u.is_empty()))
}

/// The `config.toml` keys `alondra config set` changes, and whether the daemon reads each one only
/// when it starts, so changing it restarts the daemon.
pub const SETTABLE: &[(&str, bool)] = &[
    ("name", false),
    ("public_url", false),
    ("listen", true),
    ("port", true),
    ("wake", true),
    ("retention_hours", true),
    ("service_label", true),
];

pub fn restarts_daemon(key: &str) -> bool {
    SETTABLE.iter().any(|(k, restarts)| *k == key && *restarts)
}

/// Why `set_key` refused: the CLI says it in the user's language.
#[derive(Debug, PartialEq)]
pub enum SetError {
    UnknownKey,
    Invalid,
    /// The file is unreadable or could not be written.
    File(String),
}

fn typed(key: &str, value: &str) -> Result<toml::Value, SetError> {
    let value = value.trim();
    let text = || (!value.is_empty()).then(|| toml::Value::String(value.into())).ok_or(SetError::Invalid);
    match key {
        "name" => text(),
        "public_url" => (value.starts_with("http://") || value.starts_with("https://")).then(text).unwrap_or(Err(SetError::Invalid)),
        "service_label" => (!value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')))
            .then(text).unwrap_or(Err(SetError::Invalid)),
        "listen" => value.parse::<IpAddr>().map(|_| toml::Value::String(value.into())).map_err(|_| SetError::Invalid),
        "port" => value.parse::<u16>().ok().filter(|p| *p > 0).map(|p| toml::Value::Integer(p.into())).ok_or(SetError::Invalid),
        "wake" => value.parse::<bool>().map(toml::Value::Boolean).map_err(|_| SetError::Invalid),
        "retention_hours" => value.parse::<f64>().ok().filter(|h| h.is_finite() && *h > 0.0).map(toml::Value::Float).ok_or(SetError::Invalid),
        _ => Err(SetError::UnknownKey),
    }
}

/// Sets one of `SETTABLE` in `config.toml`, or takes it out with `None` so its default applies.
/// The rest of the file stays as it was; the new file replaces the old one in one rename.
pub fn set_key(home: &Path, key: &str, value: Option<&str>) -> Result<(), SetError> {
    if !SETTABLE.iter().any(|(k, _)| *k == key) {
        return Err(SetError::UnknownKey);
    }
    let path = path_in(home);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(SetError::File(format!("{}: {e}", path.display()))),
    };
    FileConfig::parse(&text).map_err(SetError::File)?;
    let mut table: toml::Table = toml::from_str(&text).map_err(|e| SetError::File(e.to_string()))?;
    match value {
        Some(value) => { table.insert(key.into(), typed(key, value)?); }
        None => { table.remove(key); }
    }
    let rendered = toml::to_string(&table).map_err(|e| SetError::File(e.to_string()))?;
    FileConfig::parse(&rendered).map_err(|_| SetError::Invalid)?;
    std::fs::create_dir_all(home).map_err(|e| SetError::File(e.to_string()))?;
    let next = path.with_extension(format!("toml.{}", std::process::id()));
    std::fs::write(&next, rendered).and_then(|()| std::fs::rename(&next, &path)).map_err(|e| SetError::File(format!("{}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    #[test]
    fn set_key_changes_one_key_keeps_the_rest_and_refuses_bad_values() {
        let dir = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*dir).unwrap();
        std::fs::write(path_in(&dir), "name = \"David\"\nauto_update = true\n").unwrap();
        set_key(&dir, "port", Some("47900")).unwrap();
        set_key(&dir, "listen", Some("0.0.0.0")).unwrap();
        set_key(&dir, "name", None).unwrap();
        let file = FileConfig::parse(&std::fs::read_to_string(path_in(&dir)).unwrap()).unwrap();
        assert_eq!((file.port, file.listen, file.name.clone(), file.auto_update), (Some(47900), Some("0.0.0.0".parse().unwrap()), None, Some(true)));
        for (key, bad) in [("port", "0"), ("port", "web"), ("listen", "home"), ("wake", "yes"), ("retention_hours", "-1"), ("public_url", "mac.local"), ("service_label", "a b"), ("name", " ")] {
            assert_eq!(set_key(&dir, key, Some(bad)), Err(SetError::Invalid), "{key} = {bad}");
        }
        assert_eq!(set_key(&dir, "auto_update", Some("false")), Err(SetError::UnknownKey));
        assert_eq!(set_key(&dir, "max_working_nodes", Some("4")), Err(SetError::UnknownKey));
        assert_eq!(FileConfig::parse(&std::fs::read_to_string(path_in(&dir)).unwrap()).unwrap(), file, "a refused value writes nothing");
        assert!(std::fs::read_dir(&dir).unwrap().count() == 1, "no temporary file is left");
        assert!(restarts_daemon("port") && restarts_daemon("wake") && !restarts_daemon("name") && !restarts_daemon("public_url"));
    }

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| pairs.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.clone())
    }

    fn resolve(file: &FileConfig, vars: &[(&str, &str)]) -> Config {
        Config::resolve(PathBuf::from("/h/.alondra"), file, env(vars), || Some("Ana".into()))
    }

    #[test]
    fn zero_config_is_local_and_quiet() {
        let c = resolve(&FileConfig::default(), &[]);
        assert_eq!(c.name, "Ana");
        assert_eq!(c.port, crate::DEFAULT_PORT);
        assert_eq!(c.listen.to_string(), "127.0.0.1");
        assert_eq!(c.public_url, None);
        assert!(!c.legacy_bridge && !c.wake);
        assert_eq!(c.retention_hours, DEFAULT_RETENTION_HOURS);
        assert_eq!(c.service_label, DEFAULT_LABEL);
        let nameless = Config::resolve(PathBuf::new(), &FileConfig::default(), env(&[]), || None);
        assert_eq!(nameless.name, "the user");
    }

    #[test]
    fn env_beats_file_beats_default() {
        let file = FileConfig::parse(
            "name = \"Mostafa\"\nport = 47801\nlisten = \"0.0.0.0\"\npublic_url = \"https://a.example\"\nlegacy_bridge = true\nretention_hours = 24.0\nservice_label = \"x.y\"\n",
        )
        .unwrap();
        let c = resolve(&file, &[]);
        assert_eq!((c.name.as_str(), c.port, c.listen.to_string().as_str()), ("Mostafa", 47801, "0.0.0.0"));
        assert_eq!(c.public_url.as_deref(), Some("https://a.example"));
        assert!(c.legacy_bridge);
        assert_eq!((c.retention_hours, c.service_label.as_str()), (24.0, "x.y"));

        let c = resolve(
            &file,
            &[
                ("ALONDRA_NAME", "Bea"),
                ("ALONDRA_PORT", "47897"),
                ("ALONDRA_LISTEN", "127.0.0.1"),
                ("ALONDRA_PUBLIC_URL", "https://b.example"),
                ("ALONDRA_LEGACY_BRIDGE", "0"),
                ("ALONDRA_RETENTION_HOURS", "0.5"),
            ],
        );
        assert_eq!((c.name.as_str(), c.port, c.listen.to_string().as_str()), ("Bea", 47897, "127.0.0.1"));
        assert_eq!(c.public_url.as_deref(), Some("https://b.example"));
        assert!(!c.legacy_bridge);
        assert_eq!(c.retention_hours, 0.5);
    }

    #[test]
    fn empty_or_broken_env_values_fall_through() {
        let file = FileConfig { port: Some(47801), public_url: Some("https://a.example".into()), ..Default::default() };
        let c = resolve(&file, &[("ALONDRA_PORT", "nope"), ("ALONDRA_PUBLIC_URL", " "), ("ALONDRA_NAME", "")]);
        assert_eq!((c.port, c.public_url.as_deref(), c.name.as_str()), (47801, Some("https://a.example"), "Ana"));
    }

    #[test]
    fn only_an_explicit_env_makes_a_sandbox() {
        assert!(!resolve(&FileConfig::default(), &[]).sandbox);
        assert!(!resolve(&FileConfig::default(), &[("ALONDRA_PORT", "47891")]).sandbox);
        assert!(resolve(&FileConfig::default(), &[("ALONDRA_SANDBOX", "1")]).sandbox);
        assert!(!resolve(&FileConfig::default(), &[("ALONDRA_SANDBOX", "0")]).sandbox);
    }

    #[test]
    fn only_an_explicit_env_frees_node_starts_from_the_macs_load() {
        assert!(!resolve(&FileConfig::default(), &[]).ignore_mac_load);
        assert!(!resolve(&FileConfig::default(), &[("ALONDRA_IGNORE_MAC_LOAD", "0")]).ignore_mac_load);
        assert!(resolve(&FileConfig::default(), &[("ALONDRA_IGNORE_MAC_LOAD", "1")]).ignore_mac_load);
    }

    #[test]
    fn fast_timing_is_only_for_a_sandbox() {
        assert!(!resolve(&FileConfig::default(), &[("ALONDRA_SANDBOX_TIMING", "fast")]).fast_timing);
        assert!(!resolve(&FileConfig::default(), &[("ALONDRA_SANDBOX", "0"), ("ALONDRA_SANDBOX_TIMING", "fast")]).fast_timing);
        assert!(!resolve(&FileConfig::default(), &[("ALONDRA_SANDBOX", "1")]).fast_timing);
        assert!(resolve(&FileConfig::default(), &[("ALONDRA_SANDBOX", "1"), ("ALONDRA_SANDBOX_TIMING", "fast")]).fast_timing);
    }

    #[test]
    fn wake_needs_the_file_or_an_explicit_env_and_never_follows_a_port_override() {
        let on = FileConfig { wake: Some(true), ..Default::default() };
        assert!(resolve(&on, &[]).wake);
        assert!(!resolve(&on, &[("ALONDRA_PORT", "47897")]).wake);
        assert!(resolve(&on, &[("ALONDRA_PORT", "47897"), ("ALONDRA_WAKE", "1")]).wake);
        assert!(!resolve(&on, &[("ALONDRA_WAKE", "0")]).wake);
        assert!(resolve(&FileConfig::default(), &[("ALONDRA_WAKE", "1")]).wake);
        assert!(!resolve(&FileConfig::default(), &[]).wake);
    }

    #[test]
    fn unknown_keys_are_an_error_and_render_round_trips() {
        assert!(FileConfig::parse("nmae = \"x\"").is_err());
        let file = FileConfig { name: Some("Ana".into()), listen: Some([0, 0, 0, 0].into()), wake: Some(true), ..Default::default() };
        assert_eq!(FileConfig::parse(&file.render()).unwrap(), file);
        assert_eq!(FileConfig::default().render(), "");
    }

    #[test]
    fn overlay_only_replaces_what_is_set() {
        let base = FileConfig { name: Some("Ana".into()), port: Some(1), ..Default::default() };
        let over = FileConfig { port: Some(2), wake: Some(true), ..Default::default() };
        assert_eq!(base.overlay(&over), FileConfig { name: Some("Ana".into()), port: Some(2), wake: Some(true), ..Default::default() });
    }

    #[test]
    fn dead_nodes_are_resumed_and_finished_tasks_nodes_closed_after_30_minutes_unless_the_config_says_otherwise() {
        let c = resolve(&FileConfig::default(), &[]);
        assert!(c.resume_dead_nodes);
        assert_eq!(c.close_free_nodes_after, Some(Duration::from_secs(1_800)));
        assert_eq!(resolve(&FileConfig::parse("close_free_nodes_after = \"off\"").unwrap(), &[]).close_free_nodes_after, None);
        assert_eq!(resolve(&FileConfig::default(), &[("ALONDRA_CLOSE_FREE_NODES_AFTER", "off")]).close_free_nodes_after, None);

        let file = FileConfig::parse("resume_dead_nodes = false\nclose_free_nodes_after = \"2h\"\n").unwrap();
        let c = resolve(&file, &[]);
        assert!(!c.resume_dead_nodes);
        assert_eq!(c.close_free_nodes_after, Some(Duration::from_secs(7200)));
        let c = resolve(&file, &[("ALONDRA_RESUME_DEAD_NODES", "1"), ("ALONDRA_CLOSE_FREE_NODES_AFTER", "30m")]);
        assert!(c.resume_dead_nodes);
        assert_eq!(c.close_free_nodes_after, Some(Duration::from_secs(1800)));
        let c = resolve(&file, &[("ALONDRA_CLOSE_FREE_NODES_AFTER", "soon")]);
        assert_eq!(c.close_free_nodes_after, Some(Duration::from_secs(7200)), "a broken env value falls through to the file");

        assert!(FileConfig::parse("close_free_nodes_after = \"2 hours\"").unwrap_err().contains("close_free_nodes_after"), "a typo in the file is an error, not silently off");
        assert_eq!(FileConfig::parse(&file.render()).unwrap(), file);
    }

    #[test]
    fn idle_nodes_are_compacted_from_thirty_percent_unless_the_config_says_otherwise() {
        assert_eq!(resolve(&FileConfig::default(), &[]).compact_idle_nodes_at, Some(30.0));
        let file = FileConfig::parse("compact_idle_nodes_at = 50\n").unwrap();
        assert_eq!(resolve(&file, &[]).compact_idle_nodes_at, Some(50.0));
        assert_eq!(resolve(&file, &[("ALONDRA_COMPACT_IDLE_NODES_AT", "20")]).compact_idle_nodes_at, Some(20.0));
        assert_eq!(resolve(&file, &[("ALONDRA_COMPACT_IDLE_NODES_AT", "off")]).compact_idle_nodes_at, None);
        assert_eq!(resolve(&file, &[("ALONDRA_COMPACT_IDLE_NODES_AT", "lots")]).compact_idle_nodes_at, Some(50.0), "a broken env value falls through to the file");
        assert_eq!(resolve(&FileConfig::parse("compact_idle_nodes_at = 0").unwrap(), &[]).compact_idle_nodes_at, None);
        assert!(FileConfig::parse("compact_idle_nodes_at = 130").unwrap_err().contains("compact_idle_nodes_at"));
        assert_eq!(FileConfig::parse(&file.render()).unwrap(), file);
    }

    #[test]
    fn devices_expire_after_thirty_days_and_updates_wait_unless_the_config_says_otherwise() {
        assert!(FileConfig::parse("archived_tasks_days = 7\n").is_ok(), "a file from before clearing replaced archiving still loads");
        assert_eq!(resolve(&FileConfig::default(), &[]).device_expiry_days, Some(DEFAULT_DEVICE_EXPIRY_DAYS));
        let file = FileConfig::parse("device_expiry_days = 0\n").unwrap();
        assert_eq!(resolve(&file, &[]).device_expiry_days, None, "0 turns it off");
        assert_eq!(resolve(&file, &[("ALONDRA_DEVICE_EXPIRY_DAYS", "0.5")]).device_expiry_days, Some(0.5));
        assert!(FileConfig::parse("device_expiry_days = -1\n").is_err());
        assert!(!resolve(&FileConfig::default(), &[]).auto_update, "updates are only installed when asked");
        let file = FileConfig::parse("auto_update = true\n").unwrap();
        assert!(resolve(&file, &[]).auto_update);
        assert!(!resolve(&file, &[("ALONDRA_AUTO_UPDATE", "0")]).auto_update);
        assert_eq!(FileConfig::parse(&file.render()).unwrap(), file);
    }

    #[test]
    fn orchestrator_compaction_has_an_independent_validated_threshold() {
        assert_eq!(resolve(&FileConfig::default(), &[]).compact_orchestrator_at, Some(50.0));
        let file = FileConfig::parse("compact_orchestrator_at = 70").unwrap();
        assert_eq!(resolve(&file, &[]).compact_orchestrator_at, Some(70.0));
        assert_eq!(resolve(&file, &[("ALONDRA_COMPACT_ORCHESTRATOR_AT", "40")]).compact_orchestrator_at, Some(40.0));
        assert_eq!(resolve(&file, &[("ALONDRA_COMPACT_ORCHESTRATOR_AT", "off")]).compact_orchestrator_at, None);
        assert!(FileConfig::parse("compact_orchestrator_at = 101").is_err());
        assert!(FileConfig::parse("compact_orchestrator_at = nan").is_err());
        assert_eq!(FileConfig::parse(&file.render()).unwrap(), file);
    }

    #[test]
    fn durations_are_a_positive_number_and_a_unit() {
        let cases = [("90s", Some(90)), ("30m", Some(1800)), (" 2h ", Some(7200)), ("1.5d", Some(129_600)), ("2", None), ("0h", None), ("-1h", None), ("h", None), ("", None), ("2 hours", None)];
        for (text, secs) in cases {
            assert_eq!(parse_duration(text).ok().map(|d| d.as_secs()), secs, "{text:?}");
        }
    }

    #[test]
    fn alondra_home_follows_the_env() {
        assert_eq!(alondra_home(&env(&[("HOME", "/u")])), PathBuf::from("/u/.alondra"));
        assert_eq!(alondra_home(&env(&[("HOME", "/u"), ("ALONDRA_HOME", "/s")])), PathBuf::from("/s"));
    }
}
