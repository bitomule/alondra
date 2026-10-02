//! V2-only producer transport. Outbox messages for an Agent reach its Claude
//! session the two ways legacy used: injected into the turn that just ended
//! (Stop hook `additionalContext`) or typed into its background terminal. A
//! handoff is technical evidence only; every attempt is claimed before any
//! external write and finished as handed, retryable or uncertain afterwards.
//!
//! Hooks observe sessions the domain already knows. They never create an
//! Agent, a Task or a decision. The one transition they make is the one a
//! session's own activity proves: its assigned Task has started.
use crate::domain_agent_lifecycle as lifecycle;
use crate::domain_messages::{ClaudeAgents, Session, Sessions};
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::{DeliveryClaim, OutboxMessage};
use crate::wake::WriteFailure;
use alondra_core::delivery::State;
use alondra_core::domain_v2::{
    Actor, AgentId, Author, Command, CommandEnvelope, DomainState, InputValue, InteractionId, InteractionPayload,
    PROTOCOL_VERSION, Recipient, Scope, ScheduleLaunchState, TaskId, TaskState,
};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
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

async fn handle(stream: UnixStream, domain: &DomainRuntime) -> Result<()> {
    handle_with(stream, domain, |session| crate::watcher::is_background_session(&crate::watcher::claude_home(), session)).await
}

async fn handle_with(mut stream: UnixStream, domain: &DomainRuntime, background: impl Fn(&str) -> bool) -> Result<()> {
    let len = stream.read_u32().await? as u64;
    if len > MAX_HOOK_BYTES {
        bail!("hook input exceeds {MAX_HOOK_BYTES} bytes");
    }
    let mut input = vec![0; len as usize];
    stream.read_exact(&mut input).await?;
    let envelope: Envelope = serde_json::from_slice(&input).context("invalid local hook envelope")?;
    let reply = respond(domain, &envelope.payload, background)?;
    let (body, claims) = match reply {
        Reply::Plain(body) => (body, vec![]),
        Reply::Delivery { body, claims } => (body, claims),
        Reply::Held(held) => {
            let answer = tokio::select! {
                answer = crate::domain_holds::wait(domain, &held.id) => answer,
                gone = stream.read_u8() => {
                    log!("hook holding {} went away: {gone:?}", held.id);
                    return crate::domain_holds::withdraw(domain, &held.id);
                }
            };
            (answered(domain, &envelope.payload, &held, answer?), vec![])
        }
    };
    let bytes = serde_json::to_vec(&body)?;
    let sent = async {
        stream.write_all(&200u16.to_be_bytes()).await?;
        stream.write_all(&bytes).await?;
        stream.shutdown().await?;
        if claims.is_empty() {
            return Ok(());
        }
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
    Held(crate::domain_holds::Held),
}

/// The reply to a held hook. Words the user added to a refusal reach the session at its
/// next turn end; a card that can no longer be answered lets Claude Code ask in its terminal.
fn answered(domain: &DomainRuntime, payload: &Value, held: &crate::domain_holds::Held,
    answer: Option<alondra_core::domain_v2::Answer>) -> Value {
    let Some(answer) = answer else { return json!({}) };
    if let (Some(text), Some(session)) = (crate::domain_holds::follow_up(held.hold, &answer), text(payload, "session_id")) {
        domain.held.lock().expect("held messages poisoned").entry(AgentId(session)).or_default().push(text);
    }
    crate::domain_holds::decision(held, &answer, &crate::domain_messages::user_name())
}

fn text(payload: &Value, key: &str) -> Option<String> {
    payload.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn respond(domain: &DomainRuntime, payload: &Value, background: impl Fn(&str) -> bool) -> Result<Reply> {
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
    let Some(was_connected) = domain.state()?.agents.get(&agent_id).map(|agent| agent.connected) else {
        return Ok(Reply::Plain(json!({})));
    };
    domain.live.observe(payload);
    // A SessionStart that arrives before the launch registers the Agent is dropped, so any
    // later hook proves the session alive, as every hook did in 8ccd131.
    let connected = match event.as_str() {
        "SessionStart" => Some(true),
        "SessionEnd" => Some(false),
        _ => (!was_connected).then_some(true),
    };
    if let Some(connected) = connected {
        let event_id = format!("{event}-{session_id}-{}", crate::auth::random_hex(8));
        lifecycle::observe_instance(domain, &agent_id, connected, &lifecycle::prepare_event(domain, &event_id)?)?;
        if !connected {
            domain.revoke_agent(&session_id)?;
            crate::domain_sessions::remove_for(domain.home(), &agent_id)?;
        }
    }
    if !matches!(event.as_str(), "SessionStart" | "SessionEnd") {
        start_assigned(domain, &agent_id)?;
    }
    if event == "PreToolUse"
        && let Some(reason) = forbidden_for_nodes(tool.as_deref(), payload.get("tool_input"))
    {
        log!("blocked {session_id} from acting as the user: {reason}");
        return Ok(Reply::Plain(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
            "permissionDecision": "deny", "permissionDecisionReason": reason}})));
    }
    if let Some(held) = crate::domain_holds::hold(domain, payload, background(&session_id))? {
        return Ok(Reply::Held(held));
    }
    match event.as_str() {
        "SessionStart" | "SessionEnd" => Ok(Reply::Plain(json!({}))),
        "Stop" => {
            let held = crate::domain_messages::take_held(domain, &agent_id);
            let batch = claim_batch(domain, &agent_id)?;
            let (text, claims) = match batch {
                Some(batch) => (held.into_iter().chain([batch.text]).collect::<Vec<_>>().join("\n\n"), batch.claims),
                None if held.is_empty() => match unreported(domain, &agent_id, payload)? {
                    Some(text) => (text, vec![]),
                    None => return Ok(Reply::Plain(json!({}))),
                },
                None => (held.join("\n\n"), vec![]),
            };
            let body = json!({"hookSpecificOutput": {"hookEventName": "Stop", "additionalContext": text}});
            Ok(Reply::Delivery { body, claims })
        }
        _ => Ok(Reply::Plain(json!({}))),
    }
}

