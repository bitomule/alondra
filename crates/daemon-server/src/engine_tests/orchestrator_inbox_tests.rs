//! The orchestrator's inbox over the engine's and the SDK's own fixtures: what it hands back, reminds and settles
//! when a dependency is cancelled, a start refuses a plan, a node stops or a decision holds plans.

/// A batch of the orchestrator read every message waiting for it and ended with its result: what
/// it owes is settled.
fn turn_ends(domain: &alondra_engine_kernel::domain_runtime::DomainRuntime) {
    {
        let mut store = domain.store.lock().unwrap();
        for message in store.pending_messages().unwrap().into_iter().filter(|message| message.recipient_kind == "orchestrator") {
            if let Some(claim) = store.claim_message(message.id).unwrap() {
                store.finish_message(claim.attempt_id, claim.version, alondra_core::delivery::State::Handed, None).unwrap();
            }
        }
    }
    alondra_engine_orchestrator::orchestrator_inbox::settle(domain);
}

/// A task that waits for a cancelled one is the orchestrator's to re-plan, handed back at every rest.
mod cancelled_dependencies {
    use alondra_domain_model::{Actor, Command, TaskId, TaskState};
    use alondra_engine_sdk::auto_launch::pass;
    use crate::engine_tests::engine_fixtures::launches::{Fixture, command, create, launches, open_task, plan, problem, told, user, world};

    /// The orchestrator ended a turn: what it still has to re-plan is handed to it again.
    fn rest(f: &Fixture) {
        f.domain.orchestrator_rests.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        alondra_engine_orchestrator::orchestrator_inbox::hand_replans(&f.domain);
    }

