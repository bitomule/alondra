# Pull request checks

GitHub Actions validates every pull request to `main`, whoever opens it, as an additional check
that stops broken code entering `main`. It does not replace local validation with musts and
Bazel, and it does not change Alondra's merge behavior. Whether the check is required to merge is
a branch-protection setting the maintainer decides; this workflow does not configure it.

First-time fork contributors still need a maintainer to inspect the changes and approve the
workflow through GitHub. The repository's contributor approval policy remains
`first_time_contributors`. No job grants that approval automatically. See
[approving runs from forks](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/approve-runs-from-forks).

Maintainers with write access can explicitly launch `workflow_dispatch` to check the runner and
measure every MUSTS target set, even when the chosen branch matches `main`. Its optional `check`
input selects one complete MUSTS check for diagnosis; blank runs all checks. This input never
narrows a pull request's scope. Manual runs have separate concurrency groups, so a diagnostic run
does not cancel an ongoing measurement. They have the same
read-only token, no secrets and no publication steps. This manual probe does not validate a
pull request or the first-time fork approval path.
Use `gh workflow run ci.yml --repo bitomule/alondra --ref <branch>` once GitHub has registered the
workflow. See [manual workflow execution](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow)
and the [CLI reference](https://cli.github.com/manual/gh_workflow_run).
For example, append `-f check=plugin-mod` to diagnose the plugin tests. A successful focused run
only proves that named check, not the complete validation set.

## What runs

`tools/ci.py plan` compares the checked-out PR merge commit with its merge-base, including both
paths of a rename and the old path of a deletion. It selects checks from both the base and PR's
`MUSTS.yml`, so narrowing a scope in the PR cannot hide the checks that previously read a changed
file. Changes to the manifest, selector, shared glob matcher, workflow or CI tool lock select every check.
Local musts evidence is never used to skip CI.

The manifest reader accepts the repository's block syntax for `bazel/test`, `paths` and
`with.targets`. The explicit `mac-inventory-review` judgment check is listed, with its review
facts, in the selection summary when affected. CI neither performs nor approves that review;
a maintainer still checks the Mac inventory. Base scopes remain effective if a PR narrows them.
An unsupported check type, YAML construct or unmapped check name fails selection with an error. Extend the parser and runner map with tests when introducing such a change.

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
Keychain dependency, core's Bonjour DNS-SD calls, the process-group crate's libproc calls, Mach-O daemon binaries, native
system tests and the packaged Node hosts. The source-only Apple catalog and entitlement checks
run on Linux. Both platforms use the Bazel release in `.bazelversion`; Rust and Node come from
the pinned Bazel toolchains. CI limits Bazel to two build jobs and one local test at a time.

The plugin mod check also needs Claude Code's local plugin validator and test runner. Only that
job installs the exact version in `.github/ci-tools/package-lock.json`, including the integrity
of its platform binary. It runs with the check's disposable HOME, without a login or model key;
CI disables auto-updates and nonessential traffic. The version initially matches the local CLI,
2.1.291. See the [official installation and version-pinning instructions](https://code.claude.com/docs/en/setup).

The image for macOS 27 uses the label `xcode-27`, not `macos-27` or `macos-latest`. Its contents
are described in the [official image inventory](https://github.com/actions/runner-images/blob/main/images/macos/xcode-27-arm64-Readme.md)
and [image announcement](https://github.com/actions/runner-images/issues/14404).

## Checks that stay local

`apple-tests` leases an iOS simulator whose pinned device and runtime the hosted image does not
provide, so `tools/ci.py` never schedules it. Run it locally with musts and Bazel. Moving the
simulator and its image references to the hosted runtime is a separate change.

## Credentials, cache and releases

The workflow uses `pull_request` and explicit `workflow_dispatch`, and grants `GITHUB_TOKEN`
only `contents: read`. Checkout does
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
Failed tests print their errors into the job log, so diagnosis does not depend on downloading
files from a discarded runner. CI raises Bazel's per-test output limit to 16 MiB: a diagnostic
run produced a 1.2 MB daemon-engine failure log that Bazel omitted at its default 1 MiB limit.

Keep the commit and target set identical when comparing machines. Record runner image, Xcode
and simulator builds, and distinguish a fresh GitHub VM from a warm local cache. Do not erase a
shared local cache to manufacture a cold comparison. Use the same Bazel target arguments locally
and read build/test durations from its build event file when a finer breakdown is needed.

Failures and timeouts stay visible. There is no automatic retry to turn a flaky failure green.
A maintainer can investigate and deliberately rerun the same commit; retain the failed attempt
in the evidence rather than reporting only the successful rerun. Jobs awaiting fork approval
remain GitHub's responsibility; this workflow introduces no Alondra waiting card or merge gate.

## Runner measurements

These manual probes measure runner behavior; they do not validate a pull request.
The times below are the Bazel command durations printed by `tools/ci.py`, including compilation
and tests on fresh GitHub VMs. They exclude selection, runner queueing and checkout.

| Scope | Runner | Bazel duration | Result | Evidence |
| --- | --- | --- | --- | --- |
| CI selector and policy tests | `ubuntu-24.04` | 13.776 s | Passed | [Run 42713464331](https://github.com/bitomule/alondra/actions/runs/42713464331) |
| Offline plugin validation | `ubuntu-24.04` | 15.346 s | Passed | [Run 42712322608](https://github.com/bitomule/alondra/actions/runs/42712322608) |
| Local publication safety tests | `xcode-27` | 34.754 s | Three targets passed | [Run 42713472365](https://github.com/bitomule/alondra/actions/runs/42713472365) |

The first full probe, [run 42648436515](https://github.com/bitomule/alondra/actions/runs/42648436515),
finished with 42 successful target groups and eight failures; selection also passed. Its Apple Bazel command took
2,738.850 s (45 min 38.9 s); 20 of 25 tests passed. The canonical iPhone 17 / iOS 27.0 creation
and boot probe succeeded before that command. This failed attempt remains part of the record;
it is not a successful validation or a speed comparison with local execution.

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
