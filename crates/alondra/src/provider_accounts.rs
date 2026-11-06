//! `alondra accounts`: provider accounts nodes may run on. The daemon keeps their configuration.
//! Alondra never signs an account in: the user runs the provider's own command in Terminal, and
//! `add` says which one. The user names an account, never a folder.
use std::time::Duration;

use alondra_core::accounts::{AccountBilling, AccountChange, AccountId, AccountState, AccountStatus, AccountsConfig, Provider, ProviderAccount};
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::Client;

const PATH: &str = "/settings/accounts";
const CHECK: &str = "/settings/accounts/check";

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct Row {
    #[serde(default)]
    pub provider: Provider,
    pub id: AccountId,
    pub name: String,
    pub enabled: bool,
    pub default: bool,
    #[serde(default)]
    pub billing: AccountBilling,
    pub state: AccountState,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
}

/// The section as the file holds it, and every account as the daemon last saw it.
#[derive(Deserialize, Clone, Debug)]
pub(crate) struct Listing {
    pub version: String,
    pub config: AccountsConfig,
    pub accounts: Vec<Row>,
    #[serde(default = "legacy_providers")]
    pub supported_providers: Vec<Provider>,
}

fn legacy_providers() -> Vec<Provider> { vec![Provider::Claude] }

impl Listing {
    fn find(&self, name: &str) -> Result<&Row> {
        if let Some(row) = self.accounts.iter().find(|row| row.provider == Provider::Claude && shown_name(row).eq_ignore_ascii_case(name.trim())) {
            return Ok(row);
        }
        let id = self.config.find(Provider::Claude, name).map(|account| account.id)
            .ok_or_else(|| anyhow!(trf!("No account is called {name}: see alondra accounts list", "Ninguna cuenta se llama {name}: mira alondra accounts list")))?;
        self.accounts.iter().find(|row| row.provider == Provider::Claude && row.id == id).context("the account is not in the listing")
    }

    fn without(&self, id: &AccountId) -> AccountsConfig {
        self.config.changed(Provider::Claude, &alondra_core::accounts::AccountChange::Remove { account: id.clone() }).expect("the listed named account exists")
    }
}

fn daemon() -> Client {
    Client::new(Duration::from_secs(120))
}

fn exchange(client: &Client, method: &str, path: &str, body: Option<&Value>) -> Result<Listing> {
    let (status, value) = client.exchange(method, path, body)
        .context(tr!("Alondra is not running on this Mac: start it and try again", "Alondra no está en marcha en este Mac: arráncalo y vuelve a probar"))?;
    if status == 404 {
        bail!(tr!("This Alondra cannot manage accounts yet: update it", "Este Alondra aún no gestiona cuentas: actualízalo"));
    }
    if status >= 400 {
        bail!("{}", value["message"].as_str().or(value["error"].as_str()).unwrap_or("the accounts could not be changed"));
    }
    Ok(serde_json::from_value(value)?)
}

fn listing_path() -> String {
    format!("{PATH}?capabilities=provider_accounts")
}

fn listing(client: &Client) -> Result<Listing> {
    exchange(client, "GET", &listing_path(), None)
}

/// The accounts of every provider as the daemon reports them, for callers that only read.
pub(crate) fn accounts(client: &Client) -> Option<Value> {
    let listing: Value = client.get(&listing_path()).ok()?;
    listing.get("accounts").cloned()
}

fn save(client: &Client, seen: &Listing, next: &AccountsConfig) -> Result<Listing> {
    exchange(client, "PUT", &format!("{PATH}?capabilities=provider_accounts"), Some(&json!({"version": seen.version, "accounts": next})))
}

fn check(client: &Client) -> Result<Listing> {
    exchange(client, "POST", &format!("{CHECK}?capabilities=provider_accounts"), None)
}

fn status(row: &Row) -> AccountStatus {
    AccountStatus {
        provider: row.provider, id: row.id.clone(), name: row.name.clone(), state: row.state,
        native_identity: None, email: row.email.clone(), plan: row.plan.clone(), billing: row.billing.clone(), sign_ins: 0, profile: row.profile.clone(),
    }
}

fn lang() -> alondra_core::views::Lang {
    match crate::i18n::lang() {
        crate::i18n::Lang::En => alondra_core::views::Lang::En,
        crate::i18n::Lang::Es => alondra_core::views::Lang::Es,
    }
}

fn state_text(row: &Row) -> String {
    alondra_core::views::accounts::subtitle(&status(row), lang())
}

fn shown_name(row: &Row) -> String {
    alondra_core::views::accounts::name(&status(row), lang())
}

