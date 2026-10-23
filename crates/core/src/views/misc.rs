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
                id: name.clone(),
                label: name.clone(),
                description: None,
            });
        }
    }
    if !project.is_empty() && !projects.iter().any(|item| item.id == project) {
        projects.push(ChoiceItem {
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
            required: true, multiline: true };
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

/// This Mac's settings the apps show in Settings: how it merges PRs and how many nodes work at once.
fn settings_section(context: &RenderContext<'_>) -> Option<super::Section> {
    let mut items: Vec<Item> = super::merge_method::settings_row(context).into_iter().collect();
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
