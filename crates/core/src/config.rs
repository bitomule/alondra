use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "config.toml";
pub const DEFAULT_LABEL: &str = "dev.alondra.alondrad";
pub const DEFAULT_RETENTION_HOURS: f64 = 14.0 * 24.0;
pub const DEFAULT_ARCHIVED_TASKS_DAYS: f64 = 30.0;
pub const DEFAULT_CLOSE_FREE_NODES_AFTER: Duration = Duration::from_secs(7 * 24 * 3600);
pub const DEFAULT_COMPACT_IDLE_NODES_AT: f64 = 30.0;
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
    /// How long a node stays idle before Alondra closes it, as `30m`, `2h` or `7d`, or `off`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_free_nodes_after: Option<String>,
    /// How full an idle node's context may be, in percent, before Alondra compacts it to give it
    /// a new task. 0 turns it off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compact_idle_nodes_at: Option<f64>,
    /// Days an archived task is kept before Alondra forgets it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_tasks_days: Option<f64>,
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
        take!(name, port, listen, public_url, legacy_bridge, wake, retention_hours, service_label, resume_dead_nodes, close_free_nodes_after, compact_idle_nodes_at, archived_tasks_days);
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
    /// Restart a node whose process ended while it still had work. Only with `wake`.
    pub resume_dead_nodes: bool,
    /// Close a node once it has been idle this long with nothing pending, or ask the user when
    /// something is. A week unless set; `None` is off.
    pub close_free_nodes_after: Option<Duration>,
    /// Compact an idle node whose context is at least this full, in percent, before typing it
    /// a new task. 30 unless set; `None` is off.
    pub compact_idle_nodes_at: Option<f64>,
    pub archived_tasks_days: f64,
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
            archived_tasks_days: var("ALONDRA_ARCHIVED_TASKS_DAYS")
                .and_then(|v| v.parse().ok())
                .or(file.archived_tasks_days)
                .unwrap_or(DEFAULT_ARCHIVED_TASKS_DAYS),
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

#[cfg(test)]
mod tests {
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
    fn dead_nodes_are_resumed_and_idle_nodes_closed_after_a_week_unless_the_config_says_otherwise() {
        let c = resolve(&FileConfig::default(), &[]);
        assert!(c.resume_dead_nodes);
        assert_eq!(c.close_free_nodes_after, Some(Duration::from_secs(7 * 86_400)));
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
    fn archived_tasks_are_kept_thirty_days_unless_the_config_says_otherwise() {
        assert_eq!(resolve(&FileConfig::default(), &[]).archived_tasks_days, DEFAULT_ARCHIVED_TASKS_DAYS);
        let file = FileConfig::parse("archived_tasks_days = 7\n").unwrap();
        assert_eq!(resolve(&file, &[]).archived_tasks_days, 7.0);
        assert_eq!(resolve(&file, &[("ALONDRA_ARCHIVED_TASKS_DAYS", "0.5")]).archived_tasks_days, 0.5);
        assert_eq!(resolve(&file, &[("ALONDRA_ARCHIVED_TASKS_DAYS", "soon")]).archived_tasks_days, 7.0, "a broken env value falls through to the file");
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
