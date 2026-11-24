//! How an account and its quota read in a list, a choice or a task decision.
use crate::text::{t, tf, Lang};
use alondra_core::accounts::{AccountBilling, AccountState, AccountStatus, Provider};
use alondra_core::usage::WindowKind;

/// The account's name; the default one the user never named is called so in their language.
pub fn name(account: &AccountStatus, lang: Lang) -> String {
    if account.id.is_default() && account.name == alondra_core::accounts::ProviderAccount::implicit_default().name {
        t!(lang, "Default", "Principal").to_owned()
    } else {
        account.name.clone()
    }
}

/// Compact, localized native quota for account rows, choices and task decisions.
pub fn quota_summary(measured: Option<&alondra_core::usage::AccountUsage>, now: Option<alondra_core::Millis>, lang: Lang) -> String {
    let Some(usage) = measured else { return t!(lang, "Quota unknown", "Cuota desconocida").into() };
    let windows: Vec<_> = usage.windows.iter().filter(|window| !window.reading.kind.is_native_limit_marker()).collect();
    let mut parts: Vec<_> = windows.iter().map(|window| {
        let label = window_label(usage.provider, &window.reading.kind, lang);
        let used = percent(window.reading.used);
        format!("{label} {used} %")
    }).collect();
    if parts.is_empty() { parts.push(t!(lang, "Quota unknown", "Cuota desconocida").into()); }
    if native_limit_reported(Some(usage)) { parts.push(t!(lang, "Provider reported a usage limit", "El proveedor ha comunicado un límite de uso").into()); }
    let expired = windows.iter().any(|window| window.reading.resets_at.zip(now).is_some_and(|(reset, now)| reset <= now));
    if expired { parts.push(t!(lang, "Previous reading · reset has passed", "Lectura anterior · el reinicio ya ha pasado").into()); }
    else if let Some((reset, now)) = windows.iter().filter_map(|window| window.reading.resets_at).min().zip(now) {
        let duration = super::text::short_duration(reset.saturating_sub(now) as u64 / 1000);
        parts.push(tf!(lang, "resets in {duration}", "reinicia en {duration}"));
    }
    let age = windows.iter().map(|window| window.measured_at).min().zip(now).map(|(at, now)| now.saturating_sub(at));
    match age {
        Some(age) if expired || age >= 300_000 => {
            let minutes = age / 60_000;
            parts.push(tf!(lang, "reading from {minutes} min ago", "lectura de hace {minutes} min"));
        }
        None => parts.push(t!(lang, "Reading age unknown", "Antigüedad de lectura desconocida").into()),
        _ => {}
    }
    parts.join(" · ")
}

pub fn decision_quota_summary(accounts: &[AccountStatus], usage: &[alondra_core::usage::AccountUsage], now: Option<alondra_core::Millis>, lang: Lang) -> String {
    accounts.iter().map(|account| {
        let provider = provider_name(account.provider);
        let name = name(account, lang);
        let reading = usage.iter().find(|reading| reading.provider == account.provider && reading.account == account.id);
        let quota = quota_summary(reading, now, lang);
        format!("{provider} · {name}: {quota}")
    }).collect::<Vec<_>>().join("; ")
}

pub fn provider_name(provider: Provider) -> &'static str {
    provider.display_name()
}

pub fn window_label(provider: Provider, kind: &WindowKind, lang: Lang) -> String {
    match kind {
        WindowKind::Session if provider == Provider::Claude => "5 h".into(),
        WindowKind::Session => t!(lang, "Session", "Sesión").into(),
        WindowKind::Weekly => t!(lang, "week", "semana").into(),
        WindowKind::Other(_) if kind.antigravity_pool().is_some() => kind.antigravity_pool().unwrap_or_default().to_owned(),
        WindowKind::Other(name) => {
            if kind.codex_window_label().is_none() { return name.clone(); }
            match kind.duration_minutes() {
                Some(10080) => t!(lang, "week", "semana").into(),
                Some(1440) => t!(lang, "1 day", "1 día").into(),
                Some(minutes) if minutes >= 1440 && minutes % 1440 == 0 => { let days = minutes / 1440; tf!(lang, "{days} days", "{days} días") }
                Some(minutes) if minutes % 60 == 0 => format!("{} h", minutes / 60),
                Some(minutes) => super::text::short_duration(minutes.saturating_mul(60)),
                None => t!(lang, "Usage", "Uso").into(),
            }
        },
    }
}

pub fn percent(share: alondra_core::usage::Share) -> u32 {
    (share.fraction() * 100.0).round() as u32
}

pub fn native_limit_reported(measured: Option<&alondra_core::usage::AccountUsage>) -> bool { measured.is_some_and(|usage| usage.windows.iter().any(|window| window.reading.kind.is_native_limit_marker() && window.reading.exhausted)) }

pub const KEY: &str = "accounts";

/// `text` as one word for the shell, whatever it holds.
pub fn shell_word(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

pub fn state_label(state: AccountState, lang: Lang) -> &'static str {
    match state {
        AccountState::Unchecked => t!(lang, "not checked yet", "sin comprobar"),
        AccountState::SignedIn => t!(lang, "signed in", "con sesión"),
        AccountState::SignedOut => t!(lang, "signed out", "sin sesión"),
        AccountState::Unavailable => t!(lang, "can't be checked", "no se puede comprobar"),
        AccountState::Disabled => t!(lang, "disabled", "desactivada"),
    }
}

