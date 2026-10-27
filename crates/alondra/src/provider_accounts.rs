//! `alondra accounts`: the Claude accounts nodes may run on. This Mac's daemon keeps the list in
//! `options.toml` and each account's folder; this command only asks it, and signs an account in
//! with `claude auth login` in this terminal. The user names an account, never a folder.
use std::path::{Path, PathBuf};
use std::time::Duration;

use alondra_core::accounts::{AccountBilling, AccountId, AccountState, AccountStatus, AccountsConfig, Provider, ProviderAccount};
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::{self, Client};
use crate::commands::NeedsPerson;

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

/// Another account signed in with the same email as `id`.
pub(crate) fn same_email<'a>(rows: &'a [Row], id: &AccountId) -> Option<&'a Row> {
    same_email_for(rows, Provider::Claude, id)
}

pub(crate) fn same_email_for<'a>(rows: &'a [Row], provider: Provider, id: &AccountId) -> Option<&'a Row> {
    let email = rows.iter().find(|row| row.provider == provider && &row.id == id)?.email.as_deref()?.to_lowercase();
    rows.iter().find(|row| row.provider == provider && &row.id != id && row.email.as_deref().is_some_and(|other| other.to_lowercase() == email))
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

fn listing(client: &Client) -> Result<Listing> {
    exchange(client, "GET", &format!("{PATH}?capabilities=provider_accounts"), None)
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
        native_identity: None, email: row.email.clone(), plan: row.plan.clone(), billing: row.billing.clone(), sign_ins: 0,
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

fn needs_terminal(no_input: bool, command: &str) -> Result<()> {
    if crate::doctor::can_ask(no_input) {
        return Ok(());
    }
    Err(NeedsPerson(trf!(
        "Signing in may open your browser: run alondra accounts {command} in a terminal",
        "Iniciar sesión abre el navegador: ejecuta alondra accounts {command} en una terminal"
    )).into())
}

fn user_home() -> PathBuf {
    alondra_core::config::home_dir(&|key| std::env::var(key).ok())
}

/// Claude Code as this terminal runs it for the account: with the profile the daemon's nodes run
/// it with, `default_profile` being the default account's.
fn claude_for(home: &Path, id: &AccountId, default_profile: Option<&Path>) -> crate::claude_auth::ContextEnv {
    crate::claude_auth::ContextEnv {
        home: user_home(),
        path: std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
        config_dir: alondra_core::claude_account::profile(home, id, default_profile).map(|folder| folder.to_string_lossy().into_owned()),
    }
}

/// Signs the account in, then has the daemon check it. An email another account already has is
/// refused.
fn sign_in(client: &Client, id: &AccountId) -> Result<Listing> {
    let home = client::config().home.clone();
    let profile = crate::claude_auth::installed_profile(&user_home());
    if !id.is_default() {
        alondra_core::claude_account::prepare(&home, id, &alondra_core::claude_account::config_folder(profile.as_deref(), &user_home()))
            .context(tr!("The account's folder could not be prepared", "No se pudo preparar la carpeta de la cuenta"))?;
    }
    claude_for(&home, id, profile.as_deref()).sign_in()?;
    let listing = check(client)?;
    let row = listing.accounts.iter().find(|row| &row.id == id).context("the account is gone")?;
    if row.state != AccountState::SignedIn {
        let name = shown_name(row);
        bail!(trf!(
            "{name} is still not signed in to Claude: run alondra accounts login \"{name}\"",
            "{name} sigue sin sesión en Claude: ejecuta alondra accounts login \"{name}\""
        ));
    }
    Ok(listing)
}

pub fn add(name: &str, no_input: bool) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        bail!(tr!("Give the account a name: alondra accounts add Work", "Dale un nombre a la cuenta: alondra accounts add Trabajo"));
    }
    needs_terminal(no_input, "add")?;
    let client = daemon();
    let seen = listing(&client)?;
    let mut next = seen.config.clone();
    if next.find(Provider::Claude, name).is_some_and(|account| account.name.trim().eq_ignore_ascii_case(name)) {
        bail!(trf!("There is already an account called {name}", "Ya hay una cuenta que se llama {name}"));
    }
    let first = !next.is_connected(Provider::Claude);
    next.connect(Provider::Claude);
    let taken: Vec<AccountId> = seen.accounts.iter().filter(|row| row.provider == Provider::Claude).map(|row| row.id.clone()).collect();
    let id = if first { AccountId::default_account() } else { AccountId::for_name(name, &taken) };
    next.claude.push(ProviderAccount { id: id.clone(), name: name.to_owned(), enabled: true, billing: AccountBilling::default() });
    finish_added_account(&mut next)?;
    save(&client, &seen, &next)?;
    println!("{}", trf!("Sign in with the Claude account for {name}.", "Inicia sesión con la cuenta de Claude de {name}."));
    let listing = match sign_in(&client, &id) {
        Ok(listing) => listing,
        Err(error) => {
            eprintln!("{}", trf!("{name} stays without a session; sign it in later with alondra accounts login \"{name}\".",
                "{name} se queda sin sesión; iníciala luego con alondra accounts login \"{name}\"."));
            return Err(error);
        }
    };
    if let Some(other) = same_email(&listing.accounts, &id) {
        let other = shown_name(other);
        save(&client, &listing, &listing.without(&id))?;
        bail!(trf!(
            "That Claude account is already in Alondra as {other}: sign in with another one. {name} was not added.",
            "Esa cuenta de Claude ya está en Alondra como {other}: inicia sesión con otra. {name} no se añadió."
        ));
    }
    let state = listing.accounts.iter().find(|row| row.id == id).map(state_text).unwrap_or_default();
    println!("{}", trf!("Added {name}: {state}", "Añadida {name}: {state}"));
    Ok(())
}

