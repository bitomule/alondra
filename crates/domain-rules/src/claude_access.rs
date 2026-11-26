//! Whether Claude can work for the tasks that run through the Agent SDK. A turn a Claude API error
//! ended holds its task on a pause of its lifecycle, on a card of its own as every wait is, until
//! something outside the task changes: the user signs in to Claude again, the API answers, or the
//! usage limit resets. A sign-in and a usage limit belong to the provider account the task runs
//! on; the API is the same for every account. Waiting for You shows the cards of one hold (of one
//! account) as a single card: the oldest, and none while a probe runs. Only facts release them:
//! the model answering an SDK task, which lets go on the tasks its account's sign-in or limit
//! held and every task the API held, or the one held task the user's answer or the daemon's
//! schedule sent on as a probe.
use super::*;
use alondra_core::accounts::{AccountId, AccountKey, AccountState, Provider};
use std::collections::BTreeSet;

/// The daemon's schedule: the task of the oldest card of `hold` goes on, to probe whether
/// Claude works again; for a limit, of `account` when it names one. Its card is withdrawn:
/// nobody answered it.
pub(crate) fn probe_claude(domain: &mut DomainState, provider: Provider, hold: ClaudeHold, account: Option<&AccountId>, lifecycles: &mut Lifecycles) -> Result<Option<TaskId>, DomainError> {
    let held = domain.claude_held().into_iter().filter(|held| held.provider == provider && held.hold == hold && account.is_none_or(|account| held.account == *account));
    let Some(probe) = held.min_by_key(|held| domain.interactions.get(&held.card).map(|i| i.order)) else {
        return Ok(None);
    };
    domain.withdraw_hold(&probe.card);
    domain.resume_held(&probe, None, lifecycles)?;
    Ok(Some(probe.task))
}
/// The user answered a hold's card: its task goes on as the probe, told what they wrote on the
/// card as a message of theirs the node has yet to answer. False for any other card.
pub(crate) fn settle_claude_card(domain: &mut DomainState, card: &InteractionId, written: Option<&str>, lifecycles: &mut Lifecycles) -> Result<bool, DomainError> {
    if domain.claude_hold(card).is_none() {
        return Ok(false);
    }
    if domain.interactions.get(card).is_some_and(|card| !domain.current_hold_credentials(card)) {
        return Err(DomainError::new("account_credentials_changed", "This node belongs to an earlier sign-in and stays paused. Create a new node for the current account credentials."));
    }
    let written = written.map(str::trim).filter(|text| !text.is_empty());
    let Some(held) = domain.claude_held().into_iter().find(|held| held.card == *card) else {
        if written.is_some() {
            return Err(DomainError::new(CLOSING, "This task is closing, so what you wrote reaches no node. Send it as a new request."));
        }
        return Ok(true);
    };
    let input = domain.next_input(&held.task);
    domain.resume_held(&held, written, lifecycles)?;
    if written.is_some() {
        awaits_answer_to_user(domain, &held.task, input);
    }
    Ok(true)
}
/// The execution admitted for `task` on `to` leaves the usage limit its account holds it on
/// with `card`: the card is withdrawn and the task goes on with an input of its own, whose
/// words are the ones the new execution is launched with.
pub(crate) fn leave_limit(domain: &mut DomainState, task: &TaskId, card: &InteractionId, provider: Provider, to: &AccountId, lifecycles: &mut Lifecycles) -> Result<(), DomainError> {
    let Some(held) = domain.claude_held().into_iter().find(|held| held.task == *task && held.card == *card && held.hold == ClaudeHold::Usage) else {
        return Err(DomainError::new("conflict", "The task no longer waits on that usage limit"));
    };
    let target = AccountKey::new(provider,to.clone());
    if AccountKey::new(held.provider,held.account.clone()) == target {
        return Err(DomainError::new("invalid_input", "A task leaves its account's usage limit only for another account"));
    }
    if let Some(refusal) = refuses_work_off_a_hold(domain, task, &target) {
        return Err(refusal);
    }
    domain.withdraw_hold(&held.card);
    let record = LifecycleRecord::Resumed { card: held.card.clone(), input: domain.next_input(task), text: None };
    domain.decide(lifecycles, task, record)
}
/// A task that moves to `to` leaves the hold the account it leaves puts on it: that account's
/// usage limit or expired sign-in, or its provider's outage when it moves to another provider.
/// The card is withdrawn and the next node, on `to`, starts with what already waits for it, or
/// with an input of its own when nothing does. A hold `to` would share, or an account that
/// cannot take the work, keeps the task where it is.
pub(crate) fn leave_hold_for_the_move(domain: &mut DomainState, task: &TaskId, to: &AccountKey, lifecycles: &mut Lifecycles) -> Result<(), DomainError> {
    let Some(held) = domain.claude_held().into_iter().find(|held| held.task == *task) else { return Ok(()) };
    let from = AccountKey::new(held.provider, held.account.clone());
    let left = match held.hold {
        ClaudeHold::Outage => from.provider != to.provider,
        ClaudeHold::Usage | ClaudeHold::SignedOut => from != *to,
    };
    if !left || refuses_work_off_a_hold(domain, task, to).is_some() {
        return Ok(());
    }
    domain.withdraw_hold(&held.card);
    if domain.lifecycle(lifecycles, task)?.pending_inputs().is_empty() {
        return domain.resume_held(&held, None, lifecycles);
    }
    domain.decide(lifecycles, task, LifecycleRecord::PauseEnded { card: held.card.clone() })
}
pub(crate) fn refuses_work_off_a_hold(domain: &DomainState, task: &TaskId, target: &AccountKey) -> Option<DomainError> {
    let paid_granted = domain.tasks.get(task).and_then(|task| task.selection.as_ref()).is_some_and(|selection| selection.account.as_ref() == Some(target) && selection.paid_account_grant.as_ref() == Some(target));
    match domain.accounts.iter().find(|status| status.provider == target.provider && status.id == target.account) {
        Some(status) if matches!(status.state, AccountState::Disabled | AccountState::SignedOut) || (status.billing == alondra_core::accounts::AccountBilling::PayPerUse && !paid_granted) => {
            Some(DomainError::new("conflict", "That account cannot take the task's work now"))
        }
        None if target.provider != Provider::Claude || !target.account.is_default() || !domain.accounts.is_empty() => {
            Some(DomainError::new("not_found", "That account is not one of the accounts Alondra knows"))
        }
        _ => None,
    }
}
/// The accounts the daemon saw, as the domain keeps them: an account whose expired sign-in
/// holds a task stays signed out until Claude answers on it, whatever its sign-in check says,
/// and one seen signed in again after it was signed out counts its credential's next generation.
pub(crate) fn record_accounts(domain: &mut DomainState, seen: &[alondra_core::accounts::AccountStatus]) {
    let held: BTreeSet<AccountKey> = domain.claude_held().into_iter().filter(|held| held.hold == ClaudeHold::SignedOut).map(|held| AccountKey::new(held.provider,held.account)).collect();
    let before = std::mem::take(&mut domain.accounts);
    let mut replaced = BTreeSet::new();
    let mut credentials_changed = BTreeSet::new();
    domain.accounts = seen
        .iter()
        .map(|status| {
            let mut status = status.clone();
            let prior = before.iter().find(|prior| prior.provider == status.provider && prior.id == status.id);
            status.sign_ins = prior.map_or(0, |prior| prior.sign_ins);
            if status.email.is_none() {
                status.email = prior.and_then(|prior| prior.email.clone());
            }
            if status.native_identity.is_none() { status.native_identity = prior.and_then(|prior| prior.native_identity.clone()); }
            if status.profile.is_none() { status.profile = prior.and_then(|prior| prior.profile.clone()); }
            let was_out = prior.is_some_and(|prior| prior.state == AccountState::SignedOut);
            let key = AccountKey::new(status.provider, status.id.clone());
            let identity_changed = prior.is_some_and(|prior|
                matches!((&prior.email, &status.email), (Some(before), Some(now)) if !before.trim().is_empty() && !now.trim().is_empty() && !before.trim().eq_ignore_ascii_case(now.trim()))
                || matches!((&prior.native_identity, &status.native_identity), (Some(before), Some(now)) if before != now));
            let billing_changed = prior.is_some_and(|prior| std::mem::discriminant(&prior.billing) != std::mem::discriminant(&status.billing));
            if billing_changed { credentials_changed.insert(key.clone()); }
            if identity_changed {
                replaced.insert(key.clone());
                credentials_changed.insert(key.clone());
                status.sign_ins += 1;
            }
            let refusal_stands = prior.is_some_and(|prior| prior.state == AccountState::PolicyRefused)
                && !identity_changed && !billing_changed
                && matches!(status.state, AccountState::SignedIn | AccountState::Unchecked);
            if refusal_stands {
                status.state = AccountState::PolicyRefused;
            } else if status.state == AccountState::SignedIn && !identity_changed
                && prior.is_none_or(|prior| prior.state != AccountState::SignedIn) && held.contains(&key)
            {
                status.state = AccountState::SignedOut;
            } else if !identity_changed && status.state == AccountState::SignedIn && (was_out || billing_changed) {
                status.sign_ins += 1;
                credentials_changed.insert(key);
            }
            status
        })
        .collect();
    for card in domain.interactions.values_mut() {
        if card.author != Author::System || ClaudeHold::of(card).is_none() { continue; }
        let Scope::Task { task_id } = &card.scope else { continue };
        let Some(task) = domain.tasks.get(task_id) else { continue };
        let key = task.account_key();
        let prior_generation = before.iter().find(|account| account.provider == key.provider && account.id == key.account).map_or(1, |account| account.sign_ins + 1);
        if replaced.contains(&key) && card.card.cause.is_none() {
            card.card.cause = Some(credential_cause(&key, prior_generation));
            card.revision = domain.revision + 1;
        } else if credentials_changed.contains(&key) && !replaced.contains(&key)
            && card.card.cause.as_ref() == Some(&credential_cause(&key, prior_generation))
        {
            let generation = domain.accounts.iter().find(|account| account.provider == key.provider && account.id == key.account).map_or(1, |account| account.sign_ins + 1);
            card.card.cause = Some(credential_cause(&key, generation));
            card.revision = domain.revision + 1;
        }
    }
    domain.model_catalogs.retain(|catalog| !credentials_changed.contains(&catalog.account));
    domain.account_usage.retain(|usage| !replaced.contains(&AccountKey::new(usage.provider, usage.account.clone())));
}

