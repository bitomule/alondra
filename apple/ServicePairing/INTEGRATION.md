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

## Local collection migration

`KeychainApprovedPairingStore` keeps the existing service
`com.bitomule.alondra.service-pairing.v1` and account `approved-phone`. Reads accept
both the original single pairing and a version-1 collection envelope. A legacy read
is non-mutating; the next successful approval writes the collection in one in-place
Keychain update. Private keys, pinned keys and `approvedAtMs` of retained entries
stay unchanged, preserving their durable journal scope. Unknown versions, corrupt
collections and locked Keychain reads fail closed. A failed write never deletes the
previous item. Older binaries cannot decode the new envelope: this is migration of
old data, not support for downgrading the application after new data are written.

Binding identity is `(relayURL, ownerSub, macID, deviceID)`. Adding an approval
retains other bindings and selects that approval for the existing single-active
adapter. Saving the same identity with changed private keys, or a previously pinned
Mac with changed public keys, is rejected until explicit removal. Re-saving an
identical identity retains its original approval timestamp. `delete()` remains an
explicit removal of the whole collection, as used by logout; it must not silently
switch the user to a different retained account or Mac.

All synchronous read/modify/write operations share a process-local `Mutex`; separate
store instances cannot lose each other's writes. The notification extension does
not write this item. This does not provide cross-process serialization for future
writers. Tests inject item I/O and exercise the actual codec and transaction with
independent store instances; they do not use personal Keychain data.

The legacy model does not record an OIDC issuer. Filtering by subject and relay is
not a claim of multi-issuer isolation. `CloudPairingOwner` retains its configured
provider, verified-subject comparison and relay allowlist before activating keys.
Do not expose Mac selection until push bindings, NSE contexts, account scoping and
notification routing are stored and selected per binding. No UI or notification
storage changes ship in this foundation block.
