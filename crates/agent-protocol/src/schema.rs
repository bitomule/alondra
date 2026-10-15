use crate::{Error, MAX_FRAME_BYTES, MAX_SAFE_INTEGER, PROTOCOL_VERSION, SDK_VERSION};
use alondra_foundation::{AgentId, CommandId, DataEpoch, MessageUuid, RequestId, TaskId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub protocol_version: u16,
    pub body: Body,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Body {
    Hello(Hello),
    Command(Command),
    Receipt(Receipt),
    Event(Event),
    Challenge(Challenge),
    Attach(Attach),
    Attached(Attached),
    Acknowledge(Acknowledge),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fence {
    pub profile_id: String,
    pub account_id: String,
    pub data_epoch: DataEpoch,
    pub host_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub conversation_id: String,
    pub process_instance_id: String,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub protocol_versions: Vec<u16>,
    pub capabilities: Vec<String>,
    pub fence: Fence,
    pub host_release: String,
    pub host_sha256: String,
    pub sdk_version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Challenge {
    pub fence: Fence,
    pub execution: Execution,
    pub host_instance_id: String,
    pub nonce: String,
    pub executable_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attach {
    pub fence: Fence,
    pub execution: Execution,
    pub host_instance_id: String,
    pub nonce: String,
    pub owner_generation: u64,
    pub host_release: String,
    pub host_sha256: String,
    pub executable_sha256: String,
    pub sdk_version: String,
    pub proof: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attached {
    pub fence: Fence,
    pub execution: Execution,
    pub host_instance_id: String,
    pub owner_generation: u64,
    pub proof: String,
}

/// Technical journal acknowledgement, never an SDK input-consumption fact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Acknowledge {
    pub fence: Fence,
    pub execution: Execution,
    pub host_instance_id: String,
    pub owner_generation: u64,
    pub cursor: u64,
}

/// Expectations come from the owned host manifest, never from the peer's hello.
pub struct HostExpectation<'a> {
    pub fence: &'a Fence,
    pub host_release: &'a str,
    pub host_sha256: &'a str,
    pub required_capabilities: &'a [&'a str],
}

impl Hello {
    pub fn negotiate(&self, expected: &HostExpectation<'_>) -> Result<u16, Error> {
        self.validate()?;
        if self.fence != *expected.fence {
            return Err(Error::FenceMismatch);
        }
        if self.host_release != expected.host_release
            || self.host_sha256 != expected.host_sha256
            || self.sdk_version != SDK_VERSION
        {
            return Err(Error::HostMismatch);
        }
        if !self.protocol_versions.contains(&PROTOCOL_VERSION) {
            return Err(Error::UnsupportedVersion);
        }
        if expected
            .required_capabilities
            .iter()
            .any(|required| !self.capabilities.iter().any(|c| c == required))
        {
            return Err(Error::MissingCapability);
        }
        Ok(PROTOCOL_VERSION)
    }

    fn validate(&self) -> Result<(), Error> {
        self.fence.validate()?;
        text(&self.host_release, 256)?;
        digest(&self.host_sha256)?;
        text(&self.sdk_version, 64)?;
        if self.protocol_versions.is_empty()
            || self.protocol_versions.len() > 16
            || self.protocol_versions.iter().any(|v| *v == 0)
        {
            return Err(Error::Invalid("protocol versions"));
        }
        if self.capabilities.len() > 32 {
            return Err(Error::Invalid("capabilities"));
        }
        for capability in &self.capabilities {
            text(capability, 64)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub fence: Fence,
    pub execution: Execution,
    pub command_id: CommandId,
    pub deadline_ms: u64,
    pub action: Action,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Start {
        cwd: String,
        input_id: MessageUuid,
        prompt: String,
    },
    Offer {
        input_id: MessageUuid,
        prompt: String,
    },
    Interrupt,
    Close,
    Respond {
        request_id: RequestId,
        decision: Decision,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "behavior", rename_all = "snake_case", deny_unknown_fields)]
pub enum Decision {
    Allow {
        #[serde(deserialize_with = "crate::json::object")]
        updated_input: serde_json::Map<String, Value>,
    },
    Deny {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub fence: Fence,
    pub execution: Execution,
    pub command_id: CommandId,
    pub command_sha256: String,
    pub outcome: ReceiptOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReceiptOutcome {
    Accepted { phase: ReceiptPhase },
    Rejected { code: String, message: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptPhase {
    Admitted,
    Offered,
    Applied,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub fence: Fence,
    pub execution: Execution,
    /// Sequence is scoped to this execution, not to all sessions in the host.
    pub cursor: u64,
    pub observation: Observation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Observation {
    Sdk {
        kind: SdkEventKind,
        native: NativeCorrelation,
        #[serde(deserialize_with = "crate::json::value")]
        payload: Value,
        failures: Vec<SdkFailure>,
    },
    DecisionRequested {
        request_id: RequestId,
        tool_use_id: String,
        kind: DecisionKind,
        #[serde(deserialize_with = "crate::json::value")]
        payload: Value,
    },
    ProcessExited {
        #[serde(deserialize_with = "required_nullable")]
        exit_code: Option<i32>,
        #[serde(deserialize_with = "required_nullable")]
        signal: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SdkEventKind {
    Initialized,
    Message,
    Result,
    Task,
    Status,
    Diagnostic,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Permission,
    Question,
    Plan,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCorrelation {
    #[serde(
        default,
        deserialize_with = "present_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub message_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "present_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub turn_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "present_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_use_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "present_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_tool_use_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "present_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub task_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "present_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub run_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SdkFailure {
    pub surface: FailureSurface,
    /// Native code retained verbatim, including new codes this release does not know.
    pub code: String,
    #[serde(deserialize_with = "crate::json::value")]
    pub details: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureSurface {
    Assistant,
    Startup,
    Result,
    Terminal,
    Exit,
    Hook,
    RateLimit,
    Settings,
    Mirror,
    Auth,
    Control,
    Iterator,
    Transport,
}

impl SdkFailure {
    pub fn is_known(&self) -> bool {
        known_codes(self.surface).contains(&self.code.as_str())
    }
}

/// Closed SDK error unions from the inspected 0.3.300 declarations. Open exception
/// surfaces use adapter codes, retaining the original exception in details.
pub fn known_codes(surface: FailureSurface) -> &'static [&'static str] {
    match surface {
        FailureSurface::Assistant => &[
            "authentication_failed",
            "oauth_org_not_allowed",
            "account_on_hold",
            "verification_required",
            "billing_error",
            "rate_limit",
            "overloaded",
            "invalid_request",
            "model_not_found",
            "server_error",
            "unknown",
            "max_output_tokens",
            "cloud_credential_error",
        ],
        FailureSurface::Startup => &[
            "org_pin_api_key_conflict",
            "provider_not_allowed",
            "org_verify_failed",
            "org_pin_mismatch",
            "managed_settings_invalid",
            "remote_settings_required_unavailable",
            "gateway_signin_required",
            "gateway_access_denied",
            "proxy_invalid",
            "temp_dir_unusable",
            "cwd_unavailable",
            "shell_tool_missing",
            "session_held_by_background",
            "worktree_resume_refused",
            "worktree_unverified",
            "cli_version_too_old",
            "bypass_root",
            "org_config_required_unavailable",
            "org_config_refused",
        ],
        FailureSurface::Result => &[
            "error_during_execution",
            "error_max_turns",
            "error_max_budget_usd",
            "error_max_structured_output_retries",
            "success_is_error",
        ],
        FailureSurface::Terminal => &[
            "blocking_limit",
            "rapid_refill_breaker",
            "prompt_too_long",
            "image_error",
            "model_error",
            "api_error",
            "malformed_tool_use_exhausted",
            "aborted_streaming",
            "aborted_tools",
            "stop_hook_prevented",
            "hook_stopped",
            "tool_deferred",
            "max_turns",
            "background_requested",
            "completed",
            "budget_exhausted",
            "structured_output_retry_exhausted",
            "tool_deferred_unavailable",
            "turn_setup_failed",
        ],
        FailureSurface::Exit => &["clear", "resume", "logout", "prompt_input_exit", "other"],
        FailureSurface::Hook => &["success", "error", "cancelled"],
        FailureSurface::RateLimit => &["allowed", "allowed_warning", "rejected"],
        FailureSurface::Settings => &["settings_parse_error", "permission_rules_error"],
        FailureSurface::Mirror => &["mirror_error"],
        FailureSurface::Auth => &["auth_status_error"],
        FailureSurface::Control => &["control_rejected", "callback_error", "aborted"],
        FailureSurface::Iterator => &["iterator_error", "eof_without_result"],
        FailureSurface::Transport => &["malformed_sdk_message", "unknown_sdk_message"],
    }
}

impl Frame {
    pub fn new(body: Body) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            body,
        }
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(Error::UnsupportedVersion);
        }
        match &self.body {
            Body::Hello(hello) => hello.validate(),
            Body::Command(command) => command.validate(),
            Body::Receipt(receipt) => receipt.validate(),
            Body::Event(event) => event.validate(),
            Body::Challenge(challenge) => challenge.validate(),
            Body::Attach(attach) => attach.validate(),
            Body::Attached(attached) => attached.validate(),
            Body::Acknowledge(acknowledge) => acknowledge.validate(),
        }
    }
}

impl Challenge {
    pub fn validate(&self) -> Result<(), Error> {
        self.fence.validate()?;
        self.execution.validate()?;
        text(&self.host_instance_id, 256)?;
        digest(&self.nonce)?;
        digest(&self.executable_sha256)
    }
}
impl Attach {
    pub fn validate(&self) -> Result<(), Error> {
        self.validate_claim()?;
        digest(&self.proof)
    }

    pub(crate) fn validate_claim(&self) -> Result<(), Error> {
        self.fence.validate()?;
        self.execution.validate()?;
        text(&self.host_instance_id, 256)?;
        digest(&self.nonce)?;
        positive(self.owner_generation)?;
        text(&self.host_release, 256)?;
        digest(&self.host_sha256)?;
        digest(&self.executable_sha256)?;
        text(&self.sdk_version, 64)
    }
}
impl Attached {
    pub fn validate(&self) -> Result<(), Error> {
        self.fence.validate()?;
        self.execution.validate()?;
        text(&self.host_instance_id, 256)?;
        positive(self.owner_generation)?;
        digest(&self.proof)
    }
}
impl Acknowledge {
    pub fn validate(&self) -> Result<(), Error> {
        self.fence.validate()?;
        self.execution.validate()?;
        text(&self.host_instance_id, 256)?;
        positive(self.owner_generation)?;
        positive(self.cursor)
    }
}

impl Fence {
    pub fn validate(&self) -> Result<(), Error> {
        for field in [
            &self.profile_id,
            &self.account_id,
            &self.data_epoch.0,
            &self.host_id,
        ] {
            text(field, 256)?;
        }
        Ok(())
    }
}
impl Execution {
    pub fn validate(&self) -> Result<(), Error> {
        for field in [
            &self.task_id.0,
            &self.agent_id.0,
            &self.conversation_id,
            &self.process_instance_id,
        ] {
            text(field, 256)?;
        }
        positive(self.generation)
    }
}
impl Command {
    pub fn validate(&self) -> Result<(), Error> {
        self.fence.validate()?;
        self.execution.validate()?;
        text(&self.command_id.0, 256)?;
        positive(self.deadline_ms)?;
        match &self.action {
            Action::Start {
                cwd,
                input_id: _,
                prompt,
            } => {
                text(cwd, 4096)?;
                if !cwd.starts_with('/') {
                    return Err(Error::Invalid("absolute cwd required"));
                }
                text(prompt, MAX_FRAME_BYTES)?;
            }
            Action::Offer {
                input_id: _,
                prompt,
            } => {
                text(prompt, MAX_FRAME_BYTES)?;
            }
            Action::Respond {
                request_id,
                decision,
            } => {
                text(&request_id.0, 256)?;
                if let Decision::Deny { message } = decision {
                    text(message, MAX_FRAME_BYTES)?;
                }
            }
            Action::Interrupt | Action::Close => {}
        }
        Ok(())
    }
}
impl Receipt {
    pub fn validate(&self) -> Result<(), Error> {
        self.fence.validate()?;
        self.execution.validate()?;
        text(&self.command_id.0, 256)?;
        digest(&self.command_sha256)?;
        if let ReceiptOutcome::Rejected { code, message } = &self.outcome {
            text(code, 256)?;
            text(message, MAX_FRAME_BYTES)?;
        }
        Ok(())
    }
}
impl Event {
    pub fn validate(&self) -> Result<(), Error> {
        self.fence.validate()?;
        self.execution.validate()?;
        positive(self.cursor)?;
        match &self.observation {
            Observation::Sdk {
                native,
                payload,
                failures,
                ..
            } => {
                if !payload.is_object() {
                    return Err(Error::Invalid("SDK object required"));
                }
                for field in [
                    &native.message_id,
                    &native.turn_id,
                    &native.tool_use_id,
                    &native.parent_tool_use_id,
                    &native.task_id,
                    &native.run_id,
                ]
                .into_iter()
                .flatten()
                {
                    text(field, 256)?;
                }
                if failures.len() > 32 {
                    return Err(Error::Invalid("too many failures"));
                }
                for failure in failures {
                    text(&failure.code, 256)?;
                }
            }
            Observation::DecisionRequested {
                request_id,
                tool_use_id,
                payload,
                ..
            } => {
                text(&request_id.0, 256)?;
                text(tool_use_id, 256)?;
                if !payload.is_object() {
                    return Err(Error::Invalid("decision object required"));
                }
            }
            Observation::ProcessExited { signal, .. } => {
                if let Some(signal) = signal {
                    text(signal, 64)?;
                }
            }
        }
        Ok(())
    }

    /// An unknown observation is preserved as evidence but cannot authorize effects.
    /// Even supported events still require domain validation after durable storage.
    pub fn validate_for_application(&self) -> Result<(), Error> {
        self.validate()?;
        if let Observation::Sdk { kind, failures, .. } = &self.observation
            && (*kind == SdkEventKind::Unsupported
                || failures.iter().any(|failure| !failure.is_known()))
        {
            return Err(Error::UnsupportedSdk);
        }
        Ok(())
    }
}

fn present_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

fn required_nullable<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}

fn text(value: &str, max: usize) -> Result<(), Error> {
    if value.is_empty() || value.len() > max || value.contains('\0') {
        return Err(Error::Invalid("string bound"));
    }
    Ok(())
}
fn positive(value: u64) -> Result<(), Error> {
    if value == 0 || value > MAX_SAFE_INTEGER {
        return Err(Error::Invalid("safe positive integer"));
    }
    Ok(())
}
fn digest(value: &str) -> Result<(), Error> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(Error::Invalid("SHA256"));
    }
    Ok(())
}
