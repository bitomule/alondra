//! How an account and its quota read in a list, a choice or a task decision.
use crate::text::{t, tf, Lang};
use alondra_core::accounts::{AccountStatus, Provider};
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
    match provider { Provider::Claude => "Claude", Provider::Codex => "Codex" }
}

pub fn window_label(provider: Provider, kind: &WindowKind, lang: Lang) -> String {
    match kind {
        WindowKind::Session if provider == Provider::Claude => "5 h".into(),
        WindowKind::Session => t!(lang, "Session", "Sesión").into(),
        WindowKind::Weekly => t!(lang, "week", "semana").into(),
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
