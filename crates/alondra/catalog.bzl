"""The crash report's log catalog: what each crate's production sources add to it.

The root compiles the catalog; `//crates/cli-report`'s test reads the same sources and files.
"""

# Each crate's part of the crash report's log catalog, from its production sources, in the
# catalog's order: `--log` reads its log calls, `--words` the words of its code
# (`crates/cli-report/src/report/extract.rs`). The report's own schema adds the values it expects in
# the state it reads, among them phases only older releases stored.
CATALOG_FRAGMENTS = [
    ("alondrad", "//crates/alondrad:report_sources", "--log --words"),
    ("daemon_server", "//crates/daemon-server:report_sources", "--log --words"),
    ("alondrad_launcher", "//crates/alondrad:launcher_report_sources", "--log"),
    ("daemon_support", "//crates/daemon-support:report_sources", "--log --words"),
    ("system", "//crates/system:report_sources", "--log --words"),
    ("daemon_engine", "//crates/daemon-engine:report_sources", "--log --words"),
    ("daemon_engine_model", "//crates/daemon-engine-model:report_sources", "--log --words"),
    ("store_sqlite", "//crates/store-sqlite:report_sources", "--log --words"),
    ("daemon_engine_machine", "//crates/daemon-engine-machine:report_sources", "--log --words"),
    ("engine_admission", "//crates/engine-admission:report_sources", "--log --words"),
    ("engine_github", "//crates/engine-github:report_sources", "--log --words"),
    ("engine_live", "//crates/engine-live:report_sources", "--log --words"),
    ("engine_mcp", "//crates/engine-mcp:report_sources", "--log --words"),
    ("engine_worktrees", "//crates/engine-worktrees:report_sources", "--log --words"),
    ("engine_kernel", "//crates/engine-kernel:report_sources", "--log --words"),
    ("engine_ports", "//crates/engine-ports:report_sources", "--words"),
    ("github_gh", "//crates/github-gh:report_sources", "--words"),
    ("engine_orchestrator", "//crates/engine-orchestrator:report_sources", "--log --words"),
    ("engine_sdk", "//crates/engine-sdk:report_sources", "--log --words"),
    ("core", "//crates/core:report_sources", "--log --words"),
    ("host", "//crates/host:report_sources", "--log --words"),
    ("domain_rules", "//crates/domain-rules:report_sources", "--log --words"),
    ("view_model", "//crates/view-model:report_sources", "--log --words"),
    ("views", "//crates/views:report_sources", "--log --words"),
    ("provider_claude", "//crates/provider-claude:report_sources", "--log --words"),
    ("domain", "//crates/domain:report_sources", "--words"),
    ("foundation", "//crates/foundation:report_sources", "--log --words"),
    ("store", "//crates/store:report_sources", "--words"),
    ("agent_application", "//crates/agent-application:report_sources", "--words"),
    ("agent_protocol", "//crates/agent-protocol:report_sources", "--words"),
    ("agent_runtime", "//crates/agent-runtime:report_sources", "--words"),
    ("provider_contract", "//crates/provider-contract:report_sources", "--words"),
    ("process_group", "//crates/process-group:report_sources", "--words"),
    ("report_schema", "//crates/cli-report:src/report/check/schema.rs", "--words"),
]

# The crates' production sources the catalog reads; the report's schema is the report's own.
REPORT_SOURCES = [sources for name, sources, _ in CATALOG_FRAGMENTS if name != "report_schema"]
