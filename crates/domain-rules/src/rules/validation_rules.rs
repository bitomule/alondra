use super::*;
use alondra_domain_model::spec_card::{self, SpecCard, VALIDATION_PREFIX};
use alondra_domain_model::specs::{self, AmendmentState, SpecAmendment, SpecFiles};
use alondra_domain_model::validation::{self, DeliveredValidation, RoundItem, RoundState, ValidationItem, ValidationKind, ValidationRecord, ValidationRound, ValidationState, ValidationVerdict,
MAX_ROUND_ITEMS, MAX_SCOPE_FILES, MAX_VALIDATION_OUTPUT, MAX_VALIDATION_ROUNDS, MAX_VALIDATION_TEXT};
use alondra_domain_model::{stage_input, ExecutionRole, HandoverCause, RoleTool, StageHandover};

/// What the validator says of one validation, as `record_validation` takes it.
pub(crate) struct Recorded<'a> {
pub subject: &'a str,
pub kind: ValidationKind,
pub verdict: ValidationVerdict,
pub reasoning: Option<&'a str>,
pub nearest_wrong: Option<&'a str>,
pub output: Option<&'a str>,
pub evidence: &'a [String],
pub inputs: &'a str,
/// What the spec engine lists on the round's commit, as Alondra read it.
pub listed: Option<&'a [ValidationItem]>,
}

fn handover(to: ExecutionRole, cause: HandoverCause) -> StageHandover {
StageHandover { to, cause, transition: None, attempts: 0, refused: false }
}

fn bounded(field: &str, text: Option<&str>) -> Result<Option<String>> {
let Some(text) = text.map(str::trim).filter(|text| !text.is_empty()) else { return Ok(None) };
if text.chars().count() > MAX_VALIDATION_TEXT {
    return refuse("invalid_input", field, &format!("Say it in at most {MAX_VALIDATION_TEXT} characters"));
}
Ok(Some(text.to_owned()))
}

fn open_validation_round<'a>(domain: &'a DomainState, task_id: &TaskId) -> Result<&'a ValidationRound> {
    match domain.tasks.get(task_id).and_then(|task| task.validation.current()) {
        Some(round) if round.state == RoundState::Validating => Ok(round),
        _ => refuse("no_validation", "task_id",
            "No round of validation is open for you: the implementer says the work is ready with ready_for_validation, and a validator records only inside that round"),
    }
}

/// The implementer says its work is ready: a new execution, the validator, checks what the
/// spec engine lists on its commit.
pub(crate) fn ready_for_validation(domain: &mut DomainState, actor: &Actor, task_id: &TaskId, head: &str, items: &[ValidationItem], scope: &[String], lifecycles: &mut Lifecycles) -> Result<()> {
    role_allows(domain, actor, task_id, RoleTool::ReadyForValidation)?;
    let task = domain.assigned(actor, task_id)?;
    if task.stages.role() != Some(ExecutionRole::Implementer) {
        return refuse("validation_needs_stages", "task_id",
            "Only the implementer of a task in stages hands its work to a validator. Deliver with present_result as usual");
    }
    if task.stages.handover.is_some() {
        return refuse("handover_pending", "task_id", "This task already passes to another agent: end your turn");
    }
    if task.specs.amendments.iter().any(|a| matches!(a.state, AmendmentState::Requested | AmendmentState::Proposed)) {
        return refuse("spec_change_pending", "task_id",
            "A change of the specs waits for the user: the work is validated against the specs the user decides, so carry on with what does not depend on it and say it is ready once it is settled");
    }
    if task.validation.exhausted.is_some() {
        return refuse("validation_exhausted", "task_id", "The user decides how this task goes on after its last round of validation failed: wait for that answer");
    }
    if !is_commit_sha(head) {
        return refuse("worktree_unreadable", "task_id", "Alondra could not read the commit of your worktree: commit your work and call ready_for_validation again");
    }
    if items.is_empty() {
        return refuse("nothing_to_validate", "task_id", &format!(
            "The spec engine lists nothing to validate on {head}: every scenario, suite and section is valid there. Deliver with present_result as usual"));
    }
    if items.len() > MAX_ROUND_ITEMS || items.iter().any(|item| item.subject.trim().is_empty() || item.inputs.trim().is_empty()) {
        return refuse("invalid_input", "items", "Alondra could not read what the spec engine lists for this commit");
    }
    let number = task.validation.rounds.last().map_or(1, |round| round.number + 1);
    let round = ValidationRound {
        number,
        head: head.to_owned(),
        implemented: head.to_owned(),
        items: items.iter().map(|item| RoundItem { subject: item.subject.clone(), kind: item.kind, inputs: item.inputs.clone(), state: ValidationState::Running, reason: None }).collect(),
        scope: scope.iter().take(MAX_SCOPE_FILES).cloned().collect(),
        state: RoundState::Requested,
        generation: None,
        findings: None,
        retried: false,
    };
    let task = domain.tasks.get_mut(task_id).expect("assigned");
    task.validation.push_round(round);
    task.stages.handover = Some(handover(ExecutionRole::Validator, HandoverCause::ReadyForValidation { round: number }));
    let input = domain.next_input(task_id);
    domain.decide(lifecycles, task_id, LifecycleRecord::Told { input, text: Some(stage_input(ExecutionRole::Validator)), answers: None })
}

