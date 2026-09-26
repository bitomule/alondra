use crate::Server;
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;

pub async fn run(server: Arc<Server>) {
    loop {
        // Failed jobs retain a bounded lease and are retried after restart or timeout.
        match tokio::time::timeout(Duration::from_secs(40), step(&server)).await {
            Ok(Ok(true)) => continue,
            Ok(Ok(false)) => (),
            _ => eprintln!("alondra: account reconciliation pending"), // no identifiers/provider bodies
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
pub async fn step(server: &Server) -> Result<bool> {
    let Some(db) = &server.auth.management else {
        return Ok(false);
    };
    let Some(keycloak) = &server.management_keycloak else {
        return Ok(false);
    };
    let Some(job) = db.next_reconciliation().await? else {
        return Ok(false);
    };
    match job.action.as_str() {
        "keycloak_disable" => keycloak.disable(job.account_id).await?,
        "keycloak_delete" => keycloak.delete(job.account_id).await?,
        "relay_revoke" => {
            if let Some(owner) = &job.subject {
                let _gate = server.device_gate.write().await;
                server.auth.revoke_owner(owner)?;
                server.queue.purge_owner(owner);
                server.pairing.purge_owner(owner);
            }
        }
        _ => anyhow::bail!("unknown reconciliation action"),
    }
    db.reconciliation_complete(&job).await?;
    Ok(true)
}
