# Running the tests

Tests run **only through Bazel**, never `cargo test`, `swift test` or `xcodebuild`. Bazel pins its
own toolchains; install it with `bazelisk` (see
[`building-from-source.md`](building-from-source.md)).

| What | Command |
|---|---|
| Rust crates | `make test` (`bazelisk test //crates/...`) |
| Apple apps | `bazelisk test --build_tests_only -- //apple/... -//apple/NativeHarness/...` (the plain wildcard builds iOS-only libraries for macOS) |
| Brew packaging | `python3 -m unittest discover -s deploy/brew/tests` |
| Web | in `web/`: `npm ci --include=dev`, `npm run typecheck`, `npm run build`, `npm run test:e2e` |

A new integration test needs a Bazel target, or it does not run.

## Cached results

`bazelisk test` runs again only the targets whose declared inputs changed; every other target reports
`(cached) PASSED` from the local or remote cache. Run the scope you touched (`//crates/<crate>/...`
or `//...`) and let the cache skip the rest: there is no separate list of affected tests to keep.

A cached result is only as good as the inputs the target declares, so every test reads what its
BUILD target lists, from its runfiles. A test that reads the checkout (a lint over many packages)
lists the `tree` of each directory it reads: every package calls `repo_tree()` from
`tools/tree.bzl`, which declares `:files` (that package's files) and `:tree` (those plus the `:tree`
of every package below). `//tools:hermetic_tests_test` refuses a test that resolves a runfiles link
back to the checkout, and a test tagged `external`, `local`, `no-cache` or `no-remote` without a
reason in `tools/uncacheable_tests.tsv`.

`bazel run //tools:edit_bench` measures what one-line edits cost: the crates and test targets each
reopens, the actions Bazel really runs, the time to build alone, the time to edit and test, and the
test target that took longest. `--edit <path>` picks the files; without it, the files edited most
often in the last two weeks; `--tracked` measures one edit per layer. `--query-only` only counts.
On a shared Mac, where bazelqueue keeps a `bazel run` in its build slot until the program ends,
build the script and run it outside Bazel:
`bazelisk run --script_path=.scratch/edit_bench //tools:edit_bench && .scratch/edit_bench --edit <path>`.

`bazel run //tools:test_catalog -- list` prints every Rust test of `//crates/...` with its target and
whether it runs (`run`, `ignored`, or `manual` for a target no wildcard runs), and a line per
conversation scenario. A change that moves tests between targets shows in its pull request
`test_catalog -- compare before.tsv after.tsv`: a test that only changed target or module is found
by its function name, and the command fails on a test that is nowhere any more, or that ran and no
longer does, unless a line of `--moves` says where it went and why.

A test's package decides how often it runs. A test outside the root may not depend on a crate or a
binary of the root (`//tools/bazel:crate_layers_test` fails on it): it would rerun on every edit of
the daemon. The tests that start the real daemon several times are `manual` targets of the root
that the release's tests name (`RELEASE_ONLY_TESTS` in `deploy/brew/local-release.py`);
`bazelisk test //crates/alondrad:codex_adoption_process_test` runs them on request.

## Where a test writes

A test owns everything it creates. The wrapper gives it `TMPDIR`, a short private folder it makes
under `/tmp`, and removes it when the test ends, fails or is cut; `ALONDRA_SHORT_TMPDIR` names the
same folder for the fixtures that bind a socket (`Directory::for_unix_socket`, the scenario runner).
It is not Bazel's `TEST_TMPDIR`: a unix socket path is capped at 104 bytes and `TEST_TMPDIR` is longer
than that leaves room for, and it sits in the execroot, which links the checkout's `.git`, so a git or
project lookup from a fixture there would find the real repository. The wrapper empties `TEST_TMPDIR`
too. Test code never falls back to `/tmp`: a fixture that finds no short folder fails and says to run
through Bazel. `//tools:test_scratch_guard_test` refuses a fallback to, or a file created in, a shared
temporary folder in test-side Rust code.

## Invalid cached executables

`make test` selects the repository's test wrapper explicitly, including when a user bazelrc
selects another wrapper. For a direct invocation, use
`bazelisk test <target> --run_under=//tools:test_tmpdir`. The wrapper asks the operating system to
execute the test without Bash's fallback that interprets unrecognized executable bytes as shell
commands. Invalid executables fail with status 126 and a recovery command; valid native binaries
and scripts with a shebang keep their arguments and exit status.

When the wrapper rejects an executable it prints its size, SHA-256, first bytes, modification time and whether it sits on the boot volume or another one: keep that line from the test log before rebuilding.
Rebuild only the affected target in a new output base, without accepting or uploading cached
results:

