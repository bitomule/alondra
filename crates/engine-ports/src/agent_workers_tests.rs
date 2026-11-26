use super::*;
use crate::Registry;

struct Named(Provider);

impl AgentProvider for Named {
    fn provider(&self) -> Provider { self.0 }
    fn configured(&self, _: ProviderBinding, _: WorkerConfiguration) -> Result<Box<dyn AgentWorker>, AdapterError> { Err(AdapterError::Unsupported) }
    fn conversation_session(&self, _: &[u8]) -> Option<String> { None }
    fn answered_by_event(&self, _: &[u8]) -> Option<CommandId> { None }
    fn answered_control(&self, _: &OrderedObservation) -> Option<CommandId> { None }
    fn native_exited(&self, _: &OrderedObservation) -> bool { false }
    fn background_left(&self, _: &[u8]) -> Vec<String> { Vec::new() }
    fn sdk_message(&self, _: &OrderedObservation) -> Option<serde_json::Value> { None }
    fn native_message(&self, _: &OrderedObservation) -> Option<serde_json::Value> { None }
    fn signed_out(&self, _: &OrderedObservation) -> bool { false }
    fn session_failure(&self, _: &OrderedObservation) -> Option<String> { None }
    fn account_refusal(&self, _: &OrderedObservation) -> Option<&'static str> { None }
    fn blocked_policy(&self, _: &[u8]) -> Option<String> { None }
}

#[test]
fn a_provider_nothing_wired_is_refused_by_name() {
    let registry = Registry::default();
    assert_eq!(of(&registry, Provider::Codex).err().unwrap().to_string(), "no provider for Codex is wired");
    AgentProviders::add(&registry, Arc::new(Named(Provider::Claude)));
    assert_eq!(of(&registry, Provider::Claude).unwrap().provider(), Provider::Claude);
    assert_eq!(of(&registry, Provider::Antigravity).err().unwrap().to_string(), "no provider for Antigravity is wired");
}

#[test]
fn the_registry_names_each_provider_left_unwired() {
    let registry = Registry::default();
    assert!(registry.missing().contains(&"AgentProviders"));
    AgentProviders::add(&registry, Arc::new(Named(Provider::Codex)));
    let missing = registry.missing();
    assert!(!missing.contains(&"AgentProviders"));
    assert!(missing.contains(&"AgentProviders: Claude") && missing.contains(&"AgentProviders: Antigravity"), "{missing:?}");
    assert!(!missing.contains(&"AgentProviders: Codex"), "{missing:?}");
}
