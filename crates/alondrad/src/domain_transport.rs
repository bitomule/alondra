//! V2-only producer transport. Outbox messages for an Agent reach its Claude
//! session the two ways legacy used: injected into the turn that just ended
//! (Stop hook `additionalContext`) or typed into its background terminal. A
//! handoff is technical evidence only; every attempt is claimed before any
//! external write and finished as handed, retryable or uncertain afterwards.
//!
//! Hooks observe sessions the domain already knows. They never create an
//! Agent, a Task or a decision, and a hook payload never carries authority.
use crate::domain_agent_lifecycle as lifecycle;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::{DeliveryClaim, OutboxMessage};
use crate::wake::WriteFailure;
use alondra_core::delivery::State;
use alondra_core::domain_v2::{AgentId, DomainState, InputValue, InteractionId, InteractionPayload, ScheduleLaunchState};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

const MAX_HOOK_BYTES: u64 = 25 * 1024 * 1024;
const MAX_BATCH: usize = 32;
const TERMINAL_RETRY: Duration = Duration::from_secs(120);

#[derive(Deserialize)]
struct Envelope {
    #[allow(dead_code)]
    launched: bool,
    payload: Value,
}

pub(crate) async fn serve(listener: UnixListener, domain: Arc<DomainRuntime>) -> Result<()> {
    loop {
        let (stream, _) = listener.accept().await.context("accepting local hook")?;
        let domain = domain.clone();
        tokio::spawn(async move {
            if let Err(error) = handle(stream, &domain).await {
                log!("v2 local hook failed: {error:#}");
            }
        });
    }
}

async fn handle(mut stream: UnixStream, domain: &DomainRuntime) -> Result<()> {
    let len = stream.read_u32().await? as u64;
    if len > MAX_HOOK_BYTES {
        bail!("hook input exceeds {MAX_HOOK_BYTES} bytes");
    }
    let mut input = vec![0; len as usize];
    stream.read_exact(&mut input).await?;
    let envelope: Envelope = serde_json::from_slice(&input).context("invalid local hook envelope")?;
    let reply = respond(domain, &envelope.payload)?;
    let (body, claims) = match reply {
        Reply::Plain(body) => (body, vec![]),
        Reply::Delivery { body, claims } => (body, claims),
    };
    let bytes = serde_json::to_vec(&body)?;
    let sent = async {
        stream.write_all(&200u16.to_be_bytes()).await?;
        stream.write_all(&bytes).await?;
        stream.shutdown().await?;
        anyhow::ensure!(
            tokio::time::timeout(Duration::from_secs(5), stream.read_u8()).await?? == 6,
            "hook reply was not acknowledged"
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;
    // A reply already written may have reached Claude even without the
    // acknowledgement, so a missing ack is uncertain, never retryable.
    let (state, reason) = match &sent {
        Ok(()) => (State::Handed, None),
        Err(_) => (State::Uncertain, Some("hook_ack_missing")),
    };
    finish_all(domain, &claims, state, reason)?;
    sent
}

enum Reply {
    Plain(Value),
    Delivery { body: Value, claims: Vec<DeliveryClaim> },
}

fn text(payload: &Value, key: &str) -> Option<String> {
    payload.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn respond(domain: &DomainRuntime, payload: &Value) -> Result<Reply> {
    let Some(session_id) = text(payload, "session_id") else {
        return Ok(Reply::Plain(json!({})));
    };
    let event = text(payload, "hook_event_name").unwrap_or_default();
    let tool = text(payload, "tool_name");
    if event == "PreToolUse" && tool.as_deref().is_some_and(|t| t.starts_with("mcp__plugin_alondra")) {
        return Ok(Reply::Plain(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
            "permissionDecision": "allow", "permissionDecisionReason": "Alondra tool"}})));
    }
    let agent_id = AgentId(session_id.clone());
    if !domain.state()?.agents.contains_key(&agent_id) {
        return Ok(Reply::Plain(json!({})));
    }
    domain.live.observe(payload);
    match event.as_str() {
        "SessionStart" | "SessionEnd" => {
            let connected = event == "SessionStart";
            let event_id = format!("{event}-{session_id}-{}", crate::auth::random_hex(8));
            let event = lifecycle::prepare_event(domain, &event_id)?;
            lifecycle::observe_instance(domain, &agent_id, connected, &event)?;
            if !connected {
                domain.revoke_agent(&session_id)?;
                crate::domain_sessions::remove_for(domain.home(), &agent_id)?;
            }
            Ok(Reply::Plain(json!({})))
        }
        "Stop" => {
            let held = crate::domain_messages::take_held(domain, &agent_id);
            let batch = claim_batch(domain, &agent_id)?;
            let (text, claims) = match batch {
                Some(batch) => (held.into_iter().chain([batch.text]).collect::<Vec<_>>().join("\n\n"), batch.claims),
                None if held.is_empty() => return Ok(Reply::Plain(json!({}))),
                None => (held.join("\n\n"), vec![]),
            };
            let body = json!({"hookSpecificOutput": {"hookEventName": "Stop", "additionalContext": text}});
            Ok(Reply::Delivery { body, claims })
        }
        _ => Ok(Reply::Plain(json!({}))),
    }
}

