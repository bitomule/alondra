use super::*;

#[test]
fn work_that_ends_after_a_panic_holding_the_work_in_flight_leaves_it() {
    let inflight: InFlight = Arc::default();
    let key: WorkKey = ("device".into(), "request".into());
    inflight.lock().unwrap().insert(key.clone(), ([0; 32], false));
    crate::locks::poison(&inflight.work);
    drop(WorkGuard { inflight: inflight.clone(), key, changes: false, saturated: false });
    assert!(inflight.lock().unwrap().is_empty(), "a stale entry would hold every retry of that request");
}

#[test]
fn paired_devices_are_read_and_replaced_after_a_panic_holding_them() {
    let devices = Devices::default();
    crate::locks::poison_rw(&devices.0);
    assert!(devices.get("phone").is_none());
    devices.replace(HashMap::new());
    assert!(devices.snapshot().is_empty());
}
