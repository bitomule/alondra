//! V2-only producer transport. Outbox messages for an Agent reach its Claude session through
//! the connection its node mod holds open (`node_connection`): written down it the moment they
//! exist, handed once the mod acknowledges them. A handoff is technical evidence only.
//!
//! Hooks observe sessions the domain already knows. They never create an
//! Agent, a Task or a decision. The one transition they make is the one a
//! session's own activity proves: its assigned Task has started.
use crate::locks::Poisonable as _;
use crate::domain_agent_lifecycle as lifecycle;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::OutboxMessage;
use crate::node_reports::Report;
use alondra_core::delivery::State;
use alondra_core::node_guard::forbidden_for_nodes;
use alondra_core::domain::{
    Actor, AgentId, Author, Command, CommandEnvelope, DomainState, InputValue, InteractionId, InteractionPayload, NodeEvent,
    PROTOCOL_VERSION, Recipient, Scope, ScheduleLaunchState, SuggestionDecision, Task, TaskId, TaskState,
};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
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
                log!("local hook failed: {error:#}");
            }
        });
    }
}

async fn handle(mut stream: UnixStream, domain: &Arc<DomainRuntime>) -> Result<()> {
    let len = stream.read_u32().await.context("reading the frame")? as u64;
    if len > MAX_HOOK_BYTES {
        bail!("hook input exceeds {MAX_HOOK_BYTES} bytes");
    }
    let mut input = vec![0; len as usize];
    stream.read_exact(&mut input).await.context("reading the frame")?;
    let mut frame: Value = serde_json::from_slice(&input).context("invalid local hook envelope")?;
    if frame.get("probe").is_some() {
        stream.write_all(&crate::hook_socket::probe_ack()).await.context("probe: writing the ack")?;
        return Ok(());
    }
    if let Some(report) = frame.get_mut("node_event").map(Value::take) {
        return node_event(stream, domain, report).await.context("node event");
    }
    if let Some(hello) = frame.get_mut("node_connect").map(Value::take) {
        return node_connection(stream, domain, hello).await.context("node connection");
    }
    if let Some(ack) = frame.get_mut("node_ack").map(Value::take) {
        return node_ack(stream, domain, ack).await.context("node ack");
    }
    if let Some(card) = frame.get_mut("node_hold").map(Value::take) {
        return node_hold(stream, domain, card).await.context("node hold");
    }
    answer_hook(stream, domain, frame).await
}

async fn answer_hook<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, domain: &Arc<DomainRuntime>, frame: Value) -> Result<()> {
    let envelope: Envelope = serde_json::from_value(frame).context("invalid local hook envelope")?;
    let event = text(&envelope.payload, "hook_event_name").unwrap_or_else(|| "unnamed".into());
    let payload = Arc::new(envelope.payload);
    let first = if event == "PreToolUse" {
        let checking = domain.clone();
        let input = payload.clone();
        domain.guard_work.run(move || answer_first(&checking, &input)).await?
    } else {
        answer_first(domain, &payload)
    };
    if let Some(reply) = first {
        let written = write_reply(&mut stream, 200, &serde_json::to_vec(&reply)?).await;
        let recorded = record(domain, payload.clone()).await;
        written.with_context(|| format!("{event} hook: writing its reply"))?;
        recorded.with_context(|| format!("{event} hook: recording it after its reply"))?;
        return Ok(());
    }
    answer_after_recording(stream, domain, &payload).await.with_context(|| format!("{event} hook"))
}

/// The reply as one write. The hook reads it as one JSON value and exits the moment it has it, so it
/// may be gone before the shutdown: a reply delivered, not a failure.
async fn write_reply<S: AsyncWrite + Unpin>(stream: &mut S, status: u16, body: &[u8]) -> std::io::Result<()> {
    let mut frame = status.to_be_bytes().to_vec();
    frame.extend_from_slice(body);
    stream.write_all(&frame).await?;
    match stream.shutdown().await {
        Err(error) if error.kind() == std::io::ErrorKind::NotConnected => Ok(()),
        other => other,
    }
}

/// The reply of a hook that does not depend on what recording it changes. Claude Code gives these
/// hooks five seconds and discards a later reply; a PreToolUse that times out runs its tool through
/// the normal permission flow, with no pause or refusal of Alondra's. Recording waits on the domain
/// store's writer lock, which is busy for seconds when several nodes work at once (13 October: 1,863
/// replies written to hooks Claude Code had already killed), so the reply goes first and the
/// recording after. The Stop hook's hold-back and an elicitation's card are what recording decides,
/// so they wait for it.
fn answer_first(domain: &DomainRuntime, payload: &Value) -> Option<Value> {
    let event = text(payload, "hook_event_name").unwrap_or_default();
    match event.as_str() {
        "Stop" | "Elicitation" => None,
        "PreToolUse" => {
            let Some(session_id) = text(payload, "session_id") else { return Some(json!({})) };
            let tool = text(payload, "tool_name");
            if let Some(reply) = unrecorded_pre_tool_use(domain, &session_id, tool.as_deref()) {
                return Some(reply);
            }
            let known = domain.state().ok()?.agents.contains_key(&AgentId(session_id));
            Some(match forbidden_for_nodes(tool.as_deref(), payload.get("tool_input")).filter(|_| known) {
                Some(reason) => pre_tool_use_denied(&reason),
                None => json!({}),
            })
        }
        _ => Some(json!({})),
    }
}

/// A PreToolUse answered without recording the hook: a node the gate holds, and Alondra's own tools.
fn unrecorded_pre_tool_use(domain: &DomainRuntime, session_id: &str, tool: Option<&str>) -> Option<Value> {
    if let Some(reason) = domain.live.gate.holding(domain, session_id) {
        let why = if reason == crate::pauses::Reason::ByUser { PAUSED_BY_USER } else { PAUSED_FOR_MEMORY };
        return Some(pre_tool_use_denied(why));
    }
    tool.is_some_and(|t| t.starts_with("mcp__plugin_alondra")).then(|| json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
        "permissionDecision": "allow", "permissionDecisionReason": "Alondra tool"}}))
}

fn pre_tool_use_denied(reason: &str) -> Value {
    json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}})
}

async fn record(domain: &Arc<DomainRuntime>, payload: Arc<Value>) -> Result<Reply> {
    let recording = domain.clone();
    domain.hook_work.run(move || respond(&recording, &payload)).await?
}

async fn withdraw(domain: &Arc<DomainRuntime>, id: InteractionId) -> Result<()> {
    let withdrawing = domain.clone();
    domain.hook_work.run(move || crate::domain_holds::withdraw(&withdrawing, &id)).await?
}

async fn answer_after_recording<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, domain: &Arc<DomainRuntime>, payload: &Arc<Value>) -> Result<()> {
    let reply = record(domain, payload.clone()).await.context("recording it")?;
    let body = match reply {
        Reply::Plain(body) => body,
        Reply::Held(held) => {
            let answer = tokio::select! {
                answer = crate::domain_holds::wait(domain, &held.id) => answer,
                gone = stream.read_u8() => {
                    log!("hook holding {} went away: {gone:?}", held.id);
                    return withdraw(domain, held.id).await;
                }
            };
            answered(&held, answer?.and_then(|answers| answers.into_iter().next()))
        }
        Reply::Block { reason, session } => {
            let unregistered = unregistered_pr(domain, &session).await.unwrap_or_default();
            json!({"decision": "block", "reason": format!("{reason}{unregistered}")})
        }
    };
    write_reply(&mut stream, 200, &serde_json::to_vec(&body)?).await.context("writing its reply")?;
    Ok(())
}

async fn refuse(mut stream: UnixStream, reason: String) -> Result<()> {
    stream.write_all(&400u16.to_be_bytes()).await?;
    stream.write_all(reason.as_bytes()).await?;
    stream.shutdown().await?;
    bail!(reason)
}

/// One report of a node mod, as `alondra node event` hands it over.
async fn node_event(mut stream: UnixStream, domain: &Arc<DomainRuntime>, report: Value) -> Result<()> {
    let envelope: crate::node_reports::Envelope = match serde_json::from_value(report) {
        Ok(envelope) => envelope,
        Err(error) => return refuse(stream, format!("invalid node report: {error}")).await,
    };
    let reporting = domain.clone();
    domain.hook_work.run(move || {
        let domain = reporting;
        match &envelope.report {
            Report::SessionEnd | Report::TurnComplete { aborted: true, .. } => {
                crate::domain_holds::release_session(&domain, &envelope.session_id)?
            }
            Report::ToolEnd { tool_use_id: Some(call) } => crate::domain_holds::release_call(&domain, &envelope.session_id, call)?,
            _ => {}
        }
        let ended = matches!(envelope.report, Report::SessionEnd).then(|| envelope.session_id.clone());
        domain.report(envelope, alondra_core::now_millis());
        if let Some(session) = ended {
            domain.live.losses.report(&session);
        }
        Ok::<_, anyhow::Error>(())
    }).await??;
    write_reply(&mut stream, 200, b"{}").await.context("writing its reply")?;
    Ok(())
}

/// A card a node puts to the user, answered on the same connection. A mod helper that goes away
/// leaves the card open: the mod's wait was cut, not the question, and its next post finds the card.
/// A permission hook that goes away takes its card with it.
async fn node_hold(mut stream: UnixStream, domain: &Arc<DomainRuntime>, card: Value) -> Result<()> {
    let card: crate::domain_holds::Card = match serde_json::from_value(card) {
        Ok(card) => card,
        Err(error) => return refuse(stream, format!("invalid hold: {error}")).await,
    };
    let card = Arc::new(card);
    let posting = domain.clone();
    let request = card.clone();
    let posted = domain.hook_work.run(move ||
        crate::domain_holds::post(&posting, &request, posting.live.mods.is_node(&request.session_id))
    ).await??;
    let mut written = None;
    let reply = match posted {
        crate::domain_holds::Posted::NotHeld => json!({"held": false}),
        crate::domain_holds::Posted::Denied(reason) => crate::domain_holds::denied(card.kind, &reason),
        crate::domain_holds::Posted::Held(id) => {
            let answer = tokio::select! {
                answer = crate::domain_holds::wait(domain, &id) => answer?,
                _ = stream.read_u8() => {
                    return if card.withdraw_when_gone { withdraw(domain, id).await } else { Ok(()) };
                }
            };
            let state = domain.state()?;
            match (answer, state.holds.get(&id)) {
                (Some(answer), Some(hold)) => {
                    let interaction = state.interactions.get(&id);
                    let allow = interaction.and_then(|interaction| interaction.card.allow_always.as_ref());
                    let reply = crate::domain_holds::mod_reply(*hold, &card.input, allow, &answer, &crate::domain_messages::user_name());
                    if let Some(rules) = allow.and_then(alondra_core::allow_always::AllowAlways::rules)
                        && answer.first().is_some_and(|answer| answer.option_id == alondra_core::domain::Hold::ALWAYS)
                    {
                        let task = interaction.and_then(|interaction| match &interaction.scope {
                            alondra_core::domain::Scope::Task { task_id } => Some(task_id.0.clone()),
                            _ => None,
                        });
                        written = Some((rules.to_vec(), task, id.0.clone()));
                    }
                    reply
                }
                _ if card.kind == crate::domain_holds::Kind::Plan => {
                    crate::domain_holds::denied(card.kind, &crate::domain_holds::plan_gone(domain, &card.session_id)?)
                }
                _ => json!({"held": false}),
            }
        }
    };
    write_reply(&mut stream, 200, &serde_json::to_vec(&reply)?).await.context("writing its reply")?;
    if let Some((rules, task, interaction)) = written {
        crate::allow_always::chosen(domain, card.cwd.as_deref(), &rules, task.as_deref(), &interaction).await?;
    }
    Ok(())
}