const USER_KEY_FILE: &str = "tui-key.pem";
const USER_ONLY_COMMANDS: &[&str] = &["answer", "send", "pair", "devices", "orchestrator-retry", "orchestrator-cancel", "orchestrator-acknowledge"];

/// Nodes share the user's account, so they could run the CLI that signs as the user. The
/// plugin's hook refuses it; a determined node could still hide the command, which is why
/// the phone and the Mac app keep their keys in the Secure Enclave.
pub(crate) fn forbidden_for_nodes(tool: Option<&str>, input: Option<&Value>) -> Option<String> {
    let field = |k: &str| input.and_then(|i| i.get(k)).and_then(Value::as_str).unwrap_or("");
    let touches_key = |s: &str| s.contains(USER_KEY_FILE) || (s.contains("Application Support") && s.to_lowercase().contains("alondra"));
    match tool? {
        "Bash" => {
            let command = field("command");
            let words: Vec<&str> = command.split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '`')).filter(|w| !w.is_empty()).collect();
            let runs_user_command = words
                .windows(2)
                .any(|w| (w[0] == "alondra" || w[0].ends_with("/alondra")) && USER_ONLY_COMMANDS.contains(&w[1]));
            if runs_user_command {
                return Some("Nodes cannot run `alondra answer`, `send`, `pair` or `devices`: they act as the user. Use your Alondra MCP tools (ask, present_result) instead.".into());
            }
            touches_key(command).then(|| "Nodes cannot touch the user's signing key.".into())
        }
        _ => ["file_path", "path", "pattern", "notebook_path"]
            .iter()
            .any(|k| touches_key(field(k)))
            .then(|| "Nodes cannot touch the user's signing key.".into()),
    }
}

/// The session bound to a Task works on it: its first hook after the assignment starts the
/// Task, as the assigned Agent, so it shows as working without the Agent calling `start_task`.
fn start_assigned(domain: &DomainRuntime, agent_id: &AgentId) -> Result<()> {
    let state = domain.state()?;
    let waiting = state.tasks.values()
        .filter(|task| task.agent_id.as_ref() == Some(agent_id) && !task.started && task.state == TaskState::NotStarted);
    for task in waiting {
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: format!("start-{}", crate::auth::random_hex(16)).into(),
            expected_revision: state.revision, command: Command::StartTask { task_id: task.id.clone() },
        };
        let actor = Actor::Agent { agent_id: agent_id.clone() };
        if let Err(error) = domain.execute(&actor, &envelope, None, || Ok(())) {
            log!("could not start {} for its working agent: {error:?}", task.id);
        }
    }
    Ok(())
}

/// A turn that ends while the Agent's Task has neither a result nor a question for anyone gets,
/// once per Task, the reminder 8ccd131 typed into such a node. A turn the reminder itself
/// continued is never held again.
fn unreported(domain: &DomainRuntime, agent_id: &AgentId, payload: &Value) -> Result<Option<String>> {
    if payload.get("stop_hook_active") == Some(&Value::Bool(true)) {
        return Ok(None);
    }
    let state = domain.state()?;
    let Some(task) = state.tasks.values().find(|task| task.agent_id.as_ref() == Some(agent_id)
        && matches!(task.state, TaskState::NotStarted | TaskState::Running)
        && task.final_presentation.is_none())
    else {
        return Ok(None);
    };
    if !domain.nudged.lock().expect("nudged tasks poisoned").insert(task.id.clone()) {
        return Ok(None);
    }
    Ok(Some(nudge_text(&crate::domain_messages::user_name(), &task.id)))
}

