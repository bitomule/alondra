//! The terminal consumes a destination invitation; only a human on that Mac approves it.
use crate::service_client::{self as service, Profile};
use alondra_e2e::{Context as EnvelopeContext, Envelope};
use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use p256::{
    PublicKey, SecretKey,
    ecdsa::{Signature, SigningKey, VerifyingKey, signature::Verifier},
};
use serde::Deserialize;
use serde_json::json;
use std::{
    io::{self, Write},
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Invitation {
    version: u8,
    url: String,
    mac_id: String,
    mac_signing_key: String,
    mac_agreement_key: String,
    session_id: String,
    secret: String,
    expires_at_ms: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    v: u8,
    session_id: String,
    owner_sub: String,
    mac_id: String,
    mac_signing_key: String,
    mac_agreement_key: String,
    device_id: String,
    phone_signing_key: String,
    phone_agreement_key: String,
    expires_at_ms: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    approval: Approval,
    signature: String,
}
impl Approval {
    fn bytes(&self) -> Vec<u8> {
        let mut bytes = b"alondra-service-pairing-approval-v1\0".to_vec();
        bytes.push(self.v);
        for value in [
            &self.session_id,
            &self.owner_sub,
            &self.mac_id,
            &self.mac_signing_key,
            &self.mac_agreement_key,
            &self.device_id,
            &self.phone_signing_key,
            &self.phone_agreement_key,
        ] {
            bytes.extend_from_slice(&(value.len() as u32).to_be_bytes());
            bytes.extend_from_slice(value.as_bytes());
        }
        bytes.extend_from_slice(&self.expires_at_ms.to_be_bytes());
        bytes
    }
}
fn verify(
    proof: &Proof,
    invite: &Invitation,
    profile: &Profile,
    signing_public: &str,
    agreement_public: &str,
    now: u64,
) -> Result<()> {
    let a = &proof.approval;
    if a.v != 1
        || a.session_id != invite.session_id
        || a.owner_sub != profile.owner_sub
        || a.mac_id != invite.mac_id
        || a.mac_signing_key != invite.mac_signing_key
        || a.mac_agreement_key != invite.mac_agreement_key
        || a.device_id != profile.device_id
        || a.phone_signing_key != signing_public
        || a.phone_agreement_key != agreement_public
        || a.expires_at_ms != invite.expires_at_ms
        || now >= a.expires_at_ms
    {
        bail!("destination approval does not match this invitation and terminal");
    }
    VerifyingKey::from_sec1_bytes(&B64.decode(&invite.mac_signing_key)?)?.verify(
        &a.bytes(),
        &Signature::from_slice(&B64.decode(&proof.signature)?)?,
    )?;
    Ok(())
}
pub fn pair(mac: &str, owner: &str) -> Result<Profile> {
    println!(
        "On destination {mac}, run `alondra device invite` and copy its terminal invitation here. Approval must be entered on that destination Mac."
    );
    print!("Invitation JSON: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    if input.len() > 8192 {
        bail!("invitation too large");
    }
    let invite: Invitation =
        serde_json::from_str(input.trim()).context("invalid destination invitation")?;
    let now = service::now_ms()?;
    if invite.version != 1
        || invite.url.trim_end_matches('/') != service::RELAY
        || invite.mac_id != mac
        || !service::valid_id(&invite.session_id)
        || B64.decode(&invite.secret)?.len() != 16
        || invite.expires_at_ms <= now
        || invite.expires_at_ms - now > 600_000
    {
        bail!("invalid or expired destination invitation");
    }
    let mac_signing = VerifyingKey::from_sec1_bytes(&B64.decode(&invite.mac_signing_key)?)?;
    let mac_agreement = PublicKey::from_sec1_bytes(&B64.decode(&invite.mac_agreement_key)?)?;
    let signing = SigningKey::from_slice(&service::random::<32>()?)?;
    let agreement = SecretKey::from_slice(&service::random::<32>()?)?;
    let signing_public = B64.encode(signing.verifying_key().to_sec1_bytes());
    let agreement_public = B64.encode(agreement.public_key().to_sec1_bytes());
    let profile = Profile {
        mac_id: mac.into(),
        owner_sub: owner.into(),
        device_id: format!("client-{}", B64.encode(service::random::<16>()?)),
        mac_signing_key: invite.mac_signing_key.clone(),
        mac_agreement_key: invite.mac_agreement_key.clone(),
        signing_key: B64.encode(signing.to_bytes()),
        agreement_key: B64.encode(agreement.to_bytes()),
    };
    let comparison_nonce = B64.encode(service::random::<{ service::PAIRING_NONCE_LEN }>()?);
    let fingerprint = service::pairing_comparison_code(mac, &invite.session_id, &signing_public, &agreement_public, &comparison_nonce);
    println!(
        "Terminal comparison code: {fingerprint}\nEnter this code on destination {mac}, then approve this terminal there."
    );
    let plaintext = serde_json::to_vec(
        &json!({"device_id":profile.device_id,"signing_key":signing_public,"agreement_key":agreement_public,"display_name":"Alondra terminal","owner_sub":owner,"session_id":invite.session_id,"comparison_nonce":comparison_nonce}),
    )?;
    let envelope = Envelope::seal_random(
        EnvelopeContext {
            route: mac,
            sender: &profile.device_id,
            recipient: mac,
            request_id: &invite.session_id,
            kind: "pairing",
            expires_at_ms: invite.expires_at_ms,
        },
        &plaintext,
        &signing,
        &mac_agreement,
    )?;
    // The invitation's signing pin is retained until the Mac-signed proof is verified.
    let _ = mac_signing;
    let candidate = serde_json::to_vec(
        &json!({"secret":invite.secret,"device_id":profile.device_id,"signing_key":signing_public,"envelope":envelope}),
    )?;
    let (status, _) = profile.relay_call(
        "POST",
        &format!("/v1/pairings/{}/candidates", invite.session_id),
        &candidate,
        Instant::now() + Duration::from_secs(15),
    )?;
    if status != 202 {
        bail!("destination pairing request rejected ({status})");
    }
    let deadline = Instant::now() + Duration::from_millis(invite.expires_at_ms - now);
    loop {
        if Instant::now() >= deadline || service::now_ms()? >= invite.expires_at_ms {
            bail!("destination approval expired; request a new invitation");
        }
        let (status, body) = profile.relay_call(
            "GET",
            &format!("/v1/pairings/{}/approval", invite.session_id),
            &[],
            deadline.min(Instant::now() + Duration::from_secs(15)),
        )?;
        match status {
            200 => {
                let proof: Proof = serde_json::from_value(body)?;
                verify(
                    &proof,
                    &invite,
                    &profile,
                    &signing_public,
                    &agreement_public,
                    service::now_ms()?,
                )?;
                return Ok(profile);
            }
            204 => std::thread::sleep(Duration::from_secs(2)),
            _ => bail!("destination approval unavailable ({status})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Signer;

    #[test]
    fn only_the_pinned_destination_can_approve_this_exact_terminal_invitation() {
        let mac = SigningKey::from_slice(&[3; 32]).unwrap();
        let client = SigningKey::from_slice(&[4; 32]).unwrap();
        let agreement = SecretKey::from_slice(&[5; 32]).unwrap();
        let signing_public = B64.encode(client.verifying_key().to_sec1_bytes());
        let agreement_public = B64.encode(agreement.public_key().to_sec1_bytes());
        let invitation = Invitation {
            version: 1,
            url: service::RELAY.into(),
            mac_id: "mac-first".into(),
            mac_signing_key: B64.encode(mac.verifying_key().to_sec1_bytes()),
            mac_agreement_key: agreement_public.clone(),
            session_id: "pair-1".into(),
            secret: B64.encode([1; 16]),
            expires_at_ms: 2000,
        };
        let profile = Profile {
            mac_id: invitation.mac_id.clone(),
            owner_sub: "account-one".into(),
            device_id: "client-one".into(),
            mac_signing_key: invitation.mac_signing_key.clone(),
            mac_agreement_key: invitation.mac_agreement_key.clone(),
            signing_key: B64.encode(client.to_bytes()),
            agreement_key: B64.encode(agreement.to_bytes()),
        };
        let approval = Approval {
            v: 1,
            session_id: invitation.session_id.clone(),
            owner_sub: profile.owner_sub.clone(),
            mac_id: profile.mac_id.clone(),
            mac_signing_key: profile.mac_signing_key.clone(),
            mac_agreement_key: profile.mac_agreement_key.clone(),
            device_id: profile.device_id.clone(),
            phone_signing_key: signing_public.clone(),
            phone_agreement_key: agreement_public.clone(),
            expires_at_ms: 2000,
        };
        let signature: Signature = mac.sign(&approval.bytes());
        let mut proof = Proof {
            approval,
            signature: B64.encode(signature.to_bytes()),
        };
        assert!(
            verify(
                &proof,
                &invitation,
                &profile,
                &signing_public,
                &agreement_public,
                1000
            )
            .is_ok()
        );
        assert!(
            verify(
                &proof,
                &invitation,
                &profile,
                &signing_public,
                &agreement_public,
                2000
            )
            .is_err()
        );
        proof.approval.owner_sub = "account-two".into();
        let signature: Signature = mac.sign(&proof.approval.bytes());
        proof.signature = B64.encode(signature.to_bytes());
        assert!(
            verify(
                &proof,
                &invitation,
                &profile,
                &signing_public,
                &agreement_public,
                1000
            )
            .is_err(),
            "even genuine Mac signatures cannot approve a different account"
        );
        proof.approval.owner_sub = profile.owner_sub.clone();
        proof.approval.device_id = "client-other".into();
        let signature: Signature = mac.sign(&proof.approval.bytes());
        proof.signature = B64.encode(signature.to_bytes());
        assert!(
            verify(
                &proof,
                &invitation,
                &profile,
                &signing_public,
                &agreement_public,
                1000
            )
            .is_err()
        );
        proof.approval.device_id = profile.device_id.clone();
        let impostor = SigningKey::from_slice(&[6; 32]).unwrap();
        let signature: Signature = impostor.sign(&proof.approval.bytes());
        proof.signature = B64.encode(signature.to_bytes());
        assert!(
            verify(
                &proof,
                &invitation,
                &profile,
                &signing_public,
                &agreement_public,
                1000
            )
            .is_err(),
            "directory/account alone cannot substitute a destination key"
        );
    }
}
