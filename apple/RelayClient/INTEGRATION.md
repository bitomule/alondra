# RelayClient integration contract

This module implements the cloud relay wire defined by Rust `crates/e2e` and `crates/relay` at `b34e0f5` plus multi-Mac routing in `21cf4c7`. It is deliberately separate from `AlondraAPIClient`, which still speaks the local daemon's HTTP API. The relay has no plaintext `/v1/overview` endpoint.

## Ownership and call sequence

1. Pair directly with the Mac. The phone pins the Mac P-256 signing and agreement keys from the local QR, generates distinct phone signing/agreement keys, and verifies the Mac's signed approval. The Mac enrolls the phone with signed `POST /v1/devices`. This module does **not** provide pairing or key storage; do not accept keys from the relay as a trust root.
2. Obtain a Keycloak access token with `aud=alondra-relay`. Construct `AlondraRelayClient` with the relay HTTPS base URL, paired phone device ID, a Keychain-backed signing closure, and an async bearer provider. The constructor rejects plain HTTP, including loopback, and credentials in the URL. Tests inject a fake transport behind an HTTPS URL. The default URLSession transport rejects all redirects, so a bearer cannot cross origins.
3. `bootstrap()` reads `{protocol:1, epoch:<base64url 16 bytes>}`. Signed operations lazily bootstrap. Each request signs `alondra-http-auth-v1` plus length-framed device ID, method, exact path and query, relay epoch, SHA-256 of exact body bytes, decimal millisecond timestamp, and fresh 16-byte nonce. Headers are `Authorization`, `x-device-id`, `x-relay-epoch`, `x-device-timestamp-ms`, `x-device-nonce`, `x-device-signature`. The signer returns 64-byte raw P-256 ECDSA `r||s`.
4. E2E `EnvelopeV1.seal` uses a fresh P-256 ephemeral key and 12-byte nonce, HKDF-SHA256 with salt SHA-256 of the framed authenticated header, ChaCha20-Poly1305 and an outer P-256 signature. `open` verifies pinned sender, recipient, route, expiry and signature before decrypting. The application must also match `request_id` and `kind` against its pending operation and enforce Mac epoch/tick ordering for changes.
5. Persist the encoded request envelope and its `request_id` before `submit`. POST receives 200/202 and a job view. Do not issue a new envelope on ambiguous timeout. Query `status(requestID:)`, then use `changes(since:epoch:)` for updates. The relay deduplicates only **identical raw request body bytes** under the same ID. This client does not auto-retry mutations. `registerPush` is also a POST and has no automatic retry.

## Wire routes

| Route | Response/role |
| --- | --- |
| `GET /v1/bootstrap` | Public protocol/epoch |
| `GET /healthz` | Public relay process health |
| `POST /v1/requests` | Opaque signed request envelope; 200/202 job view |
| `GET /v1/requests/{id}` | Device-scoped status and optional opaque response envelope |
| `GET /v1/changes?since=&epoch=&wait_s=` | Device-scoped revision and changed job views; max 25s wait |
| `POST /v1/push/register` | APNs token/environment/32-byte push binding; 204 |

## Remaining integration gates

- Add direct Mac pairing, persistent pinned keys and a durable outgoing request journal. No app view should own HTTP or signing. Put service/use-case adapters around this module, then drive them from TCA reducers.
- Define app-specific encrypted `LocalRequest`/`LocalResponse` adapters only after the Mac allowlist lands. Backend PR #156 at `1bcd9df` accepts JSON `{method,path,body?}` inside the envelope; successful local responses are `{status,body}`, where a non-JSON body becomes `{text:...}`. Its allowlist covers specific GET/POST/PUT/DELETE local endpoints; it rejects arbitrary paths and uploads. A local response above 180 KiB produces a sealed `502 response_too_large` fallback; the action may already have completed. This module therefore transports opaque plaintext bytes and does not claim upload support.
- A relay `409 relay_epoch_changed` or `stale_cursor` requires a fresh bootstrap and Mac-level resynchronization; this module surfaces `.conflict` and does not silently reset cursors.
- Verify real pairing, encrypted request/response, APNs, and notification extension key sharing with MAV on device before release.

`RelayClientTests` uses the Rust `cryptokit_vector.swift` fixture to verify decryption and signature plus a deterministic transport to verify signed HTTP bytes, bearer placement, and the one-POST contract. Build and test with `bazel test //apple/RelayClient:AlondraRelayClientTests`.
