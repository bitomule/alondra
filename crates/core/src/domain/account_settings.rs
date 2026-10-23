//! What the user changes of the provider accounts from Settings, judged on the accounts the
//! domain last recorded: the daemon writes the change to `options.toml` and records it here, so
//! every screen sees it before the next check.
use super::*;
use crate::accounts::{AccountBilling, AccountChange, AccountId, AccountState, AccountStatus, Provider, WEIGHT_LIMIT};

impl DomainState {
    /// The open tasks whose executions run on `account`; the provider's own sign-in runs those
    /// that name none.
    pub fn tasks_on_account(&self, account: &AccountId) -> usize {
        self.tasks.values().filter(|task| !task.over() && task.agent_id.is_some())
            .filter(|task| task.account.as_ref().map_or(account.is_default(), |on| on == account))
            .count()
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
        let enabled = |status: &&AccountStatus| status.state != AccountState::Disabled;
        match change {
            AccountChange::Enable { .. } if status.state != AccountState::Disabled => fail("unchanged", "The account is already on"),
            AccountChange::Remove { .. } if account.is_default() => {
                fail("invalid_input", "The provider's own sign-in on this Mac stays").map_err(|e| e.because(Refusal::DefaultAccountStays { name: name() }))
            }
            AccountChange::Disable { .. } if status.state == AccountState::Disabled => fail("unchanged", "The account is already off"),
            AccountChange::Disable { .. } if !listed.iter().any(|other| other.id != *account && enabled(other)) => {
                fail("invalid_input", "One account stays on").map_err(|e| e.because(Refusal::LastAccountOn { name: name() }))
            }
            AccountChange::Remove { .. } if self.tasks_on_account(account) > 0 => {
                fail("blocked", "Open tasks still run on the account").map_err(|e| e.because(Refusal::AccountInUse { name: name() }))
            }
            AccountChange::MoveUp { .. } if at == 0 => fail("unchanged", "The account is already first"),
            AccountChange::MoveDown { .. } if at + 1 == listed.len() => fail("unchanged", "The account is already last"),
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
            }
            AccountChange::MoveUp { account } | AccountChange::MoveDown { account } => {
                let up = matches!(change, AccountChange::MoveUp { .. });
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
