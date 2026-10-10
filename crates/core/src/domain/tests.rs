use super::*;
use std::collections::{BTreeMap, BTreeSet};
fn user() -> Actor {
    Actor::User {
        user_id: "u".into(),
    }
}
fn agent() -> Actor {
    Actor::Agent {
        agent_id: "a".into(),
    }
}
fn agent_b() -> Actor {
    Actor::Agent {
        agent_id: "b".into(),
    }
}
fn register_b(s: &mut DomainState) {
    exec(s, Actor::System, Command::RegisterAgent { agent_id: "b".into(), name: "Instance B".into(), at: 0 }).unwrap();
}
/// The task waits in Closing for its node; Alondra records the node closed and the task is done.
fn end_node(s: &mut DomainState, task: &str) {
    assert_eq!(s.tasks[&task.into()].state, TaskState::Closing);
    exec(s, Actor::System, Command::RecordNodeEnd { task_id: task.into(), end: NodeEnd::Closed, agent_id: None }).unwrap();
}
/// Every card in Waiting for You has something the user can do now and nothing holding it back,
/// whatever state a test reached (the property of the cards design, checked after every command).
fn assert_cards_usable(s: &DomainState) {
    for item in s.projection().inbox.iter().filter(|item| item.pending) {
        assert!(item.blocked_by.is_empty(), "{} is listed while held back", item.id);
        assert!(item.available_actions.iter().any(|action| action.enabled), "{} is listed with nothing the user can do", item.id);
    }
}
fn exec(s: &mut DomainState, a: Actor, c: Command) -> Result<CommandOutcome, DomainError> {
    let outcome = s.execute(
        &a,
        &CommandEnvelope {
            protocol_version: 2,
            data_epoch: s.data_epoch.clone(),
            command_id: format!("c-{}", s.revision).into(),
            expected_revision: s.revision,
            command: c,
        },
    );
    assert_cards_usable(s);
    outcome
}
fn fixture() -> DomainState {
    let mut s = DomainState::new("epoch");
    exec(
        &mut s,
        Actor::System,
        Command::RegisterAgent {
            agent_id: "a".into(),
            name: "Instance A".into(), at: 0
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::CreateRequest {
            request_id: "r".into(),
            body: "Build requested result".into(), auto_merge: false, maintenance: false, message: false,
        },
    )
    .unwrap();
    exec(
        &mut s,
        orchestrator(),
        Command::CreateTask {
            task_id: "t".into(),
            request_id: "r".into(),
            title: "Result".into(),
            agent_id: Some("a".into()),
            review_prs: true,
            dependencies: BTreeSet::new(),
            stack: None,
            launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")),
        },
    )
    .unwrap();
    s.requests.get_mut(&"r".into()).unwrap().title = Some("Requested result".into());
    s
}
fn schedule() -> Schedule {
    Schedule {
        id: "schedule-1".into(),
        title: "Daily review".into(),
        text: "Review the project".into(),
        project: Some("alondra".into()),
        options: BTreeMap::new(),
        disallowed_tools: vec!["mcp__example__write-*".into()],
        frequency: ScheduleFrequency::Daily,
        hour: 9,
        minute: 0,
        weekday: None,
        interval: None,
        start_at: None,
        enabled: true,
        next_run: Some(100),
        revision: 1,
        deleted_at: None,
    }
}
fn scheduled_request_task(request_id: &str, task_id: &str) -> (Request, Task) {
    let request = Request::new(request_id.into(), "Review the project".into());
    let task = Task {
        id: task_id.into(),
        request_id: request.id.clone(),
        agent_id: None,
        title: "Updated review".into(),
        state: TaskState::NotStarted,
        revision: 0,
        started: false,
        review_prs: false,
        final_presentation: None,
        pr_id: None,
        dependencies: BTreeSet::new(),
        stack: None,
        documents: BTreeMap::new(),
        archived: false,
        model: None,
        delivered_dependencies: Default::default(), blocks: Default::default(), verdict: None, review_rounds: 0, wrap_up: WrapUp::default(), priority: Default::default(),
    };
    (request, task)
}
fn scheduled_launch() -> ScheduleLaunchIntent {
    let options = crate::options::OptionsFile::default()
        .resolve(Some("alondra"), &BTreeMap::new())
        .unwrap();
    ScheduleLaunchIntent {
        name: "scheduled/run-1".into(),
        cwd: "/synthetic/Projects/alondra".into(),
        prompt: "Review the project".into(),
        options_json: serde_json::to_string(&options).unwrap(),
    }
}
fn text() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::Text {
        text: "The result".into(),
    }]
}

#[test]
fn schedule_commands_preserve_claim_and_result_history() {
    let mut state = fixture();
    reject_unchanged(
        &mut state,
        user(),
        Command::CreateSchedule {
            schedule: schedule(),
        },
        "forbidden",
    );
    exec(
        &mut state,
        Actor::System,
        Command::CreateSchedule {
            schedule: schedule(),
        },
    )
    .unwrap();
    let original = state.schedule.schedules[&ScheduleId::from("schedule-1")].clone();
    let mut stale = original.clone();
    stale.title = "Stale edit".into();
    reject_unchanged(
        &mut state,
        Actor::System,
        Command::UpdateSchedule { schedule: stale },
        "stale_revision",
    );
    let mut updated = original;
    updated.revision += 1;
    updated.title = "Updated review".into();
    exec(
        &mut state,
        Actor::System,
        Command::UpdateSchedule { schedule: updated },
    )
    .unwrap();
    reject_unchanged(
        &mut state,
        user(),
        Command::ClaimScheduleRun {
            schedule_id: "schedule-1".into(),
            run_id: "run-1".into(),
            origin: ScheduleRunOrigin::Due,
            now: 100,
            next_after_now: Some(200),
            expected_schedule_revision: None,
        },
        "forbidden",
    );
    exec(
        &mut state,
        Actor::System,
        Command::ClaimScheduleRun {
            schedule_id: "schedule-1".into(),
            run_id: "run-1".into(),
            origin: ScheduleRunOrigin::Due,
            now: 100,
            next_after_now: Some(200),
            expected_schedule_revision: None,
        },
    )
    .unwrap();
    let run = &state.schedule.runs[&ScheduleRunId::from("run-1")];
    assert_eq!(run.due_at, Some(100));
    assert_eq!(run.definition_snapshot.as_ref().unwrap().title, "Updated review");
    let (request, mut task) = scheduled_request_task("scheduled-request", "scheduled-task");
    task.request_id = "wrong-request".into();
    reject_unchanged(
        &mut state,
        Actor::System,
        Command::RecordScheduleRunSent {
            run_id: "run-1".into(),
            request,
            task,
            launch: scheduled_launch(),
            finished_at: 101,
        },
        "invalid_input",
    );
    let (request, task) = scheduled_request_task("scheduled-request", "scheduled-task");
    exec(
        &mut state,
        Actor::System,
        Command::RecordScheduleRunSent {
            run_id: "run-1".into(),
            request,
            task,
            launch: scheduled_launch(),
            finished_at: 101,
        },
    )
    .unwrap();
    reject_unchanged(&mut state, user(), Command::AssignTask {
        task_id: "scheduled-task".into(), agent_id: "a".into(), model: None
    }, "conflict");
    reject_unchanged(&mut state, user(), Command::SetDependencies {
        task_id: "scheduled-task".into(), dependencies: ["t".into()].into_iter().collect(), stack: None,
    }, "conflict");
    state.tasks.get_mut(&TaskId::from("scheduled-task")).unwrap().agent_id = Some("a".into());
    state.validate().unwrap();
    reject_unchanged(&mut state, Actor::System, Command::ClaimScheduleLaunch {
        run_id: "run-1".into(), now: 102,
    }, "conflict");
    state.tasks.get_mut(&TaskId::from("scheduled-task")).unwrap().agent_id = None;
    state.tasks.get_mut(&TaskId::from("scheduled-task")).unwrap().dependencies = ["t".into()].into_iter().collect();
    reject_unchanged(&mut state, Actor::System, Command::ClaimScheduleLaunch {
        run_id: "run-1".into(), now: 102,
    }, "conflict");
    state.tasks.get_mut(&TaskId::from("scheduled-task")).unwrap().dependencies.clear();
    let failed = exec(
        &mut state,
        Actor::System,
        Command::RecordScheduleRunFailed {
            run_id: "run-1".into(),
            reason: "Workspace not trusted.\n".into(),
            finished_at: 102,
        },
    )
    .unwrap();
    assert_eq!(failed.effects, [
        DomainEffect::ScheduleLaunchFailed {
            task_id: "scheduled-task".into(),
            title: "Updated review".into(),
            reason: "Workspace not trusted.".into(),
        },
        DomainEffect::NotifyUser { task_id: Some("scheduled-task".into()), interaction_id: "problem-launch-run-1".into() },
    ]);
    let problem = &state.interactions[&"problem-launch-run-1".into()];
    assert_eq!((problem.card.kind, problem.author.clone()), (CardKind::Problem, Author::System));
    assert!(problem.card.need.contains("Updated review") && problem.card.why.contains("Workspace not trusted."), "{:?}", problem.card);
    assert_eq!(problem.card.content.as_ref().map(|c| c.kind), Some(ContentKind::Diagnosis));
    let run = &state.schedule.runs[&ScheduleRunId::from("run-1")];
    assert_eq!(run.state, ScheduleRunState::Failed);
    assert_eq!(run.finished_at, Some(101));
    assert_eq!(run.request_id, Some("scheduled-request".into()));
    assert_eq!(run.task_id, Some("scheduled-task".into()));
    assert_eq!(state.schedule.schedules[&ScheduleId::from("schedule-1")].next_run, Some(200));
    state.validate().unwrap();
}

#[test]
fn the_user_cancels_a_task_whose_scheduled_launch_failed() {
    let mut state = fixture();
    let mut definition = schedule();
    definition.title = "Updated review".into();
    exec(&mut state, Actor::System, Command::CreateSchedule { schedule: definition }).unwrap();
    exec(&mut state, Actor::System, Command::ClaimScheduleRun {
        schedule_id: "schedule-1".into(), run_id: "run-1".into(), origin: ScheduleRunOrigin::Due,
        now: 100, next_after_now: Some(200), expected_schedule_revision: None,
    }).unwrap();
    let (request, task) = scheduled_request_task("scheduled-request", "scheduled-task");
    exec(&mut state, Actor::System, Command::RecordScheduleRunSent {
        run_id: "run-1".into(), request, task, launch: scheduled_launch(), finished_at: 101,
    }).unwrap();
    exec(&mut state, Actor::System, Command::RecordScheduleRunFailed {
        run_id: "run-1".into(), reason: "Workspace not trusted.\n".into(), finished_at: 102,
    }).unwrap();
    let cancel = state.task_actions(&"scheduled-task".into()).unwrap()
        .into_iter().find(|action| action.kind == "cancel_task").unwrap();
    assert!(cancel.enabled, "{cancel:?}");
    exec(&mut state, user(), Command::CancelTask { task_id: "scheduled-task".into() }).unwrap();
    assert_eq!(state.tasks[&TaskId::from("scheduled-task")].state, TaskState::Cancelled);
    assert_eq!(state.schedule.runs[&ScheduleRunId::from("run-1")].state, ScheduleRunState::Failed);
    state.validate().unwrap();
}

#[test]
fn schedule_delete_and_restart_keep_history_without_replaying_due_slots() {
    let mut state = DomainState::new("epoch");
    exec(
        &mut state,
        Actor::System,
        Command::CreateSchedule {
            schedule: schedule(),
        },
    )
    .unwrap();
    exec(
        &mut state,
        Actor::System,
        Command::ClaimScheduleRun {
            schedule_id: "schedule-1".into(),
            run_id: "run-1".into(),
            origin: ScheduleRunOrigin::Due,
            now: 100,
            next_after_now: Some(200),
            expected_schedule_revision: None,
        },
    )
    .unwrap();
    let mut changed = state.schedule.schedules[&ScheduleId::from("schedule-1")].clone();
    changed.revision += 1;
    changed.title = "Changed while sending".into();
    reject_unchanged(
        &mut state,
        Actor::System,
        Command::UpdateSchedule { schedule: changed },
        "conflict",
    );
    reject_unchanged(
        &mut state,
        Actor::System,
        Command::DeleteSchedule {
            schedule_id: "schedule-1".into(),
            deleted_at: 150,
            expected_schedule_revision: None,
        },
        "conflict",
    );
    exec(
        &mut state,
        Actor::System,
        Command::InterruptScheduleRunsOnRestart,
    )
    .unwrap();
    assert_eq!(state.schedule.runs[&ScheduleRunId::from("run-1")].state, ScheduleRunState::Interrupted);
    reject_unchanged(
        &mut state,
        Actor::System,
        Command::ClaimScheduleRun {
            schedule_id: "schedule-1".into(),
            run_id: "run-2".into(),
            origin: ScheduleRunOrigin::Due,
            now: 100,
            next_after_now: Some(200),
            expected_schedule_revision: None,
        },
        "conflict",
    );
    exec(
        &mut state,
        Actor::System,
        Command::DeleteSchedule {
            schedule_id: "schedule-1".into(),
            deleted_at: 150,
            expected_schedule_revision: None,
        },
    )
    .unwrap();
    assert_eq!(state.schedule.schedules[&ScheduleId::from("schedule-1")].deleted_at, Some(150));
    reject_unchanged(
        &mut state,
        Actor::System,
        Command::ClaimScheduleRun {
            schedule_id: "schedule-1".into(),
            run_id: "run-2".into(),
            origin: ScheduleRunOrigin::Manual,
            now: 150,
            next_after_now: None,
            expected_schedule_revision: None,
        },
        "not_found",
    );
    state.validate().unwrap();
}

