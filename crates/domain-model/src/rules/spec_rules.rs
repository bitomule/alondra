use super::*;
use crate::spec_card::{self, SpecCard, AMENDMENT_PREFIX, DECISION_PREFIX};
use crate::specs::{self, AmendmentState, DecisionAnswer, DecisionFields, DecisionVerdict, ProposedDecision, SpecAmendment, SpecCheck, SpecDraft, SpecFile, SpecFiles, SpecLevel, SpecMode, SpecProblem, MAX_SPEC_REASON};
use crate::{stage_input, ExecutionRole, HandoverCause, RoleTool, StageHandover};

/// Specs exist only where the task's project turns them on.
const SPECS_OFF: &str = "This project does not use specs: its alondra.toml does not turn them on, so there is nothing to propose";

impl DomainState {
    /// The task's plan card the user has still to decide on.
    pub fn open_plan_card(&self, task: &TaskId) -> Option<&Interaction> {
        self.interactions.values().find(|i| self.holds.get(&i.id) == Some(&Hold::Plan) && !i.withdrawn
            && matches!(&i.scope, Scope::Task { task_id } if task_id == task)
            && matches!(i.payload, InteractionPayload::Question { answer: None, .. }))
    }

    fn proposal_files(level: SpecLevel, files: &[SpecFile]) -> Result<SpecFiles> {
        specs::proposal(level, files).map_err(|(field, message)| refusal("invalid_input", &field, &message))
    }

    /// What the engine found wrong in the worktree with a proposal over it, as a refusal.
    fn check_refusal(check: &SpecCheck) -> Result<()> {
        match check.mode {
            SpecMode::Off => refuse("specs_disabled", "files", SPECS_OFF),
            SpecMode::Unreadable => refuse("spec_settings_unreadable", "files", &format!(
                "The project's alondra.toml cannot be read, so no spec can be proposed or approved until it is fixed:\n{}", check.summary())),
            _ if !check.problems.is_empty() => refuse("specs_unreadable", "files", &format!(
                "The engine cannot read the specs with this proposal; fix each problem and send the whole proposal again:\n{}", check.summary())),
            _ => Ok(()),
        }
    }

    /// The author proposes the specs that go with its plan: the plan's card shows them, the user
    /// edits them there, and approving the plan approves this exact set.
    pub(super) fn propose_specs(&mut self, actor: &Actor, task_id: &TaskId, level: SpecLevel, files: &[SpecFile], check: &SpecCheck) -> Result<()> {
        self.role_allows(actor, task_id, RoleTool::ProposeSpecs)?;
        let task = self.assigned(actor, task_id)?;
        if !task.stages.staged {
            return refuse("specs_need_stages", "task_id",
                "Specs are proposed by the author of a task in stages, and this task has none. Deliver without changing any spec; if the work needs new specs, tell the orchestrator with ask so it plans a task in stages for them");
        }
        if task.specs.approved.as_ref().is_some_and(|approved| self.holds.get(&approved.card) == Some(&Hold::Plan)) {
            return refuse("specs_approved", "task_id",
                "The user already approved this task's specs: a change to them goes through request_spec_change, never a new proposal");
        }
        if self.open_plan_card(task_id).is_some() {
            return refuse("plan_open", "task_id",
                "The user is deciding on your plan: specs go before the plan. To change them, wait for the user's answer, propose them again and present the plan again");
        }
        let files = Self::proposal_files(level, files)?;
        Self::check_refusal(check)?;
        let revision = self.revision + 1;
        let specs = &mut self.tasks.get_mut(task_id).expect("assigned").specs;
        specs.settings = None;
        specs.draft = Some(SpecDraft { level, files, check: check.clone(), revision, edited: false });
        Ok(())
    }

