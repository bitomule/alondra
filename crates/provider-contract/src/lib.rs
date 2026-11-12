#![forbid(unsafe_code)]
//! Technical worker execution contract. Business decisions and dispatch authority stay in the application.
use alondra_foundation::{CommandId, ContinuationId, ExecutionStamp, InputId, TurnId, WorkId};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidBinding;
fn text(value: &str) -> Result<(), InvalidBinding> {
    if value.trim().is_empty() || value.len() > 256 || value.contains('\0') {
        Err(InvalidBinding)
    } else {
        Ok(())
    }
}
fn number(value: u64) -> Result<(), InvalidBinding> {
    if value == 0 || value > alondra_foundation::MAX_REVISION {
        Err(InvalidBinding)
    } else {
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    OfferInput,
    Interrupt,
    BackgroundWork,
    DurableResume,
    SetModel,
    SetPermissionMode,
    Compact,
    McpStatus,
    Decisions,
    McpReconnect,
    McpSignIn,
    /// Ends Claude's process when a turn stays open after it accepted the interrupt.
    ForceStop,
    /// Has Claude forget, and revoke, its stored sign-in to an MCP server.
    McpClearAuth,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderBinding {
    id: String,
    driver: String,
    instance: String,
    configuration: String,
    configuration_revision: u64,
    capability_revision: u64,
    capabilities: BTreeSet<Capability>,
    credential: String,
    credential_generation: u64,
}
impl ProviderBinding {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        driver: String,
        instance: String,
        configuration: String,
        configuration_revision: u64,
        capability_revision: u64,
        capabilities: BTreeSet<Capability>,
        credential: String,
        credential_generation: u64,
    ) -> Result<Self, InvalidBinding> {
        for value in [&id, &driver, &instance, &configuration, &credential] {
            text(value)?;
        }
        for value in [
            configuration_revision,
            capability_revision,
            credential_generation,
        ] {
            number(value)?;
        }
        Ok(Self {
            id,
            driver,
            instance,
            configuration,
            configuration_revision,
            capability_revision,
            capabilities,
            credential,
            credential_generation,
        })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn driver(&self) -> &str {
        &self.driver
    }
    pub fn instance(&self) -> &str {
        &self.instance
    }
    pub fn configuration(&self) -> &str {
        &self.configuration
    }
    pub fn configuration_revision(&self) -> u64 {
        self.configuration_revision
    }
    pub fn capability_revision(&self) -> u64 {
        self.capability_revision
    }
    pub fn capabilities(&self) -> &BTreeSet<Capability> {
        &self.capabilities
    }
    pub fn credential(&self) -> &str {
        &self.credential
    }
    pub fn credential_generation(&self) -> u64 {
        self.credential_generation
    }
    pub fn permits(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    LocalDirectory(String),
    RemoteWorkspace(String),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Launch,
    Input,
}
/// Correlation data is not dispatch authority. The application privately owns the submitting adapter.
pub struct Invocation<'a> {
    pub binding: &'a ProviderBinding,
    pub execution: &'a ExecutionStamp,
    pub command: CommandId,
    pub input: &'a InputId,
    pub text: &'a str,
    pub operation: Operation,
    pub placement: Option<&'a Placement>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handoff {
    Offered,
    RefusedBeforeOffer,
    Uncertain,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observation {
    Available,
    TurnStarted {
        input: InputId,
        turn: TurnId,
    },
    /// Work that outlived its turn woke Claude, which took up its `continuation` in a root turn of
    /// its own, with no input of Alondra's.
    TurnResumed {
        continuation: ContinuationId,
        turn: TurnId,
    },
    /// Claude opened a root turn by itself, with no input of Alondra's and no work that woke it:
    /// a message a subagent handed back after the last turn ended.
    TurnUnprompted {
        turn: TurnId,
    },
    InputConsumed {
        input: InputId,
    },
    /// The provider named `input` as held: its own evidence says it has it and will read it.
    InputHeld {
        input: InputId,
    },
    /// Claude finished or cancelled an input of Alondra's without taking it in, or answered it
    /// with a result that never delivered it: it will not read it.
    InputDropped {
        input: InputId,
    },
    WorkStarted {
        turn: TurnId,
        work: WorkId,
    },
    WorkEnded {
        work: WorkId,
        continuation: ContinuationId,
    },
    /// The root turn that started the work is still open when it ends: that same turn goes on with
    /// the work's continuation, so no later turn waits for it.
    ContinuationConsumed {
        continuation: ContinuationId,
    },
    /// Live native work needs a model follow-up through the domain's durable input queue.
    ContinuationRequested { work: WorkId, activation: String, text: String },
    /// The root turn ended, and how.
    TurnEnded {
        turn: TurnId,
        outcome: TurnOutcome,
    },
    /// The provider said, as a whole, what runs now: no root turn when `idle`, and of its work
    /// only `live`. Whatever the task's record holds beyond that ended, though the facts that
    /// would have said so were lost or never sent.
    Reconciled {
        idle: bool,
        live: BTreeSet<WorkId>,
    },
    /// The model answered the root turn: the API and the account Claude Code uses work.
    ModelAnswered,
    TransportLost,
    ResourceReleased,
    /// Claude asks for a decision. `epoch` is the host's journal cursor of the request, unique in
    /// its execution; `tool_use` names the tool call it is about, absent for an MCP server's
    /// request for input; `payload` is the request exactly as the host persisted it.
    Asked {
        request: String,
        epoch: u64,
        kind: DecisionKind,
        tool_use: Option<String>,
        payload: Vec<u8>,
    },
    /// Claude Code made or entered the git worktree at `path`: with EnterWorktree, or for a
    /// subagent (`subagent`) launched with an isolated worktree that it kept.
    WorktreeMade {
        path: String,
        branch: Option<String>,
        subagent: bool,
    },
    /// A subagent with an isolated worktree went on in the background: Claude Code names its
    /// worktree `agent-<agent>` in the repository's `.claude/worktrees`.
    SubagentWorktree {
        agent: String,
    },
    /// A command of the node or a subagent opened this pull request with `gh pr create`.
    PullRequestOpened(OpenedPullRequest),
    /// A command ran `gh pr create` and finished well, and its output named no pull request.
    PullRequestNotNamed,
    /// The request at `epoch` is over: `answered` when a decision resolved it, otherwise it was
    /// aborted, closed or its decision could not be delivered. `continued` when the root turn that
    /// asked is still open, so an answered request lets that same turn go on.
    DecisionSettled {
        epoch: u64,
        answered: bool,
        continued: bool,
    },
    /// The MCP servers Claude listed and how each is: as each prompt starts, or as a control of
    /// Alondra's answered.
    McpServers {
        servers: Vec<McpServerReport>,
    },
    /// How the sign-in Claude opened to `server` goes on.
    McpSignIn {
        server: String,
        sign_in: McpSignInReport,
    },
    /// A call to an MCP server's tool failed: how its server is may have changed.
    McpToolFailed,
    /// What the provider said of its account's usage: about the account of the binding that
    /// produced the observation.
    Usage(UsageReport),
}
/// Which window of the account's usage limits a reading is about: the provider's short session
/// window, its week, or another it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsageWindowKind {
    Session,
    Weekly,
    Other(String),
}
/// One usage window as the provider reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageWindow {
    pub kind: UsageWindowKind,
    /// The share used, in ten-thousandths: 0 is none and 10 000 all of it.
    pub used: u16,
    /// When the window resets, in Unix milliseconds.
    pub resets_at: Option<i64>,
    /// The provider refused work because this window ran out.
    pub exhausted: bool,
}
/// Some of the account's windows and, when the provider said it, the account's plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageReport {
    pub windows: Vec<UsageWindow>,
    pub plan: Option<String>,
}
/// An MCP server as the provider reported it; its status verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerReport {
    pub name: String,
    pub status: String,
    pub error: Option<String>,
    pub source: Option<String>,
    pub transport: Option<String>,
    /// A remote server's address, without credentials, query or fragment.
    pub url: Option<String>,
    /// Whose it is, where the provider found it and how the user connects it.
    pub traits: alondra_foundation::McpTraits,
}
/// How a sign-in Claude opened goes on: where the user signs in, and the port and state of
/// Claude's own listener the browser comes back to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpSignInReport {
    Browser {
        url: String,
        callback_port: Option<u16>,
        state: Option<String>,
    },
    Connected,
    Refused {
        error: String,
    },
}
/// What Claude asks a decision about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DecisionKind {
    Permission,
    Question,
    Plan,
    Elicitation,
}
/// How a root turn ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed,
    /// A Claude API error ended it, past Claude Code's own retries.
    ApiFailed(ApiFailure),
    /// Claude Code ended it as failed with `code`, such as `error_max_turns`, known or not.
    Failed { code: String },
}
/// The Claude API error a root turn ended with, as Claude Code reports it after its own retries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiFailure {
    /// Claude Code's error category, such as `authentication_failed` or `rate_limit`.
    pub error: String,
    /// The HTTP status of the failed request, when one was answered.
    pub status: Option<u16>,
    /// When a rejected usage limit resets, in Unix seconds.
    pub resets_at: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderedObservation {
    pub owner_generation: u64,
    pub execution: ExecutionStamp,
    pub cursor: u64,
    pub evidence: Vec<u8>,
}
#[derive(Debug)]
pub struct PreparedCommand {
    bytes: Vec<u8>,
}
impl PreparedCommand {
    pub fn new(bytes: Vec<u8>) -> Result<Self, AdapterError> {
        if bytes.is_empty() || bytes.len() > 1_048_576 {
            return Err(AdapterError::Protocol("command capacity".into()));
        }
        Ok(Self { bytes })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
#[derive(Debug)]
pub enum AdapterError {
    Disconnected,
    InvalidObservation,
    Unsupported,
    /// The observation names an input Alondra never handed over: the native process's own.
    UnknownInput,
    Storage(String),
    Protocol(String),
}
/// Production implementations keep their endpoint and launch/ownership credentials private.
pub trait Adapter {
    type NativeInput;
    fn binding(&self) -> &ProviderBinding;
    fn prepare(
        &mut self,
        invocation: Invocation<'_>,
        native: Self::NativeInput,
    ) -> Result<PreparedCommand, AdapterError>;
    fn submit(
        &mut self,
        invocation: Invocation<'_>,
        command: &PreparedCommand,
    ) -> Result<Handoff, AdapterError>;
    fn next(&mut self) -> Result<Option<OrderedObservation>, AdapterError>;
    fn acknowledge(&mut self, execution: &ExecutionStamp, cursor: u64) -> Result<(), AdapterError>;
}

/// Where a pull request `gh pr create` printed lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedPullRequest {
    pub host: String,
    pub repository: String,
    pub number: u64,
}

/// Whether a shell command line runs `gh pr create`, also inside `sh -c '...'`. It only says that
/// the output deserves a look: the pull request itself is what GitHub says of the address printed.
pub fn runs_gh_pr_create(command: &str) -> bool {
    let words: Vec<&str> = command.split(|c: char| c.is_whitespace() || "'\"`()".contains(c)).filter(|word| !word.is_empty()).collect();
    words.iter().enumerate().filter(|(_, word)| **word == "gh").any(|(at, _)| {
        let call: Vec<&str> = words[at + 1..].iter().copied().take_while(|word| !word.contains([';', '|', '&'])).collect();
        call.windows(2).any(|pair| pair == ["pr", "create"])
    })
}

/// The pull requests a command's output names by their address, each once, in order.
pub fn pull_requests_named(output: &str) -> Vec<OpenedPullRequest> {
    let mut found: Vec<OpenedPullRequest> = Vec::new();
    for word in output.split(|c: char| c.is_whitespace() || "()<>[]\"'`".contains(c)) {
        if let Some(pr) = pull_request_at(word.trim_end_matches(['.', ',', ';', ':'])).filter(|pr| !found.contains(pr)) {
            found.push(pr);
        }
    }
    found
}

fn pull_request_at(address: &str) -> Option<OpenedPullRequest> {
    let (host, path) = address.strip_prefix("https://")?.split_once('/')?;
    let parts: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    let [owner, name, "pull", number] = parts.as_slice() else { return None };
    let plain = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if !host.contains('.') || !plain(host) || !plain(owner) || !plain(name) {
        return None;
    }
    Some(OpenedPullRequest { host: host.to_owned(), repository: format!("{owner}/{name}"), number: number.parse().ok().filter(|number| *number > 0)? })
}

#[cfg(test)]
mod pull_request_tests {
    use super::*;