/// The validator records what one validation found, at the fingerprint Alondra read.
pub(crate) fn record_validation(domain: &mut DomainState, actor: &Actor, task_id: &TaskId, record: Recorded<'_>) -> Result<()> {
    role_allows(domain, actor, task_id, RoleTool::RecordValidation)?;
    domain.assigned(actor, task_id)?;
    let round = open_validation_round(domain, task_id)?;
    let (number, generation) = (round.number, round.generation.unwrap_or_default());
    nonempty(record.subject)?;
    if record.inputs.trim().is_empty() {
        return refuse("unknown_subject", "subject", &format!(
            "The spec engine has no {} validation of {} on the commit this round validates: use a subject alondra spec validate lists",
            record.kind.as_str(), record.subject));
    }
    let reasoning = bounded("reasoning", record.reasoning)?;
    let nearest_wrong = bounded("nearest_wrong", record.nearest_wrong)?;
    if record.verdict == ValidationVerdict::Incomplete && reasoning.is_none() {
        return refuse("invalid_input", "reasoning", "Say in reasoning why it could not run: a missing tool, a time limit, an error before the tests started");
    }
    if record.kind.judgment() && record.verdict != ValidationVerdict::Incomplete && reasoning.is_none() {
        return refuse("invalid_input", "reasoning", "A judgment carries its reasoning: what you looked at and why it holds or does not");
    }
    if record.kind == ValidationKind::Adequate && record.verdict == ValidationVerdict::Met && nearest_wrong.is_none() {
        return refuse("invalid_input", "nearest_wrong",
            "Name in nearest_wrong the nearest wrong behaviour these tests would still accept: it shows how far the tests reach");
    }
    if record.evidence.len() > 32 || record.evidence.iter().any(|path| specs::path_problem(path).is_some()) {
        return refuse("invalid_input", "evidence", "Evidence is at most 32 paths of committed files, relative to the repository root");
    }
    let output = record.output.map(str::trim).filter(|output| !output.is_empty()).map(|output| validation::tail(output, MAX_VALIDATION_OUTPUT));
    let revision = domain.revision + 1;
    let task = domain.tasks.get_mut(task_id).expect("assigned");
    task.validation.push_record(ValidationRecord {
        subject: record.subject.trim().to_owned(), kind: record.kind, inputs: record.inputs.to_owned(), verdict: record.verdict, round: number, generation,
        reasoning, nearest_wrong, output, evidence: record.evidence.to_vec(), revision,
    });
    let round = task.validation.current_mut().expect("open above");
    if let Some(listed) = record.listed.filter(|listed| listed.len() <= MAX_ROUND_ITEMS) {
        round.items = listed.iter().map(|item| round.items.iter()
            .find(|known| known.subject == item.subject && known.kind == item.kind && known.inputs == item.inputs).cloned()
            .unwrap_or_else(|| RoundItem { subject: item.subject.clone(), kind: item.kind, inputs: item.inputs.clone(), state: ValidationState::Running, reason: None }))
            .collect();
    }
    if let Some(item) = round.item_mut(record.subject.trim(), record.kind).filter(|item| item.inputs == record.inputs) {
        item.state = match record.verdict {
            ValidationVerdict::Met => ValidationState::Met,
            ValidationVerdict::Unmet => ValidationState::Unmet,
            ValidationVerdict::Incomplete => ValidationState::Incomplete,
        };
        item.reason = (record.verdict == ValidationVerdict::Incomplete).then(|| record.reasoning.unwrap_or_default().trim().to_owned());
    }
    Ok(())
}

