use super::{
    BuildError, ChoiceItem, Confirm, Field, FieldKind, Form, Icon, Item, Notice, RenderContext,
    Style, Tone, Trailing, View, action_id::Verb, common, text::t,
};
use crate::domain_v2::{ScheduleFrequency, ScheduleId, ScheduleRunState};
use serde_json::{Value, json};

fn frequency(f: ScheduleFrequency) -> &'static str {
    match f {
        ScheduleFrequency::Daily => "daily",
        ScheduleFrequency::Weekly => "weekly",
        ScheduleFrequency::EveryHours => "every_hours",
        ScheduleFrequency::EveryDays => "every_days",
        ScheduleFrequency::EveryMonths => "every_months",
        ScheduleFrequency::Once => "once",
    }
}

fn frequency_label(f: ScheduleFrequency, lang: super::Lang) -> &'static str {
    match f {
        ScheduleFrequency::Daily => t!(lang, "Daily", "Diariamente"),
        ScheduleFrequency::Weekly => t!(lang, "Weekly", "Semanalmente"),
        ScheduleFrequency::EveryHours => t!(lang, "Hours", "Horas"),
        ScheduleFrequency::EveryDays => t!(lang, "Days", "Días"),
        ScheduleFrequency::EveryMonths => t!(lang, "Months", "Meses"),
        ScheduleFrequency::Once => t!(lang, "Once", "Una vez"),
    }
}

fn instant(millis: i64) -> String {
    let seconds = millis.div_euclid(1_000);
    let z = seconds.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    let daytime = seconds.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        daytime / 3_600,
        daytime / 60 % 60,
        daytime % 60,
        millis.rem_euclid(1_000)
    )
}

pub(super) fn list(context: &RenderContext<'_>) -> View {
    let mut view = common::view(
        "schedules",
        "schedules",
        Some(t!(context.lang, "Scheduled Tasks", "Tareas programadas").into()),
        context,
        vec![],
    );
    let rows = context.domain.schedule.schedules.values().filter(|s| s.deleted_at.is_none()).map(|schedule| {
        let mut row = common::row(&schedule.id.0, &format!("schedule:{}", schedule.id), Icon::Schedule, schedule.title.clone(), Some(frequency_label(schedule.frequency, context.lang).into()));
        if !schedule.enabled { row.badges.push(super::Label { text: t!(context.lang, "Paused", "Pausada").into(), tone: Tone::Warning }); }
        if schedule.frequency == ScheduleFrequency::Once && schedule.next_run.is_none() { row.badges.push(super::Label { text: t!(context.lang, "Completed", "Completada").into(), tone: Tone::Neutral }); }
        if let Some(next) = schedule.next_run { row.trailing = Some(Trailing::Time { at: instant(next), format: super::TimeFormat::Absolute }); }
        let revision = schedule.revision as u64;
        for (verb, label, style) in [(Verb::SchedRun, t!(context.lang, "Run Now", "Ejecutar ahora"), Style::Secondary), (Verb::SchedToggle, if schedule.enabled { t!(context.lang, "Pause", "Pausar") } else { t!(context.lang, "Resume", "Reanudar") }, Style::Secondary), (Verb::SchedDelete, t!(context.lang, "Delete", "Eliminar"), Style::Destructive)] {
            if let Some(mut action) = common::action(verb, &format!("s-{}", schedule.id.0), revision, label, style) {
                action.key = Some(match verb {
                    Verb::SchedRun => "enter",
                    Verb::SchedToggle => "p",
                    Verb::SchedDelete => "d",
                    _ => unreachable!(),
                }.into());
                if verb == Verb::SchedDelete { action.confirm = Some(Confirm { title: label.into(), message: t!(context.lang, "Future runs will stop. Delete this schedule?", "Las próximas ejecuciones se detendrán. ¿Eliminar esta programación?").into(), button: label.into() }); }
                row.actions.push(action);
            }
        }
        Item::Row(row)
    }).collect();
    view.sections.push(common::section("schedules", None, rows));
    view.sections.push(common::section(
        "actions",
        None,
        vec![Item::Actions(super::ActionBar {
            actions: vec![common::view_link(
                "schedule:new",
                t!(context.lang, "Add", "Añadir"),
            )],
        })],
    ));
    if context.domain.schedule.schedules.is_empty() {
        view.empty = Some(Notice {
            text: t!(
                context.lang,
                "No scheduled tasks",
                "No hay tareas programadas"
            )
            .into(),
            tone: Tone::Neutral,
            action: None,
        });
    }
    view
}

