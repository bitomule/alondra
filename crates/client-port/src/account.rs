//! The Alondra account this terminal signed in with: the relay and the directory of Macs ask for
//! its tokens. The root installs the keychain that keeps them.
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Result, anyhow};

#[derive(Clone)]
pub struct AccountToken {
    pub sub: String,
    pub access_token: String,
    pub valid_for: Duration,
}

/// Contention is temporary and does not indicate expired credentials.
#[derive(Debug)]
pub struct AccountBusy;

impl std::fmt::Display for AccountBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("account login or refresh is busy; retry when it finishes")
    }
}

impl std::error::Error for AccountBusy {}

pub trait Account: Send + Sync {
    /// A token for the relay's service routes, cached while it lasts.
    fn token_for_service(&self) -> Result<AccountToken>;
    /// A token to enroll with, signing in first when there is no session.
    fn ensure_token_for_enrollment(&self) -> Result<AccountToken>;
}

static ACCOUNT: OnceLock<Arc<dyn Account>> = OnceLock::new();

/// Installs the process's account; the first one stays.
pub fn install(account: Arc<dyn Account>) {
    let _ = ACCOUNT.set(account);
}

pub fn account() -> Result<&'static Arc<dyn Account>> {
    ACCOUNT.get().ok_or_else(|| anyhow!("this build has no Alondra account"))
}
