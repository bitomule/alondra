use super::*;
use alondra_core::accounts::AccountKey;

pub(crate) fn begin_account_login(domain: &mut DomainState, actor: &Actor, account: &AccountKey, attempt: &str, started_at: Millis) -> Result<()> {
    domain.begin_account_login_guard(actor, account, attempt)?;
    domain.account_logins.retain(|login| login.account != *account);
    domain.account_logins.push(AccountLogin { account: account.clone(), attempt: attempt.into(), started_at,
        phase: AccountLoginPhase::Starting, challenge: None, error: None });
    Ok(())
}
pub(crate) fn cancel_account_login(domain: &mut DomainState, actor: &Actor, account: &AccountKey, attempt: &str) -> Result<()> {
    domain.cancel_account_login_guard(actor, account, attempt)?;
    let login = domain.account_logins.iter_mut().find(|login| login.account == *account).expect("guarded");
    login.phase = AccountLoginPhase::Cancelled;
    login.challenge = None;
    Ok(())
}
pub(crate) fn record_account_login(domain: &mut DomainState, actor: &Actor, login: &AccountLogin) -> Result<()> {
    system(actor)?;
    let Some(current) = domain.account_logins.iter_mut().find(|current| current.account == login.account && current.attempt == login.attempt && current.phase.active()) else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Apply;
    use alondra_core::accounts::Provider;

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
        alondra_domain_model::assert_index_matches(&state);
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
