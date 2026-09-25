//! Endpoint-only E2E envelope primitives. The relay handles these envelopes as opaque bytes.
//! Private keys, plaintext and the Mac's operation receipt journal never belong on the relay.
//!
//! ## Enrollment and key continuity
//! The Mac presents a QR directly to the iPhone containing its route ID, P-256 signing and
//! agreement public keys, a random 128-bit one-use challenge, and an expiry. The iPhone pins
//! both Mac keys from that QR in local Keychain, creates separate signing and agreement keys,
//! and returns its public keys plus the challenge. The Mac consumes the challenge atomically,
//! records approved device keys locally, and signs an approval binding *all* four public keys,
//! route, device ID and challenge. The iPhone verifies that approval against its QR pin before
//! allowing content. A Keycloak account or relay directory entry cannot grant content access.
//! On Mac key change the client must stop and demand visible re-pairing. A lost final endpoint
//! key cannot be recovered by the operator. These enrollment/storage operations are outside
//! this primitive crate and must be implemented and reviewed before production data transits.
//!
//! ## Replay and ordering
//! The envelope authenticates route, sender, recipient, request ID, kind, expiry,
//! ephemeral key and nonce. The caller must also match response `request_id` to its pending
//! request and check kind. HTTP method and application path belong inside the encrypted body.
//! Mutations use a persistent Mac receipt journal keyed by
//! (device ID, operation ID), rejecting the same ID with different bytes and returning the
//! previous result for exact retries. The relay may redeliver; crypto alone cannot stop replay.
//! `/v1/changes` carries the Mac's epoch and tick inside the encrypted response. Clients reject
//! stale epochs/ticks; after reconnect they resynchronize from the Mac. Push uses `kind=push` and
//! per-device encryption with a generic APNs fallback. The notification extension needs the
//! approved device agreement key in a shared Keychain access group.
//! The standalone `tests/cryptokit_vector.swift` checks an independently reconstructed header,
//! HKDF key, ChaChaPoly plaintext and P-256 signature against the Rust fixture.
//! For Mac-to-relay HTTP authentication, `/v1/bootstrap` publishes a random relay epoch.
//! The Mac signs that epoch, device ID, HTTP method/path, body digest, timestamp and a fresh
//! 128-bit nonce. The relay uses one shared `HttpChallengeVerifier`: it rejects clock skew,
//! repeated nonces and signatures from a prior relay process epoch.

#![forbid(unsafe_code)]

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hkdf::Hkdf;
use p256::{
    PublicKey, SecretKey,
    ecdh::diffie_hellman,
    ecdsa::{
        Signature, SigningKey, VerifyingKey,
        signature::{Signer, Verifier},
    },
    elliptic_curve::sec1::ToSec1Point,
};
use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const ENVELOPE_DOMAIN: &[u8] = b"alondra-e2e-envelope-v1";
const KEY_DOMAIN: &[u8] = b"alondra-e2e-key-v1";
const HTTP_DOMAIN: &[u8] = b"alondra-http-auth-v1";
const MAX_CIPHERTEXT: usize = 20 * 1024 * 1024 + 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidField,
    InvalidEncoding,
    InvalidKey,
    InvalidSignature,
    WrongIdentity,
    Expired,
    Crypto,
    ClockSkew,
    ClockRollback,
    Replay,
    Capacity,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

/// Public routing metadata is signed and authenticated as AEAD associated data.
/// `route` is the target Mac ID, including on responses and pushes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnvelopeV1 {
    pub v: u8,
    pub route: String,
    pub sender: String,
    pub recipient: String,
    pub request_id: String,
    pub kind: String,
    pub expires_at_ms: u64,
    /// Uncompressed SEC1 P-256 public key, base64url without padding.
    pub ephemeral_key: String,
    /// 12 random bytes, base64url without padding. Never reuse with the same derived key.
    pub nonce: String,
    /// ChaCha20-Poly1305 ciphertext and tag, base64url without padding.
    pub ciphertext: String,
    /// P-256 ECDSA SHA-256 signature as 64-byte r||s, base64url without padding.
    pub signature: String,
}

pub struct Context<'a> {
    pub route: &'a str,
    pub sender: &'a str,
    pub recipient: &'a str,
    pub request_id: &'a str,
    pub kind: &'a str,
    pub expires_at_ms: u64,
}

