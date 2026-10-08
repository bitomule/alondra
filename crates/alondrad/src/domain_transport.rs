//! V2-only producer transport. Outbox messages for an Agent reach its Claude session through
//! the connection its node mod holds open (`node_connection`): written down it the moment they
//! exist, handed once the mod acknowledges them. A handoff is technical evidence only.
//!
//! Hooks observe sessions the domain already knows. They never create an
//! Agent, a Task or a decision. The one transition they make is the one a
//! session's own activity proves: its assigned Task has started.
use crate::domain_agent_lifecycle as lifecycle;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::OutboxMessage;
use crate::node_reports::Report;
use alondra_core::delivery::State;
use alondra_core::domain::{
    Actor, AgentId, Author, Command, CommandEnvelope, DomainState, InputValue, InteractionId, InteractionPayload,
    PROTOCOL_VERSION, Recipient, Scope, ScheduleLaunchState, TaskId, TaskState,
};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

const MAX_HOOK_BYTES: u64 = 25 * 1024 * 1024;

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
    let mut frame: Value = serde_json::from_slice(&input).context("invalid local hook envelope")?;
    if frame.get("probe").is_some() {
        stream.write_all(&crate::hook_socket::probe_ack()).await?;
        return Ok(());
    }
    if let Some(report) = frame.get_mut("node_event").map(Value::take) {
        return node_event(stream, domain, report).await;
    }
    if let Some(hello) = frame.get_mut("node_connect").map(Value::take) {
        return node_connection(stream, domain, hello).await;
    }
    if let Some(ack) = frame.get_mut("node_ack").map(Value::take) {
        return node_ack(stream, domain, ack).await;
    }
    if let Some(card) = frame.get_mut("node_hold").map(Value::take) {
        return node_hold(stream, domain, card).await;
    }
    let envelope: Envelope = serde_json::from_value(frame).context("invalid local hook envelope")?;
    let reply = respond(domain, &envelope.payload)?;
    let body = match reply {
        Reply::Plain(body) => body,
        Reply::Held(held) => {
            let answer = tokio::select! {
                answer = crate::domain_holds::wait(domain, &held.id) => answer,
                gone = stream.read_u8() => {
                    log!("hook holding {} went away: {gone:?}", held.id);
                    return crate::domain_holds::withdraw(domain, &held.id);
                }
            };
            answered(&held, answer?.and_then(|answers| answers.into_iter().next()))
        }
        Reply::Block { reason, session } => {
            let unregistered = unregistered_pr(domain, &session).await.unwrap_or_default();
            json!({"decision": "block", "reason": format!("{reason}{unregistered}")})
        }
    };
    let bytes = serde_json::to_vec(&body)?;
    stream.write_all(&200u16.to_be_bytes()).await?;
    stream.write_all(&bytes).await?;
    stream.shutdown().await?;
    Ok(())
}

async fn refuse(mut stream: UnixStream, reason: String) -> Result<()> {
    stream.write_all(&400u16.to_be_bytes()).await?;
    stream.write_all(reason.as_bytes()).await?;
    stream.shutdown().await?;
    bail!(reason)
}

/// One report of a node mod, as `alondra node event` hands it over.
async fn node_event(mut stream: UnixStream, domain: &DomainRuntime, report: Value) -> Result<()> {
    let envelope: crate::node_reports::Envelope = match serde_json::from_value(report) {
        Ok(envelope) => envelope,
        Err(error) => return refuse(stream, format!("invalid node report: {error}")).await,
    };
    match &envelope.report {
        Report::SessionEnd | Report::TurnComplete { aborted: true, .. } => {
            crate::domain_holds::release_session(domain, &envelope.session_id)?
        }
        Report::ToolEnd { tool_use_id: Some(call) } => crate::domain_holds::release_call(domain, &envelope.session_id, call)?,
        _ => {}
    }
    let ended = matches!(envelope.report, Report::SessionEnd).then(|| envelope.session_id.clone());
    let measured = matches!(envelope.report, Report::Measure { .. });
    domain.live.mods.apply(envelope, alondra_core::now_millis());
    if measured {
        domain.live.note_measures();
    }
    if let Some(session) = ended {
        record_node_cost(domain, &session);
        domain.live.losses.report(&session);
    }
    stream.write_all(&200u16.to_be_bytes()).await?;
    stream.write_all(b"{}").await?;
    stream.shutdown().await?;
    Ok(())
}

/// A card a node puts to the user, answered on the same connection. A mod helper that goes away
/// leaves the card open: the mod's wait was cut, not the question, and its next post finds the card.
/// A permission hook that goes away takes its card with it.
async fn node_hold(mut stream: UnixStream, domain: &DomainRuntime, card: Value) -> Result<()> {
    let card: crate::domain_holds::Card = match serde_json::from_value(card) {
        Ok(card) => card,
        Err(error) => return refuse(stream, format!("invalid hold: {error}")).await,
    };
    let reply = match crate::domain_holds::post(domain, &card, domain.live.mods.is_node(&card.session_id))? {
        crate::domain_holds::Posted::NotHeld => json!({"held": false}),
        crate::domain_holds::Posted::Denied(reason) => crate::domain_holds::denied(card.kind, &reason),
        crate::domain_holds::Posted::Held(id) => {
            let answer = tokio::select! {
                answer = crate::domain_holds::wait(domain, &id) => answer?,
                _ = stream.read_u8() => {
                    return if card.withdraw_when_gone { crate::domain_holds::withdraw(domain, &id) } else { Ok(()) };
                }
            };
            let state = domain.state()?;
            match (answer, state.holds.get(&id)) {
                (Some(answer), Some(hold)) => {
                    crate::domain_holds::mod_reply(*hold, &card.input, &card.suggestions, &answer, &crate::domain_messages::user_name())
                }
                _ if card.kind == crate::domain_holds::Kind::Plan => {
                    crate::domain_holds::denied(card.kind, &crate::domain_holds::plan_gone(domain, &card.session_id)?)
                }
                _ => json!({"held": false}),
            }
        }
    };
    stream.write_all(&200u16.to_be_bytes()).await?;
    stream.write_all(&serde_json::to_vec(&reply)?).await?;
    stream.shutdown().await?;
    Ok(())
}

/// The connection a node mod holds open for its session's life. While it is open the node is
/// reachable and the moment it closes, from either side, it is not. Down it go the session's
/// pending outbox messages, each once per connection and again on the next until acknowledged,
/// the direct messages and the interrupts addressed to it.
async fn node_connection(mut stream: UnixStream, domain: &DomainRuntime, hello: Value) -> Result<()> {
    let hello: crate::node_reports::Hello = match serde_json::from_value(hello) {
        Ok(hello) => hello,
        Err(error) => return refuse(stream, format!("invalid node connection: {error}")).await,
    };
    let session = hello.session_id.clone();
    if domain.live.sandbox.load(std::sync::atomic::Ordering::Relaxed) {
        register_probe(domain, &session)?;
    }
    let launch_key = hello.launch_key.clone();
    let number = domain.live.mods.connect(hello, alondra_core::now_millis());
    if let Some(key) = launch_key
        && let Err(error) = crate::domain_sessions::reissue(domain, &key, &AgentId(session.clone()))
    {
        log!("could not issue {session} a new capability: {error:#}");
    }
    if let Some(pid) = stream.peer_cred().ok().and_then(|credentials| credentials.pid()) {
        domain.live.mods.set_peer(&session, pid as u32);
    }
    let mut lines = domain.live.pushes.attach(&session, number);
    let mut changed = domain.subscribe();
    let held = async {
        stream.write_all(&200u16.to_be_bytes()).await?;
        let mut sent = BTreeSet::new();
        let mut buffer = [0u8; 256];
        loop {
            for line in pending_lines(domain, &session, &mut sent)? {
                stream.write_all(format!("{line}\n").as_bytes()).await?;
            }
            tokio::select! {
                read = stream.read(&mut buffer) => {
                    if read? == 0 {
                        return Ok(());
                    }
                }
                line = lines.recv() => match line {
                    Some(line) => stream.write_all(format!("{line}\n").as_bytes()).await?,
                    None => return Ok(()),
                },
                _ = changed.changed() => {}
            }
        }
    }
    .await;
    domain.live.pushes.detach(&session, number);
    if domain.live.mods.disconnect(&session, number) {
        domain.live.losses.report(&session);
    }
    held.or_else(|error: anyhow::Error| match error.downcast_ref::<std::io::Error>().map(std::io::Error::kind) {
        Some(std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe) => Ok(()),
        _ => Err(error),
    })
}

fn register_probe(domain: &DomainRuntime, session: &str) -> Result<()> {
    let state = domain.state()?;
    if state.agents.contains_key(&AgentId(session.to_owned())) {
        return Ok(());
    }
    let short = &session[..session.len().min(8)];
    let user = || Actor::User { user_id: "sandbox".into() };
    let steps = [
        (Actor::System, "register", Command::RegisterAgent { agent_id: session.into(), name: format!("test/{short}") }),
        (user(), "request", Command::CreateRequest {
            request_id: format!("sandbox-probe-{session}").into(), body: "Probe".into(), auto_merge: false, maintenance: false, message: false,
        }),
        (user(), "task", Command::CreateTask {
            task_id: format!("sandbox-probe-{session}").into(), request_id: format!("sandbox-probe-{session}").into(),
            title: format!("Probe {short}"), agent_id: Some(session.into()), review_prs: false,
            dependencies: BTreeSet::new(), stack: None,
        }),
    ];
    for (actor, step, command) in steps {
        let state = domain.state()?;
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: format!("sandbox-probe-{step}-{session}").into(), expected_revision: state.revision, command,
        };
        domain.execute(&actor, &envelope, None, || Ok(())).map_err(|error| anyhow::anyhow!("{error:?}"))?;
    }
    Ok(())
}

