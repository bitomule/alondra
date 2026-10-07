# Apple apps: TestFlight betas

Betas of the iOS and Mac apps go out with the `/beta` skill
(`~/.claude/skills/beta`, script `scripts/beta.sh`), run from a clean, up-to-date
`main`. The skill reads its settings from the `Makefile`:

| Variable | Value | Meaning |
| --- | --- | --- |
| `APP_ID` | `6816262826` | the App Store Connect app, iOS and macOS platforms |
| `IPA_PATH` | `.beta-build/ios/Alondra.ipa` | written by `make build-release` |
| `PKG_PATH` | `.beta-build/macos/Alondra.pkg` | written by `make build-release` |
| `BETA_GROUP` | `Internal Pilot,Private Pilot` | groups of that app; they get iOS and Mac builds |

`BETA_GROUP` lists the internal group on purpose: «Internal Pilot» does not
receive builds automatically, and `asc publish testflight` / `asc builds add-groups`
take several comma-separated groups.

## One App Store Connect record

iOS and Mac share the bundle ID `com.bitomule.alondra` and the record `6816262826`
(universal purchase). macOS was added to it as a platform; the bundle ID is a
universal App ID, so the Mac App Store profile in `signing/` is issued for it too.
That profile carries push because the App ID has it; the Mac app does not use push.

Until October 2026 the Mac app was `com.bitomule.alondra.mac`, record `6816263014`.
A sandboxed Mac app's container and login-keychain items belong to its bundle ID,
so the Mac starts empty after the move: it signs in once more, approves its paired
Macs again and pairs with its local daemon on its own. Its keychain services are
`com.bitomule.alondra.mac.*`, distinct from the old app's, so the old items are
never read and macOS never asks for the keychain password. The old app can be
deleted from `/Applications`. Its TestFlight builds stay available until the Mac
build from this record is confirmed working, and are expired only then.

## Version

`tools/shared.bzl` holds the marketing version and the build number of both apps.
Bazel reads it (`apple/BUILD.bazel`, target `Version`) for every build, release
included.

- Raise `version` by hand, in a reviewed PR.
- Never edit `build_number`: `make increment-version` bumps it, commits
  `🔖 Release <version> (<build>)` and tags `v<version>-<build>`; `/beta` then
  raises it further if App Store Connect is ahead.

The app version is independent from the daemon's. Builds up to iOS 36 / Mac 12
were uploaded with the daemon version (up to 0.2.49), so the apps restart at 0.3.0.

`make version-guard` (`deploy/apple/version_guard.py`) refuses a version lower than
any build App Store Connect has on either platform, or an equal version without a
higher build number. TestFlight never offers a lower version to someone who has a
higher one installed. `make increment-version` and `make build-release` both run it.

## Release notes

`/beta` writes `.beta_notes` in the repo root before the run and deletes it after.
It is never committed. Each section name is the App Store Connect locale:

```text
[en-US]
- Test the new workspace and selected details.

[de-DE]
- Testen Sie den neuen Arbeitsbereich und die ausgewählten Details.

[es-MX]
- Prueba el nuevo espacio de trabajo y los detalles seleccionados.
```

## Release build

`make build-release` runs `deploy/apple/prepare.py` for iOS and for Mac. It never
calls App Store Connect or uploads anything. It builds with
`--define=alondra_distribution=1` (App Store profiles and entitlements in
`apple/BUILD.bazel`), one Bazel job, arm64 only, and verifies the result before
writing it to `.beta-build/` (ignored by git, replaced on every run):

- the checkout is clean and committed (only `.beta_notes` may be untracked) and does
  not change during the build;
- the profiles in `signing/` belong to team `658YBSK6MG`, are unexpired App Store
  profiles and contain the signing certificate;
- the signing identity is the only valid Apple Distribution identity of the team in
  the keychain (Mac: also the only valid Mac Installer identity), unless
  `--identity` / `--installer-identity` name one;
- the bundles have the expected IDs, the version and build of `tools/shared.bzl`,
  device Mach-O, the validated embedded profile, the requested signer, production
  APNs and the shared Keychain group (iOS) or the sandbox entitlements (Mac);
- the Bazel archive is copied to a private snapshot before validation, so a later
  Bazel run cannot change what ships;
- the Mac PKG signature is trusted by Apple and its leaf certificate is exactly the
  requested Installer identity.

Each output folder gets a `manifest.json` with source commit, version, build,
artifact SHA-256, identities and profile hashes. Failed commands leave a private
diagnostic log at the path the error names.

| Destination | Profile in `signing/` | Bundle identifier |
| --- | --- | --- |
| iOS app | `Alondra_AppStore.mobileprovision` | `com.bitomule.alondra` |
| iOS extension | `Alondra_NotificationService_AppStore.mobileprovision` | `com.bitomule.alondra.NotificationService` |
| Mac App Store | `Alondra_Mac_AppStore.provisionprofile` | `com.bitomule.alondra` |

`python3 deploy/apple/prepare.py ios --output .beta-build/ios --preflight-only`
checks identities and profiles without building.

## Tests

```sh
bazelisk test //deploy/apple:prepare_test //deploy/apple:version_guard_test
```

They exercise the packaging gates with real plist and ZIP fixtures and the version
guard with recorded App Store Connect responses. They do not prove signing,
App Store processing or runtime behaviour.
