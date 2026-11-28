//! A task's specs: proposed by its author with the plan, edited and approved by the user with it,
//! written while no agent works, and changed afterwards only through the user's approval.
use super::lifecycle_tests::{assert_aggregate_rules, exec, must, node, task, user, world};
use super::specs_tests::{changes, decision, Mem};
use super::*;
use crate::spec_card::{SpecCard, APPROVE, REJECT};
use crate::specs::{self, SpecEdit};
use alondra_core::accounts::{AccountKey, Provider};
use alondra_core::model_selection::{ModelKey, ModelProfile, ModelSelection, PendingTaskSelection, TaskSelection};
use serde_json::json;
use std::collections::BTreeMap;

pub(super) const PARTIAL_PATH: &str = "specs/billing/partial.feature";
const PARTIAL: &str = "@spec:billing.partial\nFeature: Partial refunds\n  Part of an order comes back.\n\n  @id:part\n  Scenario: Part of an order\n    Given a paid order of 30 EUR\n    When 10 EUR are refunded\n    Then the customer receives 10 EUR\n";

fn staged(s: &mut DomainState, staged: bool) {
    let selections = if staged { TaskStages::role_selections(&TaskSelection::automatic(ModelProfile::Implementation)) } else { BTreeMap::new() };
    must(s, Actor::System, Command::RecordTaskStages { task_id: "t".into(), staged, reason: "Large".into(), selections });
}
pub(super) fn admit(s: &mut DomainState, agent: &str, generation: u64) {
    if !s.agents.contains_key(&AgentId::from(agent)) {
        must(s, Actor::System, Command::RegisterAgent { agent_id: agent.into(), name: agent.into(), at: 0 });
    }
    must(s, Actor::System, Command::AdmitExecution { provider: Default::default(), task_id: "t".into(), agent_id: agent.into(),
        conversation: format!("conversation-{generation}"), instance: format!("instance-{generation}"), host: "sdk-host".into(),
        model: None, account: None, leaves_limit: None });
    let selection = s.tasks[&TaskId::from("t")].selection.clone();
    if generation > 1 {
        must(s, Actor::System, Command::AbortTaskSelection { task_id: "t".into(), selection, refused: None });
    }
}
fn running(stages: bool) -> DomainState {
    let mut s = world();
    task(&mut s, "t", None);
    staged(&mut s, stages);
    admit(&mut s, "a", 1);
    s
}
fn files(entries: &[(&str, Option<&str>)]) -> Vec<SpecFile> {
    entries.iter().map(|(path, text)| SpecFile::of((*path).into(), text.map(Into::into))).collect()
}
fn proposal(entries: &[(&str, Option<&str>)]) -> Command {
    let check = specs::check(&Mem::project(), &changes(entries), &specs::NoImages);
    Command::ProposeSpecs { task_id: "t".into(), level: SpecLevel::Light, files: files(entries), check }
}
fn plan_card() -> Command {
    let body = "# Plan\n\n- [ ] Add partial refunds\n";
    let card = CardSpec::new(CardKind::Review, "Review the plan").content(ContentKind::Plan, body);
    Command::RecordDecisionRequest { task_id: "t".into(), generation: 1, epoch: 7, hold: Hold::Plan, tool: Some("plan".into()), input: json!({}), mcp_server: None,
        prompt: DecisionPrompt::Card { body: body.into(), options: Hold::Plan.options_offering(false), card } }
}
fn plan_id() -> InteractionId {
    decision_card_id(&"t".into(), 1, 7)
}
pub(super) fn answer(s: &mut DomainState, card: InteractionId, option: &str, words: Option<&str>) -> Result<CommandOutcome, DomainError> {
    let inputs = words.map(|words| BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text(words.into()))])).unwrap_or_default();
    exec(s, user(), Command::AnswerQuestion { attachments: vec![], interaction_id: card, option_id: option.into(), inputs })
}
fn specs_of(s: &DomainState) -> &TaskSpecs {
    &s.tasks[&TaskId::from("t")].specs
}
fn pending() -> PendingTaskSelection {
    PendingTaskSelection { account: AccountKey::native(Provider::Claude),
        model: ModelSelection { model: ModelKey { provider: Provider::Claude, model: "sonnet".into() }, reasoning_effort: None, catalog_revision: "catalog".into() },
        reason: "The next role takes the task over".into() }
}
pub(super) fn begin(s: &mut DomainState, transition: &str) -> Result<CommandOutcome, DomainError> {
    let expected_selection = s.task_selection_stamp(&"t".into()).unwrap();
    exec(s, Actor::System, Command::BeginStageHandover { task_id: "t".into(), expected_selection, transition: transition.into(), pending: pending() })
}
fn written(s: &DomainState) -> Command {
    let due = specs_of(s).due();
    Command::RecordSpecsWritten { task_id: "t".into(), written: due.iter().map(|(path, text)| (path.clone(), specs::fingerprint(text.as_ref()))).collect() }
}
pub(super) fn told(s: &DomainState) -> Vec<String> {
    s.lifecycle_journal[&TaskId::from("t")].iter().filter_map(|record| match record {
        LifecycleRecord::Told { text: Some(text), .. } => Some(text.clone()),
        _ => None,
    }).collect()
}
/// A staged task whose author proposed partial refunds and a decision, with its plan approved.
fn approved() -> DomainState {
    let mut s = running(true);
    must(&mut s, node("a"), proposal(&[(PARTIAL_PATH, Some(PARTIAL)), ("docs/decisions/keep-refunds.md", Some(&decision("keep-refunds", "proposed")))]));
    must(&mut s, Actor::System, plan_card());
    answer(&mut s, plan_id(), Hold::APPROVE, Some("Fine, keep refunds simple")).unwrap();
    s
}
/// `approved`, with the approved files written and the implementer at work.
pub(super) fn implementing() -> DomainState {
    let mut s = approved();
    must(&mut s, Actor::System, Command::RecordDecisionSettled { task_id: "t".into(), generation: 1, epoch: 7 });
    let record = written(&s);
    must(&mut s, Actor::System, record);
    begin(&mut s, "stage-1").unwrap();
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 1, resumes: false, ended_work: vec![], by_alondra: true });
    admit(&mut s, "b", 2);
    s
}