/// The lines for this session's pending outbox messages that this connection has not carried yet.
fn pending_lines(domain: &DomainRuntime, session: &str, sent: &mut BTreeSet<i64>) -> Result<Vec<String>> {
    let pending: Vec<OutboxMessage> = domain.store.lock().expect("domain store poisoned").pending_messages()?.into_iter()
        .filter(|message| message.recipient_kind == "agent" && message.agent_id.as_deref() == Some(session)
            && !sent.contains(&message.id))
        .collect();
    if pending.is_empty() {
        return Ok(vec![]);
    }
    let state = domain.state()?;
    let origins: Vec<_> = {
        let store = domain.store.lock().expect("domain store poisoned");
        pending.iter().map(|message| store.message_origin(message.id)).collect::<Result<_, _>>()?
    };
    Ok(pending.into_iter().zip(origins).map(|(message, origin)| {
        sent.insert(message.id);
        let (at, actor) = origin.unwrap_or_else(|| (alondra_core::now_millis(), String::new()));
        let text = crate::node_push::stamped(&crate::node_push::author_of(&actor), at, &render(&state, &message));
        crate::node_push::message_line(&crate::node_push::outbox_id(message.id), &text, from_user(&state, &message))
    }).collect())
}

/// The mod took these messages: the outbox ones become handed, the direct ones stop being resent.
async fn node_ack(mut stream: UnixStream, domain: &DomainRuntime, ack: Value) -> Result<()> {
    #[derive(Deserialize)]
    struct Ack {
        session_id: String,
        ids: Vec<String>,
    }
    let ack: Ack = match serde_json::from_value(ack) {
        Ok(ack) => ack,
        Err(error) => return refuse(stream, format!("invalid node acknowledgement: {error}")).await,
    };
    hand_over(domain, &ack.session_id, &ack.ids)?;
    domain.live.pushes.acknowledged(&ack.session_id, &ack.ids);
    stream.write_all(&200u16.to_be_bytes()).await?;
    stream.write_all(b"{}").await?;
    stream.shutdown().await?;
    Ok(())
}

fn hand_over(domain: &DomainRuntime, session: &str, ids: &[String]) -> Result<()> {
    let mut store = domain.store.lock().expect("domain store poisoned");
    let pending = store.pending_messages()?;
    for id in ids.iter().filter_map(|id| id.strip_prefix(crate::node_push::OUTBOX_PREFIX)?.parse::<i64>().ok()) {
        if !pending.iter().any(|message| message.id == id && message.agent_id.as_deref() == Some(session)) {
            continue;
        }
        if let Some(claim) = store.claim_message(id)? {
            anyhow::ensure!(store.finish_message(claim.attempt_id, claim.version, State::Handed, None)?, "domain delivery claim changed while finishing");
        }
    }
    Ok(())
}

pub(crate) async fn run(listener: UnixListener, domain: Arc<DomainRuntime>) -> Result<()> {
    crate::domain_holds::release_orphans(&domain)?;
    serve(listener, domain).await
}

enum Reply {
    Plain(Value),
    Held(crate::domain_holds::Held),
    /// A turn end Alondra holds back: the node goes on with `reason`.
    Block { reason: String, session: String },
}

/// The reply to a held hook; a card that can no longer be answered lets Claude Code ask in its terminal.
fn answered(held: &crate::domain_holds::Held, answer: Option<alondra_core::domain::Answer>) -> Value {
    answer.map_or_else(|| json!({}), |answer| crate::domain_holds::elicitation_decision(held, &answer))
}

