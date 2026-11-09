# Checks for external contributions

GitHub Actions validates pull requests from contributors other than the repository owner.
The owner's changes, including changes opened by local agents under that account, continue to
use musts and Bazel locally. This workflow does not configure branch protection, require a
check, or change Alondra's merge behavior.

The pull request's author decides whether jobs run, not the person who pushes another commit,
approves a workflow, or reruns a job. Owner-authored pull requests are excluded even when their
head branch is in a fork. GitHub can still show a skipped workflow entry for these pull requests;
no runner is allocated. See [GitHub's job conditions](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-jobs-with-conditions).

First-time fork contributors still need a maintainer to inspect the changes and approve the
workflow through GitHub. The repository's contributor approval policy remains
`first_time_contributors`. No job grants that approval automatically. See
[approving runs from forks](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/approve-runs-from-forks).

## What runs

`tools/ci.py plan` compares the checked-out PR merge commit with its merge-base, including both
paths of a rename and the old path of a deletion. It selects checks from both the base and PR's
`MUSTS.yml`, so narrowing a scope in the PR cannot hide the checks that previously read a changed
file. Changes to the manifest, selector, shared glob matcher or workflow select every check.
Local musts evidence is never used to skip CI.

The manifest reader accepts the repository's block syntax for `bazel/test`, `paths` and
`with.targets`. An unsupported check type, YAML construct or unmapped check name fails selection
with an error. Extend the parser and runner map with tests when introducing such a change.

Each selected check gets a fresh runner. Identical target patterns assigned to the same operating
system run once. At most four jobs run concurrently; other jobs wait in GitHub's queue. A new
commit cancels an older execution of the same PR. A documentation-only PR with no affected check
finishes selection with an explanation instead of starting test runners.

Some target sets contain libraries and no tests. Bazel reports those with its documented exit
status 4; the job then confirms them with `bazel build`. A failed compilation or test remains a
failure. This also means tests added to a previously library-only package run automatically.

| Runner | Checks | Job timeout |
| --- | --- | --- |
| `ubuntu-24.04` | Portable Rust crates and source/script checks | 90 minutes |
| `xcode-27` (macOS 27, Apple Silicon) | Apple builds/tests and checks needing native macOS APIs | 120 minutes |

The exact check-to-runner assignment is in `tools/ci.py`. The macOS group includes the CLI's
Keychain dependency, the process-group crate's libproc calls, Mach-O daemon binaries, native
system tests and the packaged Node hosts. The source-only Apple catalog and entitlement checks
run on Linux. Both platforms use the Bazel release in `.bazelversion`; Rust and Node come from
the pinned Bazel toolchains. CI limits Bazel to two build jobs and one local test at a time.

The image for macOS 27 uses the label `xcode-27`, not `macos-27` or `macos-latest`. Its contents
are described in the [official image inventory](https://github.com/actions/runner-images/blob/main/images/macos/xcode-27-arm64-Readme.md)
and [image announcement](https://github.com/actions/runner-images/issues/14404).

## Simulator and image references

`apple/Testing/Simulator/config.bzl` is the shared contract for snapshot and touch tests and the
capture tools. `tools/simulator.py` checks the toolchain and runtime's supported device types,
then creates and boots an isolated disposable simulator. A runtime merely appearing in the
inventory is insufficient: iOS 27.1's device set can support only iPhone Duo, as documented in
[runner-images #15000](https://github.com/actions/runner-images/issues/15000).

References are recorded locally once for an OS migration and reviewed in their own PR. CI
always verifies references; it never updates them. A runner or simulator failure is reported
with its inventory and logs, without dropping targets or changing image tolerances.

After confirming the same runtime can boot on both machines, `make snapshots-record-all`
records the complete iOS and Mermaid reference set. Its manual migration target runs all screen
suites in one process, so the account warm-up suite is captured once. The recorder uses the
canonical Xcode version and a unique output generation; it refuses missing, unexpected or
duplicate references before copying anything. Verify the result with the ordinary three
snapshot targets and Mermaid before committing the images.

## Credentials, cache and releases

The workflow uses `pull_request` and grants `GITHUB_TOKEN` only `contents: read`. Checkout does
not persist its credential. No repository secrets are referenced or passed to commands, and
there is no `pull_request_target`, environment with deployment secrets, self-hosted runner or
publication step. Actions use full commit SHAs. The workflow is written as JSON, a valid YAML
mapping, so its security tests can inspect its structure with Python's standard JSON parser.

CI reads neither the system nor the user's Bazel rc file. Its `ci` configuration disables the
remote cache, uploads and BuildBuddy event streaming. There is no shared Actions build cache or
artifact upload. Personal provisioning profiles and signing identities are absent. Test hosts
may receive the ad hoc signatures needed to execute on Apple Silicon; they do not receive
distribution signatures.

Releases remain local. `deploy/brew/release.sh`, the local release entry points and the underlying
publisher refuse execution when `GITHUB_ACTIONS=true`, before preparing artifacts, signing,
creating a tag or calling the publication API. The presence of check workflows does not prevent
a local release.

## Reading failures and timings

The job log prints the selected base, paths and target sets. Each validation job reports the
Bazel command's elapsed seconds and exit code in its log and GitHub step summary. That duration
includes compilation and tests; GitHub's job timestamps also include checkout and preparation.
Measure queue delay separately from `created_at` to the first job's `started_at` when comparing
the contributor's total wait with local validation.

Keep the commit and target set identical when comparing machines. Record runner image, Xcode
and simulator builds, and distinguish a fresh GitHub VM from a warm local cache. Do not erase a
shared local cache to manufacture a cold comparison. Use the same Bazel target arguments locally
and read build/test durations from its build event file when a finer breakdown is needed.

Failures and timeouts stay visible. There is no automatic retry to turn a flaky failure green.
A maintainer can investigate and deliberately rerun the same commit; retain the failed attempt
in the evidence rather than reporting only the successful rerun. Jobs awaiting fork approval
remain GitHub's responsibility; this workflow introduces no Alondra waiting card or merge gate.

## Cost and limits

Checked on 2026-11-09: standard GitHub-hosted runners are free for public repositories and do
not consume the private-repository compute allowance. Larger runners are billed even for public
repositories. Artifact and cache storage have their own allowances; this workflow uploads
neither. See [GitHub Actions billing](https://docs.github.com/en/billing/concepts/product-billing/github-actions)
and [standard runner specifications](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

GitHub documents a six-hour execution limit per hosted job and plan-dependent concurrency
limits, including a separate macOS limit. This workflow's job limits are shorter. Preview image
availability and software can differ from stable images; an image failure is evidence to report,
not a reason to remove coverage. See [Actions limits](https://docs.github.com/en/actions/reference/limits).

## Comparable maintainer workflows

[RepoBar's agent instructions](https://github.com/steipete/RepoBar/blob/main/AGENTS.md) require
local checks, while its [CI workflow](https://github.com/steipete/RepoBar/blob/main/.github/workflows/ci.yml)
uses Xcode 27, read-only permissions, pinned actions, a timeout and cancellation of older runs.
[LLM's agent instructions](https://github.com/simonw/llm/blob/main/AGENTS.md) also describe local
tests; its [workflow](https://github.com/simonw/llm/blob/main/.github/workflows/test.yml) runs tests,
analysis and builds across operating systems with read-only repository permissions.

Those workflows run on both pushes and PRs. Excluding the owner is this repository's policy:
local automation already validates its changes, and CI is provided for outside contributions.
