# alondra-sandbox

Throwaway Macs, relay and phone for `make e2e-sandbox`. Everything listens on loopback and lives
in `SANDBOX_DIR` (default `/tmp/alondra-sandbox`); nothing touches `~/.alondra` or port 47800.

```sh
make e2e-sandbox MACS=2            # N daemons in service mode + relay + token issuer, in the background
make e2e-sandbox-logs MAC=2        # tail one daemon's log (MAC=relay for the relay)
make e2e-sandbox-stop              # stop everything and delete SANDBOX_DIR (KEEP=1 keeps it)
bazelisk test //tests/sandbox:scenarios   # opt-in: fresh two-Mac sandbox + automatic scenarios
make e2e-doc-review                # a document reviewed in the real TUI (tmux): the node gets the comments
bazelisk test //tests/sandbox:doc_review  # the same, opt-in through Bazel
```

`SANDBOX_PORT` (default 47890) moves every port: the issuer on `PORT-1`, the relay on `PORT`,
Mac *n* on `PORT+n`, and its direct listener on `PORT+20+n`. The direct listener binds loopback;
`SANDBOX_LAN=1` binds it on every interface so a real phone on the same Wi-Fi can reach it.
Sandbox Macs advertise on Bonjour as `_alondra-sandbox._tcp`, never as `_alondra._tcp`, so real
apps never list them; `ALONDRA_SANDBOX=1 alondra mac connect` searches for that type. The iOS and
Mac apps browse it only when built with `--define=alondra_sandbox=1` and launched with
`ALONDRA_SANDBOX=1` (the Mac app also takes `ALONDRA_PORT` for the sandbox Mac's loopback port).

## Layout

- `mac-N/`: that Mac's HOME, with `service-keys/`, `service.toml`, `config.toml`, `env`,
  `settings.json`, `alondrad.log`.
- `relay.json`, `relay.log`: the real `alondra-relay`, without Postgres or Keycloak.
- `issuer/`: the RSA key that signs sandbox account tokens and its `jwks.json`, served on loopback.
- `phones/<name>/mac-N.json`: the synthetic phone's identity toward each Mac through the relay;
  `mac-N-direct.json` the one it paired directly.
- `sandbox.json`: Macs, ports and URLs, for the phone and later tools.

## Synthetic phone

```sh
alondra-sandbox phone --mac 1 pair                      # approve on the Mac, wait for relay enrollment
alondra-sandbox phone --mac 1 request GET /capabilities # one encrypted request, prints the response
alondra-sandbox phone --mac 1 revoke                    # revoke on the Mac, wait until the relay refuses
alondra-sandbox phone --mac 1 pair --direct             # pair over the direct listener, codes compared
alondra-sandbox phone --mac 1 request --direct GET /capabilities   # the same request, without the relay
alondra-sandbox scenario                                # the automatic checks, on every Mac
```

`pair --direct` runs the local pairing as the iPhone will: `hello`, commitment, reveal, then it
reads the code the Mac lists on `GET /devices/pending` (what the Mac app and the TUI show), checks
it matches its own, and approves through `POST /devices/pending/<id>/approve` as a person would.

A terminal can pair directly too, in a HOME of its own:

```sh
ALONDRA_HOME=$SANDBOX_DIR/client ALONDRA_SANDBOX=1 alondra mac connect --host 127.0.0.1:47911
. $SANDBOX_DIR/mac-1/env && alondra device list         # lists the request with its code
. $SANDBOX_DIR/mac-1/env && alondra device approve <id>
ALONDRA_HOME=$SANDBOX_DIR/client alondra               # the TUI, with every Mac it paired
```

`--name` keeps several phones apart. A route the daemon does not approve for phones gets no
response, so `request` times out after 90 s instead of failing fast.

Pairing approves the phone through the daemon's local `POST /service/approve-device`, signed
with the Mac's key, the same two steps `alondra device invite` takes after the code matches. Answering an
ask needs one to exist, and only a Claude session the daemon launched can create one: launch a
probe node, then `phone request POST /interactions/<id>/answers '<body>'`.

## Cards end to end

`tools/e2e_cards.py` starts its own sandbox, has two nodes without a model put one card of each
kind in front of the user (an option, a request for text, a question answered by writing in its
reply field, four questions asked together, a tool permission the node waits on, and a delivery),
and answers every one from a client. Each card must leave at once, as its action's
`resolves_card` promised, and never come back; each card of the group must open on its own screen.

```sh
make e2e-relay-cards                 # the synthetic phone, every request through the relay
make e2e-direct-cards                # the same, over the Mac's direct listener
make e2e-cards-tui                   # the real TUI in tmux
make e2e-ios-cards                   # the iOS app in a pooled simulator, driven with MAV
E2E_CAPTURES=/tmp/cards make e2e-cards-tui   # keeps what the client showed at each step
```

The iOS run builds `//apple:AlondraiOS` for the simulator with `--define=alondra_sandbox=1`,
leases an iPhone 17 Pro on iOS 26.3 from `simpool`, finds the sandbox Mac on Bonjour
(`SANDBOX_LAN=1`, so its direct listener answers on the LAN address Bonjour gives) and approves the
pairing through the Mac's local route. It covers the direct connection only: the app signs in to
the relay with the production account server, which the sandbox does not have, so the relay is
covered by the synthetic phone. `tools/e2e_cards.py` with no `--client` keeps a seeded sandbox up
for a person until Ctrl+C. None of these run in `musts validate`; the same flows without a client
run there as `every_card_kind_leaves_the_queue_when_its_action_says_so_and_never_returns`
(`crates/alondrad/src/domain_e2e_tests.rs`).
