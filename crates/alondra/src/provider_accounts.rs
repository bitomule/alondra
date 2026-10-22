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
}

impl Listing {
    fn find(&self, name: &str) -> Result<&Row> {
        if let Some(row) = self.accounts.iter().find(|row| shown_name(row).eq_ignore_ascii_case(name.trim())) {
            return Ok(row);
        }
        let id = self.config.find(Provider::Claude, name).map(|account| account.id)
            .ok_or_else(|| anyhow!(trf!("No account is called {name}: see alondra accounts list", "Ninguna cuenta se llama {name}: mira alondra accounts list")))?;
        self.accounts.iter().find(|row| row.id == id).context("the account is not in the listing")
    }

    fn without(&self, id: &AccountId) -> AccountsConfig {
        AccountsConfig { claude: self.config.claude.iter().filter(|account| &account.id != id).cloned().collect(), ..self.config.clone() }
    }
}

/// Another account signed in with the same email as `id`.
pub(crate) fn same_email<'a>(rows: &'a [Row], id: &AccountId) -> Option<&'a Row> {
    let email = rows.iter().find(|row| &row.id == id)?.email.as_deref()?.to_lowercase();
    rows.iter().find(|row| &row.id != id && row.email.as_deref().is_some_and(|other| other.to_lowercase() == email))
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
    exchange(client, "GET", PATH, None)
}

fn save(client: &Client, seen: &Listing, next: &AccountsConfig) -> Result<Listing> {
    exchange(client, "PUT", PATH, Some(&json!({"version": seen.version, "accounts": next})))
}

fn check(client: &Client) -> Result<Listing> {
    exchange(client, "POST", CHECK, None)
}

fn status(row: &Row) -> AccountStatus {
    AccountStatus {
        provider: Provider::Claude, id: row.id.clone(), name: row.name.clone(), state: row.state,
        email: row.email.clone(), plan: row.plan.clone(), billing: row.billing.clone(), sign_ins: 0,
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
            "id": row.id, "name": row.name, "enabled": row.enabled, "default": row.default, "billing": row.billing,
            "state": row.state, "email": row.email, "plan": row.plan,
        })).collect();
        println!("{}", serde_json::to_string_pretty(&json!({"schema": "alondra.accounts/1", "ok": true, "distribution": listing.config.distribution, "accounts": accounts}))?);
        return Ok(());
    }
    for line in lines(&listing.accounts) {
        println!("{line}");
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
        "Signing in to Claude opens your browser: run alondra accounts {command} in a terminal",
        "Iniciar sesión en Claude abre el navegador: ejecuta alondra accounts {command} en una terminal"
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
    let taken: Vec<AccountId> = seen.accounts.iter().map(|row| row.id.clone()).collect();
    let id = AccountId::for_name(name, &taken);
    next.claude.push(ProviderAccount { id: id.clone(), name: name.to_owned(), enabled: true, billing: AccountBilling::default() });
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
    if row.default && !enabled {
        let name = shown_name(&row);
        bail!(trf!(
            "{name} is Claude's own sign-in on this Mac, which nodes still run on: it can't be disabled",
            "{name} es la sesión propia de Claude en este Mac, con la que aún trabajan los nodos: no se puede desactivar"
        ));
    }
    let mut next = seen.config.clone();
    match next.claude.iter_mut().find(|account| account.id == row.id) {
        Some(account) => account.enabled = enabled,
        None => next.claude.insert(0, ProviderAccount { enabled, ..ProviderAccount::implicit_default() }),
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

#[cfg(test)]
#[path = "provider_accounts_tests.rs"]
mod tests;
