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
fn exec(s: &mut DomainState, a: Actor, c: Command) -> Result<CommandOutcome, DomainError> {
    s.execute(
        &a,
        &CommandEnvelope {
            protocol_version: 2,
            data_epoch: s.data_epoch.clone(),
            command_id: format!("c-{}", s.revision).into(),
            expected_revision: s.revision,
            command: c,
        },
    )
}
fn fixture() -> DomainState {
    let mut s = DomainState::new("epoch");
    exec(
        &mut s,
        Actor::System,
        Command::RegisterAgent {
            agent_id: "a".into(),
            name: "Instance A".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::CreateRequest {
            request_id: "r".into(),
            body: "Build requested result".into(), auto_merge: false,
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::CreateTask {
            task_id: "t".into(),
            request_id: "r".into(),
            title: "Result".into(),
            agent_id: Some("a".into()),
            review_prs: true,
            dependencies: BTreeSet::new(),
            stack: None,
        },
    )
    .unwrap();
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
        task_id: "scheduled-task".into(), agent_id: "a".into(),
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
    exec(
        &mut state,
        Actor::System,
        Command::RecordScheduleRunFailed {
            run_id: "run-1".into(),
            reason: "Launch failed".into(),
            finished_at: 102,
        },
    )
    .unwrap();
    let run = &state.schedule.runs[&ScheduleRunId::from("run-1")];
    assert_eq!(run.state, ScheduleRunState::Failed);
    assert_eq!(run.finished_at, Some(101));
    assert_eq!(run.request_id, Some("scheduled-request".into()));
    assert_eq!(run.task_id, Some("scheduled-task".into()));
    assert_eq!(state.schedule.schedules[&ScheduleId::from("schedule-1")].next_run, Some(200));
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
            is_final: true,
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
        agent(),
        Command::PublishInformation {
            attachments: vec![],
            interaction_id: "info".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Useful result".into(),
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
        agent(),
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
        },
    )
    .unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Running);
    exec(
        &mut s,
        Actor::System,
        Command::ObserveAgent {
            agent_id: "a".into(),
            connected: false,
        },
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
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
    reject_unchanged(
        &mut s,
        agent(),
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
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Closing);
    exec(
        &mut s,
        Actor::System,
        Command::RegisterAgent {
            agent_id: "b".into(),
            name: "Instance B".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::AssignTask {
            task_id: "t".into(),
            agent_id: "b".into(),
        },
    )
    .unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::InReview);
    merged_by_someone_else(&mut s);
    assert_eq!(s.tasks[&"t".into()].state, TaskState::InReview);
    reject_unchanged(
        &mut s,
        Actor::Agent {
            agent_id: "b".into(),
        },
        complete(),
        "acceptance_missing",
    );
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
        }],
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
            }],
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
    assert_eq!(s.tasks[&"t".into()].state, TaskState::NeedsYou);
    reject_unchanged(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "blocking_interaction",
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
    assert_eq!(out.effects.len(), 1);
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done, "answering the last blocker completes an accepted Task");
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
    reject_unchanged(&mut s, agent(), complete(), "merge_not_confirmed");
    merged_by_someone_else(&mut s);
    assert_eq!(s.pull_requests[&"pr".into()].state, PrState::Merged);
    exec(&mut s, agent(), complete()).unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
}
#[test]
fn an_observed_merge_of_a_task_that_did_not_ask_for_auto_merge_still_needs_the_users_acceptance() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    merged_by_someone_else(&mut s);
    reject_unchanged(&mut s, agent(), complete(), "acceptance_missing");
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
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "merge_not_confirmed",
    );
    exec(
        &mut s,
        agent(),
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
        agent(),
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
        agent(),
        Command::ClaimMergeAuthorization {
            authorization_id: "auth".into(),
            remote_head: "head2".into(),
            remote_base: "main".into(),
            base_verified: true,
        },
        "stale_head",
    );
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
    exec(&mut s, user(), Command::CreateRequest { request_id: "q".into(), body: "Is main green?".into(), auto_merge: false }).unwrap();
    reject_unchanged(&mut s, agent(), Command::CloseRequest { request_id: "q".into(), answer: Some("Yes".into()), withdrawn: false, title: None }, "forbidden");
    reject_unchanged(&mut s, orchestrator(), Command::CloseRequest { request_id: "q".into(), answer: Some(" ".into()), withdrawn: false, title: None }, "invalid_input");
    exec(&mut s, orchestrator(), Command::CloseRequest { request_id: "q".into(), answer: Some("Main is green".into()),
        withdrawn: false, title: Some("CI status".into()) }).unwrap();
    let closed = &s.requests[&"q".into()];
    assert!(closed.closed && !closed.withdrawn);
    assert_eq!((closed.answer.as_deref(), closed.title.as_deref()), (Some("Main is green"), Some("CI status")));
    exec(&mut s, user(), Command::CreateRequest { request_id: "w".into(), body: "Never mind".into(), auto_merge: false }).unwrap();
    exec(&mut s, user(), Command::CloseRequest { request_id: "w".into(), answer: None, withdrawn: true, title: None }).unwrap();
    assert!(s.requests[&"w".into()].withdrawn);
    reject_unchanged(&mut s, user(), Command::CloseRequest { request_id: "r".into(), answer: None, withdrawn: true, title: None }, "task_not_active");
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
        recipient: Recipient::User, body: "Ship?".into(), blocking: true, options: vec![option.clone()] }).unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::NeedsYou);
    reject_unchanged(&mut s, user(), Command::WithdrawQuestion { interaction_id: "q".into() }, "forbidden");
    reject_unchanged(&mut s, orchestrator(), Command::WithdrawQuestion { interaction_id: "q".into() }, "forbidden");
    exec(&mut s, agent(), Command::WithdrawQuestion { interaction_id: "q".into() }).unwrap();
    assert!(s.interactions[&"q".into()].withdrawn);
    assert_ne!(s.tasks[&"t".into()].state, TaskState::NeedsYou, "a withdrawn question no longer blocks");
    let action = s.projection().inbox.iter().flat_map(|r| std::iter::once(r).chain(&r.children))
        .find(|i| matches!(&i.subject, InboxSubject::Interaction { interaction_id } if interaction_id.0 == "q"))
        .map(|i| i.available_actions[0].clone());
    assert_eq!(action.map(|a| (a.enabled, a.disabled_reason)), Some((false, Some("withdrawn".into()))));
    reject_unchanged(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "q".into(), option_id: "yes".into(),
        inputs: BTreeMap::new() }, "withdrawn");
    exec(&mut s, agent(), Command::AskQuestion { attachments: vec![], interaction_id: "q2".into(), scope: Scope::Task { task_id: "t".into() },
        recipient: Recipient::User, body: "Again?".into(), blocking: false, options: vec![option] }).unwrap();
    exec(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "q2".into(), option_id: "yes".into(),
        inputs: BTreeMap::new() }).unwrap();
    reject_unchanged(&mut s, agent(), Command::WithdrawQuestion { interaction_id: "q2".into() }, "invalid_action_for_kind");
    exec(&mut s, orchestrator(), Command::AskQuestion { attachments: vec![], interaction_id: "oq".into(),
        scope: Scope::Orchestrator { request_id: Some("r".into()) }, recipient: Recipient::User, body: "Scope?".into(), blocking: false,
        options: vec![QuestionOption { id: "a".into(), label: "A".into(), description: None, inputs: vec![] }] }).unwrap();
    exec(&mut s, orchestrator(), Command::WithdrawQuestion { interaction_id: "oq".into() }).unwrap();
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
        agent(),
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
            name: "Instance B".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::CreateTask {
            task_id: "t2".into(),
            request_id: "r".into(),
            title: "Second task".into(),
            agent_id: Some("b".into()),
            review_prs: false,
            dependencies: BTreeSet::new(),
            stack: None,
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
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
}

