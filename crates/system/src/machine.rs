//! The gauges of the Mac the daemon runs on: its memory from the kernel, its load and cores, and its
//! own disk read through the `Disk` port.
use alondra_core::{Machine, MemoryCompressor, MemoryPressure, now_millis};
use alondra_engine_ports::disk::{BOOT_DATA, Disk};
use alondra_engine_ports::machine::MachineProbe;
use alondra_foundation::locks::Poisonable as _;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use sysinfo::System;

const UNRELIABLE_LOAD: f64 = 50.0;

pub struct Mac {
    system: Mutex<System>,
    disk: Arc<dyn Disk>,
}

impl Mac {
    pub fn new(disk: Arc<dyn Disk>) -> Self {
        Self { system: Mutex::new(System::new()), disk }
    }
}

impl MachineProbe for Mac {
    fn sample(&self) -> Machine {
        let mut sys = self.system.lock_or_recover("the machine sampler");
        sys.refresh_memory();
        sys.refresh_cpu_usage();
        let pressure = forced_memory_free_percent().or_else(memorystatus_level);
        let load = System::load_average();
        let root = self.disk.space(Path::new(BOOT_DATA)).or_else(|| self.disk.space(Path::new("/")));
        Machine {
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
        }
    }

    fn size(&self) -> (usize, u64) {
        static SIZE: OnceLock<(usize, u64)> = OnceLock::new();
        *SIZE.get_or_init(|| {
            let mut system = System::new();
            system.refresh_memory();
            (std::thread::available_parallelism().map_or(1, |cores| cores.get()), system.total_memory())
        })
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
    let swap = sysctl_value::<libc::xsw_usage>(c"vm.swapusage")
        .filter(|swap| swap.xsu_used <= swap.xsu_total);
    Some(MemoryCompressor {
        page_size,
        uncompressed_pages: u64::from(stats.free_count) + u64::from(stats.active_count) + u64::from(stats.inactive_count),
        compressor_pages: u64::from(stats.compressor_page_count),
        stored_pages: stats.total_uncompressed_pages_in_compressor,
        swap_used: swap.map_or(0, |swap| swap.xsu_used),
        swap_total: swap.map(|swap| swap.xsu_total),
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

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    #[test]
    fn the_counters_macos_weighs_are_read_from_the_kernel() {
        let counters = super::compressor().expect("macOS with more than 3 GB reports its compressor");
        assert!(counters.page_size >= 4096);
        assert!(counters.uncompressed_pages > 0);
        assert!((0.0..1.0).contains(&counters.share()));
    }

    #[test]
    fn the_macs_gauges_keep_the_contract_the_doubles_keep() {
        alondra_engine_doubles::contract::machine(&super::Mac::new(std::sync::Arc::new(crate::Disks)));
    }
}
