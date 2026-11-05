# Relay operator API

The relay (`crates/relay`) holds devices, pairings and pushes. Who may still use it is decided by an
account service beside it. That service talks to the relay through the operator API, and the relay
reads the service's complete list of removals. The relay keeps every removal in its own registry and
never consults the service's database. Code: `crates/relay/src/operator.rs`, registry side in
`crates/relay/src/auth.rs` (`Removal`, `apply_removals`, `reconcile_removals`).

## Where it is served

- Only on the operator listener, `ALONDRA_OPERATOR_LISTEN`, which must be a loopback address or an
  address of a private network: the API is never meant to face the internet. The public listener
  answers `404` for every `/v1/operator/*` path.
- Without `ALONDRA_OPERATOR_LISTEN` there is no operator listener at all. Setting it without
  `ALONDRA_OPERATOR_TOKEN_FILE` stops the relay at start.
- Every request carries `Authorization: Bearer <token>`, compared in constant time with the
  contents of `ALONDRA_OPERATOR_TOKEN_FILE` (at least 32 characters). A missing or wrong token
  answers `401 operator_token_invalid`.
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

A removal never touches a device of another account, nor an ID the registry holds for the other
kind of device (a `client` naming a Mac, a `mac` naming a client). Such a removal is skipped and
logged, and the removals around it still apply. A `client` that the registry pairs with another
Mac of the same account than the one named is removed all the same.

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
- `409 {"error": "removals_behind", "applied": n}`: the batch's highest number is below `n`, the
  number the relay holds. The source's numbering went back, for example after a restore. Nothing
  was applied. The source treats it as a reset: the relay reads the complete list and adopts its
  number, and the source then sends again what it still has to deliver.
- `400 invalid_removals` for an empty ID or a batch too long. `422` for an unknown `kind` or a
  malformed body.
- `503 registry_unavailable`: the registry could not be written. Nothing was applied; send again.

The source sends one batch at a time and waits for its answer.

The source retries a batch until it gets `200`. A removal is therefore lost only if the source
loses it, never because the relay was down.

### The complete list

With `ALONDRA_ACCOUNT_SOURCE_URL`, the relay reads `GET <url>` with the operator token. The source
answers `200` with the complete list, at most 16 MiB:

```json
{"seq": 42, "removals": [{"kind": "owner", "owner": "…"}]}
```

`seq` is the number of the last removal the list includes. The relay applies every entry, then
holds `seq`, even when it is below the number it held: the source's numbering is followed. An entry
with an empty or too long ID is skipped and logged. When nothing in the list is new, the registry
is not written again. It reads the list:

- **on start.** Until the first read succeeds, every route that checks the account answers
  `503 account_access_unavailable` and `/readyz` answers `503`. A device removed while the relay
  was down is never accepted in the meantime.
- **after a gap**, as soon as `POST /v1/operator/removals` answers `removals_gap`.
- **every 10 minutes**, as a safety net.

A failed read is retried, from 2 s and doubling up to 60 s. `ALONDRA_ACCOUNT_SOURCE_URL` must be
`https`, or `http` to loopback or to a single-label host of a private container network.

## What a Mac reads of its removals

A removal that takes clients away also tells the Mac they were paired with, so that the Mac unpairs
them too and they lose access on the local network and over a direct path, not only through the
relay. For each Mac the registry holds the clients the account removed from it, each with when, and,
once the account removed the Mac itself, the Mac's signing key, so that the removed Mac can still
read this. A removal the Mac made itself (`DELETE /v1/devices/{id}`, `DELETE /v1/macs/{id}`) is not
held: the Mac already knows it.

The Mac reads it in the bootstrap it already asks for once a minute. `GET /v1/bootstrap` stays
public; a Mac that signs it, with its usual headers over the epoch it last knew, gets one more field:

```json
{"protocol": 1, "epoch": "…", "removals": {"mac_removed": false, "clients": [{"client_id": "…", "removed_at_ms": 1767225600000}]}}
```

- `503 account_access_unavailable` to a signed bootstrap until the complete list is first read.
- A signature that does not hold, such as one over an epoch the relay no longer has, gets the
  public answer without `removals`, so that the Mac always learns the current epoch and signs the
  next bootstrap with it.

The daemon signs every bootstrap once it knows an epoch, also the first one after a failure, so a
Mac that was off reads its removals in its first request back (its second, if the relay restarted
meanwhile). It unpairs each listed client it approved before `removed_at_ms`, as revoking it in the
Mac app does: a client paired again after its removal stays. A relay that predates this field
never sends it, and a daemon that predates it ignores it.

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

## Not yet

- One token serves both directions; a token per direction would let either side rotate its own.
- Operator requests share the public listener's limit of concurrent requests.
- Closed accounts and the removal number are new registry fields. A relay binary older than this
  API keeps the registry readable but drops both when it writes it. After such a rollback, the
  complete list read by a current binary restores them.
