//! Controls of a running SDK execution: decided by the domain, staged in the same commit, claimed
//! and sent only by the owner under the same fences as dispatches, and reconciled after a crash.
use super::*;
use alondra_domain::{
    ActivityView, CallbackView, Control, Decision, OperationId, PermissionMode, TurnId, decision_callback,
};
use alondra_foundation::CallbackId;
use alondra_provider_contract::DecisionKind;

/// A decision Claude asked for, as its host persisted it: the request it answers, the tool call it
/// is about (none for an MCP server's request for input) and its exact payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionRequest {
    request_id: String,
    tool_use_id: Option<String>,
    kind: DecisionKind,
    payload: Vec<u8>,
}
impl DecisionRequest {
    pub fn new(request_id: String, tool_use_id: Option<String>, kind: DecisionKind, payload: Vec<u8>) -> Result<Self> {
        identity(&request_id)?;
        if let Some(tool_use_id) = &tool_use_id {
            identity(tool_use_id)?;
        }
        if (kind == DecisionKind::Elicitation) != tool_use_id.is_none() || payload.is_empty() || payload.len() > MAX_PAYLOAD {
            return Err(Error::InvalidInput);
        }
        Ok(Self { request_id, tool_use_id, kind, payload })
    }
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    pub fn tool_use_id(&self) -> Option<&str> {
        self.tool_use_id.as_deref()
    }
    pub fn kind(&self) -> DecisionKind {
        self.kind
    }
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

/// The decision the domain recorded for the request its host persisted at `epoch`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    epoch: u64,
    request: DecisionRequest,
    decision: Decision,
}
impl Response {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn request(&self) -> &DecisionRequest {
        &self.request
    }
    pub fn decision(&self) -> &Decision {
        &self.decision
    }
}

/// What a claimed control does to the execution: an operation the domain requested, or the
/// response to a decision Claude waits for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlOperation {
    Control(Control),
    Respond(Response),
}

fn kind_name(kind: DecisionKind) -> &'static str {
    match kind {
        DecisionKind::Permission => "permission",
        DecisionKind::Question => "question",
        DecisionKind::Plan => "plan",
        DecisionKind::Elicitation => "elicitation",
    }
}
fn kind_of(name: &str) -> Result<DecisionKind> {
    Ok(match name {
        "permission" => DecisionKind::Permission,
        "question" => DecisionKind::Question,
        "plan" => DecisionKind::Plan,
        "elicitation" => DecisionKind::Elicitation,
        _ => return Err(Error::CorruptJournal),
    })
}

/// A committed control the owner may claim once. Its correlation data grants nothing:
///
/// ```
/// use alondra_foundation::ExecutionStamp;
/// use alondra_domain::OperationId;
/// use alondra_store::{AuthorityScope, ControlPermit};
/// fn forward(permit: ControlPermit) -> ControlPermit {
///     let _: i64 = permit.id();
///     let _: &ExecutionStamp = permit.execution();
///     let _: &OperationId = permit.control();
///     let _: &AuthorityScope = permit.scope();
///     permit
/// }
/// ```
///
/// ```compile_fail,E0451
/// use alondra_foundation::ExecutionStamp;
/// use alondra_domain::OperationId;
/// use alondra_store::{AuthorityScope, ControlPermit};
/// fn fabricate(scope: AuthorityScope, stamp: ExecutionStamp, control: OperationId) -> ControlPermit {
///     ControlPermit { id: 1, scope, stamp, control }
/// }
/// ```
///
/// ```compile_fail,E0277
/// use alondra_store::ControlPermit;
/// fn requires_clone<T: Clone>() {}
/// requires_clone::<ControlPermit>();
/// ```
#[derive(Debug)]
pub struct ControlPermit {
    id: i64,
    scope: AuthorityScope,
    stamp: ExecutionStamp,
    control: OperationId,
}
impl ControlPermit {
    pub fn id(&self) -> i64 {
        self.id
    }
    pub fn execution(&self) -> &ExecutionStamp {
        &self.stamp
    }
    pub fn control(&self) -> &OperationId {
        &self.control
    }
    pub fn scope(&self) -> &AuthorityScope {
        &self.scope
    }
}

