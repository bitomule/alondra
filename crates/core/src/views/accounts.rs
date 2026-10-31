//! Settings › provider accounts: each account in the user's order with its email, plan, weight,
//! state, usage and nodes, as the committed read index holds them, never a folder; how new work
//! is spread over them; and adding one, which only asks for a name and signs it in on the Mac.
use super::action_id::Verb;
use super::text::{t, tf};
use super::{common, Action, Confirm, Icon, Input, Item, Label, Lang, Link, LocalLink, Presentation, RenderContext, Role, Style, Tone, View};
use crate::accounts::{AccountBilling, AccountChange, AccountId, AccountKey, AccountState, AccountStatus, Policy, Provider};
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
    let order = context.index.account_order();
    let positions: std::collections::BTreeMap<_, _> = order.iter().enumerate().map(|(at, key)| (key.clone(), at)).collect();
    let mut accounts: Vec<&AccountStatus> = context.index.accounts().iter().collect();
    accounts.sort_by_key(|account| positions.get(&AccountKey { provider: account.provider, account: account.id.clone() }).copied().unwrap_or(usize::MAX));
    let measured: std::collections::BTreeMap<_, _> = context.index.account_usage().iter()
        .map(|usage| (AccountKey { provider: usage.provider, account: usage.account.clone() }, usage)).collect();
    let mut rows: Vec<Item> = accounts.iter().enumerate().map(|(at, account)| {
        let key = AccountKey { provider: account.provider, account: account.id.clone() };
        account_row(account, at + 1, measured.get(&key).copied(), context)
    }).collect();
    rows.splice(0..0, context.index.account_logins().iter().filter_map(|login| login_row(login, context)));
    let mut view = common::view(KEY, KEY, Some(t!(lang, "Accounts", "Cuentas").into()), context, vec![]);
    view.account_configuration = (!context.index.account_config_version().is_empty()).then(|| super::AccountConfigurationPresentation { version: context.index.account_config_version().into() });
    let mut section = list_style(common::section("accounts", None, std::mem::take(&mut rows)));
    if accounts.len() > 1 && !context.index.account_config_version().is_empty() {
        if let Some(mut reorder) = common::action(Verb::AccountOrder, "accounts", context.domain.revision, t!(lang, "Reorder accounts", "Reordenar cuentas"), Style::Secondary) {
            reorder.id = super::action_id::ActionId::new(Verb::AccountOrder, "accounts", context.domain.revision,
                Some(context.index.account_config_version().into())).expect("account config version is an action identifier").to_string();
            section.actions.push(reorder);
        }
    }
    if let Some(check) = common::action(Verb::AccountCheck, "accounts", context.domain.revision, t!(lang, "Check accounts", "Comprobar cuentas"), Style::Secondary) { section.actions.push(check); }
    view.sections.push(section);
    let provider_rows = [Provider::Claude, Provider::Codex].into_iter().map(|provider| {
        if accounts.iter().any(|account| account.provider == provider) { add_row(provider, lang) } else { connect_row(provider, context) }
    }).collect();
    view.sections.push(list_style(common::section("providers", Some(t!(lang, "Providers", "Proveedores").into()), provider_rows)));
    if accounts.len() > 1 {
        let mut policy = vec![policy_row(context)];
        if legacy_margin_available(context) { policy.push(moves_row(context)); }
        view.sections.push(list_style(common::section("policy", Some(t!(lang, "New work", "Trabajo nuevo").into()), policy)));
    }
    let profiles = super::model_controls::profile_rows(context);
    if !profiles.is_empty() { view.sections.push(list_style(common::section("model_profiles", Some(t!(lang, "Models by role", "Modelos por función").into()), profiles))); }
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

fn native_limit_reported(measured: Option<&crate::usage::AccountUsage>) -> bool { measured.is_some_and(|usage| usage.windows.iter().any(|window| window.reading.kind.is_native_limit_marker() && window.reading.exhausted)) }