#[cfg(test)]
mod credential_identity_tests {
    use super::*;
    use crate::Apply;
    use alondra_core::accounts::{AccountBilling, AccountId, AccountStatus, Provider};

    #[test]
    fn known_account_identity_or_billing_kind_changes_require_a_new_credential_generation() {
        for provider in [Provider::Claude, Provider::Codex] {
            let mut state = DomainState::new("epoch");
            let mut seen = AccountStatus { antigravity: None, provider, id: AccountId::default_account(), name: "Personal".into(), state: AccountState::SignedIn,
                native_identity: None, email: None, plan: None, billing: AccountBilling::default(), sign_ins: 999, profile: None };
            record_accounts(&mut state, &[seen.clone()]); assert_eq!(state.accounts[0].sign_ins,0);
            seen.email = Some("first@example.invalid".into());
            record_accounts(&mut state, &[seen.clone()]); assert_eq!(state.accounts[0].sign_ins,0,"discovering an identity is not proof it changed");
            seen.plan = Some("max".into()); seen.billing = AccountBilling::Subscription { plan: Some("max".into()), weight: Some(2) };
            record_accounts(&mut state, &[seen.clone()]); assert_eq!(state.accounts[0].sign_ins,0,"plan and distribution weight do not replace credentials");
            seen.email = Some("second@example.invalid".into());
            record_accounts(&mut state, &[seen.clone()]); assert_eq!(state.accounts[0].sign_ins,1);
            record_accounts(&mut state, &[seen.clone()]); assert_eq!(state.accounts[0].sign_ins,1,"identical probes are idempotent");
            seen.billing = AccountBilling::PayPerUse;
            record_accounts(&mut state, &[seen.clone()]); assert_eq!(state.accounts[0].sign_ins,2);
            seen.email = None;
            record_accounts(&mut state, &[seen.clone()]); assert_eq!(state.accounts[0].sign_ins,2,"an absent identity is unknown");
            seen.state = AccountState::SignedOut; record_accounts(&mut state, &[seen.clone()]);
            seen.state = AccountState::SignedIn; record_accounts(&mut state, &[seen]); assert_eq!(state.accounts[0].sign_ins,3,"existing reauthentication rule remains");
        }
    }