/// A claimed control: it authorizes sending only after its claim transaction commits.
///
/// ```
/// use alondra_store::{ControlAttempt, ControlOperation};
/// fn forward(attempt: ControlAttempt) -> ControlAttempt {
///     let _: i64 = attempt.id();
///     let _: &ControlOperation = attempt.operation();
///     let _ = attempt.command_id();
///     attempt
/// }
/// ```
///
/// ```compile_fail,E0451
/// use alondra_store::ControlAttempt;
/// fn replace_identity(attempt: ControlAttempt) -> ControlAttempt {
///     ControlAttempt { id: 2, ..attempt }
/// }
/// ```
///
/// ```compile_fail,E0277
/// use alondra_store::ControlAttempt;
/// fn requires_clone<T: Clone>() {}
/// requires_clone::<ControlAttempt>();
/// ```
#[derive(Debug)]
pub struct ControlAttempt {
    id: i64,
    scope: AuthorityScope,
    revision: Revision,
    stamp: ExecutionStamp,
    control: OperationId,
    operation: ControlOperation,
    binding: ProviderBinding,
}
impl ControlAttempt {
    pub fn id(&self) -> i64 {
        self.id
    }
    pub fn command_id(&self) -> alondra_foundation::CommandId {
        format!("lifecycle-control-{}", self.id).into()
    }
    pub fn revision(&self) -> Revision {
        self.revision
    }
    pub fn execution(&self) -> &ExecutionStamp {
        &self.stamp
    }
    pub fn control(&self) -> &OperationId {
        &self.control
    }
    pub fn operation(&self) -> &ControlOperation {
        &self.operation
    }
    pub fn binding(&self) -> &ProviderBinding {
        &self.binding
    }
    pub fn scope(&self) -> &AuthorityScope {
        &self.scope
    }
}

#[derive(Debug)]
pub(super) struct StagedControl {
    attempt: ControlAttempt,
    owner_generation: u64,
    evidence: CommandEvidence,
}
impl StagedControl {
    pub(super) fn seal(self) -> CommittedControl {
        CommittedControl { staged: self }
    }
}

/// The exact control command the owner recorded, issued only after its transaction commits.
/// ```
/// use alondra_store::CommittedControl;
/// fn inspect(value: CommittedControl) -> CommittedControl { let _ = (value.bytes(), value.attempt(), value.owner_generation()); value }
/// ```
/// ```compile_fail,E0451
/// use alondra_store::CommittedControl;
/// fn reconstruct(value: CommittedControl) -> CommittedControl { CommittedControl { ..value } }
/// ```
/// ```compile_fail,E0277
/// use alondra_store::CommittedControl;
/// fn requires_clone<T: Clone>() {}
/// requires_clone::<CommittedControl>();
/// ```
/// ```compile_fail,E0432
/// use alondra_store::StagedControl;
/// ```
#[derive(Debug)]
pub struct CommittedControl {
    staged: StagedControl,
}
impl CommittedControl {
    pub fn attempt(&self) -> &ControlAttempt {
        &self.staged.attempt
    }
    pub fn owner_generation(&self) -> u64 {
        self.staged.owner_generation
    }
    pub fn bytes(&self) -> &[u8] {
        self.staged.evidence.bytes()
    }
}

/// A control the owner claimed and recorded but whose receipt a crash lost, with its exact bytes.
#[derive(Debug)]
pub struct UnresolvedControl {
    pub id: i64,
    pub command: Vec<u8>,
}

