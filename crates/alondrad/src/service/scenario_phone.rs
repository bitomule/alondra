//! The phone of a scenario on the Mac's side: the service a paired phone talks to, with its push
//! destination registered through the same encrypted request the app sends, so a push has
//! somewhere to go and a scenario sees the ones that leave.

use super::{
    Device, Envelope, EnvelopeContext, PUSH_BINDING_ROUTE, Service, URL_SAFE_NO_PAD, now_ms,
};
use anyhow::{Result, bail, ensure};
use base64::Engine as _;
use p256::{SecretKey, ecdsa::SigningKey};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;

const MAC_ID: &str = "scenario-mac";

/// A phone whose keys derive from one secret: the signing key it was paired with, and the
/// agreement key the Mac seals what it sends to it for.
pub struct Phone {
    id: String,
    signing: SigningKey,
    agreement: SecretKey,
}

impl Phone {
    pub fn new(id: &str, secret: [u8; 32]) -> Result<Self> {
        let mut seed = Sha256::new();
        seed.update(b"scenario phone agreement");
        seed.update(secret);
        Ok(Self {
            id: id.to_owned(),
            signing: SigningKey::from_slice(&secret)?,
            agreement: SecretKey::from_slice(&seed.finalize())?,
        })
    }
}

/// Writes the Mac's side of the service into `home`, with no phone approved yet: the keys it signs
/// and seals with, and a `service.toml` with no relay.
pub fn prepare(home: &Path) -> Result<()> {
    if home.join("service.toml").exists() {
        return Ok(());
    }
    for (name, secret) in [("signing.key", [1u8; 32]), ("agreement.key", [2u8; 32])] {
        let path = home.join(name);
        std::fs::write(&path, URL_SAFE_NO_PAD.encode(secret))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::write(home.join("service.toml"), listing(None))?;
    Ok(())
}

fn listing(phone: Option<&Phone>) -> String {
    let mut text = format!("mac_id = \"{MAC_ID}\"\nsigning_key_path = \"signing.key\"\nagreement_key_path = \"agreement.key\"\n");
    match phone {
        None => text.push_str("devices = []\n"),
        Some(phone) => text.push_str(&format!(
            "[[devices]]\ndevice_id = \"{}\"\nsigning_key = \"{}\"\nagreement_key = \"{}\"\n",
            phone.id,
            URL_SAFE_NO_PAD.encode(phone.signing.verifying_key().to_sec1_bytes()),
            URL_SAFE_NO_PAD.encode(phone.agreement.public_key().to_sec1_bytes()),
        )),
    }
    text
}

impl Service {
    /// The phone, already approved by the daemon's own authentication, is added to the service and
    /// enrolled (the relay's part, which a scenario has none of), and registers `binding` as the
    /// destination of its pushes the way the app does.
    pub(crate) async fn register_phone_push(&self, phone: &Phone, binding: &str) -> Result<()> {
        ensure!(self.mac_id == MAC_ID, "the service was not prepared for a scenario phone");
        std::fs::write(self.home.join("service.toml"), listing(Some(phone)))?;
        self.reload_devices()?;
        let device: std::sync::Arc<Device> = self.devices.get(&phone.id).ok_or_else(|| anyhow::anyhow!("the phone is not a service device"))?;
        self.journal.reserve_enrollment(&phone.id, &device.signing.to_sec1_bytes())?;
        self.journal.activate_enrollment(&phone.id)?;
        let request = json!({"method": "POST", "path": PUSH_BINDING_ROUTE, "body": {"push_binding": binding}});
        let envelope = Envelope::seal_random(
            EnvelopeContext {
                route: MAC_ID,
                sender: &phone.id,
                recipient: MAC_ID,
                request_id: "scenario-phone-push-binding",
                kind: "request",
                expires_at_ms: now_ms() + 600_000,
            },
            &serde_json::to_vec(&request)?,
            &phone.signing,
            &self.agreement.public_key(),
        )?;
        let (sealed, _lease) = self.handle_direct(envelope).await.map_err(|refusal| anyhow::anyhow!("the daemon refused the push binding: {refusal:?}"))?;
        let answer: Envelope = serde_json::from_slice(&sealed)?;
        let opened = answer.open(self.signing.verifying_key(), &phone.agreement, MAC_ID, &phone.id, MAC_ID, now_ms())?;
        let response: serde_json::Value = serde_json::from_slice(&opened)?;
        if response["status"] != 204 {
            bail!("the daemon did not accept the push binding: {response}");
        }
        Ok(())
    }
}
