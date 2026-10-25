//! Settings › Claude accounts: each account in the user's order with its email, plan, weight,
//! state, usage and nodes, as the committed read index holds them, never a folder; how new work
//! is spread over them; and adding one, which only asks for a name and signs it in on the Mac.
use super::action_id::Verb;
use super::text::{t, tf};
use super::{common, Action, Confirm, Icon, Input, Item, Label, Lang, Link, LocalLink, Presentation, RenderContext, Role, Style, Tone, View};
use crate::accounts::{AccountBilling, AccountChange, AccountId, AccountState, AccountStatus, Policy, Provider};
use crate::usage::WindowKind;

pub const KEY: &str = "accounts";
/// What the Mac runs to add an account, followed by its name.
pub const ADD_COMMAND: &str = "alondra accounts add";

const WEIGHTS: [u32; 7] = [1, 2, 3, 5, 10, 15, 20];

/// `text` as one word for the shell, whatever it holds.
pub fn shell_word(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

pub(super) fn list(context: &RenderContext<'_>) -> View {
    let lang = context.lang;
    let accounts: Vec<&AccountStatus> = context.index.accounts().iter().filter(|account| account.provider == Provider::Claude).collect();
    let mut rows: Vec<Item> = accounts.iter().map(|account| account_row(account, context)).collect();
    rows.push(add_row(lang));
    let mut view = common::view(KEY, KEY, Some(t!(lang, "Claude accounts", "Cuentas de Claude").into()), context, vec![]);
    view.sections.push(list_style(common::section("claude", None, rows)));
    if accounts.len() > 1 {
        view.sections.push(list_style(common::section("policy", Some(t!(lang, "New work", "Trabajo nuevo").into()),
            vec![policy_row(context), moves_row(context)])));
    }
    view
}

fn list_style(mut section: super::Section) -> super::Section {
    section.style = Some(super::SectionStyle::List);
    section
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
    [account.email.clone(), billing(account, lang), Some(state_label(account.state, lang).to_owned())].into_iter().flatten().collect::<Vec<_>>().join(" · ")
}

fn billing(account: &AccountStatus, lang: Lang) -> Option<String> {
    match &account.billing {
        AccountBilling::PayPerUse => Some(t!(lang, "pay per use", "pago por uso").to_owned()),
        AccountBilling::Subscription { .. } => account.plan.clone(),
    }
}

fn state_badge(state: AccountState, lang: Lang) -> Label {
    let (text, tone) = match state {
        AccountState::SignedIn => (t!(lang, "Signed in", "Con sesión"), Tone::Success),
        AccountState::SignedOut => (t!(lang, "Signed out", "Sin sesión"), Tone::YourTurn),
        AccountState::Unavailable => (t!(lang, "Can't be checked", "No se puede comprobar"), Tone::Critical),
        AccountState::Unchecked => (t!(lang, "Not checked yet", "Sin comprobar"), Tone::Neutral),
        AccountState::Disabled => (t!(lang, "Off", "Desactivada"), Tone::Neutral),
    };
    Label { text: text.into(), tone, elapsed: None }
}

/// The weight the account counts for, and where it comes from.
fn weight_text(account: &AccountStatus, measured_plan: Option<&str>, lang: Lang) -> Option<String> {
    let weight = crate::usage::weight(&account.billing, measured_plan.or(account.plan.as_deref()))?;
    Some(match &account.billing {
        AccountBilling::Subscription { weight: Some(_), .. } => tf!(lang, "weight {weight}, set by you", "peso {weight}, fijado por ti"),
        _ => tf!(lang, "weight {weight}, by plan", "peso {weight}, por plan"),
    })
}

fn percent(share: crate::usage::Share) -> u32 {
    (share.fraction() * 100.0).round() as u32
}

/// `5 h 40 % · week 30 % · 2 nodes`, each part when known.
fn usage_line(account: &AccountStatus, context: &RenderContext<'_>) -> Option<String> {
    let lang = context.lang;
    let measured = context.index.account_usage().iter().find(|usage| usage.provider == account.provider && usage.account == account.id);
    let used = |kind: &WindowKind| measured.and_then(|usage| usage.window(kind)).map(|window| percent(window.reading.used));
    let mut parts = vec![];
    if let Some(session) = used(&WindowKind::Session) {
        parts.push(tf!(lang, "5 h {session} %", "5 h {session} %"));
    }
    if let Some(week) = used(&WindowKind::Weekly) {
        parts.push(tf!(lang, "week {week} %", "semana {week} %"));
    }
    match context.index.nodes_on_account(&account.id) {
        0 => {}
        1 => parts.push(t!(lang, "1 node", "1 nodo").into()),
        nodes => parts.push(tf!(lang, "{nodes} nodes", "{nodes} nodos")),
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn allowed(context: &RenderContext<'_>, change: AccountChange) -> Result<(), crate::domain::Refusal> {
    let available = context.domain.account_action(Provider::Claude, change);
    match available.refusal {
        Some(refusal) if !available.enabled => Err(refusal),
        _ => Ok(()),
    }
}

fn change_action(verb: Verb, account: &AccountId, change: AccountChange, label: &str, style: Style, context: &RenderContext<'_>) -> Option<Action> {
    allowed(context, change).ok()?;
    common::action(verb, account.as_str(), context.domain.revision, label, style)
}

fn account_row(account: &AccountStatus, context: &RenderContext<'_>) -> Item {
    let lang = context.lang;
    let shown = name(account, lang);
    let measured_plan = context.index.account_usage().iter()
        .find(|usage| usage.provider == account.provider && usage.account == account.id)
        .and_then(|usage| usage.plan.as_deref());
    let about = [account.email.clone(), billing(account, lang), weight_text(account, measured_plan, lang)].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    let subtitle = [Some(about), usage_line(account, context)].into_iter().flatten().filter(|line| !line.is_empty()).collect::<Vec<_>>().join("\n");
    let id = format!("{}-{}", account.provider.as_str(), account.id);
    let mut row = common::row(&id, KEY, Icon::Machine, shown.clone(), (!subtitle.is_empty()).then_some(subtitle));
    row.link = None;
    row.badges.push(state_badge(account.state, lang));
    if account.state == AccountState::SignedOut {
        let mut sign_in = common::view_link("", t!(lang, "Sign in", "Iniciar sesión"));
        sign_in.link = Some(Link::Local { local: LocalLink::AccountSignIn {
            account: shown.clone(),
            command: format!("alondra accounts login {}", shell_word(&shown)),
        } });
        sign_in.style = Style::Primary;
        sign_in.role = Some(Role::Advance);
        sign_in.description = Some(t!(lang, "Opens Claude's sign-in on the Mac", "Abre el inicio de sesión de Claude en el Mac").into());
        row.actions.push(sign_in);
    }
    row.actions.extend(weight_action(account, measured_plan, context));
    let up = AccountChange::MoveUp { account: account.id.clone() };
    row.actions.extend(change_action(Verb::AccountUp, &account.id, up, t!(lang, "Move up", "Subir"), Style::Secondary, context));
    let down = AccountChange::MoveDown { account: account.id.clone() };
    row.actions.extend(change_action(Verb::AccountDown, &account.id, down, t!(lang, "Move down", "Bajar"), Style::Secondary, context));
    let enable = AccountChange::Enable { account: account.id.clone() };
    row.actions.extend(change_action(Verb::AccountEnable, &account.id, enable, t!(lang, "Turn on", "Activar"), Style::Secondary, context));
    let disable = AccountChange::Disable { account: account.id.clone() };
    if let Some(mut off) = change_action(Verb::AccountDisable, &account.id, disable, t!(lang, "Turn off", "Desactivar"), Style::Secondary, context) {
        off.description = Some(t!(lang, "No new work starts on it; what runs on it goes on", "No empieza trabajo nuevo en ella; lo que ya trabaja sigue").into());
        row.actions.push(off);
    }
    row.actions.extend(remove_action(account, &shown, context));
    Item::Row(row)
}

/// The weight to count the account for: its plan's, or one the user picks.
fn weight_action(account: &AccountStatus, measured_plan: Option<&str>, context: &RenderContext<'_>) -> Option<Action> {
    let lang = context.lang;
    let mut action = common::action(Verb::AccountWeight, account.id.as_str(), context.domain.revision, t!(lang, "Weight", "Peso"), Style::Secondary)?;
    action.presentation = Presentation::Menu;
    action.description = Some(t!(lang, "How much room it counts for when work is spread, as plans do: Pro 1, Max 5x 5, Max 20x 10",
        "Cuánto margen cuenta al repartir el trabajo, como los planes: Pro 1, Max 5x 5, Max 20x 10").into());
    let option = |id: String, label: String, description: Option<String>| super::ActionOption {
        id, label, description, style: Style::Secondary, input: Input::None, key: None,
    };
    let by_plan = crate::usage::plan_weight(measured_plan.or(account.plan.as_deref()));
    if allowed(context, AccountChange::SetWeight { account: account.id.clone(), weight: None }).is_ok() {
        action.options.push(option("auto".into(), tf!(lang, "By plan ({by_plan})", "Por plan ({by_plan})"), None));
    }
    for weight in WEIGHTS {
        if allowed(context, AccountChange::SetWeight { account: account.id.clone(), weight: Some(weight) }).is_ok() {
            action.options.push(option(weight.to_string(), weight.to_string(), None));
        }
    }
    (!action.options.is_empty()).then_some(action)
}

/// Removing signs the account out and forgets it; while open tasks run on it, it says why it can't.
fn remove_action(account: &AccountStatus, shown: &str, context: &RenderContext<'_>) -> Option<Action> {
    let lang = context.lang;
    let refused = allowed(context, AccountChange::Remove { account: account.id.clone() }).err();
    if refused.as_ref().is_some_and(|refusal| !matches!(refusal, crate::domain::Refusal::AccountInUse { .. })) {
        return None;
    }
    let mut remove = common::action(Verb::AccountRemove, account.id.as_str(), context.domain.revision, t!(lang, "Remove", "Quitar"), Style::Destructive)?;
    if let Some(refusal) = refused {
        remove.enabled = false;
        remove.disabled_reason = Some(super::messages::refusal_text(&refusal, lang));
        return Some(remove);
    }
    remove.confirm = Some(Confirm {
        title: tf!(lang, "Remove {shown}?", "¿Quitar {shown}?"),
        message: tf!(lang,
            "Alondra signs {shown} out of Claude on this Mac and forgets it. Your Claude account itself stays as it is.",
            "Alondra cierra la sesión de {shown} en Claude en este Mac y la olvida. Tu cuenta de Claude sigue como está."),
        button: t!(lang, "Remove", "Quitar").into(),
    });
    remove.stamp = Some(t!(lang, "Removed", "Quitada").into());
    Some(remove)
}

fn add_row(lang: Lang) -> Item {
    let mut row = common::row("claude-add", KEY, Icon::Action, t!(lang, "Add an account", "Añadir una cuenta").into(),
        Some(t!(lang, "It asks only for a name; then you sign in to Claude on the Mac", "Solo pide un nombre; luego inicias sesión en Claude en el Mac").into()));
    row.link = None;
    let mut add = common::view_link("", t!(lang, "Add", "Añadir"));
    add.link = Some(Link::Local { local: LocalLink::AccountAdd { command: ADD_COMMAND.into() } });
    add.style = Style::Primary;
    add.role = Some(Role::Advance);
    row.actions.push(add);
    Item::Row(row)
}

fn policy_text(policy: Policy, lang: Lang) -> (&'static str, &'static str) {
    match policy {
        Policy::Margin => (t!(lang, "Most room first", "Primero la de más margen"),
            t!(lang, "Each new task starts on the account with the most room left, weighed by its plan",
                "Cada tarea nueva empieza en la cuenta con más margen, según su plan")),
        Policy::Chain => (t!(lang, "In list order", "En el orden de la lista"),
            t!(lang, "Each new task starts on the first account in the list that has room left",
                "Cada tarea nueva empieza en la primera cuenta de la lista que tenga margen")),
    }
}

/// Whether a node whose account reaches its limit goes on with another account, off unless the user
/// turns it on, always with what it risks.
fn moves_row(context: &RenderContext<'_>) -> Item {
    let lang = context.lang;
    let on = context.index.account_moves();
    let mut row = common::row("claude-moves", KEY, Icon::Node,
        t!(lang, "Move a node when its account runs out", "Mover un nodo cuando su cuenta se agota").into(),
        Some(t!(lang, "Anthropic doesn't say whether switching accounts when one reaches its limit is allowed.",
            "Anthropic no dice si cambiar de cuenta al llegar al límite está permitido.").into()));
    row.link = None;
    row.badges.push(Label {
        text: if on { t!(lang, "On", "Activado") } else { t!(lang, "Off", "Desactivado") }.into(),
        tone: if on { Tone::Warning } else { Tone::Neutral },
        elapsed: None,
    });
    if allowed(context, AccountChange::SetMoveWhenExhausted { on: !on }).is_ok() {
        let label = if on { t!(lang, "Turn off", "Desactivar") } else { t!(lang, "Turn on", "Activar") };
        if let Some(mut switch) = common::action(Verb::AccountMoves, "moves", context.domain.revision, label, Style::Secondary) {
            switch.option_id = Some(if on { "off" } else { "on" }.into());
            switch.description = Some(if on {
                t!(lang, "A node whose account reaches its limit waits for it to reset", "Un nodo cuya cuenta llega al límite espera a que se reinicie")
            } else {
                t!(lang, "The node goes on with another account, in the same conversation", "El nodo sigue con otra cuenta, en la misma conversación")
            }.into());
            row.actions.push(switch);
        }
    }
    Item::Row(row)
}

fn policy_row(context: &RenderContext<'_>) -> Item {
    let lang = context.lang;
    let policy = context.index.account_policy();
    let (title, about) = policy_text(policy, lang);
    let mut row = common::row("claude-policy", KEY, Icon::Node, title.into(), Some(about.into()));
    row.link = None;
    let other = match policy {
        Policy::Margin => Policy::Chain,
        Policy::Chain => Policy::Margin,
    };
    if allowed(context, AccountChange::SetPolicy { policy: other }).is_ok()
        && let Some(mut change) = common::action(Verb::AccountPolicy, "policy", context.domain.revision, t!(lang, "Change", "Cambiar"), Style::Secondary)
    {
        let (label, description) = policy_text(other, lang);
        let id = match other {
            Policy::Margin => "margin",
            Policy::Chain => "chain",
        };
        change.presentation = Presentation::Menu;
        change.options.push(super::ActionOption {
            id: id.into(), label: label.into(), description: Some(description.into()), style: Style::Secondary, input: Input::None, key: None,
        });
        row.actions.push(change);
    }
    Item::Row(row)
}
