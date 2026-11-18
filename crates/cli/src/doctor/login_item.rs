//! Whether macOS lets the service's LaunchAgent run (System Settings > General > Login Items &
//! Extensions), read through ServiceManagement: `+[SMAppService statusForLegacyURL:]` answers for
//! a plist in `~/Library/LaunchAgents` without asking for any authorization, unlike
//! `sfltool dumpbtm`, which shows an administrator prompt on macOS 26 and later.
use std::path::Path;

use super::system::StartupItem;

/// `SMAppServiceStatus`, as `SMAppService.h` numbers it.
const ENABLED: isize = 1;
const REQUIRES_APPROVAL: isize = 2;

/// Only `requiresApproval` means the person turned it off; not registered, not found or no answer
/// say nothing for certain.
pub fn startup_item_of(status: Option<isize>) -> StartupItem {
    match status {
        Some(ENABLED) => StartupItem::Allowed,
        Some(REQUIRES_APPROVAL) => StartupItem::Off,
        _ => StartupItem::Unknown,
    }
}

pub fn startup_item(plist: &Path) -> StartupItem {
    startup_item_of(legacy_status(plist))
}

#[cfg(target_os = "macos")]
fn legacy_status(plist: &Path) -> Option<isize> {
    use objc2::runtime::AnyClass;
    use objc2_foundation::{NSString, NSURL};

    #[link(name = "ServiceManagement", kind = "framework")]
    unsafe extern "C" {}

    let class = AnyClass::get(c"SMAppService")?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(plist.to_str()?));
    Some(unsafe { objc2::msg_send![class, statusForLegacyURL: &*url] })
}

#[cfg(not(target_os = "macos"))]
fn legacy_status(_plist: &Path) -> Option<isize> {
    None
}
