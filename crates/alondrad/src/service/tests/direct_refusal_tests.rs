use super::*;
use std::sync::atomic::AtomicU64;

struct Fixture {
    service: Service,
    phone: SecretKey,
    clock: Arc<AtomicU64>,
    home: crate::test_support::Directory,
}

const CAPABILITIES: &[u8] = br#"{"method":"GET","path":"/capabilities"}"#;

impl Fixture {
    fn new() -> Self {
        let (home, mut service, phone) = push_fixture();
        let clock = Arc::new(AtomicU64::new(1_000_000));
        service.envelope_clock = Some(clock.clone());
        Self { service, phone, clock, home }
    }

    fn now(&self) -> u64 {
        self.clock.load(Ordering::Relaxed)
    }

    fn sealed(&self, sender: &str, signer: u8, id: &str, expires_at_ms: u64) -> Envelope {
        Envelope::seal_random(
            EnvelopeContext { route: "mac-1", sender, recipient: "mac-1", request_id: id, kind: "request", expires_at_ms },
            CAPABILITIES,
            &SigningKey::from_slice(&[signer; 32]).unwrap(),
            &self.service.agreement.public_key(),
        )
        .unwrap()
    }

    fn opened(&self, bytes: &[u8]) -> Value {
        let response: Envelope = serde_json::from_slice(bytes).unwrap();
        let body = response
            .open(self.service.signing.verifying_key(), &self.phone, "mac-1", "phone-1", "mac-1", self.now())
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn refusal(&self, envelope: Envelope) -> DirectRefusal {
        self.service.handle_direct(envelope).await.map(drop).unwrap_err()
    }
}

#[tokio::test]
async fn each_unauthorized_envelope_names_the_check_it_failed() {
    let f = Fixture::new();
    let fresh = f.now() + 60_000;
    assert_eq!(
        f.refusal(f.sealed("phone-9", 33, "unknown", fresh)).await,
        DirectRefusal::Unauthorized(UnauthorizedCheck::UnknownDevice)
    );
    assert_eq!(
        f.refusal(f.sealed("phone-1", 71, "forged", fresh)).await,
        DirectRefusal::Unauthorized(UnauthorizedCheck::BadSignature)
    );
    assert_eq!(
        f.refusal(f.sealed("phone-1", 71, "forged-late", f.now() - 5_000)).await,
        DirectRefusal::Unauthorized(UnauthorizedCheck::BadSignature),
        "a late envelope signed by another key is a bad signature, and its sender learns nothing"
    );
    let mut garbled = f.sealed("phone-1", 33, "garbled", fresh);
    garbled.signature = "!".into();
    assert_eq!(f.refusal(garbled).await, DirectRefusal::Unauthorized(UnauthorizedCheck::BadEncoding));

    std::fs::write(f.home.join("devices.json"), "[]").unwrap();
    assert_eq!(
        f.refusal(f.sealed("phone-1", 33, "unapproved", fresh)).await,
        DirectRefusal::Unauthorized(UnauthorizedCheck::NotApproved)
    );
}

#[tokio::test]
async fn an_envelope_its_device_signed_that_arrives_late_gets_the_macs_clock_sealed_to_that_device() {
    let f = Fixture::new();
    let expired_at = f.now() - 21_430;
    let DirectRefusal::Expired { late_ms, sealed } = f.refusal(f.sealed("phone-1", 33, "late", expired_at)).await else {
        panic!("a late envelope from the approved device is told so");
    };
    assert_eq!(late_ms, 21_430);
    let answer = f.opened(&sealed);
    assert_eq!(answer["status"], 503);
    assert_eq!(answer["body"]["error"], "envelope_expired");
    assert_eq!(answer["body"]["mac_now_ms"].as_u64(), Some(f.now()));
    assert_eq!(answer["body"]["late_ms"].as_u64(), Some(21_430));
    assert!(f.service.handle_direct(f.sealed("phone-1", 33, "late", f.now() + 60_000)).await.is_ok());
}

#[tokio::test]
async fn approvals_the_mac_cannot_read_leave_the_device_undecided() {
    let f = Fixture::new();
    std::fs::write(f.home.join("devices.json"), "[{\"id\":").unwrap();
    assert_eq!(f.refusal(f.sealed("phone-1", 33, "unreadable", f.now() + 60_000)).await, DirectRefusal::Unavailable);
}

#[tokio::test]
async fn over_iroh_a_late_envelope_is_answered_sealed_and_unreadable_approvals_as_unavailable() {
    let f = Fixture::new();
    let late = serde_json::to_vec(&f.sealed("phone-1", 33, "late", f.now() - 1)).unwrap();
    let answer: Value = serde_json::from_slice(&crate::iroh_endpoint::answer(&f.service, "phone-1", &late).await).unwrap();
    let opened = f.opened(&serde_json::to_vec(&answer["response"]).unwrap());
    assert_eq!(opened["body"]["error"], "envelope_expired");

    std::fs::write(f.home.join("devices.json"), "[{\"id\":").unwrap();
    let fresh = serde_json::to_vec(&f.sealed("phone-1", 33, "fresh", f.now() + 60_000)).unwrap();
    let answer: Value = serde_json::from_slice(&crate::iroh_endpoint::answer(&f.service, "phone-1", &fresh).await).unwrap();
    assert_eq!(answer, json!({"error": "unavailable"}));
}

#[test]
fn the_log_names_the_check_and_how_late_without_any_key_or_content() {
    assert_eq!(
        DirectRefusal::Unauthorized(UnauthorizedCheck::Expired { late_ms: 21_430 }).log_fields(),
        "status=401 reason=unauthorized_envelope check=expired late_ms=21430"
    );
    assert_eq!(
        DirectRefusal::Unauthorized(UnauthorizedCheck::BadSignature).log_fields(),
        "status=401 reason=unauthorized_envelope check=bad_signature"
    );
    assert_eq!(
        DirectRefusal::Expired { late_ms: 900, sealed: b"secret".to_vec() }.log_fields(),
        "status=401 reason=envelope_expired check=expired late_ms=900"
    );
    assert_eq!(DirectRefusal::Unavailable.log_fields(), "status=503 reason=approvals_unreadable");
}