    /// 20 October: a cancelled dependency put a card in front of the user at once. The task never
    /// starts while it waits for a cancelled task; it is the orchestrator's to decide, and it is
    /// handed back at the orchestrator's rests, up to its last reminder, never to the user. Letting it start without the
    /// cancelled task is the orchestrator's decision.
    #[tokio::test]
    async fn a_cancelled_dependency_is_the_orchestrators_to_decide_and_never_the_users() {
        super::super::wired();
        let f = world();
        open_task(&f, "f0");
        create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
        command(&f.domain, user(), Command::CancelTask { task_id: "f0".into(), reason: None });
        pass(&*f.domain, &f.home).await.unwrap();
        pass(&*f.domain, &f.home).await.unwrap();
        assert_eq!(launches(&f), 0);
        assert!(problem(&f, "f1").is_none(), "the orchestrator re-plans it first");

        alondra_engine_orchestrator::orchestrator_inbox::hand_replans(&f.domain);
        let notices = || told(&f).into_iter().filter(|id| id.starts_with("dependency-cancelled-f1--f0-")).collect::<Vec<_>>();
        assert_eq!(notices(), ["dependency-cancelled-f1--f0-0"]);
        let state = f.domain.state().unwrap();
        let notice = &state.interactions[&"dependency-cancelled-f1--f0-0".into()].body;
        assert!(notice.contains("“Task f1” (f1) waits for “Task f0” (f0), which was cancelled") && notice.contains("set_dependencies")
            && notice.contains("cancel_task") && notice.contains("yours to decide"), "{notice}");
        alondra_engine_orchestrator::orchestrator_inbox::hand_replans(&f.domain);
        assert_eq!(notices().len(), 1, "once per rest");
        for _ in 0..5 {
            rest(&f);
        }
        assert_eq!(notices().len(), alondra_engine_orchestrator::orchestrator_inbox::REPLAN_REMINDERS, "one per rest, up to the last reminder");
        assert!(problem(&f, "f1").is_none(), "the user is never asked");
        pass(&*f.domain, &f.home).await.unwrap();
        assert_eq!(launches(&f), 0);

        command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::SetDependencies { task_id: "f1".into(), dependencies: Default::default(), stack: None, reason: None });
        pass(&*f.domain, &f.home).await.unwrap();
        assert_eq!(launches(&f), 1, "the orchestrator let it start without the cancelled task");
    }

    /// The orchestrator replaces the cancelled task: the task follows its new dependencies, nobody is
    /// asked, and a card from before that asked the user is withdrawn.
    #[tokio::test]
    async fn a_task_the_orchestrator_re_points_follows_its_replacement_and_its_old_card_is_withdrawn() {
        super::super::wired();
        let f = world();
        open_task(&f, "f0");
        open_task(&f, "f2");
        create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
        command(&f.domain, user(), Command::CancelTask { task_id: "f0".into(), reason: None });
        command(&f.domain, Actor::System, Command::RaiseStartProblem { task_id: "f1".into(), problem: alondra_domain_model::StartProblem::DependencyCancelled { dependency: "f0".into() } });
        let card = problem(&f, "f1").expect("a card from before the orchestrator decided");
        command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::SetDependencies {
            task_id: "f1".into(), dependencies: ["f2".into()].into(), stack: None, reason: None,});
        rest(&f);
        assert!(f.domain.state().unwrap().interactions[&card.id].withdrawn, "the card no longer asks anything");
        assert!(problem(&f, "f1").is_none());
        pass(&*f.domain, &f.home).await.unwrap();
        assert_eq!(launches(&f), 0, "it waits for its replacement");
        assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("f1")].state, TaskState::WaitingDependency);
        let before = told(&f).len();
        rest(&f);
        assert_eq!(told(&f).len(), before, "nothing more for the orchestrator or the user");
    }

    /// The orchestrator cancels a task and re-points the one that waited for it in the same batch:
    /// nobody is told anything.
    #[tokio::test]
    async fn cancelling_and_re_pointing_in_the_same_batch_tells_nobody() {
        super::super::wired();
        let f = world();
        open_task(&f, "f0");
        open_task(&f, "f2");
        create(&f, "f1", &["f0"], Some(&plan(&f, "app/f1")));
        command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CancelTask { task_id: "f0".into(), reason: None });
        command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::SetDependencies {
            task_id: "f1".into(), dependencies: ["f2".into()].into(), stack: None, reason: None,});
        rest(&f);
        assert!(!told(&f).iter().any(|id| id.starts_with("dependency-cancelled-")));
        assert!(problem(&f, "f1").is_none());
    }

    #[tokio::test]
    async fn a_delivery_dependency_cancelled_stays_with_the_orchestrator_and_its_cancel_closes_the_task() {
        super::super::wired();
        let f = world();
        open_task(&f, "a");
        create(&f, "after", &[], Some(&plan(&f, "app/after")));
        command(&f.domain, user(), Command::SetRelations { task_id: "after".into(), delivered_dependencies: ["a".into()].into(), blocks: Default::default() });
        command(&f.domain, user(), Command::CancelTask { task_id: "a".into(), reason: None });
        pass(&*f.domain, &f.home).await.unwrap();
        assert!(problem(&f, "after").is_none());
        for _ in 0..4 {
            rest(&f);
        }
        assert!(problem(&f, "after").is_none(), "the user is never asked");
        command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CancelTask { task_id: "after".into(), reason: None });
        assert_eq!(f.domain.state().unwrap().tasks[&TaskId::from("after")].state, TaskState::Cancelled);
        pass(&*f.domain, &f.home).await.unwrap();
        assert_eq!(launches(&f), 0);
    }
}

/// A launch plan the start refused stays the orchestrator's until it decides.
mod refused_plans {
    use alondra_domain_model::TaskId;
    use crate::engine_tests::engine_fixtures::orchestrator_tools::{refuse_its_start, refused_plan, world};
    use alondra_store_sqlite::AsSqlite as _;

