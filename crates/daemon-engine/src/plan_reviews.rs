//! What the orchestrator reads when a decision of the user holds the tasks that wait for the one
//! decided on, and why a launch of one of them is refused until it reviews their plans.
pub(crate) use crate::domain_store::wording::orchestrator_text;
use alondra_core::domain::{DomainState, TaskId};

/// Why a held task does not start now, or nothing if no decision holds it.
pub(crate) fn held(state: &DomainState, task: &TaskId) -> Option<String> {
    let (_, review) = state.plan_reviews_holding(task).next()?;
    let source = state.tasks.get(&review.task_id).map_or("", |t| t.title.as_str());
    Some(format!(
        "{task} waits for the orchestrator to review its plan after the user's decision on «{source}»: it starts once settle_plan_review settles it"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_daemon_engine_model::LaunchPlan;
    use crate::domain_runtime::DomainRuntime;
    use alondra_core::domain::{
        Actor, CardKind, CardSpec, Command, CommandEnvelope, PLAN_REVIEW_KEEP_PLANS, PLAN_REVIEW_PROBLEM_PREFIX, PLAN_REVIEW_REMIND,
        PROTOCOL_VERSION, PlanReviewOutcome, QuestionOption, Recipient, Scope, SuggestionDecision,
    };
    use alondra_core::orchestrator_cards::PLAN_REVIEW_NOTICE_PREFIX;
    use std::collections::{BTreeMap, BTreeSet};

    fn run(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) -> Result<(), alondra_core::domain::DomainError> {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(),
            expected_revision: state.revision, command,
        }, None, || Ok(())).map(drop).map_err(|error| match error {
            crate::domain_store::StoreError::Domain(error) => error,
            crate::domain_store::StoreError::Storage(error) => panic!("{error:#}"),
        })
    }

    fn must(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
        let shown = format!("{command:?}");
        if let Err(error) = run(domain, actor, id, command) {
            panic!("{shown} was refused: {error:?}");
        }
    }

    fn orchestrator() -> Actor {
        Actor::Orchestrator { generation: 0 }
    }

    fn plan(name: &str, mission: &str) -> LaunchPlan {
        LaunchPlan { mission: Some(mission.into()), ..LaunchPlan::new(name, "/tmp") }
    }

    fn task(domain: &DomainRuntime, id: &str, agent: Option<&str>, dependencies: &[&str], mission: &str) {
        must(domain, orchestrator(), &format!("create-{id}"), Command::CreateTask {
            task_id: id.into(), request_id: "r".into(), title: format!("Redraft index · Phase {}", id.to_uppercase()), agent_id: agent.map(Into::into),
            review_prs: false, dependencies: dependencies.iter().map(|d| TaskId::from(*d)).collect(), stack: None, launch: Some(plan(id, mission)),
            plan_items: None, continues: None,
        });
    }

    /// 15 October: phase B of Redraft's index, with C, D and E waiting on plans written for the 360 px copy.
    fn redraft() -> (crate::test_support::Directory, DomainRuntime) {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        must(&domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "Phase B".into(), at: 0 });
        must(&domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest {
            request_id: "r".into(), body: "Redraft index".into(), auto_merge: false, maintenance: false, message: false });
        must(&domain, orchestrator(), "title", Command::TitleRequest { request_id: "r".into(), title: "Redraft index".into() });
        task(&domain, "b", Some("a"), &[], "Shadow mode with the 360 px copy");
        must(&domain, Actor::Agent { agent_id: "a".into() }, "start", Command::StartTask { task_id: "b".into() });
        task(&domain, "c", None, &["b"], "Phase C: group with prints of the 360 px copy");
        task(&domain, "d", None, &["c"], "Phase D: 360 px everywhere");
        task(&domain, "e", None, &["d"], "Phase E: drop the old engine");
        must(&domain, Actor::Agent { agent_id: "a".into() }, "ask", Command::AskQuestion {
            attachments: vec![], interaction_id: "decision-1".into(), scope: Scope::Task { task_id: "b".into() }, recipient: Recipient::User,
            body: "Only 11 of 21 groups match with the 360 px copy".into(), blocking: true,
            options: vec![
                QuestionOption { id: "threshold".into(), label: "Adjust the threshold and repeat".into(), description: None },
                QuestionOption { id: "plan-512".into(), label: "Alternative plan: 512 px prints".into(), description: None },
            ],
            card: CardSpec::new(CardKind::Decide, "Decide the index's next step").why("Phase C starts after it"),
        });
        must(&domain, Actor::User { user_id: "u".into() }, "answer", Command::AnswerQuestion {
            attachments: vec![], interaction_id: "decision-1".into(), option_id: "plan-512".into(), inputs: BTreeMap::new() });
        (home, domain)
    }

    fn orchestrator_messages(domain: &DomainRuntime) -> Vec<String> {
        let store = domain.store.lock().unwrap();
        store.pending_messages().unwrap().into_iter().filter(|m| m.recipient_kind == "orchestrator").map(|m| m.message).collect()
    }

    fn settle(domain: &DomainRuntime, id: &str, task: &str, outcome: PlanReviewOutcome) -> Result<(), alondra_core::domain::DomainError> {
        run(domain, orchestrator(), id, Command::SettlePlanReview { review_id: "decision-1".into(), task_id: task.into(), outcome })
    }

    #[test]
    fn the_orchestrator_is_told_the_decision_with_every_held_plan_in_the_answers_commit() {
        let (_home, domain) = redraft();
        let told: Vec<String> = orchestrator_messages(&domain).into_iter().filter(|m| m.contains("settle_plan_review")).collect();
        assert_eq!(told.len(), 1, "{told:?}");
        for expected in ["“Decide the index's next step” → Alternative plan: 512 px prints", "Phase C: group with prints of the 360 px copy",
            "Phase D: 360 px everywhere", "Phase E: drop the old engine", "review_id: \"decision-1\""] {
            assert!(told[0].contains(expected), "{expected} missing from {}", told[0]);
        }
        assert!(!told[0].contains("Shadow mode"), "the task decided on is not held");
    }

    #[test]
    fn a_hold_survives_a_restart_and_a_new_plan_is_stored_with_its_settlement() {
        let (home, domain) = redraft();
        let queued = alondra_daemon_engine_model::LaunchBody::from_plan("c".into(), &plan("c", "old")).to_queue();
        domain.store.lock().unwrap().enqueue_launch(&"c".into(), &queued, "reopen", 1).unwrap();
        drop(domain);
        let domain = DomainRuntime::open(&home).unwrap();
        assert!(held(&domain.state().unwrap(), &"c".into()).is_some(), "the hold is persisted");
        let at_before: i64 = domain.store.lock().unwrap().planned_tasks().unwrap()[&TaskId::from("c")];
        settle(&domain, "replan-c", "c", PlanReviewOutcome::PlanChanged { launch: plan("c", "Phase C: group with 512 px prints"), reason: "The user chose 512 px".into() }).unwrap();
        assert_eq!(domain.launch_plan(&"c".into()).unwrap().unwrap().mission.as_deref(), Some("Phase C: group with 512 px prints"));
        assert_eq!(domain.store.lock().unwrap().planned_tasks().unwrap()[&TaskId::from("c")], at_before, "a new plan keeps the task's place");
        let queue = domain.store.lock().unwrap().launch_queue().unwrap();
        let body = alondra_daemon_engine_model::LaunchBody::from_queue(&queue[0].body).unwrap();
        assert_eq!(body.to_queue()["body"]["prompt"], "Phase C: group with 512 px prints", "the queued launch starts with the new plan");
        assert!(held(&domain.state().unwrap(), &"c".into()).is_none());
        assert!(held(&domain.state().unwrap(), &"d".into()).is_some(), "each dependent is settled on its own");
        settle(&domain, "cancel-e", "e", PlanReviewOutcome::Cancelled { reason: "Phase E no longer applies".into() }).unwrap();
        assert_eq!(domain.store.lock().unwrap().cancel_reasons().unwrap()[&TaskId::from("e")], "Phase E no longer applies");
    }

    #[test]
    fn a_held_plan_is_not_replaced_behind_the_review() {
        let home = crate::test_support::Directory::new();
        let (_root, domain) = redraft();
        std::fs::create_dir_all(&home).unwrap();
        let body = serde_json::from_value(serde_json::json!({"task_id": "d", "launch": {"name": "d", "cwd": "/tmp", "model": "sonnet", "reason": "r"}})).unwrap();
        let refused = crate::domain_orchestrator_tools::set_launch_plan(&domain, &home, body).unwrap_err();
        assert_eq!(refused.code, "plan_review_pending");
        let patch = serde_json::from_value(serde_json::json!({"task_id": "d", "changes": {"model": "codex/gpt-6"}})).unwrap();
        let partial = crate::domain_orchestrator_tools::update_launch_plan(&domain, &home, patch).unwrap_err();
        assert_eq!(partial.code, "plan_review_pending");
        assert!(refused.message.contains("settle_plan_review"), "{}", refused.message);
        assert_eq!(domain.launch_plan(&"d".into()).unwrap().unwrap().mission.as_deref(), Some("Phase D: 360 px everywhere"));
    }

    #[test]
    fn a_suggestion_that_becomes_a_task_keeps_its_launch_plan() {
        let (_home, domain) = redraft();
        must(&domain, Actor::Agent { agent_id: "a".into() }, "suggest", Command::AskQuestion {
            attachments: vec![], interaction_id: "p".into(), scope: Scope::Task { task_id: "b".into() }, recipient: Recipient::Orchestrator,
            body: "The README still names the old flag".into(), blocking: false, options: alondra_core::domain::suggestion_options(),
            card: CardSpec::new(CardKind::Propose, "Update the README's old flag").why("It is not part of this task"),
        });
        must(&domain, orchestrator(), "settle", Command::SettleSuggestion { interaction_id: "p".into(), decision: SuggestionDecision::NewTask {
            task_id: "readme".into(), request_id: "r-readme".into(), title: "Update the README flag".into(),
            launch: plan("readme", "Replace the old flag in the README"), dependencies: BTreeSet::new(), note: None,
        } });
        assert_eq!(domain.launch_plan(&"readme".into()).unwrap().unwrap().mission.as_deref(), Some("Replace the old flag in the README"));
    }

    fn problems(domain: &DomainRuntime) -> Vec<String> {
        domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with(PLAN_REVIEW_PROBLEM_PREFIX)).map(|id| id.0.clone()).collect()
    }

    fn reminders(domain: &DomainRuntime) -> usize {
        domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with(PLAN_REVIEW_NOTICE_PREFIX)).count()
    }

    #[test]
    fn an_unsettled_review_goes_back_once_then_the_user_keeps_the_plans() {
        let (_home, domain) = redraft();
        settle(&domain, "c-same", "c", PlanReviewOutcome::Unchanged { reason: "Phase C does not depend on the print size".into() }).unwrap();
        crate::orchestrator_inbox::settle(&domain);
        assert_eq!((reminders(&domain), problems(&domain).len()), (1, 0), "the orchestrator gets one more go first");
        let reminder = domain.state().unwrap().interactions.values().find(|i| i.id.0.starts_with(PLAN_REVIEW_NOTICE_PREFIX)).unwrap().body.clone();
        assert!(reminder.contains("Phase D: 360 px everywhere") && !reminder.contains("Phase C: group"), "only what is still held: {reminder}");
        crate::orchestrator_inbox::settle(&domain);
        let raised = problems(&domain);
        assert_eq!(raised.len(), 1);
        crate::orchestrator_inbox::settle(&domain);
        assert_eq!(problems(&domain).len(), 1, "an open problem is not raised twice");
        let state = domain.state().unwrap();
        let card = &state.interactions[&raised[0].as_str().into()];
        let (need, why) = alondra_core::domain::PlanReviewProblem::from_body(&card.body).unwrap().text(alondra_core::views::Lang::Es);
        assert_eq!(need, "Las tareas tras «Redraft index · Phase B» esperan revisión de plan");
        assert!(why.contains("«Redraft index · Phase D», «Redraft index · Phase E»"), "{why}");
        must(&domain, Actor::User { user_id: "u".into() }, "remind", Command::AnswerQuestion {
            attachments: vec![], interaction_id: raised[0].as_str().into(), option_id: PLAN_REVIEW_REMIND.into(), inputs: BTreeMap::new() });
        crate::orchestrator_inbox::settle(&domain);
        assert_eq!((reminders(&domain), problems(&domain).len()), (2, 1), "reminding hands the decision over again");
        crate::orchestrator_inbox::settle(&domain);
        let raised = problems(&domain);
        assert_eq!(raised.len(), 2);
        let second = raised.iter().find(|id| id.ends_with("-2")).unwrap();
        must(&domain, Actor::User { user_id: "u".into() }, "keep", Command::AnswerQuestion {
            attachments: vec![], interaction_id: second.as_str().into(), option_id: PLAN_REVIEW_KEEP_PLANS.into(), inputs: BTreeMap::new() });
        let state = domain.state().unwrap();
        assert!(held(&state, &"d".into()).is_none() && held(&state, &"e".into()).is_none(), "keeping the plans releases them");
        assert!(matches!(state.plan_reviews[&"decision-1".into()].dependents[&TaskId::from("d")].as_ref().unwrap().outcome, PlanReviewOutcome::KeptByUser));
        crate::orchestrator_inbox::settle(&domain);
        assert_eq!((reminders(&domain), problems(&domain).len()), (2, 2), "a settled review asks nothing more");
    }

    #[test]
    fn the_problem_goes_once_the_orchestrator_settles_what_it_held() {
        let (_home, domain) = redraft();
        crate::orchestrator_inbox::settle(&domain);
        crate::orchestrator_inbox::settle(&domain);
        let raised = problems(&domain);
        for task in ["c", "d", "e"] {
            settle(&domain, &format!("same-{task}"), task, PlanReviewOutcome::Unchanged { reason: "Same plan".into() }).unwrap();
        }
        assert!(domain.state().unwrap().interactions[&raised[0].as_str().into()].withdrawn, "nothing is left to ask the user");
    }
}
