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
| `eternal` | 900 | `manual` tests only |

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
| `//crates/daemon-engine:daemon_engine_test` | 302.4 | 600 |
| `//apple/Testing/TriageTouch:TriageTouchUITests` | 200.0 | 600 |
| `//apple/Testing/Snapshots:AlondraTriageSnapshotTests` | 140.3 | 300 |
| `//apple/Testing/Snapshots:AlondraDetailSnapshotTests` | 110.1 | 300 |
| `//apple/Testing/Snapshots:AlondraScreenSnapshotTests` | 92.1 | 300 |
| `//agent-host:codex_session_test` | 63.3 | 300 |
| `//apple/Mermaid:AlondraMermaidSnapshotTests` | 47.8 | 300 |
| `//crates/core:alondra_core_test` | 44.5 | 300 |
| `//apple/Testing/MacSnapshots:AlondraMacSnapshotTests` | 31.3 | 300 |
| `//agent-host:sea_test` | 26.6 | 300 |
| `//crates/alondrad:alondrad_test` | 25.4 | 300 |
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

## End to end

`make e2e-sandbox` starts a throwaway daemon in a temporary home with fake `claude` and `gh`, and
`make e2e-journeys` runs the scripted journeys against it. `make tui-sandbox SESSION=name` opens
the TUI in tmux on a seeded sandbox, to try a change you can see; `make tui-sandbox-stop
SESSION=name` removes it. Neither ever touches your real daemon.
