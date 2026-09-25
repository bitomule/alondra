# Integration handoff for C

`//apple/APIClient:AlondraAPIClient` and its wire tests build independently. The old `AlondraShared.AlondraClient` is still the public app adapter. C owns `apple/BUILD.bazel` and can add `//apple/APIClient:AlondraAPIClient` to Shared dependencies without changing this package.

## Adapter seam

- Build one `AlondraAPIClient` from the old client's `base`, `device` and signer. Its signer protocol has the same requirements as the current Shared protocol; bridge with a small wrapper or move the old signature declarations when DeviceIdentity is extracted. The API client does not own `DeviceKeyStore`.
- Call `client.send(AlondraEndpoints.*)` for raw `Data`, then decode existing Shared DTOs in the old methods. For `answer`, pass the old client's `device` explicitly. For schedules, encode `ScheduledTaskDraft` in Shared and pass the exact bytes to `saveSchedule`.
- The old localization remains in Shared. Map `APIError` to `AlondraError` there: `unexpected(status: 404)` is the no-body activity fallback, while `.server(status: 404, ...)` is an alondrad JSON error. `forbidden` and `conflict` carry server `kind`, `detail`, and `reasons`; the adapter must translate structured close-node reasons to the existing `CloseRefused` text. Preserve 401, 403, 409 and 5xx semantics when the feature layer can consume typed errors.
- `upload` signs its separate Data argument because `URLSession.upload(for:from:)` sends those bytes while `URLRequest.httpBody` is nil. Keep `v1/activity` heartbeats, `DELETE v1/devices/{id}`, and the existing 15/95/70/120 second policies.
- No mutation retry is implemented. A caller must not add one until the backend has a reviewed idempotency contract.

The `AlondraEndpoints` factories enumerate every route in the previous `AlondraClient.swift`; API response DTOs that still live in Shared are deliberately decoded by the adapter. `AlondraAPIClient` depends only on Contracts and system networking/crypto/logging libraries.
