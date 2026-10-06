//! A sandbox Mac's service identity, in the files `alondra setup` writes on a real Mac.
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use p256::{PublicKey, SecretKey, ecdsa::{SigningKey, VerifyingKey}, elliptic_curve::sec1::ToSec1Point};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    issuer::OWNER,
    relay::{Relay, Signer, signed_headers},
    sandbox::{hex, http, random},
};

#[derive(Serialize, Deserialize)]
struct PublicMac {
    mac_id: String,
    signing_key: String,
    agreement_key: String,
}

pub struct Mac {
    pub home: PathBuf,
    pub id: String,
    pub signing: SigningKey,
    pub agreement: PublicKey,
    public: PublicMac,
}

fn new_key() -> Result<[u8; 32]> {
    loop {
        let candidate = random::<32>()?;
        if SigningKey::from_slice(&candidate).is_ok() {
            return Ok(candidate);
        }
    }
}

fn write_private(path: &Path, bytes: &[u8; 32]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    file.write_all(B64.encode(bytes).as_bytes())?;
    Ok(())
}

fn read_private(path: &Path) -> Result<Vec<u8>> {
    Ok(B64.decode(fs::read_to_string(path)?.trim())?)
}

pub fn signing_wire(key: &VerifyingKey) -> String {
    B64.encode(key.to_sec1_point(false).as_bytes())
}

pub fn agreement_wire(key: &PublicKey) -> String {
    B64.encode(key.to_sec1_point(false).as_bytes())
}

pub fn provision(home: &Path, relay: &str) -> Result<String> {
    let keys = home.join("service-keys");
    if keys.exists() {
        bail!("{} already has a Mac identity", home.display());
    }
    fs::create_dir_all(&keys)?;
    fs::set_permissions(&keys, fs::Permissions::from_mode(0o700))?;
    let signing = new_key()?;
    let agreement = new_key()?;
    write_private(&keys.join("signing.key"), &signing)?;
    write_private(&keys.join("agreement.key"), &agreement)?;
    let public = PublicMac {
        mac_id: format!("mac-{}", hex(&random::<16>()?)),
        signing_key: signing_wire(SigningKey::from_slice(&signing)?.verifying_key()),
        agreement_key: agreement_wire(&SecretKey::from_slice(&agreement)?.public_key()),
    };
    fs::write(keys.join("public.json"), serde_json::to_vec_pretty(&public)?)?;
    let service = toml::toml! {
        relay_url = relay
        mac_id = (public.mac_id.clone())
        owner_sub = OWNER
        signing_key_path = "service-keys/signing.key"
        agreement_key_path = "service-keys/agreement.key"
        devices = []
    };
    fs::write(home.join("service.toml"), toml::to_string(&service)?)?;
    Ok(public.mac_id)
}

impl Mac {
    pub fn open(home: &Path) -> Result<Self> {
        let keys = home.join("service-keys");
        let public: PublicMac = serde_json::from_slice(
            &fs::read(keys.join("public.json"))
                .with_context(|| format!("{} is not a provisioned sandbox Mac", home.display()))?,
        )?;
        Ok(Self {
            home: home.into(),
            id: public.mac_id.clone(),
            signing: SigningKey::from_slice(&read_private(&keys.join("signing.key"))?)?,
            agreement: SecretKey::from_slice(&read_private(&keys.join("agreement.key"))?)?.public_key(),
            public,
        })
    }

    pub fn verifying(&self) -> VerifyingKey {
        *self.signing.verifying_key()
    }

    pub fn enroll(&self, relay: &Relay, token: &str) -> Result<()> {
        let body = serde_json::to_vec(&self.public)?;
        let signer = Signer { id: &self.id, key: &self.signing };
        match relay.call(&signer, Some(token), "POST", "/v1/macs", &body)? {
            (200 | 201, _) => Ok(()),
            (status, reply) => bail!("relay refused Mac {} ({status}): {}", self.id, String::from_utf8_lossy(&reply)),
        }
    }

    /// Adds a phone to `service.toml` and approves it through the daemon's local route,
    /// signed with this Mac's key: the two steps `alondra pair` takes after the code matches.
    pub fn approve_device(&self, local: &str, id: &str, name: &str, signing: &str, agreement: &str) -> Result<()> {
        let path = self.home.join("service.toml");
        let mut service: toml::Table = toml::from_str(&fs::read_to_string(&path)?)?;
        let devices = service
            .get_mut("devices")
            .and_then(toml::Value::as_array_mut)
            .ok_or_else(|| anyhow!("service.toml has no devices list"))?;
        devices.retain(|device| device.get("device_id").and_then(toml::Value::as_str) != Some(id));
        devices.push(toml::Value::Table(toml::toml! {
            device_id = id
            signing_key = signing
            agreement_key = agreement
        }));
        let next = path.with_extension("toml.next");
        fs::write(&next, toml::to_string(&service)?)?;
        fs::rename(&next, &path)?;

        let mut bootstrap = http().get(format!("{local}/service/bootstrap")).call()?;
        if bootstrap.status().as_u16() != 200 {
            bail!("daemon service bootstrap failed ({})", bootstrap.status());
        }
        let epoch: [u8; 16] = B64
            .decode(bootstrap.body_mut().read_json::<Value>()?["epoch"].as_str().unwrap_or_default())?
            .try_into()
            .map_err(|_| anyhow!("invalid service epoch"))?;
        let body = serde_json::to_vec(&json!({"device_id": id, "name": name, "signing_key": signing}))?;
        let signer = Signer { id: &self.id, key: &self.signing };
        let mut request = http().post(format!("{local}/service/approve-device"));
        for (header, value) in signed_headers(&signer, &epoch, "POST", "/service/approve-device", &body)? {
            request = request.header(header, value);
        }
        let mut response = request.header("content-type", "application/json").send(&body[..])?;
        match response.status().as_u16() {
            200 | 204 => Ok(()),
            status => bail!("daemon refused the approval ({status}): {}", response.body_mut().read_to_string()?),
        }
    }

    pub fn revoke_device(&self, local: &str, id: &str) -> Result<()> {
        let mut response = http().delete(format!("{local}/devices/{id}")).call()?;
        match response.status().as_u16() {
            200 | 204 => Ok(()),
            status => bail!("daemon refused the revocation ({status}): {}", response.body_mut().read_to_string()?),
        }
    }
}