#[test]
fn the_author_proposes_the_plans_specs_and_the_plan_card_waits_for_them() {
    let mut s = running(true);
    must(&mut s, node("a"), proposal(&[(PARTIAL_PATH, Some(PARTIAL))]));
    let draft = specs_of(&s).draft.clone().expect("the proposal waits for the plan");
    assert_eq!(crate::specs_tests::text(&draft.files, PARTIAL_PATH), Some(PARTIAL));
    assert!(draft.check.clean());
    must(&mut s, Actor::System, plan_card());
    let refused = exec(&mut s, node("a"), proposal(&[(PARTIAL_PATH, Some(PARTIAL))])).unwrap_err();
    assert_eq!(refused.code, "plan_open", "specs go before the plan, never under the user's eyes");
    assert_aggregate_rules(&s);
}

#[test]
fn a_proposal_is_refused_where_specs_are_off_or_unreadable() {
    let mut s = running(true);
    let off = Command::ProposeSpecs { task_id: "t".into(), level: SpecLevel::Light, files: files(&[(PARTIAL_PATH, Some(PARTIAL))]), check: SpecCheck::default() };
    assert_eq!(exec(&mut s, node("a"), off).unwrap_err().code, "specs_disabled");
    let broken = PARTIAL.replace("  @id:part\n", "");
    let unreadable = proposal(&[(PARTIAL_PATH, Some(&broken))]);
    let refused = exec(&mut s, node("a"), unreadable).unwrap_err();
    assert_eq!(refused.code, "specs_unreadable");
    assert!(refused.message.contains(PARTIAL_PATH), "the refusal names the file: {}", refused.message);
    let settings = Command::ProposeSpecs { task_id: "t".into(), level: SpecLevel::Light, files: files(&[(PARTIAL_PATH, Some(PARTIAL))]),
        check: SpecCheck::unreadable(SpecProblem::new("invalid_settings", "alondra.toml", 2, "expected `]`")) };
    assert_eq!(exec(&mut s, node("a"), settings).unwrap_err().code, "spec_settings_unreadable");
    assert_eq!(specs_of(&s).draft, None);
}

