//! Direct pairing over a local network, without a relay or QR.
//!
//! A client that found a Mac by Bonjour (or by address) reads the Mac's public keys from
//! `GET /direct/hello`, then runs a commit-then-reveal exchange so both screens can show the
//! same short code, as Bluetooth numeric comparison does:
//!
//! 1. The client sends its fresh keys and `commitment(Np, keys)` without revealing `Np`.
//! 2. The Mac answers with its own random `Nm`.
//! 3. The client reveals `Np`; the Mac checks it against the commitment.
//!
//! Both sides show [`Transcript::code`]. A machine in the middle has to fix its half before it
//! sees the other side's nonce, so it gets one guess per attempt at a 24-bit code instead of
//! being able to grind keys until two codes collide.
//!
//! Once the user approves on the Mac, the Mac signs an [`Approval`] that binds every public key,
//! both identities and the transcript. The client verifies it against the keys it read from
//! `hello` and pins those keys. The domains below differ from relay pairing so neither proof can
//! stand in for the other.

use crate::{Error, decode};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::ecdsa::{
    Signature, SigningKey, VerifyingKey,
    signature::{Signer, Verifier},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const COMMIT_DOMAIN: &[u8] = b"alondra-direct-pairing-commit\0";
const CODE_DOMAIN: &[u8] = b"alondra-direct-pairing-code\0";
const TRANSCRIPT_DOMAIN: &[u8] = b"alondra-direct-pairing-transcript\0";
const APPROVAL_DOMAIN: &[u8] = b"alondra-direct-pairing-approval\0";
pub const NONCE_LEN: usize = 32;
pub const CODE_LEN: usize = 6;

fn field(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
}

fn nonce(value: &str) -> Result<Vec<u8>, Error> {
    let bytes = decode(value)?;
    if bytes.len() != NONCE_LEN {
        return Err(Error::InvalidField);
    }
    Ok(bytes)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A fresh random nonce, base64url without padding.
pub fn random_nonce() -> Result<String, Error> {
    let mut bytes = [0u8; NONCE_LEN];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|_| Error::Crypto)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The client's binding to its secret nonce and keys, sent before the Mac picks its nonce.
pub fn commitment(
    client_nonce: &str,
    device_id: &str,
    signing_key: &str,
    agreement_key: &str,
) -> Result<String, Error> {
    let mut bytes = COMMIT_DOMAIN.to_vec();
    field(&mut bytes, &nonce(client_nonce)?);
    for value in [device_id, signing_key, agreement_key] {
        field(&mut bytes, value.as_bytes());
    }
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)))
}

pub fn verify_commitment(
    commitment_value: &str,
    client_nonce: &str,
    device_id: &str,
    signing_key: &str,
    agreement_key: &str,
) -> Result<(), Error> {
    let expected = commitment(client_nonce, device_id, signing_key, agreement_key)?;
    if !constant_time_eq(expected.as_bytes(), commitment_value.as_bytes()) {
        return Err(Error::InvalidSignature);
    }
    Ok(())
}

/// Everything both sides saw during one pairing attempt. Keys are the base64url wire strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript<'a> {
    pub pairing_id: &'a str,
    pub mac_id: &'a str,
    pub mac_signing_key: &'a str,
    pub mac_agreement_key: &'a str,
    pub device_id: &'a str,
    pub client_signing_key: &'a str,
    pub client_agreement_key: &'a str,
    pub client_nonce: &'a str,
    pub mac_nonce: &'a str,
}

