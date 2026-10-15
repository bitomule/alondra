use crate::{Attach, Attached, Error};
use hmac::{Hmac, Mac};
use sha2::Sha256;

const OWNER_PROOF_DOMAIN: &str = "alondra-host-owner-v1";
const HOST_PROOF_DOMAIN: &str = "alondra-host-accept-v1";

/// Authenticate a bounded controller claim with the raw 32-byte control secret.
/// The proof field is ignored so a caller can populate it with the returned tag.
/// This does not compare bootstrap expectations or grant durable ownership.
pub fn owner_proof(secret: &[u8; 32], attach: &Attach) -> Result<String, Error> {
    Ok(format!(
        "{:x}",
        claim_mac(secret, attach, OWNER_PROOF_DOMAIN)?
            .finalize()
            .into_bytes()
    ))
}

/// Authenticate the controller claim using a constant-time tag comparison.
/// Callers still check fresh nonce, full bootstrap identity/manifest and durable
/// owner generation before admitting commands or acknowledging events.
pub fn verify_owner_proof(secret: &[u8; 32], attach: &Attach) -> Result<(), Error> {
    attach.validate()?;
    verify_tag(
        claim_mac(secret, attach, OWNER_PROOF_DOMAIN)?,
        &attach.proof,
        "owner proof",
    )
}

/// Sign the host response to an authenticated controller request.
/// The different domain and appended request tag prevent reflecting its proof.
/// This helper does not commit ownership or establish process provenance.
pub fn host_proof(secret: &[u8; 32], attach: &Attach) -> Result<String, Error> {
    verify_owner_proof(secret, attach)?;
    Ok(format!(
        "{:x}",
        host_mac(secret, attach)?.finalize().into_bytes()
    ))
}

/// Authenticate a response against the caller's own pending request, never a
/// request supplied by the peer. Freshness and verified launch provenance still
/// belong to the consumer; successful verification grants no durable authority.
pub fn verify_host_proof(
    secret: &[u8; 32],
    pending: &Attach,
    attached: &Attached,
) -> Result<(), Error> {
    verify_owner_proof(secret, pending)?;
    attached.validate()?;
    if attached.fence != pending.fence {
        return Err(Error::FenceMismatch);
    }
    if attached.execution != pending.execution {
        return Err(Error::ExecutionMismatch);
    }
    if attached.host_instance_id != pending.host_instance_id
        || attached.owner_generation != pending.owner_generation
    {
        return Err(Error::HostMismatch);
    }
    verify_tag(host_mac(secret, pending)?, &attached.proof, "host proof")
}

fn verify_tag(mac: Hmac<Sha256>, encoded: &str, surface: &'static str) -> Result<(), Error> {
    if encoded.len() != 64
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(Error::Invalid(surface));
    }
    let mut proof = [0; 32];
    for (byte, encoded) in proof.iter_mut().zip(encoded.as_bytes().chunks_exact(2)) {
        let pair = std::str::from_utf8(encoded).map_err(|_| Error::Invalid(surface))?;
        *byte = u8::from_str_radix(pair, 16).map_err(|_| Error::Invalid(surface))?;
    }
    mac.verify_slice(&proof)
        .map_err(|_| Error::Invalid(surface))
}

fn host_mac(secret: &[u8; 32], attach: &Attach) -> Result<Hmac<Sha256>, Error> {
    let mut mac = claim_mac(secret, attach, HOST_PROOF_DOMAIN)?;
    mac.update(&(attach.proof.len() as u32).to_be_bytes());
    mac.update(attach.proof.as_bytes());
    Ok(mac)
}

fn claim_mac(secret: &[u8; 32], attach: &Attach, domain: &str) -> Result<Hmac<Sha256>, Error> {
    attach.validate_claim()?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret).map_err(|_| Error::Invalid("owner secret"))?;
    let owner_generation = attach.owner_generation.to_string();
    let generation = attach.execution.generation.to_string();
    for segment in [
        domain,
        &attach.nonce,
        &attach.host_instance_id,
        &owner_generation,
        &attach.fence.profile_id,
        &attach.fence.account_id,
        &attach.fence.data_epoch.0,
        &attach.fence.host_id,
        &attach.execution.task_id.0,
        &attach.execution.agent_id.0,
        &attach.execution.conversation_id,
        &attach.execution.process_instance_id,
        &generation,
        &attach.host_release,
        &attach.host_sha256,
        &attach.executable_sha256,
        &attach.sdk_version,
    ] {
        mac.update(&(segment.len() as u32).to_be_bytes());
        mac.update(segment.as_bytes());
    }
    Ok(mac)
}