/// One line per account: its name, then its email, plan and state.
pub(crate) fn lines(rows: &[Row]) -> Vec<String> {
    let names: Vec<String> = rows.iter().map(shown_name).collect();
    let width = names.iter().map(|name| name.chars().count()).max().unwrap_or(0);
    names.iter().zip(rows).map(|(name, row)| format!("{name:<width$}  {}", state_text(row))).collect()
}

pub fn list(json: bool) -> Result<()> {
    let listing = check(&daemon())?;
    if json {
        let accounts: Vec<Value> = listing.accounts.iter().map(|row| json!({
            "provider": row.provider, "id": row.id, "name": row.name, "enabled": row.enabled, "default": row.default, "billing": row.billing,
            "state": row.state, "email": row.email, "plan": row.plan,
        })).collect();
        println!("{}", serde_json::to_string_pretty(&json!({"schema": "alondra.accounts/1", "ok": true, "distribution": listing.config.distribution, "accounts": accounts}))?);
        return Ok(());
    }
    let mixed = listing.accounts.iter().any(|row| row.provider == Provider::Claude) && listing.accounts.iter().any(|row| row.provider == Provider::Codex);
    for (row, line) in listing.accounts.iter().zip(lines(&listing.accounts)) {
        if mixed { println!("[{}] {line}", row.provider.as_str()); } else { println!("{line}"); }
    }
    Ok(())
}

fn print_row(listing: &Listing, id: &AccountId) {
    if let Some(row) = listing.accounts.iter().find(|row| &row.id == id) {
        let (name, state) = (shown_name(row), state_text(row));
        println!("{}", trf!("{name}: {state}", "{name}: {state}"));
    }
}

/// Registers an account named `name` and prints the provider's own command that signs it in. The
/// provider is connected first when it is not yet. Alondra never runs the command.
pub fn add(provider: Provider, name: &str) -> Result<()> {
    for line in add_with(&daemon(), provider, name)? { println!("{line}"); }
    Ok(())
}

fn add_with(client: &Client, provider: Provider, name: &str) -> Result<Vec<String>> {
    let name = name.trim();
    if name.is_empty() {
        bail!(tr!("Give the account a name: alondra accounts add Work", "Dale un nombre a la cuenta: alondra accounts add Trabajo"));
    }
    let seen = listing(client)?;
    if !seen.supported_providers.contains(&provider) { bail!(tr!("Update Alondra before adding an account of this provider", "Actualiza Alondra antes de añadir una cuenta de este proveedor")); }
    if seen.config.find(provider, name).is_some_and(|account| account.name.trim().eq_ignore_ascii_case(name)) {
        bail!(trf!("There is already an account called {name}", "Ya hay una cuenta que se llama {name}"));
    }
    let next = seen.config.changed(provider, &AccountChange::Add { name: name.into() }).map_err(anyhow::Error::msg)?;
    let listed = save(client, &seen, &next)?;
    let row = listed.accounts.iter().find(|row| row.provider == provider && row.name.trim().eq_ignore_ascii_case(name)).context("the account is not in the listing")?;
    let state = state_text(row);
    let mut lines = vec![trf!("Added {name}: {state}", "Añadida {name}: {state}")];
    if row.state != AccountState::SignedIn {
        lines.push(alondra_core::views::accounts::account_sign_in_hint(&status(row), lang()));
    }
    Ok(lines)
}

/// Connects `provider` with its own session on this Mac, and tells how to sign it in when it has
/// none. Alondra never signs in.
pub fn connect(provider: Provider) -> Result<Option<String>> {
    let client = daemon();
    let mut seen = listing(&client)?;
    if !seen.supported_providers.contains(&provider) { bail!(tr!("Update Alondra before connecting this provider", "Actualiza Alondra antes de conectar este proveedor")); }
    if !seen.config.is_connected(provider) {
        let next = seen.config.changed(provider, &AccountChange::Connect).map_err(anyhow::Error::msg)?;
        seen = save(&client, &seen, &next)?;
    }
    let row = seen.accounts.iter().find(|row| row.provider == provider && row.default).context("the provider's own account is not in the listing")?;
    Ok((row.state != AccountState::SignedIn).then(|| alondra_core::views::accounts::account_sign_in_hint(&status(row), lang())))
}

pub fn set_enabled(name: &str, enabled: bool) -> Result<()> {
    let client = daemon();
    let seen = listing(&client)?;
    let row = seen.find(name)?.clone();
    let mut next = seen.config.clone();
    match next.claude.iter_mut().find(|account| account.id == row.id) {
        Some(account) => account.enabled = enabled,
        None => next.claude.insert(0, ProviderAccount { enabled, ..ProviderAccount::implicit_default() }),
    }
    if !next.any_enabled_globally() {
        let name = shown_name(&row);
        bail!(trf!(
            "{name} is the only Claude account still on: with it off no node could start. Enable another one first",
            "{name} es la única cuenta de Claude activa: sin ella no arrancaría ningún nodo. Activa antes otra"
        ));
    }
    let listing = save(&client, &seen, &next)?;
    print_row(&listing, &row.id);
    Ok(())
}