    fn account(provider: Provider, email: &str) -> AccountStatus {
        AccountStatus { antigravity: None, provider, id: AccountId::default_account(), name: "Personal".into(), state: AccountState::SignedIn,
            native_identity: None, email: Some(email.into()), plan: None, billing: AccountBilling::default(), sign_ins: 0, profile: None }
    }

    fn cache(state: &mut DomainState, key: &AccountKey) {
        use alondra_core::model_selection::{AccountModelCatalog, CatalogModel};
        use alondra_core::usage::{AccountUsage, Share, UsageWindow, WindowKind, WindowReading};
        state.model_catalogs.push(AccountModelCatalog { account: key.clone(), revision: "native-catalog".into(), measured_at: 1,
            native_default: Some("native-model".into()), models: vec![CatalogModel { model: "native-model".into(), display_name: "Native model".into(),
                reasoning_efforts: vec![], default_reasoning_effort: None, context_window: None, input_modalities: vec![], is_default: true, hidden: false }] });
        state.account_usage.push(AccountUsage { provider: key.provider, account: key.account.clone(), plan: None, day_start: None,
            windows: vec![UsageWindow { measured_at: 1, reading: WindowReading { kind: WindowKind::Session, used: Share::NONE, exhausted: true, resets_at: None } }] });
    }

