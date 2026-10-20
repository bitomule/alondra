# alondra-sandbox

Throwaway Macs, relay and phone for `make e2e-sandbox`. Everything listens on loopback and lives
in `SANDBOX_DIR` (default `/tmp/alondra-sandbox`); nothing touches `~/.alondra` or port 47800.

```sh
make e2e-sandbox MACS=2            # N daemons in service mode + relay + token issuer, in the background
make e2e-sandbox-logs MAC=2        # tail one daemon's log (MAC=relay for the relay)
make e2e-sandbox-stop              # stop everything and delete SANDBOX_DIR (KEEP=1 keeps it)
bazelisk test //tests/sandbox:scenarios   # opt-in: fresh two-Mac sandbox + automatic scenarios
make e2e-sandbox FAKES=1           # the same, with the sandbox's own claude and gh (below)
make e2e-journeys                  # opt-in: fresh sandbox with the fakes + scripted journeys
make e2e-explore                   # the tester agent drives each Mac's TUI and writes a report
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
- `mac-N/start`: runs that Mac's daemon with its environment; a journey restarts the daemon with it.
- With `FAKES=1`: `fakes/` (the `claude` and `gh` wrappers, every call in `calls.jsonl`, the
  scripts, the fake nodes, the fake GitHub's PRs in `github/prs.json`), `claude-home/` (the
  daemons' `CLAUDE_HOME` and `CLAUDE_CONFIG_DIR`), `plugin/` and `plugin-orchestrator/` (the
  marketplace), and `repo/` with its bare `remote.git`.

## Fakes (`FAKES=1`)

The daemons run with `$SANDBOX_DIR/fakes` first on their PATH, wake on and
`ALONDRA_SANDBOX_TIMING=fast` (revivals, the task closer, PR polling and the spacing between node starts take seconds, and
a busy Mac never holds the fake nodes' starts; only a
sandbox daemon honours it). No model and no GitHub are involved, and nothing reads `~/.claude`.

- **Orchestrator** (`claude -p`): creates and launches the tasks the run scripted for each request
  its batch names, through the `alondra mcp-orchestrator` Alondra configured.
- **Node** (`claude --bg`): a process with a worktree of `repo/`, its job file in `claude-home/jobs`,
  the plugin's reports and connection (`alondra node event|connect|ack`), the classic hooks
  (`alondra hook`) and the real `alondra mcp-node` with the settings file Alondra wrote. It cleans
  up when asked and follows its task's script; `claude stop|respawn|rm|agents` act on it, and
  `claude agents` names the process of a running one, as Claude Code does. While
  `fakes/hold-stop` exists, `claude stop` fails and the session goes on.
- **GitHub** (`gh`): PRs whose head is their branch's tip in `remote.git`; checks, drafts and merge
  failures are set per PR.

Move a node by hand: `alondra-sandbox fake nodes`, then
`alondra-sandbox fake node <task> work|ask|deliver|cleanup|end|die` (or `--step '<json>'`).

## Journeys

`make e2e-journeys` (`//tests/sandbox:journeys`) starts a two-Mac sandbox with the fakes, runs
`scenario`, then `alondra-sandbox journey all`; `JOURNEY=task-cycle|session|local` runs one, and
`alondra-sandbox journey <name>` runs against a sandbox already started with `FAKES=1`. A phone acts
as the user through views and actions, the way the iPhone does.

- `task-cycle` (through the relay):
  - TC1: request → task → node → PR delivered → review card → merge → clean-up → `claude rm` → finished.
  - TC2: a question answered on its card reaches the node.
  - TC3: requested changes, a second delivery, the new head merged.
  - TC4: cancelling removes the node and closes the PR.
  - F1–F4: refusals Alondra owes. A final delivery with an uncommitted file (named), a draft PR,
    failing checks handed back to the node, a second PR for one task.
- `session`:
  - S1: a crashed session is respawned and finishes.
  - S2: a session the user stopped is not respawned and its task is freed.
  - S3: a daemon restart keeps the card, the task and the node.
  - S4: an ended session's bearer is refused.
- `local` (last, since it stops the relay):
  - L1: a whole task over the direct listener with the relay down.
  - L2: each Mac shows only its own tasks, and a terminal pairs directly with both.
  - L3: a revoked phone gets its sealed `device_revoked` and creates nothing.

Evidence lands in `bazel-testlogs/tests/sandbox/journeys/test.outputs/` (`$SANDBOX_DIR/journeys`
when run by hand):
- `journeys.md` and `journeys.json`: each step with what it expected and saw;
- `views/<case>/`: the views at each milestone;
- `fakes/`: every `claude` and `gh` call, and each node's messages and steps;
- `logs/`: the daemons' and relay's logs, kept when a case failed.

