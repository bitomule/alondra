//! Advertises this Mac as `_alondra._tcp` so phones and terminals on the same network find it
//! without typing an address. The TXT record is only a hint: identity comes from the keys
//! `/direct/hello` returns. A sandbox daemon uses another service type, so real clients never
//! list test Macs.
use crate::locks::Poisonable as _;
use alondra_core::bonjour::{Advertisement, POLICY_DENIED, advertise as register, txt_record};
use std::sync::Mutex;
use std::time::Duration;

pub(crate) const SERVICE_TYPE: &str = "_alondra._tcp";
pub(crate) const SANDBOX_SERVICE_TYPE: &str = "_alondra-sandbox._tcp";
const CHECK: Duration = Duration::from_secs(5);
const RETRY: Duration = Duration::from_secs(60);

/// What `/daemon` reports about the advertisement.
/// A holder that panics leaves it whole, so it is recovered: one assignment.
static STATUS: Mutex<&str> = Mutex::new("off");

pub(crate) fn status() -> &'static str {
    *STATUS.lock_or_recover("the Bonjour status")
}

fn set_status(value: &'static str) {
    *STATUS.lock_or_recover("the Bonjour status") = value;
}

pub(crate) fn service_type(sandbox: bool) -> &'static str {
    if sandbox { SANDBOX_SERVICE_TYPE } else { SERVICE_TYPE }
}

/// Stops advertising when dropped.
pub(crate) struct Advert(tokio::task::JoinHandle<()>);

impl Drop for Advert {
    fn drop(&mut self) {
        self.0.abort();
        set_status("off");
    }
}

fn report(advertisement: &Advertisement, logged: &mut Option<i32>) {
    let current = advertisement.status();
    if current == *logged {
        return;
    }
    match current {
        Some(0) => {
            log!("advertising this Mac on Bonjour");
            set_status("advertising");
        }
        Some(POLICY_DENIED) => {
            log!("Bonjour advertisement withheld: allow Local Network for Alondra (alondrad), or add this Mac by address");
            set_status("unavailable");
            crate::local_network::current();
        }
        Some(error) => {
            log!("Bonjour advertisement failed ({error}); add this Mac by address");
            set_status("unavailable");
        }
        None => set_status("pending"),
    }
    *logged = current;
}

/// Keeps this Mac advertised while the listener runs. macOS may hold the registration until the
/// user allows Local Network access for alondrad; the listener works meanwhile.
pub(crate) fn advertise(name: &str, mac_id: &str, port: u16, sandbox: bool) -> Advert {
    let (name, mac_id) = (name.to_owned(), mac_id.to_owned());
    Advert(tokio::spawn(async move {
        let txt = txt_record(&[("mac_id", &mac_id), ("name", &name)]);
        loop {
            match register(&name, service_type(sandbox), port, &txt) {
                Ok(advertisement) => {
                    let mut logged = None;
                    loop {
                        report(&advertisement, &mut logged);
                        tokio::time::sleep(CHECK).await;
                    }
                }
                Err(error) => {
                    log!("Bonjour advertisement unavailable ({error}); add this Mac by address");
                    set_status("unavailable");
                }
            }
            tokio::time::sleep(RETRY).await;
        }
    }))
}