fn field(id: &str, label: &str, kind: FieldKind, value: Value, required: bool) -> Field {
    Field {
        id: id.into(),
        label: label.into(),
        kind,
        value,
        required,
        help: None,
        visible_when: None,
        reload_on_change: false,
    }
}

pub(super) fn editor(key: &str, context: &RenderContext<'_>) -> Result<View, BuildError> {
    let id = key
        .strip_prefix("schedule:")
        .ok_or(BuildError::InvalidKey)?;
    let id = id.split('?').next().ok_or(BuildError::InvalidKey)?;
    let project = common::key_param(key, &format!("schedule:{id}"), "project")?;
    if id != "new" {
        common::id(id)?;
    }
    let schedule = if id == "new" {
        None
    } else {
        Some(
            context
                .domain
                .schedule
                .schedules
                .get(&ScheduleId(id.into()))
                .ok_or(BuildError::NotFound)?,
        )
    };
    let mut view = common::view(
        "schedule",
        key,
        Some(schedule.map_or_else(
            || t!(context.lang, "New Schedule", "Nueva programación").into(),
            |s| s.title.clone(),
        )),
        context,
        vec![],
    );
    let selected_project = project
        .as_deref()
        .or_else(|| schedule.and_then(|s| s.project.as_deref()))
        .unwrap_or("");
    let mut projects = vec![ChoiceItem {
        id: "".into(),
        label: t!(context.lang, "Projects folder", "Carpeta de proyectos").into(),
    }];
    for name in context
        .live
        .projects
        .iter()
        .map(|project| &project.name)
        .chain(
            context
                .domain
                .schedule
                .schedules
                .values()
                .filter_map(|s| s.project.as_ref()),
        )
    {
        if !projects.iter().any(|item| item.id == *name) {
            projects.push(ChoiceItem {
                id: name.clone(),
                label: name.clone(),
            });
        }
    }
    if !selected_project.is_empty() && !projects.iter().any(|item| item.id == selected_project) {
        projects.push(ChoiceItem {
            id: selected_project.into(),
            label: selected_project.into(),
        });
    }
    let mut fields = vec![
        field(
            "name",
            t!(context.lang, "Name", "Nombre"),
            FieldKind::Text { placeholder: None },
            json!(schedule.map(|s| s.title.as_str()).unwrap_or("")),
            true,
        ),
        field(
            "mission",
            t!(context.lang, "Mission", "Misión"),
            FieldKind::Multiline { placeholder: None },
            json!(schedule.map(|s| s.text.as_str()).unwrap_or("")),
            true,
        ),
        field(
            "project",
            t!(context.lang, "Project", "Proyecto"),
            FieldKind::Choice { items: projects },
            json!(selected_project),
            false,
        ),
        field(
            "repeat",
            t!(context.lang, "Repeat", "Repetir"),
            FieldKind::Choice {
                items: [
                    "daily",
                    "weekly",
                    "every_hours",
                    "every_days",
                    "every_months",
                    "once",
                ]
                .into_iter()
                .map(|id| ChoiceItem {
                    id: id.into(),
                    label: frequency_label(
                        match id {
                            "daily" => ScheduleFrequency::Daily,
                            "weekly" => ScheduleFrequency::Weekly,
                            "every_hours" => ScheduleFrequency::EveryHours,
                            "every_days" => ScheduleFrequency::EveryDays,
                            "every_months" => ScheduleFrequency::EveryMonths,
                            _ => ScheduleFrequency::Once,
                        },
                        context.lang,
                    )
                    .into(),
                })
                .collect(),
            },
            json!(schedule.map(|s| frequency(s.frequency)).unwrap_or("daily")),
            true,
        ),
        field(
            "weekday",
            t!(context.lang, "Weekday", "Día de la semana"),
            FieldKind::Choice {
                items: (0..7)
                    .map(|n| ChoiceItem {
                        id: n.to_string(),
                        label: match n {
                            0 => t!(context.lang, "Sunday", "Domingo"),
                            1 => t!(context.lang, "Monday", "Lunes"),
                            2 => t!(context.lang, "Tuesday", "Martes"),
                            3 => t!(context.lang, "Wednesday", "Miércoles"),
                            4 => t!(context.lang, "Thursday", "Jueves"),
                            5 => t!(context.lang, "Friday", "Viernes"),
                            _ => t!(context.lang, "Saturday", "Sábado"),
                        }
                        .into(),
                    })
                    .collect(),
            },
            json!(schedule.and_then(|s| s.weekday)),
            false,
        ),
        field(
            "interval",
            t!(context.lang, "Interval", "Intervalo"),
            FieldKind::Stepper { min: 1, max: 365 },
            json!(schedule.and_then(|s| s.interval).unwrap_or(1)),
            false,
        ),
        field(
            "start_at",
            t!(context.lang, "Start date", "Fecha de inicio"),
            FieldKind::Date,
            json!(schedule.and_then(|s| s.start_at)),
            false,
        ),
        field(
            "time",
            t!(context.lang, "Time", "Hora"),
            FieldKind::Time,
            json!(
                schedule
                    .map(|s| format!("{:02}:{:02}", s.hour, s.minute))
                    .unwrap_or_else(|| "09:00".into())
            ),
            true,
        ),
        field(
            "active",
            t!(context.lang, "Active", "Activa"),
            FieldKind::Toggle,
            json!(schedule.is_none_or(|s| s.enabled)),
            false,
        ),
        field(
            "disallowed_tools",
            t!(
                context.lang,
                "Disallowed tools",
                "Herramientas no permitidas"
            ),
            FieldKind::Lines { placeholder: None },
            json!(
                schedule
                    .map(|s| s.disallowed_tools.join("\n"))
                    .unwrap_or_default()
            ),
            false,
        ),
    ];
    let option_specs = context.live.project_options.get(selected_project)
        .unwrap_or(&context.live.options);
    fields.extend(option_specs.iter().map(|spec| {
        let mut field = super::misc::option_field(spec);
        if let Some(value) = schedule
            .filter(|schedule| schedule.project.as_deref().unwrap_or("") == selected_project)
            .and_then(|schedule| schedule.options.get(&spec.id)) {
            field.value = json!(value);
        }
        field
    }));
    fields[2].reload_on_change = true;
    fields[4].visible_when = Some(super::Condition {
        field: "repeat".into(),
        equals: json!("weekly"),
    });
    fields[4].required = true;
    fields[5].visible_when = Some(super::Condition {
        field: "repeat".into(),
        equals: json!("every_days"),
    });
    let subject = if id == "new" {
        "view".to_owned()
    } else {
        format!("s-{id}")
    };
    let submit = common::action(
        Verb::SchedSave,
        &subject,
        schedule.map_or(context.domain.revision, |s| s.revision as u64),
        t!(context.lang, "Save Schedule", "Guardar programación"),
        Style::Primary,
    )
    .ok_or(BuildError::InvalidKey)?;
    view.sections.push(common::section(
        "form",
        None,
        vec![Item::Form(Form {
            id: "schedule".into(),
            fields,
            submit,
        })],
    ));
    if let Some(s) = schedule {
        let runs: Vec<_> = context
            .domain
            .schedule
            .runs
            .values()
            .filter(|run| run.schedule_id == s.id)
            .collect();
        if let Some(run) = runs.last() {
            let text = match run.state {
                ScheduleRunState::Launching => t!(
                    context.lang,
                    "Waiting for orchestrator",
                    "Esperando al orquestador"
                )
                .to_owned(),
                ScheduleRunState::Sent => t!(context.lang, "Delivered", "Entregada").to_owned(),
                ScheduleRunState::Failed | ScheduleRunState::Interrupted => run
                    .error
                    .clone()
                    .unwrap_or_else(|| t!(context.lang, "Failed", "Falló").into()),
            };
            view.notices.push(Notice {
                text,
                tone: if run.state == ScheduleRunState::Failed {
                    Tone::Warning
                } else {
                    Tone::Neutral
                },
                action: None,
            });
        }
    }
    Ok(view)
}

#[cfg(test)]
mod tests {
    #[test]
    fn schedule_instants_are_absolute_utc() {
        assert_eq!(super::instant(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            super::instant(1_784_204_760_123),
            "2026-07-16T12:26:00.123Z"
        );
    }
}
