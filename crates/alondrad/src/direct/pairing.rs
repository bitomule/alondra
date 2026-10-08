//! Pairing requests that arrive over the local network, held until someone on this Mac
//! approves or denies them. The protocol (commitment, code, signed approval) lives in
//! `alondra_e2e::pairing`; this module keeps the requests, their limits and the decisions.
use alondra_e2e::Envelope;
use alondra_e2e::pairing::{self, Approval, Transcript};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::ecdsa::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub(crate) const PENDING_TTL_MS: u64 = 120_000;
const DECIDED_TTL_MS: u64 = 600_000;
const APPROVAL_TTL_MS: u64 = 600_000;
const MAX_PENDING: usize = 4;
const MAX_PENDING_PER_ADDRESS: usize = 1;
const STARTS_PER_ADDRESS: usize = 6;
const START_WINDOW_MS: u64 = 600_000;
const MAX_DENIED: usize = 256;
const MAX_NAME: usize = 80;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PairingError {
    Invalid(&'static str),
    Busy,
    Denied,
    Conflict,
    NotFound,
    Expired,
    Internal(String),
}

impl PairingError {
    pub(crate) fn code(&self) -> &str {
        match self {
            Self::Invalid(_) => "invalid_pairing",
            Self::Busy => "pairing_busy",
            Self::Denied => "pairing_denied",
            Self::Conflict => "pairing_conflict",
            Self::NotFound => "pairing_not_found",
            Self::Expired => "pairing_expired",
            Self::Internal(_) => "pairing_failed",
        }
    }
}

/// What the Mac needs to know about itself to answer pairing requests.
pub(crate) struct MacIdentity<'a> {
    pub(crate) mac_id: &'a str,
    pub(crate) signing: &'a SigningKey,
    pub(crate) signing_key: &'a str,
    pub(crate) agreement_secret: &'a p256::SecretKey,
    pub(crate) agreement_key: &'a str,
}

