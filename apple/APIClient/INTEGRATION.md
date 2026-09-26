# One service API client

`AlondraAPIClient(serviceURL:deviceID:operations:)` is the product API for both apps.
This Bazel module owns the service HTTP challenge, bootstrap, wire DTOs and envelope
cryptography. `Service/Internal/RelayTransport` is internal; infrastructure callers use
`makeServiceTransport` and its `ServiceTransport` capability. Pairing shares that HTTP
implementation rather than constructing its own URLSession. Production HTTP rejects
redirects, cookies and caches; signed requests retain exact body/path/query bytes.

`ServiceEnvelopeCodec` owns canonical request encoding, the keyed semantic binding,
sealing and pinned-response verification. Data's RelaySession owns operation identity,
persistence before POST, ordering, cancellation and status-only reconciliation. RelayOutbox
owns durable envelopes and tombstones. The APIClient module never imports Data.
`RelayReceipt` and `RelayRequestTransport` live here so the dependency stays one-way.
Neither the codec nor the HTTP transport automatically retries a mutation.

Pairing owns approval policy, attempts, polling and Keychain persistence. Its HTTP calls
use the same challenge/bootstrap path. Bootstrap now checks protocol version 1 as well
as the 16-byte epoch for pairing, matching the service's published contract; unsupported
versions fail before candidate submission. Pinned Mac keys remain the trust root.

## Verification

APIClient tests retain the Rust encryption vector, signed HTTP bytes, bearer isolation,
endpoint rejection and one-POST assertions. Data integration tests retain lost responses,
relaunch, status-only reconciliation, canonical identity conflicts and journal semantics.
Pairing tests retain approval signature/account verification and cover unsupported bootstrap
protocols. Notification decoding depends directly on this module's envelope format.
Run the APIClient, RelaySession, RelayOutbox, ServicePairing and Notifications Bazel tests,
then compile both apps and the notification extension before merging.

## Remaining legacy removal gate

The separate AlondraRelayClient module/type and PairingTransport are removed. The deprecated
direct-daemon initializer and Shared.AlondraClient remain temporarily until strict original
UI parity is accepted. Remove their HTTP signing, endpoint adapters and SyncSession identity
inference in that dependent cleanup. Replace reachable AttachedQuestion consumers and retain
presentation components reused by service views. This transport change does not modify UI.
