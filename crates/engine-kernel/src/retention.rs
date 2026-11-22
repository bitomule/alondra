//! What the store keeps only while a reader needs it: raw SDK observations, and the rows of each
//! table retention policy. This loop releases the rest and gives the disk back the space, away from
//! startup.
use alondra_core::clock;
use crate::domain_runtime::DomainRuntime;
use std::sync::Arc;
use std::time::Duration;

const FIRST_AFTER: Duration = Duration::from_secs(60);
const EVERY: Duration = Duration::from_secs(60 * 60);

pub async fn run(domain: Arc<DomainRuntime>) {
    clock::sleep(FIRST_AFTER).await;
    loop {
        let pass = {
            let domain = domain.clone();
            tokio::task::spawn_blocking(move || {
                let expired = remove_expired_files(&domain);
                (domain.retain(alondra_core::now_millis()), expired)
            }).await
        };
        let pass = match pass {
            Ok((retained, expired)) => {
                match expired {
                    Ok(removed) if removed.entries > 0 => log!("removed {} expired entries of the files folder, {} bytes", removed.entries, removed.bytes),
                    Ok(_) => {}
                    Err(error) => log!("could not sweep the files folder: {error}"),
                }
                Ok(retained)
            }
            Err(error) => Err(error),
        };
        match pass {
            Ok(Ok(retained)) => {
                if retained.evidence.released + retained.evidence.removed > 0 {
                    log!("released {} normalized SDK observations and removed {} of finished executions", retained.evidence.released, retained.evidence.removed);
                }
                let evidence = retained.evidence;
                if evidence.commands_removed + evidence.texts_released > 0 {
                    log!("removed {} provider commands of ended executions and released {} input texts of ended tasks", evidence.commands_removed, evidence.texts_released);
                }
                if retained.compacted > 0 {
                    log!("compacted the cards of {} ended tasks", retained.compacted);
                }
                if retained.folded > 0 {
                    log!("folded the metrics of {} ended tasks", retained.folded);
                }
                for (policy, changed) in retained.tables.iter().filter(|(_, changed)| *changed > 0) {
                    log!("retention of {policy}: {changed} rows");
                }
            }
            Ok(Err(error)) => log!("could not run the store's retention: {error}"),
            Err(error) => log!("could not run the store's retention: {error}"),
        }
        if let Some(github) = domain.ports().port::<alondra_engine_ports::github::GitHub>() {
            crate::metrics_capture::pr_sizes(&domain, &*github.reads, alondra_core::now_millis()).await;
        }
        clock::sleep(EVERY).await;
    }
}

/// Deletes what the files folder's retention rule lists as expired: the saved results and what cards
/// showed, once their tasks and cards are done and their period is over.
pub fn remove_expired_files(domain: &DomainRuntime) -> Result<crate::files_retention::Removed, String> {
    let state = domain.state().map_err(|error| error.to_string())?;
    let report = crate::files_retention::sweep(domain.home(), &state, clock::system_time());
    Ok(crate::files_retention::remove_listed(domain.home(), &report))
}