struct Batch {
    text: String,
    claims: Vec<DeliveryClaim>,
}

/// Claim every pending message of one Agent under the store lock, so the
/// terminal worker and a Stop hook can never hand the same message twice.
fn claim_batch(domain: &DomainRuntime, agent_id: &AgentId) -> Result<Option<Batch>> {
    let mut store = domain.store.lock().expect("domain store poisoned");
    let pending: Vec<OutboxMessage> = store.pending_messages()?.into_iter()
        .filter(|message| message.recipient_kind == "agent"
            && message.agent_id.as_deref() == Some(agent_id.0.as_str()))
        .take(MAX_BATCH)
        .collect();
    if pending.is_empty() {
        return Ok(None);
    }
    let mut claims = vec![];
    for message in pending {
        if let Some(claim) = store.claim_message(message.id)? {
            claims.push(claim);
        }
    }
    if claims.is_empty() {
        return Ok(None);
    }
    let state = store.load()?;
    Ok(Some(Batch { text: batch_text(&state, &claims), claims }))
}

/// Producer-facing delivery contains only the message written for the Agent.
/// Delivery identity, interaction data and authorization remain in Alondra.
pub(crate) fn render(state: &DomainState, message: &OutboxMessage) -> String {
    let interaction = message.interaction_id.as_ref()
        .and_then(|id| state.interactions.get(&InteractionId(id.clone())));
    let Some(interaction) = interaction else { return message.message.clone() };
    let (options, answer) = match &interaction.payload {
        InteractionPayload::Question { options, answer } | InteractionPayload::ActionRequest { options, answer } => (options, answer),
        InteractionPayload::Delivery { .. } => {
            let mut text = message.message.clone();
            if state.merge_authorizations.values().any(|authorization| {
                authorization.presentation_id == interaction.id && authorization.valid
                    && authorization.attempt == alondra_core::domain_v2::MergeAttemptState::Unclaimed
            }) && !text.contains("The user authorized merging the presented pull request.") {
                text.push_str("\n\nThe user authorized merging the presented pull request.");
            }
            return text;
        }
        InteractionPayload::Information { .. } => return message.message.clone(),
    };
    let Some(answer) = answer else {
        let mut text = interaction.body.clone();
        let labels: Vec<&str> = options.iter().map(|option| option.label.as_str()).collect();
        if !labels.is_empty() {
            text.push_str(&format!("\nOptions: {}.", labels.join(", ")));
        }
        return text;
    };
    let option = options.iter().find(|option| option.id == answer.option_id).map(|option| option.label.as_str());
    let values: Vec<String> = answer.inputs.values().map(|value| match value {
        InputValue::Text(value) => value.clone(),
        InputValue::Boolean(value) => value.to_string(),
        InputValue::Integer(value) => value.to_string(),
    }).filter(|value| !value.trim().is_empty()).collect();
    let mut text = match option {
        Some(option) => format!("The user answered: {option}."),
        None if !values.is_empty() => format!("The user answered: {}.", values.join("; ")),
        None => "The user answered the question.".into(),
    };
    if !values.is_empty() && option.is_some() {
        text.push_str(&format!(" Additional details: {}.", values.join("; ")));
    }
    if !answer.attachments.is_empty() {
        for attachment in &answer.attachments {
            text.push_str(&format!(" Open the attached file “{}” at {}.", attachment.name, attachment.content_reference));
        }
    }
    text
}

