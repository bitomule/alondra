use super::*;
use serde_json::json;

fn offer(q: &Queue, owner: &str, phone: &str, mac: &str, id: &str, now: u64) -> Result<JobView, QueueError> {
    q.submit(owner,phone,mac,id,[1;32],json!("x"),now+MAX_LIFETIME_MS,now)
}
fn finish(q: &Queue, mac: &str, phone: &str, id: &str, response: serde_json::Value, now: u64) -> JobView {
    q.mark_received(mac,id,now).unwrap();
    q.respond(mac,phone,id,response,now).unwrap()
}

#[test]
fn uneven_device_backlogs_rotate_without_reordering_each_devices_work() {
    let q=Queue::new();
    for i in 0..32 { offer(&q,"owner","a","mac",&format!("a-{i}"),1).unwrap(); }
    for i in 0..2 { offer(&q,"owner","b","mac",&format!("b-{i}"),1).unwrap(); }
    offer(&q,"owner","c","mac","c-0",1).unwrap();
    let mut selected=Vec::new();
    for _ in 0..7 { selected.push(q.next_work("mac",2).unwrap().request_id); }
    assert_eq!(selected,["a-0","b-0","c-0","a-1","b-1","a-2","a-3"]);
    offer(&q,"other","other-a","other-mac","other-0",1).unwrap();
    assert_eq!(q.next_work("other-mac",2).unwrap().request_id,"other-0");
    let _=finish(&q,"mac","a","a-0",json!("result"),3);
    let revision=q.get("owner","a","a-0",3).unwrap().revision;
    q.acknowledge("owner","a","a-0",revision,3).unwrap();
    assert_eq!(q.get("owner","a","a-0",3).unwrap_err(),QueueError::ResultEvicted);
    // A receipt retained over a lease retry never reverts to dispatched.
    q.mark_received("mac","b-0",3).unwrap();
    let mut retries=Vec::new();
    for _ in 0..3 { retries.push(q.next_work("mac",LEASE_MS+4).unwrap().request_id); }
    assert_eq!(retries,["c-0","b-0","a-1"]);
    assert_eq!(q.get("owner","b","b-0",LEASE_MS+4).unwrap().status,JobStatus::Received);
}

#[test]
fn byte_admission_reserves_every_accepted_maximum_reply_until_verified_consumption() {
    let q=Queue { capacity:Capacity {bytes:2*MAX_RESPONSE_BYTES+6,owner_bytes:2*MAX_RESPONSE_BYTES+6,owner_pending:64},..Queue::default() };
    offer(&q,"o","p","m","a",1).unwrap();offer(&q,"o","p","m","b",1).unwrap();
    assert_eq!(offer(&q,"o","p","m","c",1).unwrap_err(),QueueError::Full);
    assert_eq!(q.process_fixture_statistics()["admitted_byte_charge"],2*MAX_RESPONSE_BYTES+6);
    let response=json!("y".repeat(MAX_RESPONSE_BYTES-2));
    assert_eq!(q.next_work("m",2).unwrap().request_id,"a");
    let a=finish(&q,"m","p","a",response.clone(),2);
    assert_eq!(q.next_work("m",2).unwrap().request_id,"b");
    let _=finish(&q,"m","p","b",response.clone(),2);
    assert_eq!(q.get("o","p","a",3).unwrap().response,Some(response.clone()));
    assert_eq!(offer(&q,"o","p","m","c",3).unwrap_err(),QueueError::Full);
    assert_eq!(offer(&q,"o","p","m","a",3).unwrap().response,Some(response));
    assert_eq!(q.acknowledge("o","wrong","a",a.revision,3),Err(QueueError::NotFound));
    assert_eq!(q.acknowledge("o","p","a",a.revision+1,3),Err(QueueError::Conflict));
    q.acknowledge("o","p","a",a.revision,3).unwrap();
    let deadline=q.inner.lock().unwrap().tombstones["a"].until_ms;
    q.acknowledge("o","p","a",a.revision,4).unwrap();
    assert_eq!(q.inner.lock().unwrap().tombstones["a"].until_ms,deadline);
    assert_eq!(offer(&q,"o","p","m","a",4).unwrap_err(),QueueError::ResultEvicted);
    offer(&q,"o","p","m","c",4).unwrap();
    assert_eq!(q.process_fixture_statistics()["cached_payload_bytes"],MAX_RESPONSE_BYTES+3);
}

