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
    #[serde(default)]
    pub antigravity: Option<alondra_core::accounts::AntigravityStatus>,
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
    AccountStatus { antigravity: row.antigravity.clone(),
        provider: row.provider, id: row.id.clone(), name: row.name.clone(), state: row.state,
        native_identity: None, email: row.email.clone(), plan: row.plan.clone(), billing: row.billing.clone(), sign_ins: 0, profile: row.profile.clone(),
    }
}

fn lang() -> alondra_view_model::Lang {
    match crate::i18n::lang() {
        crate::i18n::Lang::En => alondra_view_model::Lang::En,
        crate::i18n::Lang::Es => alondra_view_model::Lang::Es,
    }
}

fn state_text(row: &Row) -> String {
    alondra_view_model::accounts::subtitle(&status(row), lang())
}

fn shown_name(row: &Row) -> String {
    alondra_view_model::accounts::name(&status(row), lang())
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
            "state": row.state, "email": row.email, "plan": row.plan, "antigravity": row.antigravity,
        })).collect();
        println!("{}", serde_json::to_string_pretty(&json!({"schema": "alondra.accounts/1", "ok": true, "distribution": listing.config.distribution, "accounts": accounts}))?);
        return Ok(());
    }
    let mixed = Provider::ALL.iter().filter(|provider| listing.accounts.iter().any(|row| row.provider == **provider)).count() > 1;
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
pub fn add(provider: Provider, name: &str, antigravity: Option<alondra_core::antigravity_account::Settings>) -> Result<()> {
    if antigravity.is_some() && provider != Provider::Antigravity {
        bail!(tr!("--access, --project, --location and --credentials-file are for Antigravity accounts: add --provider antigravity",
            "--access, --project, --location y --credentials-file son para cuentas de Antigravity: añade --provider antigravity"));
    }
    for line in add_with(&daemon(), provider, name, antigravity)? { println!("{line}"); }
    Ok(())
}