fn batch_text(state: &DomainState, claims: &[DeliveryClaim]) -> String {
    claims.iter().map(|claim| render(state, &claim.message)).collect::<Vec<_>>().join("\n\n")
}

fn finish_all(domain: &DomainRuntime, claims: &[DeliveryClaim], state: State,
    reason: Option<&str>) -> Result<()> {
    let mut store = domain.store.lock().expect("domain store poisoned");
    for claim in claims {
        let finished = store.finish_message(claim.attempt_id, claim.version, state, reason)?;
        anyhow::ensure!(finished, "domain delivery claim changed while finishing");
    }
    Ok(())
}

/// Background sessions the daemon launched: the terminal `claude attach`
/// opens is known from the Schedule launch that handed the session over.
pub(crate) fn terminals(state: &DomainState) -> BTreeMap<AgentId, String> {
    state.schedule.runs.values().filter_map(|run| {
        let launch = run.launch.as_ref()?;
        if launch.state != ScheduleLaunchState::Handed {
            return None;
        }
        let short = launch.session_id.clone()?;
        let agent = state.tasks.get(run.task_id.as_ref()?)?.agent_id.clone()?;
        Some((agent, short))
    }).collect()
}

#[derive(Default)]
pub(crate) struct Backoff {
    failed: HashMap<AgentId, Instant>,
}

impl Backoff {
    fn ready(&mut self, agent: &AgentId) -> bool {
        self.failed.retain(|_, at| at.elapsed() < TERMINAL_RETRY);
        !self.failed.contains_key(agent)
    }
    fn outcome(&mut self, agent: &AgentId, handed: bool) {
        if handed {
            self.failed.remove(agent);
        } else {
            self.failed.insert(agent.clone(), Instant::now());
        }
    }
}

pub(crate) trait Terminal: Sync + Send + 'static {
    fn write(&self, short: &str, text: &str) -> std::result::Result<(), WriteFailure>;
}

pub(crate) struct Tmux;
impl Terminal for Tmux {
    fn write(&self, short: &str, text: &str) -> std::result::Result<(), WriteFailure> {
        crate::wake::type_into(short, text)
    }
}

/// One pass over every launched session with pending messages. A session
/// whose last write failed is left alone for a while; a terminal that was not
/// ready stays retryable, and a write that may have landed is uncertain.
pub(crate) async fn deliver_terminals(domain: &DomainRuntime, home: &Path,
    terminal: &Arc<impl Terminal>, backoff: &mut Backoff) -> Result<()> {
    let state = domain.state()?;
    for (agent, short) in terminals(&state) {
        if !backoff.ready(&agent) {
            continue;
        }
        let Some(batch) = claim_batch(domain, &agent)? else { continue };
        let typed: Vec<(u64, String, Option<u64>)> = batch.claims.iter()
            .map(|claim| (claim.message.id as u64, render(&state, &claim.message), None))
            .collect();
        let outcome = match crate::wake::turn_for(home, &typed) {
            Err(error) => {
                log!("could not prepare v2 message: {error}");
                (State::Retryable, Some("prepare_failed"))
            }
            Ok(text) => {
                let terminal = terminal.clone();
                let target = short.clone();
                match tokio::task::spawn_blocking(move || terminal.write(&target, &text)).await {
                    Ok(Ok(())) => (State::Handed, None),
                    Ok(Err(WriteFailure::NotStarted)) => (State::Retryable, Some("terminal_not_ready")),
                    Ok(Err(WriteFailure::Uncertain)) => (State::Uncertain, Some("terminal_write_uncertain")),
                    Err(_) => (State::Uncertain, Some("delivery_worker_failed")),
                }
            }
        };
        backoff.outcome(&agent, outcome.0 == State::Handed);
        finish_all(domain, &batch.claims, outcome.0, outcome.1)?;
    }
    Ok(())
}

pub(crate) async fn run(listener: UnixListener, domain: Arc<DomainRuntime>, home: PathBuf) -> Result<()> {
    tokio::select! {
        result = serve(listener, domain.clone()) => result,
        result = run_terminals(domain, home) => result,
    }
}

