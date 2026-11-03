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

## End to end

`make e2e-sandbox` starts a throwaway daemon in a temporary home with fake `claude` and `gh`, and
`make e2e-journeys` runs the scripted journeys against it. `make tui-sandbox SESSION=name` opens
the TUI in tmux on a seeded sandbox, to try a change you can see; `make tui-sandbox-stop
SESSION=name` removes it. Neither ever touches your real daemon.
