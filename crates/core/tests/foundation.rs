use alondra_core::{domain, views};
use alondra_foundation as shared;

macro_rules! same_type {
    ($($name:ident),+ $(,)?) => {$ (
        const _: fn(shared::$name) -> domain::$name = |value| value;
        const _: fn(domain::$name) -> shared::$name = |value| value;
    )+};
}
same_type!(TaskId, AgentId, InteractionId, DocumentId, DocumentRevisionId, PrId,
    RequestId, AuthorizationId, CommandId, DataEpoch, ScheduleId, ScheduleRunId, Revision);
const _: fn(shared::Lang) -> views::Lang = |value| value;
const _: fn(shared::Millis) -> alondra_core::Millis = |value| value;

#[test]
fn shared_identifiers_work_in_existing_domain_payloads_without_conversion() {
    let id: shared::TaskId = domain::TaskId::from("task-a");
    let scope = domain::Scope::Task { task_id: id };
    assert_eq!(serde_json::to_string(&scope).unwrap(), r#"{"kind":"task","task_id":"task-a"}"#);
    let agent: shared::AgentId = "agent-a".into();
    let actor = domain::Actor::Agent { agent_id: agent };
    assert_eq!(serde_json::to_string(&actor).unwrap(), r#"{"kind":"agent","agent_id":"agent-a"}"#);
    assert_eq!(domain::MAX_REVISION, shared::MAX_REVISION);
}
