//! Bounded, immutable, device-owned overview snapshots. No filesystem paths or mutations.
use std::{collections::HashMap, sync::Mutex, time::{Duration, Instant}};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub const CHUNK_BYTES: usize = 65536;
const TTL: Duration = Duration::from_secs(120);
const GLOBAL_SLOTS: usize = 4;
const DEVICE_SLOTS: usize = 2;

#[derive(Default)]
pub struct Snapshots(Mutex<HashMap<String, Entry>>);
struct Entry {
    owner: String,
    binding: String,
    created: Instant,
    data: Option<Vec<u8>>,
    hash: String,
    revoked: bool,
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn route(method: &str, path: &str) -> bool {
    if path.len() > 4096 { return false; }
    if method == "GET" && path == "/v1/service/overview-snapshot" { return true; }
    let Some(suffix) = path.strip_prefix("/v1/service/overview-snapshot/") else { return false; };
    if method == "DELETE" { return valid_id(suffix); }
    let Some((id, offset)) = suffix.split_once("?offset=") else { return false; };
    method == "GET" && valid_id(id) && !offset.is_empty()
        && offset.bytes().all(|b| b.is_ascii_digit()) && offset.parse::<usize>().is_ok()
}

fn error(status: u16, code: &str) -> Value { json!({"status":status,"error":code}) }
fn chunk(id: &str, entry: &Entry, offset: usize) -> Value {
    let data = entry.data.as_ref().expect("completed snapshot");
    if offset >= data.len() || offset % CHUNK_BYTES != 0 { return error(422,"snapshot_offset_invalid"); }
    json!({"status":200,"body":{
        "schema":1,"snapshot_id":id,"total_bytes":data.len(),"sha256":entry.hash,
        "chunk_bytes":CHUNK_BYTES,"offset":offset,
        "data":URL_SAFE_NO_PAD.encode(&data[offset..data.len().min(offset+CHUNK_BYTES)])
    }})
}

// A cancelled/panicked capture cannot leave a reserved slot behind.
struct Reservation<'a> { snapshots: &'a Snapshots, id: String, committed: bool }
impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            if let Ok(mut slots) = self.snapshots.0.lock() { slots.remove(&self.id); }
        }
    }
}

impl Snapshots {
    pub fn begin(&self, owner: &str, binding: &str, capture: impl FnOnce(usize)->anyhow::Result<Vec<u8>>,
        authorize: impl FnOnce()->anyhow::Result<()>) -> Value {
        use ring::rand::SecureRandom;
        let mut random = [0u8;16];
        if ring::rand::SystemRandom::new().fill(&mut random).is_err() { return error(503,"snapshot_random_failed"); }
        let id = random.iter().map(|b| format!("{b:02x}")).collect::<String>();
        {
            let mut slots = self.0.lock().expect("overview snapshots lock poisoned");
            // Building slots remain reserved until their owner completes/drops.
            slots.retain(|_,v| v.data.is_none() || v.created.elapsed() < TTL);
            if slots.len() >= GLOBAL_SLOTS || slots.values().filter(|v| v.owner == owner).count() >= DEVICE_SLOTS {
                return error(429,"snapshot_busy");
            }
            if slots.contains_key(&id) { return error(503,"snapshot_random_collision"); }
            slots.insert(id.clone(), Entry { owner:owner.into(),binding:binding.into(),created:Instant::now(),data:None,hash:String::new(),revoked:false });
        }
        let mut reservation = Reservation { snapshots:self,id:id.clone(),committed:false };
        let data = match capture(MAX_BYTES) {
            Ok(data) if !data.is_empty() && data.len() <= MAX_BYTES => data,
            Ok(_) => return error(413,"snapshot_too_large"),
            Err(error) if error.is::<crate::http::OverviewTooLarge>() => return self::error(413,"snapshot_too_large"),
            Err(_) => return error(500,"snapshot_capture_failed"),
        };
        if authorize().is_err() { return error(403,"snapshot_access_denied"); }
        let hash = Sha256::digest(&data).iter().map(|b|format!("{b:02x}")).collect::<String>();
        let mut slots = self.0.lock().expect("overview snapshots lock poisoned");
        let Some(entry) = slots.get_mut(&id) else { return error(404,"snapshot_missing"); };
        if entry.revoked { return error(403,"snapshot_access_denied"); }
        entry.data = Some(data); entry.hash = hash;
        let response = chunk(&id,entry,0);
        reservation.committed = true;
        response
    }