fn add_with(client: &Client, provider: Provider, name: &str, antigravity: Option<alondra_core::antigravity_account::Settings>) -> Result<Vec<String>> {
    let name = name.trim();
    if name.is_empty() {
        bail!(tr!("Give the account a name: alondra accounts add Work", "Dale un nombre a la cuenta: alondra accounts add Trabajo"));
    }
    let seen = listing(client)?;
    if !seen.supported_providers.contains(&provider) { bail!(tr!("Update Alondra before adding an account of this provider", "Actualiza Alondra antes de añadir una cuenta de este proveedor")); }
    if seen.config.find(provider, name).is_some_and(|account| account.name.trim().eq_ignore_ascii_case(name)) {
        bail!(trf!("There is already an account called {name}", "Ya hay una cuenta que se llama {name}"));
    }
    let next = seen.config.changed(provider, &AccountChange::Add { antigravity, name: name.into() }).map_err(anyhow::Error::msg)?;
    let listed = save(client, &seen, &next)?;
    let row = listed.accounts.iter().find(|row| row.provider == provider && row.name.trim().eq_ignore_ascii_case(name)).context("the account is not in the listing")?;
    let state = state_text(row);
    let mut lines = vec![trf!("Added {name}: {state}", "Añadida {name}: {state}")];
    if row.state != AccountState::SignedIn {
        lines.push(alondra_view_model::accounts::account_sign_in_hint(&status(row), lang()));
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
    Ok((row.state != AccountState::SignedIn).then(|| alondra_view_model::accounts::account_sign_in_hint(&status(row), lang())))
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
        .ok_or_else(|| { let provider = provider.display_name(); anyhow!(trf!("No {provider} account is called {name}", "No hay ninguna cuenta de {provider} que se llame {name}")) })
}

pub fn list_for(provider: Provider, json: bool) -> Result<()> {
    if provider == Provider::Claude { return list(json); }
    let listing = check(&daemon())?;
    let rows: Vec<Row> = listing.accounts.into_iter().filter(|row| row.provider == provider).collect();
    if json {
        let rows: Vec<Value> = rows.iter().map(|row| json!({"provider":row.provider,"id":row.id,"name":row.name,"state":row.state,"email":row.email,"plan":row.plan,"enabled":row.enabled,"default":row.default,"billing":row.billing,"antigravity":row.antigravity})).collect();
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

/// The provider and account `name` names: `antigravity/<id or name>`, or a bare one of `provider`.
pub(crate) fn named_account(provider: Provider, name: &str) -> (Provider, String) {
    let name = name.trim();
    match name.split_once('/').and_then(|(prefix, rest)| Provider::parse(prefix).ok().map(|provider| (provider, rest.trim()))) {
        Some((provider, rest)) => (provider, rest.to_owned()),
        None => (provider, name.to_owned()),
    }
}

/// Sets the key of the named Antigravity account `name`, read from standard input without echoing
/// it, or removes it (`clear`). It goes to the daemon as the account's credential action, a signed
/// request to the daemon on this Mac; Alondra keeps it in a file of the account's folder only the
/// user can read. A terminal it may not ask on (`no_input`) gets no prompt.
pub fn set_key(provider: Provider, name: &str, clear: bool, no_input: bool) -> Result<()> {
    let (provider, name) = named_account(provider, name);
    if provider != Provider::Antigravity {
        bail!(tr!("Only an Antigravity account keeps a key: alondra accounts set-key antigravity/<account>",
            "Solo una cuenta de Antigravity guarda una clave: alondra accounts set-key antigravity/<cuenta>"));
    }
    let seen = listing(&daemon())?;
    let row = provider_row(&seen, provider, &name)?.clone();
    if row.default {
        bail!(tr!("The default account is agy's own sign-in: add a named account for a key",
            "La cuenta principal es la sesión propia de agy: añade una cuenta con nombre para usar una clave"));
    }
    let shown = shown_name(&row);
    if !row.antigravity.as_ref().is_some_and(|status| status.access.takes_key()) {
        bail!(trf!("{shown} signs in with Google and keeps no key", "{shown} inicia sesión con Google y no guarda ninguna clave"));
    }
    let key = if clear { None } else { Some(read_secret(&trf!("Key for {shown}: ", "Clave para {shown}: "), no_input)?) };
    let client = crate::client::Client::for_user(Duration::from_secs(120));
    let view = client.views(alondra_view_model::accounts::KEY, None).context(tr!("Alondra is not running on this Mac: start it and try again", "Alondra no está en marcha en este Mac: arráncalo y vuelve a probar"))?;
    let wanted = |action: &alondra_view_model::Action| alondra_view_model::action_id::ActionId::parse(&action.id).is_ok_and(|id|
        id.verb == alondra_view_model::action_id::Verb::AccountCredential && id.subject == row.id.as_str() && id.extra.as_deref() == Some(provider.as_str()))
        && (action.option_id.as_deref() == Some("clear")) == clear;
    let action = view.sections.iter().flat_map(|section| &section.items).filter_map(|item| match item {
        alondra_view_model::Item::Row(row) => Some(row.actions.iter()),
        _ => None,
    }).flatten().find(|action| wanted(action)).cloned()
        .with_context(|| if clear { trf!("{shown} has no key to remove", "{shown} no tiene ninguna clave que quitar") } else { tr!("Update Alondra to set an account's key", "Actualiza Alondra para poner la clave de una cuenta").into() })?;
    let input = match &key { Some(key) => json!({"text": key.expose()}), None => json!({}) };
    let reply = client.action(&view.data_epoch, &action.id, action.option_id.as_deref(), input)?;
    drop(key);
    if reply["outcome"] != "applied" {
        bail!("{}", reply["message"].as_str().unwrap_or("the key could not be saved"));
    }
    let listed = listing(&daemon())?;
    let state = provider_row(&listed, provider, row.id.as_str()).map(state_text).unwrap_or_default();
    println!("{}", if clear { trf!("Removed the key of {shown}: {state}", "Quitada la clave de {shown}: {state}") } else { trf!("Saved the key of {shown}: {state}", "Guardada la clave de {shown}: {state}") });
    Ok(())
}

/// The terminal's settings before the key prompt hid what is typed, for an interrupt to put back.
static TYPED: std::sync::OnceLock<libc::termios> = std::sync::OnceLock::new();

extern "C" fn interrupted(signal: libc::c_int) {
    if let Some(attributes) = TYPED.get() { unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, attributes) }; }
    unsafe { libc::_exit(128 + signal) };
}

/// Whether the key is asked on a terminal (`Some(true)`), read from a pipe (`Some(false)`), or
/// needs a person it may not ask (`None`).
pub(crate) fn key_prompt(stdin_terminal: bool, may_ask: bool) -> Option<bool> {
    match (stdin_terminal, may_ask) { (false, _) => Some(false), (true, true) => Some(true), (true, false) => None }
}

/// One line of standard input, without echoing it when it is a terminal: an interrupt puts the
/// terminal's echo back before it ends the command.
fn read_secret(prompt: &str, no_input: bool) -> Result<alondra_core::antigravity_account::Secret> {
    use std::io::{BufRead as _, IsTerminal as _, Write as _};
    let stdin = std::io::stdin();
    let may_ask = !no_input && std::env::var("ALONDRA_NO_INPUT").as_deref() != Ok("1");
    let Some(terminal) = key_prompt(stdin.is_terminal(), may_ask) else {
        return Err(crate::commands::NeedsPerson(tr!("Setting a key needs you to type it: run alondra accounts set-key in a terminal, or pipe the key to it",
            "Poner una clave necesita que la escribas: ejecuta alondra accounts set-key en una terminal, o pásale la clave por una tubería").into()).into());
    };
    let mut saved: Option<libc::termios> = None;
    if terminal {
        eprint!("{prompt}");
        let _ = std::io::stderr().flush();
        let mut attributes = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut attributes) } == 0 {
            saved = Some(attributes);
            let _ = TYPED.set(attributes);
            for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
                unsafe { libc::signal(signal, interrupted as *const () as libc::sighandler_t) };
            }
            attributes.c_lflag &= !libc::ECHO;
            unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &attributes) };
        }
    }
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);
    if let Some(attributes) = saved {
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &attributes) };
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] { unsafe { libc::signal(signal, libc::SIG_DFL) }; }
        eprintln!();
    }
    read.context(tr!("Could not read the key", "No se pudo leer la clave"))?;
    let key = alondra_core::antigravity_account::Secret::new(line.trim().to_owned());
    line.clear();
    if key.expose().is_empty() { bail!(tr!("Write the key, or use --clear to remove it", "Escribe la clave, o usa --clear para quitarla")); }
    Ok(key)
}

#[cfg(test)]
#[path = "provider_accounts_tests.rs"]
mod tests;