pub(crate) fn nudge_text(user: &str, task: &TaskId) -> String {
    format!(
        "[Alondra] Your turn ended without telling {user} anything through Alondra, so they cannot tell whether you are \
         done or waiting on them; your terminal does not reach them. The Alondra tools declare your state for task {task}: \
         `present_result` with is_final true when you are done (the answer in body, files in attachments), `ask` for a \
         decision (one question per decision, with options), `request_action` for something {user} does outside Alondra, \
         `wait_for_machine` when you are waiting for resources. If you are still working on your own (a build, an upload, \
         a review, a long command), there is nothing to send."
    )
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
        let labels: Vec<String> = options.iter().map(|option| format!("{} ({})", option.label, option.id)).collect();
        if !labels.is_empty() {
            text.push_str(&format!("\nOptions: {}.", labels.join(", ")));
        }
        if matches!(interaction.recipient, Recipient::Agent { .. } | Recipient::Orchestrator) {
            text.push_str(&format!("\nAnswer with answer_question(\"{}\", option: \"<option_id>\").", interaction.id));
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
        Some(option) if values.is_empty() => format!("{option}."),
        Some(option) => format!("{option}. {}", values.join("; ")),
        None => values.join("; "),
    };
    for attachment in &answer.attachments {
        text.push_str(&format!(" Open the attached file “{}” at {}.", attachment.name, attachment.content_reference));
    }
    text = text.trim_start().to_owned();
    if matches!(interaction.recipient, Recipient::Orchestrator) && matches!(interaction.author, Author::Agent { .. }) {
        text = format!("The orchestrator answered: {text}");
    }
    let question = interaction.body.trim();
    if matches!(interaction.author, Author::Orchestrator { .. }) {
        let request_title = match &interaction.scope {
            Scope::Orchestrator { request_id: Some(request_id) } => state.requests.get(request_id)
                .map(|request| request.title.as_deref().unwrap_or(&request.body)),
            Scope::Task { task_id } => state.tasks.get(task_id)
                .and_then(|task| state.requests.get(&task.request_id))
                .map(|request| request.title.as_deref().unwrap_or(&request.body)),
            _ => None,
        };
        text = match request_title {
            Some(title) => format!("About \"{question}\" (request \"{}\"): {text}", title.trim()),
            None => format!("About \"{question}\": {text}"),
        };
    } else if let Author::Agent { agent_id } = &interaction.author {
        let other_open = state.interactions.values().any(|other| {
            other.id != interaction.id && other.author == Author::Agent { agent_id: agent_id.clone() }
                && !other.withdrawn && matches!(&other.payload,
                    InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. })
        });
        if other_open {
            text = format!("About \"{question}\": {text}");
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

/// Agents with messages waiting, however their session was launched.
fn waiting_agents(domain: &DomainRuntime) -> Result<BTreeSet<AgentId>> {
    Ok(domain.store.lock().expect("domain store poisoned").pending_messages()?.into_iter()
        .filter(|message| message.recipient_kind == "agent")
        .filter_map(|message| message.agent_id.map(AgentId))
        .collect())
}

/// One pass over every background session with pending messages: a Schedule
/// launch knows its terminal, any other session is found in `claude agents`.
/// An interactive session gets its messages from its next Stop hook. A session
/// whose last write failed, that could not be found or that is interactive is
/// left alone for a while; a terminal that was not ready stays retryable, and
/// a write that may have landed is uncertain.
pub(crate) async fn deliver_terminals(domain: &DomainRuntime, home: &Path, sessions: &impl Sessions,
    terminal: &Arc<impl Terminal>, backoff: &mut Backoff) -> Result<()> {
    let state = domain.state()?;
    let mut known = terminals(&state);
    for agent in waiting_agents(domain)? {
        if !backoff.ready(&agent) {
            continue;
        }
        let short = match known.remove(&agent) {
            Some(short) => short,
            None => match sessions.find(&agent.0).await {
                Some(Session { interactive: false, short }) => short,
                _ => {
                    backoff.outcome(&agent, false);
                    continue;
                }
            },
        };
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
    crate::domain_holds::release_orphans(&domain)?;
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
        deliver_terminals(&domain, &home, &ClaudeAgents, &terminal, &mut backoff).await?;
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

    #[test]
    fn answered_question_delivery_contains_only_the_users_answer() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        answered_question(&domain, "agent-a");
        let state = domain.state().unwrap();
        let message = domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .find(|message| message.interaction_id.as_deref() == Some("question-agent-a"))
            .unwrap();
        let rendered = render(&state, &message);
        assert_eq!(rendered, "Yes.");
        assert!(!rendered.contains("data_epoch"));
        assert!(!rendered.contains("protocol"));
    }

    #[test]
    fn an_orchestrator_can_answer_a_node_question_from_the_delivered_text() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        command(&domain, Actor::System, "register", Command::RegisterAgent {
            agent_id: "agent-a".into(), name: "Agent A".into(),
        });
        command(&domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest {
            request_id: "request".into(), body: "Build a widget".into(), auto_merge: false,
        });
        command(&domain, Actor::User { user_id: "u".into() }, "task", Command::CreateTask {
            task_id: "task".into(), request_id: "request".into(), title: "Widget".into(),
            agent_id: Some("agent-a".into()), review_prs: false, dependencies: BTreeSet::new(), stack: None,
        });
        command(&domain, Actor::Agent { agent_id: "agent-a".into() }, "question", Command::AskQuestion {
            attachments: vec![], interaction_id: "question-1".into(), scope: Scope::Task { task_id: "task".into() },
            recipient: Recipient::Orchestrator, body: "Which API should I use?".into(), blocking: true,
            options: vec![QuestionOption { id: "new_api".into(), label: "New API".into(), description: None, inputs: vec![] }],
        });
        let state = domain.state().unwrap();
        let message = domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .find(|message| message.interaction_id.as_deref() == Some("question-1")).unwrap();
        let delivered = render(&state, &message);
        assert!(delivered.contains("Which API should I use?"), "{delivered}");
        assert!(delivered.contains("New API (new_api)"), "{delivered}");
        let parse_answer = |text: &str| {
            let id = text.split_once("Answer with answer_question(\"")?.1.split_once('"')?.0;
            let option = text.split_once("New API (")?.1.split_once(')')?.0;
            Some((id.to_owned(), option.to_owned()))
        };
        assert!(parse_answer(&delivered.replace("Answer with answer_question", "Omitted")).is_none(),
            "without the invocation line the recipient cannot recover the interaction ID");
        let (interaction_id, option_id) = parse_answer(&delivered).unwrap();
        command(&domain, Actor::Orchestrator { generation: 0 }, "answer", Command::AnswerQuestion {
            attachments: vec![], interaction_id: interaction_id.into(), option_id, inputs: Default::default(),
        });
        assert!(matches!(&domain.state().unwrap().interactions[&"question-1".into()].payload,
            InteractionPayload::Question { answer: Some(_), .. }));
    }

    #[test]
    fn answers_name_the_question_when_ambiguous_and_always_name_the_orchestrators_request() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        answered_question(&domain, "agent-a");
        command(&domain, Actor::Agent { agent_id: "agent-a".into() }, "another-question", Command::AskQuestion {
            attachments: vec![], interaction_id: "another-question".into(),
            scope: Scope::Task { task_id: "task-agent-a".into() }, recipient: Recipient::User,
            body: "Second choice?".into(), blocking: true,
            options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None, inputs: vec![] }],
        });
        let state = domain.state().unwrap();
        let message = domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .find(|message| message.interaction_id.as_deref() == Some("question-agent-a")).unwrap();
        assert_eq!(render(&state, &message), "About \"Choose\": Yes.");

        command(&domain, Actor::Orchestrator { generation: 0 }, "orchestrator-question", Command::AskQuestion {
            attachments: vec![], interaction_id: "orchestrator-question".into(),
            scope: Scope::Orchestrator { request_id: Some("request-agent-a".into()) },
            recipient: Recipient::User, body: "Ship this?".into(), blocking: true,
            options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None, inputs: vec![] }],
        });
        command(&domain, Actor::User { user_id: "u".into() }, "orchestrator-answer", Command::AnswerQuestion {
            attachments: vec![], interaction_id: "orchestrator-question".into(),
            option_id: "yes".into(), inputs: Default::default(),
        });
        let state = domain.state().unwrap();
        let message = domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .find(|message| message.interaction_id.as_deref() == Some("orchestrator-question")
                && message.recipient_kind == "orchestrator").unwrap();
        assert_eq!(render(&state, &message), "About \"Ship this?\" (request \"Work\"): Yes.");
    }

    #[test]
    fn a_users_option_and_text_reach_the_node_as_plain_language() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        command(&domain, Actor::System, "register", Command::RegisterAgent {
            agent_id: "agent-a".into(), name: "Agent A".into(),
        });
        command(&domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest {
            request_id: "request".into(), body: "Work".into(), auto_merge: false,
        });
        command(&domain, Actor::User { user_id: "u".into() }, "task", Command::CreateTask {
            task_id: "task".into(), request_id: "request".into(), title: "Task".into(),
            agent_id: Some("agent-a".into()), review_prs: false, dependencies: BTreeSet::new(), stack: None,
        });
        command(&domain, Actor::Agent { agent_id: "agent-a".into() }, "question", Command::AskQuestion {
            attachments: vec![], interaction_id: "question".into(), scope: Scope::Task { task_id: "task".into() },
            recipient: Recipient::User, body: "Which approach?".into(), blocking: true,
            options: vec![QuestionOption { id: "new".into(), label: "New API".into(), description: None,
                inputs: vec![InputSpec { id: "details".into(), kind: InputKind::Text, required: true }] }],
        });
        command(&domain, Actor::User { user_id: "u".into() }, "answer", Command::AnswerQuestion {
            attachments: vec![], interaction_id: "question".into(), option_id: "new".into(),
            inputs: BTreeMap::from([("details".into(), InputValue::Text("Keep the old response format".into()))]),
        });
        let state = domain.state().unwrap();
        let message = domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .find(|message| message.interaction_id.as_deref() == Some("question")
                && message.recipient_kind == "agent").unwrap();
        assert_eq!(render(&state, &message), "New API. Keep the old response format");
    }

    fn free_text_question(domain: &DomainRuntime, id: &str, actor: Actor, scope: Scope, recipient: Recipient, body: &str) {
        command(domain, actor, &format!("ask-{id}"), Command::AskQuestion {
            attachments: vec![], interaction_id: id.into(), scope, recipient,
            body: body.into(), blocking: true, options: vec![],
        });
    }

    fn free_text_answer(domain: &DomainRuntime, actor: Actor, id: &str, text: &str) {
        command(domain, actor, &format!("answer-{id}"), Command::AnswerQuestion {
            attachments: vec![], interaction_id: id.into(), option_id: String::new(),
            inputs: BTreeMap::from([("text".into(), InputValue::Text(text.into()))]),
        });
    }

    fn rendered_for(domain: &DomainRuntime, id: &str, recipient_kind: &str) -> String {
        let state = domain.state().unwrap();
        let message = domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .find(|message| message.interaction_id.as_deref() == Some(id) && message.recipient_kind == recipient_kind)
            .unwrap();
        render(&state, &message)
    }

    fn node_with_task(domain: &DomainRuntime) {
        command(domain, Actor::System, "register", Command::RegisterAgent {
            agent_id: "agent-a".into(), name: "Agent A".into(),
        });
        command(domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest {
            request_id: "request".into(), body: "Work".into(), auto_merge: false,
        });
        command(domain, Actor::User { user_id: "u".into() }, "task", Command::CreateTask {
            task_id: "task".into(), request_id: "request".into(), title: "Task".into(),
            agent_id: Some("agent-a".into()), review_prs: false, dependencies: BTreeSet::new(), stack: None,
        });
    }

    fn agent_actor() -> Actor {
        Actor::Agent { agent_id: "agent-a".into() }
    }

    fn user_actor() -> Actor {
        Actor::User { user_id: "u".into() }
    }

    fn task_scope() -> Scope {
        Scope::Task { task_id: "task".into() }
    }

    #[test]
    fn a_free_text_answer_reaches_the_node_as_the_text_alone() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        free_text_question(&domain, "q", agent_actor(), task_scope(), Recipient::User, "Which name?");
        free_text_answer(&domain, user_actor(), "q", "Call it Atlas");
        assert_eq!(rendered_for(&domain, "q", "agent"), "Call it Atlas");
    }

    #[test]
    fn a_free_text_answer_names_the_question_only_when_another_is_open() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        free_text_question(&domain, "q", agent_actor(), task_scope(), Recipient::User, "Which name?");
        free_text_question(&domain, "other", agent_actor(), task_scope(), Recipient::User, "Which color?");
        free_text_answer(&domain, user_actor(), "q", "Call it Atlas");
        assert_eq!(rendered_for(&domain, "q", "agent"), "About \"Which name?\": Call it Atlas");
    }

    #[test]
    fn a_free_text_answer_to_the_orchestrator_names_the_question_and_request() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        free_text_question(&domain, "q", Actor::Orchestrator { generation: 0 },
            Scope::Orchestrator { request_id: Some("request".into()) }, Recipient::User, "Which name?");
        free_text_answer(&domain, user_actor(), "q", "Call it Atlas");
        assert_eq!(rendered_for(&domain, "q", "orchestrator"),
            "About \"Which name?\" (request \"Work\"): Call it Atlas");
    }

    #[test]
    fn an_orchestrators_free_text_answer_to_a_node_says_who_answered_and_never_the_user() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        free_text_question(&domain, "q", agent_actor(), task_scope(), Recipient::Orchestrator, "Which name?");
        free_text_answer(&domain, Actor::Orchestrator { generation: 0 }, "q", "Call it Atlas");
        let rendered = rendered_for(&domain, "q", "agent");
        assert_eq!(rendered, "The orchestrator answered: Call it Atlas");
        assert!(!rendered.contains("user"));
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
                let _ = client.write_u8(ack).await;
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
        assert_eq!(context, "Yes.");
        assert!(!context.contains("data_epoch") && !context.contains("revision=")
            && !context.contains("task_id=") && !context.contains("interaction_id=")
            && !context.contains("option_id"), "{context}");
        assert!(agent_outbox(&domain, "session-a").is_empty());
        assert_eq!(attempts(&domain), vec![(pending[0].0, "handed".into(), None)]);

        let (reply, _) = hook(&domain, json!({"session_id": "session-a", "hook_event_name": "Stop"}), Some(6)).await;
        assert!(reminder_only(&reply), "a handed message is never delivered twice: {reply}");

        answered_question(&domain, "session-b");
        let id = agent_outbox(&domain, "session-b")[0].0;
        let (reply, result) = hook(&domain, json!({"session_id": "session-b", "hook_event_name": "Stop"}), None).await;
        assert!(result.is_err());
        let uncertain_context = reply["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert_eq!(uncertain_context, "Yes.");
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
        assert_eq!(context, "Use the new API\n\nYes.");
        assert!(agent_outbox(&domain, "session-a").is_empty());
        domain.held.lock().unwrap().entry("session-a".into()).or_default().push("Only this".into());
        let (body, _) = hook(&domain, stop.clone(), Some(6)).await;
        assert_eq!(body["hookSpecificOutput"]["additionalContext"], "Only this");
        let (body, _) = hook(&domain, stop, Some(6)).await;
        assert!(reminder_only(&body), "a held message is handed once: {body}");
    }

    fn reminder_only(reply: &Value) -> bool {
        reply["hookSpecificOutput"]["additionalContext"].as_str()
            .is_some_and(|text| text.starts_with("[Alondra] Your turn ended") && !text.contains("Yes.") && !text.contains("Only this"))
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
        assert_eq!(context, "Yes.\n\nThe user authorized merging the presented pull request.");
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
    async fn a_session_whose_start_hook_beat_its_registration_connects_on_its_next_hook() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        hook(&domain, json!({"session_id": "session-a", "hook_event_name": "SessionStart"}), Some(6)).await.1.unwrap();
        command(&domain, Actor::System, "register", Command::RegisterAgent {
            agent_id: "session-a".into(), name: "A".into(),
        });
        assert!(!domain.state().unwrap().agents[&"session-a".into()].connected);
        hook(&domain, json!({"session_id": "session-a", "hook_event_name": "UserPromptSubmit"}), Some(6)).await.1.unwrap();
        assert!(domain.state().unwrap().agents[&"session-a".into()].connected);
        let revision = domain.state().unwrap().revision;
        hook(&domain, json!({"session_id": "session-a", "hook_event_name": "PostToolUse", "tool_name": "Bash"}), Some(6)).await.1.unwrap();
        assert_eq!(domain.state().unwrap().revision, revision, "a connected Agent's hooks record nothing more");
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

    #[tokio::test]
    async fn a_node_cannot_act_as_the_user_and_a_session_alondra_does_not_follow_is_untouched() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        command(&domain, Actor::System, "register", Command::RegisterAgent { agent_id: "session-a".into(), name: "A".into() });
        let pre = |session: &str, tool: &str, input: &Value| json!({"session_id": session, "hook_event_name": "PreToolUse",
            "tool_name": tool, "tool_input": input});
        for (tool, input) in [
            ("Bash", json!({"command": "alondra answer 55 Merge"})),
            ("Bash", json!({"command": "cd /tmp && ~/.local/bin/alondra orchestrator-retry"})),
            ("Read", json!({"file_path": "/Users/x/.alondra/tui-key.pem"})),
        ] {
            let (reply, result) = hook(&domain, pre("session-a", tool, &input), None).await;
            result.unwrap();
            assert_eq!(reply["hookSpecificOutput"]["permissionDecision"], "deny", "{input}");
            assert_eq!(hook(&domain, pre("stranger", tool, &input), None).await.0, json!({}), "{input}");
        }
        assert_eq!(hook(&domain, pre("session-a", "Bash", &json!({"command": "alondra status"})), None).await.0, json!({}));
    }

    #[tokio::test]
    async fn an_ordinary_hook_hangs_up_after_its_reply_without_waiting_for_an_acknowledgement() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        let (mut client, server) = UnixStream::pair().unwrap();
        let body = serde_json::to_vec(&json!({"launched": true, "payload": {"session_id": "agent-a",
            "hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": "ls"}}})).unwrap();
        client.write_u32(body.len() as u32).await.unwrap();
        client.write_all(&body).await.unwrap();
        handle(server, &domain).await.expect("nothing was handed, so no acknowledgement is awaited");
        let mut reply = vec![];
        client.read_to_end(&mut reply).await.unwrap();
        assert_eq!(&reply[..2], 200u16.to_be_bytes());
        assert_eq!(serde_json::from_slice::<Value>(&reply[2..]).unwrap(), json!({}));
    }

    fn task_state(domain: &DomainRuntime) -> TaskState {
        domain.state().unwrap().tasks[&TaskId::from("task")].state
    }

    #[tokio::test]
    async fn the_assigned_sessions_first_activity_starts_its_task() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "SessionStart"}), Some(6)).await.1.unwrap();
        assert_eq!(task_state(&domain), TaskState::NotStarted, "a session that only opened has not worked yet");
        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "UserPromptSubmit", "prompt": "Work"}), Some(6)).await.1.unwrap();
        assert_eq!(task_state(&domain), TaskState::Running);
        let revision = domain.state().unwrap().revision;
        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "PreToolUse", "tool_name": "Bash"}), Some(6)).await.1.unwrap();
        assert_eq!(domain.state().unwrap().revision, revision, "a started Task is not started again");
        command(&domain, agent_actor(), "start", Command::StartTask { task_id: "task".into() });
        assert_eq!(task_state(&domain), TaskState::Running, "start_task still works when the Agent calls it");
    }

    #[tokio::test]
    async fn another_sessions_activity_never_starts_the_task() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        command(&domain, Actor::System, "register-b", Command::RegisterAgent { agent_id: "agent-b".into(), name: "B".into() });
        hook(&domain, json!({"session_id": "agent-b", "hook_event_name": "PreToolUse", "tool_name": "Bash"}), Some(6)).await.1.unwrap();
        hook(&domain, json!({"session_id": "stranger", "hook_event_name": "PreToolUse", "tool_name": "Bash"}), Some(6)).await.1.unwrap();
        assert_eq!(task_state(&domain), TaskState::NotStarted);
    }

    #[tokio::test]
    async fn a_turn_that_ends_without_a_result_is_reminded_once() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        let stop = json!({"session_id": "agent-a", "hook_event_name": "Stop"});
        let (reply, result) = hook(&domain, stop.clone(), Some(6)).await;
        result.unwrap();
        let context = reply["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(context.starts_with("[Alondra] Your turn ended") && context.contains("`present_result`")
            && context.contains("task task"), "{context}");
        let (reply, _) = hook(&domain, stop, Some(6)).await;
        assert_eq!(reply, json!({}), "the reminder comes once, never in a loop");
    }

    #[tokio::test]
    async fn a_turn_continued_by_a_stop_hook_or_ending_after_a_result_is_not_reminded() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        let (reply, _) = hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "Stop", "stop_hook_active": true}), Some(6)).await;
        assert_eq!(reply, json!({}));
        command(&domain, agent_actor(), "present", Command::PresentResult {
            attachments: vec![], task_id: "task".into(), interaction_id: "delivery".into(),
            body: "Done".into(), targets: vec![DeliveryTarget::Text { text: "The answer".into() }], is_final: true,
        });
        let (reply, _) = hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "Stop"}), Some(6)).await;
        assert_eq!(reply, json!({}), "a presented result already tells the user");
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

    #[derive(Default)]
    struct Listed(BTreeMap<String, Session>);
    impl Sessions for Listed {
        async fn find(&self, session_id: &str) -> Option<Session> {
            self.0.get(session_id).cloned()
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
        deliver_terminals(&domain, &fixture.0, &Listed::default(), &terminal, &mut Backoff::default()).await.unwrap();
        assert!(terminal.writes.lock().unwrap().is_empty(), "a session without a known terminal is not typed into");
        assert_eq!(agent_outbox(&domain, "session-a").len(), 1);

        launched_by_schedule(&domain, "session-a", "ab1234");
        let id = agent_outbox(&domain, "session-a")[0].0;
        deliver_terminals(&domain, &fixture.0, &Listed::default(), &terminal, &mut backoff).await.unwrap();
        let writes = terminal.writes.lock().unwrap().clone();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, "ab1234");
        assert_eq!(writes[0].1, "Yes.");
        assert!(!writes[0].1.contains("data_epoch") && !writes[0].1.contains("revision=")
            && !writes[0].1.contains("interaction_id="), "{}", writes[0].1);
        assert!(agent_outbox(&domain, "session-a").is_empty());
        assert!(attempts(&domain).contains(&(id, "handed".into(), None)));
        deliver_terminals(&domain, &fixture.0, &Listed::default(), &terminal, &mut backoff).await.unwrap();
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
        deliver_terminals(&domain, &fixture.0, &Listed::default(), &terminal, &mut backoff).await.unwrap();
        assert_eq!(agent_outbox(&domain, "session-a"), vec![(second, "retryable".into())]);
        deliver_terminals(&domain, &fixture.0, &Listed::default(), &terminal, &mut backoff).await.unwrap();
        assert_eq!(terminal.writes.lock().unwrap().len(), 2, "a failed session backs off");
        let mut fresh = Backoff::default();
        *terminal.outcome.lock().unwrap() = Err(WriteFailure::Uncertain);
        deliver_terminals(&domain, &fixture.0, &Listed::default(), &terminal, &mut fresh).await.unwrap();
        assert!(agent_outbox(&domain, "session-a").is_empty());
        assert!(attempts(&domain).contains(&(second, "uncertain".into(), Some("terminal_write_uncertain".into()))));
        drop(domain);
        let reopened = fixture.open();
        assert!(agent_outbox(&reopened, "session-a").is_empty(), "an uncertain write is not retyped after restart");
    }

    #[tokio::test]
    async fn a_node_launched_for_a_request_gets_its_pending_messages_typed_into_its_background_session() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        answered_question(&domain, "launched");
        answered_question(&domain, "interactive");
        answered_question(&domain, "gone");
        let sessions = Listed(BTreeMap::from([
            ("launched".to_string(), Session { short: "fe12d68c".into(), interactive: false }),
            ("interactive".to_string(), Session { short: "ab12cd34".into(), interactive: true }),
        ]));
        let terminal = Arc::new(FakeTerminal { writes: Mutex::new(vec![]), outcome: Mutex::new(Ok(())) });
        let id = agent_outbox(&domain, "launched")[0].0;
        deliver_terminals(&domain, &fixture.0, &sessions, &terminal, &mut Backoff::default()).await.unwrap();
        assert_eq!(*terminal.writes.lock().unwrap(), [("fe12d68c".to_string(), "Yes.".to_string())]);
        assert!(agent_outbox(&domain, "launched").is_empty());
        assert!(attempts(&domain).contains(&(id, "handed".into(), None)));
        assert_eq!(agent_outbox(&domain, "interactive").len(), 1, "an interactive session takes its messages at its next Stop hook");
        assert_eq!(agent_outbox(&domain, "gone").len(), 1, "a session Claude no longer lists keeps its messages");
    }

    fn working_agent(domain: &DomainRuntime, agent: &str) {
        let user = || Actor::User { user_id: "u".into() };
        command(domain, Actor::System, &format!("register-{agent}"), Command::RegisterAgent { agent_id: agent.into(), name: agent.into() });
        command(domain, user(), &format!("request-{agent}"),
            Command::CreateRequest { request_id: format!("request-{agent}").into(), body: "Work".into(), auto_merge: false });
        command(domain, user(), &format!("task-{agent}"), Command::CreateTask {
            task_id: format!("task-{agent}").into(), request_id: format!("request-{agent}").into(),
            title: "Task".into(), agent_id: Some(agent.into()), review_prs: false, dependencies: BTreeSet::new(), stack: None,
        });
    }

    fn permission(agent: &str) -> Value {
        json!({"session_id": agent, "hook_event_name": "PermissionRequest", "tool_name": "Bash",
            "tool_input": {"command": "rm -rf build", "description": "Clean the build"}})
    }

    fn plan(agent: &str) -> Value {
        json!({"session_id": agent, "hook_event_name": "PreToolUse", "tool_name": "ExitPlanMode",
            "tool_input": {"plan": "# Ship the parser\n\n1. Split the lexer"}})
    }

    async fn open_hold(domain: &DomainRuntime) -> InteractionId {
        let mut changed = domain.subscribe();
        loop {
            let state = domain.state().unwrap();
            let open = state.holds.keys().find(|id| matches!(state.interactions[*id].payload,
                InteractionPayload::Question { answer: None, .. }) && !state.interactions[*id].withdrawn);
            if let Some(id) = open { return id.clone() }
            changed.changed().await.unwrap();
        }
    }

    /// The hook as Claude Code runs it, answered by the user from a card while it waits.
    async fn held_hook(domain: &DomainRuntime, payload: Value, option: &str, note: Option<&str>) -> (Value, InteractionId) {
        let (mut client, server) = UnixStream::pair().unwrap();
        let body = serde_json::to_vec(&json!({"launched": true, "payload": payload})).unwrap();
        client.write_u32(body.len() as u32).await.unwrap();
        client.write_all(&body).await.unwrap();
        let handler = handle_with(server, domain, |_| true);
        tokio::pin!(handler);
        let id = tokio::select! {
            id = open_hold(domain) => id,
            result = &mut handler => panic!("the hook was answered without waiting: {result:?}"),
        };
        let inputs = note.map(|note| BTreeMap::from([(Hold::NOTE.to_owned(), InputValue::Text(note.into()))])).unwrap_or_default();
        command(domain, Actor::User { user_id: "u".into() }, &format!("answer-{id}"), Command::AnswerQuestion {
            attachments: vec![], interaction_id: id.clone(), option_id: option.into(), inputs,
        });
        let mut bytes = vec![];
        let (result, _) = tokio::join!(handler, async {
            client.read_to_end(&mut bytes).await.unwrap();
            let _ = client.write_u8(6).await;
        });
        result.unwrap();
        assert_eq!(u16::from_be_bytes([bytes[0], bytes[1]]), 200);
        (serde_json::from_slice(&bytes[2..]).unwrap(), id)
    }

    #[tokio::test]
    async fn a_permission_prompt_waits_for_allow_or_deny_and_the_reason_follows_as_a_message() {
        for (option, note, behavior, message, follows) in [
            (Hold::ALLOW, None, "allow", None, None),
            (Hold::DENY, None, "deny", Some("denies it"), None),
            (Hold::DENY, Some("Branch first"), "deny", Some("denied it; the message follows"), Some("Branch first")),
        ] {
            let fixture = Fixture::new();
            let domain = fixture.open();
            working_agent(&domain, "agent-a");
            let (reply, id) = held_hook(&domain, permission("agent-a"), option, note).await;
            let output = &reply["hookSpecificOutput"];
            assert_eq!(output["hookEventName"], "PermissionRequest");
            assert_eq!(output["decision"]["behavior"], behavior, "{option}");
            let said = output["decision"]["message"].as_str();
            assert_eq!(said.is_some(), message.is_some(), "{option}");
            assert!(message.is_none_or(|message| said.unwrap().ends_with(message)), "{option}: {said:?}");
            let state = domain.state().unwrap();
            assert_eq!(state.holds[&id], Hold::Permission);
            assert!(state.interactions[&id].body.contains("Bash: Clean the build"), "{}", state.interactions[&id].body);
            assert!(agent_outbox(&domain, "agent-a").is_empty(), "the hook, not the outbox, hands the answer");
            assert_eq!(crate::domain_messages::take_held(&domain, &"agent-a".into()), follows.map(str::to_owned).into_iter().collect::<Vec<_>>());
        }
    }

    #[tokio::test]
    async fn a_plan_waits_for_approve_or_request_changes() {
        for (option, decision) in [(Hold::APPROVE, "allow"), (Hold::REQUEST_CHANGES, "deny")] {
            let fixture = Fixture::new();
            let domain = fixture.open();
            working_agent(&domain, "agent-a");
            let (reply, id) = held_hook(&domain, plan("agent-a"), option, None).await;
            let output = &reply["hookSpecificOutput"];
            assert_eq!((output["hookEventName"].as_str(), output["permissionDecision"].as_str()), (Some("PreToolUse"), Some(decision)));
            if decision == "allow" {
                assert_eq!(output["updatedInput"], plan("agent-a")["tool_input"]);
            } else {
                assert!(output["permissionDecisionReason"].as_str().unwrap().ends_with("requests changes to the plan"));
            }
            assert_eq!(domain.state().unwrap().holds[&id], Hold::Plan);
        }
    }

    #[tokio::test]
    async fn only_a_background_agent_with_a_task_is_held() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        command(&domain, Actor::System, "register-idle", Command::RegisterAgent { agent_id: "idle".into(), name: "idle".into() });
        let mut subagent_plan = plan("agent-a");
        subagent_plan["agent_id"] = json!("sub-1");
        let mut subagent_question = ask("agent-a");
        subagent_question["agent_id"] = json!("sub-1");
        for (payload, background) in [(permission("agent-a"), false), (subagent_plan, true), (subagent_question, true),
            (elicitation("agent-a"), false), (permission("idle"), true), (permission("stranger"), true)] {
            assert!(matches!(respond(&domain, &payload, |_| background).unwrap(), Reply::Plain(body) if body == json!({})), "{payload}");
        }
        assert!(domain.state().unwrap().holds.is_empty());
    }

    fn ask(agent: &str) -> Value {
        json!({"session_id": agent, "hook_event_name": "PreToolUse", "tool_name": "AskUserQuestion",
            "tool_input": {"questions": [
                {"question": "Which database?", "options": [{"label": "Postgres", "description": "Shared server"}, {"label": "SQLite"}]},
                {"question": "Also migrate the old rows?"}]}})
    }

    fn elicitation(agent: &str) -> Value {
        json!({"session_id": agent, "hook_event_name": "Elicitation", "mcp_server_name": "github", "message": "Which repository?",
            "requested_schema": {"type": "object", "properties": {"repo": {"type": "string", "title": "Repository"}}}})
    }

    #[tokio::test]
    async fn ask_user_question_waits_and_answers_every_question_with_the_choice_or_the_users_words() {
        for (option, note, said) in [("o1", None, "SQLite"), ("", Some("MySQL"), "MySQL")] {
            let fixture = Fixture::new();
            let domain = fixture.open();
            working_agent(&domain, "agent-a");
            let (reply, id) = held_hook(&domain, ask("agent-a"), option, note).await;
            let output = &reply["hookSpecificOutput"];
            assert_eq!((output["hookEventName"].as_str(), output["permissionDecision"].as_str()), (Some("PreToolUse"), Some("allow")));
            assert_eq!(output["updatedInput"]["questions"], ask("agent-a")["tool_input"]["questions"]);
            assert_eq!(output["updatedInput"]["answers"], json!({"Which database?": said, "Also migrate the old rows?": said}));
            let state = domain.state().unwrap();
            assert_eq!(state.holds[&id], Hold::Question);
            let InteractionPayload::Question { options, .. } = &state.interactions[&id].payload else { panic!("question") };
            assert_eq!(options.iter().map(|o| (o.id.as_str(), o.label.as_str())).collect::<Vec<_>>(), [("o0", "Postgres"), ("o1", "SQLite")]);
            assert_eq!(state.interactions[&id].body, "Which database?\n\n- Also migrate the old rows?");
            assert!(agent_outbox(&domain, "agent-a").is_empty());
        }
    }

    #[tokio::test]
    async fn an_mcp_elicitation_waits_for_accept_with_the_form_values_or_decline() {
        for (option, note, expected) in [
            (Hold::ACCEPT, Some("bitomule/alondra"), json!({"hookEventName": "Elicitation", "action": "accept", "content": {"repo": "bitomule/alondra"}})),
            (Hold::DECLINE, None, json!({"hookEventName": "Elicitation", "action": "decline"})),
        ] {
            let fixture = Fixture::new();
            let domain = fixture.open();
            working_agent(&domain, "agent-a");
            let (reply, id) = held_hook(&domain, elicitation("agent-a"), option, note).await;
            assert_eq!(reply["hookSpecificOutput"], expected);
            let state = domain.state().unwrap();
            assert_eq!(state.holds[&id], Hold::Elicitation);
            assert!(state.interactions[&id].body.starts_with("Which repository?\n\nMCP server “github” requests your input."));
            let InteractionPayload::Question { options, .. } = &state.interactions[&id].payload else { panic!("question") };
            assert_eq!(options[0].description.as_deref(), Some("Write the Repository"));
        }
    }

    #[tokio::test]
    async fn a_hook_that_goes_away_withdraws_its_card_and_a_restart_releases_the_rest() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let (mut client, server) = UnixStream::pair().unwrap();
        let body = serde_json::to_vec(&json!({"launched": true, "payload": permission("agent-a")})).unwrap();
        client.write_u32(body.len() as u32).await.unwrap();
        client.write_all(&body).await.unwrap();
        let id = {
            let handler = handle_with(server, &domain, |_| true);
            tokio::pin!(handler);
            let id = tokio::select! {
                id = open_hold(&domain) => id,
                result = &mut handler => panic!("the hook was answered without waiting: {result:?}"),
            };
            drop(client);
            handler.await.unwrap();
            id
        };
        assert!(domain.state().unwrap().interactions[&id].withdrawn);
        let Reply::Held(orphan) = respond(&domain, &plan("agent-a"), |_| true).unwrap() else { panic!("held") };
        drop(domain);
        let reopened = fixture.open();
        crate::domain_holds::release_orphans(&reopened).unwrap();
        let state = reopened.state().unwrap();
        assert!(state.interactions[&orphan.id].withdrawn);
        assert!(state.projection().inbox.iter().all(|item| !item.pending || !matches!(item.kind.as_str(), "permission" | "plan")));
    }
}

#[cfg(test)]
mod node_guard_tests {
    use super::*;

    fn bash(c: &str) -> Option<String> {
        forbidden_for_nodes(Some("Bash"), Some(&json!({"command": c})))
    }

    #[test]
    fn nodes_cannot_answer_as_the_user() {
        assert!(bash("alondra answer 55 Mergea").is_some());
        assert!(bash("cd /tmp && ~/.local/bin/alondra send orquestador hola").is_some());
        assert!(bash("cat ~/.alondra/tui-key.pem").is_some());
        assert!(bash("ls ~/Library/'Application Support'/com.bitomule.alondra.mac").is_some());
        assert!(forbidden_for_nodes(Some("Read"), Some(&json!({"file_path": "/Users/x/.alondra/tui-key.pem"}))).is_some());
    }

    #[test]
    fn ordinary_commands_pass() {
        assert!(bash("alondra status").is_none());
        assert!(bash("alondra doctor --json").is_none());
        assert!(bash("git commit -m 'answer the question'").is_none());
        assert!(forbidden_for_nodes(Some("Read"), Some(&json!({"file_path": "/tmp/x.md"}))).is_none());
    }
}