fn text(payload: &Value, key: &str) -> Option<String> {
    payload.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// The bearer of a session that ended is revoked. Its settings file goes with it unless the Agent
/// still has work: then `claude respawn` finds the file it passes again.
pub(crate) fn session_ended(domain: &DomainRuntime, agent: &AgentId) -> Result<()> {
    record_node_cost(domain, &agent.0);
    domain.revoke_agent(&agent.0)?;
    if crate::node_revival::has_open_task(domain, &agent.0) {
        crate::domain_sessions::suspend_for(domain.home(), agent)
    } else {
        crate::domain_sessions::remove_for(domain.home(), agent)
    }
}

/// Keeps what the session's mod last reported about its cost and context, which the daemon
/// otherwise forgets when it restarts.
pub(crate) fn record_node_cost(domain: &DomainRuntime, session: &str) {
    let Some(reported) = domain.live.mods.session(session) else { return };
    let task_id = domain.state().ok().and_then(|state| {
        let mine = || state.tasks.values().filter(|task| task.agent_id.as_ref().is_some_and(|agent| agent.0 == session));
        mine().find(|task| !task.state.terminal()).or_else(|| mine().next()).map(|task| task.id.0.clone())
    });
    let cost = crate::domain_store::model_metrics::NodeCost {
        session_id: session.to_owned(),
        task_id,
        model: reported.model,
        cost_usd: reported.cost_usd,
        context_peak_percent: reported.context_peak.or_else(|| reported.context.as_ref().and_then(crate::node_reports::ContextReading::full_percent)),
        started_at: reported.started_at,
        ended_at: alondra_core::now_millis(),
    };
    if let Err(error) = domain.record_node_cost(&cost) {
        log!("could not record what session {session} cost: {error:#}");
    }
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
            session_ended(domain, &agent_id)?;
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
    if let Some(held) = crate::domain_holds::hold_elicitation(domain, payload, domain.live.mods.is_node(&session_id))? {
        return Ok(Reply::Held(held));
    }
    match event.as_str() {
        "SessionStart" | "SessionEnd" => Ok(Reply::Plain(json!({}))),
        "Stop" => {
            let held = crate::domain_messages::take_held(domain, &agent_id);
            if !held.is_empty() {
                return Ok(Reply::Plain(json!({"hookSpecificOutput": {"hookEventName": "Stop", "additionalContext": held.join("\n\n")}})));
            }
            Ok(match undelivered(domain, &agent_id, payload)? {
                Some(reason) => Reply::Block { reason, session: session_id },
                None => Reply::Plain(json!({})),
            })
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
            let opens_home_database = words.iter().any(|w| *w == "sqlite3" || w.ends_with("/sqlite3"))
                && words.iter().any(|w| w.contains(".alondra/"))
                && !words.iter().any(|w| *w == "-readonly" || *w == "--readonly" || w.contains("mode=ro"));
            if opens_home_database {
                return Some("Open Alondra's databases with `sqlite3 -readonly`: a plain `sqlite3` creates the file when it is missing, and an empty alondra.db stops the daemon from starting.".into());
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

/// Turn ends Alondra holds back in a row for one Task before it stops trying and tells the user.
pub(crate) const STOP_BLOCKS: u32 = 3;

/// A turn of a node Alondra launched may not end while its Task is open with neither a result
/// for the user nor a question from it: Claude Code goes on with the reason. A turn the block
/// itself continued (`stop_hook_active`) counts too, so the cap, not the flag, ends the loop:
/// after `STOP_BLOCKS` turn ends in a row the user gets a Problem card and the node may stop. A
/// delivery or a question starts the count again; the count is kept in the domain store, so a
/// restart does not reset it. A session the user drives (not a background node, or one whose
/// terminal they attached to), work running in the background and a node stopped by the machine
/// are never held.
fn undelivered(domain: &DomainRuntime, agent_id: &AgentId, payload: &Value) -> Result<Option<String>> {
    let driven_by_user = !domain.live.mods.is_node(&agent_id.0)
        || domain.live.node(&agent_id.0).is_some_and(|node| node.attached);
    if driven_by_user
        || payload.get("background_tasks").and_then(Value::as_array).is_some_and(|tasks| !tasks.is_empty())
        || domain.live.node(&agent_id.0).is_some_and(|node| node.status == alondra_core::NodeStatus::WaitingMachine
            || node.turn_signal == Some(alondra_core::TurnSignal::WaitingMachine))
    {
        return Ok(None);
    }
    let state = domain.state()?;
    let Some(task) = state.tasks.values().find(|task| task.agent_id.as_ref() == Some(agent_id)
        && matches!(task.state, TaskState::NotStarted | TaskState::Running)
        && (task.final_presentation.is_none() || state.task_handed_back(&task.id).is_some()))
    else {
        return Ok(None);
    };
    let scope = Scope::Task { task_id: task.id.clone() };
    let author = Author::Agent { agent_id: agent_id.clone() };
    let mine = || state.interactions.values().filter(|card| card.scope == scope && card.author == author);
    if mine().any(|card| !card.withdrawn && matches!(card.payload,
        InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. }))
    {
        return Ok(None);
    }
    let marker = mine().filter(|card| !matches!(card.payload, InteractionPayload::Information { .. }))
        .map(|card| card.order as i64).max().unwrap_or(0);
    let stored = domain.store.lock().expect("domain store poisoned").stop_blocks(&task.id)?;
    let blocks = match stored {
        Some((blocks, previous)) if previous == marker => blocks,
        Some((blocks, previous)) => {
            if blocks > STOP_BLOCKS {
                let _ = domain.withdraw_problem(&undelivered_problem_id(&task.id, previous).into());
            }
            0
        }
        None => 0,
    };
    let user = crate::domain_messages::user_name();
    if blocks >= STOP_BLOCKS {
        if blocks == STOP_BLOCKS {
            let name = state.agents.get(agent_id).map_or(agent_id.0.as_str(), |agent| agent.name.as_str());
            if let Err(error) = raise_undelivered(domain, &task.id, marker, name, &task.title) {
                log!("could not tell {user} that {name} cannot deliver {}: {error:?}", task.id);
            }
            domain.store.lock().expect("domain store poisoned").set_stop_blocks(&task.id, blocks + 1, marker)?;
        }
        return Ok(None);
    }
    domain.store.lock().expect("domain store poisoned").set_stop_blocks(&task.id, blocks + 1, marker)?;
    Ok(Some(block_text(&user, &task.title)))
}

fn undelivered_problem_id(task: &TaskId, marker: i64) -> String {
    format!("problem-undelivered-{task}-{marker}")
}

fn raise_undelivered(domain: &DomainRuntime, task: &TaskId, marker: i64, name: &str, title: &str) -> Result<(), crate::domain_store::StoreError> {
    let need = alondra_core::domain::clip(&format!("{name} cannot manage to deliver “{title}”"), alondra_core::domain::CardSpec::NEED_MAX);
    let why = alondra_core::domain::clip(&format!(
        "It ended its turn {STOP_BLOCKS} times in a row without a result or a question, and Alondra sent it back each time; now it has stopped."
    ), alondra_core::domain::CardSpec::WHY_MAX);
    let diagnosis = format!(
        "The node working on “{title}” tried to stop {STOP_BLOCKS} times in a row without presenting a result or asking anything. \
         Alondra sent it back to work each time and now lets it stop. Open the node to see where it is stuck, send it a message, or cancel the task."
    );
    domain.raise_problem(&undelivered_problem_id(task, marker), Some(&format!("undelivered:{task}")), Scope::Task { task_id: task.clone() }, need, why, diagnosis, vec![]).map(|_| ())
}

pub(crate) fn block_text(user: &str, title: &str) -> String {
    format!(
        "[Alondra] Do not end your turn yet: your task “{title}” is still open and {user} has not heard from you. Your terminal \
         does not reach {user}; only the Alondra tools do. If the work is done, deliver it with `present_result` and is_final true \
         (the answer in body, files in attachments, the PR you registered in targets). If you need a decision or something only \
         {user} can do, use `ask` or `request_action`. If the machine stopped you (memory, a full disk, a usage limit, a build \
         killed for lack of memory; a build queue is not a stop), say so with `wait_for_machine`; never ask {user} whether to run it again. Otherwise keep working until one of these applies."
    )
}

/// The open PR of the node's branch that no Task registered, which the node should register
/// and deliver. GitHub gets three seconds: the Stop hook waits on this answer.
async fn unregistered_pr(domain: &DomainRuntime, session: &str) -> Option<String> {
    let dir = crate::worktree::worktree_of(session)?;
    let (repository, number) = crate::domain_github::GhCli::new().branch_pr(&dir, Duration::from_secs(3)).await?;
    unregistered_pr_text(&*domain.state().ok()?, &repository, number)
}

pub(crate) fn unregistered_pr_text(state: &DomainState, repository: &str, number: u64) -> Option<String> {
    let known = state.pull_requests.values().any(|pr| pr.repository.eq_ignore_ascii_case(repository) && pr.number == number);
    (!known).then(|| format!(
        " Your branch has an open pull request, {repository}#{number}, that Alondra does not know: register it with `register_pr` \
         and deliver it as a target of `present_result`."
    ))
}

/// The user's answer to a card reaches the session as the user's own words, which Claude Code
/// shows as they are: anything else it wraps in a plugin notice of about 40 tokens.
pub(crate) fn from_user(state: &DomainState, message: &OutboxMessage) -> bool {
    message.interaction_id.as_ref()
        .and_then(|id| state.interactions.get(&InteractionId(id.clone())))
        .is_some_and(|interaction| matches!(interaction.recipient, Recipient::User) && matches!(&interaction.payload,
            InteractionPayload::Question { answer: Some(_), .. } | InteractionPayload::ActionRequest { answer: Some(_), .. }))
}

const QUESTION_LABEL_LIMIT: usize = 80;

/// The asker already holds the whole question; its first words are enough to tell it apart.
fn question_label(question: &str) -> String {
    let line = question.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default();
    match line.char_indices().nth(QUESTION_LABEL_LIMIT) {
        Some((end, _)) => format!("{}…", line[..end].trim_end()),
        None if line.len() < question.trim().len() => format!("{line}…"),
        None => line.to_owned(),
    }
}

/// What the user answered to each of the questions asked together, in the order asked: the node
/// is given them all at once, when the last is in.
fn grouped_answers(state: &DomainState, interaction: &alondra_core::domain::Interaction) -> String {
    let lines: Vec<String> = state.group_members(interaction).into_iter().enumerate().map(|(index, member)| {
        let said = match &member.payload {
            InteractionPayload::Question { options, answer: Some(answer) } => options.iter()
                .find(|option| option.id == answer.option_id).map(|option| option.label.clone())
                .or_else(|| answer.inputs.values().find_map(|value| match value {
                    InputValue::Text(text) if !text.trim().is_empty() => Some(text.trim().to_owned()),
                    _ => None,
                }))
                .unwrap_or_default(),
            _ => String::new(),
        };
        format!("{}. {} → {said}", index + 1, member.card.need)
    }).collect();
    format!("The user answered your {} questions:\n{}", lines.len(), lines.join("\n"))
}

/// Producer-facing delivery contains only the message written for the Agent.
/// Delivery identity, interaction data and authorization remain in Alondra.
pub(crate) fn render(state: &DomainState, message: &OutboxMessage) -> String {
    let interaction = message.interaction_id.as_ref()
        .and_then(|id| state.interactions.get(&InteractionId(id.clone())));
    let Some(interaction) = interaction else { return message.message.clone() };
    let (options, answer) = match &interaction.payload {
        InteractionPayload::Question { options, answer } | InteractionPayload::ActionRequest { options, answer } => (options, answer),
        InteractionPayload::Delivery { .. } => return message.message.clone(),
        InteractionPayload::Information { .. } if matches!(interaction.recipient, Recipient::Orchestrator) => return interaction.body.clone(),
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
    if interaction.card.group.is_some() {
        return grouped_answers(state, interaction);
    }
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
    let question = question_label(&interaction.body);
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

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::*;
    use std::collections::BTreeMap;

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
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: id.into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    /// An Agent with a Task, a question and the User's answer queued for it.
    fn answered_question(domain: &DomainRuntime, agent: &str) {
        domain.live.mods.loaded_in(agent);
        command(domain, Actor::System, &format!("register-{agent}"), Command::RegisterAgent {
            agent_id: agent.into(), name: agent.into(),
        });
        command(domain, Actor::User { user_id: "u".into() }, &format!("request-{agent}"),
            Command::CreateRequest { request_id: format!("request-{agent}").into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        command(domain, Actor::User { user_id: "u".into() }, &format!("task-{agent}"), Command::CreateTask {
            task_id: format!("task-{agent}").into(), request_id: format!("request-{agent}").into(),
            title: "Task".into(), agent_id: Some(agent.into()), review_prs: false,
            dependencies: BTreeSet::new(), stack: None,
        });
        command(domain, Actor::Agent { agent_id: agent.into() }, &format!("question-{agent}"), Command::AskQuestion {
            attachments: vec![], interaction_id: format!("question-{agent}").into(),
            scope: Scope::Task { task_id: format!("task-{agent}").into() }, recipient: Recipient::User,
            body: "Choose".into(), blocking: true,
            options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None, inputs: vec![] }], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
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
            request_id: "request".into(), body: "Build a widget".into(), auto_merge: false, maintenance: false, message: false,
        });
        command(&domain, Actor::User { user_id: "u".into() }, "task", Command::CreateTask {
            task_id: "task".into(), request_id: "request".into(), title: "Widget".into(),
            agent_id: Some("agent-a".into()), review_prs: false, dependencies: BTreeSet::new(), stack: None,
        });
        command(&domain, Actor::Agent { agent_id: "agent-a".into() }, "question", Command::AskQuestion {
            attachments: vec![], interaction_id: "question-1".into(), scope: Scope::Task { task_id: "task".into() },
            recipient: Recipient::Orchestrator, body: "Which API should I use?".into(), blocking: true,
            options: vec![QuestionOption { id: "new_api".into(), label: "New API".into(), description: None, inputs: vec![] }], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
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
            options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None, inputs: vec![] }], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
        });
        let state = domain.state().unwrap();
        let message = domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .find(|message| message.interaction_id.as_deref() == Some("question-agent-a")).unwrap();
        assert_eq!(render(&state, &message), "About \"Choose\": Yes.");

        command(&domain, Actor::Orchestrator { generation: 0 }, "orchestrator-question", Command::AskQuestion {
            attachments: vec![], interaction_id: "orchestrator-question".into(),
            scope: Scope::Orchestrator { request_id: Some("request-agent-a".into()) },
            recipient: Recipient::User, body: "Ship this?".into(), blocking: true,
            options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None, inputs: vec![] }], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
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
            request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
        });
        command(&domain, Actor::User { user_id: "u".into() }, "task", Command::CreateTask {
            task_id: "task".into(), request_id: "request".into(), title: "Task".into(),
            agent_id: Some("agent-a".into()), review_prs: false, dependencies: BTreeSet::new(), stack: None,
        });
        command(&domain, Actor::Agent { agent_id: "agent-a".into() }, "question", Command::AskQuestion {
            attachments: vec![], interaction_id: "question".into(), scope: Scope::Task { task_id: "task".into() },
            recipient: Recipient::User, body: "Which approach?".into(), blocking: true,
            options: vec![QuestionOption { id: "new".into(), label: "New API".into(), description: None,
                inputs: vec![InputSpec { id: "details".into(), kind: InputKind::Text, required: true }] }], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
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
            body: body.into(), blocking: false, options: vec![], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
        });
    }

    fn free_text_answer(domain: &DomainRuntime, actor: Actor, id: &str, text: &str) {
        command(domain, actor, &format!("answer-{id}"), Command::AnswerQuestion {
            attachments: vec![], interaction_id: id.into(), option_id: String::new(),
            inputs: BTreeMap::from([("text".into(), InputValue::Text(text.into()))]),
        });
    }

    fn outbox_message(domain: &DomainRuntime, id: &str, recipient_kind: &str) -> OutboxMessage {
        domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .find(|message| message.interaction_id.as_deref() == Some(id) && message.recipient_kind == recipient_kind)
            .unwrap()
    }

    /// Request 9a5996ed (5 Oct): every answer reached the node inside Claude Code's plugin notice,
    /// about 180 characters around a «Hecho.».
    #[test]
    fn the_users_answer_goes_as_the_users_words_and_the_orchestrators_does_not() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        free_text_question(&domain, "q", agent_actor(), task_scope(), Recipient::User, "Which name?");
        free_text_answer(&domain, user_actor(), "q", "Call it Atlas");
        free_text_question(&domain, "o", agent_actor(), task_scope(), Recipient::Orchestrator, "Which repo?");
        free_text_answer(&domain, Actor::Orchestrator { generation: 0 }, "o", "alondra");
        let state = domain.state().unwrap();
        assert!(from_user(&state, &outbox_message(&domain, "q", "agent")));
        assert!(!from_user(&state, &outbox_message(&domain, "o", "agent")));
    }

    /// The «About …» echo of a 600-character question cost 150 tokens on a 15-token answer.
    #[test]
    fn an_answer_names_its_question_by_its_first_words_not_the_whole_text() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        let question = format!("Tu mensaje sobre el nodo de Parcel llegó cortado. ¿Qué te muestra la app para la tarea?\n\n{}",
            "Lo que ya sé: la ejecución programada falló antes de lanzar Claude. ".repeat(8));
        free_text_question(&domain, "q", Actor::Orchestrator { generation: 0 },
            Scope::Orchestrator { request_id: Some("request".into()) }, Recipient::User, &question);
        free_text_answer(&domain, user_actor(), "q", "Sí, la veo pendiente");
        let rendered = rendered_for(&domain, "q", "orchestrator");
        assert_eq!(rendered,
            "About \"Tu mensaje sobre el nodo de Parcel llegó cortado. ¿Qué te muestra la app para la…\" (request \"Work\"): Sí, la veo pendiente");
        assert!(rendered.len() < question.len() / 4, "{} of {}", rendered.len(), question.len());
    }

    #[test]
    fn a_short_question_is_named_whole_and_a_multiline_one_by_its_first_line() {
        assert_eq!(question_label("  Ship this?  "), "Ship this?");
        assert_eq!(question_label("Ship this?\n\nIt has two commits."), "Ship this?…");
        assert_eq!(question_label(&"á".repeat(81)), format!("{}…", "á".repeat(80)));
    }

    fn rendered_for(domain: &DomainRuntime, id: &str, recipient_kind: &str) -> String {
        let state = domain.state().unwrap();
        render(&state, &outbox_message(domain, id, recipient_kind))
    }

    fn node_with_task(domain: &DomainRuntime) {
        domain.live.mods.loaded_in("agent-a");
        command(domain, Actor::System, "register", Command::RegisterAgent {
            agent_id: "agent-a".into(), name: "Agent A".into(),
        });
        command(domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest {
            request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
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
    async fn the_stop_hook_no_longer_delivers_the_outbox() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        answered_question(&domain, "session-a");
        let (reply, result) = hook(&domain, json!({"session_id": "stranger", "hook_event_name": "Stop"}), Some(6)).await;
        result.unwrap();
        assert_eq!(reply, json!({}), "an unknown session gets nothing and creates nothing");
        assert!(domain.state().unwrap().agents.get(&"stranger".into()).is_none());

        let (reply, result) = hook(&domain, json!({"session_id": "session-a", "hook_event_name": "Stop"}), Some(6)).await;
        result.unwrap();
        assert!(reminder_only(&reply), "only the nudge, never the answer: {reply}");
        assert_eq!(agent_outbox(&domain, "session-a").len(), 1, "the message waits for the node's connection");
        assert!(attempts(&domain).is_empty());
    }

    #[tokio::test]
    async fn a_held_user_message_reaches_the_next_stop_hook_once() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        answered_question(&domain, "session-a");
        domain.held.lock().unwrap().entry("session-a".into()).or_default().push("Use the new API".into());
        let stop = json!({"session_id": "session-a", "hook_event_name": "Stop"});
        let (body, result) = hook(&domain, stop.clone(), Some(6)).await;
        result.unwrap();
        assert_eq!(body["hookSpecificOutput"]["additionalContext"], "Use the new API");
        assert_eq!(agent_outbox(&domain, "session-a").len(), 1, "the outbox is not the Stop hook's");
        domain.held.lock().unwrap().entry("session-a".into()).or_default().push("Only this".into());
        let (body, _) = hook(&domain, stop.clone(), Some(6)).await;
        assert_eq!(body["hookSpecificOutput"]["additionalContext"], "Only this");
        let (body, _) = hook(&domain, stop, Some(6)).await;
        assert!(reminder_only(&body), "a held message is handed once: {body}");
    }

    fn reminder_only(reply: &Value) -> bool {
        reply["decision"] == "block" && reply["reason"].as_str()
            .is_some_and(|text| text.starts_with("[Alondra] Do not end your turn yet") && !text.contains("Yes.") && !text.contains("Only this"))
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
            is_final: true, nothing_to_review: false, card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
        });
        command(&domain, Actor::User { user_id: "u".into() }, "authorize", Command::AuthorizeMerge {
            authorization_id: "auth".into(), presentation_id: "delivery".into(), pr_id: "pr".into(),
            expected_head: head.into(), method: MergeMethod::Squash,
        });
        let domain = Arc::new(domain);
        let mut node = connect(&domain, "session-a").await;
        let answer = node.line().await.unwrap();
        let context = answer["text"].as_str().unwrap();
        let user = crate::domain_messages::user_name();
        assert!(context.starts_with(&format!("[From {user} · ")) && context.ends_with("] Yes."), "the answer says who and when: {context}");
        assert_eq!(agent_outbox(&domain, "session-a").len(), 1, "accepting the merge wakes the node for nothing");
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
    async fn a_turn_that_ends_without_a_result_is_held_back() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        let (reply, result) = hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "Stop"}), Some(6)).await;
        result.unwrap();
        assert_eq!(reply["decision"], "block");
        let reason = reply["reason"].as_str().unwrap();
        assert!(reason.starts_with("[Alondra] Do not end your turn yet: your task “Task” is still open")
            && reason.contains("`present_result`") && reason.contains("`ask`") && reason.contains("`wait_for_machine`"), "{reason}");
        assert!(!reason.contains("task-") && !reason.contains("agent-a"), "no internal ids: {reason}");
    }

    fn problems(domain: &DomainRuntime) -> Vec<alondra_core::domain::Interaction> {
        domain.state().unwrap().interactions.values().cloned().filter(|card| card.card.kind == CardKind::Problem).collect()
    }

    /// Three held-back turn ends in a row, kept across a daemon restart; the fourth tells the
    /// user and lets the node stop, and a question from the node starts the count again.
    #[tokio::test]
    async fn after_three_held_back_turn_ends_the_user_is_told_and_a_question_starts_over() {
        let fixture = Fixture::new();
        let stop = json!({"session_id": "agent-a", "hook_event_name": "Stop"});
        {
            let domain = fixture.open();
            node_with_task(&domain);
            for _ in 0..2 {
                assert!(reminder_only(&hook(&domain, stop.clone(), Some(6)).await.0));
            }
        }
        let domain = fixture.open();
        domain.live.mods.loaded_in("agent-a");
        assert!(reminder_only(&hook(&domain, stop.clone(), Some(6)).await.0), "the count survives a restart");
        assert!(problems(&domain).is_empty());
        assert_eq!(hook(&domain, stop.clone(), Some(6)).await.0, json!({}), "the fourth turn end is let through");
        let raised = problems(&domain);
        assert_eq!(raised.len(), 1);
        assert_eq!(raised[0].card.need, "Agent A cannot manage to deliver “Task”");
        assert_eq!(raised[0].scope, task_scope());
        assert_eq!(hook(&domain, stop.clone(), Some(6)).await.0, json!({}));
        assert_eq!(problems(&domain).len(), 1, "one card, however often it stops");

        free_text_question(&domain, "q", agent_actor(), task_scope(), Recipient::User, "Which name?");
        assert_eq!(hook(&domain, stop.clone(), Some(6)).await.0, json!({}), "a node waiting on its question may stop");
        free_text_answer(&domain, user_actor(), "q", "Atlas");
        assert!(reminder_only(&hook(&domain, stop.clone(), Some(6)).await.0), "asking started the count again");
        assert!(problems(&domain).iter().all(|card| card.withdrawn), "the card went once the node asked");
    }

    /// What Claude Code sends: the Stop after a block carries `stop_hook_active`, and those turn
    /// ends count until the cap lets the node stop.
    #[tokio::test]
    async fn turns_a_block_continued_are_held_until_the_cap_then_the_user_is_told() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        let first = json!({"session_id": "agent-a", "hook_event_name": "Stop"});
        let continued = json!({"session_id": "agent-a", "hook_event_name": "Stop", "stop_hook_active": true});
        assert!(reminder_only(&hook(&domain, first, Some(6)).await.0));
        assert!(reminder_only(&hook(&domain, continued.clone(), Some(6)).await.0));
        assert!(reminder_only(&hook(&domain, continued.clone(), Some(6)).await.0));
        assert!(problems(&domain).is_empty());
        assert_eq!(hook(&domain, continued, Some(6)).await.0, json!({}), "the cap lets it stop");
        assert_eq!(problems(&domain).len(), 1);
    }

    #[tokio::test]
    async fn a_session_the_user_drives_is_never_held() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "PreToolUse", "tool_name": "Bash"}), Some(6)).await.1.unwrap();
        domain.live.apply(&alondra_core::Event::AttachChanged { session_id: "agent-a".into(), attached: true });
        assert!(domain.live.node("agent-a").is_some_and(|node| node.attached));
        assert_eq!(hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "Stop"}), Some(6)).await.0, json!({}),
            "the user attached to its terminal");
        working_agent_without_mod(&domain, "agent-z");
        assert_eq!(hook(&domain, json!({"session_id": "agent-z", "hook_event_name": "Stop"}), Some(6)).await.0, json!({}),
            "not a node Alondra launched");
    }

    #[tokio::test]
    async fn work_running_in_the_background_or_a_wait_for_the_machine_lets_the_turn_end() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        let (reply, _) = hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "Stop",
            "background_tasks": [{"id": "b1", "type": "shell", "command": "make test"}]}), Some(6)).await;
        assert_eq!(reply, json!({}));
        assert_eq!(domain.store.lock().unwrap().stop_blocks(&"task".into()).unwrap(), None, "nothing counted");
    }

    #[tokio::test]
    async fn a_delivery_handed_back_must_be_presented_again_before_the_turn_ends() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        command(&domain, agent_actor(), "present", Command::PresentResult {
            attachments: vec![], task_id: "task".into(), interaction_id: "delivery".into(),
            body: "Done".into(), targets: vec![DeliveryTarget::Text { text: "The answer".into() }], is_final: true,
            nothing_to_review: false,
            card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
        });
        let stop = json!({"session_id": "agent-a", "hook_event_name": "Stop"});
        assert_eq!(hook(&domain, stop.clone(), Some(6)).await.0, json!({}), "in review");
        command(&domain, user_actor(), "changes", Command::SubmitFeedback {
            attachments: vec![], presentation_id: "delivery".into(),
            targets: vec![DeliveryTarget::Text { text: "The answer".into() }], body: "Shorter".into(),
            instruction: Instruction::Changes,
        });
        assert!(reminder_only(&hook(&domain, stop, Some(6)).await.0));
    }

    #[test]
    fn an_open_pr_of_the_branch_that_alondra_does_not_know_is_named() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        command(&domain, agent_actor(), "pr", Command::RegisterPr {
            task_id: "task".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
            number: 7, head: "0123456789abcdef0123456789abcdef01234567".into(), base: "main".into(),
            allowed_methods: vec![MergeMethod::Squash],
        });
        let state = domain.state().unwrap();
        assert_eq!(unregistered_pr_text(&state, "Owner/Repo", 7), None);
        let text = unregistered_pr_text(&state, "owner/repo", 8).unwrap();
        assert!(text.contains("owner/repo#8") && text.contains("register_pr"), "{text}");
    }

    #[tokio::test]
    async fn a_turn_ending_after_a_result_is_not_held() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        command(&domain, agent_actor(), "present", Command::PresentResult {
            attachments: vec![], task_id: "task".into(), interaction_id: "delivery".into(),
            body: "Done".into(), targets: vec![DeliveryTarget::Text { text: "The answer".into() }], is_final: true, nothing_to_review: false, card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
        });
        let (reply, _) = hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "Stop"}), Some(6)).await;
        assert_eq!(reply, json!({}), "a presented result already tells the user");
    }

    /// The connection a node mod holds open, served by the daemon over a socket pair.
    struct Connection {
        reader: tokio::io::BufReader<UnixStream>,
        served: tokio::task::JoinHandle<Result<()>>,
    }

    async fn connect(domain: &Arc<DomainRuntime>, session: &str) -> Connection {
        let (mut client, server) = UnixStream::pair().unwrap();
        let body = serde_json::to_vec(&json!({"node_connect": {"session_id": session, "version": "2.1.287"}})).unwrap();
        client.write_u32(body.len() as u32).await.unwrap();
        client.write_all(&body).await.unwrap();
        let domain = domain.clone();
        let served = tokio::spawn(async move { handle(server, &domain).await });
        assert_eq!(client.read_u16().await.unwrap(), 200);
        Connection { reader: tokio::io::BufReader::new(client), served }
    }

    impl Connection {
        async fn line(&mut self) -> Option<Value> {
            use tokio::io::AsyncBufReadExt;
            let mut line = String::new();
            let read = tokio::time::timeout(Duration::from_secs(5), self.reader.read_line(&mut line)).await.expect("no line arrived");
            (read.unwrap() > 0).then(|| serde_json::from_str(&line).unwrap())
        }

        async fn nothing_arrives(&mut self) -> bool {
            use tokio::io::AsyncBufReadExt;
            let mut line = String::new();
            tokio::time::timeout(Duration::from_millis(150), self.reader.read_line(&mut line)).await.is_err()
        }

        async fn close(self) {
            drop(self.reader);
            self.served.await.unwrap().unwrap();
        }
    }

    async fn acknowledge(domain: &DomainRuntime, session: &str, ids: Value) -> Result<()> {
        let (mut client, server) = UnixStream::pair().unwrap();
        let body = serde_json::to_vec(&json!({"node_ack": {"session_id": session, "ids": ids}})).unwrap();
        client.write_u32(body.len() as u32).await.unwrap();
        client.write_all(&body).await.unwrap();
        let (result, status) = tokio::join!(handle(server, domain), client.read_u16());
        assert_eq!(status.unwrap(), 200);
        result
    }

    #[tokio::test]
    async fn a_connected_node_is_written_its_message_the_moment_it_exists_and_the_ack_hands_it() {
        let fixture = Fixture::new();
        let domain = Arc::new(fixture.open());
        let mut node = connect(&domain, "session-a").await;
        assert!(node.nothing_arrives().await, "nothing is written while nothing is pending, and nothing polls");

        answered_question(&domain, "session-a");
        let line = node.line().await.unwrap();
        let id = agent_outbox(&domain, "session-a")[0].0;
        assert_eq!((line["id"].as_str(), line["as_user"].as_bool()), (Some(format!("o-{id}").as_str()), Some(true)));
        let user = crate::domain_messages::user_name();
        let text = line["text"].as_str().unwrap();
        assert!(text.starts_with(&format!("[From {user} · ")) && text.ends_with("] Yes."), "the user's answer says who and when: {text}");
        assert_eq!(agent_outbox(&domain, "session-a").len(), 1, "written is not handed");

        acknowledge(&domain, "other-session", json!([format!("o-{id}")])).await.unwrap();
        assert_eq!(agent_outbox(&domain, "session-a").len(), 1, "another session cannot hand it over");
        acknowledge(&domain, "session-a", json!([format!("o-{id}"), "o-9999", "d-1"])).await.unwrap();
        assert!(agent_outbox(&domain, "session-a").is_empty());
        assert_eq!(attempts(&domain), vec![(id, "handed".into(), None)]);
        acknowledge(&domain, "session-a", json!([format!("o-{id}")])).await.unwrap();
        assert_eq!(attempts(&domain).len(), 1, "a second ack changes nothing");
        node.close().await;
    }

    #[tokio::test]
    async fn an_unacknowledged_message_is_written_again_on_every_new_connection_until_it_is_acked() {
        let fixture = Fixture::new();
        let domain = Arc::new(fixture.open());
        answered_question(&domain, "session-a");
        let id = agent_outbox(&domain, "session-a")[0].0;

        let mut first = connect(&domain, "session-a").await;
        assert_eq!(first.line().await.unwrap()["id"], format!("o-{id}"), "a message that waited is written on connect");
        assert!(first.nothing_arrives().await, "once per connection");
        first.close().await;

        let mut second = connect(&domain, "session-a").await;
        assert_eq!(second.line().await.unwrap()["id"], format!("o-{id}"), "no ack, so it comes again");
        acknowledge(&domain, "session-a", json!([format!("o-{id}")])).await.unwrap();
        second.close().await;

        let mut third = connect(&domain, "session-a").await;
        assert!(third.nothing_arrives().await, "acked, so never again");
        third.close().await;
    }

    #[tokio::test]
    async fn only_a_sandbox_daemon_registers_the_agent_of_a_connecting_probe() {
        let fixture = Fixture::new();
        let domain = Arc::new(fixture.open());
        connect(&domain, "8a73d968-0000").await.close().await;
        assert!(domain.state().unwrap().agents.is_empty(), "a real daemon never invents an Agent");
        domain.live.sandbox.store(true, std::sync::atomic::Ordering::Relaxed);
        connect(&domain, "8a73d968-0000").await.close().await;
        connect(&domain, "8a73d968-0000").await.close().await;
        let state = domain.state().unwrap();
        assert_eq!(state.agents[&"8a73d968-0000".into()].name, "test/8a73d968");
        assert_eq!(state.agents.len(), 1);
        let tasks: Vec<_> = state.tasks.values().collect();
        assert_eq!(tasks.len(), 1, "one probe Task, so a card has something to belong to");
        assert_eq!(tasks[0].agent_id.as_ref(), Some(&"8a73d968-0000".into()));
        assert!(!tasks[0].state.terminal());
    }

    #[tokio::test]
    async fn a_message_for_another_agent_is_not_written_and_a_newer_connection_replaces_the_older() {
        let fixture = Fixture::new();
        let domain = Arc::new(fixture.open());
        answered_question(&domain, "session-b");
        let mut lost = domain.live.losses.take().unwrap();
        let mut older = connect(&domain, "session-a").await;
        assert!(older.nothing_arrives().await);
        let mut newer = connect(&domain, "session-a").await;
        assert!(older.line().await.is_none(), "the older connection is closed by the daemon");
        domain.live.pushes.interrupt("session-a").unwrap();
        assert_eq!(newer.line().await.unwrap(), json!({"interrupt": true}));
        assert!(lost.try_recv().is_err(), "a connection the daemon replaced is not a lost one");
        newer.close().await;
        assert!(domain.live.mods.session("session-a").unwrap().connection_lost());
        assert_eq!(lost.recv().await.as_deref(), Some("session-a"), "the current connection closing is reported for revival");
    }

    fn working_agent(domain: &DomainRuntime, agent: &str) {
        working_agent_without_mod(domain, agent);
        domain.live.mods.loaded_in(agent);
    }

    fn working_agent_without_mod(domain: &DomainRuntime, agent: &str) {
        let user = || Actor::User { user_id: "u".into() };
        command(domain, Actor::System, &format!("register-{agent}"), Command::RegisterAgent { agent_id: agent.into(), name: agent.into() });
        command(domain, user(), &format!("request-{agent}"),
            Command::CreateRequest { request_id: format!("request-{agent}").into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        command(domain, user(), &format!("task-{agent}"), Command::CreateTask {
            task_id: format!("task-{agent}").into(), request_id: format!("request-{agent}").into(),
            title: "Task".into(), agent_id: Some(agent.into()), review_prs: false, dependencies: BTreeSet::new(), stack: None,
        });
    }

    fn single_question(agent: &str) -> Value {
        let mut card = ask(agent);
        card["input"]["questions"].as_array_mut().unwrap().truncate(1);
        card
    }

    fn ask(agent: &str) -> Value {
        json!({"session_id": agent, "id": "toolu-ask", "kind": "question",
            "input": {"questions": [
                {"question": "Which database?", "options": [{"label": "Postgres", "description": "Shared server"}, {"label": "SQLite"}]},
                {"question": "Also migrate the old rows?"}]}})
    }

    fn permission(agent: &str) -> Value {
        json!({"session_id": agent, "id": "toolu-bash", "kind": "permission", "tool": "Bash",
            "input": {"command": "rm -rf build", "description": "Clean the build"}})
    }

    fn plan(agent: &str) -> Value {
        json!({"session_id": agent, "id": "toolu-plan", "kind": "plan",
            "input": {"plan": "# Ship the parser\n\n1. Split the lexer"}})
    }

    fn elicitation(agent: &str) -> Value {
        json!({"session_id": agent, "hook_event_name": "Elicitation", "mcp_server_name": "github", "message": "Which repository?",
            "requested_schema": {"type": "object", "properties": {"repo": {"type": "string", "title": "Repository"}}}})
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

    async fn send(client: &mut UnixStream, frame: &Value) {
        let body = serde_json::to_vec(frame).unwrap();
        client.write_u32(body.len() as u32).await.unwrap();
        client.write_all(&body).await.unwrap();
    }

    async fn reply(client: &mut UnixStream) -> Value {
        let mut bytes = vec![];
        client.read_to_end(&mut bytes).await.unwrap();
        let _ = client.write_u8(6).await;
        assert_eq!(u16::from_be_bytes([bytes[0], bytes[1]]), 200);
        serde_json::from_slice(&bytes[2..]).unwrap()
    }

    fn answer(domain: &DomainRuntime, id: &InteractionId, option: &str, note: Option<&str>) {
        let inputs = note.map(|note| BTreeMap::from([(Hold::NOTE.to_owned(), InputValue::Text(note.into()))])).unwrap_or_default();
        command(domain, Actor::User { user_id: "u".into() }, &format!("answer-{id}"), Command::AnswerQuestion {
            attachments: vec![], interaction_id: id.clone(), option_id: option.into(), inputs,
        });
    }

    /// A frame as Claude Code's hook or the node mod's helper sends it, answered by the user from a card while it waits.
    async fn held(domain: &DomainRuntime, frame: Value, option: &str, note: Option<&str>) -> (Value, InteractionId) {
        let (mut client, server) = UnixStream::pair().unwrap();
        send(&mut client, &frame).await;
        let handler = handle(server, domain);
        tokio::pin!(handler);
        let id = tokio::select! {
            id = open_hold(domain) => id,
            result = &mut handler => panic!("answered without waiting: {result:?}"),
        };
        answer(domain, &id, option, note);
        let (result, body) = tokio::join!(handler, reply(&mut client));
        result.unwrap();
        (body, id)
    }

    async fn held_card(domain: &DomainRuntime, card: Value, option: &str, note: Option<&str>) -> (Value, InteractionId) {
        held(domain, json!({"node_hold": card}), option, note).await
    }

    #[tokio::test]
    async fn a_permission_prompt_waits_for_allow_or_deny_and_the_reason_travels_with_the_denial() {
        for (option, note, allowed, reason) in [
            (Hold::ALLOW, None, true, "allowed this call"),
            (Hold::DENY, None, false, "denies it"),
            (Hold::DENY, Some("Branch first"), false, "denied it: Branch first"),
        ] {
            let fixture = Fixture::new();
            let domain = fixture.open();
            working_agent(&domain, "agent-a");
            let (reply, id) = held_card(&domain, permission("agent-a"), option, note).await;
            assert_eq!((reply["held"].as_bool(), reply["allowed"].as_bool()), (Some(true), Some(allowed)), "{option}");
            assert!(reply["reason"].as_str().unwrap().ends_with(reason), "{option}: {reply}");
            let state = domain.state().unwrap();
            assert_eq!(state.holds[&id], Hold::Permission);
            assert!(state.interactions[&id].body.contains("Bash: Clean the build"), "{}", state.interactions[&id].body);
            assert!(agent_outbox(&domain, "agent-a").is_empty());
            assert!(crate::domain_messages::take_held(&domain, &"agent-a".into()).is_empty(), "the denial carries the words");
        }
    }

    fn suggested(agent: &str) -> Value {
        let mut card = permission(agent);
        card["suggestions"] = json!([
            {"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "rm -rf build"}], "behavior": "allow", "destination": "userSettings"},
            {"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "rm -rf build"}], "behavior": "allow", "destination": "localSettings"}]);
        card
    }

    #[tokio::test]
    async fn always_allows_the_call_and_hands_back_the_suggested_rule() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let (reply, id) = held_card(&domain, suggested("agent-a"), Hold::ALWAYS, None).await;
        assert_eq!((reply["held"].as_bool(), reply["allowed"].as_bool()), (Some(true), Some(true)), "{reply}");
        assert_eq!(reply["permissions"], json!([{"type": "addRules", "behavior": "allow", "destination": "localSettings",
            "rules": [{"toolName": "Bash", "ruleContent": "rm -rf build"}]}]));
        let InteractionPayload::Question { options, .. } = &domain.state().unwrap().interactions[&id].payload else { panic!("question") };
        assert_eq!(options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), ["allow", "always", "deny"]);
    }

    #[tokio::test]
    async fn plain_allow_and_deny_carry_no_rule_even_when_claude_suggested_one() {
        for option in [Hold::ALLOW, Hold::DENY] {
            let fixture = Fixture::new();
            let domain = fixture.open();
            working_agent(&domain, "agent-a");
            let (reply, _) = held_card(&domain, suggested("agent-a"), option, None).await;
            assert!(reply.get("permissions").is_none(), "{option}: {reply}");
        }
    }

    #[tokio::test]
    async fn without_a_usable_suggestion_there_is_no_always() {
        for suggestions in [json!([]), Value::Null, json!([{"type": "setMode", "mode": "acceptEdits", "destination": "session"}])] {
            let fixture = Fixture::new();
            let domain = fixture.open();
            working_agent(&domain, "agent-a");
            let mut card = permission("agent-a");
            card["suggestions"] = suggestions.clone();
            let (_, id) = held_card(&domain, card, Hold::ALLOW, None).await;
            let InteractionPayload::Question { options, .. } = &domain.state().unwrap().interactions[&id].payload else { panic!("question") };
            assert_eq!(options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), ["allow", "deny"], "{suggestions}");
        }
    }

    #[tokio::test]
    async fn a_cancelled_task_never_asks_for_permission() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        command(&domain, Actor::User { user_id: "u".into() }, "cancel", Command::CancelTask { task_id: "task-agent-a".into() });
        let before = domain.state().unwrap().interactions.len();
        let (mut client, server) = UnixStream::pair().unwrap();
        send(&mut client, &json!({"node_hold": permission("agent-a")})).await;
        let (result, reply) = tokio::join!(handle(server, &domain), reply(&mut client));
        result.unwrap();
        assert_eq!((reply["held"].as_bool(), reply["allowed"].as_bool()), (Some(true), Some(false)), "{reply}");
        assert!(reply["reason"].as_str().unwrap().contains("was cancelled"), "{reply}");
        assert_eq!(domain.state().unwrap().interactions.len(), before, "no card is put to the user");
    }

    #[tokio::test]
    async fn a_cancelled_tasks_plan_is_turned_down_and_never_left_to_other_plan_hooks() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        command(&domain, Actor::User { user_id: "u".into() }, "cancel", Command::CancelTask { task_id: "task-agent-a".into() });
        let before = domain.state().unwrap().interactions.len();
        let (mut client, server) = UnixStream::pair().unwrap();
        send(&mut client, &json!({"node_hold": plan("agent-a")})).await;
        let (result, reply) = tokio::join!(handle(server, &domain), reply(&mut client));
        result.unwrap();
        assert_eq!((reply["held"].as_bool(), reply["approved"].as_bool()), (Some(true), Some(false)), "{reply}");
        assert!(reply["feedback"].as_str().unwrap().contains("was cancelled"), "{reply}");
        assert_eq!(domain.state().unwrap().interactions.len(), before, "no card is put to the user");
    }

    #[tokio::test]
    async fn a_plan_waiting_when_its_task_is_cancelled_is_turned_down() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let (mut client, server) = UnixStream::pair().unwrap();
        send(&mut client, &json!({"node_hold": plan("agent-a")})).await;
        let driver = async {
            let id = open_hold(&domain).await;
            command(&domain, Actor::User { user_id: "u".into() }, "cancel", Command::CancelTask { task_id: "task-agent-a".into() });
            (reply(&mut client).await, id)
        };
        let (result, (reply, id)) = tokio::join!(handle(server, &domain), driver);
        result.unwrap();
        assert!(domain.state().unwrap().interactions[&id].withdrawn);
        assert_eq!((reply["held"].as_bool(), reply["approved"].as_bool()), (Some(true), Some(false)), "{reply}");
        assert!(reply["feedback"].as_str().unwrap().contains("was cancelled"), "{reply}");
    }

    #[test]
    fn a_held_prompt_carries_the_card_it_stands_for() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let post = |frame: Value| match crate::domain_holds::post(&domain, &serde_json::from_value(frame).unwrap(), true).unwrap() {
            crate::domain_holds::Posted::Held(id) => domain.state().unwrap().interactions[&id].card.clone(),
            _ => panic!("not held"),
        };
        let permission = post(permission("agent-a"));
        assert_eq!(permission.kind, CardKind::Authorize);
        assert!(permission.need.contains("Bash: Clean the build"), "{}", permission.need);
        let command = permission.content.unwrap();
        assert_eq!(command.kind, ContentKind::Command);
        assert!(command.body.contains("rm -rf build") && !command.body.contains("Bash: Clean the build"), "{}", command.body);
        let plan = post(plan("agent-a"));
        assert_eq!((plan.kind, plan.need.as_str()), (CardKind::Review, "Ship the parser"));
        assert_eq!(plan.content.unwrap().kind, ContentKind::Plan);
        let mut question = single_question("agent-a");
        question["input"]["questions"][0]["options"][1]["label"] = json!("SQLite (Recommended)");
        let question = post(question);
        assert_eq!((question.kind, question.need.as_str(), question.recommended.as_deref()), (CardKind::Decide, "Which database?", Some("o1")));
        assert!(question.group.is_none(), "one question is an ordinary card");
    }

    #[test]
    fn a_single_question_marks_its_recommendation_instead_of_labelling_it() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let mut single = single_question("agent-a");
        single["input"]["questions"][0]["options"][0]["label"] = json!("Postgres (Recommended)");
        let crate::domain_holds::Posted::Held(id) = crate::domain_holds::post(&domain, &serde_json::from_value(single).unwrap(), true).unwrap()
        else { panic!("not held") };
        let state = domain.state().unwrap();
        let interaction = &state.interactions[&id];
        assert_eq!(interaction.card.recommended.as_deref(), Some("o0"));
        let InteractionPayload::Question { options, .. } = &interaction.payload else { panic!("question") };
        assert_eq!(options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), ["Postgres", "SQLite"]);
    }

    #[tokio::test]
    async fn a_plan_waits_for_approve_or_request_changes() {
        for (option, note, approved, feedback) in [
            (Hold::APPROVE, None, true, None),
            (Hold::REQUEST_CHANGES, None, false, Some("requests changes to the plan")),
            (Hold::REQUEST_CHANGES, Some("Use v2"), false, Some("requested changes to the plan: Use v2")),
        ] {
            let fixture = Fixture::new();
            let domain = fixture.open();
            working_agent(&domain, "agent-a");
            let (reply, id) = held_card(&domain, plan("agent-a"), option, note).await;
            assert_eq!(reply["approved"].as_bool(), Some(approved));
            assert_eq!(reply["feedback"].as_str().map(|text| text.split_once(' ').unwrap().1), feedback);
            assert_eq!(domain.state().unwrap().holds[&id], Hold::Plan);
        }
    }

    #[tokio::test]
    async fn ask_user_question_with_one_question_waits_and_answers_it_with_the_choice_as_claude_wrote_it_or_the_users_words() {
        for (option, note, said) in [("o1", None, "SQLite (Recommended)"), ("", Some("MySQL"), "MySQL")] {
            let fixture = Fixture::new();
            let domain = fixture.open();
            working_agent(&domain, "agent-a");
            let mut single = single_question("agent-a");
            single["input"]["questions"][0]["options"][1]["label"] = json!("SQLite (Recommended)");
            let (reply, id) = held_card(&domain, single, option, note).await;
            assert_eq!(reply, json!({"held": true, "answers": {"Which database?": said}}));
            let state = domain.state().unwrap();
            assert_eq!(state.holds[&id], Hold::Question);
            let InteractionPayload::Question { options, .. } = &state.interactions[&id].payload else { panic!("question") };
            assert_eq!(options.iter().map(|o| (o.id.as_str(), o.label.as_str())).collect::<Vec<_>>(), [("o0", "Postgres"), ("o1", "SQLite")]);
            assert_eq!(state.interactions[&id].card.recommended.as_deref(), Some("o1"));
            assert!(agent_outbox(&domain, "agent-a").is_empty());
        }
    }

    fn three_questions(agent: &str) -> Value {
        json!({"session_id": agent, "id": "toolu-three", "kind": "question",
            "input": {"questions": [
                {"question": "How do the devices reach the Mac?", "header": "Network", "options": [
                    {"label": "LAN and Tailscale (Recommended)", "description": "Works at home and away"},
                    {"label": "LAN only", "description": "Only at home"}]},
                {"question": "Which port should it listen on?", "options": [{"label": "7777"}, {"label": "8080 (Recommended)"}]},
                {"question": "Keep the old pairings?", "options": [{"label": "Keep them"}, {"label": "Pair again"}]}]}})
    }

    const GROUP: [&str; 3] = ["hold-question-toolu-three-q1", "hold-question-toolu-three-q2", "hold-question-toolu-three-q3"];

    async fn post_group(domain: &DomainRuntime) -> (UnixStream, std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + '_>>) {
        let (mut client, server) = UnixStream::pair().unwrap();
        send(&mut client, &json!({"node_hold": three_questions("agent-a")})).await;
        let handler = Box::pin(handle(server, domain));
        (client, handler)
    }

    async fn still_waiting<F: std::future::Future + Unpin>(handler: &mut F) -> bool {
        tokio::time::timeout(Duration::from_millis(300), handler).await.is_err()
    }

    #[tokio::test]
    async fn three_questions_are_three_cards_and_claude_gets_every_answer_only_when_the_last_is_in() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let (mut client, mut handler) = post_group(&domain).await;
        tokio::select! { _ = open_hold(&domain) => {}, result = &mut handler => panic!("answered without waiting: {result:?}") }
        let state = domain.state().unwrap();
        let cards: Vec<_> = GROUP.iter().map(|id| &state.interactions[&(*id).into()].card).collect();
        assert_eq!(cards.iter().map(|card| (card.kind, card.need.as_str())).collect::<Vec<_>>(), [
            (CardKind::Decide, "How do the devices reach the Mac?"), (CardKind::Decide, "Which port should it listen on?"),
            (CardKind::Decide, "Keep the old pairings?")]);
        assert_eq!(cards.iter().map(|card| card.group.as_ref().map(|g| (g.index, g.total))).collect::<Vec<_>>(), [Some((1, 3)), Some((2, 3)), Some((3, 3))]);
        assert_eq!((cards[0].recommended.as_deref(), cards[1].recommended.as_deref(), cards[2].recommended.as_deref()), (Some("o0"), Some("o1"), None));
        assert!(state.holds.keys().filter(|id| GROUP.contains(&id.0.as_str())).count() == 3);

        answer(&domain, &GROUP[0].into(), "o0", None);
        answer(&domain, &GROUP[1].into(), "o0", None);
        assert!(still_waiting(&mut handler).await, "two answers of three send nothing to Claude");
        answer(&domain, &GROUP[2].into(), "o1", None);
        let (result, reply) = tokio::join!(handler, reply(&mut client));
        result.unwrap();
        assert_eq!(reply, json!({"held": true, "answers": {
            "How do the devices reach the Mac?": "LAN and Tailscale (Recommended)",
            "Which port should it listen on?": "7777",
            "Keep the old pairings?": "Pair again",
        }}), "each question gets its own answer");
        assert!(agent_outbox(&domain, "agent-a").is_empty());
    }

    #[tokio::test]
    async fn an_answer_taken_back_is_asked_again_and_the_new_one_is_what_claude_gets() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let (mut client, mut handler) = post_group(&domain).await;
        tokio::select! { _ = open_hold(&domain) => {}, result = &mut handler => panic!("answered without waiting: {result:?}") }
        answer(&domain, &GROUP[0].into(), "o0", None);
        answer(&domain, &GROUP[1].into(), "o0", None);
        command(&domain, Actor::User { user_id: "u".into() }, "retake", Command::ReopenAnswer { interaction_id: GROUP[0].into() });
        let state = domain.state().unwrap();
        assert!(matches!(&state.interactions[&GROUP[0].into()].payload, InteractionPayload::Question { answer: None, .. }));
        answer(&domain, &GROUP[2].into(), "o0", None);
        assert!(still_waiting(&mut handler).await, "a reopened question is still owed");
        command(&domain, Actor::User { user_id: "u".into() }, "answer-again", Command::AnswerQuestion {
            attachments: vec![], interaction_id: GROUP[0].into(), option_id: "o1".into(), inputs: Default::default(),
        });
        let (result, reply) = tokio::join!(handler, reply(&mut client));
        result.unwrap();
        assert_eq!(reply["answers"]["How do the devices reach the Mac?"], "LAN only");
    }

    #[tokio::test]
    async fn a_hook_that_goes_away_withdraws_every_question_of_the_group_and_its_answers() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let (mut client, server) = UnixStream::pair().unwrap();
        send(&mut client, &json!({"node_hold": three_questions("agent-a")})).await;
        let driver = async {
            open_hold(&domain).await;
            answer(&domain, &GROUP[0].into(), "o0", None);
            report(&domain, json!({"session_id": "agent-a", "event": "tool.end", "tool_use_id": "toolu-three"})).await;
            reply(&mut client).await
        };
        let (result, body) = tokio::join!(handle(server, &domain), driver);
        result.unwrap();
        assert_eq!(body, json!({"held": false}));
        let state = domain.state().unwrap();
        assert!(GROUP.iter().all(|id| state.interactions[&(*id).into()].withdrawn), "all three go together, answered or not");
    }

    #[tokio::test]
    async fn a_group_posted_again_is_the_group_already_there() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let post = || match crate::domain_holds::post(&domain, &serde_json::from_value(three_questions("agent-a")).unwrap(), true).unwrap() {
            crate::domain_holds::Posted::Held(id) => id,
            _ => panic!("not held"),
        };
        let first = post();
        assert_eq!(first.0, GROUP[0]);
        assert_eq!(post(), first);
        assert_eq!(domain.state().unwrap().interactions.values().filter(|i| i.card.group.is_some()).count(), 3);
    }

    async fn answered_without_a_card(domain: &DomainRuntime, card: Value) -> Value {
        let (mut client, server) = UnixStream::pair().unwrap();
        send(&mut client, &json!({"node_hold": card})).await;
        let (result, body) = tokio::join!(handle(server, domain), reply(&mut client));
        result.unwrap();
        body
    }

    #[tokio::test]
    async fn only_a_node_whose_mod_loaded_and_has_a_task_is_held() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent_without_mod(&domain, "no-mod");
        command(&domain, Actor::System, "register-idle", Command::RegisterAgent { agent_id: "idle".into(), name: "idle".into() });
        domain.live.mods.loaded_in("idle");
        domain.live.mods.loaded_in("stranger");
        for card in [permission("no-mod"), permission("idle"), permission("stranger")] {
            assert_eq!(answered_without_a_card(&domain, card.clone()).await, json!({"held": false}), "{card}");
        }
        assert!(matches!(respond(&domain, &elicitation("no-mod")).unwrap(), Reply::Plain(body) if body == json!({})));
        assert!(domain.state().unwrap().holds.is_empty());
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
            let (reply, id) = held(&domain, json!({"launched": true, "payload": elicitation("agent-a")}), option, note).await;
            assert_eq!(reply["hookSpecificOutput"], expected);
            let state = domain.state().unwrap();
            assert_eq!(state.holds[&id], Hold::Elicitation);
            assert!(state.interactions[&id].body.starts_with("Which repository?\n\nMCP server “github” requests your input."));
            let InteractionPayload::Question { options, .. } = &state.interactions[&id].payload else { panic!("question") };
            assert_eq!(options[0].description.as_deref(), Some("Write the Repository"));
        }
    }

    #[tokio::test]
    async fn an_elicitation_hook_that_goes_away_withdraws_its_card_and_a_restart_releases_the_rest() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let (mut client, server) = UnixStream::pair().unwrap();
        send(&mut client, &json!({"launched": true, "payload": elicitation("agent-a")})).await;
        let id = {
            let handler = handle(server, &domain);
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
        let Reply::Held(orphan) = respond(&domain, &elicitation("agent-a")).unwrap() else { panic!("held") };
        drop(domain);
        let reopened = fixture.open();
        crate::domain_holds::release_orphans(&reopened).unwrap();
        assert!(reopened.state().unwrap().interactions[&orphan.id].withdrawn);
    }

    #[tokio::test]
    async fn a_card_the_mod_posted_survives_a_helper_that_goes_away_and_a_daemon_restart_and_is_posted_again_once() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let (mut helper, server) = UnixStream::pair().unwrap();
        send(&mut helper, &json!({"node_hold": plan("agent-a")})).await;
        let id = {
            let handler = handle(server, &domain);
            tokio::pin!(handler);
            let id = tokio::select! {
                id = open_hold(&domain) => id,
                result = &mut handler => panic!("answered without waiting: {result:?}"),
            };
            drop(helper);
            handler.await.unwrap();
            id
        };
        assert!(!domain.state().unwrap().interactions[&id].withdrawn, "a cut wait is not a withdrawn question");
        drop(domain);

        let reopened = fixture.open();
        crate::domain_holds::release_orphans(&reopened).unwrap();
        assert!(!reopened.state().unwrap().interactions[&id].withdrawn, "a restart keeps the mod's cards");
        let before = reopened.state().unwrap().interactions.len();

        let (mut again, server) = UnixStream::pair().unwrap();
        send(&mut again, &json!({"node_hold": plan("agent-a")})).await;
        let handler = handle(server, &reopened);
        tokio::pin!(handler);
        tokio::select! {
            _ = open_hold(&reopened) => {},
            result = &mut handler => panic!("answered without waiting: {result:?}"),
        }
        assert_eq!(reopened.state().unwrap().interactions.len(), before, "the same card, not a second one");
        answer(&reopened, &id, Hold::APPROVE, None);
        let (result, body) = tokio::join!(handler, reply(&mut again));
        result.unwrap();
        assert_eq!(body, json!({"held": true, "approved": true}));
    }

    #[tokio::test]
    async fn a_permission_hook_that_goes_away_withdraws_its_card() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let mut card = permission("agent-a");
        card["withdraw_when_gone"] = json!(true);
        let (mut hook, server) = UnixStream::pair().unwrap();
        send(&mut hook, &json!({"node_hold": card})).await;
        let handler = handle(server, &domain);
        tokio::pin!(handler);
        let id = tokio::select! {
            id = open_hold(&domain) => id,
            result = &mut handler => panic!("answered without waiting: {result:?}"),
        };
        drop(hook);
        handler.await.unwrap();
        assert!(domain.state().unwrap().interactions[&id].withdrawn, "the prompt was answered in the terminal or abandoned");
    }

    #[tokio::test]
    async fn a_card_already_answered_is_answered_again_to_a_repost() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let (first, _) = held_card(&domain, single_question("agent-a"), "o0", None).await;
        let again = answered_without_a_card(&domain, single_question("agent-a")).await;
        assert_eq!(again, first);
    }

    async fn report(domain: &DomainRuntime, report: Value) {
        let (mut client, server) = UnixStream::pair().unwrap();
        send(&mut client, &json!({"node_event": report})).await;
        let (result, _) = tokio::join!(handle(server, domain), reply(&mut client));
        result.unwrap();
    }

    async fn withdrawn_by(domain: &DomainRuntime, card: Value, ended: Value) -> (Value, InteractionId) {
        let (mut client, server) = UnixStream::pair().unwrap();
        send(&mut client, &json!({"node_hold": card})).await;
        let driver = async {
            let id = open_hold(domain).await;
            report(domain, ended).await;
            (reply(&mut client).await, id)
        };
        let (result, out) = tokio::join!(handle(server, domain), driver);
        result.unwrap();
        out
    }

    #[tokio::test]
    async fn a_finished_tool_call_an_aborted_turn_and_an_ended_session_stop_their_cards_waiting() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let mut other_plan = plan("agent-a");
        other_plan["id"] = json!("toolu-other-plan");
        let mut third_plan = plan("agent-a");
        third_plan["id"] = json!("toolu-third-plan");
        for (card, ended) in [
            (permission("agent-a"), json!({"session_id": "agent-a", "event": "tool.end", "tool_use_id": "toolu-bash"})),
            (plan("agent-a"), json!({"session_id": "agent-a", "event": "turn.complete", "answer": "", "aborted": true})),
            (other_plan, json!({"session_id": "agent-a", "event": "session.end"})),
        ] {
            let is_plan = card["kind"] == "plan";
            let (body, id) = withdrawn_by(&domain, card, ended).await;
            if is_plan {
                assert_eq!((body["held"].as_bool(), body["approved"].as_bool()), (Some(true), Some(false)), "{body}");
            } else {
                assert_eq!(body, json!({"held": false}));
            }
            assert!(domain.state().unwrap().interactions[&id].withdrawn);
        }

        let (mut helper, server) = UnixStream::pair().unwrap();
        send(&mut helper, &json!({"node_hold": third_plan})).await;
        let driver = async {
            let id = open_hold(&domain).await;
            report(&domain, json!({"session_id": "agent-a", "event": "tool.end", "tool_use_id": "another-call"})).await;
            assert!(!domain.state().unwrap().interactions[&id].withdrawn, "another call's end leaves the card");
            answer(&domain, &id, Hold::APPROVE, None);
            reply(&mut helper).await
        };
        let (result, body) = tokio::join!(handle(server, &domain), driver);
        result.unwrap();
        assert_eq!(body, json!({"held": true, "approved": true}));
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
    fn nodes_open_alondra_databases_only_read_only() {
        let refused = bash("sqlite3 ~/.alondra/alondra.db \".tables\" 2>/dev/null | head").unwrap();
        assert!(refused.contains("sqlite3 -readonly"), "{refused}");
        assert!(bash("cd /tmp; /usr/bin/sqlite3 /Users/x/.alondra/domain.db 'select 1'").is_some());
        assert!(bash("sqlite3 -readonly ~/.alondra/alondra.db .tables").is_none());
        assert!(bash("sqlite3 'file:/Users/x/.alondra/domain.db?mode=ro' .tables").is_none());
        assert!(bash("sqlite3 /tmp/copy.db .tables").is_none());
    }

    #[test]
    fn ordinary_commands_pass() {
        assert!(bash("alondra status").is_none());
        assert!(bash("alondra doctor --json").is_none());
        assert!(bash("git commit -m 'answer the question'").is_none());
        assert!(forbidden_for_nodes(Some("Read"), Some(&json!({"file_path": "/tmp/x.md"}))).is_none());
    }
}
