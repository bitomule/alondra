# Relay operator API

The relay (`crates/relay`) holds devices, pairings and pushes. Who may still use it is decided by an
account service beside it. That service talks to the relay through the operator API, and the relay
reads the service's complete list of removals. The relay keeps every removal in its own registry and
never consults the service's database. Code: `crates/relay/src/operator.rs`, registry side in
`crates/relay/src/auth.rs` (`Removal`, `apply_removals`, `reconcile_removals`).

## Where it is served

- Only on the operator listener, `ALONDRA_OPERATOR_LISTEN`. The public listener answers `404` for
  every `/v1/operator/*` path.
- Every request carries `Authorization: Bearer <token>`, compared in constant time with the
  contents of `ALONDRA_OPERATOR_TOKEN_FILE` (at least 32 characters). Without that file every
  route answers `404`; a missing or wrong token answers `401 operator_token_invalid`.
- The same token authenticates the relay to the account source.

## Removals

A removal is one of:

| `kind` | Fields | Effect |
|---|---|---|
| `mac` | `owner`, `mac_id` | The Mac and every client paired with it leave the registry; their IDs stay revoked. |
| `client` | `owner`, `client_id`, `mac_id` | That client leaves the registry; its ID stays revoked. |
| `owner` | `owner` | Every device of the account leaves; the account is closed. A closed account's tokens are refused (`403 account_access_denied`), so it cannot enroll a Mac or a client again. |

Pairings in progress and Live Activity reports of what was removed go with it. Nothing is ever
restored: a removal the source no longer lists stays held.

### `POST /v1/operator/removals`

```json
{"removals": [{"seq": 7, "kind": "client", "owner": "…", "client_id": "…", "mac_id": "…"}]}
```

The account source numbers its removals from 1 without gaps and sends them in order, at most 1 000
per request. The relay holds the number of the last one it applied.

- Numbers already held are skipped, so sending a batch again changes nothing.
- `200 {"applied": n}`: every removal up to `n` is written to disk. Only then may the source
  forget it has to deliver them.
- `409 {"error": "removals_gap", "applied": n}`: the first new number is not `n + 1`. Nothing
  was applied; the source sends again from `n + 1`, and the relay reads the complete list.
- `409 {"error": "removal_conflict"}`: a removal names a device of another account or Mac than
  the one registered. Nothing of the batch was applied.
- `400 invalid_removals` for an empty ID or a batch too long. `422` for an unknown `kind` or a
  malformed body.
- `503 registry_unavailable`: the registry could not be written. Nothing was applied; send again.

The source retries a batch until it gets `200`. A removal is therefore lost only if the source
loses it, never because the relay was down.

### The complete list

With `ALONDRA_ACCOUNT_SOURCE_URL`, the relay reads `GET <url>` with the operator token. The source
answers `200` with the complete list, at most 16 MiB:

```json
{"seq": 42, "removals": [{"kind": "owner", "owner": "…"}]}
```

`seq` is the number of the last removal the list includes. The relay applies every entry, then
holds `seq`. It reads the list:

- **on start.** Until the first read succeeds, every route that checks the account answers
  `503 account_access_unavailable` and `/readyz` answers `503`. A device removed while the relay
  was down is never accepted in the meantime.
- **after a gap**, as soon as `POST /v1/operator/removals` answers `removals_gap`.
- **every 10 minutes**, as a safety net.

A failed read is retried, from 2 s and doubling up to 60 s. `ALONDRA_ACCOUNT_SOURCE_URL` must be
`https`, or `http` to loopback or to a single-label host of a private container network.

## An account's devices

`GET /v1/operator/owners/{owner}/devices`:

```json
{"macs": [{"mac_id": "…", "clients": ["…"]}]}
```

Only IDs leave the relay: no public keys, push tokens or endpoints.

## Pushes of finished sign-in sessions

A phone's push registration remembers the identity provider's offline session that made it. The
account service decides when such a session ended.

- `GET /v1/operator/push-sessions` lists the sessions:
  `{"sessions": [{"owner", "client_id", "registrations": [{"device_id", "sid"}]}]}`.
- `POST /v1/operator/push-sessions/ended` with `{"ended": [{"device_id", "sid"}]}` drops those
  registrations. A registration made since by another session stays. It answers `204`.
