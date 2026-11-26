//! What the user changes of the provider accounts from Settings, judged on the accounts the
//! domain last recorded: the daemon writes the change to `options.toml` and records it here, so
//! every screen sees it before the next check.
use super::*;
use alondra_core::accounts::{AccountBilling, AccountChange, AccountId, AccountKey, AccountState, AccountStatus, Provider};

pub(crate) fn replace_account_configuration(domain: &mut DomainState, actor: &Actor, config: &alondra_core::accounts::AccountsConfig, expected_version: &str) -> Result<()> {
    user(actor)?;
    if expected_version.is_empty() || expected_version != domain.account_config_version { return fail("stale_config", "The accounts changed; refresh Settings before saving"); }
    config.check().map_err(|why| DomainError::new("invalid_input", why))?;
    if !config.any_enabled_globally() { return fail("invalid_input", "At least one account stays on"); }
    let ordered = config.ordered_accounts();
    let keys: BTreeSet<_> = ordered.iter().map(|(key, _)| key.clone()).collect();
    if let Some(task) = domain.tasks.values().find(|task| !task.over() && task.agent_id.is_some() && !keys.contains(&task.account_key())) {
        return fail("account_in_use", &format!("Open task {} still runs on {}; finish it before removing the account", task.id, task.account_key().wire_id()));
    }
    let statuses: Vec<_> = ordered.iter().map(|(key, account)| {
        let previous = domain.accounts.iter().find(|previous| previous.provider == key.provider && previous.id == key.account);
        let billing = previous.filter(|previous| previous.billing == AccountBilling::PayPerUse).map_or_else(|| account.billing.clone(), |_| AccountBilling::PayPerUse);
        AccountStatus { provider: key.provider, id: key.account.clone(), name: account.name.clone(), billing,
            state: if !account.enabled { AccountState::Disabled } else { previous.map_or(AccountState::Unchecked, |previous| if previous.state == AccountState::Disabled { AccountState::Unchecked } else { previous.state }) },
            native_identity: previous.and_then(|previous| previous.native_identity.clone()), email: previous.and_then(|previous| previous.email.clone()), plan: previous.and_then(|previous| previous.plan.clone()), sign_ins: previous.map_or(0, |previous| previous.sign_ins), profile: previous.and_then(|previous| previous.profile.clone()),
            antigravity: (key.provider == Provider::Antigravity).then(|| alondra_core::accounts::AntigravityStatus::of(&config.antigravity_settings(&key.account), previous.and_then(|previous| previous.antigravity.as_ref()).is_some_and(|status| status.key_set))) }
    }).collect();
    record_accounts(domain, &statuses);
    domain.account_order = ordered.into_iter().map(|(key, _)| key).collect();
    domain.account_config_version = alondra_core::accounts::config_version(config);
    domain.account_reserve = alondra_core::usage::Share::of(config.distribution.reserve);
    domain.account_policy = config.distribution.policy;
    domain.account_moves = config.distribution.move_when_exhausted;
    domain.model_catalogs.retain(|catalog| keys.contains(&catalog.account));
    Ok(())
}
/// The user set or cleared the key of a named Antigravity account: the daemon already wrote it
/// to the account's folder, and only that its credential changed is recorded. The account is
/// checked again on its next credential, so its models and usage of the old one go.
pub(crate) fn record_account_credential(domain: &mut DomainState, actor: &Actor, account: &AccountKey, set: bool) -> Result<()> {
    user(actor)?;
    if account.provider != Provider::Antigravity { return fail("invalid_input", "Only an Antigravity account keeps a key of Alondra's"); }
    if account.account.is_default() { return fail("invalid_input", "The default account is agy's own sign-in: add a named account for a key"); }
    let Some(status) = domain.accounts.iter_mut().find(|status| status.provider == account.provider && status.id == account.account) else {
        return fail("not_found", "The account is no longer listed");
    };
    let Some(antigravity) = status.antigravity.as_mut().filter(|antigravity| !set || antigravity.access.takes_key()) else {
        return fail("invalid_input", "This account signs in with Google and keeps no key");
    };
    antigravity.key_set = set;
    status.sign_ins += 1;
    if status.state != AccountState::Disabled { status.state = AccountState::Unchecked; }
    domain.model_catalogs.retain(|catalog| catalog.account != *account);
    domain.account_usage.retain(|usage| !(usage.provider == account.provider && usage.account == account.account));
    Ok(())
}
/// Makes `change` in the recorded accounts, as the next check will see it.
pub(crate) fn change_accounts(domain: &mut DomainState, provider: Provider, change: &AccountChange) {
    let at = |state: &DomainState, account: &AccountId| state.accounts.iter().position(|s| s.provider == provider && s.id == *account);
    match change {
        AccountChange::Rename { account, name } => {
            if let Some(at) = at(domain, account) { domain.accounts[at].name = name.trim().into(); }
        }
        AccountChange::SetPolicy { policy } => domain.account_policy = *policy,
        AccountChange::SetMoveWhenExhausted { on } => domain.account_moves = *on,
        AccountChange::Connect | AccountChange::Add { .. } => {}
        AccountChange::Enable { account } => {
            if let Some(at) = at(domain, account) {
                domain.accounts[at].state = AccountState::Unchecked;
            }
        }
        AccountChange::Disable { account } => {
            if let Some(at) = at(domain, account) {
                let status = &mut domain.accounts[at];
                status.state = AccountState::Disabled;
                status.email = None;
            }
        }
        AccountChange::Remove { account } => {
            domain.accounts.retain(|s| !(s.provider == provider && s.id == *account));
            domain.account_usage.retain(|u| !(u.provider == provider && u.account == *account));
            domain.account_order.retain(|key| !(key.provider == provider && key.account == *account));
            domain.model_catalogs.retain(|catalog| !(catalog.account.provider == provider && catalog.account.account == *account));
        }
        AccountChange::MoveUp { account } | AccountChange::MoveDown { account } => {
            let up = matches!(change, AccountChange::MoveUp { .. });
            if !domain.account_order.is_empty() {
                if let Some(from) = domain.account_order.iter().position(|key| key.provider == provider && key.account == *account) {
                    let to = if up { from.checked_sub(1) } else { Some(from + 1).filter(|to| *to < domain.account_order.len()) };
                    if let Some(to) = to { domain.account_order.swap(from, to); }
                }
                return;
            }
            let same: Vec<usize> = domain.accounts.iter().enumerate().filter(|(_, s)| s.provider == provider).map(|(i, _)| i).collect();
            if let Some(from) = same.iter().position(|i| domain.accounts[*i].id == *account) {
                let to = if up { from.checked_sub(1) } else { Some(from + 1).filter(|to| *to < same.len()) };
                if let Some(to) = to {
                    domain.accounts.swap(same[from], same[to]);
                }
            }
        }
        AccountChange::SetWeight { account, weight } => {
            if let Some(at) = at(domain, account)
                && let AccountBilling::Subscription { weight: set, .. } = &mut domain.accounts[at].billing
            {
                *set = *weight;
            }
        }
    }
}
