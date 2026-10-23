# alondra-sandbox

Throwaway Macs, relay and phone for `make e2e-sandbox`. Everything listens on loopback and lives
in `SANDBOX_DIR` (default `.scratch/sandbox` at the root of the checkout or worktree it runs from,
which git ignores); nothing touches `~/.alondra` or port 47800. A Unix socket's path fits in 104
bytes, so the Macs' homes and the fakes, which hold sockets, live in a short folder of their own
(`/private/tmp/asb.*`, linked from `SANDBOX_DIR/mac-N` and `SANDBOX_DIR/fakes`) that the scripts
create and delete; everything else, `bin/` included, stays in `SANDBOX_DIR`.

```sh
make e2e-sandbox MACS=2            # N daemons in service mode + relay + token issuer, in the background
make e2e-sandbox-logs MAC=2        # tail one daemon's log (MAC=relay for the relay)
make e2e-sandbox-stop              # stop everything and delete SANDBOX_DIR (KEEP=1 keeps it)
bazelisk test //tests/sandbox:scenarios   # opt-in: fresh two-Mac sandbox + automatic scenarios
make e2e-sandbox FAKES=1           # the same, with the sandbox's own claude and gh (below)
make e2e-journeys                  # opt-in: fresh sandbox with the fakes + scripted journeys
make e2e-explore                   # the tester agent drives each Mac's TUI and writes a report
make e2e-doc-review                # a document reviewed in the real TUI (tmux): the node gets the comments
make e2e-doc-links                 # a README with HTML in the real TUI (tmux): v opens its links and images
make e2e-turn-end-wait             # a turn that ends on a message to the orchestrator waits for it, in the real TUI (tmux)
make e2e-turn-end-wait TO_USER=1   # the same with a question to the user: no card says the task stopped
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
- `phones/<name>/mac-N.json`: the synthetic phone's identity toward each Mac it paired with the
  account (enrolled at the relay, reached over iroh); `mac-N-direct.json` the one it paired directly.
- `sandbox.json`: Macs, ports and URLs, for the phone and later tools.
- `mac-N/start`: runs that Mac's daemon with its environment; a journey restarts the daemon with it.
- `mac-N/sdk-backend.json` and `sdk/`: the Agent SDK backend every Mac gets when `SDK_NODE` and
  `SDK_HOST` name the pinned Node and the host bundle (`make e2e-sandbox` always passes them):
  `sdk/runtime/` holds their copies, `sdk/home/` and `sdk/tmp/` are Claude's HOME and TMPDIR, and
  the native Claude is the sandbox's `fakes/claude`. Each host's state root is a short folder in
  `/private/tmp/as-sdk-*` (named in `mac-N/sdk-state-root`), for its Unix socket.
- With `FAKES=1`: `fakes/` (the `claude` and `gh` wrappers, every call in `calls.jsonl`, the
  scripts, the native sessions in `sessions/`, the fake GitHub's PRs in `github/prs.json`),
  `claude-home/` (the daemons' `CLAUDE_HOME` and `CLAUDE_CONFIG_DIR`), `plugin/` and
  `plugin-orchestrator/` (the marketplace), and `repo/` with its bare `remote.git`.

## Fakes (`FAKES=1`)

The daemons run with `$SANDBOX_DIR/fakes` first on their PATH, wake on and
`ALONDRA_SANDBOX_TIMING=fast` (revivals, the task closer, PR polling and the spacing between node starts take seconds, and
a busy Mac never holds the fake nodes' starts; only a
sandbox daemon honours it). No model and no GitHub are involved, and nothing reads `~/.claude`.

- **Native Claude** (what the SDK host spawns, `claude --output-format stream-json --verbose
  --input-format stream-json …`, `src/fake/native.rs`): speaks the host's stream-json protocol as
  `agent-host/persistent-fake-claude.py` does. An orchestrator batch (`--restricted`) creates and
  launches the tasks the run scripted for each request it names, or closes the request with its
  scripted answer, through the `alondra mcp-orchestrator` its `--mcp-config` names; a card a run
  wrote in `fakes/orchestrator/ask.json` it asks the user once, on its next batch. A task's session
  works in the worktree Alondra made and acts through the real `alondra mcp-node` with the
  environment the host gave it: each input opens or joins a turn, which ends once it delivers, asks
  or cleans up; it follows its task's script, cleans up when asked, asks the host for a Bash
  command's permission unless the main checkout's `.claude/settings.local.json` allows it, and keeps
  each start, input, step and its transcript in `fakes/sessions/<first 8 of its id>/`, so
  `--resume` finds the conversation. A journey moves it through `order.sock` there, inside its turn.
- **Background session** (`claude --bg`, `src/fake/node.rs`) and **`claude -p`**: what a daemon
  without the SDK backend calls, as the previous release does in `deploy/brew/upgrade-rehearsal.py`.
  The node keeps its job file in `claude-home/jobs`, the plugin's reports and connection (`alondra
  node event|connect|ack`), the classic hooks (`alondra hook`) and the real `alondra mcp-node` with
  the settings file Alondra wrote; `claude stop|respawn|rm|agents` act on it. While
  `fakes/hold-stop` exists, `claude stop` fails and the session goes on.
  While `fakes/hold-interrupt` exists, the native Claude accepts an interrupt and its turn goes
  on, so only «Force stop» ends it. While `fakes/fail-turn` exists, each turn ends as Claude Code
  ends one at its turn limit (`error_max_turns`); while `fakes/drop-input` exists, each input after
  the mission is completed without being started, so Claude never reads it.
- **Claude accounts**: the native Claude runs on the account its `CLAUDE_CONFIG_DIR` folder names,
  `default` without one, and writes it in each start. `claude auth status|login|logout` keep each
  folder's sign-in in its `.sandbox-account`; the synthetic HOME's `~/.claude` is signed in. The
  usage probe answers `fakes/usage/<account>.json` (Claude's usage endpoint as is). While
  `fakes/refuse/<account>` holds `rate_limit` or `authentication_failed`, every turn on that account
  ends as Claude Code ends one the API refused (429 with the 5-hour window's reset, or 401). The step
  `{"do": "rate_limit", "info": {…}}` sends a `rate_limit_event` with that `rate_limit_info`.
- **GitHub** (`gh`): PRs whose head is their branch's tip in `remote.git`; checks, drafts and merge
  failures are set per PR.

Move a node by hand: `alondra-sandbox fake nodes`, then
`alondra-sandbox fake node <task> work|ask|deliver|cleanup|end|die` (or `--step '<json>'`, one step
or a list). `die` kills a native session's Claude, as a crash does; its host goes on.
`{"do": "emit", "message": {…}}` has a native Claude say one message of its own as is (a
`task_started`, `background_tasks_changed` or `task_notification`), to put background work in a
task's detail.

## Journeys

`make e2e-journeys` (`//tests/sandbox:journeys`) starts a two-Mac sandbox with the fakes, runs
`scenario`, then `alondra-sandbox journey all`; `JOURNEY=task-cycle|session|reopen|mcp|local|accounts` runs one, and
`alondra-sandbox journey <name>` runs against a sandbox already started with `FAKES=1`. A phone acts
as the user through views and actions, the way the iPhone does.

