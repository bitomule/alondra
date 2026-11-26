//! A device revoked while a domain command it authorized is being written waits for that commit:
//! the revocation (`auth`) and the runtime's write share the approval lease.
use crate::auth::Auth;
use crate::domain_runtime::DomainRuntime;
use alondra_domain_model::{Actor, Command, CommandEnvelope, PROTOCOL_VERSION};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use p256::ecdsa::SigningKey;
use std::sync::{Arc, mpsc};
use std::time::Duration;

#[test]
fn external_revocation_waits_for_authorized_domain_commit() {
    let scratch_directory = crate::test_support::Directory::new();
    let home = scratch_directory.parent().unwrap().join(format!(
        "alondra-domain-lease-{}",
        alondra_foundation::random_hex(8)
    ));
    std::fs::create_dir_all(&home).unwrap();
    let auth = Auth::open(&home).unwrap();
    let key = SigningKey::from_slice(&[9; 32]).unwrap();
    let device = auth
        .pair(
            &auth.new_pairing_code(),
            "Test Phone",
            &STANDARD.encode(key.verifying_key().to_sec1_bytes()),
        )
        .unwrap();
    let runtime = Arc::new(DomainRuntime::open_unwired_at(&home, &crate::domain_store::active_path(&home)).unwrap());
    let state = runtime.state().unwrap();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: "command-lease".into(),
        expected_revision: state.revision,
        command: Command::CreateRequest {
            request_id: "request-lease".into(),
            body: "Work".into(),
            auto_merge: false, maintenance: false, message: false,
        },
    };
    let (checked_tx, checked_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let writing = runtime.clone();
    let writer_home = home.clone();
    let device_id = device.id.clone();
    let writer = std::thread::spawn(move || {
        writing.execute(
            &Actor::User {
                user_id: format!("device:{device_id}"),
            },
            &envelope,
            None,
            || {
                assert!(
                    Auth::open(&writer_home)
                        .unwrap()
                        .list()
                        .iter()
                        .any(|entry| entry.id == device_id)
                );
                checked_tx.send(()).unwrap();
                resume_rx.recv().unwrap();
                Ok(())
            },
        )
    });
    checked_rx.recv().unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (revoked_tx, revoked_rx) = mpsc::channel();
    let revoker_home = home.clone();
    let revoke_id = device.id.clone();
    let revoker = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let changed = Auth::open(&revoker_home)
            .unwrap()
            .revoke(&revoke_id)
            .unwrap();
        revoked_tx.send(changed).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(
        revoked_rx.recv_timeout(Duration::from_millis(30)).is_err(),
        "revoke returned while writer still held approval lease"
    );
    resume_tx.send(()).unwrap();
    assert!(writer.join().unwrap().unwrap().changed);
    assert!(revoked_rx.recv_timeout(Duration::from_secs(2)).unwrap());
    revoker.join().unwrap();
    assert!(
        runtime
            .state()
            .unwrap()
            .requests
            .contains_key(&"request-lease".into())
    );
    assert!(
        !Auth::open(&home)
            .unwrap()
            .list()
            .iter()
            .any(|entry| entry.id == device.id)
    );
    drop(runtime);
    std::fs::remove_dir_all(home).unwrap();
}