impl Transcript<'_> {
    fn bytes(&self, domain: &[u8]) -> Result<Vec<u8>, Error> {
        let mut bytes = domain.to_vec();
        for value in [
            self.pairing_id,
            self.mac_id,
            self.mac_signing_key,
            self.mac_agreement_key,
            self.device_id,
            self.client_signing_key,
            self.client_agreement_key,
        ] {
            field(&mut bytes, value.as_bytes());
        }
        field(&mut bytes, &nonce(self.client_nonce)?);
        field(&mut bytes, &nonce(self.mac_nonce)?);
        Ok(bytes)
    }

    /// The code both screens show: six uppercase hex characters.
    pub fn code(&self) -> Result<String, Error> {
        let digest = Sha256::digest(self.bytes(CODE_DOMAIN)?);
        let hex: String = digest[..CODE_LEN / 2].iter().map(|b| format!("{b:02X}")).collect();
        Ok(hex)
    }

    /// The transcript hash the Mac's approval signs.
    pub fn digest(&self) -> Result<String, Error> {
        Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(self.bytes(TRANSCRIPT_DOMAIN)?)))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    pub pairing_id: String,
    pub mac_id: String,
    pub mac_signing_key: String,
    pub mac_agreement_key: String,
    pub device_id: String,
    pub client_signing_key: String,
    pub client_agreement_key: String,
    pub transcript: String,
    pub expires_at_ms: u64,
}

impl Approval {
    pub fn signed_bytes(&self) -> Vec<u8> {
        let mut bytes = APPROVAL_DOMAIN.to_vec();
        for value in [
            &self.pairing_id,
            &self.mac_id,
            &self.mac_signing_key,
            &self.mac_agreement_key,
            &self.device_id,
            &self.client_signing_key,
            &self.client_agreement_key,
            &self.transcript,
        ] {
            field(&mut bytes, value.as_bytes());
        }
        bytes.extend_from_slice(&self.expires_at_ms.to_be_bytes());
        bytes
    }

    pub fn sign(&self, mac: &SigningKey) -> String {
        let signature: Signature = mac.sign(&self.signed_bytes());
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    }

