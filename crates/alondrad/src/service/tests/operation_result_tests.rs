use super::*;
use std::sync::atomic::AtomicU64;
use alondra_core::views::action_id::{ActionId, Verb};

struct Fixture {
    service: Service,
    domain: Arc<crate::domain_runtime::DomainRuntime>,
    phone: SecretKey,
    clock: Arc<AtomicU64>,
    home: crate::test_support::Directory,
}

impl Fixture {
    fn new() -> Self {
        let (home, mut service, phone) = push_fixture();
        activate_push(&service);
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&home).unwrap());
        let client = crate::client_routes::Client::open(domain.clone(), &home, Default::default());
        let clock = Arc::new(AtomicU64::new(1_000));
        service.envelope_clock = Some(clock.clone());
        service = service.with_domain(domain.clone()).with_client(client);
        Self { home, service, domain, phone, clock }
    }

    fn work(&self, id: &str, method: &str, path: &str, body: Option<Value>) -> Envelope {
        let mut value = json!({"method":method,"path":path});
        if let Some(body) = body { value["body"] = body; }
        Envelope::seal_random(EnvelopeContext {
            route:"mac-1", sender:"phone-1", recipient:"mac-1", request_id:id, kind:"request",
            expires_at_ms:self.clock.load(Ordering::Relaxed)+60_000,
        }, &serde_json::to_vec(&value).unwrap(), &SigningKey::from_slice(&[33u8;32]).unwrap(),
        &self.service.agreement.public_key()).unwrap()
    }

    fn action(&self) -> Envelope {
        let state = self.domain.state().unwrap();
        let action = ActionId::new(Verb::SetLimit, "limit", state.revision, Some("4".into())).unwrap();
        self.work("historical-action", "POST", "/actions", Some(json!({"protocol":1,
            "command_id":"5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10", "data_epoch":state.data_epoch,
            "client":"ios", "lang":"en", "action_id":action.to_string(), "option_id":"4", "input":{}})))
    }

    fn path(&self, original: &Envelope) -> String {
        let digest = operation_results::digest(original).unwrap();
        format!("/operation-results/{}?digest={}&data_epoch={}", original.request_id,
            digest.iter().map(|b| format!("{b:02x}")).collect::<String>(), self.domain.state().unwrap().data_epoch.0)
    }

    fn opened(&self, bytes: &[u8]) -> Value {
        let response: Envelope = serde_json::from_slice(bytes).unwrap();
        let body = response.open(self.service.signing.verifying_key(), &self.phone,
            "mac-1","phone-1","mac-1",self.clock.load(Ordering::Relaxed)).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn direct(&self, envelope: Envelope) -> Vec<u8> {
        let (bytes, lease) = self.service.handle_direct(envelope).await.unwrap();
        drop(lease);
        bytes
    }
}

#[tokio::test]
async fn a_fresh_read_recovers_expired_action_ciphertext_after_a_daemon_restart_without_reapplying_it() {
    let mut f = Fixture::new();
    let original = f.action();
    let bytes = f.direct(original.clone()).await;
    let original_result = f.opened(&bytes);
    assert_eq!(original_result["body"]["outcome"], "applied", "{original_result}");
    assert!(std::fs::read_to_string(alondra_core::options::path_in(&f.home)).unwrap().contains("max_working_nodes = 4"));
    let historical: Envelope = serde_json::from_slice(&bytes).unwrap();
    f.clock.store(700_000, Ordering::Relaxed);
    assert!(historical.open(f.service.signing.verifying_key(), &f.phone,
        "mac-1","phone-1","mac-1",700_000).is_err(), "generic expired response verification must remain closed");
    assert!(matches!(f.service.handle_direct(original.clone()).await, Err(DirectRefusal::Unauthorized)),
        "expired original mutations cannot reach even an existing receipt");
    let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&f.home).unwrap());
    let client = crate::client_routes::Client::open(domain.clone(), &f.home, Default::default());
    let mut restarted = Service::open(&f.home, 0, Arc::new(Auth::open(&f.home).unwrap())).unwrap().unwrap()
        .with_domain(domain.clone()).with_client(client);
    restarted.envelope_clock = Some(f.clock.clone());
    f.service = restarted;
    f.domain = domain;
    // A later user change makes any accidental reapplication visible.
    let options = alondra_core::options::path_in(&f.home);
    let changed = std::fs::read_to_string(&options).unwrap().replace("max_working_nodes = 4", "max_working_nodes = 5");
    std::fs::write(&options, &changed).unwrap();
    for index in 0..2 {
        let read = f.work(&format!("historical-read-{index}"), "GET", &f.path(&original), None);
        let response = f.opened(&f.direct(read).await);
        assert_eq!(response["status"], 200, "{response}");
        assert_eq!(response["body"]["request_id"], original.request_id);
        assert_eq!(response["body"]["response"], serde_json::to_value(&historical).unwrap());
        assert_eq!(std::fs::read_to_string(&options).unwrap(), changed);
    }
}

