//! The wait reports of SDK nodes: where each execution's host writes its own, and clearing it once
//! the turn it reported for ends. The host's state folder is the SDK backend's, so they live with
//! the SDK owners; reading a report is the live view's (`sdk_live::read_wait_file`).
use crate::sdk_owner::SdkOwners as _;

pub fn wait_file(domain: &crate::domain_runtime::DomainRuntime, provider: alondra_core::accounts::Provider, instance: &str) -> Option<std::path::PathBuf> {
    let (backend, _) = domain.sdk()?;
    Some(backend.for_provider(provider, domain.home()).ok()?.state_root.join(instance).join("wait.json"))
}

/// Called before publishing a committed turn boundary, including host loss and interruptions.
pub(crate) fn clear_ended_waits(domain: &crate::domain_runtime::DomainRuntime, before: &alondra_core::domain::DomainState, after: &alondra_core::domain::DomainState) {
    if domain.sdk().is_none() { return; }
    for (id, journal) in &before.lifecycle_journal {
        if after.lifecycle_journal.get(id).is_some_and(|new| new.len() == journal.len()) { continue; }
        let Some(task) = before.tasks.get(id) else { continue };
        let Ok(old) = before.lifecycle_of(&task.id) else { continue };
        if old.turn().is_none() { continue; }
        let same_turn = after.lifecycle_of(&task.id).is_ok_and(|new| new.execution() == old.execution() && new.turn() == old.turn());
        if same_turn { continue; }
        let Some(stamp) = old.execution() else { continue };
        if let Some(path) = wait_file(domain, task.account_key().provider, stamp.instance().as_str()) {
            if let Err(error) = std::fs::remove_file(path) && error.kind() != std::io::ErrorKind::NotFound {
                log!("could not clear the ended turn's wait report for {}: {error}", task.id);
            }
        }
    }
}

/// Ends the waits of SDK nodes whose lifecycle a commit moved on.
pub(crate) struct EndedWaits;

impl crate::domain_runtime::CommitObserver for EndedWaits {
    fn published(&self, domain: &crate::domain_runtime::DomainRuntime, before: &alondra_core::domain::DomainState, after: &alondra_core::domain::DomainState) {
        clear_ended_waits(domain, before, after);
    }
}