    /// Checks the Mac's signature and that the approval names exactly the transcript the client
    /// ran, with the keys it read from `hello` and generated itself.
    pub fn verify(
        &self,
        signature: &str,
        mac: &VerifyingKey,
        expected: &Transcript<'_>,
        now_ms: u64,
    ) -> Result<(), Error> {
        let signature =
            Signature::from_slice(&decode(signature)?).map_err(|_| Error::InvalidSignature)?;
        mac.verify(&self.signed_bytes(), &signature)
            .map_err(|_| Error::InvalidSignature)?;
        let matches = self.pairing_id == expected.pairing_id
            && self.mac_id == expected.mac_id
            && self.mac_signing_key == expected.mac_signing_key
            && self.mac_agreement_key == expected.mac_agreement_key
            && self.device_id == expected.device_id
            && self.client_signing_key == expected.client_signing_key
            && self.client_agreement_key == expected.client_agreement_key
            && self.transcript == expected.digest()?;
        if !matches {
            return Err(Error::WrongIdentity);
        }
        if now_ms >= self.expires_at_ms {
            return Err(Error::Expired);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::elliptic_curve::sec1::ToSec1Point;

    fn key(seed: u8) -> String {
        let signing = SigningKey::from_slice(&[seed; 32]).unwrap();
        URL_SAFE_NO_PAD.encode(signing.verifying_key().to_sec1_point(false).as_bytes())
    }

    fn nonce_of(seed: u8) -> String {
        URL_SAFE_NO_PAD.encode([seed; NONCE_LEN])
    }

    struct Fixture {
        mac_signing: String,
        mac_agreement: String,
        client_signing: String,
        client_agreement: String,
        client_nonce: String,
        mac_nonce: String,
    }

    fn fixture() -> Fixture {
        Fixture {
            mac_signing: key(1),
            mac_agreement: key(2),
            client_signing: key(3),
            client_agreement: key(4),
            client_nonce: nonce_of(5),
            mac_nonce: nonce_of(6),
        }
    }

    fn transcript(f: &Fixture) -> Transcript<'_> {
        Transcript {
            pairing_id: "pairing-1",
            mac_id: "mac-1",
            mac_signing_key: &f.mac_signing,
            mac_agreement_key: &f.mac_agreement,
            device_id: "client-1",
            client_signing_key: &f.client_signing,
            client_agreement_key: &f.client_agreement,
            client_nonce: &f.client_nonce,
            mac_nonce: &f.mac_nonce,
        }
    }

    #[test]
    fn fixed_vector_for_swift() {
        let f = fixture();
        let t = transcript(&f);
        assert_eq!(
            commitment(&f.client_nonce, "client-1", &f.client_signing, &f.client_agreement).unwrap(),
            "tGD__kdXmgEOwFVNao_SKTOW91CptgNOe-SWHLOzP1w"
        );
        assert_eq!(t.code().unwrap(), "36A0E0");
        assert_eq!(t.digest().unwrap(), "oLgRJFfS1Fdz6LXD2bn1nVz1sOq9NHBkf-WevmCEiBM");
    }

    #[test]
    fn commitment_rejects_a_different_nonce_or_key() {
        let f = fixture();
        let c = commitment(&f.client_nonce, "client-1", &f.client_signing, &f.client_agreement).unwrap();
        assert!(verify_commitment(&c, &f.client_nonce, "client-1", &f.client_signing, &f.client_agreement).is_ok());
        assert_eq!(
            verify_commitment(&c, &nonce_of(7), "client-1", &f.client_signing, &f.client_agreement),
            Err(Error::InvalidSignature)
        );
        assert_eq!(
            verify_commitment(&c, &f.client_nonce, "client-1", &key(9), &f.client_agreement),
            Err(Error::InvalidSignature)
        );
        assert_eq!(
            commitment("short", "client-1", &f.client_signing, &f.client_agreement),
            Err(Error::InvalidEncoding)
        );
    }

    #[test]
    fn a_substituted_mac_or_client_key_changes_the_code() {
        let f = fixture();
        let honest = transcript(&f).code().unwrap();
        let fake_mac = key(10);
        let fake_client = key(11);
        assert_ne!(Transcript { mac_signing_key: &fake_mac, ..transcript(&f) }.code().unwrap(), honest);
        assert_ne!(Transcript { client_signing_key: &fake_client, ..transcript(&f) }.code().unwrap(), honest);
        let other_nonce = nonce_of(12);
        assert_ne!(Transcript { mac_nonce: &other_nonce, ..transcript(&f) }.code().unwrap(), honest);
        assert_eq!(honest.len(), CODE_LEN);
    }

    fn approval(f: &Fixture) -> Approval {
        let t = transcript(f);
        Approval {
            pairing_id: t.pairing_id.into(),
            mac_id: t.mac_id.into(),
            mac_signing_key: f.mac_signing.clone(),
            mac_agreement_key: f.mac_agreement.clone(),
            device_id: t.device_id.into(),
            client_signing_key: f.client_signing.clone(),
            client_agreement_key: f.client_agreement.clone(),
            transcript: t.digest().unwrap(),
            expires_at_ms: 2_000,
        }
    }

    #[test]
    fn approval_verifies_only_against_the_pinned_mac_and_own_transcript() {
        let f = fixture();
        let mac = SigningKey::from_slice(&[1u8; 32]).unwrap();
        let a = approval(&f);
        let signature = a.sign(&mac);
        assert!(a.verify(&signature, mac.verifying_key(), &transcript(&f), 1_000).is_ok());

        let impostor = SigningKey::from_slice(&[10u8; 32]).unwrap();
        assert_eq!(
            a.verify(&a.sign(&impostor), mac.verifying_key(), &transcript(&f), 1_000),
            Err(Error::InvalidSignature)
        );
        let other_nonce = nonce_of(13);
        let other = Transcript { mac_nonce: &other_nonce, ..transcript(&f) };
        assert_eq!(a.verify(&signature, mac.verifying_key(), &other, 1_000), Err(Error::WrongIdentity));
        assert_eq!(a.verify(&signature, mac.verifying_key(), &transcript(&f), 2_000), Err(Error::Expired));

        let mut tampered = a.clone();
        tampered.device_id = "client-2".into();
        assert_eq!(
            tampered.verify(&signature, mac.verifying_key(), &transcript(&f), 1_000),
            Err(Error::InvalidSignature)
        );
    }

    #[test]
    fn direct_approval_bytes_differ_from_relay_pairing_domain() {
        let f = fixture();
        assert!(approval(&f).signed_bytes().starts_with(b"alondra-direct-pairing-approval\0"));
    }
}