- `task-cycle` (over iroh):
  - TC1: request → task → node → PR delivered → review card → merge → clean-up → the node's host
    ends and its worktree goes → finished.
  - TC2: a question answered on its card reaches the node.
  - TC3: requested changes, a second delivery, the new head merged.
  - TC4: cancelling ends the node and the task is no longer in progress.
  - F1–F4: refusals Alondra owes. A final delivery with an uncommitted file (named), a draft PR,
    failing checks handed back to the node, a second PR for one task.
- `session`:
  - S1: a node whose native Claude crashed carries on by itself in the resumed session, with no card, and finishes.
  - S2: «Interrupt» on the task ends the turn with one card, and the node's Claude is not restarted.
  - S3: a daemon restart keeps the card, the task and the node, whose host it adopts.
  - S4: a crashed session's bearer is refused once it resumes with a new one.
  - S5: from a turn Claude started by itself, with the user's message waiting behind it, a plan of 41 points is refused and Claude gets why; its shorter plan reaches the user's Review card and the message then runs.
  - S6: a node force-stopped while it runs its work after the merge leaves a card that names that work and offers «Carry on» and «Close the task»; closing finishes the task.
- `local` (last, since it stops the relay):
  - L1: a whole task over the direct listener with the relay down.
  - L2: each Mac shows only its own tasks, and a terminal pairs directly with both.
  - L3: a revoked phone gets its sealed `device_revoked` and creates nothing.
- `reopen` (over the direct listener):
  - R1: a task reopened with every slot taken waits in the queue saying why, then starts with a new node.
- `accounts` (after `local`, over the direct listener), with the default account and Trabajo, added
  through `PUT /settings/accounts`:
  - A1: a new task starts on the account with the most margin: Trabajo at 10 % of its week, then
    the default once Claude reports Trabajo at 90 %.
  - A2: the default account's turn is refused for its usage limit: its task waits on its card, the
    Mac does not wait, and a new task starts on Trabajo.
  - A3: with Trabajo out too the Mac waits, a reopened task reads «Didn't start · Claude usage ran
    out» and nothing starts; once Claude reports Trabajo's window reset, both start on Trabajo, its
    held task goes on, and the default's still waits.
  - A4: Trabajo's sign-in expires: its task's card says to run `alondra accounts login "Trabajo"`,
    `alondra accounts list` shows it signed out, a new task starts on the default and the default's
    held task goes on. Trabajo is removed afterwards.