/// The validator ends its round without delivering. What failed goes back to the implementer,
/// up to three rounds in a row, and so do commits the PR received after the implementer handed it
/// over; what it could not run goes to one new validator, then to the user.
pub(crate) fn end_validation(domain: &mut DomainState, actor: &Actor, task_id: &TaskId, findings: &str,
    effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
    role_allows(domain, actor, task_id, RoleTool::EndValidation)?;
    domain.assigned(actor, task_id)?;
    let round = open_validation_round(domain, task_id)?;
    let Some(findings) = bounded("findings", Some(findings))? else {
        return refuse("invalid_input", "findings", "Say what failed and why, so the implementer can fix it");
    };
    let unmet = round.items.iter().any(|item| item.state == ValidationState::Unmet);
    let owed = round.owed().count() > 0;
    let moved = round.head != round.implemented;
    if !unmet && !owed && !moved {
        return refuse("nothing_failed", "task_id",
            "Every validation of this round is met: commit only the ledger, push, and deliver with present_result");
    }
    let (number, retried) = (round.number, round.retried);
    let task = domain.tasks.get_mut(task_id).expect("assigned");
    let round = task.validation.current_mut().expect("open above");
    round.findings = Some(findings.clone());
    if unmet || moved {
        round.state = RoundState::Failed;
        if unmet {
            task.validation.failed = task.validation.failed.saturating_add(1);
        }
        if unmet && task.validation.failed >= MAX_VALIDATION_ROUNDS {
            let card_id = InteractionId(format!("{VALIDATION_PREFIX}{}-{number}", task_id.0));
            task.validation.exhausted = Some(card_id.clone());
            let specs = task.specs.approved.is_some();
            let body = SpecCard::ValidationExhausted { task: task_id.0.clone(), round: number, specs };
            return raise_spec_card(domain, &card_id, task_id, &body, effects, created);
        }
        task.stages.handover = Some(handover(ExecutionRole::Implementer, HandoverCause::ValidationFailed { round: number }));
    } else {
        round.state = RoundState::Interrupted;
        for item in round.items.iter_mut().filter(|item| item.state == ValidationState::Running) {
            item.state = ValidationState::Incomplete;
            item.reason = Some(findings.clone());
        }
        if retried {
            let card_id = InteractionId(format!("{VALIDATION_PREFIX}{}-{number}-stuck", task_id.0));
            if domain.interactions.contains_key(&card_id) {
                return refuse("validation_stuck", "task_id", "The user already decides how this task goes on: end your turn");
            }
            domain.tasks.get_mut(task_id).expect("assigned").validation.exhausted = Some(card_id.clone());
            let body = SpecCard::ValidationStuck { task: task_id.0.clone(), round: number };
            return raise_spec_card(domain, &card_id, task_id, &body, effects, created);
        }
        round.retried = true;
        task.stages.handover = Some(handover(ExecutionRole::Validator, HandoverCause::ValidatorRetry { round: number }));
    }
    let to = domain.tasks[task_id].stages.handover.as_ref().expect("set above").to;
    let input = domain.next_input(task_id);
    domain.decide(lifecycles, task_id, LifecycleRecord::Told { input, text: Some(stage_input(to)), answers: None })
}

