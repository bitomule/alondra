//! Private product access. Identity and administration never grant content keys.
//! External side effects are reconciled separately from PostgreSQL transactions.
pub mod devices;
pub mod http;
pub mod keycloak;
pub mod worker;
// PostgreSQL-only SQLx components avoid the optional SQLite native-link conflict
// with the desktop workspace's newer rusqlite. Versions are pinned together.
mod sqlx {
    pub use sqlx_core::{
        acquire::Acquire, pool, query::query, query_scalar::query_scalar, raw_sql::raw_sql,
        row::Row, transaction::Transaction,
    };
    pub use sqlx_postgres as postgres;
    pub use sqlx_postgres::{PgPool, Postgres};
}

use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgPoolOptions};
use uuid::Uuid;

const ADMIN_LOCK: i64 = 0x414c4f4e445241;

#[derive(Clone)]
pub struct Management {
    pub(crate) pool: PgPool,
    issuer: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Account {
    pub id: Uuid,
    pub status: String,
    pub is_admin: bool,
    pub display_name: String,
}
impl Account {
    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self> {
        Ok(Self {
            id: row.try_get("id")?,
            status: row.try_get("status")?,
            is_admin: row.try_get("is_admin")?,
            display_name: row.try_get("display_name")?,
        })
    }
}
#[derive(Serialize)]
pub struct AdminAccount {
    #[serde(flatten)]
    pub account: Account,
    pub email: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct Invitation {
    pub id: Uuid,
    pub email: String,
    pub status: String,
    pub account_id: Option<Uuid>,
    pub expires_at_ms: i64,
}
impl Invitation {
    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self> {
        Ok(Self {
            id: row.try_get("id")?,
            email: row.try_get("email")?,
            status: row.try_get("status")?,
            account_id: row.try_get("account_id")?,
            expires_at_ms: row.try_get("expires_at_ms")?,
        })
    }
}
/// Deliberately not Debug: a newly generated bearer invitation is returned only once.
pub struct IssuedInvitation {
    pub invitation: Invitation,
    pub token: String,
}
#[derive(Serialize)]
pub struct Page<T: Serialize> {
    pub items: Vec<T>,
    pub next_cursor: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainError {
    Forbidden,
    NotFound,
    Conflict,
    InvalidInput,
    Unavailable,
    ProviderUnavailable,
    AlreadyApplied,
    TokenAlreadyIssued,
    IdempotencyConflict,
}
impl std::fmt::Display for DomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::InvalidInput => "invalid_input",
            Self::Unavailable => "invitation_unavailable",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::AlreadyApplied => "action_already_applied",
            Self::TokenAlreadyIssued => "invitation_token_already_issued",
            Self::IdempotencyConflict => "idempotency_conflict",
        })
    }
}
impl std::error::Error for DomainError {}
fn domain<T>(error: DomainError) -> Result<T> {
    Err(error.into())
}
fn hash_token(token: &str) -> Result<Vec<u8>> {
    let bytes = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| DomainError::Unavailable)?;
    ensure!(
        bytes.len() == 32 && URL_SAFE_NO_PAD.encode(&bytes) == token,
        DomainError::Unavailable
    );
    Ok(Sha256::digest(bytes).to_vec())
}
fn token() -> Result<(String, Vec<u8>)> {
    let mut bytes = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|_| anyhow::anyhow!("secure random unavailable"))?;
    Ok((
        URL_SAFE_NO_PAD.encode(bytes),
        Sha256::digest(bytes).to_vec(),
    ))
}
fn email(input: &str) -> Result<String> {
    let value = input.trim().to_ascii_lowercase();
    ensure!(
        value.len() <= 254
            && value.is_ascii()
            && !value
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control()),
        DomainError::InvalidInput
    );
    let parts = value.split('@').collect::<Vec<_>>();
    ensure!(
        parts.len() == 2
            && !parts[0].is_empty()
            && parts[1].contains('.')
            && !parts[1].starts_with('.')
            && !parts[1].ends_with('.'),
        DomainError::InvalidInput
    );
    Ok(value)
}
fn ttl(seconds: i64) -> Result<i64> {
    ensure!((300..=604800).contains(&seconds), DomainError::InvalidInput);
    Ok(seconds)
}
fn invitation_lock(id: Uuid) -> i64 {
    i64::from_be_bytes(id.as_bytes()[..8].try_into().unwrap())
}
async fn lock_invitation(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(invitation_lock(id))
        .execute(&mut **tx)
        .await?;
    Ok(())
}
async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    actor: Option<Uuid>,
    action: &str,
    target: Uuid,
) -> Result<()> {
    sqlx::query("INSERT INTO management_audit(actor_id,action,target_id) VALUES($1,$2,$3)")
        .bind(actor)
        .bind(action)
        .bind(target)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
async fn enqueue(tx: &mut Transaction<'_, Postgres>, account: Uuid, action: &str) -> Result<()> {
    sqlx::query("INSERT INTO management_outbox(id,account_id,action) VALUES($1,$2,$3) ON CONFLICT(account_id,action) DO NOTHING")
        .bind(Uuid::new_v4()).bind(account).bind(action).execute(&mut **tx).await?;
    Ok(())
}

/// Only hashes enter PostgreSQL; neither the idempotency key nor invitation bearer is persisted.
pub struct AdminRequest {
    key_hash: Vec<u8>,
    request_hash: Vec<u8>,
    issues_token: bool,
}
impl AdminRequest {
    pub fn new(
        key: &str,
        operation: &str,
        body: &serde_json::Value,
        issues_token: bool,
    ) -> Result<Self> {
        ensure!(
            (16..=128).contains(&key.len())
                && key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
            DomainError::InvalidInput
        );
        let request = serde_json::to_vec(&(operation, body))?;
        Ok(Self {
            key_hash: Sha256::digest(key.as_bytes()).to_vec(),
            request_hash: Sha256::digest(request).to_vec(),
            issues_token,
        })
    }
    async fn reserve(&self, tx: &mut Transaction<'_, Postgres>, actor: Uuid) -> Result<()> {
        if let Some(previous) = sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT request_hash FROM management_admin_requests WHERE actor_id=$1 AND key_hash=$2",
        )
        .bind(actor)
        .bind(&self.key_hash)
        .fetch_optional(&mut **tx)
        .await?
        {
            ensure!(
                previous == self.request_hash,
                DomainError::IdempotencyConflict
            );
            return domain(if self.issues_token {
                DomainError::TokenAlreadyIssued
            } else {
                DomainError::AlreadyApplied
            });
        }
        sqlx::query("INSERT INTO management_admin_requests(actor_id,key_hash,request_hash) VALUES($1,$2,$3)")
            .bind(actor).bind(&self.key_hash).bind(&self.request_hash).execute(&mut **tx).await?;
        Ok(())
    }
}

