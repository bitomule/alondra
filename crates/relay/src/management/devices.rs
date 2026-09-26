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