    #[test]
    fn a_known_identity_replacement_drops_its_usage_but_billing_kind_alone_preserves_it() {
        for provider in [Provider::Claude, Provider::Codex] {
            for billing_changed in [false, true] {
                let other = if provider == Provider::Claude { Provider::Codex } else { Provider::Claude };
                let key = AccountKey::native(provider);
                let other_key = AccountKey::native(other);
                let mut state = DomainState::new("epoch");
                let mut seen = vec![account(provider, "first@example.invalid"), account(other, "other@example.invalid")];
                record_accounts(&mut state, &seen);
                cache(&mut state, &key); cache(&mut state, &other_key);
                if billing_changed { seen[0].billing = AccountBilling::PayPerUse; }
                else { seen[0].email = Some("second@example.invalid".into()); }
                record_accounts(&mut state, &seen);
                assert_eq!(state.accounts[0].sign_ins, 1);
                assert_eq!(state.model_catalogs.iter().map(|catalog| &catalog.account).collect::<Vec<_>>(), [&other_key]);
                let keys = state.account_usage.iter().map(|usage| AccountKey::new(usage.provider, usage.account.clone())).collect::<Vec<_>>();
                if billing_changed { assert_eq!(keys, [key, other_key], "billing kind alone does not prove the exhausted identity changed"); }
                else { assert_eq!(keys, [other_key]); }
            }
        }
    }

    #[test]
    fn known_identity_changes_invalidate_disabled_accounts_without_enabling_new_work() {
        for workspace_changed in [false, true] {
            let key = AccountKey::native(Provider::Codex);
            let other = AccountKey::native(Provider::Claude);
            let mut state = DomainState::new("epoch");
            let mut seen = vec![account(Provider::Codex, "first@example.invalid"), account(Provider::Claude, "other@example.invalid")];
            seen[0].native_identity = Some("first-workspace".into());
            seen[0].state = AccountState::Disabled;
            record_accounts(&mut state, &seen); cache(&mut state, &key); cache(&mut state, &other);
            if workspace_changed { seen[0].native_identity = Some("second-workspace".into()); }
            else { seen[0].email = Some("second@example.invalid".into()); }
            record_accounts(&mut state, &seen);
            assert_eq!(state.accounts[0].state, AccountState::Disabled);
            assert_eq!(state.accounts[0].sign_ins, 1);
            assert_eq!(state.model_catalogs.iter().map(|catalog| &catalog.account).collect::<Vec<_>>(), [&other]);
            assert_eq!(state.account_usage.iter().map(|usage| AccountKey::new(usage.provider, usage.account.clone())).collect::<Vec<_>>(), [other]);
            let known = state.accounts[0].clone();
            seen[0].email = None; seen[0].native_identity = None;
            record_accounts(&mut state, &seen);
            assert_eq!(state.accounts[0], known, "unknown partial metadata cannot undo a known identity replacement");
        }
    }

    #[test]
    fn workspace_discovery_and_email_case_changes_do_not_claim_a_replacement() {
        let key = AccountKey::native(Provider::Codex);
        let mut state = DomainState::new("epoch");
        let mut seen = account(Provider::Codex, "first@example.invalid");
        record_accounts(&mut state, &[seen.clone()]); cache(&mut state, &key);
        let usage = state.account_usage.clone(); let catalogs = state.model_catalogs.clone();
        seen.native_identity = Some("actual-native-workspace".into());
        seen.email = Some(" FIRST@EXAMPLE.INVALID ".into());
        record_accounts(&mut state, &[seen.clone()]);
        assert_eq!(state.accounts[0].sign_ins, 0);
        assert_eq!(state.account_usage, usage); assert_eq!(state.model_catalogs, catalogs);
        seen.native_identity = None; seen.email = None;
        record_accounts(&mut state, &[seen]);
        assert_eq!(state.accounts[0].native_identity.as_deref(), Some("actual-native-workspace"));
        assert_eq!(state.accounts[0].sign_ins, 0);
    }

