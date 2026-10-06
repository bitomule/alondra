//! A synthetic phone: its own keys per Mac, the same envelopes and relay calls as the iPhone.
use std::{fs, os::unix::fs::OpenOptionsExt, path::PathBuf, time::Duration};

use alondra_e2e::{Context as Envelope, Envelope};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use p256::{SecretKey, ecdsa::SigningKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    mac::{agreement_wire, signing_wire},
    relay::Signer,
    sandbox::{Sandbox, Target, hex, now_ms, random, wait_for},
};

pub struct Phone<'a> {
    sandbox: &'a Sandbox,
    name: String,
    dir: PathBuf,
}

/// The phone's identity toward one Mac; the relay binds a device ID to a single Mac.
#[derive(Serialize, Deserialize)]
struct LinkFile {
    device_id: String,
    signing: String,
    agreement: String,
}

pub struct Link {
    pub device_id: String,
    signing: SigningKey,
    agreement: SecretKey,
}

#[derive(Serialize)]
pub struct Reply {
    pub request_id: String,
    pub response: Value,
}

pub enum Submitted {
    Accepted,
    Refused(u16, String),
}

impl<'a> Phone<'a> {
    pub fn open(sandbox: &'a Sandbox, name: &str) -> Result<Self> {
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            bail!("a phone name uses letters, digits and '-'");
        }
        let dir = sandbox.dir.join("phones").join(name);
        fs::create_dir_all(&dir)?;
        Ok(Self { sandbox, name: name.into(), dir })
    }

    fn link_path(&self, target: &Target) -> PathBuf {
        self.dir.join(format!("mac-{}.json", target.number))
    }

    pub fn link(&self, target: &Target) -> Result<Link> {
        let file: LinkFile = serde_json::from_slice(&fs::read(self.link_path(target)).with_context(|| {
            format!("phone {} is not paired with Mac {}; run phone pair first", self.name, target.number)
        })?)?;
        Ok(Link {
            device_id: file.device_id,
            signing: SigningKey::from_slice(&B64.decode(file.signing)?)?,
            agreement: SecretKey::from_slice(&B64.decode(file.agreement)?)?,
        })
    }

    fn create_link(&self, target: &Target) -> Result<Link> {
        let key = || -> Result<[u8; 32]> {
            loop {
                let candidate = random::<32>()?;
                if SigningKey::from_slice(&candidate).is_ok() {
                    return Ok(candidate);
                }
            }
        };
        let (signing, agreement) = (key()?, key()?);
        let file = LinkFile {
            device_id: format!("phone-{}-{}", self.name, hex(&random::<6>()?)),
            signing: B64.encode(signing),
            agreement: B64.encode(agreement),
        };
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.link_path(target))?;
        std::io::Write::write_all(&mut out, &serde_json::to_vec_pretty(&file)?)?;
        self.link(target)
    }

    pub fn pair(&self, target: &Target) -> Result<()> {
        let link = match self.link(target) {
            Ok(link) => link,
            Err(_) => self.create_link(target)?,
        };
        target.mac.approve_device(
            &target.local,
            &link.device_id,
            &self.name,
            &signing_wire(link.signing.verifying_key()),
            &agreement_wire(&link.agreement.public_key()),
        )?;
        wait_for("the Mac to enroll the phone with the relay", Duration::from_secs(30), || {
            match self.submit(&link, &self.seal(target, &link, &new_request_id()?, &probe())?)? {
                Submitted::Accepted => Ok(Some(())),
                Submitted::Refused(401, _) => Ok(None),
                Submitted::Refused(status, reply) => bail!("relay refused the paired phone ({status}): {reply}"),
            }
        })?;
        println!("phone {} paired with Mac {} ({}) as {}", self.name, target.number, target.mac.id, link.device_id);
        Ok(())
    }

    pub fn revoke(&self, target: &Target) -> Result<()> {
        let link = self.link(target)?;
        target.mac.revoke_device(&target.local, &link.device_id)?;
        wait_for("the relay to refuse the revoked phone", Duration::from_secs(30), || {
            match self.submit(&link, &self.seal(target, &link, &new_request_id()?, &probe())?)? {
                Submitted::Refused(401, _) => Ok(Some(())),
                _ => Ok(None),
            }
        })?;
        println!("phone {} revoked on Mac {}", self.name, target.number);
        Ok(())
    }

    pub fn request(
        &self,
        target: &Target,
        method: &str,
        path: &str,
        body: Option<Value>,
        request_id: Option<String>,
    ) -> Result<Reply> {
        let link = self.link(target)?;
        let request_id = request_id.map_or_else(new_request_id, Ok)?;
        let mut plaintext = json!({"method": method, "path": path});
        if let Some(body) = body {
            plaintext["body"] = body;
        }
        let sealed = self.seal(target, &link, &request_id, &plaintext)?;
        match self.submit(&link, &sealed)? {
            Submitted::Accepted => self.response(target, &link, &request_id),
            Submitted::Refused(status, reply) => bail!("relay refused the request ({status}): {reply}"),
        }
    }

    pub fn seal(&self, target: &Target, link: &Link, request_id: &str, plaintext: &Value) -> Result<Vec<u8>> {
        let envelope = Envelope::seal_random(
            Envelope {
                route: &target.mac.id,
                sender: &link.device_id,
                recipient: &target.mac.id,
                request_id,
                kind: "request",
                expires_at_ms: now_ms() + 120_000,
            },
            &serde_json::to_vec(plaintext)?,
            &link.signing,
            &target.mac.agreement,
        )
        .map_err(|error| anyhow!("seal request: {error:?}"))?;
        Ok(serde_json::to_vec(&envelope)?)
    }

    pub fn submit(&self, link: &Link, sealed: &[u8]) -> Result<Submitted> {
        let token = self.sandbox.token()?;
        let signer = Signer { id: &link.device_id, key: &link.signing };
        Ok(match self.sandbox.relay.call(&signer, Some(&token), "POST", "/v1/requests", sealed)? {
            (200 | 202, _) => Submitted::Accepted,
            (status, reply) => Submitted::Refused(status, String::from_utf8_lossy(&reply).into()),
        })
    }

    /// The stored response envelope, as the relay returns it, and its decrypted body.
    pub fn response_envelope(&self, target: &Target, link: &Link, request_id: &str) -> Result<(Value, Value)> {
        let token = self.sandbox.token()?;
        let signer = Signer { id: &link.device_id, key: &link.signing };
        let path = format!("/v1/requests/{request_id}");
        let result = wait_for("the Mac's response", Duration::from_secs(90), || {
            let (status, reply) = self.sandbox.relay.call(&signer, Some(&token), "GET", &path, &[])?;
            if status != 200 {
                bail!("relay refused the result ({status}): {}", String::from_utf8_lossy(&reply));
            }
            let value: Value = serde_json::from_slice(&reply)?;
            Ok((value["status"] == "completed").then_some(value))
        })?;
        let envelope: Envelope = serde_json::from_value(result["response"].clone())?;
        let clear = envelope
            .open(&target.mac.verifying(), &link.agreement, &target.mac.id, &link.device_id, &target.mac.id, now_ms())
            .map_err(|error| anyhow!("open response: {error:?}"))?;
        Ok((result["response"].clone(), serde_json::from_slice(&clear)?))
    }

    fn response(&self, target: &Target, link: &Link, request_id: &str) -> Result<Reply> {
        let (_, response) = self.response_envelope(target, link, request_id)?;
        Ok(Reply { request_id: request_id.into(), response })
    }
}

pub fn probe() -> Value {
    json!({"method": "GET", "path": "/capabilities"})
}

pub fn new_request_id() -> Result<String> {
    Ok(format!("sandbox-{}", hex(&random::<12>()?)))
}
