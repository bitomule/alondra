# Relay service session

`RelaySession` is the internal approved-device operation coordinator. Product callers use the single `AlondraAPIClient(serviceURL:deviceID:operations:)` facade; inject this coordinator as its operation capability. It also conforms to `SyncTransport`.
Construct it with the approved pairing's phone signing/agreement private keys, pinned Mac
public keys, `RelayOutboxScope` (including credential generation), journal location, and
an account-scoped bearer closure. It obtains a service transport capability and envelope codec from APIClient so HTTP
challenges and envelopes always use the same signing key. APIClient owns wire/crypto;
this coordinator owns durable operation ordering and recovery. No SwiftUI or local HTTP client is needed.

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

`fetchOverview()` obtains a pinned, verified snapshot through APIClient's codec. Refresh
uses the encrypted daemon `/v1/changes?after=…&wait=…` endpoint with a wait capped at
50 seconds. Its successful completion requires a verified Mac envelope with a 2xx status.
While that read is pending, relay `/v1/changes` waits up to 25 seconds per GET using the
submission receipt's revision and bootstrap epoch. A completed event for this request
wakes a status GET, which retrieves the envelope. Received events and other jobs only
advance the relay cursor; they never invalidate daemon content. Authentication errors,
epoch changes and cancellation stop the invocation without reposting it.
A cancellable monotonic budget of daemon wait plus five seconds starts before bootstrap
and covers submission, status GETs and long polls. Expiry cancels in-flight HTTP and
late responses cannot become success; transports must cooperate with cancellation to
release their resources promptly.

Challenge bootstrap happens before new submission. Ordinary reads and mutation status
reconciliation refresh once after an unauthorized status GET; only GET may be retried.
A restart racing POST remains uncertain. On account/pairing replacement, deactivate the old
session and replace the SyncSession transport with a fresh session and identity; the
parent SyncSession generation drops old publications. Deactivation prevents new work
and ignores late results; an already dispatched mutation still requires reconciliation.

The lower-level execute API is for domain service/use-case adapters. Typed operations
and attachment encoding are supplied by the product APIClient facade; this coordinator
treats request bodies as opaque JSON and does not implement attachment policy.

GET operations use an in-memory, bounded request/status lifecycle and never create outbox records. Only mutations retain durable no-replay identities. A future refresh may issue a new GET safely; the same invocation never replays its POST after a lost response.

Mutation IDs are durably bound to canonical method/path/JSON bytes with an HMAC key derived
from the approved agreement key using a separate HKDF context. The journal stores no
plaintext or unkeyed low-entropy body digest. Mismatched requests fail before reconciliation,
including competing reservations and retired tombstones. Older records without a binding
cannot be reused through execute; explicit status-only resume remains available.
