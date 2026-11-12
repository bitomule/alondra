use crate::records::{InputRecord, SessionRecord};
use alondra_agent_application::{Normalization, ObservationMetadata, ObservationNormalizer};
use alondra_agent_protocol::{
    Body, ControlResult, DecisionKind, DecisionOutcome, Event, Fence, FramedReader, McpServer, NativeCorrelation,
    Observation as WireObservation, SdkEventKind, SignIn,
};
use alondra_foundation::{ContinuationId, MessageUuid, TurnId, WorkId};
use alondra_provider_contract::{
    pull_requests_named, runs_gh_pr_create, reconciled_continuation, ENDED_WITHOUT_RESULT,
    AdapterError, ApiFailure, McpServerReport, McpSignInReport, Observation, OrderedObservation, TurnOutcome, UsageReport, UsageWindow,
    UsageWindowKind,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

const CHECKPOINT_FORMAT: u16 = 1;
/// More decisions open at once than the host ever holds pending.
const MAX_ASKED: usize = 64;
/// Most MCP tool calls whose result is still to come that a failure is traced back to.
const MAX_MCP_CALLS: usize = 64;
/// Most inputs of ours Claude took in and has not finished yet.
const MAX_TAKEN: usize = 64;
/// How a turn still open ends when Claude starts another prompt.
const TURN_SUPERSEDED: &str = "turn_superseded";
/// The `type` of the payload the host reports the session's account with.
const ACCOUNT_REPORT: &str = "account";
const MAX_PLAN_BYTES: usize = 64;
const MAX_WINDOW_NAME_BYTES: usize = 64;

pub(super) struct Normalizer {
    pub session: Option<SessionRecord>,
    pub fence: Option<Fence>,
    /// The first cursor of a host launched after its execution's previous host ended: the turn and
    /// work that host's Claude had open ended with it.
    pub first_cursor: Option<u64>,
    /// The events being committed are the last of a host that ended: the exit of its Claude and the
    /// end of its SDK iterator are evidence, not facts.
    pub host_ended: bool,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    format: u16,
    cursor: u64,
    initialized: bool,
    turn: Option<LogicalTurn>,
    work: BTreeMap<String, LogicalWork>,
    seen_tasks: BTreeSet<String>,
    /// The decisions Claude asked for and still waits on: the request id of each, by epoch.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    asked: BTreeMap<u64, String>,
    /// The Agent calls with an isolated worktree whose result is still to come, by tool use id,
    /// oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    isolated: Vec<String>,
    /// The commands running `gh pr create` whose result is still to come, by tool use id.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pr_commands: BTreeSet<String>,
    /// Work that ended after its turn, oldest first: its notification wakes a root turn of
    /// Claude's own, which takes up every notification waiting, and whose result comes from a
    /// notification, naming the run it took up when the work had one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    woken: Vec<Woken>,
    /// The calls to an MCP server's tool whose result is still to come, by tool use id.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    mcp_calls: BTreeSet<String>,
    /// The inputs of ours Claude started or consumed and has not finished yet, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    taken: Vec<MessageUuid>,
    /// The inputs of ours Claude finished, oldest first: a result that names one after its end
    /// does not take it again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    finished: Vec<MessageUuid>,
    /// When the usage limit Claude refused a request on outside a turn resets, in Unix seconds:
    /// the next turn the limit ends carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit_resets_at: Option<u64>,
    /// The background tasks Claude Code listed as live last, by task id: each list replaces the
    /// one before, and a Claude that starts again has none until it lists them.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    listed: BTreeSet<String>,
    /// The event in which Claude Code said the session runs while no turn was open: the input of
    /// ours it starts next opens the turn it named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    running: Option<String>,
}
impl Checkpoint {
    /// A host launched after the execution's previous one ended: the turn, the work and the inputs
    /// that host's Claude had open ended with it.
    fn forget_ended_host(&mut self) {
        self.turn = None;
        self.work.clear();
        self.woken.clear();
        self.taken.clear();
        self.finished.clear();
        self.listed.clear();
        self.running = None;
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Woken {
    /// The run of the work that ended. A background Bash has none: Claude Code 2.1.300 names a
    /// run only for a subagent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run: Option<String>,
    continuation: String,
    /// The work that ended, as Claude names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LogicalTurn {
    // Alondra logical identity assigned at an observed SDK root-message boundary.
    id: String,
    consumed: BTreeSet<MessageUuid>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    answered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resets_at: Option<u64>,
    /// The run of the work whose notification opened this turn, when no input of Alondra's did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    woken_by: Option<String>,
    /// A notification opened this turn: its result comes from one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    notified: bool,
    /// Claude opened this turn by itself: whatever result comes next ends it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    unprompted: bool,
    /// The session said it runs and started an input of ours, which opened this turn before the
    /// init Claude Code sends for each prompt: that init belongs to this turn.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    awaits_init: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LogicalWork {
    run: Option<String>,
    id: String,
    /// What Claude said the work does when it started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// Claude Code runs it in the background: it lists the work as live until it ends.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    background: bool,
}
const DESCRIPTION_LIMIT: usize = 200;

/// The background work of Claude's that a host left behind when it ended, by the checkpoint of its
/// last observation: each task still running, and each that ended whose notification Claude had
/// not taken up yet, named as Claude knows it.
pub fn background_left(checkpoint: &[u8]) -> Vec<String> {
    let Ok(checkpoint) = serde_json::from_slice::<Checkpoint>(checkpoint) else { return Vec::new() };
    let named = |task: &str, description: Option<&str>| match description {
        Some(description) => format!("{task} ({description})"),
        None => task.to_owned(),
    };
    let running = checkpoint.work.iter().map(|(task, work)| named(task, work.description.as_deref()));
    let unread = checkpoint.woken.iter().filter_map(|woken| {
        woken.task.as_deref().map(|task| format!("{}, which had ended: its notification was never read", named(task, woken.description.as_deref())))
    });
    running.chain(unread).collect()
}

/// The checkpoint the observation before `batch` left, which `batch` must follow.
fn prior_checkpoint(batch: &OrderedObservation, metadata: &ObservationMetadata<'_, '_>) -> Result<Checkpoint, AdapterError> {
    let Some(prior) = metadata.checkpoint().map_err(storage)? else {
        if batch.cursor != 1 {
            return Err(AdapterError::InvalidObservation);
        }
        return Ok(Checkpoint { format: CHECKPOINT_FORMAT, ..Checkpoint::default() });
    };
    let decoded: Checkpoint = serde_json::from_slice(prior.bytes()).map_err(|_| AdapterError::InvalidObservation)?;
    if decoded.format != CHECKPOINT_FORMAT
        || decoded.cursor != prior.cursor()
        || decoded.cursor.checked_add(1) != Some(batch.cursor)
    {
        return Err(AdapterError::InvalidObservation);
    }
    Ok(decoded)
}

impl ObservationNormalizer for Normalizer {
    fn skip(&self, batch: &OrderedObservation, metadata: &ObservationMetadata<'_, '_>) -> Result<Normalization, AdapterError> {
        if &batch.execution != metadata.execution() || batch.cursor != metadata.cursor() {
            return Err(AdapterError::InvalidObservation);
        }
        let mut checkpoint = prior_checkpoint(batch, metadata)?;
        checkpoint.cursor = batch.cursor;
        Normalization::new(Vec::new(), serde_json::to_vec(&checkpoint).map_err(|_| AdapterError::InvalidObservation)?)
    }
    fn normalize(
        &self,
        batch: &OrderedObservation,
        metadata: &ObservationMetadata<'_, '_>,
    ) -> Result<Normalization, AdapterError> {
        if &batch.execution != metadata.execution()
            || batch.cursor != metadata.cursor()
            || batch.evidence.is_empty()
            || batch.evidence.len() > alondra_agent_protocol::MAX_FRAME_BYTES
        {
            return Err(AdapterError::InvalidObservation);
        }
        let mut framed = (batch.evidence.len() as u32).to_be_bytes().to_vec();
        framed.extend_from_slice(&batch.evidence);
        let received = FramedReader::new(Cursor::new(framed))
            .read_received()
            .map_err(|_| AdapterError::InvalidObservation)?
            .ok_or(AdapterError::InvalidObservation)?;
        let Body::Event(event) = &received.frame().body else {
            return Err(AdapterError::InvalidObservation);
        };
        let alondra_agent_protocol::Execution::Task(execution) = &event.execution else {
            return Err(AdapterError::InvalidObservation);
        };
        if event.cursor != batch.cursor
            || self.fence.as_ref() != Some(&event.fence)
            || execution.task_id != *batch.execution.task()
            || execution.agent_id != *batch.execution.node()
            || execution.conversation_id != batch.execution.conversation().as_str()
            || execution.process_instance_id != batch.execution.instance().as_str()
            || execution.generation != batch.execution.generation()
            || event.fence.host_id != batch.execution.host().as_str()
        {
            return Err(AdapterError::InvalidObservation);
        }
        event.validate().map_err(|_| AdapterError::Unsupported)?;
        let unknown_message = matches!(
            &event.observation,
            WireObservation::Sdk { kind: SdkEventKind::Unsupported, .. }
        );
        let mut checkpoint = prior_checkpoint(batch, metadata)?;
        if self.first_cursor == Some(batch.cursor) {
            checkpoint.forget_ended_host();
        }
        let session = self
            .session
            .as_ref()
            .ok_or(AdapterError::InvalidObservation)?;
        if unknown_message || (self.host_ended && ended_with_its_host(&event.observation)) {
            checkpoint.cursor = batch.cursor;
            let bytes = serde_json::to_vec(&checkpoint).map_err(|_| AdapterError::InvalidObservation)?;
            return Normalization::new(Vec::new(), bytes);
        }
        let before = serde_json::to_vec(&checkpoint).map_err(|_| AdapterError::InvalidObservation)?;
        let folded = fold(event, session, &mut checkpoint, |ids| {
            let keys = ids
                .iter()
                .map(|id| {
                    serde_json::to_vec(&InputRecord {
                        message_id: id.clone(),
                    })
                    .map_err(|_| AdapterError::InvalidObservation)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let records = metadata.lookup_inputs(&keys).map_err(storage)?;
            if records.len() != ids.len() {
                return Err(AdapterError::InvalidObservation);
            }
            let mut mappings = BTreeMap::new();
            for record in records {
                let native: InputRecord = serde_json::from_slice(record.metadata())
                    .map_err(|_| AdapterError::InvalidObservation)?;
                if !ids.contains(&native.message_id)
                    || mappings
                        .insert(native.message_id, record.input().clone())
                        .is_some()
                {
                    return Err(AdapterError::InvalidObservation);
                }
            }
            ids.iter()
                .map(|id| mappings.remove(id).ok_or(AdapterError::InvalidObservation))
                .collect()
        });
        let facts = match folded {
            Ok(facts) => facts,
            Err(AdapterError::Unsupported | AdapterError::InvalidObservation) => {
                checkpoint = serde_json::from_slice(&before).map_err(|_| AdapterError::InvalidObservation)?;
                Vec::new()
            }
            Err(error) => return Err(error),
        };
        checkpoint.cursor = batch.cursor;
        let bytes =
            serde_json::to_vec(&checkpoint).map_err(|_| AdapterError::InvalidObservation)?;
        Normalization::new(facts, bytes)?.with_context(public_context(event))
    }
}

fn public_context(event: &Event) -> Vec<alondra_store::TaskContextItem> {
    use alondra_store::{ContextSource, TaskContextItem};
    let WireObservation::Sdk { payload, native, .. } = &event.observation else { return vec![]; };
    if native.parent_tool_use_id.is_some() || payload.get("parent_tool_use_id").is_some_and(|parent| !parent.is_null()) { return vec![]; }
    let kind = payload.get("type").and_then(Value::as_str).unwrap_or_default();
    if !matches!(kind, "assistant" | "user") { return vec![]; }
    let Some(content) = payload.pointer("/message/content").and_then(Value::as_array) else { return vec![]; };
    let identity = payload.get("uuid").and_then(Value::as_str).or(native.message_id.as_deref()).map(str::to_owned).unwrap_or_else(|| format!("cursor-{}", event.cursor));
    let mut items = vec![];
    for (ordinal, block) in content.iter().enumerate() {
        let native_id = alondra_store::native_context_id(&format!("{identity}/{ordinal}"));
        let item = match (kind, block.get("type").and_then(Value::as_str)) {
            ("assistant", Some("text")) => block.get("text").and_then(Value::as_str).filter(|text| !text.is_empty())
                .and_then(|text| TaskContextItem::text(native_id, ContextSource::Assistant, text).ok()),
            ("user", Some("tool_result")) => {
                let text = match block.get("content") {
                    Some(Value::String(text)) => Some(text.clone()),
                    Some(Value::Array(blocks)) => {
                        let text = blocks.iter().filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                            .filter_map(|part| part.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n");
                        (!text.is_empty()).then_some(text)
                    }
                    _ => None,
                };
                text.and_then(|text| TaskContextItem::tool_result(native_id, text, None, None).ok())
            }
            _ => None,
        };
        if let Some(item) = item { items.push(item); }
    }
    items
}
/// What a host leaves behind as it ends: its Claude's exit, and the end of the SDK iterator that
/// read it (closed by the host, or cut by the exit).
fn ended_with_its_host(observation: &WireObservation) -> bool {
    match observation {
        WireObservation::ProcessExited { .. } => true,
        WireObservation::Sdk { kind: SdkEventKind::Diagnostic, failures, .. } => {
            !failures.is_empty()
                && failures.iter().all(|failure| {
                    matches!(failure.surface, alondra_agent_protocol::FailureSurface::Control | alondra_agent_protocol::FailureSurface::Iterator)
                })
        }
        _ => false,
    }
}
fn fold(
    event: &Event,
    session: &SessionRecord,
    state: &mut Checkpoint,
    lookup: impl FnOnce(&[MessageUuid]) -> Result<Vec<alondra_foundation::InputId>, AdapterError>,
) -> Result<Vec<Observation>, AdapterError> {
    match &event.observation {
        WireObservation::DecisionRequested { request_id, tool_use_id, kind, payload } => {
            if state.asked.len() >= MAX_ASKED || state.asked.values().any(|open| *open == request_id.0) {
                return Err(AdapterError::InvalidObservation);
            }
            state.asked.insert(event.cursor, request_id.0.clone());
            return Ok(vec![Observation::Asked {
                request: request_id.0.clone(),
                epoch: event.cursor,
                kind: match kind {
                    DecisionKind::Permission => alondra_provider_contract::DecisionKind::Permission,
                    DecisionKind::Question => alondra_provider_contract::DecisionKind::Question,
                    DecisionKind::Plan => alondra_provider_contract::DecisionKind::Plan,
                    DecisionKind::Elicitation => alondra_provider_contract::DecisionKind::Elicitation,
                },
                tool_use: tool_use_id.clone(),
                payload: serde_json::to_vec(payload).map_err(|_| AdapterError::InvalidObservation)?,
            }]);
        }
        WireObservation::DecisionSettled { request_id, epoch, outcome } => {
            if state.asked.get(epoch) != Some(&request_id.0) {
                return Err(AdapterError::InvalidObservation);
            }
            state.asked.remove(epoch);
            return Ok(vec![Observation::DecisionSettled {
                epoch: *epoch,
                answered: *outcome == DecisionOutcome::Answered,
                continued: state.turn.is_some() || !state.work.is_empty(),
            }]);
        }
        _ => {}
    }
    let WireObservation::Sdk {
        kind,
        native,
        payload,
        failures,
    } = &event.observation
    else {
        return Err(AdapterError::Unsupported);
    };
    if *kind == SdkEventKind::Diagnostic
        && payload.get("source").and_then(Value::as_str) == Some("stderr")
    {
        let object = payload
            .as_object()
            .ok_or(AdapterError::InvalidObservation)?;
        if object.len() != 3
            || object.get("text").and_then(Value::as_str).is_none()
            || object.get("truncated_at_bytes").and_then(Value::as_u64) != Some(16_384)
            || native != &NativeCorrelation::default()
            || !failures.is_empty()
        {
            return Err(AdapterError::InvalidObservation);
        }
        return Ok(Vec::new());
    }
    if *kind == SdkEventKind::ControlResult {
        // What a control answered, validated by the protocol: evidence, and how the MCP servers are.
        if native != &NativeCorrelation::default() || !failures.is_empty() {
            return Err(AdapterError::InvalidObservation);
        }
        let result = ControlResult::deserialize(payload).map_err(|_| AdapterError::InvalidObservation)?;
        return Ok(vec![match result {
            ControlResult::McpStatus { servers, .. } | ControlResult::McpReconnect { servers, .. } | ControlResult::McpClearAuth { servers, .. } => Observation::McpServers {
                servers: servers.into_iter().map(report).collect(),
            },
            ControlResult::McpAuthenticate { server, sign_in, .. } => Observation::McpSignIn {
                server,
                sign_in: match sign_in {
                    SignIn::Browser { url, callback_port, state } => McpSignInReport::Browser { url, callback_port, state },
                    SignIn::Connected {} => McpSignInReport::Connected,
                    SignIn::Refused { error } => McpSignInReport::Refused { error },
                },
            },
        }]);
    }
    if *kind == SdkEventKind::Diagnostic && payload.get("session_id").is_none() {
        // The host's own report, such as a request too large to ask: evidence, no fact.
        return Ok(Vec::new());
    }
    let native_session = payload.get("session_id").and_then(Value::as_str);
    if native_session != Some(session.session_id.as_str()) {
        use std::io::Write as _;
        let _ = writeln!(
            std::io::stderr(),
            "alondrad: an SDK message of Claude session {} is kept as evidence only; this host runs session {}",
            native_session.map_or("(none)".into(), |id| id.chars().take(64).collect::<String>()),
            session.session_id
        );
        return Ok(Vec::new());
    }
    let kind = string(payload, "type")?;
    let mut facts = Vec::new();
    if kind == "command_lifecycle" {
        // The native Claude's trace of an input. One of ours it refuses, discards, cancels or
        // completes without having started it is one no turn reads: completed unstarted, it is
        // one its transcript already held. Another's command is evidence.
        let Some(command) = payload.get("command_uuid").and_then(Value::as_str).and_then(|id| MessageUuid::new(id).ok()) else {
            return Ok(facts);
        };
        match payload.get("state").and_then(Value::as_str) {
            Some("started") => match lookup(std::slice::from_ref(&command)) {
                Ok(mut inputs) => {
                    take(state, command.clone());
                    if state.turn.is_none() && let Some(running) = state.running.take() {
                        if !state.initialized {
                            state.initialized = true;
                            facts.push(Observation::Available);
                        }
                        state.woken.clear();
                        let logical = format!("claude-run:{running}");
                        facts.push(Observation::TurnStarted { input: inputs.remove(0), turn: TurnId::new(logical.clone()).map_err(|_| AdapterError::InvalidObservation)? });
                        state.turn = Some(LogicalTurn {
                            id: logical,
                            consumed: BTreeSet::from([command]),
                            answered: false,
                            error: None,
                            resets_at: None,
                            woken_by: None,
                            notified: false,
                            unprompted: false,
                            awaits_init: true,
                        });
                    }
                }
                Err(AdapterError::UnknownInput) => {}
                Err(error) => return Err(error),
            },
            Some("queued") => match lookup(std::slice::from_ref(&command)) {
                Ok(mut inputs) => facts.push(Observation::InputHeld { input: inputs.remove(0) }),
                Err(AdapterError::UnknownInput) => {}
                Err(error) => return Err(error),
            },
            Some("completed" | "cancelled" | "discarded" | "refused") => {
                if let Some(index) = state.taken.iter().position(|taken| *taken == command) {
                    state.taken.remove(index);
                    remember(&mut state.finished, command);
                } else if state.initialized {
                    match lookup(std::slice::from_ref(&command)) {
                        Ok(mut inputs) => facts.push(Observation::InputDropped { input: inputs.remove(0) }),
                        Err(AdapterError::UnknownInput) => {}
                        Err(error) => return Err(error),
                    }
                }
            }
            _ => {}
        }
        return Ok(facts);
    }
    if kind == "system" && payload.get("subtype").and_then(Value::as_str) == Some("init") {
        if let Some(turn) = state.turn.as_mut().filter(|turn| turn.awaits_init) {
            turn.awaits_init = false;
        } else if state.initialized {
            // The native Claude opens each prompt with init: a turn still open ended without a
            // result to read.
            if let Some(turn) = state.turn.take() {
                facts.push(Observation::TurnEnded {
                    turn: TurnId::new(turn.id).map_err(|_| AdapterError::InvalidObservation)?,
                    outcome: match turn.error {
                        Some(error) => TurnOutcome::ApiFailed(ApiFailure { error, status: None, resets_at: turn.resets_at.or(state.limit_resets_at) }),
                        None => TurnOutcome::Failed { code: TURN_SUPERSEDED.into() },
                    },
                });
                state.limit_resets_at = None;
            }
            // A prompt no input of ours started and no work woke: Claude goes on by itself, as
            // when a subagent hands its report back after the last turn ended.
            if state.taken.is_empty() && state.woken.is_empty() {
                state.running = None;
                let logical = format!("claude-message:{}", native_uuid(payload, "uuid")?.as_str());
                facts.push(Observation::TurnUnprompted { turn: TurnId::new(logical.clone()).map_err(|_| AdapterError::InvalidObservation)? });
                state.turn = Some(LogicalTurn {
                    id: logical,
                    consumed: BTreeSet::new(),
                    answered: false,
                    error: None,
                    resets_at: None,
                    woken_by: None,
                    notified: false,
                    unprompted: true,
                    awaits_init: false,
                });
            }
        } else {
            state.initialized = true;
            facts.push(Observation::Available);
        }
        facts.extend(listed_servers(payload));
        return Ok(facts);
    }
    if kind == "system" && payload.get("subtype").and_then(Value::as_str) == Some("session_state_changed") {
        match payload.get("state").and_then(Value::as_str) {
            Some("running") if state.turn.is_none() => state.running = Some(native_uuid(payload, "uuid")?.as_str().to_owned()),
            // Claude Code's own word that the turn is over, after its result and once no
            // subagent of the turn runs: nothing runs but the background work it lists.
            Some("idle") => {
                state.running = None;
                if let Some(turn) = state.turn.take() {
                    facts.push(Observation::TurnEnded {
                        turn: TurnId::new(turn.id).map_err(|_| AdapterError::InvalidObservation)?,
                        outcome: match turn.error {
                            Some(error) => TurnOutcome::ApiFailed(ApiFailure { error, status: None, resets_at: turn.resets_at.or(state.limit_resets_at) }),
                            None => TurnOutcome::Failed { code: ENDED_WITHOUT_RESULT.into() },
                        },
                    });
                    state.limit_resets_at = None;
                }
                let listed = &state.listed;
                state.work.retain(|task, _| listed.contains(task));
                if state.initialized {
                    facts.push(Observation::Reconciled { idle: true, live: live_work(state)? });
                }
            }
            _ => {}
        }
        return Ok(facts);
    }
    if kind == ACCOUNT_REPORT {
        // The host reports the account Claude signed in with as the session initializes.
        if let Some(plan) = account_plan(payload) {
            facts.push(Observation::Usage(UsageReport { windows: vec![], plan: Some(plan) }));
        }
        return Ok(facts);
    }
    if kind == "system"
        && matches!(
            payload.get("subtype").and_then(Value::as_str),
            Some("hook_started" | "hook_progress" | "hook_response")
        )
    {
        // The hooks of the user's and Alondra's plugins: SessionStart's run before init.
        return Ok(facts);
    }
    if !state.initialized {
        // Only an input of ours consumed proves Claude is up; anything else before init is evidence.
        let consumes = matches!(kind, "assistant" | "result")
            && payload.get("user_message_uuids").is_some()
            && payload.get("parent_tool_use_id").is_none_or(Value::is_null);
        if !consumes {
            return Ok(facts);
        }
        state.initialized = true;
        facts.push(Observation::Available);
    }
    if kind == "system" {
        match payload.get("subtype").and_then(Value::as_str) {
            Some("task_started") => {
                let task = string(payload, "task_id")?;
                let turn = state.turn.as_ref().ok_or(AdapterError::Unsupported)?;
                let run = optional_string(payload, "run_id")?;
                if state.work.contains_key(task)
                    || (state.seen_tasks.contains(task) && run.is_none())
                {
                    return Err(AdapterError::Unsupported);
                }
                let id = format!("claude-task:{}", native_uuid(payload, "uuid")?.as_str());
                state.seen_tasks.insert(task.into());
                state.work.insert(
                    task.into(),
                    LogicalWork {
                        run,
                        id: id.clone(),
                        description: payload.get("description").and_then(Value::as_str).map(|text| text.chars().take(DESCRIPTION_LIMIT).collect()),
                        background: payload.get("is_backgrounded").and_then(Value::as_bool) == Some(true) || state.listed.contains(task),
                    },
                );
                facts.push(Observation::WorkStarted {
                    turn: TurnId::new(turn.id.clone())
                        .map_err(|_| AdapterError::InvalidObservation)?,
                    work: WorkId::new(id).map_err(|_| AdapterError::InvalidObservation)?,
                });
            }
            Some("task_notification") => {
                let task = string(payload, "task_id")?;
                if !matches!(
                    payload.get("status").and_then(Value::as_str),
                    Some("completed" | "failed" | "stopped" | "killed")
                ) || payload.get("reason").is_some()
                {
                    return Err(AdapterError::Unsupported);
                }
                let run = optional_string(payload, "run_id")?;
                let work = state.work.get(task).ok_or(AdapterError::Unsupported)?;
                if work.run != run {
                    return Err(AdapterError::Unsupported);
                }
                let work = state.work.remove(task).ok_or(AdapterError::Unsupported)?;
                let task = task.to_owned();
                let basis = native_uuid(payload, "uuid")?;
                let continuation = ContinuationId::new(format!("claude-notification:{}", basis.as_str()))
                    .map_err(|_| AdapterError::InvalidObservation)?;
                facts.push(Observation::WorkEnded {
                    work: WorkId::new(work.id).map_err(|_| AdapterError::InvalidObservation)?,
                    continuation: continuation.clone(),
                });
                // Claude Code tracks a foreground command that runs for seconds as its own work:
                // it ends inside the turn that waits for it, and only work that outlives its turn
                // wakes another.
                if state.turn.is_some() {
                    facts.push(Observation::ContinuationConsumed { continuation });
                } else {
                    state.woken.push(Woken { run, continuation: continuation.as_str().into(), task: Some(task.into()), description: work.description });
                }
            }
            Some("background_tasks_changed") => {
                let tasks = payload.get("tasks").and_then(Value::as_array).ok_or(AdapterError::InvalidObservation)?;
                let listed: BTreeSet<String> = tasks.iter().filter_map(|task| task.get("task_id").and_then(Value::as_str)).map(str::to_owned).collect();
                let gone: Vec<String> = state.work.iter().filter(|(task, work)| work.background && !listed.contains(*task)).map(|(task, _)| task.clone()).collect();
                for (task, work) in &mut state.work {
                    work.background |= listed.contains(task);
                }
                state.listed = listed;
                if state.turn.is_some() {
                    for task in gone {
                        let work = WorkId::new(state.work.remove(&task).ok_or(AdapterError::InvalidObservation)?.id).map_err(|_| AdapterError::InvalidObservation)?;
                        let continuation = reconciled_continuation(&work);
                        facts.push(Observation::WorkEnded { work, continuation: continuation.clone() });
                        facts.push(Observation::ContinuationConsumed { continuation });
                    }
                } else {
                    for task in &gone {
                        state.work.remove(task);
                    }
                    facts.push(Observation::Reconciled { idle: false, live: live_work(state)? });
                }
            }
            Some("conversation_reset") => return Err(AdapterError::Unsupported),
            _ => {}
        }
        return Ok(facts);
    }
    if kind == "rate_limit_event" {
        let info = payload.get("rate_limit_info");
        if info.and_then(|info| info.get("status")).and_then(Value::as_str) == Some("rejected") {
            let resets_at = info.and_then(|info| info.get("resetsAt")).and_then(Value::as_u64);
            match state.turn.as_mut() {
                Some(turn) => turn.resets_at = resets_at,
                None => state.limit_resets_at = resets_at.or(state.limit_resets_at),
            }
        }
        if let Some(window) = info.and_then(rate_limit_window) {
            facts.push(Observation::Usage(UsageReport { windows: vec![window], plan: None }));
        }
        return Ok(facts);
    }
    if kind == "assistant" {
        note_isolated_agents(payload, state);
        note_pr_commands(payload, state);
        note_mcp_calls(payload, state);
    }
    if kind == "user" {
        let failed = mcp_call_failed(payload, state);
        let mut facts = worktree_facts(payload, state);
        facts.extend(pull_request_facts(payload, state));
        if failed {
            facts.push(Observation::McpToolFailed);
        }
        return Ok(facts);
    }
    if kind != "assistant" && kind != "result" && kind != "stream_event" {
        return Ok(facts);
    }
    if payload
        .get("parent_tool_use_id")
        .is_some_and(|value| !value.is_null())
    {
        return Ok(facts);
    }
    if kind == "stream_event" {
        // A partial stream event is not the root assistant/result boundary for our logical turn.
        if payload.get("user_message_uuids").is_some() {
            return Err(AdapterError::Unsupported);
        }
        return Ok(facts);
    }
    // The turn a notification opens takes up every notification waiting. A work that was
    // stopped opens one that only has its result.
    let opens = kind == "assistant" || (kind == "result" && from_notification(payload));
    if opens && state.turn.is_none() && payload.get("user_message_uuids").is_none() && !state.woken.is_empty() {
        let woken = std::mem::take(&mut state.woken).remove(0);
        state.running = None;
        let logical = format!("claude-message:{}", native_uuid(payload, "uuid")?.as_str());
        facts.push(Observation::TurnResumed {
            continuation: ContinuationId::new(woken.continuation).map_err(|_| AdapterError::InvalidObservation)?,
            turn: TurnId::new(logical.clone()).map_err(|_| AdapterError::InvalidObservation)?,
        });
        state.turn = Some(LogicalTurn {
            id: logical,
            consumed: BTreeSet::new(),
            answered: false,
            error: None,
            resets_at: None,
            woken_by: woken.run,
            notified: true,
            unprompted: false,
            awaits_init: false,
        });
    }
    if kind == "result" && state.initialized && let Some(input) = undelivered(payload, failures, state) {
        // A failed result that echoes only an input of ours no turn took in: Claude never reads it.
        let input = lookup(std::slice::from_ref(&input))?.remove(0);
        facts.push(Observation::InputDropped { input });
        return Ok(facts);
    }
    let mut consumed = Vec::new();
    if let Some(list) = payload.get("user_message_uuids") {
        let list = list.as_array().ok_or(AdapterError::InvalidObservation)?;
        if list.is_empty() {
            return Err(AdapterError::InvalidObservation);
        }
        let mut unique = BTreeSet::new();
        for value in list {
            let id = MessageUuid::new(value.as_str().ok_or(AdapterError::InvalidObservation)?)
                .map_err(|_| AdapterError::InvalidObservation)?;
            if !unique.insert(id.clone()) {
                return Err(AdapterError::InvalidObservation);
            }
            consumed.push(id);
        }
        let singular = native_uuid(payload, "user_message_uuid")?;
        if !unique.contains(&singular) {
            return Err(AdapterError::InvalidObservation);
        }
        let inputs = lookup(&consumed)?;
        for id in &consumed {
            take(state, id.clone());
        }
        if state.turn.is_none() {
            state.woken.clear();
            state.running = None;
            let basis = native_uuid(payload, "uuid")?;
            let logical = format!("claude-message:{}", basis.as_str());
            facts.push(Observation::TurnStarted {
                input: inputs[0].clone(),
                turn: TurnId::new(logical.clone()).map_err(|_| AdapterError::InvalidObservation)?,
            });
            state.turn = Some(LogicalTurn {
                id: logical,
                consumed: BTreeSet::from([consumed[0].clone()]),
                answered: false,
                error: None,
                resets_at: None,
                woken_by: None,
                notified: false,
                unprompted: false,
                awaits_init: false,
            });
        }
        let turn = state
            .turn
            .as_mut()
            .ok_or(AdapterError::InvalidObservation)?;
        for (id, input) in consumed.iter().zip(inputs) {
            if turn.consumed.insert(id.clone()) {
                facts.push(Observation::InputConsumed { input });
            }
        }
    }
    if kind == "assistant" {
        if let Some(turn) = state.turn.as_mut() {
            match payload.get("error") {
                // The synthetic message Claude Code ends a turn with once a Claude API error
                // outlasted its own retries.
                Some(error) => turn.error = Some(error_code(error)),
                None if !turn.answered && turn.error.is_none() => {
                    turn.answered = true;
                    facts.push(Observation::ModelAnswered);
                }
                None => {}
            }
        }
    }
    if kind == "result" {
        // A singular failed-delivery echo alone cannot prove input consumption or a turn.
        let turn = state.turn.as_ref().ok_or(AdapterError::Unsupported)?;
        if consumed.is_empty() && !turn.unprompted {
            if turn.notified || turn.woken_by.is_some() {
                let run = payload.get("origin").and_then(|origin| origin.get("runId")).and_then(Value::as_str);
                if !from_notification(payload) || turn.woken_by.as_deref().is_some_and(|woken| run != Some(woken)) {
                    return Err(AdapterError::Unsupported);
                }
            } else {
                let singular = native_uuid(payload, "user_message_uuid")?;
                if !turn.consumed.contains(&singular) {
                    return Err(AdapterError::Unsupported);
                }
            }
        }
        let outcome = match &turn.error {
            Some(error) => TurnOutcome::ApiFailed(ApiFailure {
                error: error.clone(),
                status: payload
                    .get("api_error_status")
                    .and_then(Value::as_u64)
                    .and_then(|status| u16::try_from(status).ok()),
                resets_at: turn.resets_at.or(state.limit_resets_at),
            }),
            None => match failures.iter().find(|failure| ends_turn(failure)) {
                Some(failure) => TurnOutcome::Failed { code: failure.code.clone() },
                None => TurnOutcome::Completed,
            },
        };
        facts.push(Observation::TurnEnded {
            turn: TurnId::new(turn.id.clone()).map_err(|_| AdapterError::InvalidObservation)?,
            outcome,
        });
        state.turn = None;
        state.limit_resets_at = None;
    }
    Ok(facts)
}
fn live_work(state: &Checkpoint) -> Result<BTreeSet<WorkId>, AdapterError> {
    state.work.values().map(|work| WorkId::new(work.id.clone()).map_err(|_| AdapterError::InvalidObservation)).collect()
}
/// Whether the host read `failure` off a result as the end of a failed turn: its startup, its
/// subtype or its terminal reason.
fn ends_turn(failure: &alondra_agent_protocol::SdkFailure) -> bool {
    use alondra_agent_protocol::FailureSurface;
    matches!(failure.surface, FailureSurface::Startup | FailureSurface::Result | FailureSurface::Terminal)
}
/// The input of a failed result that names only it, in its singular echo, when the open turn did
/// not consume it: Claude started it, perhaps, and never delivered it.
fn undelivered(payload: &Value, failures: &[alondra_agent_protocol::SdkFailure], state: &Checkpoint) -> Option<MessageUuid> {
    if payload.get("user_message_uuids").is_some() || !failures.iter().any(ends_turn) {
        return None;
    }
    let input = native_uuid(payload, "user_message_uuid").ok()?;
    let answers_turn = state.turn.as_ref().is_some_and(|turn| turn.notified || turn.woken_by.is_some() || turn.consumed.contains(&input));
    (!answers_turn).then_some(input)
}
/// A result Claude ran because a notification of its background work came.
fn from_notification(payload: &Value) -> bool {
    payload.get("origin").and_then(|origin| origin.get("kind")).and_then(Value::as_str) == Some("task-notification")
}
/// Claude took `input` in: its end, later, is no drop. One it already finished stays finished.
fn take(state: &mut Checkpoint, input: MessageUuid) {
    if state.taken.contains(&input) || state.finished.contains(&input) {
        return;
    }
    remember(&mut state.taken, input);
}
fn remember(inputs: &mut Vec<MessageUuid>, input: MessageUuid) {
    if inputs.len() >= MAX_TAKEN {
        inputs.remove(0);
    }
    inputs.push(input);
}
fn report(server: McpServer) -> McpServerReport {
    let traits = crate::mcp::mcp_traits(server.source.as_deref(), server.transport.as_deref(), &server.status);
    McpServerReport {
        traits,
        name: server.name,
        status: server.status,
        error: server.error,
        source: server.source,
        transport: server.transport,
        url: server.url,
    }
}
fn bounded(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).filter(|text| !text.is_empty() && text.len() <= 256 && !text.contains('\0')).map(str::to_owned)
}
/// The MCP servers init lists, by name, status and source. A server init describes in a way this
/// release cannot keep is left out: what Claude says of its servers never refuses the observation.
fn listed_servers(payload: &Value) -> Option<Observation> {
    let listed = payload.get("mcp_servers")?.as_array()?;
    let servers = listed
        .iter()
        .filter_map(|server| {
            let (name, status, source) = (bounded(server.get("name"))?, bounded(server.get("status"))?, bounded(server.get("source")));
            let traits = crate::mcp::mcp_traits(source.as_deref(), None, &status);
            Some(McpServerReport { name, status, error: None, source, transport: None, url: None, traits })
        })
        .take(MAX_MCP_SERVERS)
        .collect();
    Some(Observation::McpServers { servers })
}
const MAX_MCP_SERVERS: usize = alondra_agent_protocol::MAX_MCP_SERVERS;
/// Remembers each call to an MCP server's tool, of the node or of a subagent, until its result.
fn note_mcp_calls(payload: &Value, state: &mut Checkpoint) {
    let Some(content) = payload.pointer("/message/content").and_then(Value::as_array) else { return };
    for block in content {
        let is_mcp = block.get("type").and_then(Value::as_str) == Some("tool_use")
            && block.get("name").and_then(Value::as_str).is_some_and(|name| name.starts_with("mcp__"));
        if let Some(id) = block.get("id").and_then(Value::as_str).filter(|_| is_mcp) {
            if state.mcp_calls.len() < MAX_MCP_CALLS && id.len() <= 256 {
                state.mcp_calls.insert(id.to_owned());
            }
        }
    }
}
/// Whether a result this message carries is the failure of a remembered MCP call.
fn mcp_call_failed(payload: &Value, state: &mut Checkpoint) -> bool {
    let mut failed = false;
    for block in payload.pointer("/message/content").and_then(Value::as_array).into_iter().flatten() {
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else { continue };
        if state.mcp_calls.remove(id) && block.get("is_error").and_then(Value::as_bool) == Some(true) {
            failed = true;
        }
    }
    failed
}
/// Most Agent calls with an isolated worktree that can wait on their result at once.
const MAX_ISOLATED: usize = 64;
/// Remembers each Agent call, of the node or of a subagent, that asks for an isolated worktree;
/// past the limit, the oldest is forgotten.
fn note_isolated_agents(payload: &Value, state: &mut Checkpoint) {
    let Some(content) = payload.pointer("/message/content").and_then(Value::as_array) else { return };
    for block in content {
        let is_agent = block.get("type").and_then(Value::as_str) == Some("tool_use")
            && matches!(block.get("name").and_then(Value::as_str), Some("Agent" | "Task"));
        if !is_agent || block.pointer("/input/isolation").and_then(Value::as_str) != Some("worktree") {
            continue;
        }
        let Some(id) = block.get("id").and_then(Value::as_str).filter(|id| id.len() <= 256) else { continue };
        if state.isolated.iter().any(|open| open == id) {
            continue;
        }
        if state.isolated.len() >= MAX_ISOLATED {
            state.isolated.remove(0);
        }
        state.isolated.push(id.to_owned());
    }
}
/// Most `gh pr create` commands that can wait on their result at once.
const MAX_PR_COMMANDS: usize = 64;
/// Remembers each shell command, of the node or of a subagent, that runs `gh pr create`.
fn note_pr_commands(payload: &Value, state: &mut Checkpoint) {
    for block in payload.pointer("/message/content").and_then(Value::as_array).into_iter().flatten() {
        let is_shell = block.get("type").and_then(Value::as_str) == Some("tool_use") && block.get("name").and_then(Value::as_str) == Some("Bash");
        let creates = block.pointer("/input/command").and_then(Value::as_str).is_some_and(runs_gh_pr_create);
        if let Some(id) = block.get("id").and_then(Value::as_str).filter(|id| is_shell && creates && id.len() <= 256) {
            if state.pr_commands.len() < MAX_PR_COMMANDS {
                state.pr_commands.insert(id.to_owned());
            }
        }
    }
}
/// What the result of a remembered `gh pr create` says: the pull requests its output names.
fn pull_request_facts(payload: &Value, state: &mut Checkpoint) -> Vec<Observation> {
    let mut facts = Vec::new();
    for block in payload.pointer("/message/content").and_then(Value::as_array).into_iter().flatten() {
        let is_result = block.get("type").and_then(Value::as_str) == Some("tool_result");
        let Some(id) = block.get("tool_use_id").and_then(Value::as_str).filter(|_| is_result) else { continue };
        if !state.pr_commands.remove(id) || block.get("is_error").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let output = match block.get("content") {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Array(parts)) => parts.iter().filter_map(|part| part.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),
            _ => String::new(),
        };
        let named = pull_requests_named(&output);
        if named.is_empty() {
            facts.push(Observation::PullRequestNotNamed);
        }
        facts.extend(named.into_iter().map(Observation::PullRequestOpened));
    }
    facts
}
/// What a tool result says about a worktree: EnterWorktree's and a kept isolated subagent's
/// `worktreePath`, or the id of an isolated subagent that went on in the background, whose
/// worktree outlives this result. ExitWorktree's result says nothing settled: its `remove` can
/// keep the worktree, so only the disk tells whether it is gone.
fn worktree_facts(payload: &Value, state: &mut Checkpoint) -> Vec<Observation> {
    let mut facts = Vec::new();
    let Some(result) = payload.get("tool_use_result").filter(|result| result.is_object()) else { return facts };
    let ids: Vec<&str> = payload.pointer("/message/content").and_then(Value::as_array).into_iter().flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .filter_map(|block| block.get("tool_use_id").and_then(Value::as_str))
        .collect();
    let before = state.isolated.len();
    state.isolated.retain(|open| !ids.contains(&open.as_str()));
    let isolated = state.isolated.len() != before;
    if let Some(path) = result.get("worktreePath").and_then(Value::as_str).filter(|_| result.get("action").is_none()) {
        if path.starts_with('/') && path.len() <= 4096 {
            facts.push(Observation::WorktreeMade {
                path: path.to_owned(),
                branch: optional_string(result, "worktreeBranch").ok().flatten(),
                subagent: result.get("agentId").is_some(),
            });
        }
    } else if isolated && result.get("status").and_then(Value::as_str) == Some("async_launched") {
        let agent = result.get("agentId").and_then(Value::as_str).filter(|agent| {
            (1..=64).contains(&agent.len()) && agent.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        });
        if let Some(agent) = agent {
            facts.push(Observation::SubagentWorktree { agent: agent.to_owned() });
        }
    }
    facts
}
/// The API error Claude Code names; one this release cannot keep is `unknown`.
fn error_code(value: &Value) -> String {
    value
        .as_str()
        .filter(|code| !code.is_empty() && code.len() <= 64 && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
        .unwrap_or("unknown")
        .to_owned()
}
fn native_uuid(payload: &Value, key: &str) -> Result<MessageUuid, AdapterError> {
    MessageUuid::new(string(payload, key)?).map_err(|_| AdapterError::InvalidObservation)
}
fn string<'a>(payload: &'a Value, key: &str) -> Result<&'a str, AdapterError> {
    let value = payload
        .get(key)
        .and_then(Value::as_str)
        .ok_or(AdapterError::InvalidObservation)?;
    if value.is_empty() || value.len() > 256 || value.contains('\0') {
        return Err(AdapterError::InvalidObservation);
    }
    Ok(value)
}
fn storage(error: alondra_store::Error) -> AdapterError {
    match error {
        alondra_store::Error::MissingAdapterInput => AdapterError::UnknownInput,
        error => AdapterError::Storage(error.to_string()),
    }
}

fn optional_string(payload: &Value, key: &str) -> Result<Option<String>, AdapterError> {
    payload
        .get(key)
        .map(|_| string(payload, key).map(str::to_owned))
        .transpose()
}

/// The usage window a `rate_limit_event` describes: the 5-hour window is the session's, the
/// 7-day one the week's, any other kept by its name. A share used is a fraction from 0 to 1; a
/// rejection means the window ran out. An event that names no window, or says nothing of how much
/// was used and refused nothing, reads no window.
fn rate_limit_window(info: &Value) -> Option<UsageWindow> {
    let kind = match info.get("rateLimitType").and_then(Value::as_str)? {
        "five_hour" => UsageWindowKind::Session,
        "seven_day" => UsageWindowKind::Weekly,
        other if !other.is_empty() && other.len() <= MAX_WINDOW_NAME_BYTES && !other.chars().any(char::is_control) => {
            UsageWindowKind::Other(other.to_owned())
        }
        _ => return None,
    };
    let exhausted = info.get("status").and_then(Value::as_str) == Some("rejected");
    let utilization = info.get("utilization").and_then(Value::as_f64).filter(|used| used.is_finite());
    let used = match (utilization, exhausted) {
        (Some(used), _) => used,
        (None, true) => 1.0,
        (None, false) => return None,
    };
    let resets_at = info
        .get("resetsAt")
        .and_then(Value::as_u64)
        .and_then(|seconds| i64::try_from(seconds).ok())
        .and_then(|seconds| seconds.checked_mul(1000));
    Some(UsageWindow { kind, used: (used.clamp(0.0, 1.0) * 10_000.0).round() as u16, resets_at, exhausted })
}

/// The plan in the account the host reported: Claude's `subscriptionType`, such as `max`.
fn account_plan(payload: &Value) -> Option<String> {
    let plan = payload.get("account")?.get("subscriptionType")?.as_str()?.trim();
    (!plan.is_empty() && plan.len() <= MAX_PLAN_BYTES && !plan.chars().any(char::is_control)).then(|| plan.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_agent_protocol::{Execution, NativeCorrelation, SdkEventKind, TaskExecution};
    use alondra_foundation::{AgentId, DataEpoch, InputId, TaskId};
    use serde_json::json;

    const SESSION: &str = "10000000-0000-4000-8000-000000000001";
    const FIRST: &str = "10000000-0000-4000-8000-000000000002";
    const SECOND: &str = "10000000-0000-4000-8000-000000000003";
    const REPLY: &str = "10000000-0000-4000-8000-000000000004";
    const NOTIFIED: &str = "10000000-0000-4000-8000-000000000005";
    const THIRD: &str = "10000000-0000-4000-8000-000000000006";
    const FOURTH: &str = "10000000-0000-4000-8000-000000000007";
    fn event(mut payload: Value) -> Event {
        payload["session_id"] = json!(SESSION);
        Event {
            fence: Fence {
                profile_id: "fixture".into(),
                account_id: "fixture".into(),
                data_epoch: DataEpoch::from("epoch"),
                host_id: "host".into(),
            },
            execution: Execution::Task(TaskExecution {
                task_id: TaskId::from("task"),
                agent_id: AgentId::from("node"),
                conversation_id: "conversation".into(),
                process_instance_id: "process".into(),
                generation: 1,
            }),
            cursor: 1,
            observation: WireObservation::Sdk {
                kind: SdkEventKind::Message,
                native: NativeCorrelation::default(),
                payload,
                failures: vec![],
            },
        }
    }

    #[test]
    fn long_native_message_ids_keep_their_public_content_and_replay_identity() {
        let message = event(json!({"type":"assistant","uuid":"native-id".repeat(1024),"message":{"content":[{"type":"text","text":"The durable public result"}]}}));
        let first = public_context(&message); assert_eq!(first.len(),1); assert_eq!(first[0].content,"The durable public result");
        assert!(first[0].native_id.len()<=4096);assert_eq!(first,public_context(&message));
        let other = event(json!({"type":"assistant","uuid":"other-native-id".repeat(1024),"message":{"content":[{"type":"text","text":"The durable public result"}]}}));
        assert_ne!(first[0].native_id,public_context(&other)[0].native_id);
    }
    #[test]
    fn portable_context_keeps_public_text_and_never_thinking_or_tool_configuration() {
        let message = event(json!({"type":"assistant","uuid":FIRST,"message":{"content":[
            {"type":"text","text":"The public result"},
            {"type":"thinking","thinking":"Private reasoning"},
            {"type":"tool_use","id":"tool","name":"Read","input":{"token":"private-config"}}
        ]}}));
        let context = public_context(&message);
        assert_eq!(context.len(), 1);
        assert_eq!(context[0].content, "The public result");
        assert_eq!(context[0].source, alondra_store::ContextSource::Assistant);
        let child = event(json!({"type":"assistant","parent_tool_use_id":"child","message":{"content":[{"type":"text","text":"Child internals"}]}}));
        assert!(public_context(&child).is_empty());
        let tool = event(json!({"type":"user","uuid":SECOND,"message":{"content":[
            {"type":"tool_result","tool_use_id":"tool","content":[{"type":"text","text":"Command result"},{"type":"thinking","thinking":"Never retain"}]}
        ]}}));
        let context = public_context(&tool);
        assert_eq!(context.len(), 1);
        assert_eq!(context[0].content, "Command result");
        assert_eq!(context[0].source, alondra_store::ContextSource::ToolResult);
    }
    fn session() -> SessionRecord {
        SessionRecord {
            session_id: MessageUuid::new(SESSION).unwrap(),
            continued: false,
        }
    }
    fn state() -> Checkpoint {
        Checkpoint {
            format: CHECKPOINT_FORMAT,
            initialized: true,
            ..Checkpoint::default()
        }
    }
    fn input(ids: &[MessageUuid]) -> Result<Vec<InputId>, AdapterError> {
        Ok(ids
            .iter()
            .map(|id| InputId::new(id.as_str()).unwrap())
            .collect())
    }
    // What the native Claude 2.1.300 sends around each prompt: its lifecycle, from before init,
    // and init again as the next prompt starts. Evidence of the inputs, ours or not, with no fact;
    // init again with no prompt of ours started is a turn Claude opens by itself.
    #[test]
    fn the_native_prompt_lifecycle_records_only_what_it_holds_and_an_init_no_prompt_of_ours_started_opens_claudes_own_turn() {
        let mut state = Checkpoint { format: CHECKPOINT_FORMAT, ..Checkpoint::default() };
        let queued = event(json!({"type":"command_lifecycle","command_uuid":FIRST,"state":"queued","uuid":REPLY}));
        assert_eq!(fold(&queued, &session(), &mut state, input).unwrap(), vec![Observation::InputHeld { input: InputId::new(FIRST).unwrap() }], "queued is Claude holding the input");
        let foreign = |_: &[MessageUuid]| -> Result<Vec<InputId>, AdapterError> { panic!("a lifecycle needs no input of ours") };
        let native = event(json!({"type":"command_lifecycle","command_uuid":"cli-own-command","state":"completed","uuid":REPLY}));
        assert_eq!(fold(&native, &session(), &mut state, foreign).unwrap(), vec![], "Claude's own command is evidence");
        let init = event(json!({"type":"system","subtype":"init","uuid":REPLY}));
        assert_eq!(fold(&init, &session(), &mut state, input).unwrap(), vec![Observation::Available]);
        let started = event(json!({"type":"command_lifecycle","command_uuid":FIRST,"state":"started","uuid":SECOND}));
        assert_eq!(fold(&started, &session(), &mut state, input).unwrap(), vec![]);
        assert_eq!(fold(&init, &session(), &mut state, input).unwrap(), vec![], "init as a prompt of ours starts records nothing");
        let completed = event(json!({"type":"command_lifecycle","command_uuid":FIRST,"state":"completed","uuid":THIRD}));
        fold(&completed, &session(), &mut state, input).unwrap();
        assert_eq!(fold(&init, &session(), &mut state, input).unwrap(), vec![
            Observation::TurnUnprompted { turn: TurnId::new(format!("claude-message:{REPLY}")).unwrap() },
        ], "init with no prompt of ours started opens a turn of Claude's own");
    }
    // Production, 2026-11-07 (t-idle-node-release-processes, observations 1340-1345): after the
    // turn our input opened ended, the native Claude started a command of its own, one Alondra
    // never handed over. Its lifecycle is evidence, and the init that follows opens the turn
    // Claude goes on with by itself.
    #[test]
    fn a_command_the_native_claude_starts_by_itself_is_evidence_and_its_init_opens_claudes_own_turn() {
        const OWN_COMMAND: &str = "10000000-0000-4000-8000-000000000010";
        const OWN_PROMPT: &str = "10000000-0000-4000-8000-000000000011";
        let ours = |ids: &[MessageUuid]| if ids.iter().all(|id| id.as_str() == FIRST) { input(ids) } else { Err(AdapterError::UnknownInput) };
        let mut state = state();
        let started = event(json!({"type":"command_lifecycle","command_uuid":FIRST,"state":"started","uuid":REPLY}));
        assert_eq!(fold(&started, &session(), &mut state, ours).unwrap(), vec![]);
        let answer = event(json!({"type":"assistant","uuid":SECOND,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        fold(&answer, &session(), &mut state, ours).unwrap();
        let result = event(json!({"type":"result","uuid":THIRD,"subtype":"success","user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        let ended = fold(&result, &session(), &mut state, ours).unwrap();
        assert!(matches!(ended.last(), Some(Observation::TurnEnded { outcome: TurnOutcome::Completed, .. })), "{ended:?}");
        let completed = event(json!({"type":"command_lifecycle","command_uuid":FIRST,"state":"completed","uuid":FOURTH}));
        assert_eq!(fold(&completed, &session(), &mut state, ours).unwrap(), vec![]);
        let own = event(json!({"type":"command_lifecycle","command_uuid":OWN_COMMAND,"state":"started","uuid":NOTIFIED}));
        assert_eq!(fold(&own, &session(), &mut state, ours).unwrap(), vec![], "a command Claude starts by itself is evidence");
        let init = event(json!({"type":"system","subtype":"init","uuid":OWN_PROMPT}));
        assert_eq!(fold(&init, &session(), &mut state, ours).unwrap(), vec![
            Observation::TurnUnprompted { turn: TurnId::new(format!("claude-message:{OWN_PROMPT}")).unwrap() },
        ], "its prompt is a turn of Claude's own");
        let finished = event(json!({"type":"command_lifecycle","command_uuid":OWN_COMMAND,"state":"completed","uuid":SESSION}));
        assert_eq!(fold(&finished, &session(), &mut state, ours).unwrap(), vec![], "and its end drops no input of ours");
    }
    // A subagent hands its report back after the turn ended: Claude takes it up in a turn of its
    // own, which its next result ends whatever it names, and a message of ours that arrives
    // meanwhile joins that turn.
    #[test]
    fn a_turn_claude_opens_by_itself_ends_with_its_result_and_takes_in_what_we_send_meanwhile() {
        let mut state = state();
        let init = event(json!({"type":"system","subtype":"init","uuid":REPLY}));
        let own = TurnId::new(format!("claude-message:{REPLY}")).unwrap();
        assert_eq!(fold(&init, &session(), &mut state, input).unwrap(), vec![Observation::TurnUnprompted { turn: own.clone() }]);
        let answer = event(json!({"type":"assistant","uuid":SECOND,"parent_tool_use_id":null}));
        assert_eq!(fold(&answer, &session(), &mut state, |_| panic!("no input of ours opened this turn")).unwrap(), vec![Observation::ModelAnswered]);
        let ours = event(json!({"type":"assistant","uuid":THIRD,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        assert_eq!(fold(&ours, &session(), &mut state, input).unwrap(), vec![Observation::InputConsumed { input: InputId::new(FIRST).unwrap() }]);
        let result = event(json!({"type":"result","subtype":"success","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":"10000000-0000-4000-8000-000000000099"}));
        assert_eq!(fold(&result, &session(), &mut state, input).unwrap(), vec![Observation::TurnEnded { turn: own, outcome: TurnOutcome::Completed }]);
        assert!(state.turn.is_none());
    }
    // An init is the start of a turn: a turn still open when it comes ended without a result to
    // read.
    #[test]
    fn an_init_inside_an_open_turn_ends_that_turn_and_goes_on() {
        let mut state = state();
        let assistant = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        fold(&assistant, &session(), &mut state, input).unwrap();
        let init = event(json!({"type":"system","subtype":"init","uuid":SECOND,"mcp_servers":[]}));
        assert_eq!(fold(&init, &session(), &mut state, input).unwrap(), vec![
            Observation::TurnEnded { turn: TurnId::new(format!("claude-message:{REPLY}")).unwrap(), outcome: TurnOutcome::Failed { code: TURN_SUPERSEDED.into() } },
            Observation::McpServers { servers: vec![] },
        ]);
        assert!(state.turn.is_none());
        let next = event(json!({"type":"assistant","uuid":THIRD,"parent_tool_use_id":null,"user_message_uuid":SECOND,"user_message_uuids":[SECOND]}));
        assert!(matches!(fold(&next, &session(), &mut state, input).unwrap().first(), Some(Observation::TurnStarted { .. })), "the next prompt opens its own turn");

        let mut state = self::state();
        let failed = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST],"error":"server_error"}));
        fold(&failed, &session(), &mut state, input).unwrap();
        assert_eq!(fold(&init, &session(), &mut state, input).unwrap()[0], Observation::TurnEnded {
            turn: TurnId::new(format!("claude-message:{REPLY}")).unwrap(),
            outcome: TurnOutcome::ApiFailed(ApiFailure { error: "server_error".into(), status: None, resets_at: None }),
        });
    }
    // Before init only the consumption of an input of ours proves Claude is up; every other
    // message is evidence, and a decision Claude asks for is asked.
    #[test]
    fn before_init_an_input_consumed_makes_claude_available_and_the_rest_is_evidence() {
        let mut state = Checkpoint { format: CHECKPOINT_FORMAT, ..Checkpoint::default() };
        for payload in [
            json!({"type":"system","subtype":"session_title_changed","title":"Fix","uuid":REPLY}),
            json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}}),
            json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null}),
            json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":"toolu_1","user_message_uuid":FIRST,"user_message_uuids":[FIRST]}),
        ] {
            assert_eq!(fold(&event(payload.clone()), &session(), &mut state, input).unwrap(), vec![], "{payload}");
        }
        assert!(!state.initialized);
        let assistant = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        assert_eq!(fold(&assistant, &session(), &mut state, input).unwrap(), vec![
            Observation::Available,
            Observation::TurnStarted { input: InputId::new(FIRST).unwrap(), turn: TurnId::new(format!("claude-message:{REPLY}")).unwrap() },
            Observation::ModelAnswered,
        ]);
        assert!(state.initialized);
    }
    fn tool_result(id: &str, result: Value) -> Event {
        event(json!({"type":"user","uuid":NOTIFIED,"parent_tool_use_id":null,
            "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":"done"}]},"tool_use_result":result}))
    }
    fn agent_call(id: &str, isolation: Option<&str>) -> Event {
        let mut input = json!({"description":"Fix the parser","prompt":"Fix it","subagent_type":"general-purpose"});
        if let Some(isolation) = isolation {
            input["isolation"] = json!(isolation);
        }
        event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,
            "message":{"role":"assistant","content":[{"type":"tool_use","id":id,"name":"Agent","input":input}]}}))
    }
    fn shell_call(id: &str, command: &str) -> Event {
        event(json!({"type":"assistant","uuid":SECOND,"message":{"content":[{"type":"tool_use","id":id,"name":"Bash","input":{"command":command}}]}}))
    }
    fn shell_result(id: &str, output: &str, is_error: bool) -> Event {
        event(json!({"type":"user","uuid":THIRD,"message":{"content":[{"type":"tool_result","tool_use_id":id,"content":output,"is_error":is_error}]}}))
    }

    #[test]
    fn a_pr_a_command_opened_is_a_fact_of_its_task_in_any_repository() {
        let mut state = state();
        fold(&shell_call("toolu_pr", "cd ../cloud && gh pr create --base main --head fix --title Fix | tail -1"), &session(), &mut state, input).unwrap();
        let facts = fold(&shell_result("toolu_pr", "https://github.com/acme/storefront/pull/6\n", false), &session(), &mut state, input).unwrap();
        assert_eq!(facts, vec![Observation::PullRequestOpened(alondra_provider_contract::OpenedPullRequest { host: "github.com".into(), repository: "acme/storefront".into(), number: 6 })]);
        assert!(state.pr_commands.is_empty(), "the call is settled");

        fold(&shell_call("toolu_hidden", "gh pr create --fill > /dev/null"), &session(), &mut state, input).unwrap();
        assert_eq!(fold(&shell_result("toolu_hidden", "", false), &session(), &mut state, input).unwrap(), vec![Observation::PullRequestNotNamed]);

        fold(&shell_call("toolu_failed", "gh pr create --fill"), &session(), &mut state, input).unwrap();
        let failed = fold(&shell_result("toolu_failed", "a pull request already exists: https://github.com/o/r/pull/1", true), &session(), &mut state, input).unwrap();
        assert_eq!(failed, vec![], "a command that failed opened nothing");

        fold(&shell_call("toolu_view", "gh pr view 12 --json url"), &session(), &mut state, input).unwrap();
        let viewed = fold(&shell_result("toolu_view", "https://github.com/o/r/pull/12", false), &session(), &mut state, input).unwrap();
        assert_eq!(viewed, vec![], "naming a PR is not opening it");
    }

    // The shapes Claude Code's own tools give their results (2.1.291 and the SDK reference): a
    // kept isolated subagent and EnterWorktree name the worktree; a background isolated subagent
    // names only its id; ExitWorktree's `remove` can keep the worktree, so it settles nothing.
    #[test]
    fn worktrees_a_node_or_its_subagents_make_are_facts_of_its_task() {
        let mut state = state();
        fold(&agent_call("toolu_kept", Some("worktree")), &session(), &mut state, input).unwrap();
        let kept = tool_result("toolu_kept", json!({"status":"completed","agentId":"a9e1782746c156d58",
            "worktreePath":"/repo/.claude/worktrees/agent-a9e1782746c156d58","worktreeBranch":"worktree-agent-a9e1782746c156d58"}));
        assert_eq!(fold(&kept, &session(), &mut state, input).unwrap(), vec![Observation::WorktreeMade {
            path: "/repo/.claude/worktrees/agent-a9e1782746c156d58".into(), branch: Some("worktree-agent-a9e1782746c156d58".into()), subagent: true,
        }]);
        assert!(state.isolated.is_empty(), "the call is settled");

        fold(&agent_call("toolu_background", Some("worktree")), &session(), &mut state, input).unwrap();
        let launched = tool_result("toolu_background", json!({"status":"async_launched","agentId":"abd91aef2f770f04e","description":"Fix","prompt":"Fix it","outputFile":"/tmp/out"}));
        assert_eq!(fold(&launched, &session(), &mut state, input).unwrap(), vec![Observation::SubagentWorktree { agent: "abd91aef2f770f04e".into() }]);

        fold(&agent_call("toolu_shared", None), &session(), &mut state, input).unwrap();
        let shared = tool_result("toolu_shared", json!({"status":"async_launched","agentId":"a0000000000000001","description":"Look","prompt":"Look","outputFile":"/tmp/out"}));
        assert_eq!(fold(&shared, &session(), &mut state, input).unwrap(), vec![], "a subagent without an isolated worktree makes none");

        fold(&agent_call("toolu_clean", Some("worktree")), &session(), &mut state, input).unwrap();
        let clean = tool_result("toolu_clean", json!({"status":"completed","agentId":"a0000000000000002","content":[]}));
        assert_eq!(fold(&clean, &session(), &mut state, input).unwrap(), vec![], "Claude Code removed a worktree with no changes");

        let entered = tool_result("toolu_enter", json!({"worktreePath":"/repo/.claude/worktrees/spike","worktreeBranch":"worktree-spike","message":"Created worktree at /repo/.claude/worktrees/spike"}));
        assert_eq!(fold(&entered, &session(), &mut state, input).unwrap(), vec![Observation::WorktreeMade {
            path: "/repo/.claude/worktrees/spike".into(), branch: Some("worktree-spike".into()), subagent: false,
        }]);
        let exited = tool_result("toolu_exit", json!({"action":"remove","worktreePath":"/repo/.claude/worktrees/spike","worktreeBranch":"worktree-spike","message":"Exited worktree but could not remove it"}));
        assert_eq!(fold(&exited, &session(), &mut state, input).unwrap(), vec![]);
        let relative = tool_result("toolu_enter_2", json!({"worktreePath":"spike","message":"Created"}));
        assert_eq!(fold(&relative, &session(), &mut state, input).unwrap(), vec![], "a path Alondra cannot place is evidence");
    }
    // An agent id of another shape names its worktree the same way: only one Alondra could not
    // place in a path loses its fact.
    #[test]
    fn a_subagent_id_of_any_path_safe_shape_names_its_worktree() {
        for (agent, kept) in [("agent_7-Fix", true), (&*"a".repeat(64), true), ("../escape", false), ("", false), (&*"a".repeat(65), false)] {
            let mut state = state();
            fold(&agent_call("toolu_bg", Some("worktree")), &session(), &mut state, input).unwrap();
            let launched = tool_result("toolu_bg", json!({"status":"async_launched","agentId":agent,"outputFile":"/tmp/out"}));
            let facts = fold(&launched, &session(), &mut state, input).unwrap();
            assert_eq!(facts, if kept { vec![Observation::SubagentWorktree { agent: agent.into() }] } else { vec![] }, "{agent}");
            assert!(state.isolated.is_empty(), "the call is settled either way");
        }
    }
    // Past 64 isolated Agent calls waiting on their result, the oldest is forgotten and the
    // message keeps its facts.
    #[test]
    fn past_the_isolated_agent_limit_the_oldest_call_is_forgotten() {
        let mut state = state();
        for n in 0..=MAX_ISOLATED {
            assert_eq!(fold(&agent_call(&format!("toolu_{n}"), Some("worktree")), &session(), &mut state, input).unwrap(), vec![]);
        }
        assert_eq!(state.isolated.len(), MAX_ISOLATED);
        assert_eq!(state.isolated.first().map(String::as_str), Some("toolu_1"));
        let forgotten = tool_result("toolu_0", json!({"status":"async_launched","agentId":"a0000000000000000","outputFile":"/tmp/out"}));
        assert_eq!(fold(&forgotten, &session(), &mut state, input).unwrap(), vec![]);
        let last = tool_result(&format!("toolu_{MAX_ISOLATED}"), json!({"status":"async_launched","agentId":"a0000000000000064","outputFile":"/tmp/out"}));
        assert_eq!(fold(&last, &session(), &mut state, input).unwrap(), vec![Observation::SubagentWorktree { agent: "a0000000000000064".into() }]);
    }
    // A tool result the host had to shrink to frame it keeps the fields the normalizer reads, and
    // says so: its facts are those of the whole message.
    #[test]
    fn a_truncated_tool_result_keeps_its_facts() {
        use alondra_agent_protocol::{FailureSurface, SdkFailure};
        let mut state = state();
        fold(&agent_call("toolu_kept", Some("worktree")), &session(), &mut state, input).unwrap();
        let mut truncated = tool_result("toolu_kept", json!({"status":"completed","agentId":"a9e1782746c156d58",
            "worktreePath":"/repo/.claude/worktrees/agent-a9e1782746c156d58","worktreeBranch":"worktree-agent-a9e1782746c156d58","content":null}));
        let WireObservation::Sdk { failures, .. } = &mut truncated.observation else { unreachable!() };
        failures.push(SdkFailure { surface: FailureSurface::Transport, code: "truncated_sdk_message".into(),
            details: json!({"bytes":5_242_880,"sha256":"0".repeat(64),"reason":"size"}) });
        truncated.validate_for_application().unwrap();
        assert_eq!(fold(&truncated, &session(), &mut state, input).unwrap(), vec![Observation::WorktreeMade {
            path: "/repo/.claude/worktrees/agent-a9e1782746c156d58".into(), branch: Some("worktree-agent-a9e1782746c156d58".into()), subagent: true,
        }]);
    }
    #[test]
    fn result_only_plural_preserves_consumption_and_pending_terminal_boundary() {
        let result = event(
            json!({"type":"result","uuid":REPLY,"subtype":"success","user_message_uuid":SECOND,"user_message_uuids":[FIRST,SECOND]}),
        );
        let mut state = state();
        let facts = fold(&result, &session(), &mut state, input).unwrap();
        assert_eq!(
            facts,
            vec![
                Observation::TurnStarted {
                    input: InputId::new(FIRST).unwrap(),
                    turn: TurnId::new(format!("claude-message:{REPLY}")).unwrap()
                },
                Observation::InputConsumed {
                    input: InputId::new(SECOND).unwrap()
                },
                Observation::TurnEnded {
                    turn: TurnId::new(format!("claude-message:{REPLY}")).unwrap(),
                    outcome: TurnOutcome::Completed,
                }
            ]
        );
        assert!(state.turn.is_none());
    }
    #[test]
    fn failed_delivery_singular_does_not_establish_consumption_or_turn() {
        let result = || event(
            json!({"type":"result","uuid":REPLY,"subtype":"error_during_execution","is_error":true,"num_turns":0,"user_message_uuid":FIRST}),
        );
        let mut state = state();
        assert!(matches!(
            fold(&result(), &session(), &mut state, |_| panic!(
                "singular echo must not query consumption"
            )),
            Err(AdapterError::Unsupported)
        ));
        assert!(state.turn.is_none());
        let started = event(json!({"type":"command_lifecycle","command_uuid":FIRST,"state":"started","uuid":SECOND}));
        fold(&started, &session(), &mut state, input).unwrap();
        let failed = failing(result(), &[(alondra_agent_protocol::FailureSurface::Result, "error_during_execution")]);
        assert_eq!(fold(&failed, &session(), &mut state, input).unwrap(), vec![Observation::InputDropped { input: InputId::new(FIRST).unwrap() }],
            "a failed result that only echoes an input no turn consumed never delivered it");
        assert!(state.turn.is_none());
        let completed = event(json!({"type":"command_lifecycle","command_uuid":FIRST,"state":"completed","uuid":SECOND}));
        assert_eq!(fold(&completed, &session(), &mut state, input).unwrap(), vec![], "it is dropped once");
    }
    #[test]
    fn error_result_plural_still_proves_actual_consumption() {
        let result = event(
            json!({"type":"result","uuid":REPLY,"subtype":"error_during_execution","is_error":true,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}),
        );
        let facts = fold(&result, &session(), &mut state(), input).unwrap();
        assert!(facts.contains(&Observation::TurnStarted {
            input: InputId::new(FIRST).unwrap(),
            turn: TurnId::new(format!("claude-message:{REPLY}")).unwrap(),
        }));
        assert!(!facts.contains(&Observation::InputConsumed {
            input: InputId::new(FIRST).unwrap()
        }));
        assert!(matches!(facts.last(), Some(Observation::TurnEnded { .. })));
    }
    #[test]
    fn repeated_assistant_consumption_does_not_start_another_turn() {
        let assistant = event(
            json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}),
        );
        let mut state = state();
        assert_eq!(
            fold(&assistant, &session(), &mut state, input)
                .unwrap()
                .len(),
            2,
            "the turn starts and the model answered"
        );
        assert!(
            fold(&assistant, &session(), &mut state, input)
                .unwrap()
                .is_empty()
        );
        let result = event(json!({"type":"result","uuid":SECOND,"user_message_uuid":FIRST}));
        assert_eq!(
            fold(&result, &session(), &mut state, input).unwrap(),
            vec![Observation::TurnEnded {
                turn: TurnId::new(format!("claude-message:{REPLY}")).unwrap(),
                outcome: TurnOutcome::Completed,
            }]
        );
    }
    #[test]
    fn restarted_or_wrong_run_notification_cannot_end_work() {
        let mut state = state();
        state.turn = Some(LogicalTurn {
            id: "turn".into(),
            consumed: BTreeSet::new(),
            answered: false,
            error: None,
            resets_at: None,
            woken_by: None,
            notified: false,
            unprompted: false,
            awaits_init: false,
        });
        let started = event(
            json!({"type":"system","subtype":"task_started","uuid":REPLY,"task_id":"task-native","run_id":"run-first"}),
        );
        let facts = fold(&started, &session(), &mut state, input).unwrap();
        assert_eq!(
            facts,
            vec![Observation::WorkStarted {
                turn: TurnId::new("turn").unwrap(),
                work: WorkId::new(format!("claude-task:{REPLY}")).unwrap()
            }]
        );
        for payload in [
            json!({"type":"system","subtype":"task_notification","uuid":SECOND,"task_id":"task-native","run_id":"run-other","status":"completed"}),
            json!({"type":"system","subtype":"task_notification","uuid":SECOND,"task_id":"task-native","run_id":"run-first","status":"stopped","reason":"worker_restart"}),
        ] {
            assert!(matches!(
                fold(&event(payload), &session(), &mut state, input),
                Err(AdapterError::Unsupported)
            ));
            assert!(state.work.contains_key("task-native"));
        }
        let ended = event(
            json!({"type":"system","subtype":"task_notification","uuid":SECOND,"task_id":"task-native","run_id":"run-first","status":"completed"}),
        );
        let continuation = ContinuationId::new(format!("claude-notification:{SECOND}")).unwrap();
        assert_eq!(
            fold(&ended, &session(), &mut state, input).unwrap(),
            vec![
                Observation::WorkEnded { work: WorkId::new(format!("claude-task:{REPLY}")).unwrap(), continuation: continuation.clone() },
                Observation::ContinuationConsumed { continuation },
            ]
        );
        assert!(state.work.is_empty());
    }
    // What the native Claude 2.1.300 sends for a foreground Bash that runs ≈3 s or longer: its own
    // work, not backgrounded, that ends before the turn's result.
    #[test]
    fn foreground_work_that_ends_inside_its_turn_is_consumed_by_that_turn_which_then_ends() {
        let mut state = state();
        let assistant = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        fold(&assistant, &session(), &mut state, input).unwrap();
        let turn = TurnId::new(format!("claude-message:{REPLY}")).unwrap();
        let started = event(json!({"type":"system","subtype":"task_started","uuid":SECOND,"task_id":"bash-1","tool_use_id":"toolu_1","task_type":"local_bash","is_backgrounded":false}));
        let work = WorkId::new(format!("claude-task:{SECOND}")).unwrap();
        assert_eq!(fold(&started, &session(), &mut state, input).unwrap(), vec![Observation::WorkStarted { turn: turn.clone(), work: work.clone() }]);
        let ended = event(json!({"type":"system","subtype":"task_notification","uuid":NOTIFIED,"task_id":"bash-1","tool_use_id":"toolu_1","status":"completed"}));
        let continuation = ContinuationId::new(format!("claude-notification:{NOTIFIED}")).unwrap();
        assert_eq!(fold(&ended, &session(), &mut state, input).unwrap(), vec![
            Observation::WorkEnded { work, continuation: continuation.clone() },
            Observation::ContinuationConsumed { continuation },
        ], "the turn that waited for the command goes on with it");
        let result = event(json!({"type":"result","uuid":SECOND,"user_message_uuid":FIRST}));
        assert_eq!(fold(&result, &session(), &mut state, input).unwrap(), vec![Observation::TurnEnded { turn, outcome: TurnOutcome::Completed }]);
    }
    // Work the turn leaves running ends after the turn's result: its continuation stays, to wake
    // the turn that takes it.
    #[test]
    fn background_work_that_ends_after_its_turn_keeps_its_continuation() {
        let mut state = state();
        let assistant = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        fold(&assistant, &session(), &mut state, input).unwrap();
        let started = event(json!({"type":"system","subtype":"task_started","uuid":SECOND,"task_id":"bash-1","tool_use_id":"toolu_1","task_type":"local_bash","is_backgrounded":true}));
        fold(&started, &session(), &mut state, input).unwrap();
        let result = event(json!({"type":"result","uuid":SECOND,"user_message_uuid":FIRST}));
        fold(&result, &session(), &mut state, input).unwrap();
        let ended = event(json!({"type":"system","subtype":"task_notification","uuid":NOTIFIED,"task_id":"bash-1","tool_use_id":"toolu_1","status":"completed"}));
        assert_eq!(fold(&ended, &session(), &mut state, input).unwrap(), vec![Observation::WorkEnded {
            work: WorkId::new(format!("claude-task:{SECOND}")).unwrap(),
            continuation: ContinuationId::new(format!("claude-notification:{NOTIFIED}")).unwrap(),
        }]);
    }
    // Claude Code ends a background command that reached its time limit and says it was killed:
    // that ends the work like any other end.
    #[test]
    fn background_work_killed_at_its_time_limit_ends() {
        let mut state = state();
        let assistant = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        fold(&assistant, &session(), &mut state, input).unwrap();
        let started = event(json!({"type":"system","subtype":"task_started","uuid":SECOND,"task_id":"bash-1","tool_use_id":"toolu_1","task_type":"local_bash","is_backgrounded":true}));
        fold(&started, &session(), &mut state, input).unwrap();
        fold(&event(json!({"type":"result","uuid":SECOND,"user_message_uuid":FIRST})), &session(), &mut state, input).unwrap();
        let killed = event(json!({"type":"system","subtype":"task_notification","uuid":NOTIFIED,"task_id":"bash-1","tool_use_id":"toolu_1","status":"killed"}));
        assert_eq!(fold(&killed, &session(), &mut state, input).unwrap(), vec![Observation::WorkEnded {
            work: WorkId::new(format!("claude-task:{SECOND}")).unwrap(),
            continuation: ContinuationId::new(format!("claude-notification:{NOTIFIED}")).unwrap(),
        }]);
        assert!(state.work.is_empty());
    }
    // A Monitor watch is a background task of its own (`local_bash`, per the SDK's
    // SDKTaskStartedMessage): the turn that started it ends behind it, and its end wakes the node.
    #[test]
    fn a_monitor_watch_is_work_the_turn_ends_behind() {
        let mut state = state();
        let assistant = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        fold(&assistant, &session(), &mut state, input).unwrap();
        let turn = TurnId::new(format!("claude-message:{REPLY}")).unwrap();
        let started = event(json!({"type":"system","subtype":"task_started","uuid":SECOND,"task_id":"monitor-1","tool_use_id":"toolu_monitor",
            "task_type":"local_bash","description":"gh pr checks --watch","is_backgrounded":true}));
        let work = WorkId::new(format!("claude-task:{SECOND}")).unwrap();
        assert_eq!(fold(&started, &session(), &mut state, input).unwrap(), vec![Observation::WorkStarted { turn: turn.clone(), work: work.clone() }]);
        let result = event(json!({"type":"result","uuid":SECOND,"user_message_uuid":FIRST}));
        assert_eq!(fold(&result, &session(), &mut state, input).unwrap(), vec![Observation::TurnEnded { turn, outcome: TurnOutcome::Completed }]);
        let ended = event(json!({"type":"system","subtype":"task_notification","uuid":NOTIFIED,"task_id":"monitor-1","tool_use_id":"toolu_monitor","status":"completed"}));
        assert_eq!(fold(&ended, &session(), &mut state, input).unwrap(), vec![Observation::WorkEnded {
            work, continuation: ContinuationId::new(format!("claude-notification:{NOTIFIED}")).unwrap(),
        }]);
    }
    /// The messages a real Claude Code 2.1.322 sent with CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS,
    /// recorded through stream-json and stripped of paths and content.
    fn recorded(fixture: &str) -> Vec<Value> {
        let text = match fixture {
            "normal" => include_str!("../fixtures/native/normal.jsonl"),
            "background" => include_str!("../fixtures/native/background.jsonl"),
            "interrupt" => include_str!("../fixtures/native/interrupt.jsonl"),
            _ => unreachable!(),
        };
        text.lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }
    /// Folds each message as the normalizer does: one it cannot read leaves no fact and the
    /// checkpoint as it was.
    fn replay(messages: &[Value]) -> Vec<Vec<Observation>> {
        let mut state = Checkpoint { format: CHECKPOINT_FORMAT, ..Checkpoint::default() };
        messages.iter().map(|message| {
            let before = serde_json::to_vec(&state).unwrap();
            match fold(&event(message.clone()), &session(), &mut state, input) {
                Ok(facts) => facts,
                Err(AdapterError::Unsupported) => {
                    state = serde_json::from_slice(&before).unwrap();
                    vec![]
                }
                Err(error) => panic!("{message}: {error:?}"),
            }
        }).collect()
    }
    fn running_uuids(messages: &[Value]) -> Vec<&str> {
        messages.iter().filter(|message| message["subtype"] == "session_state_changed" && message["state"] == "running").map(|message| message["uuid"].as_str().unwrap()).collect()
    }
    // Two prompts of a recorded session. Each turn starts as Claude Code takes the input the
    // session said it would run, not with the model's first message, which in production came
    // minutes later for one start in ten; that message opens no second turn.
    #[test]
    fn a_turn_starts_when_the_session_says_running() {
        let messages = recorded("normal");
        let facts = replay(&messages);
        let runs = running_uuids(&messages);
        let started: Vec<(usize, &Observation)> = facts.iter().enumerate().flat_map(|(at, facts)| facts.iter().map(move |fact| (at, fact)))
            .filter(|(_, fact)| matches!(fact, Observation::TurnStarted { .. })).collect();
        assert_eq!(started.len(), 2, "{facts:#?}");
        for ((at, fact), run) in started.into_iter().zip(runs) {
            let taken = &messages[at];
            assert_eq!((taken["type"].as_str(), taken["state"].as_str()), (Some("command_lifecycle"), Some("started")));
            assert_eq!(*fact, Observation::TurnStarted {
                input: InputId::new(taken["command_uuid"].as_str().unwrap()).unwrap(),
                turn: TurnId::new(format!("claude-run:{run}")).unwrap(),
            });
        }
        let ended: Vec<&Observation> = facts.iter().flatten().filter(|fact| matches!(fact, Observation::TurnEnded { .. })).collect();
        assert_eq!(ended.len(), 2);
        assert!(ended.iter().all(|fact| matches!(fact, Observation::TurnEnded { outcome: TurnOutcome::Completed, .. })), "{ended:?}");
        let at_idle: Vec<&Vec<Observation>> = facts.iter().zip(&messages).filter(|(_, message)| message["state"] == "idle").map(|(facts, _)| facts).collect();
        assert!(at_idle.iter().all(|facts| **facts == vec![Observation::Reconciled { idle: true, live: BTreeSet::new() }]), "{at_idle:?}");
    }
    // A turn whose result never came ends when the session says it is idle.
    #[test]
    fn idle_without_a_result_ends_the_turn() {
        let messages: Vec<Value> = recorded("normal").into_iter().filter(|message| message["type"] != "result").collect();
        let facts = replay(&messages);
        let runs = running_uuids(&messages);
        let at_idle: Vec<&Vec<Observation>> = facts.iter().zip(&messages).filter(|(_, message)| message["state"] == "idle").map(|(facts, _)| facts).collect();
        assert_eq!(at_idle.len(), 2);
        for (facts, run) in at_idle.into_iter().zip(runs) {
            assert_eq!(*facts, vec![
                Observation::TurnEnded { turn: TurnId::new(format!("claude-run:{run}")).unwrap(), outcome: TurnOutcome::Failed { code: ENDED_WITHOUT_RESULT.into() } },
                Observation::Reconciled { idle: true, live: BTreeSet::new() },
            ]);
        }
    }
    // An interrupted turn ends with its result, and the idle after it ends nothing again.
    #[test]
    fn an_interrupted_turn_ends_once() {
        let messages = recorded("interrupt");
        let facts = replay(&messages);
        assert_eq!(facts.iter().flatten().filter(|fact| matches!(fact, Observation::TurnEnded { .. })).count(), 1, "{facts:#?}");
        assert_eq!(facts.last().unwrap(), &vec![Observation::Reconciled { idle: true, live: BTreeSet::new() }]);
    }
    // A background command outlives its turn; its notification ends it, the empty list that
    // follows finds nothing more to end, and the turn Claude opens to read the notification is
    // its own, though the session said it runs.
    #[test]
    fn a_background_command_ends_with_its_notification_and_its_turn_is_claudes_own() {
        let messages = recorded("background");
        let facts = replay(&messages);
        let all: Vec<&Observation> = facts.iter().flatten().collect();
        assert_eq!(all.iter().filter(|fact| matches!(fact, Observation::TurnStarted { .. })).count(), 1, "{facts:#?}");
        assert_eq!(all.iter().filter(|fact| matches!(fact, Observation::WorkStarted { .. })).count(), 1);
        assert_eq!(all.iter().filter(|fact| matches!(fact, Observation::WorkEnded { continuation, .. } if continuation.as_str().starts_with("claude-notification:"))).count(), 1);
        assert_eq!(all.iter().filter(|fact| matches!(fact, Observation::TurnResumed { .. })).count(), 1);
        assert_eq!(all.iter().filter(|fact| matches!(fact, Observation::TurnEnded { outcome: TurnOutcome::Completed, .. })).count(), 2);
        let emptied = messages.iter().position(|message| message["subtype"] == "background_tasks_changed" && message["tasks"].as_array().unwrap().is_empty()).unwrap();
        assert_eq!(facts[emptied], vec![Observation::Reconciled { idle: false, live: BTreeSet::new() }]);
    }
    // Claude Code lists a background command before its task_started, and keeps listing it while
    // it runs: an idle session still runs it.
    #[test]
    fn an_idle_session_keeps_the_background_work_claude_code_lists() {
        let mut state = state();
        fold(&event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]})), &session(), &mut state, input).unwrap();
        let listed = event(json!({"type":"system","subtype":"background_tasks_changed","uuid":NOTIFIED,"tasks":[{"task_id":"bash-1","task_type":"local_bash","description":"npm run dev"}]}));
        assert_eq!(fold(&listed, &session(), &mut state, input).unwrap(), vec![]);
        fold(&event(json!({"type":"system","subtype":"task_started","uuid":SECOND,"task_id":"bash-1","tool_use_id":"toolu_1","task_type":"local_bash","description":"npm run dev"})), &session(), &mut state, input).unwrap();
        fold(&event(json!({"type":"result","uuid":THIRD,"user_message_uuid":FIRST})), &session(), &mut state, input).unwrap();
        let idle = event(json!({"type":"system","subtype":"session_state_changed","state":"idle","uuid":FOURTH}));
        assert_eq!(fold(&idle, &session(), &mut state, input).unwrap(),
            vec![Observation::Reconciled { idle: true, live: BTreeSet::from([WorkId::new(format!("claude-task:{SECOND}")).unwrap()]) }]);
        assert!(state.work.contains_key("bash-1"));
    }
    // The list is the whole set of live background work: work it no longer names ended, though its
    // notification never came. Inside its turn the turn goes on with it.
    #[test]
    fn work_missing_from_the_full_list_has_ended() {
        let mut state = state();
        fold(&event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]})), &session(), &mut state, input).unwrap();
        fold(&event(json!({"type":"system","subtype":"task_started","uuid":SECOND,"task_id":"bash-1","tool_use_id":"toolu_1","task_type":"local_bash","is_backgrounded":true})), &session(), &mut state, input).unwrap();
        fold(&event(json!({"type":"system","subtype":"task_started","uuid":THIRD,"task_id":"bash-2","tool_use_id":"toolu_2","task_type":"local_bash","is_backgrounded":false})), &session(), &mut state, input).unwrap();
        fold(&event(json!({"type":"system","subtype":"task_started","uuid":FOURTH,"task_id":"bash-3","tool_use_id":"toolu_3","task_type":"local_bash","is_backgrounded":true})), &session(), &mut state, input).unwrap();
        let emptied = event(json!({"type":"system","subtype":"background_tasks_changed","uuid":NOTIFIED,"tasks":[{"task_id":"bash-3","task_type":"local_bash"}]}));
        let work = WorkId::new(format!("claude-task:{SECOND}")).unwrap();
        let continuation = reconciled_continuation(&work);
        assert_eq!(fold(&emptied, &session(), &mut state, input).unwrap(), vec![
            Observation::WorkEnded { work, continuation: continuation.clone() },
            Observation::ContinuationConsumed { continuation },
        ]);
        assert_eq!(state.work.keys().collect::<Vec<_>>(), ["bash-2", "bash-3"], "foreground work is never listed, and the list does not end it");
    }
    // After its turn, the work a list drops is reconciled with what the task's record holds:
    // nothing it holds beyond the live list keeps counting.
    #[test]
    fn background_work_a_list_no_longer_names_after_its_turn_is_reconciled() {
        let mut state = state();
        fold(&event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]})), &session(), &mut state, input).unwrap();
        fold(&event(json!({"type":"system","subtype":"task_started","uuid":SECOND,"task_id":"bash-1","tool_use_id":"toolu_1","task_type":"local_bash","is_backgrounded":true})), &session(), &mut state, input).unwrap();
        fold(&event(json!({"type":"result","uuid":THIRD,"user_message_uuid":FIRST})), &session(), &mut state, input).unwrap();
        let emptied = event(json!({"type":"system","subtype":"background_tasks_changed","uuid":NOTIFIED,"tasks":[]}));
        assert_eq!(fold(&emptied, &session(), &mut state, input).unwrap(), vec![Observation::Reconciled { idle: false, live: BTreeSet::new() }]);
        assert!(state.work.is_empty());
        assert!(matches!(fold(&event(json!({"type":"system","subtype":"task_notification","uuid":FOURTH,"task_id":"bash-1","status":"completed"})), &session(), &mut state, input), Err(AdapterError::Unsupported)),
            "its late notification ends nothing again");
    }
    // What the native Claude 2.1.300 sends when a background subagent ends after its turn: the
    // notification names the run, Claude answers it with no input of Alondra's, and the result
    // names the same run as its origin.
    #[test]
    fn work_that_ends_after_its_turn_wakes_a_turn_that_takes_up_its_continuation() {
        let mut state = state();
        let assistant = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        fold(&assistant, &session(), &mut state, input).unwrap();
        let started = event(json!({"type":"system","subtype":"task_started","uuid":SECOND,"task_id":"agent-1","tool_use_id":"toolu_1","run_id":"run-1"}));
        fold(&started, &session(), &mut state, input).unwrap();
        fold(&event(json!({"type":"result","uuid":SECOND,"user_message_uuid":FIRST})), &session(), &mut state, input).unwrap();
        let ended = event(json!({"type":"system","subtype":"task_notification","uuid":NOTIFIED,"task_id":"agent-1","tool_use_id":"toolu_1","run_id":"run-1","status":"completed"}));
        let continuation = ContinuationId::new(format!("claude-notification:{NOTIFIED}")).unwrap();
        fold(&ended, &session(), &mut state, input).unwrap();
        let woken = event(json!({"type":"assistant","uuid":THIRD,"parent_tool_use_id":null}));
        let turn = TurnId::new(format!("claude-message:{THIRD}")).unwrap();
        assert_eq!(
            fold(&woken, &session(), &mut state, |_| panic!("no input of Alondra's opened this turn")).unwrap(),
            vec![Observation::TurnResumed { continuation, turn: turn.clone() }, Observation::ModelAnswered],
        );
        let mut other = serde_json::from_slice::<Checkpoint>(&serde_json::to_vec(&state).unwrap()).unwrap();
        let elsewhere = event(json!({"type":"result","uuid":FOURTH,"origin":{"kind":"task-notification","runId":"run-2"}}));
        assert!(matches!(fold(&elsewhere, &session(), &mut other, input), Err(AdapterError::Unsupported)), "a result that names another run ends nothing");
        let unnamed = event(json!({"type":"result","uuid":FOURTH}));
        assert!(matches!(fold(&unnamed, &session(), &mut other, input), Err(AdapterError::Unsupported)), "nor one that names none");
        let result = event(json!({"type":"result","uuid":FOURTH,"subtype":"success","origin":{"kind":"task-notification","producer":"session-task","runId":"run-1"}}));
        assert_eq!(fold(&result, &session(), &mut state, input).unwrap(), vec![Observation::TurnEnded { turn, outcome: TurnOutcome::Completed }]);
        assert!(state.turn.is_none() && state.woken.is_empty());
    }
    #[test]
    fn a_control_result_is_how_the_mcp_servers_are_and_carries_no_failure() {
        let mut result = event(json!({"control":"mcp_status","command_id":"lifecycle-control-1","servers":[{"name":"fixture","status":"connected"}]}));
        let WireObservation::Sdk { kind, payload, .. } = &mut result.observation else { unreachable!() };
        *kind = SdkEventKind::ControlResult;
        payload.as_object_mut().unwrap().remove("session_id");
        result.validate_for_application().unwrap();
        let mut state = state();
        let fixture = McpServerReport { name: "fixture".into(), status: "connected".into(), error: None, source: None, transport: None, url: None,
            traits: crate::mcp::mcp_traits(None, None, "connected") };
        assert_eq!(fold(&result, &session(), &mut state, |_| panic!("a control result consumes no input")).unwrap(), vec![Observation::McpServers { servers: vec![fixture] }]);
        let WireObservation::Sdk { failures, .. } = &mut result.observation else { unreachable!() };
        failures.push(alondra_agent_protocol::SdkFailure { surface: alondra_agent_protocol::FailureSurface::Result, code: "error_during_execution".into(), details: json!({}) });
        assert!(matches!(fold(&result, &session(), &mut state, input), Err(AdapterError::InvalidObservation)), "a failed control result is not evidence");
    }
    // How Claude's MCP servers are reaches Alondra as each prompt starts, with nothing Alondra cannot
    // keep; what a reconnection or a sign-in answered is a fact too, and so is the failure of a
    // call to an MCP server's tool, never of another tool.
    #[test]
    fn mcp_servers_sign_ins_and_failed_mcp_calls_are_facts() {
        let mut state = Checkpoint { format: CHECKPOINT_FORMAT, ..Checkpoint::default() };
        let init = event(json!({"type":"system","subtype":"init","uuid":REPLY,"mcp_servers":[
            {"name":"linear","status":"needs-auth","source":"user"},
            {"name":"","status":"connected"},
            {"name":"files","status":"connected"},
            {"status":"connected"},
        ]}));
        let listed = |status: &str| vec![
            McpServerReport { name: "linear".into(), status: status.into(), error: None, source: Some("user".into()), transport: None, url: None,
                traits: crate::mcp::mcp_traits(Some("user"), None, status) },
            McpServerReport { name: "files".into(), status: "connected".into(), error: None, source: None, transport: None, url: None,
                traits: crate::mcp::mcp_traits(None, None, "connected") },
        ];
        assert_eq!(fold(&init, &session(), &mut state, input).unwrap(), vec![Observation::Available, Observation::McpServers { servers: listed("needs-auth") }]);
        let started = event(json!({"type":"command_lifecycle","command_uuid":FIRST,"state":"started","uuid":SECOND}));
        fold(&started, &session(), &mut state, input).unwrap();
        let again = event(json!({"type":"system","subtype":"init","uuid":REPLY,"mcp_servers":[{"name":"linear","status":"connected","source":"user"},{"name":"files","status":"connected"}]}));
        assert_eq!(fold(&again, &session(), &mut state, input).unwrap(), vec![Observation::McpServers { servers: listed("connected") }], "init repeated as a prompt starts");

        let control = |payload: Value| {
            let mut result = event(payload);
            let WireObservation::Sdk { kind, payload, .. } = &mut result.observation else { unreachable!() };
            *kind = SdkEventKind::ControlResult;
            payload.as_object_mut().unwrap().remove("session_id");
            result.validate_for_application().unwrap();
            result
        };
        let reconnected = control(json!({"control":"mcp_reconnect","command_id":"c","server":"linear","error":"Server not found","servers":[{"name":"linear","status":"failed","transport":"http","url":"https://mcp.linear.app/mcp"}]}));
        assert_eq!(fold(&reconnected, &session(), &mut state, input).unwrap(), vec![Observation::McpServers { servers: vec![McpServerReport {
            name: "linear".into(), status: "failed".into(), error: None, source: None, transport: Some("http".into()), url: Some("https://mcp.linear.app/mcp".into()),
            traits: crate::mcp::mcp_traits(None, Some("http"), "failed"),
        }] }]);
        let browser = control(json!({"control":"mcp_authenticate","command_id":"c","server":"linear","sign_in":{"outcome":"browser","url":"https://linear.app/oauth/authorize?state=s","callback_port":49200,"state":"s"}}));
        assert_eq!(fold(&browser, &session(), &mut state, input).unwrap(), vec![Observation::McpSignIn {
            server: "linear".into(),
            sign_in: McpSignInReport::Browser { url: "https://linear.app/oauth/authorize?state=s".into(), callback_port: Some(49200), state: Some("s".into()) },
        }]);
        let connected = control(json!({"control":"mcp_authenticate","command_id":"c","server":"linear","sign_in":{"outcome":"connected"}}));
        assert_eq!(fold(&connected, &session(), &mut state, input).unwrap(), vec![Observation::McpSignIn { server: "linear".into(), sign_in: McpSignInReport::Connected }]);

        let calls = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"message":{"role":"assistant","content":[
            {"type":"tool_use","id":"toolu_mcp","name":"mcp__linear__list_issues","input":{}},
            {"type":"tool_use","id":"toolu_bash","name":"Bash","input":{"command":"false"}},
            {"type":"tool_use","id":"toolu_mcp_ok","name":"mcp__files__read","input":{}},
        ]}}));
        fold(&calls, &session(), &mut state, input).ok();
        assert_eq!(state.mcp_calls, BTreeSet::from(["toolu_mcp".to_owned(), "toolu_mcp_ok".to_owned()]));
        let results = |id: &str, failed: bool| event(json!({"type":"user","uuid":NOTIFIED,"parent_tool_use_id":null,
            "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":id,"is_error":failed,"content":"x"}]}}));
        assert_eq!(fold(&results("toolu_bash", true), &session(), &mut state, input).unwrap(), vec![], "another tool's failure says nothing of MCP");
        assert_eq!(fold(&results("toolu_mcp_ok", false), &session(), &mut state, input).unwrap(), vec![]);
        assert_eq!(fold(&results("toolu_mcp", true), &session(), &mut state, input).unwrap(), vec![Observation::McpToolFailed]);
        assert_eq!(fold(&results("toolu_mcp", true), &session(), &mut state, input).unwrap(), vec![], "a call is traced back once");
        assert!(state.mcp_calls.is_empty());
    }
    // What a `/compact` input produces: the boundary records nothing, and its result ends the
    // turn it consumed like any other input.
    #[test]
    fn a_compaction_turn_records_its_boundary_as_nothing_and_ends_like_any_input_turn() {
        let mut state = state();
        for status in [json!({"type":"system","subtype":"status","status":"compacting","uuid":REPLY}),
            json!({"type":"system","subtype":"compact_boundary","compact_metadata":{"trigger":"manual","pre_tokens":1},"uuid":REPLY})] {
            assert_eq!(fold(&event(status), &session(), &mut state, input).unwrap(), vec![]);
        }
        let result = event(json!({"type":"result","uuid":REPLY,"subtype":"success","is_error":false,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        let turn = TurnId::new(format!("claude-message:{REPLY}")).unwrap();
        assert_eq!(fold(&result, &session(), &mut state, input).unwrap(), vec![
            Observation::TurnStarted { input: InputId::new(FIRST).unwrap(), turn: turn.clone() },
            Observation::TurnEnded { turn, outcome: TurnOutcome::Completed },
        ]);
    }
    fn decision(cursor: u64, observation: WireObservation) -> Event {
        let mut decision = event(json!({}));
        decision.cursor = cursor;
        decision.observation = observation;
        decision
    }
    fn requested(cursor: u64, request: &str) -> Event {
        decision(cursor, WireObservation::DecisionRequested {
            request_id: alondra_foundation::RequestId(request.into()),
            tool_use_id: Some("toolu_1".into()),
            kind: DecisionKind::Permission,
            payload: json!({"tool": "Bash", "input": {"command": "ls"}, "request": {}}),
        })
    }
    fn settled(cursor: u64, request: &str, epoch: u64, outcome: DecisionOutcome) -> Event {
        decision(cursor, WireObservation::DecisionSettled { request_id: alondra_foundation::RequestId(request.into()), epoch, outcome })
    }
    // A decision Claude asks for is asked at the host's cursor of its request, with the payload
    // the host kept; its settlement says whether the turn that asked goes on with it.
    #[test]
    fn a_decision_request_is_asked_at_its_cursor_and_settles_once_with_the_turn_it_continues() {
        let mut state = state();
        let assistant = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        fold(&assistant, &session(), &mut state, input).unwrap();
        let asked = fold(&requested(7, "decision-1"), &session(), &mut state, input).unwrap();
        let [Observation::Asked { request, epoch, kind, tool_use, payload }] = asked.as_slice() else { panic!("{asked:?}") };
        assert_eq!((request.as_str(), *epoch, *kind, tool_use.as_deref()), ("decision-1", 7, alondra_provider_contract::DecisionKind::Permission, Some("toolu_1")));
        assert_eq!(serde_json::from_slice::<Value>(payload).unwrap(), json!({"tool": "Bash", "input": {"command": "ls"}, "request": {}}));
        assert_eq!(fold(&settled(8, "decision-1", 7, DecisionOutcome::Answered), &session(), &mut state, input).unwrap(),
            vec![Observation::DecisionSettled { epoch: 7, answered: true, continued: true }]);
        assert!(state.asked.is_empty());
        assert!(matches!(fold(&settled(9, "decision-1", 7, DecisionOutcome::Answered), &session(), &mut state, input), Err(AdapterError::InvalidObservation)), "settled once");
        state.turn = None;
        fold(&requested(10, "decision-2"), &session(), &mut state, input).unwrap();
        assert_eq!(fold(&settled(11, "decision-2", 10, DecisionOutcome::Cancelled), &session(), &mut state, input).unwrap(),
            vec![Observation::DecisionSettled { epoch: 10, answered: false, continued: false }]);
        state.work.insert("agent-1".into(), LogicalWork { run: Some("run-1".into()), id: "claude-task:agent".into(), description: None, background: false });
        fold(&requested(12, "decision-3"), &session(), &mut state, input).unwrap();
        assert_eq!(fold(&settled(13, "decision-3", 12, DecisionOutcome::Answered), &session(), &mut state, input).unwrap(),
            vec![Observation::DecisionSettled { epoch: 12, answered: true, continued: true }], "the background work that asked goes on with it");
    }
    #[test]
    fn a_settlement_of_an_unknown_epoch_or_another_request_and_a_repeated_request_are_refused() {
        let mut state = state();
        fold(&requested(3, "decision-1"), &session(), &mut state, input).unwrap();
        assert!(matches!(fold(&requested(4, "decision-1"), &session(), &mut state, input), Err(AdapterError::InvalidObservation)), "the same request twice");
        assert!(matches!(fold(&settled(5, "decision-1", 4, DecisionOutcome::Answered), &session(), &mut state, input), Err(AdapterError::InvalidObservation)), "an epoch nobody asked at");
        assert!(matches!(fold(&settled(5, "decision-9", 3, DecisionOutcome::Answered), &session(), &mut state, input), Err(AdapterError::InvalidObservation)), "another request at that epoch");
        assert_eq!(state.asked, BTreeMap::from([(3, "decision-1".to_owned())]));
    }
    #[test]
    fn a_decision_claude_asks_for_before_init_is_asked() {
        let mut fresh = Checkpoint { format: CHECKPOINT_FORMAT, ..Checkpoint::default() };
        assert!(matches!(fold(&requested(1, "decision-1"), &session(), &mut fresh, input).unwrap().as_slice(), [Observation::Asked { epoch: 1, .. }]));
        assert_eq!(fresh.asked, BTreeMap::from([(1, "decision-1".to_owned())]));
    }
    #[test]
    fn provider_owned_retry_and_allowed_rate_limit_do_not_end_business_execution() {
        for payload in [
            json!({"type":"system","subtype":"api_retry","attempt":1,"error_status":401}),
            json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning"}}),
        ] {
            assert!(
                fold(&event(payload), &session(), &mut state(), input)
                    .unwrap()
                    .is_empty()
            );
        }
    }
    // The rate_limit_event Claude Code 2.1.300 writes from its usage headers: `utilization` is the
    // window's share used from 0 to 1, `resetsAt` Unix seconds.
    #[test]
    fn a_rate_limit_event_reads_the_window_it_names_for_the_bindings_account() {
        let window = |info: Value| fold(&event(json!({"type":"rate_limit_event","uuid":SECOND,"rate_limit_info":info})), &session(), &mut state(), input).unwrap();
        assert_eq!(window(json!({"status":"allowed_warning","unifiedRateLimitFallbackAvailable":false,"resetsAt":1_790_202_000_u64,"rateLimitType":"five_hour","utilization":0.82,"surpassedThreshold":0.8})),
            [Observation::Usage(UsageReport { windows: vec![UsageWindow { kind: UsageWindowKind::Session, used: 8_200, resets_at: Some(1_790_202_000_000), exhausted: false }], plan: None })]);
        assert_eq!(window(json!({"status":"allowed","resetsAt":1_790_553_600_u64,"rateLimitType":"seven_day","utilization":0.53,"isUsingOverage":false})),
            [Observation::Usage(UsageReport { windows: vec![UsageWindow { kind: UsageWindowKind::Weekly, used: 5_300, resets_at: Some(1_790_553_600_000), exhausted: false }], plan: None })]);
        assert_eq!(window(json!({"status":"rejected","resetsAt":1_790_553_600_u64,"rateLimitType":"seven_day_opus","overageStatus":"rejected","overageDisabledReason":"out_of_credits"})),
            [Observation::Usage(UsageReport { windows: vec![UsageWindow { kind: UsageWindowKind::Other("seven_day_opus".into()), used: 10_000, resets_at: Some(1_790_553_600_000), exhausted: true }], plan: None })],
            "a refusal without a share is the window used up");
        assert!(window(json!({"status":"allowed","rateLimitType":"five_hour"})).is_empty(), "nothing said of how much was used");
        assert!(window(json!({"status":"rejected"})).is_empty(), "no window named");
    }
    #[test]
    fn a_rejected_limit_reads_its_window_used_up_and_still_ends_the_turn_with_its_reset() {
        let facts = turn_end(&mut state(), vec![
            json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST],"error":"rate_limit"}),
            json!({"type":"rate_limit_event","uuid":SECOND,"rate_limit_info":{"status":"rejected","resetsAt":1_790_202_000_u64,"rateLimitType":"five_hour","utilization":1.0}}),
            json!({"type":"result","uuid":SECOND,"is_error":true,"api_error_status":429,"terminal_reason":"blocking_limit","user_message_uuid":FIRST}),
        ]);
        assert!(facts.contains(&Observation::Usage(UsageReport {
            windows: vec![UsageWindow { kind: UsageWindowKind::Session, used: 10_000, resets_at: Some(1_790_202_000_000), exhausted: true }], plan: None,
        })), "{facts:?}");
        assert_eq!(failure(&facts).and_then(|failure| failure.resets_at), Some(1_790_202_000));
    }
    #[test]
    fn the_account_the_host_reports_as_claude_initializes_says_its_plan() {
        let account = |payload: Value| fold(&event(payload), &session(), &mut state(), input).unwrap();
        assert_eq!(account(json!({"type":"account","account":{"subscriptionType":"max"}})),
            [Observation::Usage(UsageReport { windows: vec![], plan: Some("max".into()) })]);
        assert!(account(json!({"type":"account","account":{}})).is_empty(), "an API key has no plan");
        assert!(account(json!({"type":"account","account":{"subscriptionType":" "}})).is_empty());
    }
    fn turn_end(state: &mut Checkpoint, payloads: Vec<Value>) -> Vec<Observation> {
        payloads
            .into_iter()
            .flat_map(|payload| fold(&event(payload), &session(), state, input).unwrap())
            .collect()
    }
    fn failure(facts: &[Observation]) -> Option<ApiFailure> {
        match facts.last() {
            Some(Observation::TurnEnded { outcome: TurnOutcome::ApiFailed(failure), .. }) => Some(failure.clone()),
            Some(Observation::TurnEnded { .. }) => None,
            other => panic!("the turn did not end: {other:?}"),
        }
    }
    // Claude Code 2.1.300 retries a 401 itself and says so with api_retry, also between two
    // requests of one turn; the turn then goes on with an answer of the model. A retry is never a
    // logout.
    #[test]
    fn a_retried_authentication_error_that_the_model_then_answers_ends_the_turn_without_failure() {
        let mut state = state();
        let facts = turn_end(&mut state, vec![
            json!({"type":"system","subtype":"api_retry","attempt":1,"max_retries":10,"retry_delay_ms":500,"error_status":401,"error":"authentication_failed","uuid":SECOND}),
            json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}),
            json!({"type":"system","subtype":"api_retry","attempt":1,"max_retries":10,"retry_delay_ms":500,"error_status":401,"error":"authentication_failed","uuid":SECOND}),
            json!({"type":"assistant","uuid":SECOND,"parent_tool_use_id":null}),
            json!({"type":"result","uuid":SECOND,"subtype":"success","is_error":false,"user_message_uuid":FIRST}),
        ]);
        assert!(facts.contains(&Observation::ModelAnswered), "{facts:?}");
        assert_eq!(failure(&facts), None);
    }
    // The synthetic message Claude Code ends a turn with when the API refused its sign-in.
    #[test]
    fn a_turn_ended_by_an_authentication_error_carries_it_and_proves_no_answer() {
        let mut state = state();
        let facts = turn_end(&mut state, vec![
            json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST],"error":"authentication_failed"}),
            json!({"type":"result","uuid":SECOND,"subtype":"success","is_error":true,"api_error_status":401,"terminal_reason":"api_error","user_message_uuid":FIRST}),
        ]);
        assert!(!facts.contains(&Observation::ModelAnswered), "{facts:?}");
        assert_eq!(failure(&facts), Some(ApiFailure { error: "authentication_failed".into(), status: Some(401), resets_at: None }));
    }
    #[test]
    fn a_forbidden_request_keeps_its_status_and_a_rejected_limit_its_reset() {
        let forbidden = turn_end(&mut state(), vec![
            json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST],"error":"authentication_failed"}),
            json!({"type":"result","uuid":SECOND,"is_error":true,"api_error_status":403,"user_message_uuid":FIRST}),
        ]);
        assert_eq!(failure(&forbidden).and_then(|failure| failure.status), Some(403));
        let limited = turn_end(&mut state(), vec![
            json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST],"error":"rate_limit"}),
            json!({"type":"rate_limit_event","uuid":SECOND,"rate_limit_info":{"status":"rejected","resetsAt":4_102_444_800_u64,"rateLimitType":"five_hour"}}),
            json!({"type":"result","uuid":SECOND,"is_error":true,"api_error_status":429,"terminal_reason":"blocking_limit","user_message_uuid":FIRST}),
        ]);
        assert_eq!(failure(&limited), Some(ApiFailure { error: "rate_limit".into(), status: Some(429), resets_at: Some(4_102_444_800) }));
    }
    // Claude Code 2.1.300 always reports SessionStart hooks, before init: a plugin with hooks,
    // the user's or Alondra's, must not stop the task.
    #[test]
    fn hooks_reported_before_init_are_evidence_without_facts() {
        let mut state = Checkpoint { format: CHECKPOINT_FORMAT, ..Checkpoint::default() };
        for subtype in ["hook_started", "hook_progress", "hook_response"] {
            let hook = event(json!({"type":"system","subtype":subtype,"hook_id":"h","hook_name":"SessionStart:startup","hook_event":"SessionStart","uuid":REPLY}));
            assert_eq!(fold(&hook, &session(), &mut state, input).unwrap(), vec![], "{subtype}");
        }
        let init = event(json!({"type":"system","subtype":"init","uuid":REPLY}));
        assert_eq!(fold(&init, &session(), &mut state, input).unwrap(), vec![Observation::Available]);
    }
    // An error code or status of a shape Alondra does not know still ends the turn as failed.
    #[test]
    fn an_error_code_or_status_alondra_cannot_read_still_fails_the_turn() {
        let facts = turn_end(&mut state(), vec![
            json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST],"error":"Not Logged In"}),
            json!({"type":"result","uuid":SECOND,"is_error":true,"api_error_status":"401","user_message_uuid":FIRST}),
        ]);
        assert_eq!(failure(&facts), Some(ApiFailure { error: "unknown".into(), status: None, resets_at: None }));
    }
    // A subagent's message that names inputs is evidence: only root messages consume ours.
    #[test]
    fn a_subagent_message_naming_inputs_is_evidence() {
        let mut state = state();
        let subagent = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":"toolu_1","user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        assert_eq!(fold(&subagent, &session(), &mut state, |_| panic!("a subagent consumes no input of ours")).unwrap(), vec![]);
        assert!(state.turn.is_none());
    }
    // More than 64 inputs of ours consumed in one turn are each consumed.
    #[test]
    fn more_than_sixty_four_inputs_consumed_are_each_recorded() {
        let ids: Vec<String> = (0..80).map(|n| format!("20000000-0000-4000-8000-{n:012}")).collect();
        let mut state = state();
        let first = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":ids[0],"user_message_uuids":ids[..70]}));
        let facts = fold(&first, &session(), &mut state, input).unwrap();
        assert_eq!(facts.iter().filter(|fact| matches!(fact, Observation::InputConsumed { .. })).count(), 69, "the first starts the turn");
        let rest = event(json!({"type":"assistant","uuid":SECOND,"parent_tool_use_id":null,"user_message_uuid":ids[70],"user_message_uuids":ids[60..]}));
        let facts = fold(&rest, &session(), &mut state, input).unwrap();
        assert_eq!(facts.iter().filter(|fact| matches!(fact, Observation::InputConsumed { .. })).count(), 10);
        assert_eq!(state.turn.as_ref().map(|turn| turn.consumed.len()), Some(80));
    }
    // Corruption is still refused: an input of ours the store cannot map back.
    #[test]
    fn an_input_of_ours_mapped_wrong_is_still_refused() {
        let assistant = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        let unmapped = |_: &[MessageUuid]| Err(AdapterError::InvalidObservation);
        assert!(matches!(fold(&assistant, &session(), &mut state(), unmapped), Err(AdapterError::InvalidObservation)));
    }
    fn failing(mut event: Event, failures: &[(alondra_agent_protocol::FailureSurface, &str)]) -> Event {
        if let WireObservation::Sdk { failures: carried, .. } = &mut event.observation {
            *carried = failures.iter().map(|(surface, code)| alondra_agent_protocol::SdkFailure { surface: *surface, code: (*code).into(), details: json!({}) }).collect();
        }
        event
    }
    fn ended(state: &mut Checkpoint, result: Event) -> TurnOutcome {
        let assistant = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
        fold(&assistant, &session(), state, input).unwrap();
        match fold(&result, &session(), state, input).unwrap().last() {
            Some(Observation::TurnEnded { outcome, .. }) => outcome.clone(),
            other => panic!("the turn did not end: {other:?}"),
        }
    }
    // A turn whose result is not a success ends as failed, with the result's code, known or not.
    #[test]
    fn a_result_claude_ended_as_failed_ends_its_turn_failed_with_its_code() {
        use alondra_agent_protocol::FailureSurface::{Result as Subtype, Startup, Terminal};
        let result = || event(json!({"type":"result","uuid":SECOND,"user_message_uuid":FIRST,"subtype":"error_max_turns","is_error":true}));
        assert_eq!(ended(&mut state(), failing(result(), &[(Subtype, "error_max_turns")])), TurnOutcome::Failed { code: "error_max_turns".into() });
        assert_eq!(ended(&mut state(), failing(result(), &[(Subtype, "error_from_a_later_claude")])), TurnOutcome::Failed { code: "error_from_a_later_claude".into() });
        assert_eq!(ended(&mut state(), failing(result(), &[(Startup, "cwd_unavailable"), (Subtype, "error_during_execution")])), TurnOutcome::Failed { code: "cwd_unavailable".into() });
        assert_eq!(ended(&mut state(), failing(result(), &[(Terminal, "prompt_too_long")])), TurnOutcome::Failed { code: "prompt_too_long".into() });
        assert_eq!(ended(&mut state(), event(json!({"type":"result","uuid":SECOND,"user_message_uuid":FIRST,"subtype":"success","is_error":false}))), TurnOutcome::Completed);

        let mut state = state();
        let refused = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST],"error":"billing_error"}));
        fold(&refused, &session(), &mut state, input).unwrap();
        let facts = fold(&failing(result(), &[(Subtype, "success_is_error")]), &session(), &mut state, input).unwrap();
        assert_eq!(facts, vec![Observation::TurnEnded {
            turn: TurnId::new(format!("claude-message:{REPLY}")).unwrap(),
            outcome: TurnOutcome::ApiFailed(ApiFailure { error: "billing_error".into(), status: None, resets_at: None }),
        }], "the API's failure comes first");
    }
    // A prompt Claude completes without starting fails its turn: Claude never reads it, and the
    // input leaves what the task waits on.
    #[test]
    fn an_input_claude_finishes_or_cancels_without_taking_it_in_is_dropped() {
        let lifecycle = |command: &str, state: &str| event(json!({"type":"command_lifecycle","command_uuid":command,"state":state,"uuid":REPLY}));
        let dropped = |id: &str| vec![Observation::InputDropped { input: InputId::new(id).unwrap() }];
        let held = |id: &str| vec![Observation::InputHeld { input: InputId::new(id).unwrap() }];
        let mut state = state();
        assert_eq!(fold(&lifecycle(FIRST, "queued"), &session(), &mut state, input).unwrap(), held(FIRST));
        assert_eq!(fold(&lifecycle(FIRST, "completed"), &session(), &mut state, input).unwrap(), dropped(FIRST));
        assert_eq!(fold(&lifecycle(SECOND, "cancelled"), &session(), &mut state, input).unwrap(), dropped(SECOND));

        assert_eq!(fold(&lifecycle(THIRD, "started"), &session(), &mut state, input).unwrap(), vec![]);
        assert_eq!(fold(&lifecycle(THIRD, "cancelled"), &session(), &mut state, input).unwrap(), vec![], "an interrupt cancels what it started");
        let assistant = event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FOURTH,"user_message_uuids":[FOURTH]}));
        fold(&assistant, &session(), &mut state, input).unwrap();
        assert_eq!(fold(&lifecycle(FOURTH, "completed"), &session(), &mut state, input).unwrap(), vec![], "a consumed input ends as it should");
        assert!(state.taken.is_empty(), "what ended is forgotten");

        let foreign = |_: &[MessageUuid]| -> Result<Vec<InputId>, AdapterError> { Err(AdapterError::InvalidObservation) };
        assert!(fold(&lifecycle(NOTIFIED, "completed"), &session(), &mut state, foreign).is_err(), "another's command is evidence");
        let mut early = Checkpoint { format: CHECKPOINT_FORMAT, ..Checkpoint::default() };
        assert_eq!(fold(&lifecycle(FIRST, "completed"), &session(), &mut early, |_| panic!("before init nothing is dropped")).unwrap(), vec![]);
    }
    // Claude Code 2.1.322 refuses an input before queueing it, or discards one still queued as its
    // session ends: neither will run in this session.
    #[test]
    fn an_input_claude_refuses_or_discards_is_dropped() {
        let lifecycle = |command: &str, state: &str| event(json!({"type":"command_lifecycle","command_uuid":command,"state":state,"uuid":REPLY}));
        let dropped = |id: &str| vec![Observation::InputDropped { input: InputId::new(id).unwrap() }];
        let mut state = state();
        assert_eq!(fold(&lifecycle(FIRST, "refused"), &session(), &mut state, input).unwrap(), dropped(FIRST));
        assert_eq!(fold(&lifecycle(SECOND, "queued"), &session(), &mut state, input).unwrap(), vec![Observation::InputHeld { input: InputId::new(SECOND).unwrap() }]);
        assert_eq!(fold(&lifecycle(SECOND, "discarded"), &session(), &mut state, input).unwrap(), dropped(SECOND));
        assert_eq!(fold(&lifecycle(THIRD, "started"), &session(), &mut state, input).unwrap(), vec![]);
        assert_eq!(fold(&lifecycle(THIRD, "discarded"), &session(), &mut state, input).unwrap(), vec![], "what it started is no drop");
    }
    // Recorded from Claude Code 2.1.322 on 12 Nov 2026: a second input written while the first ran
    // was queued during that turn, and the first turn's result still counted no queued send
    // (`queued_turn_count` 0) before Claude started it. That count is no evidence an input left the
    // queue: only the input's own lifecycle or a result naming it decides.
    #[test]
    fn a_result_that_counts_no_queued_send_drops_nothing_still_queued() {
        const LATER: &str = "10000000-0000-4000-8000-000000000008";
        let lifecycle = |command: &str, state: &str| event(json!({"type":"command_lifecycle","command_uuid":command,"state":state,"uuid":NOTIFIED}));
        let session_state = |state: &str| event(json!({"type":"system","subtype":"session_state_changed","state":state,"uuid":THIRD}));
        let assistant = |reply: &str, id: &str| event(json!({"type":"assistant","uuid":reply,"parent_tool_use_id":null,"user_message_uuid":id,"user_message_uuids":[id]}));
        let result = |id: &str| event(json!({"type":"result","uuid":NOTIFIED,"subtype":"success","is_error":false,"user_message_uuid":id,"user_message_uuids":[id],"queued_turn_count":0}));
        let mut state = state();
        let mut facts = vec![];
        for frame in [
            session_state("running"), lifecycle(FIRST, "queued"), lifecycle(FIRST, "started"), lifecycle(SECOND, "queued"),
            event(json!({"type":"system","subtype":"init","uuid":FOURTH})), assistant(REPLY, FIRST), result(FIRST), lifecycle(FIRST, "completed"),
            lifecycle(SECOND, "started"), event(json!({"type":"system","subtype":"init","uuid":LATER})), assistant(LATER, SECOND), result(SECOND),
            lifecycle(SECOND, "completed"), session_state("idle"),
        ] {
            facts.extend(fold(&frame, &session(), &mut state, input).unwrap());
        }
        assert!(!facts.iter().any(|fact| matches!(fact, Observation::InputDropped { .. })), "{facts:?}");
        let started: Vec<_> = facts.iter().filter_map(|fact| match fact { Observation::TurnStarted { input, .. } => Some(input.as_str().to_owned()), _ => None }).collect();
        assert_eq!(started, [FIRST, SECOND]);
        assert_eq!(facts.iter().filter(|fact| matches!(fact, Observation::TurnEnded { outcome: TurnOutcome::Completed, .. })).count(), 2);
    }
    // 25 Oct, t-duo-parcel and t-release-apps-beta-61: Claude Code folded an input of ours into a
    // running turn and finished it before that turn's result, which named it again. The input
    // stayed taken, so the init of the turn a background Bash woke later opened nothing, and the
    // whole turn, its usage limit too, was dropped: the task stayed at work for 13 hours.
    #[test]
    fn a_turn_a_background_command_wakes_after_an_input_finished_before_its_result_ends_on_the_limit() {
        let mut state = state();
        let lifecycle = |state: &str| event(json!({"type":"command_lifecycle","command_uuid":FIRST,"state":state,"uuid":REPLY}));
        fold(&lifecycle("started"), &session(), &mut state, input).unwrap();
        fold(&event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]})), &session(), &mut state, input).unwrap();
        fold(&event(json!({"type":"system","subtype":"task_started","uuid":SECOND,"task_id":"bash-1","tool_use_id":"toolu_1","task_type":"local_bash","is_backgrounded":true})),
            &session(), &mut state, input).unwrap();
        fold(&lifecycle("completed"), &session(), &mut state, input).unwrap();
        fold(&event(json!({"type":"result","uuid":THIRD,"subtype":"success","user_message_uuid":FIRST,"user_message_uuids":[FIRST]})), &session(), &mut state, input).unwrap();
        fold(&event(json!({"type":"system","subtype":"task_notification","uuid":NOTIFIED,"task_id":"bash-1","tool_use_id":"toolu_1","status":"completed"})),
            &session(), &mut state, input).unwrap();
        assert_eq!(fold(&event(json!({"type":"system","subtype":"init","uuid":FOURTH})), &session(), &mut state, input).unwrap(), vec![]);
        let woken = TurnId::new("claude-message:10000000-0000-4000-8000-000000000010").unwrap();
        let facts = turn_end(&mut state, vec![
            json!({"type":"rate_limit_event","uuid":REPLY,"rate_limit_info":{"status":"rejected","resetsAt":1_792_972_800_u64,"rateLimitType":"seven_day"}}),
            json!({"type":"assistant","uuid":"10000000-0000-4000-8000-000000000010","parent_tool_use_id":null,"error":"rate_limit"}),
        ]);
        assert!(facts.contains(&Observation::TurnResumed {
            continuation: ContinuationId::new(format!("claude-notification:{NOTIFIED}")).unwrap(), turn: woken.clone(),
        }), "the notification's prompt takes up the work that ended: {facts:?}");
        let result = failing(event(json!({"type":"result","uuid":"10000000-0000-4000-8000-000000000011","subtype":"success","is_error":true,"api_error_status":429,
            "terminal_reason":"api_error","origin":{"kind":"task-notification","producer":"session-task"}})), &[(alondra_agent_protocol::FailureSurface::Result, "success_is_error")]);
        let ended = fold(&result, &session(), &mut state, input).expect("the result ends the woken turn");
        assert!(facts.iter().all(|fact| !matches!(fact, Observation::TurnEnded { .. })), "{facts:?}");
        assert_eq!(ended, vec![Observation::TurnEnded {
            turn: woken,
            outcome: TurnOutcome::ApiFailed(ApiFailure { error: "rate_limit".into(), status: Some(429), resets_at: Some(1_792_972_800) }),
        }]);
    }
    // 25 Oct, t-alpha-report-1-local, from its host's outbox: a background Bash names no run, nor
    // does the result of the turn its notification opens. That turn and the nine after it, the
    // pull request's registration among them, were dropped, and the work's continuation was left
    // waiting for a turn that had already run.
    #[test]
    fn the_turn_a_background_bash_without_a_run_wakes_is_recorded_whole() {
        let mut state = state();
        fold(&event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]})), &session(), &mut state, input).unwrap();
        fold(&event(json!({"type":"system","subtype":"task_started","uuid":SECOND,"task_id":"bdzu2notx","task_type":"local_bash","is_backgrounded":true})), &session(), &mut state, input).unwrap();
        fold(&event(json!({"type":"result","uuid":THIRD,"subtype":"success","terminal_reason":"completed","user_message_uuid":FIRST,"user_message_uuids":[FIRST]})), &session(), &mut state, input).unwrap();
        let continuation = ContinuationId::new(format!("claude-notification:{NOTIFIED}")).unwrap();
        assert_eq!(fold(&event(json!({"type":"system","subtype":"task_notification","uuid":NOTIFIED,"task_id":"bdzu2notx","status":"completed"})), &session(), &mut state, input).unwrap(),
            vec![Observation::WorkEnded { work: WorkId::new(format!("claude-task:{SECOND}")).unwrap(), continuation: continuation.clone() }]);
        assert_eq!(fold(&event(json!({"type":"system","subtype":"init","uuid":FOURTH})), &session(), &mut state, input).unwrap(), vec![]);
        let turn = TurnId::new("claude-message:10000000-0000-4000-8000-000000000020").unwrap();
        assert_eq!(
            fold(&event(json!({"type":"assistant","uuid":"10000000-0000-4000-8000-000000000020","parent_tool_use_id":null})), &session(), &mut state, |_| panic!("no input of Alondra's opened this turn")).unwrap(),
            vec![Observation::TurnResumed { continuation, turn: turn.clone() }, Observation::ModelAnswered],
        );
        assert_eq!(fold(&event(json!({"type":"system","subtype":"task_started","uuid":"10000000-0000-4000-8000-000000000021","task_id":"b6buw6kkb","task_type":"local_bash","is_backgrounded":true})), &session(), &mut state, input).unwrap(),
            vec![Observation::WorkStarted { turn: turn.clone(), work: WorkId::new("claude-task:10000000-0000-4000-8000-000000000021").unwrap() }], "the woken turn starts work of its own");
        let result = event(json!({"type":"result","uuid":"10000000-0000-4000-8000-000000000022","subtype":"success","terminal_reason":"completed","origin":{"kind":"task-notification","producer":"session-task"}}));
        assert_eq!(fold(&result, &session(), &mut state, input).unwrap(), vec![Observation::TurnEnded { turn, outcome: TurnOutcome::Completed }]);
    }
    // Two background commands stopped at once: Claude Code answers both notifications with one
    // turn that has only its result.
    #[test]
    fn one_turn_takes_up_every_notification_waiting_even_with_nothing_but_its_result() {
        let mut state = state();
        fold(&event(json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]})), &session(), &mut state, input).unwrap();
        for (task, id) in [("bp4z36ytc", SECOND), ("bdx5wrplj", THIRD)] {
            fold(&event(json!({"type":"system","subtype":"task_started","uuid":id,"task_id":task,"task_type":"local_bash","is_backgrounded":true})), &session(), &mut state, input).unwrap();
        }
        fold(&event(json!({"type":"result","uuid":FOURTH,"subtype":"success","user_message_uuid":FIRST,"user_message_uuids":[FIRST]})), &session(), &mut state, input).unwrap();
        fold(&event(json!({"type":"system","subtype":"task_notification","uuid":NOTIFIED,"task_id":"bp4z36ytc","status":"stopped"})), &session(), &mut state, input).unwrap();
        fold(&event(json!({"type":"system","subtype":"task_notification","uuid":"10000000-0000-4000-8000-000000000030","task_id":"bdx5wrplj","status":"stopped"})), &session(), &mut state, input).unwrap();
        assert_eq!(fold(&event(json!({"type":"system","subtype":"init","uuid":"10000000-0000-4000-8000-000000000031"})), &session(), &mut state, input).unwrap(), vec![]);
        let turn = TurnId::new("claude-message:10000000-0000-4000-8000-000000000032").unwrap();
        let result = event(json!({"type":"result","uuid":"10000000-0000-4000-8000-000000000032","subtype":"success","is_error":false,"origin":{"kind":"task-notification"}}));
        assert_eq!(fold(&result, &session(), &mut state, input).unwrap(), vec![
            Observation::TurnResumed { continuation: ContinuationId::new(format!("claude-notification:{NOTIFIED}")).unwrap(), turn: turn.clone() },
            Observation::TurnEnded { turn, outcome: TurnOutcome::Completed },
        ]);
        assert!(state.turn.is_none() && state.woken.is_empty(), "nothing is left to wake another turn");
    }
    /// A prompt as the native Claude 2.1.300 runs it, from a resting session.
    #[derive(Clone, Copy, Debug)]
    enum Prompt {
        /// An input of ours. Another may join the running turn; Claude Code finishes the inputs
        /// before or after the turn's result; a Bash may stay running in the background.
        Input { joined: bool, finished_first: bool, background: bool, limited: bool },
        /// The oldest background Bash ends and its notification wakes a turn of Claude's own,
        /// naming its run or not.
        Woken { run: bool, limited: bool },
        /// A subagent hands its report back: Claude opens a turn by itself.
        Own { limited: bool },
    }

    fn prompts() -> Vec<Prompt> {
        let flags = [false, true];
        let mut all = vec![];
        for joined in flags {
            for finished_first in flags {
                for background in flags {
                    for limited in flags {
                        all.push(Prompt::Input { joined, finished_first, background, limited });
                    }
                }
            }
        }
        for limited in flags {
            all.extend([Prompt::Woken { run: false, limited }, Prompt::Woken { run: true, limited }, Prompt::Own { limited }]);
        }
        all
    }

    struct Session {
        state: Checkpoint,
        serial: u64,
        background: Vec<(String, bool)>,
    }

    impl Session {
        fn uuid(&mut self) -> String {
            self.serial += 1;
            format!("20000000-0000-4000-8000-{:012}", self.serial)
        }
        fn fold(&mut self, payload: Value) -> Result<Vec<Observation>, String> {
            let shown = payload.to_string();
            fold(&event(payload), &session(), &mut self.state, input).map_err(|error| format!("{shown} was dropped: {error:?}"))
        }
        fn fold_all(&mut self, payloads: Vec<Value>) -> Result<Vec<Observation>, String> {
            let mut facts = vec![];
            for payload in payloads {
                facts.extend(self.fold(payload)?);
            }
            Ok(facts)
        }
        fn init(&mut self) -> Value {
            json!({"type":"system","subtype":"init","uuid":self.uuid()})
        }
        fn answer(&mut self, limited: bool) -> Value {
            let mut answer = json!({"type":"assistant","uuid":self.uuid(),"parent_tool_use_id":null});
            if limited {
                answer["error"] = json!("rate_limit");
            }
            answer
        }
        fn result(&mut self, limited: bool, mut extra: Value) -> Value {
            let mut result = json!({"type":"result","uuid":self.uuid(),"subtype":"success","is_error":limited});
            if limited {
                result["api_error_status"] = json!(429);
                result["terminal_reason"] = json!("api_error");
            }
            result.as_object_mut().unwrap().append(extra.as_object_mut().unwrap());
            result
        }

        /// Runs `prompt`: its turn ends, on the limit when the limit ended it.
        fn run(&mut self, prompt: Prompt) -> Result<(), String> {
            let (facts, limited) = match prompt {
                Prompt::Input { joined, finished_first, background, limited } => {
                    let first = self.uuid();
                    let mut inputs = vec![first.clone()];
                    let lifecycle = |command: &str, state: &str, uuid: String| json!({"type":"command_lifecycle","command_uuid":command,"state":state,"uuid":uuid});
                    let mut payloads = vec![lifecycle(&first, "started", self.uuid()), self.init(),
                        json!({"type":"assistant","uuid":self.uuid(),"parent_tool_use_id":null,"user_message_uuid":first,"user_message_uuids":[first]})];
                    if background {
                        let task = format!("bash-{}", self.serial);
                        let run = self.serial % 2 == 0;
                        let mut started = json!({"type":"system","subtype":"task_started","uuid":self.uuid(),"task_id":task,"tool_use_id":format!("toolu-{task}"),"task_type":"local_bash","is_backgrounded":true});
                        if run {
                            started["run_id"] = json!(format!("run-{task}"));
                        }
                        payloads.push(started);
                        self.background.push((task, run));
                    }
                    if joined {
                        let second = self.uuid();
                        payloads.push(lifecycle(&second, "started", self.uuid()));
                        payloads.push(json!({"type":"assistant","uuid":self.uuid(),"parent_tool_use_id":null,"user_message_uuid":second,"user_message_uuids":[first, second]}));
                        inputs.push(second);
                    }
                    payloads.push(self.answer(limited));
                    let finished: Vec<Value> = inputs.iter().map(|input| lifecycle(input, "completed", self.uuid())).collect();
                    let result = self.result(limited, json!({"user_message_uuid":first,"user_message_uuids":inputs}));
                    if finished_first {
                        payloads.extend(finished);
                        payloads.push(result);
                    } else {
                        payloads.push(result);
                        payloads.extend(finished);
                    }
                    (self.fold_all(payloads)?, limited)
                }
                Prompt::Woken { limited, .. } if self.background.is_empty() => return self.run(Prompt::Own { limited }),
                Prompt::Woken { run, limited } => {
                    let (task, named) = self.background.remove(0);
                    let mut notification = json!({"type":"system","subtype":"task_notification","uuid":self.uuid(),"task_id":task,"tool_use_id":format!("toolu-{task}"),"status":"completed"});
                    let mut origin = json!({"kind":"task-notification","producer":"session-task"});
                    if named && run {
                        notification["run_id"] = json!(format!("run-{task}"));
                        origin["runId"] = json!(format!("run-{task}"));
                    } else if named {
                        self.background.insert(0, (task, named));
                        return self.run(Prompt::Own { limited });
                    }
                    let payloads = vec![notification, self.init(), self.answer(limited), self.result(limited, json!({"origin":origin}))];
                    (self.fold_all(payloads)?, limited)
                }
                Prompt::Own { limited } => {
                    let payloads = vec![self.init(), self.answer(limited), self.result(limited, json!({}))];
                    (self.fold_all(payloads)?, limited)
                }
            };
            let ended: Vec<&TurnOutcome> = facts.iter().filter_map(|fact| match fact {
                Observation::TurnEnded { outcome, .. } => Some(outcome),
                _ => None,
            }).collect();
            match ended.as_slice() {
                [outcome] if limited && !matches!(outcome, TurnOutcome::ApiFailed(failure) if failure.error == "rate_limit") =>
                    Err(format!("the limit ended the turn as {outcome:?}")),
                [_] => Ok(()),
                other => Err(format!("the prompt ended {} turns: {facts:?}", other.len())),
            }
        }
    }

    // Every prompt the native Claude runs ends one turn, whatever came before it, and one the
    // usage limit ends says so: a turn that ends nowhere leaves its task at work for nothing.
    #[test]
    fn every_turn_claude_runs_ends_and_one_the_limit_ends_says_so_after_any_three_prompts() {
        let all = prompts();
        for a in &all {
            for b in &all {
                for c in &all {
                    let mut session = Session { state: state(), serial: 0, background: vec![] };
                    for (n, prompt) in [a, b, c].into_iter().enumerate() {
                        if let Err(why) = session.run(*prompt) {
                            panic!("prompt {} of {:?}: {why}", n + 1, [a, b, c]);
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn a_host_that_takes_over_forgets_the_inputs_the_ended_host_had_taken() {
        let mut state = state();
        fold(&event(json!({"type":"command_lifecycle","command_uuid":FIRST,"state":"started","uuid":REPLY})), &session(), &mut state, input).unwrap();
        state.forget_ended_host();
        assert_eq!(fold(&event(json!({"type":"system","subtype":"init","uuid":SECOND})), &session(), &mut state, input).unwrap(),
            vec![Observation::TurnUnprompted { turn: TurnId::new(format!("claude-message:{SECOND}")).unwrap() }],
            "the new host's Claude opens its own turn: nothing of ours is still being taken");
    }
    // 25 Oct, t-codex-support-design: the refusal came for a subagent's request before the init of
    // the turn the limit then ended, and its card had no reset to wait for.
    #[test]
    fn a_limit_refused_before_its_turn_opens_still_says_when_it_resets() {
        let mut state = state();
        fold(&event(json!({"type":"rate_limit_event","uuid":REPLY,"rate_limit_info":{"status":"rejected","resetsAt":1_792_972_800_u64,"rateLimitType":"seven_day"}})),
            &session(), &mut state, input).unwrap();
        let facts = turn_end(&mut state, vec![
            json!({"type":"system","subtype":"init","uuid":SECOND}),
            json!({"type":"assistant","uuid":THIRD,"parent_tool_use_id":null,"error":"rate_limit"}),
            json!({"type":"result","uuid":FOURTH,"is_error":true,"api_error_status":429,"terminal_reason":"api_error"}),
        ]);
        assert_eq!(failure(&facts).and_then(|failure| failure.resets_at), Some(1_792_972_800));
        let next = turn_end(&mut state, vec![
            json!({"type":"system","subtype":"init","uuid":NOTIFIED}),
            json!({"type":"assistant","uuid":REPLY,"parent_tool_use_id":null,"error":"rate_limit"}),
            json!({"type":"result","uuid":SECOND,"is_error":true,"api_error_status":429,"terminal_reason":"api_error"}),
        ]);
        assert_eq!(failure(&next).and_then(|failure| failure.resets_at), None, "a reset is said once, for the turn it came before");
    }
    #[test]
    fn the_inputs_claude_took_in_are_bounded_and_the_oldest_is_forgotten() {
        let mut state = state();
        for n in 0..=MAX_TAKEN {
            let started = event(json!({"type":"command_lifecycle","command_uuid":format!("20000000-0000-4000-8000-{n:012}"),"state":"started","uuid":REPLY}));
            fold(&started, &session(), &mut state, input).unwrap();
        }
        assert_eq!(state.taken.len(), MAX_TAKEN);
        assert_eq!(state.taken[0].as_str(), "20000000-0000-4000-8000-000000000001");
    }
}
#[cfg(test)]
mod diagnostic_tests {
    use super::*;
    use alondra_agent_protocol::{Execution, NativeCorrelation, SdkEventKind, TaskExecution};
    use alondra_foundation::{AgentId, DataEpoch, InputId, TaskId};
    use serde_json::json;
    const SESSION: &str = "30000000-0000-4000-8000-000000000001";
    const INPUT: &str = "30000000-0000-4000-8000-000000000002";
    fn event(kind: SdkEventKind, payload: Value) -> Event {
        Event {
            fence: Fence {
                profile_id: "fixture".into(),
                account_id: "fixture".into(),
                data_epoch: DataEpoch::from("epoch"),
                host_id: "host".into(),
            },
            execution: Execution::Task(TaskExecution {
                task_id: TaskId::from("task"),
                agent_id: AgentId::from("node"),
                conversation_id: "conversation".into(),
                process_instance_id: "process".into(),
                generation: 1,
            }),
            cursor: 1,
            observation: WireObservation::Sdk {
                kind,
                native: NativeCorrelation::default(),
                payload,
                failures: vec![],
            },
        }
    }
    fn session() -> SessionRecord {
        SessionRecord {
            session_id: MessageUuid::new(SESSION).unwrap(),
            continued: false,
        }
    }
    fn state() -> Checkpoint {
        Checkpoint {
            format: CHECKPOINT_FORMAT,
            initialized: true,
            ..Checkpoint::default()
        }
    }
    fn assistant() -> Value {
        json!({"type":"assistant","session_id":SESSION,"uuid":"30000000-0000-4000-8000-000000000003","parent_tool_use_id":null,"user_message_uuid":INPUT,"user_message_uuids":[INPUT]})
    }
    fn input(_: &[MessageUuid]) -> Result<Vec<InputId>, AdapterError> {
        Ok(vec![InputId::new("initial").unwrap()])
    }
    #[test]
    fn real_host_stderr_diagnostic_needs_no_sdk_session_and_produces_no_facts() {
        let diagnostic = event(
            SdkEventKind::Diagnostic,
            json!({"source":"stderr","text":"Redacted fixture diagnostic","truncated_at_bytes":16384}),
        );
        assert!(
            fold(&diagnostic, &session(), &mut state(), |_| panic!(
                "diagnostic is not consumption"
            ))
            .unwrap()
            .is_empty()
        );
    }
    #[test]
    fn opaque_stderr_source_on_assistant_cannot_hide_consumption() {
        let plain = fold(
            &event(SdkEventKind::Message, assistant()),
            &session(),
            &mut state(),
            input,
        )
        .unwrap();
        assert!(matches!(
            plain.first(),
            Some(Observation::TurnStarted { .. })
        ));
        let mut payload = assistant();
        payload["source"] = json!("stderr");
        let with_source = fold(
            &event(SdkEventKind::Message, payload),
            &session(),
            &mut state(),
            input,
        )
        .unwrap();
        assert_eq!(
            with_source, plain,
            "opaque SDK source must not become host diagnostic authority"
        );
    }
    // A message of another Claude session, with or without a stderr source, is evidence: it
    // neither consumes an input of ours nor changes the turn.
    #[test]
    fn a_message_of_another_session_is_evidence_even_with_a_stderr_source() {
        let mut payload = assistant();
        payload["session_id"] = json!("wrong-session");
        for payload in [payload.clone(), { payload["source"] = json!("stderr"); payload }] {
            let mut state = state();
            let before = serde_json::to_value(&state).unwrap();
            let facts = fold(&event(SdkEventKind::Message, payload), &session(), &mut state, |_| panic!("another session consumes no input of ours"));
            assert_eq!(facts.unwrap(), vec![]);
            assert_eq!(serde_json::to_value(&state).unwrap(), before);
        }
    }

    // The host denies Claude a request too large to ask and reports it: evidence, no fact, even
    // with a turn open.
    #[test]
    fn a_decision_too_large_to_ask_is_evidence_without_facts() {
        use alondra_agent_protocol::{FailureSurface, SdkFailure};
        let details = json!({"request_id":"decision-1","kind":"permission","bytes":2_097_152,"sha256":"0".repeat(64),"reason":"size"});
        let mut diagnostic = event(SdkEventKind::Diagnostic, details.clone());
        let WireObservation::Sdk { failures, .. } = &mut diagnostic.observation else { unreachable!() };
        failures.push(SdkFailure { surface: FailureSurface::Transport, code: "decision_too_large".into(), details });
        diagnostic.validate_for_application().unwrap();
        let mut state = state();
        fold(&event(SdkEventKind::Message, assistant()), &session(), &mut state, input).unwrap();
        let before = serde_json::to_value(&state).unwrap();
        assert_eq!(fold(&diagnostic, &session(), &mut state, |_| panic!("a diagnostic consumes no input")).unwrap(), vec![]);
        assert_eq!(serde_json::to_value(&state).unwrap(), before, "the turn stays as it was");
        assert!(!ended_with_its_host(&diagnostic.observation), "the host goes on");
    }
    #[test]
    fn only_the_exit_and_the_iterator_end_are_what_a_host_leaves_as_it_ends() {
        use alondra_agent_protocol::{FailureSurface, SdkFailure};
        let failure = |surface| SdkFailure { surface, code: "aborted".into(), details: json!({}) };
        let diagnostic = |failures| WireObservation::Sdk { kind: SdkEventKind::Diagnostic, native: NativeCorrelation::default(), payload: json!({}), failures };
        assert!(ended_with_its_host(&WireObservation::ProcessExited { exit_code: Some(0), signal: None }));
        assert!(ended_with_its_host(&diagnostic(vec![failure(FailureSurface::Control)])));
        assert!(ended_with_its_host(&diagnostic(vec![failure(FailureSurface::Iterator)])));
        assert!(!ended_with_its_host(&diagnostic(vec![failure(FailureSurface::Auth)])), "a sign-in failure is a fact, not the host's end");
        assert!(!ended_with_its_host(&diagnostic(vec![])));
        assert!(!ended_with_its_host(&event(SdkEventKind::Result, json!({"type": "result"})).observation));
    }
}
