use std::path::{Path, PathBuf};
use std::time::Duration;

use alondra_core::views::VIEWS_PROTOCOL;
use anyhow::Result;
use serde_json::{Value, json};

use crate::client::Client;

const CLI: &str = env!("CARGO_PKG_VERSION");

/// One `GET /daemon` with a short limit: knowing the version never depends on the daemon, so a
/// daemon that does not answer is reported, never an error.
pub fn run(as_json: bool) -> Result<()> {
    let daemon = Client::new(Duration::from_millis(500)).get::<Value>("/daemon").ok();
    let path = std::env::current_exe().and_then(|p| p.canonicalize()).unwrap_or_default();
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if as_json {
        println!("{}", report(daemon.as_ref(), &path, &home));
    } else {
        println!("{}", line(service_version(daemon.as_ref())));
    }
    Ok(())
}

fn service_version(daemon: Option<&Value>) -> Option<&str> {
    daemon.and_then(|d| d["version"].as_str())
}

/// The comparison the terminal app also uses to notice it is behind its daemon.
pub fn same(cli: &str, service: &str) -> bool {
    cli.trim() == service.trim()
}

pub fn install(path: &Path, home: &Path) -> &'static str {
    if alondra_core::update::brew_prefix(path).is_some() {
        "homebrew"
    } else if path.parent() == Some(&home.join(".local/bin")) {
        "source"
    } else {
        "other"
    }
}

fn line(service: Option<&str>) -> String {
    match service {
        None => trf!("alondra {CLI} · service not running", "alondra {CLI} · servicio parado"),
        Some(service) if alondra_core::update::newer(service, CLI) => trf!(
            "alondra {CLI} · service {service} — this terminal is older: run `alondra update`",
            "alondra {CLI} · servicio {service}: este terminal es más antiguo; ejecuta `alondra update`"
        ),
        Some(service) => trf!("alondra {CLI} · service {service}", "alondra {CLI} · servicio {service}"),
    }
}

fn report(daemon: Option<&Value>, path: &Path, home: &Path) -> Value {
    let mut service = json!({"reachable": daemon.is_some()});
    if let Some(version) = service_version(daemon) {
        service["version"] = json!(version);
    }
    if let Some(range) = daemon.map(|d| &d["views_protocol"]).filter(|r| r.is_object()) {
        service["views_protocol"] = range.clone();
    }
    json!({
        "schema": "alondra.version/1",
        "cli": {"version": CLI, "views_protocol": VIEWS_PROTOCOL, "install": install(path, home), "path": path},
        "service": service,
        "same_version": service_version(daemon).is_some_and(|v| same(CLI, v)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Lang, set_for_this_thread};

    #[test]
    fn a_daemon_that_does_not_answer_is_reported_without_a_version() {
        let doc = report(None, Path::new("/usr/local/bin/alondra"), Path::new("/Users/me"));
        assert_eq!(doc["schema"], "alondra.version/1");
        assert_eq!(doc["cli"]["version"], CLI);
        assert_eq!(doc["cli"]["views_protocol"], VIEWS_PROTOCOL);
        assert_eq!(doc["service"], json!({"reachable": false}));
        assert_eq!(doc["same_version"], false);
    }

    #[test]
    fn a_daemon_that_answers_gives_its_version_and_protocol_range() {
        let daemon = json!({"version": CLI, "views_protocol": {"min": 1, "max": 1}});
        let doc = report(Some(&daemon), Path::new("/x/alondra"), Path::new("/Users/me"));
        assert_eq!(doc["service"], json!({"reachable": true, "version": CLI, "views_protocol": {"min": 1, "max": 1}}));
        assert_eq!(doc["same_version"], true);
        let older = json!({"version": "0.0.1"});
        assert_eq!(report(Some(&older), Path::new("/x/alondra"), Path::new("/Users/me"))["same_version"], false);
    }

    #[test]
    fn install_is_read_from_where_the_running_binary_lives() {
        let root = crate::test_support::Directory::new();
        let keg = root.join("homebrew/Cellar/alondra/0.4.0/bin");
        std::fs::create_dir_all(&keg).unwrap();
        std::fs::write(keg.join("alondra"), "").unwrap();
        let home = root.join("home");
        assert_eq!(install(&keg.join("alondra"), &home), "homebrew");
        assert_eq!(install(&home.join(".local/bin/alondra"), &home), "source");
        assert_eq!(install(Path::new("/opt/elsewhere/alondra"), &home), "other");
    }

    #[test]
    fn the_line_says_which_side_is_behind_in_the_user_language() {
        set_for_this_thread(Lang::En);
        assert_eq!(line(Some(CLI)), format!("alondra {CLI} · service {CLI}"));
        assert_eq!(line(None), format!("alondra {CLI} · service not running"));
        assert_eq!(line(Some("999.0.0")), format!("alondra {CLI} · service 999.0.0 — this terminal is older: run `alondra update`"));
        set_for_this_thread(Lang::Es);
        assert_eq!(line(None), format!("alondra {CLI} · servicio parado"));
    }
}
