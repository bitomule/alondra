//! Persist browser sessions without storing their bearer capabilities or OIDC
//! credentials in plaintext. Never derive Debug for secret-bearing structs.
use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{aead, rand::{SecureRandom, SystemRandom}};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use sqlx_core::{query::query, row::Row};
use sqlx_postgres::PgPool;
use std::{path::Path, sync::Arc};
use uuid::Uuid;

pub const SESSION_IDLE_MS: i64 = 30 * 60 * 1000;
pub const SESSION_MAX_MS: i64 = 8 * 60 * 60 * 1000;
pub const FLOW_MAX_MS: i64 = 10 * 60 * 1000;
const MAX_SEALED: usize = 64 * 1024;

#[derive(Serialize, Deserialize)]
pub struct FlowSecrets { pub verifier: String, pub nonce: String }
#[derive(Serialize, Deserialize)]
pub struct SessionSecrets { pub access_token: String, pub refresh_token: String, pub csrf_token: String, pub nonce: String }
pub struct Session {
    pub account_id: Uuid,
    pub subject: String,
    pub secrets: SessionSecrets,
    pub access_expires_ms: i64,
    pub revision: i64,
    pub refresh_state: String,
}

#[derive(Clone)]
pub struct Store { pub pool: PgPool, key: Arc<aead::LessSafeKey> }

pub fn random_token() -> Result<String> {
    let mut value = [0u8; 32];
    SystemRandom::new().fill(&mut value).map_err(|_| anyhow::anyhow!("random unavailable"))?;
    Ok(URL_SAFE_NO_PAD.encode(value))
}
pub fn digest(value: &str) -> Vec<u8> { Sha256::digest(value.as_bytes()).to_vec() }
pub fn cookie_digest(value: &str) -> Result<Vec<u8>> {
    let decoded = URL_SAFE_NO_PAD.decode(value).context("invalid session capability")?;
    ensure!(decoded.len() == 32 && URL_SAFE_NO_PAD.encode(&decoded) == value, "invalid session capability");
    Ok(digest(value))
}

