# bazel/env: developer tools pinned by Bazel

`musts` and `simpool` are pinned in `bazel/env/tools.bzl` (URL and sha256 of each release).
`//bazel/env:bazel_env` ([bazel_env.bzl](https://github.com/buildbuddy-io/bazel_env.bzl)) collects
them into `bazel/env/.bazel_env/bin`. Nothing has to be installed by hand:

- **Launchers.** bazelisk runs `tools/bazel` before every Bazel command (`.bazeliskrc`). It installs
  `bazel/env/launcher.sh` into `~/.local/bin` as `musts` and `simpool`, and never replaces a file it
  did not put there. The first Bazel command of a checkout is therefore enough: afterwards `musts
  validate` and `simpool` work in any shell. Keep `~/.local/bin` on `PATH`, ahead of any Homebrew copy.
  Apple silicon only; CI installs `musts` itself and the wrapper does nothing there.
- **Versions.** Each launcher runs the tool pinned by the repo you are in. `ensure.sh` rebuilds
  `.bazel_env` first when it is missing or out of date with `MODULE.bazel` or this package (about 5 s
  the first time in a worktree, about 25 ms per call otherwise); `tools/bazel` runs it before each Bazel
  command too, so the launchers always find the pinned tools.
  Outside this repo, a launcher runs the next copy of the tool on PATH.
- **Bumping a tool.** Change its `url` and `sha256` in `tools.bzl` in one PR. For simpool, bump the
  binary and the test runner archive together: they come from the same release. For `musts`, the
  pull request workflow installs the Linux build on its own: `//tools:ci_test` fails until
  `.github/workflows/ci.yml` pins the same version and the Linux hash held in `tools.bzl`.
- **Snapshot tests.** The iOS snapshot targets pass the pinned simpool's path to the test runner as
  `SIMPOOL_BIN` (`SIMPOOL_TEST_ENV` from `@simpool_env//:env.bzl`), because a test action has no PATH.
  They need no installed `simpool`.

Nothing is downloaded until something builds it: the Rust and web builds never fetch any of it.
