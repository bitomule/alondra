# Native client scenarios (opt-in)

This harness launches a Debug test host and calls Alondra's real Swift services
directly. It follows Redraft's `ImageProcessingTestApp` and BlurCamera's headless
runners: select a scenario at launch, execute the production pipeline, write a
run-specific JSON report, validate that report outside the app. It uses no
XCUITest and needs no UI navigation.

The `testonly` host shares one runner between iOS and macOS. It cannot be a
shipping-app dependency; it never creates the normal app delegate, pairing owner,
APNs runtime, browser login or Keychain storage. The app's distribution bundles,
UI, signing profiles and production configuration are untouched. The runner
always builds with `-c dbg`, never from a TestFlight artifact.

## What it proves

| Scenario | Production components | Independent observations |
| --- | --- | --- |
| `large-resource-read` | `AuthSession`, `RelaySession`, request/response envelope codec, resource reads | Exact large Unicode document assembled from verified document revision chunks, only summary/document/content resource paths, encrypted requests opened by the synthetic Mac, one token exchange |
| `session-revoked` | Same read pipeline and actual `AuthSession.logout()` during the first content chunk | Read rejected, no later chunks, issuer revocation, credential removal, cached lease rejection, no extra token exchange |
| `lost-response` | Encrypted resource mutation, `RelaySession`, file-backed `RelayOutbox`, receipt verification | Synthetic Mac accepts one request, drops response, client reads receipt and recovers exact result; a replacement session reopens the journal without submitting again |

**Declared doubles:** HTTP peers, OIDC browser (always forbidden), memory credential
storage, synthetic keys/tokens and fixed service clock. The peers implement
`ServiceHTTPTransport` / `AuthHTTPTransport`; there is no socket, URLSession call,
Rust daemon, PostgreSQL, Keycloak, Pi or cloud request. The synthetic Mac opens
and signs envelopes with CryptoKit. This is native **client integration**, not a
real-service E2E, authentication interoperability, UI, APNs or production capacity
gate. The shipping backend integration suite remains under `tests/system`.

## Commands

Plan only (starts nothing and writes nothing):

```sh
python3 tests/native/run.py --platform ios
python3 tests/native/run.py --platform mac --scenario lost-response
```

Fast runner contracts (no build or simulator):

```sh
python3 -m unittest discover -s tests/native -p 'test_*.py'
```

After reserving the shared heavy-work slot, from a committed clean checkout:

```sh
python3 tests/native/run.py --platform ios --run \
  --runtime com.apple.CoreSimulator.SimRuntime.iOS-26-3 \
  --output /private/tmp/alondra-native-ios-UNIQUE

python3 tests/native/run.py --platform mac --run \
  --output /private/tmp/alondra-native-mac-UNIQUE
```

Use an actually installed runtime. The iOS command creates a new simulator with
an explicit runtime/device type, installs only the test host there, then deletes
that exact UUID. It never selects a personal booted simulator. `--device-type`
can select another installed iPhone type. `--scenario` runs one case, `all` is the
default; `--timeout` bounds each scenario (90 seconds by default).

The macOS command executes the dedicated headless test host, never the user's
Alondra app. It uses only its explicit private work directory and memory
credentials. It can also run inside a disposable Tart guest with the same
checkout/toolchain. This script does not create or manage a VM and does not claim
Tart platform acceptance.

Both commands require 8 GiB free on output and cache volumes, use one build job
and a dedicated locked Bazel cache, ignore home/system bazelrc and disable remote
cache/BES. The default persistent cache is
`~/Library/Caches/Alondra/native-harness`; use `--build-cache /Volumes/Externo/bazel/alondra-native-harness`
to place it on external storage. It must be new or carry this runner's ownership
marker; an arbitrary existing Apple cache is rejected. Concurrent users of the
same cache are refused. The server is stopped but the persistent cache is never
deleted. Only explicit `--fresh-build` creates/removes a disposable per-run cache.
The exact owned simulator is cleaned at the end; extracted test artifacts, logs
and reports are retained in the chosen new output directory. SIGINT/SIGTERM enter cleanup, repeat signals are deferred, and owned command
process groups are terminated on timeout/interruption. An interrupted cleanup is a
failed run. Before creation the manifest reserves a UUID-based simulator name;
if create is interrupted before returning its ID, cleanup reconciles only that
exact name. Missing or ambiguous evidence is reported as unresolved cleanup,
never as confirmation that no resource remains. Successful creation also stores
the exact simulator ID for recovery.
No global cache pruning, recurring automation or hosted GitHub runner is enabled.

## Evidence contract

`report.md` is the human-readable evidence summary, including every expected check
and links to exact command logs and original JSON. Failed attempts are preserved.

`manifest.json` records source commit, clean state, platform/Xcode, Debug
configuration, executable SHA-256, simulator runtime/UUID, report hashes and
cleanup outcome. Each scenario uses a new UUID. A passing app report must carry
schema 1, the expected run/scenario, final `passed` status, exactly the scenario's
named assertions, and literal true for each. Missing, stale, partial, empty,
unknown or false assertions fail. JSON is written atomically. Failed reports are
preserved before validation and simulator cleanup. A successful `simctl launch`
or a process exit alone never counts as a passing scenario.

The host's journal directory is deleted after each scenario; reports contain only
synthetic outcomes. Its environment is allowlisted and does not receive OP_* or
other host credential/launch overrides.

## Subsequent gates

MAV flows can consume the same scenario concept to place the real UI directly in
a desired state, then assert its visible behavior. That UI adapter is not part
of these three client-service scenarios. Reconnection/long-poll lifecycle,
multiple-Mac presentation, settings, notification routing and actual backend
interoperability require additional scenarios and explicit evidence. Do not use
this first harness as proof that those flows or the entire application passed.
