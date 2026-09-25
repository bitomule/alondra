# Device identity integration

This branch owns `AlondraDeviceIdentity` and the two existing pairing presenters. The
central `apple/BUILD.bazel` integrator must add `//apple/DeviceIdentity:AlondraDeviceIdentity`
to `AlondraiOSEntry` and `AlondraMacEntry`, then exclude
`Shared/Sources/DeviceKeyStore.swift` and `Shared/Sources/RequestSigning.swift` from
`AlondraShared`. Those old files define the same types and must not compile alongside
the new module. Existing callers that explicitly need these types should import
`AlondraDeviceIdentity`; `RequestSigner` and `RequestSignature` remain in
`AlondraAPIClient`.

On iOS, construct clients with `pairing.signer(for: serverURL)` at every call site:
`AlondraApp.swift`, `SettingsView.swift`, and `PushNotifications.swift`. Passing the
unscoped `pairing.signer` when a candidate server URL differs from the paired URL can
send the identity to the wrong server. The existing `signer` property remains only as a
temporary compatibility path for root migration; remove it once those call sites are
updated. A server switch must also restart sync and discard any queued command tied
to the previous identity (owned by D1a/D1b).

The signing algorithm and persisted key identifiers are unchanged. `DeviceKeyStore`
uses `Bundle.main.bundleIdentifier + ".signing-key"` with `secure-enclave` and `software`
accounts, and the same macOS Application Support file paths. iOS keeps
`pairedDeviceID`, `pairedURL`, and `serverURL`; Mac keeps `localDeviceIDs` and
`serverURL`. No migration or key deletion is needed when upgrading.
An unreadable key representation is treated as an invalid pairing so the user can
pair again. Keychain access failures still throw; they are not mistaken for a
missing key. On Mac, replacing an invalid key clears stale server IDs.

Validation: `bazelisk test //apple/DeviceIdentity:AlondraDeviceIdentityTests` exercises
headless reload, per-server IDs with key reuse, a real P-256 signature verified
against the API wire message, canonical URL matching, corrupt-key recovery versus
Keychain errors, and rollback of an unsuccessful iOS pairing. Full iOS
and Mac builds need the central Bazel integration above.