#[tokio::test]
async fn historical_reads_require_the_exact_digest_epoch_and_current_approval_generation() {
    let f = Fixture::new();
    let original = f.action();
    f.direct(original.clone()).await;
    let path = f.path(&original);
    for (index, rejected) in [path.replace("data_epoch=", "data_epoch=old-"), path.replace("historical-action", "missing-action")]
        .into_iter().enumerate() {
        assert!(operation_results::lookup(&rejected).is_some());
        let read = f.work(&format!("wrong-result-{index}"), "GET", &rejected, None);
        assert_ne!(f.opened(&f.direct(read).await)["status"], 200);
    }
    let mut wrong = operation_results::lookup(&path).unwrap();
    wrong.digest[0] ^= 1;
    assert_eq!(f.service.operation_result("phone-1", &wrong, Transport::Direct).await.unwrap()["status"], 404);
    let mut approval: Value = serde_json::from_slice(&std::fs::read(f.home.join("devices.json")).unwrap()).unwrap();
    approval[0]["created_at"] = json!(1);
    std::fs::write(f.home.join("devices.json"), serde_json::to_vec(&approval).unwrap()).unwrap();
    let read = f.work("reapproved-read", "GET", &path, None);
    assert_eq!(f.opened(&f.direct(read).await)["status"], 404);
    Auth::open(&f.home).unwrap().revoke("phone-1").unwrap();
    assert!(f.service.handle_direct(f.work("revoked-read", "GET", &path, None)).await.is_err());
}

#[test]
fn the_historical_route_grammar_and_digest_are_bounded_and_json_order_independent() {
    let f = Fixture::new();
    let original = f.action();
    let expected = operation_results::digest(&original).unwrap();
    let reordered: Envelope = serde_json::from_value(serde_json::to_value(&original).unwrap()).unwrap();
    assert_eq!(operation_results::digest(&reordered).unwrap(), expected);
    let path = f.path(&original);
    assert!(operation_results::lookup(&path).is_some());
    for invalid in [path.clone()+"&extra=1", path.clone()+"&digest="+&"0".repeat(64),
        path.replace("/operation-results/", "/operation-results/%2f"),
        path.replace("digest=", "digest=A")] {
        assert!(operation_results::lookup(&invalid).is_none(), "{invalid}");
    }
}

#[tokio::test]
async fn missing_legacy_and_incomplete_receipts_never_authorize_reexecution_or_return_a_result() {
    let f = Fixture::new();
    let original = f.action();
    let path = f.path(&original);
    let lookup = operation_results::lookup(&path).unwrap();
    let device = f.service.devices.get("phone-1").unwrap();
    let request: LocalRequest = serde_json::from_slice(&original.open(&device.signing, &f.service.agreement,
        "phone-1","mac-1","mac-1",1_000).unwrap()).unwrap();
    let scope = f.service.operation_scope(&original, &request, &device, Transport::Direct).unwrap().unwrap();
    assert_eq!(f.service.operation_result("phone-1", &lookup, Transport::Direct).await.unwrap()["status"], 404);
    f.service.journal.accept_scoped("phone-1", &original.request_id, &[1;32], Some(&scope)).unwrap();
    assert_eq!(f.service.operation_result("phone-1", &lookup, Transport::Direct).await.unwrap()["status"], 404);
    f.service.journal.executing("phone-1", &original.request_id, &[1;32]).unwrap();
    assert_eq!(f.service.operation_result("phone-1", &lookup, Transport::Direct).await.unwrap()["status"], 404);
    let other = serde_json::to_vec(&Envelope::seal_random(EnvelopeContext {
        route:"mac-1",sender:"mac-1",recipient:"phone-1",request_id:&original.request_id,
        kind:"response",expires_at_ms:601_000,
    }, br#"{"status":200,"body":{"outcome":"applied"}}"#, &f.service.signing,
        &f.phone.public_key()).unwrap()).unwrap();
    f.service.journal.complete("phone-1", &original.request_id, &other).unwrap();
    assert_eq!(f.service.operation_result("phone-1", &lookup, Transport::Direct).await.unwrap()["status"], 200);
    let mut absent = lookup;
    absent.id = "legacy-action".into();
    f.service.journal.accept("phone-1", &absent.id, &[2;32]).unwrap();
    f.service.journal.executing("phone-1", &absent.id, &[2;32]).unwrap();
    f.service.journal.complete("phone-1", &absent.id, &other).unwrap();
    assert_eq!(f.service.operation_result("phone-1", &absent, Transport::Direct).await.unwrap()["status"], 404);
    assert!(f.service.operation_result("foreign-device", &absent, Transport::Direct).await.is_err());
    assert!(!alondra_core::options::path_in(&f.home).exists(), "a result lookup must never execute the pending action");
}

