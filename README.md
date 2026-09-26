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
`close_free_nodes_after`, `compact_idle_nodes_at`, `archived_tasks_days`). An `ALONDRA_*` environment
variable overrides the file; `alondra config` shows the values in effect. What Alondra does with
idle nodes (compact at 30 % before a new task, close or ask after 7 days) is in
[docs/idle-nodes.md](docs/idle-nodes.md).

**The orchestrator** is started with `--plugin-dir ~/Projects/alondra/plugin --plugin-dir
~/Projects/alondra/plugin-orchestrator` and the same mark: the node's hooks plus the `fleet`,
`node`, `machine`, `send`, `interrupt` and `launch` tools, and
`define`, `candidates`, `cancel` and `reopen` to turn the user's requests into tasks. Alondra
notifies it on its own, every 90 s and in a single message, of:
- new reports from the nodes;
- questions addressed to it;
- nodes that finish without saying what they want;
- the machine, when it runs out of memory or gets saturated.

(`claude --bg` does not inherit environment variables, so the role goes in its own plugin.)

If no orchestrator is running, alondrad launches one this way on its own and posts a notice
saying so (only when `wake` is on).

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
