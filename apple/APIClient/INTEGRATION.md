# APIClient migration and consolidation

> Historical adapter notes below describe an intermediate migration seam, not the
> final architecture. The active app roots use the service facade. Removal remains
> gated on strict UI parity and replacement of every reachable legacy consumer.

## Historical adapter handoff

`//apple/APIClient:AlondraAPIClient` and its wire tests build independently. The old `AlondraShared.AlondraClient` is still the public app adapter. C owns `apple/BUILD.bazel` and can add `//apple/APIClient:AlondraAPIClient` to Shared dependencies without changing this package.

## Adapter seam

- Build one `AlondraAPIClient` from the old client's `base`, `device` and signer. Its signer protocol has the same requirements as the current Shared protocol; bridge with a small wrapper or move the old signature declarations when DeviceIdentity is extracted. The API client does not own `DeviceKeyStore`.
- Call `client.send(AlondraEndpoints.*)` for raw `Data`, then decode existing Shared DTOs in the old methods. For `answer`, pass the old client's `device` explicitly. For schedules, encode `ScheduledTaskDraft` in Shared and pass the exact bytes to `saveSchedule`.
- The old localization remains in Shared. Map `APIError` to `AlondraError` there: `unexpected(status: 404)` is the no-body activity fallback, while `.server(status: 404, ...)` is an alondrad JSON error. `forbidden` and `conflict` carry server `kind`, `detail`, and `reasons`; the adapter must translate structured close-node reasons to the existing `CloseRefused` text. Preserve 401, 403, 409 and 5xx semantics when the feature layer can consume typed errors.
- `upload` signs its separate Data argument because `URLSession.upload(for:from:)` sends those bytes while `URLRequest.httpBody` is nil. Keep `v1/activity` heartbeats, `DELETE v1/devices/{id}`, and the existing 15/95/70/120 second policies.
- No mutation retry is implemented. A caller must not add one until the backend has a reviewed idempotency contract.

The `AlondraEndpoints` factories enumerate every route in the previous `AlondraClient.swift`; API response DTOs that still live in Shared are deliberately decoded by the adapter. `AlondraAPIClient` depends only on Contracts and system networking/crypto/logging libraries.

## Service facade migration

`AlondraAPIClient(serviceURL:deviceID:operations:)` is the product API for both apps.
`ServiceOperationTransport` is an infrastructure capability implemented by Data's durable
coordinator, not a second app-facing client. It receives logical executor paths and opaque
operation identities; relay HTTP, encryption, status reconciliation, and journal retention
remain below this boundary. Mutation callers supply the identity already recorded by their
command use case. Unknown outcomes must be reconciled by that coordinator and never cause
an automatic mutation retry. Reads may use ephemeral IDs and must not accumulate permanent
mutation tombstones.

This PR deliberately retains the deprecated direct-daemon initializer only to keep the
unmigrated app roots compiling during the PR train. Before migration completion, remove
that initializer, request signing/direct HTTP implementation, Shared AlondraClient, and
SyncSession convenience overloads that infer identity from local signers. Wire the iOS and
macOS roots to the same service initializer and explicit approved pairing identity. The
service initializer cannot create a direct URLRequest or upload via the local daemon.

The current Data RelaySession still seals envelopes. Move that wire responsibility behind
APIClient's internal RelayTransport when consolidating RelayClient; Data must retain durable
operation ordering, outbox persistence, and reconciliation. APIClient must never import Data.


## Remaining executable consolidation sequence

Audit of the integrated service roots found three HTTP implementations: the product
API client, RelayClient, and ServicePairing/PairingTransport. A module rename alone
does not meet the single-client requirement.

1. Move the relay wire DTOs and transport contracts (`RelayReceipt` and
   `RelayRequestTransport`) into APIClient. Keep existing encoded representations.
   Move relay HTTP behind an internal implementation in APIClient, using its common
   HTTP transport with redirects rejected. Replace public `AlondraRelayClient`
   construction with narrow infrastructure capabilities from the same module.
2. Move request encoding, semantic binding, sealing, response opening and verification
   out of Data/RelaySession into APIClient `Service/Wire` and `Service/Crypto`.
   Data retains operation identity reservations, persistence before POST, locks,
   status reconciliation, read/mutation retention and cancellation ownership.
3. Route pairing HTTP/bootstrap/challenge through APIClient. Pairing keeps approval,
   attempts, Keychain and polling ownership. Update notification decoding imports.
   APIClient must not import Data, RelayOutbox, app stores or UI frameworks.
4. Update Bazel dependencies and tests for RelaySession, RelayOutbox, ServicePairing
   and Notifications; remove the separate RelayClient target. Preserve crypto
   vectors, lost-POST/relaunch/status-only integration tests, journal compatibility,
   identity validation and redirect/bearer protections. Build both apps and extension.
5. After UI parity, remove Shared.AlondraClient, direct-daemon API initialization,
   HTTP signing and identity-inference conveniences. First replace reachable
   AttachedQuestion consumers and extract reused presentation components from mixed
   legacy source files. Do not delete complete files merely because a legacy view
   shares the file with a component still used by the service UI.

Steps 1–4 form one reviewed transport consolidation branch. Step 5 is a dependent
cleanup branch after the visual gate. No automatic mutation retries may be introduced.
The final evidence must show no AlondraRelayClient module or direct-daemon constructor,
real restart recovery with the original operation identity, and unchanged rendered UI.
