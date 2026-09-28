//! Schedule v2's local-calendar and launch preparation adapter. The command
//! carrying this intent must be committed with its Run, Request, and Task before
//! a technical launch claim can call Claude.
use alondra_core::domain_v2::{ScheduleDefinitionSnapshot, ScheduleFrequency, ScheduleId, ScheduleLaunchIntent, ScheduleRunId};
use alondra_core::options::{self, OptionsFile};
use axum::http::StatusCode;
use std::path::Path;

pub(crate) fn next_after(schedule: &alondra_core::domain_v2::Schedule, now: i64) -> Option<i64> {
    if !schedule.enabled {
        return None;
    }
    let frequency = match schedule.frequency {
        ScheduleFrequency::Daily => crate::schedules::Frequency::Daily,
        ScheduleFrequency::Weekly => crate::schedules::Frequency::Weekly,
        ScheduleFrequency::EveryHours => crate::schedules::Frequency::EveryHours,
        ScheduleFrequency::EveryDays => crate::schedules::Frequency::EveryDays,
        ScheduleFrequency::EveryMonths => crate::schedules::Frequency::EveryMonths,
        ScheduleFrequency::Once => crate::schedules::Frequency::Once,
    };
    crate::schedules::next_after(
        frequency,
        schedule.hour,
        schedule.minute,
        schedule.weekday,
        schedule.interval,
        schedule.start_at,
        now,
    )
}

/// Capture the exact technical new-node command at dispatch time. Neither a
/// later Schedule edit nor an options.toml edit can change this saved intent.
pub(crate) fn prepare_intent(
    schedule_id: &ScheduleId,
    run_id: &ScheduleRunId,
    definition: &ScheduleDefinitionSnapshot,
    file: &OptionsFile,
    projects_root: &Path,
) -> Result<ScheduleLaunchIntent, String> {
    let resolved = file.resolve(definition.project.as_deref(), &definition.options)?;
    let cwd = match definition.project.as_deref() {
        Some(project) => {
            if !options::projects(projects_root)
                .iter()
                .any(|(name, _)| name == project)
            {
                return Err("schedule project is not an available project folder".into());
            }
            projects_root.join(project)
        }
        None => projects_root.to_path_buf(),
    };
    if !cwd.is_dir() {
        return Err("schedule project directory does not exist".into());
    }
    let cwd = cwd.to_str().ok_or("schedule directory path is not UTF-8")?;
    let name = format!(
        "scheduled/{}-{}",
        schedule_id.0.chars().take(8).collect::<String>(),
        run_id.0
    );
    let launch = ScheduleLaunchIntent {
        name,
        cwd: cwd.into(),
        prompt: definition.text.clone(),
        options_json: serde_json::to_string(&resolved).map_err(|error| error.to_string())?,
    };
    launch.validate(definition).map_err(|error| error.to_string())?;
    Ok(launch)
}

/// Invoke only after the technical intent has been claimed durably. The caller
/// records the result against that claim; this function never retries Claude.
async fn invoke_claimed(
    launch: &ScheduleLaunchIntent,
    request_id: &str,
    disallowed_tools: &[String],
    launcher: &impl crate::orchestrate::Launcher,
) -> Result<Option<String>, crate::orchestrate::Refusal> {
    if let Some(reason) = crate::http::launch_blocked(launcher.usage().as_ref(), false) {
        return Err((StatusCode::CONFLICT, reason));
    }
    if !Path::new(&launch.cwd).is_dir() {
        return Err((StatusCode::BAD_REQUEST, "schedule project directory does not exist".into()));
    }
    let Some(plugin) = launcher.plugin() else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "Alondra's Claude plugin is unavailable".into()));
    };
    let resolved: alondra_core::options::Resolved = serde_json::from_str(&launch.options_json)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let mut args = crate::options::launch_args(
        &launch.name, None, None, &launch.prompt, &resolved, Some(request_id), &plugin,
    );
    if !disallowed_tools.is_empty() {
        let separator = args.iter().position(|arg| arg == "--").expect("launch separator");
        args.splice(separator..separator, ["--disallowedTools".into(), disallowed_tools.join(" ")]);
    }
    let output = launcher.run(&launch.cwd, &args).await?;
    Ok(alondra_core::sessions::launched_short(&output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain_v2::{Schedule, ScheduleFrequency};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    struct FakeLauncher(Mutex<Vec<(String, Vec<String>)>>);
    impl crate::orchestrate::Launcher for FakeLauncher {
        async fn run(&self, cwd: &str, args: &[String]) -> Result<String, crate::orchestrate::Refusal> {
            self.0.lock().unwrap().push((cwd.into(), args.to_vec()));
            Ok("backgrounded · ab1234 · scheduled/run-1".into())
        }
        fn usage(&self) -> Option<alondra_core::usage::Usage> { None }
        fn plugin(&self) -> Option<std::path::PathBuf> { Some("/synthetic/claude-plugin".into()) }
    }

    #[test]
    fn calendar_matches_legacy_for_manual_disabled_and_due_slots() {
        let mut schedule = Schedule {
            id: "schedule-1".into(), title: "Review".into(), text: "Review project".into(),
            project: None, options: BTreeMap::new(), disallowed_tools: vec![],
            frequency: ScheduleFrequency::EveryHours, hour: 9, minute: 0,
            weekday: None, interval: Some(2), start_at: Some(1_000),
            enabled: true, next_run: Some(1_000), revision: 1, deleted_at: None,
        };
        assert_eq!(next_after(&schedule, 1_001), Some(7_201_000));
        schedule.enabled = false;
        schedule.next_run = None;
        assert_eq!(next_after(&schedule, 1_001), None);
        schedule.enabled = true;
        schedule.next_run = Some(1_000);
        schedule.frequency = ScheduleFrequency::Once;
        schedule.interval = None;
        schedule.start_at = Some(1_000);
        assert_eq!(next_after(&schedule, 1_000), None);
    }

    #[tokio::test]
    async fn captured_launch_uses_request_policy_and_explicit_new_node_command() {
        let root = std::env::temp_dir().join(format!("alondra-schedule-prep-{}", crate::auth::random_hex(8)));
        let project = root.join("App");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        let definition = ScheduleDefinitionSnapshot {
            title: "Review".into(), text: "Review project".into(), project: Some("App".into()),
            options: BTreeMap::new(), disallowed_tools: vec!["Bash".into()],
        };
        let intent = prepare_intent(
            &ScheduleId::from("schedule-1"), &ScheduleRunId::from("run-1"),
            &definition, &OptionsFile::default(), &root,
        ).unwrap();
        let fake = FakeLauncher(Mutex::new(vec![]));
        let session = invoke_claimed(&intent, "request-1", &definition.disallowed_tools, &fake).await.unwrap();
        assert_eq!(session.as_deref(), Some("ab1234"));
        let calls = fake.0.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, project.to_str().unwrap());
        let args = &calls[0].1;
        assert!(args.windows(2).any(|pair| pair == ["--name", "scheduled/schedule-run-1"]));
        assert!(args.windows(2).any(|pair| pair == ["--disallowedTools", "Bash"]));
        assert!(args.last().unwrap().contains("request-1"));
        drop(calls);
        std::fs::remove_dir_all(root).unwrap();
    }
}
