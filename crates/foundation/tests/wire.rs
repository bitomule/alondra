use alondra_foundation::*;

#[test]
fn identifiers_keep_their_transparent_wire_contract() {
    macro_rules! check {
        ($($name:ident),+ $(,)?) => {$({
            let id = $name::from("scope-🦉\n");
            assert_eq!(serde_json::to_string(&id).unwrap(), r#""scope-🦉\n""#);
            assert_eq!(id.to_string(), "scope-🦉\n");
            assert_eq!(serde_json::from_str::<$name>(r#""scope-🦉\n""#).unwrap(), id);
            assert_eq!(serde_json::to_string(&$name::default()).unwrap(), r#""""#);
        })+};
    }
    check!(TaskId, AgentId, InteractionId, DocumentId, DocumentRevisionId, PrId,
        RequestId, AuthorizationId, CommandId, DataEpoch, ScheduleId, ScheduleRunId);
}

#[test]
fn language_and_numeric_boundaries_keep_their_wire_contract() {
    assert_eq!(serde_json::to_string(&Lang::En).unwrap(), "\"en\"");
    assert_eq!(serde_json::to_string(&Lang::Es).unwrap(), "\"es\"");
    assert_eq!(serde_json::from_str::<Lang>("\"es\"").unwrap(), Lang::Es);
    assert!(serde_json::from_str::<Lang>("\"fr\"").is_err());
    let revision: Revision = MAX_REVISION;
    assert_eq!(serde_json::to_string(&revision).unwrap(), "9007199254740991");
    let at: Millis = -1;
    assert_eq!(serde_json::to_string(&at).unwrap(), "-1");
}
