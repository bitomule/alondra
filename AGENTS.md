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

## Tests

- Every behavior change ships with a test that fails without the change and passes with it. Show it
  in the pull request: remove the fix, see the test fail, put it back, see it pass.
- No flaky tests: no wall-clock assertions, no sleeps racing other work, no retries to hide failures.
- Tests never touch the real `~/.alondra`, launchd job, Keychain, Homebrew install or provider
  logins. Use temporary `HOME` fixtures, removed on success and failure.
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
