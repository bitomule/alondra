//! Explicit inspection/resolution of durable delivery evidence. Never retries a POST.
use crate::client::Client;
use alondra_core::delivery::State;
use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Args)]
pub struct Options {
    /// Explicit approved destination; omitted means Local, never the last remote Mac.
    #[arg(long, global = true)]
    pub mac: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}
#[derive(Subcommand)]
pub enum Command {
    /// Inspect recorded attempts, their versions and recovery instructions.
    List,
    /// Allow another attempt only after inspecting the destination terminal.
    Retry {
        id: u64,
        #[arg(long)]
        expected_version: u64,
        #[arg(long, required = true)]
        terminal_checked: bool,
        #[arg(long, required = true)]
        confirm_duplicate_risk: bool,
    },
    /// Stop retrying this attempt; this does not undo an external effect.
    Discard {
        id: u64,
        #[arg(long)]
        expected_version: u64,
        #[arg(long, required = true)]
        terminal_checked: bool,
    },
}
/// The channel is descriptive and extensible; unknown states fail deserialization closed.
#[derive(Clone, Debug, Deserialize)]
pub struct Attempt {
    pub id: u64,
    pub session_id: String,
    pub channel: String,
    pub state: State,
    pub version: u64,
    pub reason: Option<String>,
}
#[derive(Deserialize)]
struct Listing {
    attempts: Vec<Attempt>,
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
/// Used by every answer consumer rather than treating HTTP 200 as completed execution.
pub fn response_notice(value: &Value) -> Option<String> {
    let raw = value.get("delivery_state")?;
    let text = serde_json::from_value::<State>(raw.clone())
        .map(state_text)
        .unwrap_or(tr!(
            "Unknown delivery outcome; inspect before repeating",
            "Resultado desconocido; revisa antes de repetir"
        ));
    Some(format!(
        "{} · {}",
        text,
        tr!(
            "inspect with alondra delivery list / :deliveries",
            "consulta alondra delivery list / :deliveries"
        )
    ))
}
pub fn post_feedback(
    calls: Vec<(String, Value)>,
    ok: String,
    mut post: impl FnMut(&str, &Value) -> Result<Value>,
) -> Result<String> {
    let mut feedback = ok;
    for (index, (path, body)) in calls.into_iter().enumerate() {
        match post(&path, &body) {
            Ok(value) if index == 0 => {
                if let Some(notice) = response_notice(&value) {
                    feedback = notice;
                    if value["delivery_state"] != "handed" {
                        break;
                    }
                }
            }
            Err(error) if index == 0 => return Err(error),
            _ => {}
        }
    }
    Ok(feedback)
}
fn command_prefix(mac: Option<&str>) -> String {
    match mac {
        Some(mac) => format!("alondra delivery --mac {mac}"),
        None => "alondra delivery".into(),
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
        lines.push(format!(
            "#{} · v{} · {} · {}",
            a.id, a.version, a.session_id, a.channel
        ));
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
            lines.push(format!("{prefix} retry {} --expected-version {} --terminal-checked --confirm-duplicate-risk", a.id, a.version));
            lines.push(format!(
                "{prefix} discard {} --expected-version {} --terminal-checked",
                a.id, a.version
            ));
        }
    }
    lines
}
fn resolve(
    id: u64,
    version: u64,
    retry: bool,
    checked: bool,
    risk: bool,
    mut request: impl FnMut(&str, Option<&Value>) -> Result<Value>,
) -> Result<Attempt> {
    ensure!(
        checked && (!retry || risk),
        "Inspect the terminal and explicitly confirm duplicate risk before retrying"
    );
    let listing: Listing = serde_json::from_value(request("/v1/delivery-attempts", None)?)?;
    let attempt = listing
        .attempts
        .iter()
        .find(|a| a.id == id)
        .context("Delivery attempt no longer exists; refresh the list")?;
    ensure!(
        attempt.version == version && attempt.state == State::Uncertain,
        "Delivery attempt changed; refresh the list before deciding"
    );
    let action = if retry { "retry" } else { "discard" };
    let body = json!({"expected_version": version, "terminal_checked": checked, "confirm_duplicate_risk": retry && risk});
    // The server checks the version again atomically. A rejection/timeout is returned, never retried.
    let value = request(&format!("/v1/delivery-attempts/{id}/{action}"), Some(&body))?;
    let result: Attempt = serde_json::from_value(
        value
            .get("attempt")
            .context("Missing delivery result; inspect before repeating")?
            .clone(),
    )?;
    ensure!(
        result.id == id
            && result.session_id == attempt.session_id
            && result.channel == attempt.channel
            && result.version == version.checked_add(1).context("Invalid delivery version")?
            && result.state
                == if retry {
                    State::Retryable
                } else {
                    State::Discarded
                },
        "Unexpected delivery result; inspect before repeating"
    );
    Ok(result)
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
    let request = |path: &str, body: Option<&Value>| match body {
        Some(body) => client.post(path, body),
        None => client.get(path),
    };
    match options.command {
        Command::List => {
            let listing: Listing = client.get("/v1/delivery-attempts")?;
            for line in details(&listing.attempts, options.mac.as_deref()) {
                println!("{line}");
            }
        }
        Command::Retry {
            id,
            expected_version,
            terminal_checked,
            confirm_duplicate_risk,
        } => {
            let a = resolve(
                id,
                expected_version,
                true,
                terminal_checked,
                confirm_duplicate_risk,
                request,
            )?;
            println!("#{}: {}", a.id, state_text(a.state));
        }
        Command::Discard {
            id,
            expected_version,
            terminal_checked,
        } => {
            let a = resolve(
                id,
                expected_version,
                false,
                terminal_checked,
                false,
                request,
            )?;
            println!("#{}: {}", a.id, state_text(a.state));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn attempt(state: &str, version: u64) -> Value {
        json!({"id":7,"session_id":"mac-session","channel":"menu_decision","state":state,"version":version,"reason":null})
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
    fn ambiguous_post_does_not_claim_success_or_run_follow_up_actions() {
        for state in [
            "dispatching",
            "uncertain",
            "retryable",
            "discarded",
            "future",
            "handed",
        ] {
            let mut paths = vec![];
            let result = post_feedback(
                vec![
                    ("answer".into(), json!({})),
                    ("mark-read".into(), json!({})),
                ],
                "Answered successfully".into(),
                |path, _| {
                    paths.push(path.to_owned());
                    Ok(json!({"delivery_state":state}))
                },
            )
            .unwrap();
            assert_ne!(result, "Answered successfully");
            assert_eq!(paths.len(), if state == "handed" { 2 } else { 1 });
        }
    }
    #[test]
    fn confirmations_and_stale_versions_never_post() {
        let mut calls = 0;
        assert!(
            resolve(7, 2, true, true, false, |_, _| {
                calls += 1;
                panic!("must not contact server")
            })
            .is_err()
        );
        assert!(
            resolve(7, 2, false, false, false, |_, _| {
                calls += 1;
                panic!("must not contact server")
            })
            .is_err()
        );
        assert_eq!(calls, 0);
        for (state, version) in [("uncertain", 3), ("dispatching", 2), ("handed", 2)] {
            assert!(
                resolve(7, 2, true, true, true, |path, body| {
                    assert_eq!(path, "/v1/delivery-attempts");
                    assert!(body.is_none());
                    Ok(json!({"attempts":[attempt(state,version)]}))
                })
                .is_err()
            );
        }
    }
    #[test]
    fn exact_versioned_payload_and_server_conflict_are_not_retried() {
        for retry in [true, false] {
            let mut calls = vec![];
            let result=resolve(7,2,retry,true,true,|path,body| {
                calls.push(path.to_owned());
                if let Some(body)=body {
                    assert_eq!(body,&json!({"expected_version":2,"terminal_checked":true,"confirm_duplicate_risk":retry}));
                    Ok(json!({"attempt":attempt(if retry {"retryable"} else {"discarded"},3)}))
                } else { Ok(json!({"attempts":[attempt("uncertain",2)]})) }
            }).unwrap();
            assert_eq!(result.version, 3);
            assert_eq!(
                calls,
                vec![
                    "/v1/delivery-attempts".to_string(),
                    format!(
                        "/v1/delivery-attempts/7/{}",
                        if retry { "retry" } else { "discard" }
                    )
                ]
            );
        }
        let mut posts = 0;
        assert!(
            resolve(7, 2, true, true, true, |_, body| {
                if body.is_some() {
                    posts += 1;
                    anyhow::bail!("409 changed during inspection")
                }
                Ok(json!({"attempts":[attempt("uncertain",2)]}))
            })
            .is_err()
        );
        assert_eq!(posts, 1);
    }
    #[test]
    fn detail_commands_preserve_explicit_destination_and_only_uncertain_resolves() {
        let a: Attempt = serde_json::from_value(attempt("uncertain", 2)).unwrap();
        let remote = details(&[a.clone()], Some("mac-approved-B")).join("\n");
        assert!(
            remote.contains("alondra delivery --mac mac-approved-B retry 7 --expected-version 2")
        );
        assert!(remote.contains("alondra delivery --mac mac-approved-B discard 7"));
        assert!(!details(&[a.clone()], None).join("\n").contains("--mac"));
        let mut pending = a;
        pending.state = State::Dispatching;
        assert!(pending.blocks());
        assert!(
            !details(&[pending], None)
                .join("\n")
                .contains("--terminal-checked")
        );
        for state in [
            "dispatching",
            "uncertain",
            "retryable",
            "discarded",
            "future",
        ] {
            assert!(response_notice(&json!({"delivery_state":state})).is_some());
        }
        assert!(response_notice(&json!({"ok":true})).is_none());
    }
}
