//! The schedule and the runtime the scheduler's tests run on: over the doubles of the engine in
//! `engine_sdk_test`, over the whole engine in `engine_sdk_engine_test`.
use super::*;
use crate::domain_runtime::DomainRuntime;
use alondra_domain_model::{Schedule, ScheduleFrequency};
use std::collections::BTreeMap;

/// The runtime is dropped before the folder it writes in: fields drop in order.
pub struct Fixture {
    pub domain: std::sync::Arc<DomainRuntime>,
    pub home: PathBuf,
    pub root: crate::test_support::Directory,
}
impl Fixture {
    #[cfg(not(feature = "whole-engine"))]
    pub fn open(home: &Path) -> DomainRuntime {
        crate::model_fixture::open(home).unwrap()
    }

    #[cfg(feature = "whole-engine")]
    pub fn open(home: &Path) -> DomainRuntime {
        let domain = crate::engine_fixtures::open(home).unwrap();
        crate::sdk_owner::fixtures::measure_healthy_machine(&domain);
        domain
    }

    pub fn new() -> Self {
        let root = crate::test_support::Directory::new();
        let home = root.join("home");
        let projects = root.join("projects");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir(&projects).unwrap();
        std::fs::write(
            options::path_in(&home),
            format!("projects_root = {:?}\n", projects.to_str().unwrap()),
        )
        .unwrap();
        let domain = std::sync::Arc::new(Self::open(&home));
        execute_system(
            &*domain,
            Command::CreateSchedule {
                schedule: Schedule {
                    paid_account_grant: None,
                    id: "schedule-1".into(),
                    title: "Review".into(),
                    text: "Review project".into(),
                    project: None,
                    options: BTreeMap::new(),
                    disallowed_tools: vec!["Bash".into()],
                    frequency: ScheduleFrequency::Daily,
                    hour: 9,
                    minute: 0,
                    weekday: None,
                    interval: None,
                    start_at: None,
                    enabled: false,
                    next_run: None,
                    revision: 1,
                    deleted_at: None,
                },
            },
        )
        .unwrap();
        Self { domain, home, root }
    }
    pub fn claim(&self, id: &str) {
        execute_system(
            &*self.domain,
            Command::ClaimScheduleRun {
                schedule_id: "schedule-1".into(),
                run_id: id.into(),
                origin: ScheduleRunOrigin::Manual,
                now: alondra_core::now_millis(),
                next_after_now: None,
                expected_schedule_revision: None,
            },
        )
        .unwrap();
    }
}

fn git_repository(dir: &Path) {
    alondra_engine_doubles::worktrees::repository(dir);
}

impl Fixture {
    /// Points the schedule at `name`, a git repository in the projects folder.
    pub fn in_project(&self, name: &str) -> PathBuf {
        let project = self.root.join("projects").join(name);
        git_repository(&project);
        let mut schedule = self.domain.state().unwrap().schedule.schedules[&ScheduleId::from("schedule-1")].clone();
        schedule.project = Some(name.into());
        schedule.revision += 1;
        execute_system(&*self.domain, Command::UpdateSchedule { schedule }).unwrap();
        project
    }

    /// How many nodes were started: each SDK launch registers its own.
    pub fn launches(&self) -> usize {
        self.domain.state().unwrap().agents.len()
    }
}
