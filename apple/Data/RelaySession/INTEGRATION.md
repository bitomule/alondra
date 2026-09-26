# Relay service session

`RelaySession` is the internal approved-device operation coordinator. Product callers use the single `AlondraAPIClient(serviceURL:deviceID:operations:)` facade; inject this coordinator as its operation capability. It also conforms to `SyncTransport`.
Construct it with the approved pairing's phone signing/agreement private keys, pinned Mac
public keys, `RelayOutboxScope` (including credential generation), journal location, and
an account-scoped bearer closure. It constructs its own relay client so HTTP challenges
and envelopes always use the same signing key. No SwiftUI or local HTTP client is needed.

Use cases persist a semantic `requestID` before calling `execute(requestID:method:path:body:)`.
The body is JSON bytes, not an encoded string or HTTP headers. The result is either a
verified local status/body or `unknown`. Existing IDs are status-only reconciliation,
including after process recreation, timeout, relay loss, or 404. An unknown result must
remain visible as unresolved; it never authorizes an automatic retry with a fresh ID.
`resume(requestID:)` explicitly reconciles an existing operation. Once the use case has
durably applied a completed result, call `retireAppliedResponse(requestID:)`.

All Mac 5xx responses and explicit `error: uncertain` remain unknown; in particular a
502 `response_too_large` may follow a mutation that already executed. A completed 4xx is
returned to the use case for domain interpretation. Only verified Mac envelopes carry
local results. Relay acknowledgement/status alone never proves business success.

`fetchOverview()` decrypts and validates the snapshot before returning it. The relay changes feed describes job lifecycle, not daemon data invalidation; consuming
our own overview completions would create an unbounded polling loop. Refresh instead waits
a cancellable bounded timer (1–25 seconds; SyncSession requests 10 seconds), so background
Mac changes appear within the wait plus request latency. No server-push freshness is claimed.
Challenge bootstrap happens before new submission and refreshes once after an unauthorized
status GET; only the GET may be retried. A restart racing POST remains uncertain. On account/pairing replacement, deactivate the old
session and replace the SyncSession transport with a fresh session and identity; the
parent SyncSession generation drops old publications. Deactivation prevents new work
and ignores late results; an already dispatched mutation still requires reconciliation.

The lower-level execute API is for domain service/use-case adapters. Typed feature
operations and the removal of existing app-local clients remain integration work.
Binary uploads are unsupported by the current backend relay contract.

GET operations use an in-memory, bounded request/status lifecycle and never create outbox records. Only mutations retain durable no-replay identities. A future refresh may issue a new GET safely; the same invocation never replays its POST after a lost response.

Mutation IDs are durably bound to canonical method/path/JSON bytes with an HMAC key derived
from the approved agreement key using a separate HKDF context. The journal stores no
plaintext or unkeyed low-entropy body digest. Mismatched requests fail before reconciliation,
including competing reservations and retired tombstones. Older records without a binding
cannot be reused through execute; explicit status-only resume remains available.
