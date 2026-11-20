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
| `//crates/engine-sdk:engine_sdk_test` | 226.7 | 600 |
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
  case's folder; the pull request shows that diff for a reviewer to read.
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