    #[tokio::test]
    async fn a_refused_plan_the_orchestrator_leaves_is_handed_back_at_every_rest_and_never_cancelled_for_it() {
        super::super::wired();
        let f = world();
        let plan = refused_plan(&f);
        f.domain.store.lock().unwrap().sqlite_mut().set_launch_plan(&"t".into(), &plan, 1).unwrap();
        refuse_its_start(&f).await;
        let reminders = || f.domain.state().unwrap().interactions.values()
            .filter(|i| i.id.0.starts_with(alondra_domain_model::orchestrator_cards::PLAN_REFUSED_NOTICE_PREFIX)).count();
        alondra_engine_orchestrator::orchestrator_inbox::hand_replans(&f.domain);
        alondra_engine_orchestrator::orchestrator_inbox::hand_replans(&f.domain);
        assert_eq!(reminders(), 1, "once per rest");
        for _ in 0..3 {
            f.domain.orchestrator_rests.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            alondra_engine_orchestrator::orchestrator_inbox::hand_replans(&f.domain);
        }
        assert_eq!(reminders(), alondra_engine_orchestrator::orchestrator_inbox::REPLAN_REMINDERS, "one per rest, up to the last reminder");
        for hour in 0..30 {
            alondra_engine_orchestrator::orchestrator_inbox::settle_overdue(&f.domain, hour * 3_600_000);
        }
        let state = f.domain.state().unwrap();
        assert!(!state.tasks[&TaskId::from("t")].cancelled(), "no clock cancels it for the orchestrator");
        assert!(state.open_plan_refusal(&"t".into()).is_some(), "it stays open until the orchestrator decides");
        let reminder = state.interactions.values().find(|i| i.id.0.ends_with("-2") && i.id.0.starts_with(alondra_domain_model::orchestrator_cards::PLAN_REFUSED_NOTICE_PREFIX)).unwrap();
        assert_eq!(reminder.recipient, alondra_domain_model::Recipient::Orchestrator);
        assert!(reminder.body.starts_with("Still open, reminder 2:"), "{}", reminder.body);
        assert!(state.interactions.values().all(|i| i.recipient != alondra_domain_model::Recipient::User
            || !matches!(&i.scope, alondra_domain_model::Scope::Task { task_id } if task_id.0 == "t")), "the user is never asked");
    }
}

/// A stopped node's case is reminded to the orchestrator when a turn of it ends, and settled by
/// Alondra when the next one ends.
mod stopped_nodes {
    use alondra_domain_model::*;
    use crate::engine_tests::owner_fixtures::*;
    use std::sync::Arc;

    /// A stopped node's case the orchestrator leaves unsettled when a turn of it ends comes back to
    /// it once, and when the next turn ends too Alondra hands the node its turn back itself; the
    /// count of stops, the case and the reminder outlive a restart, the clock decides nothing, and no
    /// card ever asks the user.
    #[test]
    fn a_stop_left_unsettled_is_reminded_once_then_handed_back_and_both_outlive_a_restart() {
        super::super::wired();
        use alondra_domain_model::{STALL_ATTEMPTS, UNSETTLED_INSTRUCT};
        use alondra_domain_model::orchestrator_cards::STALL_NOTICE_PREFIX;
        const HOUR: i64 = 3_600_000;
        let (fixture, mut store, execution) = running();
        let normalizer = woken(&mut store, &execution);
        let runtime = runtime(&fixture, store);
        let state = Arc::clone(runtime.read().state());
        runtime.own(scope_of(&state), |owner| owner.stage_observation(&binding(), 1, &batch(&execution, WOKEN_CURSOR), &normalizer, no_home(), false)).unwrap();
        let case = stall_case_of("turn-2");
        alondra_engine_orchestrator::orchestrator_inbox::settle_overdue(&runtime, 24 * HOUR);
        assert!(!runtime.state().unwrap().interactions.keys().any(|id| id.0.starts_with(STALL_NOTICE_PREFIX)), "the clock reminds of nothing");
        super::turn_ends(&runtime);
        let state = runtime.state().unwrap();
        assert!(state.interactions.contains_key(&InteractionId::from(format!("{STALL_NOTICE_PREFIX}{case}-1").as_str())), "the orchestrator is reminded");
        assert!(state.open(&state.interactions[&case]) && !state.tasks[&task()].over(), "it gets one more go first");
        assert_eq!(state.tasks[&task()].stalls, 1);
        assert!(STALL_ATTEMPTS > state.tasks[&task()].stalls);
        let runtime = runtime_reopened(&fixture, runtime);
        let state = runtime.state().unwrap();
        assert_eq!(state.tasks[&task()].stalls, 1, "the count outlives the restart");
        assert!(state.open(&state.interactions[&case]), "and so does the case");
        alondra_engine_orchestrator::orchestrator_inbox::settle_overdue(&runtime, 48 * HOUR);
        assert!(runtime.state().unwrap().open(&runtime.state().unwrap().interactions[&case]), "the clock settles nothing");
        super::turn_ends(&runtime);
        let runtime = runtime_reopened(&fixture, runtime);
        let state = runtime.state().unwrap();
        assert!(!state.open(&state.interactions[&case]), "Alondra settled it, and that outlives the restart");
        assert!(matches!(state.lifecycle_journal[&task()].iter().rev().find(|record| matches!(record, LifecycleRecord::Answered { .. })),
            Some(LifecycleRecord::Answered { text: Some(text), .. }) if text == UNSETTLED_INSTRUCT), "the node is told to go on");
        assert!(!state.tasks[&task()].over());
        let observer = observer(&fixture);
        let asked: i64 = observer.query_row("SELECT count(*) FROM interactions WHERE task_id=?1 AND recipient_kind='user' AND (id LIKE 'turn-ended-%' OR id LIKE 'stalled-%' OR id LIKE 'problem-%')",
            [TASK], |r| r.get(0)).unwrap();
        assert_eq!(asked, 0, "no card ever asked the user");
    }