fn field(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
}

fn decode(value: &str) -> Result<Vec<u8>, Error> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| Error::InvalidEncoding)?;
    if URL_SAFE_NO_PAD.encode(&bytes) != value {
        return Err(Error::InvalidEncoding);
    }
    Ok(bytes)
}

fn validate_text(value: &str) -> Result<(), Error> {
    if value.is_empty() || value.len() > 256 || value.bytes().any(|b| !(0x21..=0x7e).contains(&b)) {
        return Err(Error::InvalidField);
    }
    Ok(())
}

impl EnvelopeV1 {
    fn validate_shape(&self) -> Result<(), Error> {
        if self.v != 1
            || !matches!(self.kind.as_str(), "request" | "response" | "push")
            || self.expires_at_ms == 0
        {
            return Err(Error::InvalidField);
        }
        for s in [&self.route, &self.sender, &self.recipient] {
            validate_text(s)?;
        }
        if self.request_id.is_empty()
            || self.request_id.len() > 128
            || self
                .request_id
                .bytes()
                .any(|b| !b.is_ascii_alphanumeric() && b != b'_' && b != b'-')
        {
            return Err(Error::InvalidField);
        }
        Ok(())
    }

    /// Stable framing independent of JSON key order and escaping.
    pub fn authenticated_header(&self) -> Result<Vec<u8>, Error> {
        self.validate_shape()?;
        let epk = decode(&self.ephemeral_key)?;
        let nonce = decode(&self.nonce)?;
        if epk.len() != 65 || epk[0] != 4 || nonce.len() != 12 {
            return Err(Error::InvalidEncoding);
        }
        let mut out = ENVELOPE_DOMAIN.to_vec();
        for value in [
            vec![self.v],
            self.route.as_bytes().to_vec(),
            self.sender.as_bytes().to_vec(),
            self.recipient.as_bytes().to_vec(),
            self.request_id.as_bytes().to_vec(),
            self.kind.as_bytes().to_vec(),
            self.expires_at_ms.to_be_bytes().to_vec(),
            epk,
            nonce,
        ] {
            field(&mut out, &value);
        }
        Ok(out)
    }

    fn signed_bytes(&self) -> Result<Vec<u8>, Error> {
        let mut bytes = self.authenticated_header()?;
        let ciphertext = decode(&self.ciphertext)?;
        if ciphertext.len() < 16 || ciphertext.len() > MAX_CIPHERTEXT {
            return Err(Error::InvalidField);
        }
        field(&mut bytes, &ciphertext);
        Ok(bytes)
    }

    pub fn verify_signature(
        &self,
        sender_key: &VerifyingKey,
        expected_sender: &str,
        expected_recipient: &str,
        expected_route: &str,
        now_ms: u64,
    ) -> Result<(), Error> {
        if self.sender != expected_sender
            || self.recipient != expected_recipient
            || self.route != expected_route
        {
            return Err(Error::WrongIdentity);
        }
        if now_ms > self.expires_at_ms {
            return Err(Error::Expired);
        }
        let signature = Signature::from_slice(&decode(&self.signature)?)
            .map_err(|_| Error::InvalidSignature)?;
        sender_key
            .verify(&self.signed_bytes()?, &signature)
            .map_err(|_| Error::InvalidSignature)
    }

