//! 14 Oct 2026, alondrad 0.2.71: an iPhone card answer arrived over the relay while another relay
//! response was being delivered, and the daemon stopped answering anything. One tokio worker
//! held the domain store and slept in `flock` for the shared approval lease; the other waited
//! for the store in the feed's `gather_at`. The delivery that held the shared lease across its
//! upload could not be polled, so it never let go.
//!
//! The shared lease waited behind another shared lease because macOS wakes one waiter when an
//! exclusive lock is released and parks the rest behind that waiter's lock.
use crate::approval_lease::ApprovalLease;
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::*;
use std::sync::{Arc, mpsc};
use std::time::Duration;

const SETTLE: Duration = Duration::from_millis(200);

#[test]
fn a_relay_action_and_a_feed_read_finish_while_a_delivery_holds_the_lease() {
    let directory = crate::test_support::Directory::new();
    std::fs::create_dir_all(&*directory).unwrap();
    let home = directory.to_path_buf();
    let domain = Arc::new(DomainRuntime::open(&home).unwrap());
    let state = domain.state().unwrap();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: "command-card-answer".into(),
        expected_revision: state.revision,
        command: Command::CreateRequest {
            request_id: "request-card-answer".into(),
            body: "Answer".into(),
            auto_merge: false,
            maintenance: false,
            message: false,
        },
    };

    let enrollment = ApprovalLease::exclusive(&home).unwrap();
    let (finished_tx, finished_rx) = mpsc::channel();
    let (spawned_tx, spawned_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let delivery_home = home.clone();
        let delivery = runtime.spawn(async move {
            crate::service::approval_bound_delivery(&delivery_home, || Ok(()), || async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok(())
            })
            .await
        });
        std::thread::sleep(SETTLE);
        let acting = domain.clone();
        let action = runtime.spawn(async move {
            acting.execute(
                &Actor::User { user_id: "device:phone".into() },
                &envelope,
                None,
                || Ok(()),
            )
        });
        std::thread::sleep(SETTLE);
        let reading = domain.clone();
        let feed = runtime.spawn(async move {
            drop(reading.store.lock().expect("domain store poisoned"));
        });
        spawned_tx.send(()).unwrap();
        let outcome = runtime.block_on(async {
            (delivery.await.unwrap().is_ok(), action.await.unwrap().is_ok(), feed.await.is_ok())
        });
        finished_tx.send(outcome).unwrap();
        runtime.shutdown_background();
    });
    spawned_rx.recv().unwrap();
    drop(enrollment);

    let outcome = finished_rx.recv_timeout(Duration::from_secs(5)).expect(
        "the daemon's runtime deadlocked: the action holds the store while waiting for the lease \
         a suspended delivery holds, and the feed read holds the last worker",
    );
    assert_eq!(outcome, (true, true, true));
}
