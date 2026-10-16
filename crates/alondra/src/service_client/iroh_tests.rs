//! An account-paired Mac over a real iroh endpoint on loopback, with the relay mailbox as a
//! recording fake that answers through the same sealing Mac.
use super::super::Profile;
use super::*;
use crate::direct_client::iroh_tests::{self as fake, Behaviour, FakeIroh};
use ring::signature::{ED25519, UnparsedPublicKey};

fn profile() -> Profile {
    Profile {
        mac_id: fake::MAC.into(),
        owner_sub: "owner".into(),
        device_id: fake::DEVICE.into(),
        mac_signing_key: fake::wire(fake::mac_signing().verifying_key()),
        mac_agreement_key: B64.encode(fake::mac_agreement().public_key().to_sec1_bytes()),
        signing_key: B64.encode(fake::device_signing().to_bytes()),
        agreement_key: B64.encode(fake::device_agreement().to_bytes()),
    }
}

fn home() -> crate::test_support::Directory {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&*home).unwrap();
    home
}

/// The relay mailbox: keeps each submitted envelope and hands back the Mac's sealed answer.
#[derive(Default)]
struct Mailbox {
    submitted: Mutex<Vec<Vec<u8>>>,
}

impl Mailbox {
    fn call(&self, method: &str, path: &str, body: &[u8]) -> Result<(u16, Value)> {
        if method == "POST" && path == "/v1/requests" {
            self.submitted.lock().unwrap().push(body.to_vec());
            let envelope: Value = serde_json::from_slice(body)?;
            return Ok((202, json!({"request_id": envelope["request_id"], "status": "queued", "revision": 1})));
        }
        let submitted = self.submitted.lock().unwrap().last().cloned().context("nothing submitted")?;
        let envelope: Value = serde_json::from_slice(&submitted)?;
        let response: Value = serde_json::from_slice(&fake::answer(&submitted, &Value::Null))?;
        Ok((200, json!({"request_id": envelope["request_id"], "status": "completed", "revision": 2, "response": response})))
    }

    fn submitted(&self) -> Vec<Vec<u8>> {
        self.submitted.lock().unwrap().clone()
    }
}

fn read(home: &Path, learn: impl FnOnce() -> Option<Dial>, mailbox: &Mailbox) -> Result<(u16, Value)> {
    profile().request_over(home, "GET", "/capabilities", None, Instant::now() + Duration::from_secs(5), learn,
        |method, path, body, _| mailbox.call(method, path, body))
}

#[test]
fn an_account_paired_mac_answers_over_iroh_and_the_mailbox_is_not_used() {
    let (home, iroh, mailbox) = (home(), FakeIroh::start(Behaviour::Answer), Mailbox::default());
    crate::direct_client::keep_dial(&home, fake::MAC, Some(iroh.dial())).unwrap();
    assert_eq!(read(&home, || panic!("the known dial works"), &mailbox).unwrap(), (200, json!({"path": "/capabilities"})));
    assert_eq!(read(&home, || panic!("the open connection is reused"), &mailbox).unwrap().0, 200);
    assert_eq!(iroh.received().len(), 2);
    assert!(mailbox.submitted().is_empty());
}

#[test]
fn an_envelope_lost_on_iroh_goes_through_the_mailbox_byte_for_byte() {
    let (home, iroh, mailbox) = (home(), FakeIroh::start(Behaviour::DropAfterRequest), Mailbox::default());
    crate::direct_client::keep_dial(&home, fake::MAC, Some(iroh.dial())).unwrap();
    assert_eq!(read(&home, || None, &mailbox).unwrap(), (200, json!({"path": "/capabilities"})));
    assert_eq!(mailbox.submitted(), iroh.received(), "the Mac sees the same envelope twice, and its journal runs it once");
}

#[test]
fn a_refusal_over_iroh_is_the_macs_answer_and_is_not_sent_again() {
    let (home, iroh, mailbox) = (home(), FakeIroh::start(Behaviour::Refuse("idempotency_conflict")), Mailbox::default());
    crate::direct_client::keep_dial(&home, fake::MAC, Some(iroh.dial())).unwrap();
    assert_eq!(read(&home, || None, &mailbox).unwrap(), (409, json!({"error": "idempotency_conflict"})));
    assert!(mailbox.submitted().is_empty());
}

#[test]
fn an_unknown_dial_is_learned_once_and_kept_after_it_connects() {
    let (home, iroh, mailbox) = (home(), FakeIroh::start(Behaviour::Answer), Mailbox::default());
    assert_eq!(read(&home, || Some(iroh.dial()), &mailbox).unwrap().0, 200);
    assert_eq!(crate::iroh_client::load_dial(&home, fake::MAC), Some(iroh.dial()));
    assert!(mailbox.submitted().is_empty());
}

#[test]
fn a_mac_iroh_cannot_reach_goes_through_the_mailbox_and_is_not_dialled_again() {
    let (home, mailbox) = (home(), Mailbox::default());
    assert_eq!(read(&home, || None, &mailbox).unwrap(), (200, json!({"path": "/capabilities"})));
    assert_eq!(read(&home, || panic!("this process already knows iroh is down"), &mailbox).unwrap().0, 200);
    assert_eq!(mailbox.submitted().len(), 2);
}

#[test]
fn the_directory_endpoint_gives_the_dial_after_the_terminal_binds_its_own() {
    let home = home();
    let endpoint = iroh::SecretKey::from_bytes(&[9; 32]).public().to_string();
    let bound = Mutex::new(None);
    let dial = profile().learned_dial(&home, Some(&endpoint), Some("https://iroh.example.com/"), |body| {
        *bound.lock().unwrap() = Some(serde_json::from_slice::<Value>(body)?);
        Ok(204)
    });
    assert_eq!(dial, Some(Dial { endpoint, addrs: vec![], relay: Some("https://iroh.example.com/".into()) }));
    let bound = bound.into_inner().unwrap().expect("the terminal's endpoint is bound at the relay");
    let key = iroh_client::key(&home).unwrap().public();
    assert_eq!(bound["iroh_node_id"], key.to_string());
    let proof = B64.decode(bound["proof"].as_str().unwrap()).unwrap();
    UnparsedPublicKey::new(&ED25519, key.as_bytes())
        .verify(&[IROH_NODE_PROOF_DOMAIN, fake::DEVICE.as_bytes()].concat(), &proof)
        .expect("the proof the relay checks: this endpoint's key over the device ID");
}

#[test]
fn a_mac_the_directory_lists_without_an_endpoint_gives_no_dial_and_binds_nothing() {
    let home = home();
    let bind = |_: &[u8]| -> Result<u16> { panic!("nothing to bind for") };
    assert_eq!(profile().learned_dial(&home, None, None, bind), None);
    assert_eq!(profile().learned_dial(&home, Some("not-an-endpoint"), None, bind), None);
}
