//! What the store keeps only while a reader needs it: raw SDK observations, and the rows of each
//! table retention policy. This loop releases the rest and gives the disk back the space, away from
//! startup.
use crate::domain_runtime::DomainRuntime;
use std::sync::Arc;
use std::time::Duration;

const FIRST_AFTER: Duration = Duration::from_secs(60);
const EVERY: Duration = Duration::from_secs(60 * 60);

pub async fn run(domain: Arc<DomainRuntime>) {
    tokio::time::sleep(FIRST_AFTER).await;
    loop {
        let pass = {
            let domain = domain.clone();
            tokio::task::spawn_blocking(move || domain.retain(alondra_core::now_millis())).await
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
        crate::metrics_capture::pr_sizes(&domain, &crate::prs::GhCli, alondra_core::now_millis()).await;
        tokio::time::sleep(EVERY).await;
    }
}
