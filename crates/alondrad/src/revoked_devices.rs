//! Devices this Mac revoked, kept so a direct request from one can be answered, sealed, with the
//! reason it failed. A bare 401 proves nothing to the device, so without this a revoked iPhone
//! would read its Mac as asleep forever. Public keys only; the newest few are kept.
use alondra_e2e::Envelope;
use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::{PublicKey, ecdsa::VerifyingKey, elliptic_curve::sec1::ToSec1Point};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAX_KEPT: usize = 64;
const FILE: &str = "revoked-devices.json";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct Tombstone {
    pub(crate) device_id: String,
    pub(crate) signing_key: String,
    pub(crate) agreement_key: String,
    pub(crate) revoked_at_ms: u64,
}

impl Tombstone {
    pub(crate) fn new(device_id: &str, signing: &VerifyingKey, agreement: &PublicKey, revoked_at_ms: u64) -> Self {
        Self {
            device_id: device_id.into(),
            signing_key: URL_SAFE_NO_PAD.encode(signing.to_sec1_point(false).as_bytes()),
            agreement_key: URL_SAFE_NO_PAD.encode(agreement.to_sec1_point(false).as_bytes()),
            revoked_at_ms,
        }
    }

    fn keys(&self) -> Option<(VerifyingKey, PublicKey)> {
        let signing = VerifyingKey::from_sec1_bytes(&URL_SAFE_NO_PAD.decode(&self.signing_key).ok()?).ok()?;
        let agreement = PublicKey::from_sec1_bytes(&URL_SAFE_NO_PAD.decode(&self.agreement_key).ok()?).ok()?;
        Some((signing, agreement))
    }
}

pub(crate) struct RevokedDevices {
    path: PathBuf,
    kept: Mutex<Vec<Tombstone>>,
}

impl RevokedDevices {
    /// A missing or unreadable file means nobody is remembered: such a device gets the plain 401.
    pub(crate) fn open(home: &Path) -> Self {
        let path = home.join(FILE);
        let kept = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        Self { path, kept: Mutex::new(kept) }
    }

    pub(crate) fn add(&self, revoked: Vec<Tombstone>) -> Result<()> {
        if revoked.is_empty() {
            return Ok(());
        }
        let mut kept = self.kept.lock().expect("revoked devices lock poisoned");
        kept.retain(|old| !revoked.iter().any(|new| new.device_id == old.device_id));
        kept.extend(revoked);
        let excess = kept.len().saturating_sub(MAX_KEPT);
        kept.drain(..excess);
        let temporary = self.path.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec(&*kept)?).context("write revoked devices")?;
        std::fs::rename(&temporary, &self.path).context("replace revoked devices")?;
        Ok(())
    }

    /// The remembered keys of `envelope`'s sender, only when the envelope is signed by them: the
    /// proof that the sender really is that device and may be told it was revoked.
    pub(crate) fn proven_sender(
        &self,
        envelope: &Envelope,
        mac_id: &str,
        now_ms: u64,
    ) -> Option<PublicKey> {
        let tombstone = self
            .kept
            .lock()
            .expect("revoked devices lock poisoned")
            .iter()
            .find(|tombstone| tombstone.device_id == envelope.sender)
            .cloned()?;
        let (signing, agreement) = tombstone.keys()?;
        envelope.verify_signature(&signing, &envelope.sender, mac_id, mac_id, now_ms).ok()?;
        Some(agreement)
    }

    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self { path: PathBuf::from("/nonexistent/revoked-devices.json"), kept: Mutex::new(Vec::new()) }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.kept.lock().expect("revoked devices lock poisoned").len()
    }
}