    #[test]
    fn reauthentication_invalidates_models_without_claiming_fresh_quota_capacity() {
        for provider in [Provider::Claude, Provider::Codex] {
            let key = AccountKey::native(provider);
            let mut state = DomainState::new("epoch");
            let mut seen = account(provider, "same@example.invalid");
            record_accounts(&mut state, &[seen.clone()]); cache(&mut state, &key);
            let usage = state.account_usage.clone();
            seen.plan = Some("new-plan".into());
            record_accounts(&mut state, &[seen.clone()]);
            assert_eq!(state.model_catalogs.len(), 1, "plan metadata alone does not replace credentials");
            seen.state = AccountState::SignedOut;
            record_accounts(&mut state, &[seen.clone()]);
            seen.state = AccountState::SignedIn;
            record_accounts(&mut state, &[seen.clone()]);
            assert!(state.model_catalogs.is_empty());
            assert_eq!(state.account_usage, usage, "sign-in alone does not clear the old limit marker");
            cache(&mut state, &key); state.account_usage = usage.clone();
            seen.state = AccountState::SignedOut; record_accounts(&mut state, &[seen]);
            state.claude_answered(&"fixture-source".into(), &key, &mut Lifecycles::new()).unwrap();
            assert!(state.model_catalogs.is_empty(), "a successful sign-in probe also changes credential generation");
            assert_eq!(state.account_usage, usage);
        }
    }


    #[test]
    fn partial_metadata_keeps_the_last_known_identity_for_replacement_detection() {
        for provider in [Provider::Claude, Provider::Codex] {
            let mut state = DomainState::new("epoch");
            let mut seen = account(provider, "first@example.invalid");
            let key = AccountKey::native(provider);
            record_accounts(&mut state, &[seen.clone()]); cache(&mut state, &key);
            seen.state = AccountState::Unavailable; seen.email = None;
            record_accounts(&mut state, &[seen.clone()]);
            assert_eq!(state.accounts[0].email.as_deref(), Some("first@example.invalid"));
            assert_eq!(state.accounts[0].state, AccountState::Unavailable);
            assert_eq!(state.accounts[0].sign_ins, 0);
            seen.state = AccountState::SignedIn; seen.email = Some("second@example.invalid".into());
            record_accounts(&mut state, &[seen]);
            assert_eq!(state.accounts[0].sign_ins, 1);
            assert!(state.account_usage.is_empty()); assert!(state.model_catalogs.is_empty());
        }
    }


    #[test]
    fn user_paid_policy_on_the_same_identity_cannot_erase_an_exhausted_native_marker() {
        for provider in [Provider::Claude, Provider::Codex] {
            let mut state = DomainState::new("epoch");
            let key = AccountKey::native(provider);
            let mut config = alondra_core::accounts::AccountsConfig::fresh(); config.connect(provider);
            record_accounts(&mut state, &[account(provider, "same@example.invalid")]);
            state.account_config_version = alondra_core::accounts::config_version(&config);
            cache(&mut state, &key);
            if provider == Provider::Codex {
                state.account_usage[0].windows[0].reading.kind = alondra_core::usage::WindowKind::Other("codex:account:limit".into());
            }
            let quota = state.account_usage.clone();
            let mut same = state.accounts.clone(); same[0].email = Some(" SAME@example.invalid ".into());
            record_accounts(&mut state, &same);
            assert_eq!(state.account_usage, quota, "email formatting alone does not prove a different native identity");
            assert_eq!(state.accounts[0].sign_ins, 0);
            let paid = alondra_core::accounts::ProviderAccount { billing: AccountBilling::PayPerUse, ..alondra_core::accounts::ProviderAccount::implicit_default() };
            match provider { Provider::Claude => config.claude = vec![paid], Provider::Codex => config.codex = vec![paid], Provider::Antigravity => config.antigravity = vec![alondra_core::accounts::AntigravityAccount::new(paid, alondra_core::antigravity_account::Settings::default())] }
            let expected = state.account_config_version.clone();
            state.execute(&Actor::User { user_id: "user".into() }, &CommandEnvelope {
                protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "explicit-paid-policy".into(), expected_revision: state.revision,
                command: Command::ReplaceAccountConfiguration { accounts: config.to_toml(), expected_config_version: expected },
            }).unwrap();
            assert_eq!(state.accounts[0].billing, AccountBilling::PayPerUse);
            assert_eq!(state.accounts[0].email.as_deref(), Some(" SAME@example.invalid "));
            assert_eq!(state.account_usage, quota, "a user spending policy edit is not native quota recovery evidence");
            assert!(state.model_catalogs.is_empty());
        }
    }

}