#[test]
fn schedule_state_deserializes_old_snapshots_and_rejects_broken_links() {
    let old = serde_json::json!({
        "data_epoch": "epoch", "revision": 0, "orchestrator_generation": 0,
        "requests": {}, "agents": {}, "tasks": {}, "documents": {},
        "document_revisions": {}, "document_reviews": [], "pull_requests": {},
        "interactions": {}, "merge_authorizations": {}, "stack_readiness": {}
    });
    let old: DomainState = serde_json::from_value(old).unwrap();
    assert_eq!(old.schedule, ScheduleState::default());
    let mut state = fixture();
    exec(&mut state, Actor::System, Command::CreateSchedule { schedule: schedule() }).unwrap();
    exec(&mut state, Actor::System, Command::ClaimScheduleRun {
        schedule_id: "schedule-1".into(), run_id: "run-1".into(),
        origin: ScheduleRunOrigin::Due, now: 100, next_after_now: Some(200),
        expected_schedule_revision: None,
    }).unwrap();
    let (request, mut task) = scheduled_request_task("scheduled-request", "scheduled-task");
    task.title = "Daily review".into();
    exec(&mut state, Actor::System, Command::RecordScheduleRunSent {
        run_id: "run-1".into(), request, task, launch: scheduled_launch(), finished_at: 101,
    }).unwrap();
    let encoded = serde_json::to_value(&state).unwrap();
    let decoded: DomainState = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, state);
    state.schedule.runs.get_mut(&ScheduleRunId::from("run-1")).unwrap().request_id = Some("other".into());
    assert_eq!(state.validate().unwrap_err().code, "invalid_input");
}
fn present(s: &mut DomainState, id: &str, targets: Vec<DeliveryTarget>) {
    exec(
        s,
        agent(),
        Command::PresentResult {
            attachments: vec![],
            task_id: "t".into(),
            interaction_id: id.into(),
            body: "Result ready".into(),
            targets,
            is_final: true, nothing_to_review: false, card: crate::domain::CardSpec::delivery("Review the result", &[], true),
        },
    )
    .unwrap();
}
fn reject_unchanged(s: &mut DomainState, a: Actor, c: Command, code: &str) {
    let before = s.clone();
    assert_eq!(exec(s, a, c).unwrap_err().code, code);
    assert_eq!(*s, before, "Rejected command changed state");
}
#[test]
fn information_read_is_not_a_reply_or_acceptance() {
    let mut s = fixture();
    exec(
        &mut s,
        orchestrator(),
        Command::PublishInformation {
            attachments: vec![],
            interaction_id: "info".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Useful result".into(), card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Read the update"),
        },
    )
    .unwrap();
    let task = s.tasks[&"t".into()].clone();
    let out = exec(
        &mut s,
        user(),
        Command::MarkRead {
            interaction_id: "info".into(),
        },
    )
    .unwrap();
    assert!(out.effects.is_empty());
    assert_eq!(s.tasks[&"t".into()], task);
    assert_eq!(s.projection().counts.unread_information, 0);
}
#[test]
fn explicit_acceptance_completes_the_task_and_feedback_revokes_it_first() {
    let mut s = fixture();
    present(&mut s, "delivery", text());
    reject_unchanged(
        &mut s,
        Actor::System,
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "delivery".into(),
        },
        "acceptance_missing",
    );
    exec(
        &mut s,
        user(),
        Command::SubmitFeedback {
            attachments: vec![],
            presentation_id: "delivery".into(),
            targets: text(),
            body: "One correction".into(),
            instruction: Instruction::Changes,
        },
    )
    .unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Running);
    exec(
        &mut s,
        Actor::System,
        Command::MoveNode { agent_id: "a".into(), event: NodeEvent::SessionLost, at: 0 },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::AcceptDelivery {
            presentation_id: "delivery".into(),
            targets: text(),
        },
    )
    .unwrap();
    end_node(&mut s, "t");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
    reject_unchanged(
        &mut s,
        Actor::System,
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "delivery".into(),
        },
        "task_not_active",
    );
}
#[test]
fn reassigning_a_task_waiting_for_its_merge_needs_fresh_acceptance() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    authorize(&mut s);
    assert_eq!(s.tasks[&"t".into()].state, TaskState::WaitingDependency, "accepted, waiting for Alondra's merge");
    exec(
        &mut s,
        Actor::System,
        Command::RegisterAgent {
            agent_id: "b".into(),
            name: "Instance B".into(), at: 0
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::AssignTask {
            task_id: "t".into(),
            agent_id: "b".into(), model: None
        },
    )
    .unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::ForYourReview);
    reject_unchanged(&mut s, Actor::System, complete(), "acceptance_missing");
    merged_by_someone_else(&mut s);
    end_node(&mut s, "t");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done, "a merged delivery needs no acceptance");
}
#[test]
fn an_option_carries_an_optional_description() {
    let ask = |id: &str, description: Option<&str>| Command::AskQuestion {
        attachments: vec![],
        interaction_id: id.into(),
        scope: Scope::Task { task_id: "t".into() },
        recipient: Recipient::User,
        body: "Which one?".into(),
        blocking: false,
        options: vec![QuestionOption {
            id: "keep".into(),
            label: "Keep it".into(),
            description: description.map(String::from),
            inputs: vec![],
        }], card: crate::domain::CardSpec::derived(crate::domain::CardKind::Provide, "Answer the question"),
    };
    let mut s = fixture();
    reject_unchanged(&mut s, agent(), ask("blank", Some("  ")), "invalid_input");
    exec(&mut s, agent(), ask("q", Some("Nothing changes for the release"))).unwrap();
    let InteractionPayload::Question { options, .. } = &s.interactions[&"q".into()].payload else { panic!("not a question") };
    assert_eq!(options[0].description.as_deref(), Some("Nothing changes for the release"));
    let wire = serde_json::to_value(&options[0]).unwrap();
    assert_eq!(wire["description"], "Nothing changes for the release");
    let bare = QuestionOption { id: "keep".into(), label: "Keep it".into(), description: None, inputs: vec![] };
    assert_eq!(serde_json::to_value(&bare).unwrap(), serde_json::json!({"id": "keep", "label": "Keep it", "inputs": []}),
        "an option without a description serializes as before");
    let older: QuestionOption = serde_json::from_value(serde_json::json!({"id": "keep", "label": "Keep it", "inputs": []})).unwrap();
    assert_eq!(older, bare);
}
#[test]
fn typed_option_ids_and_open_question_guard() {
    let mut s = fixture();
    present(&mut s, "d", text());
    exec(
        &mut s,
        agent(),
        Command::AskQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Explain decision".into(),
            blocking: true,
            options: vec![QuestionOption {
                id: "yes".into(),
                label: "Sí".into(),
                description: None,
                inputs: vec![InputSpec {
                    id: "why".into(),
                    kind: InputKind::Text,
                    required: true,
                }],
            }], card: crate::domain::CardSpec::derived(crate::domain::CardKind::Provide, "Answer the question"),
        },
    )
    .unwrap();
    reject_unchanged(&mut s, user(), Command::AcceptDelivery { presentation_id: "d".into(), targets: text() }, "acceptance_stale");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::NeedsYou);
    reject_unchanged(
        &mut s,
        Actor::System,
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "acceptance_stale",
    );
    reject_unchanged(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            option_id: "Sí".into(),
            inputs: BTreeMap::new(),
        },
        "invalid_input",
    );
    reject_unchanged(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            option_id: "yes".into(),
            inputs: BTreeMap::new(),
        },
        "invalid_input",
    );
    let out = exec(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            option_id: "yes".into(),
            inputs: BTreeMap::from([("why".into(), InputValue::Text("Reason".into()))]),
        },
    )
    .unwrap();
    assert_eq!(out.effects.len(), 1, "the node hears the answer; there is no delivery to bring back");
    present(&mut s, "d2", text());
    exec(&mut s, user(), Command::AcceptDelivery { presentation_id: "d2".into(), targets: text() }).unwrap();
    end_node(&mut s, "t");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done, "answered, the node delivers again and the Task completes");
}
fn register(s: &mut DomainState) {
    exec(
        s,
        agent(),
        Command::RegisterPr {
            task_id: "t".into(),
            pr_id: "pr".into(),
            host: "github.com".into(),
            repository: "owner/repo".into(),
            number: 1,
            head: "head1".into(),
            base: "main".into(),
            allowed_methods: vec![MergeMethod::Squash],
        },
    )
    .unwrap();
}
fn register_with(host: &str, head: &str) -> Command {
    Command::RegisterPr {
        task_id: "t".into(),
        pr_id: "pr".into(),
        host: host.into(),
        repository: "owner/repo".into(),
        number: 1,
        head: head.into(),
        base: "main".into(),
        allowed_methods: vec![MergeMethod::Squash],
    }
}
#[test]
fn a_host_that_is_not_a_real_host_is_refused_with_the_fix() {
    let mut s = fixture();
    for host in ["github", "localhost"] {
        let error = exec(&mut s, agent(), register_with(host, "head1")).unwrap_err();
        assert_eq!(error.code, "invalid_input");
        assert!(error.message.contains("use github.com"), "{}", error.message);
    }
    assert!(s.pull_requests.is_empty());
    exec(&mut s, agent(), register_with("github.example.com", "head1")).unwrap();
}
#[test]
fn a_pr_id_the_views_cannot_address_is_refused_with_the_fix() {
    let mut s = fixture();
    for pr_id in ["bitomule/alondra#688", "owner/repo#1", "pr 1", "pr:1"] {
        let mut command = register_with("github.com", "head1");
        if let Command::RegisterPr { pr_id: id, .. } = &mut command {
            *id = pr_id.into();
        }
        let error = exec(&mut s, agent(), command).unwrap_err();
        assert_eq!(error.code, "invalid_input", "{pr_id}");
        assert!(error.message.contains("github.com/owner/repo#N"), "{}", error.message);
    }
    assert!(s.pull_requests.is_empty());
    for pr_id in ["pr-1", "github.com/owner/repo#1"] {
        let mut s = fixture();
        let mut command = register_with("github.com", "head1");
        if let Command::RegisterPr { pr_id: id, .. } = &mut command {
            *id = pr_id.into();
        }
        exec(&mut s, agent(), command).unwrap();
    }
}
#[test]
fn the_owning_task_corrects_its_registration_of_the_same_pr() {
    let mut s = fixture();
    register(&mut s);
    exec(&mut s, agent(), register_with("github.com", "head2")).unwrap();
    let pr = &s.pull_requests[&"pr".into()];
    assert_eq!((pr.host.as_str(), pr.head.as_str()), ("github.com", "head2"));
    assert_eq!(s.pull_requests.len(), 1);
    reject_unchanged(
        &mut s,
        agent(),
        Command::RegisterPr {
            task_id: "t".into(),
            pr_id: "pr".into(),
            host: "github.com".into(),
            repository: "owner/repo".into(),
            number: 9,
            head: "head3".into(),
            base: "main".into(),
            allowed_methods: vec![MergeMethod::Squash],
        },
        "task_already_has_pr",
    );
}
#[test]
fn a_second_pr_for_a_task_is_refused_and_the_node_is_told_to_ask_for_a_new_task() {
    let mut s = fixture();
    register(&mut s);
    let mut second = register_with("github.com", "head9");
    if let Command::RegisterPr { pr_id, number, .. } = &mut second {
        *pr_id = "pr-2".into();
        *number = 2;
    }
    let error = exec(&mut s, agent(), second).unwrap_err();
    assert_eq!(error.code, "task_already_has_pr");
    assert!(error.message.starts_with("This task already has PR #1") && error.message.contains("ask the orchestrator for a new task"),
        "{}", error.message);
    assert_eq!(s.pull_requests.len(), 1);
}
fn pr_targets() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::PullRequest {
        pr_id: "pr".into(),
        head: "head1".into(),
    }]
}
fn authorize(s: &mut DomainState) {
    exec(
        s,
        user(),
        Command::AuthorizeMerge {
            authorization_id: "auth".into(),
            presentation_id: "d".into(),
            pr_id: "pr".into(),
            expected_head: "head1".into(),
            method: MergeMethod::Squash,
        },
    )
    .unwrap();
}
fn merged_by_someone_else(s: &mut DomainState) {
    exec(s, Actor::System, Command::ObservePrState { pr_id: "pr".into(), head: "head1".into(), base: "main".into(),
        state: PrState::Merged, merged_head: Some("head1".into()) }).unwrap();
}
fn complete() -> Command {
    Command::CompleteTask { task_id: "t".into(), presentation_id: "d".into() }
}
#[test]
fn an_observed_merge_completes_an_auto_merge_task_without_the_users_authorization() {
    let mut s = fixture();
    s.requests.get_mut(&"r".into()).unwrap().auto_merge = true;
    register(&mut s);
    present(&mut s, "d", pr_targets());
    reject_unchanged(&mut s, Actor::System, complete(), "merge_not_confirmed");
    merged_by_someone_else(&mut s);
    assert_eq!(s.pull_requests[&"pr".into()].state, PrState::Merged);
    end_node(&mut s, "t");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done, "the merge settles the delivery, no call needed");
}
#[test]
fn a_pr_merged_outside_alondra_while_in_review_closes_the_task_and_tells_the_node() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    assert_eq!(s.tasks[&"t".into()].state, TaskState::ForYourReview);
    let out = exec(&mut s, Actor::System, Command::ObservePrState { pr_id: "pr".into(), head: "head1".into(),
        base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()) }).unwrap();
    end_node(&mut s, "t");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done, "the user does not have to accept a merged delivery");
    assert_eq!(out.effects, vec![
        DomainEffect::NotifyAgent { agent_id: "a".into(), task_id: Some("t".into()), interaction_id: None, message: cleanup_request("Result", &"t".into()) },
    ]);
    assert_eq!(merged_notice(1, 0), "Pull request #1 is merged; no pull request of this task is left to merge.");
}
#[test]
fn a_node_can_neither_claim_a_merge_nor_complete_its_task() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    authorize(&mut s);
    let claim = Command::ClaimMergeAuthorization { authorization_id: "auth".into(), remote_head: "head1".into(),
        remote_base: "main".into(), base_verified: true };
    for command in [claim, complete()] {
        let before = s.clone();
        let error = exec(&mut s, agent(), command).unwrap_err();
        assert_eq!((error.code.as_str(), error.message.as_str()), ("forbidden", ALONDRA_MERGES));
        assert_eq!(s, before);
    }
}
#[test]
fn accepting_a_pr_delivery_authorizes_alondra_to_merge_it() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    let out = exec(&mut s, user(), Command::AcceptDelivery { presentation_id: "d".into(), targets: pr_targets() }).unwrap();
    let authorization = s.merge_authorizations.values().next().expect("an authorization for the PR");
    assert_eq!((authorization.head.as_str(), authorization.method, authorization.attempt, authorization.valid),
        ("head1", MergeMethod::Squash, MergeAttemptState::Unclaimed, true));
    assert_eq!(s.tasks[&"t".into()].state, TaskState::WaitingDependency, "accepted, waiting for Alondra's merge");
    assert!(!out.effects.iter().any(|e| matches!(e, DomainEffect::NotifyAgent { .. })), "nothing for the node to do: {:?}", out.effects);
}
#[test]
fn a_node_claim_left_from_before_is_taken_over_by_alondra() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    authorize(&mut s);
    let a = s.merge_authorizations.get_mut(&"auth".into()).unwrap();
    a.attempt = MergeAttemptState::Claimed;
    a.claimed_by = Some("a".into());
    let claim = Command::ClaimMergeAuthorization { authorization_id: "auth".into(), remote_head: "head1".into(),
        remote_base: "main".into(), base_verified: true };
    exec(&mut s, Actor::System, claim.clone()).unwrap();
    assert_eq!(s.merge_authorizations[&"auth".into()].claimed_by, None);
    reject_unchanged(&mut s, Actor::System, claim, "delivery_uncertain");
}
#[test]
fn alondra_authorizes_its_own_merge_only_for_an_untouched_auto_merge_delivery() {
    let auto = |id: &str| Command::AuthorizeMerge { authorization_id: id.into(), presentation_id: "d".into(),
        pr_id: "pr".into(), expected_head: "head1".into(), method: MergeMethod::Squash };
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    reject_unchanged(&mut s, Actor::System, auto("auto-1"), "forbidden");
    let mut s = fixture();
    s.requests.get_mut(&"r".into()).unwrap().auto_merge = true;
    register(&mut s);
    present(&mut s, "d", pr_targets());
    let out = exec(&mut s, Actor::System, auto("auto-1")).unwrap();
    assert!(out.effects.is_empty(), "nothing to tell anyone before the merge");
    let authorization = &s.merge_authorizations[&"auto-1".into()];
    assert_eq!((authorization.user_id.as_str(), authorization.valid), (Feedback::ALONDRA, true));
    assert!(s.pull_requests[&"pr".into()].review.as_ref().unwrap().decisions.is_empty(), "no review decision is made up");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::ForYourReview, "no acceptance is made up");
    exec(&mut s, user(), Command::SubmitFeedback { attachments: vec![], presentation_id: "d".into(), targets: pr_targets(),
        body: "Wait".into(), instruction: Instruction::Changes }).unwrap();
    assert!(!s.merge_authorizations[&"auto-1".into()].valid, "the user's feedback stops Alondra's merge");
}
#[test]
fn alondra_takes_over_an_uncertain_node_attempt() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    authorize(&mut s);
    let a = s.merge_authorizations.get_mut(&"auth".into()).unwrap();
    a.attempt = MergeAttemptState::Uncertain;
    a.claimed_by = Some("a".into());
    exec(&mut s, Actor::System, Command::ClaimMergeAuthorization { authorization_id: "auth".into(), remote_head: "head1".into(),
        remote_base: "main".into(), base_verified: true }).unwrap();
    let a = &s.merge_authorizations[&"auth".into()];
    assert_eq!((a.attempt, a.claimed_by.as_ref()), (MergeAttemptState::Claimed, None));
}
#[test]
fn a_merge_failure_reads_back_its_number_and_reason() {
    let body = merge_failed_text(7, "https://github.com/o/r/pull/7", "main", "its checks failed (lint).");
    assert_eq!(merge_failure(&body), Some((7, "its checks failed (lint)")));
    assert_eq!(merge_failure("The user asks you to resolve the conflicts"), None);
}
#[test]
fn a_claimed_attempt_of_a_superseded_delivery_does_not_hold_back_the_new_merge() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    authorize(&mut s);
    let a = s.merge_authorizations.get_mut(&"auth".into()).unwrap();
    a.attempt = MergeAttemptState::Claimed;
    a.claimed_by = Some("a".into());
    let again = |id: &str, presentation: &str, head: &str| Command::AuthorizeMerge { authorization_id: id.into(),
        presentation_id: presentation.into(), pr_id: "pr".into(), expected_head: head.into(), method: MergeMethod::Squash };
    reject_unchanged(&mut s, user(), again("auth-same", "d", "head1"), "delivery_uncertain");
    exec(&mut s, Actor::System, Command::ObservePrState { pr_id: "pr".into(), head: "head2".into(), base: "main".into(),
        state: PrState::Open, merged_head: None }).unwrap();
    present(&mut s, "d2", vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head2".into() }]);
    exec(&mut s, user(), again("auth2", "d2", "head2")).unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::WaitingDependency);
}
#[test]
fn authorization_merge_and_completion_are_three_distinct_steps() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    authorize(&mut s);
    assert_eq!(s.pull_requests[&"pr".into()].state, PrState::Open);
    reject_unchanged(
        &mut s,
        Actor::System,
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "merge_not_confirmed",
    );
    exec(
        &mut s,
        Actor::System,
        Command::ClaimMergeAuthorization {
            authorization_id: "auth".into(),
            remote_head: "head1".into(),
            remote_base: "main".into(),
            base_verified: true,
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        Actor::System,
        Command::ClaimMergeAuthorization {
            authorization_id: "auth".into(),
            remote_head: "head1".into(),
            remote_base: "main".into(),
            base_verified: true,
        },
        "delivery_uncertain",
    );
    exec(
        &mut s,
        Actor::System,
        Command::ObservePrState {
            pr_id: "pr".into(),
            head: "head1".into(),
            base: "main".into(),
            state: PrState::Merged,
            merged_head: Some("head1".into()),
        },
    )
    .unwrap();
    end_node(&mut s, "t");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done, "the observed merge of the authorized head completes it");
}
#[test]
fn head_push_invalidates_authorization_and_merge_external_does_not_accept() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    authorize(&mut s);
    exec(
        &mut s,
        Actor::System,
        Command::ObservePrState {
            pr_id: "pr".into(),
            head: "head2".into(),
            base: "main".into(),
            state: PrState::Open,
            merged_head: None,
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        Actor::System,
        Command::ClaimMergeAuthorization {
            authorization_id: "auth".into(),
            remote_head: "head2".into(),
            remote_base: "main".into(),
            base_verified: true,
        },
        "acceptance_stale",
    );
    assert!(s.interactions[&"d".into()].withdrawn, "the delivery behind the PR is withdrawn");

    assert!(!s.merge_authorizations[&"auth".into()].valid);
}
#[test]
fn lifetime_pr_survives_cancel_reopen_and_register_is_noop() {
    let mut s = fixture();
    register(&mut s);
    let before = s.clone();
    register(&mut s);
    assert_eq!(before, s);
    exec(
        &mut s,
        user(),
        Command::CancelTask {
            task_id: "t".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::ReopenTask {
            task_id: "t".into(),
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        agent(),
        Command::RegisterPr {
            task_id: "t".into(),
            pr_id: "pr2".into(),
            host: "github.com".into(),
            repository: "owner/repo".into(),
            number: 2,
            head: "other".into(),
            base: "main".into(),
            allowed_methods: vec![MergeMethod::Squash],
        },
        "task_already_has_pr",
    );
}
#[test]
fn closed_request_prevents_reopening_its_terminal_task() {
    let mut s = fixture();
    exec(
        &mut s,
        user(),
        Command::CancelTask {
            task_id: "t".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::CloseRequest {
            request_id: "r".into(),
            answer: None,
            withdrawn: false,
            title: None,
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        user(),
        Command::ReopenTask {
            task_id: "t".into(),
        },
        "not_found",
    );
    assert!(s.requests[&"r".into()].closed);
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Cancelled);
}
fn orchestrator() -> Actor {
    Actor::Orchestrator { generation: 0 }
}
#[test]
fn a_request_that_needed_no_task_closes_with_its_answer_title_or_withdrawal() {
    let mut s = fixture();
    exec(&mut s, user(), Command::CreateRequest { request_id: "q".into(), body: "Is main green?".into(), auto_merge: false, maintenance: false, message: false }).unwrap();
    reject_unchanged(&mut s, agent(), Command::CloseRequest { request_id: "q".into(), answer: Some("Yes".into()), withdrawn: false, title: None }, "forbidden");
    reject_unchanged(&mut s, orchestrator(), Command::CloseRequest { request_id: "q".into(), answer: Some(" ".into()), withdrawn: false, title: None }, "invalid_input");
    exec(&mut s, orchestrator(), Command::CloseRequest { request_id: "q".into(), answer: Some("Main is green".into()),
        withdrawn: false, title: Some("CI status".into()) }).unwrap();
    let closed = &s.requests[&"q".into()];
    assert!(closed.closed && !closed.withdrawn);
    assert_eq!((closed.answer.as_deref(), closed.title.as_deref()), (Some("Main is green"), Some("CI status")));
    exec(&mut s, user(), Command::CreateRequest { request_id: "w".into(), body: "Never mind".into(), auto_merge: false, maintenance: false, message: false }).unwrap();
    exec(&mut s, user(), Command::CloseRequest { request_id: "w".into(), answer: None, withdrawn: true, title: None }).unwrap();
    assert!(s.requests[&"w".into()].withdrawn);
    reject_unchanged(&mut s, user(), Command::CloseRequest { request_id: "r".into(), answer: None, withdrawn: true, title: None }, "task_not_active");
}
/// Every `alondra send` used to be a Request whose answer became a card for the user.
#[test]
fn a_maintenance_message_closes_without_a_card_unless_it_asked_for_an_answer() {
    let mut s = fixture();
    let cards = |s: &DomainState| s.interactions.values().filter(|i| i.recipient == Recipient::User).count();
    let info = |id: &str, request: &str| Command::PublishInformation {
        attachments: vec![], interaction_id: id.into(), scope: Scope::Orchestrator { request_id: Some(request.into()) },
        recipient: Recipient::User, body: "Done".into(), card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Done"),
    };
    exec(&mut s, user(), Command::CreateRequest { request_id: "m".into(), body: "Launch two more".into(), auto_merge: false, maintenance: true, message: false }).unwrap();
    let error = exec(&mut s, orchestrator(), info("m-info", "m")).unwrap_err();
    assert_eq!(error.code, "maintenance_request");
    assert!(error.message.contains("--reply"), "{error}");
    let closed = exec(&mut s, orchestrator(), Command::CloseRequest { request_id: "m".into(), answer: Some("Launched".into()),
        withdrawn: false, title: Some("Two more nodes".into()) }).unwrap();
    assert_eq!(cards(&s), 0);
    assert!(!closed.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { .. })));
    exec(&mut s, user(), Command::CreateRequest { request_id: "q".into(), body: "Is main green?".into(), auto_merge: false, maintenance: false, message: false }).unwrap();
    exec(&mut s, orchestrator(), Command::CloseRequest { request_id: "q".into(), answer: Some("Main is green".into()),
        withdrawn: false, title: None }).unwrap();
    assert_eq!(cards(&s), 1);
    exec(&mut s, user(), Command::CreateRequest { request_id: "q2".into(), body: "And now?".into(), auto_merge: false, maintenance: false, message: false }).unwrap();
    exec(&mut s, orchestrator(), info("q2-info", "q2")).unwrap();
    assert_eq!(cards(&s), 2);
}
#[test]
fn the_orchestrator_names_a_request_with_a_short_title() {
    let mut s = fixture();
    reject_unchanged(&mut s, agent(), Command::TitleRequest { request_id: "r".into(), title: "Mine".into() }, "forbidden");
    reject_unchanged(&mut s, orchestrator(), Command::TitleRequest { request_id: "r".into(), title: " ".into() }, "invalid_input");
    reject_unchanged(&mut s, orchestrator(), Command::TitleRequest { request_id: "r".into(), title: "x".repeat(61) }, "invalid_input");
    reject_unchanged(&mut s, orchestrator(), Command::TitleRequest { request_id: "missing".into(), title: "X".into() }, "not_found");
    exec(&mut s, orchestrator(), Command::TitleRequest { request_id: "r".into(), title: "x".repeat(60) }).unwrap();
    exec(&mut s, orchestrator(), Command::TitleRequest { request_id: "r".into(), title: "Conflicts before push".into() }).unwrap();
    assert_eq!(s.requests[&"r".into()].title.as_deref(), Some("Conflicts before push"));
}
fn task_in(request: &str, id: &str) -> Command {
    Command::CreateTask { task_id: id.into(), request_id: request.into(), title: id.into(), agent_id: None,
        review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")), }
}
#[test]
fn a_task_never_exists_without_its_launch_plan() {
    let mut s = fixture();
    exec(&mut s, user(), Command::CreateRequest { request_id: "weekly".into(), body: "Weekly improvement".into(), auto_merge: false, maintenance: false, message: false }).unwrap();
    let mut planless = task_in("weekly", "registry");
    if let Command::CreateTask { launch, .. } = &mut planless {
        *launch = None;
    }
    let before = s.clone();
    let error = exec(&mut s, orchestrator(), planless).unwrap_err();
    assert_eq!((error.code.as_str(), error.subject.as_deref()), ("launch_plan_required", Some("launch")));
    assert!(error.message.contains("name and cwd"), "{error}");
    assert_eq!(s, before, "no task is left behind");
    let mut folderless = task_in("weekly", "registry");
    if let Command::CreateTask { launch: Some(plan), .. } = &mut folderless {
        plan.cwd = "  ".into();
    }
    let error = exec(&mut s, orchestrator(), folderless).unwrap_err();
    assert_eq!((error.code.as_str(), error.subject.as_deref()), ("launch_plan_required", Some("launch.cwd")));
    exec(&mut s, orchestrator(), task_in("weekly", "registry")).unwrap();
    assert!(s.tasks.contains_key(&"registry".into()));
}
#[test]
fn only_the_orchestrator_creates_tasks() {
    let mut s = fixture();
    exec(&mut s, user(), Command::CreateRequest { request_id: "two".into(), body: "Two things".into(), auto_merge: false, maintenance: false, message: false }).unwrap();
    reject_unchanged(&mut s, user(), task_in("two", "mine"), "forbidden");
    reject_unchanged(&mut s, agent(), task_in("two", "mine"), "forbidden");
    reject_unchanged(&mut s, Actor::System, task_in("two", "mine"), "forbidden");
    exec(&mut s, orchestrator(), task_in("two", "mine")).unwrap();
}
#[test]
fn a_request_needs_its_title_before_it_gets_a_second_task() {
    let mut s = fixture();
    exec(&mut s, user(), Command::CreateRequest { request_id: "two".into(), body: "Two things".into(), auto_merge: false, maintenance: false, message: false }).unwrap();
    exec(&mut s, orchestrator(), task_in("two", "first")).unwrap();
    let error = exec(&mut s, orchestrator(), task_in("two", "second")).unwrap_err();
    assert_eq!((error.code.as_str(), error.subject.as_deref()), ("request_title_required", Some("request.title")));
    assert!(error.message.contains("title_request"), "{error}");
    assert!(!s.tasks.contains_key(&"second".into()));
    exec(&mut s, orchestrator(), Command::TitleRequest { request_id: "two".into(), title: "Two things".into() }).unwrap();
    exec(&mut s, orchestrator(), task_in("two", "second")).unwrap();
    assert!(s.tasks.contains_key(&"second".into()));
}
#[test]
fn a_manager_retitles_a_task_and_moves_its_revision() {
    let mut s = fixture();
    let before = s.tasks[&"t".into()].revision;
    reject_unchanged(&mut s, agent(), Command::RetitleTask { task_id: "t".into(), title: "Mine".into() }, "forbidden");
    reject_unchanged(&mut s, orchestrator(), Command::RetitleTask { task_id: "t".into(), title: "".into() }, "invalid_input");
    reject_unchanged(&mut s, orchestrator(), Command::RetitleTask { task_id: "missing".into(), title: "X".into() }, "not_found");
    exec(&mut s, orchestrator(), Command::RetitleTask { task_id: "t".into(), title: "Editor colours".into() }).unwrap();
    assert_eq!(s.tasks[&"t".into()].title, "Editor colours");
    assert!(s.tasks[&"t".into()].revision > before);
}
#[test]
fn only_its_author_withdraws_an_open_question_which_then_neither_blocks_nor_takes_an_answer() {
    let mut s = fixture();
    let option = QuestionOption { id: "yes".into(), label: "Yes".into(), description: None, inputs: vec![] };
    exec(&mut s, agent(), Command::AskQuestion { attachments: vec![], interaction_id: "q".into(), scope: Scope::Task { task_id: "t".into() },
        recipient: Recipient::User, body: "Ship?".into(), blocking: true, options: vec![option.clone()], card: crate::domain::CardSpec::derived(crate::domain::CardKind::Provide, "Answer the question") }).unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::NeedsYou);
    reject_unchanged(&mut s, user(), Command::Withdraw { interaction_id: "q".into() }, "forbidden");
    reject_unchanged(&mut s, orchestrator(), Command::Withdraw { interaction_id: "q".into() }, "forbidden");
    exec(&mut s, agent(), Command::Withdraw { interaction_id: "q".into() }).unwrap();
    assert!(s.interactions[&"q".into()].withdrawn);
    assert_ne!(s.tasks[&"t".into()].state, TaskState::NeedsYou, "a withdrawn question no longer blocks");
    let action = s.projection().inbox.iter()
        .find(|i| matches!(&i.subject, InboxSubject::Interaction { interaction_id } if interaction_id.0 == "q"))
        .map(|i| i.available_actions[0].clone());
    assert_eq!(action.map(|a| (a.enabled, a.disabled_reason)), Some((false, Some("withdrawn".into()))));
    reject_unchanged(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "q".into(), option_id: "yes".into(),
        inputs: BTreeMap::new() }, "withdrawn");
    exec(&mut s, agent(), Command::AskQuestion { attachments: vec![], interaction_id: "q2".into(), scope: Scope::Task { task_id: "t".into() },
        recipient: Recipient::User, body: "Again?".into(), blocking: false, options: vec![option], card: crate::domain::CardSpec::derived(crate::domain::CardKind::Provide, "Answer the question") }).unwrap();
    exec(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "q2".into(), option_id: "yes".into(),
        inputs: BTreeMap::new() }).unwrap();
    reject_unchanged(&mut s, agent(), Command::Withdraw { interaction_id: "q2".into() }, "question_already_answered");
    exec(&mut s, orchestrator(), Command::AskQuestion { attachments: vec![], interaction_id: "oq".into(),
        scope: Scope::Orchestrator { request_id: Some("r".into()) }, recipient: Recipient::User, body: "Scope?".into(), blocking: false,
        options: vec![QuestionOption { id: "a".into(), label: "A".into(), description: None, inputs: vec![] }], card: crate::domain::CardSpec::derived(crate::domain::CardKind::Provide, "Answer the question") }).unwrap();
    exec(&mut s, orchestrator(), Command::Withdraw { interaction_id: "oq".into() }).unwrap();
    assert!(s.interactions[&"oq".into()].withdrawn);
}
#[test]
fn document_revision_invalidates_entire_joint_delivery() {
    let mut s = fixture();
    exec(
        &mut s,
        agent(),
        Command::CreateDocument {
            task_id: "t".into(),
            document_id: "doc".into(),
            title: "Design".into(),
            revision_id: "v1".into(),
            content_digest: "a".repeat(64),
            content_reference: "https://example.org/design".into(),
            role: "result".into(),
        },
    )
    .unwrap();
    let targets = vec![
        text()[0].clone(),
        DeliveryTarget::DocumentRevision {
            document_id: "doc".into(),
            revision_id: "v1".into(),
        },
    ];
    present(&mut s, "d", targets.clone());
    exec(
        &mut s,
        user(),
        Command::DecideDocumentReview {
            task_id: "t".into(),
            document_id: "doc".into(),
            revision_id: "v1".into(),
            accepted: true,
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        Actor::System,
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "acceptance_missing",
    );
    exec(
        &mut s,
        agent(),
        Command::ReviseDocument {
            task_id: "t".into(),
            document_id: "doc".into(),
            revision_id: "v2".into(),
            content_digest: "b".repeat(64),
            content_reference: "https://example.org/design".into(),
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        user(),
        Command::AcceptDelivery {
            presentation_id: "d".into(),
            targets,
        },
        "acceptance_stale",
    );
    assert_eq!(s.document_revisions.len(), 2);
    assert_ne!(s.tasks[&"t".into()].state, TaskState::Done);
}
#[test]
fn only_a_manager_can_link_another_tasks_document() {
    let mut s = fixture();
    exec(
        &mut s,
        Actor::System,
        Command::RegisterAgent {
            agent_id: "b".into(),
            name: "Instance B".into(), at: 0
        },
    )
    .unwrap();
    exec(
        &mut s,
        orchestrator(),
        Command::CreateTask {
            task_id: "t2".into(),
            request_id: "r".into(),
            title: "Second task".into(),
            agent_id: Some("b".into()),
            review_prs: false,
            dependencies: BTreeSet::new(),
            stack: None,
            launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")),
        },
    )
    .unwrap();
    exec(
        &mut s,
        agent(),
        Command::CreateDocument {
            task_id: "t".into(),
            document_id: "doc".into(),
            title: "Private draft".into(),
            revision_id: "v1".into(),
            content_digest: "a".repeat(64),
            content_reference: "files/draft".into(),
            role: "result".into(),
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        Actor::Agent {
            agent_id: "b".into(),
        },
        Command::LinkTaskDocument {
            task_id: "t2".into(),
            document_id: "doc".into(),
            role: "result".into(),
        },
        "forbidden",
    );
    assert!(!s.tasks[&"t2".into()].documents.contains_key(&"doc".into()));
    exec(
        &mut s,
        user(),
        Command::LinkTaskDocument {
            task_id: "t2".into(),
            document_id: "doc".into(),
            role: "shared".into(),
        },
    )
    .unwrap();
    assert!(s.tasks[&"t2".into()].documents.contains_key(&"doc".into()));
}
#[test]
fn sole_document_review_accepts_exact_final_delivery() {
    let mut s = fixture();
    exec(
        &mut s,
        agent(),
        Command::CreateDocument {
            task_id: "t".into(),
            document_id: "doc".into(),
            title: "Design".into(),
            revision_id: "v1".into(),
            content_digest: "a".repeat(64),
            content_reference: "files/design".into(),
            role: "result".into(),
        },
    )
    .unwrap();
    present(
        &mut s,
        "d",
        vec![DeliveryTarget::DocumentRevision {
            document_id: "doc".into(),
            revision_id: "v1".into(),
        }],
    );
    exec(
        &mut s,
        user(),
        Command::DecideDocumentReview {
            task_id: "t".into(),
            document_id: "doc".into(),
            revision_id: "v1".into(),
            accepted: true,
        },
    )
    .unwrap();
    end_node(&mut s, "t");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
}

#[test]
fn accepting_a_text_delivery_moves_the_task_to_closing_and_asks_its_node_to_clean_up() {
    let mut state = fixture();
    present(&mut state, "final", text());
    let accepted = exec(&mut state, user(), Command::AcceptDelivery {
        presentation_id: "final".into(), targets: text(),
    }).unwrap();
    assert_eq!(state.tasks[&"t".into()].state, TaskState::Closing);
    assert_eq!(state.tasks[&"t".into()].wrap_up.asked_at, Some(state.revision));
    assert_eq!(accepted.effects, vec![DomainEffect::NotifyAgent { agent_id: "a".into(), task_id: Some("t".into()),
        interaction_id: None, message: cleanup_request("Result", &"t".into()) }]);
    end_node(&mut state, "t");
    assert_eq!(state.tasks[&"t".into()].state, TaskState::Done);
}
#[test]
fn a_task_left_closing_by_an_older_daemon_completes_with_the_next_command() {
    let mut state = fixture();
    present(&mut state, "final", text());
    exec(&mut state, user(), Command::AcceptDelivery {
        presentation_id: "final".into(), targets: text(),
    }).unwrap();
    state.tasks.get_mut(&"t".into()).unwrap().state = TaskState::Closing;
    exec(&mut state, Actor::System, Command::MoveNode { agent_id: "a".into(), event: NodeEvent::TurnStarted, at: 0 }).unwrap();
    end_node(&mut state, "t");
    assert_eq!(state.tasks[&"t".into()].state, TaskState::Done);
}
/// The release of 0.2.51: the node asked whether to merge its PR, the PR merged, and the final
/// delivery the user accepted named only the published tag.
#[test]
fn an_accepted_text_delivery_whose_registered_pr_merged_closes_its_node() {
    let mut state = fixture();
    register(&mut state);
    merged_by_someone_else(&mut state);
    present(&mut state, "final", text());
    let accepted = exec(&mut state, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets: text() }).unwrap();
    let task = &state.tasks[&"t".into()];
    assert!(state.finished(task), "the accepted delivery is settled");
    assert_eq!((task.state, task.wrap_up.asked_at), (TaskState::Closing, Some(state.revision)));
    assert_eq!(accepted.effects, vec![DomainEffect::NotifyAgent { agent_id: "a".into(), task_id: Some("t".into()),
        interaction_id: None, message: cleanup_request("Result", &"t".into()) }]);
    end_node(&mut state, "t");
    assert_eq!(state.tasks[&"t".into()].state, TaskState::Done);
}
#[test]
fn a_presented_pr_closed_after_the_user_accepted_it_closes_the_node() {
    let mut state = fixture();
    register(&mut state);
    present(&mut state, "d", pr_targets());
    exec(&mut state, user(), Command::AcceptDelivery { presentation_id: "d".into(), targets: pr_targets() }).unwrap();
    assert_eq!((state.tasks[&"t".into()].state, state.tasks[&"t".into()].wrap_up.asked_at), (TaskState::WaitingDependency, None), "Alondra merges it first");
    exec(&mut state, Actor::System, Command::ObservePrState { pr_id: "pr".into(), head: "head1".into(), base: "main".into(),
        state: PrState::Closed, merged_head: None }).unwrap();
    assert!(state.tasks[&"t".into()].wrap_up.asked_at.is_some(), "nothing will merge it, so the node is asked to clean up");
    end_node(&mut state, "t");
    assert_eq!(state.tasks[&"t".into()].state, TaskState::Done);
}
/// Every task in Closing either waits for its node to clean up and be closed, or for an open PR
/// that Alondra merges or GitHub settles.
#[test]
fn no_task_stays_closing_without_a_way_out() {
    let closing_has_a_way_out = |s: &DomainState| {
        let t = &s.tasks[&"t".into()];
        t.state != TaskState::Closing
            || (s.in_cleanup(t) && s.wraps_up(t))
            || t.pr_id.as_ref().is_some_and(|pr| s.pull_requests[pr].state == PrState::Open)
    };
    for presented in [false, true] {
        for end in [PrState::Open, PrState::Merged, PrState::Closed] {
            for accept in [false, true] {
                let mut s = fixture();
                register(&mut s);
                let observe = |s: &mut DomainState| {
                    exec(s, Actor::System, Command::ObservePrState { pr_id: "pr".into(), head: "head1".into(), base: "main".into(),
                        state: end, merged_head: (end == PrState::Merged).then(|| "head1".into()) }).unwrap();
                };
                let targets = if presented { pr_targets() } else { text() };
                if !presented && end != PrState::Open {
                    observe(&mut s);
                }
                let shown = exec(&mut s, agent(), Command::PresentResult {
                    attachments: vec![], task_id: "t".into(), interaction_id: "d".into(), body: "Result ready".into(),
                    targets: targets.clone(), is_final: true, nothing_to_review: false, card: crate::domain::CardSpec::delivery("Review the result", &[], true),
                });
                if shown.is_err() {
                    assert!(!presented && end == PrState::Open, "only an open PR left out of the delivery is refused");
                    continue;
                }
                if accept {
                    exec(&mut s, user(), Command::AcceptDelivery { presentation_id: "d".into(), targets }).unwrap();
                }
                if presented && end != PrState::Open {
                    observe(&mut s);
                }
                exec(&mut s, Actor::System, Command::ReconcileNodes { listed: [("a".into(), Listed::Idle)].into(), at: 0 }).unwrap();
                assert!(closing_has_a_way_out(&s), "presented={presented} {end:?} accepted={accept}: {:?}", s.tasks[&"t".into()]);
            }
        }
    }
}
#[test]
fn cross_task_delivery_and_dependency_cycle_roll_back() {
    let mut s = fixture();
    present(&mut s, "d", text());
    register_b(&mut s);
    exec(
        &mut s,
        orchestrator(),
        Command::CreateTask {
            task_id: "t2".into(),
            request_id: "r".into(),
            title: "Other".into(),
            agent_id: Some("b".into()),
            review_prs: false,
            dependencies: BTreeSet::from(["t".into()]),
            stack: None,
            launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")),
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        user(),
        Command::SetDependencies {
            task_id: "t".into(),
            dependencies: BTreeSet::from(["t2".into()]),
            stack: None,
        },
        "dependency_cycle",
    );
    reject_unchanged(
        &mut s,
        agent_b(),
        Command::CompleteTask {
            task_id: "t2".into(),
            presentation_id: "d".into(),
        },
        "forbidden",
    );
    reject_unchanged(
        &mut s,
        agent_b(),
        Command::StartTask {
            task_id: "t2".into(),
        },
        "dependency_not_done",
    );
}
fn second(agent_id: Option<&str>) -> Command {
    Command::CreateTask {
        task_id: "t2".into(),
        request_id: "r".into(),
        title: "Second".into(),
        agent_id: agent_id.map(Into::into),
        review_prs: false,
        dependencies: BTreeSet::new(),
        stack: None,
        launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")),
    }
}
#[test]
fn an_agent_works_on_one_task_until_it_ends() {
    let mut s = fixture();
    reject_unchanged(&mut s, orchestrator(), second(Some("a")), "agent_busy");
    exec(&mut s, orchestrator(), second(None)).unwrap();
    let take = Command::AssignTask { task_id: "t2".into(), agent_id: "a".into(), model: None };
    reject_unchanged(&mut s, user(), take.clone(), "agent_busy");
    exec(&mut s, user(), Command::AssignTask { task_id: "t".into(), agent_id: "a".into(), model: None }).unwrap();
    exec(&mut s, user(), Command::CancelTask { task_id: "t".into() }).unwrap();
    exec(&mut s, user(), take).unwrap();
    assert_eq!(s.tasks[&"t2".into()].agent_id, Some("a".into()), "a node whose task ended takes the next one");
    exec(&mut s, user(), Command::ReopenTask { task_id: "t".into() }).unwrap();
    assert_eq!(
        (s.tasks[&"t".into()].state, s.tasks[&"t".into()].agent_id.clone()),
        (TaskState::Running, None),
        "a reopened task whose node moved on waits for another node"
    );
    assert_eq!(s.busy_with(&"a".into(), &"t".into()).map(|t| t.id.0.as_str()), Some("t2"));
}
#[test]
fn an_assignment_records_the_model_and_who_chose_it_and_old_records_still_load() {
    let mut s = fixture();
    let launched = TaskModel { model: "opus".into(), chosen_by: ModelChooser::Orchestrator };
    exec(&mut s, user(), Command::AssignTask { task_id: "t".into(), agent_id: "a".into(), model: Some(launched.clone()) }).unwrap();
    assert_eq!(s.tasks[&"t".into()].model, Some(launched.clone()));
    exec(&mut s, user(), Command::AssignTask { task_id: "t".into(), agent_id: "a".into(), model: None }).unwrap();
    assert_eq!(s.tasks[&"t".into()].model, Some(launched.clone()), "an assignment that names no model keeps the one recorded");

    let saved = serde_json::to_value(&s).unwrap();
    assert_eq!(saved["tasks"]["t"]["model"], serde_json::json!({"model": "opus", "chosen_by": "orchestrator"}));
    let mut old = saved.clone();
    old["tasks"]["t"].as_object_mut().unwrap().remove("model");
    let loaded: DomainState = serde_json::from_value(old).unwrap();
    assert_eq!(loaded.tasks[&"t".into()].model, None, "state saved before models were recorded loads without one");
    assert_eq!(serde_json::from_value::<DomainState>(saved).unwrap().tasks[&"t".into()].model, Some(launched));

    let old_event = serde_json::json!({"kind": "assign_task", "task_id": "t", "agent_id": "a"});
    let command: Command = serde_json::from_value(old_event).unwrap();
    assert_eq!(command, Command::AssignTask { task_id: "t".into(), agent_id: "a".into(), model: None });
}
#[test]
fn state_saved_with_several_open_tasks_on_one_agent_still_loads() {
    let mut s = fixture();
    exec(&mut s, orchestrator(), second(None)).unwrap();
    s.tasks.get_mut(&"t2".into()).unwrap().agent_id = Some("a".into());
    s.validate().unwrap();
    exec(&mut s, agent(), Command::StartTask { task_id: "t2".into() }).unwrap();
    exec(&mut s, orchestrator(), Command::CreateTask { task_id: "t3".into(), request_id: "r".into(), title: "Third".into(),
        agent_id: None, review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")), }).unwrap();
    reject_unchanged(&mut s, user(), Command::AssignTask { task_id: "t3".into(), agent_id: "a".into(), model: None }, "agent_busy");
}
#[test]
fn old_epoch_revision_generation_and_actor_rejected() {
    let mut s = fixture();
    let before = s.clone();
    let mut e = CommandEnvelope {
        protocol_version: 1,
        data_epoch: s.data_epoch.clone(),
        command_id: "x".into(),
        expected_revision: s.revision,
        command: Command::StartTask {
            task_id: "t".into(),
        },
    };
    assert_eq!(
        s.execute(&agent(), &e).unwrap_err().code,
        "incompatible_protocol"
    );
    e.protocol_version = 2;
    e.data_epoch = "old".into();
    assert_eq!(s.execute(&agent(), &e).unwrap_err().code, "stale_epoch");
    assert_eq!(s, before);
    exec(
        &mut s,
        Actor::System,
        Command::AdvanceOrchestratorGeneration { generation: 2 },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        Actor::Orchestrator { generation: 1 },
        Command::StartTask {
            task_id: "t".into(),
        },
        "stale_generation",
    );
    reject_unchanged(
        &mut s,
        Actor::Agent {
            agent_id: "foreign".into(),
        },
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "forbidden",
    );
}
#[test]
fn question_explicitly_linked_to_pr_keeps_identity_and_answer() {
    let mut s = fixture();
    exec(
        &mut s,
        agent(),
        Command::AskQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Choose".into(),
            blocking: true,
            options: vec![QuestionOption {
                id: "ok".into(),
                label: "Okay".into(),
                description: None,
                inputs: vec![],
            }], card: crate::domain::CardSpec::derived(crate::domain::CardKind::Provide, "Answer the question"),
        },
    )
    .unwrap();
    register(&mut s);
    exec(
        &mut s,
        agent(),
        Command::CreateDocument {
            task_id: "t".into(),
            document_id: "doc".into(),
            title: "Design".into(),
            revision_id: "v1".into(),
            content_digest: "a".repeat(64),
            content_reference: "files/design".into(),
            role: "result".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        agent(),
        Command::LinkInteractionSubject {
            interaction_id: "q".into(),
            subject: InteractionSubject::PullRequest { pr_id: "pr".into() },
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        agent(),
        Command::LinkInteractionSubject {
            interaction_id: "q".into(),
            subject: InteractionSubject::DocumentReview {
                document_id: "doc".into(),
                revision_id: "v1".into(),
            },
        },
        "subject_already_linked",
    );
    exec(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            option_id: "ok".into(),
            inputs: BTreeMap::new(),
        },
    )
    .unwrap();
    let p = s.projection();
    assert_eq!(p.inbox.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(), ["interaction:q"],
        "a question about a PR is a card of its own, never a group under the PR");
    assert!(matches!(
        s.interactions[&"q".into()].payload,
        InteractionPayload::Question {
            answer: Some(_),
            ..
        }
    ));
}

#[test]
fn projection_wire_roundtrip_and_optional_fixture_export() {
    let mut s = fixture();
    exec(
        &mut s,
        orchestrator(),
        Command::PublishInformation {
            attachments: vec![AttachmentRef {
                id: "fixture-file".into(),
                name: "Report.pdf".into(),
                content_digest: "c".repeat(64),
                content_reference: "files/fixture-file".into(),
                bytes: 321,
            }],
            interaction_id: "info".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Information with Unicode: revisión ✓".into(), card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Read the update"),
        },
    )
    .unwrap();
    let information = s.projection();
    exec(
        &mut s,
        agent(),
        Command::AskQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Choose an option".into(),
            blocking: true,
            options: vec![QuestionOption {
                id: "proceed".into(),
                label: "Continuar".into(),
                description: None,
                inputs: vec![InputSpec {
                    id: "explanation".into(),
                    kind: InputKind::Text,
                    required: true,
                }],
            }], card: crate::domain::CardSpec::derived(crate::domain::CardKind::Provide, "Answer the question"),
        },
    )
    .unwrap();
    let question = s.projection();
    exec(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            option_id: "proceed".into(),
            inputs: BTreeMap::from([("explanation".into(), InputValue::Text("Reviewed".into()))]),
        },
    )
    .unwrap();
    present(&mut s, "d", text());
    exec(
        &mut s,
        user(),
        Command::AcceptDelivery {
            presentation_id: "d".into(),
            targets: text(),
        },
    )
    .unwrap();
    let completed = s.projection();
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    exec(
        &mut s,
        agent(),
        Command::AskQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "PR question".into(),
            blocking: true,
            options: vec![QuestionOption {
                id: "proceed".into(),
                label: "Proceed".into(),
                description: Some("Merge once the checks pass".into()),
                inputs: vec![],
            }], card: crate::domain::CardSpec::derived(crate::domain::CardKind::Provide, "Answer the question"),
        },
    )
    .unwrap();
    exec(
        &mut s,
        agent(),
        Command::LinkInteractionSubject {
            interaction_id: "q".into(),
            subject: InteractionSubject::PullRequest { pr_id: "pr".into() },
        },
    )
    .unwrap();
    let pr_question = s.projection();
    let fixtures = BTreeMap::from([
        ("information", information),
        ("question", question),
        ("completed", completed),
        ("pr_question", pr_question),
    ]);
    for projection in fixtures.values() {
        let bytes = serde_json::to_vec(projection).unwrap();
        let decoded: DomainProjection = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(&decoded, projection);
    }
    if let Ok(path) = std::env::var("ALONDRA_DOMAIN_FIXTURE_OUTPUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&fixtures).unwrap()).unwrap();
    }
}

#[test]
fn recipient_scope_and_generation_preserve_logical_coordination() {
    let mut s = fixture();
    exec(
        &mut s,
        Actor::System,
        Command::AdvanceOrchestratorGeneration { generation: 1 },
    )
    .unwrap();
    exec(
        &mut s,
        agent(),
        Command::AskQuestion {
            attachments: vec![],
            interaction_id: "to-orch".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::Orchestrator,
            body: "Need coordination".into(),
            blocking: true,
            options: vec![QuestionOption {
                id: "ok".into(),
                label: "Proceed".into(),
                description: None,
                inputs: vec![],
            }], card: crate::domain::CardSpec::derived(crate::domain::CardKind::Provide, "Answer the question"),
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "to-orch".into(),
            option_id: "ok".into(),
            inputs: BTreeMap::new(),
        },
        "forbidden",
    );
    exec(
        &mut s,
        Actor::System,
        Command::AdvanceOrchestratorGeneration { generation: 2 },
    )
    .unwrap();
    exec(
        &mut s,
        Actor::Orchestrator { generation: 2 },
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "to-orch".into(),
            option_id: "ok".into(),
            inputs: BTreeMap::new(),
        },
    )
    .unwrap();
    assert_eq!(
        s.interactions[&"to-orch".into()].scope,
        Scope::Task {
            task_id: "t".into()
        }
    );
    exec(
        &mut s,
        Actor::Orchestrator { generation: 2 },
        Command::AskQuestion {
            attachments: vec![],
            interaction_id: "coord".into(),
            scope: Scope::Orchestrator {
                request_id: Some("r".into()),
            },
            recipient: Recipient::User,
            body: "Coordinate work".into(),
            blocking: true,
            options: vec![QuestionOption {
                id: "ok".into(),
                label: "Okay".into(),
                description: None,
                inputs: vec![],
            }], card: crate::domain::CardSpec::derived(crate::domain::CardKind::Provide, "Answer the question"),
        },
    )
    .unwrap();
    assert_eq!(s.tasks.len(), 1);
    exec(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "coord".into(),
            option_id: "ok".into(),
            inputs: BTreeMap::new(),
        },
    )
    .unwrap();
}