impl Store {
    pub fn from_key_file(pool: PgPool, path: &Path) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let file = std::fs::File::open(path).context("session encryption key unavailable")?;
        let metadata = file.metadata()?;
        ensure!(metadata.is_file() && metadata.permissions().mode() & 0o077 == 0 && metadata.len() == 32,
            "session key must be a private 32-byte file");
        let mut raw = [0u8; 32];
        use std::io::Read;
        (&file).read_exact(&mut raw)?;
        let result = Self::from_key(pool, &raw);
        raw.fill(0);
        result
    }
    fn from_key(pool: PgPool, raw: &[u8]) -> Result<Self> {
        let key = aead::UnboundKey::new(&aead::AES_256_GCM, raw).map_err(|_| anyhow::anyhow!("invalid session key"))?;
        Ok(Self { pool, key: Arc::new(aead::LessSafeKey::new(key)) })
    }
    fn seal<T: Serialize>(&self, value: &T, purpose: &[u8], hash: &[u8]) -> Result<Vec<u8>> {
        let mut body = serde_json::to_vec(value)?;
        ensure!(body.len() < MAX_SEALED - 64, "session payload too large");
        let mut nonce = [0u8; 12];
        SystemRandom::new().fill(&mut nonce).map_err(|_| anyhow::anyhow!("random unavailable"))?;
        let aad = [purpose, hash].concat();
        self.key.seal_in_place_append_tag(aead::Nonce::assume_unique_for_key(nonce), aead::Aad::from(aad), &mut body)
            .map_err(|_| anyhow::anyhow!("session encryption failed"))?;
        let mut sealed = Vec::with_capacity(body.len()+13);
        sealed.push(1); sealed.extend_from_slice(&nonce); sealed.extend_from_slice(&body);
        Ok(sealed)
    }
    fn unseal<T: DeserializeOwned>(&self, mut value: Vec<u8>, purpose: &[u8], hash: &[u8]) -> Result<T> {
        ensure!((29..=MAX_SEALED).contains(&value.len()) && value[0] == 1, "invalid sealed session");
        let nonce: [u8; 12] = value[1..13].try_into()?;
        let aad = [purpose, hash].concat();
        let plain = self.key.open_in_place(aead::Nonce::assume_unique_for_key(nonce), aead::Aad::from(aad), &mut value[13..])
            .map_err(|_| anyhow::anyhow!("session authentication failed"))?;
        serde_json::from_slice(plain).context("invalid session payload")
    }
    pub async fn create_flow(&self, cookie: &str, state: &str, secrets: &FlowSecrets, now: i64) -> Result<()> {
        let hash = cookie_digest(cookie)?;
        let sealed = self.seal(secrets, b"alondra-web-flow-v1", &hash)?;
        let mut tx = self.pool.begin().await?;
        // Bound unauthenticated resource allocation across concurrent instances.
        query("SELECT pg_advisory_xact_lock(719804183902)").execute(&mut *tx).await?;
        query("DELETE FROM web_login_flows WHERE expires_ms <= $1").bind(now).execute(&mut *tx).await?;
        let count: i64 = query("SELECT count(*) AS count FROM web_login_flows").fetch_one(&mut *tx).await?.try_get("count")?;
        ensure!(count < 1024, "login capacity reached");
        query("INSERT INTO web_login_flows(cookie_hash,state_hash,sealed,expires_ms) VALUES($1,$2,$3,$4)")
            .bind(hash).bind(digest(state)).bind(sealed).bind(now + FLOW_MAX_MS).execute(&mut *tx).await?;
        tx.commit().await?; Ok(())
    }
    pub async fn consume_flow(&self, cookie: &str, state: &str, now: i64) -> Result<FlowSecrets> {
        let hash = cookie_digest(cookie)?;
        // Both browser cookie and returned state are needed. Wrong state cannot
        // destroy another browser's flow. A valid callback consumes it once.
        let row = query("DELETE FROM web_login_flows WHERE cookie_hash=$1 AND state_hash=$2 AND expires_ms>$3 RETURNING sealed")
            .bind(&hash).bind(digest(state)).bind(now).fetch_optional(&self.pool).await?.context("login flow unavailable")?;
        self.unseal(row.try_get("sealed")?, b"alondra-web-flow-v1", &hash)
    }
    pub async fn create_session(&self, cookie: &str, account: Uuid, subject: &str, secrets: &SessionSecrets, access_expires: i64, now: i64) -> Result<()> {
        let hash = cookie_digest(cookie)?;
        let sealed = self.seal(secrets, b"alondra-web-session-v1", &hash)?;
        let result = query("INSERT INTO web_sessions(cookie_hash,account_id,subject,sealed,created_ms,used_ms,expires_ms,access_expires_ms) SELECT $1,$2,$3,$4,$5,$5,$6,$7 WHERE EXISTS(SELECT 1 FROM management_accounts WHERE id=$2 AND status='active')")
            .bind(hash).bind(account).bind(subject).bind(sealed).bind(now).bind(now+SESSION_MAX_MS).bind(access_expires).execute(&self.pool).await?;
        ensure!(result.rows_affected() == 1, "account is no longer active"); Ok(())
    }
    pub async fn load_session(&self, cookie: &str, now: i64) -> Result<Option<Session>> {
        let hash = cookie_digest(cookie)?;
        let row = query("SELECT s.* FROM web_sessions s JOIN management_accounts a ON a.id=s.account_id WHERE s.cookie_hash=$1 AND s.expires_ms>$2 AND s.used_ms>$3 AND s.refresh_state!='revoked' AND a.status='active'")
            .bind(&hash).bind(now).bind(now-SESSION_IDLE_MS).fetch_optional(&self.pool).await?;
        row.map(|row| Ok(Session { account_id: row.try_get("account_id")?, subject: row.try_get("subject")?,
            secrets: self.unseal(row.try_get("sealed")?, b"alondra-web-session-v1", &hash)?,
            access_expires_ms: row.try_get("access_expires_ms")?, revision: row.try_get("revision")?, refresh_state: row.try_get("refresh_state")? })).transpose()
    }
    pub async fn touch(&self, cookie: &str, now: i64) -> Result<bool> {
        let result = query("UPDATE web_sessions s SET used_ms=$2 WHERE cookie_hash=$1 AND expires_ms>$2 AND used_ms>$3 AND refresh_state='ready' AND EXISTS(SELECT 1 FROM management_accounts a WHERE a.id=s.account_id AND a.status='active')")
            .bind(cookie_digest(cookie)?).bind(now).bind(now-SESSION_IDLE_MS).execute(&self.pool).await?;
        Ok(result.rows_affected()==1)
    }
    // Persist BEFORE sending a rotating refresh token. A crash or ambiguous
    // provider response leaves refreshing, which must be revoked, never replayed.
    pub async fn begin_refresh(&self, cookie: &str, revision: i64, now: i64) -> Result<bool> {
        let result = query("UPDATE web_sessions s SET refresh_state='refreshing' WHERE cookie_hash=$1 AND revision=$2 AND refresh_state='ready' AND expires_ms>$3 AND used_ms>$4 AND EXISTS(SELECT 1 FROM management_accounts a WHERE a.id=s.account_id AND a.status='active')")
            .bind(cookie_digest(cookie)?).bind(revision).bind(now).bind(now-SESSION_IDLE_MS).execute(&self.pool).await?;
        Ok(result.rows_affected()==1)
    }
    pub async fn finish_refresh(&self, cookie: &str, revision: i64, secrets: &SessionSecrets, access_expires: i64, now: i64) -> Result<bool> {
        let hash=cookie_digest(cookie)?;
        let sealed=self.seal(secrets,b"alondra-web-session-v1",&hash)?;
        let result=query("UPDATE web_sessions s SET sealed=$3,access_expires_ms=$4,used_ms=$5,revision=revision+1,refresh_state='ready' WHERE cookie_hash=$1 AND revision=$2 AND refresh_state='refreshing' AND expires_ms>$5 AND used_ms>$6 AND EXISTS(SELECT 1 FROM management_accounts a WHERE a.id=s.account_id AND a.status='active')")
            .bind(hash).bind(revision).bind(sealed).bind(access_expires).bind(now).bind(now-SESSION_IDLE_MS).execute(&self.pool).await?;
        Ok(result.rows_affected()==1)
    }
    pub async fn revoke(&self, cookie: &str) -> Result<()> {
        query("UPDATE web_sessions SET refresh_state='revoked' WHERE cookie_hash=$1").bind(cookie_digest(cookie)?).execute(&self.pool).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn store() -> Store { Store::from_key(sqlx_postgres::PgPoolOptions::new().connect_lazy("postgresql://localhost/synthetic").unwrap(), &[7;32]).unwrap() }
    #[tokio::test] async fn sealed_credentials_reject_tampering_wrong_row_purpose_and_key() {
        let s=store(); let cookie=random_token().unwrap(); let hash=cookie_digest(&cookie).unwrap();
        let secret=FlowSecrets{verifier:"synthetic verifier".into(),nonce:"synthetic nonce".into()};
        let sealed=s.seal(&secret,b"flow",&hash).unwrap();
        assert!(!sealed.windows(secret.verifier.len()).any(|x|x==secret.verifier.as_bytes()));
        assert_eq!(s.unseal::<FlowSecrets>(sealed.clone(),b"flow",&hash).unwrap().nonce,secret.nonce);
        assert!(s.unseal::<FlowSecrets>(sealed.clone(),b"session",&hash).is_err());
        assert!(s.unseal::<FlowSecrets>(sealed.clone(),b"flow",&[9;32]).is_err());
        let other=Store::from_key(s.pool.clone(),&[8;32]).unwrap();
        assert!(other.unseal::<FlowSecrets>(sealed.clone(),b"flow",&hash).is_err());
        let mut tampered=sealed; *tampered.last_mut().unwrap()^=1;
        assert!(s.unseal::<FlowSecrets>(tampered,b"flow",&hash).is_err());
    }
    #[test] fn capabilities_are_canonical_and_have_256_bits() {
        let value=random_token().unwrap(); assert_eq!(value.len(),43); assert_eq!(cookie_digest(&value).unwrap().len(),32);
        for bad in ["", "one", &format!("{value}="), &"A".repeat(44)] { assert!(cookie_digest(bad).is_err()); }
    }
}