## Agent SDK backend (`SDK=1`)

`SDK=1 FAKES=1 make e2e-sandbox` gives Mac 1 the Agent SDK backend: every task it launches runs
through the SDK host (`agent-host`, pinned Node) and the pinned native Claude Code 2.1.300, never
through `claude --bg`. `tools/e2e_sdk_backend.py` sets it up:
- copies the pinned Node and `persistent.cjs`, and extracts the native Claude from its npm archive
  (size and SHA256 checked);
- starts the local model double (`tests/native/claude/model_double.py`), which answers each
  `/v1/messages` with one text reply, or with the tool call `sdk/model-tools` scripts for the task's
  prompt, and logs the request and the tool results it carries in `$SANDBOX_DIR/sdk/model-requests.jsonl`;
- writes the Seatbelt profile the whole host runs under (deny default, no mach-lookup, network only
  to the model double, writes only in the sandbox's SDK folders, no programs but the shell, `sleep`
  and Alondra's CLI) and the Mac's `sdk-backend.json`.

Claude gets a synthetic HOME, configuration and temporary folder and the public fixture key: never
the real login, Keychain or `~/.claude.json`. Only a sandbox daemon keeps a synthetic HOME: outside
one, the configuration's native Claude runs in the user's real HOME with their default profile, and
`alondrad --preflight` checks the configuration as the daemon would admit it.

`make e2e-sdk` (`//tests/sandbox:sdk_journey`, opt-in, macOS arm64) starts such a sandbox and runs
the `sdk` journey: the phone sends a request, the native Claude's first turn ends with the card
«stopped without delivering or asking», «Carry on» with a note reaches Claude, its next turn ends
with a card of its own, and what the user writes on that card reaches Claude too. Then the model
double answers the tasks' requests with a 401 and later a 529 while `sdk/model-refuse` holds that
status: the native Claude's error holds each task on its sign-in or API card, and «I signed in» or
«Try now» lets it go on once the double answers again. Last, a task's native Claude, in a synthetic HOME
that is its default profile, loads the user's CLAUDE.md, skill and enabled plugin, the project's
CLAUDE.md, the local CLAUDE.local.md and Alondra's node plugin, whose MCP server connects.
Then the double answers tasks with tool calls the journey scripts in `sdk/model-tools`: a command
waits on its permission card and runs once the phone allows it; a question's option reaches Claude
as its answer; a plan the model writes to plan mode's plan file waits on its Review card and Claude
goes on with it once the phone approves it; «Interrupt» in Fleet stops a running command and the turn ends with one card; a
command that acts as the user is refused by the node guard with no card; a command that runs for
seconds finishes and its turn ends with one card.
`alondra-sandbox journey sdk` runs it against a sandbox already started with `SDK=1`; `journey all`
leaves it out.

Start a sandbox you will drive from the TUI under `/private/tmp` (`SANDBOX_DIR=/private/tmp/…`):
the local action journal opens SQLite with `NOFOLLOW`, and `/tmp` is a symbolic link, so the TUI's
actions would get `503 outcome_unknown` and be retried in silence.

## Tester agent

`make e2e-explore` (`tools/e2e-explore.sh`) builds a fresh two-Mac sandbox with the fakes and seeds
it (`alondra-sandbox journey seed`):
- on Mac 1, a working task, a question, a PR in review, a finished task, a stopped node and a
  pending pairing;
- on Mac 2, a PR in review.

It opens each Mac's TUI in tmux (160x45, Spanish) and runs a print-mode Claude (`MODEL`, default
opus; `BUDGET` in USD, default 5) with `tools/e2e/explorer.md`. The agent drives the TUI with
`tmux send-keys`/`capture-pane`, moves the fake nodes, reads the views, and writes `report.md`
with the findings, captures and views in `/tmp/alondra-explore-<stamp>-report` (`REPORT_DIR`).

Its Claude starts with a clean environment and the Alondra plugins off, so it never reaches the
real Alondra. It may only run tmux, the sandbox's binaries and write in its report folder.
`FOCUS="…"` adds instructions.

## Synthetic phone

```sh
alondra-sandbox phone --mac 1 pair                      # approve on the Mac, wait for relay enrollment
alondra-sandbox phone --mac 1 request GET /capabilities # one encrypted request, prints the response
alondra-sandbox phone --mac 1 revoke                    # revoke on the Mac, wait until the relay refuses
alondra-sandbox phone --mac 1 pair --direct             # pair over the direct listener, codes compared
alondra-sandbox phone --mac 1 request --direct GET /capabilities   # the same request, without the relay
alondra-sandbox phone --mac 1 press --direct card:<id> "Short name"   # press a view's action by label or id prefix
alondra-sandbox phone --mac 1 press --direct task:<id> msg: --text "…" # an action with its text field
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
