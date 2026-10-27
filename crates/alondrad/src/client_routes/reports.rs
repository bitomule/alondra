//! A crash report from a screen: the daemon has its own CLI build it (`alondra report`), hands
//! out the files the preview shows, exactly as they would leave the Mac, and sends it when the
//! user says so (`alondra report send`). Nothing is sent without that last request.
use super::{Client, Reply};
use alondra_core::domain::DomainError;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

const BUILD: Duration = Duration::from_secs(5 * 60);
const SEND: Duration = Duration::from_secs(15 * 60);

/// The kinds of failure a report's issue names, as `alondra report send --kind` takes them.
pub(crate) const FAILURES: [&str; 4] = ["session-ended", "message-not-delivered", "not-resumed", "other"];

/// The files of a report a screen may read: what the package holds, never what stays on the Mac
/// (the pseudonyms' originals, how sending goes).
fn shown_file(dir: &Path, name: &str) -> bool {
    if name == "manifest.json" {
        return true;
    }
    let Ok(manifest) = std::fs::read(dir.join("manifest.json")) else { return false };
    let Ok(manifest) = serde_json::from_slice::<Value>(&manifest) else { return false };
    (name == "description.txt" && manifest["description"].is_object())
        || manifest["sections"].as_array().into_iter().flatten().any(|section| section["name"] == name && section["sha256"].is_string())
}

fn invalid(why: &str) -> Reply {
    Reply::error(DomainError::new("invalid_input", why))
}

fn failure_of(body: &Value) -> Result<&str, Reply> {
    match body.get("failure") {
        None | Some(Value::Null) => Ok("other"),
        Some(Value::String(word)) if FAILURES.contains(&word.as_str()) => Ok(FAILURES.iter().find(|f| **f == word).unwrap()),
        Some(_) => Err(invalid("unknown kind of failure")),
    }
}

fn lang_of(body: &Value) -> &'static str {
    if body.get("lang").and_then(Value::as_str) == Some("es") { "es" } else { "en" }
}

/// What the CLI printed, as the document it is, or why there is none.
async fn run(cli: PathBuf, home: &Path, lang: &str, args: Vec<String>, limit: Duration) -> Result<Value, Reply> {
    let mut command = tokio::process::Command::new(cli);
    command.args(&args).env("ALONDRA_HOME", home).env("ALONDRA_LANG", lang)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(limit, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            log!("could not run alondra report: {error}");
            return Err(Reply::error(DomainError::new("unavailable", "Alondra could not build the report")));
        }
        Err(_) => return Err(Reply::error(DomainError::new("unavailable", "The report took too long"))),
    };
    match serde_json::from_slice::<Value>(&output.stdout) {
        Ok(document) if output.status.success() => Ok(document),
        _ => {
            let why = String::from_utf8_lossy(&output.stderr);
            let why = why.trim().trim_start_matches("Error: ").lines().next().unwrap_or_default().to_owned();
            Err(Reply::error(DomainError::new("unavailable", if why.is_empty() { "Alondra could not build the report".to_owned() } else { why })))
        }
    }
}

impl Client {
    fn report_cli(&self) -> Result<PathBuf, Reply> {
        self.report_cli.lock().expect("report cli poisoned").clone()
            .ok_or_else(|| Reply::error(DomainError::new("unavailable", "Alondra's command line is not beside the daemon")))
    }

    #[cfg(test)]
    pub(crate) fn use_report_cli(&self, cli: PathBuf) {
        *self.report_cli.lock().expect("report cli poisoned") = Some(cli);
    }

    /// `POST /reports`: builds a new report, with `description` as the user wrote it, and answers
    /// its summary, where it would be sent and the kind of failure it goes as.
    pub(super) async fn new_report(&self, body: &Value) -> Reply {
        let (failure, cli) = match (failure_of(body), self.report_cli()) {
            (Ok(failure), Ok(cli)) => (failure, cli),
            (Err(reply), _) | (_, Err(reply)) => return reply,
        };
        let mut args: Vec<String> = ["--json", "--no-input", "report"].map(str::to_owned).into();
        match body.get("description") {
            None | Some(Value::Null) => {}
            Some(Value::String(text)) if text.len() <= 16 * 1024 => {
                if !text.trim().is_empty() {
                    args.extend(["--description".to_owned(), text.clone()]);
                }
            }
            Some(_) => return invalid("invalid description"),
        }
        match run(cli, &self.home, lang_of(body), args, BUILD).await {
            Ok(mut summary) => {
                summary["failure"] = json!(failure);
                Reply::ok(summary)
            }
            Err(reply) => reply,
        }
    }

    /// `GET /reports/<id>/files/<name>`: one file of the package, whole.
    pub(super) fn report_file(&self, id: &str, name: &str) -> axum::response::Response {
        use axum::response::IntoResponse;
        let dir = self.home.join("reports").join(id);
        if !crate::reports::is_report_id(id) || !shown_file(&dir, name) {
            return invalid("not a file of the report").into_response();
        }
        match std::fs::read_to_string(dir.join(name)) {
            Ok(text) => axum::Json(json!({"id": id, "name": name, "text": text})).into_response(),
            Err(_) => Reply::error(DomainError::new("not_found", "That report is no longer on this Mac")).into_response(),
        }
    }

    /// `POST /reports/<id>/send`: sends a report already previewed, as the kind of failure given.
    pub(super) async fn send_report(&self, id: &str, body: &Value) -> Reply {
        let (failure, cli) = match (failure_of(body), self.report_cli()) {
            (Ok(failure), Ok(cli)) => (failure, cli),
            (Err(reply), _) | (_, Err(reply)) => return reply,
        };
        let args = ["--json", "--no-input", "report", "send", id, "--kind", failure].map(str::to_owned).into();
        match run(cli, &self.home, lang_of(body), args, SEND).await {
            Ok(outcome) => Reply::ok(outcome),
            Err(reply) => reply,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        let manifest = json!({"sections": [{"name": "daemon.log", "bytes": 4, "sha256": "x"}, {"name": "nodes.jsonl", "empty": "nothing to report"}], "description": {"bytes": 3, "sha256": "y"}});
        std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
        for name in ["daemon.log", "description.txt", "pseudonyms.json", "sending.json", "nodes.jsonl"] {
            std::fs::write(dir.join(name), name).unwrap();
        }
    }

    #[test]
    fn a_screen_reads_only_the_files_that_leave_the_mac() {
        let home = crate::test_support::Directory::new();
        let dir = home.join("reports/r-20261027-0000abcd");
        report(&dir);
        for name in ["manifest.json", "daemon.log", "description.txt"] {
            assert!(shown_file(&dir, name), "{name}");
        }
        for name in ["pseudonyms.json", "sending.json", "nodes.jsonl", "../manifest.json", ""] {
            assert!(!shown_file(&dir, name), "{name}");
        }
    }

    #[test]
    fn the_failure_is_other_unless_a_known_one_is_named() {
        assert_eq!(failure_of(&json!({})).ok(), Some("other"));
        assert_eq!(failure_of(&json!({"failure": null})).ok(), Some("other"));
        for word in FAILURES {
            assert_eq!(failure_of(&json!({"failure": word})).ok(), Some(word));
        }
        assert!(failure_of(&json!({"failure": "crash"})).is_err());
        assert!(failure_of(&json!({"failure": 3})).is_err());
    }
}
