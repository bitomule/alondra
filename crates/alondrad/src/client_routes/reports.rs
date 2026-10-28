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
/// What an app's section may weigh, as `alondra report --app-section` takes it.
const APP_SECTION: usize = 512 * 1024;

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

/// The most a page of a report's file holds, so its answer fits in a sealed reply to a phone even
/// when every character of it is escaped.
const FILE_PAGE: usize = 256 * 1024;

/// The text from `offset` up to the last whole line that fits in a page, and where the next page
/// starts, if there is more.
fn page(text: &str, offset: usize) -> Option<(&str, Option<usize>)> {
    if offset > text.len() || !text.is_char_boundary(offset) {
        return None;
    }
    let mut end = (offset + FILE_PAGE).min(text.len());
    if end < text.len() {
        match text[offset..end].rfind('\n') {
            Some(newline) => end = offset + newline + 1,
            None => while !text.is_char_boundary(end) { end -= 1 },
        }
    }
    Some((&text[offset..end], (end < text.len()).then_some(end)))
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

/// The section an app adds to its report (`apps.jsonl`), already redacted by the app; the CLI
/// keeps it only as JSON lines and runs its final check on it like on the rest.
fn app_section_of(body: &Value) -> Result<Option<String>, Reply> {
    match body.get("app_section") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.len() <= APP_SECTION => Ok(Some(text.clone()).filter(|t| !t.trim().is_empty())),
        Some(_) => Err(invalid("invalid app section")),
    }
}

fn lang_of(body: &Value) -> &'static str {
    if body.get("lang").and_then(Value::as_str) == Some("es") { "es" } else { "en" }
}

/// What the CLI printed, as the document it is, or why there is none.
async fn run(cli: PathBuf, home: &Path, lang: &str, args: Vec<String>, input: Option<String>, limit: Duration) -> Result<Value, Reply> {
    use tokio::io::AsyncWriteExt;
    let mut command = tokio::process::Command::new(cli);
    command.args(&args).env("ALONDRA_HOME", home).env("ALONDRA_LANG", lang)
        .stdin(if input.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() })
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let finished = async {
        let mut child = command.spawn()?;
        if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin.write_all(input.as_bytes()).await?;
        }
        child.wait_with_output().await
    };
    let output = match tokio::time::timeout(limit, finished).await {
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
        let app = match app_section_of(body) {
            Ok(app) => app,
            Err(reply) => return reply,
        };
        if app.is_some() {
            args.extend(["--app-section".to_owned(), "-".to_owned()]);
        }
        match run(cli, &self.home, lang_of(body), args, app, BUILD).await {
            Ok(mut summary) => {
                summary["failure"] = json!(failure);
                Reply::ok(summary)
            }
            Err(reply) => reply,
        }
    }

    /// `GET /reports/<id>/files/<name>`: one file of the package, whole; with `?offset=` one page of
    /// it from there, ending at a line, for a screen whose answers cannot hold a long log.
    pub(super) fn report_file(&self, id: &str, name: &str, offset: Option<usize>) -> axum::response::Response {
        use axum::response::IntoResponse;
        let dir = self.home.join("reports").join(id);
        if !crate::reports::is_report_id(id) || !shown_file(&dir, name) {
            return invalid("not a file of the report").into_response();
        }
        match std::fs::read_to_string(dir.join(name)) {
            Ok(text) => match offset {
                None => axum::Json(json!({"id": id, "name": name, "text": text})).into_response(),
                Some(offset) => match page(&text, offset) {
                    Some((page, next)) => axum::Json(json!({"id": id, "name": name, "text": page, "offset": offset, "next": next, "bytes": text.len()})).into_response(),
                    None => invalid("not a place in the file").into_response(),
                },
            },
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
        match run(cli, &self.home, lang_of(body), args, None, SEND).await {
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
    fn a_long_file_is_read_in_pages_that_end_at_a_line_and_add_up_to_it() {
        let line = format!("{}ñ\n", "x".repeat(999));
        let text = line.repeat(700);
        let mut offset = 0;
        let mut joined = String::new();
        loop {
            let (part, next) = page(&text, offset).unwrap();
            assert!(part.len() <= FILE_PAGE && part.ends_with('\n'));
            joined.push_str(part);
            match next {
                Some(next) => offset = next,
                None => break,
            }
        }
        assert_eq!(joined, text);
        assert_eq!(page("short", 0), Some(("short", None)));
        assert_eq!(page("short", 5), Some(("", None)));
        assert_eq!(page("short", 6), None);
        assert_eq!(page("ñ", 1), None, "never in the middle of a character");
        let one_line = "y".repeat(FILE_PAGE + 10);
        assert_eq!(page(&one_line, 0).unwrap().1, Some(FILE_PAGE));
    }

    #[test]
    fn an_app_section_is_text_of_at_most_512_kilobytes() {
        assert_eq!(app_section_of(&json!({})).ok(), Some(None));
        assert_eq!(app_section_of(&json!({"app_section": " \n"})).ok(), Some(None));
        assert_eq!(app_section_of(&json!({"app_section": "{}\n"})).ok(), Some(Some("{}\n".to_owned())));
        assert!(app_section_of(&json!({"app_section": "x".repeat(APP_SECTION + 1)})).is_err());
        assert!(app_section_of(&json!({"app_section": ["{}"]})).is_err());
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
