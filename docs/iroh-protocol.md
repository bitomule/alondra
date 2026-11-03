# iroh protocol between devices and a Mac

What a client (the CLI and the apps) speaks to `alondrad`'s iroh endpoint. The daemon side is `crates/alondrad/src/iroh_endpoint.rs`.

## Finding the Mac

- **Endpoint ID**: the Mac's iroh public key, 64 lowercase hex characters. A client learns it
  from the pairing QR (`mac_iroh_endpoint`, phones only), from `GET /direct/hello`
  (`iroh_endpoint`) and from the encrypted `GET /routes` (`body.iroh_endpoint`). `/routes` is
  authenticated end to end, so a client paired before iroh existed learns it through the relay.
- **Where to dial it**: the encrypted `/routes` also gives `iroh_addrs` (the endpoint's direct
  addresses, `ip:port`) and `iroh_relay` (the iroh relay it uses, or `null`). The CLI keeps them in
  `direct-iroh.json` beside its pairings and learns them again when they stop working.
- **Relay**: the Alondra relay's `GET /v1/bootstrap` names it in `iroh_relay` when it has one.
  The Mac binds its endpoint there (`PUT /v1/iroh-node`); the iroh relay admits only bound
  endpoints. A phone with an account binds its own endpoint the same way.
- **A device without an account** (a terminal paired directly) cannot bind its endpoint itself.
  The Mac does it for the device: once the device's hello succeeds, the Mac sends
  `PUT /v1/devices/{device_id}/iroh-node` with `{"iroh_node_id", "hello_signature"}`, signed as the
  Mac, after binding its own endpoint, once per relay epoch and whenever the endpoint changes. The
  relay checks that the device is enrolled for that Mac and account and that the signature is the
  device's own hello from that endpoint to this Mac's bound endpoint, so the Mac cannot bind an
  endpoint the device never used. Revoking the device, the Mac or the account ends the binding.
  The device therefore reaches the iroh relay only after one hello over the LAN or Tailscale, which
  pairing already does.
- **ALPN**: `alondra/1`.

## Hello

The first bidirectional stream of every connection. The client writes, then finishes the stream:

```json
{"device_id": "<paired device ID>", "signature": "<base64url P-256 ECDSA, 64 bytes r‖s>"}
```

The signature is made with the device's paired signing key (the one that signs envelopes) over
`alondra_e2e::iroh_hello_bytes`: the ASCII domain `alondra-iroh-hello-v1`, then four fields,
each a 4-byte big-endian length followed by its bytes:

1. the Mac ID (`mac-…`);
2. the device ID;
3. the client's own iroh endpoint ID, 32 raw bytes;
4. the Mac's iroh endpoint ID, 32 raw bytes.

Both endpoint IDs tie the signature to this connection: it cannot be replayed from another
endpoint or to another Mac. `iroh_hello_fixed_vector` in `crates/e2e/src/lib.rs` holds a vector
to check another implementation against.

The Mac answers `{"ok": true}`, or `{"error": "unauthorized"}` and closes the connection with
application code 1. A connection without a valid hello within 10 seconds is closed with code 2.
A device revoked on this Mac but still remembered is let in, so its first request is told,
sealed, that it was revoked.

## Requests

Every later bidirectional stream carries one request. The client writes one sealed request
envelope (the same JSON as `POST /direct/request`, at most 256 KiB) and finishes the stream. The
Mac answers with one JSON object and finishes:

| Answer | Meaning |
|---|---|
| `{"response": <sealed response envelope>}` | Answered; open it like a direct response |
| `{"revoked": <sealed response envelope>}` | This device was revoked here; it opens to `device_revoked` |
| `{"error": "wrong_sender"}` | The envelope's sender is not the device of the hello |
| `{"error": "unauthorized"}` | Bad signature, expired, or the device is no longer approved |
| `{"error": "wrong_mac"}` | The envelope is for another Mac |
| `{"error": "idempotency_conflict"}` | That request ID was already used with other bytes |
| `{"error": "rejected", "detail": "…"}` | The Mac refused the request |
| `{"error": "invalid_envelope"}` / `{"error": "too_large"}` | Not an envelope, or over 256 KiB |

The envelope goes through the same handler as the direct listener, with one receipt journal:
the same envelope gets the same answer whichever path it took, and a repeated mutation never
runs twice. Approval is checked again on every stream.

## Limits

32 streams at a time per connection (unidirectional streams are not accepted) and 64
connections per Mac; another connection is closed with code 3.

## The Mac's `service.toml`

```toml
[iroh]
enabled = true     # default
relay = "auto"     # the iroh relay /v1/bootstrap names; "off", or an https URL
port = 47811       # UDP port, falling back to a free one when taken; 0 picks a free one
# listen = "127.0.0.1"  # bind one address instead of every interface
portmapper = true  # ask the router for a port mapping (UPnP, NAT-PMP, PCP)
```

The key is `iroh.key` beside the service signing key (`~/.alondra/service-keys/`), created by
the daemon on its first start. `/daemon` reports the endpoint, its relay, and whether macOS lets
the daemon send to the local network (`local_network`), checked again on each read older than two
seconds.

## Reachability

A phone on a mobile network usually sits behind a symmetric NAT, so a direct path opens only
when the Mac has a way in: a port the router maps, a forwarded port, a public IPv6 address, or no
NAT. The daemon diagnoses that from iroh's net report, a probe of the router's port mapping
protocols and its own addresses, and counts each closed device connection by the path it used.
It serves the result as `iroh.reachability` on `/daemon` and `direct_connection` on
`GET /settings`:

```json
{"verdict": "relay_only", "confirmed": false, "reasons": ["no_port_mapping", "no_ipv6"],
 "port_mapping": {"upnp": true, "nat_pmp": true, "pcp": true, "mapped": null},
 "public_ipv4": "203.0.113.7", "public_ipv6": null, "nat": "unknown",
 "port": 47811, "port_fixed": true, "lan_ip": "192.168.1.20",
 "sessions": {"lan": 0, "internet": 0, "relay": 0}}
```

`verdict` is `reachable`, `relay_only`, `checking` or `unknown`; `confirmed` says a device away
from home already connected directly. Reasons: `local_network_denied`, `relay_off`,
`relay_disconnected`, `captive_portal`, `relay_on_same_network`, `no_udp`, `symmetric_nat`,
`router_offers_no_mapping`, `no_port_mapping`, `no_ipv6`, `port_not_fixed`. Clients word them and
read an unknown code as nothing.
