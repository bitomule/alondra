//! What the user changes of the provider accounts from Settings, judged on the accounts the
//! domain last recorded: the daemon writes the change to `options.toml` and records it here, so
//! every screen sees it before the next check.
use super::*;
use alondra_core::accounts::{AccountBilling, AccountChange, AccountId, AccountKey, AccountState, AccountStatus, Provider, WEIGHT_LIMIT};

impl DomainState {

    pub fn model_profile_guard(&self, actor: &Actor, provider: Provider, profile: alondra_core::model_selection::ModelProfile, policy: &alondra_core::model_selection::RoleModelPolicy, expected_version: &str) -> Result<()> {
        user(actor)?;
        if expected_version.is_empty() || expected_version != self.model_profiles_version { return fail("stale_config", "The model profiles changed; refresh Settings before saving"); }
        if !self.accounts.iter().any(|account| account.provider == provider) { return fail("not_found", "Connect an account of this provider before choosing its models"); }
        let mut profiles = self.model_profiles.clone();
        profiles.entry(provider).or_default().insert(profile, policy.clone());
        alondra_core::model_selection::validate_profiles(&profiles).map_err(|why| DomainError::new("invalid_input", why))?;
        if let alondra_core::model_selection::RoleModelPolicy::OrderedCandidates { candidates } = policy {
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

    pub fn validate_account_order(&self, order: &[AccountKey]) -> Result<()> {
        let known: BTreeSet<_> = self.accounts.iter().map(|status| AccountKey::new(status.provider, status.id.clone())).collect();
        let proposed: BTreeSet<_> = order.iter().cloned().collect();
        if order.len() != proposed.len() || proposed != known {
            return fail("invalid_input", "The order must contain every configured account exactly once");
        }
        Ok(())
    }

    pub fn account_order_guard(&self, actor: &Actor, order: &[AccountKey], expected_version: &str) -> Result<()> {
        user(actor)?;
        if expected_version.is_empty() || expected_version != self.account_config_version {
            return fail("stale_config", "The accounts changed; refresh Settings before reordering them");
        }
        self.validate_account_order(order)
    }

    pub fn accounts_guard(&self, actor: &Actor, provider: Provider, change: &AccountChange) -> Result<()> {
        user(actor)?;
        let Some(account) = change.account() else {
            return match change {
                AccountChange::SetPolicy { policy } if *policy == self.account_policy => fail("unchanged", "The accounts are already spread this way"),
                AccountChange::SetMoveWhenExhausted { on } if *on == self.account_moves => fail("unchanged", "Moving nodes is already so"),
                AccountChange::Connect if self.accounts.iter().any(|status| status.provider == provider) => fail("unchanged", "This provider is already connected"),
                AccountChange::Add { .. } if !self.accounts.iter().any(|status| status.provider == provider) => fail("not_found", "Connect this provider before adding an account"),
                AccountChange::Add { antigravity: Some(_), .. } if provider != Provider::Antigravity => fail("invalid_input", "Only an Antigravity account says how it reaches Google"),
                AccountChange::Add { antigravity: Some(settings), .. } if settings.check().is_err() => fail("invalid_input", &settings.check().unwrap_err()),
                AccountChange::Add { name, .. } if name.trim().is_empty() || name.trim().chars().count() > 60 || name.chars().any(char::is_control) => fail("invalid_input", "An account name has 1 to 60 characters"),
                AccountChange::Add { name, .. } if self.accounts.iter().any(|other| other.provider == provider && other.name.trim().eq_ignore_ascii_case(name.trim())) => fail("invalid_input", "Another account already has this name"),
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
            AccountChange::Disable { .. } if provider != Provider::Antigravity && !self.accounts.iter().any(|other| (other.provider != provider || other.id != *account)
                && other.provider != Provider::Antigravity && other.state != AccountState::Disabled) => {
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


}