    /// The user edits the proposal on the plan's card: `expected` is the revision of the proposal
    /// they edited, so an edit never overwrites one they did not see.
    pub(super) fn revise_specs(&mut self, actor: &Actor, task_id: &TaskId, expected: Revision, level: SpecLevel, files: &[SpecFile], check: &SpecCheck) -> Result<()> {
        let Actor::User { .. } = actor else {
            return fail("forbidden", "Only the user edits the specs on the plan's card");
        };
        let task = self.active(task_id)?;
        let Some(draft) = task.specs.draft.clone() else {
            return refuse("no_specs", "task_id", "This task has no proposed specs to edit");
        };
        let Some(card) = self.open_plan_card(task_id).map(|card| card.id.clone()) else {
            return refuse("plan_not_open", "task_id", "The specs are edited on the plan's card while it waits for you");
        };
        if draft.revision != expected {
            return refuse("stale_specs", "expected", "The specs changed since you opened them: look at them again");
        }
        let files = Self::proposal_files(level, files)?;
        Self::check_refusal(check)?;
        let removed = specs::removed_decisions(&draft, &files);
        let revision = self.revision + 1;
        let specs = &mut self.tasks.get_mut(task_id).expect("active").specs;
        for id in removed {
            specs.answered(DecisionAnswer { id, verdict: DecisionVerdict::Rejected, card: card.clone(), words: None, revision });
        }
        specs.settings = None;
        specs.draft = Some(SpecDraft { level, files, check: check.clone(), revision, edited: true });
        Ok(())
    }

    /// What Alondra last read of the settings of the task's worktree.
    pub(super) fn record_spec_settings(&mut self, actor: &Actor, task_id: &TaskId, problem: Option<&SpecProblem>) -> Result<()> {
        system(actor)?;
        self.task(task_id)?;
        self.tasks.get_mut(task_id).expect("checked above").specs.settings = problem.cloned();
        Ok(())
    }

    /// Why the plan of `task` cannot be approved for its specs, if it cannot: the settings of its
    /// worktree cannot be read, or its proposal holds what the engine cannot read.
    pub(super) fn plan_specs_refusal(&self, task_id: &TaskId) -> Result<()> {
        let Some(task) = self.tasks.get(task_id) else { return Ok(()) };
        if let Some(problem) = &task.specs.settings {
            return refuse("spec_settings_unreadable", "option_id", &format!(
                "The plan cannot be approved while the project's alondra.toml cannot be read: {}", problem.located()));
        }
        if let Some(draft) = &task.specs.draft
            && !draft.check.clean()
        {
            return refuse("specs_unreadable", "option_id", &format!("The plan's specs cannot be read:\n{}", draft.check.summary()));
        }
        Ok(())
    }

    /// The user approved the plan on `card`: its proposal becomes the task's approved specs, with
    /// the decisions it proposes accepted in the user's words.
    pub(super) fn approve_plan_specs(&mut self, task_id: &TaskId, card: &InteractionId, words: Option<&str>) {
        let revision = self.revision + 1;
        let Some(task) = self.tasks.get_mut(task_id) else { return };
        let Some(draft) = task.specs.draft.take() else { return };
        let dirs = draft.check.dirs.clone();
        for (_, fields) in specs::draft_decisions(&draft.files, &dirs) {
            task.specs.answered(DecisionAnswer { id: fields.id, verdict: DecisionVerdict::Accepted, card: card.clone(), words: words.map(str::to_owned), revision });
        }
        let files = specs::accepted(&draft.files, &dirs);
        task.specs.approve(card, revision, draft.level, draft.check.mode, &dirs, &files, &draft.check.before);
    }

    /// Alondra wrote these approved files into the task's worktree while no agent worked. The
    /// execution working on the task is told to commit them, unless a new one starts from them.
    pub(super) fn record_specs_written(&mut self, actor: &Actor, task_id: &TaskId, written: &BTreeMap<String, String>, lifecycles: &mut Lifecycles) -> Result<()> {
        system(actor)?;
        self.task(task_id)?;
        let task = self.tasks.get_mut(task_id).expect("checked above");
        let done = task.specs.written(written);
        if done.is_empty() || task.stages.handover.is_some() {
            return Ok(());
        }
        let text = format!(
            "Alondra wrote into your worktree the spec changes the user approved: {}. Commit them exactly as they are, in a commit of their own; never edit them.",
            done.join(", "));
        self.tell(task_id, text, lifecycles)
    }