/// What Alondra read of a validator's delivery once it passed the gate, for its card.
pub(crate) fn record_delivered_validation(domain: &mut DomainState, actor: &Actor, task_id: &TaskId, delivered: &DeliveredValidation) -> Result<()> {
    system(actor)?;
    domain.task(task_id)?;
    domain.tasks.get_mut(task_id).expect("checked above").validation.push_delivered(delivered.clone());
    Ok(())
}

/// The validator delivered: its round is over.
pub(crate) fn validator_delivered(domain: &mut DomainState, task_id: &TaskId) {
    let Some(task) = domain.tasks.get_mut(task_id) else { return };
    if task.stages.role() != Some(ExecutionRole::Validator) {
        return;
    }
    if let Some(round) = task.validation.current_mut().filter(|round| round.state == RoundState::Validating) {
        round.state = RoundState::Delivered;
        task.validation.failed = 0;
    }
}

/// The PR of the validator's delivery received a commit after it: the same validator validates
/// again on that commit, and only what changed fingerprint is owed. Code in that commit is not the
/// implementer's: the validator never delivers it, it ends the round so the implementer takes it.
pub(crate) fn validated_head_moved(domain: &mut DomainState, task_id: &TaskId, head: &str) -> Option<String> {
    let task = domain.tasks.get_mut(task_id)?;
    if task.stages.role() != Some(ExecutionRole::Validator) || task.stages.handover.is_some() {
        return None;
    }
    let round = task.validation.current_mut().filter(|round| round.state == RoundState::Delivered)?;
    round.head = head.to_owned();
    round.state = RoundState::Validating;
    Some(format!(
        "Your validated pull request received commit {head} after your delivery, so the delivery was withdrawn. Bring that commit into your worktree, run alondra spec validate, \
         validate again only what it lists (record each with record_validation), commit only the ledger, push, and deliver again with present_result. \
         If that commit changed anything but the ledger, end the round with end_validation instead, naming those files: the implementer takes them."))
}

/// The user asked for changes, or the PR has conflicts: an implementer does that work, never the
/// validator.
pub(crate) fn return_to_implementer(domain: &mut DomainState, task_id: &TaskId) {
    let Some(task) = domain.tasks.get_mut(task_id) else { return };
    if task.stages.role() == Some(ExecutionRole::Validator) && task.stages.handover.is_none() {
        task.stages.handover = Some(handover(ExecutionRole::Implementer, HandoverCause::DeliveryReturned));
    }
}

