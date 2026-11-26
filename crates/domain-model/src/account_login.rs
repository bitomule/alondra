use super::*;
use alondra_core::accounts::{AccountKey, Provider};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountLoginPhase { Starting, Pending, Publishing, Connected, Cancelled, Failed }

impl AccountLoginPhase {
    pub fn active(self) -> bool { matches!(self, Self::Starting | Self::Pending | Self::Publishing) }
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccountLogin {
    pub account: AccountKey,
    pub attempt: String,
    pub started_at: Millis,
    pub phase: AccountLoginPhase,
    pub challenge: Option<alondra_core::codex_login::Challenge>,
    pub error: Option<String>,
}

impl DomainState {
    pub fn begin_account_login_guard(&self, actor: &Actor, account: &AccountKey, attempt: &str) -> Result<()> {
        user(actor)?;
        id(attempt)?;
        if account.provider != Provider::Codex { return fail("invalid_input", "Choose a Codex account"); }
        if !account.account.is_default() && !self.accounts.iter().any(|known| known.provider == account.provider && known.id == account.account) {
            return fail("not_found", "The account is no longer listed");
        }
        if self.accounts.iter().any(|known| known.provider == account.provider && known.id == account.account && known.state == alondra_core::accounts::AccountState::Disabled) {
            return fail("account_disabled", "Enable the account before signing in");
        }
        if self.account_logins.iter().any(|login| login.account == *account && login.phase.active()) {
            return fail("account_login_in_progress", "A sign-in is already open for this account");
        }
        Ok(())
    }

    pub fn cancel_account_login_guard(&self, actor: &Actor, account: &AccountKey, attempt: &str) -> Result<()> {
        user(actor)?;
        if !self.account_logins.iter().any(|login| login.account == *account && login.attempt == attempt && matches!(login.phase, AccountLoginPhase::Starting | AccountLoginPhase::Pending)) {
            return fail("account_login_ended", "This sign-in is no longer open");
        }
        Ok(())
    }



}