#[test]
fn accepting_a_final_delivery_completes_the_task_without_asking_the_agent() {
    let mut state = fixture();
    present(&mut state, "final", text());
    let accepted = exec(&mut state, user(), Command::AcceptDelivery {
        presentation_id: "final".into(), targets: text(),
    }).unwrap();
    assert!(!accepted.effects.iter().any(|effect| matches!(effect, DomainEffect::NotifyAgent { .. })),
        "the Agent has nothing left to do");
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
    exec(&mut state, Actor::System, Command::ObserveAgent { agent_id: "a".into(), connected: true }).unwrap();
    assert_eq!(state.tasks[&"t".into()].state, TaskState::Done);
}
#[test]
fn cross_task_delivery_and_dependency_cycle_roll_back() {
    let mut s = fixture();
    present(&mut s, "d", text());
    exec(
        &mut s,
        user(),
        Command::CreateTask {
            task_id: "t2".into(),
            request_id: "r".into(),
            title: "Other".into(),
            agent_id: Some("a".into()),
            review_prs: false,
            dependencies: BTreeSet::from(["t".into()]),
            stack: None,
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
        agent(),
        Command::CompleteTask {
            task_id: "t2".into(),
            presentation_id: "d".into(),
        },
        "forbidden",
    );
    reject_unchanged(
        &mut s,
        agent(),
        Command::StartTask {
            task_id: "t2".into(),
        },
        "dependency_not_done",
    );
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
        "not_assigned",
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
            }],
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
    assert_eq!(p.inbox[0].id, "pr:pr");
    assert_eq!(p.inbox[0].children[0].id, "interaction:q");
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
        agent(),
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
            body: "Information with Unicode: revisión ✓".into(),
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
            }],
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
            }],
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
    present(&mut s, "d", pr_targets());
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
    if let Ok(path) = std::env::var("ALONDRA_DOMAIN_V2_FIXTURE_OUTPUT") {
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
            }],
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
            }],
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
            name: "Instance B".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::AssignTask {
            task_id: "t".into(),
            agent_id: "b".into(),
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
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
        Actor::Agent {
            agent_id: "b".into(),
        },
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "acceptance_missing",
    );
}