    /// The orchestrator's instruction settles the case for good: no reminder follows it.
    #[test]
    fn a_stop_the_orchestrator_settles_gets_no_reminder() {
        super::super::wired();
        use alondra_domain_model::orchestrator_cards::STALL_NOTICE_PREFIX;
        let (fixture, mut store, execution) = running();
        let normalizer = woken(&mut store, &execution);
        let runtime = runtime(&fixture, store);
        let state = Arc::clone(runtime.read().state());
        runtime.own(scope_of(&state), |owner| owner.stage_observation(&binding(), 1, &batch(&execution, WOKEN_CURSOR), &normalizer, no_home(), false)).unwrap();
        let state = Arc::clone(runtime.read().state());
        let settle = Command::SettleStall { case_id: stall_case_of("turn-2"), task_id: task(), outcome: alondra_domain_model::StallOutcome::Instruct { text: "Present it".into() } };
        let fresh = CommandEnvelope { expected_revision: state.revision, ..envelope(&state, "orchestrator-instructs", settle) };
        runtime.execute(&Actor::Orchestrator { generation: state.orchestrator_generation }, &fresh, None, || Ok(())).unwrap();
        super::turn_ends(&runtime);
        assert!(!runtime.state().unwrap().interactions.keys().any(|id| id.0.starts_with(STALL_NOTICE_PREFIX)));
    }
}

/// Plans a decision of the user holds are reminded when a turn of the orchestrator ends, then
/// released unchanged with a note when the next one ends.
mod held_plans {
    use alondra_domain_model::{PlanReviewOutcome, Recipient, TaskId};
    use alondra_domain_model::orchestrator_cards::PLAN_REVIEW_NOTICE_PREFIX;
    use alondra_daemon_engine::plan_reviews::held;
    use alondra_daemon_engine::plan_reviews::fixtures::{plan, redraft, settle};
    use alondra_engine_kernel::domain_runtime::DomainRuntime;