/// The connection a node mod holds open for its session's life. While it is open the node is
/// reachable and the moment it closes, from either side, it is not. Down it go the session's
/// pending outbox messages, each once per connection and again on the next until acknowledged,
/// the direct messages and the interrupts addressed to it.
async fn node_connection(mut stream: UnixStream, domain: &Arc<DomainRuntime>, hello: Value) -> Result<()> {
    let hello: crate::node_reports::Hello = match serde_json::from_value(hello) {
        Ok(hello) => hello,
        Err(error) => return refuse(stream, format!("invalid node connection: {error}")).await,
    };
    let session = hello.session_id.clone();
    let connecting = domain.clone();
    let connecting_session = session.clone();
    let number = domain.hook_work.run(move || {
        let domain = connecting;
        let session = connecting_session;
        if domain.live.sandbox.load(std::sync::atomic::Ordering::Relaxed) {
            register_probe(&domain, &session)?;
        }
        let launch_key = hello.launch_key.clone();
        let number = domain.connect_node(hello, alondra_core::now_millis());
        if let Some(key) = launch_key
            && let Err(error) = crate::domain_sessions::reissue(&domain, &key, &AgentId(session.clone()))
        {
            log!("could not issue {session} a new capability: {error:#}");
        }
        Ok::<_, anyhow::Error>(number)
    }).await??;
    if domain.live.mods.session(&session).and_then(|reported| reported.connection) != Some(number) {
        return Ok(());
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
            let reading = domain.clone();
            let reading_session = session.clone();
            let (pending, next_sent) = domain.hook_work.run(move || {
                let pending = pending_lines(&reading, &reading_session, &mut sent)?;
                Ok::<_, anyhow::Error>((pending, sent))
            }).await??;
            sent = next_sent;
            if domain.live.mods.session(&session).and_then(|reported| reported.connection) != Some(number) {
                return Ok(());
            }
            for line in pending {
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
        (Actor::System, "register", Command::RegisterAgent { agent_id: session.into(), name: format!("test/{short}"), at: 0 }),
        (user(), "request", Command::CreateRequest {
            request_id: format!("sandbox-probe-{session}").into(), body: "Probe".into(), auto_merge: false, maintenance: false, message: false,
        }),
        (Actor::Orchestrator { generation: state.orchestrator_generation }, "task", Command::CreateTask { plan_items: None, continues: None,
            task_id: format!("sandbox-probe-{session}").into(), request_id: format!("sandbox-probe-{session}").into(),
            title: format!("Probe {short}"), agent_id: Some(session.into()), review_prs: false,
            dependencies: BTreeSet::new(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new(format!("test/{short}"), domain.home().to_string_lossy())),
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
/// A session that moves to the SDK with them is carried none: they go with its move.
fn pending_lines(domain: &DomainRuntime, session: &str, sent: &mut BTreeSet<i64>) -> Result<Vec<String>> {
    let pending: Vec<OutboxMessage> = domain.store.lock_or_exit("the domain store").pending_messages()?.into_iter()
        .filter(|message| message.recipient_kind == "agent" && message.agent_id.as_deref() == Some(session)
            && !sent.contains(&message.id))
        .collect();
    if pending.is_empty() || crate::sdk_handover::holds_messages(domain, &AgentId(session.to_owned())) {
        return Ok(vec![]);
    }
    let state = domain.state()?;
    let origins: Vec<_> = {
        let store = domain.store.lock_or_exit("the domain store");
        pending.iter().map(|message| store.message_origin(message.id)).collect::<Result<_, _>>()?
    };
    Ok(pending.into_iter().zip(origins).map(|(message, origin)| {
        sent.insert(message.id);
        let (at, actor) = origin.unwrap_or_else(|| (alondra_core::now_millis(), String::new()));
        let text = crate::node_push::stamped(&crate::node_push::author_of(&actor), at, &render(&state, &message));
        crate::node_push::message_line(&crate::node_push::outbox_id(message.id), &text, from_user(&state, &message))
    }).collect())
}

/// What an outbox message says to its node, stamped with who sent it and when, as its node mod
/// carries it.
pub(crate) fn node_text(domain: &DomainRuntime, state: &DomainState, message: &OutboxMessage) -> Result<String> {
    let origin = domain.store.lock_or_exit("the domain store").message_origin(message.id)?;
    let (at, actor) = origin.unwrap_or_else(|| (alondra_core::now_millis(), String::new()));
    Ok(crate::node_push::stamped(&crate::node_push::author_of(&actor), at, &render(state, message)))
}

/// The mod took these messages: the outbox ones become handed, the direct ones stop being resent.
async fn node_ack(mut stream: UnixStream, domain: &Arc<DomainRuntime>, ack: Value) -> Result<()> {
    #[derive(Deserialize)]
    struct Ack {
        session_id: String,
        ids: Vec<String>,
    }
    let ack: Ack = match serde_json::from_value(ack) {
        Ok(ack) => ack,
        Err(error) => return refuse(stream, format!("invalid node acknowledgement: {error}")).await,
    };
    let acknowledging = domain.clone();
    domain.hook_work.run(move || {
        hand_over(&acknowledging, &ack.session_id, &ack.ids)?;
        acknowledging.live.pushes.acknowledged(&ack.session_id, &ack.ids);
        Ok::<_, anyhow::Error>(())
    }).await??;
    write_reply(&mut stream, 200, b"{}").await.context("writing its reply")?;
    Ok(())
}

fn hand_over(domain: &DomainRuntime, session: &str, ids: &[String]) -> Result<()> {
    let mut store = domain.store.lock_or_exit("the domain store");
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
    domain.revoke_agent(&agent.0)?;
    if crate::node_revival::has_open_task(domain, &agent.0) {
        crate::domain_sessions::suspend_for(domain.home(), agent)
    } else {
        crate::domain_sessions::remove_for(domain.home(), agent)
    }
}

/// Hooks Claude Code fires only while a turn runs: a prompt, a tool call, a wait inside one, a
/// subagent it starts. Every other hook may come between turns: `Notification` (`idle_prompt` a
/// minute after each turn end), `MessageDisplay`, a background subagent's `SubagentStop`,
/// `FileChanged`, a compaction, a model switch. Taking one of those for a turn moved a node waiting
/// for its own work out of that wait, and Claude Code's listing then rested it on a card.
fn proves_a_turn(event: &str) -> bool {
    matches!(event, "UserPromptSubmit" | "UserPromptExpansion" | "PreToolUse" | "PostToolUse" | "PostToolUseFailure"
        | "PostToolBatch" | "PermissionRequest" | "PermissionDenied" | "Elicitation" | "SubagentStart")
}

fn respond(domain: &DomainRuntime, payload: &Value) -> Result<Reply> {
    let Some(session_id) = text(payload, "session_id") else {
        return Ok(Reply::Plain(json!({})));
    };
    let event = text(payload, "hook_event_name").unwrap_or_default();
    let tool = text(payload, "tool_name");
    if event == "PreToolUse" && let Some(reply) = unrecorded_pre_tool_use(domain, &session_id, tool.as_deref()) {
        return Ok(Reply::Plain(reply));
    }
    let agent_id = AgentId(session_id.clone());
    let state = domain.state()?;
    if !state.agents.contains_key(&agent_id) {
        return Ok(Reply::Plain(json!({})));
    }
    domain.live.observe_hook(&state, payload);
    if crate::api_failures::proves_an_answer(&event) {
        domain.live.api_watch.answered(&session_id, alondra_core::now_millis());
        crate::accounts::answered(domain);
    }
    match event.as_str() {
        event if proves_a_turn(event) => {
            lifecycle::node_event(domain, &agent_id, NodeEvent::TurnStarted)?;
        }
        "StopFailure" => {
            crate::api_failures::turn_failed(domain, &agent_id, payload, alondra_core::now_millis())?;
            return Ok(Reply::Plain(json!({})));
        }
        "SessionEnd" => {
            lifecycle::node_event(domain, &agent_id, NodeEvent::SessionLost)?;
            session_ended(domain, &agent_id)?;
        }
        _ => {}
    }
    if !matches!(event.as_str(), "SessionStart" | "SessionEnd") {
        start_assigned(domain, &agent_id)?;
    }
    if event == "PostToolUse"
        && let Err(error) = crate::task_worktrees::record_from_hook(domain, &agent_id, payload)
    {
        log!("could not record the worktree {session_id}'s tool made: {error:#}");
    }
    if event == "PreToolUse"
        && let Some(reason) = forbidden_for_nodes(tool.as_deref(), payload.get("tool_input"))
    {
        log!("blocked {session_id} from acting as the user: {reason}");
        return Ok(Reply::Plain(pre_tool_use_denied(&reason)));
    }
    if let Some(held) = crate::domain_holds::hold_elicitation(domain, payload, domain.live.mods.is_node(&session_id))? {
        return Ok(Reply::Held(held));
    }
    match event.as_str() {
        "SessionStart" | "SessionEnd" => Ok(Reply::Plain(json!({}))),
        "Stop" if domain.live.gate.paused_by_user(domain, &session_id) => Ok(Reply::Plain(json!({}))),
        "Stop" => {
            if let Some(said) = text(payload, "last_assistant_message") {
                lifecycle::record_last_words(domain, &agent_id, &said)?;
            }
            let held = crate::domain_messages::take_held(domain, &agent_id);
            if !held.is_empty() {
                return Ok(Reply::Plain(json!({"hookSpecificOutput": {"hookEventName": "Stop", "additionalContext": held.join("\n\n")}})));
            }
            Ok(match undelivered(domain, &agent_id, payload)? {
                Some(reason) => Reply::Block { reason, session: session_id },
                None => {
                    crate::api_failures::turn_ended(domain, &agent_id)?;
                    let ended = match background_work(domain, &agent_id, payload) {
                        Some(work) => NodeEvent::TurnEndedBehind { work },
                        None => NodeEvent::TurnEnded,
                    };
                    lifecycle::node_event(domain, &agent_id, ended)?;
                    Reply::Plain(json!({}))
                }
            })
        }
        _ => Ok(Reply::Plain(json!({}))),
    }
}

/// A node the user paused may run nothing until the user resumes it or writes to it, whatever
/// started its turn.
pub(crate) const PAUSED_BY_USER: &str = "The user paused you: do nothing and end your turn; only the user can resume you.";
pub(crate) const PAUSED_FOR_MEMORY: &str = "Alondra paused you because the Mac is out of memory: do nothing and end your turn; Alondra tells you when to carry on.";

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

/// Turn ends Alondra holds back in a row for one Task before it lets the turn end, and the node
/// machine puts the node's last words in a card for the user.
pub(crate) const STOP_BLOCKS: u32 = 3;

/// A turn of a node Alondra launched may not end while its Task is open with neither a result
/// for the user nor a question from it: Claude Code goes on with the reason. A turn the block
/// itself continued (`stop_hook_active`) counts too, so the cap, not the flag, ends the loop:
/// after `STOP_BLOCKS` turn ends in a row the node may stop, and its last words reach the user. A
/// delivery or a question starts the count again; the count is kept in the domain store, so a
/// restart does not reset it. A session the user drives (not a background node, or one whose
/// terminal they attached to), work running in the background and a node the machine paused are
/// never held.
/// The turn ended behind background work of its own that will wake it: the node waits for it.
fn works_in_background(domain: &DomainRuntime, agent_id: &AgentId, payload: &Value) -> bool {
    background_work(domain, agent_id, payload).is_some()
}

/// The oldest work the turn left running in the background (`classic.Stop`'s `background_tasks`),
/// with when it started as the node's mod saw it. However long it has run, Claude Code says it is
/// in flight, and its end wakes the node.
fn background_work(domain: &DomainRuntime, agent_id: &AgentId, payload: &Value) -> Option<alondra_core::domain::Background> {
    use alondra_core::BackgroundKind;
    let tasks = payload.get("background_tasks").and_then(Value::as_array).filter(|tasks| !tasks.is_empty())?;
    let now = alondra_core::now_millis();
    let reported = domain.live.mods.session(&agent_id.0);
    let field = |task: &Value, key: &str| task.get(key).and_then(Value::as_str).filter(|text| !text.trim().is_empty()).map(str::to_owned);
    let started = |task: &Value| field(task, "id")
        .and_then(|id| reported.as_ref()?.background.iter().find(|(known, _)| *known == id).map(|(_, item)| item.started_at))
        .unwrap_or(now);
    let oldest = tasks.iter().min_by_key(|task| started(task))?;
    let work = match oldest.get("type").and_then(Value::as_str) {
        Some("shell") => BackgroundKind::Shell,
        Some("agent" | "subagent") => BackgroundKind::Agent,
        _ => BackgroundKind::Other,
    };
    let label = field(oldest, "command").or_else(|| field(oldest, "description")).or_else(|| field(oldest, "name"))
        .map_or_else(|| "background work".to_owned(), |label| crate::node_reports::short_label(&label));
    Some(alondra_core::domain::Background { work, label, since: started(oldest), others: tasks.len() as u32 - 1, gone: None })
}

/// Nodes waiting for work of their own that Alondra now sees gone, with no turn to take its result
/// yet: Claude Code's whole list of it is empty, or holds only commands whose shells exited. The
/// node machine marks the wait, and only a later listing that finds the node without work rests
/// it, as Claude Code wakes it with its work's notification. Work that never ends keeps it waiting,
/// named, however long it takes.
pub(crate) fn background_over(domain: &DomainRuntime) -> Vec<AgentId> {
    let Ok(state) = domain.state() else { return vec![] };
    state.agents.values()
        .filter(|agent| matches!(agent.phase, alondra_core::domain::NodePhase::WaitingBackground(alondra_core::domain::Background { gone: None, .. })))
        .filter(|agent| domain.live.mods.session(&agent.id.0).is_some_and(|reported| reported.work_gone()))
        .map(|agent| agent.id.clone())
        .collect()
}

fn undelivered(domain: &DomainRuntime, agent_id: &AgentId, payload: &Value) -> Result<Option<String>> {
    if driven_by_user(domain, agent_id) || works_in_background(domain, agent_id, payload) || paused(&*domain.state()?, agent_id) {
        return Ok(None);
    }
    hold_back(domain, agent_id)
}

fn driven_by_user(domain: &DomainRuntime, agent_id: &AgentId) -> bool {
    !domain.live.mods.is_node(&agent_id.0) || domain.live.node(&agent_id.0).is_some_and(|node| node.attached)
}

/// The node machine paused the node (the Mac, its own `wait_for_machine`, Alondra's pacing of
/// usage, the user, an API error's retry): the one stop that is not Alondra's to undo, and it ends
/// with the resume whoever writes to the node then. What the node says of itself, such as a
/// budget it thinks is spent, is no pause.
fn paused(state: &DomainState, agent_id: &AgentId) -> bool {
    state.node_pause(agent_id).is_some()
}

/// The node machine has the node waiting for work of its own that Alondra has not seen end.
fn waits_on_own_work(state: &DomainState, agent_id: &AgentId) -> bool {
    matches!(state.node_phase(agent_id),
        Some(alondra_core::domain::NodePhase::WaitingBackground(alondra_core::domain::Background { gone: None, .. })))
}

/// What a node last showed Alondra: its latest report or connection, and whether its work was
/// seen gone. A reminder answers one of these; only a new one sends another.
type Fact = (Option<alondra_core::Millis>, bool);

/// Nodes that ended their turn with nothing running that will wake them (no background work, or
/// only commands whose shells are gone), their Task open and neither a result nor a question:
/// no turn end will come to hold them back (their work ended without waking them, or the Stop hook
/// never reached Alondra), so the reminder the Stop hook gives goes to them as a message from
/// Alondra, counted the same way; past the cap it is left alone. A node that has not begun a turn
/// has nothing to be reminded of. `reminded` keeps the fact each node was last sent one for: the
/// clock never sends another, only a new fact (a turn end, its work's notification, its shell seen
/// gone).
pub(crate) fn behind_hung_work(domain: &DomainRuntime,
    reminded: &mut std::collections::HashMap<String, Fact>) -> Vec<(String, String)> {
    let Ok(state) = domain.state() else { return vec![] };
    let mut out = vec![];
    for (agent_id, agent) in &state.agents {
        let session = agent_id.0.as_str();
        let Some(fact) = domain.live.mods.session(session).map(|reported| (reported.last_event_at, reported.work_gone())) else { continue };
        let counted = |task: &TaskId| domain.store.lock_or_exit("the domain store").stop_blocks(task).ok().flatten();
        if matches!(agent.phase, alondra_core::domain::NodePhase::Starting { .. } | alondra_core::domain::NodePhase::ThroughSdk) || reminded.get(session) == Some(&fact)
            || stopped_without_delivering(domain, &state, agent_id, counted).is_none()
        {
            continue;
        }
        match hold_back(domain, agent_id) {
            Ok(Some(text)) => {
                reminded.insert(session.to_owned(), fact);
                out.push((session.to_owned(), text));
            }
            Ok(None) => {}
            Err(error) => log!("could not check whether {session} stopped without delivering: {error:#}"),
        }
    }
    out
}

/// The node of an open Task ended its turn with nothing running that will wake it, neither a
/// result nor a question: whether Alondra still sends it back (`Some(true)`) or its cap was
/// reached (`Some(false)`). A node the user drives, one the node machine
/// paused, one waiting on a card, one whose session stopped mid-turn (restarted by
/// `node_revival`) and one that is not connected are not stopped this way. One Claude Code shows
/// blocked waits on the user in a terminal nobody reads, whatever turn its mod last reported: it
/// is sent back too, so it asks through a card.
/// `stop_blocks` reads the count kept for a Task, so a caller holding the store can answer.
pub(crate) fn stopped_without_delivering(domain: &DomainRuntime, state: &DomainState, agent_id: &AgentId,
    stop_blocks: impl Fn(&TaskId) -> Option<(u32, i64)>) -> Option<bool> {
    let session = agent_id.0.as_str();
    let idle = domain.live.mods.session(session).is_some_and(|reported| reported.connection.is_some()
        && reported.tool.is_none() && (reported.background.is_empty() || reported.work_gone())
        && (reported.job.as_ref().is_some_and(|job| job.phase == crate::job_state::Phase::Blocked)
            || reported.working_since.is_none() && reported.halt().is_none()));
    if !idle || driven_by_user(domain, agent_id) || paused(state, agent_id) || waits_on_own_work(state, agent_id)
        || domain.live.is_closed(session) || domain.live.gate.paused(domain, session).is_some()
        || crate::domain_holds::waits_on_user(state, session)
    {
        return None;
    }
    let (task, marker) = undelivered_task(state, agent_id)?;
    Some(!stop_blocks(&task.id).is_some_and(|(blocks, previous)| previous == marker && blocks >= STOP_BLOCKS))
}

/// The agent's open Task that has neither a result for the user nor an open question from it,
/// with the order of its latest card, which starts the count of held-back turn ends again.
fn undelivered_task<'a>(state: &'a DomainState, agent_id: &AgentId) -> Option<(&'a Task, i64)> {
    let task = state.tasks.values().find(|task| task.agent_id.as_ref() == Some(agent_id)
        && matches!(task.state, TaskState::NotStarted | TaskState::Running)
        && (task.final_presentation.is_none() || state.task_handed_back(&task.id).is_some()))?;
    let scope = Scope::Task { task_id: task.id.clone() };
    let author = Author::Agent { agent_id: agent_id.clone() };
    let mine = || state.interactions.values().filter(|card| card.scope == scope && card.author == author);
    if mine().any(|card| !card.withdrawn && matches!(card.payload,
        InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. }))
    {
        return None;
    }
    let marker = mine().filter(|card| !matches!(card.payload, InteractionPayload::Information { .. }))
        .map(|card| card.order as i64).max().unwrap_or(0);
    Some((task, marker))
}

/// Counts one more turn end of the node's open Task without a result or a question, and gives the
/// reason to send it back, or nothing once the cap let it stop and told the user.
fn hold_back(domain: &DomainRuntime, agent_id: &AgentId) -> Result<Option<String>> {
    let state = domain.state()?;
    let Some((task, marker)) = undelivered_task(&state, agent_id) else { return Ok(None) };
    let stored = domain.store.lock_or_exit("the domain store").stop_blocks(&task.id)?;
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
    if blocks >= STOP_BLOCKS {
        return Ok(None);
    }
    domain.store.lock_or_exit("the domain store").set_stop_blocks(&task.id, blocks + 1, marker)?;
    Ok(Some(block_text(&crate::domain_messages::user_name(), &task.title)))
}

fn undelivered_problem_id(task: &TaskId, marker: i64) -> String {
    format!("problem-undelivered-{task}-{marker}")
}

pub(crate) fn block_text(user: &str, title: &str) -> String {
    format!(
        "[Alondra] Do not end your turn yet: your task “{title}” is still open and {user} has not heard from you. Your terminal \
         does not reach {user}; only the Alondra tools do. If the work is done, deliver it with `present_result` and is_final true \
         (the answer in body, files in attachments, the PR you registered in targets). If you need a decision or something only \
         {user} can do, use `ask` or `request_action`. If the Mac stopped you (memory, a full disk, a build killed for lack \
         of memory; a build queue is not a stop), say so with `wait_for_machine`; the subscription's usage is never a stop: keep working; never ask {user} whether to run it again. Otherwise keep working until one of these applies."
    )
}

/// The open PR of the node's branch that no Task registered, which the node should register
/// and deliver. GitHub gets three seconds: the Stop hook waits on this answer.
async fn unregistered_pr(domain: &DomainRuntime, session: &str) -> Option<String> {
    let dir = crate::worktree::worktree_of(session)?;
    let (host, repository, number) = crate::domain_github::GhCli::new().branch_pr(&dir, Duration::from_secs(3)).await?;
    unregistered_pr_text(&*domain.state().ok()?, &host, &repository, number)
}

pub(crate) fn unregistered_pr_text(state: &DomainState, host: &str, repository: &str, number: u64) -> Option<String> {
    let known = state.pull_requests.values()
        .any(|pr| pr.host == host && pr.repository.eq_ignore_ascii_case(repository) && pr.number == number);
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

/// What was answered to each of the cards asked together, in the order asked: the node is given
/// them all at once, when the last is in, each naming its card.
fn grouped_answers(state: &DomainState, interaction: &alondra_core::domain::Interaction) -> String {
    let lines: Vec<String> = state.group_members(interaction).into_iter().enumerate().map(|(index, member)| {
        let said = alondra_core::domain::answer_said(member).unwrap_or_default();
        format!("{}. {} (card {}) → {said}", index + 1, member.card.need, member.id)
    }).collect();
    let who = if matches!(interaction.recipient, Recipient::Orchestrator) { "The orchestrator" } else { "The user" };
    format!("{who} answered your {} cards:\n{}", lines.len(), lines.join("\n"))
}

/// What the node that suggested something reads of the orchestrator's decision.
fn settled_text(decision: &SuggestionDecision) -> String {
    let note = |note: Option<&str>| note.map(|note| format!(" {}", note.trim())).unwrap_or_default();
    match decision {
        SuggestionDecision::NewTask { task_id, title, note: n, .. } =>
            format!("The orchestrator settled your suggestion: it is a new task, «{title}» ({task_id}), already created.{}", note(n.as_deref())),
        SuggestionDecision::CurrentTask { note: n } =>
            format!("The orchestrator settled your suggestion: it is part of your plan, do it in this task.{}", note(Some(n))),
        SuggestionDecision::Discard { reason } =>
            format!("The orchestrator settled your suggestion: discarded.{}", note(Some(reason))),
    }
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
        if interaction.card.kind == alondra_core::domain::CardKind::Propose {
            text.push_str(&format!("\nSettle it with settle_suggestion(\"{}\", decision: new_task, current_task or discard).", interaction.id));
        } else if matches!(interaction.recipient, Recipient::Agent { .. } | Recipient::Orchestrator) && !alondra_core::domain::is_stall_case(interaction) {
            text.push_str(&format!("\nAnswer with answer_question(\"{}\", option: \"<option_id>\").", interaction.id));
        }
        return text;
    };
    if let Some(decision) = &answer.decision {
        return settled_text(decision);
    }
    if interaction.card.group.is_some() {
        return grouped_answers(state, interaction);
    }
    let said = alondra_core::domain::answer_said(interaction).unwrap_or_default();
    match &interaction.author {
        Author::Orchestrator { .. } => {
            let question = question_label(&interaction.body);
            let request_title = match &interaction.scope {
                Scope::Orchestrator { request_id: Some(request_id) } => state.requests.get(request_id)
                    .map(|request| request.title.as_deref().unwrap_or(&request.body)),
                Scope::Task { task_id } => state.tasks.get(task_id)
                    .and_then(|task| state.requests.get(&task.request_id))
                    .map(|request| request.title.as_deref().unwrap_or(&request.body)),
                _ => None,
            };
            match request_title {
                Some(title) => format!("About \"{question}\" (card {}, request \"{}\"): {said}", interaction.id.0, title.trim()),
                None => format!("About \"{question}\" (card {}): {said}", interaction.id.0),
            }
        }
        Author::Agent { .. } if matches!(interaction.recipient, Recipient::Orchestrator) =>
            format!("The orchestrator answered your card {} ({}): {said}", interaction.id, interaction.card.need),
        Author::Agent { .. } => format!("Answer to your card {} ({}): {said}", interaction.id, interaction.card.need),
        _ => said,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::*;
    use std::collections::BTreeMap;

    struct Fixture(crate::test_support::Directory);
    impl Fixture {
        fn new() -> Self {
            let home = crate::test_support::Directory::new();
            std::fs::create_dir(&home).unwrap();
            Self(home)
        }
        fn open(&self) -> Arc<DomainRuntime> {
            Arc::new(DomainRuntime::open(&self.0).unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn command(domain: &Arc<DomainRuntime>, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: id.into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    /// An Agent with a Task, a question and the User's answer queued for it.
    fn answered_question(domain: &Arc<DomainRuntime>, agent: &str) {
        domain.live.mods.loaded_in(agent);
        command(domain, Actor::System, &format!("register-{agent}"), Command::RegisterAgent {
            agent_id: agent.into(), name: agent.into(), at: 0
        });
        command(domain, Actor::User { user_id: "u".into() }, &format!("request-{agent}"),
            Command::CreateRequest { request_id: format!("request-{agent}").into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        command(domain, Actor::Orchestrator { generation: 0 }, &format!("task-{agent}"), Command::CreateTask { plan_items: None, continues: None,
            task_id: format!("task-{agent}").into(), request_id: format!("request-{agent}").into(),
            title: "Task".into(), agent_id: Some(agent.into()), review_prs: false,
            dependencies: BTreeSet::new(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
        command(domain, Actor::Agent { agent_id: agent.into() }, &format!("question-{agent}"), Command::AskQuestion {
            attachments: vec![], interaction_id: format!("question-{agent}").into(),
            scope: Scope::Task { task_id: format!("task-{agent}").into() }, recipient: Recipient::User,
            body: "Choose".into(), blocking: true,
            options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None }], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
        });
        command(domain, Actor::User { user_id: "u".into() }, &format!("answer-{agent}"), Command::AnswerQuestion {
            attachments: vec![], interaction_id: format!("question-{agent}").into(),
            option_id: "yes".into(), inputs: Default::default(),
        });
    }

    fn agent_outbox(domain: &Arc<DomainRuntime>, agent: &str) -> Vec<(i64, String)> {
        domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .filter(|m| m.agent_id.as_deref() == Some(agent))
            .map(|m| (m.id, m.state)).collect()
    }

    /// 13 October: the user answered 1d6d53c5's card while Claude Code listed it blocked; the answer is
    /// what unblocks it. Whatever its listing, the answer goes down the node's connection the
    /// moment one is open, and the node no longer counts as working while it waited.
    #[tokio::test]
    async fn the_answer_to_a_node_listed_blocked_is_handed_to_it() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        answered_question(&domain, "agent-a");
        lifecycle::node_event(&domain, &"agent-a".into(), NodeEvent::TurnStarted).unwrap();
        assert_eq!(domain.state().unwrap().node_phase(&"agent-a".into()), Some(&NodePhase::Working));
        lifecycle::reconcile(&domain, [("agent-a".into(), Listed::Blocked)].into()).unwrap();
        assert_ne!(domain.state().unwrap().node_phase(&"agent-a".into()), Some(&NodePhase::Working));
        let mut sent = BTreeSet::new();
        let lines = pending_lines(&domain, "agent-a", &mut sent).unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("Yes."), "{}", lines[0]);
        let ids: Vec<String> = sent.iter().map(|id| crate::node_push::outbox_id(*id)).collect();
        hand_over(&domain, "agent-a", &ids).unwrap();
        assert!(agent_outbox(&domain, "agent-a").is_empty(), "handed");
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
        assert_eq!(rendered, "Answer to your card question-agent-a (Answer the question): Yes.");
        assert!(!rendered.contains("data_epoch"));
        assert!(!rendered.contains("protocol"));
    }

    #[test]
    fn an_orchestrator_can_answer_a_node_question_from_the_delivered_text() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        command(&domain, Actor::System, "register", Command::RegisterAgent {
            agent_id: "agent-a".into(), name: "Agent A".into(), at: 0
        });
        command(&domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest {
            request_id: "request".into(), body: "Build a widget".into(), auto_merge: false, maintenance: false, message: false,
        });
        command(&domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None, continues: None,
            task_id: "task".into(), request_id: "request".into(), title: "Widget".into(),
            agent_id: Some("agent-a".into()), review_prs: false, dependencies: BTreeSet::new(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
        command(&domain, Actor::Agent { agent_id: "agent-a".into() }, "question", Command::AskQuestion {
            attachments: vec![], interaction_id: "question-1".into(), scope: Scope::Task { task_id: "task".into() },
            recipient: Recipient::Orchestrator, body: "Which API should I use?".into(), blocking: true,
            options: vec![QuestionOption { id: "new_api".into(), label: "New API".into(), description: None }], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
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
    fn answers_always_name_their_card_and_the_orchestrators_request() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        answered_question(&domain, "agent-a");
        command(&domain, Actor::Agent { agent_id: "agent-a".into() }, "another-question", Command::AskQuestion {
            attachments: vec![], interaction_id: "another-question".into(),
            scope: Scope::Task { task_id: "task-agent-a".into() }, recipient: Recipient::User,
            body: "Second choice?".into(), blocking: true,
            options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None }], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
        });
        let state = domain.state().unwrap();
        let message = domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .find(|message| message.interaction_id.as_deref() == Some("question-agent-a")).unwrap();
        assert_eq!(render(&state, &message), "Answer to your card question-agent-a (Answer the question): Yes.");

        command(&domain, Actor::Orchestrator { generation: 0 }, "orchestrator-question", Command::AskQuestion {
            attachments: vec![], interaction_id: "orchestrator-question".into(),
            scope: Scope::Orchestrator { request_id: Some("request-agent-a".into()) },
            recipient: Recipient::User, body: "Ship this?".into(), blocking: true,
            options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None }], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
        });
        command(&domain, Actor::User { user_id: "u".into() }, "orchestrator-answer", Command::AnswerQuestion {
            attachments: vec![], interaction_id: "orchestrator-question".into(),
            option_id: "yes".into(), inputs: Default::default(),
        });
        let state = domain.state().unwrap();
        let message = domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .find(|message| message.interaction_id.as_deref() == Some("orchestrator-question")
                && message.recipient_kind == "orchestrator").unwrap();
        assert_eq!(render(&state, &message), "About \"Ship this?\" (card orchestrator-question, request \"Work\"): Yes.");
    }

    #[test]
    fn a_users_option_and_text_reach_the_node_as_plain_language() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        command(&domain, Actor::System, "register", Command::RegisterAgent {
            agent_id: "agent-a".into(), name: "Agent A".into(), at: 0
        });
        command(&domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest {
            request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
        });
        command(&domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None, continues: None,
            task_id: "task".into(), request_id: "request".into(), title: "Task".into(),
            agent_id: Some("agent-a".into()), review_prs: false, dependencies: BTreeSet::new(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
        command(&domain, Actor::Agent { agent_id: "agent-a".into() }, "question", Command::AskQuestion {
            attachments: vec![], interaction_id: "question".into(), scope: Scope::Task { task_id: "task".into() },
            recipient: Recipient::User, body: "Which approach?".into(), blocking: true,
            options: vec![QuestionOption { id: "new".into(), label: "New API".into(), description: None }], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
        });
        command(&domain, Actor::User { user_id: "u".into() }, "answer", Command::AnswerQuestion {
            attachments: vec![], interaction_id: "question".into(), option_id: "new".into(),
            inputs: BTreeMap::from([("text".into(), InputValue::Text("Keep the old response format".into()))]),
        });
        let state = domain.state().unwrap();
        let message = domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .find(|message| message.interaction_id.as_deref() == Some("question")
                && message.recipient_kind == "agent").unwrap();
        assert_eq!(render(&state, &message), "Answer to your card question (Answer the question): New API. Keep the old response format");
    }

    fn free_text_question(domain: &Arc<DomainRuntime>, id: &str, actor: Actor, scope: Scope, recipient: Recipient, body: &str) {
        command(domain, actor, &format!("ask-{id}"), Command::AskQuestion {
            attachments: vec![], interaction_id: id.into(), scope, recipient,
            body: body.into(), blocking: false, options: vec![], card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
        });
    }

    fn free_text_answer(domain: &Arc<DomainRuntime>, actor: Actor, id: &str, text: &str) {
        command(domain, actor, &format!("answer-{id}"), Command::AnswerQuestion {
            attachments: vec![], interaction_id: id.into(), option_id: String::new(),
            inputs: BTreeMap::from([("text".into(), InputValue::Text(text.into()))]),
        });
    }

    fn outbox_message(domain: &Arc<DomainRuntime>, id: &str, recipient_kind: &str) -> OutboxMessage {
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
            "About \"Tu mensaje sobre el nodo de Parcel llegó cortado. ¿Qué te muestra la app para la…\" (card q, request \"Work\"): Sí, la veo pendiente");
        assert!(rendered.len() < question.len() / 4, "{} of {}", rendered.len(), question.len());
    }

    #[test]
    fn a_short_question_is_named_whole_and_a_multiline_one_by_its_first_line() {
        assert_eq!(question_label("  Ship this?  "), "Ship this?");
        assert_eq!(question_label("Ship this?\n\nIt has two commits."), "Ship this?…");
        assert_eq!(question_label(&"á".repeat(81)), format!("{}…", "á".repeat(80)));
    }

    fn rendered_for(domain: &Arc<DomainRuntime>, id: &str, recipient_kind: &str) -> String {
        let state = domain.state().unwrap();
        render(&state, &outbox_message(domain, id, recipient_kind))
    }

    fn node_with_task(domain: &Arc<DomainRuntime>) {
        domain.live.mods.loaded_in("agent-a");
        command(domain, Actor::System, "register", Command::RegisterAgent {
            agent_id: "agent-a".into(), name: "Agent A".into(), at: 0
        });
        command(domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest {
            request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
        });
        command(domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None, continues: None,
            task_id: "task".into(), request_id: "request".into(), title: "Task".into(),
            agent_id: Some("agent-a".into()), review_prs: false, dependencies: BTreeSet::new(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
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
    fn a_free_text_answer_reaches_the_node_as_the_text_and_its_card() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        free_text_question(&domain, "q", agent_actor(), task_scope(), Recipient::User, "Which name?");
        free_text_answer(&domain, user_actor(), "q", "Call it Atlas");
        assert_eq!(rendered_for(&domain, "q", "agent"), "Answer to your card q (Answer the question): Call it Atlas");
    }

    #[test]
    fn a_free_text_answer_names_its_card_whether_or_not_another_is_open() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        free_text_question(&domain, "q", agent_actor(), task_scope(), Recipient::User, "Which name?");
        free_text_question(&domain, "other", agent_actor(), task_scope(), Recipient::User, "Which color?");
        free_text_answer(&domain, user_actor(), "q", "Call it Atlas");
        assert_eq!(rendered_for(&domain, "q", "agent"), "Answer to your card q (Answer the question): Call it Atlas");
    }

    /// 3 November: a node asked its card after presenting its result; the user answered in words
    /// only, and the node read the bare text with nothing saying it answered its card.
    #[test]
    fn a_text_answer_names_the_card_it_answers_like_an_option() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        command(&domain, agent_actor(), "ask-options", Command::AskQuestion {
            attachments: vec![], interaction_id: "options".into(), scope: task_scope(), recipient: Recipient::User,
            body: "Which name?".into(), blocking: false,
            options: vec![QuestionOption { id: "atlas".into(), label: "Atlas".into(), description: None }],
            card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Pick a name"),
        });
        command(&domain, user_actor(), "answer-options", Command::AnswerQuestion {
            attachments: vec![], interaction_id: "options".into(), option_id: "atlas".into(), inputs: Default::default(),
        });
        free_text_question(&domain, "text", agent_actor(), task_scope(), Recipient::User, "Which color?");
        free_text_answer(&domain, user_actor(), "text", "Blue");
        assert_eq!(rendered_for(&domain, "options", "agent"), "Answer to your card options (Pick a name): Atlas.");
        assert_eq!(rendered_for(&domain, "text", "agent"), "Answer to your card text (Answer the question): Blue");
    }

    #[test]
    fn an_answer_the_node_cannot_take_yet_waits_instead_of_being_dropped() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        free_text_question(&domain, "q", agent_actor(), task_scope(), Recipient::User, "Which name?");
        free_text_answer(&domain, user_actor(), "q", "Call it Atlas");
        assert_eq!(crate::domain_delivery::tell_sdk_nodes(&domain), 0, "the task has no execution to take it yet");
        assert_eq!(outbox_message(&domain, "q", "agent").state, "retryable", "the answer waits for the node");
    }

    #[test]
    fn a_free_text_answer_to_the_orchestrator_names_the_question_its_card_and_request() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        free_text_question(&domain, "q", Actor::Orchestrator { generation: 0 },
            Scope::Orchestrator { request_id: Some("request".into()) }, Recipient::User, "Which name?");
        free_text_answer(&domain, user_actor(), "q", "Call it Atlas");
        assert_eq!(rendered_for(&domain, "q", "orchestrator"),
            "About \"Which name?\" (card q, request \"Work\"): Call it Atlas");
    }

    #[test]
    fn an_orchestrators_free_text_answer_to_a_node_says_who_answered_and_never_the_user() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        free_text_question(&domain, "q", agent_actor(), task_scope(), Recipient::Orchestrator, "Which name?");
        free_text_answer(&domain, Actor::Orchestrator { generation: 0 }, "q", "Call it Atlas");
        let rendered = rendered_for(&domain, "q", "agent");
        assert_eq!(rendered, "The orchestrator answered your card q (Answer the question): Call it Atlas");
        assert!(!rendered.contains("user"));
    }

    fn suggestion(domain: &Arc<DomainRuntime>, id: &str) {
        command(domain, agent_actor(), &format!("suggest-{id}"), Command::AskQuestion {
            attachments: vec![], interaction_id: id.into(), scope: task_scope(), recipient: Recipient::Orchestrator,
            body: "README.md:12".into(), blocking: false, options: alondra_core::domain::suggestion_options(),
            card: alondra_core::domain::CardSpec::new(alondra_core::domain::CardKind::Propose, "The README names the old flag")
                .why("This task only touches the CLI"),
        });
    }

    #[test]
    fn the_orchestrator_is_told_to_settle_a_suggestion_and_the_node_reads_the_decision() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        suggestion(&domain, "p");
        assert!(rendered_for(&domain, "p", "orchestrator").contains("Settle it with settle_suggestion(\"p\""));
        command(&domain, Actor::Orchestrator { generation: 0 }, "settle-p", Command::SettleSuggestion {
            interaction_id: "p".into(),
            decision: SuggestionDecision::NewTask {
                task_id: "t-readme".into(), request_id: "r-readme".into(), title: "Update the README flag".into(),
                launch: alondra_core::domain::LaunchPlan::new("readme", "/tmp"), dependencies: Default::default(), note: None,
            },
        });
        assert_eq!(rendered_for(&domain, "p", "agent"),
            "The orchestrator settled your suggestion: it is a new task, «Update the README flag» (t-readme), already created.");
        drop(domain);
        let reopened = fixture.open();
        let state = reopened.state().unwrap();
        let InteractionPayload::Question { answer: Some(answer), .. } = &state.interactions[&"p".into()].payload else { panic!("unanswered") };
        assert!(matches!(&answer.decision, Some(SuggestionDecision::NewTask { task_id, .. }) if task_id.0 == "t-readme"),
            "the decision survives the store: {answer:?}");
        assert!(state.tasks.contains_key(&"t-readme".into()));
    }

    #[test]
    fn a_discarded_suggestion_reaches_the_node_with_its_reason() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        suggestion(&domain, "p");
        command(&domain, Actor::Orchestrator { generation: 0 }, "settle-p", Command::SettleSuggestion {
            interaction_id: "p".into(), decision: SuggestionDecision::Discard { reason: "Task 12 already covers it".into() },
        });
        assert_eq!(rendered_for(&domain, "p", "agent"), "The orchestrator settled your suggestion: discarded. Task 12 already covers it");
    }

    fn attempts(domain: &Arc<DomainRuntime>) -> Vec<(i64, String, Option<String>)> {
        domain.store.lock().unwrap().delivery_evidence_page(0).unwrap().into_iter()
            .map(|item| (item.message_id, item.state, item.reason)).collect()
    }

    #[test]
    fn runtime_progress_view_and_feed_sql_and_render_run_outside_the_async_worker() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "isolation");
        let client = crate::client_routes::Client::open(domain.clone(), &fixture.0, Default::default());
        domain.reads.lock().unwrap().begin_live_read_trace();
        let driver = std::thread::current().id();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let response = crate::views::routes::relay(&client, "phone", "/views/home?protocol=1&client=ios&lang=en").await;
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
            let view: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(view["key"], "home");
            let response = crate::views::routes::relay_feed(&client, "phone", "/changes?protocol=1&client=ios&lang=en&wait=0").await;
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
            let feed: Value = serde_json::from_slice(&bytes).unwrap();
            assert!(feed["cursor"].as_str().unwrap().starts_with("c1."));
            assert!(feed["status"].is_object());
            let runtime = client.read("/runtime").await.expect("runtime route");
            assert_eq!(runtime.status(), axum::http::StatusCode::OK);
        });
        let reads = domain.reads.lock().unwrap().end_live_read_trace();
        assert!(!reads.is_empty());
        assert!(reads.iter().any(|read| read.rows > 0), "the real populated store was read");
        assert!(reads.iter().all(|read| read.thread != format!("{driver:?}")), "SQLite ran on the async worker: {reads:?}");
        let builds = client.views.build_threads.lock().unwrap();
        assert!(!builds.is_empty(), "a cold projection was actually rendered");
        assert!(builds.iter().all(|thread| *thread != driver), "render ran on the async worker: {builds:?}");
    }

    struct ReleaseRead(Option<std::sync::mpsc::Sender<()>>);
    impl Drop for ReleaseRead {
        fn drop(&mut self) {
            if let Some(release) = self.0.take() { let _ = release.send(()); }
        }
    }

    #[test]
    fn runtime_progress_a_held_view_and_hook_recording_do_not_stop_guards_or_machine_samples() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "progress");
        let client = crate::client_routes::Client::open(domain.clone(), &fixture.0, Default::default());
        let driver = std::thread::current().id();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, held) = std::sync::mpsc::channel();
        let entered = std::sync::Mutex::new(Some(entered));
        domain.reads.lock().unwrap().on_live_read(move |_| {
            // A wrong-thread regression fails before the controlled block, rather than hanging.
            if let Some(entered) = entered.lock().unwrap().take() {
                assert_ne!(std::thread::current().id(), driver, "view SQL occupies the async worker");
                entered.send(()).unwrap();
                held.recv().unwrap();
            }
        });
        let (recording_entered, recording_started) = tokio::sync::oneshot::channel();
        let recording_entered = std::sync::Mutex::new(Some(recording_entered));
        domain.hook_work.on_start(move || {
            if let Some(entered) = recording_entered.lock().unwrap().take() {
                assert_ne!(std::thread::current().id(), driver, "hook recording occupies the async worker");
                entered.send(()).unwrap();
            }
        });
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let release = ReleaseRead(Some(release));
            let reading = tokio::spawn({
                let client = client.clone();
                async move { crate::views::routes::relay(&client, "phone", "/views/home?protocol=1&client=ios&lang=en").await }
            });
            started.await.expect("the real view must enter its controlled SQL read");
            let (mut poster, server) = UnixStream::pair().unwrap();
            let recording = tokio::spawn({
                let domain = domain.clone();
                async move { handle(server, &domain).await }
            });
            send_hook(&mut poster, json!({"session_id":"progress", "hook_event_name":"PostToolUse"})).await;
            assert_eq!(reply(&mut poster).await, json!({}));
            recording_started.await.expect("hook recording started while SQL was held");
            let (mut guard, server) = UnixStream::pair().unwrap();
            let guarding = tokio::spawn({
                let domain = domain.clone();
                async move { handle(server, &domain).await }
            });
            send_hook(&mut guard, json!({"session_id":"progress", "hook_event_name":"PreToolUse", "tool_name":"Bash", "tool_input":{"command":"alondra answer q --option yes"}})).await;
            let denied = reply(&mut guard).await;
            assert_eq!(denied["hookSpecificOutput"]["permissionDecision"], "deny");
            let gauges = std::sync::Arc::new(std::sync::Mutex::new(Some((alondra_core::Machine {
                at: 1_700_000_000_000, memory_total: 100, memory_available: 50, memory_free_percent: Some(50),
                disk_total: 500_000_000_000, disk_available: 400_000_000_000, cpus: 8,
                ..Default::default()
            }, None, vec![]))));
            let watcher = tokio::spawn(crate::machine_watch::run(domain.clone(), gauges, Silent));
            domain.live.machine_watch.sampled.notified().await;
            let threads = domain.live.machine_watch.sample_threads.lock().unwrap().clone();
            assert!(!threads.is_empty());
            assert!(threads.iter().all(|thread| *thread != driver));
            let samples = fixture.0.join(crate::machine_state::SAMPLES_DIR);
            assert!(std::fs::read_dir(samples).unwrap().next().is_some(), "machine sample was persisted while view held SQL");
            watcher.abort();
            assert!(watcher.await.unwrap_err().is_cancelled());
            drop(release);
            assert_eq!(reading.await.unwrap().status(), axum::http::StatusCode::OK);
            recording.await.unwrap().unwrap();
            guarding.await.unwrap().unwrap();
            println!("view_sql_held=true post_reply=ok pre_tool_use=deny machine_sample=persisted physical_jobs=drained");
        });
    }

    #[test]
    fn runtime_progress_hook_recording_and_guards_run_off_the_async_worker() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "hook-isolation");
        let driver = std::thread::current().id();
        let recorded = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let checked = recorded.clone();
        domain.hook_work.on_start(move || {
            assert_ne!(std::thread::current().id(), driver, "hook recording occupies the async worker");
            checked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        let guarded = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let checked = guarded.clone();
        domain.guard_work.on_start(move || {
            assert_ne!(std::thread::current().id(), driver, "PreToolUse guard occupies the async worker");
            checked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            for event in ["PostToolUse", "PreToolUse"] {
                let (body, result) = hook(&domain, json!({"session_id":"hook-isolation", "hook_event_name":event,
                    "tool_name":"Bash", "tool_input":{"command":"alondra answer q --option yes"}}), None).await;
                result.unwrap();
                if event == "PreToolUse" {
                    assert_eq!(body["hookSpecificOutput"]["permissionDecision"], "deny");
                } else {
                    assert_eq!(body, json!({}));
                }
            }
        });
        assert_eq!(recorded.load(std::sync::atomic::Ordering::Relaxed), 2);
        assert_eq!(guarded.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn runtime_progress_machine_watch_persists_off_the_async_worker() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        let driver = std::thread::current().id();
        let gauges = std::sync::Arc::new(std::sync::Mutex::new(Some((alondra_core::Machine {
            at: 1_700_000_000_000, memory_total: 100, memory_available: 50, memory_free_percent: Some(50),
            disk_total: 500_000_000_000, disk_available: 400_000_000_000, cpus: 8, ..Default::default()
        }, None, vec![]))));
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let watcher = tokio::spawn(crate::machine_watch::run(domain.clone(), gauges, Silent));
            domain.live.machine_watch.sampled.notified().await;
            let threads = domain.live.machine_watch.sample_threads.lock().unwrap().clone();
            watcher.abort();
            assert!(watcher.await.unwrap_err().is_cancelled());
            assert!(!threads.is_empty());
            assert!(threads.iter().all(|thread| *thread != driver), "machine file sampling occupied the async worker: {threads:?}");
            assert!(std::fs::read_dir(fixture.0.join(crate::machine_state::SAMPLES_DIR)).unwrap().next().is_some());
        });
    }

    struct Silent;
    impl crate::usage_pause::Alerts for Silent {
        async fn tell(&self, _: impl Fn(bool) -> String + Send + Sync, _: bool) {}
    }

    async fn hook(domain: &Arc<DomainRuntime>, payload: Value, ack: Option<u8>) -> (Value, Result<()>) {
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

    async fn send_hook(client: &mut UnixStream, payload: Value) {
        let body = serde_json::to_vec(&json!({"launched": true, "payload": payload})).unwrap();
        client.write_u32(body.len() as u32).await.unwrap();
        client.write_all(&body).await.unwrap();
    }

    /// 13 October: 1,863 replies went to hooks Claude Code had already killed at their five
    /// seconds, because recording a hook waits on the store's writer lock and the reply waited for
    /// the recording. A hook whose reply does not depend on that recording gets it while the lock
    /// is held, and is still recorded once the lock is free.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_hook_is_answered_while_the_store_is_busy_and_recorded_after() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        command(&domain, Actor::System, "register", Command::RegisterAgent { agent_id: "session-a".into(), name: "A".into(), at: 0 });
        for (event, tool) in [("PostToolBatch", None), ("PreToolUse", Some("Bash"))] {
            lifecycle::node_event(&domain, &"session-a".into(), NodeEvent::TurnEnded).unwrap();
            assert_ne!(domain.state().unwrap().node_phase(&"session-a".into()), Some(&NodePhase::Working));
            let (taken, release) = std::sync::mpsc::channel();
            let busy = {
                let domain = domain.clone();
                std::thread::spawn(move || {
                    let _writer = domain.store.lock().unwrap();
                    taken.send(()).unwrap();
                    std::thread::sleep(Duration::from_secs(2));
                })
            };
            release.recv().unwrap();
            let (mut client, server) = UnixStream::pair().unwrap();
            let handling = tokio::spawn({
                let domain = domain.clone();
                async move { handle(server, &domain).await }
            });
            send_hook(&mut client, json!({"session_id": "session-a", "hook_event_name": event, "tool_name": tool,
                "tool_input": {"command": "ls"}})).await;
            let mut reply = vec![];
            let read = tokio::time::timeout(Duration::from_secs(1), client.read_to_end(&mut reply)).await;
            assert!(read.is_ok(), "{event} waited for the store before its reply");
            assert_eq!(&reply[..], b"\x00\xc8{}");
            handling.await.unwrap().unwrap();
            busy.join().unwrap();
            assert_eq!(domain.state().unwrap().node_phase(&"session-a".into()), Some(&NodePhase::Working), "{event} recorded");
        }
    }

    /// A hook Claude Code killed before its reply is still recorded, and the failure names the hook
    /// and the step, so the log says which hooks miss their replies.
    #[tokio::test]
    async fn a_hook_gone_before_its_reply_is_recorded_and_named() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        command(&domain, Actor::System, "register", Command::RegisterAgent { agent_id: "session-a".into(), name: "A".into(), at: 0 });
        let (client, server) = tokio::io::duplex(64);
        drop(client);
        let frame = json!({"launched": true, "payload": {"session_id": "session-a", "hook_event_name": "PostToolUse"}});
        let error = answer_hook(server, &domain, frame).await.unwrap_err();
        assert!(format!("{error:#}").starts_with("PostToolUse hook: writing its reply: "), "{error:#}");
        assert_eq!(domain.state().unwrap().node_phase(&"session-a".into()), Some(&NodePhase::Working));
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
            checklist: Vec::new(),
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
        assert!(context.starts_with(&format!("[From {user} · ")) && context.ends_with("] Answer to your card question-session-a (Answer the question): Yes."), "the answer says who and when: {context}");
        assert_eq!(agent_outbox(&domain, "session-a").len(), 1, "accepting the merge wakes the node for nothing");
        assert!(!context.contains("authorization_id") && !context.contains("pr_id")
            && !context.contains("head") && !context.contains("auth-"), "{context}");
    }

    #[tokio::test]
    async fn session_hooks_track_connection_and_end_revokes_the_bearer() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        command(&domain, Actor::System, "register", Command::RegisterAgent {
            agent_id: "session-a".into(), name: "A".into(), at: 0
        });
        let bearer = domain.bind_agent("session-a").unwrap().expose_for_child_environment().to_owned();
        let (_, result) = hook(&domain, json!({"session_id": "session-a", "hook_event_name": "SessionStart"}), Some(6)).await;
        result.unwrap();
        assert!(domain.state().unwrap().agents[&"session-a".into()].phase.holds_slot());
        assert!(domain.resolve_bearer(&bearer).is_ok());
        let (_, result) = hook(&domain, json!({"session_id": "session-a", "hook_event_name": "SessionEnd"}), Some(6)).await;
        result.unwrap();
        assert!(!domain.state().unwrap().agents[&"session-a".into()].phase.holds_slot());
        assert!(domain.resolve_bearer(&bearer).is_err(), "a finished session's bearer is revoked");
        let (reply, _) = hook(&domain, json!({"session_id": "session-a", "hook_event_name": "PreToolUse",
            "tool_name": "mcp__plugin_alondra_alondra__publish_information"}), Some(6)).await;
        assert_eq!(reply["hookSpecificOutput"]["permissionDecision"], "allow");
    }

    #[tokio::test]
    async fn the_worktree_a_nodes_isolated_subagent_kept_becomes_its_tasks() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        command(&domain, Actor::System, "register", Command::RegisterAgent { agent_id: "session-a".into(), name: "A".into(), at: 0 });
        command(&domain, Actor::User { user_id: "u".into() }, "request",
            Command::CreateRequest { request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        command(&domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None, continues: None,
            task_id: "task".into(), request_id: "request".into(), title: "Task".into(), agent_id: Some("session-a".into()), review_prs: false,
            dependencies: BTreeSet::new(), stack: None, launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
        let path = "/repo/.claude/worktrees/agent-a9e1782746c156d58";
        hook(&domain, json!({"session_id": "session-a", "hook_event_name": "PostToolUse", "tool_name": "Agent",
            "tool_input": {"isolation": "worktree", "prompt": "Fix it"},
            "tool_response": {"status": "completed", "agentId": "a9e1782746c156d58", "worktreePath": path, "worktreeBranch": "worktree-agent-a9e1782746c156d58"}}), Some(6)).await.1.unwrap();
        let worktrees = domain.state().unwrap().tasks[&"task".into()].worktrees.clone();
        assert_eq!(worktrees.keys().collect::<Vec<_>>(), [path]);
        assert_eq!(worktrees[path].origin, alondra_core::domain::WorktreeOrigin::Subagent);
    }

    #[tokio::test]
    async fn a_session_whose_start_hook_beat_its_registration_connects_on_its_next_hook() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        hook(&domain, json!({"session_id": "session-a", "hook_event_name": "SessionStart"}), Some(6)).await.1.unwrap();
        command(&domain, Actor::System, "register", Command::RegisterAgent {
            agent_id: "session-a".into(), name: "A".into(), at: 0
        });
        let phase = || domain.state().unwrap().agents[&"session-a".into()].phase.clone();
        assert_eq!(phase(), alondra_core::domain::NodePhase::Starting { reviving: false, idle_since: None });
        hook(&domain, json!({"session_id": "session-a", "hook_event_name": "UserPromptSubmit"}), Some(6)).await.1.unwrap();
        assert_eq!(phase(), alondra_core::domain::NodePhase::Working);
        let revision = domain.state().unwrap().revision;
        hook(&domain, json!({"session_id": "session-a", "hook_event_name": "PostToolUse", "tool_name": "Bash"}), Some(6)).await.1.unwrap();
        assert_eq!(domain.state().unwrap().revision, revision, "a connected Agent's hooks record nothing more");
    }

    #[tokio::test]
    async fn hooks_of_a_known_session_are_observed_live_and_a_stranger_is_not() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        command(&domain, Actor::System, "register", Command::RegisterAgent { agent_id: "session-a".into(), name: "A".into(), at: 0 });
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
        command(&domain, Actor::System, "register", Command::RegisterAgent { agent_id: "session-a".into(), name: "A".into(), at: 0 });
        let pre = |session: &str, tool: &str, input: &Value| json!({"session_id": session, "hook_event_name": "PreToolUse",
            "tool_name": tool, "tool_input": input});
        for (tool, input) in [
            ("Bash", json!({"command": "alondra answer 55 Merge"})),
            ("Bash", json!({"command": "cd /tmp && ~/.local/bin/alondra orchestrator retry run-1 --confirm"})),
            ("Bash", json!({"command": "alondra --no-input device approve req-1"})),
            ("Bash", json!({"command": "alondra device invite"})),
            ("Read", json!({"file_path": "/Users/x/.alondra/tui-key.pem"})),
        ] {
            let (reply, result) = hook(&domain, pre("session-a", tool, &input), None).await;
            result.unwrap();
            assert_eq!(reply["hookSpecificOutput"]["permissionDecision"], "deny", "{input}");
            assert_eq!(hook(&domain, pre("stranger", tool, &input), None).await.0, json!({}), "{input}");
        }
        for allowed in ["alondra status", "alondra --json mac list", "alondra doctor --json"] {
            assert_eq!(hook(&domain, pre("session-a", "Bash", &json!({"command": allowed})), None).await.0, json!({}), "{allowed}");
        }
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

    fn task_state(domain: &Arc<DomainRuntime>) -> TaskState {
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
    async fn a_turn_an_api_error_ended_waits_for_alondra_to_send_it_back_and_never_counts_as_work() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "UserPromptSubmit", "prompt": "Work"}), Some(6)).await.1.unwrap();
        assert_eq!(domain.state().unwrap().node_phase(&"agent-a".into()), Some(&NodePhase::Working));
        let (reply, result) = hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "StopFailure", "error_type": "server_error",
            "error_details": "API Error: Connection lost mid-response. The response above may be incomplete."}), Some(6)).await;
        result.unwrap();
        assert_eq!(reply, json!({}));
        let state = domain.state().unwrap();
        let phase = state.node_phase(&"agent-a".into()).unwrap();
        assert!(matches!(phase, NodePhase::Paused(Pause { reason: PauseReason::ApiError(ApiRetry { attempt: 1, .. }), .. })), "{phase:?}");
        assert!(state.slots().is_empty(), "it holds no slot while it waits");
    }

    #[tokio::test]
    async fn another_sessions_activity_never_starts_the_task() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        command(&domain, Actor::System, "register-b", Command::RegisterAgent { agent_id: "agent-b".into(), name: "B".into(), at: 0 });
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

    fn problems(domain: &Arc<DomainRuntime>) -> Vec<alondra_core::domain::Interaction> {
        domain.state().unwrap().interactions.values().cloned().filter(|card| card.card.kind == CardKind::Problem).collect()
    }

    /// The open cards that hold the node's last words for the user.
    fn unheard(domain: &Arc<DomainRuntime>) -> Vec<alondra_core::domain::Interaction> {
        let state = domain.state().unwrap();
        state.interactions.values().filter(|card| card.id.0.starts_with(alondra_core::domain::UNHEARD_PREFIX) && state.open(card))
            .cloned().collect()
    }

    /// Three held-back turn ends in a row, kept across a daemon restart; the fourth is let through
    /// and the node's last words reach the user in a card, never a Problem; a question from the
    /// node starts the count again.
    #[tokio::test]
    async fn after_three_held_back_turn_ends_the_node_s_last_words_reach_the_user_and_a_question_starts_over() {
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
        assert!(unheard(&domain).is_empty());
        let said = json!({"session_id": "agent-a", "hook_event_name": "Stop", "last_assistant_message": "I cannot reach the relay."});
        assert_eq!(hook(&domain, said, Some(6)).await.0, json!({}), "the fourth turn end is let through");
        let cards = unheard(&domain);
        assert_eq!(cards.len(), 1);
        assert_eq!((cards[0].body.as_str(), &cards[0].scope), ("I cannot reach the relay.", &task_scope()));
        assert_eq!(phase_of(&domain), NodePhase::WaitingUser { in_turn: false });
        assert!(problems(&domain).is_empty(), "its words are the card, not a Problem");
        assert_eq!(hook(&domain, stop.clone(), Some(6)).await.0, json!({}), "a node waiting on that card may stop");
        assert_eq!(unheard(&domain).len(), 1, "one card, however often it stops");

        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "UserPromptSubmit", "prompt": "go on"}), Some(6)).await.1.unwrap();
        assert!(unheard(&domain).is_empty(), "a turn of its own makes its last words moot");
        free_text_question(&domain, "q", agent_actor(), task_scope(), Recipient::User, "Which name?");
        assert_eq!(hook(&domain, stop.clone(), Some(6)).await.0, json!({}), "a node waiting on its question may stop");
        free_text_answer(&domain, user_actor(), "q", "Atlas");
        assert!(reminder_only(&hook(&domain, stop.clone(), Some(6)).await.0), "asking started the count again");
    }

    /// What Claude Code sends: the Stop after a block carries `stop_hook_active`, and those turn
    /// ends count until the cap lets the node stop.
    #[tokio::test]
    async fn turns_a_block_continued_are_held_until_the_cap_then_the_node_s_words_reach_the_user() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        let first = json!({"session_id": "agent-a", "hook_event_name": "Stop"});
        let continued = json!({"session_id": "agent-a", "hook_event_name": "Stop", "stop_hook_active": true});
        assert!(reminder_only(&hook(&domain, first, Some(6)).await.0));
        assert!(reminder_only(&hook(&domain, continued.clone(), Some(6)).await.0));
        assert!(reminder_only(&hook(&domain, continued.clone(), Some(6)).await.0));
        assert!(unheard(&domain).is_empty());
        assert_eq!(hook(&domain, continued, Some(6)).await.0, json!({}), "the cap lets it stop");
        assert_eq!(unheard(&domain).len(), 1);
        assert!(problems(&domain).is_empty());
    }

    /// Whatever starts a turn of a node the user paused (a background command ending, a queued
    /// message), it may run no tool, Alondra's included, and its turn end is not held back.
    #[tokio::test]
    async fn a_node_the_user_paused_runs_no_tool_and_may_end_its_turn() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        domain.live.gate.pause_by_user(&domain, "agent-a", alondra_core::now_millis()).unwrap();
        for tool in ["Bash", "mcp__plugin_alondra_alondra__present_result"] {
            let (reply, result) = hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "PreToolUse", "tool_name": tool}), Some(6)).await;
            result.unwrap();
            assert_eq!(reply["hookSpecificOutput"]["permissionDecision"], "deny", "{tool}: {reply}");
            assert_eq!(reply["hookSpecificOutput"]["permissionDecisionReason"], PAUSED_BY_USER);
        }
        assert_eq!(hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "Stop"}), Some(6)).await.0, json!({}));

        domain.live.gate.release_by_user_message(&domain, "agent-a");
        let (reply, _) = hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "PreToolUse", "tool_name": "Bash"}), Some(6)).await;
        assert_ne!(reply["hookSpecificOutput"]["permissionDecision"], "deny", "{reply}");
        assert_eq!(hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "Stop"}), Some(6)).await.0["decision"], "block");
    }

    /// A node paused for memory wakes up when Alondra stops the build the pause cut: that turn
    /// runs no tool, so it cannot start the build again, and its turn end is not held back.
    #[tokio::test]
    async fn a_node_paused_for_memory_runs_no_tool_until_it_is_resumed() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        assert!(domain.live.gate.hold_back(&domain, "agent-a", crate::pauses::Reason::Memory, alondra_core::now_millis()));
        for tool in ["Bash", "mcp__plugin_alondra_alondra__wait_for_machine"] {
            let (reply, result) = hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "PreToolUse", "tool_name": tool}), Some(6)).await;
            result.unwrap();
            assert_eq!(reply["hookSpecificOutput"]["permissionDecision"], "deny", "{tool}: {reply}");
            assert_eq!(reply["hookSpecificOutput"]["permissionDecisionReason"], PAUSED_FOR_MEMORY);
        }
        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "UserPromptSubmit"}), Some(6)).await.1.unwrap();
        assert_eq!(domain.state().unwrap().node_phase(&AgentId("agent-a".into())).map(NodePhase::key), Some("paused"),
            "the hooks of that turn do not take it out of the pause");
        assert_eq!(hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "Stop"}), Some(6)).await.0, json!({}));
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
    async fn the_origin_the_mod_reports_for_a_prompt_says_whether_the_user_typed_it() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        mod_report(&domain, json!({"event": "prompt.submit", "origin": "plugin"}), 1);
        let node = domain.live.node("agent-a").unwrap();
        assert_eq!((node.last_prompt_origin, node.user_prompt_at), (Some(alondra_core::PromptOrigin::Other), None),
            "Alondra's mod submitting a message is not the user");
        mod_report(&domain, json!({"event": "prompt.submit", "origin": "composer"}), 2);
        let node = domain.live.node("agent-a").unwrap();
        assert_eq!(node.last_prompt_origin, Some(alondra_core::PromptOrigin::Composer));
        assert!(node.user_prompt_at.is_some(), "the user typed in the terminal");
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

    fn phase_of(domain: &Arc<DomainRuntime>) -> NodePhase {
        domain.state().unwrap().node_phase(&"agent-a".into()).cloned().unwrap()
    }

    fn mod_report(domain: &Arc<DomainRuntime>, report: Value, at: alondra_core::Millis) {
        let mut report = report;
        report["session_id"] = json!("agent-a");
        domain.report(serde_json::from_value(report).unwrap(), at);
    }

    const RELEASE: &str = "./deploy/brew/release.sh 0.2.67 > release.log";

    /// The node of the release, connected, its release script started in the background
    /// `minutes_ago` and still alive, and its turn ended behind it.
    async fn waiting_for_release(domain: &Arc<DomainRuntime>, minutes_ago: i64) -> alondra_core::Millis {
        let now = alondra_core::now_millis();
        let started = now - minutes_ago * 60_000;
        node_with_task(domain);
        domain.connect_node(serde_json::from_value(json!({"session_id": "agent-a", "version": "2.1.287"})).unwrap(), started);
        hook(domain, json!({"session_id": "agent-a", "hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": RELEASE}}), Some(6)).await;
        mod_report(domain, json!({"event": "shell.start", "id": "b1", "command": RELEASE}), started);
        domain.live.mods.set_shell_alive("agent-a", Some(true), started + 1);
        let stop = json!({"session_id": "agent-a", "hook_event_name": "Stop",
            "background_tasks": [{"id": "b1", "type": "shell", "command": RELEASE}]});
        assert_eq!(hook(domain, stop, Some(6)).await.0, json!({}), "its command will wake it: no hold-back");
        mod_report(domain, json!({"event": "background", "tasks": [{"id": "b1", "type": "shell", "command": RELEASE}]}), now);
        now
    }

    /// 13 October, the release of 0.2.67: the node ran the release script in the background and
    /// ended its turn to wait for it. Alondra knew of the script yet recorded nothing, so the node
    /// read as idle with nothing to show. It waits for its own command, named and timed, holding
    /// its slot, and the turn the command's end starts takes it back to work.
    #[tokio::test]
    async fn a_turn_that_ends_behind_its_own_command_waits_for_it_until_the_command_wakes_it() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        let now = waiting_for_release(&domain, 1).await;
        let NodePhase::WaitingBackground(work) = phase_of(&domain) else { panic!("{:?}", phase_of(&domain)) };
        assert_eq!((work.work, work.label.as_str(), work.others), (alondra_core::BackgroundKind::Shell, RELEASE, 0));
        assert!(now - work.since >= 60_000, "since the script started, as the node's mod saw it");
        assert_eq!(domain.state().unwrap().slots().len(), 1, "its command uses the Mac: it keeps its slot");
        assert_eq!(domain.store.lock().unwrap().stop_blocks(&"task".into()).unwrap(), None, "nothing counted against it");
        assert!(background_over(&domain).is_empty(), "its command still runs");

        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "UserPromptSubmit", "prompt": "<task-notification>"}), Some(6)).await;
        assert_eq!(phase_of(&domain), NodePhase::Working, "the command ended and woke it");
    }

    /// The command's shell is gone and no turn took its result: the node goes on waiting for the
    /// notification that would wake it, and once a later listing finds it without work it rests,
    /// frees its slot, and its last words reach the user.
    #[tokio::test]
    async fn a_node_whose_command_died_without_waking_it_rests_on_a_card_with_its_last_words() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        let now = waiting_for_release(&domain, 1).await;
        domain.live.mods.set_shell_alive("agent-a", Some(false), now);
        assert_eq!(background_over(&domain), ["agent-a".into()], "a look found its shell gone");
        let gone = alondra_core::now_millis() + 1;
        crate::domain_agent_lifecycle::node_event_at(&domain, &"agent-a".into(), NodeEvent::BackgroundEnded, gone).unwrap();
        assert!(background_over(&domain).is_empty(), "seen gone once");
        assert!(matches!(phase_of(&domain), NodePhase::WaitingBackground(Background { gone: Some(_), .. })));
        assert_eq!(domain.state().unwrap().slots().len(), 1, "its command's notification may still wake it");
        lifecycle::reconcile_at(&domain, [("agent-a".into(), Listed::Idle)].into(), gone).unwrap();
        assert!(unheard(&domain).is_empty(), "a listing asked for before its work was seen gone says nothing of it");
        lifecycle::reconcile_at(&domain, [("agent-a".into(), Listed::Idle)].into(), gone + 1).unwrap();
        assert_eq!(phase_of(&domain), NodePhase::WaitingUser { in_turn: false });
        assert_eq!(unheard(&domain).len(), 1, "with nothing for the user, its last words go to them");
        assert!(domain.state().unwrap().slots().is_empty(), "a dead command holds no slot");

        mod_report(&domain, json!({"event": "background", "tasks": []}), now);
        let mut reminded = std::collections::HashMap::new();
        assert!(behind_hung_work(&domain, &mut reminded).is_empty(), "it waits on that card");
    }

    const ABLATE: &str = "python3 ablate.py > ablation.txt";

    /// A turn end behind the ablation script and a Monitor on its results, with the node's words.
    async fn ends_behind_its_ablation(domain: &Arc<DomainRuntime>, said: &str, at: alondra_core::Millis) {
        let tasks = json!([{"id": "b1", "type": "shell", "command": ABLATE}, {"id": "m1", "type": "monitor", "description": "ablation results"}]);
        let stop = json!({"session_id": "agent-a", "hook_event_name": "Stop", "last_assistant_message": said, "background_tasks": tasks});
        assert_eq!(hook(domain, stop, Some(6)).await.0, json!({}), "its own work will wake it: no hold-back");
        mod_report(domain, json!({"event": "turn.complete", "turn_id": "t", "answer": said}), at);
        mod_report(domain, json!({"event": "background", "tasks": tasks}), at);
    }

    async fn between_turns(domain: &Arc<DomainRuntime>, event: &str, extra: Value) {
        let mut payload = json!({"session_id": "agent-a", "hook_event_name": event});
        payload.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        hook(domain, payload, Some(6)).await.1.unwrap();
    }

    /// 15 October, 06:12–06:27: t-fastconn-6-reachable-mac ran its ablation in the background,
    /// watched its results with a Monitor and ended each turn saying it waited («Esperando el
    /// resto.»). Claude Code's `idle_prompt` a minute after each turn end and its other notices
    /// read as turns, and its model listed the session blocked: cards 7701, 7703 and 7713 with the
    /// node's words came and went. Waiting for its own work, it works: no card while that work
    /// runs, however long, and its end wakes it.
    #[tokio::test]
    async fn a_node_waiting_for_its_ablation_and_monitor_works_through_claude_code_s_notices_and_listings() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        let now = alondra_core::now_millis();
        node_with_task(&domain);
        domain.connect_node(serde_json::from_value(json!({"session_id": "agent-a", "version": "2.1.287"})).unwrap(), now);
        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "PreToolUse", "tool_name": "Bash",
            "tool_input": {"command": ABLATE, "run_in_background": true}}), Some(6)).await;
        mod_report(&domain, json!({"event": "shell.start", "id": "b1", "command": ABLATE}), now);
        domain.live.mods.set_shell_alive("agent-a", Some(true), now + 1);
        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "PreToolUse", "tool_name": "Monitor",
            "tool_input": {"description": "ablation results", "command": "tail -f ablation.txt"}}), Some(6)).await;

        let waits = |domain: &Arc<DomainRuntime>, when: &str| {
            assert!(matches!(phase_of(domain), NodePhase::WaitingBackground(_)), "{when}: {:?}", phase_of(domain));
            assert!(unheard(domain).is_empty(), "{when}: a card with its words");
            assert_eq!(domain.state().unwrap().slots().len(), 1, "{when}: it works, holding its slot");
        };
        let listed_blocked = |domain: &Arc<DomainRuntime>| lifecycle::reconcile(domain, [("agent-a".into(), Listed::Blocked)].into()).unwrap();

        ends_behind_its_ablation(&domain, "Esperando resultados de la ablación.", now).await;
        waits(&domain, "06:12:28 turn end");
        between_turns(&domain, "Notification", json!({"notification_type": "idle_prompt", "message": "Claude is waiting for your input"})).await;
        waits(&domain, "06:13:28 idle prompt");

        for (event, said) in ["Sigue la ablación.", "Esperando el resto.", "Esperando el resto."].into_iter().enumerate() {
            let at = alondra_core::now_millis();
            mod_report(&domain, json!({"event": "turn.start", "turn_id": format!("monitor-{event}")}), at);
            assert_eq!(phase_of(&domain), NodePhase::Working, "the Monitor's event starts a turn");
            ends_behind_its_ablation(&domain, said, at).await;
            waits(&domain, said);
            between_turns(&domain, "Notification", json!({"notification_type": "agent_needs_input"})).await;
            between_turns(&domain, "MessageDisplay", json!({"turn_id": "t", "final": true, "delta": said})).await;
            listed_blocked(&domain);
            waits(&domain, "listed blocked");
            between_turns(&domain, "Notification", json!({"notification_type": "idle_prompt"})).await;
            listed_blocked(&domain);
            waits(&domain, "listed blocked after the idle prompt");
        }
        for event in ["SubagentStop", "TaskCompleted", "TeammateIdle", "FileChanged", "ConfigChange", "CwdChanged", "InstructionsLoaded",
            "PreCompact", "PostCompact", "PostModelSwitch"] {
            between_turns(&domain, event, json!({})).await;
            waits(&domain, event);
        }

        let later = alondra_core::now_millis() + 30 * 60 * 60_000;
        domain.live.mods.set_shell_alive("agent-a", Some(false), later);
        assert!(background_over(&domain).is_empty(), "its ablation is over, yet its Monitor still runs");
        lifecycle::reconcile_at(&domain, [("agent-a".into(), Listed::Idle)].into(), later + 1).unwrap();
        waits(&domain, "thirty hours on, its Monitor still runs");
        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "UserPromptSubmit", "prompt": "<task-notification>"}), Some(6)).await;
        assert_eq!(phase_of(&domain), NodePhase::Working, "the Monitor's end wakes it");
    }

    /// A workflow, a monitor or any other work with no shell to watch ends when Claude Code says it
    /// did: forty minutes or three hours in, its node still waits for it, with no card.
    #[tokio::test]
    async fn a_node_waiting_for_a_long_workflow_keeps_waiting_for_it() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        let now = alondra_core::now_millis();
        node_with_task(&domain);
        domain.connect_node(serde_json::from_value(json!({"session_id": "agent-a", "version": "2.1.287"})).unwrap(), now);
        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "PreToolUse", "tool_name": "Workflow"}), Some(6)).await;
        let tasks = json!([{"id": "w1", "type": "workflow", "name": "review-changes", "description": "Review the branch"}]);
        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "Stop", "background_tasks": tasks}), Some(6)).await;
        mod_report(&domain, json!({"event": "background", "tasks": tasks}), now);
        for minutes in [40, 180] {
            let at = now + minutes * 60_000;
            domain.live.mods.set_shell_alive("agent-a", Some(false), at);
            assert!(background_over(&domain).is_empty(), "{minutes} minutes in, the workflow still runs");
            lifecycle::reconcile_at(&domain, [("agent-a".into(), Listed::Idle)].into(), at + 1).unwrap();
            assert!(matches!(phase_of(&domain), NodePhase::WaitingBackground(Background { gone: None, .. })), "{minutes} minutes in");
            assert!(unheard(&domain).is_empty());
        }
    }

    /// A subagent runs until its end is reported, however long; then the node waits for the turn
    /// its end starts.
    #[tokio::test]
    async fn a_node_waits_for_its_subagent_until_its_end_is_reported() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        background_subagent(&domain, 150);
        let tasks = json!([{"id": "sub", "type": "subagent", "description": "musts run"}]);
        let stop = json!({"session_id": "agent-a", "hook_event_name": "Stop", "background_tasks": tasks});
        assert_eq!(hook(&domain, stop, Some(6)).await.0, json!({}));
        mod_report(&domain, json!({"event": "background", "tasks": tasks}), alondra_core::now_millis());
        assert!(matches!(phase_of(&domain), NodePhase::WaitingBackground(Background { work: alondra_core::BackgroundKind::Agent, .. })),
            "two and a half hours in, Claude Code still lists it");
        assert!(background_over(&domain).is_empty(), "a subagent at work is no shell: it still runs");
        mod_report(&domain, json!({"event": "subagent.end", "id": "sub"}), alondra_core::now_millis());
        assert_eq!(background_over(&domain), ["agent-a".into()], "its end was reported");
    }

    /// A subagent the node started in the background `minutes_ago`, with no shell call alive.
    fn background_subagent(domain: &Arc<DomainRuntime>, minutes_ago: i64) {
        let at = alondra_core::now_millis() - minutes_ago * 60_000;
        domain.connect_node(serde_json::from_value(json!({"session_id": "agent-a", "version": "2.1.287"})).unwrap(), at);
        domain.report(serde_json::from_value(json!({"session_id": "agent-a", "event": "subagent.end", "id": "sub"})).unwrap(), at);
        domain.report(serde_json::from_value(json!({"session_id": "agent-a", "event": "subagent.start", "id": "sub"})).unwrap(), at);
        domain.live.mods.set_shell_alive("agent-a", Some(false), at + 1);
    }

    /// The node of the flaky snapshots task: its one question answered, it ended its turn saying
    /// it waits. While its background work runs it may stop, as that work wakes it; once that
    /// work is over it is held back like any turn end without a result, and the user is told at
    /// the cap.
    #[tokio::test]
    async fn an_answered_question_and_finished_background_work_never_let_a_node_stop_without_delivering() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        free_text_question(&domain, "q", agent_actor(), task_scope(), Recipient::User, "Deliver only the daemon fix?");
        free_text_answer(&domain, user_actor(), "q", "Only the daemon fix");
        let stop = json!({"session_id": "agent-a", "hook_event_name": "Stop",
            "background_tasks": [{"id": "sub", "type": "agent", "description": "musts run"}]});

        background_subagent(&domain, 5);
        assert_eq!(hook(&domain, stop.clone(), Some(6)).await.0, json!({}), "its work runs and will wake it");
        assert_eq!(domain.store.lock().unwrap().stop_blocks(&"task".into()).unwrap(), None, "nothing counted");

        mod_report(&domain, json!({"event": "subagent.end", "id": "sub"}), alondra_core::now_millis());
        let stop = json!({"session_id": "agent-a", "hook_event_name": "Stop", "background_tasks": []});
        for _ in 0..STOP_BLOCKS {
            assert!(reminder_only(&hook(&domain, stop.clone(), Some(6)).await.0), "an answered question is no open question");
        }
        assert!(unheard(&domain).is_empty());
        assert_eq!(hook(&domain, stop, Some(6)).await.0, json!({}), "the cap lets it stop");
        assert_eq!(unheard(&domain).len(), 1, "its last words reach the user");
        assert!(problems(&domain).is_empty());
    }

    /// Hours with nothing new from a node: what keeps it from being reminded again is a fact, not a gap.
    const HOURS: alondra_core::Millis = 6 * 60 * 60 * 1000;

    /// A node already stopped whose background work ended without waking it ends no more turns:
    /// Alondra sends it back itself, once for each new thing the node shows (a turn end, its work's
    /// end), counting each like a held-back turn end; hours without one send nothing. Past the cap
    /// the listing that finds it idle puts its last words in front of the user.
    #[tokio::test]
    async fn a_node_idle_behind_finished_background_work_is_sent_back_once_per_fact_until_the_cap_then_its_words_reach_the_user() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "UserPromptSubmit", "prompt": "go"}), Some(6)).await.1.unwrap();
        let mut reminded = std::collections::HashMap::new();
        let now = alondra_core::now_millis();

        background_subagent(&domain, 150);
        assert!(behind_hung_work(&domain, &mut reminded).is_empty(), "its work still runs, however long");
        assert!(behind_hung_work(&domain, &mut reminded).is_empty(), "hours of it change nothing");

        mod_report(&domain, json!({"event": "subagent.end", "id": "sub"}), now);
        for round in 0..STOP_BLOCKS {
            let at = now + i64::from(round) * HOURS;
            let sent = behind_hung_work(&domain, &mut reminded);
            assert_eq!(sent.len(), 1, "round {round}");
            assert_eq!(sent[0].0, "agent-a");
            assert!(sent[0].1.starts_with("[Alondra] Do not end your turn yet"), "{}", sent[0].1);
            assert!(behind_hung_work(&domain, &mut reminded).is_empty(), "round {round}: nothing new from it, nothing sent, hours later");
            mod_report(&domain, json!({"event": "turn.complete", "turn_id": "t", "answer": "Waiting."}), at + 1);
        }
        assert!(behind_hung_work(&domain, &mut reminded).is_empty(), "the cap: no more reminders");
        assert!(problems(&domain).is_empty());
        let listed = alondra_core::now_millis();
        lifecycle::reconcile_at(&domain, [("agent-a".into(), Listed::Idle)].into(), listed).unwrap();
        lifecycle::reconcile_at(&domain, [("agent-a".into(), Listed::Idle)].into(), listed + 1).unwrap();
        assert_eq!(unheard(&domain).len(), 1, "the second listing that finds it idle puts its last words in front of the user");

        free_text_question(&domain, "q", agent_actor(), task_scope(), Recipient::User, "Which name?");
        mod_report(&domain, json!({"event": "turn.complete", "turn_id": "t", "answer": "Asked."}), now + 5 * HOURS);
        assert!(behind_hung_work(&domain, &mut reminded).is_empty(), "a node waiting on its question is left alone");
    }

    /// The Stop hook of a node never reached Alondra (its connection broke): the node comes back
    /// idle with nothing running, and that reconnection is what sends it back, once; hours later
    /// nothing else is sent until it shows something new.
    #[tokio::test]
    async fn a_node_idle_with_nothing_running_whose_stop_was_never_held_is_sent_back_once_per_fact() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        let now = alondra_core::now_millis();
        let connect = |at| domain.connect_node(serde_json::from_value(json!({"session_id": "agent-a", "version": "2.1.287"})).unwrap(), at);
        let mut reminded = std::collections::HashMap::new();
        connect(now);
        assert!(behind_hung_work(&domain, &mut reminded).is_empty(), "no turn has begun: nothing to remind it of");
        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "UserPromptSubmit", "prompt": "go"}), Some(6)).await.1.unwrap();
        connect(now + 1);
        assert_eq!(behind_hung_work(&domain, &mut reminded).len(), 1, "it came back without a turn running");
        assert_eq!(domain.store.lock().unwrap().stop_blocks(&"task".into()).unwrap().map(|(blocks, _)| blocks), Some(1));
        assert!(behind_hung_work(&domain, &mut reminded).is_empty(), "hours with nothing new from it");
        assert_eq!(domain.store.lock().unwrap().stop_blocks(&"task".into()).unwrap().map(|(blocks, _)| blocks), Some(1));
        connect(now + HOURS);
        assert_eq!(behind_hung_work(&domain, &mut reminded).len(), 1, "a new connection is a new fact");
    }

    /// What `wait_for_machine` records: the gate's declared pause, which the node machine holds,
    /// and the live view's signal.
    fn waits_for_machine(domain: &Arc<DomainRuntime>, now: alondra_core::Millis) {
        let reason = "Memory: the Bazel tests are cut for lack of memory".to_owned();
        assert!(domain.live.gate.hold_back(domain, "agent-a", crate::pauses::Reason::Declared { reason: reason.clone() }, now));
        domain.live.apply(&alondra_core::Event::WaitingMachine { session_id: "agent-a".into(), reason });
        assert!(matches!(domain.state().unwrap().node_phase(&"agent-a".into()), Some(NodePhase::Paused(_))));
    }

    fn quiet_since(domain: &Arc<DomainRuntime>, at: alondra_core::Millis) {
        domain.connect_node(serde_json::from_value(json!({"session_id": "agent-a", "version": "2.1.291"})).unwrap(), at);
    }

    /// While the machine pauses a node, its turn ends are its own: no reminder, no count, no card.
    #[tokio::test]
    async fn a_node_the_machine_paused_is_left_alone() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        let now = alondra_core::now_millis();
        let mut reminded = std::collections::HashMap::new();
        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "UserPromptSubmit", "prompt": "go"}), Some(6)).await.1.unwrap();
        waits_for_machine(&domain, now);
        assert_eq!(hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "Stop"}), Some(6)).await.0, json!({}));
        quiet_since(&domain, now);
        assert!(behind_hung_work(&domain, &mut reminded).is_empty());
        assert_eq!(domain.store.lock().unwrap().stop_blocks(&"task".into()).unwrap(), None);
        assert!(problems(&domain).is_empty());
    }

    /// t-gate-count-mismatch, 10 Oct: it waited for the machine at 07:43, Alondra resumed it at
    /// 15:55, and at 15:59 it ended its turn saying its budget was spent. Alondra's own resume left
    /// the live view saying it waited for the machine, so the Stop hook let it stop and nothing
    /// sent it back for two days. Claude Code listed it blocked, its session alive and idle. Only
    /// the node machine's pause excuses a node: this one is held back, and once its turn ends its
    /// words are a card in front of the user.
    #[tokio::test]
    async fn a_node_resumed_after_waiting_for_the_machine_that_stops_on_a_spent_budget_reaches_the_user() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        let mut now = alondra_core::now_millis();
        let mut reminded = std::collections::HashMap::new();
        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "UserPromptSubmit", "prompt": "go"}), Some(6)).await.1.unwrap();
        waits_for_machine(&domain, now);
        lifecycle::node_event(&domain, &"agent-a".into(), NodeEvent::Resumed).unwrap();
        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "UserPromptSubmit",
            "prompt": "[From Alondra · 15:55] You waited for the machine since 07:43. The Mac is fine again: carry on with your task."}), Some(6)).await.1.unwrap();
        assert_eq!(domain.live.node("agent-a").unwrap().turn_signal, Some(alondra_core::TurnSignal::WaitingMachine),
            "Alondra's resume leaves the live view's signal as it was");

        let stop = json!({"session_id": "agent-a", "hook_event_name": "Stop",
            "last_assistant_message": "The fix is not finished. I stop because the weekly budget is exhausted (1% left)."});
        assert!(reminder_only(&hook(&domain, stop, Some(6)).await.0), "a spent budget is no pause");

        lifecycle::node_event(&domain, &"agent-a".into(), NodeEvent::TurnEnded).unwrap();
        lifecycle::reconcile(&domain, [("agent-a".into(), Listed::Blocked)].into()).unwrap();
        assert_eq!(phase_of(&domain), NodePhase::WaitingUser { in_turn: false }, "blocked waits on the user: no work");
        let cards = unheard(&domain);
        assert_eq!(cards.len(), 1);
        assert!(cards[0].body.starts_with("The fix is not finished"), "{}", cards[0].body);
        quiet_since(&domain, now);
        assert!(behind_hung_work(&domain, &mut reminded).is_empty(), "it waits on the card with its words");
        now += HOURS;
        assert!(behind_hung_work(&domain, &mut reminded).is_empty());
        assert!(problems(&domain).is_empty());
    }

    /// 13 October: 1d6d53c5 ended its turn asking for a repro in its own terminal; neither its turn
    /// end nor its mod's reached Alondra, so the mod still said "in a turn" and nothing sent it back
    /// for 8.5 hours. Claude Code lists it blocked: it is no work, and what it said reaches the user
    /// in a card.
    #[tokio::test]
    async fn a_node_claude_code_shows_blocked_puts_its_words_in_front_of_the_user() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        let now = alondra_core::now_millis();
        let mut reminded = std::collections::HashMap::new();
        hook(&domain, json!({"session_id": "agent-a", "hook_event_name": "UserPromptSubmit", "prompt": "go"}), Some(6)).await.1.unwrap();
        quiet_since(&domain, now);
        domain.report(serde_json::from_value(json!({"session_id": "agent-a", "event": "turn.start", "turn_id": "t"})).unwrap(), now);
        lifecycle::reconcile(&domain, [("agent-a".into(), Listed::Blocked)].into()).unwrap();
        assert_eq!(phase_of(&domain), NodePhase::WaitingUser { in_turn: false }, "blocked is no work");
        let cards = unheard(&domain);
        assert_eq!(cards.len(), 1, "its words reach the user");
        assert_eq!(cards[0].card.need, "“Task” stopped without a word", "no Stop came with its words");
        assert!(cards[0].card.content.is_none());

        domain.live.mods.set_job("agent-a", Some(crate::job_state::Job {
            phase: crate::job_state::Phase::Blocked, detail: "needs user repro".into(), tasks: 0, updated_at: now, settings: None, worktree: None,
        }), None);
        assert!(behind_hung_work(&domain, &mut reminded).is_empty(), "it waits on that card");
    }

    #[tokio::test]
    async fn a_delivery_handed_back_must_be_presented_again_before_the_turn_ends() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        command(&domain, agent_actor(), "present", Command::PresentResult {
            checklist: Vec::new(),
            attachments: vec![], task_id: "task".into(), interaction_id: "delivery".into(),
            body: "Done".into(), targets: vec![DeliveryTarget::Text { text: "The answer".into() }], is_final: true,
            nothing_to_review: false,
            card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
        });
        let stop = json!({"session_id": "agent-a", "hook_event_name": "Stop"});
        assert_eq!(hook(&domain, stop.clone(), Some(6)).await.0, json!({}), "in review");
        command(&domain, user_actor(), "changes", Command::SubmitFeedback { merge_failure: None,
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
        assert_eq!(unregistered_pr_text(&state, "github.com", "Owner/Repo", 7), None);
        let text = unregistered_pr_text(&state, "github.com", "owner/repo", 8).unwrap();
        assert!(text.contains("owner/repo#8") && text.contains("register_pr"), "{text}");
        assert!(unregistered_pr_text(&state, "ghe.example.com", "owner/repo", 7).is_some(), "another host's #7 is another PR");
    }

    #[tokio::test]
    async fn a_turn_ending_after_a_result_is_not_held() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        node_with_task(&domain);
        command(&domain, agent_actor(), "present", Command::PresentResult {
            checklist: Vec::new(),
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

    async fn acknowledge(domain: &Arc<DomainRuntime>, session: &str, ids: Value) -> Result<()> {
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
        let domain = fixture.open();
        let mut node = connect(&domain, "session-a").await;
        assert!(node.nothing_arrives().await, "nothing is written while nothing is pending, and nothing polls");

        answered_question(&domain, "session-a");
        let line = node.line().await.unwrap();
        let id = agent_outbox(&domain, "session-a")[0].0;
        assert_eq!((line["id"].as_str(), line["as_user"].as_bool()), (Some(format!("o-{id}").as_str()), Some(true)));
        let user = crate::domain_messages::user_name();
        let text = line["text"].as_str().unwrap();
        assert!(text.starts_with(&format!("[From {user} · ")) && text.ends_with("] Answer to your card question-session-a (Answer the question): Yes."), "the user's answer says who and when: {text}");
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
        let domain = fixture.open();
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
        let domain = fixture.open();
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
        let domain = fixture.open();
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

    fn working_agent(domain: &Arc<DomainRuntime>, agent: &str) {
        working_agent_without_mod(domain, agent);
        domain.live.mods.loaded_in(agent);
    }

    fn working_agent_without_mod(domain: &Arc<DomainRuntime>, agent: &str) {
        let user = || Actor::User { user_id: "u".into() };
        command(domain, Actor::System, &format!("register-{agent}"), Command::RegisterAgent { agent_id: agent.into(), name: agent.into(), at: 0 });
        command(domain, user(), &format!("request-{agent}"),
            Command::CreateRequest { request_id: format!("request-{agent}").into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        command(domain, Actor::Orchestrator { generation: 0 }, &format!("task-{agent}"), Command::CreateTask { plan_items: None, continues: None,
            task_id: format!("task-{agent}").into(), request_id: format!("request-{agent}").into(),
            title: "Task".into(), agent_id: Some(agent.into()), review_prs: false, dependencies: BTreeSet::new(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
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
            "input": {"plan": "# Ship the parser\n\n- [ ] Split the lexer"}})
    }

    fn elicitation(agent: &str) -> Value {
        json!({"session_id": agent, "hook_event_name": "Elicitation", "mcp_server_name": "github", "message": "Which repository?",
            "requested_schema": {"type": "object", "properties": {"repo": {"type": "string", "title": "Repository"}}}})
    }

    async fn open_hold(domain: &Arc<DomainRuntime>) -> InteractionId {
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

    fn answer(domain: &Arc<DomainRuntime>, id: &InteractionId, option: &str, note: Option<&str>) {
        answer_with(domain, id, option, note.map(|note| BTreeMap::from([(Hold::NOTE.to_owned(), InputValue::Text(note.into()))])).unwrap_or_default());
    }

    fn answer_with(domain: &Arc<DomainRuntime>, id: &InteractionId, option: &str, inputs: BTreeMap<String, InputValue>) {
        command(domain, Actor::User { user_id: "u".into() }, &format!("answer-{id}"), Command::AnswerQuestion {
            attachments: vec![], interaction_id: id.clone(), option_id: option.into(), inputs,
        });
    }

    /// A frame as Claude Code's hook or the node mod's helper sends it, answered by the user from a card while it waits.
    async fn held(domain: &Arc<DomainRuntime>, frame: Value, option: &str, note: Option<&str>) -> (Value, InteractionId) {
        held_with(domain, frame, option, note.map(|note| BTreeMap::from([(Hold::NOTE.to_owned(), InputValue::Text(note.into()))])).unwrap_or_default()).await
    }

    async fn held_with(domain: &Arc<DomainRuntime>, frame: Value, option: &str, inputs: BTreeMap<String, InputValue>) -> (Value, InteractionId) {
        let (mut client, server) = UnixStream::pair().unwrap();
        send(&mut client, &frame).await;
        let handler = handle(server, domain);
        tokio::pin!(handler);
        let id = tokio::select! {
            id = open_hold(domain) => id,
            result = &mut handler => panic!("answered without waiting: {result:?}"),
        };
        answer_with(domain, &id, option, inputs);
        let (result, body) = tokio::join!(handler, reply(&mut client));
        result.unwrap();
        (body, id)
    }

    async fn held_card(domain: &Arc<DomainRuntime>, card: Value, option: &str, note: Option<&str>) -> (Value, InteractionId) {
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

    /// A repo with a worktree where Alondra's nodes work, in a folder the test owns.
    fn repo_with_worktree() -> (crate::test_support::Directory, std::path::PathBuf, std::path::PathBuf) {
        let dir = crate::test_support::Directory::new();
        let root = dir.join("app");
        std::fs::create_dir_all(&root).unwrap();
        let git = |dir: &std::path::Path, args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success(), "{args:?}");
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "first"]);
        let worktree = root.join(".claude/worktrees/t-1");
        git(&root, &["worktree", "add", "-q", "-b", "t-1", worktree.to_str().unwrap()]);
        (dir, root, worktree)
    }

    fn command_card(agent: &str, command: &str, worktree: &std::path::Path, suggestions: Value) -> Value {
        json!({"session_id": agent, "id": "toolu-bash", "kind": "permission", "tool": "Bash",
            "input": {"command": command, "description": "Build"}, "suggestions": suggestions,
            "cwd": worktree.to_str().unwrap(), "permission_mode": "default"})
    }

    fn rule(content: &str, destination: &str) -> Value {
        json!({"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": content}], "behavior": "allow", "destination": destination})
    }

    fn option_ids(domain: &Arc<DomainRuntime>, id: &InteractionId) -> Vec<String> {
        let InteractionPayload::Question { options, .. } = &domain.state().unwrap().interactions[id].payload else { panic!("question") };
        options.iter().map(|o| o.id.clone()).collect()
    }

    #[tokio::test]
    async fn runtime_progress_allow_always_confirmation_does_not_hold_up_an_unrelated_node_ack() {
        use std::future::Future;

        let (_dir, root, worktree) = repo_with_worktree();
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "permission-agent");
        answered_question(&domain, "ack-agent");
        let message = agent_outbox(&domain, "ack-agent")[0].0;
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let (release, confirmation) = tokio::sync::oneshot::channel();
        *domain.allow_always_wait.lock().unwrap() = Some(crate::allow_always::ConfirmationWait { entered, release: confirmation });
        let (mut client, server) = UnixStream::pair().unwrap();
        let card = command_card("permission-agent", "make build", &worktree, Value::Null);
        send(&mut client, &json!({"node_hold": card})).await;
        let holding = tokio::spawn({
            let domain = domain.clone();
            async move { handle(server, &domain).await }
        });
        let id = open_hold(&domain).await;
        answer(&domain, &id, Hold::ALWAYS, None);
        waiting.await.expect("the pending rule reached settings confirmation");
        let rows = domain.store.lock().unwrap().allowed_rules().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, crate::allow_always::RuleState::Pending);

        let mut acknowledgement = Box::pin(acknowledge(&domain, "ack-agent", json!([format!("o-{message}")])));
        let admitted = domain.hook_work.available() == 1;
        // Poll the real transport first so an occupied lane queues this acknowledgement.
        let initial = acknowledgement.as_mut().poll(&mut std::task::Context::from_waker(std::task::Waker::noop()));
        let completed_before_release = match initial {
            std::task::Poll::Ready(result) => { result.unwrap(); true }
            std::task::Poll::Pending if admitted => { acknowledgement.as_mut().await.unwrap(); true }
            std::task::Poll::Pending => false,
        };
        if completed_before_release {
            assert!(agent_outbox(&domain, "ack-agent").is_empty(), "the real acknowledgement handed the message");
        }
        std::fs::write(root.join(crate::allow_always::SETTINGS), r#"{"permissions":{"allow":["Bash(make build *)"]}}"#).unwrap();
        release.send(()).unwrap();
        let body = reply(&mut client).await;
        holding.await.unwrap().unwrap();
        if !completed_before_release { acknowledgement.await.unwrap(); }
        assert_eq!(body["allowed"], true);
        assert_eq!(domain.store.lock().unwrap().allowed_rules().unwrap()[0].state, crate::allow_always::RuleState::Active);
        assert!(completed_before_release, "the real unrelated node acknowledgement queued behind settings confirmation");
        println!("settings_confirmation=held unrelated_node_ack=completed_before_release rule=active physical_jobs=drained");
    }

    #[tokio::test]
    async fn allow_always_hands_back_alondras_own_rules_for_the_repo_and_remembers_them() {
        let (_dir, root, worktree) = repo_with_worktree();
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let card = command_card("agent-a", "make build 2>&1 | tail -1", &worktree, json!([rule("make build *", "userSettings")]));
        let (reply, id) = held_card(&domain, card, Hold::ALWAYS, None).await;
        assert_eq!((reply["held"].as_bool(), reply["allowed"].as_bool()), (Some(true), Some(true)), "{reply}");
        assert_eq!(reply["permissions"], json!([{"type": "addRules", "behavior": "allow", "destination": "localSettings",
            "rules": [{"toolName": "Bash", "ruleContent": "make build *"}]}]), "never the user's global settings");
        assert_eq!(option_ids(&domain, &id), ["allow", "always", "deny"]);
        assert_eq!(domain.state().unwrap().interactions[&id].card.allow_always, Some(alondra_core::allow_always::AllowAlways::Offered {
            repo: "app".into(), rules: vec!["make build *".into()], quiet: vec!["tail".into()] }));
        let rows = crate::allow_always::reconcile(&domain);
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].rule.as_str(), rows[0].interaction_id.as_str(), rows[0].task_id.as_deref()), ("make build *", id.0.as_str(), Some("task-agent-a")));
        assert_eq!(rows[0].settings, root.join(".claude/settings.local.json").to_string_lossy());
        assert_eq!(rows[0].state, crate::allow_always::RuleState::Missing, "Claude Code never wrote it in this test, so it is not listed as active");
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::write(root.join(".claude/settings.local.json"), r#"{"permissions":{"allow":["Bash(make build *)"]}}"#).unwrap();
        assert_eq!(crate::allow_always::reconcile(&domain)[0].state, crate::allow_always::RuleState::Active);
        let removed = crate::allow_always::remove_rule(&domain, rows[0].id).unwrap().unwrap();
        assert_eq!(removed.rule, "make build *");
        assert_eq!(std::fs::read_to_string(root.join(".claude/settings.local.json")).unwrap(), r#"{"permissions":{"allow":[]}}"#);
        assert_eq!(crate::allow_always::reconcile(&domain)[0].state, crate::allow_always::RuleState::Removed);
        assert!(crate::allow_always::remove_rule(&domain, rows[0].id).is_ok(), "removing again is harmless");
    }

    #[tokio::test]
    async fn a_command_that_keeps_asking_offers_no_always_whatever_claude_code_suggests() {
        let (_dir, _root, worktree) = repo_with_worktree();
        for option in [Hold::ALLOW, Hold::DENY] {
            let fixture = Fixture::new();
            let domain = fixture.open();
            working_agent(&domain, "agent-a");
            let card = command_card("agent-a", "make build && rm -rf out", &worktree,
                json!([rule("make build *", "localSettings"), rule("rm -rf out", "localSettings")]));
            let (reply, id) = held_card(&domain, card, option, None).await;
            assert!(reply.get("permissions").is_none(), "{option}: {reply}");
            assert_eq!(option_ids(&domain, &id), ["allow", "deny"], "no disabled third option");
            assert_eq!(domain.state().unwrap().interactions[&id].card.allow_always,
                Some(alondra_core::allow_always::Refusal::new("rm -rf out", alondra_core::allow_always::Why::Deletes).into()));
            assert!(crate::allow_always::reconcile(&domain).is_empty(), "nothing is remembered");
        }
    }

    #[tokio::test]
    async fn a_prompt_from_a_hook_that_does_not_say_where_it_runs_has_no_always() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let mut card = permission("agent-a");
        card["suggestions"] = json!([rule("make build *", "localSettings")]);
        let (reply, id) = held_card(&domain, card, Hold::ALLOW, None).await;
        assert!(reply.get("permissions").is_none());
        assert_eq!(option_ids(&domain, &id), ["allow", "deny"]);
        assert_eq!(domain.state().unwrap().interactions[&id].card.allow_always, None);
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
    async fn a_plan_without_a_checklist_goes_back_to_its_node_and_never_reaches_the_user() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let before = domain.state().unwrap().interactions.len();
        let (mut client, server) = UnixStream::pair().unwrap();
        let unlisted = json!({"session_id": "agent-a", "id": "toolu-plan", "kind": "plan", "input": {"plan": "# Ship the parser\n\n1. Split the lexer"}});
        send(&mut client, &json!({"node_hold": unlisted})).await;
        let (result, reply) = tokio::join!(handle(server, &domain), reply(&mut client));
        result.unwrap();
        assert_eq!((reply["held"].as_bool(), reply["approved"].as_bool()), (Some(true), Some(false)), "{reply}");
        assert!(reply["feedback"].as_str().unwrap().starts_with("Your plan has no checklist. Write its scope as Markdown checklist lines (`- [ ] …`)"), "{reply}");
        let state = domain.state().unwrap();
        assert_eq!(state.interactions.len(), before, "no card is put to the user");
        assert!(state.holds.is_empty());
    }

    #[test]
    fn a_plan_carries_the_markdown_files_it_links_to_and_a_mention_never_sends_it_back() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let worktree = tempfile::tempdir().unwrap();
        std::fs::create_dir(worktree.path().join("docs")).unwrap();
        std::fs::write(worktree.path().join("docs/design.md"), "# Fast connection\n\n- [ ] Measure\n- [ ] Relay").unwrap();
        let cwd = worktree.path().to_str().unwrap();
        let post = |id: &str, plan: &str| crate::domain_holds::post(&domain, &serde_json::from_value(json!({"session_id": "agent-a", "id": id,
            "kind": "plan", "cwd": cwd, "input": {"plan": plan}})).unwrap(), true).unwrap();
        let mention = "# Step 1\n\nCreate docs/fast-connection.md with the diagram.\n\n- [ ] Ship the relay";
        let id = match post("toolu-mention", mention) {
            crate::domain_holds::Posted::Held(id) => id,
            _ => panic!("a plan that only mentions a Markdown file is put to the user as written"),
        };
        assert_eq!(domain.state().unwrap().interactions[&id].body, mention);
        let id = match post("toolu-plan", "# Step 1\n\nContext in [the plan](docs/design.md).\n\n- [ ] Ship the relay") {
            crate::domain_holds::Posted::Held(id) => id,
            _ => panic!("a plan whose cited file can be read is put to the user"),
        };
        let state = domain.state().unwrap();
        let body = &state.interactions[&id].body;
        assert!(body.ends_with("## docs/design.md\n\n### Fast connection\n\n- ☐ Measure\n- ☐ Relay"), "{body}");
        assert_eq!(alondra_core::domain::checklist_items(body).len(), 1, "the file's checkboxes are not points of the plan");
    }

    #[test]
    fn a_plan_of_a_task_that_waits_for_another_goes_back_to_its_node_without_a_card() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        working_agent(&domain, "agent-b");
        command(&domain, Actor::Orchestrator { generation: 0 }, "depend", Command::SetDependencies {
            task_id: "task-agent-a".into(), dependencies: BTreeSet::from(["task-agent-b".into()]), stack: None,
        });
        let before = domain.state().unwrap().interactions.len();
        let card = json!({"session_id": "agent-a", "id": "toolu-plan", "kind": "plan", "input": {"plan": "# Step 1\n\n- [ ] Ship the relay"}});
        match crate::domain_holds::post(&domain, &serde_json::from_value(card).unwrap(), true).unwrap() {
            crate::domain_holds::Posted::Denied(reason) => assert!(reason.contains("ask the orchestrator to remove it"), "{reason}"),
            _ => panic!("a plan of a waiting task is put to the user"),
        }
        let state = domain.state().unwrap();
        assert_eq!(state.interactions.len(), before, "no card is put to the user");
        assert!(state.holds.is_empty());
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

    /// What each client shows under the answer buttons of card `id`, by option.
    fn option_words(domain: &DomainRuntime, id: &InteractionId) -> Vec<(alondra_core::views::Client, Vec<(String, Option<String>)>)> {
        use alondra_core::views::{Client, Item, RenderContext, Transport, build, live::LiveSnapshot, text::Lang};
        let read = domain.read();
        let live = LiveSnapshot::default();
        [Client::Ios, Client::Mac, Client::Tui].into_iter().map(|client| {
            let view = build(&format!("card:{}", id.0), RenderContext {
                domain: read.state(), index: read.index(), live: &live, client, lang: Lang::En,
                transport: Transport::Loopback, cursor: "test", rendered_at: "",
            }).unwrap();
            let words = view.sections.iter().flat_map(|section| &section.items).filter_map(|item| match item {
                Item::Card(card) => Some(card),
                _ => None,
            }).flat_map(|card| card.actions.iter().chain(&card.more))
                .filter(|action| action.option_id.is_some())
                .map(|action| (action.option_id.clone().unwrap(), action.description.clone())).collect();
            (client, words)
        }).collect()
    }

    #[test]
    fn claudes_question_reaches_every_client_with_the_words_under_each_option() {
        let fixture = Fixture::new();
        let domain = fixture.open();
        working_agent(&domain, "agent-a");
        let mut single = single_question("agent-a");
        single["input"]["questions"][0]["options"] = json!([
            {"label": "Markdown", "description": "Headings and lists"}, {"label": "Plain text", "description": "No markup at all"}]);
        let crate::domain_holds::Posted::Held(id) = crate::domain_holds::post(&domain, &serde_json::from_value(single).unwrap(), true).unwrap()
        else { panic!("not held") };
        let expected = vec![("o0".to_owned(), Some("Headings and lists".to_owned())), ("o1".to_owned(), Some("No markup at all".to_owned()))];
        for (client, words) in option_words(&domain, &id) {
            assert_eq!(words, expected, "{client:?}");
        }
        let crate::domain_holds::Posted::Held(first) = crate::domain_holds::post(&domain, &serde_json::from_value(three_questions("agent-a")).unwrap(), true).unwrap()
        else { panic!("not held") };
        for (client, words) in option_words(&domain, &first) {
            assert_eq!(words, [("o0".to_owned(), Some("Works at home and away".to_owned())), ("o1".to_owned(), Some("Only at home".to_owned()))],
                "{client:?}: a question asked with others keeps its words too");
        }
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

    async fn post_group(domain: &Arc<DomainRuntime>) -> (UnixStream, std::pin::Pin<Box<impl std::future::Future<Output = Result<()>> + '_>>) {
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

    async fn answered_without_a_card(domain: &Arc<DomainRuntime>, card: Value) -> Value {
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
        command(&domain, Actor::System, "register-idle", Command::RegisterAgent { agent_id: "idle".into(), name: "idle".into(), at: 0 });
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
    async fn an_mcp_form_of_a_yes_or_no_field_takes_only_the_value_the_user_set() {
        let remember = |agent: &str| json!({"session_id": agent, "hook_event_name": "Elicitation", "mcp_server_name": "db", "message": "Remember this device?",
            "requested_schema": {"type": "object", "properties": {"remember": {"type": "boolean", "title": "Remember"}}, "required": ["remember"]}});
        let filled = || BTreeMap::from([(alondra_core::domain::ANSWER_CONTENT.to_owned(), InputValue::Form(json!({"remember": true}).as_object().unwrap().clone()))]);
        let said = || BTreeMap::from([(Hold::NOTE.to_owned(), InputValue::Text("vale".into()))]);
        for (inputs, content) in [(filled(), json!({"remember": true})), (said(), json!({}))] {
            let fixture = Fixture::new();
            let domain = fixture.open();
            working_agent(&domain, "agent-a");
            let (reply, id) = held_with(&domain, json!({"launched": true, "payload": remember("agent-a")}), Hold::ACCEPT, inputs).await;
            assert_eq!(reply["hookSpecificOutput"], json!({"hookEventName": "Elicitation", "action": "accept", "content": content}));
            let InteractionPayload::Question { options, .. } = &domain.state().unwrap().interactions[&id].payload else { panic!("question") };
            assert_eq!(options[0].description, None, "a form with a control per field asks for no words");
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

    async fn report(domain: &Arc<DomainRuntime>, report: Value) {
        let (mut client, server) = UnixStream::pair().unwrap();
        send(&mut client, &json!({"node_event": report})).await;
        let (result, _) = tokio::join!(handle(server, domain), reply(&mut client));
        result.unwrap();
    }

    async fn withdrawn_by(domain: &Arc<DomainRuntime>, card: Value, ended: Value) -> (Value, InteractionId) {
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
