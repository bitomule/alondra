# Alondra agent instructions

This is the only agent instruction file of the repository: Claude Code reads `AGENTS.md` by itself
(version 2.1.277 or later) when there is no `CLAUDE.md`, so do not add one. A `CLAUDE.local.md`
would hide this file from whoever has it.

## Public repository

**Everything you commit is public and stays in git history**: code, comments, tests, fixtures,
commit messages, branch names, pull request titles, descriptions and comments, and their captures.
Never include:

- **Personal data of the maintainer or anyone else**: real emails, home paths (`/Users/<name>`),
  ids of a Mac, device or account, public IP addresses, names of their private apps or repositories
  (unless the repository is already public).
- **Secrets**: keys, tokens, passwords, certificates, `.env` files, in code, examples or history.
- **Security details not yet published**, and the infrastructure or configuration of a hosted
  service (hosts, servers, accounts, deploy steps).
- **Plans, business reports and tracker ids** of a private tracker.

Use synthetic values: `ana@example.com`, `/Users/ana`, `203.0.113.7`, `2001:db8::7`,
`00000000-0000-0000-0000-000000000001`. Private context goes on the cards Alondra sends to the user
or in the private repository that keeps it, never here.

- Commit with the GitHub `noreply` email. A captured screen shows only the invented demo world.
- Pull request titles and branch names say what changes, with no tracker ids, task ids or people:
  `Reselect disabled providers before first launch`, `fix-reselect-providers`. Link issues as `#N`.
- **Plans and internal reports go to Alondra's own files, not to the repository.** A plan, a
  research report or a review log is shared through the task that produced it, never committed.
  Scratch files go in `.scratch/` (git-ignored).
- `//tools:public_data_guard_test` fails when a tracked file carries a real email, home path, the
  maintainer's name, a public IP, a device id, a credential or a private name. Replace the value; add
  a line to `tools/public_data_allowlist.tsv` only with the reason it is harmless.
  `python3 tools/public_data_guard.py --against origin/main` checks just the lines a branch adds.
  Names that cannot be written even in the guard go one per line in a file outside the checkout that
  `ALONDRA_PRIVATE_LITERALS` points to.
- Found something private already published? Tell the user with a card. Never rewrite history, close
  or delete anything on your own.

## Build and test

- Build with Bazel through the Makefile: `make build` (`bazelisk build //crates/... //apple:AlondraMac`).
  `.bazelversion` pins Bazel; Rust comes from `rules_rust`.
- Tests run **only through Bazel**, never `cargo test`, `swift test` or `xcodebuild`:
  - Rust: `make test` (`bazelisk test //crates/...`).
  - Apple: `bazelisk test --build_tests_only -- //apple/... -//apple/NativeHarness/...`.
  - A new integration test needs a Bazel target, or it does not run.
- Web (`web/`): `npm ci --include=dev`, `npm run typecheck`, `npm run build`, `npm run test:e2e`.
- `musts validate` is the done-check before committing, when `musts` is installed. It asks only for
  the checks whose `paths` a change touches (`MUSTS.yml`); a change that only touches prose or images
  no test loads must leave it empty, otherwise a `paths` entry is wrong: fix `MUSTS.yml`.
  Iterate with `bazelisk test <target>`, then `musts run` once when the change is done, never right
  after a `bazelisk test` of the same tree. Commit `.musts/ledger.lock.yaml` and any
  `MODULE.bazel.lock` change with the code.
- **Iterating on a failure: run the test, not the binary.** Pass the test's name or module path to
  the test binary, `bazelisk test //crates/daemon-engine:daemon_engine_test --test_arg=domain_merges::`
  (several `--test_arg` filters are allowed), instead of the whole target. The tests that start real
  hosts and binaries are `//crates/daemon-engine:daemon_engine_integration_test`; every other test of
  the crate is `daemon_engine_test`. A run you mean to record with `musts record` never has a filter.
- After a `bazelisk test` that covers every target of the `bazel-test` check, `musts record
  bazel-test-root` records it instead of rerunning (`musts` 0.9.0 or later). It says what is missing
  when it answers "Not recorded". Never pass `--test_filter` or tag/size filters to a run you mean
  to record.
- A new test, fixture or `BUILD.bazel` change keeps each check's `paths` equal to what its targets
  read: `make musts-scopes` fails on a missing path, and
  `python3 tools/musts_scopes.py propose <check>` prints the paths Bazel asks for.
- Brew packaging scripts: `python3 -m unittest discover -s deploy/brew/tests`; `bash -n` for changed
  shell scripts; `ruby -c deploy/brew/alondra.rb.in` for formula changes.

## Repository map

- `crates/`: the Rust workspace (`alondrad` daemon, `alondra` CLI and TUI, `relay`, domain, store and
  provider crates). `agent-host/`: the Agent SDK host nodes run through.
- `apple/`: the iOS and Mac apps and their shared modules. `web/`: the web client.
- `plugin/`, `plugin-orchestrator/`: the Claude Code plugins for nodes and the orchestrator.
- `tools/`: scripts, the e2e sandbox and the checks run by `//tools:*`. `deploy/`: Brew packaging and
  release. `tests/`: system and native suites.
- Design notes in `docs/architecture/`; `docs/building-from-source.md`, `docs/running-tests.md`.

## Tests

- Every behavior change ships with a test that fails without the change and passes with it. Show it
  in the pull request: remove the fix, see the test fail, put it back, see it pass.
- Fixing a bug starts by reproducing it on a fresh `origin/main`; if it no longer happens, there is
  nothing to fix.
- No flaky tests: no wall-clock assertions, no sleeps racing other work, no retries to hide failures.
- Every test target declares `size` or `timeout`, at least three times what it takes under load
  (`docs/running-tests.md`). `//tools:test_limits_test` fails on one that does not.
- Tests never touch the real `~/.alondra`, launchd job, Keychain, Homebrew install or provider
  logins. Use temporary `HOME` fixtures, removed on success and failure.
- Rust disk fixtures hold `crate::test_support::Directory` until their files, databases and tasks are
  closed; never use `std::env::temp_dir()` in tests. Swift tests that write files use
  `@Suite(.temporaryFiles)` and `TestFiles.directory()`.
- Opt-in suites (VMs, native harness, real PostgreSQL/Keycloak, load) never run as part of ordinary
  tests, hooks or releases; their commands are in `tests/native/README.md` and `docs/running-tests.md`.
- Never load the shared Mac on purpose: no `yes` processes, spawners, `stress`, busy loops or
  filling the disk. A failure that only appears under load is reproduced inside the test with fake
  clocks, injected limits and deterministic fixtures.
- A test that starts a host owns it: its folder comes from `Directory::for_unix_socket` (Rust) or
  `socketRoot()` in `agent-host/owned-host.mjs`, inside the test's TMPDIR, and
  `make reap-test-hosts` ends what an older cut left.
- Kill only a process you started and still hold the PID of; never `pkill -f` a name.
- A change to what the user sees is tried in the real TUI inside tmux against a sandbox daemon
  (`make tui-sandbox SESSION=<name>`, `make tui-sandbox-stop SESSION=<name>`), and its captures go
  in the pull request.

## Release

- No GitHub Actions: releases run on the maintainer's Mac, and the release refuses to run while
  workflow files exist. Never add `.github/workflows`.
- Tags are never moved and published bytes are never rebuilt or replaced; release a new version.
- Never export signing keys or copy signing credentials anywhere.

## Pull requests

- One pull request per change that stands on its own; the title says what changed and why. Tests and
  localization ship in the same pull request, never as a follow-up.

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