pub fn login(name: &str, no_input: bool) -> Result<()> {
    needs_terminal(no_input, "login")?;
    let client = daemon();
    let seen = listing(&client)?;
    let row = seen.find(name)?.clone();
    let listing = sign_in(&client, &row.id)?;
    if let Some(other) = same_email(&listing.accounts, &row.id) {
        let (other, name) = (shown_name(other), shown_name(&row));
        if !row.id.is_default() {
            let home = client::config().home.clone();
            claude_for(&home, &row.id, None).sign_out()?;
            check(&client)?;
        }
        bail!(trf!(
            "That Claude account is already in Alondra as {other}: sign {name} in with another one",
            "Esa cuenta de Claude ya está en Alondra como {other}: inicia sesión en {name} con otra"
        ));
    }
    print_row(&listing, &row.id);
    Ok(())
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

fn codex_sign_in(client: &Client, id: &AccountId) -> Result<Listing> {
    let home = client::config().home.clone();
    let key = alondra_core::accounts::AccountKey::new(Provider::Codex, id.clone());
    let profile = alondra_core::codex_account::prepare(&home, &key).map_err(anyhow::Error::msg)?;
    let context = crate::claude_auth::installed_codex_context(&user_home())?;
    let executable = alondra_core::codex_account::executable(&context.path).context("Codex is not installed for the service on this Mac")?;
    let username = alondra_core::claude_account::effective_username().context("The current user could not be identified")?;
    let status = alondra_core::codex_account::command(&executable, &context.environment, &context.home, &context.path, &username, profile.as_deref()).arg("login").status()?;
    if !status.success() { bail!("Codex login did not complete"); }
    let listing = check(client)?;
    if !listing.accounts.iter().any(|row| row.provider == Provider::Codex && row.id == *id && row.state == AccountState::SignedIn) {
        bail!("The Codex account is still not signed in: check it in Accounts");
    }
    Ok(listing)
}

pub fn login_for(provider: Provider, name: &str, no_input: bool) -> Result<()> {
    if provider == Provider::Claude {
        let client = daemon(); let seen = listing(&client)?;
        if !seen.config.is_connected(provider) && (name == AccountId::DEFAULT || seen.config.listed(provider).iter().any(|account| account.id.as_str() == name || account.name.eq_ignore_ascii_case(name))) {
            let mut next = seen.config.clone(); next.connect(provider); finish_added_account(&mut next)?; save(&client, &seen, &next)?;
        }
        return login(name, no_input);
    }
    needs_terminal(no_input, "login --provider codex")?;
    let client = daemon();
    let mut seen = listing(&client)?;
    if !seen.supported_providers.contains(&provider) { bail!("Update the daemon before connecting this provider"); }
    if !seen.config.is_connected(provider) && (name == AccountId::DEFAULT || seen.config.listed(provider).iter().any(|account| account.id.as_str() == name || account.name.eq_ignore_ascii_case(name))) {
        let mut next = seen.config.clone();
        next.connect(provider);
        finish_added_account(&mut next)?;
        seen = save(&client, &seen, &next)?;
    }
    let id = provider_row(&seen, provider, name)?.id.clone();
    let listed = codex_sign_in(&client, &id)?;
    if let Some(other) = same_email_for(&listed.accounts, provider, &id) {
        let other_name = shown_name(other).to_owned();
        if !id.is_default() {
            let next = listed.config.changed(provider, &alondra_core::accounts::AccountChange::Remove { account: id.clone() }).map_err(anyhow::Error::msg)?;
            save(&client, &listed, &next)?;
        }
        bail!("That Codex account is already connected as {other_name}");
    }
    Ok(())
}

fn finish_added_account(config: &mut AccountsConfig) -> Result<()> {
    if config.connected_providers().len() > 1 || !config.distribution.order.is_empty() {
        let order = config.ordered_accounts().into_iter().map(|(key, _)| key).collect();
        config.set_order(order).map_err(anyhow::Error::msg)?;
    }
    Ok(())
}

pub fn add_for(provider: Provider, name: &str, no_input: bool) -> Result<()> {
    if provider == Provider::Claude { return add(name, no_input); }
    needs_terminal(no_input, "add --provider codex")?;
    let name = name.trim();
    if name.is_empty() { bail!("Give the account a name"); }
    let client = daemon();
    let seen = listing(&client)?;
    let mut next = seen.config.clone();
    if !seen.supported_providers.contains(&provider) { bail!("Update the daemon before connecting this provider"); }
    let first = !next.is_connected(provider);
    next.connect(provider);
    if next.find(provider, name).is_some_and(|account| account.name.eq_ignore_ascii_case(name)) { bail!("An account already has this name"); }
    let taken: Vec<_> = next.accounts(provider).iter().map(|account| account.id.clone()).collect();
    let id = if first { AccountId::default_account() } else { AccountId::for_name(name, &taken) };
    next.listed_mut(provider).push(ProviderAccount { id: id.clone(), name: name.into(), enabled: true, billing: AccountBilling::default() });
    finish_added_account(&mut next)?;
    save(&client, &seen, &next)?;
    let listed = codex_sign_in(&client, &id)?;
    if let Some(other) = same_email_for(&listed.accounts, provider, &id) {
        let other_name = shown_name(other).to_owned();
        if !id.is_default() {
            let next = listed.config.changed(provider, &alondra_core::accounts::AccountChange::Remove { account: id.clone() }).map_err(anyhow::Error::msg)?;
            save(&client, &listed, &next)?;
        }
        bail!("That Codex account is already connected as {other_name}");
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