    /// Caller must verify the pinned sender key and route before decrypting.
    pub fn open(
        &self,
        sender_key: &VerifyingKey,
        recipient_secret: &SecretKey,
        expected_sender: &str,
        expected_recipient: &str,
        expected_route: &str,
        now_ms: u64,
    ) -> Result<Vec<u8>, Error> {
        self.verify_signature(
            sender_key,
            expected_sender,
            expected_recipient,
            expected_route,
            now_ms,
        )?;
        let epk = PublicKey::from_sec1_bytes(&decode(&self.ephemeral_key)?)
            .map_err(|_| Error::InvalidKey)?;
        let shared = diffie_hellman(recipient_secret.to_nonzero_scalar(), epk.as_affine());
        let header = self.authenticated_header()?;
        let key = derive_key(shared.raw_secret_bytes().as_slice(), &header)?;
        let cipher =
            LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, &key).map_err(|_| Error::Crypto)?);
        let nonce: [u8; 12] = decode(&self.nonce)?
            .try_into()
            .map_err(|_| Error::InvalidEncoding)?;
        let mut ciphertext = decode(&self.ciphertext)?;
        let plaintext = cipher
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(&header),
                &mut ciphertext,
            )
            .map_err(|_| Error::Crypto)?;
        Ok(plaintext.to_vec())
    }

    /// Safe production entry point. Both the ephemeral P-256 key and nonce come from the OS CSPRNG.
    pub fn seal_random(
        context: Context<'_>,
        plaintext: &[u8],
        sender_secret: &SigningKey,
        recipient_key: &PublicKey,
    ) -> Result<Self, Error> {
        let rng = SystemRandom::new();
        let ephemeral_secret = loop {
            let mut bytes = [0u8; 32];
            rng.fill(&mut bytes).map_err(|_| Error::Crypto)?;
            if let Ok(secret) = SecretKey::from_slice(&bytes) {
                break secret;
            }
        };
        let mut nonce = [0u8; 12];
        rng.fill(&mut nonce).map_err(|_| Error::Crypto)?;
        Self::seal_with_material(
            context,
            plaintext,
            sender_secret,
            recipient_key,
            &ephemeral_secret,
            nonce,
        )
    }

    /// Deterministic core restricted to in-crate vector tests.
    fn seal_with_material(
        context: Context<'_>,
        plaintext: &[u8],
        sender_secret: &SigningKey,
        recipient_key: &PublicKey,
        ephemeral_secret: &SecretKey,
        nonce: [u8; 12],
    ) -> Result<Self, Error> {
        if plaintext.len() > MAX_CIPHERTEXT - 16 {
            return Err(Error::InvalidField);
        }
        let epk = ephemeral_secret.public_key().to_sec1_point(false);
        let mut envelope = Self {
            v: 1,
            route: context.route.into(),
            sender: context.sender.into(),
            recipient: context.recipient.into(),
            request_id: context.request_id.into(),
            kind: context.kind.into(),
            expires_at_ms: context.expires_at_ms,
            ephemeral_key: URL_SAFE_NO_PAD.encode(epk.as_bytes()),
            nonce: URL_SAFE_NO_PAD.encode(nonce),
            ciphertext: String::new(),
            signature: String::new(),
        };
        let header = envelope.authenticated_header()?;
        let shared = diffie_hellman(
            ephemeral_secret.to_nonzero_scalar(),
            recipient_key.as_affine(),
        );
        let key = derive_key(shared.raw_secret_bytes().as_slice(), &header)?;
        let cipher =
            LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, &key).map_err(|_| Error::Crypto)?);
        let mut ciphertext = plaintext.to_vec();
        cipher
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(&header),
                &mut ciphertext,
            )
            .map_err(|_| Error::Crypto)?;
        envelope.ciphertext = URL_SAFE_NO_PAD.encode(ciphertext);
        let signature: Signature = sender_secret.sign(&envelope.signed_bytes()?);
        envelope.signature = URL_SAFE_NO_PAD.encode(signature.to_bytes());
        Ok(envelope)
    }
}

fn derive_key(shared: &[u8], header: &[u8]) -> Result<[u8; 32], Error> {
    let salt = Sha256::digest(header);
    let hkdf = Hkdf::<Sha256>::new(Some(&salt), shared);
    let mut key = [0u8; 32];
    hkdf.expand(KEY_DOMAIN, &mut key)
        .map_err(|_| Error::Crypto)?;
    Ok(key)
}

/// Direct QR trust root: pin SHA-256 of the Mac's uncompressed SEC1 signing key.
/// A server-provided key must never replace this pin silently.
pub fn key_fingerprint(key: &VerifyingKey) -> [u8; 32] {
    Sha256::digest(key.to_sec1_point(false).as_bytes()).into()
}

pub fn http_challenge_bytes(
    device_id: &str,
    method: &str,
    path_and_query: &str,
    server_epoch: &[u8; 16],
    body: &[u8],
    timestamp_ms: u64,
    nonce: &[u8; 16],
) -> Result<Vec<u8>, Error> {
    validate_text(device_id)?;
    if method.is_empty()
        || method.len() > 16
        || method.bytes().any(|b| !b.is_ascii_uppercase())
        || !path_and_query.starts_with('/')
        || path_and_query.len() > 2048
    {
        return Err(Error::InvalidField);
    }
    let mut bytes = HTTP_DOMAIN.to_vec();
    for value in [
        device_id.as_bytes(),
        method.as_bytes(),
        path_and_query.as_bytes(),
        server_epoch.as_slice(),
        Sha256::digest(body).as_slice(),
        timestamp_ms.to_string().as_bytes(),
        nonce.as_slice(),
    ] {
        field(&mut bytes, value);
    }
    Ok(bytes)
}

