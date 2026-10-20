//! A synthetic phone: its own keys per Mac, the same envelopes, relay calls and iroh connection as
//! the iPhone.
mod direct;
mod iroh_session;

use std::{fs, os::unix::fs::OpenOptionsExt, path::PathBuf, time::Duration};

use alondra_e2e::{Context as EnvelopeContext, Envelope};
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

/// What the Mac answered one envelope over iroh.
pub enum Answer {
    /// The sealed response envelope.
    Response(Value),
    /// A refused hello or envelope, with what the Mac said.
    Refused(String),
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
            match self.relay_status(&link)? {
                204 => Ok(Some(())),
                401 => Ok(None),
                status => bail!("relay refused the paired phone ({status})"),
            }
        })?;
        println!("phone {} paired with Mac {} ({}) as {}", self.name, target.number, target.mac.id, link.device_id);
        Ok(())
    }

    pub fn revoke(&self, target: &Target) -> Result<()> {
        let link = self.link(target)?;
        target.mac.revoke_device(&target.local, &link.device_id)?;
        wait_for("the relay to refuse the revoked phone", Duration::from_secs(30), || {
            Ok((self.relay_status(&link)? == 401).then_some(()))
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
        match self.exchange(target, &link, &sealed)? {
            Answer::Response(envelope) => Ok(Reply { request_id, response: self.open_response(target, &link, &envelope)? }),
            Answer::Refused(reply) => bail!("Mac {} refused the request: {reply}", target.number),
        }
    }

    pub fn seal(&self, target: &Target, link: &Link, request_id: &str, plaintext: &Value) -> Result<Vec<u8>> {
        let envelope = Envelope::seal_random(
            EnvelopeContext {
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

    /// The relay's answer to a signed call from this phone that changes nothing it relies on:
    /// 204 while the relay knows the device, 401 once it does not.
    fn relay_status(&self, link: &Link) -> Result<u16> {
        let token = self.sandbox.token()?;
        let signer = Signer { id: &link.device_id, key: &link.signing };
        Ok(self.sandbox.relay.call(&signer, Some(&token), "DELETE", "/v1/iroh-node", &[])?.0)
    }

    /// Sends one sealed envelope over a fresh iroh connection to the Mac.
    pub fn exchange(&self, target: &Target, link: &Link, sealed: &[u8]) -> Result<Answer> {
        let session = match self.connect_iroh(target, link) {
            Ok(session) => session,
            Err(error) if error.to_string().contains("\"unauthorized\"") => return Ok(Answer::Refused("unauthorized".into())),
            Err(error) => return Err(error),
        };
        let reply = session.send(sealed)?;
        Ok(match reply.get("response") {
            Some(envelope) => Answer::Response(envelope.clone()),
            None => Answer::Refused(reply.to_string()),
        })
    }

    pub fn open_response(&self, target: &Target, link: &Link, envelope: &Value) -> Result<Value> {
        let envelope: Envelope = serde_json::from_value(envelope.clone())?;
        let clear = envelope
            .open(&target.mac.verifying(), &link.agreement, &target.mac.id, &link.device_id, &target.mac.id, now_ms())
            .map_err(|error| anyhow!("open response: {error:?}"))?;
        Ok(serde_json::from_slice(&clear)?)
    }
}

pub fn probe() -> Value {
    json!({"method": "GET", "path": "/capabilities"})
}

pub fn new_request_id() -> Result<String> {
    Ok(format!("sandbox-{}", hex(&random::<12>()?)))
}