- B1, after them: the sandbox's `claude` ran only as the SDK host's native Claude, never `--bg` or `-p`.

Evidence lands in `bazel-testlogs/tests/sandbox/journeys/test.outputs/` (`$SANDBOX_DIR/journeys`
when run by hand):
- `journeys.md` and `journeys.json`: each step with what it expected and saw;
- `views/<case>/`: the views at each milestone;
- `fakes/`: every `claude` and `gh` call, the orchestrator's batches and actions, and each native
  session's starts, inputs and steps;
- `logs/`: the daemons' and relay's logs, kept when a case failed.

## Agent SDK backend (`SDK=1`)

`SDK=1 FAKES=1 make e2e-sandbox` gives Mac 1 the pinned native Claude Code 2.1.300 in place of the
scripted one: every task it launches runs through the SDK host (`agent-host`, pinned Node) and the
real native Claude, against a local model double. `tools/e2e_sdk_backend.py` sets it up:
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
«Try now» lets it go on once the double answers again. While `sdk/model-usage` holds a usage as
JSON, the double reports it for the tasks' answers: a 250 000-token conversation with a five-minute
cache rests on its card and Alondra compacts it once, seconds later, keeping that card. Last, a task's native Claude, in a synthetic HOME
that is its default profile, loads the user's CLAUDE.md, skill and enabled plugin, the project's
CLAUDE.md, the local CLAUDE.local.md and Alondra's node plugin, whose MCP server connects.
Then the double answers tasks with tool calls the journey scripts in `sdk/model-tools`: a command
waits on its permission card and runs once the phone allows it; a question's option reaches Claude
as its answer; a plan the model writes to plan mode's plan file waits on its Review card and Claude
goes on with it once the phone approves it; «Interrupt» on the task stops a running command and the turn ends with one card; a
command that acts as the user is refused by the node guard with no card; a command that runs for
seconds finishes and its turn ends with one card. Last, a task's detail shows what its node reads from
the SDK host alone: its task list as «Steps», the call it and its subagent run in «Right now», its
shell and subagent in the background, then what it said last, its context and its cost.
`alondra-sandbox journey sdk` runs it against a sandbox already started with `SDK=1`; `journey all`
leaves it out.

A sandbox you will drive from the TUI needs a `SANDBOX_DIR` without symbolic links, as the default
is: the local action journal opens SQLite with `NOFOLLOW`, so under `/tmp`, itself a symbolic link,
the TUI's actions would get `503 outcome_unknown` and be retried in silence.

## Tester agent

`make e2e-explore` (`tools/e2e-explore.sh`) builds a fresh two-Mac sandbox with the fakes and seeds
it (`alondra-sandbox journey seed`):
- on Mac 1, a working task, a question, a PR in review, a finished task, a stopped node and a
  pending pairing;
- on Mac 2, a PR in review.

It opens each Mac's TUI in tmux (160x45, Spanish) and runs a print-mode Claude (`MODEL`, default
opus; `BUDGET` in USD, default 5) with `tools/e2e/explorer.md`. The agent drives the TUI with
`tmux send-keys`/`capture-pane`, moves the fake nodes, reads the views, and writes `report.md`
with the findings, captures and views in `.scratch/explore-<stamp>-report` (`REPORT_DIR`).

Its Claude starts with a clean environment and the Alondra plugins off, so it never reaches the
real Alondra. It may only run tmux, the sandbox's binaries and write in its report folder.
`FOCUS="…"` adds instructions.

## Synthetic phone

```sh
alondra-sandbox phone --mac 1 pair                      # approve on the Mac, wait for relay enrollment
alondra-sandbox phone --mac 1 request GET /capabilities # one encrypted request over iroh, prints the response
alondra-sandbox phone --mac 1 revoke                    # revoke on the Mac, wait until the relay refuses
alondra-sandbox phone --mac 1 pair --direct             # pair over the direct listener, codes compared
alondra-sandbox phone --mac 1 request --direct GET /capabilities   # the same request, over the direct listener
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
make e2e-direct-cards                # the synthetic phone, over the Mac's direct listener
make e2e-cards-tui                   # the real TUI in tmux
make e2e-ios-cards                   # the iOS app in a pooled simulator, driven with MAV
E2E_CAPTURES=.scratch/cards make e2e-cards-tui # keeps what the client showed at each step
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
