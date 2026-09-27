# Alondra

The nervous system of the hive's orchestrator: reliable state for every Claude Code node and
for the machine, structured questions that reach the user and come back to their node, and
tools for the orchestrator to act.

| Folder | What it is |
|---|---|
| `crates/core` | State model and reducer (Rust, no I/O) |
| `crates/alondrad` | 24/7 daemon: hooks, sentinel, SQLite, API |
| `crates/alondra` | CLI, TUI, the nodes' MCP server and inbox hook |
| `plugin/` | Claude Code plugin: hooks, `ask`/`report` tools and skill |
| `launchd/` | macOS service template for `alondrad` (installed by `alondra setup`) |
| `signing/` | App signing: `signing.example.bzl` → local copy `signing.bzl` and profiles |
| `docs/plan.md` | Design and phased plan |
| `docs/tasks-plan.md` | Requests, tasks and their state machine, as built |

## Trying it

On a new Mac, follow [`INSTALL.md`](INSTALL.md) (written for an agent to execute).

```bash
make install        # builds with Bazel and installs alondra and alondrad in ~/.local/bin
alondra setup       # config.toml, launchd service, plugin marketplace and iPhone pairing (--help, --dry-run)
alondra doctor      # what works and what is missing, with how to fix it
alondra             # the TUI
alondra status      # the same as text
```

Only the sessions Alondra launches are Alondra's. Its `launch` and `define` load the plugin into
each node for that session alone and mark it through `--settings` (`ALONDRA_LAUNCHED`, which
the hooks and the tools send back); background sessions keep both flags across restarts. Any
other Claude session (a cron, your own terminal) has no plugin and stays out: no node, no task,
no card. By hand, a node is launched the same way:

```bash
claude --bg --name "app/fix" --plugin-dir ~/Projects/alondra/plugin \
  --settings '{"env": {"ALONDRA_LAUNCHED": "1"}}' --permission-mode auto -- "<mission>"
```

Settings live in `~/.alondra/config.toml` (`name`, `port`, `listen`, `public_url`,
`legacy_bridge`, `wake`, `retention_hours`, `service_label`, `resume_dead_nodes`,
`close_free_nodes_after`, `compact_idle_nodes_at`, `compact_orchestrator_at`, `archived_tasks_days`). An `ALONDRA_*` environment
variable overrides the file; `alondra config` shows the values in effect. What Alondra does with
idle nodes (compact at 30 % before a new task, close or ask after 7 days) is in
[docs/idle-nodes.md](docs/idle-nodes.md).

**The orchestrator** is a durable conversation with an inbox owned by Alondra. When work
arrives, alondrad starts `claude -p --permission-mode auto` with the node and orchestrator
plugins, resumes the saved conversation, and lets the process exit after the batch. The
conversation runs in Alondra's internal runtime directory. An idle orchestrator is available;
it does not need a background process or an attachable terminal. Worker nodes still use
Claude background sessions.

The plugins provide `fleet`, `node`, `machine`, `send`, `interrupt`, `launch`, `define`,
`candidates`, `cancel` and `reopen`. User questions always use Alondra `ask`; terminal permission
and question prompts are disabled. Auto mode can deny an operation, which becomes a visible
failure instead of an invisible prompt.

Every 90 seconds Alondra queues a durable digest of new reports, questions addressed to the
orchestrator, finished nodes with unfinished worktrees, and machine capacity warnings. Inbox
claims and completion are persisted. A failed execution with unknown effects blocks its batch
until the user chooses to retry it or continue without repeating it; later messages stay queued.
These actions, plus cancellation, are available in the node detail in the apps and TUI.
`alondra status` shows the execution ID for CLI recovery:

```sh
alondra orchestrator-cancel RUN_ID
alondra orchestrator-retry RUN_ID --confirm
alondra orchestrator-acknowledge RUN_ID --confirm
```

### Uncertain deliveries

HTTP acceptance is not proof that Claude executed an action. While a handoff to a
live node is uncertain, the TUI shows "N messages not confirmed"; `:deliveries`
shows the recorded evidence and recovery commands. A handoff whose node is gone
closes by itself (`node_gone`), since there is no terminal left to check. Inspect
the destination terminal before choosing a resolution:

```sh
alondra delivery list
alondra delivery retry ID --expected-version VERSION --terminal-checked --confirm-duplicate-risk
alondra delivery discard ID --expected-version VERSION --terminal-checked
# Use an explicitly approved remote Mac instead of Local:
alondra delivery --mac MAC_ID list
```

Add the same `--mac MAC_ID` after `delivery` when resolving a remote attempt.
A changed version is rejected; inspect again before deciding. Retry can duplicate
an external effect; discard does not undo it. A menu retry reopens the question
for a new selection. No command retries an uncertain delivery automatically.

After a successful batch, Alondra measures the latest main-model input, including cache tokens.
At `compact_orchestrator_at` percent of the reported context window (default 50; 0 disables
manual compaction), it runs `/compact` on the same conversation before more work. A compact
boundary confirms reduction; absent measurements stay unknown. Claude's automatic compaction
remains enabled. The executor runs when `wake` is on.

Existing background orchestrators migrate after becoming idle and releasing their tasks,
permissions and pending deliveries. Alondra stops the old background process, retains its
conversation, and adopts its inbox. A blocked migration is shown in the fleet. See
[the transition plan](docs/orchestrator-on-demand-plan.md) and
[operational details](docs/orchestrator-on-demand.md).

### Scheduled tasks

Alondrad keeps local schedules in `~/.alondra/schedules.db` and starts a fresh task at each due
time. Create and edit them under Tasks → Scheduled Tasks in the Mac and iPhone apps, or press
`s` on the TUI Tasks screen. A schedule stores its mission, project, local time and launch
options. The editor's Advanced section accepts one Claude `--disallowedTools` pattern per line;
those patterns are applied to every run, including a task resumed after a failed start.

In Alondra-managed Claude sessions, the `PreToolUse` hook redirects `CronCreate` to the
`schedule_create` MCP tool. This keeps durable work in Alondra, where it is visible and can be
paused or edited. Subhour polling remains in the current Claude session because Alondra does
not support that cadence. The hook does not affect Claude sessions outside Alondra. See the
[Claude scheduled tasks documentation](https://code.claude.com/docs/en/scheduled-tasks).

### The TUI

- **Waiting for you**: questions from the nodes (structured, menu, permissions and plans), PR
  cards and reports, grouped by the task they belong to (`docs/card-rules.md`). `Enter` opens them
  with all their context, `1`–`4` picks an option and `r` answers with text.
- **Tasks**: each request with its tasks, what depends on what, each task's state (waiting for
  another, running, needs you, in review, closing, done, cancelled) and what its node is doing.
  `x` cancels a task and `Shift+R` reopens it (`docs/tasks-plan.md`).
- **Fleet**: everything `claude agents` lists, free, busy or waiting for the machine, with its
  tasks.
- `Tab` moves between the three, `r` messages the selected node and `Shift+M` the orchestrator.

## Development

`make build` / `make test` with Bazel. `cargo build` / `cargo test` also work, for fast
iteration and for rust-analyzer.

### Install on an iPhone

`make device` builds the arm64 iOS app, installs it on **David iPhone**, and opens it.
Use `make device DEVICE="name or UDID"` to select another paired device. The device
must have Developer Mode enabled and be reachable from this Mac. Existing app data
is retained; the running app is restarted.

Keep the development profiles for the app and notification extension in `signing/`
as configured by `signing.example.bzl` (or an ignored `signing.bzl` override).
Both profiles must include the selected device and have a matching local signing
identity. Profiles and private keys are never committed.