#[test]
fn reassign_fences_old_agent_and_preserves_historical_author() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    authorize(&mut s);
    exec(
        &mut s,
        Actor::System,
        Command::RegisterAgent {
            agent_id: "b".into(),
            name: "Instance B".into(), at: 0
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::AssignTask {
            task_id: "t".into(),
            agent_id: "b".into(), model: None
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        agent(),
        Command::RegisterPr {
            task_id: "t".into(),
            pr_id: "pr".into(),
            host: "github.com".into(),
            repository: "owner/repo".into(),
            number: 1,
            head: "head1".into(),
            base: "main".into(),
            allowed_methods: vec![MergeMethod::Squash],
        },
        "not_assigned",
    );
    assert_eq!(
        s.interactions[&"d".into()].author,
        Author::Agent {
            agent_id: "a".into()
        }
    );
    reject_unchanged(
        &mut s,
        Actor::System,
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "acceptance_missing",
    );
}

#[test]
fn review_off_records_no_pr_decision_and_an_external_merge_completes() {
    let mut s = fixture();
    s.tasks.get_mut(&"t".into()).unwrap().review_prs = false;
    register(&mut s);
    present(&mut s, "d", pr_targets());
    assert!(s.pull_requests[&"pr".into()].review.is_none());
    exec(
        &mut s,
        Actor::System,
        Command::ObservePrState {
            pr_id: "pr".into(),
            head: "head1".into(),
            base: "main".into(),
            state: PrState::Merged,
            merged_head: Some("head1".into()),
        },
    )
    .unwrap();
    assert!(s.pull_requests[&"pr".into()].review.is_none());
    end_node(&mut s, "t");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
}

#[test]
fn partial_delivery_and_unread_information_do_not_replace_final_or_block() {
    let mut s = fixture();
    present(&mut s, "d", text());
    exec(
        &mut s,
        agent(),
        Command::PresentResult {
            attachments: vec![],
            task_id: "t".into(),
            interaction_id: "partial".into(),
            body: "Supplementary partial".into(),
            targets: text(),
            is_final: false, nothing_to_review: false, card: crate::domain::CardSpec::delivery("Review the result", &[], true),
        },
    )
    .unwrap();
    exec(
        &mut s,
        orchestrator(),
        Command::PublishInformation {
            attachments: vec![],
            interaction_id: "info".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Unread news".into(), card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Read the update"),
        },
    )
    .unwrap();
    assert_eq!(s.tasks[&"t".into()].final_presentation, Some("d".into()));
    exec(
        &mut s,
        user(),
        Command::AcceptDelivery {
            presentation_id: "d".into(),
            targets: text(),
        },
    )
    .unwrap();
    end_node(&mut s, "t");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
}

