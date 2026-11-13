use super::{
    BuildError, ChoiceItem, Client, Field, FieldKind, Form, Icon, Item, Notice, RenderContext,
    Style, TimeFormat, Tone, Trailing, View, action_id::Verb, common, text::t,
};
use crate::options::{Kind as OptionKind, Spec};
use serde_json::json;

pub(super) fn new_request(key: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    let project = common::key_param(key, "new_request", "project")?.unwrap_or_default();
    let mut view = common::view(
        "new_request",
        key,
        Some(t!(context.lang, "Ask for Something New", "Pide algo nuevo").into()),
        context,
        vec![],
    );
    let mut projects = vec![ChoiceItem {
                    group: None, disabled_reason: None, available_when: None,
        id: "".into(),
        label: t!(
            context.lang,
            "Alondra picks the project",
            "Alondra elige el proyecto"
        )
        .into(),
        description: None,
    }];
    for entry in &context.live.projects {
        if !projects.iter().any(|item| item.id == entry.name) {
            projects.push(ChoiceItem {
                    group: None, disabled_reason: None, available_when: None,
                id: entry.name.clone(),
                label: entry.name.clone(),
                description: None,
            });
        }
    }
    for name in context
        .domain
        .schedule
        .schedules
        .values()
        .filter_map(|s| s.project.as_ref())
    {
        if !projects.iter().any(|item| item.id == *name) {
            projects.push(ChoiceItem {
                    group: None, disabled_reason: None, available_when: None,
                id: name.clone(),
                label: name.clone(),
                description: None,
            });
        }
    }
    if !project.is_empty() && !projects.iter().any(|item| item.id == project) {
        projects.push(ChoiceItem {
                    group: None, disabled_reason: None, available_when: None,
            id: project.clone(),
            label: project.clone(),
            description: None,
        });
    }
    let mut project_field = Field {
        id: "project".into(),
        label: t!(context.lang, "Project", "Proyecto").into(),
        kind: FieldKind::Choice { items: projects },
        value: json!(project),
        required: false,
        help: None,
        visible_when: None,
        reload_on_change: true,
    };
    project_field.reload_on_change = true;
    let mut submit = common::action(
        Verb::NewRequest,
        "view",
        context.domain.revision,
        t!(context.lang, "Send Request", "Enviar solicitud"),
        Style::Primary,
    )
    .ok_or(BuildError::InvalidKey)?;
    submit.attachments = true;
    let mut fields = vec![
        Field {
            id: "text".into(),
            label: t!(
                context.lang,
                "What do you want done?",
                "¿Qué quieres que se haga?"
            )
            .into(),
            kind: FieldKind::Multiline { placeholder: None },
            value: json!(""),
            required: true,
            help: None,
            visible_when: None,
            reload_on_change: false,
        },
        Field {
            id: "attachments".into(),
            label: t!(context.lang, "Attachments", "Adjuntos").into(),
            kind: FieldKind::Attachments,
            value: json!([]),
            required: false,
            help: None,
            visible_when: None,
            reload_on_change: false,
        },
        project_field,
    ];
    let specs = context
        .live
        .project_options
        .get(&project)
        .unwrap_or(&context.live.options);
    fields.extend(specs.iter().map(|spec| option_field(spec, context.lang, context.client)));
    enrich_model_fields(&mut fields, context);
    fields.push(Field {
        id: "issue".into(),
        label: t!(context.lang, "Issue", "Incidencia").into(),
        kind: FieldKind::Picker {
            source: "issue_picker".into(),
        },
        value: json!(""),
        required: false,
        help: Some(t!(context.lang, "Choose an open issue or paste its GitHub link", "Elige una incidencia abierta o pega su enlace de GitHub").into()),
        visible_when: None,
        reload_on_change: false,
    });
    view.sections.push(common::section(
        "form",
        None,
        vec![Item::Form(Form {
            substitution_was_edited: None,
            id: "new_request".into(),
            fields,
            submit,
        })],
    ));
    if matches!(context.client, Client::Ios | Client::Mac) {
        let mut requests: Vec<_> = context.domain.requests.values().filter(|request| context.is_defining(request)).collect();
        requests.sort_by(|left, right| {
            context.live.request_times.get(&right.id.0).cmp(&context.live.request_times.get(&left.id.0))
                .then_with(|| right.id.cmp(&left.id))
        });
        let sent = requests
            .into_iter()
            .map(|request| {
                let mut row = common::row(
                    &request.id.0,
                    &format!("request:{}", request.id),
                    Icon::Request,
                    request.title.clone().unwrap_or_else(|| {
                        request
                            .body
                            .trim_start_matches("[Alondra] ")
                            .lines()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take(100)
                            .collect()
                    }),
                    None,
                );
                row.link = None;
                row.badges.push(super::Label {
                    text: t!(context.lang, "Queued", "En cola").into(),
                    tone: Tone::Neutral,
                    elapsed: None,
                });
                row.trailing =
                    context
                        .live
                        .request_times
                        .get(&request.id.0)
                        .map(|at| Trailing::Time {
                            at: at.clone(),
                            format: TimeFormat::Relative,
                        });
                Item::Row(row)
            })
            .collect();
        view.sections.push(common::section(
            "sent",
            Some(t!(context.lang, "Pending Requests", "Solicitudes pendientes").into()),
            sent,
        ));
    }
    Ok(view)
}

