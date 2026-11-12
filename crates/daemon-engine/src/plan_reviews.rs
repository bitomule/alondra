//! What the orchestrator reads when a decision of the user holds the tasks that wait for the one
//! decided on, and why a launch of one of them is refused until it reviews their plans.
use alondra_daemon_engine_model::LaunchPlan;
use alondra_core::domain::{ANSWER_TEXT, DomainState, InputValue, InteractionId, InteractionPayload, TaskId};

/// Why a held task does not start now, or nothing if no decision holds it.
pub(crate) fn held(state: &DomainState, task: &TaskId) -> Option<String> {
    let (_, review) = state.plan_reviews_holding(task).next()?;
    let source = state.tasks.get(&review.task_id).map_or("", |t| t.title.as_str());
    Some(format!(
        "{task} waits for the orchestrator to review its plan after the user's decision on «{source}»: it starts once settle_plan_review settles it"))
}

/// What the user decided on each card of the review: the question and the option, with any words.
fn decided(state: &DomainState, decisions: &[InteractionId]) -> Vec<String> {
    decisions.iter().filter_map(|id| state.interactions.get(id)).map(|card| {
        let InteractionPayload::Question { options, answer: Some(answer) } = &card.payload else {
            return format!("- “{}”: no answer", card.card.effective_title());
        };
        let option = options.iter().find(|o| o.id == answer.option_id).map_or(answer.option_id.as_str(), |o| o.label.as_str());
        let words = match answer.inputs.get(ANSWER_TEXT) {
            Some(InputValue::Text(text)) if !text.trim().is_empty() => format!(" They wrote: {}", text.trim()),
            _ => String::new(),
        };
        format!("- “{}” → {option}.{words}\n  What the node asked: {}", card.card.effective_title(), card.body.trim())
    }).collect()
}

/// The decision and the plan of every dependent it still holds, with how to settle each.
pub(crate) fn orchestrator_text(state: &DomainState, review_id: &InteractionId, plan_of: &dyn Fn(&TaskId) -> Option<LaunchPlan>) -> String {
    let Some(review) = state.plan_reviews.get(review_id) else { return String::new() };
    let source = state.tasks.get(&review.task_id).map_or("", |t| t.title.as_str());
    let held: Vec<String> = state.plan_review_waiting(review).map(|t| {
        let (task, title) = (&t.id, t.title.as_str());
        let plan = plan_of(task).map_or_else(|| "none stored".to_owned(), |plan| serde_json::to_string_pretty(&plan).unwrap_or_default());
        format!("- “{title}” ({task}). Its launch plan:\n{plan}")
    }).collect();
    format!(
        "The user decided on Task “{source}” ({}), which other tasks wait for:\n{}\n\n\
         These tasks have not started and Alondra holds them, because their launch plans were written before that decision. \
         Review each plan against the decision and settle every one with settle_plan_review(review_id: \"{review_id}\", task_id, outcome): \
         unchanged with the reason the plan still holds, plan_changed with the whole new launch plan and the reason, or cancelled with the reason. \
         None starts until you do, and set_launch_plan does not change a held plan. If it is a product decision only the user can make, ask them first, then settle it.\n{}",
        review.task_id, decided(state, &review.decisions).join("\n"), held.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_runtime::DomainRuntime;
    use alondra_core::domain::{
        Actor, CardKind, CardSpec, Command, CommandEnvelope, PROTOCOL_VERSION, PlanReviewOutcome, QuestionOption, Recipient, Scope, SuggestionDecision,
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

    fn reminders(domain: &DomainRuntime) -> usize {
        domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with(PLAN_REVIEW_NOTICE_PREFIX)).count()
    }

    const MINUTE: i64 = 60_000;

    #[test]
    fn an_unreviewed_plan_is_released_unchanged_and_its_node_is_told() {
        let (_home, domain) = redraft();
        let queued = alondra_daemon_engine_model::LaunchBody::from_plan("d".into(), &plan("d", "Phase D: 360 px everywhere")).to_queue();
        domain.store.lock().unwrap().enqueue_launch(&"d".into(), &queued, "auto", 1).unwrap();
        settle(&domain, "c-same", "c", PlanReviewOutcome::Unchanged { reason: "Phase C does not depend on the print size".into() }).unwrap();
        crate::orchestrator_inbox::settle(&domain);
        assert_eq!(reminders(&domain), 0, "a handled batch reminds of nothing");
        for minutes in [0, 10] {
            crate::orchestrator_inbox::settle_overdue(&domain, minutes * MINUTE);
        }
        assert_eq!(reminders(&domain), 1, "reminded at its deadline");
        let reminder = domain.state().unwrap().interactions.values().find(|i| i.id.0.starts_with(PLAN_REVIEW_NOTICE_PREFIX)).unwrap().body.clone();
        assert!(reminder.contains("Phase D: 360 px everywhere") && !reminder.contains("Phase C: group"), "only what is still held: {reminder}");
        assert!(held(&domain.state().unwrap(), &"d".into()).is_some(), "one more go for the orchestrator first");
        crate::orchestrator_inbox::settle_overdue(&domain, 21 * MINUTE);
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