#[test]
fn free_text_response_with_or_without_options_and_attachment_bounds() {
    let mut s = fixture();
    let attachment = AttachmentRef {
        id: "file-1".into(),
        name: "Review.pdf".into(),
        content_digest: "a".repeat(64),
        content_reference: "files/file-1".into(),
        bytes: 42,
    };
    exec(
        &mut s,
        agent(),
        Command::AskQuestion {
            attachments: vec![attachment.clone()],
            interaction_id: "q".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Review attachment".into(),
            blocking: true,
            options: vec![], card: crate::domain::CardSpec::derived(crate::domain::CardKind::Provide, "Answer the question"),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![attachment.clone()],
            interaction_id: "q".into(),
            option_id: String::new(),
            inputs: BTreeMap::from([("text".into(), InputValue::Text("My free response".into()))]),
        },
    )
    .unwrap();
    assert_eq!(
        s.interactions[&"q".into()].attachments,
        vec![attachment.clone()]
    );
    let before = s.clone();
    let mut too_big = attachment;
    too_big.bytes = 64 * 1024 * 1024 + 1;
    assert_eq!(
        exec(
            &mut s,
            orchestrator(),
            Command::PublishInformation {
                attachments: vec![too_big],
                interaction_id: "bad".into(),
                scope: Scope::Task {
                    task_id: "t".into()
                },
                recipient: Recipient::User,
                body: String::new(), card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Read the update"),
            }
        )
        .unwrap_err()
        .code,
        "payload_too_large"
    );
    assert_eq!(s, before);
    exec(
        &mut s,
        agent(),
        Command::AskQuestion {
            attachments: vec![],
            interaction_id: "options".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Pick or explain".into(),
            blocking: false,
            options: vec![QuestionOption {
                id: "yes".into(),
                label: "Yes".into(),
                description: None,
                inputs: vec![],
            }], card: crate::domain::CardSpec::derived(crate::domain::CardKind::Provide, "Answer the question"),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "options".into(),
            option_id: String::new(),
            inputs: BTreeMap::from([(
                "text".into(),
                InputValue::Text("Something different".into()),
            )]),
        },
    )
    .unwrap();
}