#[test]
fn approving_the_plan_approves_that_exact_set_and_accepts_its_decisions_in_the_users_words() {
    let s = approved();
    let specs = specs_of(&s);
    assert_eq!(specs.draft, None);
    let approved = specs.approved.as_ref().expect("approved with the plan");
    assert_eq!(approved.card, plan_id());
    assert_eq!(crate::specs_tests::text(&approved.files, PARTIAL_PATH), Some(PARTIAL));
    assert_eq!(specs::frontmatter(crate::specs_tests::text(&approved.files, "docs/decisions/keep-refunds.md").unwrap(), "status").as_deref(), Some("accepted"));
    let answer = &specs.decisions[0];
    assert_eq!((answer.id.as_str(), answer.verdict, answer.words.as_deref()), ("keep-refunds", DecisionVerdict::Accepted, Some("Fine, keep refunds simple")));
    assert_eq!(specs.due().len(), 2, "both files are owed to the worktree");
    assert_aggregate_rules(&s);
}

#[test]
fn the_implementer_starts_only_after_alondra_wrote_the_approved_files() {
    let mut s = approved();
    must(&mut s, Actor::System, Command::RecordDecisionSettled { task_id: "t".into(), generation: 1, epoch: 7 });
    assert!(s.specs_due(&"t".into()));
    assert_eq!(begin(&mut s, "stage-1").unwrap_err().code, "specs_unwritten");
    let stale = Command::RecordSpecsWritten { task_id: "t".into(), written: BTreeMap::from([(PARTIAL_PATH.to_owned(), specs::fingerprint(Some(&"another text".into())))]) };
    must(&mut s, Actor::System, stale);
    assert!(s.specs_due(&"t".into()), "writing other text than the approved one clears nothing");
    let record = written(&s);
    must(&mut s, Actor::System, record);
    assert!(!s.specs_due(&"t".into()));
    begin(&mut s, "stage-1").unwrap();
    assert!(told(&s).iter().all(|text| !text.contains("Alondra wrote")), "the new implementer learns of them from its context, not from a message to the author");
}

#[test]
fn a_plan_is_not_approved_while_the_settings_cannot_be_read() {
    let mut s = running(true);
    must(&mut s, node("a"), proposal(&[(PARTIAL_PATH, Some(PARTIAL))]));
    must(&mut s, Actor::System, plan_card());
    must(&mut s, Actor::System, Command::RecordSpecSettings { task_id: "t".into(), problem: Some(SpecProblem::new("invalid_settings", "alondra.toml", 3, "unknown key")) });
    assert_eq!(answer(&mut s, plan_id(), Hold::APPROVE, None).unwrap_err().code, "spec_settings_unreadable");
    assert_eq!(specs_of(&s).approved, None);
    must(&mut s, Actor::System, Command::RecordSpecSettings { task_id: "t".into(), problem: None });
    answer(&mut s, plan_id(), Hold::APPROVE, None).unwrap();
    assert!(specs_of(&s).approved.is_some());
}

