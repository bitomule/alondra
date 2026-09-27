use super::*;

#[derive(Clone, Serialize)]
pub struct DeviceRevocation {
    pub kind: String,
    pub device_id: String,
    pub mac_id: String,
    pub state: String,
}
pub struct DeviceRevocationJob {
    pub account_id: Uuid,
    pub subject: String,
    pub revocation: DeviceRevocation,
}
impl Management {
    /// A durable tombstone is authorization state immediately, before JSON cleanup.
    pub async fn require_live_device(
        &self,
        subject: &str,
        mac_id: &str,
        client_id: Option<&str>,
    ) -> Result<()> {
        let permitted:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM management_accounts a JOIN management_identities i ON i.account_id=a.id WHERE i.issuer=$1 AND i.subject=$2 AND a.status='active' AND NOT EXISTS(SELECT 1 FROM management_device_revocations r WHERE (r.kind='mac' AND r.device_id=$3) OR (r.kind='client' AND r.device_id=$4)))")
            .bind(&self.issuer).bind(subject).bind(mac_id).bind(client_id).fetch_one(&self.pool).await?;
        ensure!(permitted, DomainError::Forbidden);
        Ok(())
    }
    pub async fn revoked_devices(&self, actor: Uuid) -> Result<Vec<DeviceRevocation>> {
        let mut tx = self.actor_tx(actor, false).await?;
        let rows=sqlx::query("SELECT kind,device_id,mac_id,state FROM management_device_revocations WHERE account_id=$1 ORDER BY kind,device_id")
            .bind(actor).fetch_all(&mut *tx).await?;
        let result = rows
            .iter()
            .map(|r| {
                Ok(DeviceRevocation {
                    kind: r.try_get("kind")?,
                    device_id: r.try_get("device_id")?,
                    mac_id: r.try_get("mac_id")?,
                    state: r.try_get("state")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        tx.commit().await?;
        Ok(result)
    }
    /// Caller obtains these IDs only from an owner-scoped registry snapshot.
    pub async fn revoke_device_request(
        &self,
        actor: Uuid,
        kind: &str,
        device_id: &str,
        mac_id: &str,
        clients: &[String],
        request: &AdminRequest,
    ) -> Result<()> {
        ensure!(
            matches!(kind, "mac" | "client")
                && !device_id.is_empty()
                && device_id.len() <= 128
                && !mac_id.is_empty()
                && mac_id.len() <= 128,
            DomainError::InvalidInput
        );
        let mut tx = self.actor_tx(actor, false).await?;
        request.reserve(&mut tx, actor).await?;
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT account_id FROM management_device_revocations WHERE kind=$1 AND device_id=$2",
        )
        .bind(kind)
        .bind(device_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(owner) = existing {
            ensure!(owner == actor, DomainError::NotFound);
        } else {
            sqlx::query("INSERT INTO management_device_revocations(account_id,kind,device_id,mac_id) VALUES($1,$2,$3,$4)")
                .bind(actor).bind(kind).bind(device_id).bind(mac_id).execute(&mut *tx).await?;
            audit(&mut tx, Some(actor), "device_revoke", actor).await?;
        }
        if kind == "mac" {
            for client in clients {
                ensure!(
                    !client.is_empty() && client.len() <= 128,
                    DomainError::InvalidInput
                );
                sqlx::query("INSERT INTO management_device_revocations(account_id,kind,device_id,mac_id) VALUES($1,'client',$2,$3) ON CONFLICT(kind,device_id) DO NOTHING")
                    .bind(actor).bind(client).bind(mac_id).execute(&mut *tx).await?;
                let owner:Uuid=sqlx::query_scalar("SELECT account_id FROM management_device_revocations WHERE kind='client' AND device_id=$1").bind(client).fetch_one(&mut *tx).await?;
                ensure!(owner == actor, DomainError::NotFound);
            }
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn next_device_revocation(&self) -> Result<Option<DeviceRevocationJob>> {
        let mut tx = self.pool.begin().await?;
        let row=sqlx::query("SELECT r.account_id,r.kind,r.device_id,r.mac_id,r.state,i.subject FROM management_device_revocations r JOIN management_identities i ON i.account_id=r.account_id AND i.issuer=$1 WHERE r.available_at<=clock_timestamp() AND (r.state='pending' OR r.completed_at<clock_timestamp()-interval '5 minutes') ORDER BY r.available_at,r.kind,r.device_id FOR UPDATE OF r SKIP LOCKED LIMIT 1")
            .bind(&self.issuer).fetch_optional(&mut *tx).await?;
        let Some(r) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let job = DeviceRevocationJob {
            account_id: r.try_get("account_id")?,
            subject: r.try_get("subject")?,
            revocation: DeviceRevocation {
                kind: r.try_get("kind")?,
                device_id: r.try_get("device_id")?,
                mac_id: r.try_get("mac_id")?,
                state: r.try_get("state")?,
            },
        };
        sqlx::query("UPDATE management_device_revocations SET available_at=clock_timestamp()+interval '60 seconds' WHERE kind=$1 AND device_id=$2")
            .bind(&job.revocation.kind).bind(&job.revocation.device_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some(job))
    }
    pub async fn complete_device_revocation(&self, job: &DeviceRevocationJob) -> Result<()> {
        sqlx::query("UPDATE management_device_revocations SET state='completed',completed_at=clock_timestamp() WHERE account_id=$1 AND kind=$2 AND device_id=$3")
            .bind(job.account_id).bind(&job.revocation.kind).bind(&job.revocation.device_id).execute(&self.pool).await?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DeviceLabel {
    pub kind: String,
    pub device_id: String,
    pub mac_id: String,
    pub label: Option<String>,
    pub label_revision: i64,
}

pub fn normalized_label(value: Option<&str>) -> Result<Option<String>> {
    use unicode_normalization::UnicodeNormalization;
    let Some(value) = value else { return Ok(None) };
    use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};
    // Reject rather than erase invisible control input; aliases are never trust IDs.
    ensure!(
        !value.chars().any(|c| matches!(
            c.general_category(),
            GeneralCategory::Control | GeneralCategory::Format
        )),
        DomainError::InvalidInput
    );
    let label: String = value.trim().nfc().collect();
    ensure!(label.chars().count() <= 128, DomainError::InvalidInput);
    Ok((!label.is_empty()).then_some(label))
}

impl Management {
    pub async fn device_labels(&self, actor: Uuid) -> Result<Vec<DeviceLabel>> {
        let mut tx = self.actor_tx(actor, false).await?;
        let rows=sqlx::query("SELECT kind,device_id,mac_id,label,revision FROM management_device_labels WHERE account_id=$1 ORDER BY kind,device_id")
            .bind(actor).fetch_all(&mut *tx).await?;
        let labels = rows
            .iter()
            .map(|row| {
                Ok(DeviceLabel {
                    kind: row.try_get("kind")?,
                    device_id: row.try_get("device_id")?,
                    mac_id: row.try_get("mac_id")?,
                    label: row.try_get("label")?,
                    label_revision: row.try_get("revision")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        tx.commit().await?;
        Ok(labels)
    }

    /// IDs come from an owner-scoped registry lookup. Recheck that lookup while the
    /// account transaction serializes label writes with lifecycle/revocation commits.
    pub async fn rename_device_request(
        &self,
        actor: Uuid,
        kind: &str,
        id: &str,
        mac: &str,
        label: Option<&str>,
        expected_revision: i64,
        request: &AdminRequest,
        still_owned: impl FnOnce() -> Result<bool>,
    ) -> Result<DeviceLabel> {
        ensure!(
            matches!(kind, "mac" | "client")
                && !id.is_empty()
                && id.len() <= 128
                && !mac.is_empty()
                && mac.len() <= 128
                && (kind != "mac" || id == mac)
                && expected_revision >= 0
                && expected_revision < i64::MAX,
            DomainError::InvalidInput
        );
        let label = normalized_label(label)?;
        let mut tx = self.actor_tx(actor, false).await?;
        ensure!(still_owned()?, DomainError::NotFound);
        let revoked:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM management_device_revocations WHERE (kind='mac' AND device_id=$1) OR (kind=$2 AND device_id=$3))")
            .bind(mac).bind(kind).bind(id).fetch_one(&mut *tx).await?;
        ensure!(!revoked, DomainError::NotFound);
        request.reserve(&mut tx, actor).await?;
        let current=sqlx::query("SELECT mac_id,revision FROM management_device_labels WHERE account_id=$1 AND kind=$2 AND device_id=$3 FOR UPDATE")
            .bind(actor).bind(kind).bind(id).fetch_optional(&mut *tx).await?;
        if let Some(row) = current {
            ensure!(
                row.try_get::<String, _>("mac_id")? == mac,
                DomainError::NotFound
            );
            ensure!(
                row.try_get::<i64, _>("revision")? == expected_revision,
                DomainError::Conflict
            );
        } else {
            ensure!(expected_revision == 0, DomainError::Conflict);
        }
        let revision = expected_revision + 1;
        sqlx::query("INSERT INTO management_device_labels(account_id,kind,device_id,mac_id,label,revision) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(account_id,kind,device_id) DO UPDATE SET label=EXCLUDED.label,revision=EXCLUDED.revision")
            .bind(actor).bind(kind).bind(id).bind(mac).bind(&label).bind(revision).execute(&mut *tx).await?;
        audit(&mut tx, Some(actor), "device_label", actor).await?;
        tx.commit().await?;
        Ok(DeviceLabel {
            kind: kind.into(),
            device_id: id.into(),
            mac_id: mac.into(),
            label,
            label_revision: revision,
        })
    }
}

#[cfg(test)]
mod label_tests {
    use super::*;
    #[test]
    fn labels_normalize_without_accepting_hidden_controls_or_excess_length() {
        assert_eq!(
            normalized_label(Some("  Cafe\u{301}  "))
                .unwrap()
                .as_deref(),
            Some("Café")
        );
        assert_eq!(normalized_label(Some("  ")).unwrap(), None);
        assert_eq!(normalized_label(None).unwrap(), None);
        assert!(normalized_label(Some(&"é".repeat(128))).is_ok());
        assert!(normalized_label(Some(&"é".repeat(129))).is_err());
        for value in [
            "A\nB",
            "A\u{0}B",
            "A\u{202e}B",
            "A\u{2066}B",
            "A\u{200d}B",
            "A\u{ad}B",
        ] {
            assert!(normalized_label(Some(value)).is_err());
        }
    }
}
