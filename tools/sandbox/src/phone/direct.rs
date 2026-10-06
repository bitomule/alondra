//! The synthetic phone over a Mac's direct listener, as the iPhone will use it on the same
//! Wi-Fi: pairing with the code both screens show, and requests answered in one exchange.
use std::{fs, os::unix::fs::OpenOptionsExt, path::PathBuf, time::Duration};

use alondra_e2e::pairing::{self as protocol, Approval, Transcript};
use alondra_e2e::{Context as EnvelopeContext, Envelope};
use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use p256::{SecretKey, ecdsa::SigningKey};
use serde_json::{Value, json};

use super::{Link, LinkFile, Phone, Reply, new_request_id};
use crate::{
    mac::{agreement_wire, signing_wire},
    sandbox::{Target, hex, http, now_ms, random, wait_for},
};

/// One pairing attempt, up to the code both screens show.
pub struct Attempt {
    pub pairing_id: String,
    pub code: String,
    start: Vec<u8>,
    link: Link,
    transcript: TranscriptParts,
}

struct TranscriptParts {
    hello: Value,
    signing_key: String,
    agreement_key: String,
    nonce: String,
    mac_nonce: String,
}

impl TranscriptParts {
    fn transcript<'a>(&'a self, pairing_id: &'a str, device_id: &'a str) -> Transcript<'a> {
        Transcript {
            pairing_id,
            mac_id: self.hello["mac_id"].as_str().unwrap_or_default(),
            mac_signing_key: self.hello["signing_key"].as_str().unwrap_or_default(),
            mac_agreement_key: self.hello["agreement_key"].as_str().unwrap_or_default(),
            device_id,
            client_signing_key: &self.signing_key,
            client_agreement_key: &self.agreement_key,
            client_nonce: &self.nonce,
            mac_nonce: &self.mac_nonce,
        }
    }
}

fn post_json(url: &str, body: &[u8]) -> Result<(u16, Vec<u8>)> {
    let mut response = http().post(url).header("content-type", "application/json").send(body)?;
    Ok((response.status().as_u16(), response.body_mut().read_to_vec()?))
}

fn get_json(url: &str) -> Result<(u16, Value)> {
    let mut response = http().get(url).call()?;
    let status = response.status().as_u16();
    Ok((status, serde_json::from_slice(&response.body_mut().read_to_vec()?).unwrap_or(Value::Null)))
}

fn new_key() -> Result<[u8; 32]> {
    loop {
        let candidate = random::<32>()?;
        if SigningKey::from_slice(&candidate).is_ok() {
            return Ok(candidate);
        }
    }
}