#[test]
fn the_user_edits_the_proposal_on_the_card_and_a_removed_decision_is_rejected() {
    let mut s = running(true);
    must(&mut s, node("a"), proposal(&[(PARTIAL_PATH, Some(PARTIAL)), ("docs/decisions/keep-refunds.md", Some(&decision("keep-refunds", "proposed")))]));
    let draft = specs_of(&s).draft.clone().unwrap();
    let edit = SpecEdit::RemoveDecision { path: "docs/decisions/keep-refunds.md".into() };
    let (level, edited) = specs::edited(&draft, &edit).unwrap();
    let revise = |expected| Command::ReviseSpecs { task_id: "t".into(), expected, level, files: edited.iter().map(|(path, text)| SpecFile::of(path.clone(), text.clone())).collect(),
        check: specs::check(&Mem::project(), &edited, &specs::NoImages) };
    assert_eq!(exec(&mut s, user(), revise(draft.revision)).unwrap_err().code, "plan_not_open", "edits happen on the plan's card");
    must(&mut s, Actor::System, plan_card());
    assert_eq!(exec(&mut s, user(), revise(draft.revision - 1)).unwrap_err().code, "stale_specs");
    assert_eq!(exec(&mut s, node("a"), revise(draft.revision)).unwrap_err().code, "forbidden", "only the user edits on the card");
    must(&mut s, user(), revise(draft.revision));
    let specs = specs_of(&s);
    assert!(specs.draft.as_ref().unwrap().edited);
    assert!(!specs.draft.as_ref().unwrap().files.contains_key("docs/decisions/keep-refunds.md"));
    assert_eq!((specs.decisions[0].id.as_str(), specs.decisions[0].verdict), ("keep-refunds", DecisionVerdict::Rejected));
    answer(&mut s, plan_id(), Hold::APPROVE, None).unwrap();
    assert_eq!(specs_of(&s).approved.as_ref().unwrap().files.keys().collect::<Vec<_>>(), [PARTIAL_PATH], "the approval takes the edited set");
}

#[test]
fn specs_kept_in_alondra_are_approved_but_never_written_into_the_worktree() {
    let mut s = running(true);
    let elsewhere = Mem::default().with(specs::SETTINGS_PATH, "format = 1\n\n[specs]\nenabled = true\ncommitted = false\n")
        .with("specs/billing/boundary.toml", super::specs_tests::BOUNDARY).with(super::specs_tests::REFUNDS_PATH, super::specs_tests::REFUNDS);
    let changed = changes(&[(PARTIAL_PATH, Some(PARTIAL))]);
    let check = specs::check(&elsewhere, &changed, &specs::NoImages);
    assert_eq!(check.mode, SpecMode::Uncommitted);
    must(&mut s, node("a"), Command::ProposeSpecs { task_id: "t".into(), level: SpecLevel::Light, files: files(&[(PARTIAL_PATH, Some(PARTIAL))]), check });
    must(&mut s, Actor::System, plan_card());
    answer(&mut s, plan_id(), Hold::APPROVE, None).unwrap();
    let approved = specs_of(&s).approved.clone().unwrap();
    assert_eq!(approved.mode, SpecMode::Uncommitted);
    assert!(specs_of(&s).due().is_empty(), "nothing is owed to the worktree");
    assert!(approved.allowed().is_empty(), "and the branch may not hold them either");
    must(&mut s, Actor::System, Command::RecordDecisionSettled { task_id: "t".into(), generation: 1, epoch: 7 });
    begin(&mut s, "stage-1").unwrap();
}

/// `implementing`, with the implementer's change drafted by a new author and waiting for the user
/// on its card, and the text the change gives the spec.
fn drafted() -> (DomainState, InteractionId, String) {
    let mut s = implementing();
    assert_eq!(exec(&mut s, node("b"), proposal(&[(PARTIAL_PATH, Some(PARTIAL))])).unwrap_err().code, "role_forbids_tool", "the implementer never writes specs");
    must(&mut s, node("b"), Command::RequestSpecChange { task_id: "t".into(), reason: "A refund of zero is not covered".into() });
    let handover = s.tasks[&TaskId::from("t")].stages.handover.clone().unwrap();
    assert_eq!((handover.to, handover.cause), (ExecutionRole::Author, HandoverCause::SpecChangeRequested { amendment: "a1".into() }));
    assert_eq!(exec(&mut s, node("b"), Command::RequestSpecChange { task_id: "t".into(), reason: "Again".into() }).unwrap_err().code, "spec_change_pending");
    begin(&mut s, "stage-2").unwrap();
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 2, resumes: false, ended_work: vec![], by_alondra: true });
    admit(&mut s, "c", 3);
    assert_eq!(s.tasks[&TaskId::from("t")].stages.role(), Some(ExecutionRole::Author));
    let zero = PARTIAL.replace("    Then the customer receives 10 EUR\n", "    Then the customer receives 10 EUR\n\n  @id:zero\n  Scenario: Nothing to refund\n    Given a paid order of 30 EUR\n    When 0 EUR are refunded\n    Then the refund is refused\n");
    let base = Mem::project().with(PARTIAL_PATH, PARTIAL);
    let check = specs::check(&base, &changes(&[(PARTIAL_PATH, Some(&zero))]), &specs::NoImages);
    assert!(check.clean(), "{}", check.summary());
    must(&mut s, node("c"), Command::ProposeAmendment { task_id: "t".into(), amendment_id: "a1".into(), files: files(&[(PARTIAL_PATH, Some(&zero))]), check });
    let card = specs_of(&s).amendment("a1").and_then(|a| a.card.clone()).unwrap();
    (s, card, zero)
}