fn required_by(kind: &Control) -> Capability {
    match kind {
        Control::Interrupt { .. } => Capability::Interrupt,
        Control::ForceStop { .. } => Capability::ForceStop,
        Control::SetModel { .. } => Capability::SetModel,
        Control::SetPermissionMode { .. } => Capability::SetPermissionMode,
        Control::ReadMcpStatus => Capability::McpStatus,
        Control::ReconnectMcp { .. } => Capability::McpReconnect,
        Control::StartMcpSignIn { .. } => Capability::McpSignIn,
        Control::ClearMcpSignIn { .. } => Capability::McpClearAuth,
    }
}
fn required(operation: &ControlOperation) -> Capability {
    match operation {
        ControlOperation::Control(kind) => required_by(kind),
        ControlOperation::Respond(_) => Capability::Decisions,
    }
}
fn columns(kind: &Control) -> (&'static str, Option<&str>) {
    match kind {
        Control::Interrupt { turn } => ("interrupt", turn.as_ref().map(TurnId::as_str)),
        Control::ForceStop { turn } => ("force_stop", Some(turn.as_str())),
        Control::SetModel { model } => ("set_model", Some(model.as_str())),
        Control::SetPermissionMode { mode } => ("set_permission_mode", Some(match mode {
            PermissionMode::Default => "default",
            PermissionMode::Plan => "plan",
            PermissionMode::DontAsk => "dont_ask",
        })),
        Control::ReadMcpStatus => ("mcp_status", None),
        Control::ReconnectMcp { server } => ("mcp_reconnect", Some(server.as_str())),
        Control::StartMcpSignIn { server } => ("mcp_sign_in", Some(server.as_str())),
        Control::ClearMcpSignIn { server } => ("mcp_clear_auth", Some(server.as_str())),
    }
}
fn control_of(kind: &str, argument: Option<String>) -> Result<Control> {
    Ok(match (kind, argument) {
        ("interrupt", turn) => Control::Interrupt { turn: turn.map(TurnId::new).transpose()? },
        ("force_stop", Some(turn)) => Control::ForceStop { turn: TurnId::new(turn)? },
        ("set_model", Some(model)) => Control::SetModel { model },
        ("set_permission_mode", Some(mode)) => Control::SetPermissionMode {
            mode: match mode.as_str() {
                "default" => PermissionMode::Default,
                "plan" => PermissionMode::Plan,
                "dont_ask" => PermissionMode::DontAsk,
                _ => return Err(Error::CorruptJournal),
            },
        },
        ("mcp_status", None) => Control::ReadMcpStatus,
        ("mcp_reconnect", Some(server)) => Control::ReconnectMcp { server },
        ("mcp_sign_in", Some(server)) => Control::StartMcpSignIn { server },
        ("mcp_clear_auth", Some(server)) => Control::ClearMcpSignIn { server },
        _ => return Err(Error::CorruptJournal),
    })
}
fn status_of(receipt: DispatchReceipt) -> &'static str {
    match receipt {
        DispatchReceipt::Handed => "handed",
        DispatchReceipt::Rejected => "rejected",
        DispatchReceipt::Uncertain => "uncertain",
    }
}
/// Whether an interrupt still names what the execution runs: its root turn, or, without a turn,
/// its own work, the wait for that work to wake Claude, or input Claude has not taken.
fn still_running(aggregate: &TaskLifecycle, turn: Option<&TurnId>) -> bool {
    match aggregate.activity() {
        ActivityView::Working => turn.is_some() && aggregate.turn() == turn,
        ActivityView::OwnWork | ActivityView::Continuation | ActivityView::Starting => turn.is_none(),
        _ => false,
    }
}
/// Whether the operation still has something to act on: an interrupt or a force stop the turn or
/// work it names, a response the decided callback, which neither settled nor was aborted since.
fn still_wanted(aggregate: &TaskLifecycle, generation: u64, operation: &ControlOperation) -> bool {
    match operation {
        ControlOperation::Control(Control::Interrupt { turn }) => still_running(aggregate, turn.as_ref()),
        ControlOperation::Control(Control::ForceStop { turn }) => still_running(aggregate, Some(turn)),
        ControlOperation::Control(_) => true,
        ControlOperation::Respond(response) => {
            aggregate.callback(&decision_callback(generation, response.epoch)) == Some(CallbackView::Answered)
        }
    }
}