```sh
bazelisk --output_base="$PWD/.scratch/recovery" test <target> \
  --noremote_accept_cached --disk_cache= --noremote_upload_local_results \
  --run_under=//tools:test_tmpdir
```

Do not purge a shared cache to recover one target. Bazel 8.4.2 verifies downloaded digests by
default; a matching digest proves identity with the recorded output, not that a compiler produced
a valid executable. `--experimental_remote_capture_corrupted_outputs="$PWD/.scratch/corrupt"`
can preserve outputs whose download fails verification.

Bazel 8.4.2 already defaults to `--remote_download_outputs=toplevel`. Use `all`, `toplevel` and
`minimal` explicitly when comparing downloads from fresh output bases. Keep `minimal` scoped to
cache warming: commands outside Bazel that copy or execute `bazel-bin` outputs need those files
materialized locally.

## Writing tests

- Every behavior change comes with a test that fails without the change and passes with it.
- No flaky tests: no wall-clock assertions, no sleeps racing other work, no retries that hide
  failures.
- Tests never touch the real `~/.alondra`, launchd job, Keychain, Homebrew install or provider
  logins. Use temporary `HOME` fixtures.
- Fixtures that write files own them: Rust tests use `crate::test_support::Directory`, Swift tests
  use `@Suite(.temporaryFiles)` and `TestFiles.directory()`. The tree is removed on success and
  failure.
- A test of the engine that launches a node starts no process: `engine-sdk`'s `model_fixture`
  gives its domain the doubled hosts of `engine-doubles`, and `//crates/provider-claude:claude_host_model`
  plays each Claude host and its native Claude. `//crates/engine-sdk:engine_sdk_test` and
  `//crates/engine-sdk:engine_sdk_engine_test` have no host executable among their inputs. A test
  that opens the whole engine (`engine_fixtures`) goes under the feature `whole-engine`, which only
  `engine_sdk_engine_test` and `engine_sdk_host_test` compile; `engine_sdk_test` runs on
  `model_fixture::open`, whose runtime has the doubles of `engine-doubles` for every other piece,
  `engine-doubles/src/sdk.rs` for what the SDK asks the engine. That double fails a question it
  does not answer with the name of the question. A test that needs the real Node or Codex host, or a fake native
  Claude's scripted behaviour the model does not play, goes under the feature `real-hosts`, which
  only `//crates/engine-sdk:engine_sdk_host_test` compiles. What the model plays is kept honest by
  the behaviour cases of `provider-claude/src/host_cases/`, which `sdk_worker_test` runs on the real
  host and `claude_host_model_test` on the model.

## Programs a test runs

A test runs no external tool: no `git`, `gh`, `claude`, `codex`, Node host, `launchctl` or any
other program but its own executable and the interpreter of its script. The test wrapper
(`tools/test-tmpdir.sh`, which every `bazel test` runs under) turns that into a kernel sandbox: any
other exec fails with `Operation not permitted`, and a failing test prints how to see what was
refused. The kernel names the refused program only in the unified log, so run the test again
while this runs in another terminal:

```sh
log stream --style compact --predicate 'sender == "Sandbox" AND eventMessage CONTAINS "process-exec"'
```

The test's own executable is its resolved path, so a test that runs itself again still may. A
script test also runs the interpreter its first line names and what Bazel's launchers run before
it: `dirname`, `basename`, `uname`, `ln`, `env`, `bash`, the `python3` on its `PATH` and the
programs of Bazel's external repositories, the toolchains (the Node of a `js_test`). A program of
this repository is never part of the test: a sibling binary is an exception like any other.

The exceptions of today are declared per target, in its BUILD `env`, one entry per program:

```starlark
env = {"TEST_EXECS": "/usr/bin/git $(rootpath :alondra) tmpdir"},
```

- a system path under `/bin`, `/sbin`, `/usr/bin`, `/usr/sbin` or `/usr/libexec`. An Xcode shim
  (`/usr/bin/git`, `/usr/bin/python3`) brings the tool it runs and the `/bin/sh` and `xcodebuild`
  it asks the first time a new `HOME` runs it;
- `$(rootpath <label>)` of an executable the target lists in `data`;
- `path:<name>`: the program of that name the test's `PATH` finds, for a tool installed outside the
  system folders (the Claude Code `//:plugin_mod_test` runs);
- `tmpdir`: programs the test writes under its own `TMPDIR` or `TEST_TMPDIR` (fake `gh`, `claude`);
- `own-sandbox`, alone: the test starts `sandbox-exec` itself, which cannot run inside the guard's
  sandbox, so the target runs without the guard;
