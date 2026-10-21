//! The provider accounts nodes may run on, as the user declares them in the `[accounts]` section
//! of `options.toml`, and what the daemon last saw of each. Without the section there is one
//! implicit account per provider, the provider's own default sign-in on this Mac, and nothing
//! changes. Every other account keeps its provider's configuration in a folder of its own inside
//! Alondra's home, which the user never sees: they know an account by its name and email.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

/// The section's name in `options.toml`.
pub const SECTION: &str = "accounts";
/// The folder inside Alondra's home that holds the folder of every account but the default one.
pub const FOLDER: &str = "accounts";
const ID_LIMIT: usize = 32;
const NAME_LIMIT: usize = 60;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Claude,
}

impl Provider {
    pub const ALL: [Provider; 1] = [Provider::Claude];
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
        }
    }
}

/// How Alondra and its folders name an account: lowercase letters, digits and dashes. `default`
/// is the provider's own sign-in on this Mac.
#[derive(Serialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct AccountId(String);

impl AccountId {
    pub const DEFAULT: &'static str = "default";

    pub fn parse(text: &str) -> Result<Self, String> {
        let valid = !text.is_empty()
            && text.len() <= ID_LIMIT
            && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !text.starts_with('-')
            && !text.ends_with('-');
        if valid {
            Ok(Self(text.to_owned()))
        } else {
            Err(format!("account id {text:?}: use 1 to {ID_LIMIT} lowercase letters, digits and dashes"))
        }
    }

    pub fn default_account() -> Self {
        Self(Self::DEFAULT.to_owned())
    }

    pub fn is_default(&self) -> bool {
        self.0 == Self::DEFAULT
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A new id for an account called `name`, unlike every id `taken` holds and never `default`.
    pub fn for_name(name: &str, taken: &[AccountId]) -> Self {
        let mut slug = String::new();
        for c in name.trim().chars().flat_map(char::to_lowercase) {
            match c {
                'a'..='z' | '0'..='9' => slug.push(c),
                'á' | 'à' | 'ä' | 'â' => slug.push('a'),
                'é' | 'è' | 'ë' | 'ê' => slug.push('e'),
                'í' | 'ì' | 'ï' | 'î' => slug.push('i'),
                'ó' | 'ò' | 'ö' | 'ô' => slug.push('o'),
                'ú' | 'ù' | 'ü' | 'û' => slug.push('u'),
                'ñ' => slug.push('n'),
                _ if !slug.is_empty() && !slug.ends_with('-') => slug.push('-'),
                _ => {}
            }
        }
        let stem: String = slug.trim_end_matches('-').chars().take(ID_LIMIT - 3).collect();
        let stem = match stem.trim_end_matches('-') {
            "" => "account".to_owned(),
            stem => stem.to_owned(),
        };
        let free = |id: &str| id != Self::DEFAULT && !taken.iter().any(|t| t.0 == id);
        let id = (1..)
            .map(|n| if n == 1 { stem.clone() } else { format!("{stem}-{n}") })
            .find(|id| free(id))
            .expect("some suffix is free");
        Self(id)
    }
}

impl std::fmt::Display for AccountId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AccountId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        AccountId::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// What using an account costs: a subscription with its usage limits, whose `plan` and `weight`
/// the user may set (otherwise the plan the provider reports and its usual weight), or an account
/// paid per use, which Alondra never picks on its own.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AccountBilling {
    Subscription {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<u32>,
    },
    PayPerUse,
}

impl Default for AccountBilling {
    fn default() -> Self {
        AccountBilling::Subscription { plan: None, weight: None }
    }
}

fn is_default_billing(billing: &AccountBilling) -> bool {
    *billing == AccountBilling::default()
}

fn yes() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProviderAccount {
    pub id: AccountId,
    pub name: String,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "is_default_billing")]
    pub billing: AccountBilling,
}

impl ProviderAccount {
    /// The provider's own sign-in on this Mac, as it stands when the file does not list it.
    pub fn implicit_default() -> Self {
        ProviderAccount { id: AccountId::default_account(), name: "Default".into(), enabled: true, billing: AccountBilling::default() }
    }
}

/// How new work is spread over the accounts: to the one with the most margin, or to the first in
/// the list that has some.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    #[default]
    Margin,
    Chain,
}

pub const RESERVE: f64 = 0.90;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Distribution {
    pub policy: Policy,
    /// How much of the 5-hour window an account may use before it counts as having no margin.
    pub reserve: f64,
    /// A node whose account runs out goes on in another one with margin, instead of waiting.
    pub move_when_exhausted: bool,
}

