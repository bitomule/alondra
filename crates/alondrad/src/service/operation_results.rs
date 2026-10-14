use super::*;
use p256::elliptic_curve::sec1::ToSec1Point;

pub(crate) struct Lookup {
    pub id: String,
    pub digest: [u8; 32],
    pub digest_text: String,
    pub data_epoch: String,
    pub offset: Option<usize>,
}

pub(crate) fn lookup(path: &str) -> Option<Lookup> {
    let (path, query) = path.split_once('?')?;
    let id = path.strip_prefix("/operation-results/")?;
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let (mut digest_text, mut data_epoch, mut offset_text) = (None, None, None);
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        let slot = match name.as_ref() {
            "digest" => &mut digest_text,
            "data_epoch" => &mut data_epoch,
            "offset" => &mut offset_text,
            _ => return None,
        };
        if slot.replace(value.into_owned()).is_some() {
            return None;
        }
    }
    let digest_text = digest_text?;
    if digest_text.len() != 64
        || !digest_text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut digest = [0; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digest_text[index * 2..index * 2 + 2], 16).ok()?;
    }
    let data_epoch = data_epoch?;
    if data_epoch.is_empty()
        || data_epoch.len() > 256
        || !data_epoch.bytes().all(|b| (33..=126).contains(&b))
    {
        return None;
    }
    let offset = match offset_text {
        Some(value) => {
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            let offset: usize = value.parse().ok()?;
            if offset > MAX_RELAY_RESPONSE || !offset.is_multiple_of(RESULT_CHUNK_BYTES) {
                return None;
            }
            Some(offset)
        }
        None => None,
    };
    Some(Lookup {
        id: id.into(),
        digest,
        digest_text,
        data_epoch,
        offset,
    })
}

fn frame(bytes: &[u8], output: &mut Vec<u8>) -> Result<()> {
    output.extend_from_slice(&u32::try_from(bytes.len())?.to_be_bytes());
    output.extend_from_slice(bytes);
    Ok(())
}

const RESULT_CHUNK_BYTES: usize = 256 * 1024;

pub(super) fn digest(envelope: &Envelope) -> Result<[u8; 32]> {
    let mut bytes = envelope
        .authenticated_header()
        .map_err(|error| anyhow!("invalid recovery envelope: {error}"))?;
    frame(&URL_SAFE_NO_PAD.decode(&envelope.ciphertext)?, &mut bytes)?;
    frame(&URL_SAFE_NO_PAD.decode(&envelope.signature)?, &mut bytes)?;
    Ok(Sha256::digest(bytes).into())
}

#[derive(Clone)]
pub(super) struct RecoveryScope {
    pub digest: [u8; 32],
    pub binding: [u8; 32],
    pub operation: Option<crate::operation_journal::Scope>,
}

impl Service {
    fn operation_binding(
        &self,
        sender: &str,
        device: &Device,
        transport: Transport,
        epoch: &str,
    ) -> Result<[u8; 32]> {
        let approval = self.approval_binding(sender, device, transport)?;
        let mac_signing = URL_SAFE_NO_PAD.encode(self.signing.verifying_key().to_sec1_bytes());
        let mac_agreement =
            URL_SAFE_NO_PAD.encode(self.agreement.public_key().to_sec1_point(false).as_bytes());
        Ok(Sha256::digest(serde_json::to_vec(&(
            &self.mac_id,
            mac_signing,
            mac_agreement,
            sender,
            epoch,
            approval,
        ))?)
        .into())
    }

