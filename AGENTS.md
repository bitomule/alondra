# Alondra agent instructions

## Public repository

- **Everything you commit is public**: code, comments, tests, fixtures, pull request descriptions
  and their captures. Never include secrets, personal data, private repository names, device
  identifiers or addresses of a private network.
- **Plans and internal reports go to Alondra's own files, not to the repository.** A plan, a
  research report or a review log is shared through the task that produced it, never committed.

## Build and test

- Build with Bazel through the Makefile: `make build` (`bazelisk build //crates/... //apple:AlondraMac`).
  `.bazelversion` pins Bazel; Rust comes from `rules_rust`.
- Tests run **only through Bazel**, never `cargo test`, `swift test` or `xcodebuild`:
  - Rust: `make test` (`bazelisk test //crates/...`).
  - Apple: `bazelisk test --build_tests_only -- //apple/... -//apple/NativeHarness/...`.
  - A new integration test needs a Bazel target, or it does not run.
- Web (`web/`): `npm ci --include=dev`, `npm run typecheck`, `npm run build`, `npm run test:e2e`.
- `musts validate` is the done-check before committing, when `musts` is installed.
- `musts validate` asks only for the checks whose `paths` a change touches, and a check's `paths` are
  the files its targets read (`MUSTS.yml`). Iterate with `bazelisk test <target>`; when the change is
  done, run `musts run` once, not after a `bazelisk test` of the same tree, and do not rerun it when
  nothing changed (read the failing log first). A change that only touches prose or images no test
  loads must leave `musts validate` empty: if it does not, a `paths` entry is wrong, so fix
  `MUSTS.yml` instead of running the tests.
- **Tests already run? Record them.** After a `bazelisk test` that covers every target of the
  `bazel-test` check, record it with `musts record bazel-test-root` instead of `musts run`, which
  would run the same tests again. It reads `.musts/bep.json`, which every `bazelisk test` writes
  here, and records the check only if every target passed and nothing in its scope changed since the
  run. When it answers "Not recorded", the reason says what is missing:
  - edited after testing, or a test that did not pass: fix it and test again;
  - a target missing from the run, or a pattern not covered: run all the check's targets, or use
    `musts run bazel-test-root`;
  - no musts stamp: `musts` is not installed or too old (`musts record` needs 0.9.0 or later).

  Never pass `--test_filter` or test tag/size filters to a run you mean to record.
- A new test, fixture or `BUILD.bazel` change keeps each check's `paths` equal to what its targets
  read: `make musts-scopes` fails on a missing path, and
  `python3 tools/musts_scopes.py propose <check>` prints the paths Bazel asks for.

## Tests

- Every behavior change ships with a test that fails without the change and passes with it. Show it
  in the pull request: remove the fix, see the test fail, put it back, see it pass.
- No flaky tests: no wall-clock assertions, no sleeps racing other work, no retries to hide failures.
- Every test target declares `size` or `timeout`, at least three times what it takes under load
  (`docs/running-tests.md`). `//tools:test_limits_test` fails on one that does not.
- Tests never touch the real `~/.alondra`, launchd job, Keychain, Homebrew install or provider
  logins. Use temporary `HOME` fixtures, removed on success and failure.
- Never load the shared Mac on purpose: no `yes` processes, spawners, `stress`, busy loops or
  filling the disk. A failure that only appears under load is reproduced inside the test with fake
  clocks, injected limits and deterministic fixtures.
- A test that starts a host owns it: its folder comes from `Directory::for_unix_socket` or
  `socketRoot()`, inside the test's TMPDIR, and `make reap-test-hosts` ends what an older cut left.
- A change to what the user sees is tried in the real TUI inside tmux against a sandbox daemon
  (`make tui-sandbox SESSION=<name>`, `make tui-sandbox-stop SESSION=<name>`), and its captures go
  in the pull request.

## Release

- No GitHub Actions: releases run on the maintainer's Mac, and the release refuses to run while
  workflow files exist. Never add `.github/workflows`.

## Code

- UI and code strings in English; Spanish through the existing `tr!()` and `.xcstrings`.
- Alondra is general: no maintainer-specific defaults in built-ins.
- No comments that restate the code.
- No version in the name of a new type, module, file or function. Values on the wire or on disk
  keep their bytes, held in a constant whose name carries no version.
- Control decisions never read prose. A branch that changes state or behavior decides on a type:
  an exit status, `io::ErrorKind`, an HTTP status, a documented JSON field or an Alondra error type.
  It never decides on the text of an error, a process's output or what a model wrote.
- Stored formats must decode old data: serde defaults on new fields, additive enums.
- A new screen reads the committed read index (`docs/architecture/read-side.md`); it never walks
  tasks, nodes or cards inside a view.