impl Phone<'_> {
    fn direct_link_path(&self, target: &Target) -> PathBuf {
        self.dir.join(format!("mac-{}-direct.json", target.number))
    }

    pub fn direct_link(&self, target: &Target) -> Result<Link> {
        let file: LinkFile = serde_json::from_slice(&fs::read(self.direct_link_path(target)).with_context(|| {
            format!("phone {} is not paired directly with Mac {}; run phone pair --direct first", self.name, target.number)
        })?)?;
        Ok(Link {
            device_id: file.device_id,
            signing: SigningKey::from_slice(&B64.decode(file.signing)?)?,
            agreement: SecretKey::from_slice(&B64.decode(file.agreement)?)?,
        })
    }

    /// Sends this phone's fresh keys and commitment, then reveals its nonce.
    pub fn start_direct(&self, target: &Target) -> Result<Attempt> {
        let (status, hello) = get_json(&format!("http://{}/direct/hello", target.direct))?;
        ensure!(status == 200, "Mac {} has no direct listener ({status})", target.number);
        ensure!(hello["mac_id"] == target.mac.id.as_str(), "the direct port answers as another Mac");
        let link = Link {
            device_id: format!("phone-{}-direct-{}", self.name, hex(&random::<6>()?)),
            signing: SigningKey::from_slice(&new_key()?)?,
            agreement: SecretKey::from_slice(&new_key()?)?,
        };
        let signing_key = signing_wire(link.signing.verifying_key());
        let agreement_key = agreement_wire(&link.agreement.public_key());
        let nonce = protocol::random_nonce().map_err(|e| anyhow!("{e:?}"))?;
        let commitment = protocol::commitment(&nonce, &link.device_id, &signing_key, &agreement_key)
            .map_err(|e| anyhow!("{e:?}"))?;
        let start = self.seal_pairing(target, &link, json!({"device_id": link.device_id, "name": format!("Sandbox phone {}", self.name),
            "signing_key": signing_key, "agreement_key": agreement_key, "commitment": commitment}))?;
        let body = serde_json::to_vec(&json!({"signing_key": signing_key, "envelope": start}))?;
        let (status, reply) = post_json(&format!("http://{}/direct/pairing", target.direct), &body)?;
        let start = body;
        ensure!(status == 200, "pairing start refused ({status}): {}", String::from_utf8_lossy(&reply));
        let started: Value = serde_json::from_slice(&reply)?;
        let pairing_id = started["pairing_id"].as_str().context("pairing id")?.to_owned();
        let mac_nonce = started["mac_nonce"].as_str().context("Mac nonce")?.to_owned();
        let reveal = self.seal_pairing(target, &link, json!({"pairing_id": pairing_id, "client_nonce": nonce}))?;
        let (status, reply) = post_json(
            &format!("http://{}/direct/pairing/{pairing_id}/reveal", target.direct),
            &serde_json::to_vec(&json!({"envelope": reveal}))?,
        )?;
        ensure!(status == 200, "pairing reveal refused ({status}): {}", String::from_utf8_lossy(&reply));
        let transcript = TranscriptParts { hello, signing_key, agreement_key, nonce, mac_nonce };
        let code = transcript.transcript(&pairing_id, &link.device_id).code().map_err(|e| anyhow!("{e:?}"))?;
        Ok(Attempt { pairing_id, code, start, link, transcript })
    }

    /// Sends an attempt's first step again, byte for byte, and returns the Mac's status.
    pub fn repeat_start(&self, target: &Target, attempt: &Attempt) -> Result<u16> {
        Ok(post_json(&format!("http://{}/direct/pairing", target.direct), &attempt.start)?.0)
    }

    pub fn pairing_state(&self, target: &Target, attempt: &Attempt) -> Result<String> {
        let (_, status) = get_json(&format!("http://{}/direct/pairing/{}", target.direct, attempt.pairing_id))?;
        Ok(status["state"].as_str().unwrap_or_default().to_owned())
    }

    /// The status a plain GET of `path` gets on the Mac's direct port.
    pub fn direct_status(&self, target: &Target, path: &str) -> Result<u16> {
        Ok(http().get(format!("http://{}{path}", target.direct)).call()?.status().as_u16())
    }

    fn seal_pairing(&self, target: &Target, link: &Link, plain: Value) -> Result<Envelope> {
        Envelope::seal_random(
            EnvelopeContext {
                route: &target.mac.id,
                sender: &link.device_id,
                recipient: &target.mac.id,
                request_id: &new_request_id()?,
                kind: "pairing",
                expires_at_ms: now_ms() + 120_000,
            },
            &serde_json::to_vec(&plain)?,
            &link.signing,
            &target.mac.agreement,
        )
        .map_err(|error| anyhow!("seal pairing: {error:?}"))
    }

    /// The code the Mac shows for this attempt, as the Mac app or the TUI lists it.
    pub fn code_on_mac(&self, target: &Target, attempt: &Attempt) -> Result<String> {
        let (status, pending) = get_json(&format!("{}/devices/pending", target.local))?;
        ensure!(status == 200, "pending requests unavailable ({status})");
        pending["pending"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|request| request["id"] == attempt.pairing_id.as_str())
            .and_then(|request| request["code"].as_str().map(str::to_owned))
            .context("the Mac does not list this pairing request")
    }

    /// A person on the Mac taps Allow or Deny.
    pub fn decide_on_mac(&self, target: &Target, attempt: &Attempt, allow: bool) -> Result<()> {
        let decision = if allow { "approve" } else { "deny" };
        let (status, reply) =
            post_json(&format!("{}/devices/pending/{}/{decision}", target.local, attempt.pairing_id), b"{}")?;
        ensure!(status == 200, "the Mac refused to {decision} ({status}): {}", String::from_utf8_lossy(&reply));
        Ok(())
    }

    /// Waits for the Mac's signed approval, verifies it against `hello`, and keeps the link.
    pub fn finish_direct(&self, target: &Target, attempt: Attempt) -> Result<Link> {
        let url = format!("http://{}/direct/pairing/{}", target.direct, attempt.pairing_id);
        let status = wait_for("the Mac's decision", Duration::from_secs(30), || {
            let (_, status) = get_json(&url)?;
            Ok(matches!(status["state"].as_str(), Some("approved" | "denied" | "expired")).then_some(status))
        })?;
        ensure!(status["state"] == "approved", "pairing ended as {}", status["state"]);
        let approval: Approval = serde_json::from_value(status["approval"].clone())?;
        approval
            .verify(
                status["signature"].as_str().context("signature")?,
                &target.mac.verifying(),
                &attempt.transcript.transcript(&attempt.pairing_id, &attempt.link.device_id),
                now_ms(),
            )
            .map_err(|error| anyhow!("the Mac's approval does not verify: {error:?}"))?;
        let file = LinkFile {
            device_id: attempt.link.device_id.clone(),
            signing: B64.encode(attempt.link.signing.to_bytes()),
            agreement: B64.encode(attempt.link.agreement.to_bytes()),
        };
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(self.direct_link_path(target))?;
        std::io::Write::write_all(&mut out, &serde_json::to_vec_pretty(&file)?)?;
        Ok(attempt.link)
    }

    /// The whole pairing: the phone and the Mac must show the same code before it is approved.
    pub fn pair_direct(&self, target: &Target) -> Result<Link> {
        let attempt = self.start_direct(target)?;
        let on_mac = self.code_on_mac(target, &attempt)?;
        if on_mac != attempt.code {
            bail!("codes differ: phone {} vs Mac {on_mac}", attempt.code);
        }
        self.decide_on_mac(target, &attempt, true)?;
        let link = self.finish_direct(target, attempt)?;
        println!(
            "phone {} paired directly with Mac {} ({}) as {}",
            self.name, target.number, target.mac.id, link.device_id
        );
        Ok(link)
    }

    /// Sends sealed bytes to a Mac's direct port and returns its status and body.
    pub fn send_direct(&self, target: &Target, sealed: &[u8]) -> Result<(u16, Vec<u8>)> {
        post_json(&format!("http://{}/direct/request", target.direct), sealed)
    }

    pub fn open_direct(&self, target: &Target, link: &Link, bytes: &[u8]) -> Result<Value> {
        let envelope: Envelope = serde_json::from_slice(bytes)?;
        let clear = envelope
            .open(&target.mac.verifying(), &link.agreement, &target.mac.id, &link.device_id, &target.mac.id, now_ms())
            .map_err(|error| anyhow!("open response: {error:?}"))?;
        Ok(serde_json::from_slice(&clear)?)
    }

    pub fn request_direct(
        &self,
        target: &Target,
        method: &str,
        path: &str,
        body: Option<Value>,
        request_id: Option<String>,
    ) -> Result<Reply> {
        let link = self.direct_link(target)?;
        let request_id = request_id.map_or_else(new_request_id, Ok)?;
        let mut plaintext = json!({"method": method, "path": path});
        if let Some(body) = body {
            plaintext["body"] = body;
        }
        let sealed = self.seal(target, &link, &request_id, &plaintext)?;
        let (status, reply) = self.send_direct(target, &sealed)?;
        ensure!(status == 200, "the Mac refused the direct request ({status}): {}", String::from_utf8_lossy(&reply));
        Ok(Reply { request_id, response: self.open_direct(target, &link, &reply)? })
    }
}