pub(super) fn option_field(spec: &Spec, lang: super::Lang, client: Client) -> Field {
    let builtin_label = |id: &str, value: Option<&str>, fallback: &str| -> String {
        if !spec.builtin { return fallback.into(); }
        if id == "effort" {
            return match value {
                None => t!(lang, "Effort", "Esfuerzo").into(),
                Some(value) => common::effort_label(value, lang).unwrap_or(fallback).into(),
            };
        }
        if id == crate::options::ACCOUNT {
            return match value {
                None => t!(lang, "Account", "Cuenta").into(),
                Some(crate::options::AUTO) => t!(lang, "Automatic", "Automática").into(),
                Some(_) => fallback.into(),
            };
        }
        if client == Client::Tui { return fallback.into(); }
        match (id, value, lang) {
            ("pr", None, _) => "PRs",
            ("pr", Some("review"), super::Lang::En) => "Review each one",
            ("pr", Some("review"), super::Lang::Es) => "Revisar cada una",
            ("pr", Some("auto_merge"), super::Lang::En) => "Auto-merge",
            ("pr", Some("auto_merge"), super::Lang::Es) => "Merge automático",
            _ => fallback,
        }.into()
    };
    let builtin_description = |value: &crate::options::SpecValue| -> Option<String> {
        if spec.builtin && spec.id == "effort" {
            return Some(match value.id.as_str() {
                "auto" => t!(lang, "Use configured task profiles; an explicit model stays selected", "Usar los perfiles de tarea configurados; un modelo explícito se mantiene"),
                "light" => t!(lang, "Small, focused work; an explicit model stays selected", "Trabajo pequeño y concreto; un modelo explícito se mantiene"),
                "normal" => t!(lang, "Standard work using the configured task profiles", "Trabajo normal con los perfiles de tarea configurados"),
                "hard" => t!(lang, "Demanding work; an explicit model and native effort stay binding", "Trabajo exigente; el modelo explícito y su esfuerzo nativo siguen siendo obligatorios"),
                _ => return value.description.clone(),
            }.into());
        }
        if spec.builtin && spec.id == "account" {
            return Some(match value.description.as_deref() {
                Some("Alondra follows the configured account distribution") => t!(lang, "Alondra follows the configured account distribution", "Alondra sigue el reparto configurado de las cuentas"),
                Some("Paid per use: used only when explicitly selected") => t!(lang, "Paid per use: used only when explicitly selected", "Pago por uso: solo se utiliza si se selecciona expresamente"),
                Some("Keep this request's work on this account") => t!(lang, "Keep this request's work on this account", "Mantener el trabajo de esta solicitud en esta cuenta"),
                _ => return value.description.clone(),
            }.into());
        }
        if !spec.builtin || lang == super::Lang::En {
            return value.description.clone();
        }
        match (spec.id.as_str(), value.id.as_str()) {
            ("pr", "review") => Some("Cada PR que abre el nodo es una tarjeta para ti; Alondra hace el merge cuando la aceptas"),
            ("pr", "auto_merge") => Some("Alondra hace merge de la PR cuando pasan sus comprobaciones"),
            ("effort", "auto") => Some("Alondra elige el modelo y el esfuerzo según la tarea"),
            ("effort", "light") => Some("Un modelo rápido y más barato para trabajo pequeño y claro"),
            ("effort", "normal") => Some("El modelo y el esfuerzo por defecto de Claude"),
            ("effort", "hard") => Some("El modelo más potente y el máximo esfuerzo"),
            ("account", "auto") => Some("Alondra elige una cuenta de suscripción por su margen"),
            ("account", _) => Some("De pago por uso: las tareas de esta petición trabajan con ella"),
            _ => return value.description.clone(),
        }
        .map(str::to_owned)
    };
    let kind = match spec.kind {
        OptionKind::Toggle => FieldKind::Toggle,
        OptionKind::Choice => FieldKind::Choice {
            items: spec
                .values
                .iter()
                .map(|value| ChoiceItem {
                    group: None, disabled_reason: None, available_when: None,
                    id: value.id.clone(),
                    label: builtin_label(&spec.id, Some(&value.id), &value.label),
                    description: builtin_description(value),
                })
                .collect(),
        },
    };
    Field {
        id: spec.id.clone(),
        label: builtin_label(&spec.id, None, &spec.label),
        kind,
        value: serde_json::to_value(&spec.default).unwrap_or(serde_json::Value::Null),
        required: false,
        help: None,
        visible_when: None,
        reload_on_change: false,
    }
}