    /// Queues `text` for the task's current execution, if it has one that takes inputs.
    fn tell(&mut self, task_id: &TaskId, text: String, lifecycles: &mut Lifecycles) -> Result<()> {
        let Some(execution) = self.lifecycle(lifecycles, task_id)?.execution().cloned() else { return Ok(()) };
        if execution.host().as_str() == alondra_domain::CLAUDE_BG_HOST || self.tasks.get(task_id).is_none_or(|task| task.over()) {
            return Ok(());
        }
        let input = self.next_input(task_id);
        self.decide(lifecycles, task_id, LifecycleRecord::Told { input, text: Some(text), answers: None })
    }

    /// The implementer found something the approved specs do not cover: the author drafts the
    /// change once the implementer rests, and the user decides on it.
    pub(super) fn request_spec_change(&mut self, actor: &Actor, task_id: &TaskId, reason: &str, lifecycles: &mut Lifecycles) -> Result<()> {
        self.role_allows(actor, task_id, RoleTool::RequestSpecChange)?;
        let task = self.assigned(actor, task_id)?;
        nonempty(reason)?;
        if reason.chars().count() > MAX_SPEC_REASON {
            return refuse("invalid_input", "reason", &format!("Say why in at most {MAX_SPEC_REASON} characters"));
        }
        if task.specs.approved.is_none() {
            return refuse("no_approved_specs", "task_id", "This task has no approved specs to change: deliver what the plan asks, or ask the user");
        }
        if task.stages.role() != Some(ExecutionRole::Implementer) {
            return refuse("specs_need_stages", "task_id", "Only the implementer of a task in stages asks for a change of its specs");
        }
        if task.specs.amendments.iter().any(|a| matches!(a.state, AmendmentState::Requested | AmendmentState::Proposed)) || task.stages.handover.is_some() {
            return refuse("spec_change_pending", "task_id",
                "A change of the specs is already on its way: the author drafts it or the user decides on it. Carry on with what does not depend on it, and ask again once it is settled");
        }
        let next = task.specs.amendments.iter().filter_map(|a| a.id.strip_prefix('a')?.parse::<u64>().ok()).max().unwrap_or(0) + 1;
        let id = format!("a{next}");
        let task = self.tasks.get_mut(task_id).expect("assigned");
        task.specs.amended(SpecAmendment { id: id.clone(), reason: reason.trim().to_owned(), state: AmendmentState::Requested, files: SpecFiles::new(), before: BTreeMap::new(), card: None, words: None });
        task.stages.handover = Some(StageHandover { to: ExecutionRole::Author, cause: HandoverCause::SpecChangeRequested { amendment: id }, transition: None, attempts: 0, refused: false });
        let input = self.next_input(task_id);
        self.decide(lifecycles, task_id, LifecycleRecord::Told { input, text: Some(stage_input(ExecutionRole::Author)), answers: None })
    }