#[allow(clippy::too_many_arguments)] // Mirrors the signed HTTP fields explicitly.
pub fn sign_http_challenge(
    secret: &SigningKey,
    device_id: &str,
    method: &str,
    path_and_query: &str,
    server_epoch: &[u8; 16],
    body: &[u8],
    timestamp_ms: u64,
    nonce: &[u8; 16],
) -> Result<String, Error> {
    let signature: Signature = secret.sign(&http_challenge_bytes(
        device_id,
        method,
        path_and_query,
        server_epoch,
        body,
        timestamp_ms,
        nonce,
    )?);
    Ok(URL_SAFE_NO_PAD.encode(signature.to_bytes()))
}

#[allow(clippy::too_many_arguments)]
fn verify_http_signature(
    key: &VerifyingKey,
    signature: &str,
    device_id: &str,
    method: &str,
    path_and_query: &str,
    server_epoch: &[u8; 16],
    body: &[u8],
    timestamp_ms: u64,
    nonce: &[u8; 16],
) -> Result<(), Error> {
    let signature =
        Signature::from_slice(&decode(signature)?).map_err(|_| Error::InvalidSignature)?;
    key.verify(
        &http_challenge_bytes(
            device_id,
            method,
            path_and_query,
            server_epoch,
            body,
            timestamp_ms,
            nonce,
        )?,
        &signature,
    )
    .map_err(|_| Error::InvalidSignature)
}

/// One-time verification for signed Mac HTTP requests. Keep one instance shared across all
/// relay workers; do not create a fresh instance for every request. Rejected signatures do not
/// consume nonces. A full live registry rejects new requests rather than evicting replay records.
/// A fresh signed server epoch invalidates captured requests after process restart. A backward
/// wall-clock step fails closed because previously expired nonce records cannot be recovered.
pub struct HttpChallengeVerifier {
    server_epoch: [u8; 16],
    max_skew_ms: u64,
    max_entries: usize,
    used: HashMap<(String, [u8; 16]), u64>,
    high_water_ms: Option<u64>,
}

impl HttpChallengeVerifier {
    pub fn new(max_skew_ms: u64, max_entries: usize) -> Result<Self, Error> {
        if max_skew_ms == 0 || max_skew_ms > 300_000 || max_entries == 0 {
            return Err(Error::InvalidField);
        }
        let mut server_epoch = [0u8; 16];
        SystemRandom::new()
            .fill(&mut server_epoch)
            .map_err(|_| Error::Crypto)?;
        Ok(Self {
            server_epoch,
            max_skew_ms,
            max_entries,
            used: HashMap::new(),
            high_water_ms: None,
        })
    }

    /// Publish this value as base64url on the bootstrap endpoint. The Mac must sign it exactly.
    pub fn server_epoch(&self) -> [u8; 16] {
        self.server_epoch
    }

