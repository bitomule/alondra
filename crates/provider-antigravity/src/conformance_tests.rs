//! The provider conformance suite, run through Antigravity's real normalizer.
use super::normalizer::Normalizer;
use super::records::{InputRecord, SessionRecord};
use super::wire::HostOwner;
use alondra_antigravity_protocol::{Body, Event, Fence, Frame, Observation, encode};
use alondra_foundation::MessageUuid;
use alondra_provider_conformance::{Host, Native, Provider};
use alondra_provider_contract::{Capability, OrderedObservation};
use alondra_store::OwnerGrant;
use serde_json::Value;
use std::collections::BTreeSet;

/// The conversation every Antigravity transcript of the suite runs in.
const SESSION: &str = "30000000-0000-4000-8000-000000000001";

struct Antigravity;

fn fence(owner: &OwnerGrant) -> Fence {
    Fence { profile_id: "conformance".into(), account_id: "conformance".into(), data_epoch: owner.epoch().clone(), host_id: owner.host().into() }
}

impl Native for Antigravity {
    type Normalizer = Normalizer;
    fn provider(&self) -> Provider {
        Provider::Antigravity
    }
    /// The set the `antigravity-acp` binding declares: no background work, compaction or MCP
    /// controls, which ACP does not offer.
    fn capabilities(&self) -> BTreeSet<Capability> {
        BTreeSet::from([
            Capability::OfferInput,
            Capability::Interrupt,
            Capability::ForceStop,
            Capability::DurableResume,
            Capability::SetModel,
            Capability::SetPermissionMode,
            Capability::Decisions,
        ])
    }
    fn normalizer(&self, owner: &OwnerGrant, host: Host) -> Normalizer {
        Normalizer {
            session: Some(SessionRecord { session_id: MessageUuid::new(SESSION).expect("a session id") }),
            fence: Some(fence(owner)),
            first_cursor: host.first_cursor,
            host_ended: host.ended,
        }
    }
    fn evidence(&self, owner: &OwnerGrant, cursor: u64, observation: &Value) -> Result<Vec<u8>, String> {
        let observation: Observation = serde_json::from_value(observation.clone()).map_err(|error| format!("observation {cursor}: {error}"))?;
        let frame = Frame::new(Body::Event(Event { fence: fence(owner), execution: owner.wire_execution(), cursor, observation }));
        let framed = encode(&frame).map_err(|error| format!("observation {cursor}: {error:?}"))?;
        Ok(framed[4..].to_vec())
    }
    fn input_record(&self, native: &str) -> Result<Vec<u8>, String> {
        let message_id = MessageUuid::new(native).map_err(|error| format!("{native}: {error}"))?;
        serde_json::to_vec(&InputRecord { message_id }).map_err(|error| error.to_string())
    }
    fn ends_host(&self, observation: &OrderedObservation) -> bool {
        crate::native_exited(observation)
    }
}

// spec: antigravity.conformance/simple-turn
// spec: antigravity.conformance/several-turns
// spec: antigravity.conformance/input-during-turn-waits-for-the-prompt
// spec: antigravity.conformance/interrupt
// spec: antigravity.conformance/process-dies-then-resume
// spec: antigravity.conformance/finished-input-not-taken-again
// spec: antigravity.conformance/consumed-input-named-by-provider
// spec: antigravity.conformance/decisions
#[test]
fn every_scenario_of_the_suite_holds_for_antigravity() {
    alondra_provider_conformance::run(&Antigravity).assert_conforms();
}