/// The user decided how the task goes on after its validation could not succeed.
pub(crate) fn settle_validation_card(domain: &mut DomainState, actor: &Actor, card: &InteractionId, option: &str, words: Option<&str>,
    effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
    let Some(i) = domain.interactions.get(card).filter(|i| i.system_card == Some(SystemCard::SpecChange) && i.author == Author::System) else { return Ok(()) };
    let Scope::Task { task_id } = i.scope.clone() else { return Ok(()) };
    let Some(body @ (SpecCard::ValidationExhausted { .. } | SpecCard::ValidationStuck { .. })) = SpecCard::from_body(&i.body) else { return Ok(()) };
    let Some(task) = domain.tasks.get(&task_id).filter(|task| !task.over() && task.validation.exhausted.as_ref() == Some(card)) else { return Ok(()) };
    let round = match body {
        SpecCard::ValidationExhausted { round, .. } | SpecCard::ValidationStuck { round, .. } => round,
        _ => unreachable!("matched above"),
    };
    let failing = spec_card::failing(&task.validation, round, alondra_core::text::Lang::En);
    let said = words.map(str::trim).filter(|words| !words.is_empty()).map(|words| format!(" The user wrote: «{words}»")).unwrap_or_default();
    if option == spec_card::CANCEL {
        domain.tasks.get_mut(&task_id).expect("checked above").validation.exhausted = None;
        return domain.close_task(actor, &task_id, None, effects, created, lifecycles);
    }
    let task = domain.tasks.get_mut(&task_id).expect("checked above");
    task.validation.exhausted = None;
    task.validation.failed = 0;
    let next = match option {
        spec_card::STRONGER => {
            if let Some(author) = task.stages.selections.get(&ExecutionRole::Author).cloned() {
                task.stages.selections.insert(ExecutionRole::Implementer, author);
            }
            handover(ExecutionRole::Implementer, HandoverCause::ValidationDecided { round })
        }
        spec_card::CHANGE_SPECS if task.specs.approved.is_some() => {
            let next = task.specs.amendments.iter().filter_map(|a| a.id.strip_prefix('a')?.parse::<u64>().ok()).max().unwrap_or(0) + 1;
            let id = format!("a{next}");
            let reason = clip(&format!("A validator sent the work back three times; the user chose to change the specs these checks rest on.{said}\n\n{failing}"), alondra_domain_model::specs::MAX_SPEC_REASON);
            task.specs.amended(SpecAmendment { id: id.clone(), reason, state: AmendmentState::Requested, files: SpecFiles::new(), before: BTreeMap::new(), check: None, card: None, words: words.map(str::to_owned) });
            handover(ExecutionRole::Author, HandoverCause::SpecChangeRequested { amendment: id })
        }
        spec_card::VALIDATE_AGAIN => {
            if let Some(round) = task.validation.current_mut() {
                round.retried = false;
            }
            handover(ExecutionRole::Validator, HandoverCause::ValidatorRetry { round })
        }
        _ => return Ok(()),
    };
    let to = next.to;
    task.stages.handover = Some(next);
    let input = domain.next_input(&task_id);
    domain.decide(lifecycles, &task_id, LifecycleRecord::Told { input, text: Some(stage_input(to)), answers: None })
}

/// What Alondra read of a commit of the task, kept for the guard of its delivery.
pub(crate) fn record_commit_reading(domain: &mut DomainState, actor: &Actor, task_id: &TaskId, reading: &alondra_domain_model::CommitReading) -> Result<()> {
    system(actor)?;
    domain.task(task_id)?;
    if !is_commit_sha(&reading.commit) || [&reading.since, &reading.base, &reading.tip].into_iter().flatten().any(|commit| !is_commit_sha(commit)) {
        return refuse("invalid_input", "reading", "A reading names full commit shas");
    }
    if reading.base.is_none() && reading.problems.is_empty() {
        return refuse("invalid_input", "reading", "A reading compares the commit with its base, or says why it could not");
    }
    if !reading.bounded() {
        return refuse("invalid_input", "reading", "A reading holds more than a task keeps of one commit");
    }
    domain.tasks.get_mut(task_id).expect("checked above").validation.push_reading(reading.clone());
    Ok(())
}

/// Whether `id` names a scenario of the task: one of its approved specs, or one its validation checked.
pub(crate) fn task_scenario(domain: &DomainState, task_id: &TaskId, id: &str) -> bool {
    let Some(task) = domain.tasks.get(task_id) else { return false };
    let Some((spec, scenario)) = id.split_once('/') else { return false };
    let approved = task.specs.approved.iter().flat_map(|approved| approved.files.iter().map(move |(path, text)| (path, text, &approved.dirs)))
        .filter(|(path, _, dirs)| dirs.kind(path) == Some(alondra_domain_model::SpecFileKind::Spec))
        .filter_map(|(path, text, _)| alondra_domain_model::specs::engine::read_spec(path, text.as_ref()?.text()?.as_bytes()).ok())
        .any(|read| read.id == spec && read.scenarios.iter().any(|s| s.id == scenario));
    approved || task.validation.rounds.iter().flat_map(|round| &round.items).any(|item| item.kind != ValidationKind::Suite && item.subject == id)
        || task.validation.delivered.iter().flat_map(|delivered| &delivered.specs).flat_map(|spec| &spec.scenarios).any(|s| s.id == id)
}
