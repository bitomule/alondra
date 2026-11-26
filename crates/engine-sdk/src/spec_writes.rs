//! Alondra writes the spec files the user approved into a task's worktree, and only while no agent
//! of the task works: it has no execution, or its execution rests with no turn, no work of its
//! own, no decision it waits on and no input waiting for it. The write and its record are one
//! owner transaction, so nothing reaches the execution in between. A change of role waits for
//! them, so a new implementer always starts from the approved files.
use crate::domain_store::{Attached, OwnerJournal as _, StoreError};
use alondra_domain::ActivityView;
use alondra_domain_model::*;
use alondra_engine_ports::Runtime;
use alondra_engine_ports::kept_images::HomeImages;
use alondra_engine_ports::spec_worktrees::FileBytes;
use alondra_domain_model::specs::{self, KeptImages, SpecContent};
use alondra_foundation::log;
use alondra_store::AuthorityScope;

/// Whether no agent of `task` works now nor is about to: the inputs a change of role owes are the
/// next execution's, never the resting one's.
fn resting(state: &DomainState, task: &Task) -> bool {
    let Ok(lifecycle) = state.lifecycle_of(&task.id) else { return false };
    if lifecycle.execution().is_none() {
        return true;
    }
    lifecycle.turn().is_none()
        && !matches!(lifecycle.activity(), ActivityView::Working | ActivityView::OwnWork)
        && lifecycle.cards().iter().all(|(card, _)| lifecycle.card_callback(card).is_none())
        && (task.stages.handover.is_some() || lifecycle.pending_inputs().is_empty())
}

/// The bytes of each file Alondra owes: a mockup's come from the copy it keeps.
fn bytes_of(due: &specs::SpecFiles, images: &dyn KeptImages) -> Result<FileBytes, String> {
    due.iter().map(|(path, content)| {
        let bytes = match content {
            None => None,
            Some(SpecContent::Text(text)) => Some(text.clone().into_bytes()),
            Some(SpecContent::Mockup(image)) => Some(images.read(image).ok_or_else(|| format!("the copy of the mockup {path} is gone from Alondra's files"))?),
        };
        Ok((path.clone(), bytes))
    }).collect()
}

fn envelope(state: &DomainState, id: &str, command: Command) -> CommandEnvelope {
    CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(), expected_revision: 0, command }
}

/// Writes what each resting task is owed and records it. True when it wrote anything.
pub fn write_due(domain: &dyn Runtime) -> bool {
    let Some(port) = domain.ports().spec_worktrees() else { return false };
    let owed: Vec<TaskId> = domain.state().tasks.values().filter(|task| !task.over() && !task.specs.due().is_empty()).map(|task| task.id.clone()).collect();
    let mut wrote = false;
    for task_id in owed {
        let Ok(scope) = AuthorityScope::new(domain.state().data_epoch.clone()) else { return wrote };
        let written = domain.own(scope, |owner| {
            let state = owner.domain_state().clone();
            let Some(task) = state.tasks.get(&task_id).filter(|task| resting(&state, task)) else { return Ok(None) };
            let due = task.specs.due();
            let Some(root) = port.own(&state, &task_id).filter(|_| !due.is_empty()) else { return Ok(None) };
            let unwritten = |error: String| StoreError::Domain(DomainError::new("specs_unwritten", error));
            let bytes = bytes_of(&due, &HomeImages::new(domain.home())).map_err(unwritten)?;
            let written = port.write(&root, &bytes).map_err(unwritten)?;
            let id = format!("specs-written-{}-{}", task_id, state.revision);
            owner.stage_command(&Actor::System, &envelope(&state, &id, Command::RecordSpecsWritten { task_id: task_id.clone(), written }), Attached::None)?;
            Ok(Some(due.into_keys().collect::<Vec<_>>()))
        });
        match written {
            Ok((Some(paths), _)) => {
                log!("{task_id} has its approved specs in its worktree: {}", paths.join(", "));
                wrote = true;
            }
            Ok((None, _)) => {}
            Err(error) => log!("the approved specs of {task_id} could not be written: {error:?}"),
        }
    }
    wrote
}

/// Records, for each task whose plan waits for the user or that has specs, whether the settings of
/// its worktree can be read: a plan is not approved, nor a delivery taken, while they cannot.
pub fn note_settings(domain: &dyn Runtime) {
    let Some(port) = domain.ports().spec_worktrees() else { return };
    let state = domain.state();
    for task in state.tasks.values().filter(|task| !task.over()) {
        let concerned = state.open_plan_card(&task.id).is_some() || task.specs.draft.is_some() || task.specs.approved.is_some();
        let Some(root) = port.own(&state, &task.id).filter(|_| concerned) else { continue };
        let problem = match port.settings(&root) {
            Ok(bytes) => specs::settings_problem(bytes.as_deref()),
            Err(error) => Some(SpecProblem::new("unreadable_file", specs::SETTINGS_PATH, 0, error)),
        };
        if problem == task.specs.settings {
            continue;
        }
        let id = format!("spec-settings-{}-{}", task.id, state.revision);
        if let Err(error) = domain.execute_as_system(envelope(&state, &id, Command::RecordSpecSettings { task_id: task.id.clone(), problem })) {
            log!("what the settings of {} say could not be recorded: {error:?}", task.id);
        }
    }
}