impl Management {
    pub async fn open(database_url: &str, issuer: String) -> Result<Self> {
        ensure!(!issuer.is_empty(), "management requires an issuer");
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::query("SET statement_timeout = '5s'")
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect(database_url)
            .await
            .context("management database unavailable")?;
        let mut migration = pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(ADMIN_LOCK)
            .execute(&mut *migration)
            .await?;
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS management_schema(version INTEGER PRIMARY KEY, checksum BYTEA NOT NULL)").execute(&mut *migration).await?;
        let future: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM management_schema WHERE version>2)")
                .fetch_one(&mut *migration)
                .await?;
        ensure!(!future, "management schema is newer than this binary");
        for (version, source) in [
            (1i32, include_str!("../../migrations/0001_management.sql")),
            (2i32, include_str!("../../migrations/0002_web_sessions.sql")),
        ] {
            let checksum = Sha256::digest(source.as_bytes()).to_vec();
            let previous: Option<Vec<u8>> = sqlx::query_scalar("SELECT checksum FROM management_schema WHERE version=$1")
                .bind(version).fetch_optional(&mut *migration).await?;
            if let Some(previous) = previous {
                ensure!(previous == checksum, "management migration checksum mismatch");
            } else {
                sqlx::raw_sql(source).execute(&mut *migration).await?;
                sqlx::query("INSERT INTO management_schema VALUES($1,$2)")
                    .bind(version).bind(checksum).execute(&mut *migration).await?;
            }
        }
        migration.commit().await?;
        Ok(Self { pool, issuer })
    }
    /// Bootstrap only an empty installation from an operator-supplied immutable OIDC subject.
    /// Restart never re-enables, promotes, or relinks an existing identity.
    pub async fn bootstrap(&self, subject: &str, name: &str) -> Result<Account> {
        ensure!(
            !subject.is_empty() && !name.is_empty() && name.len() <= 256,
            DomainError::InvalidInput
        );
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(ADMIN_LOCK)
            .execute(&mut *tx)
            .await?;
        if let Some(row)=sqlx::query("SELECT a.id,a.status,a.is_admin,a.display_name FROM management_accounts a JOIN management_identities i ON i.account_id=a.id WHERE i.issuer=$1 AND i.subject=$2")
            .bind(&self.issuer).bind(subject).fetch_optional(&mut *tx).await? {
            let account=Account::from_row(&row)?;
            ensure!(account.is_admin && account.status=="active",DomainError::Forbidden);
            tx.commit().await?;return Ok(account)
        }
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM management_accounts")
            .fetch_one(&mut *tx)
            .await?;
        ensure!(count == 0, DomainError::Forbidden);
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO management_accounts(id,status,is_admin,display_name) VALUES($1,'active',true,$2)")
            .bind(id).bind(name).execute(&mut *tx).await?;
        sqlx::query(
            "INSERT INTO management_identities(issuer,subject,account_id) VALUES($1,$2,$3)",
        )
        .bind(&self.issuer)
        .bind(subject)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        audit(&mut tx, Some(id), "bootstrap", id).await?;
        tx.commit().await?;
        Ok(Account {
            id,
            status: "active".into(),
            is_admin: true,
            display_name: name.into(),
        })
    }
    pub async fn active_identity(&self, issuer: &str, subject: &str) -> Result<Account> {
        ensure!(issuer == self.issuer, DomainError::Forbidden);
        let row=sqlx::query("SELECT a.id,a.status,a.is_admin,a.display_name FROM management_accounts a JOIN management_identities i ON i.account_id=a.id WHERE i.issuer=$1 AND i.subject=$2 AND a.status='active'")
            .bind(issuer).bind(subject).fetch_optional(&self.pool).await?.ok_or(DomainError::Forbidden)?;
        Account::from_row(&row)
    }
    async fn admin_tx(&self, actor: Uuid) -> Result<Transaction<'_, Postgres>> {
        self.actor_tx(actor, true).await
    }
    async fn actor_tx(
        &self,
        actor: Uuid,
        require_admin: bool,
    ) -> Result<Transaction<'_, Postgres>> {
        let mut tx = self.pool.begin().await?;
        // Serializes administrator lifecycle checks, including the last-admin invariant.
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(ADMIN_LOCK)
            .execute(&mut *tx)
            .await?;
        let permitted:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM management_accounts WHERE id=$1 AND status='active' AND (NOT $2 OR is_admin))")
            .bind(actor).bind(require_admin).fetch_one(&mut *tx).await?;
        ensure!(permitted, DomainError::Forbidden);
        Ok(tx)
    }
    pub async fn list_accounts(
        &self,
        actor: Uuid,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<Page<AdminAccount>> {
        ensure!((1..=100).contains(&limit), DomainError::InvalidInput);
        let mut tx = self.admin_tx(actor).await?;
        let rows=sqlx::query("SELECT a.id,a.status,a.is_admin,a.display_name,(SELECT email FROM management_invitations i WHERE i.account_id=a.id LIMIT 1) AS email FROM management_accounts a WHERE ($1::uuid IS NULL OR a.id>$1) ORDER BY a.id LIMIT $2")
            .bind(after).bind(limit+1).fetch_all(&mut *tx).await?;
        let mut items = rows
            .iter()
            .map(|row| {
                Ok(AdminAccount {
                    account: Account::from_row(row)?,
                    email: row.try_get("email")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let next_cursor = if items.len() > limit as usize {
            items.truncate(limit as usize);
            items.last().map(|a| a.account.id)
        } else {
            None
        };
        tx.commit().await?;
        Ok(Page { items, next_cursor })
    }
    pub async fn list_invitations(
        &self,
        actor: Uuid,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<Page<Invitation>> {
        ensure!((1..=100).contains(&limit), DomainError::InvalidInput);
        let mut tx = self.admin_tx(actor).await?;
        let rows=sqlx::query("SELECT id,email,status,account_id,(extract(epoch FROM expires_at)*1000)::bigint AS expires_at_ms FROM management_invitations WHERE ($1::uuid IS NULL OR id>$1) ORDER BY id LIMIT $2")
            .bind(after).bind(limit+1).fetch_all(&mut *tx).await?;
        let mut items = rows
            .iter()
            .map(Invitation::from_row)
            .collect::<Result<Vec<_>>>()?;
        let next_cursor = if items.len() > limit as usize {
            items.truncate(limit as usize);
            items.last().map(|a| a.id)
        } else {
            None
        };
        tx.commit().await?;
        Ok(Page { items, next_cursor })
    }
    #[cfg(test)]
    pub async fn create_invitation(
        &self,
        actor: Uuid,
        address: &str,
        seconds: i64,
    ) -> Result<IssuedInvitation> {
        self.create_invitation_request(actor, address, seconds, None)
            .await
    }
    pub async fn create_invitation_request(
        &self,
        actor: Uuid,
        address: &str,
        seconds: i64,
        request: Option<&AdminRequest>,
    ) -> Result<IssuedInvitation> {
        let address = email(address)?;
        let seconds = ttl(seconds)?;
        let id = Uuid::new_v4();
        let (token, hash) = token()?;
        let mut tx = self.admin_tx(actor).await?;
        if let Some(request) = request {
            request.reserve(&mut tx, actor).await?;
        }
        // Expired invitations must be canceled explicitly, so provisioning cleanup is never skipped.
        let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM management_invitations i LEFT JOIN management_accounts a ON a.id=i.account_id WHERE i.email=$1 AND (i.status IN ('pending','provisioning') OR a.status!='deleted'))")
            .bind(&address).fetch_one(&mut *tx).await?;
        ensure!(!exists, DomainError::Conflict);
        let row=sqlx::query("INSERT INTO management_invitations(id,email,token_hash,status,expires_at,created_by) VALUES($1,$2,$3,'pending',now()+make_interval(secs=>$4),$5) RETURNING id,email,status,account_id,(extract(epoch FROM expires_at)*1000)::bigint AS expires_at_ms")
            .bind(id).bind(&address).bind(hash).bind(seconds as f64).bind(actor).fetch_one(&mut *tx).await?;
        audit(&mut tx, Some(actor), "invite_create", id).await?;
        tx.commit().await?;
        Ok(IssuedInvitation {
            invitation: Invitation::from_row(&row)?,
            token,
        })
    }
    #[cfg(test)]
    pub async fn cancel_invitation(&self, actor: Uuid, id: Uuid) -> Result<Invitation> {
        self.cancel_invitation_request(actor, id, None).await
    }
    pub async fn cancel_invitation_request(
        &self,
        actor: Uuid,
        id: Uuid,
        request: Option<&AdminRequest>,
    ) -> Result<Invitation> {
        let mut tx = self.admin_tx(actor).await?;
        if let Some(request) = request {
            request.reserve(&mut tx, actor).await?;
        }
        lock_invitation(&mut tx, id).await?;
        let row=sqlx::query("SELECT id,email,status,account_id,(extract(epoch FROM expires_at)*1000)::bigint AS expires_at_ms FROM management_invitations WHERE id=$1 FOR UPDATE")
            .bind(id).fetch_optional(&mut *tx).await?.ok_or(DomainError::NotFound)?;
        let mut invitation = Invitation::from_row(&row)?;
        ensure!(invitation.status != "accepted", DomainError::Conflict);
        if invitation.status != "canceled" {
            sqlx::query("UPDATE management_invitations SET status='canceled' WHERE id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            if let Some(account) = invitation.account_id {
                sqlx::query("UPDATE management_accounts SET status='deleting',updated_at=now() WHERE id=$1 AND status='provisioning'").bind(account).execute(&mut *tx).await?;
                enqueue(&mut tx, account, "keycloak_disable").await?;
                enqueue(&mut tx, account, "relay_revoke").await?;
                enqueue(&mut tx, account, "keycloak_delete").await?;
            }
            audit(&mut tx, Some(actor), "invite_cancel", id).await?;
            invitation.status = "canceled".into();
        }
        tx.commit().await?;
        Ok(invitation)
    }
    #[cfg(test)]
    pub async fn disable_account(&self, actor: Uuid, id: Uuid, delete: bool) -> Result<Account> {
        self.disable_account_request(actor, id, delete, None).await
    }
    pub async fn disable_account_request(
        &self,
        actor: Uuid,
        id: Uuid,
        delete: bool,
        request: Option<&AdminRequest>,
    ) -> Result<Account> {
        self.change_account_access(actor, id, delete, false, request)
            .await
    }
    /// Called only after an explicitly confirmed self-service deletion request.
    pub async fn delete_self_request(
        &self,
        actor: Uuid,
        request: &AdminRequest,
    ) -> Result<Account> {
        self.change_account_access(actor, actor, true, true, Some(request))
            .await
    }
    async fn change_account_access(
        &self,
        actor: Uuid,
        id: Uuid,
        delete: bool,
        self_delete: bool,
        request: Option<&AdminRequest>,
    ) -> Result<Account> {
        let mut tx = self.actor_tx(actor, !self_delete).await?;
        if let Some(request) = request {
            request.reserve(&mut tx, actor).await?;
        }
        ensure!(actor != id || self_delete && delete, DomainError::Forbidden);
        let row=sqlx::query("SELECT id,status,is_admin,display_name FROM management_accounts WHERE id=$1 FOR UPDATE")
            .bind(id).fetch_optional(&mut *tx).await?.ok_or(DomainError::NotFound)?;
        let mut account = Account::from_row(&row)?;
        if account.is_admin && account.status == "active" {
            let admins: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM management_accounts WHERE is_admin AND status='active'",
            )
            .fetch_one(&mut *tx)
            .await?;
            ensure!(admins > 1, DomainError::Forbidden);
        }
        if account.status == "deleted" || account.status == "deleting" && !delete {
            tx.commit().await?;
            return Ok(account);
        }
        let next = if delete { "deleting" } else { "disabled" };
        if account.status != next {
            sqlx::query("UPDATE management_accounts SET status=$2,updated_at=now() WHERE id=$1")
                .bind(id)
                .bind(next)
                .execute(&mut *tx)
                .await?;
            audit(
                &mut tx,
                Some(actor),
                if delete {
                    "account_delete"
                } else {
                    "account_disable"
                },
                id,
            )
            .await?;
            account.status = next.into();
        }
        enqueue(&mut tx, id, "keycloak_disable").await?;
        enqueue(&mut tx, id, "relay_revoke").await?;
        if delete {
            enqueue(&mut tx, id, "keycloak_delete").await?;
        }
        tx.commit().await?;
        Ok(account)
    }
}

/// Reservation holds a session advisory lock on a dedicated connection. Dropping it
/// closes that connection, including on cancellation, rather than returning a locked
/// connection to the pool. Keycloak creation must be create-only, never reset-password.
pub struct Acceptance {
    connection: sqlx::pool::PoolConnection<Postgres>,
    pub account_id: Uuid,
    pub invitation_id: Uuid,
    pub email: String,
    pub display_name: String,
    issuer: String,
}
impl Acceptance {
    pub fn username(&self) -> String {
        format!("alondra-{}", self.account_id)
    }
    /// Subject comes exclusively from a verified Keycloak correlation lookup/create.
    /// Revalidates cancellation, expiry and account status after the external operation.
    pub async fn complete(mut self, subject: &str) -> Result<Account> {
        ensure!(!subject.is_empty(), DomainError::InvalidInput);
        use sqlx::Acquire;
        let mut tx = self.connection.begin().await?;
        let row=sqlx::query("SELECT status,expires_at>clock_timestamp() AS live FROM management_invitations WHERE id=$1 FOR UPDATE")
            .bind(self.invitation_id).fetch_one(&mut *tx).await?;
        let status: String = row.try_get("status")?;
        let live: bool = row.try_get("live")?;
        let account_status: String =
            sqlx::query_scalar("SELECT status FROM management_accounts WHERE id=$1 FOR UPDATE")
                .bind(self.account_id)
                .fetch_one(&mut *tx)
                .await?;
        // Persist a correlated identity even on cancellation so cleanup can target only it.
        sqlx::query("INSERT INTO management_identities(issuer,subject,account_id) VALUES($1,$2,$3) ON CONFLICT(issuer,subject) DO NOTHING")
            .bind(&self.issuer).bind(subject).bind(self.account_id).execute(&mut *tx).await?;
        let actual: Uuid = sqlx::query_scalar(
            "SELECT account_id FROM management_identities WHERE issuer=$1 AND subject=$2",
        )
        .bind(&self.issuer)
        .bind(subject)
        .fetch_one(&mut *tx)
        .await?;
        ensure!(actual == self.account_id, DomainError::Conflict);
        if status != "provisioning" || !live || account_status != "provisioning" {
            sqlx::query("UPDATE management_accounts SET status='deleting',updated_at=now() WHERE id=$1 AND status='provisioning'")
                .bind(self.account_id).execute(&mut *tx).await?;
            sqlx::query("UPDATE management_invitations SET status='canceled' WHERE id=$1 AND status='provisioning'")
                .bind(self.invitation_id).execute(&mut *tx).await?;
            enqueue(&mut tx, self.account_id, "keycloak_disable").await?;
            enqueue(&mut tx, self.account_id, "relay_revoke").await?;
            enqueue(&mut tx, self.account_id, "keycloak_delete").await?;
            tx.commit().await?;
            return domain(DomainError::Unavailable);
        }
        sqlx::query("UPDATE management_accounts SET status='active',updated_at=now() WHERE id=$1")
            .bind(self.account_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE management_invitations SET status='accepted',accepted_at=now() WHERE id=$1",
        )
        .bind(self.invitation_id)
        .execute(&mut *tx)
        .await?;
        audit(&mut tx, None, "invite_accept", self.invitation_id).await?;
        tx.commit().await?;
        Ok(Account {
            id: self.account_id,
            status: "active".into(),
            is_admin: false,
            display_name: self.display_name.clone(),
        })
    }
}
impl Management {
    /// Public lookup deliberately returns no email, account identifier or display name.
    pub async fn invitation_status(&self, token: &str) -> Result<&'static str> {
        let hash = hash_token(token)?;
        let status:Option<String>=sqlx::query_scalar("SELECT status FROM management_invitations WHERE token_hash=$1 AND expires_at>clock_timestamp() AND status IN ('pending','provisioning')")
            .bind(hash).fetch_optional(&self.pool).await?;
        match status.as_deref() {
            Some("pending") => Ok("pending"),
            Some("provisioning") => Ok("provisioning"),
            _ => domain(DomainError::Unavailable),
        }
    }
    pub async fn reserve_acceptance(&self, token: &str, name: &str) -> Result<Acceptance> {
        ensure!(
            !name.trim().is_empty() && name.len() <= 256 && !name.chars().any(char::is_control),
            DomainError::InvalidInput
        );
        let hash = hash_token(token)?;
        let id: Uuid =
            sqlx::query_scalar("SELECT id FROM management_invitations WHERE token_hash=$1")
                .bind(&hash)
                .fetch_optional(&self.pool)
                .await?
                .ok_or(DomainError::Unavailable)?;
        let mut connection = self.pool.acquire().await?;
        connection.close_on_drop();
        // No unbounded wait: a simultaneous acceptance fails safely and can be retried.
        let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(invitation_lock(id))
            .fetch_one(&mut *connection)
            .await?;
        ensure!(acquired, DomainError::Conflict);
        use sqlx::Acquire;
        let mut tx = connection.begin().await?;
        let row=sqlx::query("SELECT email,status,account_id,expires_at>clock_timestamp() AS live FROM management_invitations WHERE id=$1 AND token_hash=$2 FOR UPDATE")
            .bind(id).bind(hash).fetch_one(&mut *tx).await?;
        let state: String = row.try_get("status")?;
        ensure!(
            row.try_get::<bool, _>("live")? && (state == "pending" || state == "provisioning"),
            DomainError::Unavailable
        );
        let address: String = row.try_get("email")?;
        let existing: Option<Uuid> = row.try_get("account_id")?;
        let account_id = if let Some(id) = existing {
            let state: String =
                sqlx::query_scalar("SELECT status FROM management_accounts WHERE id=$1 FOR UPDATE")
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await?;
            ensure!(state == "provisioning", DomainError::Unavailable);
            id
        } else {
            let account_id = Uuid::new_v4();
            sqlx::query("INSERT INTO management_accounts(id,status,display_name) VALUES($1,'provisioning',$2)")
                .bind(account_id).bind(name.trim()).execute(&mut *tx).await?;
            sqlx::query(
                "UPDATE management_invitations SET status='provisioning',account_id=$2 WHERE id=$1",
            )
            .bind(id)
            .bind(account_id)
            .execute(&mut *tx)
            .await?;
            account_id
        };
        let display_name: String =
            sqlx::query_scalar("SELECT display_name FROM management_accounts WHERE id=$1")
                .bind(account_id)
                .fetch_one(&mut *tx)
                .await?;
        tx.commit().await?;
        Ok(Acceptance {
            connection,
            account_id,
            invitation_id: id,
            email: address,
            display_name,
            issuer: self.issuer.clone(),
        })
    }
    /// Cancels the previous bearer and issues a new one in one transaction.
    #[cfg(test)]
    pub async fn reissue_invitation(
        &self,
        actor: Uuid,
        id: Uuid,
        seconds: i64,
    ) -> Result<IssuedInvitation> {
        self.reissue_invitation_request(actor, id, seconds, None)
            .await
    }
    pub async fn reissue_invitation_request(
        &self,
        actor: Uuid,
        id: Uuid,
        seconds: i64,
        request: Option<&AdminRequest>,
    ) -> Result<IssuedInvitation> {
        let seconds = ttl(seconds)?;
        let (token, hash) = token()?;
        let next_id = Uuid::new_v4();
        let mut tx = self.admin_tx(actor).await?;
        if let Some(request) = request {
            request.reserve(&mut tx, actor).await?;
        }
        lock_invitation(&mut tx, id).await?;
        let row = sqlx::query(
            "SELECT email,status,account_id FROM management_invitations WHERE id=$1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(DomainError::NotFound)?;
        let status: String = row.try_get("status")?;
        ensure!(status != "accepted", DomainError::Conflict);
        let address: String = row.try_get("email")?;
        sqlx::query("UPDATE management_invitations SET status='canceled' WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        if let Some(account) = row.try_get::<Option<Uuid>, _>("account_id")? {
            sqlx::query("UPDATE management_accounts SET status='deleting',updated_at=now() WHERE id=$1 AND status='provisioning'").bind(account).execute(&mut *tx).await?;
            enqueue(&mut tx, account, "keycloak_disable").await?;
            enqueue(&mut tx, account, "relay_revoke").await?;
            enqueue(&mut tx, account, "keycloak_delete").await?;
        }
        let row=sqlx::query("INSERT INTO management_invitations(id,email,token_hash,status,expires_at,created_by) VALUES($1,$2,$3,'pending',now()+make_interval(secs=>$4),$5) RETURNING id,email,status,account_id,(extract(epoch FROM expires_at)*1000)::bigint AS expires_at_ms")
            .bind(next_id).bind(address).bind(hash).bind(seconds as f64).bind(actor).fetch_one(&mut *tx).await?;
        audit(&mut tx, Some(actor), "invite_cancel", id).await?;
        audit(&mut tx, Some(actor), "invite_create", next_id).await?;
        tx.commit().await?;
        Ok(IssuedInvitation {
            invitation: Invitation::from_row(&row)?,
            token,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests;

#[derive(Debug)]
pub struct Reconciliation {
    pub id: Uuid,
    pub account_id: Uuid,
    pub action: String,
    pub subject: Option<String>,
}
impl Management {
    /// A worker claims a row with a bounded lease; every external operation is idempotent.
    /// Failure or process death leaves the durable desired state available for retry.
    pub async fn next_reconciliation(&self) -> Result<Option<Reconciliation>> {
        let mut tx = self.pool.begin().await?;
        // Expiration must reconcile a create whose HTTP response was lost, even if
        // the recipient never retries. Product access remains denied throughout.
        let expired=sqlx::query("UPDATE management_invitations SET status='canceled' WHERE status IN ('pending','provisioning') AND expires_at<=clock_timestamp() RETURNING account_id")
            .fetch_all(&mut *tx).await?;
        for row in expired {
            if let Some(account) = row.try_get::<Option<Uuid>, _>("account_id")? {
                sqlx::query("UPDATE management_accounts SET status='deleting',updated_at=now() WHERE id=$1 AND status='provisioning'").bind(account).execute(&mut *tx).await?;
                enqueue(&mut tx, account, "keycloak_disable").await?;
                enqueue(&mut tx, account, "relay_revoke").await?;
                enqueue(&mut tx, account, "keycloak_delete").await?;
            }
        }
        // Keep desired-state tombstones: an upstream create can commit after our
        // request times out and after an earlier cleanup observed 'not found'.
        // Rechecking inactive accounts makes that absence provisional, not final.
        sqlx::query("UPDATE management_outbox o SET state='pending',available_at=clock_timestamp() FROM management_accounts a WHERE a.id=o.account_id AND a.status IN ('disabled','deleting','deleted') AND o.state='completed' AND o.completed_at<clock_timestamp()-interval '5 minutes'")
            .execute(&mut *tx).await?;
        let row=sqlx::query("SELECT o.id,o.account_id,o.action,i.subject FROM management_outbox o LEFT JOIN management_identities i ON i.account_id=o.account_id AND i.issuer=$1 WHERE o.state='pending' AND o.available_at<=clock_timestamp() AND (o.action!='keycloak_delete' OR NOT EXISTS(SELECT 1 FROM management_outbox before_delete WHERE before_delete.account_id=o.account_id AND before_delete.action!='keycloak_delete' AND before_delete.state='pending')) ORDER BY o.available_at,o.id FOR UPDATE OF o SKIP LOCKED LIMIT 1")
            .bind(&self.issuer).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let result = Reconciliation {
            id: row.try_get("id")?,
            account_id: row.try_get("account_id")?,
            action: row.try_get("action")?,
            subject: row.try_get("subject")?,
        };
        sqlx::query("UPDATE management_outbox SET attempts=attempts+1,available_at=clock_timestamp()+interval '60 seconds' WHERE id=$1").bind(result.id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some(result))
    }
    pub async fn reconciliation_complete(&self, job: &Reconciliation) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE management_outbox SET state='completed',completed_at=now() WHERE id=$1 AND account_id=$2 AND action=$3")
            .bind(job.id).bind(job.account_id).bind(&job.action).execute(&mut *tx).await?;
        sqlx::query("UPDATE management_accounts a SET status='deleted',updated_at=now(),display_name='' WHERE id=$1 AND status='deleting' AND NOT EXISTS(SELECT 1 FROM management_outbox o WHERE o.account_id=a.id AND state!='completed')")
            .bind(job.account_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}
