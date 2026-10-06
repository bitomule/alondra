# alondra-sandbox

Throwaway Macs, relay and phone for `make e2e-sandbox`. Everything listens on loopback and lives
in `SANDBOX_DIR` (default `/tmp/alondra-sandbox`); nothing touches `~/.alondra` or port 47800.

```sh
make e2e-sandbox MACS=2            # N daemons in service mode + relay + token issuer, in the background
make e2e-sandbox-logs MAC=2        # tail one daemon's log (MAC=relay for the relay)
make e2e-sandbox-stop              # stop everything and delete SANDBOX_DIR (KEEP=1 keeps it)
bazelisk test //tests/sandbox:scenarios   # opt-in: fresh two-Mac sandbox + automatic scenarios
```

`SANDBOX_PORT` (default 47890) moves every port: the issuer on `PORT-1`, the relay on `PORT`,
Mac *n* on `PORT+n`, and its future direct port on `PORT+20+n` (reserved in `sandbox.json`).

## Layout

- `mac-N/`: that Mac's HOME, with `service-keys/`, `service.toml`, `config.toml`, `env`,
  `settings.json`, `alondrad.log`.
- `relay.json`, `relay.log`: the real `alondra-relay`, without Postgres or Keycloak.
- `issuer/`: the RSA key that signs sandbox account tokens and its `jwks.json`, served on loopback.
- `phones/<name>/mac-N.json`: the synthetic phone's identity toward each Mac.
- `sandbox.json`: Macs, ports and URLs, for the phone and later tools.

## Synthetic phone

```sh
alondra-sandbox phone --mac 1 pair                      # approve on the Mac, wait for relay enrollment
alondra-sandbox phone --mac 1 request GET /capabilities # one encrypted request, prints the response
alondra-sandbox phone --mac 1 revoke                    # revoke on the Mac, wait until the relay refuses
alondra-sandbox scenario                                # the automatic checks, on every Mac
```

`--name` keeps several phones apart. A route the daemon does not approve for phones gets no
response, so `request` times out after 90 s instead of failing fast.

Pairing approves the phone through the daemon's local `POST /service/approve-device`, signed
with the Mac's key, the same two steps `alondra pair` takes after the code matches. Answering an
ask needs one to exist, and only a Claude session the daemon launched can create one: launch a
probe node, then `phone request POST /interactions/<id>/answers '<body>'`.
