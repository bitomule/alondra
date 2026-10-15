use alondra_foundation::*;

#[test]
fn opaque_identities_reject_empty_values_and_preserve_non_native_spelling() {
    macro_rules! check {
        ($($identity:ident),+ $(,)?) => {$(
            for value in ["", " \t\n", "\u{a0}\u{2003}"] {
                assert_eq!($identity::new(value), Err(IdentityError::EmptyIdentity));
            }
            for value in ["remote:input/a", "not-a-uuid", " identifier ", "工作", "0"] {
                assert_eq!($identity::new(value).unwrap().as_str(), value);
            }
        )+};
    }
    check!(
        ConversationId,
        InstanceId,
        HostId,
        InputId,
        TurnId,
        WorkId,
        ContinuationId,
        CallbackId,
        OperationId
    );
}

fn stamp(task: &str, node: &str, generation: u64) -> Result<ExecutionStamp, IdentityError> {
    ExecutionStamp::new(
        task.into(),
        node.into(),
        ConversationId::new("conversation").unwrap(),
        InstanceId::new("instance").unwrap(),
        HostId::new("logical-host").unwrap(),
        generation,
    )
}

#[test]
fn execution_correlation_requires_valid_owner_and_generation() {
    for (task, node) in [("", "node"), ("task", " \t")] {
        assert_eq!(stamp(task, node, 1), Err(IdentityError::EmptyIdentity));
    }
    for generation in [0, MAX_REVISION + 1, u64::MAX] {
        assert_eq!(
            stamp("task", "node", generation),
            Err(IdentityError::InvalidGeneration)
        );
    }
    for generation in [1, MAX_REVISION] {
        let value = stamp("task", "node", generation).unwrap();
        assert_eq!(value.task(), &TaskId::from("task"));
        assert_eq!(value.node(), &AgentId::from("node"));
        assert_eq!(value.conversation().as_str(), "conversation");
        assert_eq!(value.instance().as_str(), "instance");
        assert_eq!(value.host().as_str(), "logical-host");
        assert_eq!(value.generation(), generation);
    }
}
