use alondra_domain::{Event, Refusal, TaskLifecycle};
use alondra_foundation::{
    ConversationId, ExecutionStamp, HostId, IdentityError, InputId, InstanceId, TaskId,
};

#[test]
fn domain_consumes_the_same_foundation_values_without_identity_conversion() {
    let input: alondra_domain::InputId = InputId::new("non-uuid-input").unwrap();
    let mut task = TaskLifecycle::planned(TaskId::from("task"), input.clone()).unwrap();
    task.apply(
        0,
        Event::Admit {
            node: "node".into(),
            conversation: ConversationId::new("conversation").unwrap(),
            instance: InstanceId::new("instance").unwrap(),
            host: HostId::new("logical-host").unwrap(),
        },
    )
    .unwrap();
    let stamp: &ExecutionStamp = task.execution().unwrap();
    assert_eq!(stamp.task(), &TaskId::from("task"));
    assert_eq!(stamp.instance().as_str(), "instance");
    assert_eq!(task.pending_inputs(), vec![&input]);
    let same: &alondra_domain::ExecutionStamp = stamp;
    assert_eq!(same.generation(), 1);
    assert_eq!(
        Refusal::from(IdentityError::EmptyIdentity),
        Refusal::EmptyIdentity
    );
    assert_eq!(
        Refusal::from(IdentityError::InvalidGeneration),
        Refusal::InvalidState
    );
}