#[test]
fn stack_readiness_is_explicit_and_merge_requires_upstream_merge_and_retarget() {
    let mut s = fixture();
    register(&mut s);
    register_b(&mut s);
    present(&mut s, "d", pr_targets());
    let stack = StackDependency {
        upstream_task: "t".into(),
        presentation_id: "d".into(),
        pr_id: "pr".into(),
        head: "head1".into(),
    };
    exec(
        &mut s,
        orchestrator(),
        Command::CreateTask {
            task_id: "down".into(),
            request_id: "r".into(),
            title: "Downstream".into(),
            agent_id: Some("b".into()),
            review_prs: true,
            dependencies: BTreeSet::new(),
            stack: Some(stack),
            launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")),
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        agent_b(),
        Command::StartTask {
            task_id: "down".into(),
        },
        "stack_not_ready",
    );
    exec(
        &mut s,
        agent(),
        Command::PublishStackReadiness {
            task_id: "t".into(),
            presentation_id: "d".into(),
            pr_id: "pr".into(),
            head: "head1".into(),
            ready: true,
        },
    )
    .unwrap();
    exec(
        &mut s,
        agent_b(),
        Command::StartTask {
            task_id: "down".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        agent_b(),
        Command::RegisterPr {
            task_id: "down".into(),
            pr_id: "pr-down".into(),
            host: "github.com".into(),
            repository: "owner/repo".into(),
            number: 2,
            head: "head-down".into(),
            base: "upstream-branch".into(),
            allowed_methods: vec![MergeMethod::Squash],
        },
    )
    .unwrap();
    exec(
        &mut s,
        agent_b(),
        Command::PresentResult {
            attachments: vec![],
            task_id: "down".into(),
            interaction_id: "d-down".into(),
            body: "Downstream ready".into(),
            targets: vec![DeliveryTarget::PullRequest {
                pr_id: "pr-down".into(),
                head: "head-down".into(),
            }],
            is_final: true, nothing_to_review: false, card: crate::domain::CardSpec::delivery("Review the result", &[], true),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::AuthorizeMerge {
            authorization_id: "auth-down".into(),
            presentation_id: "d-down".into(),
            pr_id: "pr-down".into(),
            expected_head: "head-down".into(),
            method: MergeMethod::Squash,
        },
    )
    .unwrap();
    let claim = Command::ClaimMergeAuthorization {
        authorization_id: "auth-down".into(),
        remote_head: "head-down".into(),
        remote_base: "upstream-branch".into(),
        base_verified: true,
    };
    reject_unchanged(&mut s, Actor::System, claim.clone(), "stack_not_ready");
    authorize(&mut s);
    exec(
        &mut s,
        Actor::System,
        Command::ObservePrState {
            pr_id: "pr".into(),
            head: "head1".into(),
            base: "main".into(),
            state: PrState::Merged,
            merged_head: Some("head1".into()),
        },
    )
    .unwrap();
    reject_unchanged(&mut s, Actor::System, claim, "stack_not_ready");
    exec(
        &mut s,
        Actor::System,
        Command::ObservePrState {
            pr_id: "pr-down".into(),
            head: "head-down".into(),
            base: "main".into(),
            state: PrState::Open,
            merged_head: None,
        },
    )
    .unwrap();
    assert!(
        s.merge_authorizations[&"auth-down".into()].valid,
        "retarget without content change preserves acceptance"
    );
    exec(
        &mut s,
        Actor::System,
        Command::ClaimMergeAuthorization {
            authorization_id: "auth-down".into(),
            remote_head: "head-down".into(),
            remote_base: "main".into(),
            base_verified: true,
        },
    )
    .unwrap();
    end_node(&mut s, "t");
    assert_eq!(
        s.tasks[&"t".into()].state,
        TaskState::Done,
        "the observed merge of its authorized delivery completes upstream"
    );
}

#[test]
fn cancellation_during_merge_preserves_observation_without_completion() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    authorize(&mut s);
    exec(
        &mut s,
        Actor::System,
        Command::ClaimMergeAuthorization {
            authorization_id: "auth".into(),
            remote_head: "head1".into(),
            remote_base: "main".into(),
            base_verified: true,
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::CancelTask {
            task_id: "t".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        Actor::System,
        Command::ObservePrState {
            pr_id: "pr".into(),
            head: "head1".into(),
            base: "main".into(),
            state: PrState::Merged,
            merged_head: Some("head1".into()),
        },
    )
    .unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Cancelled);
    assert_eq!(
        s.merge_authorizations[&"auth".into()].attempt,
        MergeAttemptState::Confirmed
    );
    assert!(!s.merge_authorizations[&"auth".into()].valid);
    reject_unchanged(
        &mut s,
        Actor::System,
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "task_not_active",
    );
}

#[test]
fn protocol_rejects_client_actor_and_unsafe_revisions_without_mutation() {
    let request = serde_json::json!({"protocol_version":2,"data_epoch":"epoch","command_id":"c","expected_revision":0,"actor":{"kind":"system"},"command":{"kind":"mark_read","interaction_id":"i"}});
    assert!(serde_json::from_value::<CommandEnvelope>(request).is_err());
    let mut s = fixture();
    let before = s.clone();
    let envelope = CommandEnvelope {
        protocol_version: 2,
        data_epoch: s.data_epoch.clone(),
        command_id: "stale".into(),
        expected_revision: s.revision - 1,
        command: Command::StartTask {
            task_id: "t".into(),
        },
    };
    assert_eq!(
        s.execute(&agent(), &envelope).unwrap_err().code,
        "stale_revision"
    );
    assert_eq!(s, before);
    s.revision = MAX_REVISION;
    let before = s.clone();
    let envelope = CommandEnvelope {
        protocol_version: 2,
        data_epoch: s.data_epoch.clone(),
        command_id: "overflow".into(),
        expected_revision: MAX_REVISION,
        command: Command::StartTask {
            task_id: "t".into(),
        },
    };
    assert_eq!(
        s.execute(&agent(), &envelope).unwrap_err().code,
        "invalid_input"
    );
    assert_eq!(s, before);
}

#[test]
fn information_read_is_shared_across_authorized_devices() {
    let mut s = fixture();
    exec(
        &mut s,
        orchestrator(),
        Command::PublishInformation {
            attachments: vec![],
            interaction_id: "i".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Read once".into(), card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Read the update"),
        },
    )
    .unwrap();
    exec(
        &mut s,
        Actor::User {
            user_id: "device-1".into(),
        },
        Command::MarkRead {
            interaction_id: "i".into(),
        },
    )
    .unwrap();
    let revision = s.revision;
    let out = exec(
        &mut s,
        Actor::User {
            user_id: "device-2".into(),
        },
        Command::MarkRead {
            interaction_id: "i".into(),
        },
    )
    .unwrap();
    assert!(!out.changed);
    assert_eq!(s.revision, revision);
    let item = &s.projection().inbox[0];
    assert!(!item.pending);
    assert!(!item.available_actions[0].enabled);
}

#[test]
fn mixed_package_needs_merge_authorization_and_document_decision() {
    let mut s = fixture();
    register(&mut s);
    exec(
        &mut s,
        agent(),
        Command::CreateDocument {
            task_id: "t".into(),
            document_id: "doc".into(),
            title: "Design".into(),
            revision_id: "v1".into(),
            content_digest: "a".repeat(64),
            content_reference: "files/design".into(),
            role: "result".into(),
        },
    )
    .unwrap();
    let targets = vec![
        DeliveryTarget::Text {
            text: "Summary".into(),
        },
        DeliveryTarget::DocumentRevision {
            document_id: "doc".into(),
            revision_id: "v1".into(),
        },
        DeliveryTarget::PullRequest {
            pr_id: "pr".into(),
            head: "head1".into(),
        },
    ];
    present(&mut s, "d", targets.clone());
    let accepted = |s: &DomainState| {
        matches!(&s.interactions[&"d".into()].payload,InteractionPayload::Delivery{acceptance:Some(a),..} if a.valid)
    };
    let decide = |s: &mut DomainState, accepted: bool| {
        exec(
            s,
            user(),
            Command::DecideDocumentReview {
                task_id: "t".into(),
                document_id: "doc".into(),
                revision_id: "v1".into(),
                accepted,
            },
        )
        .unwrap()
    };
    let complete = |s: &mut DomainState| {
        exec(
            s,
            Actor::System,
            Command::CompleteTask {
                task_id: "t".into(),
                presentation_id: "d".into(),
            },
        )
    };
    // The merge authorization covers the PR and the text, never the document.
    authorize(&mut s);
    assert!(!accepted(&s));
    assert_eq!(s.tasks[&"t".into()].state, TaskState::ForYourReview);
    assert!(s.document_reviews[0].decisions.is_empty());
    assert_eq!(complete(&mut s).unwrap_err().code, "acceptance_missing");
    // A rejection invalidates the authorization; a later approval alone is not enough.
    decide(&mut s, false);
    assert!(!s.merge_authorizations[&"auth".into()].valid);
    decide(&mut s, true);
    assert!(!accepted(&s));
    assert_eq!(s.document_reviews[0].decisions.len(), 2);
    assert_eq!(
        s.document_reviews[0].decisions[1].presentation_id,
        Some("d".into())
    );
    exec(
        &mut s,
        user(),
        Command::AuthorizeMerge {
            authorization_id: "auth2".into(),
            presentation_id: "d".into(),
            pr_id: "pr".into(),
            expected_head: "head1".into(),
            method: MergeMethod::Squash,
        },
    )
    .unwrap();
    assert!(accepted(&s), "PR authorized and document approved: package accepted");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::WaitingDependency);
    assert_eq!(complete(&mut s).unwrap_err().code, "merge_not_confirmed");
    // Feedback dates the document approval; re-authorizing the PR is not enough.
    exec(
        &mut s,
        user(),
        Command::SubmitFeedback {
            attachments: vec![],
            presentation_id: "d".into(),
            targets: targets.clone(),
            body: "Tighten the summary".into(),
            instruction: Instruction::Changes,
        },
    )
    .unwrap();
    assert!(!accepted(&s));
    exec(
        &mut s,
        user(),
        Command::AuthorizeMerge {
            authorization_id: "auth3".into(),
            presentation_id: "d".into(),
            pr_id: "pr".into(),
            expected_head: "head1".into(),
            method: MergeMethod::Squash,
        },
    )
    .unwrap();
    assert!(!accepted(&s));
    decide(&mut s, true);
    assert!(accepted(&s));
    exec(
        &mut s,
        Actor::System,
        Command::ObservePrState {
            pr_id: "pr".into(),
            head: "head1".into(),
            base: "main".into(),
            state: PrState::Merged,
            merged_head: Some("head1".into()),
        },
    )
    .unwrap();
    end_node(&mut s, "t");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
    assert_eq!(complete(&mut s).unwrap_err().code, "task_not_active");
}
#[test]
fn authorization_covers_pr_and_text_but_not_a_document_only_package() {
    let mut s = fixture();
    register(&mut s);
    present(
        &mut s,
        "d",
        vec![
            DeliveryTarget::Text {
                text: "Summary".into(),
            },
            DeliveryTarget::PullRequest {
                pr_id: "pr".into(),
                head: "head1".into(),
            },
        ],
    );
    authorize(&mut s);
    assert!(
        matches!(&s.interactions[&"d".into()].payload,InteractionPayload::Delivery{acceptance:Some(a),..} if a.valid)
    );
    let mut s = fixture();
    exec(
        &mut s,
        agent(),
        Command::CreateDocument {
            task_id: "t".into(),
            document_id: "doc".into(),
            title: "Design".into(),
            revision_id: "v1".into(),
            content_digest: "a".repeat(64),
            content_reference: "files/design".into(),
            role: "result".into(),
        },
    )
    .unwrap();
    present(
        &mut s,
        "d",
        vec![
            DeliveryTarget::Text {
                text: "Summary".into(),
            },
            DeliveryTarget::DocumentRevision {
                document_id: "doc".into(),
                revision_id: "v1".into(),
            },
        ],
    );
    exec(
        &mut s,
        user(),
        Command::DecideDocumentReview {
            task_id: "t".into(),
            document_id: "doc".into(),
            revision_id: "v1".into(),
            accepted: true,
        },
    )
    .unwrap();
    assert!(
        matches!(&s.interactions[&"d".into()].payload,InteractionPayload::Delivery{acceptance:None,..}),
        "a document approval does not accept the text piece"
    );
}
#[test]
fn a_registered_pr_is_presented_while_open_and_an_accepted_task_settles_once_it_is_merged_or_closed() {
    let mut open = fixture();
    register(&mut open);
    let before = open.clone();
    let error = exec(&mut open, agent(), Command::PresentResult {
        attachments: vec![], task_id: "t".into(), interaction_id: "d".into(), body: "Result ready".into(),
        targets: text(), is_final: true, nothing_to_review: false, card: crate::domain::CardSpec::delivery("Review the result", &[], true),
    }).unwrap_err();
    assert_eq!((error.code.as_str(), error.subject.as_deref()), ("pr_missing", Some("targets")));
    assert!(error.message.contains("PR #1 is open") && error.message.contains("head1"), "{}", error.message);
    assert_eq!(open, before, "a refused delivery changes nothing");

    for end in [PrState::Merged, PrState::Closed] {
        let mut s = fixture();
        register(&mut s);
        exec(&mut s, Actor::System, Command::ObservePrState { pr_id: "pr".into(), head: "head1".into(), base: "main".into(),
            state: end, merged_head: (end == PrState::Merged).then(|| "head1".into()) }).unwrap();
        present(&mut s, "d", text());
        exec(&mut s, user(), Command::AcceptDelivery { presentation_id: "d".into(), targets: text() }).unwrap();
        assert!(s.tasks[&"t".into()].wrap_up.asked_at.is_some(), "{end:?}: the node is asked to clean up");
        exec(&mut s, Actor::System, complete()).unwrap();
        end_node(&mut s, "t");
        assert_eq!(s.tasks[&"t".into()].state, TaskState::Done, "{end:?}");
    }
}
#[test]
fn answer_reaches_the_current_assignee_after_reassignment() {
    let mut s = fixture();
    exec(
        &mut s,
        agent(),
        Command::AskQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            scope: Scope::Task { task_id: "t".into() },
            recipient: Recipient::User,
            body: "Which colour?".into(),
            blocking: true,
            options: vec![], card: crate::domain::CardSpec::derived(crate::domain::CardKind::Provide, "Answer the question"),
        },
    )
    .unwrap();
    exec(
        &mut s,
        Actor::System,
        Command::RegisterAgent {
            agent_id: "b".into(),
            name: "Instance B".into(), at: 0
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::AssignTask {
            task_id: "t".into(),
            agent_id: "b".into(), model: None
        },
    )
    .unwrap();
    let outcome = exec(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            option_id: String::new(),
            inputs: BTreeMap::from([("text".into(), InputValue::Text("Blue".into()))]),
        },
    )
    .unwrap();
    assert_eq!(
        outcome.effects,
        vec![DomainEffect::NotifyAgent {
            agent_id: "b".into(),
            task_id: Some("t".into()),
            interaction_id: Some("q".into()),
            message: "Question answered".into(),
        }]
    );
    assert_eq!(
        s.interactions[&"q".into()].author,
        Author::Agent { agent_id: "a".into() }
    );
}

#[test]
fn unread_count_ignores_archived_tasks_like_the_inbox_does() {
    let mut s = fixture();
    exec(
        &mut s,
        orchestrator(),
        Command::PublishInformation {
            attachments: vec![],
            interaction_id: "info".into(),
            scope: Scope::Task { task_id: "t".into() },
            recipient: Recipient::User,
            body: "Progress".into(), card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Read the update"),
        },
    )
    .unwrap();
    present(&mut s, "d", text());
    exec(&mut s, user(), Command::AcceptDelivery { presentation_id: "d".into(), targets: text() }).unwrap();
    assert_eq!(s.projection().counts.unread_information, 1);
    end_node(&mut s, "t");
    exec(&mut s, user(), Command::ArchiveTask { task_id: "t".into(), archived: true }).unwrap();
    let projection = s.projection();
    assert_eq!(projection.counts.unread_information, 0);
    assert!(projection.inbox.iter().all(|item| item.id != "info"));
    exec(&mut s, user(), Command::ArchiveTask { task_id: "t".into(), archived: false }).unwrap();
    assert_eq!(s.projection().counts.unread_information, 1);
}

#[test]
fn revision_checks_are_per_subject_and_invalidation_dates_decisions() {
    let mut s = fixture();
    let run = |s: &mut DomainState, actor: Actor, id: &str, expected: Revision, command: Command| {
        s.execute(
            &actor,
            &CommandEnvelope {
                protocol_version: 2,
                data_epoch: s.data_epoch.clone(),
                command_id: id.into(),
                expected_revision: expected,
                command,
            },
        )
    };
    let info = |id: &str| Command::PublishInformation {
        attachments: vec![],
        interaction_id: id.into(),
        scope: Scope::Task { task_id: "t".into() },
        recipient: Recipient::User,
        body: "Progress".into(), card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Read the update"),
    };
    let answer = |text: &str| Command::AnswerQuestion {
        attachments: vec![],
        interaction_id: "q".into(),
        option_id: String::new(),
        inputs: BTreeMap::from([("text".into(), InputValue::Text(text.into()))]),
    };
    exec(
        &mut s,
        agent(),
        Command::AskQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            scope: Scope::Task { task_id: "t".into() },
            recipient: Recipient::User,
            body: "Which colour?".into(),
            blocking: false,
            options: vec![], card: crate::domain::CardSpec::derived(crate::domain::CardKind::Provide, "Answer the question"),
        },
    )
    .unwrap();
    let seen = s.revision;
    exec(&mut s, orchestrator(), info("i1")).unwrap();
    assert!(s.revision > seen);
    // A snapshot older than unrelated information still decides this question once.
    run(&mut s, user(), "a1", seen, answer("Blue")).unwrap();
    let lost = run(&mut s, user(), "a2", seen, answer("Red")).unwrap_err();
    assert_eq!(lost.code, "stale_revision");
    assert_eq!(lost.current_revision, Some(s.revision));
    let future = s.revision + 1;
    assert_eq!(
        run(&mut s, orchestrator(), "future", future, info("i2")).unwrap_err().code,
        "stale_revision"
    );
    // Information neither blocks nor dates a Task decision.
    let seen = s.revision;
    exec(&mut s, orchestrator(), info("i3")).unwrap();
    run(&mut s, agent(), "start", seen, Command::StartTask { task_id: "t".into() }).unwrap();
    // Invalidating a delivery dates every decision taken on it.
    register(&mut s);
    present(&mut s, "d", pr_targets());
    authorize(&mut s);
    let seen = s.revision;
    exec(
        &mut s,
        Actor::System,
        Command::ObservePrState {
            pr_id: "pr".into(),
            head: "head2".into(),
            base: "main".into(),
            state: PrState::Open,
            merged_head: None,
        },
    )
    .unwrap();
    assert_eq!(
        run(&mut s, user(), "late-accept", seen, Command::AcceptDelivery {
            presentation_id: "d".into(),
            targets: pr_targets(),
        })
        .unwrap_err()
        .code,
        "stale_revision"
    );
    // Two managers deciding the same Task from one snapshot: one winner.
    for id in ["b", "c"] {
        exec(&mut s, Actor::System, Command::RegisterAgent { agent_id: id.into(), name: id.into(), at: 0 }).unwrap();
    }
    let seen = s.revision;
    run(&mut s, user(), "assign-b", seen, Command::AssignTask { task_id: "t".into(), agent_id: "b".into(), model: None }).unwrap();
    assert_eq!(
        run(&mut s, user(), "assign-c", seen, Command::AssignTask { task_id: "t".into(), agent_id: "c".into(), model: None })
            .unwrap_err()
            .code,
        "stale_revision"
    );
    assert_eq!(s.tasks[&"t".into()].agent_id, Some("b".into()));
    // Commands without a narrower subject still need the current revision.
    assert_eq!(
        run(&mut s, user(), "r2", seen, Command::CreateRequest { request_id: "r2".into(), body: "More".into(), auto_merge: false, maintenance: false, message: false })
            .unwrap_err()
            .code,
        "stale_revision"
    );
}