impl LifecycleTransaction<'_> {
    /// Stages `kind`, which the domain recorded as `control` in this journal, for the task's
    /// current SDK execution. An operation its binding does not permit is refused before anything
    /// is written, and the refusal poisons the whole transaction.
    pub fn stage_control(&mut self, task: &TaskId, control: &OperationId, kind: &Control) -> Result<()> {
        staging!(self, {
            let aggregate = self.load(task)?;
            let stamp = aggregate.execution().ok_or(Error::MissingIntent)?;
            through_sdk(stamp)?;
            let binding = provider_binding::for_execution(&self.transaction, stamp)?;
            if !binding.permits(required_by(kind)) {
                return Err(Error::UnsupportedCapability);
            }
            provider_binding::require_active(&self.transaction, &binding)?;
            let decided = codec::encode(&Event::RequestControl { control: control.clone(), kind: kind.clone() })?;
            let recorded: bool = self.transaction.query_row("SELECT EXISTS(SELECT 1 FROM lifecycle_events WHERE task_id=?1 AND event=?2)", params![task.0, decided], |row| row.get(0))?;
            if !recorded {
                return Err(Error::MissingIntent);
            }
            let (name, argument) = columns(kind);
            let generation = sql_number(stamp.generation())?;
            let existing: Option<(i64, String, Option<String>)> = self.transaction.query_row("SELECT generation,kind,argument FROM lifecycle_controls WHERE task_id=?1 AND control_id=?2", params![task.0, control.as_str()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()?;
            match existing {
                Some((old_generation, old_kind, old_argument)) => {
                    if old_generation != generation || old_kind != name || old_argument.as_deref() != argument {
                        return Err(Error::CommandConflict);
                    }
                }
                None => {
                    self.transaction.execute("INSERT INTO lifecycle_controls(task_id,generation,control_id,kind,argument,status) VALUES(?1,?2,?3,?4,?5,'prepared')", params![task.0, generation, control.as_str(), name, argument])?;
                }
            }
            self.touched.insert(task.clone());
            Ok(())
        })
    }
    /// Stages the input that asks the execution to compact its conversation, if its binding can.
    pub fn stage_compaction(&mut self, task: &TaskId, input: &InputId) -> Result<()> {
        staging!(self, {
            let aggregate = self.load(task)?;
            let stamp = aggregate.execution().ok_or(Error::MissingIntent)?;
            through_sdk(stamp)?;
            let binding = provider_binding::for_execution(&self.transaction, stamp)?;
            if !binding.permits(Capability::Compact) {
                return Err(Error::UnsupportedCapability);
            }
            self.stage_input(task, input)
        })
    }
    /// Keeps the decision request the task's current SDK execution persisted at `epoch`, before
    /// the domain records it. The same request again changes nothing; another one at that epoch
    /// is a conflict.
    pub fn put_decision_request(&mut self, stamp: &ExecutionStamp, epoch: u64, request: &DecisionRequest) -> Result<()> {
        staging!(self, {
            through_sdk(stamp)?;
            if self.load(stamp.task())?.execution() != Some(stamp) {
                return Err(Error::StaleOwner);
            }
            let generation = sql_number(stamp.generation())?;
            let stored = self.decision_request(stamp, epoch)?;
            match stored {
                Some(stored) if stored == *request => {}
                Some(_) => return Err(Error::ObservationConflict),
                None => {
                    self.transaction.execute("INSERT INTO lifecycle_decision_requests(task_id,generation,epoch,request_id,tool_use_id,kind,payload) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![stamp.task().0, generation, sql_number(epoch)?, request.request_id, request.tool_use_id, kind_name(request.kind), request.payload])?;
                }
            }
            self.touched.insert(stamp.task().clone());
            Ok(())
        })
    }
    fn decision_request(&self, stamp: &ExecutionStamp, epoch: u64) -> Result<Option<DecisionRequest>> {
        let row: Option<(String, Option<String>, String, Vec<u8>)> = self.transaction.query_row("SELECT request_id,tool_use_id,kind,payload FROM lifecycle_decision_requests WHERE task_id=?1 AND generation=?2 AND epoch=?3", params![stamp.task().0, sql_number(stamp.generation())?, sql_number(epoch)?], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).optional()?;
        row.map(|(request_id, tool_use_id, kind, payload)| {
            DecisionRequest::new(request_id, tool_use_id, kind_of(&kind)?, payload).map_err(|_| Error::CorruptJournal)
        })
        .transpose()
    }
    fn response(&self, stamp: &ExecutionStamp, epoch: u64, name: &str, argument: Option<String>) -> Result<Response> {
        let (Some(argument), "respond") = (argument, name) else {
            return Err(Error::CorruptJournal);
        };
        let decision: codec::DecisionRecord = serde_json::from_str(&argument).map_err(|_| Error::CorruptJournal)?;
        let request = self.decision_request(stamp, epoch)?.ok_or(Error::CorruptJournal)?;
        Ok(Response { epoch, request, decision: decision.into() })
    }
    /// Stages the response the domain decided, in this journal, for the request the task's
    /// current SDK execution persisted at `epoch`. A binding without decisions is refused before
    /// anything else, and the refusal poisons the whole transaction; so is a decision the journal
    /// does not hold for a callback that waits for its response.
    pub fn stage_response(&mut self, task: &TaskId, epoch: u64, decision: &Decision) -> Result<()> {
        staging!(self, {
            let aggregate = self.load(task)?;
            let stamp = aggregate.execution().ok_or(Error::MissingIntent)?;
            through_sdk(stamp)?;
            let binding = provider_binding::for_execution(&self.transaction, stamp)?;
            if !binding.permits(Capability::Decisions) {
                return Err(Error::UnsupportedCapability);
            }
            provider_binding::require_active(&self.transaction, &binding)?;
            let callback = decision_callback(stamp.generation(), epoch);
            if aggregate.callback(&callback) != Some(CallbackView::Answered) {
                return Err(Error::MissingIntent);
            }
            if self.decision_request(stamp, epoch)?.is_none() || self.decided(task, &callback)?.as_ref() != Some(decision) {
                return Err(Error::MissingIntent);
            }
            let argument = serde_json::to_string(&codec::DecisionRecord::from(decision))?;
            let generation = sql_number(stamp.generation())?;
            let existing: Option<(i64, String, Option<String>, Option<i64>)> = self.transaction.query_row("SELECT generation,kind,argument,epoch FROM lifecycle_controls WHERE task_id=?1 AND control_id=?2", params![task.0, callback.as_str()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).optional()?;
            match existing {
                Some((old_generation, old_kind, old_argument, old_epoch)) => {
                    if old_generation != generation || old_kind != "respond" || old_argument.as_deref() != Some(argument.as_str()) || old_epoch != Some(sql_number(epoch)?) {
                        return Err(Error::CommandConflict);
                    }
                }
                None => {
                    self.transaction.execute("INSERT INTO lifecycle_controls(task_id,generation,control_id,kind,argument,epoch,status) VALUES(?1,?2,?3,'respond',?4,?5,'prepared')", params![task.0, generation, callback.as_str(), argument, sql_number(epoch)?])?;
                }
            }
            self.touched.insert(task.clone());
            Ok(())
        })
    }
    /// The decision the task's journal recorded for `callback`: decided on its card, or by policy.
    fn decided(&self, task: &TaskId, callback: &CallbackId) -> Result<Option<Decision>> {
        let mut statement = self.transaction.prepare("SELECT event FROM lifecycle_events WHERE task_id=?1 ORDER BY revision")?;
        let events = statement.query_map(params![task.0], |row| row.get::<_, String>(0))?;
        codec::decided(events.collect::<rusqlite::Result<Vec<_>>>()?, callback)
    }
    /// Claims a prepared control. An interrupt whose turn or work already ended, or a response
    /// whose callback settled or was aborted, is revoked instead and claims nothing (`false`).
    pub fn claim_control(&mut self, permit: ControlPermit) -> Result<bool> {
        staging!(self, {
            if permit.scope != self.scope {
                return Err(Error::StalePermit);
            }
            let aggregate = self.load(permit.stamp.task())?;
            if aggregate.execution() != Some(&permit.stamp) {
                return Err(Error::StalePermit);
            }
            let row: Option<(String, String, Option<String>, Option<i64>, String)> = self.transaction.query_row("SELECT control_id,kind,argument,epoch,status FROM lifecycle_controls WHERE id=?1 AND task_id=?2 AND generation=?3", params![permit.id, permit.stamp.task().0, sql_number(permit.stamp.generation())?], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))).optional()?;
            let (control, name, argument, epoch, status) = row.ok_or(Error::StalePermit)?;
            if control != permit.control.as_str() || status != "prepared" {
                return Err(Error::StalePermit);
            }
            let operation = match epoch.map(|epoch| u64::try_from(epoch).map_err(|_| Error::CorruptJournal)).transpose()? {
                Some(epoch) => ControlOperation::Respond(self.response(&permit.stamp, epoch, &name, argument)?),
                None => ControlOperation::Control(control_of(&name, argument)?),
            };
            let binding = provider_binding::for_execution(&self.transaction, &permit.stamp)?;
            provider_binding::require_active(&self.transaction, &binding)?;
            if !binding.permits(required(&operation)) {
                return Err(Error::UnsupportedCapability);
            }
            self.touched.insert(permit.stamp.task().clone());
            if !still_wanted(&aggregate, permit.stamp.generation(), &operation) {
                self.transaction.execute("UPDATE lifecycle_controls SET status='revoked' WHERE id=?1 AND status='prepared'", params![permit.id])?;
                return Ok(false);
            }
            if aggregate.resource() != Some(ResourceEvidence::Available) {
                return Err(Error::StalePermit);
            }
            let changed = self.transaction.execute("UPDATE lifecycle_controls SET status='claimed',claimed_revision=?2 WHERE id=?1 AND status='prepared'", params![permit.id, sql_number(aggregate.revision())?])?;
            if changed != 1 {
                return Err(Error::StalePermit);
            }
            self.claimed_controls.push(ControlAttempt {
                id: permit.id,
                scope: self.scope.clone(),
                revision: aggregate.revision(),
                stamp: permit.stamp,
                control: permit.control,
                operation,
                binding,
            });
            Ok(true)
        })
    }
    /// The attempt is still claimed, on the task's current execution and its unchanged binding.
    pub fn fence_control(&self, attempt: &ControlAttempt) -> Result<()> {
        staging!(self, {
            if attempt.scope != self.scope {
                return Err(Error::StalePermit);
            }
            if self.load(attempt.stamp.task())?.execution() != Some(&attempt.stamp) {
                return Err(Error::StalePermit);
            }
            let claimed: bool = self.transaction.query_row("SELECT EXISTS(SELECT 1 FROM lifecycle_controls WHERE id=?1 AND task_id=?2 AND generation=?3 AND control_id=?4 AND status='claimed' AND claimed_revision=?5)", params![attempt.id, attempt.stamp.task().0, sql_number(attempt.stamp.generation())?, attempt.control.as_str(), sql_number(attempt.revision)?], |row| row.get(0))?;
            if !claimed {
                return Err(Error::StalePermit);
            }
            if provider_binding::for_execution(&self.transaction, &attempt.stamp)? != attempt.binding {
                return Err(Error::BindingConflict);
            }
            provider_binding::require_active(&self.transaction, &attempt.binding)
        })
    }
    /// Consumes the claim and records its exact command under the owner generation; nothing that
    /// can be sent escapes before the commit succeeds.
    pub fn record_owned_control(&mut self, attempt: ControlAttempt, owner_generation: u64, evidence: CommandEvidence) -> Result<()> {
        staging!(self, {
            self.fence_control(&attempt)?;
            self.fence_observation_owner(attempt.execution(), owner_generation)?;
            let generation = sql_number(attempt.stamp.generation())?;
            self.transaction.execute("INSERT INTO provider_control_commands(control_id,task_id,generation,binding_id,digest,bytes) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(control_id) DO NOTHING", params![attempt.id, attempt.stamp.task().0, generation, attempt.binding.id(), evidence_digest(evidence.bytes()), evidence.bytes()])?;
            if self.control_evidence(&attempt)? != evidence.bytes() {
                return Err(Error::CommandConflict);
            }
            self.touched.insert(attempt.stamp.task().clone());
            self.control_commands.push(StagedControl { attempt, owner_generation, evidence });
            Ok(())
        })
    }
    fn control_evidence(&self, attempt: &ControlAttempt) -> Result<Vec<u8>> {
        let row: Option<(Vec<u8>, Vec<u8>)> = self.transaction.query_row("SELECT bytes,digest FROM provider_control_commands WHERE control_id=?1 AND task_id=?2 AND generation=?3 AND binding_id=?4", params![attempt.id, attempt.stamp.task().0, sql_number(attempt.stamp.generation())?, attempt.binding.id()], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
        let (bytes, digest) = row.ok_or(Error::MissingCommand)?;
        if evidence_digest(&bytes) != digest {
            return Err(Error::CorruptJournal);
        }
        Ok(bytes)
    }
    pub(super) fn fence_staged_control(&self, control: &StagedControl) -> Result<()> {
        staging!(self, {
            self.fence_control(&control.attempt)?;
            self.fence_observation_owner(control.attempt.execution(), control.owner_generation)?;
            if self.control_evidence(&control.attempt)? != control.evidence.bytes() {
                return Err(Error::CommandConflict);
            }
            Ok(())
        })
    }
    /// How the host answered the control: `claimed` → `handed`/`rejected`/`uncertain`.
    pub fn record_control_receipt(&mut self, committed: CommittedControl, receipt: DispatchReceipt) -> Result<()> {
        staging!(self, {
            let attempt = &committed.staged.attempt;
            if attempt.scope != self.scope {
                return Err(Error::StalePermit);
            }
            check_epoch(&self.transaction, &self.scope)?;
            let changed = self.transaction.execute("UPDATE lifecycle_controls SET status=?5 WHERE id=?1 AND task_id=?2 AND generation=?3 AND claimed_revision=?4 AND status='claimed'", params![attempt.id, attempt.stamp.task().0, sql_number(attempt.stamp.generation())?, sql_number(attempt.revision)?, status_of(receipt)])?;
            if changed != 1 {
                return Err(Error::StalePermit);
            }
            self.touched.insert(attempt.stamp.task().clone());
            Ok(())
        })
    }
    /// The claimed controls of the task's current execution whose receipt a crash lost, with the
    /// exact bytes the owner recorded, in order. An adopting owner re-sends each to the host.
    pub fn unresolved_controls(&self, task: &TaskId) -> Result<Vec<UnresolvedControl>> {
        staging!(self, {
            let aggregate = self.load(task)?;
            let stamp = aggregate.execution().ok_or(Error::MissingIntent)?;
            let mut statement = self.transaction.prepare("SELECT l.id,c.bytes,c.digest FROM lifecycle_controls l JOIN provider_control_commands c ON c.control_id=l.id AND c.task_id=l.task_id AND c.generation=l.generation WHERE l.task_id=?1 AND l.generation=?2 AND l.status='claimed' ORDER BY l.id")?;
            let rows = statement.query_map(params![task.0, sql_number(stamp.generation())?], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?, row.get::<_, Vec<u8>>(2)?)))?;
            rows.map(|row| {
                let (id, command, digest) = row?;
                if command.is_empty() || command.len() > MAX_PAYLOAD || evidence_digest(&command) != digest {
                    return Err(Error::CorruptJournal);
                }
                Ok(UnresolvedControl { id, command })
            })
            .collect()
        })
    }
    /// Resolves a claimed control of the task's current execution with the host's answer, fenced
    /// by the owner generation it was reconciled under.
    pub fn resolve_control(&mut self, stamp: &ExecutionStamp, id: i64, owner_generation: u64, receipt: DispatchReceipt) -> Result<()> {
        staging!(self, {
            check_epoch(&self.transaction, &self.scope)?;
            self.fence_observation_owner(stamp, owner_generation)?;
            let changed = self.transaction.execute("UPDATE lifecycle_controls SET status=?4 WHERE id=?1 AND task_id=?2 AND generation=?3 AND status='claimed'", params![id, stamp.task().0, sql_number(stamp.generation())?, status_of(receipt)])?;
            if changed != 1 {
                return Err(Error::StalePermit);
            }
            self.touched.insert(stamp.task().clone());
            Ok(())
        })
    }
    pub(super) fn control_permits(&self, aggregate: &TaskLifecycle) -> Result<Vec<ControlPermit>> {
        let Some(stamp) = aggregate.execution().filter(|stamp| through_sdk(stamp).is_ok()) else {
            return Ok(vec![]);
        };
        let mut statement = self.transaction.prepare("SELECT id,control_id FROM lifecycle_controls WHERE task_id=?1 AND generation=?2 AND status='prepared' ORDER BY id")?;
        let rows = statement.query_map(params![aggregate.task().0, sql_number(stamp.generation())?], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?;
        rows.map(|row| {
            let (id, control) = row?;
            Ok(ControlPermit { id, scope: self.scope.clone(), stamp: stamp.clone(), control: OperationId::new(control)? })
        })
        .collect()
    }
    pub(super) fn claimed_control(&self, task: &TaskId) -> Result<bool> {
        Ok(self.transaction.query_row("SELECT EXISTS(SELECT 1 FROM lifecycle_controls WHERE task_id=?1 AND status='claimed')", params![task.0], |row| row.get(0))?)
    }
}