impl Default for Distribution {
    fn default() -> Self {
        Distribution { policy: Policy::Margin, reserve: RESERVE, move_when_exhausted: false }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct AccountsConfig {
    pub distribution: Distribution,
    pub claude: Vec<ProviderAccount>,
}

impl AccountsConfig {
    pub fn listed(&self, provider: Provider) -> &[ProviderAccount] {
        match provider {
            Provider::Claude => &self.claude,
        }
    }

    pub fn listed_mut(&mut self, provider: Provider) -> &mut Vec<ProviderAccount> {
        match provider {
            Provider::Claude => &mut self.claude,
        }
    }

    /// The provider's accounts in the user's order, the default one first when the file does not
    /// place it.
    pub fn accounts(&self, provider: Provider) -> Vec<ProviderAccount> {
        let listed = self.listed(provider);
        let mut accounts = Vec::with_capacity(listed.len() + 1);
        if !listed.iter().any(|a| a.id.is_default()) {
            accounts.push(ProviderAccount::implicit_default());
        }
        accounts.extend(listed.iter().cloned());
        accounts
    }

    /// The account `name` names: its name, regardless of case, or its id.
    pub fn find(&self, provider: Provider, name: &str) -> Option<ProviderAccount> {
        let name = name.trim();
        let accounts = self.accounts(provider);
        accounts
            .iter()
            .find(|a| a.name.trim().eq_ignore_ascii_case(name))
            .or_else(|| accounts.iter().find(|a| a.id.as_str() == name))
            .cloned()
    }

    pub fn check(&self) -> Result<(), String> {
        let reserve = self.distribution.reserve;
        if !(reserve > 0.0 && reserve <= 1.0) {
            return Err(format!("accounts: reserve is a fraction above 0 and at most 1, not {reserve}"));
        }
        for provider in Provider::ALL {
            let mut ids = std::collections::BTreeSet::new();
            let mut names = std::collections::BTreeSet::new();
            for account in self.listed(provider) {
                let name = account.name.trim();
                if name.is_empty() || name.chars().count() > NAME_LIMIT || name.chars().any(char::is_control) {
                    return Err(format!("accounts {}: account {} needs a name of 1 to {NAME_LIMIT} characters", provider.as_str(), account.id));
                }
                if !ids.insert(account.id.clone()) {
                    return Err(format!("accounts {}: account {} is listed twice", provider.as_str(), account.id));
                }
                if !names.insert(name.to_lowercase()) {
                    return Err(format!("accounts {}: two accounts are called {name}", provider.as_str()));
                }
                if let AccountBilling::Subscription { plan, weight } = &account.billing {
                    if plan.as_deref().is_some_and(|p| p.trim().is_empty()) {
                        return Err(format!("accounts {}: account {} has an empty plan", provider.as_str(), account.id));
                    }
                    if *weight == Some(0) {
                        return Err(format!("accounts {}: account {} needs a weight above 0", provider.as_str(), account.id));
                    }
                }
            }
        }
        Ok(())
    }

    /// The section as `options.toml` holds it; empty when everything is as without it.
    pub fn to_toml(&self) -> String {
        if *self == AccountsConfig::default() {
            return String::new();
        }
        #[derive(Serialize)]
        struct Wrapper<'a> {
            accounts: &'a AccountsConfig,
        }
        toml::to_string(&Wrapper { accounts: self }).expect("accounts serialize")
    }
}

/// What the daemon last saw of an account.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccountState {
    /// Not checked since the daemon started or the account was added.
    Unchecked,
    SignedIn,
    /// Its sign-in is missing or expired: someone has to sign in on this Mac.
    SignedOut,
    /// Its sign-in could not be checked: the provider's tool is missing or did not answer.
    Unavailable,
    /// The user turned it off: no new work starts on it.
    Disabled,
}

/// An account as every screen reads it: never a path.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AccountStatus {
    pub provider: Provider,
    pub id: AccountId,
    pub name: String,
    pub state: AccountState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// The plan the user set, or else the one the provider reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default)]
    pub billing: AccountBilling,
}

/// `text` with its `[accounts]` section replaced by `accounts`, every other line as it was: the
/// old section's tables go, and the new one is added at the end.
pub fn with_accounts(text: &str, accounts: &AccountsConfig) -> Result<String, String> {
    let mut kept: Vec<&str> = Vec::new();
    let mut inside = false;
    let mut top_level = true;
    for line in text.lines() {
        if let Some(table) = table_name(line) {
            top_level = false;
            inside = table == SECTION || table.starts_with("accounts.");
        } else if top_level && top_level_key(line) == Some(SECTION) {
            return Err("options.toml sets accounts as a single value: write it as an [accounts] table".into());
        }
        if !inside {
            kept.push(line);
        }
    }
    while kept.last().is_some_and(|line| line.trim().is_empty()) {
        kept.pop();
    }
    let mut written = kept.join("\n");
    let section = accounts.to_toml();
    if !section.is_empty() {
        if !written.is_empty() {
            written.push_str("\n\n");
        }
        written.push_str(section.trim_end());
    }
    if !written.is_empty() {
        written.push('\n');
    }
    let parsed = crate::options::OptionsFile::parse(&written)?;
    if parsed.accounts != *accounts {
        return Err("the accounts section did not take the new value".into());
    }
    Ok(written)
}

/// The table a header line opens: `[a.b]` and `[[a.b]]` both give `a.b`.
fn table_name(line: &str) -> Option<&str> {
    let line = line.trim();
    let inner = line.strip_prefix("[[").and_then(|l| l.split_once("]]")).map(|(name, _)| name)
        .or_else(|| line.strip_prefix('[').and_then(|l| l.split_once(']')).map(|(name, _)| name))?;
    Some(inner.trim())
}

fn top_level_key(line: &str) -> Option<&str> {
    let (key, _) = line.split_once('=')?;
    Some(key.trim().trim_matches('"'))
}

/// The folder of an account's provider configuration; none for the default one, which stays the
/// provider's own.
pub fn folder(home: &Path, id: &AccountId) -> Option<PathBuf> {
    (!id.is_default()).then(|| home.join(FOLDER).join(id.as_str()))
}

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod tests;