/// Whether moving to another account is on, and the risk the user takes with it.
pub(crate) fn moving_lines(on: bool) -> [String; 2] {
    let state = if on { tr!("on", "activado") } else { tr!("off", "desactivado") };
    [
        format!("{}: {state}", tr!("Move a node to another account when its own reaches its limit", "Mover un nodo a otra cuenta cuando la suya llega al límite")),
        tr!(
            "Anthropic doesn't say whether switching accounts when one reaches its limit is allowed.",
            "Anthropic no dice si cambiar de cuenta al llegar al límite está permitido."
        ).to_owned(),
    ]
}

/// Shows whether a node whose account reaches its limit goes on with another account, or turns it
/// on or off.
pub fn moving(on: Option<bool>) -> Result<()> {
    let client = daemon();
    let mut seen = listing(&client)?;
    if let Some(on) = on.filter(|on| *on != seen.config.distribution.move_when_exhausted) {
        let mut next = seen.config.clone();
        next.distribution.move_when_exhausted = on;
        seen = save(&client, &seen, &next)?;
    }
    for line in moving_lines(seen.config.distribution.move_when_exhausted) {
        println!("{line}");
    }
    Ok(())
}

pub fn remove(name: &str) -> Result<()> {
    let client = daemon();
    let seen = listing(&client)?;
    let row = seen.find(name)?.clone();
    let name = shown_name(&row);
    if row.default {
        bail!(trf!(
            "{name} is Claude's own sign-in on this Mac: Alondra never removes it",
            "{name} es la sesión propia de Claude en este Mac: Alondra nunca la quita"
        ));
    }
    save(&client, &seen, &seen.without(&row.id))?;
    println!("{}", trf!("Removed {name}: it is signed out of Claude", "Quitada {name}: ya no tiene sesión en Claude"));
    Ok(())
}

fn provider_row<'a>(listing: &'a Listing, provider: Provider, name: &str) -> Result<&'a Row> {
    listing.accounts.iter().find(|row| row.provider == provider && (row.id.as_str() == name || shown_name(row).eq_ignore_ascii_case(name.trim())))
        .ok_or_else(|| anyhow!("No {} account is called {name}", provider.as_str()))
}

pub fn list_for(provider: Provider, json: bool) -> Result<()> {
    if provider == Provider::Claude { return list(json); }
    let listing = check(&daemon())?;
    let rows: Vec<Row> = listing.accounts.into_iter().filter(|row| row.provider == provider).collect();
    if json {
        let rows: Vec<Value> = rows.iter().map(|row| json!({"provider":row.provider,"id":row.id,"name":row.name,"state":row.state,"email":row.email,"plan":row.plan,"enabled":row.enabled,"default":row.default,"billing":row.billing})).collect();
        println!("{}", serde_json::to_string_pretty(&json!({"schema":"alondra.accounts/1","ok":true,"accounts":rows}))?);
    } else {
        for line in lines(&rows) { println!("{line}"); }
    }
    Ok(())
}

pub fn set_enabled_for(provider: Provider, name: &str, enabled: bool) -> Result<()> {
    if provider == Provider::Claude { return set_enabled(name, enabled); }
    let client = daemon();
    let seen = listing(&client)?;
    let id = provider_row(&seen, provider, name)?.id.clone();
    let next = seen.config.changed(provider, &if enabled { alondra_core::accounts::AccountChange::Enable { account: id } } else { alondra_core::accounts::AccountChange::Disable { account: id } }).map_err(anyhow::Error::msg)?;
    if !next.any_enabled_globally() { bail!("At least one account stays enabled for new work"); }
    save(&client, &seen, &next)?;
    Ok(())
}

pub fn remove_for(provider: Provider, name: &str) -> Result<()> {
    if provider == Provider::Claude { return remove(name); }
    let client = daemon();
    let seen = listing(&client)?;
    let row = provider_row(&seen, provider, name)?;
    if row.default { bail!("The provider's native sign-in stays on this Mac"); }
    let next = seen.config.changed(provider, &alondra_core::accounts::AccountChange::Remove { account: row.id.clone() }).map_err(anyhow::Error::msg)?;
    save(&client, &seen, &next)?;
    Ok(())
}

#[cfg(test)]
#[path = "provider_accounts_tests.rs"]
mod tests;