    /// The author drafted the change the implementer asked for: the user gets a card with what
    /// each file held and what it would hold, and the implementer carries on meanwhile.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn propose_amendment(&mut self, actor: &Actor, task_id: &TaskId, amendment_id: &str, files: &[SpecFile], check: &SpecCheck,
        effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
        self.role_allows(actor, task_id, RoleTool::ProposeAmendment)?;
        let task = self.assigned(actor, task_id)?;
        let Some(requested) = task.specs.amendment(amendment_id).filter(|a| a.state == AmendmentState::Requested).cloned() else {
            return refuse("no_spec_change", "amendment_id", "No change of the specs with this id waits for a draft: draft the one the implementer asked for");
        };
        let Some(approved) = task.specs.approved.clone() else {
            return refuse("no_approved_specs", "task_id", "This task has no approved specs to change");
        };
        if files.is_empty() {
            let task = self.tasks.get_mut(task_id).expect("assigned");
            let amendment = task.specs.amendments.iter_mut().find(|a| a.id == requested.id).expect("found above");
            amendment.state = AmendmentState::Rejected;
            amendment.words = Some("The author found the approved specs already cover it: nothing changes".into());
            task.stages.handover = Some(StageHandover { to: ExecutionRole::Implementer, cause: HandoverCause::AmendmentProposed { amendment: requested.id.clone() }, transition: None, attempts: 0, refused: false });
            let input = self.next_input(task_id);
            let text = format!("{} The author found that the approved specs already cover what you asked («{}»): they stay as they are.", stage_input(ExecutionRole::Implementer), requested.reason);
            return self.decide(lifecycles, task_id, LifecycleRecord::Told { input, text: Some(text), answers: None });
        }
        let files = Self::proposal_files(SpecLevel::Light, files)?;
        Self::check_refusal(check)?;
        let before: BTreeMap<String, Option<String>> = files.keys()
            .map(|path| (path.clone(), approved.files.get(path).cloned().unwrap_or_else(|| check.before.get(path).cloned().flatten())))
            .collect();
        let title = task.title.clone();
        let card_id = InteractionId(format!("{AMENDMENT_PREFIX}{}-{}", task_id.0, requested.id));
        let files = specs::accepted(&files, &check.dirs);
        let task = self.tasks.get_mut(task_id).expect("assigned");
        let amendment = task.specs.amendments.iter_mut().find(|a| a.id == requested.id).expect("found above");
        amendment.state = AmendmentState::Proposed;
        amendment.files = files;
        amendment.before = before;
        amendment.card = Some(card_id.clone());
        let body = SpecCard::Amendment { task: task_id.0.clone(), id: requested.id.clone() };
        self.raise_spec_card(&card_id, task_id, &title, &body, effects, created)?;
        let task = self.tasks.get_mut(task_id).expect("assigned");
        task.stages.handover = Some(StageHandover { to: ExecutionRole::Implementer, cause: HandoverCause::AmendmentProposed { amendment: requested.id }, transition: None, attempts: 0, refused: false });
        let input = self.next_input(task_id);
        self.decide(lifecycles, task_id, LifecycleRecord::Told { input, text: Some(stage_input(ExecutionRole::Implementer)), answers: None })
    }

    /// The author proposes a lasting decision on a card of its own; Alondra writes it once the
    /// user accepts it, with any decision it replaces marked as replaced.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn propose_decision(&mut self, actor: &Actor, task_id: &TaskId, fields: &DecisionFields, files: &SpecFiles, check: &SpecCheck,
        effects: &mut Vec<DomainEffect>, created: &mut Vec<String>) -> Result<()> {
        self.role_allows(actor, task_id, RoleTool::ProposeDecision)?;
        let task = self.assigned(actor, task_id)?;
        Self::check_refusal(check)?;
        let Some(path) = check.dirs.decision_path(&fields.id) else {
            return refuse("decisions_disabled", "decision", "This project does not keep decisions: its alondra.toml has no [decisions] enabled");
        };
        if files.get(&path).is_none_or(Option::is_none) || files.len() > specs::MAX_SPEC_FILES {
            return refuse("invalid_input", "decision", "Alondra could not write this decision's file");
        }
        if fields.title.trim().is_empty() {
            return refuse("invalid_input", "decision.title", "A decision has a title");
        }
        if task.specs.proposed_decisions.iter().any(|d| d.id == fields.id) {
            return refuse("decision_pending", "decision.id", "This decision already waits for the user on its card");
        }
        let card_id = InteractionId(format!("{DECISION_PREFIX}{}-{}", task_id.0, fields.id));
        if self.interactions.contains_key(&card_id) {
            return refuse("decision_pending", "decision.id", "The user already answered a decision with this id in this task: choose another id");
        }
        let title = task.title.clone();
        let proposed = ProposedDecision { id: fields.id.clone(), title: fields.title.trim().to_owned(), card: card_id.clone(), files: files.clone(), before: check.before.clone(), dirs: check.dirs.clone(), mode: check.mode };
        self.tasks.get_mut(task_id).expect("assigned").specs.proposed_decisions.push(proposed);
        let body = SpecCard::Decision { task: task_id.0.clone(), id: fields.id.clone() };
        self.raise_spec_card(&card_id, task_id, &title, &body, effects, created)
    }

    fn raise_spec_card(&mut self, card_id: &InteractionId, task_id: &TaskId, title: &str, body: &SpecCard, effects: &mut Vec<DomainEffect>, created: &mut Vec<String>) -> Result<()> {
        let lang = alondra_core::text::Lang::En;
        let card = CardSpec { system: Some(SystemCard::SpecChange), ..body.card(title, &self.tasks[task_id].specs, lang) };
        let options = body.options(lang);
        self.insert_interaction(&Actor::System, card_id, &Scope::Task { task_id: task_id.clone() }, &Recipient::User, &body.to_body(), false, &[],
            InteractionPayload::Question { options, answer: None }, &card, Source::Question)?;
        created.push(card_id.0.clone());
        effects.push(DomainEffect::NotifyUser { task_id: Some(task_id.clone()), interaction_id: card_id.clone() });
        Ok(())
    }

    /// The user decided on one of Alondra's spec cards: an approved change or accepted decision
    /// joins the approved specs, to be written while no agent works; the task's agent is told.
    pub(super) fn settle_spec_card(&mut self, card: &InteractionId, option: &str, words: Option<&str>, lifecycles: &mut Lifecycles) -> Result<()> {
        let Some(i) = self.interactions.get(card).filter(|i| i.system_card == Some(SystemCard::SpecChange) && i.author == Author::System) else { return Ok(()) };
        let Scope::Task { task_id } = i.scope.clone() else { return Ok(()) };
        let Some(body) = SpecCard::from_body(&i.body) else { return Ok(()) };
        if self.tasks.get(&task_id).is_none_or(Task::over) {
            return Ok(());
        }
        let approved = option == spec_card::APPROVE;
        let revision = self.revision + 1;
        let words = words.map(str::trim).filter(|words| !words.is_empty()).map(str::to_owned);
        let said = words.as_deref().map(|words| format!(" The user wrote: «{words}»")).unwrap_or_default();
        let task = self.tasks.get_mut(&task_id).expect("checked above");
        let text = match body {
            SpecCard::Amendment { id, .. } => {
                let Some(amendment) = task.specs.amendments.iter_mut().find(|a| a.id == id && a.state == AmendmentState::Proposed) else { return Ok(()) };
                amendment.state = if approved { AmendmentState::Approved } else { AmendmentState::Rejected };
                amendment.words = words.clone();
                let (files, before, reason) = (amendment.files.clone(), amendment.before.clone(), amendment.reason.clone());
                if approved {
                    if let Some(specs) = task.specs.approved.as_mut() {
                        specs.merge(&files, &before);
                    }
                    format!("The user approved the change of the specs you asked for («{reason}»).{said}")
                } else {
                    format!("The user kept the approved specs: the change you asked for («{reason}») is not made. Carry on within the approved specs.{said}")
                }
            }
            SpecCard::Decision { id, .. } => {
                let Some(index) = task.specs.proposed_decisions.iter().position(|d| d.id == id) else { return Ok(()) };
                let proposed = task.specs.proposed_decisions.remove(index);
                let verdict = if approved { DecisionVerdict::Accepted } else { DecisionVerdict::Rejected };
                task.specs.answered(DecisionAnswer { id: id.clone(), verdict, card: card.clone(), words: words.clone(), revision });
                if approved {
                    let files = specs::accepted(&proposed.files, &proposed.dirs);
                    task.specs.approve(card, revision, SpecLevel::None, proposed.mode, &proposed.dirs, &files, &proposed.before);
                    format!("The user accepted the decision «{}».{said}", proposed.title)
                } else {
                    format!("The user rejected the decision «{}»: nothing is written.{said}", proposed.title)
                }
            }
        };
        if approved && !task.specs.due().is_empty() {
            return Ok(());
        }
        self.tell(&task_id, text, lifecycles)
    }

    /// Whether Alondra still owes the task's worktree approved spec files: no change of role
    /// starts until they are written.
    pub fn specs_due(&self, task_id: &TaskId) -> bool {
        self.tasks.get(task_id).is_some_and(|task| !task.specs.due().is_empty())
    }
}

fn refusal(code: &str, field: &str, message: &str) -> DomainError {
    refuse::<()>(code, field, message).expect_err("a refusal is an error")
}
