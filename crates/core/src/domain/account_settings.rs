//! What the user changes of the provider accounts from Settings, judged on the accounts the
//! domain last recorded: the daemon writes the change to `options.toml` and records it here, so
//! every screen sees it before the next check.
use super::*;
use crate::accounts::{AccountBilling, AccountChange, AccountId, AccountKey, AccountState, AccountStatus, Provider, WEIGHT_LIMIT};

impl DomainState {
    pub(super) fn replace_account_configuration(&mut self, actor: &Actor, config: &crate::accounts::AccountsConfig, expected_version: &str) -> Result<()> {
        user(actor)?;
        if expected_version.is_empty() || expected_version != self.account_config_version { return fail("stale_config", "The accounts changed; refresh Settings before saving"); }
        config.check().map_err(|why| DomainError::new("invalid_input", why))?;
        if !config.any_enabled_globally() { return fail("invalid_input", "At least one account stays on"); }
        let ordered = config.ordered_accounts();
        let keys: BTreeSet<_> = ordered.iter().map(|(key, _)| key.clone()).collect();
        if let Some(task) = self.tasks.values().find(|task| !task.over() && task.agent_id.is_some() && !keys.contains(&task.account_key())) {
            return fail("account_in_use", &format!("Open task {} still runs on {}; finish it before removing the account", task.id, task.account_key().wire_id()));
        }
        let statuses: Vec<_> = ordered.iter().map(|(key, account)| {
            let previous = self.accounts.iter().find(|previous| previous.provider == key.provider && previous.id == key.account);
            let billing = previous.filter(|previous| previous.billing == AccountBilling::PayPerUse).map_or_else(|| account.billing.clone(), |_| AccountBilling::PayPerUse);
            AccountStatus { provider: key.provider, id: key.account.clone(), name: account.name.clone(), billing,
                state: if !account.enabled { AccountState::Disabled } else { previous.map_or(AccountState::Unchecked, |previous| if previous.state == AccountState::Disabled { AccountState::Unchecked } else { previous.state }) },
                native_identity: previous.and_then(|previous| previous.native_identity.clone()), email: previous.and_then(|previous| previous.email.clone()), plan: previous.and_then(|previous| previous.plan.clone()), sign_ins: previous.map_or(0, |previous| previous.sign_ins) }
        }).collect();
        self.record_accounts(&statuses);
        self.account_order = ordered.into_iter().map(|(key, _)| key).collect();
        self.account_config_version = crate::accounts::config_version(config);
        self.account_reserve = crate::usage::Share::of(config.distribution.reserve);
        self.account_policy = config.distribution.policy;
        self.account_moves = config.distribution.move_when_exhausted;
        self.model_catalogs.retain(|catalog| keys.contains(&catalog.account));
        Ok(())
    }

    pub(super) fn model_profile_guard(&self, actor: &Actor, provider: Provider, profile: crate::model_selection::ModelProfile, policy: &crate::model_selection::RoleModelPolicy, expected_version: &str) -> Result<()> {
        user(actor)?;
        if expected_version.is_empty() || expected_version != self.model_profiles_version { return fail("stale_config", "The model profiles changed; refresh Settings before saving"); }
        if !self.accounts.iter().any(|account| account.provider == provider) { return fail("not_found", "Connect an account of this provider before choosing its models"); }
        let mut profiles = self.model_profiles.clone();
        profiles.entry(provider).or_default().insert(profile, policy.clone());
        crate::model_selection::validate_profiles(&profiles).map_err(|why| DomainError::new("invalid_input", why))?;
        if let crate::model_selection::RoleModelPolicy::OrderedCandidates { candidates } = policy {
            for candidate in candidates {
                if !self.model_catalogs.iter().filter(|catalog| catalog.account.provider == provider).any(|catalog| catalog.resolve(candidate).is_ok()) {
                    return fail("invalid_input", "Refresh the provider's models and choose an advertised model and reasoning effort");
                }
            }
        }
        Ok(())
    }
    /// The open tasks whose executions run on `account`; the provider's own sign-in runs those
    /// that name none.
    pub fn tasks_on_account(&self, account: &AccountId) -> usize {
        self.tasks_on_account_key(&AccountKey::new(Provider::Claude, account.clone()))
    }

    pub fn tasks_on_account_key(&self, account: &AccountKey) -> usize {
        self.tasks.values().filter(|task| !task.over() && task.agent_id.is_some())
            .filter(|task| task.account_key() == *account)
            .count()
    }

    pub(super) fn validate_account_order(&self, order: &[AccountKey]) -> Result<()> {
        let known: BTreeSet<_> = self.accounts.iter().map(|status| AccountKey::new(status.provider, status.id.clone())).collect();
        let proposed: BTreeSet<_> = order.iter().cloned().collect();
        if order.len() != proposed.len() || proposed != known {
            return fail("invalid_input", "The order must contain every configured account exactly once");
        }
        Ok(())
    }

    pub(super) fn account_order_guard(&self, actor: &Actor, order: &[AccountKey], expected_version: &str) -> Result<()> {
        user(actor)?;
        if expected_version.is_empty() || expected_version != self.account_config_version {
            return fail("stale_config", "The accounts changed; refresh Settings before reordering them");
        }
        self.validate_account_order(order)
    }

