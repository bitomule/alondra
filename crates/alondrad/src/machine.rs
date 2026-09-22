use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use alondra_core::{Machine, now_millis};
use sysinfo::{Disks, System};

use crate::hub::Hub;

const UNRELIABLE_LOAD: f64 = 50.0;

pub async fn run(hub: Arc<Hub>) {
    let mut sys = System::new();
    let mut disks = Disks::new_with_refreshed_list();
    let mut pressure: Option<u32> = None;
    let mut round: u64 = 0;
    loop {
        sys.refresh_memory();
        sys.refresh_cpu_usage();
        disks.refresh(true);
        if round.is_multiple_of(3) {
            pressure = tokio::task::spawn_blocking(memory_free_percent).await.ok().flatten();
        }
        let load = System::load_average();
        let root = disks
            .list()
            .iter()
            .find(|d| d.mount_point() == std::path::Path::new("/System/Volumes/Data"))
            .or_else(|| disks.list().iter().find(|d| d.mount_point() == std::path::Path::new("/")));
        let machine = Machine {
            at: now_millis(),
            memory_total: sys.total_memory(),
            memory_available: sys.available_memory(),
            memory_free_percent: pressure,
            load: [load.one, load.five, load.fifteen],
            cpus: sys.cpus().len(),
            cpu_usage: sys.global_cpu_usage(),
            disk_total: root.map(|d| d.total_space()).unwrap_or(0),
            disk_available: root.map(|d| d.available_space()).unwrap_or(0),
            unreliable: load.one > UNRELIABLE_LOAD,
        };
        hub.set_machine(machine);
        round += 1;
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

fn memory_free_percent() -> Option<u32> {
    let out = Command::new("/usr/bin/memory_pressure").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains("memory free percentage"))?;
    line.rsplit(':').next()?.trim().trim_end_matches('%').parse().ok()
}
