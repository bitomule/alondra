//! Ephemeral rendezvous for endpoint-controlled pairing. The relay cannot approve a phone.
//! Its only durable authorization remains a separate Mac-signed `/v1/devices` registration.
use alondra_e2e::Envelope;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Mutex};
use subtle::ConstantTimeEq;

const MAX_SESSIONS: usize = 64;
const MAX_TTL_MS: u64 = 10 * 60_000;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRequest {
    pub session_id: String,
    pub mac_id: String,
    pub secret_hash: String,
    pub expires_at_ms: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateRequest {
    pub secret: String,
    pub device_id: String,
    pub signing_key: String,
    pub envelope: Envelope,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CandidateView {
    pub device_id: String,
    pub signing_key: String,
    pub envelope: Envelope,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    pub v: u8,
    pub session_id: String,
    pub owner_sub: String,
    pub mac_id: String,
    pub mac_signing_key: String,
    pub mac_agreement_key: String,
    pub device_id: String,
    pub phone_signing_key: String,
    pub phone_agreement_key: String,
    pub expires_at_ms: u64,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApprovalProof {
    pub approval: Approval,
    /// P-256 ECDSA/SHA-256 64-byte r||s, base64url without padding.
    pub signature: String,
}

impl Approval {
    /// Binary protocol: domain, version byte, eight length-prefixed UTF-8 fields, u64 BE expiry.
    /// This is independent of JSON key order and straightforward to reproduce in CryptoKit.
    pub fn signed_bytes(&self) -> Vec<u8> {
        let mut out = b"alondra-service-pairing-approval-v1\0".to_vec();
        out.push(self.v);
        for field in [
            &self.session_id,
            &self.owner_sub,
            &self.mac_id,
            &self.mac_signing_key,
            &self.mac_agreement_key,
            &self.device_id,
            &self.phone_signing_key,
            &self.phone_agreement_key,
        ] {
            out.extend_from_slice(&(field.len() as u32).to_be_bytes());
            out.extend_from_slice(field.as_bytes());
        }
        out.extend_from_slice(&self.expires_at_ms.to_be_bytes());
        out
    }
}

pub fn verify_proof(proof: &ApprovalProof) -> Result<(), &'static str> {
    let key = VerifyingKey::from_sec1_bytes(
        &URL_SAFE_NO_PAD
            .decode(&proof.approval.mac_signing_key)
            .map_err(|_| "invalid_mac_key")?,
    )
    .map_err(|_| "invalid_mac_key")?;
    let raw = URL_SAFE_NO_PAD
        .decode(&proof.signature)
        .map_err(|_| "invalid_approval_signature")?;
    let signature = Signature::from_slice(&raw).map_err(|_| "invalid_approval_signature")?;
    key.verify(&proof.approval.signed_bytes(), &signature)
        .map_err(|_| "invalid_approval_signature")
}

struct Session {
    mac_id: String,
    owner_sub: String,
    secret_hash: [u8; 32],
    expires_at_ms: u64,
    candidate: Option<CandidateView>,
    approval: Option<ApprovalProof>,
}

#[derive(Default)]
pub struct Mailbox(Mutex<HashMap<String, Session>>);

impl Mailbox {
    pub fn purge_device(&self,owner:&str,kind:&str,id:&str) {
        self.0.lock().unwrap().retain(|_,s|s.owner_sub!=owner || if kind=="mac" {s.mac_id!=id} else {s.candidate.as_ref().is_none_or(|c|c.device_id!=id)});
    }
    pub fn purge_owner(&self,owner:&str) {self.0.lock().unwrap().retain(|_,session|session.owner_sub!=owner);}

    pub fn open(&self, req: SessionRequest, owner_sub: &str, now: u64) -> Result<(), &'static str> {
        if !valid_id(&req.session_id)
            || req.mac_id.is_empty()
            || req.expires_at_ms <= now
            || req.expires_at_ms > now.saturating_add(MAX_TTL_MS)
        {
            return Err("invalid_session");
        }
        let hash: [u8; 32] = base64::Engine::decode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            &req.secret_hash,
        )
        .map_err(|_| "invalid_secret_hash")?
        .try_into()
        .map_err(|_| "invalid_secret_hash")?;
        let mut sessions = self.0.lock().unwrap();
        sessions.retain(|_, s| s.expires_at_ms > now);
        if sessions.contains_key(&req.session_id) {
            return Err("session_exists");
        }
        if sessions.len() >= MAX_SESSIONS {
            return Err("too_many_sessions");
        }
        sessions.insert(
            req.session_id,
            Session {
                mac_id: req.mac_id,
                owner_sub: owner_sub.to_owned(),
                secret_hash: hash,
                expires_at_ms: req.expires_at_ms,
                candidate: None,
                approval: None,
            },
        );
        Ok(())
    }

    pub fn mac(
        &self,
        id: &str,
        mac_id: &str,
        now: u64,
    ) -> Result<Option<CandidateView>, &'static str> {
        let sessions = self.0.lock().unwrap();
        let s = sessions.get(id).ok_or("unknown_session")?;
        if s.expires_at_ms <= now || s.mac_id != mac_id {
            return Err("unknown_session");
        }
        Ok(s.candidate.clone())
    }

    pub fn route(&self, id: &str, now: u64) -> Result<(String, String), &'static str> {
        let sessions = self.0.lock().unwrap();
        let s = sessions.get(id).ok_or("unknown_session")?;
        if s.expires_at_ms <= now {
            return Err("unknown_session");
        }
        Ok((s.mac_id.clone(), s.owner_sub.clone()))
    }

    pub fn candidate(
        &self,
        id: &str,
        req: CandidateRequest,
        owner_sub: &str,
        now: u64,
    ) -> Result<(), &'static str> {
        let mut sessions = self.0.lock().unwrap();
        let s = sessions.get_mut(id).ok_or("unknown_session")?;
        if s.expires_at_ms <= now {
            return Err("unknown_session");
        }
        let secret = base64::Engine::decode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            &req.secret,
        )
        .map_err(|_| "invalid_secret")?;
        let hash: [u8; 32] = Sha256::digest(secret).into();
        if !bool::from(hash.ct_eq(&s.secret_hash)) {
            return Err("invalid_secret");
        }
        if owner_sub != s.owner_sub
            || req.envelope.kind != "pairing"
            || req.envelope.route != s.mac_id
            || req.envelope.recipient != s.mac_id
            || req.envelope.sender != req.device_id
            || req.envelope.request_id != id
            || req.envelope.expires_at_ms > s.expires_at_ms
            || req.envelope.expires_at_ms <= now
        {
            return Err("invalid_candidate");
        }
        let view = CandidateView {
            device_id: req.device_id,
            signing_key: req.signing_key,
            envelope: req.envelope,
        };
        if let Some(previous) = &s.candidate {
            return if previous == &view {
                Ok(())
            } else {
                Err("candidate_exists")
            };
        }
        s.candidate = Some(view);
        Ok(())
    }

    pub fn approve(
        &self,
        id: &str,
        proof: ApprovalProof,
        mac_id: &str,
        owner_sub: &str,
        now: u64,
    ) -> Result<(), &'static str> {
        let mut sessions = self.0.lock().unwrap();
        let s = sessions.get_mut(id).ok_or("unknown_session")?;
        if s.expires_at_ms <= now || s.mac_id != mac_id || s.owner_sub != owner_sub {
            return Err("unknown_session");
        }
        let c = s.candidate.as_ref().ok_or("no_candidate")?;
        let a = &proof.approval;
        if a.v != 1
            || a.session_id != id
            || a.owner_sub != owner_sub
            || a.mac_id != mac_id
            || a.device_id != c.device_id
            || a.phone_signing_key != c.signing_key
            || a.expires_at_ms != s.expires_at_ms
        {
            return Err("approval_mismatch");
        }
        if let Some(previous) = &s.approval {
            return if previous == &proof {
                Ok(())
            } else {
                Err("approval_exists")
            };
        }
        s.approval = Some(proof);
        Ok(())
    }

    pub fn approval(
        &self,
        id: &str,
        device_id: &str,
        owner_sub: &str,
        now: u64,
    ) -> Result<Option<ApprovalProof>, &'static str> {
        let sessions = self.0.lock().unwrap();
        let s = sessions.get(id).ok_or("unknown_session")?;
        if s.expires_at_ms <= now
            || s.owner_sub != owner_sub
            || s.candidate
                .as_ref()
                .is_none_or(|c| c.device_id != device_id)
        {
            return Err("unknown_session");
        }
        Ok(s.approval.clone())
    }

    pub fn candidate_key(
        &self,
        id: &str,
        device_id: &str,
        now: u64,
    ) -> Result<(String, String), &'static str> {
        let sessions = self.0.lock().unwrap();
        let s = sessions.get(id).ok_or("unknown_session")?;
        if s.expires_at_ms <= now {
            return Err("unknown_session");
        }
        let c = s.candidate.as_ref().ok_or("no_candidate")?;
        if c.device_id != device_id {
            return Err("unknown_candidate");
        }
        Ok((s.mac_id.clone(), c.signing_key.clone()))
    }
}