fn percent(share: crate::usage::Share) -> u32 {
    (share.fraction() * 100.0).round() as u32
}

/// `5 h 40 % · week 30 % · 2 nodes`, each part when known.
fn usage_line(account: &AccountStatus, measured: Option<&crate::usage::AccountUsage>, context: &RenderContext<'_>) -> Option<String> {
    let lang = context.lang;
    let mut parts = vec![];
    if native_limit_reported(measured) { parts.push(t!(lang, "Provider reported a usage limit", "El proveedor ha comunicado un límite de uso").into()); }
    for window in measured.into_iter().flat_map(|usage| &usage.windows).filter(|window| !window.reading.kind.is_native_limit_marker()) {
        let label = window_label(account.provider, &window.reading.kind, lang);
        let used = percent(window.reading.used);
        parts.push(format!("{label} {used} %"));
    }
    match context.index.nodes_on_account_key(&AccountKey { provider: account.provider, account: account.id.clone() }) {
        0 => {}
        1 => parts.push(t!(lang, "1 node", "1 nodo").into()),
        nodes => parts.push(tf!(lang, "{nodes} nodes", "{nodes} nodos")),
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn allowed(context: &RenderContext<'_>, provider: Provider, change: AccountChange) -> Result<(), crate::domain::Refusal> {
    let available = context.domain.account_action(provider, change);
    match available.refusal {
        Some(refusal) if !available.enabled => Err(refusal),
        _ => Ok(()),
    }
}

fn change_action(verb: Verb, account: &AccountStatus, change: AccountChange, label: &str, style: Style, context: &RenderContext<'_>) -> Option<Action> {
    allowed(context, account.provider, change).ok()?;
    scoped_action(verb, account, label, style, context)
}

fn account_row(account: &AccountStatus, position: usize, measured: Option<&crate::usage::AccountUsage>, context: &RenderContext<'_>) -> Item {
    let lang = context.lang;
    let shown = name(account, lang);
    let measured_plan = measured.and_then(|usage| usage.plan.as_deref());
    let legacy_weight = (legacy_margin_available(context) && context.index.account_policy() == Policy::Margin).then(|| weight_text(account, measured_plan, lang)).flatten();
    let about = [Some(provider_name(account.provider).into()), account.email.clone(), billing(account, lang), legacy_weight].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    let subtitle = [Some(about), usage_line(account, measured, context)].into_iter().flatten().filter(|line| !line.is_empty()).collect::<Vec<_>>().join("\n");
    let id = format!("{}-{}", account.provider.as_str(), account.id);
    let mut row = common::row(&id, KEY, Icon::Machine, shown.clone(), (!subtitle.is_empty()).then_some(subtitle));
    row.link = None;
    row.account = Some(account_presentation(account, position, measured, context));
    row.badges.push(state_badge(account.state, lang));
    if account.provider == Provider::Codex && account.state == AccountState::SignedOut {
        row.actions.extend(login_action(&AccountKey::new(Provider::Codex, account.id.clone()), None, context));
    } else if account.state == AccountState::SignedOut {
        let mut sign_in = common::view_link("", t!(lang, "Sign in", "Iniciar sesión"));
        sign_in.link = Some(Link::Local { local: LocalLink::AccountSignIn {
            account: if account.provider == Provider::Claude { shown.clone() } else { account.id.as_str().into() },
            command: login_command(account.provider, account.id.as_str()),
            provider: (account.provider != Provider::Claude).then(|| account.provider.as_str().into()),
            argv: (account.provider != Provider::Claude).then(|| login_arguments(account.provider, account.id.as_str())),
        } });
        sign_in.style = Style::Primary;
        sign_in.role = Some(Role::Advance);
        let provider = provider_name(account.provider);
        sign_in.description = Some(tf!(lang, "Opens {provider} sign-in on the Mac", "Abre el inicio de sesión de {provider} en el Mac"));
        row.actions.push(sign_in);
    }
    if legacy_margin_available(context) && context.index.account_policy() == Policy::Margin { row.actions.extend(weight_action(account, measured_plan, context)); }
    let up = AccountChange::MoveUp { account: account.id.clone() };
    row.actions.extend(change_action(Verb::AccountUp, account, up, t!(lang, "Move up", "Subir"), Style::Secondary, context));
    let down = AccountChange::MoveDown { account: account.id.clone() };
    row.actions.extend(change_action(Verb::AccountDown, account, down, t!(lang, "Move down", "Bajar"), Style::Secondary, context));
    let enable = AccountChange::Enable { account: account.id.clone() };
    row.actions.extend(change_action(Verb::AccountEnable, account, enable, t!(lang, "Turn on", "Activar"), Style::Secondary, context));
    let disable = AccountChange::Disable { account: account.id.clone() };
    if let Some(mut off) = change_action(Verb::AccountDisable, account, disable, t!(lang, "Turn off", "Desactivar"), Style::Secondary, context) {
        off.description = Some(t!(lang, "No new work starts on it; what runs on it goes on", "No empieza trabajo nuevo en ella; lo que ya trabaja sigue").into());
        row.actions.push(off);
    }
    if let Some(mut rename) = scoped_action(Verb::AccountRename, account, t!(lang, "Rename", "Renombrar"), Style::Secondary, context) {
        rename.input = Input::Text { placeholder: shown.clone(), required: true, multiline: false, secret: false };
        row.actions.push(rename);
    }
    if let Some(refresh) = scoped_action(Verb::AccountModels, account, t!(lang, "Refresh models", "Actualizar modelos"), Style::Secondary, context) { row.actions.push(refresh); }
    row.actions.extend(remove_action(account, &shown, context));
    Item::Row(row)
}

/// The weight to count the account for: its plan's, or one the user picks.
fn weight_action(account: &AccountStatus, measured_plan: Option<&str>, context: &RenderContext<'_>) -> Option<Action> {
    let lang = context.lang;
    let mut action = scoped_action(Verb::AccountWeight, account, t!(lang, "Weight", "Peso"), Style::Secondary, context)?;
    action.presentation = Presentation::Menu;
    action.description = Some(t!(lang, "How much room it counts for when work is spread, as plans do: Pro 1, Max 5x 5, Max 20x 10",
        "Cuánto margen cuenta al repartir el trabajo, como los planes: Pro 1, Max 5x 5, Max 20x 10").into());
    let option = |id: String, label: String, description: Option<String>| super::ActionOption {
        id, label, description, style: Style::Secondary, input: Input::None, key: None,
    };
    let by_plan = crate::usage::plan_weight(measured_plan.or(account.plan.as_deref()));
    if allowed(context, account.provider, AccountChange::SetWeight { account: account.id.clone(), weight: None }).is_ok() {
        action.options.push(option("auto".into(), tf!(lang, "By plan ({by_plan})", "Por plan ({by_plan})"), None));
    }
    for weight in WEIGHTS {
        if allowed(context, account.provider, AccountChange::SetWeight { account: account.id.clone(), weight: Some(weight) }).is_ok() {
            action.options.push(option(weight.to_string(), weight.to_string(), None));
        }
    }
    (!action.options.is_empty()).then_some(action)
}

/// Removing signs the account out and forgets it; while open tasks run on it, it says why it can't.
fn remove_action(account: &AccountStatus, shown: &str, context: &RenderContext<'_>) -> Option<Action> {
    let lang = context.lang;
    let refused = allowed(context, account.provider, AccountChange::Remove { account: account.id.clone() }).err();
    if refused.as_ref().is_some_and(|refusal| !matches!(refusal, crate::domain::Refusal::AccountInUse { .. })) {
        return None;
    }
    let mut remove = scoped_action(Verb::AccountRemove, account, t!(lang, "Remove", "Quitar"), Style::Destructive, context)?;
    if let Some(refusal) = refused {
        remove.enabled = false;
        remove.disabled_reason = Some(super::messages::refusal_text(&refusal, lang));
        return Some(remove);
    }
    let provider = provider_name(account.provider);
    remove.confirm = Some(Confirm {
        title: tf!(lang, "Remove {shown}?", "¿Quitar {shown}?"),
        message: tf!(lang,
            "Alondra signs {shown} out of {provider} on this Mac and forgets it. Your provider account itself stays as it is.",
            "Alondra cierra la sesión de {shown} en {provider} en este Mac y la olvida. Tu cuenta del proveedor sigue como está."),
        button: t!(lang, "Remove", "Quitar").into(),
    });
    remove.stamp = Some(t!(lang, "Removed", "Quitada").into());
    Some(remove)
}

fn add_row(provider: Provider, lang: Lang) -> Item {
    let name = provider_name(provider);
    let id = format!("{}-add", provider.as_str());
    let mut row = common::row(&id, KEY, Icon::Action, tf!(lang, "Add a {name} account", "Añadir una cuenta de {name}"),
        Some(t!(lang, "Choose a name, then finish native sign-in on the Mac", "Elige un nombre y completa el inicio de sesión nativo en el Mac").into()));
    row.link = None;
    let mut add = common::view_link("", t!(lang, "Add", "Añadir"));
    add.link = Some(Link::Local { local: LocalLink::AccountAdd {
        command: if provider == Provider::Claude { ADD_COMMAND.into() } else { format!("alondra accounts add --provider {}", provider.as_str()) },
        provider: (provider != Provider::Claude).then(|| provider.as_str().into()),
        argv: (provider != Provider::Claude).then(|| vec!["accounts".into(), "add".into(), "--provider".into(), provider.as_str().into()]),
    } });
    add.style = Style::Primary;
    add.role = Some(Role::Advance);
    row.actions.push(add);
    Item::Row(row)
}

fn connect_row(provider: Provider, context: &RenderContext<'_>) -> Item {
    let lang = context.lang;
    let name = provider_name(provider);
    let mut row = common::row(&format!("{}-connect", provider.as_str()), KEY, Icon::Machine,
        tf!(lang, "Connect {name}", "Conectar {name}"), Some(t!(lang, "Use this provider's native sign-in on the Mac", "Usar el inicio de sesión nativo de este proveedor en el Mac").into()));
    row.link = None;
    if provider == Provider::Codex {
        row.subtitle = Some(t!(lang, "Use the session on the Mac, or sign in with a code on any device", "Usa la sesión del Mac o inicia sesión con un código en cualquier dispositivo").into());
        row.actions.extend(login_action(&AccountKey::native(provider), None, context));
        return Item::Row(row);
    }
    let mut connect = common::view_link("", t!(lang, "Connect", "Conectar"));
    connect.link = Some(Link::Local { local: LocalLink::AccountSignIn { account: "default".into(), command: login_command(provider, "default"),
        provider: (provider != Provider::Claude).then(|| provider.as_str().into()), argv: (provider != Provider::Claude).then(|| login_arguments(provider, "default")), } });
    row.actions.push(connect);
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
        Some(t!(lang, "Continue with the next eligible account in your order when quota runs out",
            "Continuar con la siguiente cuenta apta de tu orden cuando se agote la cuota").into()));
    row.link = None;
    row.badges.push(Label {
        text: if on { t!(lang, "On", "Activado") } else { t!(lang, "Off", "Desactivado") }.into(),
        tone: if on { Tone::Warning } else { Tone::Neutral },
        elapsed: None,
    });
    if allowed(context, Provider::Claude, AccountChange::SetMoveWhenExhausted { on: !on }).is_ok() {
        let label = if on { t!(lang, "Turn off", "Desactivar") } else { t!(lang, "Turn on", "Activar") };
        if let Some(mut switch) = common::action(Verb::AccountMoves, "moves", context.domain.revision, label, Style::Secondary) {
            switch.option_id = Some(if on { "off" } else { "on" }.into());
            switch.description = Some(if on {
                t!(lang, "A node whose account reaches its limit waits for it to reset", "Un nodo cuya cuenta llega al límite espera a que se reinicie")
            } else {
                t!(lang, "The node continues with saved context on the next eligible account", "El nodo continúa con el contexto guardado en la siguiente cuenta apta")
            }.into());
            row.actions.push(switch);
        }
    }
    Item::Row(row)
}

fn legacy_margin_available(context: &RenderContext<'_>) -> bool { !context.index.accounts().is_empty() && context.index.accounts().iter().all(|account| account.provider == Provider::Claude) }

fn policy_row(context: &RenderContext<'_>) -> Item {
    let lang = context.lang;
    let legacy = legacy_margin_available(context);
    let policy = if legacy { context.index.account_policy() } else { Policy::Chain };
    let (title, about) = policy_text(policy, lang);
    let mut row = common::row("claude-policy", KEY, Icon::Node, title.into(), Some(about.into()));
    row.link = None;
    let other = match policy {
        Policy::Margin => Policy::Chain,
        Policy::Chain => Policy::Margin,
    };
    if legacy && allowed(context, Provider::Claude, AccountChange::SetPolicy { policy: other }).is_ok()
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


pub fn provider_name(provider: Provider) -> &'static str {
    match provider { Provider::Claude => "Claude", Provider::Codex => "Codex" }
}

pub fn action_provider(action: &super::action_id::ActionId) -> Option<Provider> {
    match action.extra.as_deref() { None | Some("claude") => Some(Provider::Claude), Some("codex") => Some(Provider::Codex), _ => None }
}

fn scoped_action(verb: Verb, account: &AccountStatus, label: &str, style: Style, context: &RenderContext<'_>) -> Option<Action> {
    let mut action = common::action(verb, account.id.as_str(), context.domain.revision, label, style)?;
    if account.provider != Provider::Claude {
        action.id = super::action_id::ActionId::new(verb, account.id.as_str(), context.domain.revision, Some(account.provider.as_str().into())).ok()?.to_string();
    }
    Some(action)
}

fn login_arguments(provider: Provider, slug: &str) -> Vec<String> {
    vec!["accounts".into(), "login".into(), "--provider".into(), provider.as_str().into(), slug.into()]
}

fn login_command(provider: Provider, slug: &str) -> String {
    if provider == Provider::Claude { format!("alondra accounts login {}", shell_word(slug)) }
    else { format!("alondra accounts login --provider {} {}", provider.as_str(), shell_word(slug)) }
}

fn window_label(provider: Provider, kind: &WindowKind, lang: Lang) -> String {
    match kind {
        WindowKind::Session if provider == Provider::Claude => "5 h".into(),
        WindowKind::Session => t!(lang, "Session", "Sesión").into(),
        WindowKind::Weekly => t!(lang, "week", "semana").into(),
        WindowKind::Other(name) => {
            if kind.codex_window_label().is_none() { return name.clone(); }
            let mut parts = name.rsplitn(3, ':'); let _duration = parts.next();
            let window = match parts.next() { Some("primary") => t!(lang, "Primary", "Principal"), Some("secondary") => t!(lang, "Secondary", "Secundaria"), _ => return name.clone() };
            let scope = match parts.next() { Some("codex:account") => t!(lang, "Account", "Cuenta").into(), Some(scope) => scope.strip_prefix("codex:limit:").map(|name| tf!(lang, "Limit {name}", "Límite {name}")).unwrap_or_else(|| scope.into()), None => return name.clone() };
            match kind.duration_minutes() { Some(minutes) => format!("{scope} · {window} · {minutes} min"), None => format!("{scope} · {window}") }
        },
    }
}

fn account_presentation(account: &AccountStatus, position: usize, measured: Option<&crate::usage::AccountUsage>, context: &RenderContext<'_>) -> super::AccountPresentation {
    let lang = context.lang;
    let key = AccountKey { provider: account.provider, account: account.id.clone() };
    let windows: Vec<super::AccountWindowPresentation> = measured.into_iter().flat_map(|usage| &usage.windows).filter(|window| !window.reading.kind.is_native_limit_marker()).map(|window| {
        let reading = &window.reading;
        super::AccountWindowPresentation {
            kind: reading.kind.name().into(), label: window_label(account.provider, &reading.kind, lang), used_percent: percent(reading.used),
            measured_at: window.measured_at, resets_at: reading.resets_at,
            stale: context.live.observed_at.is_some_and(|now| reading.resets_at.is_some_and(|reset| reset <= now)), exhausted: reading.exhausted,
        }
    }).collect();
    let last = measured.and_then(crate::usage::AccountUsage::measured_at);
    let text = match (last, context.live.observed_at) {
        (None, _) => t!(lang, "Usage not reported", "Uso sin informar").into(),
        (Some(at), Some(now)) if now.saturating_sub(at) >= 60_000 => {
            let minutes = now.saturating_sub(at) / 60_000;
            tf!(lang, "Reported {minutes} min ago", "Informado hace {minutes} min")
        }
        (Some(_), Some(_)) => t!(lang, "Reported just now", "Informado ahora").into(),
        (Some(_), None) => t!(lang, "Last reported usage", "Último uso informado").into(),
    };
    let freshness = Label { text, tone: if windows.iter().any(|window| window.stale) { Tone::Warning } else { Tone::Neutral }, elapsed: None };
    let reason = if native_limit_reported(measured) { Some(t!(lang, "The provider reported a usage limit. Check accounts again or choose another account; no usage percentage was reported.", "El proveedor ha comunicado un límite de uso. Vuelve a comprobar las cuentas o elige otra; no se ha informado un porcentaje de uso.").into()) } else { match account.state {
        AccountState::SignedOut => Some(t!(lang, "Sign in on the Mac to use this account", "Inicia sesión en el Mac para usar esta cuenta").into()),
        AccountState::Unavailable => Some(t!(lang, "The native provider could not be checked", "No se pudo comprobar el proveedor nativo").into()),
        AccountState::Disabled => Some(t!(lang, "New work is disabled; running work continues", "Trabajo nuevo desactivado; el trabajo en curso continúa").into()),
        _ => None,
    } };
    let model_catalog = context.index.model_catalogs().iter().find(|catalog| catalog.account == key).cloned();
    super::AccountPresentation { model_catalog, key: key.clone(), position: u32::try_from(position).unwrap_or(u32::MAX), availability: state_badge(account.state, lang), reason,
        email: account.email.clone(), plan: account.plan.clone(), active_nodes: u32::try_from(context.index.nodes_on_account_key(&key)).unwrap_or(u32::MAX), freshness, windows }
}


pub fn account_arguments(provider: Option<&str>, argv: Option<&[String]>, account: Option<&str>, adding: bool) -> Option<Vec<String>> {
    let provider = provider.map(Provider::parse).transpose().ok()?.unwrap_or(Provider::Claude);
    let operation = if adding { "add" } else { "login" };
    if let Some(argv) = argv {
        let prefix = ["accounts", operation, "--provider", provider.as_str()];
        if argv.len() != prefix.len() + usize::from(!adding) || !argv.iter().take(4).map(String::as_str).eq(prefix)
            || argv.iter().any(|word| word.chars().any(char::is_control)) { return None; }
        if !adding {
            let slug = argv.last()?;
            AccountId::parse(slug).ok()?;
            if provider == Provider::Codex && account != Some(slug.as_str()) { return None; }
        }
        return Some(argv.to_vec());
    }
    if provider != Provider::Claude { return None; }
    let mut args = vec!["accounts".into(), operation.into()];
    if !adding {
        let account = account.filter(|account| !account.is_empty() && !account.chars().any(char::is_control))?;
        if account.starts_with('-') { args.push("--".into()); }
        args.push(account.into());
    }
    Some(args)
}

fn login_action(account: &AccountKey, cancel: Option<&str>, context: &RenderContext<'_>) -> Option<Action> {
    let available = context.domain.account_login_action(account.clone(), cancel);
    if !available.enabled { return None; }
    let (verb, label, extra) = match cancel {
        Some(attempt) => (Verb::AccountLoginCancel, t!(context.lang, "Cancel sign-in", "Cancelar inicio de sesión"), attempt),
        None if context.index.accounts().iter().any(|known| known.provider == account.provider && known.id == account.account) =>
            (Verb::AccountLogin, t!(context.lang, "Sign in with a code", "Iniciar sesión con un código"), "codex"),
        None => (Verb::AccountLogin, t!(context.lang, "Connect", "Conectar"), "codex"),
    };
    let mut action = common::action(verb, account.account.as_str(), context.domain.revision, label, Style::Primary)?;
    action.id = super::action_id::ActionId::new(verb, account.account.as_str(), context.domain.revision, Some(extra.into())).ok()?.to_string();
    Some(action)
}

fn login_row(login: &crate::domain::AccountLogin, context: &RenderContext<'_>) -> Option<Item> {
    use crate::domain::AccountLoginPhase;
    let lang = context.lang;
    if login.phase == AccountLoginPhase::Connected { return None; }
    let account_name = context.index.accounts().iter().find(|known| known.provider == login.account.provider && known.id == login.account.account)
        .map(|known| name(known, lang)).unwrap_or_else(|| if login.account.account.is_default() { t!(lang, "Default", "Principal").into() } else { login.account.account.to_string() });
    let title = tf!(lang, "Sign in to Codex · {account_name}", "Iniciar sesión en Codex · {account_name}");
    let detail = match login.phase {
        AccountLoginPhase::Starting => t!(lang, "Checking the session on the Mac…", "Comprobando la sesión del Mac…").into(),
        AccountLoginPhase::Publishing => t!(lang, "Finishing sign-in…", "Terminando el inicio de sesión…").into(),
        AccountLoginPhase::Pending => {
            let challenge = login.challenge.as_ref()?;
            let url = &challenge.url;
            let code = &challenge.code;
            tf!(lang, "Open {url} on any device and enter this code:\n{code}", "Abre {url} en cualquier dispositivo e introduce este código:\n{code}")
        },
        AccountLoginPhase::Cancelled => t!(lang, "Sign-in cancelled. The previous session was preserved.", "Inicio de sesión cancelado. Se ha conservado la sesión anterior.").into(),
        AccountLoginPhase::Failed => t!(lang, "Sign-in did not complete. The previous session was preserved. Try again or check Codex on the Mac.", "No se completó el inicio de sesión. Se ha conservado la sesión anterior. Vuelve a intentarlo o comprueba Codex en el Mac.").into(),
        AccountLoginPhase::Connected => return None,
    };
    let mut row = common::row(&format!("codex-login-{}", login.account.account), KEY, Icon::Machine, title, Some(detail));
    row.link = None;
    if let Some(challenge) = &login.challenge {
        let mut open = common::view_link("", t!(lang, "Open verification page", "Abrir página de verificación"));
        open.link = Some(Link::Url { url: challenge.url.clone() });
        open.style = Style::Primary;
        row.actions.push(open);
    }
    if login.phase != AccountLoginPhase::Publishing { row.actions.extend(login_action(&login.account, login.phase.active().then_some(login.attempt.as_str()), context)); }
    Some(Item::Row(row))
}