#[test]
fn one_owner_cannot_use_other_owners_pending_capacity() {
    let q=Queue::new();
    for index in 0..64 { offer(&q,"noisy",if index<32 {"n-a"} else {"n-b"},"m-a",&format!("n-{index}"),1).unwrap(); }
    assert_eq!(offer(&q,"noisy","n-c","m-a","overflow",1).unwrap_err(),QueueError::Full);
    for index in 0..64 { offer(&q,"interactive",if index<32 {"i-a"} else {"i-b"},"m-b",&format!("i-{index}"),1).unwrap(); }
    assert_eq!(q.process_fixture_statistics()["pending"],128);
    q.purge_owner("noisy");
    assert_eq!(q.process_fixture_statistics()["pending"],64);
    assert_eq!(q.next_work("m-b",2).unwrap().request_id,"i-0");
}

#[test]
fn expiration_and_revocation_release_payload_reservations_and_dispatch_metadata() {
    for purge in ["owner","phone","mac","expire"] {
        let q=Queue::new();offer(&q,"o","p","m","a",1).unwrap();
        q.next_work("m",2).unwrap();
        match purge {
            "owner"=>q.purge_owner("o"), "phone"=>{ q.purge_phone("p"); },
            "mac"=>q.purge_mac("m"), _=>{ let _=q.get("o","p","a",MAX_LIFETIME_MS+2); },
        }
        assert_eq!(q.process_fixture_statistics()["reserved_response_bytes"],0);
        assert_eq!(q.process_fixture_statistics()["cached_payload_bytes"],0);
        let _=q.get("o","p","a",MAX_LIFETIME_MS+TOMBSTONE_MS+3);
        assert_eq!(q.process_fixture_statistics()["routing_cursors"],0);
    }
}

#[test]
fn exhausted_dedupe_capacity_never_discards_an_accepted_unacknowledged_result() {
    let q=Queue::new();offer(&q,"o","p","m","accepted",1).unwrap();
    q.next_work("m",2).unwrap();
    let response=json!("y".repeat(MAX_RESPONSE_BYTES-2));
    let completed=finish(&q,"m","p","accepted",response.clone(),2);
    {
        let mut inner=q.inner.lock().unwrap();
        for index in 0..MAX_TOMBSTONES {
            let id=format!("old-{index}");
            inner.tombstones.insert(id.clone(),Tombstone {
                id,owner_sub:"other".into(),phone_id:"other".into(),mac_id:"other".into(),
                request_hash:[1;32],response_hash:None,status:JobStatus::Completed,revision:1,until_ms:TOMBSTONE_MS,
            });
        }
    }
    assert_eq!(q.acknowledge("o","p","accepted",completed.revision,3),Err(QueueError::Full));
    assert_eq!(q.get("o","p","accepted",3).unwrap().response,Some(response.clone()));
    assert_eq!(q.respond("m","p","accepted",response.clone(),3).unwrap().response,Some(response));
    assert!(q.next_work("m",LEASE_MS+4).is_none());
}

#[test]
fn consumption_frees_only_the_verified_completed_revision_without_resetting_fairness() {
    let q=Queue::new();
    offer(&q,"o","a","m","a-0",1).unwrap();offer(&q,"o","a","m","a-1",1).unwrap();
    offer(&q,"o","b","m","b-0",1).unwrap();offer(&q,"o","b","m","b-1",1).unwrap();
    assert_eq!(q.next_work("m",2).unwrap().request_id,"a-0");
    assert_eq!(q.acknowledge("o","a","a-0",1,2),Err(QueueError::Conflict));
    let completed=finish(&q,"m","a","a-0",json!("result"),2);
    assert_eq!(q.acknowledge("other","a","a-0",completed.revision,3),Err(QueueError::NotFound));
    assert_eq!(q.acknowledge("o","b","a-0",completed.revision,3),Err(QueueError::NotFound));
    assert_eq!(q.acknowledge("o","a","a-0",completed.revision+1,3),Err(QueueError::Conflict));
    assert_eq!(q.get("o","a","a-0",3).unwrap().response,Some(json!("result")));
    q.acknowledge("o","a","a-0",completed.revision,3).unwrap();
    assert_eq!(q.get("o","a","a-0",3).unwrap_err(),QueueError::ResultEvicted);
    assert_eq!(q.next_work("m",3).unwrap().request_id,"b-0");
    assert_eq!(q.next_work("m",3).unwrap().request_id,"a-1");
    assert_eq!(q.next_work("m",3).unwrap().request_id,"b-1");
}