#[test]
fn the_implementer_asks_for_a_change_the_author_drafts_it_and_the_user_approves_it() {
    let (mut s, card, zero) = drafted();
    let amendment = specs_of(&s).amendment("a1").cloned().unwrap();
    assert_eq!(amendment.state, AmendmentState::Proposed);
    assert_eq!(crate::specs_tests::text(&amendment.before, PARTIAL_PATH), Some(PARTIAL), "before is the approved text");
    let raised = &s.interactions[&card];
    assert_eq!((raised.system_card.clone(), raised.author.clone(), raised.recipient.clone()), (Some(SystemCard::SpecChange), Author::System, Recipient::User));
    assert_eq!(SpecCard::from_body(&raised.body), Some(SpecCard::Amendment { task: "t".into(), id: "a1".into() }));
    assert_eq!(s.tasks[&TaskId::from("t")].stages.handover.as_ref().map(|h| h.to), Some(ExecutionRole::Implementer), "the implementer carries on while the user decides");
    begin(&mut s, "stage-3").unwrap();
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 3, resumes: false, ended_work: vec![], by_alondra: true });
    admit(&mut s, "d", 4);
    let again = exec(&mut s, node("d"), Command::RequestSpecChange { task_id: "t".into(), reason: "Another case".into() }).unwrap_err();
    assert_eq!(again.code, "spec_change_pending", "one change of the specs at a time, so none undoes another");
    answer(&mut s, card, APPROVE, Some("Yes, refuse zero")).unwrap();
    let specs = specs_of(&s);
    assert_eq!(crate::specs_tests::text(&specs.approved.as_ref().unwrap().files, PARTIAL_PATH), Some(zero.as_str()));
    assert_eq!(specs.amendment("a1").unwrap().words.as_deref(), Some("Yes, refuse zero"));
    assert!(specs.due().contains_key(PARTIAL_PATH), "the approved change is owed to the worktree");
    assert!(!told(&s).iter().any(|text| text.contains("refuse zero")), "the agent hears of it once the file is written, not before");
    let record = written(&s);
    must(&mut s, Actor::System, record);
    assert!(told(&s).iter().any(|text| text.contains(PARTIAL_PATH) && text.contains("Commit them")), "then it is told to commit it");
    assert_aggregate_rules(&s);
}

#[test]
fn a_rejected_change_leaves_the_approved_specs_and_the_agent_hears_the_users_words() {
    let (mut s, card, _) = drafted();
    answer(&mut s, card, REJECT, Some("Zero never happens")).unwrap();
    let specs = specs_of(&s);
    assert_eq!(specs.amendment("a1").unwrap().state, AmendmentState::Rejected);
    assert_eq!(crate::specs_tests::text(&specs.approved.as_ref().unwrap().files, PARTIAL_PATH), Some(PARTIAL), "the spec stays the approved one");
    assert!(specs.due().is_empty());
    assert!(told(&s).iter().any(|text| text.contains("Zero never happens")), "the user's words reach the agent");
}

