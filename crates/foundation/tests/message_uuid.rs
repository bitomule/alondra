use alondra_foundation::MessageUuid;
use std::collections::{BTreeSet, HashSet};

#[test]
fn construction_and_serde_share_canonical_lowercase_identity() {
    for value in [
        "00000000-0000-4000-8000-000000000002",
        "aBcDeF01-2345-6789-aBcD-0123456789aB",
        "00000000-0000-0000-0000-000000000000",
    ] {
        let expected = value.to_ascii_lowercase();
        let uuid = MessageUuid::new(value).unwrap();
        assert_eq!(uuid.as_str(), expected);
        assert_eq!(uuid.to_string(), expected);
        assert_eq!(
            serde_json::to_string(&uuid).unwrap(),
            serde_json::to_string(&expected).unwrap()
        );
        let decoded =
            serde_json::from_str::<MessageUuid>(&serde_json::to_string(value).unwrap()).unwrap();
        assert_eq!(decoded, uuid);
        assert_eq!(
            serde_json::from_str::<MessageUuid>(&serde_json::to_string(&uuid).unwrap()).unwrap(),
            uuid
        );
    }
}

#[test]
fn invalid_uuid_cannot_bypass_construction_through_deserialization() {
    for invalid in [
        "",
        "initial",
        "00000000000040008000000000000002",
        "g0000000-0000-4000-8000-000000000002",
        "00000000_0000-4000-8000-000000000002",
        "00000000-0000-4000-8000-000000000002\n",
        "🦉000000-0000-4000-8000-000000000002",
    ] {
        assert!(
            MessageUuid::new(invalid).is_err(),
            "constructor accepted {invalid:?}"
        );
        assert!(
            serde_json::from_str::<MessageUuid>(&serde_json::to_string(invalid).unwrap()).is_err(),
            "serde bypassed validation for {invalid:?}"
        );
    }
    for invalid in ["null", "42", "{}", "[]"] {
        assert!(serde_json::from_str::<MessageUuid>(invalid).is_err());
    }
}

#[test]
fn uppercase_aliases_cannot_create_distinct_unique_identities() {
    let lowercase = MessageUuid::new("abcdef01-2345-6789-abcd-0123456789ab").unwrap();
    let uppercase = MessageUuid::new("ABCDEF01-2345-6789-ABCD-0123456789AB").unwrap();
    assert_eq!(lowercase, uppercase);
    assert_eq!(
        BTreeSet::from([lowercase.clone(), uppercase.clone()]).len(),
        1
    );
    assert_eq!(HashSet::from([lowercase, uppercase]).len(), 1);
}