    fn reminders(domain: &DomainRuntime) -> usize {
        domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with(PLAN_REVIEW_NOTICE_PREFIX)).count()
    }

    const HOUR: i64 = 3_600_000;

    /// A review the orchestrator asked the user about waits for that answer: no reminder, no
    /// release, however many of its batches end.
    #[test]
    fn a_review_the_orchestrator_asked_the_user_about_waits_for_the_answer() {
        super::super::wired();
        let (_home, domain) = redraft();
        settle(&domain, "c-same", "c", PlanReviewOutcome::Unchanged { reason: "Phase C does not depend on the print size".into() }).unwrap();
        let state = domain.state().unwrap();
        let request = state.tasks[&TaskId::from("d")].request_id.clone();
        domain.execute(&alondra_domain_model::Actor::Orchestrator { generation: state.orchestrator_generation }, &alondra_domain_model::CommandEnvelope {
            protocol_version: alondra_domain_model::PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "ask-about-d".into(),
            expected_revision: state.revision, command: alondra_domain_model::Command::AskQuestion {
                attachments: vec![], interaction_id: "q-print".into(), scope: alondra_domain_model::Scope::Orchestrator { request_id: Some(request) },
                recipient: Recipient::User, body: "Should phase D keep the 360 px prints?".into(), blocking: false,
                options: vec![alondra_domain_model::QuestionOption { id: "keep".into(), label: "Keep".into(), description: None },
                    alondra_domain_model::QuestionOption { id: "change".into(), label: "Change".into(), description: None }],
                card: alondra_domain_model::CardSpec::derived(alondra_domain_model::CardKind::Decide, "Choose the print size of phase D"),
            } }, None, || Ok(())).unwrap();
        for _ in 0..3 {
            super::turn_ends(&domain);
        }
        assert_eq!(reminders(&domain), 0, "nothing reminds the orchestrator while the user decides");
        assert!(held(&domain.state().unwrap(), &"d".into()).is_some(), "the plan waits for the user's answer");
    }

    #[test]
    fn an_unreviewed_plan_is_released_unchanged_and_its_node_is_told() {
        super::super::wired();
        let (_home, domain) = redraft();
        let queued = alondra_daemon_engine_model::LaunchBody::from_plan("d".into(), &plan("d", "Phase D: 360 px everywhere")).to_queue();
        domain.store.lock().unwrap().enqueue_launch(&"d".into(), &queued, "auto", 1).unwrap();
        settle(&domain, "c-same", "c", PlanReviewOutcome::Unchanged { reason: "Phase C does not depend on the print size".into() }).unwrap();
        alondra_engine_orchestrator::orchestrator_inbox::settle_overdue(&domain, 24 * HOUR);
        assert_eq!(reminders(&domain), 0, "the clock reminds of nothing");
        super::turn_ends(&domain);
        assert_eq!(reminders(&domain), 1, "reminded when the turn ended with them unreviewed");
        let reminder = domain.state().unwrap().interactions.values().find(|i| i.id.0.starts_with(PLAN_REVIEW_NOTICE_PREFIX)).unwrap().body.clone();
        assert!(reminder.contains("Phase D: 360 px everywhere") && !reminder.contains("Phase C: group"), "only what is still held: {reminder}");
        assert!(held(&domain.state().unwrap(), &"d".into()).is_some(), "one more go for the orchestrator first");
        super::turn_ends(&domain);
        let state = domain.state().unwrap();
        assert!(held(&state, &"d".into()).is_none() && held(&state, &"e".into()).is_none(), "released");
        assert!(matches!(state.plan_reviews[&"decision-1".into()].dependents[&TaskId::from("d")].as_ref().unwrap().outcome, PlanReviewOutcome::Unchanged { .. }));
        let note = "The user decided “Decide the index's next step: Alternative plan: 512 px prints” on “Redraft index · Phase B” after your plan was written: check that your plan still holds before you start.";
        assert_eq!(domain.launch_plan(&"d".into()).unwrap().unwrap().mission.as_deref(), Some(format!("{note}\n\nPhase D: 360 px everywhere").as_str()));
        assert!(domain.launch_plan(&"e".into()).unwrap().unwrap().mission.unwrap().starts_with(note));
        assert_eq!(domain.launch_plan(&"c".into()).unwrap().unwrap().mission.as_deref(), Some("Phase C: group with prints of the 360 px copy"), "what the orchestrator reviewed is left as it is");
        let queue = domain.store.lock().unwrap().launch_queue().unwrap();
        let body = alondra_daemon_engine_model::LaunchBody::from_queue(&queue.iter().find(|row| row.task_id == "d".into()).unwrap().body).unwrap();
        assert!(body.to_queue()["body"]["prompt"].as_str().unwrap().starts_with(note), "its first input carries the note");
        assert!(state.interactions.values().all(|i| i.id.0 == "decision-1" || i.recipient != Recipient::User), "no card for the user");
    }
}

