//! The engine a scenario runs: the daemon's, over the doubles of the Mac.
/// Opens every runtime the engine's fixtures open on this thread as the daemon does, with the gate
/// of a test.
pub fn wired() {
    alondra_engine_kernel::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite);
    alondra_daemon_engine::wiring::open_with(open_at);
}

/// The runtime as the daemon opens it, with the doubles of the Mac and a gate that is unpaced and
/// launches on the disk the machine watch measured.
pub fn open_at(home: &std::path::Path, database: &std::path::Path, files: &dyn alondra_engine_ports::journal::JournalFiles) -> anyhow::Result<alondra_engine_sdk::domain_runtime::DomainRuntime> {
    let runtime = alondrad::wiring::open_at(home, database, files)?;
    alondra_engine_doubles::machine::wire(runtime.ports(), alondra_engine_ports::watcher::claude_home);
    alondra_agent_runtime::wire(runtime.ports(), Default::default());
    alondra_engine_admission::for_tests(&runtime);
    Ok(runtime)
}

/// `open_at` at the home's active database; the engine's fixtures this thread opens open the same.
pub fn open(home: &std::path::Path) -> anyhow::Result<alondra_engine_sdk::domain_runtime::DomainRuntime> {
    wired();
    open_at(home, &alondra_engine_ports::journal::active_path(home), &alondra_store_sqlite::Sqlite)
}