pub(super) fn issue_picker(key: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    let query = if key == "issue_picker" || key == "issue_picker?refresh=1" {
        String::new()
    } else {
        common::key_param(key, "issue_picker", "q")?.unwrap_or_default()
    };
    let mut view = common::view(
        "issue_picker",
        key,
        Some(t!(context.lang, "Issues", "Incidencias").into()),
        context,
        vec![common::section("issues", None, vec![])],
    );
    let mut rows: Vec<Item> = context
        .live
        .issues
        .iter()
        .filter(|issue| issue.matches(&query))
        .take(50)
        .map(|issue| {
            let mut row = common::row(
                &issue.reference(),
                "issue_picker",
                Icon::Mission,
                issue.reference(),
                Some(issue.title.clone()),
            );
            row.link = None;
            row.badges = issue
                .labels
                .iter()
                .take(3)
                .map(|label| super::Label {
                    text: label.clone(),
                    tone: Tone::Neutral,
                    elapsed: None,
                })
                .collect();
            row.trailing = issue.updated_at.clone().map(|at| Trailing::Time {
                at,
                format: TimeFormat::Relative,
            });
            Item::Row(row)
        })
        .collect::<Vec<_>>();
    if let Some((repo, number)) = crate::issues::parse_ref(&query) {
        let reference = format!("{repo}#{number}");
        if !rows.iter().any(|item| matches!(item, Item::Row(row) if row.id == reference)) {
            rows.insert(0, Item::Row(common::row(
                &reference, "issue_picker", Icon::Mission,
                format!("{} {}", t!(context.lang, "Use", "Usar"), query),
                Some(reference.clone()),
            )));
        }
    }
    view.sections[0].items = rows;
    if view.sections[0].items.is_empty() {
        let text = if context.live.issue_error.is_some() {
            t!(
                context.lang,
                "Issues are unavailable",
                "Las incidencias no están disponibles"
            )
        } else if query.is_empty() {
            t!(
                context.lang,
                "No open issues found",
                "No se encontraron incidencias abiertas"
            )
        } else {
            t!(
                context.lang,
                "No matching issues",
                "No hay incidencias que coincidan"
            )
        };
        view.empty = Some(Notice {
            text: text.into(),
            tone: Tone::Neutral,
            action: None,
        });
    }
    Ok(view)
}