    pub(super) fn accounts_guard(&self, actor: &Actor, provider: Provider, change: &AccountChange) -> Result<()> {
        user(actor)?;
        let Some(account) = change.account() else {
            return match change {
                AccountChange::SetPolicy { policy } if *policy == self.account_policy => fail("unchanged", "The accounts are already spread this way"),
                AccountChange::SetMoveWhenExhausted { on } if *on == self.account_moves => fail("unchanged", "Moving nodes is already so"),
                _ => Ok(()),
            };
        };
        let listed: Vec<&AccountStatus> = self.accounts.iter().filter(|status| status.provider == provider).collect();
        let Some(at) = listed.iter().position(|status| status.id == *account) else {
            return fail("not_found", "The account is no longer listed");
        };
        let status = listed[at];
        let name = || status.name.clone();
        let key = AccountKey::new(provider, account.clone());
        let position = self.account_order.iter().position(|known| *known == key).unwrap_or(at);
        let count = if self.account_order.is_empty() { listed.len() } else { self.account_order.len() };
        match change {
            AccountChange::Rename { name, .. } if name.trim() == status.name => fail("unchanged", "The account already has this name"),
            AccountChange::Rename { name, .. } if name.trim().is_empty() || name.trim().chars().count() > 60 || name.chars().any(char::is_control) => fail("invalid_input", "An account name has 1 to 60 characters"),
            AccountChange::Rename { name, .. } if listed.iter().any(|other| other.id != *account && other.name.trim().eq_ignore_ascii_case(name.trim())) => fail("invalid_input", "Another account already has this name"),
            AccountChange::Enable { .. } if status.state != AccountState::Disabled => fail("unchanged", "The account is already on"),
            AccountChange::Remove { .. } if account.is_default() => {
                fail("invalid_input", "The provider's own sign-in on this Mac stays").map_err(|e| e.because(Refusal::DefaultAccountStays { name: name() }))
            }
            AccountChange::Disable { .. } if status.state == AccountState::Disabled => fail("unchanged", "The account is already off"),
            AccountChange::Disable { .. } if !self.accounts.iter().any(|other| (other.provider != provider || other.id != *account) && other.state != AccountState::Disabled) => {
                fail("invalid_input", "One account stays on").map_err(|e| e.because(Refusal::LastAccountOn { name: name() }))
            }
            AccountChange::Remove { .. } if self.tasks_on_account_key(&key) > 0 => {
                fail("blocked", "Open tasks still run on the account").map_err(|e| e.because(Refusal::AccountInUse { name: name() }))
            }
            AccountChange::MoveUp { .. } if position == 0 => fail("unchanged", "The account is already first"),
            AccountChange::MoveDown { .. } if position + 1 == count => fail("unchanged", "The account is already last"),
            AccountChange::SetWeight { weight, .. } => match &status.billing {
                AccountBilling::PayPerUse => fail("invalid_input", "An account paid per use has no weight"),
                _ if weight.is_some_and(|weight| weight == 0 || weight > WEIGHT_LIMIT) => {
                    fail("invalid_input", &format!("An account's weight is from 1 to {WEIGHT_LIMIT}"))
                }
                AccountBilling::Subscription { weight: set, .. } if set == weight => fail("unchanged", "The account already has this weight"),
                AccountBilling::Subscription { .. } => Ok(()),
            },
            _ => Ok(()),
        }
    }

    /// Makes `change` in the recorded accounts, as the next check will see it.
    pub(super) fn change_accounts(&mut self, provider: Provider, change: &AccountChange) {
        let at = |state: &Self, account: &AccountId| state.accounts.iter().position(|s| s.provider == provider && s.id == *account);
        match change {
            AccountChange::Rename { account, name } => {
                if let Some(at) = at(self, account) { self.accounts[at].name = name.trim().into(); }
            }
            AccountChange::SetPolicy { policy } => self.account_policy = *policy,
            AccountChange::SetMoveWhenExhausted { on } => self.account_moves = *on,
            AccountChange::Enable { account } => {
                if let Some(at) = at(self, account) {
                    self.accounts[at].state = AccountState::Unchecked;
                }
            }
            AccountChange::Disable { account } => {
                if let Some(at) = at(self, account) {
                    let status = &mut self.accounts[at];
                    status.state = AccountState::Disabled;
                    status.email = None;
                }
            }
            AccountChange::Remove { account } => {
                self.accounts.retain(|s| !(s.provider == provider && s.id == *account));
                self.account_usage.retain(|u| !(u.provider == provider && u.account == *account));
                self.account_order.retain(|key| !(key.provider == provider && key.account == *account));
                self.model_catalogs.retain(|catalog| !(catalog.account.provider == provider && catalog.account.account == *account));
            }
            AccountChange::MoveUp { account } | AccountChange::MoveDown { account } => {
                let up = matches!(change, AccountChange::MoveUp { .. });
                if !self.account_order.is_empty() {
                    if let Some(from) = self.account_order.iter().position(|key| key.provider == provider && key.account == *account) {
                        let to = if up { from.checked_sub(1) } else { Some(from + 1).filter(|to| *to < self.account_order.len()) };
                        if let Some(to) = to { self.account_order.swap(from, to); }
                    }
                    return;
                }
                let same: Vec<usize> = self.accounts.iter().enumerate().filter(|(_, s)| s.provider == provider).map(|(i, _)| i).collect();
                if let Some(from) = same.iter().position(|i| self.accounts[*i].id == *account) {
                    let to = if up { from.checked_sub(1) } else { Some(from + 1).filter(|to| *to < same.len()) };
                    if let Some(to) = to {
                        self.accounts.swap(same[from], same[to]);
                    }
                }
            }
            AccountChange::SetWeight { account, weight } => {
                if let Some(at) = at(self, account)
                    && let AccountBilling::Subscription { weight: set, .. } = &mut self.accounts[at].billing
                {
                    *set = *weight;
                }
            }
        }
    }
}