    pub(super) fn operation_journal_scope(
        &self,
        sender: &str,
        device: &Device,
        transport: Transport,
        epoch: &str,
    ) -> Result<crate::operation_journal::Scope> {
        Ok(crate::operation_journal::Scope {
            issuer: format!("encrypted:{sender}"),
            binding: self
                .operation_binding(sender, device, transport, "")?
                .to_vec(),
            data_epoch: epoch.into(),
        })
    }
    pub(super) async fn operation_control(
        &self,
        sender: &str,
        request: &LocalRequest,
        transport: Transport,
    ) -> Result<Value> {
        let device = self
            .devices
            .get(sender)
            .context("operation journal device is not approved")?;
        let approved = self.approved_device(sender, &device, transport)?;
        let epoch = self
            .domain
            .as_ref()
            .context("operation journal requires domain authority")?
            .state()?
            .data_epoch
            .0
            .clone();
        let expected = if request.method == "GET" {
            crate::operation_journal::query_epoch(&request.path)
        } else {
            request
                .body
                .as_ref()
                .and_then(|body| body["data_epoch"].as_str())
                .map(str::to_owned)
        };
        if expected.as_deref() != Some(epoch.as_str()) {
            return Ok(json!({"status":409,"body":{"error":"outcome_unknown"}}));
        }
        let scope = self.operation_journal_scope(sender, &device, transport, &epoch)?;
        let original_binding = scope.binding.clone();
        let expected_approval = Auth::open(&self.home)?
            .list()
            .into_iter()
            .find(|entry| entry.id == sender)
            .context("operation journal approval missing")?;
        let approved_sender = sender.to_owned();
        let ack = if request.method == "POST" {
            Some(serde_json::from_value::<crate::operation_journal::Ack>(
                request
                    .body
                    .clone()
                    .context("operation acknowledgement body missing")?,
            )?)
        } else {
            None
        };
        let home = self.home.clone();
        let inflight = self.inflight.clone();
        let physical = self.journal.accept_jobs.clone().acquire_owned().await?;
        let result = self
            .journal
            .off_runtime(move |journal| {
                let _physical = physical;
                let _approval = crate::approval_lease::ApprovalLease::shared(&home)?;
                let current = Auth::open(&home)?
                    .list()
                    .into_iter()
                    .find(|entry| entry.id == approved_sender)
                    .context("operation journal approval was revoked")?;
                anyhow::ensure!(
                    current.public_key == expected_approval.public_key
                        && current.created_at == expected_approval.created_at,
                    "operation journal approval changed while waiting"
                );
                let mut c = journal.db()?;
                let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let result = match ack {
                    Some(ack) => {
                        let live = |id: &str| {
                            inflight
                                .lock()
                                .expect("inflight lock poisoned")
                                .contains_key(&(approved_sender.clone(), id.to_owned()))
                        };
                        match crate::operation_journal::acknowledge(
                            &tx,
                            &scope,
                            &ack,
                            crate::operation_journal::Backend::Encrypted,
                            &live,
                        ) {
                            Ok(result) => result,
                            Err(error) => {
                                log!("operation acknowledgement refused: {error:#}");
                                return Ok(None);
                            }
                        }
                    }
                    None => crate::operation_journal::descriptor(
                        &tx,
                        &scope,
                        crate::operation_journal::Backend::Encrypted,
                    )?
                    .value(),
                };
                tx.commit()?;
                Ok(Some(result))
            })
            .await?;
        approved.recheck()?;
        let current_device = self
            .devices
            .get(sender)
            .context("operation journal device changed")?;
        anyhow::ensure!(
            self.operation_journal_scope(sender, &current_device, transport, &epoch)?
                .binding
                == original_binding,
            "operation journal key binding changed"
        );
        anyhow::ensure!(
            self.domain.as_ref().unwrap().state()?.data_epoch.0 == epoch,
            "operation journal epoch changed"
        );
        Ok(match result {
            Some(result) => json!({"status":200,"body":result}),
            None => json!({"status":409,"body":{"error":"outcome_unknown"}}),
        })
    }