pub(super) fn search(key: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    let q = common::key_param(key, "search", "q")?
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut view = common::view(
        "search",
        key,
        Some(if q.is_empty() { t!(context.lang, "Search", "Buscar").to_owned() }
            else { format!("{}: {q}", t!(context.lang, "Search", "Buscar")) }),
        context,
        vec![],
    );
    let rows: Vec<_> = context
        .domain
        .tasks
        .values()
        .filter(|task| task.title.to_ascii_lowercase().contains(&q))
        .chain(context.live.search_matches.iter().flatten()
            .filter_map(|agent| common::current_task(context, &crate::domain::AgentId(agent.clone()))))
        .fold(Vec::<&crate::domain::Task>::new(), |mut found, task| {
            if !found.iter().any(|seen| seen.id == task.id) { found.push(task); }
            found
        })
        .into_iter()
        .map(|task| Item::Row(super::tasks::task_row(task, context)))
        .collect();
    let count = rows.len();
    view.sections.push(common::section("results",
        Some(if count == 1 { t!(context.lang, "1 result found", "1 resultado encontrado").into() }
            else if context.lang == super::Lang::Es { format!("{count} resultados encontrados") }
            else { format!("{count} results found") }), rows));
    if count == 0 && !q.is_empty() {
        view.empty = Some(Notice { text: t!(context.lang, "No results found", "No se encontraron resultados").into(),
            tone: Tone::Neutral, action: None });
    }
    Ok(view)
}

/// Where the CLI posts maintenance for the orchestrator: only the message action, and no screen
/// links to it, because the orchestrator is Alondra's own.
pub(super) fn orchestrator(key: &str, context: &RenderContext<'_>) -> View {
    let mut view = common::view("orchestrator", key, Some("Alondra".into()), context, vec![]);
    if let Some(mut message) = common::action(super::action_id::Verb::Msg, "view", context.domain.revision,
        t!(context.lang, "Message", "Mensaje"), super::Style::Primary) {
        message.input = super::Input::Text { placeholder: t!(context.lang, "Write to Alondra…", "Escribe a Alondra…").into(),
            required: true, multiline: true, secret: false };
        message.attachments = true;
        view.sections.push(common::section("actions", None, vec![Item::Actions(super::ActionBar { actions: vec![message] })]));
    }
    view
}

pub(super) fn machine(context: &RenderContext<'_>) -> View {
    let mut view = common::view(
        "machine",
        "machine",
        Some(t!(context.lang, "Machine", "Máquina").into()),
        context,
        vec![],
    );
    view.sections.push(common::section(
        "gauges",
        None,
        vec![Item::Facts(super::Facts {
            items: context
                .live
                .gauges
                .iter()
                .map(|g| super::Fact {
                    kind: None,
                    label: g.label.clone(),
                    value: g.text.clone(),
                    tone: None,
                    link: None,
                })
                .collect(),
        })],
    ));
    if let Some(section) = settings_section(context) {
        view.sections.push(section);
    }
    if let Some(section) = super::allow_always::section(context) {
        view.sections.push(section);
    }
    if let Some(update) = &context.live.update {
        view.notices.push(update.clone());
    }
    view
}

/// This Mac's settings the apps show in Settings: how it merges PRs, how nodes bring main into their
/// PR's branch and how many nodes work at once.
fn settings_section(context: &RenderContext<'_>) -> Option<super::Section> {
    let mut items: Vec<Item> = super::merge_method::settings_row(context).into_iter()
        .chain(super::branch_update::settings_row(context)).collect();
    if let Some(pacing) = context.live.pacing.as_ref()
        && let Some(limit) = super::pacing::limit_action(pacing, context)
    {
        let mut row = common::row("node_limit", "machine", Icon::Machine, limit.label.clone(),
            Some(super::pacing::limit_text(pacing, context.lang)));
        row.link = None;
        row.actions.push(limit);
        items.push(Item::Row(row));
    }
    (!items.is_empty()).then(|| common::section("settings", None, items))
}


