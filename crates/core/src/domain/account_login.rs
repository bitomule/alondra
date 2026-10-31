use super::*;
use crate::accounts::{AccountKey, Provider};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountLoginPhase { Starting, Pending, Publishing, Connected, Cancelled, Failed }

impl AccountLoginPhase {
    pub fn active(self) -> bool { matches!(self, Self::Starting | Self::Pending | Self::Publishing) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(state: &mut DomainState, actor: Actor, command: Command) -> Result<CommandOutcome> {
        state.execute(&actor, &CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: format!("login-test-{}", state.revision).into(), expected_revision: state.revision, command })
    }
    fn user_actor() -> Actor { Actor::User { user_id: "fixture-user".into() } }

    #[test]
    fn codex_login_cancel_binds_attempt_and_late_completion_cannot_reconnect_it() {
        let mut state = DomainState::new("fixture");
        let account = AccountKey::native(Provider::Codex);
        apply(&mut state, user_actor(), Command::BeginAccountLogin { account: account.clone(), attempt: "first".into(), started_at: 10 }).unwrap();
        assert!(!state.account_login_action(account.clone(), None).enabled);
        assert!(!state.account_login_action(account.clone(), Some("stale")).enabled);
        assert!(state.account_login_action(account.clone(), Some("first")).enabled);
        let mut completion = state.account_logins[0].clone(); completion.phase = AccountLoginPhase::Connected;
        apply(&mut state, user_actor(), Command::CancelAccountLogin { account: account.clone(), attempt: "first".into() }).unwrap();
        assert!(apply(&mut state, Actor::System, Command::RecordAccountLogin { login: completion }).is_err());
        apply(&mut state, user_actor(), Command::BeginAccountLogin { account: account.clone(), attempt: "second".into(), started_at: 20 }).unwrap();
        assert!(apply(&mut state, user_actor(), Command::CancelAccountLogin { account, attempt: "first".into() }).is_err());
        assert_eq!(state.account_logins[0].attempt, "second");
        assert_eq!(state.account_logins[0].phase, AccountLoginPhase::Starting);
        crate::domain::assert_index_matches(&state);
    }

    #[test]
    fn codex_login_cannot_cancel_publication_after_its_commit_gate() {
        let mut state = DomainState::new("fixture"); let account = AccountKey::native(Provider::Codex);
        apply(&mut state, user_actor(), Command::BeginAccountLogin { account: account.clone(), attempt: "attempt".into(), started_at: 1 }).unwrap();
        let mut publishing = state.account_logins[0].clone(); publishing.phase = AccountLoginPhase::Publishing;
        apply(&mut state, Actor::System, Command::RecordAccountLogin { login: publishing }).unwrap();
        assert!(!state.account_login_action(account.clone(), Some("attempt")).enabled);
        assert!(apply(&mut state, user_actor(), Command::CancelAccountLogin { account, attempt: "attempt".into() }).is_err());
        let mut connected = state.account_logins[0].clone(); connected.phase = AccountLoginPhase::Connected;
        apply(&mut state, Actor::System, Command::RecordAccountLogin { login: connected }).unwrap();
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccountLogin {
    pub account: AccountKey,
    pub attempt: String,
    pub started_at: Millis,
    pub phase: AccountLoginPhase,
    pub challenge: Option<crate::codex_login::Challenge>,
    pub error: Option<String>,
}

impl DomainState {
    pub(super) fn begin_account_login_guard(&self, actor: &Actor, account: &AccountKey, attempt: &str) -> Result<()> {
        user(actor)?;
        id(attempt)?;
        if account.provider != Provider::Codex { return fail("invalid_input", "Choose a Codex account"); }
        if !account.account.is_default() && !self.accounts.iter().any(|known| known.provider == account.provider && known.id == account.account) {
            return fail("not_found", "The account is no longer listed");
        }
        if self.accounts.iter().any(|known| known.provider == account.provider && known.id == account.account && known.state == crate::accounts::AccountState::Disabled) {
            return fail("account_disabled", "Enable the account before signing in");
        }
        if self.account_logins.iter().any(|login| login.account == *account && login.phase.active()) {
            return fail("account_login_in_progress", "A sign-in is already open for this account");
        }
        Ok(())
    }

    pub(super) fn cancel_account_login_guard(&self, actor: &Actor, account: &AccountKey, attempt: &str) -> Result<()> {
        user(actor)?;
        if !self.account_logins.iter().any(|login| login.account == *account && login.attempt == attempt && matches!(login.phase, AccountLoginPhase::Starting | AccountLoginPhase::Pending)) {
            return fail("account_login_ended", "This sign-in is no longer open");
        }
        Ok(())
    }

    pub(super) fn begin_account_login(&mut self, actor: &Actor, account: &AccountKey, attempt: &str, started_at: Millis) -> Result<()> {
        self.begin_account_login_guard(actor, account, attempt)?;
        self.account_logins.retain(|login| login.account != *account);
        self.account_logins.push(AccountLogin { account: account.clone(), attempt: attempt.into(), started_at,
            phase: AccountLoginPhase::Starting, challenge: None, error: None });
        Ok(())
    }

    pub(super) fn cancel_account_login(&mut self, actor: &Actor, account: &AccountKey, attempt: &str) -> Result<()> {
        self.cancel_account_login_guard(actor, account, attempt)?;
        let login = self.account_logins.iter_mut().find(|login| login.account == *account).expect("guarded");
        login.phase = AccountLoginPhase::Cancelled;
        login.challenge = None;
        Ok(())
    }

    pub(super) fn record_account_login(&mut self, actor: &Actor, login: &AccountLogin) -> Result<()> {
        system(actor)?;
        let Some(current) = self.account_logins.iter_mut().find(|current| current.account == login.account && current.attempt == login.attempt && current.phase.active()) else {
            return fail("account_login_ended", "This sign-in is no longer open");
        };
        if login.started_at != current.started_at || login.phase == AccountLoginPhase::Starting {
            return fail("invalid_input", "The sign-in identity cannot change");
        }
        if (login.phase == AccountLoginPhase::Pending) != login.challenge.is_some() {
            return fail("invalid_input", "Only a pending sign-in has a verification code");
        }
        *current = login.clone();
        Ok(())
    }
}
