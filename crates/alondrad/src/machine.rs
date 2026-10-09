use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use alondra_core::{Machine, MemoryPressure, now_millis};
use sysinfo::System;

const UNRELIABLE_LOAD: f64 = 50.0;
/// Where a Mac keeps its writable data; `/` on any other system.
pub(crate) const BOOT_DATA: &str = "/System/Volumes/Data";

/// The latest gauges of the resource API daemon, read by `/daemon` for the TUI header.
pub(crate) type Gauges = Arc<std::sync::Mutex<Option<(Machine, Option<alondra_core::usage::Usage>)>>>;

/// The plan's limits travel with the machine's gauges, read from what the node mods measured.
pub(crate) async fn run(gauges: Gauges, domain: Arc<crate::domain_runtime::DomainRuntime>) {
    sample_loop(|machine| {
        let usage = domain.live.mods.usage(machine.at);
        *gauges.lock().expect("machine gauges poisoned") = Some((machine, usage));
    }).await
}

async fn sample_loop(mut publish: impl FnMut(Machine)) {
    let mut sys = System::new();
    let mut pressure: Option<u32> = None;
    let mut round: u64 = 0;
    loop {
        sys.refresh_memory();
        sys.refresh_cpu_usage();
        pressure = match memorystatus_level() {
            Some(level) => Some(level),
            None if round.is_multiple_of(3) => tokio::task::spawn_blocking(memory_free_percent).await.ok().flatten(),
            None => pressure,
        };
        let load = System::load_average();
        let root = crate::disk::space(Path::new(BOOT_DATA)).or_else(|| crate::disk::space(Path::new("/")));
        let machine = Machine {
            at: now_millis(),
            memory_total: sys.total_memory(),
            memory_available: sys.available_memory(),
            memory_free_percent: pressure,
            memory_pressure: memorystatus_vm_pressure_level().and_then(MemoryPressure::from_kernel_level),
            load: [load.one, load.five, load.fifteen],
            cpus: sys.cpus().len(),
            cpu_usage: sys.global_cpu_usage(),
            disk_total: root.map_or(0, |space| space.total),
            disk_available: root.map_or(0, |space| space.free),
            unreliable: load.one > UNRELIABLE_LOAD,
        };
        publish(machine);
        round += 1;
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
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

#[cfg(target_os = "macos")]
fn sysctl_int(name: &std::ffi::CStr) -> Option<i32> {
    let mut value: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is NUL-terminated and `value` is a c_int whose size is passed in `size`.
    let status = unsafe { libc::sysctlbyname(name.as_ptr(), (&raw mut value).cast(), &raw mut size, std::ptr::null_mut(), 0) };
    (status == 0).then_some(value)
}

#[cfg(not(target_os = "macos"))]
fn sysctl_int(_name: &std::ffi::CStr) -> Option<i32> {
    None
}

fn memory_free_percent() -> Option<u32> {
    let out = Command::new("/usr/bin/memory_pressure").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains("memory free percentage"))?;
    line.rsplit(':').next()?.trim().trim_end_matches('%').parse().ok()
}
