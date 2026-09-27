# Disposable macOS gates

These are opt-in tests on this Mac. They do not use hosted CI or the Pi. The default
profile installs the public Brew package and exercises the local daemon and TUI.
MAV is needed only for the optional Apple app profile.

## Commands

From the repository root, inspect the plan without starting anything:

```sh
python3 tests/system/macos.py --output-dir /private/tmp/alondra-macos-UNIQUE
```

Reserve the shared heavy-work slot, then run the CLI profile:

```sh
python3 tests/system/macos.py --run --brew-version 0.1.8 \
  --output-dir /private/tmp/alondra-cli-UNIQUE
```

The Apple profile takes an already built `.app`, for example the bundle extracted
from the final distribution PKG. It never builds or launches it on the host:

```sh
python3 tests/system/macos.py --run --profile app \
  --app /absolute/path/Alondra.app --app-source FULL_SOURCE_COMMIT \
  --output-dir /private/tmp/alondra-app-UNIQUE
```

A distribution signature may prevent execution before TestFlight delivery. For
**demo diagnostics only**, `--resign-for-demo` signs the guest copy ad hoc. The
original package is preserved. This result does not verify APNs, App Groups,
receipt/provisioning behaviour or the installed TestFlight binary. The report
records this mode and the original/runtime executable hashes separately.

## Prepared base

The default local base is `alondra-clean-install-20260927`, prepared from:

`ghcr.io/cirruslabs/macos-tahoe-xcode@sha256:49c83cf0989d5c3039b8f1a5c543aa25b2cd920784fdaf30be22e18e4edeaa95`

This is a synthetic Cirrus image, not a copy of the user's Mac. It contains
Homebrew and `/Users/admin/alondra-test-tools/mav`, plus CuaDriver with guest
Accessibility and Screen Recording permissions. It must not contain Alondra
installation/state, account credentials or a signed-in Apple account. Keep it
stopped and use it only as a clone source. The preparation receipt is
`/Users/admin/alondra-test-tools/base.json` inside the image.

Every invocation creates a uniquely named `alondra-gate-*` clone, with two CPUs and
4 GiB RAM. It disables Tart auto-pruning, audio and clipboard sharing; no host
HOME, Keychain or directory is mounted. The Apple profile disables the guest Ethernet interface before launch; the CLI
profile does so after downloading and testing the public Homebrew package. This
uses the image’s synthetic guest administrator, never host sudo. It runs no
account login or cloud connector. Never point it at the Pi to fill an acceptance gap.

The base needs its driver permissions once when prepared. macOS does not provide
an ordinary unattended API to grant those permissions. Clones inherit them; the
app profile checks them and fails explicitly if they are absent. The runner does
not automate Settings, edit TCC databases, fetch a new image automatically or fall
back to `mav-macos` or another personal VM. Rebuilding the base is a separate,
explicit provisioning operation. `--base` accepts only an `alondra-*` local name
with the matching synthetic-origin receipt.

MAV 0.31's host `vm: true` mode hardcodes the shared image name `mav-macos`.
This runner instead invokes MAV **inside its own Tart clone**, so its generated
guest configuration intentionally says `vm: false`. It cannot select the host's
app: the guest script checks `hw.model` and the prepared-base marker.

## Assertions and limits

CLI: public installation, package version, Developer ID signatures, packaged
assets, Brew formula test, repeatable setup **dry-run without profile writes**,
real local daemon health/overview, CLI configuration/status, SQLite audit and
restart, then the actual default Local TUI in a PTY with normal `q` exit.
No Claude double, personal account or copied Keychain is installed. `setup` has
no offline mode: even skipping launchd/plugin still authenticates and enrolls.
The shipping CLI pins its issuer/relay; this gate does **not** claim full setup,
launchd/plugin registration, OAuth, pairing, encrypted remote service or Claude E2E.
Those belong to separate explicit integration and user-acceptance gates.

App: a fresh launch, entering the offline sample workspace, its demo banner,
reset, exit to onboarding, four named captures, video and crash checks. Text
selectors are fixed to the English test image because the macOS MAV driver does
not expose stable SwiftUI accessibility IDs. Three corresponding lint warnings
are expected; missing controls still fail the run. This is a demo smoke test,
not coverage of every screen or authenticated transport.

## Results and failure handling

Use a new output directory each time. `result.json` records runner commit/dirty
state, app source supplied by the release manifest, executable/payload hashes,
base metadata, commands, result and cleanup. Guest evidence is under `evidence/`;
the Apple profile also retains MAV's report, video and named captures. Review the
actual visual evidence before publishing. A missing/invalid video, crash, skipped
required step or missing capture fails the app gate.

Timeouts, errors and SIGINT/SIGTERM trigger cleanup of this invocation's reserved
clone only. A cleanup failure marks the run failed and names the remaining VM;
it must be removed before claiming completion. No script can handle SIGKILL or
host power loss: in that case, inspect the recorded run's VM name and remove only
that owned clone. Base images, other VMs, public release bytes and previous
results are never deleted. Keep at least 8 GiB free on the Tart and evidence disks.

Fast harness tests (no VM, network, brew install or build):

```sh
python3 -m unittest discover -s tests/system -p test_macos.py -v
bash -n tests/system/macos/guest.sh tests/system/macos/cli.sh
mav flow lint tests/system/macos/demo.yaml
```
