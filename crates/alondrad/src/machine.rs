use crate::locks::Poisonable as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use alondra_core::{Machine, MemoryCompressor, MemoryPressure, now_millis};
use sysinfo::System;

const UNRELIABLE_LOAD: f64 = 50.0;
/// Where a Mac keeps its writable data; `/` on any other system.
pub(crate) const BOOT_DATA: &str = "/System/Volumes/Data";

/// The latest gauges of the resource API daemon, read by `/daemon` for the TUI header. A sample is
/// only ever replaced whole, so a lock whose holder panicked still holds a whole one.
/// A holder that panics leaves it whole, so it is recovered: the sample is only ever replaced
/// whole.
pub(crate) type Gauges = Arc<std::sync::Mutex<Option<(Machine, Option<alondra_core::usage::Usage>, Vec<alondra_core::usage::ProviderUsage>)>>>;

/// The plan's limits travel with the machine's gauges, read from what each account measured.
pub(crate) async fn run(gauges: Gauges, domain: Arc<crate::domain_runtime::DomainRuntime>) {
    sample_loop(|machine| {
        let usage = crate::usage_probe::usage(&domain, machine.at);
        let providers = domain.read().index().providers(machine.at, &crate::node_reports::local_day(machine.at));
        *gauges.lock_or_recover("the machine gauges") = Some((machine, usage, providers));
    }).await
}

async fn sample_loop(mut publish: impl FnMut(Machine)) {
    let mut sys = System::new();
    loop {
        let (next, machine) = tokio::task::spawn_blocking(move || {
            sys.refresh_memory();
            sys.refresh_cpu_usage();
            let pressure = forced_memory_free_percent().or_else(memorystatus_level);
            let load = System::load_average();
            let root = crate::disk::space(Path::new(BOOT_DATA)).or_else(|| crate::disk::space(Path::new("/")));
            let machine = Machine {
                at: now_millis(),
                memory_total: sys.total_memory(),
                memory_available: sys.available_memory(),
                memory_free_percent: pressure,
                memory_pressure: memorystatus_vm_pressure_level().and_then(MemoryPressure::from_kernel_level),
                compressor: compressor(),
                load: [load.one, load.five, load.fifteen],
                cpus: sys.cpus().len(),
                cpu_usage: sys.global_cpu_usage(),
                disk_total: root.map_or(0, |space| space.total),
                disk_available: root.map_or(0, |space| space.free),
                disk_purgeable: root.map_or(0, |space| space.purgeable),
                unreliable: load.one > UNRELIABLE_LOAD,
            };
            (sys, machine)
        }).await.expect("machine sample worker failed");
        sys = next;
        publish(machine);
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

/// `ALONDRA_MEMORY_FREE_PERCENT` stands for the Mac's free memory, so a sandbox can try the pacing
/// on a Mac with memory to spare.
fn forced_memory_free_percent() -> Option<u32> {
    std::env::var("ALONDRA_MEMORY_FREE_PERCENT").ok()?.parse().ok().filter(|free| *free <= 100)
}

/// The free memory percentage `memory_pressure` prints, read from the kernel without a process.
fn memorystatus_level() -> Option<u32> {
    let level = sysctl_int(c"kern.memorystatus_level")?;
    (0..=100).contains(&level).then_some(level as u32)
}

/// The pressure level macOS tells apps about: 1 normal, 2 warning, 4 critical.
fn memorystatus_vm_pressure_level() -> Option<i32> {
    sysctl_int(c"kern.memorystatus_vm_pressure_level")
}

/// The counters macOS weighs to report memory pressure, with what is swapped. Under 3 GB of memory
/// macOS warns at another share of the compressor, so they are left out there.
#[cfg(target_os = "macos")]
fn compressor() -> Option<MemoryCompressor> {
    static HOST: std::sync::OnceLock<libc::mach_port_t> = std::sync::OnceLock::new();
    if sysctl_value::<u64>(c"hw.memsize")? <= 3 << 30 {
        return None;
    }
    // SAFETY: sysconf only reads a system constant.
    let page_size = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
    // SAFETY: vm_statistics64 is plain integers, for which all zeroes is a valid value.
    let mut stats: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    #[allow(deprecated, reason = "libc points to the mach2 crate, which declares the same call")]
    // SAFETY: mach_host_self has no preconditions.
    let host = *HOST.get_or_init(|| unsafe { libc::mach_host_self() });
    // SAFETY: `count` says how many integers `stats` holds.
    let status = unsafe { libc::host_statistics64(host, libc::HOST_VM_INFO64, (&raw mut stats).cast(), &raw mut count) };
    if status != libc::KERN_SUCCESS {
        return None;
    }
    let swap = sysctl_value::<libc::xsw_usage>(c"vm.swapusage")?;
    Some(MemoryCompressor {
        page_size,
        uncompressed_pages: u64::from(stats.free_count) + u64::from(stats.active_count) + u64::from(stats.inactive_count),
        compressor_pages: u64::from(stats.compressor_page_count),
        stored_pages: stats.total_uncompressed_pages_in_compressor,
        swap_used: swap.xsu_used,
        swapins: stats.swapins,
        swapouts: stats.swapouts,
    })
}

#[cfg(not(target_os = "macos"))]
fn compressor() -> Option<MemoryCompressor> {
    None
}

#[cfg(target_os = "macos")]
fn sysctl_value<T: Copy>(name: &std::ffi::CStr) -> Option<T> {
    let mut value = std::mem::MaybeUninit::<T>::zeroed();
    let mut size = std::mem::size_of::<T>();
    // SAFETY: the name is NUL-terminated, `value` has room for `size` bytes, and the result is read
    // only when the kernel filled exactly a `T`.
    let status = unsafe { libc::sysctlbyname(name.as_ptr(), value.as_mut_ptr().cast(), &raw mut size, std::ptr::null_mut(), 0) };
    // SAFETY: the kernel wrote all `size_of::<T>()` bytes, and T is plain integers.
    (status == 0 && size == std::mem::size_of::<T>()).then(|| unsafe { value.assume_init() })
}

#[cfg(target_os = "macos")]
fn sysctl_int(name: &std::ffi::CStr) -> Option<i32> {
    sysctl_value::<libc::c_int>(name)
}

#[cfg(not(target_os = "macos"))]
fn sysctl_int(_name: &std::ffi::CStr) -> Option<i32> {
    None
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn the_counters_macos_weighs_are_read_from_the_kernel() {
        let counters = super::compressor().expect("macOS with more than 3 GB reports its compressor");
        assert!(counters.page_size >= 4096);
        assert!(counters.uncompressed_pages > 0);
        assert!((0.0..1.0).contains(&counters.share()));
    }
}
