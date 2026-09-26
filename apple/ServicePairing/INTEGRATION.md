# Service pairing integration gates

This Bazel module is intentionally independent of SwiftUI, `AlondraShared`, the legacy
`DevicePairing`, and the app's OIDC implementation. It targets the service backend draft
PR #160 at `fc8c1a1`, where QR JSON includes the exact `expires_at_ms` Unix timestamp.

The iPhone integration owner needs to:

1. Add `//apple/ServicePairing:AlondraServicePairing` to the iOS entry target. Do not link it
   into the notification extension. That extension will need a narrowly scoped key reader
   later, after the shared Keychain access group and push envelope contract are ready.
2. Scan the Mac CLI QR and call `begin(qrPayload:authorization:displayName:)` only after OIDC
   login has supplied a **verified** subject and bearer. `AuthSession.verifiedSubject()` is the
   expected identity source; this module does not decode or trust an unsigned JWT payload.
3. Keep `PairingPending.comparisonCode` on screen while `waitForApproval` runs. The Mac CLI
   requires the human to type these exact eight base64url characters, then confirm `y/N`.
   Cancel the waiting task and call `discard` when leaving the pairing flow.
4. Start service traffic only after `waitForApproval` returns. It verifies the Mac's signature
   against the QR-pinned signing key and binds account, both Mac keys, both phone keys, route,
   device ID, session, and exact expiry before Keychain persistence. Do not treat the relay's
   registration alone as permission to access Mac content.
5. On logout or revocation, cancel service requests, discard pending attempts, and call
   `removeApprovedPairing`. Never silently replace pinned Mac keys from a relay response.

The module owns two fresh P-256 software keys per attempt (signing and ECDH). They are held
only in memory until approval; the approved record goes into one ThisDeviceOnly Keychain item
for atomic save. A killed app must rescan the QR. `URLSession` requests reject redirects and
do not retry candidate submission or approval polls automatically. The caller may retry the
whole user-visible pairing flow with a new QR.

Run `bazelisk test //apple/ServicePairing:AlondraServicePairingTests`. The tests open the
Swift candidate with the Mac P-256 agreement key, verify ChaCha20-Poly1305 associated data,
P-256 envelope and HTTP challenge signatures using the Rust crate's framing, and reject
forged or cross-account approval before persistence. Device verification with MAV belongs
to the iOS integration PR. After #160 merges, run the full app Bazel build and a real
Mac CLI to phone round trip against a disposable relay and test Keycloak account.