#[test]
fn review_off_does_not_authorize_and_external_merge_does_not_complete() {
    let mut s = fixture();
    s.tasks.get_mut(&"t".into()).unwrap().review_prs = false;
    register(&mut s);
    present(&mut s, "d", pr_targets());
    assert!(s.pull_requests[&"pr".into()].review.is_none());
    reject_unchanged(
        &mut s,
        user(),
        Command::AcceptDelivery {
            presentation_id: "d".into(),
            targets: pr_targets(),
        },
        "acceptance_missing",
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
    reject_unchanged(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "acceptance_missing",
    );
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
            is_final: false,
        },
    )
    .unwrap();
    exec(
        &mut s,
        agent(),
        Command::PublishInformation {
            attachments: vec![],
            interaction_id: "info".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Unread news".into(),
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
            options: vec![],
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
            agent(),
            Command::PublishInformation {
                attachments: vec![too_big],
                interaction_id: "bad".into(),
                scope: Scope::Task {
                    task_id: "t".into()
                },
                recipient: Recipient::User,
                body: String::new()
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
            }],
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
    present(&mut s, "d", pr_targets());
    let stack = StackDependency {
        upstream_task: "t".into(),
        presentation_id: "d".into(),
        pr_id: "pr".into(),
        head: "head1".into(),
    };
    exec(
        &mut s,
        user(),
        Command::CreateTask {
            task_id: "down".into(),
            request_id: "r".into(),
            title: "Downstream".into(),
            agent_id: Some("a".into()),
            review_prs: true,
            dependencies: BTreeSet::new(),
            stack: Some(stack),
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        agent(),
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
        agent(),
        Command::StartTask {
            task_id: "down".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        agent(),
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
        agent(),
        Command::PresentResult {
            attachments: vec![],
            task_id: "down".into(),
            interaction_id: "d-down".into(),
            body: "Downstream ready".into(),
            targets: vec![DeliveryTarget::PullRequest {
                pr_id: "pr-down".into(),
                head: "head-down".into(),
            }],
            is_final: true,
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
    reject_unchanged(&mut s, agent(), claim.clone(), "stack_not_ready");
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
    reject_unchanged(&mut s, agent(), claim, "stack_not_ready");
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
        agent(),
        Command::ClaimMergeAuthorization {
            authorization_id: "auth-down".into(),
            remote_head: "head-down".into(),
            remote_base: "main".into(),
            base_verified: true,
        },
    )
    .unwrap();
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
        agent(),
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
        agent(),
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
        agent(),
        Command::PublishInformation {
            attachments: vec![],
            interaction_id: "i".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Read once".into(),
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
            agent(),
            Command::CompleteTask {
                task_id: "t".into(),
                presentation_id: "d".into(),
            },
        )
    };
    // The merge authorization covers the PR and the text, never the document.
    authorize(&mut s);
    assert!(!accepted(&s));
    assert_eq!(s.tasks[&"t".into()].state, TaskState::InReview);
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
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Closing);
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
fn complete_task_requires_registered_pr_merged_and_presented_or_closed() {
    let mut s = fixture();
    register(&mut s);
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
    reject_unchanged(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "merge_not_confirmed",
    );
    let mut merged_unpresented = s.clone();
    exec(
        &mut merged_unpresented,
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
    reject_unchanged(
        &mut merged_unpresented,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "acceptance_missing",
    );
    exec(
        &mut s,
        Actor::System,
        Command::ObservePrState {
            pr_id: "pr".into(),
            head: "head1".into(),
            base: "main".into(),
            state: PrState::Closed,
            merged_head: None,
        },
    )
    .unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done, "closing the unpresented PR completes the accepted Task");
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
            options: vec![],
        },
    )
    .unwrap();
    exec(
        &mut s,
        Actor::System,
        Command::RegisterAgent {
            agent_id: "b".into(),
            name: "Instance B".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::AssignTask {
            task_id: "t".into(),
            agent_id: "b".into(),
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
        agent(),
        Command::PublishInformation {
            attachments: vec![],
            interaction_id: "info".into(),
            scope: Scope::Task { task_id: "t".into() },
            recipient: Recipient::User,
            body: "Progress".into(),
        },
    )
    .unwrap();
    present(&mut s, "d", text());
    exec(&mut s, user(), Command::AcceptDelivery { presentation_id: "d".into(), targets: text() }).unwrap();
    assert_eq!(s.projection().counts.unread_information, 1);
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
        body: "Progress".into(),
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
            options: vec![],
        },
    )
    .unwrap();
    let seen = s.revision;
    exec(&mut s, agent(), info("i1")).unwrap();
    assert!(s.revision > seen);
    // A snapshot older than unrelated information still decides this question once.
    run(&mut s, user(), "a1", seen, answer("Blue")).unwrap();
    let lost = run(&mut s, user(), "a2", seen, answer("Red")).unwrap_err();
    assert_eq!(lost.code, "stale_revision");
    assert_eq!(lost.current_revision, Some(s.revision));
    let future = s.revision + 1;
    assert_eq!(
        run(&mut s, agent(), "future", future, info("i2")).unwrap_err().code,
        "stale_revision"
    );
    // Information neither blocks nor dates a Task decision.
    let seen = s.revision;
    exec(&mut s, agent(), info("i3")).unwrap();
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
        exec(&mut s, Actor::System, Command::RegisterAgent { agent_id: id.into(), name: id.into() }).unwrap();
    }
    let seen = s.revision;
    run(&mut s, user(), "assign-b", seen, Command::AssignTask { task_id: "t".into(), agent_id: "b".into() }).unwrap();
    assert_eq!(
        run(&mut s, user(), "assign-c", seen, Command::AssignTask { task_id: "t".into(), agent_id: "c".into() })
            .unwrap_err()
            .code,
        "stale_revision"
    );
    assert_eq!(s.tasks[&"t".into()].agent_id, Some("b".into()));
    // Commands without a narrower subject still need the current revision.
    assert_eq!(
        run(&mut s, user(), "r2", seen, Command::CreateRequest { request_id: "r2".into(), body: "More".into(), auto_merge: false })
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
        Command::ObserveAgent { .. } => 12,
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
        Command::WithdrawQuestion { .. } => 42,
        Command::HoldHook { .. } => 43,
    }
}
const COMMAND_KINDS: usize = 44;
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
        (Command::InterruptScheduleRunsOnRestart, true),
        (Command::CreateRequest { request_id: ids.request.into(), body: "Work".into(), auto_merge: false }, true),
        (Command::CloseRequest { request_id: ids.request.into(), answer: None, withdrawn: false, title: None }, false),
        (Command::RegisterAgent { agent_id: ids.agent.into(), name: "Ghost".into() }, true),
        (Command::ObserveAgent { agent_id: ids.agent.into(), connected: true }, false),
        (Command::AdvanceOrchestratorGeneration { generation: 1 }, true),
        (Command::CreateTask { task_id: "fresh-task".into(), request_id: ids.request.into(),
            title: "Fresh".into(), agent_id: Some(ids.agent.into()), review_prs: false,
            dependencies: BTreeSet::new(), stack: None }, false),
        (Command::AssignTask { task_id: ids.task.into(), agent_id: ids.agent.into() }, false),
        (Command::RetitleTask { task_id: ids.task.into(), title: "Renamed".into() }, false),
        (Command::SetDependencies { task_id: ids.task.into(), dependencies: BTreeSet::new(), stack: None }, false),
        (Command::StartTask { task_id: ids.task.into() }, false),
        (Command::PublishInformation { attachments: vec![], interaction_id: "fresh-info".into(),
            scope: scope.clone(), recipient: Recipient::User, body: "Info".into() }, false),
        (Command::AskQuestion { attachments: vec![], interaction_id: "fresh-question".into(),
            scope: scope.clone(), recipient: Recipient::User, body: "Q?".into(), blocking: true,
            options: vec![option.clone()] }, false),
        (Command::RequestAction { attachments: vec![], interaction_id: "fresh-action".into(),
            scope, recipient: Recipient::User, body: "Do".into(), blocking: true,
            options: vec![option] }, false),
        (Command::MarkRead { interaction_id: ids.interaction.into() }, false),
        (Command::WithdrawQuestion { interaction_id: ids.interaction.into() }, false),
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
            is_final: true }, false),
        (Command::SubmitFeedback { attachments: vec![], presentation_id: ids.interaction.into(),
            targets: text(), body: "Change".into() }, false),
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
            options: Hold::Permission.options() }, false),
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

    exec(&mut s, Actor::System, Command::RegisterAgent { agent_id: "b".into(), name: "Instance B".into() }).unwrap();
    exec(&mut s, user(), Command::CreateTask { task_id: "t2".into(), request_id: "r".into(),
        title: "Other".into(), agent_id: Some("b".into()), review_prs: true,
        dependencies: BTreeSet::new(), stack: None }).unwrap();
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
        is_final: true }).unwrap();
    exec(&mut s, b.clone(), Command::AskQuestion { attachments: vec![], interaction_id: "q2".into(),
        scope: Scope::Task { task_id: "t2".into() }, recipient: Recipient::User, body: "Q?".into(),
        blocking: false, options: vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None, inputs: vec![] }] }).unwrap();
    exec(&mut s, user(), Command::AuthorizeMerge { authorization_id: "auth2".into(),
        presentation_id: "pres2".into(), pr_id: "pr2".into(), expected_head: "head1".into(),
        method: MergeMethod::Squash }).unwrap();
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
            assert!(matches!(error.code.as_str(), "not_assigned" | "forbidden"), "{command:?}: {}", error.code);
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
fn external_merge_requires_explicit_delivery_acceptance_not_retroactive_authorization() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
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
    reject_unchanged(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "acceptance_missing",
    );
    reject_unchanged(
        &mut s,
        user(),
        Command::AuthorizeMerge {
            authorization_id: "auth".into(),
            presentation_id: "d".into(),
            pr_id: "pr".into(),
            expected_head: "head1".into(),
            method: MergeMethod::Squash,
        },
        "invalid_action_for_kind",
    );
    exec(
        &mut s,
        user(),
        Command::AcceptDelivery {
            presentation_id: "d".into(),
            targets: pr_targets(),
        },
    )
    .unwrap();
    assert!(s.merge_authorizations.is_empty());
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
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
    exec(&mut state, agent(), Command::PublishInformation {
        attachments: vec![], interaction_id: "i".into(),
        scope: Scope::Task { task_id: "scheduled-task".into() }, recipient: Recipient::User, body: "Started".into(),
    }).unwrap();
    state.validate().unwrap();
}

#[test]
fn a_held_hook_question_reaches_the_user_and_its_answer_never_enters_the_outbox() {
    let mut s = fixture();
    let held = |id: &str, hold: Hold| Command::HoldHook { interaction_id: id.into(), task_id: "t".into(),
        agent_id: "a".into(), hold, body: "Bash: rm -rf build".into(), options: hold.options() };
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
    exec(&mut s, Actor::System, Command::WithdrawQuestion { interaction_id: "p".into() }).unwrap();
    assert!(s.interactions[&"p".into()].withdrawn);
    exec(&mut s, agent(), Command::AskQuestion { attachments: vec![], interaction_id: "q".into(),
        scope: Scope::Task { task_id: "t".into() }, recipient: Recipient::User, body: "Which?".into(),
        blocking: true, options: Hold::Permission.options() }).unwrap();
    reject_unchanged(&mut s, Actor::System, Command::WithdrawQuestion { interaction_id: "q".into() }, "forbidden");
    let ordinary = exec(&mut s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: "q".into(),
        option_id: Hold::ALLOW.into(), inputs: BTreeMap::new() }).unwrap();
    assert!(matches!(ordinary.effects[..], [DomainEffect::NotifyAgent { .. }]));
}