- `e2e`, alone, on a `manual` target tagged `e2e`: an end-to-end test that exists to run a real
  tool. It leaves the validation of every edit and runs with the release, which names it in
  `RELEASE_ONLY_TESTS` (`deploy/brew/local-release.py`); `bazelisk test <target>` runs it on
  request.

The list only shrinks. `//tools:test_execs_test` fails on a malformed entry, on a program with no
reason in `tools/test_execs.tsv`, and when the entries pass `EXEC_CEILING` or `OWN_SANDBOX_CEILING`
in `tools/test_execs.py`; a change that removes an entry lowers the ceiling. A test that needs
a program runs it inside the test, or uses a double, instead of adding an entry. The table below is
generated from the BUILD files: `bazel run //tools:test_execs -- write` rewrites it.

<!-- test-execs:begin -->
| Program | Targets | Why |
|---|---|---|
| `tmpdir` | `//:sourcekit_lsp_workspace_test`, `//agent-host:probe_test`, `//agent-host:umask_test`, `//crates/alondra:cli_api_test`, `//crates/alondra:doctor_service_test`, `//crates/alondrad:launcher_test`, `//crates/alondrad:relaunch_test`, `//crates/cli-tui:cli_tui_test`, `//crates/cli:cli_test`, `//crates/core:alondra_core_test`, `//crates/daemon-engine:daemon_engine_test`, `//crates/daemon-server:daemon_server_test`, `//crates/daemon-support:daemon_support_test`, `//crates/engine-github:engine_github_test`, `//crates/engine-kernel:engine_kernel_test`, `//crates/engine-mcp:engine_mcp_test`, `//crates/engine-orchestrator:engine_orchestrator_test`, `//crates/engine-sdk:engine_sdk_host_test`, `//crates/github-gh:github_gh_test`, `//crates/host:host_test`, `//crates/provider-claude:sdk_worker_test`, `//crates/system:system_test`, `//deploy/apple:increment_version_test`, `//deploy/brew:local_release_test`, `//tools:bazel_artifact_paths_test`, `//tools:buildbuddy_credential_helper_test`, `//tools:plugin_mod_test_tmpdir_test`, `//tools:reap_test_hosts_test` | Fake `gh`, `claude`, `codex`, `brew` and daemons the tests write under their own temporary folder. |
| `/bin/sh` | `//agent-host:umask_test`, `//crates/alondra:cli_api_test`, `//crates/alondra:doctor_service_test`, `//crates/alondrad:launcher_test`, `//crates/alondrad:relaunch_test`, `//crates/cli-tui:cli_tui_test`, `//crates/cli:cli_test`, `//crates/core:alondra_core_test`, `//crates/daemon-engine:daemon_engine_test`, `//crates/daemon-server:daemon_server_test`, `//crates/daemon-support:daemon_support_test`, `//crates/engine-github:engine_github_test`, `//crates/engine-mcp:engine_mcp_test`, `//crates/github-gh:github_gh_test`, `//crates/host:host_test`, `//crates/process-group:process_group_test`, `//crates/system:system_test`, `//deploy/apple:increment_version_test`, `//deploy/brew:local_release_test`, `//tools/worktree_setup_test:setup_test`, `//tools:bazel_artifact_paths_test`, `//tools:plugin_mod_test_tmpdir_test` | Tests run shell snippets as the process under test. |
| `/usr/bin/git` | `//crates/cli:cli_test`, `//crates/core:alondra_core_test`, `//crates/daemon-engine:daemon_engine_test`, `//crates/daemon-server:daemon_server_test`, `//crates/engine-github:engine_github_test`, `//crates/engine-kernel:engine_kernel_test`, `//crates/engine-orchestrator:engine_orchestrator_test`, `//crates/engine-sdk:engine_sdk_engine_test`, `//crates/engine-sdk:engine_sdk_host_test`, `//crates/system:system_test`, `//deploy/apple:increment_version_test`, `//tools/sandbox:alondra-sandbox_test`, `//tools/worktree_setup_test:setup_test`, `//tools:public_data_guard_test`, `//tools:worktree_volume_bench_test` | The engine and CLI tests make real repositories and worktrees. |
| `/bin/cat` | `//crates/agent-runtime:readiness_test`, `//crates/alondra:doctor_service_test`, `//crates/cli:cli_test`, `//crates/daemon-engine:daemon_engine_test`, `//crates/daemon-server:daemon_server_test`, `//crates/engine-github:engine_github_test`, `//crates/engine-mcp:engine_mcp_test`, `//crates/engine-orchestrator:engine_orchestrator_test`, `//crates/github-gh:github_gh_test`, `//crates/host:host_test`, `//crates/process-group:process_group_test`, `//tools:buildbuddy_credential_helper_test` | Tests that need a process that reads its input. |
| `/usr/bin/id` | `//crates/alondra:cli_api_test`, `//crates/alondra:doctor_service_test`, `//crates/alondrad:relaunch_test`, `//crates/cli-tui:cli_tui_test`, `//crates/daemon-engine:daemon_engine_test`, `//crates/daemon-server:daemon_server_test`, `//crates/engine-orchestrator:engine_orchestrator_test`, `//crates/engine-sdk:engine_sdk_host_test`, `//crates/scenarios:scenarios_harness_test`, `//crates/system:system_test` | The code under test asks `id` for the user it runs as. |
| `/bin/sleep` | `//crates/alondrad:launcher_test`, `//crates/daemon-server:daemon_server_test`, `//crates/daemon-support:daemon_support_test`, `//crates/engine-orchestrator:engine_orchestrator_test`, `//crates/engine-sdk:engine_sdk_host_test`, `//crates/host:host_test`, `//crates/process-group:process_group_test`, `//crates/scenarios:scenarios_harness_test`, `//tools:reap_test_hosts_test` | Tests that need a process that keeps running for a while. |
| `/usr/bin/python3` | `//agent-host:umask_test`, `//crates/daemon-engine:daemon_engine_test`, `//crates/daemon-server:daemon_server_test`, `//crates/engine-orchestrator:engine_orchestrator_test`, `//crates/engine-sdk:engine_sdk_host_test`, `//crates/provider-claude:sdk_worker_test`, `//crates/scenarios:from_report_bin_test`, `//crates/system:system_test` | `HostRoot` fixtures start a Python reaper beside each host; some tests run Python fakes. |
| `//agent-host:persistent-fake-claude.py` | `//agent-host:consecutive_inputs_test`, `//agent-host:controls_test`, `//agent-host:decisions_test`, `//agent-host:host_stop_test`, `//agent-host:probe_test`, `//agent-host:reconcile_test`, `//agent-host:umask_test` | A fake Claude Code the hosts under test talk to. |
| `//crates/alondra:alondra` | `//crates/alondra:cli_api_test`, `//crates/alondra:doctor_json_test`, `//crates/alondra:doctor_service_test`, `//crates/alondra:domain_mcp_test`, `//crates/alondra:hook_transport_test`, `//crates/alondra:version_test`, `//crates/daemon-server:daemon_server_test` | CLI tests run the real `alondra` binary. |
| `/usr/bin/defaults` | `//crates/alondra:cli_api_test`, `//crates/alondra:doctor_service_test`, `//crates/cli-tui:cli_tui_test`, `//crates/cli:cli_test`, `//crates/daemon-server:daemon_server_test`, `//crates/scenarios:from_report_bin_test` | The code under test reads macOS preferences with `defaults`. |
| `/usr/sbin/lsof` | `//crates/alondra:cli_api_test`, `//crates/alondra:doctor_service_test`, `//crates/daemon-server:daemon_server_test`, `//crates/engine-orchestrator:engine_orchestrator_test`, `//crates/engine-sdk:engine_sdk_host_test`, `//tools:reap_test_hosts_test` | Reapers and the doctor find the processes that hold a file. |
| `/bin/rm` | `//:plugin_mod_test`, `//crates/alondra:doctor_service_test`, `//crates/daemon-engine:daemon_engine_test`, `//deploy/apple:increment_version_test`, `//tools:plugin_mod_test_tmpdir_test` | The plugin test script removes its work folder. |
| `/usr/bin/true` | `//crates/agent-runtime:readiness_test`, `//crates/core:alondra_core_test`, `//crates/daemon-support:daemon_support_test`, `//crates/engine-live:engine_live_test`, `//crates/engine-orchestrator:engine_orchestrator_test` | Tests that need a process that starts and exits at once. |
| `own-sandbox` | `//agent-host:sea_test`, `//crates/agent-runtime:owned_host_test`, `//crates/scenarios:scenarios_test`, `//deploy/brew:upgrade_rehearsal_test`, `//tools:bazel_test_tmpdir_test` | The test starts sandbox-exec itself, which cannot run inside the guard's sandbox, so the target runs unguarded. |
| `//agent-host:alondra-host` | `//crates/daemon-engine:daemon_engine_test`, `//crates/daemon-server:daemon_server_test`, `//crates/engine-orchestrator:engine_orchestrator_test`, `//crates/engine-sdk:engine_sdk_host_test` | Engine and server tests run the host a release ships. |
| `/usr/bin/plutil` | `//crates/alondra:cli_api_test`, `//crates/alondra:doctor_service_test`, `//crates/cli:cli_test`, `//crates/system:system_test` | The CLI writes and checks the launchd plist with `plutil`. |
| `/usr/bin/touch` | `//crates/alondra:doctor_service_test`, `//crates/daemon-server:daemon_server_test`, `//crates/daemon-support:daemon_support_test`, `//crates/engine-github:engine_github_test` | Fake `gh` scripts leave a mark that they ran. |
| `/bin/cp` | `//:plugin_mod_test`, `//deploy/apple:increment_version_test`, `//tools:plugin_mod_test_tmpdir_test` | The plugin test script copies the plugin it tests. |
| `/bin/kill` | `//crates/alondrad:launcher_test`, `//crates/daemon-server:daemon_server_test`, `//crates/engine-sdk:engine_sdk_host_test` | Tests and their fakes signal the processes they started. |
| `/bin/mkdir` | `//agent-host:umask_test`, `//crates/alondra:doctor_service_test`, `//deploy/apple:increment_version_test` | The test script lays out a throwaway repository. |
| `/bin/mv` | `//crates/alondra:doctor_service_test`, `//crates/system:system_test`, `//tools/worktree_setup_test:setup_test` | The worktree setup script moves its generated rc file into place. |
| `/usr/bin/grep` | `//crates/alondra:doctor_service_test`, `//tools/worktree_setup_test:setup_test`, `//tools:bazel_artifact_paths_test` | The worktree setup script checks the lines it already wrote. |
| `/usr/bin/mkfifo` | `//crates/daemon-server:daemon_server_test`, `//crates/engine-orchestrator:engine_orchestrator_test`, `//crates/engine-sdk:engine_sdk_host_test` | Tests make named pipes to hold a fake host until they release it. |
| `/usr/bin/mktemp` | `//deploy/apple:increment_version_test`, `//tools/worktree_setup_test:setup_test`, `//tools:plugin_mod_test_tmpdir_test` | The plugin test script makes its own work folder. |
| `/usr/bin/wc` | `//crates/alondra:doctor_service_test`, `//crates/daemon-server:daemon_server_test`, `//crates/engine-github:engine_github_test` | Fake scripts count the calls they received. |
| `//agent-host:alondra-codex-host` | `//crates/engine-orchestrator:engine_orchestrator_test`, `//crates/engine-sdk:engine_sdk_host_test` | Orchestrator tests run the Codex host a release ships. |
| `//crates/alondrad:alondrad` | `//crates/alondrad:info_commands_test`, `//crates/daemon-server:daemon_server_test` | Tests run the real daemon binary. |
| `/bin/ls` | `//crates/daemon-engine:daemon_engine_test`, `//crates/daemon-server:daemon_server_test` | Fake scripts list the folders they were given. |
| `/usr/bin/head` | `//crates/daemon-server:daemon_server_test`, `//crates/engine-github:engine_github_test` | Fake `gh` scripts print part of a recorded answer. |
| `/usr/bin/make` | `//deploy/apple:increment_version_test`, `//tools:bazel_artifact_paths_test` | The test runs Makefile targets to check the paths they print. |
| `/usr/bin/pgrep` | `//crates/engine-orchestrator:engine_orchestrator_test`, `//crates/engine-sdk:engine_sdk_host_test` | Python reapers beside each host find the processes they end. |
| `/usr/bin/sw_vers` | `//crates/alondra:cli_api_test`, `//crates/alondra:doctor_service_test` | The CLI's doctor reads the macOS version. |
| `//agent-host:alondra-codex-plan-legacy-fixture-host` | `//crates/engine-sdk:engine_sdk_host_test` | A Codex host built from an older plan format. |
| `//agent-host:codex-legacy-fixture` | `//crates/engine-sdk:engine_sdk_host_test` | A Codex host of an older release. |
| `//agent-host:descendant-fake-claude.py` | `//agent-host:sdk_descendant_test` | A fake Claude Code that leaves a descendant behind. |
| `//agent-host:fake-codex.py` | `//agent-host:codex_session_test` | A fake Codex the Codex host under test talks to. |
| `//agent-host:pinned_node` | `//crates/provider-claude:sdk_worker_test` | The SDK worker tests run the host on the pinned Node. |
| `//crates/alondra:fake_daemon` | `//crates/alondra:doctor_service_test` | A daemon double the doctor talks to. |
| `//crates/alondrad:alondrad-launcher` | `//crates/alondrad:launcher_test` | The launcher's tests run the launcher. |
| `//crates/github-gh:diff_fixture` | `//crates/github-gh:github_gh_test` | A double of `gh pr diff` the tests run in its place. |
| `//crates/scenarios:host_relay` | `//crates/scenarios:scenarios_harness_test` | Scenarios run the relay between the daemon and a recorded host. |
| `//tools:buildbuddy-credential-helper.sh` | `//tools:buildbuddy_credential_helper_test` | The test runs the credential helper it tests. |
| `/bin/chmod` | `//deploy/apple:increment_version_test` | The test script makes a refusing git hook executable. |
| `/bin/df` | `//crates/system:system_test` | The disk checks read free space with `df`. |
| `/bin/ps` | `//tools:reap_test_hosts_test` | The test host reaper lists processes. |
| `/usr/bin/codesign` | `//tools:reap_test_hosts_test` | The reaper tells test hosts by their code signature. |
| `/usr/bin/lsbom` | `//deploy/apple:prepare_test` | The Apple release script reads the bill of materials of its package. |
| `/usr/bin/mkbom` | `//deploy/apple:prepare_test` | The Apple release script rewrites the bill of materials of its package. |
| `/usr/bin/pkgbuild` | `//deploy/apple:prepare_test` | The Apple release script builds the macOS installer package. |
| `/usr/bin/productbuild` | `//deploy/apple:prepare_test` | The Apple release script builds the macOS installer product. |
| `/usr/bin/sed` | `//deploy/apple:increment_version_test` | The test script reads the version and build number. |
| `/usr/bin/sips` | `//crates/daemon-support:daemon_support_test` | The daemon makes app icons with `sips`. |
| `/usr/bin/sourcekit-lsp` | `//:sourcekit_lsp_workspace_test` | The test starts the real SourceKit-LSP. |
| `/usr/bin/tail` | `//tools/worktree_setup_test:setup_test` | The worktree setup script checks how a file ends. |
| `/usr/bin/xattr` | `//deploy/apple:prepare_test` | The Apple release script clears extended attributes. |
| `/usr/bin/xcode-select` | `//:sourcekit_lsp_workspace_test` | SourceKit-LSP asks where Xcode is. |
| `/usr/bin/xcrun` | `//:sourcekit_lsp_workspace_test` | The test starts the real SourceKit-LSP through `xcrun`. |
| `/usr/sbin/diskutil` | `//crates/system:system_test` | The disk checks ask `diskutil` about volumes. |
| `/usr/sbin/pkgutil` | `//deploy/apple:prepare_test` | The Apple release script checks the package it built. |
| `e2e` | `//crates/alondrad:codex_adoption_process_test` | End to end: runs without the guard, only when the release names it in `RELEASE_ONLY_TESTS` or on request. |
| `path:claude` | `//:plugin_mod_test` | The node mod's tests run with the Claude Code on PATH (`claude plugin test`). |
| (none: not guarded) | `//apple/...` | Swift tests drive simulators, `xcodebuild` and the Xcode tools; they run without the guard. |
| (none: not guarded) | `//tests/...` | The opt-in suites run real daemons, relays and the native Claude on request; they run without the guard. |
| (none: not guarded) | every `rust_doc_test` | rustdoc compiles each example with the C toolchain and runs it from its own temporary folder; doctests run without the guard. |
<!-- test-execs:end -->