struct Ids {
    task: &'static str,
    agent: &'static str,
    interaction: &'static str,
    document: &'static str,
    revision: &'static str,
    pr: &'static str,
    request: &'static str,
    authorization: &'static str,
    schedule: &'static str,
    run: &'static str,
}
fn variant_index(c: &Command) -> usize {
    match c {
        Command::CreateSchedule { .. } => 0,
        Command::UpdateSchedule { .. } => 1,
        Command::DeleteSchedule { .. } => 2,
        Command::ClaimScheduleRun { .. } => 3,
        Command::RecordScheduleRunSent { .. } => 4,
        Command::ClaimScheduleLaunch { .. } => 5,
        Command::RecordScheduleLaunchHanded { .. } => 6,
        Command::RecordScheduleRunFailed { .. } => 7,
        Command::InterruptScheduleRunsOnRestart => 8,
        Command::CreateRequest { .. } => 9,
        Command::CloseRequest { .. } => 10,
        Command::RegisterAgent { .. } => 11,
        Command::MoveNode { .. } => 12,
        Command::ReconcileNodes { .. } => 12,
        Command::AdvanceOrchestratorGeneration { .. } => 13,
        Command::CreateTask { .. } => 14,
        Command::AssignTask { .. } => 15,
        Command::SetDependencies { .. } => 16,
        Command::StartTask { .. } => 17,
        Command::PublishInformation { .. } => 18,
        Command::AskQuestion { .. } => 19,
        Command::RequestAction { .. } => 20,
        Command::MarkRead { .. } => 21,
        Command::AnswerQuestion { .. } => 22,
        Command::CreateDocument { .. } => 23,
        Command::ReviseDocument { .. } => 24,
        Command::LinkTaskDocument { .. } => 25,
        Command::DecideDocumentReview { .. } => 26,
        Command::RegisterPr { .. } => 27,
        Command::LinkInteractionSubject { .. } => 28,
        Command::PresentResult { .. } => 29,
        Command::SubmitFeedback { .. } => 30,
        Command::AcceptDelivery { .. } => 31,
        Command::AuthorizeMerge { .. } => 32,
        Command::ClaimMergeAuthorization { .. } => 33,
        Command::ObserveMergeAttempt { .. } => 34,
        Command::ObservePrState { .. } => 35,
        Command::PublishStackReadiness { .. } => 36,
        Command::CompleteTask { .. } => 37,
        Command::CancelTask { .. } => 38,
        Command::ReopenTask { .. } => 39,
        Command::ArchiveTask { .. } => 40,
        Command::RetitleTask { .. } => 41,
        Command::Withdraw { .. } => 42,
        Command::HoldHook { .. } => 43,
        Command::RequestClarification { .. } => 44,
        Command::RaiseProblem { .. } => 45,
        Command::TitleRequest { .. } => 46,
        Command::AskQuestions { .. } => 47,
        Command::HoldQuestions { .. } => 48,
        Command::ReopenAnswer { .. } => 49,
        Command::ReleaseTask { .. } => 50,
        Command::ConfirmCleanup { .. } => 51,
        Command::RecordNodeEnd { .. } => 52,
        Command::SetRelations { .. } => 53,
        Command::RecordVerdict { .. } => 54,
        Command::ObservePrMergeMethods { .. } => 55,
        Command::SetPriority { .. } => 56,
        Command::DeclarePostMergeWork { .. } => 57,
        Command::BindOrchestratorSession { .. } => 58,
        Command::RaiseStartProblem { .. } => 59,
    }
}
const COMMAND_KINDS: usize = 60;
/// Every command kind naming the given ids. The flag marks commands that only
/// create a fresh entity and therefore may succeed with an unknown id.
fn commands_naming(ids: &Ids) -> Vec<(Command, bool)> {
    let scope = Scope::Task {
        task_id: ids.task.into(),
    };
    let (request, task) = scheduled_request_task(ids.request, ids.task);
    let mut schedule = schedule();
    schedule.id = ids.schedule.into();
    let mut updated = schedule.clone();
    updated.revision = 2;
    let option = QuestionOption {
        id: "yes".into(),
        label: "Yes".into(),
        description: None,
        inputs: vec![],
    };
    let list = vec![
        (Command::CreateSchedule { schedule }, true),
        (Command::UpdateSchedule { schedule: updated }, false),
        (Command::DeleteSchedule { schedule_id: ids.schedule.into(), deleted_at: 1,
            expected_schedule_revision: None }, false),
        (Command::ClaimScheduleRun { schedule_id: ids.schedule.into(), run_id: ids.run.into(),
            origin: ScheduleRunOrigin::Manual, now: 1, next_after_now: None,
            expected_schedule_revision: None }, false),
        (Command::RecordScheduleRunSent { run_id: ids.run.into(), request, task,
            launch: scheduled_launch(), finished_at: 1 }, false),
        (Command::ClaimScheduleLaunch { run_id: ids.run.into(), now: 1 }, false),
        (Command::RecordScheduleLaunchHanded { run_id: ids.run.into(), session_id: None, agent_id: None, finished_at: 1 }, false),
        (Command::RecordScheduleRunFailed { run_id: ids.run.into(), reason: "boom".into(), finished_at: 1 }, false),
        (Command::RaiseStartProblem { task_id: ids.task.into(), problem: StartProblem::NodeClosed }, false),
        (Command::InterruptScheduleRunsOnRestart, true),
        (Command::CreateRequest { request_id: ids.request.into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false }, true),
        (Command::CloseRequest { request_id: ids.request.into(), answer: None, withdrawn: false, title: None }, false),
        (Command::RegisterAgent { agent_id: ids.agent.into(), name: "Ghost".into(), at: 0 }, true),
        (Command::MoveNode { agent_id: ids.agent.into(), event: NodeEvent::TurnStarted, at: 0 }, false),
        (Command::AdvanceOrchestratorGeneration { generation: 1 }, true),
        (Command::BindOrchestratorSession { session_id: ids.agent.into() }, true),
        (Command::CreateTask { task_id: "fresh-task".into(), request_id: ids.request.into(),
            title: "Fresh".into(), agent_id: Some(ids.agent.into()), review_prs: false,
            dependencies: BTreeSet::new(), stack: None,
            launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")), }, false),
        (Command::TitleRequest { request_id: ids.request.into(), title: "Named".into() }, false),
        (Command::AssignTask { task_id: ids.task.into(), agent_id: ids.agent.into(), model: None }, false),
        (Command::RetitleTask { task_id: ids.task.into(), title: "Renamed".into() }, false),
        (Command::SetDependencies { task_id: ids.task.into(), dependencies: BTreeSet::new(), stack: None }, false),
        (Command::StartTask { task_id: ids.task.into() }, false),
        (Command::PublishInformation { attachments: vec![], interaction_id: "fresh-info".into(),
            scope: scope.clone(), recipient: Recipient::User, body: "Info".into(), card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Read the update") }, false),
        (Command::AskQuestion { attachments: vec![], interaction_id: "fresh-question".into(),
            scope: scope.clone(), recipient: Recipient::User, body: "Q?".into(), blocking: true,
            options: vec![option.clone()], card: crate::domain::CardSpec::derived(crate::domain::CardKind::Provide, "Answer the question") }, false),
        (Command::RequestAction { attachments: vec![], interaction_id: "fresh-action".into(),
            scope, recipient: Recipient::User, body: "Do".into(), blocking: true,
            options: vec![option], card: crate::domain::CardSpec::derived(crate::domain::CardKind::Do, "Do the action") }, false),
        (Command::MarkRead { interaction_id: ids.interaction.into() }, false),
        (Command::Withdraw { interaction_id: ids.interaction.into() }, false),
        (Command::AnswerQuestion { attachments: vec![], interaction_id: ids.interaction.into(),
            option_id: "yes".into(), inputs: BTreeMap::new() }, false),
        (Command::CreateDocument { task_id: ids.task.into(), document_id: "fresh-doc".into(),
            title: "Doc".into(), revision_id: "fresh-rev".into(), content_digest: "a".repeat(64),
            content_reference: "files/a".into(), role: "spec".into() }, false),
        (Command::ReviseDocument { task_id: ids.task.into(), document_id: ids.document.into(),
            revision_id: "fresh-rev".into(), content_digest: "b".repeat(64),
            content_reference: "files/b".into() }, false),
        (Command::LinkTaskDocument { task_id: ids.task.into(), document_id: ids.document.into(),
            role: "spec".into() }, false),
        (Command::DecideDocumentReview { task_id: ids.task.into(), document_id: ids.document.into(),
            revision_id: ids.revision.into(), accepted: true }, false),
        (Command::RegisterPr { task_id: ids.task.into(), pr_id: "fresh-pr".into(),
            host: "github.com".into(), repository: "owner/repo".into(), number: 9,
            head: "head9".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash] }, false),
        (Command::LinkInteractionSubject { interaction_id: ids.interaction.into(),
            subject: InteractionSubject::PullRequest { pr_id: ids.pr.into() } }, false),
        (Command::PresentResult { attachments: vec![], task_id: ids.task.into(),
            interaction_id: "fresh-delivery".into(), body: "Done".into(), targets: text(),
            is_final: true, nothing_to_review: false, card: crate::domain::CardSpec::delivery("Review the result", &[], true) }, false),
        (Command::SubmitFeedback { attachments: vec![], presentation_id: ids.interaction.into(),
            targets: text(), body: "Change".into(), instruction: Instruction::Changes }, false),
        (Command::AcceptDelivery { presentation_id: ids.interaction.into(), targets: text() }, false),
        (Command::AuthorizeMerge { authorization_id: "fresh-auth".into(),
            presentation_id: ids.interaction.into(), pr_id: ids.pr.into(),
            expected_head: "head1".into(), method: MergeMethod::Squash }, false),
        (Command::ClaimMergeAuthorization { authorization_id: ids.authorization.into(),
            remote_head: "head1".into(), remote_base: "main".into(), base_verified: true }, false),
        (Command::ObserveMergeAttempt { authorization_id: ids.authorization.into(),
            state: MergeAttemptState::Failed }, false),
        (Command::ObservePrState { pr_id: ids.pr.into(), head: "head1".into(), base: "main".into(),
            state: PrState::Closed, merged_head: None }, false),
        (Command::PublishStackReadiness { task_id: ids.task.into(),
            presentation_id: ids.interaction.into(), pr_id: ids.pr.into(), head: "head1".into(),
            ready: true }, false),
        (Command::CompleteTask { task_id: ids.task.into(), presentation_id: ids.interaction.into() }, false),
        (Command::CancelTask { task_id: ids.task.into() }, false),
        (Command::ReopenTask { task_id: ids.task.into() }, false),
        (Command::ArchiveTask { task_id: ids.task.into(), archived: false }, false),
        (Command::HoldHook { interaction_id: "fresh-hold".into(), task_id: ids.task.into(),
            agent_id: ids.agent.into(), hold: Hold::Permission, body: "Bash: ls".into(),
            options: Hold::Permission.options(), card: crate::domain::CardSpec::derived(crate::domain::CardKind::Authorize, "Allow the call") }, false),
        (Command::RequestClarification { interaction_id: ids.interaction.into(),
            reason: ClarificationReason::Unclear, text: None }, false),
        (Command::RaiseProblem { attachments: vec![], interaction_id: "fresh-problem".into(),
            scope: Scope::Task { task_id: ids.task.into() }, body: "Stuck".into(), options: vec![],
            card: CardSpec::derived(CardKind::Problem, "Unblock the node") }, true),
        (Command::AskQuestions { interaction_id: "fresh-group".into(), scope: Scope::Task { task_id: ids.task.into() },
            recipient: Recipient::User, blocking: false, why: "Why".into(), questions: vec![] }, true),
        (Command::HoldQuestions { interaction_id: "fresh-hold-group".into(), task_id: ids.task.into(),
            agent_id: "a".into(), questions: vec![] }, true),
        (Command::ReopenAnswer { interaction_id: ids.interaction.into() }, false),
        (Command::ReleaseTask { task_id: ids.task.into(), reason: "node closed by the user".into() }, false),
        (Command::ConfirmCleanup { task_id: ids.task.into() }, false),
        (Command::RecordNodeEnd { task_id: ids.task.into(), end: NodeEnd::Closed, agent_id: None }, false),
        (Command::SetRelations { task_id: ids.task.into(), delivered_dependencies: BTreeSet::new(), blocks: BTreeSet::new() }, false),
        (Command::RecordVerdict { task_id: ids.task.into(), presentation_id: ids.interaction.into(), verdict: Verdict::Approved { summary: "ok".into(), open_issues: false, serious_findings: 0 } }, false),
        (Command::ObservePrMergeMethods { pr_id: ids.pr.into(), allowed_methods: vec![MergeMethod::Squash] }, false),
        (Command::SetPriority { task_id: ids.task.into(), priority: TaskPriority::High }, false),
        (Command::DeclarePostMergeWork { task_id: ids.task.into(), reason: Some("release.sh".into()) }, false),
    ];
    let mut covered = vec![false; COMMAND_KINDS];
    for (command, _) in &list {
        covered[variant_index(command)] = true;
    }
    assert!(covered.iter().all(|c| *c), "every command kind must be exercised");
    list
}
fn assert_all_rejected(base: &DomainState, actors: &[Actor], ids: &Ids) {
    for (command, creates) in commands_naming(ids) {
        if creates {
            continue;
        }
        for actor in actors {
            let mut s = base.clone();
            let error = exec(&mut s, actor.clone(), command.clone())
                .expect_err(&format!("{actor:?} with {command:?} must be rejected"));
            assert!(!error.code.is_empty(), "rejection needs a typed code");
            assert_eq!(s, *base, "rejected {command:?} changed state");
        }
    }
}
#[test]
fn unknown_and_foreign_ids_are_typed_rejections_never_panics() {
    let actors = [
        user(),
        agent(),
        Actor::Agent {
            agent_id: "ghost".into(),
        },
        Actor::Orchestrator { generation: 0 },
        Actor::System,
    ];
    let unknown = Ids {
        task: "missing-task",
        agent: "missing-agent",
        interaction: "missing-interaction",
        document: "missing-document",
        revision: "missing-revision",
        pr: "missing-pr",
        request: "missing-request",
        authorization: "missing-authorization",
        schedule: "missing-schedule",
        run: "missing-run",
    };
    assert_all_rejected(&DomainState::new("epoch"), &actors, &unknown);
    let mut s = fixture();
    assert_all_rejected(&s, &actors, &unknown);

    exec(&mut s, Actor::System, Command::RegisterAgent { agent_id: "b".into(), name: "Instance B".into(), at: 0 }).unwrap();
    exec(&mut s, orchestrator(), Command::CreateTask { task_id: "t2".into(), request_id: "r".into(),
        title: "Other".into(), agent_id: Some("b".into()), review_prs: true,
        dependencies: BTreeSet::new(), stack: None,
        launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")), }).unwrap();
    let b = Actor::Agent { agent_id: "b".into() };
    exec(&mut s, b.clone(), Command::StartTask { task_id: "t2".into() }).unwrap();
    exec(&mut s, b.clone(), Command::RegisterPr { task_id: "t2".into(), pr_id: "pr2".into(),
        host: "github.com".into(), repository: "owner/repo".into(), number: 2,
        head: "head1".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash] }).unwrap();
    exec(&mut s, b.clone(), Command::CreateDocument { task_id: "t2".into(), document_id: "doc2".into(),
        title: "Doc".into(), revision_id: "rev2".into(), content_digest: "c".repeat(64),
        content_reference: "files/c".into(), role: "spec".into() }).unwrap();
    exec(&mut s, b.clone(), Command::PresentResult { attachments: vec![], task_id: "t2".into(),
        interaction_id: "pres2".into(), body: "Done".into(),
        targets: vec![DeliveryTarget::PullRequest { pr_id: "pr2".into(), head: "head1".into() }],
        is_final: true, nothing_to_review: false, card: crate::domain::CardSpec::delivery("Review the result", &[], true) }).unwrap();
    exec(&mut s, user(), Command::AuthorizeMerge { authorization_id: "auth2".into(),
        presentation_id: "pres2".into(), pr_id: "pr2".into(), expected_head: "head1".into(),
        method: MergeMethod::Squash }).unwrap();
    exec(&mut s, b.clone(), Command::AskQuestion { attachments: vec![], interaction_id: "q2".into(),
        scope: Scope::Task { task_id: "t2".into() }, recipient: Recipient::User, body: "Q?".into(),
        blocking: false, options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None, inputs: vec![] }], card: crate::domain::CardSpec::derived(crate::domain::CardKind::Provide, "Answer the question") }).unwrap();
    let foreign = Ids {
        task: "t2",
        agent: "b",
        interaction: "pres2",
        document: "doc2",
        revision: "rev2",
        pr: "pr2",
        request: "r",
        authorization: "auth2",
        schedule: "missing-schedule",
        run: "missing-run",
    };
    let outsiders = [agent(), Actor::Agent { agent_id: "ghost".into() }];
    for (command, creates) in commands_naming(&foreign) {
        if creates || !matches!(command, Command::StartTask { .. } | Command::PublishInformation { .. }
            | Command::AskQuestion { .. } | Command::RequestAction { .. } | Command::AnswerQuestion { .. }
            | Command::CreateDocument { .. } | Command::ReviseDocument { .. } | Command::RegisterPr { .. }
            | Command::LinkInteractionSubject { .. } | Command::PresentResult { .. }
            | Command::ClaimMergeAuthorization { .. } | Command::PublishStackReadiness { .. }
            | Command::CompleteTask { .. }) {
            continue;
        }
        for actor in &outsiders {
            let mut next = s.clone();
            let error = exec(&mut next, actor.clone(), command.clone())
                .expect_err(&format!("{actor:?} must not act on another Agent's Task with {command:?}"));
            assert!(matches!(error.code.as_str(), "not_assigned" | "no_task" | "forbidden"), "{command:?}: {}", error.code);
            assert_eq!(next, s);
        }
    }
    for (command, _) in commands_naming(&foreign) {
        for actor in &actors {
            let _ = exec(&mut s.clone(), actor.clone(), command.clone());
        }
    }
}
#[test]
fn an_external_merge_settles_the_delivery_without_authorization_or_acceptance() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    merged_by_someone_else(&mut s);
    assert!(s.merge_authorizations.is_empty());
    end_node(&mut s, "t");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
    reject_unchanged(&mut s, user(), Command::AcceptDelivery { presentation_id: "d".into(), targets: pr_targets() },
        "task_not_active");
}

#[test]
fn a_handed_schedule_launch_assigns_its_task_to_the_registered_session_once() {
    let mut state = fixture();
    let mut schedule = schedule();
    schedule.title = "Updated review".into();
    exec(&mut state, Actor::System, Command::CreateSchedule { schedule }).unwrap();
    exec(&mut state, Actor::System, Command::ClaimScheduleRun {
        schedule_id: "schedule-1".into(), run_id: "run-1".into(), origin: ScheduleRunOrigin::Due,
        now: 100, next_after_now: Some(200), expected_schedule_revision: None,
    }).unwrap();
    let (request, task) = scheduled_request_task("scheduled-request", "scheduled-task");
    exec(&mut state, Actor::System, Command::RecordScheduleRunSent {
        run_id: "run-1".into(), request, task, launch: scheduled_launch(), finished_at: 101,
    }).unwrap();
    exec(&mut state, Actor::System, Command::ClaimScheduleLaunch { run_id: "run-1".into(), now: 102 }).unwrap();
    reject_unchanged(&mut state, Actor::System, Command::RecordScheduleLaunchHanded {
        run_id: "run-1".into(), session_id: Some("ab1234".into()),
        agent_id: Some("never-registered".into()), finished_at: 103,
    }, "not_found");
    reject_unchanged(&mut state, agent(), Command::RecordScheduleLaunchHanded {
        run_id: "run-1".into(), session_id: Some("ab1234".into()), agent_id: Some("a".into()), finished_at: 103,
    }, "forbidden");
    let before = state.tasks[&TaskId::from("scheduled-task")].revision;
    exec(&mut state, Actor::System, Command::RecordScheduleLaunchHanded {
        run_id: "run-1".into(), session_id: Some("ab1234".into()), agent_id: Some("a".into()), finished_at: 103,
    }).unwrap();
    let task = &state.tasks[&TaskId::from("scheduled-task")];
    assert_eq!(task.agent_id, Some("a".into()));
    assert!(task.revision > before, "assignment moves the Task revision");
    let launch = state.schedule.runs[&ScheduleRunId::from("run-1")].launch.as_ref().unwrap();
    assert_eq!(launch.agent_id, Some("a".into()));
    assert_eq!(launch.session_id.as_deref(), Some("ab1234"));
    reject_unchanged(&mut state, Actor::System, Command::RecordScheduleLaunchHanded {
        run_id: "run-1".into(), session_id: Some("ab1234".into()), agent_id: Some("a".into()), finished_at: 104,
    }, "conflict");
    exec(&mut state, orchestrator(), Command::PublishInformation {
        attachments: vec![], interaction_id: "i".into(),
        scope: Scope::Task { task_id: "scheduled-task".into() }, recipient: Recipient::User, body: "Started".into(), card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Read the update"),
    }).unwrap();
    state.validate().unwrap();
}

#[test]
fn a_held_hook_question_reaches_the_user_and_its_answer_never_enters_the_outbox() {
    let mut s = fixture();
    let held = |id: &str, hold: Hold| Command::HoldHook { interaction_id: id.into(), task_id: "t".into(),
        agent_id: "a".into(), hold, body: "Bash: rm -rf build".into(), options: hold.options(), card: crate::domain::CardSpec::derived(crate::domain::CardKind::Authorize, "Allow the call") };
    reject_unchanged(&mut s, agent(), held("h", Hold::Permission), "forbidden");
    let outcome = exec(&mut s, Actor::System, held("h", Hold::Permission)).unwrap();
    assert_eq!(outcome.effects, [DomainEffect::NotifyUser { task_id: Some("t".into()), interaction_id: "h".into() }]);
    let interaction = &s.interactions[&"h".into()];
    assert_eq!((&interaction.author, &interaction.recipient, interaction.blocking),
        (&Author::Agent { agent_id: "a".into() }, &Recipient::User, true));
    assert_eq!(s.holds[&"h".into()], Hold::Permission);
    let kinds = |s: &DomainState| s.projection().inbox.iter().map(|item| item.kind.clone()).collect::<Vec<_>>();
    assert_eq!(kinds(&s), ["permission"]);
    exec(&mut s, Actor::System, held("p", Hold::Plan)).unwrap();
    assert_eq!(kinds(&s), ["permission", "plan"]);
    let mut inputs = BTreeMap::new();
    inputs.insert(Hold::NOTE.to_owned(), InputValue::Text("Branch first".into()));
    let answered = exec(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "h".into(),
        option_id: Hold::DENY.into(), inputs }).unwrap();
    assert!(answered.effects.is_empty(), "the hook hands the answer: {:?}", answered.effects);
    exec(&mut s, Actor::System, Command::Withdraw { interaction_id: "p".into() }).unwrap();
    assert!(s.interactions[&"p".into()].withdrawn);
    exec(&mut s, agent(), Command::AskQuestion { attachments: vec![], interaction_id: "q".into(),
        scope: Scope::Task { task_id: "t".into() }, recipient: Recipient::User, body: "Which?".into(),
        blocking: true, options: Hold::Permission.options(), card: crate::domain::CardSpec::derived(crate::domain::CardKind::Provide, "Answer the question") }).unwrap();
    reject_unchanged(&mut s, Actor::System, Command::Withdraw { interaction_id: "q".into() }, "forbidden");
    let ordinary = exec(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "q".into(),
        option_id: Hold::ALLOW.into(), inputs: BTreeMap::new() }).unwrap();
    assert!(matches!(ordinary.effects[..], [DomainEffect::NotifyAgent { .. }]));
}

fn hand_back(s: &mut DomainState, actor: Actor, instruction: Instruction) -> Result<CommandOutcome, DomainError> {
    exec(s, actor, Command::SubmitFeedback { attachments: vec![], presentation_id: "d".into(), targets: pr_targets(),
        body: "Back to you".into(), instruction })
}

fn waiting(s: &DomainState, id: &str) -> bool {
    s.item(&s.interactions[&id.into()]).pending
}

#[test]
fn a_delivery_sent_back_with_an_instruction_waits_on_its_agent_until_it_presents_again() {
    for instruction in [Instruction::Changes, Instruction::Question, Instruction::Reply, Instruction::Conflicts] {
        let mut s = fixture();
        register(&mut s);
        present(&mut s, "d", pr_targets());
        assert!(waiting(&s, "d"));
        let actor = if instruction == Instruction::Conflicts { Actor::System } else { user() };
        let outcome = hand_back(&mut s, actor, instruction).unwrap();
        assert!(matches!(&outcome.effects[..], [DomainEffect::NotifyAgent { message, .. }]
            if message == &instruction.message("Back to you")));
        assert!(!waiting(&s, "d"), "{instruction:?}");
        assert_eq!((s.tasks[&"t".into()].state, s.task_handed_back(&"t".into())), (TaskState::Running, Some(instruction)));
        exec(&mut s, orchestrator(), Command::PublishInformation { attachments: vec![], interaction_id: "note".into(),
            scope: Scope::Task { task_id: "t".into() }, recipient: Recipient::User, body: "It uses the new API".into(), card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Read the update") }).unwrap();
        assert!(!waiting(&s, "d"), "{instruction:?}: another word from the Agent never brings the delivery back");
        assert_eq!(s.task_handed_back(&"t".into()), Some(instruction));
        present(&mut s, "d2", pr_targets());
        assert!(waiting(&s, "d2"));
        assert_eq!((s.tasks[&"t".into()].state, s.task_handed_back(&"t".into())), (TaskState::ForYourReview, None));
    }
}

#[test]
fn every_instruction_but_conflicts_ends_by_asking_the_agent_to_present_again() {
    for instruction in [Instruction::Changes, Instruction::Question, Instruction::Reply] {
        let message = instruction.message("Use the new API");
        assert!(message.contains("Use the new API") && message.contains("present the result again with present_result"), "{message}");
    }
    assert_eq!(Instruction::Conflicts.message("Alondra's own words"), "Alondra's own words");
}

#[test]
fn alondra_alone_sends_only_conflicts_and_an_agent_never_sends_back() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    for (actor, instruction) in [(Actor::System, Instruction::Changes), (Actor::System, Instruction::Question),
        (agent(), Instruction::Conflicts)] {
        let before = s.clone();
        assert_eq!(hand_back(&mut s, actor, instruction).unwrap_err().code, "forbidden");
        assert_eq!(s, before);
    }
    hand_back(&mut s, Actor::System, Instruction::Conflicts).unwrap();
    let InteractionPayload::Delivery { feedback, .. } = &s.interactions[&"d".into()].payload else { unreachable!() };
    assert_eq!((feedback[0].user_id.as_str(), feedback[0].instruction), (Feedback::ALONDRA, Instruction::Conflicts));
}

fn rule_suggestion(destination: &str, rule: &str) -> serde_json::Value {
    serde_json::json!({"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": rule}], "behavior": "allow", "destination": destination})
}

#[test]
fn a_permission_offers_always_between_allow_and_deny_only_when_asked() {
    let ids = |always| Hold::Permission.options_offering(always).into_iter().map(|o| o.id).collect::<Vec<_>>();
    assert_eq!(ids(true), ["allow", "always", "deny"]);
    assert_eq!(ids(false), ["allow", "deny"]);
    assert_eq!(Hold::Permission.options(), Hold::Permission.options_offering(false));
    assert!(!Hold::Permission.approves(Hold::ALWAYS), "always is its own answer, not the plain yes");
}

