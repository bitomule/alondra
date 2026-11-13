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
pub const MANAGED_PROVIDER_FOLDER: &str = "provider-accounts";
const ID_LIMIT: usize = 32;
const NAME_LIMIT: usize = 60;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Claude,
    Codex,
}

pub fn config_version(config: &AccountsConfig) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(config.to_toml().as_bytes()).iter().take(8).map(|byte| format!("{byte:02x}")).collect()
}

impl Provider {
    pub fn is_claude(&self) -> bool { *self == Self::Claude }
    pub fn from_driver(driver: &str) -> Option<Self> {
        match driver {
            "claude-agent-sdk" => Some(Self::Claude),
            "codex-app-server" => Some(Self::Codex),
            _ => None,
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            _ => Err(format!("unknown provider {text:?}")),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct AccountKey {
    pub provider: Provider,
    pub account: AccountId,
}

impl AccountKey {
    pub fn wire_id(&self) -> String { format!("{}/{}", self.provider.as_str(), self.account) }
    pub fn parse_wire(value: &str) -> Result<Self, String> {
        match value.split_once('/') {
            Some((provider, account)) => Ok(Self::new(Provider::parse(provider)?, AccountId::parse(account)?)),
            None => Ok(Self::new(Provider::Claude, AccountId::parse(value)?)),
        }
    }
    pub fn new(provider: Provider, account: AccountId) -> Self { Self { provider, account } }
    pub fn native(provider: Provider) -> Self { Self::new(provider, AccountId::default_account()) }
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Claude, Provider::Codex];
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
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

pub fn is_default_policy(policy: &Policy) -> bool {
    *policy == Policy::default()
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<AccountKey>,
}

impl Default for Distribution {
    fn default() -> Self {
        Distribution { policy: Policy::Margin, reserve: RESERVE, move_when_exhausted: false, order: vec![] }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct AccountsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected: Option<Vec<Provider>>,
    pub distribution: Distribution,
    pub claude: Vec<ProviderAccount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codex: Vec<ProviderAccount>,
}

impl AccountsConfig {
    pub fn fresh() -> Self {
        Self { connected: Some(vec![]), ..Default::default() }
    }

    pub fn connected_providers(&self) -> &[Provider] {
        self.connected.as_deref().unwrap_or(&[Provider::Claude])
    }

    pub fn is_connected(&self, provider: Provider) -> bool {
        self.connected_providers().contains(&provider)
    }

    pub fn migrate_legacy(&mut self) {
        if self.connected.is_none() { self.connected = Some(vec![Provider::Claude]); }
    }

    pub fn preserve_connected_from(&mut self, current: &Self) {
        if self.connected.is_none() && current.connected.is_some() {
            self.connected.clone_from(&current.connected);
            self.codex.clone_from(&current.codex);
            self.distribution.order.clone_from(&current.distribution.order);
        }
    }

    pub fn connect(&mut self, provider: Provider) {
        self.migrate_legacy();
        let connected = self.connected.as_mut().expect("made explicit");
        if !connected.contains(&provider) { connected.push(provider); }
        if !self.distribution.order.is_empty() {
            let known = self.ordered_accounts().into_iter().map(|(key, _)| key).collect();
            self.distribution.order = known;
        }
    }

    /// Writes the order out once more than one provider is connected or the user set one, so
    /// every account, a new one too, has its place.
    pub fn order_every_account(&mut self) -> Result<(), String> {
        if self.connected_providers().len() > 1 || !self.distribution.order.is_empty() {
            let order = self.ordered_accounts().into_iter().map(|(key, _)| key).collect();
            self.set_order(order)?;
        }
        Ok(())
    }

    pub fn ordered_accounts(&self) -> Vec<(AccountKey, ProviderAccount)> {
        let mut remaining: Vec<_> = self.connected_providers().iter().flat_map(|provider| self.accounts(*provider).into_iter().map(|account| (AccountKey::new(*provider, account.id.clone()), account))).collect();
        let mut ordered = vec![];
        for key in &self.distribution.order {
            if let Some(at) = remaining.iter().position(|(candidate, _)| candidate == key) {
                ordered.push(remaining.remove(at));
            }
        }
        ordered.extend(remaining);
        ordered
    }

    pub fn set_order(&mut self, order: Vec<AccountKey>) -> Result<(), String> {
        let known: std::collections::BTreeSet<_> = self.ordered_accounts().into_iter().map(|(key, _)| key).collect();
        let offered: std::collections::BTreeSet<_> = order.iter().cloned().collect();
        if order.len() != offered.len() || offered != known {
            return Err("account order must contain every connected account exactly once".into());
        }
        self.distribution.order = order;
        Ok(())
    }

    pub fn any_enabled_globally(&self) -> bool {
        self.ordered_accounts().iter().any(|(_, account)| account.enabled)
    }

    pub fn listed(&self, provider: Provider) -> &[ProviderAccount] {
        match provider {
            Provider::Claude => &self.claude,
            Provider::Codex => &self.codex,
        }
    }

    pub fn listed_mut(&mut self, provider: Provider) -> &mut Vec<ProviderAccount> {
        match provider {
            Provider::Claude => &mut self.claude,
            Provider::Codex => &mut self.codex,
        }
    }

    /// The provider's accounts in the user's order, the default one first when the file does not
    /// place it.
    pub fn accounts(&self, provider: Provider) -> Vec<ProviderAccount> {
        if !self.is_connected(provider) { return vec![]; }
        let listed = self.listed(provider);
        let mut accounts = Vec::with_capacity(listed.len() + 1);
        if !listed.iter().any(|a| a.id.is_default()) {
            accounts.push(ProviderAccount::implicit_default());
        }
        accounts.extend(listed.iter().cloned());
        accounts
    }

    /// Whether some account of `provider` is on: new work needs one.
    pub fn any_enabled(&self, provider: Provider) -> bool {
        self.accounts(provider).iter().any(|a| a.enabled)
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
        let connected: std::collections::BTreeSet<_> = self.connected_providers().iter().collect();
        if connected.len() != self.connected_providers().len() { return Err("accounts: a provider is connected twice".into()); }
        if !self.distribution.order.is_empty() {
            let mut checked = self.clone();
            checked.set_order(self.distribution.order.clone())?;
        }
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
                    if weight.is_some_and(|weight| weight == 0 || weight > WEIGHT_LIMIT) {
                        return Err(format!("accounts {}: account {} needs a weight from 1 to {WEIGHT_LIMIT}", provider.as_str(), account.id));
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

/// The most an account's weight may be: twice the largest plan's.
pub const WEIGHT_LIMIT: u32 = 20;

/// One change the user makes to the accounts from Settings.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AccountChange {
    Enable { account: AccountId },
    Disable { account: AccountId },
    Remove { account: AccountId },
    MoveUp { account: AccountId },
    MoveDown { account: AccountId },
    Rename { account: AccountId, name: String },
    /// The account's weight, or its plan's usual one with none.
    SetWeight {
        account: AccountId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<u32>,
    },
    SetPolicy { policy: Policy },
    /// Whether a node whose account reaches its limit goes on with another one.
    SetMoveWhenExhausted { on: bool },
    /// Use the provider through its own sign-in on this Mac. Alondra never signs in for the user.
    Connect,
    /// A new account of the provider, named so: it stays signed out until the user signs it in
    /// with the provider's own tool in Terminal.
    Add { name: String },
}

impl AccountChange {
    pub fn account(&self) -> Option<&AccountId> {
        match self {
            AccountChange::Enable { account }
            | AccountChange::Disable { account }
            | AccountChange::Remove { account }
            | AccountChange::MoveUp { account }
            | AccountChange::MoveDown { account }
            | AccountChange::Rename { account, .. }
            | AccountChange::SetWeight { account, .. } => Some(account),
            AccountChange::SetPolicy { .. } | AccountChange::SetMoveWhenExhausted { .. } | AccountChange::Connect | AccountChange::Add { .. } => None,
        }
    }
}

impl AccountsConfig {
    /// The section with `change` made. The default account is written into the list when the
    /// change places it or sets something of its own.
    pub fn changed(&self, provider: Provider, change: &AccountChange) -> Result<AccountsConfig, String> {
        let mut next = self.clone();
        match change {
            AccountChange::SetPolicy { policy } => {
                next.distribution.policy = *policy;
                if *policy == Policy::Margin { next.distribution.order.clear(); }
                return Ok(next);
            }
            AccountChange::SetMoveWhenExhausted { on } => {
                next.distribution.move_when_exhausted = *on;
                return Ok(next);
            }
            AccountChange::Connect => {
                next.connect(provider);
                next.order_every_account()?;
                return Ok(next);
            }
            AccountChange::Add { name } => {
                let name = name.trim();
                if next.accounts(provider).iter().any(|account| account.name.trim().eq_ignore_ascii_case(name)) {
                    return Err(format!("There is already an account called {name}"));
                }
                let first = !next.is_connected(provider);
                next.connect(provider);
                let taken: Vec<AccountId> = next.accounts(provider).into_iter().map(|account| account.id).collect();
                let id = if first { AccountId::default_account() } else { AccountId::for_name(name, &taken) };
                next.listed_mut(provider).push(ProviderAccount { id, name: name.into(), enabled: true, billing: AccountBilling::default() });
                next.order_every_account()?;
                next.check()?;
                return Ok(next);
            }
            _ => {}
        }
        let account = change.account().expect("every other change names an account");
        if matches!(change, AccountChange::MoveUp { .. } | AccountChange::MoveDown { .. }) && !self.distribution.order.is_empty() {
            let mut order = self.ordered_accounts().into_iter().map(|(key, _)| key).collect::<Vec<_>>();
            let key = AccountKey::new(provider, account.clone());
            let at = order.iter().position(|key_on| key_on == &key).ok_or_else(|| format!("account {account} is not listed"))?;
            let to = if matches!(change, AccountChange::MoveUp { .. }) { at.checked_sub(1) } else { (at + 1 < order.len()).then_some(at + 1) };
            let Some(to) = to else { return Err(format!("account {account} can't move further")); };
            order.swap(at, to);
            next.set_order(order)?;
            return Ok(next);
        }
        let mut listed = self.accounts(provider);
        let at = listed.iter().position(|a| a.id == *account).ok_or_else(|| format!("account {account} is not listed"))?;
        match change {
            AccountChange::Enable { .. } => listed[at].enabled = true,
            AccountChange::Disable { .. } => listed[at].enabled = false,
            AccountChange::Rename { name, .. } => listed[at].name = name.trim().into(),
            AccountChange::Remove { .. } => {
                listed.remove(at);
                next.distribution.order.retain(|key| !(key.provider == provider && key.account == *account));
            }
            AccountChange::MoveUp { .. } if at > 0 => listed.swap(at, at - 1),
            AccountChange::MoveDown { .. } if at + 1 < listed.len() => listed.swap(at, at + 1),
            AccountChange::MoveUp { .. } | AccountChange::MoveDown { .. } => return Err(format!("account {account} can't move further")),
            AccountChange::SetWeight { weight, .. } => match &mut listed[at].billing {
                AccountBilling::Subscription { weight: set, .. } => *set = *weight,
                AccountBilling::PayPerUse => return Err(format!("account {account} is paid per use and has no weight")),
            },
            AccountChange::SetPolicy { .. } | AccountChange::SetMoveWhenExhausted { .. } | AccountChange::Connect | AccountChange::Add { .. } => unreachable!("handled above"),
        }
        let implicit = ProviderAccount::implicit_default();
        let untouched_default = !self.listed(provider).iter().any(|a| a.id.is_default())
            && listed.first().is_some_and(|first| *first == implicit);
        if untouched_default {
            listed.remove(0);
        }
        *next.listed_mut(provider) = listed;
        next.check()?;
        Ok(next)
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
    /// Public native account scope, when the provider positively identifies it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// The plan the user set, or else the one the provider reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default)]
    pub billing: AccountBilling,
    /// How many times the account was signed in again after its sign-in expired: the generation
    /// of the credential its executions run on.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sign_ins: u64,
    /// The folder of a named account's provider profile, as the user types it in Terminal (`~`
    /// for their home), so a screen can say which command signs it in; none for the default one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// `text` with its `[accounts]` section replaced by `accounts`, every other line as it was: the
/// old section's tables go, and the new one is added at the end.
pub fn with_accounts(text: &str, accounts: &AccountsConfig) -> Result<String, String> {
    let written = with_section(text, SECTION, &accounts.to_toml())?;
    let parsed = crate::options::OptionsFile::parse(&written)?;
    if parsed.accounts != *accounts {
        return Err("the accounts section did not take the new value".into());
    }
    Ok(written)
}

pub(crate) fn with_section(text: &str, name: &str, section: &str) -> Result<String, String> {
    let mut kept: Vec<&str> = Vec::new();
    let mut inside = false;
    let mut top_level = true;
    for line in text.lines() {
        if let Some(table) = table_name(line) {
            top_level = false;
            inside = table == name || table.starts_with(&format!("{name}."));
        } else if top_level && top_level_key(line) == Some(name) {
            return Err(format!("options.toml sets {name} as a single value: write it as an [{name}] table"));
        }
        if !inside {
            kept.push(line);
        }
    }
    while kept.last().is_some_and(|line| line.trim().is_empty()) {
        kept.pop();
    }
    let mut written = kept.join("\n");
    if !section.is_empty() {
        if !written.is_empty() {
            written.push_str("\n\n");
        }
        written.push_str(section.trim_end());
    }
    if !written.is_empty() {
        written.push('\n');
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

/// The folder holding every added account's own folder.
pub fn folders(home: &Path) -> PathBuf {
    home.join(FOLDER)
}

/// The folder of an account's provider configuration; none for the default one, which stays the
/// provider's own.
pub fn folder(home: &Path, id: &AccountId) -> Option<PathBuf> {
    (!id.is_default()).then(|| folders(home).join(id.as_str()))
}

pub fn provider_folder(home: &Path, key: &AccountKey) -> Option<PathBuf> {
    match key.provider {
        Provider::Claude => folder(home, &key.account),
        Provider::Codex => (!key.account.is_default()).then(|| home.join(MANAGED_PROVIDER_FOLDER).join("codex").join(key.account.as_str())),
    }
}

/// `folder` as the user types it in Terminal: under `user_home` it starts with `~`.
pub fn shown_folder(folder: &Path, user_home: &Path) -> String {
    match folder.strip_prefix(user_home) {
        Ok(inside) if !user_home.as_os_str().is_empty() => format!("~/{}", inside.display()),
        _ => folder.display().to_string(),
    }
}

/// The folder of a named account's provider profile, as [`shown_folder`] shows it.
pub fn shown_profile(home: &Path, user_home: &Path, key: &AccountKey) -> Option<String> {
    provider_folder(home, key).map(|folder| shown_folder(&folder, user_home))
}

/// What the user runs in Terminal on the Mac to sign an account in with the provider's own tool:
/// with its profile folder for a named one. Alondra never runs it.
pub fn sign_in_command(provider: Provider, profile: Option<&str>) -> String {
    let (variable, command) = match provider {
        Provider::Claude => ("CLAUDE_CONFIG_DIR", "claude"),
        Provider::Codex => ("CODEX_HOME", "codex login"),
    };
    match profile {
        Some(folder) => format!("{variable}={} {command}", shell_folder(folder)),
        None => command.into(),
    }
}

fn shell_folder(folder: &str) -> String {
    let plain = |text: &str| !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | '@' | ':' | ','));
    let quoted = |text: &str| format!("'{}'", text.replace('\'', r"'\''"));
    match folder.strip_prefix("~/") {
        Some(rest) if plain(rest) => folder.into(),
        Some(rest) => format!("~/{}", quoted(rest)),
        None if plain(folder) => folder.into(),
        None => quoted(folder),
    }
}

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod tests;