    pub(super) fn operation_scope(
        &self,
        envelope: &Envelope,
        request: &LocalRequest,
        device: &Device,
        transport: Transport,
    ) -> Result<Option<RecoveryScope>> {
        let Some(domain) = &self.domain else {
            return Ok(None);
        };
        if !crate::views::actions::approved_path(&request.method, &request.path) {
            return Ok(None);
        }
        let Some(epoch) = request
            .body
            .as_ref()
            .and_then(|body| body["data_epoch"].as_str())
        else {
            return Ok(None);
        };
        if domain.state()?.data_epoch.0 != epoch {
            return Ok(None);
        }
        let command_id = request
            .body
            .as_ref()
            .and_then(|body| body["command_id"].as_str())
            .unwrap_or_default();
        let operation =
            if let Some(identity) = alondra_core::operation_id::OperationId::parse(command_id) {
                anyhow::ensure!(
                    command_id == envelope.request_id && identity.matches_epoch(epoch),
                    "encrypted operation identity mismatch"
                );
                Some(self.operation_journal_scope(&envelope.sender, device, transport, epoch)?)
            } else {
                None
            };
        Ok(Some(RecoveryScope {
            operation,
            digest: digest(envelope)?,
            binding: self.operation_binding(&envelope.sender, device, transport, epoch)?,
        }))
    }

    pub(super) async fn operation_result(
        &self,
        sender: &str,
        request: &Lookup,
        transport: Transport,
    ) -> Result<Value> {
        let device = self
            .devices
            .get(sender)
            .context("operation result device is not approved")?;
        let approved = self.approved_device(sender, &device, transport)?;
        let domain = self
            .domain
            .as_ref()
            .context("operation results require the domain store")?;
        if domain.state()?.data_epoch.0 != request.data_epoch {
            return Ok(json!({"status":409,"body":{"error":"outcome_unknown"}}));
        }
        let binding = self.operation_binding(sender, &device, transport, &request.data_epoch)?;
        let (sender_owned, id, digest) = (sender.to_owned(), request.id.clone(), request.digest);
        let physical = self.journal.accept_jobs.clone().acquire_owned().await?;
        let result = self.journal.off_runtime(move |journal| {
            let _physical = physical;
            let c = journal.db()?;
            let stored: Option<(String, Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>)> = c.query_row(
                "SELECT state,response,recovery_digest,recovery_binding FROM service_receipts WHERE sender=?1 AND request_id=?2 AND length(response)<=?3",
                params![sender_owned,id,i64::try_from(MAX_RELAY_RESPONSE)?], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional()?;
            Ok(match stored {
                Some((state, Some(bytes), Some(prior_digest), Some(prior_binding)))
                    if state == "completed" && prior_digest == digest && prior_binding == binding => Some(bytes),
                _ => None,
            })
        }).await?;
        approved.recheck()?;
        if domain.state()?.data_epoch.0 != request.data_epoch
            || self.operation_binding(sender, &device, transport, &request.data_epoch)? != binding
        {
            return Ok(json!({"status":409,"body":{"error":"outcome_unknown"}}));
        }
        let Some(bytes) = result else {
            return Ok(json!({"status":404,"body":{"error":"outcome_unknown"}}));
        };
        let response: Envelope = serde_json::from_slice(&bytes)?;
        if response.kind != "response"
            || response.request_id != request.id
            || response.sender != self.mac_id
            || response.recipient != sender
            || response.route != self.mac_id
        {
            return Ok(json!({"status":404,"body":{"error":"outcome_unknown"}}));
        }
        let complete = json!({"status":200,"body":{"request_id":request.id,"digest":request.digest_text,
            "data_epoch":request.data_epoch,"response":response}});
        let offset = request.offset.unwrap_or(0);
        if serde_json::to_vec(&complete)?.len() <= MAX_LOCAL_RESPONSE {
            if offset != 0 {
                return Ok(json!({"status":422,"body":{"error":"outcome_unknown"}}));
            }
            return Ok(complete);
        }
        if offset >= bytes.len() {
            return Ok(json!({"status":422,"body":{"error":"outcome_unknown"}}));
        }
        let end = offset.saturating_add(RESULT_CHUNK_BYTES).min(bytes.len());
        let chunk = base64::engine::general_purpose::STANDARD.encode(&bytes[offset..end]);
        Ok(
            json!({"status":200,"body":{"request_id":request.id,"digest":request.digest_text,
            "data_epoch":request.data_epoch,"offset":offset,"total":bytes.len(),"chunk":chunk}}),
        )
    }
}