#[test]
fn a_lasting_decision_waits_on_its_own_card_and_is_kept_with_the_users_words() {
    let mut s = running(false);
    let base = Mem::project().with("docs/decisions/old.md", &decision("old", "accepted"));
    let fields = DecisionFields { id: "new".into(), title: "A new rule".into(), context: "c".into(), decision: "d".into(), consequences: "e".into(), confirmation: "f".into(), supersedes: vec!["old".into()] };
    let dirs = specs::check(&base, &SpecFiles::new(), &specs::NoImages).dirs;
    let files = specs::decision_files(&fields, "2026-11-26", &dirs, &base).unwrap();
    let check = specs::check(&base, &files, &specs::NoImages);
    must(&mut s, node("a"), Command::ProposeDecision { task_id: "t".into(), decision: fields.clone(), files: files.clone(), check: check.clone() });
    let card = specs_of(&s).proposed_decisions[0].card.clone();
    assert_eq!(exec(&mut s, node("a"), Command::ProposeDecision { task_id: "t".into(), decision: fields, files, check }).unwrap_err().code, "decision_pending");
    answer(&mut s, card.clone(), APPROVE, Some("Yes, for every new project")).unwrap();
    let specs = specs_of(&s);
    assert!(specs.proposed_decisions.is_empty());
    let approved = specs.approved.as_ref().expect("an accepted decision is approved specs of the task");
    assert_eq!(specs::frontmatter(crate::specs_tests::text(&approved.files, "docs/decisions/new.md").unwrap(), "status").as_deref(), Some("accepted"));
    assert_eq!(specs::frontmatter(crate::specs_tests::text(&approved.files, "docs/decisions/old.md").unwrap(), "status").as_deref(), Some("superseded"));
    assert_eq!(specs.decisions[0].words.as_deref(), Some("Yes, for every new project"));
    assert_eq!(specs.due().len(), 2);
}

#[test]
fn a_rejected_lasting_decision_writes_nothing() {
    let mut s = running(false);
    let base = Mem::project();
    let fields = DecisionFields { id: "rule".into(), title: "A rule".into(), context: "c".into(), decision: "d".into(), consequences: "e".into(), confirmation: "f".into(), supersedes: vec![] };
    let dirs = specs::check(&base, &SpecFiles::new(), &specs::NoImages).dirs;
    let files = specs::decision_files(&fields, "2026-11-26", &dirs, &base).unwrap();
    let check = specs::check(&base, &files, &specs::NoImages);
    must(&mut s, node("a"), Command::ProposeDecision { task_id: "t".into(), decision: fields, files, check });
    let card = specs_of(&s).proposed_decisions[0].card.clone();
    answer(&mut s, card, REJECT, None).unwrap();
    assert_eq!(specs_of(&s).approved, None);
    assert_eq!(specs_of(&s).decisions[0].verdict, DecisionVerdict::Rejected);
    let mut off = running(false);
    let refused = exec(&mut off, node("a"), Command::ProposeDecision { task_id: "t".into(), decision: DecisionFields::default(), files: SpecFiles::new(), check: SpecCheck::default() });
    assert_eq!(refused.unwrap_err().code, "specs_disabled");
}

#[test]
fn a_task_without_specs_records_none() {
    let mut s = running(true);
    must(&mut s, Actor::System, plan_card());
    answer(&mut s, plan_id(), Hold::APPROVE, None).unwrap();
    assert!(specs_of(&s).is_default(), "a project without specs keeps nothing of them");
}

#[test]
fn the_author_may_find_no_change_is_needed_and_the_implementer_carries_on() {
    let mut s = implementing();
    must(&mut s, node("b"), Command::RequestSpecChange { task_id: "t".into(), reason: "Is a refund of zero covered?".into() });
    begin(&mut s, "stage-2").unwrap();
    must(&mut s, Actor::System, Command::RecordHostEnded { task_id: "t".into(), generation: 2, resumes: false, ended_work: vec![], by_alondra: true });
    admit(&mut s, "c", 3);
    must(&mut s, node("c"), Command::ProposeAmendment { task_id: "t".into(), amendment_id: "a1".into(), files: vec![], check: SpecCheck::default() });
    assert_eq!(specs_of(&s).amendment("a1").unwrap().state, AmendmentState::Rejected, "the request is settled, so a new one can be made");
    assert_eq!(s.tasks[&TaskId::from("t")].stages.handover.as_ref().map(|h| h.to), Some(ExecutionRole::Implementer));
    assert!(told(&s).iter().any(|text| text.contains("already cover")));
    assert!(s.interactions.keys().all(|id| !id.0.starts_with(crate::spec_card::AMENDMENT_PREFIX)), "no card: nothing changes");
}
