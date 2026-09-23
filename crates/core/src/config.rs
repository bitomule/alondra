use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "config.toml";
pub const DEFAULT_LABEL: &str = "dev.alondra.alondrad";
pub const DEFAULT_RETENTION_HOURS: f64 = 14.0 * 24.0;

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
}

impl FileConfig {
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    pub fn render(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    /// Keys set in `over` replace the ones in `self`.
    pub fn overlay(mut self, over: &FileConfig) -> FileConfig {
        macro_rules! take {
            ($($f:ident),*) => { $( if over.$f.is_some() { self.$f = over.$f.clone(); } )* };
        }
        take!(name, port, listen, public_url, legacy_bridge, wake, retention_hours, service_label);
        self
    }
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
    fn alondra_home_follows_the_env() {
        assert_eq!(alondra_home(&env(&[("HOME", "/u")])), PathBuf::from("/u/.alondra"));
        assert_eq!(alondra_home(&env(&[("HOME", "/u"), ("ALONDRA_HOME", "/s")])), PathBuf::from("/s"));
    }
}