#[tokio::test]
async fn a_receipt_does_not_cross_a_domain_reset_even_when_the_journal_survives() {
    let other_home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&other_home).unwrap();
    let mut f = Fixture::new();
    let original = f.action();
    f.direct(original.clone()).await;
    let old_epoch = f.domain.state().unwrap().data_epoch.0.clone();
    let new_domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&other_home).unwrap());
    assert_ne!(new_domain.state().unwrap().data_epoch.0, old_epoch);
    f.domain = new_domain.clone();
    f.service.domain = Some(new_domain);
    let path = f.path(&original);
    let read = f.work("reset-domain-read", "GET", &path, None);
    assert_eq!(f.opened(&f.direct(read).await)["status"], 404,
        "checking only the query epoch against the current domain would expose the prior domain's result");
    drop(f);
}

#[tokio::test]
async fn a_later_read_returns_the_same_expired_inner_receipt_in_a_fresh_sealed_response() {
    let f = Fixture::new();
    let responses = Mutex::new(Vec::<Vec<u8>>::new());
    let original = f.action();
    f.service.answer_directly(Work { request_id: original.request_id.clone(), envelope: original.clone() }, &responses).await.unwrap();
    let bytes = responses.lock().unwrap().pop().unwrap();
    assert_eq!(f.opened(&bytes)["body"]["outcome"], "applied");
    f.clock.store(700_000, Ordering::Relaxed);
    let read = f.work("historical-read", "GET", &f.path(&original), None);
    f.service.answer_directly(Work { request_id: read.request_id.clone(), envelope: read }, &responses).await.unwrap();
    let recovered = f.opened(&responses.lock().unwrap().pop().unwrap());
    assert_eq!(recovered["status"], 200, "{recovered}");
    assert_eq!(recovered["body"]["response"], serde_json::from_slice::<Value>(&bytes).unwrap());
    assert!(responses.lock().unwrap().is_empty());
}