The guard runs only under the repository's wrapper. `.bazelrc` imports
`tools/test-wrapper.bazelrc`, which sets `test --run_under=//tools:test_tmpdir`; a `--run_under` in
your own `~/.bazelrc` is read after it and wins, and the tests then run unguarded. If yours sets
one, add this as its last line; it changes nothing in other repositories:

```
try-import %workspace%/tools/test-wrapper.bazelrc
```

`//tools:test_wrapper_test` fails, with that line, when the tests of a run did not run under the
repository's wrapper; `bazelisk test //tools:test_wrapper_test` tells whether yours is in effect.
The release passes `--run_under=//tools:test_tmpdir` itself and runs that test too.

What the guard does not cover:

- a failure names the test, never the refused program: only the unified log does, as above;
- it is the macOS kernel sandbox, so on Linux (part of CI) tests run unguarded;
- the kernel never lets a sandboxed process start a set-user-ID program (`/bin/ps`), so a declared
  one runs outside the sandbox, and what it starts is not guarded;
- a program a declared one starts is guarded too, so it is declared on the test's target as well;
- a test already inside another sandbox (Bazel's `darwin-sandbox` strategy) runs unguarded: macOS
  sandboxes do not nest. Tests here run with the local strategy;
- the targets of the table's last rows: Swift, the opt-in suites, doctests, `own-sandbox` and `e2e`.

## Limits

Every test target declares `size` or `timeout`, and `//tools:test_limits_test` fails on one that
does not. A test that passes its limit fails as `TIMEOUT`. `.bazelrc` sets the limits:

| `timeout` | Seconds | For |
|---|---:|---|
| `short` (`size = "small"`) | 60 | anything that takes up to 20 seconds under load |
| `moderate` | 300 | up to 100 seconds, and every test that needs a simulator |
| `long` | 600 | `manual` tests, or an entry in `tools/test_limits_allowlist.tsv` with its reason |
| `eternal` | 900 | no test uses it |

Take the limit from a run on a busy machine and leave at least three times the time measured, so
load never fails a good test. `python3 tools/test_durations.py .musts/bep.json` prints what each
target took in the last `bazelisk test`; a result read from a cache keeps its original duration, so a
run that reuses everything still prints what each target costs.

To see what is slow inside a Rust target:

```sh
bazelisk test //crates/core:alondra_core_test --test_env=RUSTC_BOOTSTRAP=1 \
  --test_arg=-Zunstable-options --test_arg=--report-time --test_output=all --nocache_test_results
```

The slowest targets, measured in a full run on a busy machine:

| Target | Seconds | Limit |
|---|---:|---:|
| `//crates/engine-sdk:engine_sdk_host_test` | 164.0 | 600 |
| `//apple/Testing/TriageTouch:TriageTouchUITests` | 200.0 | 600 |
| `//apple/Testing/Snapshots:AlondraTriageSnapshotTests` | 140.3 | 300 |
| `//apple/Testing/Snapshots:AlondraDetailSnapshotTests` | 110.1 | 300 |
| `//apple/Testing/Snapshots:AlondraScreenSnapshotTests` | 92.1 | 300 |
| `//agent-host:codex_session_test` | 63.3 | 300 |
| `//apple/Mermaid:AlondraMermaidSnapshotTests` | 47.8 | 300 |
| `//crates/core:alondra_core_test` | 44.5 | 300 |
| `//apple/Testing/MacSnapshots:AlondraMacSnapshotTests` | 31.3 | 300 |
| `//agent-host:sea_test` | 26.6 | 300 |
| `//crates/daemon-server:daemon_server_test` (was `//crates/alondrad:alondrad_test`) | 25.4 | 300 |
| `//agent-host:frame_test` | 23.8 | 300 |
| `//agent-host:model_probe_test` | 20.3 | 300 |
| `//crates/alondra:alondra_test` | 20.0 | 60 |
| `//apple/Data/Feed:ActionJournalProcessTests` | 19.7 | 60 |
| `//crates/alondrad:relaunch_test` | 17.1 | 300 |
| `//crates/daemon-support:daemon_support_test` | 16.7 | 300 |
| `//deploy/brew:upgrade_rehearsal_test` | 16.1 | 300 |

Every other target takes under 15 seconds. A test that needs a simulator pays 15 to 140 seconds before
its first case, so it only runs when `apple/` or a fixture it reads changes: `apple-tests` reads no
crate, and the two Apple tests that start the daemon are `apple-daemon-process`.

`AlondraSnapshotTests` is a suite of three targets that run side by side. To record its baselines,
`make snapshots-record` runs all three.

## Conversation scenarios

`crates/scenarios` runs the real daemon, with every loop it runs, over fake edges: GitHub (one
fake world), the clock (virtual: time moves only when a step says so), the push transport, the
Mac's load and the provider's process. A case is a folder, `crates/scenarios/cases/<name>/`, with
a `scenario.toml` (a world, steps, checkpoints) and the accepted checkpoints. At a checkpoint the
runner reads the views of the daemon by client and language (the same `GET /views/...` the apps
read) and the pushes sent since the last one, and compares them with the accepted files. The
user acts as the app does: a request is written on the new request screen, a message in the
task's composer, and an action is pressed on the card the view serves, all through
`POST /actions`; an action the daemon turns down fails the step. A node calls its tools as
`alondra mcp-node` does, at the task's routes with the capability the daemon issued to its
session, so a delivery goes through the same checks (a draft pull request is refused).

A Claude node starts as a launch in production starts it, and its SDK host is the scenario's: a
process the daemon supervises (`host_relay`, which listens on the host's socket and holds the lock
of its folder) relays the daemon's frames to a host in the runner that speaks the real protocol and
says what a Claude session says. The same edge answers Claude's command line and the host's probes
(`claude auth status`, the usage and model probes), so the daemon checks its accounts as it does on
a Mac. The world decides what the provider answers:

- `[world] accounts = ["default", "work"]` sets up those Claude accounts, each signed in, and
  `node` takes `account`. `login_expired` / `login_restored` act on one account: Claude says it is
  signed out and every turn on it ends on `authentication_failed` (401), while the others go on.
  `provider_down` makes the API fail every turn of every account with an error of
  `sdk-errors.json` (`api_error` ends one turn with one); a `rate_limit` also reads as a reached
  limit in the usage probe, until the hour after it began.
- `restart` lets the daemon go as a crash does: the hosts run on and the new daemon takes them
  over; the tasks of `lose` lose theirs, and no new host starts for them until `host_can_start`.
  `update` installs this release's own host files over a daemon of `[world] release`; a host of a
  release this one does not adopt is ended and replaced by one that resumes its session.
- `check = "hosts"` counts the hosts started for a task, those running, and the Claude sessions
  they held.
- `write_home` writes files of the daemon's home (`[[step.file]]`, a path inside it and its text),
  each whole at once, as another process of the Mac does, such as a crash report's
  `sending.json`. The daemon runs its own CLI (`alondra report send` and `report keep`) through
  the same edge, which does to `sending.json` what the real CLI does; any other command of it is
  a leak.

- `bazel test //crates/scenarios:scenarios_test` runs every case; `--test_arg=<name>` runs the
  cases whose name contains it. `//crates/scenarios:scenarios_harness_test` tests the harness
  itself: it names each failure with its own error, runs a scenario twice to the same bytes, and
  refuses anything that reaches a real process.
- `scenarios_test` runs in 16 shards, about one case each, so an edit waits for the longest case.
  Each case's line carries its real time (`scenario <name> ... ok (7.42 s)`), followed by where it
  went: assembly, steps by kind, the runner's waits for calm, checkpoints, teardown. Nearly all of it
  is those waits. The clock is virtual and knows the deadlines of the daemon's pending timed waits,
  so the runner waits for calm after a virtual second only when one of them fell inside it
  (`clock::deadline_within`, asked before the advance); a second that can wake nobody needs no wait.
- `bazel run //crates/scenarios:accept -- <case>...` writes what the checkpoints read into each
  case's folder; the pull request shows that diff for a reviewer to read. `bazel run` has no test
  wrapper, so `accept` makes its own short folder under `/tmp` (unless `ALONDRA_SHORT_TMPDIR`
  already names one), exports it for the runner, and removes it when it ends. `ablate` runs
  `bazelisk test`, which goes through the wrapper.
- `bazel run //crates/scenarios:ablate -- <case> --revert <file>...` runs only that case without
  the fix (those files as they were on the base of the branch) and with it, and prints the block
  the pull request shows.
- The scenario's phone is a service device of the daemon: it registers its push destination through
  the same encrypted request the app sends, so every push the daemon sends it is recorded. A
  checkpoint with `pushes = true` writes them to `pushes.json` (every alert; of the badge and
  retraction pushes, the last of each) and can assert `check = "no_pushes"` (optionally for one
  `delivery`: `alert`, `background` or `badge`) or `check = "badge"` with the `count` the icon ends at.
- Views show local time: the runner needs `TZ=UTC`, which the Bazel targets and `accept` set.
- `crates/scenarios-transcript` reads the transcript of a provider's process, one reader for
  every user of one.
- `crates/provider-conformance` holds the scenarios every provider adapter must pass: a native
  transcript per provider and one expected sequence of neutral facts. Each provider's test target
  runs them through its real normalizer; `bazel test //crates/provider-codex:provider_codex_test
  --test_arg=conformance_tests::` (or provider-claude) runs only the suite and prints its table.
  A difference known today is listed in its `catalogue.rs` with its cause; one that stops happening
  fails the suite until its entry is removed.

## End to end

`make e2e-sandbox` starts a throwaway daemon in a temporary home with fake `claude` and `gh`, and
`bazelisk test //tests/sandbox:journeys --nocache_test_results --zip_undeclared_test_outputs=false --test_output=streamed` runs the scripted journeys against it. `make tui-sandbox SESSION=name` opens
the TUI in tmux on a seeded sandbox, to try a change you can see; `make tui-sandbox-stop
SESSION=name` removes it. Neither ever touches your real daemon.