pub(crate) async fn run_terminals(domain: Arc<DomainRuntime>, home: PathBuf) -> Result<()> {
    let terminal = Arc::new(Tmux);
    let mut backoff = Backoff::default();
    let mut changed = domain.subscribe();
    loop {
        deliver_terminals(&domain, &home, &terminal, &mut backoff).await?;
        tokio::select! {
            _ = changed.changed() => {},
            _ = tokio::time::sleep(Duration::from_secs(10)) => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain_v2::*;
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let home = std::env::temp_dir().join(format!("alondra-v2-transport-{}", crate::auth::random_hex(8)));
            std::fs::create_dir(&home).unwrap();
            Self(home)
        }
        fn open(&self) -> DomainRuntime {
            DomainRuntime::open(&self.0).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn command(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch,
            command_id: id.into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    /// An Agent with a Task, a question and the User's answer queued for it.
    fn answered_question(domain: &DomainRuntime, agent: &str) {
        command(domain, Actor::System, &format!("register-{agent}"), Command::RegisterAgent {
            agent_id: agent.into(), name: agent.into(),
        });
        command(domain, Actor::User { user_id: "u".into() }, &format!("request-{agent}"),
            Command::CreateRequest { request_id: format!("request-{agent}").into(), body: "Work".into(), auto_merge: false });
        command(domain, Actor::User { user_id: "u".into() }, &format!("task-{agent}"), Command::CreateTask {
            task_id: format!("task-{agent}").into(), request_id: format!("request-{agent}").into(),
            title: "Task".into(), agent_id: Some(agent.into()), review_prs: false,
            dependencies: BTreeSet::new(), stack: None,
        });
        command(domain, Actor::Agent { agent_id: agent.into() }, &format!("question-{agent}"), Command::AskQuestion {
            attachments: vec![], interaction_id: format!("question-{agent}").into(),
            scope: Scope::Task { task_id: format!("task-{agent}").into() }, recipient: Recipient::User,
            body: "Choose".into(), blocking: true,
            options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None, inputs: vec![] }],
        });
        command(domain, Actor::User { user_id: "u".into() }, &format!("answer-{agent}"), Command::AnswerQuestion {
            attachments: vec![], interaction_id: format!("question-{agent}").into(),
            option_id: "yes".into(), inputs: Default::default(),
        });
    }

    fn agent_outbox(domain: &DomainRuntime, agent: &str) -> Vec<(i64, String)> {
        domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .filter(|m| m.agent_id.as_deref() == Some(agent))
            .map(|m| (m.id, m.state)).collect()
    }

    fn attempts(domain: &DomainRuntime) -> Vec<(i64, String, Option<String>)> {
        domain.store.lock().unwrap().delivery_evidence_page(0).unwrap().into_iter()
            .map(|item| (item.message_id, item.state, item.reason)).collect()
    }

    async fn hook(domain: &DomainRuntime, payload: Value, ack: Option<u8>) -> (Value, Result<()>) {
        let (mut client, server) = UnixStream::pair().unwrap();
        let body = serde_json::to_vec(&json!({"launched": true, "payload": payload})).unwrap();
        client.write_u32(body.len() as u32).await.unwrap();
        client.write_all(&body).await.unwrap();
        let mut bytes = vec![];
        let (result, _) = tokio::join!(handle(server, domain), async {
            client.read_to_end(&mut bytes).await.unwrap();
            if let Some(ack) = ack {
                client.write_u8(ack).await.unwrap();
            }
            drop(client);
        });
        assert_eq!(u16::from_be_bytes([bytes[0], bytes[1]]), 200);
        (serde_json::from_slice(&bytes[2..]).unwrap(), result)
    }

    #[tokio::test]
    async fn stop_hook_hands_only_the_message_once_and_no_ack_is_uncertain() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        answered_question(&domain, "session-a");
        let pending = agent_outbox(&domain, "session-a");
        assert_eq!(pending.len(), 1);

        let (reply, result) = hook(&domain, json!({"session_id": "stranger", "hook_event_name": "Stop"}), Some(6)).await;
        result.unwrap();
        assert_eq!(reply, json!({}), "an unknown session gets nothing and creates nothing");
        assert!(domain.state().unwrap().agents.get(&"stranger".into()).is_none());

        let (reply, result) = hook(&domain, json!({"session_id": "session-a", "hook_event_name": "Stop"}), Some(6)).await;
        result.unwrap();
        let context = reply["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert_eq!(reply["hookSpecificOutput"]["hookEventName"], "Stop");
        assert_eq!(context, "The user answered: Yes.");
        assert!(!context.contains("data_epoch") && !context.contains("revision=")
            && !context.contains("task_id=") && !context.contains("interaction_id=")
            && !context.contains("option_id"), "{context}");
        assert!(agent_outbox(&domain, "session-a").is_empty());
        assert_eq!(attempts(&domain), vec![(pending[0].0, "handed".into(), None)]);

        let (reply, _) = hook(&domain, json!({"session_id": "session-a", "hook_event_name": "Stop"}), Some(6)).await;
        assert_eq!(reply, json!({}), "a handed message is never delivered twice");

        answered_question(&domain, "session-b");
        let id = agent_outbox(&domain, "session-b")[0].0;
        let (reply, result) = hook(&domain, json!({"session_id": "session-b", "hook_event_name": "Stop"}), None).await;
        assert!(result.is_err());
        let uncertain_context = reply["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert_eq!(uncertain_context, "The user answered: Yes.");
        assert!(!uncertain_context.contains("question-session-b"));
        assert!(attempts(&domain).contains(&(id, "uncertain".into(), Some("hook_ack_missing".into()))));
        assert!(agent_outbox(&domain, "session-b").is_empty(), "an uncertain handoff is not retried blindly");
        drop(domain);
        let reopened = fixture.open();
        assert!(agent_outbox(&reopened, "session-b").is_empty());
        assert!(agent_outbox(&reopened, "session-a").is_empty());
    }

    #[tokio::test]
    async fn a_held_user_message_reaches_the_next_stop_hook_once_with_any_batch() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        answered_question(&domain, "session-a");
        domain.held.lock().unwrap().entry("session-a".into()).or_default().push("Use the new API".into());
        let stop = json!({"session_id": "session-a", "hook_event_name": "Stop"});
        let (body, result) = hook(&domain, stop.clone(), Some(6)).await;
        result.unwrap();
        let context = body["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert_eq!(context, "Use the new API\n\nThe user answered: Yes.");
        assert!(agent_outbox(&domain, "session-a").is_empty());
        domain.held.lock().unwrap().entry("session-a".into()).or_default().push("Only this".into());
        let (body, _) = hook(&domain, stop.clone(), Some(6)).await;
        assert_eq!(body["hookSpecificOutput"]["additionalContext"], "Only this");
        let (body, _) = hook(&domain, stop, Some(6)).await;
        assert_eq!(body, json!({}), "a held message is handed once");
    }

    #[tokio::test]
    async fn a_merge_authorization_stays_internal_to_alondra() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        answered_question(&domain, "session-a");
        let head = "0123456789abcdef0123456789abcdef01234567";
        command(&domain, Actor::Agent { agent_id: "session-a".into() }, "pr", Command::RegisterPr {
            task_id: "task-session-a".into(), pr_id: "pr".into(), host: "github.com".into(),
            repository: "owner/repo".into(), number: 7, head: head.into(), base: "main".into(),
            allowed_methods: vec![MergeMethod::Squash],
        });
        command(&domain, Actor::Agent { agent_id: "session-a".into() }, "present", Command::PresentResult {
            attachments: vec![], task_id: "task-session-a".into(), interaction_id: "delivery".into(),
            body: "Ready".into(), targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: head.into() }],
            is_final: true,
        });
        command(&domain, Actor::User { user_id: "u".into() }, "authorize", Command::AuthorizeMerge {
            authorization_id: "auth".into(), presentation_id: "delivery".into(), pr_id: "pr".into(),
            expected_head: head.into(), method: MergeMethod::Squash,
        });
        let (reply, result) = hook(&domain, json!({"session_id": "session-a", "hook_event_name": "Stop"}), Some(6)).await;
        result.unwrap();
        let context = reply["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert_eq!(context, "The user answered: Yes.\n\nThe user authorized merging the presented pull request.");
        assert!(!context.contains("authorization_id") && !context.contains("pr_id")
            && !context.contains("head") && !context.contains("auth-"), "{context}");
    }

    #[tokio::test]
    async fn session_hooks_track_connection_and_end_revokes_the_bearer() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        command(&domain, Actor::System, "register", Command::RegisterAgent {
            agent_id: "session-a".into(), name: "A".into(),
        });
        let bearer = domain.bind_agent("session-a").unwrap().expose_for_child_environment().to_owned();
        let (_, result) = hook(&domain, json!({"session_id": "session-a", "hook_event_name": "SessionStart"}), Some(6)).await;
        result.unwrap();
        assert!(domain.state().unwrap().agents[&"session-a".into()].connected);
        assert!(domain.resolve_bearer(&bearer).is_ok());
        let (_, result) = hook(&domain, json!({"session_id": "session-a", "hook_event_name": "SessionEnd"}), Some(6)).await;
        result.unwrap();
        assert!(!domain.state().unwrap().agents[&"session-a".into()].connected);
        assert!(domain.resolve_bearer(&bearer).is_err(), "a finished session's bearer is revoked");
        let (reply, _) = hook(&domain, json!({"session_id": "session-a", "hook_event_name": "PreToolUse",
            "tool_name": "mcp__plugin_alondra_alondra__publish_information"}), Some(6)).await;
        assert_eq!(reply["hookSpecificOutput"]["permissionDecision"], "allow");
    }

    #[tokio::test]
    async fn hooks_of_a_known_session_are_observed_live_and_a_stranger_is_not() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        command(&domain, Actor::System, "register", Command::RegisterAgent { agent_id: "session-a".into(), name: "A".into() });
        let (_, result) = hook(&domain, json!({"session_id": "session-a", "hook_event_name": "PreToolUse",
            "tool_name": "Bash", "tool_input": {"command": "ls"}, "cwd": "/Users/me/Projects/App"}), Some(6)).await;
        result.unwrap();
        hook(&domain, json!({"session_id": "stranger", "hook_event_name": "PreToolUse", "tool_name": "Bash"}), Some(6)).await.1.unwrap();
        let nodes = domain.live.nodes();
        assert_eq!(nodes.iter().map(|node| node.session_id.as_str()).collect::<Vec<_>>(), ["session-a"]);
        assert!(nodes[0].busy && nodes[0].tool.is_some());
    }

    struct FakeTerminal {
        writes: Mutex<Vec<(String, String)>>,
        outcome: Mutex<std::result::Result<(), WriteFailure>>,
    }
    impl Terminal for FakeTerminal {
        fn write(&self, short: &str, text: &str) -> std::result::Result<(), WriteFailure> {
            self.writes.lock().unwrap().push((short.into(), text.into()));
            match &*self.outcome.lock().unwrap() {
                Ok(()) => Ok(()),
                Err(WriteFailure::NotStarted) => Err(WriteFailure::NotStarted),
                Err(WriteFailure::Uncertain) => Err(WriteFailure::Uncertain),
            }
        }
    }

    fn launched_by_schedule(domain: &DomainRuntime, agent: &str, short: &str) {
        let now = alondra_core::now_millis();
        command(domain, Actor::System, "schedule", Command::CreateSchedule { schedule: Schedule {
            id: "schedule-1".into(), title: "Review".into(), text: "Review".into(), project: None,
            options: BTreeMap::new(), disallowed_tools: vec![], frequency: ScheduleFrequency::Daily,
            hour: 9, minute: 0, weekday: None, interval: None, start_at: None,
            enabled: false, next_run: None, revision: 1, deleted_at: None,
        }});
        command(domain, Actor::System, "claim-run", Command::ClaimScheduleRun {
            schedule_id: "schedule-1".into(), run_id: "run-1".into(),
            origin: ScheduleRunOrigin::Manual, now, next_after_now: None,
            expected_schedule_revision: None,
        });
        let snapshot = domain.state().unwrap().schedule.runs[&"run-1".into()].definition_snapshot.clone().unwrap();
        let request = Request::new("scheduled-request".into(), snapshot.text.clone());
        let task = Task {
            id: "scheduled-task".into(), request_id: request.id.clone(), agent_id: None,
            title: snapshot.title.clone(), state: TaskState::NotStarted, revision: 0, started: false,
            review_prs: false, final_presentation: None, pr_id: None, dependencies: BTreeSet::new(),
            stack: None, documents: BTreeMap::new(), archived: false,
        };
        let options = alondra_core::options::OptionsFile::default().resolve(None, &BTreeMap::new()).unwrap();
        let launch = ScheduleLaunchIntent { name: "scheduled/run".into(), cwd: "/tmp".into(),
            prompt: snapshot.text.clone(), options_json: serde_json::to_string(&options).unwrap() };
        command(domain, Actor::System, "sent", Command::RecordScheduleRunSent {
            run_id: "run-1".into(), request, task, launch, finished_at: now,
        });
        command(domain, Actor::System, "claim-launch", Command::ClaimScheduleLaunch { run_id: "run-1".into(), now });
        command(domain, Actor::System, "handed", Command::RecordScheduleLaunchHanded {
            run_id: "run-1".into(), session_id: Some(short.into()), agent_id: Some(agent.into()), finished_at: now,
        });
    }

    #[tokio::test]
    async fn terminal_worker_types_into_launched_sessions_and_classifies_failures() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        answered_question(&domain, "session-a");
        let terminal = Arc::new(FakeTerminal { writes: Mutex::new(vec![]), outcome: Mutex::new(Ok(())) });
        let mut backoff = Backoff::default();
        deliver_terminals(&domain, &fixture.0, &terminal, &mut backoff).await.unwrap();
        assert!(terminal.writes.lock().unwrap().is_empty(), "a session without a known terminal is not typed into");
        assert_eq!(agent_outbox(&domain, "session-a").len(), 1);

        launched_by_schedule(&domain, "session-a", "ab1234");
        let id = agent_outbox(&domain, "session-a")[0].0;
        deliver_terminals(&domain, &fixture.0, &terminal, &mut backoff).await.unwrap();
        let writes = terminal.writes.lock().unwrap().clone();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, "ab1234");
        assert_eq!(writes[0].1, "The user answered: Yes.");
        assert!(!writes[0].1.contains("data_epoch") && !writes[0].1.contains("revision=")
            && !writes[0].1.contains("interaction_id="), "{}", writes[0].1);
        assert!(agent_outbox(&domain, "session-a").is_empty());
        assert!(attempts(&domain).contains(&(id, "handed".into(), None)));
        deliver_terminals(&domain, &fixture.0, &terminal, &mut backoff).await.unwrap();
        assert_eq!(terminal.writes.lock().unwrap().len(), 1, "nothing pending, nothing typed");

        command(&domain, Actor::Agent { agent_id: "session-a".into() }, "question-2", Command::AskQuestion {
            attachments: vec![], interaction_id: "question-2".into(),
            scope: Scope::Task { task_id: "task-session-a".into() }, recipient: Recipient::User,
            body: "Again".into(), blocking: true,
            options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None, inputs: vec![] }],
        });
        command(&domain, Actor::User { user_id: "u".into() }, "answer-2", Command::AnswerQuestion {
            attachments: vec![], interaction_id: "question-2".into(), option_id: "yes".into(), inputs: Default::default(),
        });
        let second = agent_outbox(&domain, "session-a")[0].0;
        *terminal.outcome.lock().unwrap() = Err(WriteFailure::NotStarted);
        deliver_terminals(&domain, &fixture.0, &terminal, &mut backoff).await.unwrap();
        assert_eq!(agent_outbox(&domain, "session-a"), vec![(second, "retryable".into())]);
        deliver_terminals(&domain, &fixture.0, &terminal, &mut backoff).await.unwrap();
        assert_eq!(terminal.writes.lock().unwrap().len(), 2, "a failed session backs off");
        let mut fresh = Backoff::default();
        *terminal.outcome.lock().unwrap() = Err(WriteFailure::Uncertain);
        deliver_terminals(&domain, &fixture.0, &terminal, &mut fresh).await.unwrap();
        assert!(agent_outbox(&domain, "session-a").is_empty());
        assert!(attempts(&domain).contains(&(second, "uncertain".into(), Some("terminal_write_uncertain".into()))));
        drop(domain);
        let reopened = fixture.open();
        assert!(agent_outbox(&reopened, "session-a").is_empty(), "an uncertain write is not retyped after restart");
    }
}
