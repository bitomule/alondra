//! Bonjour through macOS's own DNS-SD service (mDNSResponder, `dns_sd.h` in libSystem): the daemon
//! advertises its direct listener, terminals find Macs. mDNSResponder calls back more than once
//! (a rename after a name clash, a network change), so every callback only records what it got
//! and never panics: a panic there would abort the whole process.
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

type ServiceRef = *mut c_void;
type RegisterReply =
    extern "C" fn(ServiceRef, u32, i32, *const c_char, *const c_char, *const c_char, *mut c_void);
type BrowseReply =
    extern "C" fn(ServiceRef, u32, u32, i32, *const c_char, *const c_char, *const c_char, *mut c_void);
type ResolveReply =
    extern "C" fn(ServiceRef, u32, u32, i32, *const c_char, *const c_char, u16, u16, *const u8, *mut c_void);

const NO_ERROR: i32 = 0;
pub const POLICY_DENIED: i32 = -65570;
const FLAGS_ADD: u32 = 0x2;
const PENDING: i32 = i32::MIN;

unsafe extern "C" {
    fn DNSServiceRegister(
        service: *mut ServiceRef,
        flags: u32,
        interface: u32,
        name: *const c_char,
        regtype: *const c_char,
        domain: *const c_char,
        host: *const c_char,
        port: u16,
        txt_len: u16,
        txt: *const c_void,
        callback: RegisterReply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceBrowse(
        service: *mut ServiceRef,
        flags: u32,
        interface: u32,
        regtype: *const c_char,
        domain: *const c_char,
        callback: BrowseReply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceResolve(
        service: *mut ServiceRef,
        flags: u32,
        interface: u32,
        name: *const c_char,
        regtype: *const c_char,
        domain: *const c_char,
        callback: ResolveReply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceRefSockFD(service: ServiceRef) -> c_int;
    fn DNSServiceProcessResult(service: ServiceRef) -> i32;
    fn DNSServiceRefDeallocate(service: ServiceRef);
}

/// A TXT record: length-prefixed `key=value` strings, each at most 255 bytes.
pub fn txt_record(pairs: &[(&str, &str)]) -> Vec<u8> {
    let mut record = Vec::new();
    for (key, value) in pairs {
        let mut entry = format!("{key}={value}").into_bytes();
        entry.truncate(255);
        record.push(entry.len() as u8);
        record.extend(entry);
    }
    record
}

/// Waits up to `timeout` for `service`'s socket and hands what arrived to its callback.
fn process(service: ServiceRef, timeout: Duration) -> bool {
    // SAFETY: `service` is a live reference this module created and has not deallocated.
    let fd = unsafe { DNSServiceRefSockFD(service) };
    if fd < 0 {
        return false;
    }
    let mut poll = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    // SAFETY: one valid pollfd for the duration of the call.
    let ready = unsafe { libc::poll(&mut poll, 1, timeout.as_millis().min(i32::MAX as u128) as c_int) };
    if ready > 0 && poll.revents & libc::POLLIN != 0 {
        // SAFETY: the socket is readable, so this does not block; the callback cannot unwind.
        return unsafe { DNSServiceProcessResult(service) } == NO_ERROR;
    }
    ready >= 0
}

extern "C" fn register_reply(
    _: ServiceRef,
    _: u32,
    error: i32,
    _: *const c_char,
    _: *const c_char,
    _: *const c_char,
    context: *mut c_void,
) {
    // SAFETY: `context` is the `AtomicI32` the advertising thread keeps alive while registered.
    let status = unsafe { &*(context as *const AtomicI32) };
    status.store(error, Ordering::SeqCst);
}

/// An advertised service; dropping it withdraws the advertisement.
pub struct Advertisement {
    stop: Arc<AtomicBool>,
    status: Arc<AtomicI32>,
}

impl Advertisement {
    /// The last answer from mDNSResponder: `Some(0)` advertised, `Some(POLICY_DENIED)` when
    /// macOS withholds Local Network access, `None` while it has not answered.
    pub fn status(&self) -> Option<i32> {
        Some(self.status.load(Ordering::SeqCst)).filter(|status| *status != PENDING)
    }
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Starts advertising `name` as `regtype` on `port`, on every interface, from a thread of its own.
pub fn advertise(name: &str, regtype: &str, port: u16, txt: &[u8]) -> std::io::Result<Advertisement> {
    let name = CString::new(name).map_err(std::io::Error::other)?;
    let regtype = CString::new(regtype).map_err(std::io::Error::other)?;
    let txt = txt.to_vec();
    let txt_len = u16::try_from(txt.len()).map_err(std::io::Error::other)?;
    let stop = Arc::new(AtomicBool::new(false));
    let status = Arc::new(AtomicI32::new(PENDING));
    let (stop_flag, status_cell) = (stop.clone(), status.clone());
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("alondra-bonjour".into()).spawn(move || {
        let mut service: ServiceRef = std::ptr::null_mut();
        let context = Arc::as_ptr(&status_cell) as *mut c_void;
        // SAFETY: every pointer outlives the call; `context` stays valid while `status_cell` lives,
        // which is until after the reference is deallocated below.
        let error = unsafe {
            DNSServiceRegister(
                &mut service,
                0,
                0,
                name.as_ptr(),
                regtype.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                port.to_be(),
                txt_len,
                txt.as_ptr().cast(),
                register_reply,
                context,
            )
        };
        let _ = started_tx.send(error);
        if error != NO_ERROR {
            return;
        }
        while !stop_flag.load(Ordering::SeqCst) {
            if !process(service, Duration::from_millis(250)) {
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        // SAFETY: deallocated once, by the only thread that uses it.
        unsafe { DNSServiceRefDeallocate(service) };
        drop(status_cell);
    })?;
    match started_rx.recv() {
        Ok(NO_ERROR) => Ok(Advertisement { stop, status }),
        Ok(error) => Err(std::io::Error::other(format!("DNSServiceRegister failed ({error})"))),
        Err(_) => Err(std::io::Error::other("Bonjour thread ended")),
    }
}

/// A service Bonjour found and resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub name: String,
    pub host: String,
    pub port: u16,
}

struct Seen {
    name: String,
    regtype: CString,
    domain: CString,
    interface: u32,
}

extern "C" fn browse_reply(
    _: ServiceRef,
    flags: u32,
    interface: u32,
    error: i32,
    name: *const c_char,
    regtype: *const c_char,
    domain: *const c_char,
    context: *mut c_void,
) {
    if error != NO_ERROR || flags & FLAGS_ADD == 0 || name.is_null() || regtype.is_null() || domain.is_null() {
        return;
    }
    // SAFETY: `context` is the browse loop's `Vec<Seen>`, alive for the whole browse; the strings
    // are NUL-terminated for the duration of the callback.
    let seen = unsafe { &mut *(context as *mut Vec<Seen>) };
    let (name, regtype, domain) = unsafe { (CStr::from_ptr(name), CStr::from_ptr(regtype), CStr::from_ptr(domain)) };
    seen.push(Seen {
        name: name.to_string_lossy().into_owned(),
        regtype: regtype.to_owned(),
        domain: domain.to_owned(),
        interface,
    });
}

extern "C" fn resolve_reply(
    _: ServiceRef,
    _: u32,
    _: u32,
    error: i32,
    _: *const c_char,
    host: *const c_char,
    port: u16,
    _: u16,
    _: *const u8,
    context: *mut c_void,
) {
    if error != NO_ERROR || host.is_null() {
        return;
    }
    // SAFETY: `context` is the resolver's `Option<(String, u16)>`, alive until it is deallocated.
    let resolved = unsafe { &mut *(context as *mut Option<(String, u16)>) };
    let host = unsafe { CStr::from_ptr(host) }.to_string_lossy().trim_end_matches('.').to_owned();
    *resolved = Some((host, u16::from_be(port)));
}

fn resolve(seen: &Seen, until: Instant) -> Option<(String, u16)> {
    let name = CString::new(seen.name.clone()).ok()?;
    let mut resolved: Option<(String, u16)> = None;
    let mut service: ServiceRef = std::ptr::null_mut();
    // SAFETY: the strings and `resolved` outlive the reference, deallocated before returning.
    let error = unsafe {
        DNSServiceResolve(
            &mut service,
            0,
            seen.interface,
            name.as_ptr(),
            seen.regtype.as_ptr(),
            seen.domain.as_ptr(),
            resolve_reply,
            (&mut resolved as *mut Option<(String, u16)>).cast(),
        )
    };
    if error != NO_ERROR {
        return None;
    }
    while resolved.is_none() {
        let Some(left) = until.checked_duration_since(Instant::now()) else { break };
        if !process(service, left.min(Duration::from_millis(250))) {
            break;
        }
    }
    // SAFETY: deallocated once, after its last callback.
    unsafe { DNSServiceRefDeallocate(service) };
    resolved
}

/// Every `regtype` service advertised on the network during `window`, resolved to host and port.
pub fn browse(regtype: &str, window: Duration) -> std::io::Result<Vec<Found>> {
    let regtype = CString::new(regtype).map_err(std::io::Error::other)?;
    let mut seen: Vec<Seen> = Vec::new();
    let mut service: ServiceRef = std::ptr::null_mut();
    // SAFETY: `seen` outlives the reference, deallocated before it is read.
    let error = unsafe {
        DNSServiceBrowse(
            &mut service,
            0,
            0,
            regtype.as_ptr(),
            std::ptr::null(),
            browse_reply,
            (&mut seen as *mut Vec<Seen>).cast(),
        )
    };
    if error != NO_ERROR {
        return Err(std::io::Error::other(format!("DNSServiceBrowse failed ({error})")));
    }
    let until = Instant::now() + window;
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        if !process(service, left.min(Duration::from_millis(250))) {
            break;
        }
    }
    // SAFETY: deallocated once; no callback runs after this.
    unsafe { DNSServiceRefDeallocate(service) };
    let resolve_until = Instant::now() + Duration::from_secs(2);
    let mut found: Vec<Found> = Vec::new();
    for entry in &seen {
        if let Some((host, port)) = resolve(entry, resolve_until) {
            let item = Found { name: entry.name.clone(), host, port };
            if !found.contains(&item) {
                found.push(item);
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txt_entries_are_length_prefixed() {
        assert_eq!(txt_record(&[("mac_id", "mac-1"), ("name", "Ana")]), b"\x0cmac_id=mac-1\x08name=Ana".to_vec());
        let long = "x".repeat(300);
        assert_eq!(txt_record(&[("k", &long)])[0], 255);
    }

    #[test]
    #[ignore = "advertises on this Mac's mDNSResponder; run with --ignored"]
    fn an_advertisement_survives_a_second_registration_of_its_name_and_is_found() {
        let regtype = "_alondra-sandbox._tcp";
        let txt = txt_record(&[("mac_id", "mac-test")]);
        let first = advertise("Alondra bonjour test", regtype, 47_901, &txt).unwrap();
        let second = advertise("Alondra bonjour test", regtype, 47_902, &txt).unwrap();
        let found = browse(regtype, Duration::from_secs(4)).unwrap();
        assert!(found.iter().any(|f| f.port == 47_901), "{found:?}");
        assert!(found.iter().any(|f| f.port == 47_902), "the clashing name is renamed, not fatal: {found:?}");
        assert_eq!(first.status(), Some(0));
        assert_eq!(second.status(), Some(0));
        drop(first);
        drop(second);
    }
}
