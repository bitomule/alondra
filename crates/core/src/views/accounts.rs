//! The provider accounts nodes may run on: each one's name, email, plan and state, in the user's
//! order, as the committed read index holds them. Never a folder.
use super::{Icon, Item, Lang, RenderContext, View, common, text::t};
use crate::accounts::{AccountBilling, AccountState, AccountStatus};

pub(super) fn list(context: &RenderContext<'_>) -> View {
    let lang = context.lang;
    let rows: Vec<Item> = context
        .index
        .accounts()
        .iter()
        .map(|account| {
            let icon = match account.state {
                AccountState::SignedIn => Icon::Check,
                AccountState::SignedOut | AccountState::Unavailable => Icon::Warning,
                AccountState::Unchecked | AccountState::Disabled => Icon::Machine,
            };
            let id = format!("{}:{}", account.provider.as_str(), account.id);
            let mut row = common::row(&id, "accounts", icon, name(account, lang), Some(subtitle(account, lang)));
            row.link = None;
            Item::Row(row)
        })
        .collect();
    let mut view = common::view("accounts", "accounts", Some(t!(lang, "Claude accounts", "Cuentas de Claude").into()), context, vec![]);
    if !rows.is_empty() {
        view.sections.push(common::section("claude", None, rows));
    }
    view
}

/// The account's name; the default one the user never named is called so in their language.
pub fn name(account: &AccountStatus, lang: Lang) -> String {
    if account.id.is_default() && account.name == crate::accounts::ProviderAccount::implicit_default().name {
        t!(lang, "Default", "Principal").to_owned()
    } else {
        account.name.clone()
    }
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

/// Email, plan and state, each when known: `me@example.org · max · signed in`.
pub fn subtitle(account: &AccountStatus, lang: Lang) -> String {
    let billing = match &account.billing {
        AccountBilling::PayPerUse => Some(t!(lang, "pay per use", "pago por uso").to_owned()),
        AccountBilling::Subscription { .. } => account.plan.clone(),
    };
    [account.email.clone(), billing, Some(state_label(account.state, lang).to_owned())].into_iter().flatten().collect::<Vec<_>>().join(" · ")
}