#[derive(Deserialize)]
pub(crate) struct StartBody {
    pub(crate) signing_key: String,
    pub(crate) envelope: Envelope,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartPlain {
    device_id: String,
    name: String,
    signing_key: String,
    agreement_key: String,
    commitment: String,
}

#[derive(Deserialize)]
pub(crate) struct RevealBody {
    pub(crate) envelope: Envelope,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevealPlain {
    pairing_id: String,
    client_nonce: String,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub(crate) struct Started {
    pub(crate) pairing_id: String,
    pub(crate) mac_nonce: String,
    pub(crate) expires_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    Committed,
    AwaitingApproval { client_nonce: String, code: String },
    Approved { approval: Approval, signature: String },
    Denied,
}

#[derive(Clone, Debug)]
struct Request {
    address: IpAddr,
    device_id: String,
    name: String,
    signing_key: String,
    agreement_key: String,
    commitment: String,
    mac_nonce: String,
    created_at_ms: u64,
    decided_at_ms: Option<u64>,
    state: State,
}

impl Request {
    fn open(&self, now_ms: u64) -> bool {
        matches!(self.state, State::Committed | State::AwaitingApproval { .. })
            && now_ms < self.created_at_ms + PENDING_TTL_MS
    }
    fn stale(&self, now_ms: u64) -> bool {
        match self.decided_at_ms {
            Some(decided) => now_ms >= decided + DECIDED_TTL_MS,
            None => now_ms >= self.created_at_ms + PENDING_TTL_MS + DECIDED_TTL_MS,
        }
    }
}

/// One pairing request waiting for the user, as the Mac app and the TUI list it.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub(crate) struct Pending {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) device_id: String,
    pub(crate) code: String,
    pub(crate) expires_at_ms: u64,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum Status {
    Committed,
    AwaitingApproval,
    Approved { approval: Approval, signature: String },
    Denied,
    Expired,
}

#[derive(Default)]
struct Book {
    requests: HashMap<String, Request>,
    starts: HashMap<IpAddr, VecDeque<u64>>,
}

pub(crate) struct PairingBook {
    denied_path: PathBuf,
    book: Mutex<Book>,
}

fn fingerprint(signing_key: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(signing_key.as_bytes()))
}

fn random_id() -> Result<String, PairingError> {
    let mut bytes = [0u8; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|_| PairingError::Internal("OS randomness failed".into()))?;
    Ok(format!("pairing-{}", URL_SAFE_NO_PAD.encode(bytes)))
}

fn canonical_key(value: &str) -> Result<Vec<u8>, PairingError> {
    let raw = URL_SAFE_NO_PAD.decode(value).map_err(|_| PairingError::Invalid("key encoding"))?;
    if raw.len() != 65 || raw[0] != 4 || URL_SAFE_NO_PAD.encode(&raw) != value {
        return Err(PairingError::Invalid("key encoding"));
    }
    Ok(raw)
}

fn valid_device_id(id: &str) -> bool {
    (id.starts_with("client-") || id.starts_with("phone-"))
        && id.len() <= 128
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl PairingBook {
    pub(crate) fn new(home: &Path) -> Self {
        Self { denied_path: home.join("direct-denied.json"), book: Mutex::default() }
    }

    fn denied(&self) -> Vec<String> {
        std::fs::read(&self.denied_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn remember_denied(&self, signing_key: &str) -> Result<(), PairingError> {
        let mut denied = self.denied();
        let print = fingerprint(signing_key);
        if denied.contains(&print) {
            return Ok(());
        }
        denied.push(print);
        if denied.len() > MAX_DENIED {
            denied.drain(..denied.len() - MAX_DENIED);
        }
        let bytes = serde_json::to_vec(&denied).map_err(|e| PairingError::Internal(e.to_string()))?;
        let temp = self.denied_path.with_extension("json.tmp");
        std::fs::write(&temp, bytes)
            .and_then(|()| std::fs::rename(&temp, &self.denied_path))
            .map_err(|e| PairingError::Internal(e.to_string()))
    }

    /// Step one: the client's keys and commitment. The Mac answers with its own nonce.
    pub(crate) fn start(
        &self,
        mac: &MacIdentity<'_>,
        address: IpAddr,
        body: StartBody,
        now_ms: u64,
    ) -> Result<Started, PairingError> {
        let envelope = &body.envelope;
        if envelope.kind != "pairing" || envelope.route != mac.mac_id || envelope.recipient != mac.mac_id {
            return Err(PairingError::Invalid("envelope route"));
        }
        let signing = VerifyingKey::from_sec1_bytes(&canonical_key(&body.signing_key)?)
            .map_err(|_| PairingError::Invalid("signing key"))?;
        let plaintext = envelope
            .open(&signing, mac.agreement_secret, &envelope.sender, mac.mac_id, mac.mac_id, now_ms)
            .map_err(|_| PairingError::Invalid("envelope"))?;
        let plain: StartPlain =
            serde_json::from_slice(&plaintext).map_err(|_| PairingError::Invalid("request body"))?;
        p256::PublicKey::from_sec1_bytes(&canonical_key(&plain.agreement_key)?)
            .map_err(|_| PairingError::Invalid("agreement key"))?;
        if plain.device_id != envelope.sender
            || plain.signing_key != body.signing_key
            || !valid_device_id(&plain.device_id)
            || plain.name.is_empty()
            || plain.name.chars().count() > MAX_NAME
            || plain.name.chars().any(char::is_control)
            || URL_SAFE_NO_PAD.decode(&plain.commitment).map(|c| c.len()) != Ok(32)
        {
            return Err(PairingError::Invalid("request fields"));
        }
        if self.denied().contains(&fingerprint(&plain.signing_key)) {
            return Err(PairingError::Denied);
        }
        let mut book = self.book.lock().expect("pairing book poisoned");
        book.requests.retain(|_, request| !request.stale(now_ms));
        if let Some((id, existing)) = book.requests.iter().find(|(_, r)| r.device_id == plain.device_id) {
            let same = existing.signing_key == plain.signing_key
                && existing.agreement_key == plain.agreement_key
                && existing.commitment == plain.commitment;
            if !same {
                return Err(PairingError::Conflict);
            }
            if !existing.open(now_ms) {
                return Err(PairingError::Expired);
            }
            return Ok(Started {
                pairing_id: id.clone(),
                mac_nonce: existing.mac_nonce.clone(),
                expires_at_ms: existing.created_at_ms + PENDING_TTL_MS,
            });
        }
        let open: Vec<&Request> = book.requests.values().filter(|r| r.open(now_ms)).collect();
        if open.len() >= MAX_PENDING
            || open.iter().filter(|r| r.address == address).count() >= MAX_PENDING_PER_ADDRESS
        {
            return Err(PairingError::Busy);
        }
        let starts = book.starts.entry(address).or_default();
        while starts.front().is_some_and(|at| now_ms >= at + START_WINDOW_MS) {
            starts.pop_front();
        }
        if starts.len() >= STARTS_PER_ADDRESS {
            return Err(PairingError::Busy);
        }
        starts.push_back(now_ms);
        let pairing_id = random_id()?;
        let mac_nonce = pairing::random_nonce().map_err(|e| PairingError::Internal(e.to_string()))?;
        book.requests.insert(
            pairing_id.clone(),
            Request {
                address,
                device_id: plain.device_id,
                name: plain.name,
                signing_key: plain.signing_key,
                agreement_key: plain.agreement_key,
                commitment: plain.commitment,
                mac_nonce: mac_nonce.clone(),
                created_at_ms: now_ms,
                decided_at_ms: None,
                state: State::Committed,
            },
        );
        Ok(Started { pairing_id, mac_nonce, expires_at_ms: now_ms + PENDING_TTL_MS })
    }

    /// Step two: the client reveals its nonce; only now does the request reach the user.
    pub(crate) fn reveal(
        &self,
        mac: &MacIdentity<'_>,
        pairing_id: &str,
        body: RevealBody,
        now_ms: u64,
    ) -> Result<(), PairingError> {
        let mut book = self.book.lock().expect("pairing book poisoned");
        let request = book.requests.get_mut(pairing_id).ok_or(PairingError::NotFound)?;
        if !request.open(now_ms) {
            return Err(PairingError::Expired);
        }
        let envelope = &body.envelope;
        if envelope.kind != "pairing"
            || envelope.route != mac.mac_id
            || envelope.recipient != mac.mac_id
            || envelope.sender != request.device_id
        {
            return Err(PairingError::Invalid("envelope route"));
        }
        let signing = VerifyingKey::from_sec1_bytes(&canonical_key(&request.signing_key)?)
            .map_err(|_| PairingError::Invalid("signing key"))?;
        let plaintext = envelope
            .open(&signing, mac.agreement_secret, &request.device_id, mac.mac_id, mac.mac_id, now_ms)
            .map_err(|_| PairingError::Invalid("envelope"))?;
        let plain: RevealPlain =
            serde_json::from_slice(&plaintext).map_err(|_| PairingError::Invalid("request body"))?;
        if plain.pairing_id != pairing_id {
            return Err(PairingError::Invalid("pairing id"));
        }
        pairing::verify_commitment(
            &request.commitment,
            &plain.client_nonce,
            &request.device_id,
            &request.signing_key,
            &request.agreement_key,
        )
        .map_err(|_| PairingError::Invalid("commitment"))?;
        if let State::AwaitingApproval { client_nonce, .. } = &request.state {
            return if *client_nonce == plain.client_nonce { Ok(()) } else { Err(PairingError::Conflict) };
        }
        let code = transcript(mac, pairing_id, request, &plain.client_nonce)
            .code()
            .map_err(|e| PairingError::Internal(e.to_string()))?;
        request.state = State::AwaitingApproval { client_nonce: plain.client_nonce, code };
        Ok(())
    }

    pub(crate) fn status(&self, pairing_id: &str, now_ms: u64) -> Result<Status, PairingError> {
        let book = self.book.lock().expect("pairing book poisoned");
        let request = book.requests.get(pairing_id).ok_or(PairingError::NotFound)?;
        Ok(match &request.state {
            State::Approved { approval, signature } => {
                Status::Approved { approval: approval.clone(), signature: signature.clone() }
            }
            State::Denied => Status::Denied,
            _ if !request.open(now_ms) => Status::Expired,
            State::Committed => Status::Committed,
            State::AwaitingApproval { .. } => Status::AwaitingApproval,
        })
    }

    /// The requests that wait for the user, with the code both screens show.
    pub(crate) fn pending(&self, now_ms: u64) -> Vec<Pending> {
        let book = self.book.lock().expect("pairing book poisoned");
        let mut pending: Vec<Pending> = book
            .requests
            .iter()
            .filter(|(_, r)| r.open(now_ms))
            .filter_map(|(id, r)| match &r.state {
                State::AwaitingApproval { code, .. } => Some(Pending {
                    id: id.clone(),
                    name: r.name.clone(),
                    device_id: r.device_id.clone(),
                    code: code.clone(),
                    expires_at_ms: r.created_at_ms + PENDING_TTL_MS,
                }),
                _ => None,
            })
            .collect();
        pending.sort_by_key(|p| p.expires_at_ms);
        pending
    }

    pub(crate) fn deny(&self, pairing_id: &str, now_ms: u64) -> Result<(), PairingError> {
        let signing_key = {
            let mut book = self.book.lock().expect("pairing book poisoned");
            let request = book.requests.get_mut(pairing_id).ok_or(PairingError::NotFound)?;
            if request.state == State::Denied {
                return Ok(());
            }
            if !request.open(now_ms) {
                return Err(PairingError::Expired);
            }
            request.state = State::Denied;
            request.decided_at_ms = Some(now_ms);
            request.signing_key.clone()
        };
        self.remember_denied(&signing_key)
    }

    /// Persists the device through `persist` (which pins its keys and approves it on this Mac),
    /// then signs the approval the client verifies. Persisting comes first: a signed approval
    /// for a device the Mac does not know would leave the client paired to nothing.
    pub(crate) fn approve(
        &self,
        mac: &MacIdentity<'_>,
        pairing_id: &str,
        now_ms: u64,
        persist: impl FnOnce(&ApprovedDevice) -> Result<(), String>,
    ) -> Result<(), PairingError> {
        let mut book = self.book.lock().expect("pairing book poisoned");
        let request = book.requests.get_mut(pairing_id).ok_or(PairingError::NotFound)?;
        if matches!(request.state, State::Approved { .. }) {
            return Ok(());
        }
        if !request.open(now_ms) {
            return Err(PairingError::Expired);
        }
        let State::AwaitingApproval { client_nonce, .. } = request.state.clone() else {
            return Err(PairingError::Invalid("pairing not revealed"));
        };
        persist(&ApprovedDevice {
            device_id: request.device_id.clone(),
            name: request.name.clone(),
            signing_key: request.signing_key.clone(),
            agreement_key: request.agreement_key.clone(),
        })
        .map_err(PairingError::Internal)?;
        let digest = transcript(mac, pairing_id, request, &client_nonce)
            .digest()
            .map_err(|e| PairingError::Internal(e.to_string()))?;
        let approval = Approval {
            pairing_id: pairing_id.into(),
            mac_id: mac.mac_id.into(),
            mac_signing_key: mac.signing_key.into(),
            mac_agreement_key: mac.agreement_key.into(),
            device_id: request.device_id.clone(),
            client_signing_key: request.signing_key.clone(),
            client_agreement_key: request.agreement_key.clone(),
            transcript: digest,
            expires_at_ms: now_ms + APPROVAL_TTL_MS,
        };
        let signature = approval.sign(mac.signing);
        request.state = State::Approved { approval, signature };
        request.decided_at_ms = Some(now_ms);
        Ok(())
    }
}

pub(crate) struct ApprovedDevice {
    pub(crate) device_id: String,
    pub(crate) name: String,
    pub(crate) signing_key: String,
    pub(crate) agreement_key: String,
}

fn transcript<'a>(
    mac: &'a MacIdentity<'_>,
    pairing_id: &'a str,
    request: &'a Request,
    client_nonce: &'a str,
) -> Transcript<'a> {
    Transcript {
        pairing_id,
        mac_id: mac.mac_id,
        mac_signing_key: mac.signing_key,
        mac_agreement_key: mac.agreement_key,
        device_id: &request.device_id,
        client_signing_key: &request.signing_key,
        client_agreement_key: &request.agreement_key,
        client_nonce,
        mac_nonce: &request.mac_nonce,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_e2e::Context;
    use p256::SecretKey;
    use p256::elliptic_curve::sec1::ToSec1Point;

    fn wire_signing(key: &SigningKey) -> String {
        URL_SAFE_NO_PAD.encode(key.verifying_key().to_sec1_point(false).as_bytes())
    }

    fn wire_agreement(key: &SecretKey) -> String {
        URL_SAFE_NO_PAD.encode(key.public_key().to_sec1_point(false).as_bytes())
    }

    struct Mac {
        signing: SigningKey,
        signing_key: String,
        agreement: SecretKey,
        agreement_key: String,
    }

    impl Mac {
        fn new(seed: u8) -> Self {
            let signing = SigningKey::from_slice(&[seed; 32]).unwrap();
            let agreement = SecretKey::from_slice(&[seed + 1; 32]).unwrap();
            Self {
                signing_key: wire_signing(&signing),
                agreement_key: wire_agreement(&agreement),
                signing,
                agreement,
            }
        }
        fn identity(&self) -> MacIdentity<'_> {
            MacIdentity {
                mac_id: "mac-1",
                signing: &self.signing,
                signing_key: &self.signing_key,
                agreement_secret: &self.agreement,
                agreement_key: &self.agreement_key,
            }
        }
    }

    struct Client {
        id: String,
        signing: SigningKey,
        agreement: SecretKey,
        nonce: String,
    }

    impl Client {
        fn new(seed: u8) -> Self {
            Self {
                id: format!("client-{seed}"),
                signing: SigningKey::from_slice(&[seed; 32]).unwrap(),
                agreement: SecretKey::from_slice(&[seed + 1; 32]).unwrap(),
                nonce: URL_SAFE_NO_PAD.encode([seed; 32]),
            }
        }
        fn seal(&self, mac: &Mac, request_id: &str, plain: serde_json::Value) -> Envelope {
            Envelope::seal_random(
                Context {
                    route: "mac-1",
                    sender: &self.id,
                    recipient: "mac-1",
                    request_id,
                    kind: "pairing",
                    expires_at_ms: 10_000_000,
                },
                &serde_json::to_vec(&plain).unwrap(),
                &self.signing,
                &mac.agreement.public_key(),
            )
            .unwrap()
        }
        fn start_body(&self, mac: &Mac) -> StartBody {
            let signing_key = wire_signing(&self.signing);
            let agreement_key = wire_agreement(&self.agreement);
            let commitment = pairing::commitment(&self.nonce, &self.id, &signing_key, &agreement_key).unwrap();
            StartBody {
                envelope: self.seal(
                    mac,
                    "pair-start",
                    serde_json::json!({
                        "device_id": self.id, "name": "iPhone", "signing_key": signing_key,
                        "agreement_key": agreement_key, "commitment": commitment,
                    }),
                ),
                signing_key,
            }
        }
        fn reveal_body(&self, mac: &Mac, pairing_id: &str) -> RevealBody {
            RevealBody {
                envelope: self.seal(
                    mac,
                    "pair-reveal",
                    serde_json::json!({"pairing_id": pairing_id, "client_nonce": self.nonce}),
                ),
            }
        }
        fn code(&self, mac_signing: &str, mac_agreement: &str, started: &Started) -> String {
            let signing_key = wire_signing(&self.signing);
            let agreement_key = wire_agreement(&self.agreement);
            Transcript {
                pairing_id: &started.pairing_id,
                mac_id: "mac-1",
                mac_signing_key: mac_signing,
                mac_agreement_key: mac_agreement,
                device_id: &self.id,
                client_signing_key: &signing_key,
                client_agreement_key: &agreement_key,
                client_nonce: &self.nonce,
                mac_nonce: &started.mac_nonce,
            }
            .code()
            .unwrap()
        }
    }

    fn book() -> crate::test_support::Owned<PairingBook> {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        crate::test_support::Owned::new(PairingBook::new(&home), home)
    }

    const LAN: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 20));