    #[test]
    fn a_command_line_runs_gh_pr_create_however_it_is_written() {
        for command in ["gh pr create --title x", "cd ../cloud && gh pr create -t x | tail -1", "bash -lc 'gh pr create --fill'", "gh --repo o/r pr create --fill"] {
            assert!(runs_gh_pr_create(command), "{command}");
        }
        for command in ["gh pr view 12", "gh pr list; git create", "gh issue create", "echo pr create"] {
            assert!(!runs_gh_pr_create(command), "{command}");
        }
    }

    #[test]
    fn only_a_pull_request_address_is_named_and_each_once() {
        let output = "Creating pull request\nhttps://github.com/acme/storefront/pull/6\nsee https://github.com/acme/storefront/pull/6.\nhttps://github.com/o/r/issues/2 https://example/o/r/pull/3 https://ghe.corp.io/O/R/pull/4/";
        assert_eq!(pull_requests_named(output), vec![
            OpenedPullRequest { host: "github.com".into(), repository: "acme/storefront".into(), number: 6 },
            OpenedPullRequest { host: "ghe.corp.io".into(), repository: "O/R".into(), number: 4 },
        ]);
    }
}

/// How a turn ends when its provider went idle and no result of it ever came.
pub const ENDED_WITHOUT_RESULT: &str = "ended_without_result";

/// The notification of `work`, which ended with no word of its own: the provider's whole list of
/// what runs no longer named it.
pub fn reconciled_continuation(work: &WorkId) -> ContinuationId {
    let provider = if work.as_str().starts_with("codex:") { "codex" } else { "claude" };
    ContinuationId::new(format!("{provider}-reconciled:{}", work.as_str())).expect("a work id prefixed stays a valid continuation id")
}