/// No clock reminds the orchestrator, during the usage limit or after it: only a turn that ends.
mod usage_hold {
    use alondra_daemon_engine::live::LiveOf as _;
    use alondra_domain_model::*;
    use alondra_domain_model::orchestrator_cards::STALL_NOTICE_PREFIX;
    use crate::domain_runtime::DomainRuntime;

    const MINUTE: i64 = 60_000;

    fn system(domain: &DomainRuntime, command: Command) {
        let state = domain.state().unwrap();
        let shown = format!("{command:?}");
        domain.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("c-{}", state.revision).into(),
            expected_revision: state.revision, command,
        }).unwrap_or_else(|error| panic!("{shown} was refused: {error:?}"));
    }

    fn as_actor(domain: &DomainRuntime, actor: Actor, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("c-{}", state.revision).into(),
            expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    fn task() -> TaskId {
        "t".into()
    }

    fn open_case(domain: &DomainRuntime) -> Option<InteractionId> {
        let state = domain.state().unwrap();
        state.interactions.values().find(|i| is_stall_case(i) && state.open(i)).map(|i| i.id.clone())
    }

    /// The node of task `t` starts each turn with what it was told last and stops with nothing,
    /// until a case about it is open.
    fn stop_until_a_case(domain: &DomainRuntime) -> InteractionId {
        for n in 0.. {
            if let Some(case) = open_case(domain) {
                return case;
            }
            let state = domain.state().unwrap();
            let turn = format!("turn-{}-{n}", state.revision);
            let input = state.lifecycle_of(&task()).unwrap().pending_inputs().first().map(|input| input.as_str().to_owned()).expect("something to start the turn with");
            system(domain, Command::RecordRuntimeFacts { task_id: task(), generation: 1, facts: vec![RuntimeObservation::TurnStarted { turn: turn.clone(), input }] });
            system(domain, Command::RecordTurnBoundary { task_id: task(), generation: 1, turn, failure: None, failed: None });
        }
        unreachable!()
    }

    /// Task `t`, whose node runs a turn through the SDK and stops until its first case is open.
    fn stalled(domain: &DomainRuntime) -> InteractionId {
        system(domain, Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
        as_actor(domain, Actor::User { user_id: "u".into() }, Command::CreateRequest {
            request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
        });
        as_actor(domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { priority: Default::default(),
            plan_items: None, continues: None, task_id: task(), request_id: "r".into(), title: "Task".into(), agent_id: None, review_prs: false,
            dependencies: Default::default(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
        });
        crate::domain_store::admit_through_sdk(domain, &task(), "a");
        system(domain, Command::RecordRuntimeFacts { task_id: task(), generation: 1, facts: vec![RuntimeObservation::ResourceAvailable] });
        stop_until_a_case(domain)
    }

    fn reminded(domain: &DomainRuntime, case: &InteractionId) -> bool {
        domain.state().unwrap().interactions.contains_key(&InteractionId::from(format!("{STALL_NOTICE_PREFIX}{case}-1").as_str()))
    }

    #[test]
    fn no_clock_reminds_the_orchestrator_during_the_usage_limit_or_after_it() {
        super::super::wired();
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = alondra_daemon_engine::wiring::open(&home).unwrap();
        let case = stalled(&domain);
        domain.live().usage_pause.exhaust(Some(2 * 60 * MINUTE), 0);
        for minutes in (0..=60).step_by(5) {
            alondra_engine_orchestrator::orchestrator_inbox::settle_overdue(&domain, minutes * MINUTE);
        }
        assert!(!reminded(&domain, &case), "an hour held by the usage limit counts for nothing");
        domain.live().usage_pause.resume(60 * MINUTE);
        for minutes in [61, 71, 24 * 60] {
            alondra_engine_orchestrator::orchestrator_inbox::settle_overdue(&domain, minutes * MINUTE);
        }
        assert!(!reminded(&domain, &case), "nor a day after it");
        assert!(domain.state().unwrap().open(&domain.state().unwrap().interactions[&case]));
        super::turn_ends(&domain);
        assert!(reminded(&domain, &case), "a turn of the orchestrator that ends does");
    }
}
