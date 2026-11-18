//! What a terminal and a Mac compare when they pair through the relay.
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use sha2::{Digest, Sha256};

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64)
}

pub const PAIRING_NONCE_LEN: usize = 16;

/// The short code both screens show for the user to compare.
pub fn pairing_comparison_code(mac_id: &str, session_id: &str, signing_key: &str, agreement_key: &str, nonce: &str) -> String {
    B64.encode(&pairing_comparison_digest(mac_id, session_id, signing_key, agreement_key, nonce)[..6])
}

pub fn pairing_comparison_digest(mac_id: &str, session_id: &str, signing_key: &str, agreement_key: &str, nonce: &str) -> [u8; 32] {
    let mut bytes = b"alondra-pairing-comparison\0".to_vec();
    for field in [mac_id, session_id, signing_key, agreement_key, nonce] {
        bytes.extend_from_slice(&(field.len() as u32).to_be_bytes());
        bytes.extend_from_slice(field.as_bytes());
    }
    Sha256::digest(bytes).into()
}