pub(super) fn enrich_model_fields(fields: &mut Vec<Field>, context: &RenderContext<'_>) {
    let lang = context.lang;
    let item = |id: String, label: String, group: Option<String>, disabled_reason: Option<String>, available_when: Option<super::Condition>| ChoiceItem {
        id, label, description: None, group, disabled_reason, available_when,
    };
    let automatic = || item("auto".into(), t!(lang, "Automatic", "Automático").into(), None, None, None);
    let mut models = vec![automatic()];
    let mut efforts: std::collections::BTreeMap<String, Vec<serde_json::Value>> = std::collections::BTreeMap::new();
    for catalog in context.index.model_catalogs() {
        for model in catalog.models.iter().filter(|model| !model.hidden) {
            let id = format!("{}/{}", catalog.account.provider.as_str(), model.model);
            if !models.iter().any(|item| item.id == id) {
                models.push(item(id.clone(), model.display_name.clone(), Some(super::accounts::provider_name(catalog.account.provider).into()), None, None));
            }
            for effort in &model.reasoning_efforts {
                let supported = efforts.entry(effort.clone()).or_default();
                if !supported.iter().any(|value| value == &json!(id)) { supported.push(json!(id)); }
            }
        }
    }
    let model_ids: Vec<_> = models.iter().filter(|model| model.id != "auto").map(|model| json!(model.id)).collect();
    let mut reasoning = vec![item("auto".into(), t!(lang, "Native default", "Predeterminado nativo").into(), None, None, None)];
    reasoning.extend(efforts.into_iter().map(|(effort, models)| item(effort.clone(), common::effort_label(&effort, lang).unwrap_or(&effort).into(), None, None,
        Some(super::Condition { field: "model".into(), equals: None, any_of: models }))));
    let mut accounts = vec![automatic()];
    for account in context.index.accounts() {
        let key = crate::accounts::AccountKey { provider: account.provider, account: account.id.clone() };
        let mut compatible = vec![json!("auto")];
        if let Some(catalog) = context.index.model_catalogs().iter().find(|catalog| catalog.account == key) {
            compatible.extend(catalog.models.iter().filter(|model| !model.hidden)
                .map(|model| json!(format!("{}/{}", account.provider.as_str(), model.model))));
        }
        let reason = match account.state {
            crate::accounts::AccountState::SignedOut => Some(super::accounts::account_sign_in_hint(account, lang)),
            crate::accounts::AccountState::Disabled => Some(t!(lang, "This account is turned off", "Esta cuenta está desactivada").into()),
            crate::accounts::AccountState::Unavailable => Some(t!(lang, "This account could not be checked", "No se pudo comprobar esta cuenta").into()),
            _ => None,
        };
        let paid = matches!(account.billing, crate::accounts::AccountBilling::PayPerUse);
        let mut choice = item(format!("{}/{}", account.provider.as_str(), account.id), if paid { format!("{} · {}", super::accounts::name(account, lang), t!(lang, "Pay-per-use", "Pago por uso")) } else { super::accounts::name(account, lang) },
            Some(super::accounts::provider_name(account.provider).into()), reason,
            Some(super::Condition { field: "model".into(), equals: None, any_of: compatible }));
        if paid { choice.description = Some(t!(lang, "Selecting this account authorizes paid usage for this work", "Seleccionar esta cuenta autoriza el uso de pago para este trabajo").into()); }
        if account.provider == crate::accounts::Provider::Codex {
            let measured = context.index.account_usage().iter().find(|usage| usage.provider == account.provider && usage.account == account.id);
            if let Some(quota) = super::accounts::usage_line(account, measured, context) {
                choice.description = Some(match choice.description { Some(about) => format!("{about} · {quota}"), None => quota });
            }
        }
        accounts.push(choice);
    }
    let choice = |id: &str, label: &str, items: Vec<ChoiceItem>, visible_when: Option<super::Condition>| Field {
        id: id.into(), label: label.into(), kind: FieldKind::Choice { items }, value: json!("auto"), required: false,
        help: None, visible_when, reload_on_change: false,
    };
    fields.retain(|field| !matches!(field.id.as_str(), "model" | "reasoning_effort" | "allow_model_substitution" | "account"));
    fields.push(choice("model", t!(lang, "Model", "Modelo"), models, None));
    fields.push(choice("account", t!(lang, "Account", "Cuenta"), accounts, None));
    fields.push(choice("reasoning_effort", t!(lang, "Reasoning effort", "Esfuerzo de razonamiento"), reasoning,
        Some(super::Condition { field: "model".into(), equals: None, any_of: model_ids })));
    fields.push(Field { id: "allow_model_substitution".into(), label: t!(lang, "Allow model substitution", "Permitir sustituir el modelo").into(), kind: FieldKind::Toggle,
        value: json!(true), required: false, help: Some(t!(lang, "If the requested model cannot run, continue with another eligible model", "Si el modelo solicitado no puede trabajar, continuar con otro modelo apto").into()),
        visible_when: None, reload_on_change: false });
}
