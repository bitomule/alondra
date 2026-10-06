//! Explicit inspection/resolution of the domain's durable delivery evidence. Never retries a POST.
use crate::client::Client;
use alondra_core::delivery::State;
use alondra_core::domain::{DataEpoch, PROTOCOL_VERSION};
use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

pub const ATTEMPTS: &str = "/delivery-attempts";

#[derive(Args)]
pub struct Options {
    /// Explicit approved destination; omitted means Local, never the last remote Mac.
    #[arg(id = "delivery_mac", long = "mac", global = true)]
    pub mac: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}
#[derive(Subcommand)]
pub enum Command {
    /// Inspect recorded attempts, their versions and recovery instructions.
    List,
    /// Allow another attempt after inspecting the destination terminal, agent and worktree.
    Retry {
        id: i64,
        #[arg(long)]
        expected_version: u64,
        #[arg(long, required = true)]
        terminal_checked: bool,
        #[arg(long, required = true)]
        confirm_duplicate_risk: bool,
    },
    /// Stop retrying this attempt; this does not undo an external effect.
    Discard {
        id: i64,
        #[arg(long)]
        expected_version: u64,
        #[arg(long, required = true)]
        terminal_checked: bool,
    },
}
/// One attempt to hand a domain message to its recipient; unknown states fail deserialization closed.
#[derive(Clone, Debug, Deserialize)]
pub struct Attempt {
    pub attempt_id: i64,
    pub message_id: i64,
    pub state: State,
    pub version: u64,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub recipient_kind: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub interaction_id: Option<String>,
}
#[derive(Deserialize)]
pub struct Listing {
    pub data_epoch: DataEpoch,
    pub attempts: Vec<Attempt>,
    #[serde(default)]
    pub next_after: i64,
    #[serde(default)]
    pub has_more: bool,
}
impl Attempt {
    pub fn blocks(&self) -> bool {
        matches!(self.state, State::Dispatching | State::Uncertain)
    }
}
pub fn state_text(state: State) -> &'static str {
    match state {
        State::Dispatching => tr!(
            "Dispatching: outcome pending; do not repeat",
            "En curso: resultado pendiente; no repitas"
        ),
        State::Uncertain => tr!(
            "Uncertain: inspect the terminal before deciding",
            "Incierto: revisa el terminal antes de decidir"
        ),
        State::Handed => tr!(
            "Handed to Claude; execution not confirmed",
            "Entregado a Claude; ejecución no confirmada"
        ),
        State::Retryable => tr!(
            "Retry permitted; execution not confirmed",
            "Reintento permitido; ejecución no confirmada"
        ),
        State::Discarded => tr!(
            "Discarded; previous effects are not undone",
            "Descartado; no deshace efectos anteriores"
        ),
    }
}
/// Used by every message consumer rather than treating HTTP 200 as completed execution.
pub fn response_notice(value: &Value, mac: Option<&str>) -> Option<String> {
    let raw = value.get("delivery_state")?;
    let text = serde_json::from_value::<State>(raw.clone())
        .map(state_text)
        .unwrap_or(tr!(
            "Unknown delivery outcome; inspect before repeating",
            "Resultado desconocido; revisa antes de repetir"
        ));
    Some(format!(
        "{} · {} list / :deliveries",
        text,
        command_prefix(mac)
    ))
}
fn command_prefix(mac: Option<&str>) -> String {
    match mac {
        Some(mac) => format!("alondra delivery --mac {mac}"),
        None => "alondra delivery".into(),
    }
}
/// Every page of the evidence, oldest first.
pub fn list(mut get: impl FnMut(&str) -> Result<Value>) -> Result<Listing> {
    let mut after = 0;
    let mut all: Option<Listing> = None;
    loop {
        let page: Listing = serde_json::from_value(get(&format!("{ATTEMPTS}?after={after}"))?)?;
        let (more, next) = (page.has_more, page.next_after);
        match all.as_mut() {
            None => all = Some(page),
            Some(all) => {
                ensure!(all.data_epoch == page.data_epoch, "the domain was reset while the list was read");
                all.attempts.extend(page.attempts);
            }
        }
        if !more || next <= after {
            return all.context("no delivery evidence");
        }
        after = next;
    }
}
pub fn details(attempts: &[Attempt], mac: Option<&str>) -> Vec<String> {
    let mut lines = vec![format!("{}: {}", tr!("Destination", "Destino"), mac.unwrap_or("Local")),
        tr!("Inspect this destination's terminal. Retry may duplicate effects; discard cannot undo them.", "Revisa el terminal de este destino. Reintentar puede duplicar efectos; descartar no los deshace.").into()];
    if attempts.is_empty() {
        lines.push(tr!("No recorded attempts", "Sin intentos registrados").into());
    }
    for a in attempts.iter().rev() {
        lines.push(String::new());
        let about = a.interaction_id.as_deref().or(a.task_id.as_deref()).map(|s| format!(" · {s}")).unwrap_or_default();
        lines.push(format!("#{} · v{} · {} · {}{about}", a.attempt_id, a.version, a.recipient_kind, a.message_id));
        lines.push(state_text(a.state).into());
        if let Some(reason) = &a.reason {
            lines.push(reason.clone());
        }
        if a.state == State::Uncertain {
            let prefix = command_prefix(mac);
            lines.push(
                tr!(
                    "After inspection, choose ONE command in your shell:",
                    "Tras revisar, elige UN comando en tu shell:"
                )
                .into(),
            );
            lines.push(format!("{prefix} retry {} --expected-version {} --terminal-checked --confirm-duplicate-risk", a.attempt_id, a.version));
            lines.push(format!(
                "{prefix} discard {} --expected-version {} --terminal-checked",
                a.attempt_id, a.version
            ));
        }
    }
    lines
}
#[derive(Deserialize)]
struct Resolved {
    attempt_id: i64,
    message_id: i64,
    version: u64,
    state: State,
}
fn resolve(
    id: i64,
    version: u64,
    retry: bool,
    checked: bool,
    risk: bool,
    mut get: impl FnMut(&str) -> Result<Value>,
    mut post: impl FnMut(&str, &Value) -> Result<Value>,
) -> Result<(i64, State)> {
    ensure!(
        checked && (!retry || risk),
        "Inspect the terminal and explicitly confirm duplicate risk before retrying"
    );
    let listing = list(&mut get)?;
    let attempt = listing
        .attempts
        .iter()
        .find(|a| a.attempt_id == id)
        .context("Delivery attempt no longer exists; refresh the list")?;
    ensure!(
        attempt.version == version && attempt.state == State::Uncertain,
        "Delivery attempt changed; refresh the list before deciding"
    );
    let body = json!({"protocol_version": PROTOCOL_VERSION, "data_epoch": listing.data_epoch, "expected_version": version, "decision": if retry { "retry" } else { "discard" }});
    // The server checks the version again atomically. A rejection/timeout is returned, never retried.
    let result: Resolved = serde_json::from_value(post(&format!("{ATTEMPTS}/{id}/resolve"), &body)?)?;
    let expected = if retry { State::Retryable } else { State::Discarded };
    ensure!(
        result.attempt_id == id
            && result.message_id == attempt.message_id
            && result.version == version.checked_add(1).context("Invalid delivery version")?
            && result.state == expected,
        "Unexpected delivery result; inspect before repeating"
    );
    Ok((id, result.state))
}
fn destination_client<T>(
    mac: Option<&str>,
    initialize: impl FnOnce(Option<&str>) -> Result<()>,
    capture: impl FnOnce(bool) -> T,
) -> Result<T> {
    if mac.is_some() {
        initialize(mac)?;
    }
    Ok(capture(mac.is_some()))
}
pub fn run(options: Options) -> Result<()> {
    // Capture once after successful approval lookup. No fallback on initialization/request errors.
    let client = destination_client(
        options.mac.as_deref(),
        crate::service_client::initialize,
        |remote| {
            if remote {
                Client::for_tui(Duration::from_secs(30))
            } else {
                Client::for_user(Duration::from_secs(30))
            }
        },
    )?;
    let get = |path: &str| client.get::<Value>(path);
    let post = |path: &str, body: &Value| client.post(path, body);
    let (id, state) = match options.command {
        Command::List => {
            for line in details(&list(get)?.attempts, options.mac.as_deref()) {
                println!("{line}");
            }
            return Ok(());
        }
        Command::Retry { id, expected_version, terminal_checked, confirm_duplicate_risk } => resolve(id, expected_version, true, terminal_checked, confirm_duplicate_risk, get, post)?,
        Command::Discard { id, expected_version, terminal_checked } => resolve(id, expected_version, false, terminal_checked, false, get, post)?,
    };
    println!("#{id}: {}", state_text(state));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn attempt(state: &str, version: u64) -> Value {
        json!({"attempt_id":7,"message_id":3,"state":state,"current_state":state,"version":version,"reason":null,"recipient_kind":"agent","task_id":"task-1","interaction_id":"answer-1"})
    }
    fn page(attempts: Vec<Value>) -> Value {
        json!({"protocol_version":2,"data_epoch":"e","revision":4,"attempts":attempts,"next_after":7,"has_more":false})
    }

    #[test]
    fn destination_is_explicit_and_failed_remote_never_constructs_local_client() {
        let local = destination_client(
            None,
            |_| panic!("Local must not load last remote"),
            |remote| {
                assert!(!remote);
                "local"
            },
        )
        .unwrap();
        assert_eq!(local, "local");
        let remote = destination_client(
            Some("mac-b"),
            |id| {
                assert_eq!(id, Some("mac-b"));
                Ok(())
            },
            |remote| {
                assert!(remote);
                "captured-b"
            },
        )
        .unwrap();
        assert_eq!(remote, "captured-b");
        assert!(
            destination_client(
                Some("revoked"),
                |_| anyhow::bail!("approval revoked"),
                |_| panic!("must not fall back")
            )
            .is_err()
        );
    }

    #[test]
    fn every_page_of_the_evidence_is_read_and_a_reset_between_pages_stops() {
        assert_eq!(ATTEMPTS, "/delivery-attempts");
        let mut asked = vec![];
        let listing = list(|path| {
            asked.push(path.to_string());
            Ok(if path.ends_with("after=0") {
                json!({"data_epoch":"e","attempts":[attempt("handed",1)],"next_after":7,"has_more":true})
            } else {
                json!({"data_epoch":"e","attempts":[attempt("uncertain",2)],"next_after":9,"has_more":false})
            })
        })
        .unwrap();
        assert_eq!(asked, [format!("{ATTEMPTS}?after=0"), format!("{ATTEMPTS}?after=7")]);
        assert_eq!(listing.attempts.len(), 2);
        let reset = list(|path| Ok(json!({"data_epoch": if path.ends_with("after=0") { "e" } else { "f" }, "attempts":[], "next_after":7, "has_more": path.ends_with("after=0")})));
        assert!(reset.is_err());
    }

    #[test]
    fn confirmations_and_stale_versions_never_post() {
        let mut posts = 0;
        assert!(resolve(7, 2, true, true, false, |_| panic!("must not contact server"), |_, _| { posts += 1; unreachable!() }).is_err());
        assert!(resolve(7, 2, false, false, false, |_| panic!("must not contact server"), |_, _| { posts += 1; unreachable!() }).is_err());
        for (state, version) in [("uncertain", 3), ("dispatching", 2), ("handed", 2)] {
            assert!(resolve(7, 2, true, true, true, |_| Ok(page(vec![attempt(state, version)])), |_, _| { posts += 1; unreachable!() }).is_err());
        }
        assert_eq!(posts, 0);
    }

    #[test]
    fn exact_versioned_payload_and_server_conflict_are_not_retried() {
        for retry in [true, false] {
            let mut posted = vec![];
            let result = resolve(7, 2, retry, true, true, |_| Ok(page(vec![attempt("uncertain", 2)])), |path, body| {
                posted.push((path.to_string(), body.clone()));
                Ok(json!({"attempt_id":7,"message_id":3,"version":3,"state": if retry { "retryable" } else { "discarded" }}))
            })
            .unwrap();
            assert_eq!(result.1, if retry { State::Retryable } else { State::Discarded });
            assert_eq!(posted, [(format!("{ATTEMPTS}/7/resolve"), json!({"protocol_version":2,"data_epoch":"e","expected_version":2,"decision": if retry { "retry" } else { "discard" }}))]);
        }
        let mut posts = 0;
        assert!(resolve(7, 2, true, true, true, |_| Ok(page(vec![attempt("uncertain", 2)])), |_, _| { posts += 1; anyhow::bail!("409 changed during inspection") }).is_err());
        assert_eq!(posts, 1);
    }

    #[test]
    fn detail_commands_preserve_explicit_destination_and_only_uncertain_resolves() {
        let a: Attempt = serde_json::from_value(attempt("uncertain", 2)).unwrap();
        let remote = details(&[a.clone()], Some("mac-approved-B")).join("\n");
        assert!(remote.contains("alondra delivery --mac mac-approved-B retry 7 --expected-version 2"));
        assert!(remote.contains("alondra delivery --mac mac-approved-B discard 7"));
        assert!(remote.contains("answer-1"));
        assert!(!details(&[a.clone()], None).join("\n").contains("--mac"));
        let mut pending = a;
        pending.state = State::Dispatching;
        assert!(pending.blocks());
        assert!(!details(&[pending], None).join("\n").contains("--terminal-checked"));
        for state in ["dispatching", "uncertain", "retryable", "discarded", "future"] {
            assert!(response_notice(&json!({"delivery_state":state}), None).is_some());
        }
        assert!(response_notice(&json!({"ok":true}), None).is_none());
    }
}