#[test]
fn recovery_digest_matches_the_portable_framed_wire_fixture() {
    let envelope: Envelope = serde_json::from_str(r#"{"v":1,"route":"mac-fixture","sender":"phone-fixture","recipient":"mac-fixture","request_id":"fixture-operation","kind":"request","expires_at_ms":12345,"ephemeral_key":"BGsX0fLhLEJH-Lzm5WOkQPJ3A32BLeszoPShOUXYmMKWT-NC4v4af5uO5-tKfA-eFivOM1drMV7Oy7ZAaDe_UfU","nonce":"AAECAwQFBgcICQoL","ciphertext":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","signature":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0-Pw"}"#).unwrap();
    let digest = operation_results::digest(&envelope).unwrap();
    assert_eq!(digest.iter().map(|byte|format!("{byte:02x}")).collect::<String>(), "5dca4fef6c944cf38f66e0171cab83be4e9f4d3a5d71b523bb2f6a3104e4e19d");
}

#[tokio::test]
async fn a_large_valid_expired_receipt_is_recovered_in_bounded_fresh_chunks_without_widening_the_relay_limit() {
    let f = Fixture::new();
    let original = f.action();
    f.direct(original.clone()).await;
    let payload = serde_json::to_vec(&json!({"status":200,"body":{"outcome":"applied","text":"a".repeat(730*1024)}})).unwrap();
    let historical = Envelope::seal_random(EnvelopeContext {
        route:"mac-1",sender:"mac-1",recipient:"phone-1",request_id:&original.request_id,
        kind:"response",expires_at_ms:601_000,
    },&payload,&f.service.signing,&f.phone.public_key()).unwrap();
    let bytes = serde_json::to_vec(&historical).unwrap();
    assert!(bytes.len()<=MAX_RELAY_RESPONSE && bytes.len()>MAX_LOCAL_RESPONSE);
    f.service.journal.db().unwrap().execute("UPDATE service_receipts SET response=?1 WHERE sender='phone-1' AND request_id=?2",
        params![bytes,&original.request_id]).unwrap();
    f.clock.store(700_000,Ordering::Relaxed);
    let mut recovered = Vec::new();
    let path = f.path(&original);
    for index in 0..4 {
        let request_path = if index==0 { path.clone() } else { format!("{path}&offset={}",recovered.len()) };
        let sealed = f.direct(f.work(&format!("large-result-{index}"),"GET",&request_path,None)).await;
        assert!(sealed.len()<=MAX_RELAY_RESPONSE);
        let result = f.opened(&sealed);
        assert_eq!(result["status"],200,"{result}");
        assert_eq!(result["body"]["offset"],recovered.len());
        assert_eq!(result["body"]["total"],bytes.len());
        let chunk = STANDARD.decode(result["body"]["chunk"].as_str().unwrap()).unwrap();
        assert!(!chunk.is_empty() && chunk.len()<=256*1024);
        recovered.extend_from_slice(&chunk);
        if recovered.len()==bytes.len() { break; }
    }
    assert_eq!(recovered,bytes);
    assert!(operation_results::lookup(&(path.clone()+"&offset=1")).is_none());
    assert!(operation_results::lookup(&(path.clone()+"&offset=1048577")).is_none());
    let wrong = operation_results::lookup(&(path+"&offset=1048576")).unwrap();
    assert_eq!(f.service.operation_result("phone-1",&wrong,Transport::Direct).await.unwrap()["status"],422);
}

#[tokio::test]
async fn authenticated_control_ack_removes_modern_receipts_without_reopening_a_retired_effect() {
    let f=Fixture::new();let epoch=f.domain.state().unwrap().data_epoch.0.clone();
    let descriptor=f.opened(&f.direct(f.work("journal-control-read","GET",&format!("/operation-journal?data_epoch={epoch}"),None)).await);
    let generation=descriptor["body"]["generation"].as_u64().unwrap();
    let id=format!("op_{}_{generation}_1_5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10",alondra_core::operation_id::epoch_tag(&epoch));
    let legacy=f.action();let device=f.service.devices.get("phone-1").unwrap();
    let mut payload:Value=serde_json::from_slice(&legacy.open(&device.signing,&f.service.agreement,"phone-1","mac-1","mac-1",1_000).unwrap()).unwrap();
    payload["body"]["command_id"]=json!(id);
    let original=f.work(&id,"POST","/actions",Some(payload["body"].clone()));
    assert_eq!(f.opened(&f.direct(original.clone()).await)["body"]["outcome"],"applied");
    let ack=json!({"data_epoch":epoch,"generation":generation,"ranges":[{"first":1,"last":1}]});
    for suffix in ["first","repeated"] {
        let result=f.opened(&f.direct(f.work(&format!("journal-control-{suffix}"),"POST","/operation-journal/ack",Some(ack.clone()))).await);
        assert_eq!(result["body"]["acknowledged"],true,"{result}");
    }
    let c=f.service.journal.db().unwrap();
    assert_eq!(c.query_row("SELECT count(*) FROM service_receipts WHERE request_id=?1",params![id],|row|row.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(c.query_row("SELECT count(*) FROM service_receipts WHERE request_id LIKE 'journal-control-%'",[],|row|row.get::<_,i64>(0)).unwrap(),0);
    let options=alondra_core::options::path_in(&f.home);
    let changed=std::fs::read_to_string(&options).unwrap().replace("max_working_nodes = 4","max_working_nodes = 5");
    std::fs::write(&options,&changed).unwrap();
    assert_eq!(f.opened(&f.direct(original).await)["status"],410);
    let mut forged_body=payload["body"].clone();
    forged_body["command_id"]=json!("5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a11");
    assert!(f.service.handle_direct(f.work(&id,"POST","/actions",Some(forged_body))).await.is_err(),
        "a retired encrypted ID cannot become a fresh legacy action");
    assert!(f.service.handle_direct(f.work(&id,"PUT",crate::settings::PATH,Some(json!({})))).await.is_err(),
        "a retired encrypted ID cannot be reclassified to another mutation path");
    assert_eq!(std::fs::read_to_string(&options).unwrap(),changed);
}

#[test]
fn compacted_legacy_proofs_and_bounded_read_cache_never_rerun_mutations() {
    let f=Fixture::new();
    for index in 0..70 {
        let id=format!("legacy-{index}");f.service.journal.accept("phone-1",&id,&[7;32]).unwrap();
        f.service.journal.executing("phone-1",&id,&[7;32]).unwrap();f.service.journal.complete("phone-1",&id,b"legacy response").unwrap();
    }
    assert!(matches!(f.service.journal.accept("phone-1","legacy-0",&[7;32]).unwrap(),Disposition::Retired));
    assert!(f.service.journal.accept("phone-1","legacy-0",&[8;32]).is_err());
    let c=f.service.journal.db().unwrap();
    assert_eq!(c.query_row("SELECT count(*) FROM service_receipts WHERE response IS NOT NULL",[],|row|row.get::<_,i64>(0)).unwrap(),64);
    assert_eq!(c.query_row("SELECT count(*) FROM service_receipts",[],|row|row.get::<_,i64>(0)).unwrap(),70);
    for index in 0..70 {
        let id=format!("read-{index}");f.service.journal.accept_classified("phone-1",&id,&[9;32],None,true,None).unwrap();
        f.service.journal.executing("phone-1",&id,&[9;32]).unwrap();f.service.journal.complete("phone-1",&id,b"read response").unwrap();
    }
    assert_eq!(c.query_row("SELECT count(*) FROM service_receipts WHERE execution_mode='read'",[],|row|row.get::<_,i64>(0)).unwrap(),64);
    assert_eq!(c.query_row("SELECT count(*) FROM service_receipts WHERE request_id LIKE 'legacy-%'",[],|row|row.get::<_,i64>(0)).unwrap(),70);
}

#[tokio::test]
async fn a_full_legacy_proof_budget_refuses_legacy_before_effect_but_keeps_modern_actions_available() {
    let f=Fixture::new();let c=f.service.journal.db().unwrap();
    c.execute("WITH RECURSIVE ids(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM ids WHERE n<?1)
        INSERT INTO service_receipts(sender,request_id,digest,state) SELECT 'phone-1','legacy-budget-'||n,?2,'retired' FROM ids",
        params![crate::operation_journal::MAX_LEGACY as i64,&[7u8;32]]).unwrap();
    let options=alondra_core::options::path_in(&f.home);let before=std::fs::read(&options).ok();
    let epoch=f.domain.state().unwrap().data_epoch.0.clone();
    let descriptor=f.opened(&f.direct(f.work("full-legacy-descriptor","GET",&format!("/operation-journal?data_epoch={epoch}"),None)).await);
    let rejected=f.opened(&f.direct(f.action()).await);
    assert_eq!(rejected["status"],429);assert_eq!(rejected["body"]["error"],"operation_capacity");assert_eq!(std::fs::read(&options).ok(),before);
    let id=format!("op_{}_{}_1_5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10",alondra_core::operation_id::epoch_tag(&epoch),descriptor["body"]["generation"].as_u64().unwrap());
    let original=f.action();let device=f.service.devices.get("phone-1").unwrap();
    let mut payload:Value=serde_json::from_slice(&original.open(&device.signing,&f.service.agreement,"phone-1","mac-1","mac-1",1_000).unwrap()).unwrap();
    payload["body"]["command_id"]=json!(id);
    let applied=f.opened(&f.direct(f.work(&id,"POST","/actions",Some(payload["body"].clone()))).await);
    assert_eq!(applied["body"]["outcome"],"applied","{applied}");
}

#[tokio::test]
async fn preupgrade_unclassified_reads_do_not_block_an_old_client_until_explicit_journal_opt_in() {
    let f=Fixture::new();let c=f.service.journal.db().unwrap();
    c.execute("WITH RECURSIVE ids(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM ids WHERE n<?1)
        INSERT INTO service_receipts(sender,request_id,digest,state) SELECT 'phone-1','old-read-'||n,?2,'retired' FROM ids",
        params![crate::operation_journal::MAX_LEGACY as i64+1,&[7u8;32]]).unwrap();
    assert_eq!(f.opened(&f.direct(f.action()).await)["body"]["outcome"],"applied");
    assert_eq!(c.query_row("SELECT count(*) FROM operation_issuers",[],|row|row.get::<_,i64>(0)).unwrap(),0,
        "legacy validation must not silently opt the old client in");
}

impl Fixture {
    async fn generation(&self) -> u64 {
        let epoch = self.domain.state().unwrap().data_epoch.0.clone();
        let descriptor = self.opened(&self.direct(
            self.work("journal-descriptor", "GET", &format!("/operation-journal?data_epoch={epoch}"), None)).await);
        descriptor["body"]["generation"].as_u64().unwrap()
    }

    /// A journaled action setting the node limit to `limit`, as operation `sequence`.
    fn operation(&self, generation: u64, sequence: u64, limit: &str) -> (String, Envelope) {
        let state = self.domain.state().unwrap();
        let id = format!("op_{}_{generation}_{sequence}_5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a1{sequence}",
            alondra_core::operation_id::epoch_tag(&state.data_epoch.0));
        let action = ActionId::new(Verb::SetLimit, "limit", state.revision, Some(limit.into())).unwrap();
        let envelope = self.work(&id, "POST", "/actions", Some(json!({"protocol":1, "command_id":id,
            "data_epoch":state.data_epoch, "client":"ios", "lang":"en", "action_id":action.to_string(),
            "option_id":limit, "input":{}})));
        (id, envelope)
    }

    /// Holds the envelope whose receipt is about to enter `stage` until `release` is notified.
    fn hold(&mut self, stage: ReceiptWrite, id: &str) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let (entered, release) = (Arc::new(tokio::sync::Notify::new()), Arc::new(tokio::sync::Notify::new()));
        let (signal, gate, target) = (entered.clone(), release.clone(), id.to_owned());
        self.service.journal.before_write = Some(Arc::new(move |observed, id| {
            let (signal, gate, held) = (signal.clone(), gate.clone(), observed == stage && id == target);
            Box::pin(async move {
                if held {
                    signal.notify_one();
                    gate.notified().await;
                }
            })
        }));
        (entered, release)
    }

    /// The process dies while `envelope` executes: the receipt reached `executing` and nothing
    /// else happened, exactly what a `kill -9` between those two steps leaves on disk.
    async fn killed_while_executing(&mut self, envelope: Envelope) {
        let (entered, _release) = self.hold(ReceiptWrite::ExecutingLocal, &envelope.request_id);
        let id = envelope.request_id.clone();
        {
            let running = self.service.handle_direct(envelope);
            tokio::pin!(running);
            tokio::select! {
                _ = &mut running => panic!("{id} finished instead of being cut"),
                _ = entered.notified() => {}
            }
        }
        self.service.journal.before_write = None;
        let digest: Vec<u8> = self.service.journal.db().unwrap().query_row(
            "SELECT digest FROM service_receipts WHERE request_id=?1", params![id], |row| row.get(0)).unwrap();
        let digest: [u8; 32] = digest.try_into().unwrap();
        self.service.journal.executing_local("phone-1", &id, &digest).unwrap();
    }

    fn restart(&mut self) {
        let domain = Arc::new(crate::domain_runtime::DomainRuntime::open(&self.home).unwrap());
        let client = crate::client_routes::Client::open(domain.clone(), &self.home, Default::default());
        let mut restarted = Service::open(&self.home, 0, Arc::new(Auth::open(&self.home).unwrap())).unwrap().unwrap()
            .with_domain(domain.clone()).with_client(client);
        restarted.envelope_clock = Some(self.clock.clone());
        restarted.recover_interrupted().unwrap();
        self.service = restarted;
        self.domain = domain;
    }

    fn receipt(&self, id: &str) -> Option<String> {
        self.service.journal.db().unwrap().query_row("SELECT state FROM service_receipts WHERE request_id=?1",
            params![id], |row| row.get(0)).optional().unwrap()
    }

    fn ack(&self, id: &str, generation: u64, ranges: Value) -> Envelope {
        let epoch = self.domain.state().unwrap().data_epoch.0.clone();
        self.work(id, "POST", "/operation-journal/ack", Some(json!({"data_epoch":epoch,"generation":generation,"ranges":ranges})))
    }
}

#[tokio::test]
async fn a_restart_in_the_middle_of_operations_resolves_them_so_acknowledgements_and_redelivery_finish() {
    let mut f = Fixture::new();
    let generation = f.generation().await;
    let options = alondra_core::options::path_in(&f.home);
    let (redelivered, redelivery) = f.operation(generation, 1, "4");
    let (abandoned, abandonment) = f.operation(generation, 2, "6");
    f.killed_while_executing(redelivery.clone()).await;
    f.killed_while_executing(abandonment.clone()).await;
    f.service.journal.accept_classified("phone-1", "read-cut", &[9; 32], None, true, None).unwrap();
    f.service.journal.executing("phone-1", "read-cut", &[9; 32]).unwrap();

    f.restart();
    assert_eq!(f.receipt(&redelivered).as_deref(), Some("interrupted"));
    assert_eq!(f.receipt(&abandoned).as_deref(), Some("interrupted"));
    assert_eq!(f.receipt("read-cut"), None, "a cut read has no effect to keep");

    let applied = f.opened(&f.direct(redelivery.clone()).await);
    assert_eq!(applied["body"]["outcome"], "applied", "{applied}");
    assert!(std::fs::read_to_string(&options).unwrap().contains("max_working_nodes = 4"));
    assert_eq!(f.opened(&f.direct(redelivery).await), applied, "the recovered operation runs once");

    // The client gave up on 2 and never sent 3: retiring both refuses them for good.
    let ack = f.opened(&f.direct(f.ack("ack-after-restart", generation, json!([{"first":1,"last":3}]))).await);
    assert_eq!(ack["status"], 200, "{ack}");
    assert_eq!(ack["body"]["acknowledged"], true, "{ack}");
    assert_eq!(ack["body"]["ranges"], json!([{"first":1,"last":3}]));
    assert_eq!(f.receipt(&abandoned), None);
    assert_eq!(f.opened(&f.direct(abandonment).await)["status"], 410);
    assert!(!std::fs::read_to_string(&options).unwrap().contains("max_working_nodes = 6"));
}

#[tokio::test]
async fn an_acknowledgement_with_unfinished_operations_is_answered_and_retires_the_rest() {
    let mut f = Fixture::new();
    let generation = f.generation().await;
    let (finished, finishing) = f.operation(generation, 1, "4");
    assert_eq!(f.opened(&f.direct(finishing).await)["body"]["outcome"], "applied");
    let (dead, dying) = f.operation(generation, 2, "6");
    f.killed_while_executing(dying).await;
    let (live, running) = f.operation(generation, 3, "5");
    f.killed_while_executing(running).await;
    let owner = ("phone-1".to_owned(), live.clone());
    f.service.inflight.lock().unwrap().insert(owner.clone(), ([0; 32], false));

    let answer = f.opened(&f.direct(f.ack("ack-while-running", generation, json!([{"first":1,"last":4}]))).await);
    assert_eq!(answer["status"], 200, "{answer}");
    assert_eq!(answer["body"]["acknowledged"], false, "{answer}");
    assert_eq!(answer["body"]["ranges"], json!([{"first":1,"last":2},{"first":4,"last":4}]));
    assert_eq!(f.receipt(&finished), None);
    assert_eq!(f.receipt(&dead), None, "an operation whose executor is gone is retired");
    assert_eq!(f.receipt(&live).as_deref(), Some("executing"), "a running operation keeps its receipt");

    f.service.inflight.lock().unwrap().remove(&owner);
    let again = f.opened(&f.direct(f.ack("ack-after-running", generation, json!([{"first":1,"last":4}]))).await);
    assert_eq!(again["body"]["acknowledged"], true, "{again}");
    assert_eq!(again["body"]["ranges"], json!([{"first":1,"last":4}]));

    let refused = f.opened(&f.direct(f.ack("ack-old-generation", generation + 1, json!([{"first":1,"last":1}]))).await);
    assert_eq!(refused["status"], 409, "a refused acknowledgement is still answered: {refused}");
}