    #[test]
    fn both_sides_show_the_same_code_and_the_approval_verifies_against_hello() {
        let (mac, client, book) = (Mac::new(1), Client::new(30), book());
        let started = book.start(&mac.identity(), LAN, client.start_body(&mac), 1_000).unwrap();
        assert!(book.pending(1_000).is_empty(), "the user sees nothing before the reveal");
        book.reveal(&mac.identity(), &started.pairing_id, client.reveal_body(&mac, &started.pairing_id), 1_100).unwrap();
        let pending = book.pending(1_100);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].code, client.code(&mac.signing_key, &mac.agreement_key, &started));
        let mut persisted = None;
        book.approve(&mac.identity(), &started.pairing_id, 1_200, |device| {
            persisted = Some(device.device_id.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(persisted.as_deref(), Some("client-30"));
        let Status::Approved { approval, signature } = book.status(&started.pairing_id, 1_300).unwrap() else {
            panic!("approved");
        };
        let signing_key = wire_signing(&client.signing);
        let agreement_key = wire_agreement(&client.agreement);
        let expected = Transcript {
            pairing_id: &started.pairing_id,
            mac_id: "mac-1",
            mac_signing_key: &mac.signing_key,
            mac_agreement_key: &mac.agreement_key,
            device_id: &client.id,
            client_signing_key: &signing_key,
            client_agreement_key: &agreement_key,
            client_nonce: &client.nonce,
            mac_nonce: &started.mac_nonce,
        };
        approval.verify(&signature, mac.signing.verifying_key(), &expected, 1_300).unwrap();
    }

    #[test]
    fn a_fake_mac_in_the_middle_shows_a_different_code() {
        let (real, fake, client, book) = (Mac::new(1), Mac::new(50), Client::new(30), book());
        let started = book.start(&real.identity(), LAN, client.start_body(&real), 1_000).unwrap();
        book.reveal(&real.identity(), &started.pairing_id, client.reveal_body(&real, &started.pairing_id), 1_100).unwrap();
        let on_mac = &book.pending(1_100)[0].code;
        assert_ne!(on_mac, &client.code(&fake.signing_key, &fake.agreement_key, &started));
    }

    #[test]
    fn a_reveal_that_does_not_match_the_commitment_is_refused() {
        let (mac, client, book) = (Mac::new(1), Client::new(30), book());
        let started = book.start(&mac.identity(), LAN, client.start_body(&mac), 1_000).unwrap();
        let liar = Client { nonce: URL_SAFE_NO_PAD.encode([99u8; 32]), ..Client::new(30) };
        assert_eq!(
            book.reveal(&mac.identity(), &started.pairing_id, liar.reveal_body(&mac, &started.pairing_id), 1_100),
            Err(PairingError::Invalid("commitment"))
        );
        assert!(book.pending(1_100).is_empty());
    }

    #[test]
    fn a_request_expires_after_two_minutes() {
        let (mac, client, book) = (Mac::new(1), Client::new(30), book());
        let started = book.start(&mac.identity(), LAN, client.start_body(&mac), 1_000).unwrap();
        book.reveal(&mac.identity(), &started.pairing_id, client.reveal_body(&mac, &started.pairing_id), 1_100).unwrap();
        let late = 1_000 + PENDING_TTL_MS;
        assert!(book.pending(late).is_empty());
        assert_eq!(book.status(&started.pairing_id, late).unwrap(), Status::Expired);
        assert_eq!(book.approve(&mac.identity(), &started.pairing_id, late, |_| Ok(())), Err(PairingError::Expired));
    }

    #[test]
    fn denied_keys_cannot_ask_again() {
        let (mac, client, book) = (Mac::new(1), Client::new(30), book());
        let started = book.start(&mac.identity(), LAN, client.start_body(&mac), 1_000).unwrap();
        book.reveal(&mac.identity(), &started.pairing_id, client.reveal_body(&mac, &started.pairing_id), 1_100).unwrap();
        book.deny(&started.pairing_id, 1_200).unwrap();
        assert_eq!(book.status(&started.pairing_id, 1_200).unwrap(), Status::Denied);
        let again = PairingBook::new(book.denied_path.parent().unwrap());
        assert_eq!(again.start(&mac.identity(), LAN, client.start_body(&mac), 2_000), Err(PairingError::Denied));
    }

    #[test]
    fn limits_pending_requests_per_address_and_in_total() {
        let (mac, book) = (Mac::new(1), book());
        book.start(&mac.identity(), LAN, Client::new(30).start_body(&mac), 1_000).unwrap();
        assert_eq!(
            book.start(&mac.identity(), LAN, Client::new(32).start_body(&mac), 1_000),
            Err(PairingError::Busy)
        );
        for last in 2..=4u8 {
            let address = IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, last));
            book.start(&mac.identity(), address, Client::new(30 + 2 * last).start_body(&mac), 1_000).unwrap();
        }
        let fifth = IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 9));
        assert_eq!(
            book.start(&mac.identity(), fifth, Client::new(60).start_body(&mac), 1_000),
            Err(PairingError::Busy)
        );
    }

    #[test]
    fn limits_how_often_one_address_starts_pairing() {
        let (mac, book) = (Mac::new(1), book());
        for attempt in 0..STARTS_PER_ADDRESS as u64 {
            let started = book
                .start(&mac.identity(), LAN, Client::new(30 + 2 * attempt as u8).start_body(&mac), 1_000 + attempt)
                .unwrap();
            book.deny(&started.pairing_id, 1_000 + attempt).unwrap();
        }
        assert_eq!(
            book.start(&mac.identity(), LAN, Client::new(90).start_body(&mac), 2_000),
            Err(PairingError::Busy)
        );
        assert!(book.start(&mac.identity(), LAN, Client::new(90).start_body(&mac), 1_000 + START_WINDOW_MS).is_ok());
    }

    #[test]
    fn repeating_the_first_step_returns_the_same_request() {
        let (mac, client, book) = (Mac::new(1), Client::new(30), book());
        let first = book.start(&mac.identity(), LAN, client.start_body(&mac), 1_000).unwrap();
        let again = book.start(&mac.identity(), LAN, client.start_body(&mac), 1_050).unwrap();
        assert_eq!(first, again);
    }

    #[test]
    fn an_envelope_for_another_mac_is_refused() {
        let (mac, other, client, book) = (Mac::new(1), Mac::new(50), Client::new(30), book());
        assert_eq!(
            book.start(&mac.identity(), LAN, client.start_body(&other), 1_000),
            Err(PairingError::Invalid("envelope"))
        );
    }
}
