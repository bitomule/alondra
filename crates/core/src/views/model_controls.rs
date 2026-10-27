use super::{action_id::{ActionId, Verb}, common, text::{t, tf}, Action, BuildError, ChoiceItem, Condition, Field, FieldKind, Form, Icon, Item, RenderContext, Style, View};
use crate::accounts::Provider;
use crate::domain::{ModelChooser, TaskId};
use crate::model_selection::{ModelCandidate, ModelKey, ModelProfile, PendingTaskSelection, RoleModelPolicy, TaskSelection};
use serde_json::json;

pub(super) const PROFILES: [(ModelProfile, &str); 5] = [(ModelProfile::Implementation, "implementation"), (ModelProfile::General, "general"), (ModelProfile::Design, "design"), (ModelProfile::Review, "review"), (ModelProfile::Orchestrator, "orchestrator")];

pub fn parse_profile(subject: &str) -> Option<(Provider, ModelProfile)> {
    let (provider, role) = subject.split_once('-')?;
    Some((Provider::parse(provider).ok()?, PROFILES.iter().find(|(_, name)| *name == role)?.0))
}

fn profile_label(profile: ModelProfile, context: &RenderContext<'_>) -> &'static str {
    let lang = context.lang;
    match profile { ModelProfile::Implementation => t!(lang, "Implementation", "Implementación"), ModelProfile::General => t!(lang, "General work", "Trabajo general"), ModelProfile::Design => t!(lang, "Design", "Diseño"), ModelProfile::Review => t!(lang, "Review", "Revisión"), ModelProfile::Orchestrator => t!(lang, "Orchestrator", "Orquestador") }
}

pub(super) fn profile_rows(context: &RenderContext<'_>) -> Vec<Item> {
    let providers: std::collections::BTreeSet<_> = context.index.accounts().iter().map(|account| account.provider).collect();
    providers.into_iter().flat_map(|provider| PROFILES.iter().map(move |(profile, slug)| {
        let role = profile_label(*profile, context); let provider_name = super::accounts::provider_name(provider);
        let mut row = common::row(&format!("{}-{slug}", provider.as_str()), "model_profile", Icon::Machine, format!("{provider_name} · {role}"), None);
        row.link = None;
        row.actions = profile_actions(provider, *profile, slug, context);
        row.subtitle = Some(match context.index.model_profiles().get(&provider).and_then(|profiles| profiles.get(profile)) {
            Some(RoleModelPolicy::OrderedCandidates { candidates }) => candidates.iter().map(|candidate| candidate.model.clone()).collect::<Vec<_>>().join(" → "),
            _ => t!(context.lang, "Native default when advertised", "Predeterminado nativo si se comunica").into(),
        }); Item::Row(row)
    })).collect()
}

fn profile_actions(provider: Provider, profile: ModelProfile, slug: &str, context: &RenderContext<'_>) -> Vec<Action> {
    let subject = format!("{}-{slug}", provider.as_str()); let version = context.index.model_profiles_version();
    let catalogs: Vec<_> = context.index.model_catalogs().iter().filter(|catalog| catalog.account.provider == provider).collect();
    let policy = context.index.model_profiles().get(&provider).and_then(|profiles| profiles.get(&profile));
    let mut actions = vec![];
    if let Some(mut model) = common::action(Verb::ModelProfile, &subject, 0, t!(context.lang, "Choose model", "Elegir modelo"), Style::Secondary) {
        model.id = ActionId::new(Verb::ModelProfile, &subject, 0, (!version.is_empty()).then(|| version.into())).expect("profile identity is valid").to_string();
        model.presentation = super::Presentation::Menu;
        if catalogs.iter().any(|catalog| catalog.select(&RoleModelPolicy::NativeDefault).is_ok()) { model.options.push(super::ActionOption { id: "auto".into(), label: t!(context.lang, "Native default", "Predeterminado nativo").into(), description: None, style: Style::Plain, input: super::Input::None, key: None }); }
        for catalog in &catalogs { for item in catalog.models.iter().filter(|item| !item.hidden) {
            let id = format!("model:{}", ModelKey { provider, model: item.model.clone() }.wire_id());
            if !model.options.iter().any(|option| option.id == id) { model.options.push(super::ActionOption { id, label: item.display_name.clone(), description: Some(item.model.clone()), style: Style::Plain, input: super::Input::None, key: None }); }
        } }
        model.enabled = !model.options.is_empty() && !version.is_empty();
        if !model.enabled { model.disabled_reason = Some(t!(context.lang, "Refresh models for this provider first", "Actualiza primero los modelos de este proveedor").into()); }
        actions.push(model);
    }
    if let Some(RoleModelPolicy::OrderedCandidates { candidates }) = policy && let Some(candidate) = candidates.first()
        && let Some(mut effort) = common::action(Verb::ModelProfile, &subject, 0, t!(context.lang, "Reasoning effort", "Esfuerzo de razonamiento"), Style::Secondary) {
        effort.id = ActionId::new(Verb::ModelProfile, &subject, 0, (!version.is_empty()).then(|| version.into())).expect("profile identity is valid").to_string(); effort.presentation = super::Presentation::Menu;
        effort.options.push(super::ActionOption { id: "effort:auto".into(), label: t!(context.lang, "Native default", "Predeterminado nativo").into(), description: None, style: Style::Plain, input: super::Input::None, key: None });
        for native in catalogs.iter().flat_map(|catalog| &catalog.models).filter(|item| item.model == candidate.model).flat_map(|item| &item.reasoning_efforts) {
            let id = format!("effort:{native}"); if !effort.options.iter().any(|option| option.id == id) { effort.options.push(super::ActionOption { id, label: common::effort_label(native, context.lang).unwrap_or(native).into(), description: None, style: Style::Plain, input: super::Input::None, key: None }); }
        }
        effort.enabled = !version.is_empty(); actions.push(effort);
    }
    actions.push(common::view_link(&format!("model_profile:{subject}"), t!(context.lang, "Edit ordered candidates", "Editar candidatos ordenados")));
    actions
}

fn field(id: &str, label: String, kind: FieldKind, value: serde_json::Value) -> Field {
    Field { id: id.into(), label, kind, value, required: false, help: None, visible_when: None, reload_on_change: false }
}

fn guarded(mut action: Action, available: crate::domain::AvailableAction, lang: super::Lang) -> Action {
    action.enabled = available.enabled;
    action.disabled_reason = available.refusal.map(|refusal| super::messages::refusal_text(&refusal, lang));
    action
}

pub(super) fn profile(subject: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    let (provider, profile) = parse_profile(subject).ok_or(BuildError::InvalidKey)?;
    let catalogs: Vec<_> = context.index.model_catalogs().iter().filter(|catalog| catalog.account.provider == provider).collect();
    let policy = context.index.model_profiles().get(&provider).and_then(|profiles| profiles.get(&profile)).cloned().unwrap_or_default();
    let candidates = match &policy { RoleModelPolicy::OrderedCandidates { candidates } => candidates.as_slice(), RoleModelPolicy::NativeDefault => &[] };
    let mut choices = vec![ChoiceItem { id: "auto".into(), label: t!(context.lang, "Native default", "Predeterminado nativo").into(), description: None, group: None, available_when: None,
        disabled_reason: (!catalogs.iter().any(|catalog| catalog.select(&RoleModelPolicy::NativeDefault).is_ok())).then(|| t!(context.lang, "The provider has not advertised a native default. Choose a model.", "El proveedor no ha comunicado un modelo predeterminado nativo. Elige un modelo.").into()) }];
    let mut efforts: std::collections::BTreeMap<String, Vec<serde_json::Value>> = Default::default();
    for catalog in &catalogs { for model in catalog.models.iter().filter(|model| !model.hidden) {
        let id = ModelKey { provider, model: model.model.clone() }.wire_id();
        if !choices.iter().any(|item| item.id == id) { choices.push(ChoiceItem { id: id.clone(), label: model.display_name.clone(), description: None, group: Some(super::accounts::provider_name(provider).into()), available_when: None, disabled_reason: None }); }
        for effort in &model.reasoning_efforts { let models = efforts.entry(effort.clone()).or_default(); if !models.contains(&json!(id)) { models.push(json!(id)); } }
    } }
    let model_ids: Vec<_> = choices.iter().filter(|item| item.id != "auto").map(|item| json!(item.id)).collect();
    let mut fields = vec![];
    for position in 0..=candidates.len().min(31) {
        let model_id = if position == 0 { "model".into() } else { format!("candidate_{}", position + 1) };
        let effort_id = if position == 0 { "reasoning_effort".into() } else { format!("reasoning_effort_{}", position + 1) };
        let mut items = choices.clone();
        if position > 0 { items[0].id = "".into(); items[0].label = t!(context.lang, "No additional model", "Sin otro modelo").into(); items[0].disabled_reason = None; }
        let number = position + 1;
        let mut model_field = field(&model_id, if position == 0 { t!(context.lang, "Model", "Modelo").into() } else { tf!(context.lang, "Candidate {number}", "Candidato {number}") }, FieldKind::Choice { items }, json!(candidates.get(position).map(|candidate| ModelKey { provider, model: candidate.model.clone() }.wire_id()).unwrap_or_else(|| if position == 0 { "auto".into() } else { String::new() })));
        if position > 0 { model_field.visible_when = Some(Condition { field: "model".into(), equals: None, any_of: model_ids.clone() }); }
        fields.push(model_field);
        let mut reasoning = vec![ChoiceItem { id: "auto".into(), label: t!(context.lang, "Native default", "Predeterminado nativo").into(), description: None, group: None, disabled_reason: None, available_when: None }];
        reasoning.extend(efforts.iter().map(|(effort, models)| ChoiceItem { id: effort.clone(), label: common::effort_label(effort, context.lang).unwrap_or(effort).into(), description: None, group: None, disabled_reason: None, available_when: Some(Condition { field: model_id.clone(), equals: None, any_of: models.clone() }) }));
        let mut effort = field(&effort_id, t!(context.lang, "Reasoning effort", "Esfuerzo de razonamiento").into(), FieldKind::Choice { items: reasoning }, json!(candidates.get(position).and_then(|candidate| candidate.reasoning_effort.clone()).unwrap_or_else(|| "auto".into())));
        effort.visible_when = Some(Condition { field: model_id, equals: None, any_of: model_ids.clone() }); fields.push(effort);
    }
    let version = context.index.model_profiles_version().to_owned();
    let mut submit = common::action(Verb::ModelProfile, subject, 0, t!(context.lang, "Save model profile", "Guardar perfil de modelo"), Style::Primary).ok_or(BuildError::InvalidActionId)?;
    submit.id = ActionId::new(Verb::ModelProfile, subject, 0, (!version.is_empty()).then(|| version.clone())).map_err(|_| BuildError::InvalidActionId)?.to_string();
    let representative = catalogs.iter().flat_map(|catalog| &catalog.models).filter(|model| !model.hidden).find_map(|model| {
        let candidate = RoleModelPolicy::OrderedCandidates { candidates: vec![ModelCandidate { model: model.model.clone(), reasoning_effort: None }] };
        let available = context.domain.model_profile_action(provider, profile, candidate, version.clone()); available.enabled.then_some(available)
    }).unwrap_or_else(|| context.domain.model_profile_action(provider, profile, policy.clone(), version));
    submit = guarded(submit, representative, context.lang);
    let title = format!("{} · {}", super::accounts::provider_name(provider), profile_label(profile, context));
    Ok(common::view("model_profile", &format!("model_profile:{subject}"), Some(title), context, vec![common::section("form", None, vec![Item::Form(Form { substitution_was_edited: None, id: "model_profile".into(), fields, submit })])]))
}

pub(super) fn task(id: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    let task_id = TaskId::from(id); let task = context.domain.tasks.get(&task_id).ok_or(BuildError::NotFound)?;
    let intent = context.index.task_selection(&task_id).cloned().unwrap_or_else(|| {
        let mut legacy = TaskSelection::automatic(ModelProfile::Implementation);
        if let Some(model) = &task.model && model.chosen_by == ModelChooser::User { legacy.request_model(Some(ModelKey { provider: task.provider, model: model.model.clone() })); }
        legacy
    });
    let current_account = task.account_key();
    let catalog = context.index.model_catalogs().iter().find(|catalog| catalog.account == current_account);
    let models: Vec<_> = catalog.into_iter().flat_map(|catalog| &catalog.models).filter(|model| !model.hidden).collect();
    let mut choices = vec![ChoiceItem { id: "auto".into(), label: t!(context.lang, "Automatic", "Automático").into(), description: None, group: None,
        available_when: None, disabled_reason: None }];
    let current_effort = intent.effective.as_ref().and_then(|selection| selection.reasoning_effort.as_deref());
    for model in &models {
        let id = ModelKey { provider: current_account.provider, model: model.model.clone() }.wire_id();
        choices.push(ChoiceItem { id: id.clone(), label: model.display_name.clone(), description: Some(model.model.clone()), group: None, available_when: None,
            disabled_reason: current_effort.filter(|effort| !model.reasoning_efforts.iter().any(|supported| supported == *effort)).map(|effort|
                tf!(context.lang, "This native session keeps reasoning effort {effort}, which this model does not support.", "Esta sesión nativa mantiene el esfuerzo de razonamiento {effort}, que este modelo no admite.")) });
    }
    let requested = intent.requested.as_ref().filter(|model| model.provider == current_account.provider);
    let shown = requested.or_else(|| intent.requested.as_ref().and_then(|_| intent.effective.as_ref().map(|selection| &selection.model)));
    let mut model = field("model", t!(context.lang, "Model", "Modelo").into(), FieldKind::Choice { items: choices }, json!(shown.map(ModelKey::wire_id).unwrap_or_else(|| "auto".into())));
    let name = context.index.accounts().iter().find(|status| status.provider == current_account.provider && status.id == current_account.account)
        .map(|status| super::accounts::name(status, context.lang)).unwrap_or_else(|| current_account.account.to_string());
    let provider = super::accounts::provider_name(current_account.provider);
    model.help = Some(tf!(context.lang,
        "Current account: {provider} · {name}. Create another node to use a different account or provider.",
        "Cuenta actual: {provider} · {name}. Crea otro nodo para usar otra cuenta o proveedor."));
    model.help.as_mut().expect("current-account help").push_str(t!(context.lang,
        " Reasoning effort stays unchanged for this native session.", " El esfuerzo de razonamiento no cambia en esta sesión nativa."));
    let mut substitution = field("allow_model_substitution", t!(context.lang, "Allow model substitution", "Permitir sustituir el modelo").into(), FieldKind::Toggle, json!(intent.allow_model_substitution));
    substitution.help = Some(t!(context.lang, "Only models on this node's current account and provider can be used.", "Solo se pueden usar modelos de la cuenta y el proveedor actuales del nodo.").into());
    let fields = vec![model, substitution];
    let stamp = context.domain.task_selection_stamp(&task_id).ok_or(BuildError::NotFound)?;
    let mut submit = common::action(Verb::TaskSelection, id, 0, t!(context.lang, "Change model", "Cambiar modelo"), Style::Primary).ok_or(BuildError::InvalidActionId)?;
    submit.id = ActionId::new(Verb::TaskSelection, id, 0, Some(stamp)).map_err(|_| BuildError::InvalidActionId)?.to_string();
    let mut declined = None;
    let representative = context.index.model_catalogs().iter().filter(|catalog| catalog.account == current_account).find_map(|catalog| catalog.models.iter().filter(|model| !model.hidden).find_map(|model| {
        let mut selection = catalog.resolve(&ModelCandidate { model: model.model.clone(), reasoning_effort: current_effort.map(str::to_owned) }).ok()?;
        selection.reasoning_effort = current_effort.map(str::to_owned);
        let mut candidate = intent.clone(); candidate.request_model(Some(selection.model.clone()));
        let pending = PendingTaskSelection { account: catalog.account.clone(), model: selection, reason: "User requested a selection change".into() };
        let available = context.domain.task_selection_action(&task_id, candidate, pending);
        if available.enabled { Some(available) } else { if declined.is_none() { declined = Some(available); } None }
    }));
    if let Some(available) = representative.or(declined) { submit = guarded(submit, available, context.lang); }
    else { submit.enabled = false; submit.disabled_reason = Some(t!(context.lang, "This node's account has no available model catalog. Refresh models in Accounts.", "La cuenta del nodo no tiene un catálogo de modelos disponible. Actualiza los modelos en Cuentas.").into()); }
    let edited = intent.substitution_was_edited;
    Ok(common::view("task_selection", &format!("task_selection:{id}"), Some(t!(context.lang, "Change model", "Cambiar modelo").into()), context, vec![common::section("form", None, vec![Item::Form(Form { substitution_was_edited: Some(edited), id: "task_selection".into(), fields, submit })])]))
}