    pub fn get(&self, owner: &str, binding: &str, id: &str, offset: usize) -> Value {
        let mut slots = self.0.lock().expect("overview snapshots lock poisoned");
        let Some(entry) = slots.get(id) else { return error(404,"snapshot_missing"); };
        if entry.revoked || entry.owner != owner || entry.binding != binding { return error(404,"snapshot_missing"); }
        if entry.data.is_none() { return error(429,"snapshot_busy"); }
        if entry.created.elapsed() >= TTL {
            slots.remove(id); return error(410,"snapshot_expired");
        }
        chunk(id,entry,offset)
    }

    pub fn release(&self, owner: &str, binding: &str, id: &str) -> Value {
        let mut slots = self.0.lock().expect("overview snapshots lock poisoned");
        if let Some(entry) = slots.get_mut(id).filter(|v| v.owner == owner && v.binding == binding) {
            if entry.data.is_none() { entry.revoked = true; } else { slots.remove(id); }
        }
        json!({"status":200,"body":{"released":true}})
    }

    pub fn revoke(&self, owner: &str) {
        self.0.lock().expect("overview snapshots lock poisoned").retain(|_,v| {
            if v.owner != owner { return true; }
            // A blocking capture may still own up to MAX_BYTES. Keep its reserved
            // budget until Reservation drops, even after approval is revoked.
            if v.data.is_none() { v.revoked = true; true } else { false }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn begin(s:&Snapshots,owner:&str,data:Vec<u8>)->Value { s.begin(owner,"generation-1",move |_| Ok(data),||Ok(())) }
    #[test]
    fn large_snapshot_is_exact_stable_and_device_generation_bound() {
        let s=Snapshots::default(); let data=vec![b'x';180*1024+777];
        let first=begin(&s,"a",data.clone()); assert_eq!(first["status"],200);
        let id=first["body"]["snapshot_id"].as_str().unwrap(); assert!(valid_id(id));
        let mut actual=Vec::new();
        for offset in (0..data.len()).step_by(CHUNK_BYTES) {
            let value=s.get("a","generation-1",id,offset);
            assert_eq!(value,s.get("a","generation-1",id,offset));
            assert_eq!(value["body"]["sha256"],first["body"]["sha256"]);
            actual.extend(URL_SAFE_NO_PAD.decode(value["body"]["data"].as_str().unwrap()).unwrap());
        }
        assert_eq!(actual,data); assert_eq!(first["body"]["sha256"],Sha256::digest(&actual).iter().map(|b|format!("{b:02x}")).collect::<String>());
        assert_eq!(s.get("b","generation-1",id,0)["status"],404);
        assert_eq!(s.get("a","generation-2",id,0)["status"],404);
        assert_eq!(s.get("a","generation-1",id,1)["status"],422);
        assert_eq!(s.get("a","generation-1",id,data.len())["status"],422);
        s.release("b","generation-1",id); assert_eq!(s.get("a","generation-1",id,0)["status"],200);
        s.revoke("a"); assert_eq!(s.get("a","generation-1",id,0)["status"],404);
        assert_eq!(s.release("a","generation-1",id)["status"],200);
    }
    #[test]
    fn bounded_reservations_limit_devices_global_and_capture_failure() {
        let s=Snapshots::default();
        assert_eq!(s.begin("a","g",|limit| {assert_eq!(limit,MAX_BYTES); Ok(vec![0;limit+1])},||Ok(()))["status"],413);
        assert!(s.0.lock().unwrap().is_empty());
        assert_eq!(s.begin("a","g",|_| Ok(vec![0;MAX_BYTES]),||anyhow::bail!("revoked"))["status"],403);
        assert!(s.0.lock().unwrap().is_empty());
        for owner in ["a","a","b","b"] { assert_eq!(begin(&s,owner,vec![1])["status"],200); }
        assert_eq!(s.begin("a","g",|_|panic!("must reserve before capture"),||Ok(()))["status"],429);
        assert_eq!(s.begin("c","g",|_|panic!("must reserve before capture"),||Ok(()))["status"],429);
        s.revoke("b"); assert_eq!(begin(&s,"a",vec![1])["status"],429);
        assert_eq!(begin(&s,"c",vec![1])["status"],200);
    }
    #[test]
    fn in_progress_captures_hold_slots_and_panics_release_reservation() {
        let s=Snapshots::default();
        let outer=s.begin("a","g", |_| {
            let inner=s.begin("a","g", |_| {
                assert_eq!(s.begin("a","g", |_| panic!("third capture must not execute"), ||Ok(()))["status"],429);
                Ok(vec![1])
            }, ||Ok(()));
            assert_eq!(inner["status"],200);Ok(vec![2])
        }, ||Ok(()));
        assert_eq!(outer["status"],200);s.revoke("a");
        assert!(std::panic::catch_unwind(|| s.begin("a","g", |_|panic!("capture panicked"), ||Ok(()))).is_err());
        assert!(s.0.lock().unwrap().is_empty());
    }

    #[test]
    fn revocation_during_capture_keeps_budget_reserved_until_worker_finishes() {
        let s=Snapshots::default();
        let value=s.begin("a","g", |_| {
            let id = s.0.lock().unwrap().keys().next().unwrap().clone();
            s.0.lock().unwrap().get_mut(&id).unwrap().created=Instant::now()-TTL;
            assert_eq!(s.get("a","g",&id,0)["status"],429,"expired live capture still owns its reservation");
            s.revoke("a");
            assert_eq!(s.0.lock().unwrap().len(),1,"revocation cannot release a still-allocating worker's slot");
            let second=s.begin("a","g2", |_| {
                assert_eq!(s.begin("a","g2", |_|panic!("revoked worker still holds budget"), ||Ok(()))["status"],429);
                Ok(vec![1])
            }, ||Ok(()));assert_eq!(second["status"],200);Ok(vec![1])
        }, ||Ok(()));
        assert_eq!(value["status"],403);
        assert_eq!(s.0.lock().unwrap().len(),1);
        s.revoke("a");assert!(s.0.lock().unwrap().is_empty());
    }

    #[test]
    fn expiry_restart_and_release_do_not_return_partial_or_different_snapshots() {
        let s=Snapshots::default();let first=begin(&s,"a",vec![1;CHUNK_BYTES+2]);
        let id=first["body"]["snapshot_id"].as_str().unwrap();
        s.0.lock().unwrap().get_mut(id).unwrap().created=Instant::now()-TTL;
        assert_eq!(s.get("a","generation-1",id,0)["status"],410);
        assert_eq!(s.get("a","generation-1",id,0)["status"],404);
        assert_eq!(Snapshots::default().get("a","generation-1",id,0)["status"],404);
        assert!(s.0.lock().unwrap().is_empty());
        assert!(!route("GET","/v1/service/overview-snapshot/../../secrets?offset=0"));
        assert!(!route("POST","/v1/service/overview-snapshot"));
        assert!(!route("GET",&format!("/v1/service/overview-snapshot/{id}?offset=0&offset=1")));
        assert!(route("DELETE",&format!("/v1/service/overview-snapshot/{id}")));
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;
    #[test]
    fn four_maximum_snapshots_release_all_owned_bytes() {
        let snapshots=Snapshots::default();let mut ids=Vec::new();
        for owner in ["a","a","b","b"] {
            let value=snapshots.begin(owner,"generation",|limit|Ok(vec![b'x';limit]),||Ok(()));
            assert_eq!(value["status"],200);
            ids.push((owner,value["body"]["snapshot_id"].as_str().unwrap().to_owned()));
        }
        let bytes=snapshots.0.lock().unwrap().values().map(|v|v.data.as_ref().unwrap().len()).sum::<usize>();
        assert_eq!(bytes,4*MAX_BYTES);
        assert_eq!(snapshots.begin("c","g",|_|panic!("capacity admission must precede allocation"),||Ok(()))["status"],429);
        for (owner,id) in ids { snapshots.release(owner,"generation",&id); }
        assert!(snapshots.0.lock().unwrap().is_empty());
        println!("snapshot_retained_bytes_peak={bytes} snapshot_retained_bytes_after_release=0");
        // A fresh maximum allocation succeeds after release; no lifetime slot leak.
        assert_eq!(snapshots.begin("c","g",|limit|Ok(vec![0;limit]),||Ok(()))["status"],200);
        snapshots.revoke("c");assert!(snapshots.0.lock().unwrap().is_empty());
    }
}
