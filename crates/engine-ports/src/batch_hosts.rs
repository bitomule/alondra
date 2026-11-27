//! The orchestrator's batches, each on the provider its account runs. Every provider whose agent
//! can be the orchestrator adds its own in its `wire`; the orchestrator reaches them only here,
//! with the batch's claim as their owner.
use crate::agent_host::HostStop;
use crate::agent_workers::{Provider, WorkerConfiguration};
use crate::journal::BatchOwner;
use alondra_provider_contract::AdapterError;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

/// The agent an orchestrator batch runs: its model, its tools and MCP servers and nothing else, its
/// plugins and settings. Whether the batch resumes the orchestrator's conversation is `start`'s.
#[derive(Clone, Debug, Serialize)]
pub struct OrchestratorProfile {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub mcp_servers: BTreeMap<String, McpServer>,
    pub plugins: Vec<PathBuf>,
    pub settings: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct McpServer {
    pub command: PathBuf,
    pub args: Vec<String>,
}

/// What the host saw of the batch's turn, in its order.
#[derive(Clone, Debug, PartialEq)]
pub enum HostEvent {
    /// A message of the provider's SDK, exactly as it came.
    Sdk(Value),
    /// What the native wrote to stderr, or why the host could not read the SDK any further. Its
    /// text is for display and logs only.
    Diagnostic { source: DiagnosticSource, text: String },
    /// The native session asked the orchestrator for a decision it never grants.
    UnexpectedDecision,
    /// The native process ended.
    Exited { code: Option<i32>, signal: Option<String> },
}

/// Where a diagnostic came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagnosticSource {
    Stderr,
    /// A failure the host classified, by the surface and code its protocol names.
    Failure { surface: String, code: String },
    Unknown,
}

pub struct PendingHostEvent {
    pub cursor: u64,
    pub event: HostEvent,
    /// Exact bounded JSON bytes validated against this batch's ownership fence.
    pub evidence: Vec<u8>,
}

/// The host of one batch. It takes one prompt, at `start`: a message that arrives during the turn
/// waits for the next batch and its own host.
pub trait BatchHost {
    /// The id the SDK reports back for the batch's prompt: a result that names another is not
    /// this batch's.
    fn input(&self) -> &str;
    /// The host's process id: it leads the process group the native and its MCP servers run in.
    fn process_id(&self) -> u32;
    /// Whether the host has something to read within `timeout`. Waiting decides nothing.
    fn readable(&self, timeout: Duration) -> Result<bool, AdapterError>;
    /// The next thing the host saw, left unacknowledged until the caller committed its report.
    fn next_pending(&mut self) -> Result<PendingHostEvent, AdapterError>;
    fn acknowledge(&mut self, cursor: u64) -> Result<(), AdapterError>;
    fn stopper(&self) -> HostStop;
    /// Stops the host and waits until it has exited.
    fn stop(self: Box<Self>) -> Result<(), AdapterError>;
}

/// One provider's orchestrator batches.
pub trait BatchProvider: Send + Sync {
    fn provider(&self) -> Provider;
    /// Starts a host owned by the batch's claim and hands it the batch's one prompt, as a new turn
    /// of `session` when `resume`. Once this returns the prompt was offered to the SDK; any failure
    /// before it means the native never saw it.
    fn start(&self, owner: BatchOwner, configuration: &WorkerConfiguration, session: &str, resume: bool,
        profile: &OrchestratorProfile, prompt: &str) -> Result<Box<dyn BatchHost>, AdapterError>;
}

/// Every provider the root wired orchestrator batches for, by the provider it is.
#[derive(Default)]
pub struct BatchProviders(RwLock<BTreeMap<Provider, Arc<dyn BatchProvider>>>);

impl BatchProviders {
    /// Adds `provider` to the registry's batch providers, in place of any of the same provider.
    pub fn add(registry: &crate::Registry, provider: Arc<dyn BatchProvider>) {
        let providers = registry.port::<BatchProviders>().unwrap_or_else(|| {
            let providers = Arc::new(BatchProviders::default());
            registry.provide::<BatchProviders>(providers.clone());
            providers
        });
        providers.0.write().unwrap_or_else(PoisonError::into_inner).insert(provider.provider(), provider);
    }

    /// The providers the orchestrator runs on that no adapter wired batches for.
    pub fn missing(&self) -> Vec<Provider> {
        let wired = self.0.read().unwrap_or_else(PoisonError::into_inner);
        Provider::ALL.into_iter().filter(|provider| provider.runs_orchestrator() && !wired.contains_key(provider)).collect()
    }
}

/// The registry's batches of `provider`, or an error naming it when none was wired.
pub fn of(registry: &crate::Registry, provider: Provider) -> std::io::Result<Arc<dyn BatchProvider>> {
    registry.port::<BatchProviders>()
        .and_then(|providers| providers.0.read().unwrap_or_else(PoisonError::into_inner).get(&provider).cloned())
        .ok_or_else(|| std::io::Error::other(format!("no orchestrator batch host for {} is wired", provider.display_name())))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Named(Provider);
    impl BatchProvider for Named {
        fn provider(&self) -> Provider { self.0 }
        fn start(&self, _: BatchOwner, _: &WorkerConfiguration, _: &str, _: bool, _: &OrchestratorProfile, _: &str) -> Result<Box<dyn BatchHost>, AdapterError> {
            Err(AdapterError::Unsupported)
        }
    }

    #[test]
    fn a_provider_nobody_wired_is_named_and_each_wired_one_is_its_own() {
        let registry = crate::Registry::default();
        let missing = of(&registry, Provider::Codex).err().unwrap().to_string();
        assert!(missing.contains("Codex"), "{missing}");
        BatchProviders::add(&registry, Arc::new(Named(Provider::Claude)));
        assert_eq!(of(&registry, Provider::Claude).unwrap().provider(), Provider::Claude);
        let missing = of(&registry, Provider::Codex).err().unwrap().to_string();
        assert!(missing.contains("Codex"), "{missing}");
        BatchProviders::add(&registry, Arc::new(Named(Provider::Codex)));
        assert_eq!(of(&registry, Provider::Codex).unwrap().provider(), Provider::Codex);
    }

    #[test]
    fn the_registry_names_each_orchestrator_provider_left_without_batches() {
        let registry = crate::Registry::default();
        assert!(registry.missing().contains(&"BatchProviders"));
        BatchProviders::add(&registry, Arc::new(Named(Provider::Codex)));
        let missing = registry.missing();
        assert!(!missing.contains(&"BatchProviders") && !missing.contains(&"BatchProviders: Codex"), "{missing:?}");
        assert!(missing.contains(&"BatchProviders: Claude"), "{missing:?}");
        assert!(!missing.contains(&"BatchProviders: Antigravity"), "the orchestrator never runs on Antigravity: {missing:?}");
    }
}