#[test]
fn always_applies_the_narrowest_kept_allow_rule_claude_suggested() {
    use serde_json::json;
    let user = rule_suggestion("userSettings", "git *");
    let project = rule_suggestion("projectSettings", "git push *");
    let local = rule_suggestion("localSettings", "git push origin main");
    let session = rule_suggestion("session", "git push origin main");
    assert_eq!(Hold::always_suggestion(&json!([user, project, local])), Some(local.clone()));
    assert_eq!(Hold::always_suggestion(&json!([user, session, project])), Some(project));
    assert_eq!(Hold::always_suggestion(&json!([user, session])), Some(session));
    assert_eq!(Hold::always_suggestion(&json!([user])), None, "never the user's global settings");
    assert_eq!(Hold::always_suggestion(&json!([{"type": "setMode", "mode": "acceptEdits", "destination": "localSettings"},
        {"type": "addDirectories", "directories": ["/tmp"], "destination": "localSettings"},
        {"type": "addRules", "rules": [{"toolName": "Bash"}], "behavior": "deny", "destination": "localSettings"},
        {"type": "addRules", "rules": [], "behavior": "allow", "destination": "localSettings"}])), None);
    assert_eq!(Hold::always_suggestion(&json!([])), None);
    assert_eq!(Hold::always_suggestion(&serde_json::Value::Null), None);
}
#[test]
fn a_node_cannot_send_the_user_a_notice_but_alondra_and_the_orchestrator_can() {
    let mut s = fixture();
    let notice = |id: &str, recipient: Recipient| Command::PublishInformation {
        attachments: vec![], interaction_id: id.into(), scope: Scope::Task { task_id: "t".into() }, recipient,
        body: "Update".into(), card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Read the update"),
    };
    let error = exec(&mut s, agent(), notice("n", Recipient::User)).unwrap_err();
    assert_eq!(error.code, "forbidden");
    assert!(error.to_string().contains("send notices"), "{error}");
    reject_unchanged(&mut s, agent(), notice("n", Recipient::User), "forbidden");
    exec(&mut s, Actor::System, notice("system", Recipient::User)).unwrap();
    exec(&mut s, orchestrator(), notice("orchestrator", Recipient::User)).unwrap();
}
#[test]
fn releasing_a_task_whose_node_the_user_closed_puts_it_back_to_not_started_with_no_agent() {
    let mut s = fixture();
    exec(&mut s, agent(), Command::StartTask { task_id: "t".into() }).unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Running);
    let release = Command::ReleaseTask { task_id: "t".into(), reason: "node closed by the user".into() };
    reject_unchanged(&mut s, user(), release.clone(), "forbidden");
    reject_unchanged(&mut s, agent(), release.clone(), "forbidden");
    exec(&mut s, Actor::System, release.clone()).unwrap();
    let task = &s.tasks[&"t".into()];
    assert_eq!((task.state, task.agent_id.clone(), task.started), (TaskState::NotStarted, None, false));
    assert!(s.agents.contains_key(&"a".into()), "the closed agent stays on record");
    reject_unchanged(&mut s, Actor::System, release, "conflict");
    register_b(&mut s);
    exec(&mut s, user(), Command::AssignTask { task_id: "t".into(), agent_id: "b".into(), model: None }).unwrap();
    exec(&mut s, agent_b(), Command::StartTask { task_id: "t".into() }).unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Running, "a new node takes the freed task up again");
}

#[test]
fn a_finished_task_is_not_released() {
    let mut s = fixture();
    exec(&mut s, user(), Command::CancelTask { task_id: "t".into() }).unwrap();
    reject_unchanged(&mut s, Actor::System, Command::ReleaseTask { task_id: "t".into(), reason: "node closed by the user".into() }, "task_not_active");
}

fn merge_observed(s: &mut DomainState) -> CommandOutcome {
    exec(s, Actor::System, Command::ObservePrState { pr_id: "pr".into(), head: "head1".into(),
        base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()) }).unwrap()
}
fn sent_to_node(out: &CommandOutcome) -> Vec<String> {
    out.effects.iter().filter_map(|e| match e {
        DomainEffect::NotifyAgent { message, .. } => Some(message.clone()),
        _ => None,
    }).collect()
}
fn accept_text(s: &mut DomainState) -> CommandOutcome {
    present(s, "final", text());
    exec(s, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets: text() }).unwrap()
}
fn second_task(s: &mut DomainState, id: &str, agent_id: Option<&str>) {
    exec(s, orchestrator(), Command::CreateTask { task_id: id.into(), request_id: "r".into(), title: "Second".into(),
        agent_id: agent_id.map(Into::into), review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")), }).unwrap();
}
#[test]
fn accepting_and_merging_a_pr_wakes_the_node_only_to_clean_up() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    let accepted = exec(&mut s, user(), Command::AcceptDelivery { presentation_id: "d".into(), targets: pr_targets() }).unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::WaitingDependency);
    assert!(s.tasks[&"t".into()].wrap_up.asked_at.is_none(), "merge pending: no cleanup yet");
    assert!(sent_to_node(&accepted).is_empty(), "nothing for the node to do while Alondra merges");
    let merged = merge_observed(&mut s);
    assert_eq!(sent_to_node(&merged), vec![cleanup_request("Result", &"t".into())]);
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Closing);
}
#[test]
fn a_pr_merged_on_the_remote_during_review_moves_the_task_to_closing_without_acceptance() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    merge_observed(&mut s);
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Closing);
    assert!(s.tasks[&"t".into()].wrap_up.asked_at.is_some());
}
#[test]
fn recording_the_node_end_makes_a_closing_task_done() {
    let mut s = fixture();
    accept_text(&mut s);
    end_node(&mut s, "t");
    let t = &s.tasks[&"t".into()];
    assert_eq!(t.state, TaskState::Done);
    assert_eq!(t.wrap_up.node, Some(NodeEnd::Closed));
}
#[test]
fn only_alondra_records_a_node_end() {
    let mut s = fixture();
    accept_text(&mut s);
    for actor in [agent(), orchestrator(), user()] {
        reject_unchanged(&mut s, actor, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: None }, "forbidden");
    }
}
#[test]
fn a_node_end_needs_a_closing_or_terminal_task() {
    let mut s = fixture();
    reject_unchanged(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::KeptByUser, agent_id: None }, "not_closing");
    present(&mut s, "final", text());
    reject_unchanged(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::UsersOwn, agent_id: None }, "not_closing");
}
#[test]
fn a_node_removed_while_its_task_went_back_to_work_leaves_the_task_without_a_node() {
    let mut s = fixture();
    accept_text(&mut s);
    exec(&mut s, user(), Command::SubmitFeedback { attachments: vec![], presentation_id: "final".into(),
        targets: text(), body: "One more thing".into(), instruction: Instruction::Changes }).unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Running);
    exec(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: None }).unwrap();
    let t = &s.tasks[&"t".into()];
    assert_eq!((t.state, t.agent_id.clone(), t.started), (TaskState::NotStarted, None, false), "ready to be relaunched, never working without a node");
    assert!(t.wrap_up.is_default(), "a new node closes with its own cleanup");
}
#[test]
fn the_same_node_end_twice_changes_nothing_and_another_is_a_conflict() {
    let mut s = fixture();
    accept_text(&mut s);
    end_node(&mut s, "t");
    let again = exec(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: None }).unwrap();
    assert!(!again.changed);
    reject_unchanged(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Gone, agent_id: None }, "conflict");
}
#[test]
fn only_the_tasks_node_confirms_its_cleanup_and_only_while_it_closes() {
    let mut s = fixture();
    register_b(&mut s);
    reject_unchanged(&mut s, agent(), Command::ConfirmCleanup { task_id: "t".into() }, "not_closing");
    accept_text(&mut s);
    reject_unchanged(&mut s, agent_b(), Command::ConfirmCleanup { task_id: "t".into() }, "not_assigned");
    exec(&mut s, agent(), Command::ConfirmCleanup { task_id: "t".into() }).unwrap();
    assert_eq!(s.tasks[&"t".into()].wrap_up.cleaned_at, Some(s.revision));
    end_node(&mut s, "t");
    reject_unchanged(&mut s, agent(), Command::ConfirmCleanup { task_id: "t".into() }, "task_not_active");
}
fn post_merge_work(reason: Option<&str>) -> Command {
    Command::DeclarePostMergeWork { task_id: "t".into(), reason: reason.map(str::to_owned) }
}
#[test]
fn only_the_tasks_node_declares_work_after_the_merge_and_cleanup_done_ends_it() {
    let mut s = fixture();
    register_b(&mut s);
    accept_text(&mut s);
    reject_unchanged(&mut s, agent_b(), post_merge_work(Some("release.sh")), "not_assigned");
    reject_unchanged(&mut s, user(), post_merge_work(Some("release.sh")), "not_assigned");
    exec(&mut s, agent(), post_merge_work(Some("  release.sh publishes 0.2.54  "))).unwrap();
    assert_eq!(s.tasks[&"t".into()].wrap_up.post_merge_work, Some(PostMergeWork { reason: "release.sh publishes 0.2.54".into(), declared_at: s.revision }));
    exec(&mut s, agent(), post_merge_work(Some(" "))).unwrap();
    assert_eq!(s.tasks[&"t".into()].wrap_up.post_merge_work, None, "a blank reason ends it");
    exec(&mut s, agent(), post_merge_work(Some(&"x".repeat(500)))).unwrap();
    assert!(s.tasks[&"t".into()].wrap_up.post_merge_work.as_ref().unwrap().reason.chars().count() <= rules::POST_MERGE_WORK_MAX);
    exec(&mut s, agent(), Command::ConfirmCleanup { task_id: "t".into() }).unwrap();
    assert_eq!(s.tasks[&"t".into()].wrap_up.post_merge_work, None);
    end_node(&mut s, "t");
    reject_unchanged(&mut s, agent(), post_merge_work(Some("release.sh")), "task_not_active");
}
#[test]
fn closing_the_node_drops_the_work_it_declared_after_the_merge() {
    let mut s = fixture();
    accept_text(&mut s);
    exec(&mut s, agent(), post_merge_work(Some("release.sh"))).unwrap();
    end_node(&mut s, "t");
    assert_eq!(s.tasks[&"t".into()].wrap_up.post_merge_work, None);
}
#[test]
fn a_second_cleanup_confirmation_moves_its_revision() {
    let mut s = fixture();
    accept_text(&mut s);
    exec(&mut s, agent(), Command::ConfirmCleanup { task_id: "t".into() }).unwrap();
    let first = s.tasks[&"t".into()].wrap_up.cleaned_at.unwrap();
    exec(&mut s, agent(), Command::ConfirmCleanup { task_id: "t".into() }).unwrap();
    assert!(s.tasks[&"t".into()].wrap_up.cleaned_at.unwrap() > first);
}
#[test]
fn the_cleanup_request_is_sent_once_even_when_a_blocking_question_interrupts_it() {
    let mut s = fixture();
    let accepted = accept_text(&mut s);
    assert_eq!(sent_to_node(&accepted).len(), 1);
    let asked_at = s.tasks[&"t".into()].wrap_up.asked_at;
    let question = exec(&mut s, agent(), Command::AskQuestion {
        attachments: vec![], interaction_id: "q".into(), scope: Scope::Task { task_id: "t".into() },
        recipient: Recipient::User, body: "Delete the cache?".into(), blocking: true,
        options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None, inputs: vec![] }],
        card: crate::domain::CardSpec::derived(crate::domain::CardKind::Provide, "Answer the question"),
    }).unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::NeedsYou);
    assert!(!sent_to_node(&question).iter().any(|m| m.contains("cleanup_done")));
    let answered = exec(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "q".into(),
        option_id: "yes".into(), inputs: BTreeMap::new() }).unwrap();
    assert!(!sent_to_node(&answered).iter().any(|m| m.contains("cleanup_done")), "{:?}", sent_to_node(&answered));
    assert_eq!(s.tasks[&"t".into()].wrap_up.asked_at, asked_at);
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Closing);
}
#[test]
fn feedback_on_an_accepted_text_delivery_during_cleanup_takes_the_task_back_and_forgets_the_cleanup() {
    let mut s = fixture();
    accept_text(&mut s);
    exec(&mut s, agent(), Command::ConfirmCleanup { task_id: "t".into() }).unwrap();
    exec(&mut s, user(), Command::SubmitFeedback { attachments: vec![], presentation_id: "final".into(),
        targets: text(), body: "One more thing".into(), instruction: Instruction::Changes }).unwrap();
    let t = &s.tasks[&"t".into()];
    assert_eq!(t.state, TaskState::Running);
    assert!(t.wrap_up.asked_at.is_none() && t.wrap_up.cleaned_at.is_none());
    present(&mut s, "final2", text());
    let again = exec(&mut s, user(), Command::AcceptDelivery { presentation_id: "final2".into(), targets: text() }).unwrap();
    assert_eq!(sent_to_node(&again), vec![cleanup_request("Result", &"t".into())]);
}
#[test]
fn feedback_on_a_merged_delivery_during_cleanup_is_refused() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    merge_observed(&mut s);
    reject_unchanged(&mut s, user(), Command::SubmitFeedback { attachments: vec![], presentation_id: "d".into(),
        targets: pr_targets(), body: "More".into(), instruction: Instruction::Changes }, "task_closing");
}
#[test]
fn cancelling_a_task_tells_its_node_to_stop_and_marks_it_for_closing() {
    let mut s = fixture();
    let out = exec(&mut s, user(), Command::CancelTask { task_id: "t".into() }).unwrap();
    assert_eq!(sent_to_node(&out), vec![cancelled_notice("Result")]);
    assert!(s.tasks[&"t".into()].wrap_up.close_cancelled);
    second_task(&mut s, "u", None);
    let out = exec(&mut s, user(), Command::CancelTask { task_id: "u".into() }).unwrap();
    assert!(sent_to_node(&out).is_empty());
    assert!(!s.tasks[&"u".into()].wrap_up.close_cancelled);
}
#[test]
fn a_settled_task_without_a_node_is_done_at_once() {
    let mut s = fixture();
    present(&mut s, "final", text());
    s.tasks.get_mut(&"t".into()).unwrap().agent_id = None;
    let out = exec(&mut s, user(), Command::AcceptDelivery { presentation_id: "final".into(), targets: text() }).unwrap();
    assert!(sent_to_node(&out).is_empty());
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
}
#[test]
fn complete_task_leaves_a_task_with_a_node_closing() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    exec(&mut s, user(), Command::AcceptDelivery { presentation_id: "d".into(), targets: pr_targets() }).unwrap();
    merge_observed(&mut s);
    let _ = exec(&mut s, Actor::System, Command::CompleteTask { task_id: "t".into(), presentation_id: "d".into() });
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Closing);
    end_node(&mut s, "t");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
}
#[test]
fn reopening_a_task_whose_node_was_closed_leaves_it_without_a_node() {
    let mut s = fixture();
    accept_text(&mut s);
    end_node(&mut s, "t");
    exec(&mut s, user(), Command::ReopenTask { task_id: "t".into() }).unwrap();
    let t = &s.tasks[&"t".into()];
    assert!(!t.state.terminal());
    assert!(t.agent_id.is_none());
    assert!(t.wrap_up.is_default());
}
#[test]
fn the_orchestrator_cannot_give_a_node_a_second_task() {
    let mut s = fixture();
    accept_text(&mut s);
    end_node(&mut s, "t");
    second_task(&mut s, "u", None);
    reject_unchanged(&mut s, orchestrator(), Command::AssignTask { task_id: "u".into(), agent_id: "a".into(), model: None }, "agent_used");
    reject_unchanged(&mut s, orchestrator(), Command::CreateTask { task_id: "v".into(), request_id: "r".into(), title: "Third".into(),
        agent_id: Some("a".into()), review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")), }, "agent_used");
}
#[test]
fn the_user_may_still_assign_a_node_whose_task_ended() {
    let mut s = fixture();
    accept_text(&mut s);
    end_node(&mut s, "t");
    second_task(&mut s, "u", None);
    exec(&mut s, user(), Command::AssignTask { task_id: "u".into(), agent_id: "a".into(), model: None }).unwrap();
    assert_eq!(s.tasks[&"u".into()].agent_id, Some("a".into()));
}
#[test]
fn alondra_assigns_a_node_only_to_an_unstarted_task_without_one() {
    let mut s = fixture();
    register_b(&mut s);
    second_task(&mut s, "u", None);
    exec(&mut s, Actor::System, Command::AssignTask { task_id: "u".into(), agent_id: "b".into(), model: None }).unwrap();
    assert_eq!(s.tasks[&"u".into()].agent_id, Some("b".into()));
    reject_unchanged(&mut s, Actor::System, Command::AssignTask { task_id: "u".into(), agent_id: "b".into(), model: None }, "conflict");
    reject_unchanged(&mut s, Actor::System, Command::AssignTask { task_id: "t".into(), agent_id: "b".into(), model: None }, "conflict");
}
#[test]
fn a_task_from_before_wrap_ups_decodes_with_none() {
    let mut s = fixture();
    accept_text(&mut s);
    let mut json = serde_json::to_value(&s.tasks[&"t".into()]).unwrap();
    json.as_object_mut().unwrap().remove("wrap_up");
    let task: Task = serde_json::from_value(json).unwrap();
    assert!(task.wrap_up.is_default());
}

#[test]
fn a_task_waiting_on_another_starts_in_the_step_that_finishes_it() {
    let mut s = fixture();
    second_task(&mut s, "u", None);
    exec(&mut s, user(), Command::SetDependencies { task_id: "u".into(), dependencies: ["t".into()].into(), stack: None }).unwrap();
    assert_eq!(s.tasks[&"u".into()].state, TaskState::WaitingDependency);
    accept_text(&mut s);
    end_node(&mut s, "t");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
    assert_eq!(s.tasks[&"u".into()].state, TaskState::NotStarted, "no later command is needed to release it");
}

#[test]
fn recording_the_end_of_a_node_the_task_no_longer_has_changes_nothing() {
    let mut s = fixture();
    register_b(&mut s);
    accept_text(&mut s);
    exec(&mut s, user(), Command::SubmitFeedback { attachments: vec![], presentation_id: "final".into(),
        targets: text(), body: "One more thing".into(), instruction: Instruction::Changes }).unwrap();
    exec(&mut s, orchestrator(), Command::AssignTask { task_id: "t".into(), agent_id: "b".into(), model: None }).unwrap();
    reject_unchanged(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: Some("a".into()) }, "conflict");
    assert_eq!(s.tasks[&"t".into()].agent_id, Some("b".into()), "the new node was not unassigned");
}

#[test]
fn a_delivery_stacked_on_an_open_pr_is_no_card_until_that_pr_merges_and_then_one_card_and_one_push() {
    let mut s = fixture();
    register(&mut s);
    register_b(&mut s);
    present(&mut s, "d", pr_targets());
    let stack = StackDependency { upstream_task: "t".into(), presentation_id: "d".into(), pr_id: "pr".into(), head: "head1".into() };
    exec(&mut s, orchestrator(), Command::CreateTask {
        task_id: "down".into(), request_id: "r".into(), title: "Downstream".into(), agent_id: Some("b".into()),
        review_prs: true, dependencies: BTreeSet::new(), stack: Some(stack),
        launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")),
    }).unwrap();
    exec(&mut s, agent(), Command::PublishStackReadiness {
        task_id: "t".into(), presentation_id: "d".into(), pr_id: "pr".into(), head: "head1".into(), ready: true,
    }).unwrap();
    exec(&mut s, agent_b(), Command::StartTask { task_id: "down".into() }).unwrap();
    exec(&mut s, agent_b(), Command::RegisterPr {
        task_id: "down".into(), pr_id: "pr-down".into(), host: "github.com".into(), repository: "owner/repo".into(),
        number: 2, head: "head-down".into(), base: "upstream-branch".into(), allowed_methods: vec![MergeMethod::Squash],
    }).unwrap();
    let delivered = exec(&mut s, agent_b(), Command::PresentResult {
        attachments: vec![], task_id: "down".into(), interaction_id: "d-down".into(), body: "Downstream ready".into(),
        targets: vec![DeliveryTarget::PullRequest { pr_id: "pr-down".into(), head: "head-down".into() }],
        is_final: true, nothing_to_review: false, card: crate::domain::CardSpec::delivery("Review the result", &[], true),
    }).unwrap();
    let notified = |outcome: &CommandOutcome| outcome.effects.iter().filter(|e| matches!(e, DomainEffect::NotifyUser { interaction_id, .. } if interaction_id.0 == "d-down")).count();
    assert_eq!(notified(&delivered), 0, "no push while it waits for the PR of the delivery in front");
    assert_eq!(s.attention(&s.tasks[&"down".into()]), Attention::Wait(WaitReason::UpstreamPr { number: 1 }));
    assert!(!waiting(&s, "d-down"));
    let observe = || Command::ObservePrState {
        pr_id: "pr".into(), head: "head1".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()),
    };
    let merged = exec(&mut s, Actor::System, observe()).unwrap();
    assert_eq!(notified(&merged), 1, "one push when the PR in front merges");
    assert!(waiting(&s, "d-down"));
    let again = exec(&mut s, Actor::System, observe()).unwrap();
    assert_eq!(notified(&again), 0, "and never a second time");
}