fn valid_id(id: &str) -> bool {
    id.starts_with("pair-")
        && id.len() <= 80
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::{SigningKey, signature::Signer};
    fn envelope() -> Envelope {
        Envelope {
            v: 1,
            route: "mac-a".into(),
            sender: "phone-a".into(),
            recipient: "mac-a".into(),
            request_id: "pair-test".into(),
            kind: "pairing".into(),
            expires_at_ms: 600_000,
            ephemeral_key: "opaque".into(),
            nonce: "opaque".into(),
            ciphertext: "opaque".into(),
            signature: "opaque".into(),
        }
    }
    #[test]
    fn session_is_single_candidate_and_expires() {
        let mailbox = Mailbox::default();
        let secret = [7u8; 16];
        let hash = Sha256::digest(secret);
        mailbox
            .open(
                SessionRequest {
                    session_id: "pair-test".into(),
                    mac_id: "mac-a".into(),
                    secret_hash: base64::Engine::encode(
                        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                        hash,
                    ),
                    expires_at_ms: 600_000,
                },
                "owner",
                1,
            )
            .unwrap();
        assert!(mailbox.route("pair-test", 600_000).is_err());
        assert!(mailbox.route("pair-test", 2).is_ok());
        let candidate = CandidateRequest {
            secret: URL_SAFE_NO_PAD.encode(secret),
            device_id: "phone-a".into(),
            signing_key: "key".into(),
            envelope: envelope(),
        };
        assert_eq!(
            mailbox
                .candidate("pair-test", candidate.clone(), "other", 2)
                .unwrap_err(),
            "invalid_candidate"
        );
        mailbox
            .candidate("pair-test", candidate.clone(), "owner", 2)
            .unwrap();
        mailbox
            .candidate("pair-test", candidate.clone(), "owner", 2)
            .unwrap();
        let mut substitution = candidate;
        substitution.signing_key = "attacker".into();
        assert_eq!(
            mailbox
                .candidate("pair-test", substitution, "owner", 2)
                .unwrap_err(),
            "candidate_exists"
        );
    }

    #[test]
    fn approval_signature_binds_every_public_key() {
        let signing = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let key = URL_SAFE_NO_PAD.encode(signing.verifying_key().to_sec1_bytes());
        let approval = Approval {
            v: 1,
            session_id: "pair-a".into(),
            owner_sub: "owner".into(),
            mac_id: "mac-a".into(),
            mac_signing_key: key,
            mac_agreement_key: "mac-agree".into(),
            device_id: "phone-a".into(),
            phone_signing_key: "phone-sign".into(),
            phone_agreement_key: "phone-agree".into(),
            expires_at_ms: 600_000,
        };
        let signature: Signature = signing.sign(&approval.signed_bytes());
        let mut proof = ApprovalProof {
            approval,
            signature: URL_SAFE_NO_PAD.encode(signature.to_bytes()),
        };
        verify_proof(&proof).unwrap();
        proof.approval.phone_agreement_key = "attacker-key".into();
        assert_eq!(
            verify_proof(&proof).unwrap_err(),
            "invalid_approval_signature"
        );
    }
}
