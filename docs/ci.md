# Pull request checks

GitHub Actions runs one job, `CI`, on every pull request to `main`. It certifies that the
validation recorded with musts covers the pull request's tree. It does not build or test
anything. Whether `CI` is required to merge is a branch-protection setting the maintainer
decides; this workflow does not configure it, and it does not change Alondra's merge behavior.

## Decision

CI used to select the affected checks and run them again with Bazel on fresh GitHub runners.
That repeated, slowly and on macOS runners, what `musts validate` had already proved locally
with the same checks. CI now installs musts and runs `musts validate` on the pull request's
tree: green when every check in `MUSTS.yml` has evidence recorded for exactly those files, red
naming the checks that are missing or stale.

The evidence is the ledger, one file per check under `.musts/ledger/`, committed with the
change. Editing a file a check reads invalidates that check's entry, so a green `CI` means the
author recorded passing runs for the tree that is being merged.

`.mustsignore` keeps `Cargo.lock`, `MODULE.bazel.lock` and `version.bzl` out of every check's hash, so
a merge only rewrites the ledger files of the checks whose code it touched and two pull requests
conflict in the ledger only when they change the same code. A dependency change still reopens its
checks through `Cargo.toml` and `MODULE.bazel`, which stay hashed; a change that touches only a lock
file reopens nothing. A release's version bump still reopens every check, because the version is also
in `Cargo.toml`.

### Limit

The ledger cannot be verified from outside the author's machine: anyone who can push a branch
can write an entry without having run the check. `CI` therefore proves the evidence is
present and current, not that it is true. That is acceptable while one developer owns the
repository and runs the checks locally. When other people merge changes, put a real
re-execution back in CI (the previous selector and Bazel jobs are in git history before this
change) or restrict who can merge.

Checks that cannot run on a hosted runner, such as `apple-tests` (it leases a pinned iOS
simulator), no longer need special handling: they run locally and CI only reads their evidence.

## What runs

The job runs on `ubuntu-24.04`, checks out the pull request's merge commit and:

1. Downloads the published `musts` Linux binary with `gh release download` from the project's
   GitHub release, verifies its SHA-256 and puts it on `PATH`. The version and hash are pinned in
   `ci.yml`; update both together when moving to a newer release. Bazel downloads the same
   release for local runs and pins it in `bazel/env/tools.bzl`: `//tools:ci_test` fails when the
   two disagree on version or Linux hash.
2. Runs `musts validate`. No Bazel, Rust, Xcode or git history is needed; a shallow checkout is
   enough.

Installing the binary takes seconds. Building musts with `cargo install` would compile its
whole dependency tree on every run, and a Homebrew install adds a tap and a bottle that is not
published for the Linux runner. The release page also publishes the installer script and the
checksums used here: <https://github.com/bitomule/musts/releases>.

`workflow_dispatch` runs the same job on a chosen branch. A new commit cancels the older run of
the same pull request.

## Credentials and releases

The workflow grants `GITHUB_TOKEN` only `contents: read`, which is enough to download from a
public release. Checkout does not persist its credential. No repository secrets are referenced,
and there is no `pull_request_target`, deployment environment, self-hosted runner or publication
step. The action is pinned to a full commit SHA. The workflow is written as JSON, a valid YAML
mapping, so its test (`//tools:ci_test`) can inspect its structure with Python's standard parser.

First-time fork contributors still need a maintainer to inspect the changes and approve the
workflow; see [approving runs from forks](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/approve-runs-from-forks).
A fork's evidence is its own claim, so review its diff before trusting a green `CI`.

Releases remain local. `deploy/brew/release.sh`, the local release entry points and the underlying
publisher refuse execution when `GITHUB_ACTIONS=true`.

## Cost

Standard GitHub-hosted runners are free for public repositories. The job needs no larger
runner, cache or artifact upload. See [GitHub Actions billing](https://docs.github.com/en/billing/concepts/product-billing/github-actions).