/// Email, plan and state, each when known: `me@example.org · max · signed in`. An Antigravity
/// account also says how it reaches Google, whether it keeps a key and why it could not be checked.
pub fn subtitle(account: &AccountStatus, lang: Lang) -> String {
    let antigravity = account.antigravity.as_ref();
    let access = antigravity.map(|status| access_label(status.access, lang).to_owned());
    let key = antigravity.filter(|status| status.access.takes_key() && !account.id.is_default()).map(|status| key_label(status.key_set, lang).to_owned());
    let reason = antigravity.and_then(|status| status.unavailable).filter(|_| account.state == AccountState::Unavailable).map(|why| unavailable_label(why, lang).to_owned());
    [access, account.email.clone(), billing(account, lang), key, Some(state_label(account.state, lang).to_owned()), reason].into_iter().flatten().collect::<Vec<_>>().join(" · ")
}

/// How an Antigravity account reaches Google, as a screen names it.
pub fn access_label(access: alondra_core::antigravity_account::Access, lang: Lang) -> &'static str {
    use alondra_core::antigravity_account::Access;
    match access {
        Access::Google => t!(lang, "Google account", "Cuenta de Google"),
        Access::Enterprise => "Gemini Enterprise",
        Access::ApiKey => t!(lang, "Gemini API key", "Clave de API de Gemini"),
        Access::CloudProject => t!(lang, "Google Cloud project", "Proyecto de Google Cloud"),
    }
}

/// Whether an Antigravity account keeps a key: never the key itself.
pub fn key_label(set: bool, lang: Lang) -> &'static str {
    if set { t!(lang, "Key set", "Clave puesta") } else { t!(lang, "No key", "Sin clave") }
}

/// What the user does when an Antigravity account could not be checked.
pub fn unavailable_label(why: alondra_core::antigravity_account::Unavailable, lang: Lang) -> &'static str {
    use alondra_core::antigravity_account::Unavailable;
    match why {
        Unavailable::NotInstalled => t!(lang, "Install Antigravity CLI", "Instala Antigravity CLI"),
        Unavailable::TooOld | Unavailable::UnknownVersion => t!(lang, "Update Antigravity CLI with agy update", "Actualiza Antigravity CLI con agy update"),
        Unavailable::OtherAgent => t!(lang, "The agy on this Mac is not Antigravity CLI", "El agy de este Mac no es Antigravity CLI"),
        Unavailable::Failed => t!(lang, "agy did not answer", "agy no ha respondido"),
    }
}

pub fn billing(account: &AccountStatus, lang: Lang) -> Option<String> {
    match &account.billing {
        AccountBilling::PayPerUse => Some(t!(lang, "pay per use", "pago por uso").to_owned()),
        AccountBilling::Subscription { .. } => account.plan.clone(),
    }
}

pub fn action_provider(action: &crate::action_id::ActionId) -> Option<Provider> {
    match action.extra.as_deref() { None | Some("claude") => Some(Provider::Claude), Some("codex") => Some(Provider::Codex), Some("antigravity") => Some(Provider::Antigravity), _ => None }
}

/// How the user signs an account in with the provider's own tool on the Mac, the command whole.
pub fn sign_in_hint(provider: Provider, profile: Option<&str>, lang: Lang) -> String {
    command_hint(provider, &alondra_core::accounts::sign_in_command(provider, profile), lang)
}

/// [`sign_in_hint`] for a command already built.
pub fn command_hint(provider: Provider, command: &str, lang: Lang) -> String {
    match provider {
        Provider::Claude => tf!(lang, "Open Terminal on the Mac, run {command} and sign in with /login", "Abre Terminal en el Mac, ejecuta {command} e inicia sesión con /login"),
        Provider::Codex | Provider::Antigravity => tf!(lang, "On the Mac, run {command}", "En el Mac, ejecuta {command}"),
    }
}

/// [`sign_in_hint`] for `account`. A named account whose profile folder is not known yet is named
/// instead, never sent to the default profile.
pub fn account_sign_in_hint(account: &AccountStatus, lang: Lang) -> String {
    if account.provider == Provider::Antigravity {
        return match account.sign_in_command() {
            Some(command) => command_hint(account.provider, &command, lang),
            None if account.profile.is_none() && !account.id.is_default() => named_hint(account.provider, &account.name, lang),
            None => key_hint(account, lang),
        };
    }
    match &account.profile {
        _ if account.id.is_default() => sign_in_hint(account.provider, None, lang),
        Some(profile) => sign_in_hint(account.provider, Some(profile), lang),
        None => named_hint(account.provider, &account.name, lang),
    }
}

/// How an Antigravity account that takes a key gets one: in the account's settings in the app,
/// or with `alondra accounts set-key`.
pub fn key_hint(account: &AccountStatus, lang: Lang) -> String {
    let command = format!("alondra accounts set-key antigravity/{}", account.id);
    tf!(lang, "Set its key in the account's settings, or run {command} on the Mac", "Pon su clave en los ajustes de la cuenta, o ejecuta {command} en el Mac")
}

/// How a named account whose profile folder is not known yet is signed in: by its name.
pub fn named_hint(provider: Provider, name: &str, lang: Lang) -> String {
    let provider = provider_name(provider);
    tf!(lang, "Sign {name} in to {provider} in Terminal on the Mac, with its own profile", "Inicia la sesión de {name} en {provider} en Terminal en el Mac, con su propio perfil")
}