#[test]
fn pr_observations_on_a_pending_delivery_never_notify_again() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    let observed = exec(&mut s, Actor::System, Command::ObservePrState {
        pr_id: "pr".into(), head: "head1".into(), base: "main".into(), state: PrState::Open, merged_head: None,
    }).unwrap();
    assert!(!observed.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { .. })), "the card already went out when it was delivered");
    assert!(waiting(&s, "d"));
}
/// A question the node asks without blocking, the way BlurCamera's keywords task asked whether to
/// cover card numbers that fail Luhn.
fn ask_aside(s: &mut DomainState, id: &str) -> CommandOutcome {
    exec(s, agent(), Command::AskQuestion {
        attachments: vec![], interaction_id: id.into(), scope: Scope::Task { task_id: "t".into() },
        recipient: Recipient::User, body: "Cover card numbers that fail Luhn too?".into(), blocking: false,
        options: vec![QuestionOption { id: "keep".into(), label: "Keep it".into(), description: None, inputs: vec![] },
            QuestionOption { id: "cover".into(), label: "Cover them".into(), description: None, inputs: vec![] }],
        card: crate::domain::CardSpec::derived(crate::domain::CardKind::Decide, "Decide whether to cover invalid card numbers"),
    }).unwrap()
}
fn answer_aside(s: &mut DomainState, id: &str) {
    exec(s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: id.into(),
        option_id: "keep".into(), inputs: BTreeMap::new() }).unwrap();
}
fn pending(s: &DomainState, id: &str) -> bool {
    s.projection().inbox.iter().any(|item| item.id == format!("interaction:{id}") && item.pending)
}
#[test]
fn a_final_delivery_is_refused_while_a_question_of_its_task_waits() {
    let mut s = fixture();
    register(&mut s);
    ask_aside(&mut s, "q");
    let before = s.clone();
    let refused = exec(&mut s, agent(), Command::PresentResult {
        attachments: vec![], task_id: "t".into(), interaction_id: "d".into(), body: "Result ready".into(),
        targets: pr_targets(), is_final: true, nothing_to_review: false, card: crate::domain::CardSpec::delivery("Review the result", &[], true),
    }).unwrap_err();
    assert_eq!(refused.code, "unanswered_questions");
    assert!(refused.message.contains("You have unanswered questions (“Decide whether to cover invalid card numbers”)"), "{}", refused.message);
    assert_eq!(s, before);
    answer_aside(&mut s, "q");
    present(&mut s, "d", pr_targets());
    assert!(pending(&s, "d"), "once answered, the delivery reaches the user");
}
/// 9 Oct: PR #845's node pushed after its final delivery and kept working; the user got its card
/// with no way to accept it, only a note that the PR had commits the result did not include.
#[test]
fn a_push_after_the_final_delivery_withdraws_it_until_the_node_presents_the_new_head() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    assert!(pending(&s, "d"));
    let pushed = exec(&mut s, Actor::System, Command::ObservePrState { pr_id: "pr".into(), head: "head2".into(),
        base: "main".into(), state: PrState::Open, merged_head: None }).unwrap();
    let t = &s.tasks[&"t".into()];
    assert_eq!((t.state, t.final_presentation.as_ref()), (TaskState::Running, None), "the task is in progress again");
    assert!(s.interactions[&"d".into()].withdrawn);
    assert!(!pending(&s, "d"), "no card the user cannot accept");
    assert_eq!(sent_to_node(&pushed), vec![delivery_withdrawn_behind_pr(1)], "the node hears why");
    let merge = |presentation: &str, head: &str| Command::AuthorizeMerge { authorization_id: format!("auth-{presentation}").into(),
        presentation_id: presentation.into(), pr_id: "pr".into(), expected_head: head.into(), method: MergeMethod::Squash };
    let before = s.clone();
    assert!(exec(&mut s, user(), merge("d", "head1")).is_err(), "the withdrawn delivery cannot be merged");
    assert_eq!(s, before);
    present(&mut s, "d2", vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head2".into() }]);
    assert!(pending(&s, "d2"), "presenting the new head brings the card back");
    exec(&mut s, user(), merge("d2", "head2")).unwrap();
}
#[test]
fn observing_the_head_a_delivery_presented_keeps_it() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    let out = exec(&mut s, Actor::System, Command::ObservePrState { pr_id: "pr".into(), head: "head1".into(),
        base: "main".into(), state: PrState::Open, merged_head: None }).unwrap();
    assert!(pending(&s, "d") && sent_to_node(&out).is_empty());
    assert_eq!(s.tasks[&"t".into()].state, TaskState::ForYourReview);
}
/// Revisions 1522-1570: BlurCamera's keywords task asked a question after its final delivery, the
/// PR was merged and the task closed, and the question left the inbox unanswered.
#[test]
fn a_question_asked_after_the_final_delivery_withdraws_it_until_the_node_delivers_again() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    assert!(pending(&s, "d"));
    let asked = ask_aside(&mut s, "q");
    let t = &s.tasks[&"t".into()];
    assert_eq!((t.state, t.final_presentation.as_ref()), (TaskState::Running, None), "the task is in progress again");
    assert!(s.interactions[&"d".into()].withdrawn);
    assert!(!pending(&s, "d") && pending(&s, "q"), "the PR card leaves the queue; the question stays");
    assert!(sent_to_node(&asked).contains(&delivery_withdrawn_for_questions()), "the node hears why");
    let merge = |presentation: &str| Command::AuthorizeMerge { authorization_id: "auth".into(), presentation_id: presentation.into(),
        pr_id: "pr".into(), expected_head: "head1".into(), method: MergeMethod::Squash };
    let before = s.clone();
    assert!(exec(&mut s, user(), merge("d")).is_err(), "the withdrawn delivery cannot be merged");
    assert_eq!(s, before);
    answer_aside(&mut s, "q");
    present(&mut s, "d2", pr_targets());
    assert!(pending(&s, "d2"), "delivering again brings the PR card back");
    exec(&mut s, user(), merge("d2")).unwrap();
    merge_observed(&mut s);
    end_node(&mut s, "t");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
}
/// A PR merged on GitHub is not taken back: the task waits for the answer instead of closing.
#[test]
fn a_task_whose_pr_merged_does_not_close_while_a_question_asked_afterwards_waits() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    merge_observed(&mut s);
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Closing);
    ask_aside(&mut s, "q");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::NeedsYou);
    assert!(!s.interactions[&"d".into()].withdrawn, "a merged delivery stays");
    assert!(pending(&s, "q"));
    answer_aside(&mut s, "q");
    end_node(&mut s, "t");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
}
#[test]
fn an_accepted_text_delivery_waits_for_a_question_asked_during_cleanup() {
    let mut s = fixture();
    accept_text(&mut s);
    ask_aside(&mut s, "q");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::NeedsYou);
    let ended = exec(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: None });
    assert!(ended.is_ok());
    assert_eq!(s.tasks[&"t".into()].state, TaskState::NeedsYou, "the task is not done while its question waits");
    assert!(pending(&s, "q"));
    answer_aside(&mut s, "q");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
}
fn hold_plan(s: &mut DomainState, id: &str) {
    exec(s, Actor::System, Command::HoldHook { interaction_id: id.into(), task_id: "t".into(), agent_id: "a".into(), hold: Hold::Plan,
        body: "# Tests without stray temp folders\n\n1. Point TMPDIR at Bazel's".into(), options: Hold::Plan.options(),
        card: crate::domain::CardSpec::derived(crate::domain::CardKind::Review, "Tests without stray temp folders")
            .content(crate::domain::ContentKind::Plan, "1. Point TMPDIR at Bazel's") }).unwrap();
}
fn notices(s: &DomainState) -> Vec<&Interaction> {
    s.interactions.values().filter(|i| i.id.0.starts_with(ENDED_UNANSWERED_PREFIX)).collect()
}
/// Cancelling “Tests usan TEST_TMPDIR de Bazel” with its plan open told the user the task “ended
/// before you answered”: the user stopped that work, so its cards just leave.
#[test]
fn cancelling_a_task_with_its_plan_open_takes_its_cards_away_in_silence() {
    let mut s = fixture();
    hold_plan(&mut s, "plan");
    ask_aside(&mut s, "q");
    let cancelled = exec(&mut s, user(), Command::CancelTask { task_id: "t".into() }).unwrap();
    assert!(s.interactions[&"plan".into()].withdrawn && s.interactions[&"q".into()].withdrawn);
    assert!(!pending(&s, "plan") && !pending(&s, "q"));
    assert!(notices(&s).is_empty(), "no notice about cards of a cancelled task");
    assert!(!cancelled.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { interaction_id, .. }
        if interaction_id.0.starts_with(ENDED_UNANSWERED_PREFIX))));
}
/// The node of a cancelled task asked what to do because a hook kept it from committing its
/// half-done work: the question was withdrawn in silence and the node sat blocked, unseen. What a
/// node asks while it winds down stays with the user until Alondra has closed it.
#[test]
fn a_question_the_node_of_a_cancelled_task_asks_while_it_winds_down_reaches_the_user() {
    let mut s = fixture();
    ask_aside(&mut s, "before");
    exec(&mut s, user(), Command::CancelTask { task_id: "t".into() }).unwrap();
    assert!(s.interactions[&"before".into()].withdrawn, "what it asked about the work the user stopped leaves");
    ask_aside(&mut s, "stuck");
    exec(&mut s, Actor::System, Command::ReconcileNodes { listed: BTreeMap::new(), at: 0 }).unwrap();
    assert!(!s.interactions[&"stuck".into()].withdrawn);
    assert!(pending(&s, "stuck"), "the user sees what the node asks while it winds down");
    let item = s.projection().inbox.into_iter().find(|item| item.id == "interaction:stuck").unwrap();
    assert!(item.available_actions.iter().any(|action| action.kind == "answer_question" && action.enabled), "{:?}", item.available_actions);
    exec(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "stuck".into(),
        option_id: "keep".into(), inputs: BTreeMap::new() }).unwrap();

    ask_aside(&mut s, "late");
    exec(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: Some("a".into()) }).unwrap();
    assert!(s.interactions[&"late".into()].withdrawn, "once the node is gone, nothing waits for the answer");
    assert!(!pending(&s, "late"));
}
/// A notice an older daemon left for a cancelled task leaves the inbox on the next change.
#[test]
fn a_notice_left_for_a_cancelled_task_is_taken_away() {
    let mut s = fixture();
    ask_aside(&mut s, "q");
    s.tasks.get_mut(&"t".into()).unwrap().state = TaskState::Done;
    exec(&mut s, Actor::System, Command::ReconcileNodes { listed: BTreeMap::new(), at: 0 }).unwrap();
    let notice = format!("{ENDED_UNANSWERED_PREFIX}q");
    assert!(pending(&s, &notice));
    s.tasks.get_mut(&"t".into()).unwrap().state = TaskState::Cancelled;
    exec(&mut s, Actor::System, Command::ReconcileNodes { listed: BTreeMap::new(), at: 0 }).unwrap();
    assert!(!pending(&s, &notice));
}
/// A plan nobody approved for work that is already done changes nothing for the user.
#[test]
fn a_task_done_with_its_plan_open_says_nothing_about_the_plan() {
    let mut s = fixture();
    hold_plan(&mut s, "plan");
    s.tasks.get_mut(&"t".into()).unwrap().state = TaskState::Done;
    exec(&mut s, Actor::System, Command::ReconcileNodes { listed: BTreeMap::new(), at: 0 }).unwrap();
    assert!(s.interactions[&"plan".into()].withdrawn);
    assert!(notices(&s).is_empty());
}
/// The state an older daemon left: a done task with a question nobody answered.
#[test]
fn a_task_left_done_with_an_open_question_tells_the_user_once() {
    let mut s = fixture();
    ask_aside(&mut s, "q");
    s.tasks.get_mut(&"t".into()).unwrap().state = TaskState::Done;
    exec(&mut s, Actor::System, Command::ReconcileNodes { listed: BTreeMap::new(), at: 0 }).unwrap();
    let notice = format!("{ENDED_UNANSWERED_PREFIX}q");
    assert!(pending(&s, &notice) && !pending(&s, "q"));
    let card = &s.interactions[&InteractionId(notice.clone())].card;
    assert_eq!(card.need, "No longer waits for your answer: Decide whether to cover invalid card numbers");
    assert_eq!(card.why, "The work was finished before you answered");
    assert_eq!(card.content.as_ref().unwrap().body, "Cover card numbers that fail Luhn too?");
    let again = exec(&mut s, Actor::System, Command::ReconcileNodes { listed: BTreeMap::new(), at: 0 }).unwrap();
    assert!(!again.effects.iter().any(|e| matches!(e, DomainEffect::NotifyUser { .. })));
    assert_eq!(s.interactions.keys().filter(|id| id.0.starts_with(ENDED_UNANSWERED_PREFIX)).count(), 1);
}
/// PR #765: “Diseño: instalación guiada” depended on the snapshot fix only to wait before
/// committing; its PR was accepted and the merge was refused with dependency_not_done.
#[test]
fn a_dependency_holds_back_the_start_but_never_the_merge_of_an_accepted_pr() {
    let mut s = fixture();
    second_task(&mut s, "fix", None);
    exec(&mut s, orchestrator(), Command::SetDependencies {
        task_id: "t".into(), dependencies: BTreeSet::from(["fix".into()]), stack: None }).unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::WaitingDependency);
    register(&mut s);
    present(&mut s, "d", pr_targets());
    authorize(&mut s);
    exec(&mut s, Actor::System, Command::ClaimMergeAuthorization {
        authorization_id: "auth".into(), remote_head: "head1".into(), remote_base: "main".into(), base_verified: true }).unwrap();
    merged_by_someone_else(&mut s);
    end_node(&mut s, "t");
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
    assert_ne!(s.tasks[&"fix".into()].state, TaskState::Done);
}
#[test]
fn a_dependency_still_holds_back_the_start() {
    let mut s = fixture();
    register_b(&mut s);
    second_task(&mut s, "fix", None);
    second_task(&mut s, "after", Some("b"));
    exec(&mut s, orchestrator(), Command::SetDependencies {
        task_id: "after".into(), dependencies: BTreeSet::from(["fix".into()]), stack: None }).unwrap();
    reject_unchanged(&mut s, agent_b(), Command::StartTask { task_id: "after".into() }, "dependency_not_done");
}
/// The node of “Permisos seguros” asked the user to tell it when main passed the snapshot tests:
/// waiting is a dependency the orchestrator sets, and Alondra resumes the node when it is done.
#[test]
fn a_node_waiting_on_a_task_is_resumed_once_when_that_task_is_done() {
    let mut s = fixture();
    register_b(&mut s);
    second_task(&mut s, "waits", Some("b"));
    exec(&mut s, agent_b(), Command::StartTask { task_id: "waits".into() }).unwrap();
    exec(&mut s, orchestrator(), Command::SetDependencies {
        task_id: "waits".into(), dependencies: BTreeSet::from(["t".into()]), stack: None }).unwrap();
    assert_eq!(s.tasks[&"waits".into()].state, TaskState::WaitingDependency);
    let accepted = accept_text(&mut s);
    assert!(!accepted.effects.iter().any(|e| matches!(e, DomainEffect::NotifyAgent { agent_id, .. } if agent_id.0 == "b")));
    let done = exec(&mut s, Actor::System, Command::RecordNodeEnd { task_id: "t".into(), end: NodeEnd::Closed, agent_id: None }).unwrap();
    assert_eq!(s.tasks[&"waits".into()].state, TaskState::Running);
    let resumed: Vec<_> = done.effects.iter().filter_map(|e| match e {
        DomainEffect::NotifyAgent { agent_id, task_id, message, .. } if agent_id.0 == "b" => Some((task_id.clone(), message.clone())),
        _ => None,
    }).collect();
    assert_eq!(resumed, vec![(Some("waits".into()), dependencies_done_notice(&["Result"]))]);
    let later = exec(&mut s, Actor::System, Command::ReconcileNodes { listed: BTreeMap::new(), at: 0 }).unwrap();
    assert!(!later.effects.iter().any(|e| matches!(e, DomainEffect::NotifyAgent { .. })));
}
#[test]
fn only_the_system_binds_the_orchestrators_session_and_a_new_one_replaces_it() {
    let mut s = fixture();
    for actor in [user(), agent(), Actor::Orchestrator { generation: 0 }] {
        assert_eq!(exec(&mut s.clone(), actor, Command::BindOrchestratorSession { session_id: "s1".into() }).unwrap_err().code, "forbidden");
    }
    assert_eq!(exec(&mut s.clone(), Actor::System, Command::BindOrchestratorSession { session_id: " ".into() }).unwrap_err().code, "invalid_input");
    exec(&mut s, Actor::System, Command::BindOrchestratorSession { session_id: "s1".into() }).unwrap();
    exec(&mut s, Actor::System, Command::BindOrchestratorSession { session_id: "s2".into() }).unwrap();
    assert_eq!(s.orchestrator_session, Some("s2".into()));
}
#[test]
fn a_typed_name_resolves_to_the_orchestrators_current_session_a_node_or_nobody() {
    let mut s = fixture();
    let node = s.agents.keys().next().unwrap().clone();
    assert_eq!(s.resolve_target("orchestrator"), Target::Orchestrator(None));
    exec(&mut s, Actor::System, Command::BindOrchestratorSession { session_id: "6f1c2b9e-1111-4a2b-9c3d-0123456789ab".into() }).unwrap();
    let session = Target::Orchestrator(Some("6f1c2b9e-1111-4a2b-9c3d-0123456789ab".into()));
    for name in ["orchestrator", "6f1c2b9e-1111-4a2b-9c3d-0123456789ab", "0123456789ab"] {
        assert_eq!(s.resolve_target(name), session, "{name}");
    }
    assert_eq!(s.resolve_target(&node.0), Target::Agent(node));
    for name in ["orquestador", "Orquestador", "Orchestrator", " orchestrator", "orchestrador", "orq", "", "ghost", "view"] {
        assert_eq!(s.resolve_target(name), Target::Unknown, "{name}");
    }
}

#[test]
fn preview_matches_execution_without_changing_the_input() {
    let state = fixture();
    let commands = [Command::CancelTask { task_id: "t".into() },
        Command::ReopenTask { task_id: "t".into() },
        Command::ArchiveTask { task_id: "t".into(), archived: true },
        Command::CancelTask { task_id: "missing".into() }];
    for actor in [user(), agent(), Actor::System, Actor::Orchestrator { generation: 0 }] {
        for command in &commands {
            for (protocol, epoch, revision) in [(PROTOCOL_VERSION, state.data_epoch.clone(), state.revision),
                (0, state.data_epoch.clone(), state.revision),
                (PROTOCOL_VERSION, "other-epoch".into(), state.revision),
                (PROTOCOL_VERSION, state.data_epoch.clone(), state.revision + 1)] {
                let envelope = CommandEnvelope { protocol_version: protocol, data_epoch: epoch,
                    command_id: "preview-check".into(), expected_revision: revision, command: command.clone() };
                let untouched = state.clone();
                let mut applied = state.clone();
                assert_eq!(state.preview(&actor, &envelope), applied.execute(&actor, &envelope));
                assert_eq!(state, untouched);
            }
        }
    }
}