    #[allow(clippy::too_many_arguments)] // Mirrors the HTTP authentication headers explicitly.
    pub fn verify(
        &mut self,
        key: &VerifyingKey,
        signature: &str,
        device_id: &str,
        method: &str,
        path_and_query: &str,
        body: &[u8],
        timestamp_ms: u64,
        nonce: &[u8; 16],
        now_ms: u64,
    ) -> Result<(), Error> {
        if timestamp_ms.abs_diff(now_ms) > self.max_skew_ms {
            return Err(Error::ClockSkew);
        }
        if self
            .high_water_ms
            .is_some_and(|high_water| now_ms < high_water)
        {
            return Err(Error::ClockRollback);
        }
        self.high_water_ms = Some(now_ms);
        self.used
            .retain(|_, valid_through| *valid_through >= now_ms);
        let nonce_key = (device_id.to_owned(), *nonce);
        if self.used.contains_key(&nonce_key) {
            return Err(Error::Replay);
        }
        if self.used.len() >= self.max_entries {
            return Err(Error::Capacity);
        }
        verify_http_signature(
            key,
            signature,
            device_id,
            method,
            path_and_query,
            &self.server_epoch,
            body,
            timestamp_ms,
            nonce,
        )?;
        self.used
            .insert(nonce_key, timestamp_ms.saturating_add(self.max_skew_ms));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TEST_PAYLOAD: &[u8] =
        br#"{"method":"POST","path":"/v1/questions/q1/answer","body":{"answer":"hola"}}"#;

    fn keys() -> (SigningKey, SecretKey, SecretKey) {
        (
            SigningKey::from_slice(&[1u8; 32]).unwrap(),
            SecretKey::from_slice(&[2u8; 32]).unwrap(),
            SecretKey::from_slice(&[3u8; 32]).unwrap(),
        )
    }

    fn fixture() -> EnvelopeV1 {
        let (sender, recipient, ephemeral) = keys();
        EnvelopeV1::seal_with_material(
            Context {
                route: "mac-1",
                sender: "phone-1",
                recipient: "mac-1",
                request_id: "op-123",
                kind: "request",
                expires_at_ms: 1_800_000_000_000,
            },
            TEST_PAYLOAD,
            &sender,
            &recipient.public_key(),
            &ephemeral,
            [4u8; 12],
        )
        .unwrap()
    }

    #[test]
    fn sealed_request_opens_only_for_pinned_sender_and_target() {
        let (sender, recipient, _) = keys();
        let envelope = fixture();
        assert_eq!(
            envelope
                .open(
                    sender.verifying_key(),
                    &recipient,
                    "phone-1",
                    "mac-1",
                    "mac-1",
                    1_700_000_000_000
                )
                .unwrap(),
            TEST_PAYLOAD
        );
        assert_eq!(
            envelope.verify_signature(
                sender.verifying_key(),
                "phone-1",
                "mac-2",
                "mac-1",
                1_700_000_000_000
            ),
            Err(Error::WrongIdentity)
        );
        assert_eq!(
            envelope.verify_signature(
                sender.verifying_key(),
                "phone-1",
                "mac-1",
                "mac-1",
                1_800_000_000_001
            ),
            Err(Error::Expired)
        );
        assert_eq!(
            envelope.open(
                sender.verifying_key(),
                &SecretKey::from_slice(&[5u8; 32]).unwrap(),
                "phone-1",
                "mac-1",
                "mac-1",
                1_700_000_000_000
            ),
            Err(Error::Crypto)
        );
    }

    #[test]
    fn every_routing_field_and_ciphertext_is_signed() {
        let (sender, _, _) = keys();
        let original = fixture();
        let check = |e: EnvelopeV1| {
            e.verify_signature(
                sender.verifying_key(),
                &e.sender.clone(),
                &e.recipient.clone(),
                &e.route.clone(),
                1_700_000_000_000,
            )
        };
        let mut e = original.clone();
        e.route = "mac-2".into();
        assert_eq!(check(e), Err(Error::InvalidSignature));
        let mut e = original.clone();
        e.request_id = "op-124".into();
        assert_eq!(check(e), Err(Error::InvalidSignature));
        let mut e = original.clone();
        e.kind = "response".into();
        assert_eq!(check(e), Err(Error::InvalidSignature));
        let mut e = original.clone();
        e.ciphertext = URL_SAFE_NO_PAD.encode([0u8; 32]);
        assert_eq!(check(e), Err(Error::InvalidSignature));
        let mut e = original;
        e.nonce = URL_SAFE_NO_PAD.encode([0u8; 12]);
        assert_eq!(check(e), Err(Error::InvalidSignature));
    }

    #[test]
    fn request_id_is_safe_for_relay_path_segments() {
        let mut e = fixture();
        for invalid in ["", "a/b", "a?b", "a.b", "a b"] {
            e.request_id = invalid.into();
            assert_eq!(e.authenticated_header(), Err(Error::InvalidField));
        }
        e.request_id = "a".repeat(129);
        assert_eq!(e.authenticated_header(), Err(Error::InvalidField));
        e.request_id = "f4A_0-1".into();
        assert!(e.authenticated_header().is_ok());
    }

    #[test]
    fn http_challenge_binds_body_path_and_nonce() {
        let (sender, _, _) = keys();
        let nonce = [9u8; 16];
        let now = 1_700_000_000_000;
        let mut verifier = HttpChallengeVerifier::new(30_000, 2).unwrap();
        let epoch = verifier.server_epoch();
        let sig = sign_http_challenge(
            &sender,
            "mac-1",
            "POST",
            "/v1/work?after=2",
            &epoch,
            b"payload",
            now,
            &nonce,
        )
        .unwrap();
        assert_eq!(
            verifier.verify(
                sender.verifying_key(),
                &sig,
                "mac-1",
                "POST",
                "/v1/work?after=3",
                b"payload",
                now,
                &nonce,
                now
            ),
            Err(Error::InvalidSignature)
        );
        assert_eq!(
            verifier.verify(
                sender.verifying_key(),
                &sig,
                "mac-1",
                "POST",
                "/v1/work?after=2",
                b"payload2",
                now,
                &nonce,
                now
            ),
            Err(Error::InvalidSignature)
        );
        assert!(
            verifier
                .verify(
                    sender.verifying_key(),
                    &sig,
                    "mac-1",
                    "POST",
                    "/v1/work?after=2",
                    b"payload",
                    now,
                    &nonce,
                    now,
                )
                .is_ok()
        );
        assert_eq!(
            verifier.verify(
                sender.verifying_key(),
                &sig,
                "mac-1",
                "POST",
                "/v1/work?after=2",
                b"payload",
                now,
                &nonce,
                now,
            ),
            Err(Error::Replay)
        );
        let other_nonce = [10u8; 16];
        let other_sig = sign_http_challenge(
            &sender,
            "mac-1",
            "POST",
            "/v1/work?after=2",
            &epoch,
            b"payload",
            now,
            &other_nonce,
        )
        .unwrap();
        assert_eq!(
            verifier.verify(
                sender.verifying_key(),
                &other_sig,
                "mac-1",
                "POST",
                "/v1/work?after=2",
                b"payload",
                now,
                &other_nonce,
                now + 30_001,
            ),
            Err(Error::ClockSkew)
        );
        assert_eq!(
            verifier.verify(
                sender.verifying_key(),
                &other_sig,
                "mac-1",
                "POST",
                "/v1/work?after=2",
                b"payload",
                now,
                &other_nonce,
                now - 30_001
            ),
            Err(Error::ClockSkew)
        );
        assert!(
            verifier
                .verify(
                    sender.verifying_key(),
                    &other_sig,
                    "mac-1",
                    "POST",
                    "/v1/work?after=2",
                    b"payload",
                    now,
                    &other_nonce,
                    now
                )
                .is_ok()
        );
        let third_nonce = [11u8; 16];
        let third_sig = sign_http_challenge(
            &sender,
            "mac-1",
            "POST",
            "/v1/work?after=2",
            &epoch,
            b"payload",
            now,
            &third_nonce,
        )
        .unwrap();
        assert_eq!(
            verifier.verify(
                sender.verifying_key(),
                &third_sig,
                "mac-1",
                "POST",
                "/v1/work?after=2",
                b"payload",
                now,
                &third_nonce,
                now
            ),
            Err(Error::Capacity)
        );
        assert_eq!(
            verifier.verify(
                sender.verifying_key(),
                &third_sig,
                "mac-1",
                "POST",
                "/v1/work?after=2",
                b"payload",
                now,
                &third_nonce,
                now + 30_001
            ),
            Err(Error::ClockSkew)
        );
        let mut restarted = HttpChallengeVerifier::new(30_000, 2).unwrap();
        assert_ne!(restarted.server_epoch(), epoch);
        assert_eq!(
            restarted.verify(
                sender.verifying_key(),
                &sig,
                "mac-1",
                "POST",
                "/v1/work?after=2",
                b"payload",
                now,
                &nonce,
                now
            ),
            Err(Error::InvalidSignature)
        );
    }

    #[test]
    fn cryptokit_signed_http_challenge_verifies_in_rust() {
        let (sender, _, _) = keys();
        // Generated by tests/cryptokit_vector.swift with a fixed key and message.
        let swift_signature = "OTD_fcE5DaYS2h3OxzmNoiI8-ozly1LuohvsX5JQLZUPaEgzerkRxk58vWO8SdKg7-zfG7FqzvoQMGbDLObsOw";
        assert!(
            verify_http_signature(
                sender.verifying_key(),
                swift_signature,
                "mac-1",
                "POST",
                "/v1/work?after=2",
                &[7u8; 16],
                b"payload",
                1_700_000_000_000,
                &[9u8; 16]
            )
            .is_ok()
        );
    }

    #[test]
    fn clock_rollback_cannot_replay_a_pruned_nonce() {
        let (sender, _, _) = keys();
        let mut verifier = HttpChallengeVerifier::new(30_000, 2).unwrap();
        let epoch = verifier.server_epoch();
        let t = 1_700_000_000_000;
        let first_nonce = [31u8; 16];
        let second_nonce = [32u8; 16];
        let first = sign_http_challenge(
            &sender,
            "mac-1",
            "POST",
            "/v1/work",
            &epoch,
            b"first",
            t,
            &first_nonce,
        )
        .unwrap();
        assert!(
            verifier
                .verify(
                    sender.verifying_key(),
                    &first,
                    "mac-1",
                    "POST",
                    "/v1/work",
                    b"first",
                    t,
                    &first_nonce,
                    t
                )
                .is_ok()
        );
        let later = t + 30_001;
        let second = sign_http_challenge(
            &sender,
            "mac-1",
            "POST",
            "/v1/work",
            &epoch,
            b"second",
            later,
            &second_nonce,
        )
        .unwrap();
        assert!(
            verifier
                .verify(
                    sender.verifying_key(),
                    &second,
                    "mac-1",
                    "POST",
                    "/v1/work",
                    b"second",
                    later,
                    &second_nonce,
                    later
                )
                .is_ok()
        );
        assert_eq!(verifier.used.len(), 1, "the first nonce was pruned");
        assert_eq!(
            verifier.verify(
                sender.verifying_key(),
                &first,
                "mac-1",
                "POST",
                "/v1/work",
                b"first",
                t,
                &first_nonce,
                t
            ),
            Err(Error::ClockRollback)
        );
        assert_eq!(
            verifier.verify(
                sender.verifying_key(),
                &second,
                "mac-1",
                "POST",
                "/v1/work",
                b"second",
                later,
                &second_nonce,
                later
            ),
            Err(Error::Replay)
        );
    }

    #[test]
    fn production_seal_uses_fresh_ephemeral_material() {
        let (sender, recipient, _) = keys();
        let context = || Context {
            route: "mac-1",
            sender: "phone-1",
            recipient: "mac-1",
            request_id: "op-123",
            kind: "request",
            expires_at_ms: 1_800_000_000_000,
        };
        let first =
            EnvelopeV1::seal_random(context(), b"hello", &sender, &recipient.public_key()).unwrap();
        let second =
            EnvelopeV1::seal_random(context(), b"hello", &sender, &recipient.public_key()).unwrap();
        assert_ne!(first.ephemeral_key, second.ephemeral_key);
        assert_ne!(first.ciphertext, second.ciphertext);
        assert_eq!(
            first
                .open(
                    sender.verifying_key(),
                    &recipient,
                    "phone-1",
                    "mac-1",
                    "mac-1",
                    1_700_000_000_000
                )
                .unwrap(),
            b"hello"
        );
    }

    #[test]
    fn fixed_vector_for_cryptokit() {
        let e = fixture();
        let (sender, recipient, ephemeral) = keys();
        assert!(
            e.verify_signature(
                sender.verifying_key(),
                "phone-1",
                "mac-1",
                "mac-1",
                1_700_000_000_000
            )
            .is_ok()
        );
        let header = e.authenticated_header().unwrap();
        let shared = diffie_hellman(
            ephemeral.to_nonzero_scalar(),
            recipient.public_key().as_affine(),
        );
        let key = derive_key(shared.raw_secret_bytes().as_slice(), &header).unwrap();
        println!("header={}", URL_SAFE_NO_PAD.encode(&header));
        println!("key={}", URL_SAFE_NO_PAD.encode(key));
        println!("envelope={}", serde_json::to_string(&e).unwrap());
        println!(
            "sender_public={}",
            URL_SAFE_NO_PAD.encode(sender.verifying_key().to_sec1_point(false).as_bytes())
        );
        println!(
            "recipient_secret={}",
            URL_SAFE_NO_PAD.encode(recipient.to_bytes())
        );
    }
}
