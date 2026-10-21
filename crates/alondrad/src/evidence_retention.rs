//! Raw SDK observations keep their bytes only while a reader needs them; this loop releases the
//! rest and gives the disk back the space, away from startup.
use crate::domain_runtime::DomainRuntime;
use std::sync::Arc;
use std::time::Duration;

const FIRST_AFTER: Duration = Duration::from_secs(60);
const EVERY: Duration = Duration::from_secs(60 * 60);

pub(crate) async fn run(domain: Arc<DomainRuntime>) {
    tokio::time::sleep(FIRST_AFTER).await;
    loop {
        let pass = {
            let domain = domain.clone();
            tokio::task::spawn_blocking(move || domain.retain_evidence()).await
        };
        match pass {
            Ok(Ok(retention)) if retention.released + retention.removed > 0 => {
                log!("released {} normalized SDK observations and removed {} of finished executions", retention.released, retention.removed);
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => log!("could not release normalized SDK observations: {error}"),
            Err(error) => log!("could not release normalized SDK observations: {error}"),
        }
        tokio::time::sleep(EVERY).await;
    }
}
