# Visual Debug scenarios

This opt-in suite launches `AlondraVisualiOS` or `AlondraVisualMac`, dedicated
`testonly` hosts that compile the production presentation sources and reducers.
They exclude the shipping `@main` entrypoints: no `CloudPairingOwner`, personal
Keychain, login, URLSession or APNs runtime is created. The headless service
harness in `tests/native/run.py` remains a separate gate.

## Shared production boundary

`CloudWorkspaceGate` is consumed by both shipping roots and the visual host. It
keeps the existing priority: an approved service shows its workspace; otherwise
saved/unknown configuration shows the restoring workspace; missing configuration
shows onboarding. The extracted restoring wrappers use exactly the same views,
modifiers and activity flags. Settings and Mac selection use the existing
`ServiceSettingsView` and `ServiceSettingsFeature`.

The fixtures install explicit in-memory services. A saved configuration response
and suspended restore exercise `CloudPairingFeature`. Real settings actions call
the synthetic selection adapter. That adapter replaces the scene store with the
selected identity, then the late-update command sends an Alpha update directly
to the real Beta `ServiceRootFeature`/`TriageFeature`; the fixture does not discard
it. This proves the presentation identity boundary, not `ApprovedSessionRegistry`
or real authentication/transport interoperability.

## Scenarios

| Scenario | Native state | Visual evidence |
| --- | --- | --- |
| `reconnect` | Saved configuration, suspended restore, then completed restore | Home with loading, no onboarding, then Alpha content |
| `account-settings` | Approved workspace and Settings sheet prepared at launch | Existing Account section inside the existing Settings sheet |
| `mac-selection` | Alpha and Beta choices, real button action selects Beta; stale Alpha update delivered afterwards | Beta content remains visible and Alpha content is absent |

The account and selection scenarios begin with Settings presented; they do not
claim to test discovering or navigating to Settings. The selection scenario
recreates the scene store just as the shipping scene changes service identity.
No visible test controls or alternate layouts are added to the product UI.

## Commands and isolation

Plan only, with no tools or filesystem mutations:

```sh
python3 tests/native/visual_run.py --platform ios
python3 tests/native/visual_run.py --platform mac
```

Reserve the shared heavy slot, commit a clean checkout and choose new output:

```sh
python3 tests/native/visual_run.py --platform ios --run \
  --runtime com.apple.CoreSimulator.SimRuntime.iOS-26-3 \
  --build-cache /Volumes/Externo/bazel/alondra-visual \
  --output /private/tmp/alondra-visual-ios-UNIQUE

python3 tests/native/visual_run.py --platform mac --run --build-only \
  --build-cache /Volumes/Externo/bazel/alondra-visual \
  --output /private/tmp/alondra-visual-mac-UNIQUE

# Use appPath/source from that build's manifest; retain the same heavy slot.
python3 tests/system/macos.py --run --profile visual \
  --app /absolute/path/from/manifest/AlondraVisualHarness.app \
  --app-source FULL_SOURCE_COMMIT \
  --output-dir /private/tmp/alondra-visual-tart-UNIQUE
```

The iOS runner creates, owns and deletes its exact simulator. It uses the native
runner's reserved-name recovery if simulator creation loses its result. MAV is
pinned to that simulator and attaches to the already-launched test app. Mac
runtime is reserved for the disposable Tart guest; `visual_run.py` refuses to
launch it on the host. A build-only result is `built`, never acceptance `passed`.
The `visual` Tart profile reuses the existing prepared-base checks, clone ownership,
disk guards, frozen evidence transfer and cleanup. Its guest disables networking,
starts the test binary directly, and owns each process group by its returned PID.
The host validates all three scenario reports again and compares the guest binary
hash with the input artifact. No personal app or auth state is staged.
Do not use a personal simulator, the installed Alondra app or TestFlight binaries.

If collection fails, the shared runner stops and retains only its owned clone
with `recovery.json`. Use the explicit `tests/system/macos_recover.py` procedure
in `tests/system/macos/README.md`; it retrieves the frozen evidence without
rerunning the app. Visual recovery repeats the same scenario/MAV validation and
requires the original executable hash. An original guest failure remains failed,
including across interrupted recovery attempts. A missing executable pin fails
before boot; do not manufacture a pin for an older receipt.

The runner commit may differ from the recorded `--app-source` when only runner
code changes. Reuse the preserved Debug artifact and its manifest in that case;
record both commits and the matching executable hash. Runner-only changes do not
claim that the artifact includes newer product UI changes.

Builds use Debug, one job, an owned locked cache and no user/system bazelrc or
remote cache/BES. Source commit and executable hashes are recorded. Output and
cache volumes both need 8 GiB free. The persistent cache is retained; there is no
implicit cache purge, scheduling or hosted CI.

## Evidence and controls

Each scenario has a unique UUID and atomic control/result JSON in its own app
container or guest directory. Controls are allowlisted fixture operations, not
arbitrary commands or URLs. The app has a 150-second lifetime budget for its
monitor; waits in the outer runner are bounded. Missing, old, failed or incomplete
reports fail. The expected assertions must all be literal `true`.

The outer driver separately checks actual MAV accessibility output for required
and forbidden content. It records that output, named captures, video and the
native state at each capture. Valid screenshots/video and absence of reported
crashes are mandatory. A native JSON result alone does not pass the visual gate.
Review the actual screenshots and video too: MAV's `video_status: accepted`
checks the media, but does not establish that a permissions dialog is absent.
The main manifest records cleanup failures as failures.

Fast tests do not start MAV, builds, a simulator or a VM:

```sh
python3 -m unittest discover -s tests/native -p 'test_*.py'
```

Before accepting changes to the gate, preserve source-tied positive evidence and
negative controls for saved-config routing and the real stale-identity filter.
These visual scenarios do not prove APNs, production accounts, backend E2E or
production performance. Build/runtime results must be recorded explicitly; the
presence of the harness is not passing evidence.

### macOS full accessibility observation

MAV 0.31 normalizes Cua's actionable `elements` array; that omits unindexed
static text and disabled controls. The Mac guest therefore reads the public Cua
`get_window_state` response for text assertions, while MAV still performs actions,
screenshots and video. This is a combined MAV/full-AX gate. It does not fabricate
accessibility nodes from fixture state or infer text from screenshots.

Every full-AX read targets the still-running child PID created by this guest and
an on-screen window returned for that PID. The returned PID/window and snapshot
identifier are checked. Missing, malformed or degraded trees fail. Original JSON,
response hash and snapshot identity are preserved in each scenario's `full-ax`
directory. Calls have a ten-second timeout, file/count/total bounds and owned
process and session cleanup. Screenshot capture is disabled for these tree-only
reads; MAV captures the visual evidence. Explicit degradation, node truncation and
reaching the depth cap fail. Cua does not prove complete coverage when AX child
reads fail: forbidden-text absence means absence from the observed bounded AX
walk, supplemented by inspection of the captured UI, not a universal visibility
proof. The requested walk limits are preserved in each receipt. iOS keeps its existing MAV observation path. Failure paths also
retain a direct tree and screenshot, without replacing the initial failure.
